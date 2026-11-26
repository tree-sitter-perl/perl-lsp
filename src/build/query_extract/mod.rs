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
    /// What follows a label a call passes an argument under
    /// (`#set! label.written`): what label completion writes.
    pub(crate) label_written: Option<String>,
    /// The pairs the cursor reparse may close (`#recover-pair!`).
    pub(crate) recovery: Recovery,
}

/// The directive that declares a pair the cursor reparse may close:
/// `(#recover-pair! "(" ")")`, read once at compile and never run as a
/// match filter.
pub(crate) const RECOVER_PAIR: &str = "recover-pair!";

/// The bracket pairs a document declares for half-typed code. Which token
/// opens and which closes is a fact about the language's tokens, not about
/// any one construct, so a document states it once, on its `(ERROR)`
/// pattern. Each token is resolved to its kind id at compile: a token the
/// grammar lacks, or a second closer for one open, fails the compile.
#[derive(Debug, Default)]
pub struct Recovery {
    pairs: Vec<Pair>,
}

#[derive(Debug)]
struct Pair {
    open: u16,
    close: u16,
    close_text: String,
}

impl Recovery {
    fn read(query: &Query, language: &Language) -> Result<Recovery, String> {
        let mut pairs: Vec<Pair> = Vec::new();
        for pattern in 0..query.pattern_count() {
            for p in query.general_predicates(pattern) {
                if &*p.operator != RECOVER_PAIR {
                    continue;
                }
                let [tree_sitter::QueryPredicateArg::String(open), tree_sitter::QueryPredicateArg::String(close)] = &*p.args
                else {
                    return Err(format!("#{RECOVER_PAIR} takes an open and a close token"));
                };
                let id = |tok: &str| match language.id_for_node_kind(tok, false) {
                    0 => Err(format!("#{RECOVER_PAIR}: `{tok}` is not a token of this grammar")),
                    id => Ok(id),
                };
                let (open_id, close_id) = (id(open)?, id(close)?);
                match pairs.iter().find(|q| q.open == open_id) {
                    Some(q) if q.close == close_id => {}
                    Some(_) => return Err(format!("#{RECOVER_PAIR}: `{open}` is declared with two closers")),
                    None => pairs.push(Pair { open: open_id, close: close_id, close_text: close.to_string() }),
                }
            }
        }
        Ok(Recovery { pairs })
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    /// The closer an open token's kind takes, as a kind id and as written.
    pub fn closer_of(&self, open: u16) -> Option<(u16, &str)> {
        self.pairs.iter().find(|p| p.open == open).map(|p| (p.close, p.close_text.as_str()))
    }

    pub fn is_open(&self, kind: u16) -> bool {
        self.pairs.iter().any(|p| p.open == kind)
    }

    pub fn is_close(&self, kind: u16) -> bool {
        self.pairs.iter().any(|p| p.close == kind)
    }
}

/// The recovery pairs `pack`'s document declares, through the extractor's
/// own compilation. `None` when the document does not compile.
pub fn recovery(language: &Language, pack: &LangPack) -> Option<&'static Recovery> {
    cached_query(language, pack).ok().map(|c| &c.recovery)
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
fn cached_query(language: &Language, pack: &LangPack) -> Result<&'static CompiledPack, String> {
    let source = pack.query_source;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    type Cell = OnceLock<Result<CompiledPack, String>>;
    static CACHE: OnceLock<Mutex<HashMap<usize, &'static Cell>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = source.as_ptr() as usize;
    // The map's lock covers only finding the document's cell. The compile
    // runs under the cell, so index workers that arrive together wait for
    // one compile instead of each running their own.
    let cell: &'static Cell = *cache.lock().unwrap().entry(key).or_insert_with(|| Box::leak(Box::new(OnceLock::new())));
    cell.get_or_init(|| compile(language, pack)).as_ref().map_err(String::clone)
}

fn compile(language: &Language, pack: &LangPack) -> Result<CompiledPack, String> {
    let source = pack.query_source;
    let query = Query::new(language, source).map_err(|e| format!("query: {e}"))?;
    crate::build::query_predicates::check_source(source).map_err(|e| format!("query: {e}"))?;
    let captures = vocab::capture_table(query.capture_names(), &pack.member_spaces).map_err(|e| format!("query: {e}"))?;
    crate::build::query_predicates::check(&query, language, &[RECOVER_PAIR]).map_err(|e| format!("query: {e}"))?;
    vocab::check_settings(&query).map_err(|e| format!("query: {e}"))?;
    vocab::check_space_marks(source).map_err(|e| format!("query: {e}"))?;
    vocab::check_links(&query, &captures).map_err(|e| match e {
        vocab::LinkError::Pattern(at) => {
            let line = source[..at].lines().count() + 1;
            format!("query: line {line}: a pattern with @link.<rel> needs exactly one @link.from.<walk>, and one with a from needs a link")
        }
        vocab::LinkError::Unrooted(w) => {
            format!("query: @link.from.{w} links for a walk whose root @{} the document never captures", w.root())
        }
    })?;
    let label_written = (0..query.pattern_count())
        .flat_map(|p| query.property_settings(p).iter())
        .find(|s| *s.key == *vocab::SettingKey::LabelWritten.spelling())
        .and_then(|s| s.value.as_deref().map(str::to_string));
    let recovery = Recovery::read(&query, language).map_err(|e| format!("query: {e}"))?;
    Ok(CompiledPack { query, captures, label_written, recovery })
}

/// Every capture `pack`'s document mints, through the extractor's own
/// compilation. Empty when the document does not compile, which the first
/// extraction reports.
pub fn document_captures(language: &Language, pack: &LangPack) -> Vec<Capture> {
    cached_query(language, pack)
        .map(|c| c.captures.iter().flatten().copied().collect())
        .unwrap_or_default()
}

/// The closed capture vocabulary of a query document read beside the
/// extraction — a manifest a tool generated, not source the project wrote
/// (`ClassmapCap`). Each capture name is read once, when the document
/// compiles.
#[cfg(feature = "php")]
pub trait DocumentVocab: Copy {
    fn from_capture(name: &str) -> Option<Self>;
}

/// Every match of `document` over `source` in `driver`'s grammar, each as
/// its captures and their text, in match order. A capture name `V` does
/// not hold fails the compile, so a document and its reader cannot drift
/// apart in silence.
#[cfg(feature = "php")]
pub fn document_matches<'s, V: DocumentVocab>(
    driver: &dyn crate::build::language_driver::LanguageDriver,
    document: &str,
    source: &'s str,
) -> Result<Vec<Vec<(V, &'s str)>>, String> {
    let mut parser = driver.make_parser();
    let language: Language = parser.language().map(|l| (*l).clone()).ok_or("no grammar")?;
    let query = Query::new(&language, document).map_err(|e| format!("query: {e}"))?;
    let vocab: Vec<V> = query
        .capture_names()
        .iter()
        .map(|n| V::from_capture(n).ok_or_else(|| format!("query: @{n} is not in the document's vocabulary")))
        .collect::<Result<_, _>>()?;
    let tree = crate::util::timings::phase("document::parse", || parser.parse(source, None)).ok_or("no parse")?;
    let mut out = Vec::new();
    crate::util::timings::phase("document::match", || {
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
        while let Some(m) = matches.next() {
            out.push(
                m.captures
                    .iter()
                    .filter_map(|c| Some((vocab[c.index as usize], c.node.utf8_text(source.as_bytes()).ok()?)))
                    .collect(),
            );
        }
    });
    Ok(out)
}

mod cursor;
mod extract;
mod packs;
mod places;
mod skeleton;
mod vocab;
pub use cursor::*;
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
