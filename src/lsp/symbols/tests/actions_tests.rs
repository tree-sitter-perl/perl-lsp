//! Code actions (auto-import), unimported/qualified completion, doc-symbol + hover basics.

use super::*;

#[test]
fn test_code_action_from_diagnostic() {
    let source = "use Carp qw(croak);\ncarp('oops');\n";
    let analysis = parse_analysis(source);
    let uri = Url::parse("file:///test.pl").unwrap();

    // Simulate a HINT diagnostic with data (as collect_diagnostics would produce
    // if module_index had resolved Carp)
    let diag = Diagnostic {
        range: Range {
            start: Position {
                line: 1,
                character: 0,
            },
            end: Position {
                line: 1,
                character: 4,
            },
        },
        severity: Some(DiagnosticSeverity::HINT),
        code: Some(NumberOrString::String("unresolved-function".into())),
        source: Some("perl-lsp".into()),
        message: "'carp' is exported by Carp but not imported".into(),
        data: Some(serde_json::json!({"module": "Carp", "function": "carp"})),
        ..Default::default()
    };

    let actions = code_actions(&[diag], &analysis, "", &uri);
    assert_eq!(actions.len(), 1);
    if let CodeActionOrCommand::CodeAction(action) = &actions[0] {
        assert_eq!(action.title, "Import 'carp' from Carp");
        assert_eq!(action.kind, Some(CodeActionKind::QUICKFIX));
        assert_eq!(action.is_preferred, Some(true));

        // Verify the edit inserts " carp" at the qw close paren
        let edit = action.edit.as_ref().unwrap();
        let changes = edit.changes.as_ref().unwrap();
        let text_edits = changes.get(&uri).unwrap();
        assert_eq!(text_edits.len(), 1);
        assert_eq!(text_edits[0].new_text, " carp");
    } else {
        panic!("Expected CodeAction, got Command");
    }
}

#[test]
fn test_code_action_new_use_statement() {
    let source = "use strict;\nuse warnings;\nfrobnicate();\n";
    let analysis = parse_analysis(source);
    let uri = Url::parse("file:///test.pl").unwrap();

    let diag = Diagnostic {
        range: Range {
            start: Position {
                line: 2,
                character: 0,
            },
            end: Position {
                line: 2,
                character: 11,
            },
        },
        severity: Some(DiagnosticSeverity::HINT),
        code: Some(NumberOrString::String("unresolved-function".into())),
        source: Some("perl-lsp".into()),
        message: "'frobnicate' is exported by Some::Module (not yet imported)".into(),
        data: Some(serde_json::json!({
            "modules": ["Some::Module"],
            "function": "frobnicate",
        })),
        ..Default::default()
    };

    let actions = code_actions(&[diag], &analysis, "", &uri);
    assert_eq!(actions.len(), 1);
    if let CodeActionOrCommand::CodeAction(action) = &actions[0] {
        assert_eq!(action.title, "Add 'use Some::Module qw(frobnicate)'");
        assert_eq!(action.is_preferred, Some(true));
        let edit = action.edit.as_ref().unwrap();
        let changes = edit.changes.as_ref().unwrap();
        let text_edits = changes.get(&uri).unwrap();
        assert_eq!(text_edits[0].new_text, "use Some::Module qw(frobnicate);\n");
        // Inserted after last use statement (line 2)
        assert_eq!(text_edits[0].range.start.line, 2);
    } else {
        panic!("Expected CodeAction");
    }
}

#[test]
fn test_unimported_completion_with_auto_import() {
    let source = "use strict;\nuse warnings;\n\nfir\n";
    let analysis = parse_analysis(source);

    // Simulate a cached module that exports "first"
    let idx = ModuleIndex::new_for_test();
    idx.set_workspace_root(None);
    // Insert directly into cache for testing
    idx.insert_cache(
        "List::Util",
        Some(fake_cached(
            "/usr/lib/perl5/List/Util.pm",
            &[],
            &["first", "max", "min"],
        )),
    );

    let tree = crate::index::document::Document::new(source.to_string())
        .unwrap()
        .tree;
    let items = completion_items_for_test(
        &analysis,
        &tree,
        source,
        Position {
            line: 3,
            character: 3,
        },
        &idx,
        None,
    );

    // Should find "first" from List::Util
    let first_item = items.iter().find(|i| i.label == "first");
    assert!(
        first_item.is_some(),
        "Should offer 'first' from unimported List::Util"
    );

    let first_item = first_item.unwrap();
    assert!(
        first_item.detail.as_ref().unwrap().contains("List::Util"),
        "Detail should mention the module"
    );
    assert!(
        first_item.detail.as_ref().unwrap().contains("auto-import"),
        "Detail should indicate auto-import"
    );

    // Should have additional text edit inserting `use List::Util qw(first);`
    let edits = first_item.additional_text_edits.as_ref().unwrap();
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].new_text, "use List::Util qw(first);\n");
    // Should insert after the last use statement (line 2)
    assert_eq!(edits[0].range.start.line, 2);
}

#[test]
fn test_unimported_completion_skips_imported_modules() {
    // List::Util is already imported — its exports should NOT appear as unimported completions
    let source = "use List::Util qw(max);\nfir\n";
    let analysis = parse_analysis(source);

    let idx = ModuleIndex::new_for_test();
    idx.set_workspace_root(None);
    idx.insert_cache(
        "List::Util",
        Some(fake_cached(
            "/usr/lib/perl5/List/Util.pm",
            &[],
            &["first", "max", "min"],
        )),
    );
    idx.insert_cache(
        "Scalar::Util",
        Some(fake_cached(
            "/usr/lib/perl5/Scalar/Util.pm",
            &[],
            &["blessed", "reftype"],
        )),
    );

    let tree = crate::index::document::Document::new(source.to_string())
        .unwrap()
        .tree;
    let items = completion_items_for_test(
        &analysis,
        &tree,
        source,
        Position {
            line: 1,
            character: 3,
        },
        &idx,
        None,
    );

    // "first" should appear via imported_function_completions (auto-add to qw),
    // NOT via unimported_function_completions
    let first_items: Vec<_> = items.iter().filter(|i| i.label == "first").collect();
    assert!(!first_items.is_empty(), "Should offer 'first'");
    // It should come from the imported path (adds to qw) not unimported
    for item in &first_items {
        if let Some(ref detail) = item.detail {
            assert!(
                !detail.contains("auto-import") || detail.contains("List::Util"),
                "first should come from List::Util context"
            );
        }
    }

    // "blessed" should appear as unimported (Scalar::Util not imported)
    let blessed_item = items.iter().find(|i| i.label == "blessed");
    assert!(
        blessed_item.is_some(),
        "Should offer 'blessed' from unimported Scalar::Util"
    );
    let blessed_item = blessed_item.unwrap();
    assert!(blessed_item
        .detail
        .as_ref()
        .unwrap()
        .contains("Scalar::Util"));
    let edits = blessed_item.additional_text_edits.as_ref().unwrap();
    assert!(edits[0].new_text.contains("use Scalar::Util qw(blessed)"));
}

/// Regression: typing `Package::` should NOT trigger the global
/// workspace-symbol firehose. Mirrors the EM gold corpus fixture
/// `completion_package_colon` (the pre-fix flood was 263 items); the
/// fix narrows to the package's own subs.
///
/// Multi-segment package name (`Math::Util`) on purpose — earlier
/// drafts narrowed only single-segment names, so this case is the
/// load-bearing assertion. Fixture also threads a `use constant` +
/// const-folded call site so the case is realistic Perl and the
/// constant doesn't get mistaken for a package qualifier.
#[test]
fn test_qualified_path_completion_narrows_to_package() {
    let source = "\
package Math::Util;
use constant PI => 3.14159;
sub square    { my ($n) = @_; $n * $n }
sub cube      { my ($n) = @_; $n * $n * $n }
sub circle_area {
    my ($r) = @_;
    return PI * $r * $r;           # const-folded arg flows through
}
package main;
use constant TAU => Math::Util::PI() * 2;
my $sq   = Math::Util::s
";
    let analysis = parse_analysis(source);
    let module_index = ModuleIndex::new_for_test();
    module_index.set_workspace_root(None);
    // Seed an unrelated cross-file module so the workspace flood
    // would include it if the firehose were still firing.
    module_index.insert_cache(
        "Scalar::Util",
        Some(fake_cached(
            "/usr/lib/perl5/Scalar/Util.pm",
            &[],
            &["blessed", "reftype"],
        )),
    );

    let tree = crate::index::document::Document::new(source.to_string())
        .unwrap()
        .tree;
    // Cursor at end of `Math::Util::s` on line 10 (0-indexed).
    // Line: `my $sq   = Math::Util::s` — cursor sits past the `s`.
    let line_text = source.lines().nth(10).unwrap();
    let cursor_col = line_text.len() as u32;
    let items = completion_items_for_test(
        &analysis,
        &tree,
        source,
        Position { line: 10, character: cursor_col },
        &module_index,
        None,
    );

    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"square"),
        "completion after `Math::Util::` should include `square`, got {:?}",
        labels,
    );
    assert!(
        labels.contains(&"cube"),
        "completion after `Math::Util::` should include `cube`, got {:?}",
        labels,
    );
    assert!(
        labels.contains(&"circle_area"),
        "completion after `Math::Util::` should include `circle_area` \
         (the const-folded sub), got {:?}",
        labels,
    );
    assert!(
        !labels.contains(&"blessed"),
        "completion after `Math::Util::` must NOT flood unrelated workspace \
         symbols (`blessed` is from Scalar::Util), got {:?}",
        labels,
    );
    // Tight bound — the 263-item flood is the bug we're regressing.
    // Allow some headroom for inherited / framework-synthesized
    // members but flag anything that grows past ~10× the package
    // size as a regression of the narrowing.
    assert!(
        items.len() <= 20,
        "completion after `Math::Util::` should narrow tightly to the package; \
         got {} items: {:?}",
        items.len(),
        labels,
    );
}

/// `Foo::<cursor>` should also offer the *sub-packages* nested
/// underneath, not just the methods on `Foo` itself. Typing `Mojo::`
/// expects to see `Util`, `Base`, `IOLoop` etc. alongside any methods
/// `Mojo` directly carries. Covers two sources of sub-packages —
/// in-file `package Foo::Bar` declarations AND cross-file modules
/// known to the resolver index — since either can be the right
/// answer in a real project.
#[test]
fn test_qualified_path_completion_offers_sub_packages() {
    let source = "\
package Math::Util;
sub square { my ($n) = @_; $n * $n }
package Math::Helpers;     # in-file sub-package, no module index entry
sub clamp { my ($x, $lo, $hi) = @_; $x }
package main;
my $x = Math::
";
    let analysis = parse_analysis(source);
    let module_index = ModuleIndex::new_for_test();
    module_index.set_workspace_root(None);
    // Cross-file sub-package: known via the workspace index, mirrors
    // the real "every package in its own .pm" project layout.
    module_index.insert_cache(
        "Math::Stats",
        Some(fake_cached("/usr/lib/perl5/Math/Stats.pm", &[], &["mean", "stddev"])),
    );
    // Unrelated module — must not bleed into Math:: results.
    module_index.insert_cache(
        "Scalar::Util",
        Some(fake_cached(
            "/usr/lib/perl5/Scalar/Util.pm",
            &[],
            &["blessed", "reftype"],
        )),
    );

    let tree = crate::index::document::Document::new(source.to_string())
        .unwrap()
        .tree;
    // Cursor at end of `Math::` on the last source line.
    let last_line_idx = source.lines().count() as u32 - 1;
    let line_text = source.lines().last().unwrap();
    let items = completion_items_for_test(
        &analysis,
        &tree,
        source,
        Position { line: last_line_idx, character: line_text.len() as u32 },
        &module_index,
        None,
    );

    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"Util"),
        "`Math::` should offer in-file sub-package `Util`, got {:?}",
        labels,
    );
    assert!(
        labels.contains(&"Helpers"),
        "`Math::` should offer in-file sub-package `Helpers`, got {:?}",
        labels,
    );
    assert!(
        labels.contains(&"Stats"),
        "`Math::` should offer cross-file sub-package `Stats`, got {:?}",
        labels,
    );
    assert!(
        !labels.contains(&"Scalar::Util") && !labels.contains(&"blessed"),
        "`Math::` must NOT bleed unrelated workspace symbols, got {:?}",
        labels,
    );
    // Sub-packages carry SymbolKind::MODULE, subs carry FUNCTION —
    // sanity-check the kind so clients pick the right icon.
    for item in &items {
        if matches!(item.label.as_str(), "Util" | "Helpers" | "Stats") {
            assert_eq!(
                item.kind,
                Some(tower_lsp::lsp_types::CompletionItemKind::MODULE),
                "sub-package `{}` should be SymbolKind::MODULE",
                item.label,
            );
        }
    }
}

/// Sibling case: the *package name itself* arrives via const-fold.
/// `my $pkg = 'Math::Util';` should let `$pkg->squ<cursor>` narrow
/// to `Math::Util`'s methods — but reaching that end-state needs the
/// witness bag to upgrade `$pkg` from `String` to `ClassName` when
/// it's used as a method invocant. That's a separate inference gap
/// (the QualifiedPath narrowing this PR ships handles the literal
/// `Foo::Bar::sub` syntactic form only). Marking ignored so the gap
/// is tracked rather than absorbed into this PR.
#[test]
#[ignore = "needs ClassName-from-string-invocant inference; tracked separately"]
fn test_const_folded_package_resolves_for_method_completion() {
    let source = "\
package Math::Util;
sub square    { my ($n) = @_; $n * $n }
sub cube      { my ($n) = @_; $n * $n * $n }
package main;
my $pkg = 'Math::Util';
$pkg->squ
";
    let analysis = parse_analysis(source);
    let module_index = ModuleIndex::new_for_test();
    module_index.set_workspace_root(None);

    let tree = crate::index::document::Document::new(source.to_string())
        .unwrap()
        .tree;
    // Cursor sits past the `q` in `$pkg->squ`.
    let line_text = source.lines().nth(5).unwrap();
    let cursor_col = line_text.len() as u32;
    let items = completion_items_for_test(
        &analysis,
        &tree,
        source,
        Position { line: 5, character: cursor_col },
        &module_index,
        None,
    );

    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(
        labels.contains(&"square"),
        "method completion on const-folded `$pkg` (= 'Math::Util') \
         should offer `square`, got {:?}",
        labels,
    );
    assert!(
        labels.contains(&"cube"),
        "method completion on const-folded `$pkg` (= 'Math::Util') \
         should offer `cube`, got {:?}",
        labels,
    );
}

/// Native subs emit `DocumentSymbol.name` as the bare identifier —
/// the LSP `kind` enum carries the Function/Method distinction.
/// Pre-fix we stuffed `<sub> ` into the name, which collided with
/// the EM gold corpus' protocol-correct assertions.
#[test]
fn test_document_symbol_name_is_bare_identifier() {
    let source = "\
package Demo::Symbols;
sub alpha { return 1 }
sub beta  { my ($x) = @_; $x * 2 }
1;
";
    let analysis = parse_analysis(source);
    let names: Vec<String> = analysis
        .document_symbols()
        .iter()
        .flat_map(|sym| {
            let mut acc = vec![sym.name.clone()];
            for c in &sym.children {
                acc.push(c.name.clone());
            }
            acc
        })
        .collect();
    assert!(
        names.iter().any(|n| n == "alpha"),
        "expected bare 'alpha' in document symbols, got {:?}",
        names,
    );
    assert!(
        names.iter().any(|n| n == "beta"),
        "expected bare 'beta' in document symbols, got {:?}",
        names,
    );
    for n in &names {
        assert!(
            !n.starts_with("<sub>") && !n.starts_with("<method>"),
            "DocumentSymbol.name should not carry `<sub>`/`<method>` prefix (got {:?})",
            n,
        );
    }
}

/// Hover on a Perl builtin returns the seeded perlfunc.pod entry.
/// The full POD parse pipeline is exercised separately in the
/// builtins_pod unit tests; here we just confirm the wiring from
/// perl_hover → module_index.builtin_doc fires for builtin names.
#[test]
fn test_hover_on_builtin_uses_module_index() {
    let source = "push @items, 4;\n";
    let analysis = parse_analysis(source);
    let module_index = ModuleIndex::new_for_test();
    module_index.set_workspace_root(None);
    module_index.seed_builtin_for_test(
        "push",
        "```perl\npush ARRAY,LIST\n```\n\nAppends LIST to ARRAY.",
    );

    let _tree = crate::index::document::Document::new(source.to_string())
        .unwrap()
        .tree;
    let files = crate::index::file_store::FileStore::new();
    let idx: &dyn crate::model::file_analysis::CrossFileLookup = &module_index;
    let cs = crate::index::resolve::resolve(
        &files,
        &analysis,
        crate::index::file_store::FileKey::Url(Url::parse("file:///test.pl").unwrap()),
        Point::new(0, 0),
        Some(idx),
        crate::index::resolve::OverrideScope::default(),
    )
    .with_source(source);
    let hover = perl_hover(&cs, &module_index).expect("expected hover on `push`");
    let text = match hover.contents {
        HoverContents::Markup(m) => m.value,
        _ => panic!("expected markdown hover"),
    };
    assert!(text.contains("push ARRAY,LIST"), "hover body missing: {text}");
    assert!(text.contains("Appends LIST"), "hover body missing: {text}");
}

#[test]
fn test_code_action_multiple_exporters_not_preferred() {
    let source = "use strict;\nfirst();\n";
    let analysis = parse_analysis(source);
    let uri = Url::parse("file:///test.pl").unwrap();

    let diag = Diagnostic {
        range: Range {
            start: Position {
                line: 1,
                character: 0,
            },
            end: Position {
                line: 1,
                character: 5,
            },
        },
        severity: Some(DiagnosticSeverity::HINT),
        code: Some(NumberOrString::String("unresolved-function".into())),
        source: Some("perl-lsp".into()),
        message: "...".into(),
        data: Some(serde_json::json!({
            "modules": ["List::Util", "List::MoreUtils"],
            "function": "first",
        })),
        ..Default::default()
    };

    let actions = code_actions(&[diag], &analysis, "", &uri);
    assert_eq!(actions.len(), 2);
    // Neither should be preferred (ambiguous)
    for action in &actions {
        if let CodeActionOrCommand::CodeAction(a) = action {
            assert_eq!(a.is_preferred, Some(false));
        }
    }
}

#[test]
fn lexical_subs_complete_only_inside_their_block() {
    // `my sub helper` is callable only inside its declaring block, from
    // its declaration down — the grammar's `lexical` field marks it, the
    // builder stamps `SymbolDetail::Sub{lexical}`, and `complete_general`
    // gates on the declaring scope. File-wide subs stay file-wide.
    let source = "\
sub outer {
    my sub helper { return 42; }
    return helper();
}
sub plain { return 1; }
";
    let analysis = parse_analysis(source);
    let names_at = |row: usize, col: usize| -> Vec<String> {
        analysis
            .complete_general(tree_sitter::Point { row, column: col })
            .into_iter()
            .map(|c| c.label)
            .collect()
    };
    // Inside outer's block, after the decl: helper offered.
    let inside = names_at(2, 4);
    assert!(inside.iter().any(|n| n == "helper"), "in-scope: {inside:?}");
    // At file level (inside `plain`'s body): helper is NOT offered.
    let outside = names_at(4, 12);
    assert!(!outside.iter().any(|n| n == "helper"), "out-of-scope leak: {outside:?}");
    assert!(outside.iter().any(|n| n == "outer"), "file-wide subs stay: {outside:?}");
}

#[test]
fn lexical_methods_complete_with_amp_prefix_in_scope_only() {
    // `my method hidden` dispatches ONLY as `$invocant->&hidden` — the
    // member lane must offer it with the `&` prefix, never bare, and only
    // inside the declaring block from the decl down. The class-keyed MRO
    // walk excludes it (no by-name dispatch, invisible cross-file), and
    // the bare-identifier lane never offers a lexical method at all.
    let source = "\
use v5.40;
use experimental 'class';
class Widget {
    my method hidden { return 42 }
    method go { return $self->&hidden() }
}
";
    let analysis = parse_analysis(source);
    let at = tree_sitter::Point { row: 4, column: 31 };

    let amp: Vec<String> = analysis
        .complete_lexical_methods_at(at)
        .into_iter()
        .map(|c| {
            assert_eq!(c.insert_text.as_deref(), Some(c.label.as_str()));
            c.label
        })
        .collect();
    assert!(amp.iter().any(|n| n == "&hidden"), "in-scope &-lane: {amp:?}");
    // Outside the class block: nothing.
    let outside = analysis.complete_lexical_methods_at(tree_sitter::Point { row: 6, column: 0 });
    assert!(outside.is_empty(), "lexical method leaked out of scope: {outside:?}");

    // The class-keyed walk never offers it (bare `hidden` would not dispatch).
    let class_walk: Vec<String> = analysis
        .complete_methods_for_class("Widget", None)
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert!(!class_walk.iter().any(|n| n == "hidden"), "bare leak: {class_walk:?}");
    assert!(class_walk.iter().any(|n| n == "go"), "real methods stay: {class_walk:?}");

    // Bare-identifier lane: lexical METHODS have no bare-call spelling.
    let general: Vec<String> = analysis
        .complete_general(at)
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert!(!general.iter().any(|n| n == "hidden"), "bare-lane leak: {general:?}");
}

#[test]
fn perl_list_return_destructures_positionally() {
    // `return (A->new, B->new)` is a positional tuple (`list_expression` in
    // value position types as `Sequence`), so `my ($q, $a) = mk()` binds
    // each slot to its element (docs/adr/destructuring.md). The slurpy
    // tail carries the whole source (the documented approximation).
    let source = "\
package Queue; sub new { bless {}, shift }
package Agent; sub new { bless {}, shift }
package main;
sub mk { return (Queue->new, Agent->new); }
my ($q, $a) = mk();
my ($first, @rest) = mk();
";
    use crate::model::file_analysis::InferredType;
    let analysis = parse_analysis(source);
    let at = tree_sitter::Point { row: 5, column: 0 };
    assert_eq!(analysis.inferred_type_via_bag("$q", at), Some(InferredType::ClassName("Queue".into())));
    assert_eq!(analysis.inferred_type_via_bag("$a", at), Some(InferredType::ClassName("Agent".into())));
    let at2 = tree_sitter::Point { row: 6, column: 0 };
    assert_eq!(analysis.inferred_type_via_bag("$first", at2), Some(InferredType::ClassName("Queue".into())));
    assert!(matches!(analysis.inferred_type_via_bag("@rest", at2), Some(InferredType::Sequence(_))), "slurpy tail: whole-source lattice");
}
