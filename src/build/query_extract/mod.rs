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
    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    for dir in crate::build::plugin::rhai_host::plugin_search_dirs() {
        if let Ok(read) = std::fs::read_dir(&dir) {
            for entry in read.flatten() {
                let candidate = entry.path().join("entry.json");
                if candidate.is_file() {
                    paths.push(candidate);
                }
            }
        }
    }
    paths.sort();
    let key = format!("{}|{}", pack.lang_id, paths.len());
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
    for (name, s) in pack.bundled_overlays {
        match Query::new(language, s) {
            Ok(_) => {
                assembled.push('\n');
                assembled.push_str(s);
            }
            Err(e) => {
                eprintln!("perl-lsp: bundled {} overlay {name} dropped: {e}", pack.lang_id);
            }
        }
    }
    for (p, s) in &sources {
        match Query::new(language, s) {
            Ok(_) => {
                assembled.push('\n');
                assembled.push_str(s);
            }
            Err(e) => {
                eprintln!("perl-lsp: pack overlay {} dropped: {e}", p.display());
            }
        }
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
