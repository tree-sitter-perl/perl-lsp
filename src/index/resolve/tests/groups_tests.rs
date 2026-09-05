//! resolve_symbol identity, field-projection attr groups, and the
//! implementations verb (incl. the cpp pack_symmetry lanes).

use super::*;

// ---- resolve_symbol: the single cursor→target entry point ----

/// Every kind that maps to a cross-file target must come back as
/// `Target`, lexical variables as `Local`, and blank space as `None` —
/// the same answers regardless of which handler (LSP or CLI) asks.
#[test]
fn test_resolve_symbol_kinds() {
    let src = "\
package Counter;
sub new { my ($class) = @_; return bless { count => 0 }, $class }
sub bump { my ($self) = @_; $self->{count}++; my $local = 1; return $local }
1;
";
    let fa = parse(src);
    let at = |row, col| resolve_symbol(&fa, tree_sitter::Point { row, column: col }, None);

    // `bump` decl → callable target scoped to the package ("same callable,
    // two shapes": decls surface as Sub even when call sites are Method).
    match at(2, 5) {
        Some(ResolvedTarget::Target(t)) => {
            assert_eq!(t.name, "bump");
            assert!(
                matches!(&t.kind, TargetKind::Sub { package: Some(p) } if p == "Counter"),
                "expected Sub scoped to Counter, got {:?}",
                t.kind,
            );
            assert!(t.supports_cross_file_rename());
        }
        other => panic!("expected callable target for bump decl, got {:?}", other),
    }

    // Package name → Package target.
    match at(0, 9) {
        Some(ResolvedTarget::Target(t)) => {
            assert!(matches!(t.kind, TargetKind::Package));
            assert!(t.supports_cross_file_rename());
        }
        other => panic!("expected Package target, got {:?}", other),
    }

    // `$local` → lexical, single-file.
    let local_col = src.lines().nth(2).unwrap().find("$local").unwrap() + 1;
    assert!(
        matches!(at(2, local_col), Some(ResolvedTarget::Local)),
        "expected Local for lexical $local, got {:?}",
        at(2, local_col),
    );
}

/// An owned hash key resolves to a cross-file HashKeyOfBridged target —
/// A Moo internal slot (`$self->{size}`) is one spelling of the `size` attr,
/// so it resolves to the attr's projection Group — the same group its `has`
/// decl, ctor key, reader, and mapped accessors resolve to. It
/// is NOT a plain `HashKeyOfBridged` rename: that would miss the accessor /
/// ctor-key sites the group carries. The group walks cross-file via
/// `group_rename_edits`.
#[test]
fn test_resolve_symbol_internal_slot_resolves_to_attr_group() {
    let src = "\
package Widget;
use Moo;
has size => (is => 'ro');
sub describe { my ($self) = @_; return $self->{size} }
1;
";
    let fa = parse(src);
    // Cursor on `size` inside `$self->{size}`.
    let col = src.lines().nth(3).unwrap().find("{size}").unwrap() + 1;
    match resolve_symbol(&fa, tree_sitter::Point { row: 3, column: col }, None) {
        Some(ResolvedTarget::Group { members, .. }) => {
            assert!(
                members.iter().any(|m| matches!(
                    m.target.kind,
                    TargetKind::HashKeyOfSub { .. } | TargetKind::InternalHashKey { .. }
                )),
                "slot group should carry the attr's ctor-key / internal-slot members: {:?}",
                members,
            );
        }
        other => panic!("expected the size attr projection Group, got {:?}", other),
    }
}

// ---- field projection groups: cross-file union ----

/// `field $x :param :reader` in Point.pm; a consumer constructs
/// `Point->new(x => 1)` and reads `$p->x`. References/rename from the
/// field decl must surface the consumer's ctor key and reader call;
/// from the consumer's key, the field must surface back.
#[test]
fn test_field_group_unions_across_files() {
    let store = FileStore::new();
    let point_path = PathBuf::from("/tmp/fieldgroup_point.pm");
    let user_path = PathBuf::from("/tmp/fieldgroup_user.pl");

    let point_src = "\
use v5.38;
class Point {
    field $x :param :reader;
    method magnitude () { return $x * $x; }
}
1;
";
    let user_src = "\
use Point;
my $p = Point->new(x => 3);
my $val = $p->x;
";
    let point_fa = parse(point_src);
    let user_fa = parse(user_src);
    store.insert_workspace(point_path.clone(), point_fa);
    store.insert_workspace(user_path.clone(), user_fa);

    let origin_fa = store.workspace_raw().get(&point_path).unwrap().value().clone();
    // Cursor on `$x` in the field decl (row 2, col 11 = bare name).
    let resolved = resolve_symbol(&origin_fa, tree_sitter::Point { row: 2, column: 11 }, None)
        .expect("field decl resolves");
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } = resolved else {
        panic!("expected Group, got {:?}", resolved);
    };
    assert!(pinned_spans.is_empty(), "local mint has no pinned spans");
    assert!(!local_spans.is_empty(), "field var spellings present");
    assert_eq!(members.len(), 2, "reader + ctor-key members: {:?}", members);

    let locs = group_refs(
        &store,
        None,
        &FileKey::Path(point_path.clone()),
        &local_spans,
        &pinned_spans,
        &members,
        None,
    );
    let in_user: Vec<_> = locs
        .iter()
        .filter(|l| matches!(&l.key, FileKey::Path(p) if p == &user_path))
        .map(|l| (l.span.start.row, l.span.start.column))
        .collect();
    assert!(
        in_user.contains(&(1, 19)),
        "consumer ctor key `x` included; user-file hits: {:?}",
        in_user,
    );
    assert!(
        in_user.contains(&(2, 14)),
        "consumer reader call `->x` included; user-file hits: {:?}",
        in_user,
    );
}

/// Consumer-side cursor, class elsewhere: from the ctor key (or accessor
/// call) in a file that only `use`s Point, the group is minted from the
/// CLASS's cached analysis — its field-variable/decl spans pin to the
/// class file, so rename from the consumer rewrites the field decl and
/// body uses over there too.
#[test]
fn test_consumer_cursor_mints_group_from_class_analysis() {
    let point_src = "\
use v5.38;
class Point {
    field $x :param :reader;
    method magnitude () { return $x * $x; }
}
1;
";
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let class_path = PathBuf::from("/tmp/grp_mint_point.pm");
    idx.insert_cache(
        "Point",
        Some(std::sync::Arc::new(crate::index::module_index::CachedModule::new(
            class_path.clone(),
            std::sync::Arc::new(parse(point_src)),
        ))),
    );

    let consumer = parse("use Point;\nmy $p = Point->new(x => 3);\nmy $v = $p->x;\n");

    // From the ctor key `x` (row 1, col 19).
    let resolved = resolve_symbol(&consumer, tree_sitter::Point { row: 1, column: 19 }, Some(&idx))
        .expect("consumer key resolves");
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } = resolved else {
        panic!("expected Group from consumer key, got {:?}", resolved);
    };
    assert!(local_spans.is_empty(), "remote mint: no origin spans");
    assert_eq!(members.len(), 2, "reader + ctor-key members");
    assert!(
        pinned_spans.iter().all(|(p, _)| p == &class_path),
        "pinned to the class file: {:?}",
        pinned_spans,
    );
    // Decl (row 2) + body use (row 3) pinned from the class analysis.
    let pinned_rows: Vec<usize> = pinned_spans.iter().map(|(_, s)| s.start.row).collect();
    assert!(
        pinned_rows.contains(&2) && pinned_rows.contains(&3),
        "field decl + body use pinned: {:?}",
        pinned_rows,
    );

    // From the accessor call `->x` (row 2, col 12): same group shape.
    let resolved = resolve_symbol(&consumer, tree_sitter::Point { row: 2, column: 12 }, Some(&idx))
        .expect("consumer accessor resolves");
    assert!(
        matches!(resolved, ResolvedTarget::Group { ref pinned_spans, .. } if !pinned_spans.is_empty()),
        "accessor-call cursor mints the remote group, got {:?}",
        resolved,
    );
}

/// Cross-file mapped rename: the consumer's `$w->has_size` predicate
/// call re-derives to `has_extent` when the attr renames — per-member
/// replacement texts via group_rename_edits.
#[test]
fn test_group_rename_rederives_mapped_members_cross_file() {
    let store = FileStore::new();
    let class_path = PathBuf::from("/tmp/grp_map_widget.pm");
    let user_path = PathBuf::from("/tmp/grp_map_user.pl");
    store.insert_workspace(
        class_path.clone(),
        parse("package Widget;\nuse Moo;\nhas size => (is => 'ro', predicate => 1);\n1;\n"),
    );
    store.insert_workspace(
        user_path.clone(),
        parse("use Widget;\nmy $w = Widget->new(size => 3);\nprint $w->size if $w->has_size;\n"),
    );

    let class_fa = store.workspace_raw().get(&class_path).unwrap().value().clone();
    // Cursor on the attr decl token `size` (row 2, col 4).
    let resolved = resolve_symbol(&class_fa, tree_sitter::Point { row: 2, column: 4 }, None)
        .expect("attr decl resolves");
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } = resolved else {
        panic!("expected Group, got {:?}", resolved);
    };
    let edits = group_rename_edits(
        &store,
        None,
        &FileKey::Path(class_path.clone()),
        &local_spans,
        &pinned_spans,
        &members,
        "extent",
        RoleMask::EDITABLE,
    );
    let user_edits: Vec<_> = edits
        .iter()
        .filter(|(l, _)| matches!(&l.key, FileKey::Path(p) if p == &user_path))
        .map(|(l, t)| (l.span.start.row, l.span.start.column, t.clone()))
        .collect();
    assert!(
        user_edits.contains(&(2, 22, "has_extent".to_string())),
        "consumer predicate call re-derived; user edits: {:?}",
        user_edits,
    );
    assert!(
        user_edits.iter().any(|(r, _, t)| *r == 1 && t == "extent"),
        "consumer ctor key renamed bare; user edits: {:?}",
        user_edits,
    );
}

/// Explicit-string accessor names (`predicate => 'has_size'`) define the method
/// AT that string, so renaming the attr must rewrite the defining string too —
/// not just the call sites — or Moo keeps minting `has_size` while callers say
/// `has_dim` (non-compiling). The `=> 1` derived form has no string and is
/// covered by `test_group_rename_rederives_mapped_members_cross_file`.
#[test]
fn moo_explicit_string_accessor_renames_its_defining_string() {
    let store = FileStore::new();
    let path = PathBuf::from("/tmp/moo_str_widget.pm");
    let src = "package Widget;\nuse Moo;\n\
        has size => (is => 'rw', predicate => 'has_size', clearer => 'clear_size');\n\
        sub area { my $self = shift; return $self->size if $self->has_size; }\n1;\n";
    store.insert_workspace(path.clone(), parse(src));
    let fa = store.workspace_raw().get(&path).unwrap().value().clone();
    // Cursor on the `has size` attr token (row 2, col 4).
    let resolved = resolve_symbol(&fa, tree_sitter::Point { row: 2, column: 4 }, None)
        .expect("attr decl resolves");
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } = resolved else {
        panic!("expected Group, got {:?}", resolved);
    };
    let edits = group_rename_edits(
        &store, None, &FileKey::Path(path.clone()), &local_spans, &pinned_spans, &members, "dim",
        RoleMask::EDITABLE,
    );
    let lines: Vec<&str> = src.lines().collect();
    let materialized: Vec<(String, String)> = edits
        .iter()
        .map(|(l, t)| {
            (lines[l.span.start.row][l.span.start.column..l.span.end.column].to_string(), t.clone())
        })
        .collect();
    // The defining strings rewrite to the affixed new name, and the call site
    // stays consistent with them.
    for want in [("has_size", "has_dim"), ("clear_size", "clear_dim")] {
        assert!(
            materialized.contains(&(want.0.to_string(), want.1.to_string())),
            "defining string {:?} must rewrite to {:?}; got {:?}",
            want.0, want.1, materialized,
        );
    }
    // has_dim appears at least twice: the predicate defining string + the call.
    assert!(
        materialized.iter().filter(|(_, t)| t == "has_dim").count() >= 2,
        "predicate string AND its call rename together; got {:?}",
        materialized,
    );
}

/// `our` package globals rename cross-file: `$Cfg::debug` is the same variable
/// everywhere, so renaming the `our` decl reaches every qualified access in
/// other files, and vice versa. A lexical `my` stays single-file (`Local`).
#[test]
fn package_var_our_renames_cross_file() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    let store = FileStore::new();
    let lib = PathBuf::from("/tmp/pv_cfg.pm");
    let app = PathBuf::from("/tmp/pv_app.pl");
    let lib_src = "package Cfg;\nour $debug = 0;\nsub on { $debug = 1 }\n1;\n";
    let app_src = "use Cfg;\nprint $Cfg::debug;\n$Cfg::debug = 5;\n";

    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(lib.clone(), Arc::new(parse(lib_src)));
    store.insert_workspace(lib.clone(), parse(lib_src));
    store.insert_workspace(app.clone(), parse(app_src));

    let lib_fa = store.workspace_raw().get(&lib).unwrap().value().clone();

    // Cursor on the `our $debug` decl resolves to a cross-file PackageVar.
    let col = lib_src.lines().nth(1).unwrap().find("debug").unwrap();
    let resolved = resolve_symbol(&lib_fa, tree_sitter::Point { row: 1, column: col }, Some(&idx))
        .expect("our decl resolves");
    let ResolvedTarget::Target(t) = resolved else { panic!("expected Target, got {:?}", resolved) };
    assert!(
        matches!(&t.kind, TargetKind::PackageVar { package } if package == "Cfg"),
        "our var should resolve to PackageVar, got {:?}",
        t.kind,
    );
    assert!(t.supports_cross_file_rename());

    let refs = refs_to(&store, Some(&idx), &t, RoleMask::EDITABLE);
    let hit = |p: &PathBuf| refs.iter().any(|r| matches!(&r.key, FileKey::Path(x) if x == p));
    assert!(hit(&lib), "reaches the decl file. refs: {:?}", refs);
    assert!(hit(&app), "reaches the cross-file $Cfg::debug accesses. refs: {:?}", refs);
    // Both qualified accesses in app.pl + decl + unqualified in lib.
    assert!(refs.len() >= 4, "decl + unqualified + 2 qualified accesses: {:?}", refs);

    // A lexical `my` stays single-file (Local) — package globals only.
    let my_fa = parse("my $x = 1;\nsub f { $x + 1 }\n");
    assert!(
        matches!(resolve_symbol(&my_fa, tree_sitter::Point { row: 0, column: 3 }, None), Some(ResolvedTarget::Local)),
        "lexical my stays Local",
    );
}

/// The rename name guard rejects corrupting (empty/whitespace/sigil-only)
/// names so neither entry point emits a token-deleting edit set, while real
/// names (sigil-bearing variable names included) pass.
#[test]
fn rename_name_guard_rejects_empty_and_whitespace() {
    use crate::index::resolve::is_valid_rename_name;
    for bad in ["", " ", "   ", "\t", "$", "@", "%", "$ ", "  @  "] {
        assert!(!is_valid_rename_name(bad), "should reject {bad:?}");
    }
    for ok in ["dim", "foo_bar", "$scalar", "@list", "%map", "RENAMED"] {
        assert!(is_valid_rename_name(ok), "should accept {ok:?}");
    }
}

/// Array/hash package globals rename the bare name tail only — qualified
/// element/slice reads (`$Pkg::items[0]`, `$Pkg::map{k}`) span the whole
/// `$Pkg::name`, so the rewrite must be anchored at the span end or it eats the
/// sigil + `Pkg::` qualifier and produces invalid Perl.
#[test]
fn package_var_array_hash_globals_rename_name_tail_only() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    let store = FileStore::new();
    let lib = PathBuf::from("/tmp/pvc_pkg.pm");
    let app = PathBuf::from("/tmp/pvc_user.pl");
    let lib_src = "package Pkg;\nour @items = (1, 2, 3);\nour %map = (a => 1);\n1;\n";
    let app_src = "use Pkg;\nmy $f = $Pkg::items[0];\nmy @s = @Pkg::items[0, 1];\nmy $v = $Pkg::map{a};\n";

    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(lib.clone(), Arc::new(parse(lib_src)));
    store.insert_workspace(lib.clone(), parse(lib_src));
    store.insert_workspace(app.clone(), parse(app_src));
    let lib_fa = store.workspace_raw().get(&lib).unwrap().value().clone();

    // Cursor on `our @items`.
    let col = lib_src.lines().nth(1).unwrap().find("items").unwrap();
    let ResolvedTarget::Target(t) =
        resolve_symbol(&lib_fa, tree_sitter::Point { row: 1, column: col }, Some(&idx)).unwrap()
    else {
        panic!("our @items should resolve to a target")
    };
    let refs = refs_to(&store, Some(&idx), &t, RoleMask::EDITABLE);

    // Every edit in user.pl must cover exactly `items` (the name tail), never
    // starting on the `$`/`@` sigil or the `Pkg::` qualifier.
    let app_edits: Vec<_> = refs
        .iter()
        .filter(|r| matches!(&r.key, FileKey::Path(p) if p == &app))
        .collect();
    assert_eq!(app_edits.len(), 2, "both element + slice accesses: {:?}", app_edits);
    for e in &app_edits {
        let line = app_src.lines().nth(e.span.start.row).unwrap();
        let slice = &line[e.span.start.column..e.span.end.column];
        assert_eq!(slice, "items", "rewrite only the name tail, got {slice:?} in {line:?}");
    }
}

/// A `main` package global unifies its spellings *within its file* — decl, bare
/// reads, `$main::x`, `$::x` all rename together — but stays FILE-LOCAL: `main`
/// is the shared namespace of unrelated package-less scripts, so it resolves to
/// a flat origin-file `Group`, not a cross-file `PackageVar` (which would sweep
/// a different script's `main::x`). See the entrypoint-analysis note in resolve.
#[test]
fn package_var_main_global_is_file_local_group() {
    let src = "our $gv = 1;\nprint $gv;\nsub f { return $gv + 1 }\nprint $main::gv;\nprint $::gv;\n";
    let fa = parse(src);
    let ResolvedTarget::Group { local_spans, members, .. } =
        resolve_symbol(&fa, tree_sitter::Point { row: 0, column: 5 }, None).unwrap()
    else {
        panic!("a `main` global should resolve to a file-local Group, not a cross-file target")
    };
    assert!(members.is_empty(), "a flat package-var group has no projection members");
    let mut rows: Vec<usize> = local_spans.iter().map(|s| s.start.row).collect();
    rows.sort_unstable();
    // decl(0) + bare $gv(1) + bare $gv in sub(2) + $main::gv(3) + $::gv(4).
    assert_eq!(rows, vec![0, 1, 2, 3, 4], "every in-file spelling renames: {local_spans:?}");
}

/// Two unrelated package-less scripts both declare `our $config` (both in
/// `main`). Renaming one must NOT rewrite the other — `main` globals are
/// file-local until entrypoint analysis can prove two files are one program.
#[test]
fn package_var_main_global_does_not_cross_scripts() {
    let store = FileStore::new();
    let a = PathBuf::from("/tmp/pvm_a.pl");
    let b = PathBuf::from("/tmp/pvm_b.pl");
    let a_src = "our $config = 1;\nprint $config;\n";
    store.insert_workspace(a.clone(), parse(a_src));
    store.insert_workspace(b.clone(), parse("our $config = 2;\nprint $config;\n"));

    let a_fa = store.workspace_raw().get(&a).unwrap().value().clone();
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } =
        resolve_symbol(&a_fa, tree_sitter::Point { row: 0, column: 5 }, None).unwrap()
    else {
        panic!("main global should be a file-local Group")
    };
    let edits = group_rename_edits(
        &store, None, &FileKey::Path(a.clone()), &local_spans, &pinned_spans, &members, "settings",
        RoleMask::EDITABLE,
    );
    assert!(
        edits.iter().all(|(l, _)| matches!(&l.key, FileKey::Path(p) if p == &a)),
        "rename must stay in a.pl, never touch b.pl: {edits:?}",
    );
    assert_eq!(edits.len(), 2, "a.pl decl + bare read only: {edits:?}");
}

/// `OverrideScope` toggle: a method overridden in a child resolves to the
/// whole override family under `Hierarchy` (the default IDE refactor) but only
/// its own dispatch chain under `Dispatch`. Membership is edge-gated (`@ISA`),
/// never name-matched.
#[test]
fn override_scope_hierarchy_unions_dispatch_is_precise() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    let store = FileStore::new();
    let base = PathBuf::from("/tmp/os_base.pm");
    let child = PathBuf::from("/tmp/os_child.pm");
    let base_src = "package Base;\nsub new { bless {}, shift }\nsub shared { 1 }\n1;\n";
    let child_src =
        "package Child;\nuse parent 'Base';\nsub shared { my $s = shift; $s->SUPER::shared() + 1 }\n1;\n";

    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(base.clone(), Arc::new(parse(base_src)));
    idx.register_workspace_module(child.clone(), Arc::new(parse(child_src)));
    store.insert_workspace(base.clone(), parse(base_src));
    store.insert_workspace(child.clone(), parse(child_src));

    let base_fa = store.workspace_raw().get(&base).unwrap().value().clone();

    // Hierarchy (default): Base::shared's family includes the Child override,
    // so a rename reaches Child's file.
    let h = TargetRef::method(
        "shared".to_string(), "Base".to_string(), &base_fa, Some(&idx), OverrideScope::Hierarchy,
    );
    assert!(
        h.method_classes.iter().any(|c| c == "Child"),
        "hierarchy family must include the override class: {:?}",
        h.method_classes,
    );
    let hrefs = refs_to(&store, Some(&idx), &h, RoleMask::EDITABLE);
    assert!(
        hrefs.iter().any(|r| matches!(&r.key, FileKey::Path(p) if p == &child)),
        "hierarchy rename must reach the Child override: {:?}",
        hrefs,
    );

    // Dispatch: precise — the chain stops at the defining class, so the Child
    // override is NOT pulled into Base::shared's family.
    let d = TargetRef::method(
        "shared".to_string(), "Base".to_string(), &base_fa, Some(&idx), OverrideScope::Dispatch,
    );
    assert!(
        !d.method_classes.iter().any(|c| c == "Child"),
        "dispatch chain must NOT include the override class: {:?}",
        d.method_classes,
    );
}

/// Renaming imports (`use Exp beta => { -as => 'rb' }`). Two
/// distinct identities: the REMOTE name `beta` is the source `Exp::beta`
/// (renames together, across all consumers); the LOCAL alias `rb` is a
/// binding in the CONSUMING package (the `-as` value + local calls) that
/// never touches the exporter — not `Exp::beta`, not a stray `Exp::rb`.
#[test]
fn test_renaming_import_remote_joins_source_alias_stays_local() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    let store = FileStore::new();
    let exp = PathBuf::from("/tmp/rni_exp.pm");
    let cons = PathBuf::from("/tmp/rni_cons.pm");
    let exp_src = "package Exp;\nuse Exporter 'import';\nour @EXPORT_OK = ('beta');\nsub beta { 1 }\n1;\n";
    let cons_src = "package Consumer;\nuse Exp beta => { -as => 'rb' };\nsub run { rb() }\n1;\n";

    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(exp.clone(), Arc::new(parse(exp_src)));
    store.insert_workspace(exp.clone(), parse(exp_src));
    store.insert_workspace(cons.clone(), parse(cons_src));

    let hit = |refs: &[RefLocation], p: &PathBuf| {
        refs.iter().any(|r| matches!(&r.key, FileKey::Path(x) if x == p))
    };

    // Source rename reaches the consumer's REMOTE `beta` token.
    let src = TargetRef {
        name: "beta".to_string(),
        kind: TargetKind::Sub { package: Some("Exp".to_string()) },
        method_classes: Vec::new(), scope: OverrideScope::Dispatch, def_paths: Vec::new(), bare_constant: false,
            ctor_of: None,
            class_ns: None,
            member_shape: Default::default(),
    };
    let src_refs = refs_to(&store, Some(&idx), &src, RoleMask::EDITABLE);
    assert!(hit(&src_refs, &exp), "source def missing: {:?}", src_refs);
    assert!(hit(&src_refs, &cons), "remote `beta` token must join the source: {:?}", src_refs);

    // Alias rename is local to the consuming package — never the exporter.
    let alias = TargetRef {
        name: "rb".to_string(),
        kind: TargetKind::Sub { package: Some("Consumer".to_string()) },
        method_classes: Vec::new(), scope: OverrideScope::Dispatch, def_paths: Vec::new(), bare_constant: false,
            ctor_of: None,
            class_ns: None,
            member_shape: Default::default(),
    };
    let alias_refs = refs_to(&store, Some(&idx), &alias, RoleMask::EDITABLE);
    assert!(hit(&alias_refs, &cons), "alias `-as` value + call missing: {:?}", alias_refs);
    assert!(
        !hit(&alias_refs, &exp),
        "alias rename must NOT touch the exporter: {:?}",
        alias_refs,
    );
    assert!(alias_refs.len() >= 2, "alias group = `-as` value + call: {:?}", alias_refs);
}

/// Over-reach: a hash key in a method call's args that isn't a column
/// (or verb param) must NOT hijack the method. `ref_at` returns the method-call
/// ref because its span covers the args, but only the method-name token renames
/// the method — gated on `method_name_span`.
#[test]
fn arg_key_does_not_hijack_enclosing_method() {
    let src = "package U;\nuse base 'DBIx::Class::Core';\n\
        __PACKAGE__->add_columns(qw/id/);\n\
        sub go { my $self = shift; $self->search({ id => 1 }, { order_by => 'x' }); }\n1;\n";
    let fa = parse(src);
    let col = src.lines().nth(3).unwrap().find("order_by").unwrap();
    let resolved = resolve_symbol(&fa, tree_sitter::Point { row: 3, column: col }, None);
    assert!(
        !matches!(&resolved, Some(ResolvedTarget::Target(t)) if matches!(&t.kind, TargetKind::Method { .. })),
        "a non-column arg key must never resolve to the enclosing method: {resolved:?}",
    );
}

/// A DBIC column's single-hashref call args (`update`/`find`
/// /`search` with one `{ col => ... }`) are column-keyed — the keys are
/// `Class`-owned columns, not verb params — so renaming the column reaches them.
#[test]
fn dbic_column_rename_reaches_single_arg_call_keys() {
    let store = FileStore::new();
    let path = PathBuf::from("/tmp/dbic_argkey.pm");
    let src = "package U;\nuse base 'DBIx::Class::Core';\n\
        __PACKAGE__->add_columns(qw/id name/);\n\
        sub go { my $self = shift; $self->update({ name => 1 }); return $self->find({ name => 2 }); }\n1;\n";
    store.insert_workspace(path.clone(), parse(src));
    let fa = store.workspace_raw().get(&path).unwrap().value().clone();
    let col = src.lines().nth(2).unwrap().find("name").unwrap();
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } =
        resolve_symbol(&fa, tree_sitter::Point { row: 2, column: col }, None).expect("column resolves")
    else {
        panic!("expected a column attr Group")
    };
    let edits = group_rename_edits(
        &store, None, &FileKey::Path(path.clone()), &local_spans, &pinned_spans, &members, "RENAMED",
        RoleMask::EDITABLE,
    );
    let rows: std::collections::BTreeSet<usize> = edits.iter().map(|(l, _)| l.span.start.row).collect();
    // Column def (row 2) + the `update` and `find` arg keys (both row 3).
    assert!(rows.contains(&2), "column def renames: {rows:?}");
    assert!(rows.contains(&3), "single-arg update/find column keys join: {rows:?}");
    assert!(
        edits.iter().filter(|(l, _)| l.span.start.row == 3).count() >= 2,
        "both the update and find arg keys: {edits:?}",
    );
}

/// Multi-arg `search(\%cond, \%attrs)`: only the FIRST hashref (the column
/// conditions) is column-keyed — the trailing `\%attrs` hash (`order_by`/…) is
/// never walked, so renaming a column joins `\%cond` keys but never an attr.
#[test]
fn dbic_column_rename_multiarg_search_excludes_attrs_hash() {
    let store = FileStore::new();
    let path = PathBuf::from("/tmp/dbic_multiarg.pm");
    let src = "package U;\nuse base 'DBIx::Class::Core';\n\
        __PACKAGE__->add_columns(qw/id name/);\n\
        sub go { my $self = shift; $self->search({ name => 1 }, { order_by => 'id' }); }\n1;\n";
    store.insert_workspace(path.clone(), parse(src));
    let fa = store.workspace_raw().get(&path).unwrap().value().clone();
    let col = src.lines().nth(2).unwrap().find("name").unwrap();
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } =
        resolve_symbol(&fa, tree_sitter::Point { row: 2, column: col }, None).expect("column resolves")
    else {
        panic!("expected a column attr Group")
    };
    let edits = group_rename_edits(
        &store, None, &FileKey::Path(path.clone()), &local_spans, &pinned_spans, &members, "X",
        RoleMask::EDITABLE,
    );
    let lines: Vec<&str> = src.lines().collect();
    for (l, _) in &edits {
        let s = &lines[l.span.start.row][l.span.start.column..l.span.end.column];
        assert_eq!(s, "name", "only `\\%cond` column keys rename, never `order_by`: {edits:?}");
    }
    assert!(
        edits.iter().any(|(l, _)| l.span.start.row == 3),
        "the search `\\%cond` column key joins the column: {edits:?}",
    );
}

/// Fluent-verb chain off the VALID DBIC entry: `my $rs = $schema->resultset('User')`
/// types `$rs` to a `ResultSet<User>`; `$rs->search({col})` carries it (fluent),
/// `$rs->find({col})` joins the column key, and `my $u = $rs->find; $u->col`
/// (find→row→accessor through `RowOf`) does too. Renaming a column rewrites the
/// whole chain. `Class->search` is NOT valid DBIC (search lives on the resultset,
/// which only comes from the schema) and is deliberately not typed.
#[test]
fn dbic_column_rename_reaches_fluent_resultset_chain() {
    let store = FileStore::new();
    let path = PathBuf::from("/tmp/dbic_fluent.pm");
    let src = "package User;\nuse base 'DBIx::Class::Core';\n\
        __PACKAGE__->add_columns(qw/id name/);\n\
        sub go { my ($self, $schema) = @_; my $rs = $schema->resultset('User'); my $f = $rs->search({ name => 1 }); my $u = $rs->find({ name => 2 }); return $u->name; }\n1;\n";
    store.insert_workspace(path.clone(), parse(src));
    let fa = store.workspace_raw().get(&path).unwrap().value().clone();
    let col = src.lines().nth(2).unwrap().find("name").unwrap();
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } =
        resolve_symbol(&fa, tree_sitter::Point { row: 2, column: col }, None).expect("column resolves")
    else {
        panic!("expected a column attr Group")
    };
    let edits = group_rename_edits(
        &store, None, &FileKey::Path(path.clone()), &local_spans, &pinned_spans, &members, "RENAMED",
        RoleMask::EDITABLE,
    );
    let lines: Vec<&str> = src.lines().collect();
    for (l, _) in &edits {
        let s = &lines[l.span.start.row][l.span.start.column..l.span.end.column];
        assert_eq!(s, "name", "every edit hits a `name` token: {edits:?}");
    }
    // Row 3: the `search` arg key, the `$rs->find` arg key, AND the `$u->name`
    // accessor — all three resultset-chain sites join the column.
    assert!(
        edits.iter().filter(|(l, _)| l.span.start.row == 3).count() >= 3,
        "search arg + $rs->find arg + $u->name accessor all join: {edits:?}",
    );
}

/// A DBIC column is a `Bridged` key, not a hash slot. Renaming the column from
/// any spelling rewrites the accessor + the condition-arg keys but NEVER a
/// `$row->{col}` deref (undef in DBIC). And a cursor ON the deref doesn't resolve
/// to the column group — it's not a column reference at all.
#[test]
fn dbic_column_rename_excludes_row_hashref_deref() {
    let store = FileStore::new();
    let path = PathBuf::from("/tmp/dbic_deref.pm");
    let src = "package User;\n\
use base 'DBIx::Class::Core';\n\
__PACKAGE__->add_columns(qw/id name/);\n\
sub go {\n\
    my ($self, $schema) = @_;\n\
    my $rs = $schema->resultset('User');\n\
    my $row = $rs->find({ name => 1 });\n\
    my $bad = $row->{name};\n\
    return $row->name;\n\
}\n1;\n";
    store.insert_workspace(path.clone(), parse(src));
    let fa = store.workspace_raw().get(&path).unwrap().value().clone();
    let col = src.lines().nth(2).unwrap().find("name").unwrap();
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } =
        resolve_symbol(&fa, tree_sitter::Point { row: 2, column: col }, None).expect("column resolves")
    else {
        panic!("expected a column attr Group")
    };
    let edits = group_rename_edits(
        &store, None, &FileKey::Path(path.clone()), &local_spans, &pinned_spans, &members, "X",
        RoleMask::EDITABLE,
    );
    let rows: std::collections::BTreeSet<usize> = edits.iter().map(|(l, _)| l.span.start.row).collect();
    // def (2), find-condition arg (6), accessor (8) — NOT the `$row->{name}`
    // deref (7): a column isn't a hash slot.
    assert_eq!(rows, [2, 6, 8].into_iter().collect(), "deref row 7 excluded: {edits:?}");
    // A cursor on the `$row->{name}` deref is NOT the column group.
    let deref_col = src.lines().nth(7).unwrap().find("name").unwrap();
    assert!(
        !matches!(
            resolve_symbol(&fa, tree_sitter::Point { row: 7, column: deref_col }, None),
            Some(ResolvedTarget::Group { .. })
        ),
        "cursor on the $row->{{name}} deref must not resolve to the column",
    );
}

/// A Moo `has name` attribute's group includes the cross-file CONSTRUCTOR-arg
/// key (`Widget->new(name => …)`), owned `Sub{class,new}` — so renaming the
/// attribute reaches the ctor key, and a cursor on the ctor key renames the
/// whole group (including itself). The DBIC column-keyed seam must not hijack
/// the Moo ctor key to a `Class` owner (it's not a column).
#[test]
fn moo_attr_group_includes_cross_file_constructor_key() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;
    let store = FileStore::new();
    let lib = PathBuf::from("/tmp/moo_ctor_widget.pm");
    let app = PathBuf::from("/tmp/moo_ctor_app.pl");
    let lib_src = "package Widget;\nuse Moo;\nhas name => (is => 'rw');\nsub greet { my $self = shift; $self->name }\n1;\n";
    let app_src = "use Widget;\nmy $w = Widget->new(name => 'bob');\nprint $w->name;\n";
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(lib.clone(), Arc::new(parse(lib_src)));
    store.insert_workspace(lib.clone(), parse(lib_src));
    store.insert_workspace(app.clone(), parse(app_src));
    let lib_fa = store.workspace_raw().get(&lib).unwrap().value().clone();
    let col = lib_src.lines().nth(2).unwrap().find("name").unwrap();
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } =
        resolve_symbol(&lib_fa, tree_sitter::Point { row: 2, column: col }, Some(&idx)).expect("attr resolves")
    else {
        panic!("expected a Moo attr Group")
    };
    let edits = group_rename_edits(
        &store, Some(&idx), &FileKey::Path(lib.clone()), &local_spans, &pinned_spans, &members, "X",
        RoleMask::EDITABLE,
    );
    // app.pl row 1 is the `Widget->new(name => …)` ctor key.
    assert!(
        edits.iter().any(|(l, _)| matches!(&l.key, FileKey::Path(p) if p == &app) && l.span.start.row == 1),
        "attr rename must reach the cross-file constructor key: {edits:?}",
    );
}

/// A custom-named accessor that does NOT embed the attr (`predicate =>
/// 'has_size'` for attr `x`) is an independent method — a cursor on it must
/// rename IT, not the attr group (else the click silently renames a different
/// token). Only an embedding name (`has_size` for `size`) reverse-maps.
#[test]
fn non_embedding_mapped_accessor_renames_itself_not_the_attr() {
    let src = "package W;\nuse Moo;\nhas x => (is => 'rw', predicate => 'has_size');\n\
        sub probe { my $self = shift; return $self->has_size; }\n1;\n";
    let fa = parse(src);
    let col = src.lines().nth(3).unwrap().find("has_size").unwrap();
    let resolved = resolve_symbol(&fa, tree_sitter::Point { row: 3, column: col }, None);
    assert!(
        matches!(&resolved, Some(ResolvedTarget::Target(t)) if t.name == "has_size"),
        "cursor on the non-embedding `has_size` must rename has_size itself, not attr `x`: {resolved:?}",
    );
}

/// Inheritance: renaming a base-class attr reaches a SUBCLASS construction
/// (`Dog->new(name => …)` where `Dog extends Animal`). The base attr's ctor-key
/// member `HashKeyOfSub{Animal,new}` matches the subclass ctor key across `@ISA`.
#[test]
fn inherited_attr_rename_reaches_subclass_constructor_key() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;
    let store = FileStore::new();
    let animal = PathBuf::from("/tmp/inh_animal.pm");
    let dog = PathBuf::from("/tmp/inh_dog.pm");
    let app = PathBuf::from("/tmp/inh_run.pl");
    let animal_src = "package Animal;\nuse Moo;\nhas name => (is => 'rw');\n1;\n";
    let dog_src = "package Dog;\nuse Moo;\nextends 'Animal';\n1;\n";
    let app_src = "use Dog;\nmy $d = Dog->new(name => 'Rex');\nprint $d->name;\n";
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(animal.clone(), Arc::new(parse(animal_src)));
    idx.register_workspace_module(dog.clone(), Arc::new(parse(dog_src)));
    store.insert_workspace(animal.clone(), parse(animal_src));
    store.insert_workspace(dog.clone(), parse(dog_src));
    store.insert_workspace(app.clone(), parse(app_src));
    let animal_fa = store.workspace_raw().get(&animal).unwrap().value().clone();
    let col = animal_src.lines().nth(2).unwrap().find("name").unwrap();
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } =
        resolve_symbol(&animal_fa, tree_sitter::Point { row: 2, column: col }, Some(&idx)).expect("attr resolves")
    else {
        panic!("expected a Moo attr Group")
    };
    let edits = group_rename_edits(
        &store, Some(&idx), &FileKey::Path(animal.clone()), &local_spans, &pinned_spans, &members, "X",
        RoleMask::EDITABLE,
    );
    assert!(
        edits.iter().any(|(l, _)| matches!(&l.key, FileKey::Path(p) if p == &app) && l.span.start.row == 1),
        "base attr rename must reach the subclass `Dog->new(name => …)` ctor key: {edits:?}",
    );
}

/// A Corinna `field` is per-class PRIVATE storage — NOT inherited like a Moo
/// attr. Renaming a subclass's field (where an ancestor declares the same field
/// name) must stay in the subclass: the inheritance bridge must not widen
/// field-backed groups, and the reader is scoped precisely (not the family).
#[test]
fn corinna_field_subclass_does_not_bleed_to_ancestor() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;
    let store = FileStore::new();
    let base = PathBuf::from("/tmp/cor_base.pm");
    let deriv = PathBuf::from("/tmp/cor_deriv.pm");
    let base_src = "use v5.38;\nclass CBase { field $size :param :reader; method show { return $size; } }\n";
    let deriv_src = "use v5.38;\nclass CDeriv :isa(CBase) { field $size :param :reader; method other { return $size; } }\n";
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(base.clone(), Arc::new(parse(base_src)));
    idx.register_workspace_module(deriv.clone(), Arc::new(parse(deriv_src)));
    store.insert_workspace(base.clone(), parse(base_src));
    store.insert_workspace(deriv.clone(), parse(deriv_src));
    let deriv_fa = store.workspace_raw().get(&deriv).unwrap().value().clone();
    let col = deriv_src.lines().nth(1).unwrap().find("size").unwrap();
    let resolved = resolve_symbol(&deriv_fa, tree_sitter::Point { row: 1, column: col }, Some(&idx))
        .expect("field resolves");
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } = resolved else {
        panic!("expected a field Group, got {resolved:?}")
    };
    let edits = group_rename_edits(
        &store, Some(&idx), &FileKey::Path(deriv.clone()), &local_spans, &pinned_spans, &members, "vol",
        RoleMask::EDITABLE,
    );
    assert!(
        edits.iter().all(|(l, _)| matches!(&l.key, FileKey::Path(p) if p == &deriv)),
        "Corinna field rename must stay in the subclass, never touch the ancestor: {edits:?}",
    );
}

/// An OVERRIDDEN attribute (subclass redeclares the base's `has name`) renames
/// as ONE family under Hierarchy (the chosen scope) — from either class's decl
/// the edit set is identical and spans both decls + both classes' ctor keys.
/// Minting from the root-most declarer makes the two cursors symmetric.
#[test]
fn overridden_attr_renames_whole_family_symmetrically() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;
    let store = FileStore::new();
    let animal = PathBuf::from("/tmp/ov_animal.pm");
    let dog = PathBuf::from("/tmp/ov_dog.pm");
    let animal_src = "package Animal;\nuse Moo;\nhas name => (is => 'rw');\n1;\n";
    let dog_src = "package Dog;\nuse Moo;\nextends 'Animal';\nhas name => (is => 'rw');\n1;\n";
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(animal.clone(), Arc::new(parse(animal_src)));
    idx.register_workspace_module(dog.clone(), Arc::new(parse(dog_src)));
    store.insert_workspace(animal.clone(), parse(animal_src));
    store.insert_workspace(dog.clone(), parse(dog_src));

    let edits_from = |path: &PathBuf, src: &str, row: usize| {
        let fa = store.workspace_raw().get(path).unwrap().value().clone();
        let col = src.lines().nth(row).unwrap().find("name").unwrap();
        let r = resolve_symbol(&fa, tree_sitter::Point { row, column: col }, Some(&idx)).unwrap();
        let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } = r else {
            panic!("expected Group, got {r:?}")
        };
        let mut got: Vec<String> = group_rename_edits(
            &store, Some(&idx), &FileKey::Path(path.clone()), &local_spans, &pinned_spans, &members, "X",
            RoleMask::EDITABLE,
        )
        .iter()
        .map(|(l, _)| match &l.key {
            FileKey::Path(p) => format!("{}:{}", p.file_name().unwrap().to_string_lossy(), l.span.start.row),
            FileKey::Url(u) => format!("{u}:{}", l.span.start.row),
        })
        .collect();
        got.sort();
        got.dedup();
        got
    };
    let from_animal = edits_from(&animal, animal_src, 2);
    let from_dog = edits_from(&dog, dog_src, 3);
    assert_eq!(from_animal, from_dog, "override rename is symmetric (family)");
    assert!(
        from_animal.iter().any(|s| s.starts_with("ov_animal.pm"))
            && from_animal.iter().any(|s| s.starts_with("ov_dog.pm")),
        "the family spans both class decls: {from_animal:?}",
    );
}

/// A multi-key condition hashref links EVERY column key, not just the first.
/// Perl right-nests the tail pairs of `{ a => 1, b => 2, c => 3 }`; walking only
/// the top-level children would see only `a`. (`cst::pair_nodes` flattens it.)
#[test]
fn dbic_multi_key_hashref_links_all_columns() {
    let store = FileStore::new();
    let path = PathBuf::from("/tmp/dbic_multikey.pm");
    let src = "package U;\nuse base 'DBIx::Class::Core';\n\
        __PACKAGE__->add_columns(qw/alpha beta gamma/);\n\
        sub go { my $self = shift; $self->search({ alpha => 1, beta => 2, gamma => 3 }); }\n1;\n";
    store.insert_workspace(path.clone(), parse(src));
    let fa = store.workspace_raw().get(&path).unwrap().value().clone();
    // Rename the MIDDLE column `beta` — it must reach the search arg key (row 3).
    let col = src.lines().nth(2).unwrap().find("beta").unwrap();
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } =
        resolve_symbol(&fa, tree_sitter::Point { row: 2, column: col }, None).expect("column resolves")
    else {
        panic!("expected a column attr Group")
    };
    let edits = group_rename_edits(
        &store, None, &FileKey::Path(path.clone()), &local_spans, &pinned_spans, &members, "X",
        RoleMask::EDITABLE,
    );
    assert!(
        edits.iter().any(|(l, _)| l.span.start.row == 3),
        "the 2nd-key `beta` search arg must link to the column: {edits:?}",
    );
}

/// A method dispatched through a scalar (`my $m='poke'; $self->$m()`) is a
/// reference to the method but NOT a literal name — rename must skip it (else it
/// rewrites the `$m` variable and corrupts the dispatch), while references lists
/// it. Same rewritable split as folded handlers, now for callables.
#[test]
fn folded_method_dispatch_site_is_non_rewritable() {
    let store = FileStore::new();
    let path = PathBuf::from("/tmp/folded_dispatch.pm");
    let src = "package D;\nsub poke { my $self = shift; return 1; }\n\
        sub run { my $self = shift; my $m = 'poke'; return $self->$m(); }\n1;\n";
    store.insert_workspace(path.clone(), parse(src));
    let fa = store.workspace_raw().get(&path).unwrap().value().clone();
    let resolved = resolve_symbol(&fa, tree_sitter::Point { row: 1, column: 4 }, None)
        .expect("sub poke resolves");
    let ResolvedTarget::Target(t) = resolved else { panic!("expected a Target, got {resolved:?}") };
    let refs = refs_to(&store, None, &t, RoleMask::EDITABLE);
    // The `$self->$m()` site (row 2) is present but frozen; the decl is rewritable.
    let lines: Vec<&str> = src.lines().collect();
    let folded = refs.iter().find(|r| {
        lines[r.span.start.row][r.span.start.column..r.span.end.column].starts_with("$m")
    });
    let folded = folded.expect("the folded $m dispatch site is a reference");
    assert!(!folded.rewritable, "the folded dispatch site must NOT be rewritten: {folded:?}");
    assert!(
        refs.iter().any(|r| r.rewritable),
        "the `sub poke` decl must still be rewritable: {refs:?}",
    );
}

/// Const-fold rename provenance (`Ref.folded_from`): renaming the method
/// `poke` rewrites the SOURCE string literal `my $m = 'poke'` — the folded
/// call site `$self->$m()` is a non-rewritable variable read, so the literal
/// is where the new name has to land (else the rename silently drops the
/// dispatch's only spelling of the name).
#[test]
fn folded_method_dispatch_rewrites_source_literal() {
    let store = FileStore::new();
    let path = PathBuf::from("/tmp/folded_source.pm");
    let src = "package D;\nsub poke { my $self = shift; return 1; }\n\
        sub run { my $self = shift; my $m = 'poke'; return $self->$m(); }\n1;\n";
    store.insert_workspace(path.clone(), parse(src));
    let fa = store.workspace_raw().get(&path).unwrap().value().clone();
    let resolved = resolve_symbol(&fa, tree_sitter::Point { row: 1, column: 4 }, None)
        .expect("sub poke resolves");
    let ResolvedTarget::Target(t) = resolved else { panic!("expected a Target, got {resolved:?}") };
    let refs = refs_to(&store, None, &t, RoleMask::EDITABLE);
    let lines: Vec<&str> = src.lines().collect();
    let span_text = |r: &RefLocation| &lines[r.span.start.row][r.span.start.column..r.span.end.column];
    // The source literal `'poke'` (row 2) must be a rewritable edit covering
    // exactly the inside-the-quotes name, distinct from the `$m` call token.
    let source_edit = refs.iter().find(|r| {
        r.span.start.row == 2 && r.rewritable && span_text(r) == "poke"
    });
    assert!(
        source_edit.is_some(),
        "renaming `poke` must rewrite the source literal `'poke'` on row 2: {refs:?}",
    );
    // The folded `$self->$m()` site stays frozen (renaming it corrupts `$m`).
    let folded = refs.iter().find(|r| span_text(r).starts_with("$m"));
    assert!(
        folded.is_some_and(|r| !r.rewritable),
        "the folded `$m` dispatch site must NOT be rewritten: {refs:?}",
    );
}

/// Over-reach guard: a class that defines its OWN `sub <verb>` (shadowing a
/// DBIC column-keyed verb name) is NOT column-keyed — the call dispatches to the
/// user's method, whose hash arg isn't columns. Renaming the column must not
/// touch that custom method's `{ col => … }` arg key.
#[test]
fn dbic_custom_sub_shadowing_verb_is_not_column_keyed() {
    let store = FileStore::new();
    let path = PathBuf::from("/tmp/dbic_shadow.pm");
    let src = "package Tag;\nuse base 'DBIx::Class::Core';\n\
        __PACKAGE__->add_columns(qw/id name/);\n\
        sub create { my ($self, $args) = @_; return $args->{name}; }\n\
        sub go { my $self = shift; $self->create({ name => 'x' }); }\n1;\n";
    store.insert_workspace(path.clone(), parse(src));
    let fa = store.workspace_raw().get(&path).unwrap().value().clone();
    let col = src.lines().nth(2).unwrap().find("name").unwrap();
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } =
        resolve_symbol(&fa, tree_sitter::Point { row: 2, column: col }, None).expect("column resolves")
    else {
        panic!("expected a column attr Group")
    };
    let edits = group_rename_edits(
        &store, None, &FileKey::Path(path.clone()), &local_spans, &pinned_spans, &members, "X",
        RoleMask::EDITABLE,
    );
    assert!(
        edits.iter().all(|(l, _)| l.span.start.row != 4),
        "custom `sub create`'s arg key (row 4) must NOT be column-keyed: {edits:?}",
    );
}

/// Over-reach guard: column-keying narrows to POSITIONAL arg 0. A
/// `search($cond, \%attrs)` (scalar cond) has no inline column keys — the
/// trailing `\%attrs` hash must never be walked as if it were the conditions.
#[test]
fn dbic_scalar_cond_does_not_column_key_attrs_hash() {
    let store = FileStore::new();
    let path = PathBuf::from("/tmp/dbic_scalarcond.pm");
    let src = "package Row;\nuse base 'DBIx::Class::Core';\n\
        __PACKAGE__->add_columns(qw/id name/);\n\
        sub go { my ($self, $cond) = @_; $self->search($cond, { name => 'attrs' }); }\n1;\n";
    store.insert_workspace(path.clone(), parse(src));
    let fa = store.workspace_raw().get(&path).unwrap().value().clone();
    let col = src.lines().nth(2).unwrap().find("name").unwrap();
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } =
        resolve_symbol(&fa, tree_sitter::Point { row: 2, column: col }, None).expect("column resolves")
    else {
        panic!("expected a column attr Group")
    };
    let edits = group_rename_edits(
        &store, None, &FileKey::Path(path.clone()), &local_spans, &pinned_spans, &members, "X",
        RoleMask::EDITABLE,
    );
    assert!(
        edits.iter().all(|(l, _)| l.span.start.row != 3),
        "the `\\%attrs` hash key (row 3) must NOT be walked when cond is a scalar: {edits:?}",
    );
}

/// Cross-file: a column rename reaches a consumer's `Class->search({ col => … })`
/// arg key (columns defined in another file). The consumer emits the key with a
/// deferred owner; the column-keyed-verb seam mints `Class` at query time so it
/// joins the group, both directions.
#[test]
fn dbic_column_rename_reaches_cross_file_consumer_arg_key() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;
    let store = FileStore::new();
    let lib = PathBuf::from("/tmp/dbic_user.pm");
    let app = PathBuf::from("/tmp/dbic_app.pl");
    let lib_src = "package User;\nuse base 'DBIx::Class::Core';\n__PACKAGE__->add_columns(qw/id name/);\n1;\n";
    let app_src = "use User;\nmy $rs = User->search({ name => 'x' });\n";
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(lib.clone(), Arc::new(parse(lib_src)));
    store.insert_workspace(lib.clone(), parse(lib_src));
    store.insert_workspace(app.clone(), parse(app_src));
    let lib_fa = store.workspace_raw().get(&lib).unwrap().value().clone();
    let col = lib_src.lines().nth(2).unwrap().find("name").unwrap();
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } =
        resolve_symbol(&lib_fa, tree_sitter::Point { row: 2, column: col }, Some(&idx)).expect("column resolves")
    else {
        panic!("expected a column attr Group")
    };
    let edits = group_rename_edits(
        &store, Some(&idx), &FileKey::Path(lib.clone()), &local_spans, &pinned_spans, &members, "X",
        RoleMask::EDITABLE,
    );
    assert!(
        edits.iter().any(|(l, _)| matches!(&l.key, FileKey::Path(p) if p == &app)),
        "column rename reaches the cross-file `User->search({{ name }})` arg key: {edits:?}",
    );
}

/// Owner-gating (H7-6): a synthesized column accessor's identity is the OWNING
/// class, not the bare name. Renaming one class's `id` column must NOT reach a
/// framework ancestor's real `sub id` (`DBIx::Class::PK::id`) nor an unrelated
/// SIBLING class that carries its own independent `id` column — both merely
/// share the name. Rooting the accessor family at the owner (not the topmost
/// same-named ancestor) is what keeps the edit set to the owner + its typed
/// call sites. The owner's own `$self->id` call DOES edit.
#[test]
fn dbic_column_rename_owner_gated_excludes_framework_and_siblings() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;
    let store = FileStore::new();
    let base = PathBuf::from("/tmp/owngate_base.pm");
    let widget = PathBuf::from("/tmp/owngate_widget.pm");
    let gadget = PathBuf::from("/tmp/owngate_gadget.pm");
    // A framework-ish base with a real generic `sub id` — the name-collision
    // bait (stands in for `DBIx::Class::PK::id`).
    let base_src = "package MyBase;\nsub id { my $self = shift; return $self->{_id}; }\n1;\n";
    // Two independent DBIC subclasses (direct `DBIx::Class::Core` base so
    // columns synthesize), each ALSO inheriting the framework `id` via MyBase,
    // and each with its OWN `id` column. `$self->id` in Widget is a typed
    // owner-receiver call.
    let widget_src = "package Widget;\nuse base ('DBIx::Class::Core', 'MyBase');\n\
        __PACKAGE__->add_columns(qw/id name/);\n\
        sub go { my $self = shift; return $self->id; }\n1;\n";
    let gadget_src = "package Gadget;\nuse base ('DBIx::Class::Core', 'MyBase');\n\
        __PACKAGE__->add_columns(qw/id/);\n1;\n";
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(base.clone(), Arc::new(parse(base_src)));
    idx.register_workspace_module(widget.clone(), Arc::new(parse(widget_src)));
    idx.register_workspace_module(gadget.clone(), Arc::new(parse(gadget_src)));
    store.insert_workspace(base.clone(), parse(base_src));
    store.insert_workspace(widget.clone(), parse(widget_src));
    store.insert_workspace(gadget.clone(), parse(gadget_src));

    let widget_fa = store.workspace_raw().get(&widget).unwrap().value().clone();
    let col = widget_src.lines().nth(2).unwrap().find("id").unwrap();
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } =
        resolve_symbol(&widget_fa, tree_sitter::Point { row: 2, column: col }, Some(&idx))
            .expect("column resolves to a group")
    else {
        panic!("expected a column attr Group")
    };
    let edits = group_rename_edits(
        &store, Some(&idx), &FileKey::Path(widget.clone()), &local_spans, &pinned_spans,
        &members, "renamed", RoleMask::EDITABLE,
    );
    let touched: std::collections::BTreeSet<&PathBuf> = edits
        .iter()
        .filter_map(|(l, _)| match &l.key { FileKey::Path(p) => Some(p), _ => None })
        .collect();
    assert!(
        !touched.contains(&base),
        "framework ancestor's real `sub id` must be untouched: {touched:?}",
    );
    assert!(
        !touched.contains(&gadget),
        "unrelated sibling class's own `id` column must be untouched: {touched:?}",
    );
    // The owner's decl + its typed `$self->id` call both edit.
    let widget_rows: std::collections::BTreeSet<usize> = edits
        .iter()
        .filter(|(l, _)| matches!(&l.key, FileKey::Path(p) if p == &widget))
        .map(|(l, _)| l.span.start.row)
        .collect();
    assert!(widget_rows.contains(&2), "owner column decl (row 2) edits: {edits:?}");
    assert!(
        widget_rows.contains(&3),
        "owner-typed `$self->id` call (row 3) edits: {edits:?}",
    );
}

/// Event (Handler) rename. A literal event-name site is rewritable
/// and its span is the **inside-the-quotes** name (so rename keeps the quotes);
/// a folded site — variable (`my $e='connect'; on($e)`) OR constant
/// (`use constant EVT=>'connect'; on(EVT)`) — whose span IS that other
/// identifier is a reference but NOT rewritable (references lists it, rename
/// skips it, so it can't corrupt the variable/constant). `refs_to` carries the
/// distinction.
#[test]
fn test_event_handler_refs_mark_folded_site_non_rewritable() {
    let store = FileStore::new();
    let path = PathBuf::from("/tmp/ev_handler.pm");
    let src = "package App;\n\
         use parent 'Mojo::EventEmitter';\n\
         use constant EVT => 'connect';\n\
         sub setup {\n\
         my $self = shift;\n\
         $self->on('connect', sub { 1 });\n\
         my $e = 'connect';\n\
         $self->on($e, sub { 1 });\n\
         $self->on(EVT, sub { 1 });\n\
         $self->emit('connect');\n\
         }\n1;\n";
    store.insert_workspace(path.clone(), parse(src));

    let target = TargetRef {
        name: "connect".to_string(),
        kind: TargetKind::Handler {
            owner: crate::model::file_analysis::HandlerOwner::Class("App".to_string()),
            name: "connect".to_string(),
        },
        method_classes: Vec::new(), scope: OverrideScope::Dispatch, def_paths: Vec::new(), bare_constant: false,
            ctor_of: None,
            class_ns: None,
            member_shape: Default::default(),
    };
    assert!(target.supports_cross_file_rename(), "Handler renames cross-file now");

    let refs = refs_to(&store, None, &target, RoleMask::EDITABLE);
    let lines: Vec<&str> = src.lines().collect();
    let mut rewritable = 0;
    let mut frozen = std::collections::BTreeSet::new();
    for r in &refs {
        let slice = &lines[r.span.start.row][r.span.start.column..r.span.end.column];
        if r.rewritable {
            // Quote-preservation: the rewrite is the bare name, never `'connect'`.
            assert_eq!(slice, "connect", "rewritable site must be the inner name: {r:?}");
            rewritable += 1;
        } else {
            frozen.insert(slice.to_string());
        }
    }
    assert_eq!(rewritable, 2, "the two literal `'connect'` sites: {refs:?}");
    assert_eq!(
        frozen,
        ["$e", "EVT"].iter().map(|s| s.to_string()).collect(),
        "the variable AND constant folds are frozen (kept, not rewritten): {refs:?}",
    );
}

/// A DBIC column's accessor (`$row->name`) and its key uses
/// (`search({name=>…})`, `$row->{name}`) are one renameable unit. The
/// synthesized accessor Method + the same-span `Class`-owned column HashKeyDef
/// form an attr group whose `HashKeyOfBridged` member catches the key uses
/// (`found_by` reaches the `Sub{class, verb}`-owned search args), so rename
/// from either face rewrites both. Before, accessor → `Method` and column key
/// → `HashKeyOfBridged` were disjoint.
#[test]
fn test_dbic_column_accessor_and_key_form_one_group() {
    let fa = parse(
        "package Schema::Result::User;\n\
         use base 'DBIx::Class::Core';\n\
         __PACKAGE__->add_columns(qw/id name/);\n\
         1;\n",
    );
    // Cursor on the synthesized accessor / column `name` token (row 2).
    let col = "__PACKAGE__->add_columns(qw/id ".len();
    let resolved = resolve_symbol(&fa, tree_sitter::Point { row: 2, column: col }, None)
        .expect("column resolves");
    let ResolvedTarget::Group { members, .. } = resolved else {
        panic!("expected a column attr Group, got {:?}", resolved);
    };
    assert!(
        members.iter().any(|m| matches!(m.target.kind, TargetKind::Method { .. })),
        "group carries the accessor Method member: {:?}",
        members,
    );
    assert!(
        members.iter().any(|m| matches!(m.target.kind, TargetKind::HashKeyOfBridged(_))),
        "group carries the HashKeyOfBridged member (search/deref keys): {:?}",
        members,
    );
}

/// Reverse direction: a consumer-side cursor on a name-mapped
/// accessor (`$w->has_size`) OR an internal slot (`$w->{size}`) resolves to
/// the same cross-file attr group as the decl — so rename from any spelling
/// rewrites every spelling. Before, the slot fell to a plain
/// `HashKeyOfBridged` (single-file, missed the accessors) and the mapped
/// accessor only matched itself + the decl.
#[test]
fn test_consumer_mapped_accessor_and_slot_resolve_to_attr_group_cross_file() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    let class_path = PathBuf::from("/tmp/grp_rev_widget.pm");
    let user_path = PathBuf::from("/tmp/grp_rev_user.pl");
    let class_src =
        "package Widget;\nuse Moo;\nhas size => (is => 'rw', predicate => 1, clearer => 1);\n1;\n";
    let user_src = "use Widget;\n\
         my $w = Widget->new(size => 3);\n\
         $w->has_size;\n\
         my $d = $w->{size};\n";

    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(class_path.clone(), Arc::new(parse(class_src)));
    let store = FileStore::new();
    store.insert_workspace(class_path.clone(), parse(class_src));
    store.insert_workspace(user_path.clone(), parse(user_src));

    let consumer = store.workspace_raw().get(&user_path).unwrap().value().clone();

    // Both spellings must mint the remote group (pinned to the class file) and
    // carry the accessor / ctor-key members, so a rename fans out across both.
    for (label, row, needle) in [("mapped accessor", 2usize, "has_size"), ("internal slot", 3, "{size}")] {
        let col = user_src.lines().nth(row).unwrap().find(needle).unwrap()
            + if needle.starts_with('{') { 1 } else { 0 };
        let resolved = resolve_symbol(&consumer, tree_sitter::Point { row, column: col }, Some(&idx))
            .unwrap_or_else(|| panic!("{label} cursor should resolve"));
        let ResolvedTarget::Group { pinned_spans, members, .. } = resolved else {
            panic!("{label}: expected remote Group, got {:?}", resolved);
        };
        assert!(!pinned_spans.is_empty(), "{label}: group pinned to the class file");
        assert!(
            members.iter().any(|m| !m.target.method_classes.is_empty()
                || matches!(m.target.kind, TargetKind::Method { .. } | TargetKind::HashKeyOfSub { .. })),
            "{label}: group carries accessor/ctor-key members: {:?}",
            members,
        );
    }
}

/// Internal slot pokes join the group cross-file: a subclass (or any
/// promiscuous consumer) reaching into `$self->{size}` renames with the
/// attr — under STRICT Class-owner matching, so another sub's
/// `(size => 1)` arg keys in unrelated classes stay out.
#[test]
fn test_internal_slot_pokes_join_group_cross_file() {
    let store = FileStore::new();
    let class_path = PathBuf::from("/tmp/grp_slot_widget.pm");
    let sub_path = PathBuf::from("/tmp/grp_slot_subclass.pm");
    store.insert_workspace(
        class_path.clone(),
        parse("package Widget;\nuse Moo;\nhas size => (is => 'rw');\n1;\n"),
    );
    // Subclass pokes the parent's slot directly — classic promiscuous Perl.
    store.insert_workspace(
        sub_path.clone(),
        parse("package Gadget;\nuse Moo;\nextends 'Widget';\nsub poke { my ($self) = @_; return $self->{size}; }\n1;\n"),
    );

    let class_fa = store.workspace_raw().get(&class_path).unwrap().value().clone();
    let resolved = resolve_symbol(&class_fa, tree_sitter::Point { row: 2, column: 4 }, None)
        .expect("attr decl resolves");
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } = resolved else {
        panic!("expected Group, got {:?}", resolved);
    };
    assert!(
        members.iter().any(|m| matches!(m.target.kind, TargetKind::InternalHashKey { .. })),
        "internal-key member minted: {:?}",
        members,
    );
    let edits = group_rename_edits(
        &store,
        None,
        &FileKey::Path(class_path.clone()),
        &local_spans,
        &pinned_spans,
        &members,
        "extent",
        RoleMask::EDITABLE,
    );
    assert!(
        edits.iter().any(|(l, t)| {
            matches!(&l.key, FileKey::Path(p) if p == &sub_path) && t == "extent"
        }),
        "subclass slot poke renamed; edits: {:?}",
        edits,
    );
}

/// Old-school `bless { key => ... }, $class` keys are instance slots of the
/// class — renaming the bless key must reach every `$self->{key}` access,
/// symmetric with renaming from an access. Before the `InternalKey`
/// projection on bless keys, the from-key direction minted only the strict
/// `HashKeyOfSub{C, new}` member (matching the bless key alone) and missed
/// the `Class(C)`-owned accesses; the from-access direction worked via
/// `found_by`. This pins the symmetry.
#[test]
fn rename_from_bless_key_reaches_self_slot_accesses() {
    let store = FileStore::new();
    let path = PathBuf::from("/tmp/bless_slot.pm");
    let src = "package Calc;\n\
         sub new { my ($class) = @_; return bless { history => [] }, $class; }\n\
         sub add { my ($self) = @_; push @{$self->{history}}, 1; }\n\
         sub log { my ($self) = @_; return $self->{history}; }\n1;\n";
    store.insert_workspace(path.clone(), parse(src));

    let fa = store.workspace_raw().get(&path).unwrap().value().clone();
    // Cursor on the bless key `history` (row 1, inside `bless { history`).
    let key_col = src.lines().nth(1).unwrap().find("history").unwrap();
    let resolved = resolve_symbol(&fa, tree_sitter::Point { row: 1, column: key_col }, None)
        .expect("bless key resolves");
    let ResolvedTarget::Group { local_spans, pinned_spans, members, .. } = resolved else {
        panic!("expected Group, got {:?}", resolved);
    };
    assert!(
        members.iter().any(|m| matches!(m.target.kind, TargetKind::InternalHashKey { .. })),
        "bless key must mint an internal-key member so $self->{{history}} accesses join: {:?}",
        members,
    );
    let edits = group_rename_edits(
        &store, None, &FileKey::Path(path.clone()),
        &local_spans, &pinned_spans, &members, "log",
        RoleMask::EDITABLE,
    );
    // bless key (1) + two $self->{history} accesses (rows 2, 3) = 3 spellings.
    assert_eq!(
        edits.len(), 3,
        "bless key + both $self->{{history}} accesses should rename; edits: {:?}",
        edits,
    );
}

#[test]
fn test_implementations_of_role_requires_fans_out_to_composers() {
    use crate::index::module_index::{CachedModule, ModuleIndex};
    use std::sync::Arc;

    let idx = ModuleIndex::new_for_test();
    let insert = |name: &str, src: &str| {
        let analysis = Arc::new(parse(src));
        idx.insert_cache(
            name,
            Some(Arc::new(CachedModule::new(
                PathBuf::from(format!("/fake/{}.pm", name.replace("::", "/"))),
                analysis,
            ))),
        );
    };
    insert("My::Role", "package My::Role;\nuse Moo::Role;\nrequires 'fetch';\n1;\n");
    insert(
        "My::Composer",
        "package My::Composer;\nuse Moo;\nwith 'My::Role';\nsub fetch { 42 }\n1;\n",
    );
    // Role-composing-role: re-requires the contract (a marker, not an
    // implementation) and adds a transitive hop to reach My::Deep.
    insert(
        "My::SubRole",
        "package My::SubRole;\nuse Moo::Role;\nwith 'My::Role';\nrequires 'fetch';\n1;\n",
    );
    insert("My::Deep", "package My::Deep;\nuse Moo;\nwith 'My::SubRole';\nsub fetch { 7 }\n1;\n");

    let target = TargetRef {
        name: "fetch".to_string(),
        kind: TargetKind::Method { class: "My::Role".to_string() },
        method_classes: Vec::new(), scope: OverrideScope::Dispatch, def_paths: Vec::new(), bare_constant: false,
            ctor_of: None,
            class_ns: None,
            member_shape: Default::default(),
    };
    let origin = parse("package Probe;\n1;\n");
    let results = implementations_of(&origin, Some(&idx), &target);
    let files: Vec<String> = results
        .iter()
        .map(|r| match &r.key {
            FileKey::Path(p) => p.display().to_string(),
            FileKey::Url(u) => u.to_string(),
        })
        .collect();
    assert_eq!(
        files,
        vec!["/fake/My/Composer.pm", "/fake/My/Deep.pm"],
        "direct + transitive composer defs, sorted; the SubRole re-requires marker excluded",
    );

    // Non-Method targets have no descendant-implementation semantics.
    let pkg_target = TargetRef::new("My::Role".to_string(), TargetKind::Package);
    assert!(implementations_of(&origin, Some(&idx), &pkg_target).is_empty());
}

/// A method override that lives on a SIBLING PARENT of a shared descendant
/// (Perl multi-parent composition — `use base qw(Mixin Base)`, Moo `with`,
/// DBIC `load_components`). The mixin is an ancestor of a concrete descendant
/// of the target class yet is NOT itself a descendant of it, so the plain
/// INHERITS_INV descendant sweep never reaches it. `implementations_of` must
/// still surface it (H7-7: DBIC `Row::update` overridden by `Ordered` etc.).
#[test]
fn test_implementations_finds_mixin_sibling_override() {
    use crate::index::module_index::{CachedModule, ModuleIndex};
    use std::sync::Arc;

    let idx = ModuleIndex::new_for_test();
    let insert = |name: &str, src: &str| {
        let analysis = Arc::new(parse(src));
        idx.insert_cache(
            name,
            Some(Arc::new(CachedModule::new(
                PathBuf::from(format!("/fake/{}.pm", name.replace("::", "/"))),
                analysis,
            ))),
        );
    };
    // Base defines the contract method; Mixin overrides it WITHOUT inheriting
    // Base; Child assembles its dispatch from both (Mixin ahead of Base).
    insert("Base", "package Base;\nsub save { 1 }\n1;\n");
    insert("Mixin", "package Mixin;\nsub save { 2 }\n1;\n");
    insert("Child", "package Child;\nuse base qw(Mixin Base);\n1;\n");

    let target = TargetRef {
        name: "save".to_string(),
        kind: TargetKind::Method { class: "Base".to_string() },
        method_classes: Vec::new(), scope: OverrideScope::Dispatch, def_paths: Vec::new(), bare_constant: false,
            ctor_of: None,
            class_ns: None,
            member_shape: Default::default(),
    };
    let origin = parse("package Probe;\n1;\n");
    let files: Vec<String> = implementations_of(&origin, Some(&idx), &target)
        .iter()
        .map(|r| match &r.key {
            FileKey::Path(p) => p.display().to_string(),
            FileKey::Url(u) => u.to_string(),
        })
        .collect();
    assert_eq!(
        files,
        vec!["/fake/Mixin.pm"],
        "the sibling-parent override is an implementation; Base (the contract) is excluded",
    );
}

/// H7-9: `references` on a framework verb (`belongs_to`/`has_many`/…) defined
/// in a component/mixin ancestor, seeded from a cursor ON the `sub` decl, must
/// reach every `__PACKAGE__->verb(...)` call site in descendant classes across
/// files. The receiver of each call is `__PACKAGE__` (the descendant class),
/// which `isa` the verb's owner only through a CROSS-FILE parent edge — the
/// matcher must confirm that ancestry via the index, exactly as the real DBIC
/// `CD → BaseResult → …Core → …Relationship::BelongsTo` chain does.
#[test]
fn refs_to_package_verb_reaches_cross_file_call_sites() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    let store = FileStore::new();
    let idx = ModuleIndex::new_for_test();

    let comp_path = PathBuf::from("/tmp/h9/Comp.pm");
    let a_path = PathBuf::from("/tmp/h9/ClassA.pm");
    let b_path = PathBuf::from("/tmp/h9/ClassB.pm");

    // Component defines the verb. Two classes inherit it and invoke it via
    // `__PACKAGE__->verb(...)` — the invocant is the descendant class, which
    // reaches Comp only cross-file.
    let comp_src = "package Comp;\nsub my_rel { my ($class, $rel, $target) = @_; 1 }\n1;\n";
    let a_src = "package ClassA;\nuse base 'Comp';\n__PACKAGE__->my_rel( alpha => 'ClassA::Alpha' );\n1;\n";
    let b_src = "package ClassB;\nuse base 'Comp';\n__PACKAGE__->my_rel( beta => 'ClassB::Beta' );\n1;\n";

    for (path, src) in [(&comp_path, comp_src), (&a_path, a_src), (&b_path, b_src)] {
        store.insert_workspace(path.clone(), parse(src));
        idx.register_workspace_module(path.clone(), Arc::new(parse(src)));
    }

    // Seed the query exactly as the CLI does: cursor ON the `sub my_rel` decl
    // token in Comp → `resolve()` → the `references()` projection.
    let origin = parse(comp_src);
    let decl_col = comp_src.lines().nth(1).unwrap().find("my_rel").unwrap() + 1;
    let cs = resolve(
        &store,
        &origin,
        FileKey::Path(comp_path.clone()),
        tree_sitter::Point { row: 1, column: decl_col },
        Some(&idx),
        OverrideScope::default(),
    );
    let refs = cs.references();
    let files: std::collections::HashSet<String> = refs
        .iter()
        .map(|r| match &r.key {
            FileKey::Path(p) => p.file_name().unwrap().to_str().unwrap().to_string(),
            FileKey::Url(u) => u.to_string(),
        })
        .collect();

    assert!(files.contains("Comp.pm"), "verb decl missing: {:?}", files);
    assert!(
        files.contains("ClassA.pm"),
        "__PACKAGE__->my_rel in ClassA (cross-file descendant) dropped: {:?}",
        files,
    );
    assert!(
        files.contains("ClassB.pm"),
        "__PACKAGE__->my_rel in ClassB (cross-file descendant) dropped: {:?}",
        files,
    );
}

/// The implementations verb is seeded by a cursor ON a `sub NAME` decl too —
/// which resolves to `Sub{package: Some(class)}`, not `Method{class}`. Perl
/// has no sub/method distinction, so a package sub is a dispatch root exactly
/// like a method-call target. (H7-7: the DBIC probe cursor sits on
/// `sub update` in `DBIx::Class::Row`, a plain `sub`.)
#[test]
fn test_implementations_on_sub_decl_target_finds_overrides() {
    use crate::index::module_index::{CachedModule, ModuleIndex};
    use std::sync::Arc;

    let idx = ModuleIndex::new_for_test();
    let insert = |name: &str, src: &str| {
        let analysis = Arc::new(parse(src));
        idx.insert_cache(
            name,
            Some(Arc::new(CachedModule::new(
                PathBuf::from(format!("/fake/{}.pm", name.replace("::", "/"))),
                analysis,
            ))),
        );
    };
    insert("Base", "package Base;\nsub save { 1 }\n1;\n");
    insert("Sub1", "package Sub1;\nuse base qw(Base);\nsub save { 2 }\n1;\n");

    let target = TargetRef {
        name: "save".to_string(),
        kind: TargetKind::Sub { package: Some("Base".to_string()) },
        method_classes: Vec::new(), scope: OverrideScope::Dispatch, def_paths: Vec::new(), bare_constant: false,
            ctor_of: None,
            class_ns: None,
            member_shape: Default::default(),
    };
    let origin = parse("package Probe;\n1;\n");
    let files: Vec<String> = implementations_of(&origin, Some(&idx), &target)
        .iter()
        .map(|r| match &r.key {
            FileKey::Path(p) => p.display().to_string(),
            FileKey::Url(u) => u.to_string(),
        })
        .collect();
    assert_eq!(
        files,
        vec!["/fake/Sub1.pm"],
        "a plain-subclass override, reached from a Sub-package decl target",
    );
}

/// The pack-language backward lanes: def→uses on the SAME key the forward
/// (use→def) resolutions use — enum constants / members (Method{class}),
/// macros + globals (FileScopeValue), delegation aliases.
#[cfg(feature = "cpp")]
mod pack_symmetry {
    use super::*;
    use crate::model::file_analysis::AccessKind;

    /// THE cross-language symmetry invariant, cpp instance: the origin's
    /// include-closure — ONE construction fact on the set — moves goto-def,
    /// references, AND completion gathering together. A closure-bearing
    /// origin resolves the enum constant in its included header, walks a
    /// references image that EXCLUDES a closure-disconnected file's
    /// same-named token, and gathers the header's names as completion
    /// candidates. Strip the closure (the same knob, nothing per-feature)
    /// and the projections move coherently: no completion universe, and no
    /// visibility identity to gate the backward walk with.
    #[test]
    fn closure_visibility_axis_flows_to_every_cpp_projection() {
        use std::sync::Arc;
        let header_src = "enum opcode { OP_VIS_A, OP_VIS_B };\n";
        let use_src = "int f(int t) {\n    return t == OP_VIS_A;\n}\n";

        let header = cpp(header_src);
        let mut user = cpp(use_src);
        user.pack.include_closure = crate::model::file_analysis::path_intern::ClosureList::from_iter(
            std::iter::once("/fake/vis/def.h"),
        );
        let user_bare = cpp(use_src); // same tokens, no closure
        let stranger = cpp("int g(void) { return OP_VIS_A; }\n");

        let idx = crate::index::module_index::ModuleIndex::new_for_test();
        idx.register_symbols(PathBuf::from("/fake/vis/def.h"), Arc::new(header));
        idx.register_symbols(PathBuf::from("/fake/vis/use.c"), Arc::new(user));
        idx.register_symbols(PathBuf::from("/fake/vis/use2.c"), Arc::new(user_bare));
        idx.register_symbols(PathBuf::from("/fake/vis/other.c"), Arc::new(stranger));

        let store = FileStore::new();
        let origin = idx.get_cached("f").expect("use.c registered by its fn name");
        assert!(origin.path.ends_with("use.c"));
        let cs = resolve(
            &store,
            &origin.analysis,
            FileKey::Path(PathBuf::from("/fake/vis/use.c")),
            tree_sitter::Point { row: 1, column: 16 }, // on OP_VIS_A
            Some(&idx),
            OverrideScope::default(),
        );

        // goto-def: through the closure to the header's enumerator.
        let defs = cs.definitions();
        assert!(
            defs.iter().any(|d| matches!(&d.key, FileKey::Path(p) if p.ends_with("def.h"))),
            "gd resolves through the origin's closure: {defs:?}"
        );

        // references: the closure-connected files only. The disconnected
        // file's same-named token is NOT a reference to this target.
        let refs = cs.references();
        assert!(
            refs.iter().any(|r| matches!(&r.key, FileKey::Path(p) if p.ends_with("use.c"))),
            "the origin's own use is in the image: {refs:?}"
        );
        assert!(
            refs.iter().any(|r| matches!(&r.key, FileKey::Path(p) if p.ends_with("def.h"))
                && r.access == AccessKind::Declaration),
            "the header decl is in the image: {refs:?}"
        );
        assert!(
            !refs.iter().any(|r| matches!(&r.key, FileKey::Path(p) if p.ends_with("other.c"))),
            "a closure-disconnected file's same-named token stays OUT: {refs:?}"
        );

        // completion gathering: the closure IS the identifier universe.
        let names: Vec<String> =
            cs.complete("OP_", false).into_iter().map(|c| c.label).collect();
        assert!(
            names.contains(&"OP_VIS_A".to_string()) && names.contains(&"OP_VIS_B".to_string()),
            "the included header's names are the completion universe: {names:?}"
        );

        // The SAME knob, absent: a closure-less origin has no completion
        // universe (no global fallback by design) and no visibility
        // identity to gate the backward walk with (honest fallback: the
        // ungated name walk — the disconnected file's token now matches).
        let origin2 = idx.get_cached_scoped(
            "f",
            &["/fake/vis/use2.c".to_string()].iter().cloned().collect(),
        ).expect("use2.c registered");
        assert!(origin2.path.ends_with("use2.c"));
        let cs2 = resolve(
            &store,
            &origin2.analysis,
            FileKey::Path(PathBuf::from("/fake/vis/use2.c")),
            tree_sitter::Point { row: 1, column: 16 },
            Some(&idx),
            OverrideScope::default(),
        );
        assert!(
            cs2.complete("OP_", false).is_empty(),
            "no closure, no completion universe"
        );
        let refs2 = cs2.references();
        assert!(
            refs2.iter().any(|r| matches!(&r.key, FileKey::Path(p) if p.ends_with("other.c"))),
            "no closure, no visibility identity — the ungated walk matches by name: {refs2:?}"
        );
    }

    fn cpp(source: &str) -> FileAnalysis {
        crate::build::language_driver::LanguageRegistry::with_enabled()
            .for_id("cpp")
            .unwrap()
            .analyze(source)
    }

    /// decl→def ranking: goto-def on (or through) a bodiless
    /// declaration ranks the bodied definition first, decl kept — the static
    /// forward-decl shape same-file, and the `extern` decl → defining-TU
    /// shape cross-file (reverse closure: the TU includes the header).
    #[test]
    fn goto_def_ranks_bodied_definition_above_decl() {
        use std::sync::Arc;
        // Same-file: static forward decl at row 0, bodied def at row 2.
        let src = "static int helper(int n);\nint use_it(int n) { return helper(n); }\nstatic int helper(int n) { return n * 2; }\n";
        let fa = cpp(src);
        let store = FileStore::new();
        let cs = resolve(
            &store,
            &fa,
            FileKey::Path(PathBuf::from("/fake/dd/one.c")),
            tree_sitter::Point { row: 0, column: 11 }, // on the decl's name
            None,
            OverrideScope::default(),
        )
        .with_source(src);
        let defs = cs.definitions();
        assert_eq!(defs.len(), 2, "def ranked + decl kept: {defs:?}");
        assert_eq!(defs[0].span.start.row, 2, "the bodied def ranks first: {defs:?}");
        assert_eq!(defs[1].span.start.row, 0, "the decl is kept, never pruned: {defs:?}");

        // Cross-file: `extern` decl in the header, instance in the TU whose
        // closure includes the header (the reverse-connectivity edge).
        let header_src = "extern struct GS g_state;\n";
        let header = cpp(header_src);
        let mut tu = cpp("struct GS g_state;\n");
        tu.pack.include_closure = crate::model::file_analysis::path_intern::ClosureList::from_iter(
            std::iter::once("/fake/dd/state.h"),
        );
        let idx = crate::index::module_index::ModuleIndex::new_for_test();
        idx.register_symbols(PathBuf::from("/fake/dd/state.h"), Arc::new(header));
        idx.register_symbols(PathBuf::from("/fake/dd/state.c"), Arc::new(tu));
        let origin = idx
            .get_cached_scoped("g_state", &["/fake/dd/state.h".to_string()].iter().cloned().collect())
            .expect("header registered");
        assert!(origin.path.ends_with("state.h"));
        let cs = resolve(
            &store,
            &origin.analysis,
            FileKey::Path(PathBuf::from("/fake/dd/state.h")),
            tree_sitter::Point { row: 0, column: 18 }, // on `g_state`
            Some(&idx),
            OverrideScope::default(),
        )
        .with_source(header_src);
        let defs = cs.definitions();
        assert!(
            matches!(&defs[0].key, FileKey::Path(p) if p.ends_with("state.c")),
            "the defining TU's instance ranks first: {defs:?}"
        );
        assert!(
            defs.iter().any(|d| matches!(&d.key, FileKey::Path(p) if p.ends_with("state.h"))),
            "the extern decl is kept: {defs:?}"
        );
    }

    /// Cross-file MEMBER decl→def (H7-3): a class member's out-of-line body
    /// (`void C::m(){}` in another TU) is NOT linkage-visible, so the name-keyed
    /// def-candidates table never pulls it in — the free-function decl→def hop
    /// misses it. The member fallback in `preferred_definitions` sweeps the
    /// closure-connected cached files directly. Two entry shapes, one axis:
    /// an explicitly-qualified call (`C::m(...)`, owner-anchored path) and a
    /// cursor sitting on the header declaration itself.
    #[test]
    fn goto_def_links_member_decl_to_out_of_line_def_cross_file() {
        use std::sync::Arc;
        // No enclosing namespace, so the def's `package` stays the qualifier's
        // class (the container-reanchor never fires) — exercises resolution/
        // linking on a shape whose extraction packages the member correctly.
        let header_src = "struct MemTable {\n  void Add(int s);\n};\n";
        let def_src = "void MemTable::Add(int s) { return; }\n";
        let caller_src = "void writer() {\n  MemTable::Add(3);\n}\n";

        let header = cpp(header_src);
        let mut def_tu = cpp(def_src);
        def_tu.pack.include_closure = crate::model::file_analysis::path_intern::ClosureList::from_iter(
            std::iter::once("/fake/mt/memtable.h"),
        );
        let mut caller = cpp(caller_src);
        caller.pack.include_closure = crate::model::file_analysis::path_intern::ClosureList::from_iter(
            std::iter::once("/fake/mt/memtable.h"),
        );

        let idx = crate::index::module_index::ModuleIndex::new_for_test();
        idx.register_symbols(PathBuf::from("/fake/mt/memtable.h"), Arc::new(header));
        idx.register_symbols(PathBuf::from("/fake/mt/memtable.cc"), Arc::new(def_tu));
        idx.register_symbols(PathBuf::from("/fake/mt/writer.cc"), Arc::new(caller));
        let store = FileStore::new();

        // (1) Explicitly-qualified cross-file call: `MemTable::Add(3)`.
        let origin = idx
            .get_cached_scoped(
                "writer",
                &["/fake/mt/memtable.h".to_string()].iter().cloned().collect(),
            )
            .expect("writer.cc registered");
        assert!(origin.path.ends_with("writer.cc"));
        let cs = resolve(
            &store,
            &origin.analysis,
            FileKey::Path(PathBuf::from("/fake/mt/writer.cc")),
            tree_sitter::Point { row: 1, column: 12 }, // on `Add`
            Some(&idx),
            OverrideScope::default(),
        )
        .with_source(caller_src);
        let defs = cs.definitions();
        assert!(
            defs.iter().any(|d| matches!(&d.key, FileKey::Path(p) if p.ends_with("memtable.cc"))),
            "the out-of-line body is present (def must be reachable): {defs:?}"
        );
        assert!(
            matches!(&defs[0].key, FileKey::Path(p) if p.ends_with("memtable.cc")),
            "the bodied def ranks first: {defs:?}"
        );
        assert!(
            defs.iter().any(|d| matches!(&d.key, FileKey::Path(p) if p.ends_with("memtable.h"))),
            "the header decl is kept, never pruned: {defs:?}"
        );

        // (2) Cursor ON the header declaration: still offers the body first.
        let hdr_origin = idx
            .get_cached("MemTable")
            .expect("MemTable class registered");
        assert!(hdr_origin.path.ends_with("memtable.h"));
        let cs2 = resolve(
            &store,
            &hdr_origin.analysis,
            FileKey::Path(PathBuf::from("/fake/mt/memtable.h")),
            tree_sitter::Point { row: 1, column: 7 }, // on `Add` in the decl
            Some(&idx),
            OverrideScope::default(),
        )
        .with_source(header_src);
        let defs2 = cs2.definitions();
        assert!(
            defs2.iter().any(|d| matches!(&d.key, FileKey::Path(p) if p.ends_with("memtable.cc"))),
            "goto-def on the decl reaches the out-of-line body: {defs2:?}"
        );
    }

    /// The visibility gate's textual-inclusion extension: a file whose own
    /// closure reaches no def path still joins the references image when a
    /// DIRECT seer includes it (`ae.c: #include "ae_epoll.c"`); a genuinely
    /// disconnected file's same-named token stays out.
    #[test]
    fn function_gate_admits_files_included_by_a_seer() {
        use std::sync::Arc;
        let def_src = "int compute_thing(int n) { return n; }\n";
        let mut def_tu = cpp(def_src);
        def_tu.pack.include_closure = crate::model::file_analysis::path_intern::ClosureList::from_iter(
            std::iter::once("/fake/inc/lib.h"),
        );
        let header = cpp("int compute_thing(int n);\n");
        let mut host = cpp("int a(void) { return compute_thing(1); }\n");
        host.pack.include_closure = crate::model::file_analysis::path_intern::ClosureList::from_iter(
            ["/fake/inc/lib.h", "/fake/inc/frag.c"].into_iter(),
        );
        // The fragment: same call, EMPTY closure (compiled only by textual
        // inclusion into host.c).
        let frag = cpp("int b(void) { return compute_thing(2); }\n");
        // Disconnected same-named noise.
        let noise = cpp("int c(void) { return compute_thing(3); }\n");

        let idx = crate::index::module_index::ModuleIndex::new_for_test();
        idx.register_symbols(PathBuf::from("/fake/inc/lib.c"), Arc::new(def_tu));
        idx.register_symbols(PathBuf::from("/fake/inc/lib.h"), Arc::new(header));
        idx.register_symbols(PathBuf::from("/fake/inc/host.c"), Arc::new(host));
        idx.register_symbols(PathBuf::from("/fake/inc/frag.c"), Arc::new(frag));
        idx.register_symbols(PathBuf::from("/fake/inc/noise.c"), Arc::new(noise));

        let store = FileStore::new();
        let origin = idx
            .get_cached_scoped("compute_thing", &["/fake/inc/lib.c".to_string()].iter().cloned().collect())
            .expect("def TU registered");
        assert!(origin.path.ends_with("lib.c"));
        let cs = resolve(
            &store,
            &origin.analysis,
            FileKey::Path(PathBuf::from("/fake/inc/lib.c")),
            tree_sitter::Point { row: 0, column: 4 }, // on the def's name
            Some(&idx),
            OverrideScope::default(),
        )
        .with_source(def_src);
        // The Sub target carries closure-keyed def_paths (the D3 gate).
        match cs.resolution() {
            Some(ResolvedTarget::Target(t)) => {
                assert!(!t.def_paths.is_empty(), "pack Sub target is gated: {t:?}")
            }
            other => panic!("Sub target expected: {other:?}"),
        }
        let refs = cs.references();
        let has = |suffix: &str| {
            refs.iter().any(|r| matches!(&r.key, FileKey::Path(p) if p.ends_with(suffix)))
        };
        assert!(has("host.c"), "a direct seer's call is in: {refs:?}");
        assert!(has("frag.c"), "the seer-included fragment's call is in: {refs:?}");
        assert!(!has("noise.c"), "the disconnected file stays out: {refs:?}");
    }

    /// The hover/gd agreement invariant: hover presents the
    /// CandidateSet's top-ranked definition candidate, so a position answers
    /// BOTH verbs or NEITHER — hover-specific presentation aside, the two
    /// can't disagree on what the cursor means.
    #[test]
    fn hover_projection_agrees_with_goto_def() {
        use std::sync::Arc;
        let header_src = "enum opcode { OP_HOV_A, OP_HOV_B };\n";
        let use_src = "int f(int t) {\n    return t == OP_HOV_A;\n}\n";

        // Hover renders the cross-file candidate from its file on disk.
        let dir = std::env::temp_dir().join("perl_lsp_hover_gd_invariant");
        std::fs::create_dir_all(&dir).unwrap();
        let header_path = dir.join("def.h");
        std::fs::write(&header_path, header_src).unwrap();

        let header = cpp(header_src);
        let mut user = cpp(use_src);
        user.pack.include_closure = crate::model::file_analysis::path_intern::ClosureList::from_iter(
            std::iter::once(header_path.to_string_lossy().as_ref()),
        );

        let idx = crate::index::module_index::ModuleIndex::new_for_test();
        idx.register_symbols(header_path.clone(), Arc::new(header));
        idx.register_symbols(dir.join("use.c"), Arc::new(user));

        let store = FileStore::new();
        let origin = idx.get_cached("f").expect("use.c registered by its fn name");
        let cs = resolve(
            &store,
            &origin.analysis,
            FileKey::Path(dir.join("use.c")),
            tree_sitter::Point { row: 1, column: 16 }, // on OP_HOV_A
            Some(&idx),
            OverrideScope::default(),
        )
        .with_source(use_src);

        let defs = cs.definitions();
        let hover = crate::lsp::symbols::pack_hover_markdown(&cs, "c");
        assert!(!defs.is_empty(), "gd answers at the enum-constant use");
        let hover = hover.expect("hover answers wherever gd answers");
        assert!(
            hover.contains("OP_HOV_A"),
            "hover presents the candidate gd ranks first: {hover}"
        );

        // NEITHER: a token-less position answers no verb.
        let cs_blank = resolve(
            &store,
            &origin.analysis,
            FileKey::Path(dir.join("use.c")),
            tree_sitter::Point { row: 2, column: 0 }, // the closing `}` line
            Some(&idx),
            OverrideScope::default(),
        )
        .with_source(use_src);
        assert!(cs_blank.definitions().is_empty(), "gd silent on a token-less position");
        assert!(
            crate::lsp::symbols::pack_hover_markdown(&cs_blank, "c").is_none(),
            "hover silent exactly where gd is"
        );
    }

    #[test]
    fn enum_constant_def_reaches_bare_reads_across_files() {
        let store = FileStore::new();
        let decl = cpp("enum opcode { OP_NULL, OP_SCOPE };\n");
        // Resolve at the OP_SCOPE def token (row 0, col 24).
        let target = match resolve_symbol(&decl, tree_sitter::Point::new(0, 24), None) {
            Some(ResolvedTarget::Target(t)) => t,
            other => panic!("enum-constant def resolves to a Method target: {other:?}"),
        };
        assert_eq!(target.kind, TargetKind::Method { class: "opcode".to_string() });
        store.insert_workspace(PathBuf::from("/tmp/sym_opcodes.h"), decl);
        store.insert_workspace(
            PathBuf::from("/tmp/sym_use.c"),
            cpp("int is_scope(int t) {\n    return t == OP_SCOPE;\n}\n"),
        );
        let results = refs_to(&store, None, &target, RoleMask::EDITABLE);
        assert!(
            results.iter().any(|r| matches!(&r.key, FileKey::Path(p) if p.ends_with("sym_use.c"))
                && r.span.start == tree_sitter::Point::new(1, 16)),
            "the bare value read matches by name: {results:?}"
        );
        assert!(
            results.iter().any(|r| r.access == AccessKind::Declaration),
            "the enumerator decl is included: {results:?}"
        );
    }

    #[test]
    fn pack_local_in_inline_method_stays_local() {
        // A local inside an inline method carries the class as sticky
        // `package`; it must resolve Local, never fan out as class content.
        let fa = cpp("class Box {\npublic:\n  void grow() { int localx = 1; localx += 2; }\n};\n");
        let resolved = resolve_symbol(&fa, tree_sitter::Point::new(2, 20), None);
        assert!(
            matches!(resolved, Some(ResolvedTarget::Local)),
            "a lexical local is Local: {resolved:?}"
        );
    }

    /// L2 lock: enum-constant rename is symmetric with refs/def — the full
    /// cross-file edit set (decl + bare value reads), no locations frozen. A
    /// bare enum read is a `Variable` ref whose token IS the literal name;
    /// the Perl variable-fold heuristic must not mark it non-rewritable (that
    /// made pack rename refuse with a bogus "delegating macro" reason).
    #[test]
    fn enum_constant_rename_emits_full_edit_set() {
        let store = FileStore::new();
        let decl = cpp("enum opcode { OP_NULL, OP_SCOPE };\n");
        let hpath = PathBuf::from("/tmp/ren_opcodes.h");
        store.insert_workspace(hpath.clone(), decl);
        store.insert_workspace(
            PathBuf::from("/tmp/ren_use.c"),
            cpp("int is_scope(int t) {\n    return t == OP_SCOPE;\n}\n"),
        );
        let origin = store.workspace_raw().get(&hpath).unwrap().value().clone();
        let cs = resolve(
            &store,
            &origin,
            FileKey::Path(hpath),
            tree_sitter::Point::new(0, 24),
            None,
            OverrideScope::default(),
        );
        let edits = cs
            .rename_edits("OP_RANGE")
            .expect("plain enum rename must not refuse");
        assert!(
            edits.iter().any(|(l, _)| l.access == AccessKind::Declaration),
            "the decl is edited: {edits:?}"
        );
        assert!(
            edits.iter().any(|(l, _)| matches!(&l.key, FileKey::Path(p) if p.ends_with("ren_use.c"))),
            "the cross-file use is edited: {edits:?}"
        );
    }

    #[test]
    fn object_like_macro_def_reaches_value_and_type_uses() {
        let store = FileStore::new();
        let fa = cpp("#define MAX 100\n#define BITS unsigned\nint a = MAX;\nBITS b;\n");
        let target = match resolve_symbol(&fa, tree_sitter::Point::new(0, 9), None) {
            Some(ResolvedTarget::Target(t)) => t,
            other => panic!("a `#define` def is a FileScopeValue target: {other:?}"),
        };
        assert_eq!(target.kind, TargetKind::FileScopeValue);
        // The type-alias spelling resolves too (a PackageRef use).
        let type_target = match resolve_symbol(&fa, tree_sitter::Point::new(1, 9), None) {
            Some(ResolvedTarget::Target(t)) => t,
            other => panic!("type-alias `#define`: {other:?}"),
        };
        store.insert_workspace(PathBuf::from("/tmp/sym_macro.c"), fa);
        let results = refs_to(&store, None, &target, RoleMask::EDITABLE);
        assert!(
            results.iter().any(|r| r.span.start == tree_sitter::Point::new(2, 8)),
            "the value use (expanded or left) is reached: {results:?}"
        );
        assert!(
            results.iter().any(|r| r.access == AccessKind::Declaration),
            "the `#define` decl is included: {results:?}"
        );
        let type_results = refs_to(&store, None, &type_target, RoleMask::EDITABLE);
        assert!(
            type_results.iter().any(|r| r.span.start.row == 3),
            "the type-position use of a type-alias macro is reached: {type_results:?}"
        );
    }

    #[test]
    fn delegation_alias_makes_wrapper_calls_references_of_the_real_function() {
        // gd sees through `#define WRAP(x) real(x)` forward; gr on `real`
        // must traverse the edge BACKWARD: WRAP's call sites are references
        // to `real` — listed, but never rewritable (the token spells WRAP).
        let store = FileStore::new();
        let fa = cpp("int real(int x);\n#define WRAP(x) real(x)\nvoid f() { WRAP(1); }\n");
        let target = match resolve_symbol(&fa, tree_sitter::Point::new(0, 5), None) {
            Some(ResolvedTarget::Target(t)) => t,
            other => panic!("free function target: {other:?}"),
        };
        store.insert_workspace(PathBuf::from("/tmp/sym_deleg.c"), fa);
        let results = refs_to(&store, None, &target, RoleMask::EDITABLE);
        let wrap_call = results
            .iter()
            .find(|r| r.span.start == tree_sitter::Point::new(2, 11))
            .unwrap_or_else(|| panic!("WRAP call site is a reference to `real`: {results:?}"));
        assert!(!wrap_call.rewritable, "an alias site never renames");
    }

    #[test]
    fn file_scope_global_def_reaches_bare_reads() {
        let store = FileStore::new();
        let a = cpp("int global_counter;\n");
        let target = match resolve_symbol(&a, tree_sitter::Point::new(0, 6), None) {
            Some(ResolvedTarget::Target(t)) => t,
            other => panic!("a file-scope global is a FileScopeValue target: {other:?}"),
        };
        assert_eq!(target.kind, TargetKind::FileScopeValue);
        store.insert_workspace(PathBuf::from("/tmp/sym_glob.h"), a);
        store.insert_workspace(
            PathBuf::from("/tmp/sym_glob_use.c"),
            cpp("int f() { return global_counter; }\n"),
        );
        let results = refs_to(&store, None, &target, RoleMask::EDITABLE);
        assert!(
            results.iter().any(|r| matches!(&r.key, FileKey::Path(p) if p.ends_with("sym_glob_use.c"))),
            "cross-file bare read matches by name: {results:?}"
        );
    }
}

/// The specialization FAMILY view: `--implementations` on a primary
/// template's class name enumerates every spec's def site, cross-file,
/// via the `Specializes` edge — while gr on the primary stays "uses of
/// the primary" (the spec def sites are Class symbols, not PackageRefs).
#[test]
fn test_implementations_on_primary_enumerates_specialization_family() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    let cpp = |src: &str| {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
        let tree = parser.parse(src, None).unwrap();
        crate::build::query_extract::extract(&tree, src.as_bytes(), &crate::build::query_extract::cpp_pack())
            .unwrap()
            .into_file_analysis()
    };
    let primary_src = "template <typename T, typename C> struct formatter { int parse(int c); };\n";
    let specs_src = "template <> struct formatter<int, char> { int f1(); };\n\
                     template <typename T> struct formatter<T*, char> { int f2(); };\n";
    let origin = cpp(primary_src);
    let idx = ModuleIndex::new_for_test();
    idx.register_symbols(PathBuf::from("/fake/base.h"), Arc::new(cpp(primary_src)));
    idx.register_symbols(PathBuf::from("/fake/format.h"), Arc::new(cpp(specs_src)));

    let target = TargetRef::new("formatter".to_string(), TargetKind::Package);
    let results = implementations_of(&origin, Some(&idx), &target);
    let files: Vec<String> = results
        .iter()
        .map(|r| match &r.key {
            FileKey::Path(p) => p.display().to_string(),
            FileKey::Url(u) => u.to_string(),
        })
        .collect();
    assert_eq!(
        files,
        vec!["/fake/format.h", "/fake/format.h"],
        "both specs' def sites, from the OTHER file: {results:?}"
    );
    // never rewritable — the spec's selection span is the whole spelling
    assert!(results.iter().all(|r| !r.rewritable));
}

/// `initializationOptions.rename` deserializes via the `RenameOptions` serde
/// schema (the struct IS the schema) — the LSP path no longer hand-parses the
/// `overrideScope` string. Absent key → default Hierarchy; a bad value is an
/// `Err` the handler swallows, leaving the default in place.
#[test]
fn rename_options_deserialize_from_init_json() {
    use crate::index::resolve::{OverrideScope, RenameOptions};
    let scope = |v: serde_json::Value| {
        serde_json::from_value::<RenameOptions>(v).map(|r| r.override_scope)
    };
    assert_eq!(scope(serde_json::json!({"overrideScope": "dispatch"})).unwrap(), OverrideScope::Dispatch);
    assert_eq!(scope(serde_json::json!({"overrideScope": "hierarchy"})).unwrap(), OverrideScope::Hierarchy);
    assert_eq!(scope(serde_json::json!({})).unwrap(), OverrideScope::Hierarchy);
    assert!(scope(serde_json::json!({"overrideScope": "bogus"})).is_err());
}
