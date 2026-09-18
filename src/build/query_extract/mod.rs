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
