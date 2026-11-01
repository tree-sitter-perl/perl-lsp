//! Structural query predicates: Neovim's `#has-parent?` / `#has-ancestor?`
//! family, which core tree-sitter does not evaluate. The binding hands every
//! predicate it doesn't know to the caller unevaluated
//! (`Query::general_predicates`), so a runner that never reads them treats a
//! filter as a no-op. Both query runners (pack extraction, Perl plugin
//! patterns) check their queries with `check` and `check_source` when they
//! compile and filter each match through `admits`. `docs/adr/capture-vocabulary.md`.

use tree_sitter::{Language, Node, Query, QueryMatch, QueryPredicate, QueryPredicateArg};

use crate::cst::SyntaxForest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Relation {
    /// The captured node's parent.
    Parent,
    /// Any proper ancestor of the captured node.
    Ancestor,
    /// The captured node itself.
    Node,
}

/// One structural predicate: `(#[not-]has-<relation>? @cap kind ...)`, or
/// `(#[not-]is-kind? @cap kind ...)` on the captured node itself.
struct Structural<'q> {
    relation: Relation,
    negated: bool,
    capture: u32,
    kinds: Vec<&'q str>,
}

fn relation_of(operator: &str) -> Option<(Relation, bool)> {
    Some(match operator {
        "has-parent?" => (Relation::Parent, false),
        "not-has-parent?" => (Relation::Parent, true),
        "has-ancestor?" => (Relation::Ancestor, false),
        "not-has-ancestor?" => (Relation::Ancestor, true),
        "is-kind?" => (Relation::Node, false),
        "not-is-kind?" => (Relation::Node, true),
        _ => return None,
    })
}

/// `None` when the predicate is not structural; `Some(Err)` when it is but
/// is malformed.
fn parse(p: &QueryPredicate) -> Option<Result<Structural<'_>, String>> {
    let (relation, negated) = relation_of(&p.operator)?;
    let shape = || format!("#{} needs one capture and at least one node kind", p.operator);
    let parsed = match &*p.args {
        [QueryPredicateArg::Capture(capture), kinds @ ..] if !kinds.is_empty() => kinds
            .iter()
            .map(|a| match a {
                QueryPredicateArg::String(k) => Ok(&**k),
                QueryPredicateArg::Capture(_) => Err(shape()),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(|kinds| Structural { relation, negated, capture: *capture, kinds }),
        _ => Err(shape()),
    };
    Some(parsed)
}

/// The most captures one pattern node holds: tree-sitter keeps the first
/// three and drops the rest without a word.
const MAX_NODE_CAPTURES: usize = 3;

/// The byte offset of the first node in `source` written with more
/// captures than a node holds.
fn overloaded_capture_node(source: &str) -> Option<usize> {
    let (mut run, mut at, mut in_string) = (0, 0, false);
    let mut chars = source.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if in_string {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                run = 0;
            }
            ';' => {
                while chars.next_if(|&(_, c)| c != '\n').is_some() {}
            }
            '@' => {
                if run == 0 {
                    at = i;
                }
                run += 1;
                if run > MAX_NODE_CAPTURES {
                    return Some(at);
                }
                while chars.next_if(|&(_, c)| !c.is_whitespace() && !"()[]".contains(c)).is_some() {}
            }
            c if c.is_whitespace() => {}
            _ => run = 0,
        }
    }
    None
}

/// A query's source as tree-sitter will read it: refused when a node is
/// written with more captures than it holds, so a document cannot lose a
/// capture in silence. Both runners check it beside `check`.
pub fn check_source(source: &str) -> Result<(), String> {
    match overloaded_capture_node(source) {
        Some(at) => Err(format!(
            "line {}: a node holds at most three captures; tree-sitter drops the rest",
            source[..at].lines().count()
        )),
        None => Ok(()),
    }
}

/// Every general predicate in `query` is structural and well-formed (each
/// kind a named kind of `language`), or one the caller evaluates itself
/// (`deferred`). Anything else is a misspelling that would silently match
/// everything, so it fails the compile.
pub fn check(query: &Query, language: &Language, deferred: &[&str]) -> Result<(), String> {
    for i in 0..query.pattern_count() {
        for p in query.general_predicates(i) {
            match parse(p) {
                Some(Ok(s)) => {
                    if let Some(k) = s.kinds.iter().find(|k| language.id_for_node_kind(k, true) == 0) {
                        return Err(format!("#{}: `{k}` is not a node kind of this grammar", p.operator));
                    }
                }
                Some(Err(e)) => return Err(e),
                None if deferred.contains(&&*p.operator) => {}
                None => return Err(format!("unknown predicate #{}", p.operator)),
            }
        }
    }
    Ok(())
}

/// Whether `m` satisfies its pattern's structural predicates. Every node the
/// predicate's capture holds must satisfy it; a capture with no node in the
/// match passes. Ancestry is read off the parse's [`SyntaxForest`], which
/// `syntax` hands over only when a predicate needs it, so a pattern without
/// structural predicates never pays for the tree.
pub fn admits<'s, 't: 's>(
    query: &Query,
    m: &QueryMatch<'_, 't>,
    syntax: impl FnOnce() -> &'s SyntaxForest<'t>,
) -> bool {
    let preds = query.general_predicates(m.pattern_index);
    if preds.is_empty() {
        return true;
    }
    let syn = syntax();
    preds.iter().filter_map(parse).filter_map(Result::ok).all(|s| {
        m.captures
            .iter()
            .filter(|c| c.index == s.capture)
            .all(|c| holds(&s, c.node, syn) != s.negated)
    })
}

fn holds<'t>(s: &Structural<'_>, node: Node<'t>, syn: &SyntaxForest<'t>) -> bool {
    match s.relation {
        Relation::Node => s.kinds.contains(&node.kind()),
        Relation::Parent => syn.parent(node).is_some_and(|p| s.kinds.contains(&p.kind())),
        Relation::Ancestor => syn.ancestors(node).any(|a| s.kinds.contains(&a.kind())),
    }
}

#[cfg(test)]
#[path = "query_predicates_tests.rs"]
mod tests;
