//! Query-driven entity extraction for pack languages.
//!
//! FileAnalysis extraction for a pack language is driven by declarative
//! tree-sitter queries — entities out, procedural state managed by a
//! generic driver + per-language predicates — so the per-language part
//! is DATA (a .scm query pack) rather than a hand-written walker. The
//! core is language-agnostic the way highlights.scm/tags.scm consumers
//! are. See `docs/adr/query-extraction-rings.md` for why this works for
//! ring-1/2 extraction and deliberately does not attempt ring-3 semantic
//! synthesis (that stays per-language: plugins for Perl, pack predicates
//! + the driver's fold contributors for everything else).
//!
//! Architecture:
//!   - `queries/perl/skeleton.scm` — patterns whose CAPTURE NAMES form
//!     a language-neutral entity vocabulary (`@def.*`, `@ref.*`,
//!     `@scope`, `@context.*`, `@import`).
//!   - `LangPack` — the per-language bundle: query source + host
//!     predicates for what patterns can't express (name shaping,
//!     suppression rules). The "back and forth": the driver owns
//!     ordered traversal and state (scope stack, sticky contexts);
//!     the pack answers point questions about text it understands.
//!   - `extract()` — the generic driver. Knows NO Perl: it sorts
//!     capture events, maintains the scope stack and sticky contexts,
//!     and assembles `SkelSymbol`/`SkelRef` rows.
//!
//! Wired into every pack language's `PackDriver` (`language_driver.rs`);
//! `query_extract_tests.rs` also measures it differentially against the
//! real Perl builder as an accuracy net.

use crate::model::file_analysis::{InferredType, Span};
use tree_sitter::{Language, Point, Query, QueryCursor, StreamingIterator, Tree};

/// Compile each pack's skeleton query exactly once and reuse it.
///
/// `Query::new` is expensive (~400ms for the Perl skeleton) and `extract`
/// runs per file, so recompiling every call dominates the workload. Keyed by
/// CONTENT hash (not pointer): a runtime-assembled source (bundled query +
/// pack-plugin overlays) has no stable address, and two assemblies of the
/// same bytes must share one compilation. Leaking the boxed query is bounded
/// (one per distinct (language, overlay-set) — overlays never hot-reload
/// within a process, matching the rhai registry's posture).
fn cached_query(language: &Language, source: &str) -> Result<&'static Query, String> {
    use std::collections::hash_map::DefaultHasher;
    use std::collections::HashMap;
    use std::hash::{Hash, Hasher};
    use std::sync::{Arc, Mutex, OnceLock};
    // Single-flight per source: the slot is claimed under the map lock and
    // compiled OUTSIDE it, so a second worker asking for the same query
    // waits on the slot instead of compiling a duplicate (every Rayon worker
    // started on a 1,000-line pack query at once — one wall, N CPUs).
    type Slot = Arc<OnceLock<Result<&'static Query, String>>>;
    static CACHE: OnceLock<Mutex<HashMap<u64, Slot>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = {
        let mut h = DefaultHasher::new();
        source.hash(&mut h);
        h.finish()
    };
    let slot = cache.lock().unwrap().entry(key).or_default().clone();
    slot.get_or_init(|| {
        // One compile per distinct source per process, so a printed phase
        // line stays bounded; this IS the pack cold-start floor.
        let query = crate::util::timings::phase("pack.query_compile", || {
            Query::new(language, source).map_err(|e| format!("query: {e}"))
        })?;
        Ok(Box::leak(Box::new(query)))
    })
    .clone()
}

// ---- framework-entry declarations (the heatmap's "runner-invoked" data) ----

/// One declared "a runner invokes this" rule, from an `entry.json`
/// document (bundled per pack, or `<plugin-dir>/<name>/entry.json`).
/// A rule matches a symbol when EVERY present condition holds:
///   * `attributes` — the symbol carries one of these annotation names
///     (php `#[Test]`, via the `@sym.attr` lane);
///   * `method_prefix` / `methods` — the symbol's name matches;
///   * `when_isa` — the symbol's class isa the (leaf-keyed) class.
/// Rules OR across the set. The engine only EVALUATES these; every
/// framework name lives in the data files (rule #10: the heatmap never
/// compares names itself).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct EntryMarker {
    #[serde(default)]
    pub attributes: Vec<String>,
    #[serde(default)]
    pub method_prefix: Option<String>,
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub when_isa: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct EntryDoc {
    language: String,
    entries: Vec<EntryMarker>,
}

/// One text rail: `calls` are the function names whose first single-quoted
/// argument names an entity on `rail`, scanned as TEXT in files whose path
/// ends with one of `files` (a Blade template is text to the grammar).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct TextRail {
    pub rail: String,
    pub calls: Vec<String>,
    pub files: Vec<String>,
    /// A substring every name must contain (`"."` for translation keys —
    /// a bare word is a JSON translation STRING, not a key path).
    #[serde(default)]
    pub requires: Option<String>,
}

/// One path rail: a file whose path contains `under` DEFINES a name on
/// `rail` — the rest of the path, `skip` leading segments dropped (a
/// locale), `strip` removed from the end, separators joined by `sep`
/// (`resources/views/a/b.blade.php` → `a.b`). With `keys`, the file's
/// returned-array string keys extend that name (`config/app.php` →
/// `app.name`, nested keys dotted) instead of the file naming itself.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct PathRail {
    pub rail: String,
    pub under: String,
    #[serde(default)]
    pub skip: usize,
    #[serde(default)]
    pub strip: String,
    #[serde(default = "default_sep")]
    pub sep: String,
    #[serde(default)]
    pub keys: bool,
    /// The file's METHODS define names on the rail (a policy class: every
    /// method is an ability); the path only selects the file.
    #[serde(default)]
    pub methods: bool,
}
fn default_sep() -> String {
    ".".to_string()
}

/// A rail declarations document (bundled per pack, or
/// `<plugin-dir>/<name>/rails.json`). Public so `--plugin-check`'s rail arm
/// can lint the same field list the loaders read.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct RailsDoc {
    pub language: String,
    #[serde(default)]
    pub text_rails: Vec<TextRail>,
    #[serde(default)]
    pub path_rails: Vec<PathRail>,
    /// rail → how the undefined-name lane phrases a miss (`"event": "No
    /// listener for event"`); default `Undefined <rail>`.
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
    /// Rails whose miss is a hint, not a warning: their definitions are
    /// partly runtime-only (framework-default middleware aliases, database
    /// permissions on the ability rail), so an unmatched name is a lead.
    #[serde(default)]
    pub hints: Vec<String>,
    /// rail → the separator after which a use carries PARAMETERS
    /// (`throttle:60,1` names `throttle`); the name and its span end there.
    #[serde(default)]
    pub name_seps: std::collections::HashMap<String, String>,
    /// rail → the diagnostic CODE its undefined-name findings carry
    /// (`"view": "undefined-view"`). Client-facing wire text, so it is the
    /// document's word, not a string the lane builds out of the rail name;
    /// a rail that declares none reports under one generic code with the
    /// rail in the diagnostic's `data`.
    #[serde(default)]
    pub codes: std::collections::HashMap<String, String>,
    /// rail → what its names DENOTE ([`RAIL_NAMES_ARE_CLASS`]: class
    /// identities, Laravel's event bus; a rail absent here names strings).
    /// A constant of the overlay, declared once, so every file of the pack
    /// answers the same question the same way.
    #[serde(default)]
    pub names_are: std::collections::HashMap<String, String>,
}

/// The `names_are` value that declares a rail's names to be class
/// identities (`RailNames::Classes`); any other value names strings.
pub const RAIL_NAMES_ARE_CLASS: &str = "class";

/// The lane-facing rail conventions of a language, merged over its rail
/// documents.
#[derive(Debug, Default, Clone)]
pub struct RailConventions {
    pub labels: Vec<(String, String)>,
    /// rail → the diagnostic code its findings carry.
    pub codes: Vec<(String, String)>,
    pub hints: Vec<String>,
    pub name_seps: Vec<(String, String)>,
    /// The rails the documents declare class-keyed — baked onto every file
    /// of the pack as `PackFacts::class_named_rails`.
    pub class_named_rails: Vec<String>,
}

/// Every `<plugin-dir>/<name>/<file_name>` under the shared plugin search
/// path, sorted so the discovered set is a deterministic cache key. The ONE
/// discovery helper for the plugin-loadable declaration documents
/// (`rails.json`, `entry.json`): a second copy of this walk is how a
/// document family ends up bundled-only without anything saying so.
fn plugin_documents(file_name: &str) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    for dir in crate::build::plugin::rhai_host::plugin_search_dirs() {
        if let Ok(read) = std::fs::read_dir(&dir) {
            for entry in read.flatten() {
                let candidate = entry.path().join(file_name);
                if candidate.is_file() {
                    paths.push(candidate);
                }
            }
        }
    }
    paths.sort();
    paths
}

/// The cache key for a (language, discovered document set) pair. The key IS
/// the path set, never its size: two plugin dirs of equal size would
/// otherwise serve each other's documents.
fn document_cache_key(lang_id: &str, paths: &[std::path::PathBuf]) -> String {
    paths.iter().fold(lang_id.to_string(), |mut k, p| {
        k.push('|');
        k.push_str(&p.to_string_lossy());
        k
    })
}

/// One rail document, or `None` when it declares another language. A
/// document that does not PARSE is dropped with a diagnostic naming its
/// origin: every family it carries — path rails, text rails, the lane's
/// labels and hint rails — is a feature that would otherwise go missing in
/// silence.
fn parse_rail_doc(src: &str, pack: &LangPack, origin: &dyn std::fmt::Display) -> Option<RailsDoc> {
    match serde_json::from_str::<RailsDoc>(src) {
        Ok(doc) if doc.language == pack.lang_id => Some(doc),
        Ok(_) => None,
        Err(e) => {
            eprintln!("perl-lsp: rail declarations {origin} dropped: {e}");
            None
        }
    }
}

/// Every rail document in force for a language: the pack's bundled ones
/// plus every discovered `<plugin-dir>/<name>/rails.json` declaring it —
/// the `entry.json` posture (cached per (lang, document set); documents
/// never hot-reload within a process). The three projections below read
/// THIS one loader, so a document's path rails, text rails and conventions
/// load together or not at all. Returns the cache key alongside, so a
/// projection reuses this call's discovery instead of walking again.
fn rail_docs_for(pack: &LangPack) -> (String, std::sync::Arc<Vec<RailsDoc>>) {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Vec<RailsDoc>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let paths = plugin_documents("rails.json");
    let key = document_cache_key(pack.lang_id, &paths);
    if let Some(v) = cache.lock().unwrap().get(&key) {
        return (key, Arc::clone(v));
    }
    let mut out: Vec<RailsDoc> = Vec::new();
    for src in pack.bundled_rail_docs {
        out.extend(parse_rail_doc(src, pack, &"(bundled)"));
    }
    for p in &paths {
        if let Ok(src) = std::fs::read_to_string(p) {
            out.extend(parse_rail_doc(&src, pack, &p.display()));
        }
    }
    let arc = Arc::new(out);
    cache.lock().unwrap().insert(key.clone(), Arc::clone(&arc));
    (key, arc)
}

/// The path rails in force for a language.
pub fn path_rails_for(pack: &LangPack) -> std::sync::Arc<Vec<PathRail>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Vec<PathRail>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let (key, docs) = rail_docs_for(pack);
    if let Some(v) = cache.lock().unwrap().get(&key) {
        return Arc::clone(v);
    }
    let arc = Arc::new(docs.iter().flat_map(|d| d.path_rails.iter().cloned()).collect());
    cache.lock().unwrap().insert(key, Arc::clone(&arc));
    arc
}

/// The text rails in force for a language.
pub fn text_rails_for(pack: &LangPack) -> std::sync::Arc<Vec<TextRail>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Vec<TextRail>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let (key, docs) = rail_docs_for(pack);
    if let Some(v) = cache.lock().unwrap().get(&key) {
        return Arc::clone(v);
    }
    let arc = Arc::new(docs.iter().flat_map(|d| d.text_rails.iter().cloned()).collect());
    cache.lock().unwrap().insert(key, Arc::clone(&arc));
    arc
}

/// The rail conventions (lane labels, hint rails, name separators, the
/// class-keyed rails) in force for a language.
pub fn rail_conventions_for(pack: &LangPack) -> std::sync::Arc<RailConventions> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<RailConventions>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let (key, docs) = rail_docs_for(pack);
    if let Some(v) = cache.lock().unwrap().get(&key) {
        return Arc::clone(v);
    }
    let mut out = RailConventions::default();
    for doc in docs.iter() {
        out.labels.extend(doc.labels.iter().map(|(k, v)| (k.clone(), v.clone())));
        out.codes.extend(doc.codes.iter().map(|(k, v)| (k.clone(), v.clone())));
        out.hints.extend(doc.hints.iter().cloned());
        out.name_seps.extend(doc.name_seps.iter().map(|(k, v)| (k.clone(), v.clone())));
        out.class_named_rails.extend(
            doc.names_are
                .iter()
                .filter(|(_, v)| v.as_str() == RAIL_NAMES_ARE_CLASS)
                .map(|(rail, _)| rail.clone()),
        );
    }
    out.labels.sort();
    out.codes.sort();
    out.hints.sort();
    out.name_seps.sort();
    out.class_named_rails.sort();
    out.class_named_rails.dedup();
    let arc = Arc::new(out);
    cache.lock().unwrap().insert(key, Arc::clone(&arc));
    arc
}

/// The framework-entry rules in force for a language: the pack's bundled
/// documents plus every discovered `<plugin-dir>/<name>/entry.json`
/// declaring this language. Cached per (lang, plugin-path set) like the
/// overlay assembly — entry data never hot-reloads within a process. A
/// malformed document is dropped with a stderr diagnostic (the bundled
/// rules and surviving documents still serve — the overlay posture).
pub fn entry_markers_for(pack: &LangPack) -> std::sync::Arc<Vec<EntryMarker>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Vec<EntryMarker>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let paths = plugin_documents("entry.json");
    let key = document_cache_key(pack.lang_id, &paths);
    if let Some(v) = cache.lock().unwrap().get(&key) {
        return Arc::clone(v);
    }
    let mut out: Vec<EntryMarker> = Vec::new();
    let mut fold = |src: &str, origin: &dyn std::fmt::Display| {
        match serde_json::from_str::<EntryDoc>(src) {
            Ok(doc) if doc.language == pack.lang_id => out.extend(doc.entries),
            Ok(_) => {}
            Err(e) => eprintln!("perl-lsp: entry declarations {origin} dropped: {e}"),
        }
    };
    for src in pack.bundled_entry_markers {
        fold(src, &"(bundled)");
    }
    for p in &paths {
        if let Ok(src) = std::fs::read_to_string(p) {
            fold(&src, &p.display());
        }
    }
    let arc = Arc::new(out);
    cache.lock().unwrap().insert(key, Arc::clone(&arc));
    arc
}

/// The capture names the pack's effective query mints — the bundled document
/// plus every discovered overlay, read off the compilation the extractor
/// itself uses (content-keyed, so asking costs no second compile). A
/// capability the DOCUMENT states — "these import tokens are paths"
/// (`@include.path`), "this language has a preprocessor" (`@def.macro`) — is
/// read from here, so a pack cannot declare one its patterns do not back.
pub fn query_captures(language: &Language, pack: &LangPack) -> Vec<String> {
    let source = effective_query_source(language, pack);
    cached_query(language, source)
        .map(|q| q.capture_names().iter().map(|c| c.to_string()).collect())
        .unwrap_or_default()
}

/// The builtin type names in force for a language: the pack's bundled
/// `builtins.txt` documents plus every discovered
/// `<plugin-dir>/<name>/builtins.txt`. Cached per (lang, plugin-path set)
/// like the entry markers — a runtime's surface does not hot-reload within a
/// process. One name per line; `#` starts a comment, blank lines are skipped.
pub fn builtin_types_for(pack: &LangPack) -> std::sync::Arc<Vec<String>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<Vec<String>>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let paths = plugin_documents("builtins.txt");
    let key = document_cache_key(pack.lang_id, &paths);
    if let Some(v) = cache.lock().unwrap().get(&key) {
        return Arc::clone(v);
    }
    let mut out: Vec<String> = Vec::new();
    let mut fold = |src: &str| {
        for line in src.lines() {
            let name = line.split('#').next().unwrap_or("").trim();
            if !name.is_empty() && !out.iter().any(|n| n == name) {
                out.push(name.to_string());
            }
        }
    };
    for src in pack.bundled_builtin_types {
        fold(src);
    }
    for p in &paths {
        if let Ok(src) = std::fs::read_to_string(p) {
            fold(&src);
        }
    }
    let arc = Arc::new(out);
    cache.lock().unwrap().insert(key, Arc::clone(&arc));
    arc
}

// ---- pack-plugin query overlays (tier 1, docs/prompt-pack-plugins.md) ----

/// Discovered overlay files for a language: every
/// `<plugin-dir>/<name>/queries/<lang_id>.scm` under the shared plugin
/// search path (`plugin_search_dirs` — one path for both plugin worlds),
/// sorted by path so assembly order is deterministic. Read per call, like
/// `plugin_source_paths` — cheap, and it keeps "what loads" and "what the
/// cache fingerprint hashes" the same enumeration.
pub fn pack_overlay_paths(lang_id: &str) -> Vec<std::path::PathBuf> {
    let mut out: Vec<std::path::PathBuf> = Vec::new();
    for dir in crate::build::plugin::rhai_host::plugin_search_dirs() {
        if let Ok(read) = std::fs::read_dir(&dir) {
            for entry in read.flatten() {
                let candidate = entry.path().join("queries").join(format!("{lang_id}.scm"));
                if candidate.is_file() {
                    out.push(candidate);
                }
            }
        }
    }
    out.sort();
    out
}

/// What the extractor does with a rail-suffixed capture. The consumers ask
/// the VALUE (is it a handler? is its rail class-named?) instead of
/// re-testing the family spelling, so [`RAIL_CAPTURE_FAMILIES`] is the one
/// home of the vocabulary and not a prose mirror of the match arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailCapture {
    /// `@def.handler.named.<rail>` — a registration string DECLARES a name
    /// on the rail; the captured string is the Handler symbol's name.
    Handler,
    /// `@def.handler.class.<rail>` — the same, on a rail a dispatcher CLASS
    /// names, so an unmatched firing reads as a dead emission, not an
    /// undefined name.
    ClassHandler,
    /// `@def.handler.by.<rail>` — a class-named handler whose name comes
    /// from another token of the same match, not from this capture.
    ClassHandlerNamedByMatch,
    /// `@ref.dispatch.named.<rail>` — a firing string USES a name on the rail.
    Dispatch,
    /// `@ref.dispatch.class.<rail>` — a firing on a class-named rail.
    ClassDispatch,
}

impl RailCapture {
    /// Does this capture DECLARE a name on its rail (rather than use one)?
    pub fn is_handler(self) -> bool {
        matches!(self, Self::Handler | Self::ClassHandler | Self::ClassHandlerNamedByMatch)
    }

    /// Is the rail named by a dispatcher CLASS? Such a rail's misses are
    /// dead emissions — the diagnostics lane says them as hints.
    pub fn is_class_named(self) -> bool {
        matches!(self, Self::ClassHandler | Self::ClassHandlerNamedByMatch | Self::ClassDispatch)
    }
}

/// The capture families whose name ends in a rail (`<family>.<rail>`, the
/// rail being the namespace one framework owns), each with what the
/// extractor makes of it. Both extractor arms and the lint read this
/// through [`rail_of`], so a family added here is served and reported by
/// construction.
const RAIL_CAPTURE_FAMILIES: &[(&str, RailCapture)] = &[
    ("def.handler.named", RailCapture::Handler),
    ("def.handler.class", RailCapture::ClassHandler),
    ("def.handler.by", RailCapture::ClassHandlerNamedByMatch),
    ("ref.dispatch.named", RailCapture::Dispatch),
    ("ref.dispatch.class", RailCapture::ClassDispatch),
];

/// The rail a capture names, and what the extractor makes of it:
/// `@def.handler.named.hook` → `(Handler, "hook")`. `None` for anything
/// else, including a bare family spelling with no rail (which mints
/// nothing — [`overlay_capture_findings`] says so).
pub fn rail_of(cap: &str) -> Option<(RailCapture, &str)> {
    RAIL_CAPTURE_FAMILIES.iter().find_map(|(f, kind)| {
        cap.strip_prefix(f)
            .and_then(|r| r.strip_prefix('.'))
            .filter(|r| !r.is_empty())
            .map(|r| (*kind, r))
    })
}

/// Capture names the extractor READS that no bundled skeleton spells.
/// A skeleton describes the language; these describe a FRAMEWORK's shapes
/// — a call or member whose name is a string, the companion token that
/// names a rail handler declared by another capture of the same match, an
/// array key a path rail promotes to a name, and a call expression whose
/// value the overlay declares. The extractor serves them all, so a
/// baseline that does not know them calls every one of them unserved.
const OVERLAY_ONLY_CAPTURES: &[&str] = &[
    "ref.call.named",
    "ref.method.named.self",
    "dispatch.via",
    "handler.name",
    "def.handler.key",
    "key.elem",
    "expr.annot",
];

/// The captures in `declared` the extractor does NOT serve — `--plugin-check`'s
/// vocabulary lint, answered here so the CLI holds no vocabulary of its own.
///
/// A query document's capture list is a SUBSET of the served vocabulary,
/// never the vocabulary itself: the skeleton spells what the skeleton
/// needs, the rail families are open by construction (the rail is an
/// overlay's own word), and [`OVERLAY_ONLY_CAPTURES`] is the rest. A
/// `_`-prefixed capture is a query-internal anchor, served by definition.
pub fn unserved_captures(
    pack: &LangPack,
    language: &tree_sitter::Language,
    declared: &[&str],
) -> Vec<String> {
    let skeleton: std::collections::HashSet<String> =
        match tree_sitter::Query::new(language, pack.query_source) {
            Ok(base) => base.capture_names().iter().map(|s| s.to_string()).collect(),
            Err(_) => Default::default(),
        };
    declared
        .iter()
        .filter(|c| {
            !c.starts_with('_')
                && !skeleton.contains(**c)
                && !OVERLAY_ONLY_CAPTURES.contains(*c)
                && rail_of(c).is_none()
        })
        .map(|s| s.to_string())
        .collect()
}

/// What an overlay's capture names say that the extractor cannot honour.
///
/// A capture outside the vocabulary is inert by design (an overlay written
/// against a newer engine degrades to silence). These are captures INSIDE a
/// known family whose payload is wrong — a rail family with no rail, a
/// declared attribute spelling no flag answers to — where silence reads as
/// a missing feature instead of a document to fix. `--plugin-check` reports
/// them.
pub fn overlay_capture_findings(captures: &[&str]) -> Vec<String> {
    use crate::model::file_analysis::SymbolFlags;
    let mut out = Vec::new();
    for c in captures {
        if let Some((family, _)) =
            RAIL_CAPTURE_FAMILIES.iter().find(|(f, _)| c == f || c.strip_prefix(f) == Some("."))
        {
            out.push(format!(
                "@{c} names no rail: the vocabulary is @{family}.<rail>, and a rail is the \
                 namespace one framework owns — an unnamed one would claim the whole program, \
                 so this capture mints nothing"
            ));
            continue;
        }
        if let Some(spelling) = c.strip_prefix("classattr.") {
            if SymbolFlags::try_from(spelling).is_err() {
                out.push(format!(
                    "@{c} declares the attribute `{spelling}`, which no symbol flag answers to \
                     (docs/adr/symbol-flags.md): it would stamp a string nothing reads"
                ));
            }
        }
    }
    out
}

/// Class-keyed rail CAPTURES whose rail no document declares class-keyed.
///
/// The capture family (`@def.handler.class.<rail>` / `.by.` /
/// `@ref.dispatch.class.<rail>`) is the mint path for a class-named
/// handler; `names_are` is what makes the rail's NAMES classes everywhere,
/// including the span-free minting paths a query never reaches. A capture
/// without the declaration mints handlers the lanes then read as strings —
/// silence that reads as a missing feature, so it is a finding.
pub fn class_rail_capture_findings(declared: &[String], captures: &[&str]) -> Vec<String> {
    let mut rails: Vec<&str> = captures
        .iter()
        .filter_map(|c| rail_of(c))
        .filter(|(k, _)| k.is_class_named())
        .map(|(_, r)| r)
        .filter(|r| !declared.iter().any(|d| d == r))
        .collect();
    rails.sort();
    rails.dedup();
    rails
        .into_iter()
        .map(|r| {
            format!(
                "@…class.{r} mints a class-named handler, but no rail document declares \
                 `\"names_are\": {{ \"{r}\": \"{RAIL_NAMES_ARE_CLASS}\" }}` — every lane would \
                 read the rail's names as strings"
            )
        })
        .collect()
}

/// Rails a document declares class-keyed that NO capture family mints —
/// answerable only over a pack's whole capture set (one overlay of several
/// legitimately carries none of them), which only the bundled-documents
/// tripwire holds; `--plugin-check` lints one document at a time.
#[cfg(test)]
pub fn class_rail_declaration_findings(declared: &[String], captures: &[&str]) -> Vec<String> {
    declared
        .iter()
        .filter(|d| {
            !captures
                .iter()
                .filter_map(|c| rail_of(c))
                .any(|(k, r)| k.is_class_named() && r == d.as_str())
        })
        .map(|d| {
            format!(
                "the rail document declares `{d}` class-keyed, but no overlay capture \
                 (@def.handler.class.{d} / @def.handler.by.{d} / @ref.dispatch.class.{d}) \
                 mints one"
            )
        })
        .collect()
}

/// The pack's effective query source: the bundled query plus every
/// surviving discovered overlay, assembled once per distinct overlay set
/// and leaked (`cached_query` then compiles it once by content).
///
/// Per-overlay compile ISOLATION: each overlay is test-compiled ALONE
/// against the grammar first; one that fails is dropped with a stderr
/// diagnostic naming the file, and the bundled query + surviving overlays
/// still serve — the same failure posture as a malformed `.rhai` (one bad
/// plugin cannot take the language out).
fn effective_query_source(language: &Language, pack: &LangPack) -> &'static str {
    use std::collections::hash_map::DefaultHasher;
    use std::collections::HashMap;
    use std::hash::{Hash, Hasher};
    use std::sync::{Mutex, OnceLock};
    let paths = pack_overlay_paths(pack.lang_id);
    if paths.is_empty() && pack.bundled_overlays.is_empty() {
        return pack.query_source;
    }
    static ASSEMBLED: OnceLock<Mutex<HashMap<u64, &'static str>>> = OnceLock::new();
    let cache = ASSEMBLED.get_or_init(|| Mutex::new(HashMap::new()));
    let sources: Vec<(std::path::PathBuf, String)> = paths
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|s| (p, s)))
        .collect();
    let key = {
        let mut h = DefaultHasher::new();
        pack.lang_id.hash(&mut h);
        // `bundled_overlays` is a per-`lang_id` compile-time constant, so the
        // id covers it; a runtime-configurable bundle would have to hash in.
        pack.query_source.hash(&mut h);
        for (p, s) in &sources {
            p.hash(&mut h);
            s.hash(&mut h);
        }
        h.finish()
    };
    if let Some(src) = cache.lock().unwrap().get(&key) {
        return src;
    }
    let mut assembled = String::from(pack.query_source);
    // Bundled overlays get the same isolation as plugin-dir ones: a syntax
    // slip in one framework document must not take every verb of the
    // language dark (it did — the documents used to be one `concat!`).
    // A finding does NOT drop the document: the capture that carries it
    // mints nothing while every other pattern still serves. Saying so is
    // what keeps it a document to fix rather than a missing feature.
    let fold = |src: &str, origin: &dyn std::fmt::Display, assembled: &mut String| {
        match Query::new(language, src) {
            Ok(q) => {
                for f in overlay_capture_findings(q.capture_names()) {
                    eprintln!("perl-lsp: overlay {origin}: {f}");
                }
                assembled.push('\n');
                assembled.push_str(src);
            }
            Err(e) => eprintln!("perl-lsp: overlay {origin} dropped: {e}"),
        }
    };
    for (name, s) in pack.bundled_overlays {
        fold(s, &format_args!("{} (bundled {})", name, pack.lang_id), &mut assembled);
    }
    for (p, s) in &sources {
        fold(s, &p.display(), &mut assembled);
    }
    let leaked: &'static str = Box::leak(assembled.into_boxed_str());
    cache.lock().unwrap().insert(key, leaked);
    leaked
}

mod cursor_query;
mod extract;
mod packs;
mod skeleton;
// Tested and unused until the sentinel stops consulting node-kind tables.
#[allow(unused_imports)]
pub(crate) use cursor_query::{
    capture_literals, captures_at, pack_declares_capture, pack_query, pattern_root_kinds,
    query_for, recv_peel_kinds,
};
pub use extract::*;
pub use packs::*;
pub use skeleton::*;

#[cfg(test)]
#[path = "../query_extract_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "../cpp_typedef_alias_tests.rs"]
mod cpp_typedef_alias_tests;
