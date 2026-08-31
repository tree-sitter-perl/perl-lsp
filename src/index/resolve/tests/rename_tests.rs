//! Rename-specific pins over the same refs_to(EDITABLE) path rename uses.

use super::*;

// ---- Rename-specific pins --------------------------------------------------------
//
// `rename_via_refs_to` in backend.rs calls `refs_to(EDITABLE)` directly —
// so these tests exercise the same code path rename uses. If rename and
// references ever diverge, a test here (EDITABLE) will disagree with
// the references test (VISIBLE/EDITABLE via references_mask_for).

/// Rename a base-class method: call sites on child-class invocants in
/// other workspace files must be included. This is the inheritance
/// fan-out the old per-file `rename_method_in_class` missed — it matched
/// `invocant_class == target_class` exactly, so `$child->ping()` (where
/// invocant class is "Child") fell out when targeting "Base::ping".
///
/// `refs_to` uses `method_rename_chain(invocant_class)` which checks
/// whether the target class is anywhere on the invocant's resolution
/// chain, so `$child->ping()` targeting Base IS matched when Child
/// inherits Base's `ping`.
///
/// Cross-file parent resolution requires a ModuleIndex (the consumer
/// file doesn't declare Child's parents — only child.pm does). The
/// module index is how production code surfaces this: the LSP backend
/// passes `Some(&self.module_index)` to every `refs_to` call.
#[test]
fn rename_base_method_includes_child_call_sites() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    // Base class file: defines the method being renamed.
    let base_src = r#"
package Base;
sub new { bless {}, shift }
sub ping { "pong" }
1;
"#;
    // Child class file: inherits from Base. No local `ping` override.
    let child_src = r#"
package Child;
use parent 'Base';
1;
"#;
    // Consumer file: calls `ping` on a Child instance.
    let consumer_src = r#"
package Consumer;
use Child;
my $c = Child->new;
$c->ping;
1;
"#;
    // Decoy file: unrelated class with a same-named method — must NOT appear.
    let decoy_src = r#"
package Decoy;
sub new { bless {}, shift }
sub ping { "decoy" }
my $d = Decoy->new;
$d->ping;
1;
"#;

    let base_path = PathBuf::from("/tmp/rename_base.pm");
    let child_path = PathBuf::from("/tmp/rename_child.pm");
    let consumer_path = PathBuf::from("/tmp/rename_consumer.pm");
    let decoy_path = PathBuf::from("/tmp/rename_decoy.pm");

    // Register base + child in module index so cross-file parent resolution
    // works (consumer_fa sees Child's parents via idx.parents_cached("Child")).
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(base_path.clone(), Arc::new(parse(base_src)));
    idx.register_workspace_module(child_path.clone(), Arc::new(parse(child_src)));

    let mut consumer_fa = parse(consumer_src);
    consumer_fa.enrich_imported_types_with_keys(Some(&idx));

    let store = FileStore::new();
    store.insert_workspace(base_path.clone(), parse(base_src));
    store.insert_workspace(child_path.clone(), parse(child_src));
    store.insert_workspace(consumer_path.clone(), consumer_fa);
    store.insert_workspace(decoy_path.clone(), parse(decoy_src));

    // Targeting Base::ping (where rename cursor would sit at the `sub ping` declaration).
    let target = TargetRef {
        name: "ping".to_string(),
        kind: TargetKind::Method { class: "Base".to_string() },
        method_classes: Vec::new(), scope: OverrideScope::Dispatch, def_paths: Vec::new(), bare_constant: false,
            ctor_of: None,
    };

    // Rename uses EDITABLE — workspace-only, no dep scan.
    let locs = refs_to(&store, Some(&idx), &target, RoleMask::EDITABLE);
    let hits: Vec<(&str, usize)> = locs.iter().map(|r| {
        let fname = match &r.key {
            FileKey::Path(p) => p.file_name().unwrap().to_str().unwrap(),
            FileKey::Url(_) => "url",
        };
        (fname, r.span.start.row)
    }).collect();

    // Base::ping declaration must be included.
    assert!(
        hits.iter().any(|(f, _)| *f == "rename_base.pm"),
        "rename missed Base::ping declaration: {:?}", hits,
    );
    // Consumer's $c->ping call on a Child invocant must be included —
    // Child inherits ping from Base so it's on the rename chain.
    assert!(
        hits.iter().any(|(f, _)| *f == "rename_consumer.pm"),
        "rename missed Consumer's $$c->ping call (inherited from Base via Child): {:?}", hits,
    );
    // Decoy::ping is an unrelated class — must NOT be included.
    assert!(
        !hits.iter().any(|(f, _)| *f == "rename_decoy.pm"),
        "rename wrongly included Decoy::ping (unrelated class): {:?}", hits,
    );
}

/// Rename never edits dependency files. A `ping` method that also appears
/// in a dep module (registered in ModuleIndex, not in the workspace store)
/// must not produce edits for that dep file — `RoleMask::EDITABLE` stops
/// at OPEN + WORKSPACE, which is what `rename_via_refs_to` uses.
#[test]
fn rename_does_not_edit_dep_files() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    // The dep defines Base::ping. It's in the module index (dep cache),
    // not in the workspace store.
    let dep_src = r#"
package Base;
sub new { bless {}, shift }
sub ping { "pong" }
1;
"#;
    let dep_path = PathBuf::from("/tmp/rename_dep_base.pm");

    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(dep_path.clone(), Arc::new(parse(dep_src)));

    // Consumer in the workspace calls ping on a Base invocant.
    let consumer_src = r#"
package Consumer;
my $b = Base->new;
$b->ping;
1;
"#;
    let consumer_path = PathBuf::from("/tmp/rename_dep_consumer.pm");

    let store = FileStore::new();
    store.insert_workspace(consumer_path.clone(), parse(consumer_src));

    let target = TargetRef {
        name: "ping".to_string(),
        kind: TargetKind::Method { class: "Base".to_string() },
        method_classes: Vec::new(), scope: OverrideScope::Dispatch, def_paths: Vec::new(), bare_constant: false,
            ctor_of: None,
    };

    // EDITABLE mask — rename never scans deps.
    let editable_locs = refs_to(&store, Some(&idx), &target, RoleMask::EDITABLE);
    for loc in &editable_locs {
        assert!(
            !matches!(&loc.key, FileKey::Path(p) if p == &dep_path),
            "rename emitted an edit for the dep file (read-only): {:?}", editable_locs,
        );
    }

    // Sanity: VISIBLE would find the dep decl.
    let visible_locs = refs_to(&store, Some(&idx), &target, RoleMask::VISIBLE);
    assert!(
        visible_locs.iter().any(|r| matches!(&r.key, FileKey::Path(p) if p == &dep_path)),
        "sanity: VISIBLE should see dep file's ping decl: {:?}", visible_locs,
    );
}

/// Rename and references agree on the target set for a base-class Method:
/// both call `refs_to` with the same target, so the only difference should
/// be the mask (EDITABLE vs references_mask_for's EDITABLE-or-VISIBLE).
/// When the method is defined in workspace, references_mask_for returns
/// EDITABLE — so the result sets are IDENTICAL.
///
/// This is the DRY invariant: rename and references share one code path.
#[test]
fn rename_and_references_agree_on_same_base_method_target() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    let base_src = r#"
package Greeter;
sub new { bless {}, shift }
sub hello { "hi" }
1;
"#;
    let child_src = r#"
package ChildGreeter;
use parent 'Greeter';
1;
"#;
    let consumer_src = r#"
package Main;
use ChildGreeter;
my $g = ChildGreeter->new;
$g->hello;
1;
"#;

    let base_path = PathBuf::from("/tmp/agree_greeter.pm");
    let child_path = PathBuf::from("/tmp/agree_child.pm");
    let consumer_path = PathBuf::from("/tmp/agree_consumer.pm");

    // Module index carries the Child→Base parent edge for cross-file chain walk.
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(base_path.clone(), Arc::new(parse(base_src)));
    idx.register_workspace_module(child_path.clone(), Arc::new(parse(child_src)));

    let mut consumer_fa = parse(consumer_src);
    consumer_fa.enrich_imported_types_with_keys(Some(&idx));

    let store = FileStore::new();
    store.insert_workspace(base_path.clone(), parse(base_src));
    store.insert_workspace(child_path.clone(), parse(child_src));
    store.insert_workspace(consumer_path.clone(), consumer_fa);

    let target = TargetRef {
        name: "hello".to_string(),
        kind: TargetKind::Method { class: "Greeter".to_string() },
        method_classes: Vec::new(), scope: OverrideScope::Dispatch, def_paths: Vec::new(), bare_constant: false,
            ctor_of: None,
    };

    // references uses references_mask_for (EDITABLE when def in workspace).
    let ref_mask = references_mask_for(&store, Some(&idx), &target);
    assert_eq!(ref_mask.bits(), RoleMask::EDITABLE.bits(),
        "precondition: references_mask_for should be EDITABLE when def is in workspace");

    let rename_locs = refs_to(&store, Some(&idx), &target, RoleMask::EDITABLE);
    let ref_locs    = refs_to(&store, Some(&idx), &target, ref_mask);

    // Collect (path, row) pairs for easy comparison.
    let to_set = |v: &[crate::index::resolve::RefLocation]| -> std::collections::BTreeSet<(String, usize)> {
        v.iter().map(|r| {
            let p = match &r.key {
                FileKey::Path(p) => p.display().to_string(),
                FileKey::Url(u) => u.to_string(),
            };
            (p, r.span.start.row)
        }).collect()
    };
    let rename_set = to_set(&rename_locs);
    let ref_set    = to_set(&ref_locs);

    assert_eq!(
        rename_set, ref_set,
        "rename and references disagree on target set for Greeter::hello\n\
         rename-only: {:?}\nrefs-only: {:?}",
        rename_set.difference(&ref_set).collect::<Vec<_>>(),
        ref_set.difference(&rename_set).collect::<Vec<_>>(),
    );
    // Also verify the consumer (child invocant) is in BOTH sets.
    assert!(
        rename_set.iter().any(|(f, _)| f.contains("agree_consumer")),
        "both rename and references must include consumer's $$g->hello call: {:?}",
        rename_set,
    );
}

/// Rename invoked AT THE CHILD CALL SITE of an inherited base method must
/// still edit the parent declaration. Here `rename_kind_at` returns the
/// cursor's static class (`MyWorker`), not the defining class (`BaseWorker`):
/// the parent `sub process` decl lives in a class the target's strict
/// `class` field never names. The fix precomputes the inheritance
/// rename-chain (`[MyWorker, .., BaseWorker]`) on the target — built from the
/// originating analysis, the only one that knows MyWorker's parents — so
/// `symbol_defines_target` admits a `sub NAME` in ANY chain class. Without
/// it the base declaration edit silently drops (the e2e inheritance rename
/// regression).
#[test]
fn rename_from_child_call_site_includes_inherited_base_declaration() {
    use crate::model::file_analysis::RenameKind;
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    let base_src = "package BaseWorker;\nsub new { bless {}, shift }\nsub process { 1 }\n1;\n";
    // Child inherits process; no local override.
    let child_src = "package MyWorker;\nuse parent 'BaseWorker';\n1;\n";
    // Script calls process() on a MyWorker instance.
    let script_src = "use MyWorker;\nmy $worker = MyWorker->new;\n$worker->process();\n";

    let base_path = PathBuf::from("/tmp/cs_base.pm");
    let child_path = PathBuf::from("/tmp/cs_child.pm");
    let script_path = PathBuf::from("/tmp/cs_script.pl");

    // The originating analysis (the script) must know MyWorker's parents to
    // build the chain — production threads the module index for exactly this.
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(base_path.clone(), Arc::new(parse(base_src)));
    idx.register_workspace_module(child_path.clone(), Arc::new(parse(child_src)));

    let mut script_fa = parse(script_src);
    script_fa.enrich_imported_types_with_keys(Some(&idx));

    // Resolve the target the way the rename handler does: cursor on the
    // `process` token of `$worker->process()`, mapped via from_rename_kind.
    let call_point = tree_sitter::Point { row: 2, column: 9 };
    let rk = script_fa.rename_kind_at(call_point, Some(&idx));
    assert!(
        matches!(&rk, Some(RenameKind::Method { class, .. }) if class == "MyWorker"),
        "precondition: call-site rename_kind_at should resolve invocant class MyWorker, got {:?}",
        rk,
    );
    let target = TargetRef::from_rename_kind(rk.unwrap(), &script_fa, Some(&idx), OverrideScope::Dispatch)
        .expect("Method maps to a target");
    assert!(
        target.method_classes.iter().any(|c| c == "BaseWorker"),
        "chain must reach the defining ancestor BaseWorker, got {:?}",
        target.method_classes,
    );

    let store = FileStore::new();
    store.insert_workspace(base_path.clone(), parse(base_src));
    store.insert_workspace(child_path.clone(), parse(child_src));
    store.insert_workspace(script_path.clone(), script_fa);

    let locs = refs_to(&store, Some(&idx), &target, RoleMask::EDITABLE);
    let hit = |p: &PathBuf| locs.iter().any(|r| matches!(&r.key, FileKey::Path(x) if x == p));

    assert!(
        hit(&base_path),
        "rename from child call site dropped the BaseWorker::process declaration edit. hits: {:?}",
        locs,
    );
    assert!(
        hit(&script_path),
        "rename from child call site missed the $worker->process() call edit. hits: {:?}",
        locs,
    );
}
