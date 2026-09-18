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
///   * `when_isa` — the symbol's class isa one of the (leaf-keyed)
///     classes, written as one name or a list of them.
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
    #[serde(default, deserialize_with = "de_string_or_list")]
    pub when_isa: Vec<String>,
}

/// A document field that holds one name or a list of them. A rule that
/// applies to a family of bases says so once instead of being copied per
/// base — the copy is where the seventh base gets added to one rule and
/// not its sibling.
pub(crate) fn de_string_or_list<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum OneOrMany {
        One(String),
        Many(Vec<String>),
    }
    use serde::Deserialize as _;
    Ok(match OneOrMany::deserialize(d)? {
        OneOrMany::One(s) => vec![s],
        OneOrMany::Many(v) => v,
    })
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
    /// rail → every separator its names are written with: a path rail's
    /// hierarchy `sep`, and the parameter separator `name_seps` gives it. A
    /// name that ENDS with one is a prefix the caller concatenates onto
    /// (`view('parts.' . $kind)`), which the undefined-name lane cannot
    /// answer for.
    pub seps: Vec<(String, String)>,
    pub hints: Vec<String>,
    pub name_seps: Vec<(String, String)>,
    /// The rails the documents declare class-keyed — baked onto every file
    /// of the pack as `PackFacts::class_named_rails`.
    pub class_named_rails: Vec<String>,
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

mod extract;
mod packs;
mod skeleton;
pub use extract::*;
pub use packs::*;
pub use skeleton::*;

#[cfg(test)]
#[path = "../query_extract_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "../cpp_typedef_alias_tests.rs"]
mod cpp_typedef_alias_tests;
