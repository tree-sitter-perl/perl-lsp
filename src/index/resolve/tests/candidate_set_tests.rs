//! CandidateSet: one construction, every feature a projection.

use super::*;

// ---- CandidateSet: one construction, every feature a projection ----
// docs/adr/resolution-candidate-set.md

/// Two workspace files, cursor on the producer's `sub foo` decl. The set's
/// projections must agree with each other: rename is the references image
/// (minus policy-excluded sites), never an independent walk.
fn candidate_fixture() -> (FileStore, std::sync::Arc<FileAnalysis>, PathBuf, PathBuf) {
    let store = FileStore::new();
    let path_a = PathBuf::from("/tmp/cs_test_a.pm");
    let path_b = PathBuf::from("/tmp/cs_test_b.pm");
    let fa_a = std::sync::Arc::new(parse(
        "package A;\nour @EXPORT_OK = qw/foo/;\nsub foo { 42 }\n1;\nuse My::Dep qw/imported_fn/;\n",
    ));
    store.insert_workspace_arc(path_a.clone(), fa_a.clone());
    let fa_b = parse("package B;\nuse A qw/foo/;\nsub bar { foo(); }\n1;\n");
    store.insert_workspace(path_b.clone(), fa_b);
    (store, fa_a, path_a, path_b)
}

#[test]
fn candidate_set_rename_is_subset_of_references() {
    let (store, fa_a, path_a, path_b) = candidate_fixture();
    // Cursor on `foo` in `sub foo` (row 2, col 4).
    let cs = resolve(
        &store,
        &fa_a,
        FileKey::Path(path_a.clone()),
        tree_sitter::Point { row: 2, column: 4 },
        None,
        OverrideScope::default(),
    );
    assert!(cs.renameable(), "a package sub is a cross-file renameable");

    let refs = cs.references();
    assert!(
        refs.iter().any(|r| matches!(&r.key, FileKey::Path(p) if p == &path_a)
            && r.access == AccessKind::Declaration),
        "references include the decl in A: {refs:?}",
    );
    assert!(
        refs.iter().any(|r| matches!(&r.key, FileKey::Path(p) if p == &path_b)),
        "references include the call in B: {refs:?}",
    );

    // Rename is the same set + policy — every edit span must be a reference
    // span (partial-edit-beyond-references is unrepresentable).
    let edits = cs.rename_edits("food").expect("perl rename never refuses");
    assert!(!edits.is_empty());
    for (loc, text) in &edits {
        assert_eq!(text, "food");
        assert!(
            refs.iter().any(|r| file_key_eq(&r.key, &loc.key) && r.span == loc.span),
            "rename edit at {loc:?} is not in the references image",
        );
    }
    // And the B call site is edited (rename didn't silently shrink to one file).
    assert!(
        edits.iter().any(|(l, _)| matches!(&l.key, FileKey::Path(p) if p == &path_b)),
        "rename reaches the consumer call site: {edits:?}",
    );
}

/// THE symmetry invariant: a visibility change made once, at construction,
/// moves every projection together. No projection has its own copy of the
/// axis to forget — completion's candidate gathering included.
#[test]
fn candidate_set_visibility_axis_flows_to_every_projection() {
    use crate::index::module_index::{CachedModule, ModuleIndex};
    use std::sync::Arc;

    let (store, fa_a, path_a, path_b) = candidate_fixture();
    let point = tree_sitter::Point { row: 2, column: 4 };

    // A dependency-tier module universe: one module A imports from (with a
    // remaining export surface), one it doesn't (auto-import candidate).
    let idx = ModuleIndex::new_for_test();
    let insert = |name: &str, src: &str| {
        idx.insert_cache(
            name,
            Some(Arc::new(CachedModule::new(
                PathBuf::from(format!("/fake/{}.pm", name.replace("::", "/"))),
                Arc::new(parse(src)),
            ))),
        );
    };
    insert(
        "My::Dep",
        "package My::Dep;\nour @EXPORT_OK = qw/imported_fn other_fn/;\nsub imported_fn { 1 }\nsub other_fn { 2 }\n1;\n",
    );
    insert(
        "My::Extra",
        "package My::Extra;\nour @EXPORT = qw/extra_fn/;\nsub extra_fn { 3 }\n1;\n",
    );

    let labels = |cands: &[crate::model::file_analysis::CompletionCandidate]| -> Vec<String> {
        cands.iter().map(|c| c.label.clone()).collect()
    };

    let wide = resolve(&store, &fa_a, FileKey::Path(path_a.clone()), point, Some(&idx), OverrideScope::default());
    let wide_refs = wide.references();
    let wide_edits = wide.rename_edits("food").expect("perl rename never refuses");
    assert!(wide_refs.iter().any(|r| matches!(&r.key, FileKey::Path(p) if p == &path_b)));
    assert!(wide_edits.iter().any(|(l, _)| matches!(&l.key, FileKey::Path(p) if p == &path_b)));
    let wide_names = labels(&wide.complete("", true));
    assert!(wide_names.contains(&"foo".to_string()), "local sub gathered: {wide_names:?}");
    assert!(
        wide_names.contains(&"imported_fn".to_string()),
        "explicitly imported name gathered (origin-file fact): {wide_names:?}",
    );
    assert!(
        wide_names.contains(&"other_fn".to_string()),
        "imported module's remaining export surface gathered: {wide_names:?}",
    );
    assert!(
        wide_names.contains(&"extra_fn".to_string()),
        "unimported exporter's surface gathered as auto-import: {wide_names:?}",
    );
    assert!(
        wide.complete_modules("My::").iter().any(|(n, _)| n == "My::Dep"),
        "module-name universe gathered",
    );

    assert!(
        !wide.highlights().is_empty(),
        "highlights (origin-narrowed references) answer under the default mask",
    );

    // One knob turned at construction: only the OPEN tier stays visible.
    let narrow = resolve(&store, &fa_a, FileKey::Path(path_a), point, Some(&idx), OverrideScope::default())
        .with_visibility(RoleMask::OPEN);
    assert!(
        narrow.references().is_empty(),
        "references projection inherits the narrowed visibility",
    );
    assert!(
        narrow.highlights().is_empty() && narrow.linked_editing_spans().is_empty(),
        "highlights + linked-editing inherit the SAME narrowed visibility — \
         the origin's workspace tier is outside the OPEN mask",
    );
    assert!(
        narrow.rename_edits("food").expect("perl rename never refuses").is_empty(),
        "rename projection inherits the SAME narrowed visibility — not its own copy",
    );
    let narrow_names = labels(&narrow.complete("", true));
    assert!(
        narrow_names.contains(&"foo".to_string())
            && narrow_names.contains(&"imported_fn".to_string()),
        "origin-file names (in-scope + explicit imports) are OPEN-tier: {narrow_names:?}",
    );
    assert!(
        !narrow_names.contains(&"other_fn".to_string())
            && !narrow_names.contains(&"extra_fn".to_string()),
        "dependency-supplied names inherit the SAME narrowed visibility: {narrow_names:?}",
    );
    assert!(
        narrow.complete_modules("My::").is_empty(),
        "module-name gathering inherits the SAME narrowed visibility",
    );
}

/// Lexical cursors: the set still answers, from the origin file's in-file
/// machinery — handlers no longer branch on resolution shape themselves.
#[test]
fn candidate_set_local_projections() {
    let store = FileStore::new();
    let src = "my $count = 0;\n$count++;\nprint $count;\n";
    let fa = parse(src);
    let key = FileKey::Path(PathBuf::from("/tmp/cs_local.pl"));
    // Cursor on `$count` decl (row 0, col 3).
    let cs = resolve(&store, &fa, key, tree_sitter::Point { row: 0, column: 3 }, None, OverrideScope::default());
    assert!(matches!(cs.resolution(), Some(ResolvedTarget::Local)));
    assert!(cs.renameable());
    let refs = cs.references();
    assert_eq!(refs.len(), 3, "decl + two uses: {refs:?}");
    let edits = cs.rename_edits("$total").expect("perl rename never refuses");
    assert_eq!(edits.len(), 3, "rename covers the same in-file set: {edits:?}");
    assert!(cs.implementations().is_empty());
}

/// Highlights and linked editing at a FIELD-GROUP cursor: both are
/// projections of the same Group resolution references/rename read, so a
/// Moo attr's whole spelling family lights up together (the drift the A2
/// rework closed — the old in-file highlight path lacked the group claim).
/// Linked editing additionally excludes the affix-derived accessor
/// (`has_size` for attr `size`): co-editing one text across it would
/// corrupt the affix, exactly as rename re-derives rather than bare-writes.
#[test]
fn candidate_set_highlights_and_linked_editing_at_field_group_cursor() {
    let store = FileStore::new();
    let src = "\
package Widget;
use Moo;
has 'size' => (is => 'rw', predicate => 'has_size');
sub check {
    my ($self) = @_;
    return $self->size + ($self->has_size ? 1 : 0);
}
package main;
my $w = Widget->new(size => 3);
print $w->size;
";
    let fa = parse(src);
    let key = FileKey::Path(PathBuf::from("/tmp/cs_group_hl.pl"));
    // Cursor on the `size` token of the `has` declaration (row 2).
    let decl_col = src.lines().nth(2).unwrap().find("size").unwrap();
    let cs = resolve(
        &store,
        &fa,
        key,
        tree_sitter::Point { row: 2, column: decl_col },
        None,
        OverrideScope::default(),
    );
    assert!(
        matches!(cs.resolution(), Some(ResolvedTarget::Group { .. })),
        "a has-attr cursor resolves to the projection group",
    );

    let line5 = src.lines().nth(5).unwrap();
    let size_call = (5usize, line5.find("size").unwrap());
    let has_size_call = (5usize, line5.find("has_size").unwrap());
    let ctor_key = (8usize, src.lines().nth(8).unwrap().find("size").unwrap());
    let reader_call = (9usize, src.lines().nth(9).unwrap().find("size").unwrap());

    let hl: Vec<(usize, usize)> = cs
        .highlights()
        .iter()
        .map(|l| (l.span.start.row, l.span.start.column))
        .collect();
    for want in [size_call, has_size_call, ctor_key, reader_call] {
        assert!(hl.contains(&want), "highlights missing {want:?}: {hl:?}");
    }

    let le: Vec<(usize, usize)> = cs
        .linked_editing_spans()
        .iter()
        .map(|s| (s.start.row, s.start.column))
        .collect();
    for want in [size_call, ctor_key, reader_call] {
        assert!(le.contains(&want), "linked editing missing {want:?}: {le:?}");
    }
    assert!(
        !le.contains(&has_size_call),
        "the affix-derived accessor must not co-edit the bare text: {le:?}",
    );
}

/// Goto-def is the forward projection of the same set: cursor on a call,
/// definitions() lands on the decl in the origin file.
#[test]
fn candidate_set_definitions_local() {
    let store = FileStore::new();
    let src = "sub greet { 1 }\ngreet();\n";
    let fa = parse(src);
    let key = FileKey::Path(PathBuf::from("/tmp/cs_defs.pl"));
    // Cursor on the call `greet` (row 1, col 2).
    let cs = resolve(&store, &fa, key, tree_sitter::Point { row: 1, column: 2 }, None, OverrideScope::default());
    let defs = cs.definitions();
    assert_eq!(defs.len(), 1, "one local def: {defs:?}");
    assert_eq!(defs[0].span.start.row, 0, "decl on line 0: {defs:?}");
    assert_eq!(defs[0].access, AccessKind::Declaration);
}


/// Qualified-path completion (pack lane): `fmtx::` gathers the OWNER's
/// members — functions, the nested namespace, the inline namespace's
/// lifted members — and NEVER the global pool (the similarly-named
/// free function, the in-scope caller). The completion half of
/// namespace participation; gd's owner-anchored half shares the
/// membership predicate.
#[cfg(feature = "cpp")]
#[test]
fn complete_qualified_path_pack_gathers_owner_members_only() {
    let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
    let driver = reg.for_id("cpp").expect("cpp driver");
    let src = "namespace fmtx {\n\
               void format_to(int v);\n\
               void print(int v);\n\
               namespace detail { void detail_helper(); }\n\
               inline namespace v11 { void inline_fn(); }\n\
               }\n\
               void formatter_global(int v);\n\
               void caller() {\n\
                   fmtx::f\n\
               }\n";
    let fa = driver.analyze_with_path(src, Some(std::path::Path::new("/fake/use.cpp")));
    let store = FileStore::new();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let cs = resolve(
        &store,
        &fa,
        FileKey::Path(PathBuf::from("/fake/use.cpp")),
        tree_sitter::Point { row: 8, column: 11 },
        None,
        OverrideScope::default(),
    );

    let labels: Vec<String> =
        cs.complete_qualified_path(&idx, "fmtx").into_iter().map(|c| c.label).collect();
    for want in ["format_to", "print", "detail", "inline_fn"] {
        assert!(labels.iter().any(|l| l == want), "missing {want}: {labels:?}");
    }
    for reject in ["formatter_global", "caller"] {
        assert!(!labels.iter().any(|l| l == reject), "leaked {reject}: {labels:?}");
    }

    // Nested drill: `fmtx::detail::` is the inner namespace's members only.
    let labels: Vec<String> =
        cs.complete_qualified_path(&idx, "detail").into_iter().map(|c| c.label).collect();
    assert!(labels.iter().any(|l| l == "detail_helper"), "missing detail_helper: {labels:?}");
    assert!(!labels.iter().any(|l| l == "format_to"), "parent member leaked: {labels:?}");
}

/// A cursor position far past EOF (a malformed/stale LSP request — the
/// client's document went out of sync, or the position was corrupted in
/// transit) must resolve to "no target," never panic. `symbol_at` /
/// `ref_at` / `rename_kind_at` are all span-containment checks so this
/// already held structurally; this locks it in as an explicit contract for
/// `resolve_symbol`'s attacker-influenced `point` argument.
#[test]
fn resolve_symbol_out_of_bounds_point_returns_none() {
    let fa = parse("package Foo;\nsub bar { 1 }\n1;\n");
    let resolved = resolve_symbol(
        &fa,
        tree_sitter::Point { row: 9_999, column: 9_999 },
        None,
    );
    assert!(resolved.is_none(), "expected no resolution past EOF, got {resolved:?}");
}

/// `TargetRef::from_rename_kind` is a total function over `RenameKind`: the
/// `Variable`/`HashKey` arms return `None` rather than a target. This is
/// the same "no target for this kind" contract `resolve_symbol_scoped`'s
/// `kind => { let Some(t) = ... else { return None }; }` catch-all relies
/// on instead of `.expect`-ing a target always exists.
#[test]
fn from_rename_kind_returns_none_for_kinds_with_no_target() {
    use crate::model::file_analysis::RenameKind;
    let fa = parse("package Foo;\nsub bar { 1 }\n1;\n");
    assert!(TargetRef::from_rename_kind(RenameKind::Variable, &fa, None, OverrideScope::default())
        .is_none());
    assert!(TargetRef::from_rename_kind(
        RenameKind::HashKey("k".to_string()),
        &fa,
        None,
        OverrideScope::default()
    )
    .is_none());
}

/// Regression for the `collect_from_analysis` scope-derivation refactor
/// (`callable_scope_for_refs.as_ref().unwrap()` → graceful skip): a plain
/// package-scoped sub rename/reference walk must still find both the decl
/// and the call site exactly as before the hardening.
#[test]
fn collect_from_analysis_still_finds_sub_refs_after_scope_hardening() {
    let fa = parse("package Foo;\nsub greet { 1 }\ngreet();\n1;\n");
    let target = TargetRef {
        name: "greet".to_string(),
        kind: TargetKind::Sub { package: Some("Foo".to_string()) },
        method_classes: Vec::new(),
        scope: OverrideScope::Dispatch,
        def_paths: Vec::new(),
        bare_constant: false,
            ctor_of: None,
            class_ns: None,
            member_shape: Default::default(),
    };
    let store = FileStore::new();
    let path = PathBuf::from("/tmp/resolve_test_scope_hardening.pm");
    store.insert_workspace(path.clone(), fa);
    let results = refs_to(&store, None, &target, RoleMask::EDITABLE);
    assert!(
        results.iter().any(|r| r.access == AccessKind::Declaration),
        "expected declaration, got {results:?}"
    );
    assert!(
        results.iter().any(|r| r.access == AccessKind::Read),
        "expected call-site read, got {results:?}"
    );
}

/// goto-def and references are two projections of ONE CandidateSet, so they
/// cannot disagree about whether a declaration exists. An INHERITED Moo slot
/// poke is the shape that split them: `$self->{size}` in `Gadget` carries a
/// `Class(Gadget)` owner, so goto-def's owner-keyed hash-key lookup asked the
/// SUBCLASS for a key only the base declares and found nothing, while
/// references reached `Widget`'s `has size` through the group the identity
/// already climbs to. Both must land on the base's decl token.
#[test]
fn goto_def_agrees_with_references_on_inherited_slot() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    let store = FileStore::new();
    let widget = PathBuf::from("/tmp/cs_inh_widget.pm");
    let gadget = PathBuf::from("/tmp/cs_inh_gadget.pm");
    let widget_src = "package Widget;\nuse Moo;\nhas size => (is => 'ro');\nhas color => (is => 'ro');\n1;\n";
    let gadget_src = "package Gadget;\nuse Moo;\nextends 'Widget';\nsub area { my $self = shift; return $self->{size}; }\n1;\n";
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(widget.clone(), Arc::new(parse(widget_src)));
    idx.register_workspace_module(gadget.clone(), Arc::new(parse(gadget_src)));
    store.insert_workspace(widget.clone(), parse(widget_src));
    store.insert_workspace(gadget.clone(), parse(gadget_src));

    let gadget_fa = store.workspace_raw().get(&gadget).unwrap().value().clone();
    let col = gadget_src.lines().nth(3).unwrap().find("{size}").unwrap() + 1;
    let cs = resolve(
        &store,
        &gadget_fa,
        FileKey::Path(gadget.clone()),
        tree_sitter::Point { row: 3, column: col },
        Some(&idx),
        OverrideScope::default(),
    );

    let decl_col = widget_src.lines().nth(2).unwrap().find("size").unwrap();
    let refs = cs.references();
    assert!(
        refs.iter().any(|r| matches!(&r.key, FileKey::Path(p) if p == &widget)
            && r.span.start.row == 2
            && r.span.start.column == decl_col),
        "references reaches the base decl: {refs:?}",
    );

    let defs = cs.definitions();
    assert!(
        defs.iter().any(|d| matches!(&d.key, FileKey::Path(p) if p == &widget)
            && d.span.start.row == 2
            && d.span.start.column == decl_col),
        "goto-def must land on the same base decl references already names: {defs:?}",
    );
}

/// The same projection disagreement on the TEMPLATE-METHOD shape, which is
/// plain inheritance with no roles and no plugins: a base calls
/// `$self->step_one()` and only the subclass declares it. references reaches
/// the child's decl through the override family identity already computed,
/// and goto-def's forward lane is `resolve_method_in_ancestors` — UPWARD
/// only, so it climbs past a method that lives BELOW and answers nothing.
///
/// The `Group` identity backstop does not cover it: a method resolves to
/// `ResolvedTarget::Target`, never `Group`, so it never reaches that arm.
#[test]
fn goto_def_agrees_with_references_on_template_method() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    let store = FileStore::new();
    let base = PathBuf::from("/tmp/cs_tmpl_base.pm");
    let child = PathBuf::from("/tmp/cs_tmpl_child.pm");
    let base_src = "package Base;\nsub run {\n    my $self = shift;\n    return $self->step_one() + 1;\n}\n1;\n";
    let child_src = "package Child;\nuse parent 'Base';\nsub step_one { return 41 }\n1;\n";
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(base.clone(), Arc::new(parse(base_src)));
    idx.register_workspace_module(child.clone(), Arc::new(parse(child_src)));
    store.insert_workspace(base.clone(), parse(base_src));
    store.insert_workspace(child.clone(), parse(child_src));

    let base_fa = store.workspace_raw().get(&base).unwrap().value().clone();
    let col = base_src.lines().nth(3).unwrap().find("step_one").unwrap();
    let cs = resolve(
        &store,
        &base_fa,
        FileKey::Path(base.clone()),
        tree_sitter::Point { row: 3, column: col },
        Some(&idx),
        OverrideScope::default(),
    );

    let decl_col = child_src.lines().nth(2).unwrap().find("step_one").unwrap();
    let refs = cs.references();
    assert!(
        refs.iter().any(|r| matches!(&r.key, FileKey::Path(p) if p == &child)
            && r.span.start.row == 2
            && r.span.start.column == decl_col),
        "references reaches the child decl: {refs:?}",
    );

    let defs = cs.definitions();
    assert!(
        defs.iter().any(|d| matches!(&d.key, FileKey::Path(p) if p == &child)
            && d.span.start.row == 2
            && d.span.start.column == decl_col),
        "goto-def must land on the same child decl references already names: {defs:?}",
    );
}
