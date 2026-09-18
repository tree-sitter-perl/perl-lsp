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
