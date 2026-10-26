//! Structural query predicates: Neovim's `#has-parent?` / `#has-ancestor?`
//! family, which core tree-sitter does not evaluate. The binding hands every
//! predicate it doesn't know to the caller unevaluated
//! (`Query::general_predicates`), so a runner that never reads them treats a
//! filter as a no-op. Both query runners (pack extraction, Perl plugin
//! patterns) check their queries with `check` when they compile and filter
//! each match through `admits`. `docs/adr/capture-vocabulary.md`.

use tree_sitter::{Language, Node, Query, QueryMatch, QueryPredicate, QueryPredicateArg};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Relation {
    /// The captured node's parent.
    Parent,
    /// Any proper ancestor of the captured node.
    Ancestor,
}

/// One structural predicate: `(#[not-]has-<relation>? @cap kind ...)`.
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

/// A predicate that is no filter: a match carrying one keeps all its facts,
/// and the ones it mints become a reading decided at query time
/// (`docs/adr/query-delayed-facts.md`). Each runner accepts the ones it
/// knows how to mint (`check`'s `deferred`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Deferral {
    /// `#receiver-isa? @recv "Class"`: the receiver descends from the class.
    ReceiverIsa,
    /// `#names-class? @name`: the name is a class's, so the call constructs it.
    NamesClass,
}

impl Deferral {
    pub const fn spelling(self) -> &'static str {
        match self {
            Deferral::ReceiverIsa => "receiver-isa?",
            Deferral::NamesClass => "names-class?",
        }
    }

    /// The deferrals pattern `pattern` carries.
    pub fn of_pattern(query: &Query, pattern: usize) -> Vec<Deferral> {
        let all = [Deferral::ReceiverIsa, Deferral::NamesClass];
        query
            .general_predicates(pattern)
            .iter()
            .filter_map(|p| all.into_iter().find(|d| d.spelling() == &*p.operator))
            .collect()
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
/// match passes. `root` is the node the cursor walked: ancestry is read by
/// descending from it, because `Node::parent` also descends from the root
/// and would repeat that walk per hop.
pub fn admits(query: &Query, m: &QueryMatch<'_, '_>, root: Node<'_>) -> bool {
    let preds = query.general_predicates(m.pattern_index);
    if preds.is_empty() {
        return true;
    }
    preds.iter().filter_map(parse).filter_map(Result::ok).all(|s| {
        m.captures
            .iter()
            .filter(|c| c.index == s.capture)
            .all(|c| holds(&s, c.node, root) != s.negated)
    })
}

fn holds(s: &Structural<'_>, node: Node<'_>, root: Node<'_>) -> bool {
    let mut cur = root;
    let mut parent = None;
    let mut ancestor_hit = false;
    while cur != node {
        if s.kinds.contains(&cur.kind()) {
            ancestor_hit = true;
        }
        parent = Some(cur);
        match cur.child_with_descendant(node) {
            Some(next) => cur = next,
            None => return false,
        }
    }
    match s.relation {
        Relation::Parent => parent.is_some_and(|p| s.kinds.contains(&p.kind())),
        Relation::Ancestor => ancestor_hit,
    }
}

#[cfg(test)]
#[path = "query_predicates_tests.rs"]
mod tests;
