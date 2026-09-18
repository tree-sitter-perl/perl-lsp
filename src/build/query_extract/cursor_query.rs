//! Running the pack's own query AT THE CURSOR.
//!
//! The document already says what a member access, a call, or a skippable
//! token looks like in this language; a cursor-time consumer that keeps its
//! own node-kind tables is asking the same question a second time, with an
//! answer that drifts from the one the extractor uses (rule #15). These are
//! the three seams that let it read the document instead: the compiled
//! query, the captures rooted at one node, and the node kinds a capture's
//! patterns root at.
//!
//! What makes this affordable at keystroke rate is the three bounds
//! `captures_at` documents. Without them the same idea costs ~20 ms per
//! keystroke on a large file (a full-tree traversal of a 600-pattern
//! query); with them it is ~2 µs, because the cursor visits one node.

// The sentinel's node-kind tables are what these replace; until that
// switch lands they ship tested and unused.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};
use tree_sitter::{Node, Query, QueryCursor, StreamingIterator};

use super::LangPack;

/// lang_id → (the compiled skeleton query, the source it was compiled from).
///
/// Keyed by language, not by overlay set: a query's overlays are discovered
/// once and never hot-reload within a process (the same posture that lets
/// `cached_query` leak its compilations), and re-deriving the effective
/// source to key by it would put a plugin-dir `read_dir` plus a hash of
/// every overlay on the keystroke path — the cost this memo exists to
/// avoid. The first compilation of a language wins.
fn memo() -> &'static Mutex<HashMap<&'static str, (&'static Query, &'static str)>> {
    static MEMO: OnceLock<Mutex<HashMap<&'static str, (&'static Query, &'static str)>>> =
        OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Every compiled query's own source, keyed by the object's address.
///
/// Separate from `memo` on purpose: `memo` holds ONE query per language
/// (the first wins, so a cursor keystroke never re-derives the effective
/// source), while a process that compiles a second query for the same
/// language — a different plugin-overlay set — still needs THAT object's
/// patterns readable. A lens that could not find its own query's source
/// answered EMPTY, which reads downstream as "the document says nothing".
fn sources() -> &'static Mutex<HashMap<usize, &'static str>> {
    static SOURCES: OnceLock<Mutex<HashMap<usize, &'static str>>> = OnceLock::new();
    SOURCES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record the query `extract` compiled, so a cursor-time caller gets THE
/// object the extractor matched with rather than one of its own.
pub(super) fn remember(lang_id: &'static str, query: &'static Query, source: &'static str) {
    sources().lock().unwrap().insert(query as *const Query as usize, source);
    memo().lock().unwrap().entry(lang_id).or_insert((query, source));
}

/// The pack's compiled query — the same object the extractor uses.
///
/// `None` before this language has been extracted once, which for a cursor
/// verb cannot happen: the document under the cursor was analysed by this
/// pack to produce the analysis the verb is answering from.
pub(crate) fn pack_query(pack: &LangPack) -> Option<&'static Query> {
    memo().lock().unwrap().get(pack.lang_id).map(|(q, _)| *q)
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
    remember(pack.lang_id, query, source);
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

/// The node kinds that root a pattern carrying `capture`.
///
/// This is where a consumer's node-kind table comes from once the table is
/// gone: "which nodes is a member access" is answered by the patterns that
/// capture `@member.recv`, so a document that teaches the language a new
/// member shape teaches every consumer at once.
///
/// Read off the compiled query: `capture_quantifiers` says which patterns
/// carry the capture, and each pattern's own source slice names its root.
/// A pattern whose root is not a named node — a bare anonymous token, a
/// wildcard `(_)`, a grouped sibling pattern `((a) @x . (b) @y)` — names no
/// kind and contributes none, so the set is what a consumer may match ON,
/// never a claim that nothing else can carry the capture. Empty for a query
/// this process did not compile through `extract`.
pub(crate) fn pattern_root_kinds(
    query: &'static Query,
    capture: &str,
) -> &'static HashSet<&'static str> {
    cached_over_patterns(query, capture, "roots", collect_root_kinds)
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
/// no such predicate, and for a query this process did not compile through
/// `extract`.
pub(crate) fn capture_literals(
    query: &'static Query,
    capture: &str,
) -> &'static HashSet<&'static str> {
    let cap = capture.to_string();
    cached_over_patterns(query, capture, "literals", move |src, out| {
        collect_capture_literals(src, &cap, out)
    })
}

/// The node kinds a receiver peels THROUGH — a transparent wrapper the
/// document names (`@recv.peel`, `@recv.peel.deref` where the wrapper also
/// dereferences). One set: the peel drops them all, and the distinction is
/// there for a consumer that reports what it dropped.
pub(crate) fn recv_peel_kinds(query: &'static Query) -> &'static HashSet<&'static str> {
    static CACHE: OnceLock<Mutex<HashMap<usize, &'static HashSet<&'static str>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = query as *const Query as usize;
    if let Some(set) = cache.lock().unwrap().get(&key) {
        return set;
    }
    let mut all: HashSet<&'static str> = HashSet::new();
    for cap in ["recv.peel", "recv.peel.deref"] {
        all.extend(pattern_root_kinds(query, cap).iter().copied());
    }
    let leaked: &'static HashSet<&'static str> = Box::leak(Box::new(all));
    cache.lock().unwrap().insert(key, leaked);
    leaked
}

/// Fold `collect` over the SOURCE of every pattern carrying `capture`,
/// memoised per (query, capture, lens). The pattern source is the only
/// place the compiled object still spells what the document wrote, so both
/// lenses read it the same way.
fn cached_over_patterns(
    query: &'static Query,
    capture: &str,
    lens: &str,
    collect: impl Fn(&'static str, &mut HashSet<&'static str>),
) -> &'static HashSet<&'static str> {
    static CACHE: OnceLock<Mutex<HashMap<(usize, String, String), &'static HashSet<&'static str>>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (query as *const Query as usize, capture.to_string(), lens.to_string());
    if let Some(set) = cache.lock().unwrap().get(&key) {
        return set;
    }
    let source = sources().lock().unwrap().get(&(query as *const Query as usize)).copied();
    let mut found: HashSet<&'static str> = HashSet::new();
    if let (Some(source), Some(index)) =
        (source, query.capture_names().iter().position(|n| *n == capture))
    {
        for pattern in 0..query.pattern_count() {
            let quantifiers = query.capture_quantifiers(pattern);
            if quantifiers
                .get(index)
                .is_none_or(|q| *q == tree_sitter::CaptureQuantifier::Zero)
            {
                continue;
            }
            let (start, end) = (
                query.start_byte_for_pattern(pattern),
                query.end_byte_for_pattern(pattern).min(source.len()),
            );
            if start < end {
                collect(&source[start..end], &mut found);
            }
        }
    }
    let leaked: &'static HashSet<&'static str> = Box::leak(Box::new(found));
    cache.lock().unwrap().insert(key, leaked);
    leaked
}

/// The quoted arguments of every `#eq?` / `#any-of?` predicate in `pattern`
/// whose FIRST argument is `@capture`. A predicate on another capture of
/// the same pattern states nothing about this one, so it contributes none.
fn collect_capture_literals(
    pattern: &'static str,
    capture: &str,
    out: &mut HashSet<&'static str>,
) {
    let want = format!("@{capture}");
    let mut rest = pattern;
    while let Some(hash) = rest.find('#') {
        rest = &rest[hash + 1..];
        let name_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        let predicate = &rest[..name_end];
        rest = &rest[name_end..];
        if !matches!(predicate, "eq?" | "any-of?") {
            continue;
        }
        let Some(close) = rest.find(')') else { return };
        let args = &rest[..close];
        let mut tokens = args.split_whitespace();
        if tokens.next() != Some(want.as_str()) {
            continue;
        }
        // The remaining arguments are the literals, one quoted token each.
        let mut at = 0usize;
        while let Some(open) = args[at..].find('"') {
            let start = at + open + 1;
            let Some(len) = args[start..].find('"') else { break };
            out.insert(&args[start..start + len]);
            at = start + len + 1;
        }
    }
}

/// The named-node kinds a pattern's source can root at: `(kind ...)` names
/// one, `[(a) (b)] ...` names each alternative's. Everything else — a
/// string token, `_`, `(_)`, a grouped sibling pattern — names none.
fn collect_root_kinds(pattern: &'static str, out: &mut HashSet<&'static str>) {
    let rest = skip_trivia(pattern);
    match rest.as_bytes().first() {
        Some(b'(') => {
            if let Some(kind) = leading_kind(&rest[1..]) {
                out.insert(kind);
            }
        }
        // An alternation at the root: each `(kind` inside it is a root.
        Some(b'[') => {
            let mut inner = skip_trivia(&rest[1..]);
            let mut depth = 0usize;
            while let Some(c) = inner.as_bytes().first() {
                match c {
                    b']' if depth == 0 => break,
                    b'(' if depth == 0 => {
                        if let Some(kind) = leading_kind(&inner[1..]) {
                            out.insert(kind);
                        }
                        depth += 1;
                    }
                    b'(' => depth += 1,
                    b')' => depth = depth.saturating_sub(1),
                    _ => {}
                }
                inner = skip_trivia(&inner[1..]);
            }
        }
        _ => {}
    }
}

/// The identifier at the head of `s`, when it is one — a node kind. `_`
/// (the wildcard) and `#` (a predicate) are not kinds.
fn leading_kind(s: &'static str) -> Option<&'static str> {
    let s = skip_trivia(s);
    let end = s
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(s.len());
    let head = &s[..end];
    (!head.is_empty() && head != "_" && !head.starts_with(|c: char| c.is_ascii_digit()))
        .then_some(head)
}

/// Whitespace and `;` comments at the head of a query source slice.
fn skip_trivia(mut s: &'static str) -> &'static str {
    loop {
        let trimmed = s.trim_start();
        if let Some(rest) = trimmed.strip_prefix(';') {
            s = rest.find('\n').map_or("", |i| &rest[i + 1..]);
            continue;
        }
        return trimmed;
    }
}

#[cfg(test)]
#[path = "../cursor_query_tests.rs"]
mod tests;
