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
//!     `@scope`, `@context.*`, `@import`), closed by `vocab::Capture`.
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

/// A pack's compiled skeleton query and its capture table, indexed by
/// capture id (`vocab::capture_table`).
pub(crate) struct CompiledPack {
    pub(crate) query: Query,
    pub(crate) captures: Vec<Option<Capture>>,
    /// The constructor's name: the one spelling the document's
    /// `@mark.constructor` admits, which `@expr.construct` sites call.
    pub(crate) constructor: Option<&'static str>,
}

/// The literals the document's `#eq?` / `#any-of?` predicates admit for
/// `capture`, read off the document's own text: the compiled query keeps
/// text predicates private, and the document stays the one home of the
/// spelling.
fn predicate_literals(source: &'static str, capture: &str) -> Vec<&'static str> {
    let mut out = Vec::new();
    for line in source.lines() {
        let code = line.split(';').next().unwrap_or("");
        for head in ["(#eq? @", "(#any-of? @"] {
            let mut rest = code;
            while let Some(at) = rest.find(head) {
                rest = &rest[at + head.len()..];
                let Some(args) = rest.strip_prefix(capture).filter(|a| a.starts_with(char::is_whitespace)) else {
                    continue;
                };
                let args = &args[..args.find(')').unwrap_or(args.len())];
                out.extend(args.split('"').skip(1).step_by(2));
            }
        }
    }
    out
}

/// Compile each pack's skeleton query exactly once and reuse it.
///
/// `Query::new` is expensive (~400ms for the Perl skeleton) and `extract`
/// runs per file, so recompiling every call dominates the workload. A pack's
/// `query_source` is a unique `&'static str`, so its pointer identity keys the
/// compiled query — same pack, same query, one compilation. Leaking the boxed
/// query is bounded (one per language pack) and gives the `&'static` the
/// cache needs. A capture name outside the vocabulary, a predicate outside
/// the structural set, a link without its `@link.from.<walk>`, or a walk
/// without its root fails the compile.
fn cached_query(language: &Language, source: &'static str) -> Result<&'static CompiledPack, String> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<usize, &'static CompiledPack>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = source.as_ptr() as usize;
    if let Some(q) = cache.lock().unwrap().get(&key) {
        return Ok(q);
    }
    let query = Query::new(language, source).map_err(|e| format!("query: {e}"))?;
    crate::build::query_predicates::check_source(source).map_err(|e| format!("query: {e}"))?;
    let captures = vocab::capture_table(query.capture_names()).map_err(|e| format!("query: {e}"))?;
    crate::build::query_predicates::check(&query, language, &[]).map_err(|e| format!("query: {e}"))?;
    vocab::check_links(&query, &captures).map_err(|e| match e {
        vocab::LinkError::Pattern(at) => {
            let line = source[..at].lines().count() + 1;
            format!("query: line {line}: a pattern with @link.<rel> needs exactly one @link.from.<walk>, and one with a from needs a link")
        }
        vocab::LinkError::Unrooted(w) => {
            format!("query: @link.from.{w} links for a walk whose root @{} the document never captures", w.root())
        }
    })?;
    let constructor = match predicate_literals(source, "mark.constructor").as_slice() {
        [] => None,
        [one] => Some(*one),
        _ => return Err("query: @mark.constructor admits more than one name".into()),
    };
    if constructor.is_none() && captures.contains(&Some(Capture::Expr(ExprCap::Construct))) {
        return Err("query: @expr.construct calls the constructor, and no @mark.constructor names it".into());
    }
    let leaked: &'static CompiledPack = Box::leak(Box::new(CompiledPack { query, captures, constructor }));
    cache.lock().unwrap().insert(key, leaked);
    Ok(leaked)
}

/// Every capture `pack`'s document mints, through the extractor's own
/// compilation. Empty when the document does not compile, which the first
/// extraction reports.
pub fn document_captures(language: &Language, pack: &LangPack) -> Vec<Capture> {
    cached_query(language, pack.query_source)
        .map(|c| c.captures.iter().flatten().copied().collect())
        .unwrap_or_default()
}

mod extract;
mod packs;
mod skeleton;
mod vocab;
pub use extract::*;
pub use packs::*;
pub use skeleton::*;
pub use vocab::*;

#[cfg(test)]
#[path = "../query_extract_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "../cpp_typedef_alias_tests.rs"]
mod cpp_typedef_alias_tests;

#[cfg(test)]
#[path = "../php_pack_tests.rs"]
mod php_pack_tests;

#[cfg(test)]
#[path = "../python_pack_tests.rs"]
mod python_pack_tests;
