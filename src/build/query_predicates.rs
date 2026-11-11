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

/// The most captures one pattern step holds: tree-sitter keeps the first
/// three and drops the rest without a word.
const MAX_STEP_CAPTURES: usize = 3;

/// A pattern still open around the scan: what its captures land on.
enum Open {
    /// The top level, or a predicate's own parentheses (skipped whole).
    Root,
    /// `(kind …)`: its own step, and whether its kind is read yet.
    Node { step: usize, named: bool },
    /// `((a) (b))`: the first child's first steps.
    Group { first: Option<Vec<usize>> },
    /// `[(a) (b)]`: every branch's first steps.
    Alternation { first: Vec<usize> },
}

/// Tree-sitter's capture-name characters past the first.
fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || "_-.?!".contains(c)
}

/// What a pattern step is, as the document writes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepShape {
    /// A token the document spells (`"::"`): an anonymous node.
    Spelled,
    /// A node the document names by kind (`(name)`) or leaves open (`_`).
    Node,
}

impl StepShape {
    pub fn described(self) -> &'static str {
        match self {
            StepShape::Spelled => "a token the document spells",
            StepShape::Node => "a node",
        }
    }
}

/// One capture as the document writes it: its byte offset, its name, and
/// the steps it lands on (several where it follows an alternation).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureSite<'s> {
    pub at: usize,
    pub name: &'s str,
    pub on: Vec<StepShape>,
    steps: Vec<usize>,
}

/// Every capture in `source`, in order, with the steps it lands on.
/// Tree-sitter files a pattern's captures on the pattern's first step (a
/// node's own; a group's first child's; each alternation branch's first),
/// so a group's capture stacks on the node inside it. The lexing is the
/// query parser's: a string is `"…"` with `\` escaping the next character,
/// `;` comments to the line's end between tokens, a capture name runs over
/// alphanumerics and `_-.?!`, and a parenthesis opening on `#` or `.` is a
/// predicate, whose `@` arguments name captures but add none. `None` when
/// the source does not balance (the compile reports that itself).
pub fn capture_sites(source: &str) -> Option<Vec<CaptureSite<'_>>> {
    let mut steps: Vec<StepShape> = Vec::new();
    let mut sites: Vec<CaptureSite<'_>> = Vec::new();
    let mut open: Vec<Open> = vec![Open::Root];
    // The first steps of the pattern a capture written now would follow.
    let mut last: Option<Vec<usize>> = None;
    let mut chars = source.char_indices().peekable();
    let skip_string = |chars: &mut std::iter::Peekable<std::str::CharIndices>| {
        while let Some((_, c)) = chars.next() {
            match c {
                '\\' => {
                    chars.next();
                }
                '"' => return,
                _ => {}
            }
        }
    };
    // A pattern completes: captures may follow it, and its enclosing group
    // or alternation learns its first steps.
    let complete = |first: Vec<usize>, open: &mut Vec<Open>, last: &mut Option<Vec<usize>>| {
        match open.last_mut() {
            Some(Open::Group { first: f @ None }) => *f = Some(first.clone()),
            Some(Open::Alternation { first: f }) => f.extend(first.iter().copied()),
            _ => {}
        }
        *last = Some(first);
    };
    while let Some((i, c)) = chars.next() {
        match c {
            c if c.is_whitespace() => {}
            ';' => while chars.next_if(|&(_, c)| c != '\n').is_some() {},
            '"' => {
                skip_string(&mut chars);
                steps.push(StepShape::Spelled);
                complete(vec![steps.len() - 1], &mut open, &mut last);
            }
            '@' => {
                let mut end = i + 1;
                while let Some((j, c)) = chars.next_if(|&(_, c)| is_name_char(c)) {
                    end = j + c.len_utf8();
                }
                let on: Vec<usize> = last.iter().flatten().copied().collect();
                sites.push(CaptureSite {
                    at: i,
                    name: &source[i + 1..end],
                    on: on.iter().map(|&s| steps[s]).collect(),
                    steps: on,
                });
            }
            '+' | '*' | '?' if last.is_some() => {}
            '(' => {
                last = None;
                let mut ahead = chars.clone();
                let next = loop {
                    match ahead.next() {
                        Some((_, c)) if c.is_whitespace() => {}
                        Some((_, ';')) => while ahead.next_if(|&(_, c)| c != '\n').is_some() {},
                        other => break other.map(|(_, c)| c),
                    }
                };
                match next {
                    Some('#' | '.') => {
                        let mut depth = 1;
                        while depth > 0 {
                            match chars.next() {
                                Some((_, '"')) => skip_string(&mut chars),
                                Some((_, ';')) => while chars.next_if(|&(_, c)| c != '\n').is_some() {},
                                Some((_, '(')) => depth += 1,
                                Some((_, ')')) => depth -= 1,
                                Some(_) => {}
                                None => return None,
                            }
                        }
                    }
                    Some('(' | '[' | '"') => open.push(Open::Group { first: None }),
                    _ => {
                        steps.push(StepShape::Node);
                        open.push(Open::Node { step: steps.len() - 1, named: false });
                    }
                }
            }
            '[' => {
                last = None;
                open.push(Open::Alternation { first: Vec::new() });
            }
            ')' | ']' => {
                last = None;
                let first = match open.pop() {
                    Some(Open::Node { step, .. }) => vec![step],
                    Some(Open::Group { first }) => first.unwrap_or_default(),
                    Some(Open::Alternation { first }) => first,
                    Some(Open::Root) | None => return None,
                };
                complete(first, &mut open, &mut last);
            }
            _ => {
                last = None;
                let mut word = String::from(c);
                while let Some((_, c)) = chars.next_if(|&(_, c)| !c.is_whitespace() && !"()[]\"@;".contains(c)) {
                    word.push(c);
                }
                match open.last_mut() {
                    Some(Open::Node { named: named @ false, .. }) => *named = true,
                    // A wildcard child is a step; a field name, negated
                    // field or anchor only prefixes or places one.
                    _ if word == "_" => {
                        steps.push(StepShape::Node);
                        complete(vec![steps.len() - 1], &mut open, &mut last);
                    }
                    _ => {}
                }
            }
        }
    }
    Some(sites)
}

/// The byte offset of the first capture in `source` that its step cannot
/// hold.
fn overloaded_capture(source: &str) -> Option<usize> {
    let mut held: std::collections::HashMap<usize, usize> = Default::default();
    for site in capture_sites(source)? {
        for &step in &site.steps {
            let n = held.entry(step).or_default();
            *n += 1;
            if *n > MAX_STEP_CAPTURES {
                return Some(site.at);
            }
        }
    }
    None
}

/// A query's source as tree-sitter will read it: refused when a step is
/// given more captures than it holds, written on it or stacked on it by an
/// enclosing group or alternation, so a document cannot lose a capture in
/// silence. Both runners check it beside `check`.
pub fn check_source(source: &str) -> Result<(), String> {
    match overloaded_capture(source) {
        Some(at) => Err(format!(
            "line {}: a node holds at most three captures; tree-sitter drops the rest",
            source[..at].matches('\n').count() + 1
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
