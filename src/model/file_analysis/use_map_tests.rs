use super::use_map::{QualifiedName, UseMap, UseRow};
use super::Span;

fn row(namespace: &str, leaf: &str, alias: Option<&str>) -> UseRow {
    UseRow {
        span: Span { start: tree_sitter::Point::new(0, 0), end: tree_sitter::Point::new(0, 0) },
        name: QualifiedName::new(namespace, leaf),
        alias: alias.map(str::to_string),
        reach: Span { start: tree_sitter::Point::new(0, 0), end: tree_sitter::Point::new(99, 0) },
    }
}

fn map<'a>(rows: &'a [UseRow], own: Option<&'a str>) -> UseMap<'a> {
    UseMap { rows, class_body: None, own_namespace: own, sep: "\\", site: Some(tree_sitter::Point::new(1, 0)) }
}

fn fqn(m: &UseMap<'_>, written: &str) -> String {
    m.resolve(written).identity(m.sep)
}

#[test]
fn absolute_spelling_is_its_own_identity() {
    let m = map(&[], Some("App"));
    assert_eq!(fqn(&m, "\\Vendor\\Thing"), "Vendor\\Thing");
    assert_eq!(fqn(&m, "\\Exception"), "Exception");
}

#[test]
fn bare_leaf_resolves_in_own_namespace_else_globally() {
    let m = map(&[], Some("App\\Models"));
    assert_eq!(fqn(&m, "User"), "App\\Models\\User");
    let g = map(&[], None);
    assert_eq!(fqn(&g, "User"), "User");
}

#[test]
fn use_row_binds_its_leaf_and_carries_a_tail() {
    let rows = vec![row("GuzzleHttp", "Psr7", None), row("", "Exception", None)];
    let m = map(&rows, Some("App"));
    assert_eq!(fqn(&m, "Psr7"), "GuzzleHttp\\Psr7");
    assert_eq!(fqn(&m, "Psr7\\Utils"), "GuzzleHttp\\Psr7\\Utils");
    assert_eq!(fqn(&m, "Exception"), "Exception");
}

#[test]
fn alias_wins_over_row_and_own_namespace() {
    let rows = vec![row("GuzzleHttp", "Promise", Some("P"))];
    let m = map(&rows, Some("App"));
    assert_eq!(fqn(&m, "P"), "GuzzleHttp\\Promise");
    assert_eq!(fqn(&m, "P\\Promise"), "GuzzleHttp\\Promise\\Promise");
    // the real leaf of an aliased import is NOT bound by the alias row:
    // `Promise` is whatever else names it, i.e. the file's own namespace
    assert_eq!(fqn(&m, "Promise"), "App\\Promise");
}

#[test]
fn an_aliased_row_binds_its_alias_never_its_leaf() {
    let rows = vec![row("B", "Event", Some("ScriptEvent"))];
    let m = map(&rows, Some("A"));
    assert_eq!(fqn(&m, "ScriptEvent"), "B\\Event");
    assert_eq!(fqn(&m, "Event"), "A\\Event");
}

/// A row is aliased or not on its own: importing a class under an alias
/// does not unbind a second, bare import of the same class.
#[test]
fn a_bare_row_beside_an_aliased_one_still_binds_its_leaf() {
    let rows = vec![row("B", "Event", Some("E")), row("B", "Event", None)];
    let m = map(&rows, Some("A"));
    assert_eq!(fqn(&m, "E"), "B\\Event");
    assert_eq!(fqn(&m, "Event"), "B\\Event");
}

#[test]
fn resolve_answers_parts_with_the_global_namespace_empty() {
    let m = map(&[], None);
    assert_eq!(m.resolve("Exception"), QualifiedName::new("", "Exception"));
    assert_eq!(m.resolve("\\Exception"), QualifiedName::new("", "Exception"));
    let n = map(&[], Some("A\\B"));
    assert_eq!(n.resolve("C"), QualifiedName::new("A\\B", "C"));
    let rows = vec![row("GuzzleHttp", "Psr7", None)];
    let r = map(&rows, Some("App"));
    assert_eq!(r.resolve("Psr7\\Utils"), QualifiedName::new("GuzzleHttp\\Psr7", "Utils"));
}

#[test]
fn resolve_in_takes_the_qualifier_as_written() {
    let rows = vec![row("GuzzleHttp", "Promise", Some("P"))];
    let m = map(&rows, Some("App"));
    assert_eq!(m.resolve_in("P", "Each"), QualifiedName::new("GuzzleHttp\\Promise", "Each"));
    assert_eq!(m.resolve_in("Sub\\Deeper", "X"), QualifiedName::new("App\\Sub\\Deeper", "X"));
    assert_eq!(m.resolve_in("\\Vendor", "X"), QualifiedName::new("Vendor", "X"));
    assert_eq!(m.resolve_in("", "X"), QualifiedName::new("", "X"));
}

/// `resolve` takes a WRITTEN spelling. It is not idempotent: an identity
/// whose head segment collides with an import alias would be re-qualified
/// on a second pass, so a consumer must never feed it an FQN it already
/// holds (`FileAnalysis::class_spelling_identity`'s contract).
#[test]
fn resolve_is_not_idempotent() {
    let rows = vec![row("Vendor", "App", Some("App"))];
    let m = map(&rows, Some("Site"));
    let once = fqn(&m, "App\\Models\\User");
    assert_eq!(once, "Vendor\\App\\Models\\User");
    let twice = fqn(&m, &once);
    assert_eq!(twice, "Site\\Vendor\\App\\Models\\User", "a second pass re-qualifies");
    assert_ne!(once, twice);
}

/// A row binds only inside its reach: a site past it resolves the leaf as
/// if the row were never written.
#[test]
fn a_row_binds_only_where_it_reaches() {
    let mut r = row("Lib", "Model", None);
    r.reach = Span { start: tree_sitter::Point::new(2, 0), end: tree_sitter::Point::new(5, 0) };
    let rows = vec![r];
    let at = |row: usize| UseMap { rows: &rows, class_body: None, own_namespace: Some("App"), sep: "\\", site: Some(tree_sitter::Point::new(row, 4)) };
    assert_eq!(fqn(&at(3), "Model"), "Lib\\Model");
    assert_eq!(fqn(&at(7), "Model"), "App\\Model");
    // the whole-file view sees every row
    let whole = UseMap { site: None, ..at(7) };
    assert_eq!(fqn(&whole, "Model"), "Lib\\Model");
}
