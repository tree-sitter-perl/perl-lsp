//! Running the pack's own query AT THE CURSOR.
//!
//! The document already says what a member access, a call, or a skippable
//! token looks like in this language; a cursor-time consumer that keeps its
//! own node-kind tables is asking the same question a second time, with an
//! answer that drifts from the one the extractor uses (rule #15). These are
//! the seams that let it read the document instead: the compiled query, the
//! captures rooted at one node, whether a capture fires there, and the
//! literals a capture's predicates name.
//!
//! What makes this affordable at keystroke rate is the three bounds
//! `captures_at` documents. Without them the same idea costs ~20 ms per
//! keystroke on a large file (a full-tree traversal of a 600-pattern
//! query); with them it is ~2 µs, because the cursor visits one node.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

use super::LangPack;

/// lang_id → the compiled skeleton query.
///
/// Keyed by language, not by overlay set: a query's overlays are discovered
/// once and never hot-reload within a process (the same posture that lets
/// `cached_query` leak its compilations), and re-deriving the effective
/// source to key by it would put a plugin-dir `read_dir` plus a hash of
/// every overlay on the keystroke path — the cost this memo exists to
/// avoid. The first compilation of a language wins.
fn memo() -> &'static Mutex<HashMap<&'static str, &'static Query>> {
    static MEMO: OnceLock<Mutex<HashMap<&'static str, &'static Query>>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record the query `extract` compiled, so a cursor-time caller gets THE
/// object the extractor matched with rather than one of its own.
pub(super) fn remember(lang_id: &'static str, query: &'static Query) {
    memo().lock().unwrap().entry(lang_id).or_insert(query);
}

/// The pack's compiled query — the same object the extractor uses.
///
/// `None` before this language has been extracted once, which for a cursor
/// verb cannot happen: the document under the cursor was analysed by this
/// pack to produce the analysis the verb is answering from.
pub(crate) fn pack_query(pack: &LangPack) -> Option<&'static Query> {
    memo().lock().unwrap().get(pack.lang_id).copied()
}

/// The pack's compiled query, compiling it here if this process has not
/// extracted the language yet.
///
/// `pack_query` answers only once the extractor has run, which is the
/// normal case for a cursor verb but not a guarantee for every entry
/// point. `cached_query` memoises by source, so the object minted here is
/// the SAME one the next `extract` gets — the cursor path and the
/// extractor never match with two different queries.
pub(crate) fn query_for(
    language: &tree_sitter::Language,
    pack: &LangPack,
) -> Option<&'static Query> {
    if let Some(q) = pack_query(pack) {
        return Some(q);
    }
    let source = super::effective_query_source(language, pack);
    let query = super::cached_query(language, source).ok()?;
    remember(pack.lang_id, query);
    Some(query)
}

/// Does this pack's document DECLARE `capture` at all? A capability is
/// "what the query mints", never a Rust list's emptiness (rule #15): a
/// pack that spells `@arity.args` has a call shape and therefore a
/// signature, whichever nodes it spelled it on.
///
/// Answered off the base query, compiled once per language here because a
/// capability is asked before the language has analysed anything (the
/// extractor's object is not yet remembered).
pub(crate) fn pack_declares_capture(
    language: &tree_sitter::Language,
    pack: &LangPack,
    capture: &str,
) -> bool {
    static CACHE: OnceLock<Mutex<HashMap<&'static str, Vec<String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache.lock().unwrap();
    let names = guard.entry(pack.lang_id).or_insert_with(|| {
        Query::new(language, pack.query_source)
            .map(|q| q.capture_names().iter().map(|s| s.to_string()).collect())
            .unwrap_or_default()
    });
    names.iter().any(|n| n == capture)
}

/// The captures of every match whose pattern roots AT `node`.
///
/// Three bounds, all load-bearing — a reader who drops one gets the same
/// answers at a keystroke cost that is three orders of magnitude worse:
///
///   * the cursor runs on `node`, not the tree root, so the walk starts at
///     the cursor rather than at byte 0;
///   * `set_byte_range(node.byte_range())` stops it leaving the subtree —
///     without it the cursor keeps matching forward through the rest of the
///     file once the node is exhausted;
///   * `set_max_start_depth(Some(0))` admits only patterns that root at
///     `node` itself. This is the one that matters: a member chain or a
///     large class body is a deep subtree, and matching every pattern at
///     every descendant is what turns ~2 µs into ~20 ms.
///
/// The bounds are also the CONTRACT: every returned capture is inside
/// `node`, and a pattern that roots below it does not answer here.
pub(crate) fn captures_at<'t>(
    query: &'static Query,
    node: Node<'t>,
    src: &[u8],
) -> Vec<(&'static str, Node<'t>)> {
    let names = query.capture_names();
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(node.byte_range());
    cursor.set_max_start_depth(Some(0));
    let mut out = Vec::new();
    let mut matches = cursor.matches(query, node, src);
    while let Some(m) = matches.next() {
        for c in m.captures {
            out.push((names[c.index as usize], c.node));
        }
    }
    out
}

/// Does a match rooted AT `node` capture `capture` anywhere in it? The
/// question a cursor climb asks of each ancestor — "is this a member
/// access / an argument list / a domain comparison" — answered by the
/// document's own patterns, fields and predicates included, never by the
/// node's kind alone.
///
/// Stops at the first match that carries the capture: a yes/no over a node
/// that roots one match per child (a 50,000-argument list) must not pay
/// for all of them.
pub(crate) fn fires_at(query: &'static Query, node: Node<'_>, src: &[u8], capture: &str) -> bool {
    first_match_where(query, node, src, capture, |_| true)
}

/// Is `node` ITSELF captured as `capture` by a match rooted at it — the
/// token is a bare variable read, a skip region, a receiver wrapper — as
/// opposed to the capture landing on one of its children.
pub(crate) fn is_captured_as(query: &'static Query, node: Node<'_>, src: &[u8], capture: &str) -> bool {
    let id = node.id();
    first_match_where(query, node, src, capture, |n| n.id() == id)
}

fn first_match_where(
    query: &'static Query,
    node: Node<'_>,
    src: &[u8],
    capture: &str,
    admit: impl Fn(Node<'_>) -> bool,
) -> bool {
    let Some(index) = query.capture_index_for_name(capture) else { return false };
    let mut cursor = QueryCursor::new();
    cursor.set_byte_range(node.byte_range());
    cursor.set_max_start_depth(Some(0));
    let mut matches = cursor.matches(query, node, src);
    while let Some(m) = matches.next() {
        if m.captures.iter().any(|c| c.index == index && admit(c.node)) {
            return true;
        }
    }
    false
}

/// capture name → the literals its `#eq?` / `#any-of?` predicates require,
/// per compiled query (keyed by the leaked object's address).
type Literals = HashMap<String, &'static HashSet<&'static str>>;

fn literal_tables() -> &'static Mutex<HashMap<usize, Literals>> {
    static TABLES: OnceLock<Mutex<HashMap<usize, Literals>>> = OnceLock::new();
    TABLES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Read every positive `#eq?` / `#any-of?` predicate off a raw compiled
/// query: the capture it constrains and the string literals it names.
///
/// The Rust bindings parse these into a private field, so the C API is the
/// only exact reader; `cached_query` calls this between `Query::into_raw`
/// and `Query::from_raw`, while it owns the pointer.
///
/// # Safety
/// `raw` must be a live query no other code is using.
pub(super) unsafe fn read_literals(raw: *const tree_sitter::ffi::TSQuery) -> HashMap<String, HashSet<String>> {
    use tree_sitter::ffi;
    let text = |ptr: *const std::ffi::c_char, len: u32| -> String {
        if ptr.is_null() || len == 0 {
            return String::new();
        }
        let bytes = std::slice::from_raw_parts(ptr.cast::<u8>(), len as usize);
        String::from_utf8_lossy(bytes).into_owned()
    };
    let string = |id: u32| {
        let mut len = 0u32;
        text(ffi::ts_query_string_value_for_id(raw, id, &mut len), len)
    };
    let capture = |id: u32| {
        let mut len = 0u32;
        text(ffi::ts_query_capture_name_for_id(raw, id, &mut len), len)
    };
    let mut out: HashMap<String, HashSet<String>> = HashMap::new();
    for pattern in 0..ffi::ts_query_pattern_count(raw) {
        let mut count = 0u32;
        let steps = ffi::ts_query_predicates_for_pattern(raw, pattern, &mut count);
        if steps.is_null() || count == 0 {
            continue;
        }
        let steps = std::slice::from_raw_parts(steps, count as usize);
        for predicate in steps.split(|s| s.type_ == ffi::TSQueryPredicateStepTypeDone) {
            let [op, target, args @ ..] = predicate else { continue };
            if op.type_ != ffi::TSQueryPredicateStepTypeString
                || target.type_ != ffi::TSQueryPredicateStepTypeCapture
                || !matches!(string(op.value_id).as_str(), "eq?" | "any-of?")
            {
                continue;
            }
            let set = out.entry(capture(target.value_id)).or_default();
            for arg in args.iter().filter(|a| a.type_ == ffi::TSQueryPredicateStepTypeString) {
                set.insert(string(arg.value_id));
            }
        }
    }
    out
}

/// File the literals `read_literals` found under the query they came from.
pub(super) fn record_literals(query: &'static Query, literals: HashMap<String, HashSet<String>>) {
    let table: Literals = literals
        .into_iter()
        .map(|(cap, set)| {
            let set: HashSet<&'static str> =
                set.into_iter().map(|s| &*Box::leak(s.into_boxed_str())).collect();
            (cap, &*Box::leak(Box::new(set)))
        })
        .collect();
    literal_tables().lock().unwrap().insert(query as *const Query as usize, table);
}

/// The string literals a capture's own patterns require it to equal — the
/// `#eq?` / `#any-of?` arguments written beside `@capture`.
///
/// A small closed keyword set (`self`/`static`, `parent`, `__construct`,
/// the superglobals) belongs in the document, on the capture that fires on
/// it. A consumer that needs the SET rather than one match — "does this
/// language spell its own class `self`?" — reads it back off the compiled
/// query here, so the `.scm` stays the one home and a plugin overlay that
/// widens the set widens every reader with it. Empty for a capture carrying
/// no such predicate.
pub(crate) fn capture_literals(query: &'static Query, capture: &str) -> &'static HashSet<&'static str> {
    static EMPTY: OnceLock<HashSet<&'static str>> = OnceLock::new();
    literal_tables()
        .lock()
        .unwrap()
        .get(&(query as *const Query as usize))
        .and_then(|t| t.get(capture).copied())
        .unwrap_or_else(|| EMPTY.get_or_init(Default::default))
}

/// A language's recovery vocabulary for half-typed code, declared in its
/// own document: `(#recover-pair! "(" ")")` on the pattern of the construct
/// a pair closes, `(#set! recover.terminator ";")` on a statement pattern.
/// Directives filter no match, so hanging one on an existing pattern costs
/// that pattern nothing.
#[derive(Debug, Default)]
pub(crate) struct Recovery {
    /// Each open token with its closers, both in declaration order — the
    /// skeleton's before an overlay's, because the effective query is
    /// assembled in that order. A pair declared twice is one entry.
    pub pairs: Vec<(String, Vec<String>)>,
    pub terminators: Vec<String>,
}

impl Recovery {
    pub fn closers(&self, open: &str) -> Option<&[String]> {
        self.pairs.iter().find(|(o, _)| o == open).map(|(_, c)| c.as_slice())
    }

    /// The token closes SOME declared pair — the half of the stack walk
    /// that pops.
    pub fn is_closer(&self, token: &str) -> bool {
        self.pairs.iter().any(|(_, c)| c.iter().any(|x| x == token))
    }
}

/// The recovery vocabulary `query`'s document declares, read once per
/// compiled query off `general_predicates` / `property_settings`.
pub(crate) fn recovery(query: &'static Query) -> &'static Recovery {
    static CACHE: OnceLock<Mutex<HashMap<usize, &'static Recovery>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = query as *const Query as usize;
    if let Some(r) = cache.lock().unwrap().get(&key) {
        return r;
    }
    let mut out = Recovery::default();
    for pattern in 0..query.pattern_count() {
        for p in query.general_predicates(pattern) {
            if &*p.operator != "recover-pair!" {
                continue;
            }
            let [tree_sitter::QueryPredicateArg::String(open), tree_sitter::QueryPredicateArg::String(close)] =
                &*p.args
            else {
                continue;
            };
            match out.pairs.iter_mut().find(|(o, _)| **o == **open) {
                Some((_, closers)) if closers.iter().any(|c| **c == **close) => {}
                Some((_, closers)) => closers.push(close.to_string()),
                None => out.pairs.push((open.to_string(), vec![close.to_string()])),
            }
        }
        for prop in query.property_settings(pattern) {
            if &*prop.key == "recover.terminator" {
                if let Some(t) = prop.value.as_deref() {
                    if !out.terminators.iter().any(|x| x == t) {
                        out.terminators.push(t.to_string());
                    }
                }
            }
        }
    }
    let leaked: &'static Recovery = Box::leak(Box::new(out));
    cache.lock().unwrap().insert(key, leaked);
    leaked
}

#[cfg(test)]
#[path = "../cursor_query_tests.rs"]
mod tests;
