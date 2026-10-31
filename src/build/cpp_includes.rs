//! C and C++ headers on the dependency tier: the one place an `#include`
//! becomes a file. The macro gather, the include closure, goto-def on an
//! include token and the tier's landings all resolve through `resolve`.
//!
//! The search roots are the compiler's: a translation unit's
//! `compile_commands.json` flags (`-iquote`, `-I`, `-isystem`,
//! `-idirafter`) around the probed toolchain roots. A unit the database
//! doesn't name — or a project with no database — searches the walk-up
//! roots instead (each ancestor of the includer and its `include/`), then
//! the toolchain's. A header reached from a unit borrows that unit's roots.
//!
//! The toolchain is the unit's too (`unit_toolchain`): its database entry's
//! driver, `-x` and `-std`, else its language — so a `.c` unit searches the
//! C toolchain's roots, never libstdc++'s wrappers, and ranks its macro arms
//! with `__cplusplus` undefined.

use crate::build::cpp_toolchain::{Lang, Toolchain};
use crate::build::language_driver::{DependencyLocator, LanguageDriver, Located, LocatedBy};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Where a build writes its compilation database, relative to the
/// directory it describes. The first present in the nearest ancestor wins.
const DATABASES: &[&str] = &["compile_commands.json", "build/compile_commands.json"];

/// How a directive asks for its header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncludeKind {
    /// `#include "x.h"`: the includer's own directory first.
    Quoted,
    /// `#include <x.h>`.
    Angle,
    /// `#include_next <x.h>`: the angle search, after the root the
    /// includer itself was found under.
    Next,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Directive {
    pub path: String,
    pub kind: IncludeKind,
}

impl Directive {
    /// A directive as the extractor captures its path token: a quoted
    /// path's content, or an angle path with its brackets.
    pub fn spelled(raw: &str) -> Directive {
        let raw = raw.trim();
        match raw.strip_prefix('<').and_then(|r| r.strip_suffix('>')) {
            Some(p) => Directive { path: p.to_string(), kind: IncludeKind::Angle },
            None => Directive { path: raw.trim_matches('"').to_string(), kind: IncludeKind::Quoted },
        }
    }
}

/// One unit's include flags, resolved against its `directory`, and the
/// toolchain its command compiles with.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct UnitRoots {
    quote: Vec<PathBuf>,
    angle: Vec<PathBuf>,
    after: Vec<PathBuf>,
    toolchain: Toolchain,
}

/// A compilation database: each unit it names → its include flags.
#[derive(Debug, Default)]
pub struct CompileDb {
    units: HashMap<PathBuf, Arc<UnitRoots>>,
    /// The file's path and stamp: what a cache keyed on these roots folds in.
    fingerprint: u64,
}

impl CompileDb {
    fn read(path: &Path) -> Option<CompileDb> {
        let text = std::fs::read_to_string(path).ok()?;
        let entries: Vec<serde_json::Value> = serde_json::from_str(&text).ok()?;
        let mut shared: HashMap<UnitRoots, Arc<UnitRoots>> = HashMap::new();
        let mut units = HashMap::new();
        for e in &entries {
            let Some(dir) = e.get("directory").and_then(|d| d.as_str()).map(PathBuf::from) else { continue };
            let Some(file) = e.get("file").and_then(|f| f.as_str()) else { continue };
            let args: Vec<String> = match (e.get("arguments").and_then(|a| a.as_array()), e.get("command").and_then(|c| c.as_str())) {
                (Some(a), _) => a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect(),
                (None, Some(c)) => split_command(c),
                _ => continue,
            };
            let unit = canonical(&dir.join(file));
            let roots = unit_roots(&dir, &args, &unit);
            let roots = Arc::clone(shared.entry(roots.clone()).or_insert_with(|| Arc::new(roots)));
            units.insert(unit, roots);
        }
        Some(CompileDb { units, fingerprint: stamp_hash(path) })
    }
}

/// The include flags of one command line, in the compiler's order.
fn unit_roots(dir: &Path, args: &[String], unit: &Path) -> UnitRoots {
    let mut out = UnitRoots {
        quote: Vec::new(),
        angle: Vec::new(),
        after: Vec::new(),
        toolchain: Toolchain::from_command(args, unit),
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let (flag, inline) = ["-iquote", "-isystem", "-idirafter", "-I"]
            .iter()
            .find_map(|f| a.strip_prefix(f).map(|rest| (*f, rest)))
            .unwrap_or(("", ""));
        if flag.is_empty() {
            i += 1;
            continue;
        }
        let value = if inline.is_empty() {
            i += 1;
            args.get(i).map(String::as_str).unwrap_or("")
        } else {
            inline
        };
        i += 1;
        if value.is_empty() {
            continue;
        }
        let d = dir.join(value);
        match flag {
            "-iquote" => out.quote.push(d),
            "-idirafter" => out.after.push(d),
            _ => out.angle.push(d),
        }
    }
    out
}

/// A `command` string split as a POSIX shell would split its words.
fn split_command(cmd: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = cmd.chars();
    let (mut single, mut double) = (false, false);
    while let Some(c) = chars.next() {
        match c {
            '\'' if !double => {
                single = !single;
                in_word = true;
            }
            '"' if !single => {
                double = !double;
                in_word = true;
            }
            '\\' if !single => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    in_word = true;
                }
            }
            c if c.is_whitespace() && !single && !double => {
                if in_word {
                    out.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        out.push(cur);
    }
    out
}

fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn stamp_hash(p: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    p.hash(&mut h);
    if let Ok(meta) = std::fs::metadata(p) {
        meta.len().hash(&mut h);
        meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_nanos()).hash(&mut h);
    }
    h.finish()
}

/// Every directory's nearest compilation database, every database read,
/// and every directory's sources (a header's toolchain): a resolve runs per
/// directive, so none is re-derived per call. Reading a project's map again
/// (`locator`) forgets them all.
#[derive(Default)]
struct Databases {
    nearest: HashMap<PathBuf, Option<PathBuf>>,
    read: HashMap<PathBuf, Option<Arc<CompileDb>>>,
    sources: HashMap<PathBuf, Arc<Vec<PathBuf>>>,
}

fn databases() -> &'static Mutex<Databases> {
    static D: OnceLock<Mutex<Databases>> = OnceLock::new();
    D.get_or_init(Default::default)
}

/// The compilation database describing `file`: the nearest ancestor's.
pub fn database_for(file: &Path) -> Option<Arc<CompileDb>> {
    let dir = file.parent()?.to_path_buf();
    let mut dbs = databases().lock().unwrap_or_else(|e| e.into_inner());
    let path = match dbs.nearest.get(&dir) {
        Some(p) => p.clone(),
        None => {
            let found = dir.ancestors().find_map(|a| DATABASES.iter().map(|d| a.join(d)).find(|p| p.is_file()));
            dbs.nearest.insert(dir, found.clone());
            found
        }
    }?;
    dbs.read.entry(path.clone()).or_insert_with(|| CompileDb::read(&path).map(Arc::new)).clone()
}

fn forget_databases() {
    *databases().lock().unwrap_or_else(|e| e.into_inner()) = Databases::default();
}

/// What the roots `file` searches were read from — its database's stamp
/// and its toolchain — folded into every cache keyed on what its includes
/// resolve to.
pub fn roots_fingerprint(file: &Path) -> u64 {
    database_for(file).map_or(0, |db| db.fingerprint) ^ unit_toolchain(file).fingerprint().rotate_left(7)
}

/// `unit`'s own database entry, if its nearest database names it.
fn unit_entry(unit: &Path) -> Option<Arc<UnitRoots>> {
    database_for(unit).and_then(|db| db.units.get(&canonical(unit)).cloned())
}

/// The toolchain `unit` compiles with: its database entry's, else its
/// language's. A header analyzed on its own is no unit, so it borrows the
/// toolchain of the source compiled beside it (`header_lang`).
pub fn unit_toolchain(unit: &Path) -> Toolchain {
    if let Some(u) = unit_entry(unit) {
        return u.toolchain.clone();
    }
    match Lang::of_source(unit) {
        Some(lang) => Toolchain::for_lang(lang),
        None => header_toolchain(unit),
    }
}

/// The toolchain an analysis is built with: its unit's, and C++ — the
/// superset — for a buffer with no path. Stamped on the analysis
/// (`PackFacts::toolchain`), so every later question asked from it reads the
/// answer instead of re-deriving it.
pub fn analysis_toolchain(path: Option<&Path>) -> Toolchain {
    path.map_or_else(|| Toolchain::for_lang(Lang::Cpp), unit_toolchain)
}

/// A header's toolchain, interpolated as clangd interpolates a header's
/// command: the same-stem source beside it (`util.h` → `util.c`), else the
/// sources in its directory when they agree on a language; C++ — the
/// superset — when nothing beside it says.
fn header_toolchain(header: &Path) -> Toolchain {
    let Some(dir) = header.parent() else { return Toolchain::for_lang(Lang::Cpp) };
    let sources = sources_in(dir);
    let stem = header.file_stem();
    if let Some(src) = sources.iter().find(|p| p.file_stem() == stem) {
        return match unit_entry(src) {
            Some(u) => u.toolchain.clone(),
            None => Toolchain::for_lang(Lang::of_source(src).unwrap_or(Lang::Cpp)),
        };
    }
    let mut langs = sources.iter().filter_map(|p| Lang::of_source(p));
    let lang = match langs.next() {
        Some(first) if langs.all(|l| l == first) => first,
        _ => Lang::Cpp,
    };
    Toolchain::for_lang(lang)
}

/// The source files directly in `dir`, listed once per map read.
fn sources_in(dir: &Path) -> Arc<Vec<PathBuf>> {
    if let Some(hit) = databases().lock().unwrap_or_else(|e| e.into_inner()).sources.get(dir) {
        return Arc::clone(hit);
    }
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| Lang::of_source(p).is_some())
        .collect();
    found.sort();
    let found = Arc::new(found);
    databases().lock().unwrap_or_else(|e| e.into_inner()).sources.insert(dir.to_path_buf(), Arc::clone(&found));
    found
}

/// The toolchain roots `unit` searches, in its toolchain's order.
fn system_roots_for(unit: &Path) -> Vec<PathBuf> {
    unit_toolchain(unit).info().map(|t| t.include_dirs.clone()).unwrap_or_default()
}

/// The tier's roots: every default toolchain's, C++'s first. A unit's own
/// search order is `system_roots_for`'s; this is only where the tier lives.
fn tier_roots() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for lang in [Lang::Cpp, Lang::C] {
        for d in Toolchain::for_lang(lang).info().map(|t| t.include_dirs.clone()).unwrap_or_default() {
            if !out.contains(&d) {
                out.push(d);
            }
        }
    }
    out
}

/// The roots `includer` searches for `kind`, in order, reached from the
/// unit `unit`.
fn search_roots(includer: &Path, unit: &Path, kind: IncludeKind, system: &[PathBuf]) -> Vec<PathBuf> {
    let db = unit_entry(unit);
    let own = || includer.parent().map(Path::to_path_buf);
    let mut out: Vec<PathBuf> = Vec::new();
    match db {
        Some(u) => {
            if kind == IncludeKind::Quoted {
                out.extend(own());
                out.extend(u.quote.iter().cloned());
            }
            out.extend(u.angle.iter().cloned());
            out.extend(system.iter().cloned());
            out.extend(u.after.iter().cloned());
        }
        None => {
            // `include/` is the conventional `-Iinclude`: without it a test
            // or src tree spelling `"fmt/format.h"` has an empty closure, and
            // the visibility gate cuts it off from every target.
            for dir in includer.parent().into_iter().flat_map(Path::ancestors) {
                out.push(dir.to_path_buf());
                out.push(dir.join("include"));
            }
            out.extend(system.iter().cloned());
        }
    }
    out
}

/// The file `d` names from `includer`, reached from the unit `unit` (the
/// includer itself when it was not reached through another).
pub fn resolve(unit: &Path, includer: &Path, d: &Directive) -> Option<PathBuf> {
    resolve_among(unit, includer, d, &system_roots_for(unit))
}

pub(crate) fn resolve_among(unit: &Path, includer: &Path, d: &Directive, system: &[PathBuf]) -> Option<PathBuf> {
    let roots = search_roots(includer, unit, d.kind, system);
    let start = match d.kind {
        IncludeKind::Next => {
            // The root the includer was found under: the one that spells it
            // by this path, else the deepest holding it.
            let inc = canonical(includer);
            let found = roots
                .iter()
                .position(|r| canonical(&r.join(&d.path)) == inc)
                .or_else(|| {
                    roots
                        .iter()
                        .enumerate()
                        .filter(|(_, r)| inc.starts_with(canonical(r)))
                        .max_by_key(|(_, r)| r.components().count())
                        .map(|(i, _)| i)
                });
            found.map_or(0, |i| i + 1)
        }
        _ => 0,
    };
    roots[start.min(roots.len())..].iter().map(|r| r.join(&d.path)).find(|p| p.is_file())
}

/// The C/C++ dependency map for a project: the toolchain's roots are the
/// tier's, and a header is asked by the path its includer resolved it to
/// (a spelling alone names no file: it depends on who includes it).
pub struct IncludeMap {
    system: Vec<PathBuf>,
    inputs: Vec<PathBuf>,
}

pub fn locator(root: &Path, _driver: &'static dyn LanguageDriver) -> Arc<dyn DependencyLocator> {
    forget_databases();
    Arc::new(IncludeMap { system: tier_roots(), inputs: DATABASES.iter().map(|d| root.join(d)).collect() })
}

/// The external inputs a C/C++ analysis under `root` reads: the default C
/// and C++ toolchains, the root's compilation database, and every toolchain
/// its units name.
pub fn input_fingerprint(root: Option<&Path>) -> u64 {
    let mut fp = crate::build::cpp_reparse::toolchain_fingerprint();
    if let Some(db) = root.and_then(|r| DATABASES.iter().map(|d| r.join(d)).find(|p| p.is_file())) {
        fp ^= stamp_hash(&db).rotate_left(1);
        if let Some(read) = CompileDb::read(&db) {
            let mut named: Vec<&Toolchain> = read.units.values().map(|u| &u.toolchain).collect();
            named.sort();
            named.dedup();
            for t in named {
                fp = fp.rotate_left(5) ^ t.fingerprint();
            }
        }
    }
    fp
}

impl DependencyLocator for IncludeMap {
    /// A header's own path places it.
    fn locate(&self, identity: &str) -> Vec<Located> {
        let path = Path::new(identity);
        match path.is_absolute() && path.is_file() {
            true => vec![Located { path: canonical(path), by: LocatedBy::Exact }],
            false => Vec::new(),
        }
    }
    fn roots(&self) -> &[PathBuf] {
        &self.system
    }
    fn inputs(&self) -> &[PathBuf] {
        &self.inputs
    }
    fn search_roots(&self) -> &[PathBuf] {
        &self.system
    }
    // No `for_each_name`: an identity here is a path, which completion
    // never offers; an include spelling is not one `locate` places.
}

#[cfg(test)]
#[path = "cpp_includes_tests.rs"]
mod tests;
