use super::use_map::UseMap;
use super::{ImportBinds, ImportRow, Span};

fn row(raw: &str) -> ImportRow {
    ImportRow {
        span: Span { start: tree_sitter::Point::new(0, 0), end: tree_sitter::Point::new(0, 0) },
        raw: raw.to_string(),
        binds: ImportBinds::Type,
    }
}

fn map<'a>(
    rows: &'a [ImportRow],
    aliases: &'a [(String, String, String)],
    own: Option<&'a str>,
) -> UseMap<'a> {
    UseMap { rows, aliases, own_namespace: own, sep: "\\" }
}

#[test]
fn absolute_spelling_is_its_own_identity() {
    let m = map(&[], &[], Some("App"));
    assert_eq!(m.resolve("\\Vendor\\Thing"), "Vendor\\Thing");
    assert_eq!(m.resolve("\\Exception"), "Exception");
}

#[test]
fn bare_leaf_resolves_in_own_namespace_else_globally() {
    let m = map(&[], &[], Some("App\\Models"));
    assert_eq!(m.resolve("User"), "App\\Models\\User");
    let g = map(&[], &[], None);
    assert_eq!(g.resolve("User"), "User");
}

#[test]
fn use_row_binds_its_leaf_and_carries_a_tail() {
    let rows = vec![row("GuzzleHttp\\Psr7"), row("\\Exception")];
    let m = map(&rows, &[], Some("App"));
    assert_eq!(m.resolve("Psr7"), "GuzzleHttp\\Psr7");
    assert_eq!(m.resolve("Psr7\\Utils"), "GuzzleHttp\\Psr7\\Utils");
    assert_eq!(m.resolve("Exception"), "Exception");
}

#[test]
fn alias_wins_over_row_and_own_namespace() {
    let rows = vec![row("GuzzleHttp\\Promise")];
    let aliases = vec![("P".to_string(), "GuzzleHttp".to_string(), "Promise".to_string())];
    let m = map(&rows, &aliases, Some("App"));
    assert_eq!(m.resolve("P"), "GuzzleHttp\\Promise");
    assert_eq!(m.resolve("P\\Promise"), "GuzzleHttp\\Promise\\Promise");
    // the real leaf of an aliased import is NOT bound by the alias row:
    // `Promise` is whatever else names it, i.e. the file's own namespace
    assert_eq!(m.resolve("Promise"), "App\\Promise");
}

#[test]
fn an_aliased_row_binds_its_alias_never_its_leaf() {
    // the row is in `rows` too (every import row is), yet `Event`
    // means the file's own class, not the aliased import
    let rows = vec![row("B\\Event")];
    let aliases = vec![("ScriptEvent".to_string(), "B".to_string(), "Event".to_string())];
    let m = map(&rows, &aliases, Some("A"));
    assert_eq!(m.resolve("ScriptEvent"), "B\\Event");
    assert_eq!(m.resolve("Event"), "A\\Event");
}

#[test]
fn aliased_leaf_alone_falls_to_own_namespace() {
    let aliases =
        vec![("BaseCollection".to_string(), "Support".to_string(), "Collection".to_string())];
    let m = map(&[], &aliases, Some("App"));
    assert_eq!(m.resolve("BaseCollection"), "Support\\Collection");
    assert_eq!(m.resolve("Collection"), "App\\Collection");
}

#[test]
fn split_keeps_the_global_namespace_empty() {
    let m = map(&[], &[], None);
    assert_eq!(m.resolve_split("Exception"), ("".to_string(), "Exception".to_string()));
    let n = map(&[], &[], Some("A\\B"));
    assert_eq!(n.resolve_split("C"), ("A\\B".to_string(), "C".to_string()));
}

/// `resolve` takes a WRITTEN spelling. It is not idempotent: an identity
/// whose head segment collides with an import alias would be re-qualified
/// on a second pass, so a consumer must never feed it an FQN it already
/// holds (`FileAnalysis::class_spelling_identity`'s contract).
#[test]
fn resolve_is_not_idempotent() {
    let aliases = vec![("App".to_string(), "Vendor".to_string(), "App".to_string())];
    let m = map(&[], &aliases, Some("Site"));
    let once = m.resolve("App\\Models\\User");
    assert_eq!(once, "Vendor\\App\\Models\\User");
    let twice = m.resolve(&once);
    assert_eq!(twice, "Site\\Vendor\\App\\Models\\User", "a second pass re-qualifies");
    assert_ne!(once, twice);
}
