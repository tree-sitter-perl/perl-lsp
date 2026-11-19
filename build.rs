//! Derives the cache fingerprints and the `cache-epochs/` gate versions at
//! compile time.
//!
//! A baked conclusion has a staleness class the witness bag does not: editing
//! a reducer changes what the right answer IS while the stored bytes stay
//! perfectly well-formed. Nothing downstream can catch that — the bytes
//! decode, the shape validates, the answer is simply wrong — so the guard has
//! to be a fingerprint that moves on its own when the derivation moves.
//! `docs/adr/conclusion-layer.md` owns the decision.
//!
//! Two properties this file exists to guarantee, both of which fail silently
//! if broken:
//!
//! 1. **Every hashed file is declared to cargo.** A source edit that does not
//!    re-run this script leaves the constant stale, which is a hand-maintained
//!    version with extra steps and a false sense of safety. `rerun-if-changed`
//!    on a DIRECTORY only watches that directory's own mtime — cargo does not
//!    walk it — so every file is named individually.
//! 2. **The hash is order-independent and rename-sensitive.** Directory
//!    iteration order is not stable, so paths are sorted; and the path is
//!    hashed alongside the bytes, because moving a file between layers changes
//!    behaviour here (`layering_tests`) while leaving byte content identical.

use std::path::{Path, PathBuf};

fn main() {
    grammar_vocabulary("perl", ts_parser_perl::LANGUAGE.into());
    let mut files = Vec::new();
    collect(Path::new("src"), &mut files);
    // Dependencies change fold outcomes too — a grammar bump reshapes the CST,
    // which reshapes the witnesses. The lock file is the cheapest complete
    // statement of what we built against.
    if Path::new("Cargo.lock").is_file() {
        files.push(PathBuf::from("Cargo.lock"));
    }
    files.sort();

    let mut acc: u64 = 0xcbf2_9ce4_8422_2325;
    for path in &files {
        // Cargo reruns this script when any declared path changes. Declaring
        // the file we just hashed — rather than a directory above it — is what
        // keeps the constant honest.
        println!("cargo:rerun-if-changed={}", path.display());
        let Ok(bytes) = std::fs::read(path) else { continue };
        // The separator makes the concatenation unambiguous: without it,
        // moving a trailing byte from one file's name into the next file's
        // contents would hash identically.
        fnv(&mut acc, path.to_string_lossy().as_bytes());
        fnv(&mut acc, b"\0");
        fnv(&mut acc, &bytes);
        fnv(&mut acc, b"\0");
    }

    // FNV-1a rather than DefaultHasher: the std hasher's algorithm is
    // explicitly unspecified across releases, and while that would be
    // survivable here (a different toolchain is a legitimately different
    // build), an unspecified hash is a poor thing to hang a cache invariant
    // on when the specified one is four lines.
    println!("cargo:rustc-env=PERL_LSP_CONCLUSION_FINGERPRINT={acc:016x}");

    // The SURFACE lane's own gate, narrower on purpose.
    //
    // A persisted `Surface` outlives the `Surface::project` that wrote it, so
    // it needs a version that moves when the projection does — the same class
    // as a derivation outliving its source, and the same reason a
    // hand-bumped constant is the wrong instrument. Scoped to `src/model/`
    // because that is what determines a projection's OUTPUT, and because the
    // whole-tree fingerprint above would invalidate every persisted surface on
    // an edit to an LSP handler, paying a corpus-wide re-projection for a
    // change that cannot alter one.
    //
    // Cargo.lock rides along: a grammar bump reshapes the CST, which reshapes
    // the analysis a surface is projected from.
    let mut model = Vec::new();
    collect(Path::new("src/model"), &mut model);
    if Path::new("Cargo.lock").is_file() {
        model.push(PathBuf::from("Cargo.lock"));
    }
    model.sort();
    let mut sacc: u64 = 0xcbf2_9ce4_8422_2325;
    for path in &model {
        println!("cargo:rerun-if-changed={}", path.display());
        let Ok(bytes) = std::fs::read(path) else { continue };
        fnv(&mut sacc, path.to_string_lossy().as_bytes());
        fnv(&mut sacc, b"\0");
        fnv(&mut sacc, &bytes);
        fnv(&mut sacc, b"\0");
    }
    println!("cargo:rustc-env=PERL_LSP_SURFACE_FINGERPRINT={sacc:016x}");

    epochs();
}

/// The hand-declared cache gates, one directory per gate under
/// `cache-epochs/` (`cache-epochs/README.md` is the contract).
///
/// A gate's value is a hash of the FILE NAMES in its directory, so a change
/// that invalidates a cache adds a file named for itself. Two PRs adding two
/// files never touch the same line, which is the point: a shared integer made
/// every pair of bumping PRs conflict.
///
/// `(directory, constant, type)` — the type is the column or field the
/// value is stored in.
const EPOCH_GATES: &[(&str, &str, EpochType)] = &[
    ("extract", "EXTRACT", EpochType::Int),
    ("stub", "STUB", EpochType::Str),
    ("ref-rows", "REF_ROWS", EpochType::Str),
    ("conclusion-rows", "CONCLUSION_ROWS", EpochType::Str),
    ("schema", "SCHEMA", EpochType::Str),
    ("macro-cache", "MACRO_CACHE", EpochType::Int),
];

#[derive(Clone, Copy)]
enum EpochType {
    Int,
    Str,
}

fn epochs() {
    let root = Path::new("cache-epochs");
    let mut out = String::new();
    for (dir, name, ty) in EPOCH_GATES {
        let gate = root.join(dir);
        // A directory argument makes cargo rescan it, so adding or deleting
        // an epoch file re-runs this script.
        println!("cargo:rerun-if-changed={}", gate.display());
        let mut names: Vec<String> = std::fs::read_dir(&gate)
            .unwrap_or_else(|e| panic!("{}: {e} — every cache gate needs its epoch directory", gate.display()))
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        // An empty set would hash to the same value for every gate and every
        // history that ever emptied one; refuse it rather than collide.
        assert!(!names.is_empty(), "{}: a cache gate needs at least one epoch file", gate.display());
        names.sort();
        let mut acc: u64 = 0xcbf2_9ce4_8422_2325;
        fnv(&mut acc, dir.as_bytes());
        for n in &names {
            fnv(&mut acc, b"\0");
            fnv(&mut acc, n.as_bytes());
        }
        match ty {
            EpochType::Int => out.push_str(&format!("pub const {name}: i64 = {};\n", acc as i64)),
            EpochType::Str => out.push_str(&format!("pub const {name}: &str = \"{acc:016x}\";\n")),
        }
    }
    let dest = Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("cache_epochs.rs");
    std::fs::write(&dest, out).expect("write cache_epochs.rs");
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect(&path, out),
            Ok(t) if t.is_file() => out.push(path),
            _ => {}
        }
    }
}

fn fnv(acc: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *acc ^= *b as u64;
        *acc = acc.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

/// Writes `$OUT_DIR/<lang>_grammar.rs`: the grammar's fields and named node
/// kinds as enums whose discriminants are the grammar's own ids. Code that
/// names a field or kind the grammar does not have then fails to compile,
/// and a read by id is no string search of the grammar's tables.
fn grammar_vocabulary(lang_name: &str, lang: tree_sitter::Language) {
    use std::collections::BTreeMap;
    use std::fmt::Write;
    let camel = |name: &str| {
        let mut out = String::new();
        for part in name.split(|c: char| !c.is_ascii_alphanumeric()).filter(|p| !p.is_empty()) {
            let mut chars = part.chars();
            out.extend(chars.next().map(|c| c.to_ascii_uppercase()));
            out.push_str(chars.as_str());
        }
        match out.as_str() {
            "" => None,
            "Self" => Some("Self_".to_string()),
            s if s.starts_with(|c: char| c.is_ascii_digit()) => Some(format!("K{s}")),
            _ => Some(out),
        }
    };
    let mut out = String::from("// GENERATED by build.rs from the linked grammar. Do not edit.\n\n");

    let fields: Vec<(u16, &str, String)> = (1..=lang.field_count() as u16)
        .filter_map(|id| {
            let name = lang.field_name_for_id(id)?;
            Some((id, name, camel(name)?))
        })
        .collect();
    out.push_str("/// A field of the grammar; the discriminant is its field id.\n");
    out.push_str("#[allow(dead_code)]\n#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]\n#[repr(u16)]\npub(crate) enum Field {\n");
    for (id, name, variant) in &fields {
        writeln!(out, "    /// `{name}`\n    {variant} = {id},").unwrap();
    }
    out.push_str("}\n\n#[allow(dead_code)]\nimpl Field {\n    pub(crate) const fn name(self) -> &'static str {\n        match self {\n");
    for (_, name, variant) in &fields {
        writeln!(out, "            Field::{variant} => {name:?},").unwrap();
    }
    out.push_str("        }\n    }\n}\n\n");

    // A name several symbols share (aliases) is one kind: the id a node
    // reports for it is the public one, which is the discriminant.
    let count = lang.node_kind_count() as u16;
    let mut kinds: BTreeMap<&str, (u16, String)> = BTreeMap::new();
    for id in 0..count {
        if !(lang.node_kind_is_named(id) && lang.node_kind_is_visible(id)) {
            continue;
        }
        let Some(name) = lang.node_kind_for_id(id) else { continue };
        let Some(variant) = camel(name) else { continue };
        kinds.entry(name).or_insert_with(|| (lang.id_for_node_kind(name, true), variant));
    }
    let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
    for (name, (_, variant)) in &kinds {
        if let Some(other) = seen.insert(variant.as_str(), name) {
            panic!("{lang_name} grammar: kinds `{other}` and `{name}` both spell `{variant}`");
        }
    }
    out.push_str("/// A named node kind of the grammar; the discriminant is the kind id a\n/// node of it reports. `Error` is a tree-sitter ERROR node, `Token` any\n/// node the grammar does not name (punctuation, keywords).\n");
    out.push_str("#[allow(dead_code)]\n#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]\n#[repr(u16)]\npub(crate) enum Kind {\n");
    for (name, (id, variant)) in &kinds {
        writeln!(out, "    /// `{name}`\n    {variant} = {id},").unwrap();
    }
    out.push_str("    Token = 65534,\n    Error = 65535,\n}\n\n");
    writeln!(out, "static KIND_BY_ID: [Kind; {count}] = [").unwrap();
    for id in 0..count {
        let variant = lang
            .node_kind_for_id(id)
            .filter(|_| lang.node_kind_is_named(id) && lang.node_kind_is_visible(id))
            .and_then(|name| kinds.get(name))
            .map_or("Token", |(_, v)| v.as_str());
        writeln!(out, "    Kind::{variant},").unwrap();
    }
    out.push_str("];\n\n#[allow(dead_code)]\nimpl Kind {\n    pub(crate) fn from_id(id: u16) -> Kind {\n        match KIND_BY_ID.get(id as usize) {\n            Some(kind) => *kind,\n            None if id == 65535 => Kind::Error,\n            None => Kind::Token,\n        }\n    }\n\n    pub(crate) const fn name(self) -> &'static str {\n        match self {\n");
    for (name, (_, variant)) in &kinds {
        writeln!(out, "            Kind::{variant} => {name:?},").unwrap();
    }
    out.push_str("            Kind::Token => \"\",\n            Kind::Error => \"ERROR\",\n        }\n    }\n}\n");

    let dir = std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR for a build script");
    std::fs::write(Path::new(&dir).join(format!("{lang_name}_grammar.rs")), out).expect("write the grammar vocabulary");
}
