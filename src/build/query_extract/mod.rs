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

#[derive(Debug, Clone, serde::Deserialize)]
struct RailsDoc {
    language: String,
    #[serde(default)]
    text_rails: Vec<TextRail>,
    #[serde(default)]
    path_rails: Vec<PathRail>,
    /// rail → how the undefined-name lane phrases a miss (`"event": "No
    /// listener for event"`); default `Undefined <rail>`.
    #[serde(default)]
    labels: std::collections::HashMap<String, String>,
    /// Rails whose miss is a hint, not a warning: their definitions are
    /// partly runtime-only (framework-default middleware aliases, database
    /// permissions on the ability rail), so an unmatched name is a lead.
    #[serde(default)]
    hints: Vec<String>,
    /// rail → the separator after which a use carries PARAMETERS
    /// (`throttle:60,1` names `throttle`); the name and its span end there.
    #[serde(default)]
    name_seps: std::collections::HashMap<String, String>,
}

/// The lane-facing rail conventions of a language, merged over its rail
/// documents.
#[derive(Debug, Default, Clone)]
pub struct RailConventions {
    pub labels: Vec<(String, String)>,
    pub hints: Vec<String>,
    pub name_seps: Vec<(String, String)>,
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
