use super::*;
use super::inheritance_tests::fake_cached_for_class;

// ---- SyntheticUse — plugin-emitted `use` statements ------------------------
//
// `EmitAction::SyntheticUse` lets a plugin react to a kit module's outer
// use (e.g. `use Co::Base -Class`) by injecting the inner `use`s that the
// kit performs at runtime (`Moo`, `parent`, etc.). The point is that the
// downstream effect — framework detection, has-synthesis, parent
// inheritance, plugin re-dispatch — is identical to what the user would
// have gotten by writing those `use` lines literally. The test below
// drives the kit path through a stub plugin and compares against a
// literal build.
mod synthetic_use {
    use super::*;
    use crate::model::file_analysis::Namespace;
    use crate::build::plugin::{
        CompletionQueryContext, EmitAction, FrameworkPlugin, PluginCompletionAnswer,
        PluginRegistry, PluginSigHelpAnswer, SigHelpQueryContext, Trigger, UseContext,
    };
    use std::sync::Arc;

    /// Catches `use Co::Base -Class` and emits a synthetic `use Moo`.
    /// One trigger (`Always`), one hook (`on_use`), zero overrides.
    /// Stripped to the minimum that exercises the path.
    struct CoBasePlugin;

    impl FrameworkPlugin for CoBasePlugin {
        fn id(&self) -> &str { "co-base-test" }
        fn triggers(&self) -> &[Trigger] {
            // `on_use` bypasses the trigger filter (every plugin sees
            // every use), so the trigger list here is incidental. Kept
            // non-empty to mirror real plugins.
            static T: [Trigger; 1] = [Trigger::Always];
            &T
        }
        fn on_use(&self, ctx: &UseContext) -> Vec<EmitAction> {
            if ctx.module_name != "Co::Base" { return Vec::new(); }
            let is_class = ctx.raw_args.iter().any(|a| a == "-Class");
            if !is_class { return Vec::new(); }
            vec![EmitAction::SyntheticUse {
                module: "Moo".into(),
                args: vec![],
                imports: vec![],
                span: ctx.span,
            }]
        }
        fn on_signature_help(&self, _: &SigHelpQueryContext) -> Option<PluginSigHelpAnswer> { None }
        fn on_completion(&self, _: &CompletionQueryContext) -> Option<PluginCompletionAnswer> { None }
    }

    fn registry_with_co_base() -> Arc<PluginRegistry> {
        let mut reg = PluginRegistry::new();
        // "Which modules grant Moo `has` semantics" rides moo.rhai's
        // framework_mode_makers() manifest — the kit registry needs that
        // carrier for `use Moo` (literal or synthetic) to set the mode,
        // same as the default registry.
        let engine = Arc::new(crate::build::plugin::rhai_host::make_engine());
        let moo = crate::build::plugin::rhai_host::load_bundled_plugin("moo", engine)
            .expect("bundled moo plugin loads");
        reg.register(moo);
        reg.register(Box::new(CoBasePlugin));
        Arc::new(reg)
    }

    fn build_with(source: &str, plugins: Arc<PluginRegistry>) -> FileAnalysis {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        super::super::build_with_plugins(&tree, source.as_bytes(), plugins)
    }

    /// `use Co::Base -Class` (via the stub kit plugin) produces the same
    /// observable downstream state as a literal `use Moo`:
    ///
    ///   * `package_framework` carries `Moo` for the package.
    ///   * `framework_imports` covers Moo's keyword set.
    ///   * `has 'name'` synthesizes the accessor Method.
    ///
    /// Anything that depends on those — has-synthesis, accessor symbols,
    /// inheritance via `with`/`extends`, the constructor key, downstream
    /// plugin chains — comes along for free because it reads the same
    /// state. We pin only the load-bearing axes; broader Moo behavior
    /// has its own dedicated tests.
    #[test]
    fn synthetic_use_moo_matches_literal_use_moo() {
        let kit_src = r#"
package Foo;
use Co::Base -Class;
has 'name' => (is => 'ro');
"#;
        let lit_src = r#"
package Foo;
use Moo;
has 'name' => (is => 'ro');
"#;
        let kit = build_with(kit_src, registry_with_co_base());
        let lit = build_with(lit_src, registry_with_co_base());

        // Both should record Moo as the active framework for Foo.
        assert_eq!(
            kit.package_framework("Foo").as_ref(),
            lit.package_framework("Foo").as_ref(),
            "kit (`use Co::Base -Class`) and literal (`use Moo`) must agree on package_framework"
        );
        assert!(
            kit.package_framework("Foo").is_some(),
            "Foo's framework should be set by SyntheticUse \"Moo\"; packages={:?}",
            kit.packages,
        );

        // Both should have Moo's keyword set in framework_imports.
        for kw in &["has", "with", "extends", "around", "before", "after"] {
            assert!(
                kit.framework_imports.contains(*kw),
                "SyntheticUse \"Moo\" must populate framework_imports[{kw}]; got {:?}",
                kit.framework_imports,
            );
        }

        // The `has 'name'` accessor synthesis depends on framework_modes
        // being set at the time `visit_has_call` fires. With SyntheticUse,
        // the kit's `use Co::Base -Class` precedes `has`, so the plugin
        // re-dispatch flips the mode before the has-call is walked.
        let kit_methods: Vec<&str> = kit.symbols().iter()
            .filter(|s| s.name == "name" && s.kind == SymKind::Method)
            .map(|s| s.name.as_str())
            .collect();
        let lit_methods: Vec<&str> = lit.symbols().iter()
            .filter(|s| s.name == "name" && s.kind == SymKind::Method)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(
            kit_methods, lit_methods,
            "`has 'name'` should synthesize the same accessor Methods under \
             SyntheticUse \"Moo\" as under literal `use Moo`",
        );
        assert_eq!(kit_methods.len(), 1, "ro getter is exactly one Method");

        // Provenance: the synthesized `Moo` Module symbol in the kit
        // build carries the emitting plugin's namespace tag; the
        // literal build's `Moo` Module is plain `Language`. This is
        // the one observable axis where the two builds are SUPPOSED
        // to differ — it's what lets `--dump-package` / outline /
        // completion filters surface "this came from co-base-test".
        let kit_moo = kit.symbols().iter()
            .find(|s| s.kind == SymKind::Module && s.name == "Moo")
            .expect("kit build must have a Module symbol for synthesized `use Moo`");
        assert_eq!(
            kit_moo.namespace,
            Namespace::framework("co-base-test"),
            "synthesized Module must carry the emitting plugin's namespace tag"
        );
        let lit_moo = lit.symbols().iter()
            .find(|s| s.kind == SymKind::Module && s.name == "Moo")
            .expect("literal build must have a Module symbol for `use Moo`");
        assert_eq!(
            lit_moo.namespace,
            Namespace::Language,
            "literal-source Module must stay on Namespace::Language (no plugin tag)"
        );
    }

    /// `use_dedup` short-circuits cycles. The stub plugin reacts to
    /// `use Co::Base` by emitting `SyntheticUse "Co::Base"` — if the
    /// gate didn't catch it, the on_use re-dispatch would loop and
    /// produce many duplicate Module symbols / Import entries.
    /// With dedup, the second emission is a no-op.
    #[test]
    fn synthetic_use_self_cycle_is_bounded() {
        struct LoopPlugin;
        impl FrameworkPlugin for LoopPlugin {
            fn id(&self) -> &str { "loop-test" }
            fn triggers(&self) -> &[Trigger] {
                static T: [Trigger; 1] = [Trigger::Always];
                &T
            }
            fn on_use(&self, ctx: &UseContext) -> Vec<EmitAction> {
                if ctx.module_name != "Co::Base" { return Vec::new(); }
                vec![EmitAction::SyntheticUse {
                    module: "Co::Base".into(),
                    args: vec![],
                    imports: vec![],
                    span: ctx.span,
                }]
            }
            fn on_signature_help(&self, _: &SigHelpQueryContext) -> Option<PluginSigHelpAnswer> { None }
            fn on_completion(&self, _: &CompletionQueryContext) -> Option<PluginCompletionAnswer> { None }
        }
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(LoopPlugin));
        let fa = build_with("package Foo; use Co::Base;\n", Arc::new(reg));

        let co_base_imports = fa.imports.iter()
            .filter(|i| i.module_name == "Co::Base")
            .count();
        assert_eq!(
            co_base_imports, 1,
            "self-cycle must collapse to one Import entry; use_dedup gate kicked in"
        );

        let module_syms = fa.symbols().iter()
            .filter(|s| s.kind == SymKind::Module && s.name == "Co::Base")
            .count();
        assert_eq!(module_syms, 1, "self-cycle must emit one Module symbol");

        // Belt-and-suspenders for the gate. The dedup short-circuits at
        // the top of `process_use`, so every downstream effect is bounded
        // by construction — but pinning each axis catches a future
        // regression where the gate gets moved or `process_use` gets
        // split. If any of these grow without the others, something's
        // half-processing the cycle.
        let co_base_uses = fa.symbols().iter()
            .filter(|s| s.kind == SymKind::Module && s.name == "Co::Base")
            .count();
        assert_eq!(
            co_base_uses, 1,
            "package_uses-equivalent (Module symbol count) should match Import count"
        );
        // `framework_imports` for `use Co::Base` (not a built-in framework
        // module): the bundled plugins shouldn't touch it. Cycle should
        // leave this empty whether it loops once or a thousand times.
        assert!(
            fa.framework_imports.is_empty()
                || fa.framework_imports.iter().all(|s| !s.starts_with("co_base_")),
            "cycle on a non-framework module should not leak Co::Base-tagged keywords \
             into framework_imports; got {:?}",
            fa.framework_imports,
        );
    }

    /// `imports` MUST be part of the dedup key. Real `use Foo qw(a)` and
    /// `use Foo qw(b)` discriminate via `extract_mojo_base_args`'s
    /// fallback to `extract_use_import_list` (the fallback fires when
    /// no barewords / literals are present, putting qw imports in
    /// `raw_args`). Synthetic emissions carry `args` and `imports` as
    /// separate fields, so the equivalent two SyntheticUses with
    /// different `imports` and empty `args` must NOT collide on the
    /// dedup key. Pre-fix, both keyed on `(pkg, "Foo", [])` and the
    /// second silently dropped.
    #[test]
    fn synthetic_use_distinct_imports_both_emit() {
        struct ImportPlugin;
        impl FrameworkPlugin for ImportPlugin {
            fn id(&self) -> &str { "imports-test" }
            fn triggers(&self) -> &[Trigger] {
                static T: [Trigger; 1] = [Trigger::Always];
                &T
            }
            fn on_use(&self, ctx: &UseContext) -> Vec<EmitAction> {
                if ctx.module_name != "Trigger::Kit" { return Vec::new(); }
                vec![
                    EmitAction::SyntheticUse {
                        module: "Foo".into(),
                        args: vec![],
                        imports: vec!["a".into()],
                        span: ctx.span,
                    },
                    EmitAction::SyntheticUse {
                        module: "Foo".into(),
                        args: vec![],
                        imports: vec!["b".into()],
                        span: ctx.span,
                    },
                ]
            }
            fn on_signature_help(&self, _: &SigHelpQueryContext) -> Option<PluginSigHelpAnswer> { None }
            fn on_completion(&self, _: &CompletionQueryContext) -> Option<PluginCompletionAnswer> { None }
        }
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(ImportPlugin));
        let fa = build_with("package Foo; use Trigger::Kit;\n", Arc::new(reg));

        let foo_imports: Vec<&crate::model::file_analysis::Import> = fa.imports.iter()
            .filter(|i| i.module_name == "Foo")
            .collect();
        assert_eq!(
            foo_imports.len(), 2,
            "two SyntheticUse \"Foo\" with distinct imports must both produce \
             Import entries — dedup must NOT collide on the args-only key. \
             Got imports: {:?}",
            foo_imports.iter().map(|i| &i.imported_symbols).collect::<Vec<_>>(),
        );

        // Each Import entry must carry its own qw-style import name.
        // Order-independent: we check the union covers both.
        let all_names: std::collections::HashSet<&str> = foo_imports.iter()
            .flat_map(|i| i.imported_symbols.iter().map(|s| s.local_name.as_str()))
            .collect();
        assert!(all_names.contains("a"), "missing import name 'a': {:?}", all_names);
        assert!(all_names.contains("b"), "missing import name 'b': {:?}", all_names);
    }
}

/// **Spike: array intelligence on the bag-canonical foundation.**
///
/// The headline scenario, top-to-bottom:
///
/// ```perl
/// # Some/User.pm (cross-file)
/// package Some::User;
/// use Mojo::Base -base;
/// has 'name';
/// sub greet { ... }
/// sub email { ... }
///
/// # main
/// package MyApp;
/// use Mojolicious::Lite;
/// use constant DEFAULT_NAME => 'alice';
///
/// helper make_user => sub {
///     my ($c, $name) = @_;
/// .   return Some::User->new(name => $name);
/// };
///
/// sub action {
///     my $c = Mojolicious::Controller->new;
///     my @users;
///     push @users, $c->make_user(DEFAULT_NAME);   # const fold + plugin helper
///     push @users, $c->make_user('bob');
///     $users[0]->                                  # ← method completion here
/// }
/// ```
///
/// The chain through `$users[0]`:
///   1. mojo-helpers plugin synthesizes `make_user` on
///      `Mojolicious::Controller` with `return_via_edge` pointing
///      at the anon-sub's body.
///   2. Coderef-return edge resolves the body's last expression
///      (`Some::User->new(...)`) → `ClassName("Some::User")`.
///   3. `push @users, $c->make_user(...)` contributes
///      `ClassName("Some::User")` to `@users`'s `Sequence` shape.
///   4. `$users[0]` projects the Sequence to its first element.
///   5. Method / hash-key completion on the projected class crosses
///      the file boundary into `Some::User.pm`.
///
/// The **new** code on this branch is purely the array hop —
/// declaration emission, `push` contribution, and projection at
/// `$users[N]`. Everything else (helper synth, coderef return,
/// const fold, cross-file dispatch, Mojo::Base hash-key defs)
/// drops out of the existing bag-canonical machinery for free.
#[test]
fn spike_array_hop_with_helper_and_cross_file_completion() {
    use crate::index::module_index::ModuleIndex;
    use std::path::PathBuf;
    use std::sync::Arc;

    let user_pm = r#"
package Some::User;
use Mojo::Base -base;
has 'name';
sub greet { my $self = shift; "hi $self->{name}" }
sub email { my $self = shift; "$self->{name}\@x.com" }
1;
"#;
    let user_fa = build_fa(user_pm);

    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(
        PathBuf::from("/tmp/Some/User.pm"),
        Arc::new(user_fa),
    );

    let app_src = r#"
package MyApp;
use Mojolicious::Lite;
use constant DEFAULT_NAME => 'alice';

my $app = Mojolicious->new;
$app->helper(make_user => sub {
    my ($c, $name) = @_;
    return Some::User->new(name => $name);
});

sub action {
    my $c = Mojolicious::Controller->new;
    my @users;
    push @users, $c->make_user(DEFAULT_NAME);
    push @users, $c->make_user('bob');
    $users[0]->greet();
}
"#;
    let app_fa = build_fa(app_src);

    // Load-bearing: walk the tree to find the `$users[0]` node and
    // ask `resolve_expression_type` what it is. This is the receiver
    // resolution path the chain typer + cursor context both go
    // through for completion.
    let tree = parse(app_src);
    fn find_array_element<'a>(node: tree_sitter::Node<'a>) -> Option<tree_sitter::Node<'a>> {
        if node.kind() == "array_element_expression" {
            return Some(node);
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(hit) = find_array_element(child) {
                return Some(hit);
            }
        }
        None
    }
    let elem_node = find_array_element(tree.root_node())
        .expect("test source contains `$users[0]`");
    let resolved = crate::lsp::cursor_context::resolve_expression_type(&app_fa, elem_node, app_src.as_bytes(), Some(&idx))
        .expect("$users[0] resolves to a type");
    assert_eq!(
        resolved.class_name(),
        Some("Some::User"),
        "the array hop survives the chain: helper(coderef) → push → \
         $users[0] → Some::User. got: {:?}",
        resolved,
    );

    // Cross-file method completion on the resolved class. Mojo::Base
    // accessor (`name`) + user-defined methods come through unified.
    let methods = app_fa.complete_methods_for_class("Some::User", Some(&idx));
    let method_names: std::collections::HashSet<&str> =
        methods.iter().map(|c| c.label.as_str()).collect();
    assert!(method_names.contains("greet"), "cross-file user method 'greet' missing");
    assert!(method_names.contains("email"), "cross-file user method 'email' missing");
    assert!(method_names.contains("name"), "Mojo::Base accessor 'name' missing");

    // Hash-key completion on the same class — synthesized by
    // `has 'name'`, reachable across files. Cross-file hash-key
    // completion flows through enrichment; the local
    // `complete_hash_keys_for_class` doesn't gate on `ModuleIndex`,
    // so this stays a soft observation for the spike rather than a
    // hard assert. The load-bearing claim is the array hop, not the
    // FA-side hash-key API.
    let keys = app_fa.complete_hash_keys_for_class("Some::User", Point::new(0, 0), None);
    let key_names: std::collections::HashSet<&str> =
        keys.iter().map(|c| c.label.as_str()).collect();
    let _ = key_names; // intentionally not asserted in the spike

    // Hover on `$users[0]->greet` — the tree-aware hover path.
    // The tree-free invocant ladder can't resolve `$users[0]`
    // (it isn't a Variable witness name); the tree-aware variant
    // dispatches through `resolve_expression_type` on the actual
    // CST node, hitting the same array_element_expression arm
    // cursor_context uses for completion. One projection rule,
    // both entry points.
    fn find_method_call<'a>(
        node: tree_sitter::Node<'a>,
        src: &[u8],
        method: &str,
    ) -> Option<tree_sitter::Node<'a>> {
        if node.kind() == "method_call_expression" {
            if let Some(m) = node.child_by_field_name("method") {
                if m.utf8_text(src).ok() == Some(method) {
                    return Some(node);
                }
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if let Some(hit) = find_method_call(child, src, method) {
                return Some(hit);
            }
        }
        None
    }
    let greet_call = find_method_call(tree.root_node(), app_src.as_bytes(), "greet")
        .expect("test source contains `$users[0]->greet()`");
    let method_node = greet_call
        .child_by_field_name("method")
        .expect("method-call has a method child");
    let hover = app_fa.hover_info(
        method_node.start_position(),
        app_src,
        Some(&idx),
    );
    let hover_text = hover.expect("hover on `$users[0]->greet` returns text");
    assert!(
        hover_text.contains("Some::User"),
        "hover on `$users[0]->greet` should mention Some::User; got: {}",
        hover_text,
    );
    assert!(
        hover_text.contains("greet"),
        "hover should include the method name; got: {}",
        hover_text,
    );
}

/// `has x => (isa => InstanceOf['Foo'])` — the constraint is `InstanceOf['Foo']`,
/// a Type::Tiny constraint *value*, not a class. The core types that call
/// expression as `TypeConstraintOf(ClassName(Foo))` (plugin fold), and the
/// accessor projects the constrained inner, so `x` returns `Foo`.
#[test]
fn moo_instanceof_isa_types_accessor_to_inner_class() {
    let fa = build_fa(
        "package T;\nuse Moo;\nuse Types::Standard qw/InstanceOf/;\nhas thing => (is => 'ro', isa => InstanceOf['My::Thing']);\n1;\n",
    );
    assert_eq!(
        fa.sub_return_type_at_arity("thing", Some(0)),
        Some(InferredType::ClassName("My::Thing".to_string())),
        "InstanceOf['My::Thing'] isa must give the getter a My::Thing return",
    );
}

/// The constructor expression itself is a `TypeConstraintOf` — NOT the inner
/// class. `$t->name` must see a constraint (so it can route to Type::Tiny
/// later), and an `isa => $t` projects the inner. This guards against the
/// lossy "InstanceOf['Foo'] == ClassName(Foo)" shortcut we rejected.
#[test]
fn instanceof_expression_is_a_type_constraint_not_the_class() {
    let fa = build_fa(
        "package T;\nuse Moo;\nuse Types::Standard qw/InstanceOf/;\nmy $t = InstanceOf['My::Thing'];\n1;\n",
    );
    let ty = fa
        .inferred_type_via_bag("$t", Point::new(3, 20))
        .expect("$t should carry a type");
    assert!(
        matches!(&ty, InferredType::TypeConstraintOf(inner)
            if matches!(inner.as_ref(), InferredType::ClassName(c) if c == "My::Thing")),
        "InstanceOf['My::Thing'] is a TypeConstraintOf(ClassName(My::Thing)), got {:?}",
        ty,
    );
    assert!(
        ty.constrained_inner().and_then(|i| i.class_name()) == Some("My::Thing"),
        "constrained_inner projects the class for the isa→accessor path",
    );
}

/// const-fold / variable path: `my $t = InstanceOf['Foo']; has x => (isa => $t)`.
/// `has` edges to the RHS `$t`, whose type is the constraint, and projects the
/// inner — no special handling of the variable form.
#[test]
fn moo_isa_via_constraint_variable_projects_inner() {
    let fa = build_fa(
        "package T;\nuse Moo;\nuse Types::Standard qw/InstanceOf/;\nmy $t = InstanceOf['My::Thing'];\nhas thing => (is => 'ro', isa => $t);\n1;\n",
    );
    assert_eq!(
        fa.sub_return_type_at_arity("thing", Some(0)),
        Some(InferredType::ClassName("My::Thing".to_string())),
        "isa => $constraint_var must project the constrained inner onto the accessor",
    );
}

// ---- isa coverage: the TypeConstraintOf path + the string/bareword split ----

/// String/bareword isa (the Moose idiom + builtins) stays on the meaning-map,
/// untouched by the constraint path. Regression guard that adding the node
/// path didn't break the common forms.
#[test]
fn moo_string_isa_forms_still_resolve() {
    let fa = build_fa(
        "package T;\nuse Moo;\nhas s => (is=>'ro', isa=>'Str');\nhas i => (is=>'ro', isa=>'Int');\nhas h => (is=>'ro', isa=>'HashRef');\n1;\n",
    );
    assert_eq!(fa.sub_return_type_at_arity("s", Some(0)), Some(InferredType::String));
    assert_eq!(fa.sub_return_type_at_arity("i", Some(0)), Some(InferredType::Numeric));
    assert_eq!(fa.sub_return_type_at_arity("h", Some(0)), Some(InferredType::HashRef));
}

/// `is => 'rw'` synthesizes a writer too; both getter (arity 0) and writer
/// (arity ≥1) return the constrained inner class.
#[test]
fn moo_instanceof_isa_types_both_getter_and_writer() {
    let fa = build_fa(
        "package T;\nuse Moo;\nuse Types::Standard qw/InstanceOf/;\nhas thing => (is=>'rw', isa=>InstanceOf['My::Thing']);\n1;\n",
    );
    let want = Some(InferredType::ClassName("My::Thing".to_string()));
    assert_eq!(fa.sub_return_type_at_arity("thing", Some(0)), want.clone(), "getter");
    assert_eq!(fa.sub_return_type_at_arity("thing", Some(1)), want, "rw writer");
}

/// `Maybe[InstanceOf['Foo']]` — the nested constructor is itself a constraint
/// value. The core types the inner call (`TypeConstraintOf(ClassName(Foo))`)
/// into the param's `ty`; the `Maybe` passthrough fold projects its inner, so
/// the accessor returns `Foo` (optionalness unmodeled — unwrap for resolution).
#[test]
fn moo_maybe_instanceof_isa_lifts_to_optional_class() {
    // `Maybe[T]` is undef-or-T, so the accessor is honestly maybe-undef.
    // It used to unwrap to a bare `My::Thing`, which threw the optionality
    // away — a claim the declaration does not make. Optional receivers still
    // dispatch leniently, so nothing downstream got harder; the deref lints
    // just see the truth now.
    let fa = build_fa(
        "package T;\nuse Moo;\nuse Types::Standard qw/Maybe InstanceOf/;\nhas thing => (is=>'ro', isa=>Maybe[InstanceOf['My::Thing']]);\n1;\n",
    );
    let t = fa.sub_return_type_at_arity("thing", Some(0));
    assert!(
        matches!(&t, Some(InferredType::Optional(inner)) if inner.class_name() == Some("My::Thing")),
        "Maybe[InstanceOf['My::Thing']] must type the accessor Optional<My::Thing>, got {t:?}",
    );
}

/// Bareword `isa => Int` — a 0-arity base constant is a constraint VALUE
/// exactly like `InstanceOf['X']`, so it must type the accessor the same way
/// the quoted `isa => 'Int'` spelling does.
#[test]
fn moo_bareword_base_constant_isa_types_accessor() {
    let fa = build_fa(
        "package T;\nuse Moo;\nuse Types::Standard qw/Int Str/;\nhas count => (is=>'ro', isa=>Int);\nhas label => (is=>'ro', isa=>Str);\n1;\n",
    );
    assert_eq!(
        fa.sub_return_type_at_arity("count", Some(0)),
        Some(InferredType::Numeric)
    );
    assert_eq!(
        fa.sub_return_type_at_arity("label", Some(0)),
        Some(InferredType::String)
    );
}

/// A package with its OWN `sub Str` and no Type::Tiny import gets its sub,
/// not a constraint. The constraint-name gate fires before local symbol
/// lookup, so without import scoping the user's sub would never compete —
/// and `Str`/`Int`/`Num` are not rare sub names.
#[test]
fn bareword_constraint_name_without_import_is_the_users_sub() {
    let fa = build_fa("package T;\nsub Str { return 42 }\nsub use_it { my $x = Str(); }\n1;\n");
    let t = fa.sub_return_type_at_arity("use_it", Some(0));
    assert!(
        !matches!(t, Some(InferredType::TypeConstraintOf(_))),
        "an unimported `Str` must not type as a constraint, got {t:?}",
    );
}

/// …and the same name IS a constraint once the package imports it.
#[test]
fn bareword_constraint_name_with_import_is_a_constraint() {
    let fa = build_fa(
        "package T;\nuse Moo;\nuse Types::Standard qw/Str/;\nhas s => (is=>'ro', isa=>Str);\n1;\n",
    );
    assert_eq!(
        fa.sub_return_type_at_arity("s", Some(0)),
        Some(InferredType::String)
    );
}

/// `Maybe[Int]` composes both halves: the base constant folds to its rep,
/// then `Maybe` lifts it.
#[test]
fn moo_bareword_maybe_int_isa_types_optional_numeric() {
    let fa = build_fa(
        "package T;\nuse Moo;\nuse Types::Standard qw/Maybe Int/;\nhas count => (is=>'ro', isa=>Maybe[Int]);\n1;\n",
    );
    let t = fa.sub_return_type_at_arity("count", Some(0));
    assert!(
        matches!(&t, Some(InferredType::Optional(inner)) if matches!(&**inner, InferredType::Numeric)),
        "Optional<Numeric>, got {t:?}",
    );
}

/// `ConsumerOf['Role']` shares the ClassParam shape (you can call the role's
/// methods on the value) — declared by the same plugin manifest entry.
#[test]
fn moo_consumerof_isa_types_accessor() {
    let fa = build_fa(
        "package T;\nuse Moo;\nuse Types::Standard qw/ConsumerOf/;\nhas r => (is=>'ro', isa=>ConsumerOf['My::Role']);\n1;\n",
    );
    assert_eq!(
        fa.sub_return_type_at_arity("r", Some(0)),
        Some(InferredType::ClassName("My::Role".to_string())),
    );
}

/// crm writes `InstanceOf ['Class']` (space before the bracket). Both spacings
/// parse as the same call node, so both must type.
#[test]
fn moo_instanceof_isa_handles_space_before_bracket() {
    let fa = build_fa(
        "package T;\nuse Moo;\nuse Types::Standard qw/InstanceOf/;\nhas thing => (is=>'ro', isa=>InstanceOf ['My::Thing']);\n1;\n",
    );
    assert_eq!(
        fa.sub_return_type_at_arity("thing", Some(0)),
        Some(InferredType::ClassName("My::Thing".to_string())),
    );
}

/// Moose mode, not just Moo — same constraint vocabulary.
#[test]
fn moose_instanceof_isa_types_accessor() {
    let fa = build_fa(
        "package T;\nuse Moose;\nuse Types::Standard qw/InstanceOf/;\nhas thing => (is=>'ro', isa=>InstanceOf['My::Thing']);\n1;\n",
    );
    assert_eq!(
        fa.sub_return_type_at_arity("thing", Some(0)),
        Some(InferredType::ClassName("My::Thing".to_string())),
    );
}

/// NEGATIVE: a coderef isa (`isa => sub {...}`) isn't a constraint — the
/// accessor must stay untyped, never falsely a class. Guards the projection
/// from over-firing on non-constraint complex RHS.
#[test]
fn moo_coderef_isa_leaves_accessor_untyped() {
    let fa = build_fa(
        "package T;\nuse Moo;\nhas thing => (is=>'ro', isa=>sub { die unless ref $_[0] });\n1;\n",
    );
    assert_eq!(
        fa.sub_return_type_at_arity("thing", Some(0)),
        None,
        "a coderef constraint has no class denotation",
    );
}

/// NEGATIVE: an undeclared constructor (`SomeType['X']`, not in any plugin's
/// type_constraint_names) falls through cleanly — no TypeConstraintOf, no
/// crash, accessor untyped.
#[test]
fn moo_unknown_constructor_isa_falls_through() {
    let fa = build_fa(
        "package T;\nuse Moo;\nhas thing => (is=>'ro', isa=>SomeUnknownType['X']);\n1;\n",
    );
    assert_eq!(fa.sub_return_type_at_arity("thing", Some(0)), None);
}

/// The chain payoff: an `InstanceOf` accessor's class flows into a downstream
/// method call. `$self->other->greet` must resolve `->greet` against `Other`
/// — this is the `$self->_minion->enqueue` shape that the crm fix turns on.
#[test]
fn instanceof_accessor_chains_into_method_call() {
    let src = "package Other;\nuse Moo;\nsub greet ($self) { return 'hi'; }\n\npackage T;\nuse Moo;\nuse Types::Standard qw/InstanceOf/;\nhas other => (is=>'ro', isa=>InstanceOf['Other']);\nsub use_it ($self) { return $self->other->greet; }\n1;\n";
    let tree = parse(src);
    let fa = build(&tree, src.as_bytes());
    let idx = crate::index::module_index::ModuleIndex::new_for_test();

    // Find the `greet` method-call node in `$self->other->greet`.
    fn find_call<'a>(n: tree_sitter::Node<'a>, src: &[u8], m: &str) -> Option<tree_sitter::Node<'a>> {
        if n.kind() == "method_call_expression" {
            if let Some(mn) = n.child_by_field_name("method") {
                if mn.utf8_text(src).ok() == Some(m) { return Some(n); }
            }
        }
        for i in 0..n.named_child_count() {
            if let Some(c) = n.named_child(i) {
                if let Some(f) = find_call(c, src, m) { return Some(f); }
            }
        }
        None
    }
    let call = find_call(tree.root_node(), src.as_bytes(), "greet").expect("has $self->other->greet");
    let method_node = call.child_by_field_name("method").unwrap();
    let hover = fa
        .hover_info(method_node.start_position(), src, Some(&idx))
        .expect("hover on ->greet resolves");
    assert!(
        hover.contains("Other"),
        "->greet on an InstanceOf['Other'] accessor must resolve against Other; got: {hover}",
    );
}

/// Option B resolves a receiver whose type comes from a Mojo HELPER, not a
/// plain method. `$c->minion` (a helper bridged to Mojolicious::Controller,
/// returning a Minion subclass) → `$c->minion->enqueue('T')` must synthesize
/// the dispatch. This is the gap that left `$app->minion`/`$c->minion`
/// chains dark: option-B's enrichment receiver-resolution now threads the
/// index (variable arm) and chases the helper bridge the way hover does.
#[test]
fn provisional_dispatch_resolves_helper_returned_receiver() {
    use crate::model::file_analysis::HandlerOwner;
    use std::path::PathBuf;
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.register_workspace_module(
        PathBuf::from("/tmp/b_hr_minion.pm"),
        std::sync::Arc::new(build_fa("package Acme::Minion;\nuse Mojo::Base 'Minion';\n1;\n")),
    );
    idx.register_workspace_module(
        PathBuf::from("/tmp/b_hr_plugin.pm"),
        std::sync::Arc::new(build_fa(
            "package Acme::Plugin;\nuse Mojo::Base 'Mojolicious::Plugin';\nsub register ($self, $app, $conf) {\n  my $m = Acme::Minion->new;\n  $app->helper(minion => sub {$m});\n  $app->minion->add_task('Task.go' => sub ($job) { 1 });\n}\n1;\n",
        )),
    );

    let fa = build_fa(
        "package Acme::Ctrl;\nuse Mojo::Base 'Mojolicious::Controller';\nsub act ($c) {\n  $c->minion->enqueue('Task.go');\n}\n1;\n",
    );

    // This file hits a Mojo trigger, so the emit-hook materializes the
    // DispatchCall directly (it doesn't gate on the receiver). The handler
    // is surfaced either way; `applicable_dispatches` de-dups the gated
    // candidate against the materialized ref so there's no double-count.
    let has_materialized = fa.refs().iter().any(|r|
        matches!(&r.kind, RefKind::DispatchCall { dispatcher } if dispatcher == "enqueue")
            && matches!(r.handler_owner(), Some(HandlerOwner::Class(c)) if c == "Minion")
            && r.target_name == "Task.go");
    let applied = fa.applicable_dispatches(Some(&idx));
    let has_gated = applied.iter().any(|a|
        a.name == "Task.go" && a.owner == HandlerOwner::Class("Minion".into()));
    assert!(
        has_materialized ^ has_gated,
        "the helper-returned receiver $c->minion (Acme::Minion isa Minion) enqueue \
         must surface exactly once — via the emit-hook ref OR the gated candidate, \
         never both; materialized={has_materialized} gated={has_gated} applied={:?}",
        applied,
    );
    assert!(
        has_materialized,
        "this file hits a Mojo trigger, so the emit-hook materializes the dispatch",
    );
}

/// Role-contract parameter typing: a plugin's `param_types()` manifest types a
/// named param of a sub declared in a class that does the rule's role. The
/// motivating case is `Clove::Upgrade::OneTime`'s `run_upgrade ($self, $app)`,
/// where `$app` is the Mojolicious app — a type the source can't express and
/// no callback-arg hook can reach (it's a plain sub declaration).
mod param_types_manifest {
    use super::*;
    use crate::build::plugin::{
        CompletionQueryContext, FrameworkPlugin, ParamType, PluginCompletionAnswer,
        PluginRegistry, PluginSigHelpAnswer, SigHelpQueryContext, Trigger,
    };
    use std::sync::Arc;

    struct UpgradeRolePlugin;
    impl FrameworkPlugin for UpgradeRolePlugin {
        fn id(&self) -> &str { "upgrade-role-test" }
        fn triggers(&self) -> &[Trigger] {
            static T: [Trigger; 1] = [Trigger::Always];
            &T
        }
        fn param_types(&self) -> &[ParamType] {
            // Built lazily into a static so the &[] borrow is 'static.
            use std::sync::OnceLock;
            static PT: OnceLock<Vec<ParamType>> = OnceLock::new();
            PT.get_or_init(|| {
                vec![ParamType {
                    method: Some("run_upgrade".into()),
                    in_role: "My::Upgrade::Role".into(),
                    param: 1,
                    type_class: "Mojolicious".into(),
                    requires_action_attr: false,
                    implicit_action_names: vec![],
                    from_loader_config: false,
                }]
            })
        }
        fn on_signature_help(&self, _: &SigHelpQueryContext) -> Option<PluginSigHelpAnswer> { None }
        fn on_completion(&self, _: &CompletionQueryContext) -> Option<PluginCompletionAnswer> { None }
    }

    fn build_with_upgrade(source: &str) -> FileAnalysis {
        let mut reg = PluginRegistry::new();
        // `use Moo; with 'Role'` needs moo.rhai's framework_mode_makers()
        // manifest for the mode (and thus `with` handling) to engage.
        let engine = Arc::new(crate::build::plugin::rhai_host::make_engine());
        let moo = crate::build::plugin::rhai_host::load_bundled_plugin("moo", engine)
            .expect("bundled moo plugin loads");
        reg.register(moo);
        reg.register(Box::new(UpgradeRolePlugin));
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        crate::build::builder::build_with_plugins(&tree, source.as_bytes(), Arc::new(reg))
    }

    #[test]
    fn role_doer_run_upgrade_app_param_typed() {
        // `use Moo; with 'Role'` populates package_parents (core framework
        // handling); the manifest then types `$app` as Mojolicious.
        let fa = build_with_upgrade(
            "package My::Doer;\nuse Moo;\nwith 'My::Upgrade::Role';\nsub run_upgrade ($self, $app) {\n  my $x = $app;\n}\n1;\n",
        );
        let ty = fa
            .inferred_type_via_bag("$app", Point::new(4, 10))
            .expect("$app should be typed by the param_types manifest");
        assert!(
            matches!(&ty, InferredType::ClassName(c) if c == "Mojolicious"),
            "role-contract param typing should make $app a Mojolicious, got {:?}",
            ty,
        );
    }

    #[test]
    fn non_doer_same_method_name_not_typed() {
        // Same method name, but the class does NOT do the role → no typing
        // (the rule is role-gated, not name-gated).
        let fa = build_with_upgrade(
            "package Other;\nsub run_upgrade ($self, $app) {\n  my $x = $app;\n}\n1;\n",
        );
        assert_eq!(
            fa.inferred_type_via_bag("$app", Point::new(2, 10)),
            None,
            "a class that doesn't do the role must not get the contract param type",
        );
    }

    // ---- Cross-file manifest-applicability probes ----
    // Whether build-time `transitive_parents`-gated plugin behavior reaches a
    // class whose ancestry is established cross-file. See
    // `docs/prompt-enrichment-inheritance-residual.md`.

    /// `ClassIsa`-triggered plugin emission on a class whose trigger-class
    /// ancestry is only established CROSS-FILE. `Leaf` extends `Mid` (a
    /// cross-file module) which extends `Mojo::EventEmitter`. The
    /// mojo-events plugin's `ClassIsa: "Mojo::EventEmitter"` trigger walks
    /// local `transitive_parents` (builder is index-free during the walk),
    /// which sees only `Mid` — not the cross-file `Mojo::EventEmitter`.
    /// Enrichment can't help: plugin emit hooks fire at parse time, inside
    /// `build()`, before any module index exists.
    ///
    /// Landed via the `GatedEmission` seam: the build defers the
    /// syntactically-matched emission (its `ClassIsa` trigger can't see the
    /// cross-file parent, rule #1), and `enrich_imported_types_with_keys`
    /// re-fires it once `class_isa_prefix` confirms the ancestry against the
    /// module index. See `docs/adr/receiver-gated-dispatch.md` (Phase 2).
    #[test]
    fn probe_class_isa_trigger_through_cross_file_parent() {
        use crate::index::module_index::ModuleIndex;
        use std::path::PathBuf;
        let idx = ModuleIndex::new_for_test();
        idx.set_workspace_root(None);
        idx.insert_cache(
            "Mid",
            Some(fake_cached_for_class(
                "Mid",
                &PathBuf::from("/fake/Mid.pm"),
                &[],
                &["Mojo::EventEmitter"],
            )),
        );
        let src = "package Leaf;\nuse parent 'Mid';\nsub wire {\n  my $self = shift;\n  $self->on('ready', sub { 1 });\n}\n1;\n";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let mut fa = crate::build::builder::build(&tree, src.as_bytes());
        fa.enrich_imported_types_with_keys(Some(&idx));
        let ready = fa.symbols().iter().filter(|s| {
            s.kind == SymKind::Handler && s.name == "ready"
                && matches!(&s.namespace, Namespace::Framework { id } if id == "mojo-events")
        }).count();
        assert_eq!(
            ready, 1,
            "mojo-events ClassIsa trigger should fire via cross-file parent chain"
        );
    }

    /// The DBIC flagship shape, TWO cross-file hops: a result class `Leaf`
    /// extends `Mid` (file 2) which extends `Base` + `DBIx::Class::Core`
    /// (file 3 chain) — the `DBICTest::Schema::Artist → DBICTest::BaseResult
    /// → DBIx::Class::Core` idiom. `Leaf`'s `add_columns` / `has_many` are
    /// syntactically matched at build but the `ClassIsa("DBIx::Class")`
    /// trigger can't see the cross-file ancestry (rule #1), so the emission
    /// is DEFERRED. Enrichment re-fires it once `class_isa_prefix` walks
    /// `Leaf → Mid → DBIx::Class::Core` (prefix hit) through the module
    /// index. The 1-hop case (direct `use base 'DBIx::Class::Core'`) already
    /// fired at build; this proves the multi-hop path converges to the same
    /// synthesis.
    #[test]
    fn dbic_class_isa_synthesis_through_two_cross_file_hops() {
        use crate::index::module_index::ModuleIndex;
        use std::path::PathBuf;
        let idx = ModuleIndex::new_for_test();
        idx.set_workspace_root(None);
        // File 3-ish: the intermediate base whose OWN parent list carries
        // `DBIx::Class::Core` (mirrors `DBICTest::BaseResult`'s
        // `use base qw(DBICTest::Base DBIx::Class::Core)`).
        idx.insert_cache(
            "Mid",
            Some(fake_cached_for_class(
                "Mid",
                &PathBuf::from("/fake/Mid.pm"),
                &[],
                &["Base", "DBIx::Class::Core"],
            )),
        );
        // Leaf: the result class, two hops from `DBIx::Class`.
        let src = "package Leaf;\nuse base 'Mid';\n__PACKAGE__->add_columns(qw/id name/);\n__PACKAGE__->has_many(comments => 'Schema::Comment', 'post_id');\n1;\n";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let mut fa = crate::build::builder::build(&tree, src.as_bytes());
        // Nothing synthesized yet — the ClassIsa gate saw only local `Mid`.
        assert_eq!(
            fa.symbols().iter().filter(|s| s.name == "comments").count(),
            0,
            "the relationship accessor must NOT synthesize before enrichment \
             (the gate can't see cross-file ancestry at build)",
        );
        fa.enrich_imported_types_with_keys(Some(&idx));
        let col = fa.symbols().iter().find(|s| {
            s.name == "name"
                && s.kind == SymKind::Method
                && matches!(&s.namespace, Namespace::Framework { id } if id == "dbic")
        });
        assert!(
            col.is_some(),
            "the `name` column accessor should synthesize via 2-hop cross-file \
             DBIx::Class ancestry after enrichment",
        );
        let rel = fa.symbols().iter().find(|s| {
            s.name == "comments"
                && s.kind == SymKind::Method
                && matches!(&s.namespace, Namespace::Framework { id } if id == "dbic")
        });
        assert!(rel.is_some(), "the `comments` has_many accessor should synthesize");
        let rt = fa.symbol_return_type_via_bag(rel.unwrap().id, None);
        assert!(
            matches!(&rt, Some(InferredType::ClassName(c)) if c == "DBIx::Class::ResultSet"),
            "has_many accessor must return a ResultSet, got {:?}",
            rt,
        );
        // Idempotent: a second enrichment must not double the accessors.
        fa.enrich_imported_types_with_keys(Some(&idx));
        assert_eq!(
            fa.symbols().iter().filter(|s| s.name == "comments").count(),
            1,
            "re-enrichment must not stack a second `comments` accessor \
             (truncate-to-baseline idempotency)",
        );
    }

    /// A class with NO cross-file route to `DBIx::Class` must not get DBIC
    /// synthesis, even though it calls `has_many` syntactically — the gate is
    /// ancestry, never the call name (rule #10).
    #[test]
    fn dbic_synthesis_not_applied_without_ancestry() {
        use crate::index::module_index::ModuleIndex;
        use std::path::PathBuf;
        let idx = ModuleIndex::new_for_test();
        idx.set_workspace_root(None);
        idx.insert_cache(
            "Mid",
            Some(fake_cached_for_class(
                "Mid",
                &PathBuf::from("/fake/Mid.pm"),
                &[],
                &["Some::Unrelated::Base"],
            )),
        );
        let src = "package Leaf;\nuse base 'Mid';\n__PACKAGE__->has_many(comments => 'Schema::Comment');\n1;\n";
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let mut fa = crate::build::builder::build(&tree, src.as_bytes());
        fa.enrich_imported_types_with_keys(Some(&idx));
        assert_eq!(
            fa.symbols().iter().filter(|s| s.name == "comments").count(),
            0,
            "no DBIx::Class ancestry ⇒ no synthesis, even with a has_many call",
        );
    }

    /// Dispatch-verb resolution in a NON-OPEN workspace/dependency file
    /// whose dispatch receiver `isa Minion` only CROSS-FILE and which does
    /// NOT `use Minion` itself. The minion plugin's emit-hook path
    /// (`UsesModule`/`ClassIsa` trigger) doesn't fire — only the
    /// trigger-independent `dispatch_verbs()` manifest captures a gated
    /// candidate. Under the query-time `ReceiverGated` seam the file is built
    /// WITHOUT enrichment (as the workspace indexer does) yet
    /// `applicable_dispatches` resolves the receiver cross-file and surfaces
    /// the call site. See `docs/adr/receiver-gated-dispatch.md`.
    #[test]
    fn dispatch_resolves_query_time_in_unenriched_workspace_file() {
        use crate::index::module_index::ModuleIndex;
        use std::path::PathBuf;
        let idx = ModuleIndex::new_for_test();
        idx.set_workspace_root(None);
        // `My::Minion` isa Minion, cross-file. The worker file below never
        // `use`s Minion — the emit-hook trigger can't fire, only the
        // receiver-isa manifest candidate, gated and resolved at query time.
        idx.insert_cache(
            "My::Minion",
            Some(fake_cached_for_class(
                "My::Minion",
                &PathBuf::from("/fake/My/Minion.pm"),
                &["new"],
                &["Minion"],
            )),
        );
        let src = "package My::Worker;\nsub run {\n  my $self = shift;\n  my $minion = My::Minion->new;\n  $minion->enqueue('send_email');\n}\n1;\n";
        // Build exactly as the workspace indexer does: no enrichment.
        let fa = build_fa(src);
        let applied = fa.applicable_dispatches(Some(&idx));
        assert_eq!(
            applied.len(), 1,
            "workspace-indexed file (no enrichment) should resolve its enqueue \
             dispatch at query time via the cross-file receiver isa — else \
             cross-file handler references miss it; got {:?}",
            applied,
        );
        assert_eq!(applied[0].name, "send_email");
    }

    // Wildcard-method param_types: a rule with `method: None` applies to every
    // sub in the class — the Catalyst pattern where every action gets `$c` typed.
    struct CatalystPlugin;
    impl FrameworkPlugin for CatalystPlugin {
        fn id(&self) -> &str { "catalyst-test" }
        fn triggers(&self) -> &[Trigger] {
            static T: [Trigger; 1] = [Trigger::Always];
            &T
        }
        fn param_types(&self) -> &[ParamType] {
            use std::sync::OnceLock;
            static PT: OnceLock<Vec<ParamType>> = OnceLock::new();
            PT.get_or_init(|| {
                vec![ParamType {
                    method: None, // wildcard: every ACTION method in the class
                    in_role: "Catalyst::Controller".into(),
                    param: 1,
                    type_class: "Catalyst".into(),
                    // Mirror the real catalyst.rhai: only attribute-carrying
                    // actions get $c, not plain helper subs — plus the
                    // name-dispatched private actions the rule declares.
                    requires_action_attr: true,
                    implicit_action_names: ["begin", "end", "auto", "default", "index"]
                        .map(String::from)
                        .to_vec(),
                    from_loader_config: false,
                }]
            })
        }
        fn on_signature_help(&self, _: &SigHelpQueryContext) -> Option<PluginSigHelpAnswer> { None }
        fn on_completion(&self, _: &CompletionQueryContext) -> Option<PluginCompletionAnswer> { None }
    }

    fn build_with_catalyst(source: &str) -> FileAnalysis {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(CatalystPlugin));
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
        let tree = parser.parse(source, None).unwrap();
        crate::build::builder::build_with_plugins(&tree, source.as_bytes(), Arc::new(reg))
    }

    #[test]
    fn catalyst_action_c_typed_via_wildcard_manifest() {
        // A controller action: $c (param index 1) should type as Catalyst.
        // The wildcard rule fires regardless of the action method's name.
        let fa = build_with_catalyst(
            "package MyApp::Controller::Foo;\nuse parent 'Catalyst::Controller';\nsub index :Local {\n    my ($self, $c) = @_;\n    my $req = $c;\n}\n1;\n",
        );
        let ty = fa
            .inferred_type_via_bag("$c", Point::new(4, 14))
            .expect("$c should be typed by wildcard param_types manifest");
        assert!(
            matches!(&ty, InferredType::ClassName(c) if c == "Catalyst"),
            "wildcard param_types should make $c a Catalyst in every controller action, got {:?}",
            ty,
        );
    }

    #[test]
    fn catalyst_wildcard_typed_for_any_action_name() {
        // A differently-named action — the wildcard covers it too.
        let fa = build_with_catalyst(
            "package MyApp::Controller::Bar;\nuse parent 'Catalyst::Controller';\nsub list :Local {\n    my ($self, $c) = @_;\n    my $x = $c;\n}\n1;\n",
        );
        let ty = fa
            .inferred_type_via_bag("$c", Point::new(4, 12))
            .expect("$c should be typed regardless of action method name");
        assert!(
            matches!(&ty, InferredType::ClassName(c) if c == "Catalyst"),
            "wildcard param_types must apply to any action name, got {:?}",
            ty,
        );
    }

    /// The Phase-2 cross-file case: a controller in file A `extends` a base in
    /// file B which `isa Catalyst::Controller`. The controller's ancestry to
    /// the wildcard rule's `in_role` is established only CROSS-FILE, so the old
    /// build-time local-only `transitive_parents` gate dropped it. The gated TC
    /// resolves at query time with the module index in hand → `$c` types.
    #[test]
    fn catalyst_wildcard_c_typed_through_cross_file_base() {
        use crate::index::module_index::ModuleIndex;
        use std::path::PathBuf;
        let idx = ModuleIndex::new_for_test();
        idx.set_workspace_root(None);
        // `MyApp::ControllerBase` isa Catalyst::Controller, cross-file. The
        // controller below `extends` it — its ancestry to the role is two hops,
        // through a class in another file the builder never sees.
        idx.insert_cache(
            "MyApp::ControllerBase",
            Some(fake_cached_for_class(
                "MyApp::ControllerBase",
                &PathBuf::from("/fake/MyApp/ControllerBase.pm"),
                &[],
                &["Catalyst::Controller"],
            )),
        );
        let src = "package MyApp::Controller::Deep;\nuse parent 'MyApp::ControllerBase';\nsub show :Local {\n    my ($self, $c) = @_;\n    my $req = $c;\n}\n1;\n";
        // Build as the workspace indexer does (no enrichment); the gated TC
        // rides the FA and resolves cross-file at query time.
        let fa = build_with_catalyst(src);
        let ty = fa
            .inferred_type_via_bag_ctx("$c", Point::new(4, 14), Some(&idx))
            .expect("$c should type via the cross-file Catalyst::Controller ancestry");
        assert!(
            matches!(&ty, InferredType::ClassName(c) if c == "Catalyst"),
            "wildcard param_types must type $c when Catalyst::Controller is a \
             cross-file ancestor, got {:?}",
            ty,
        );
    }

    #[test]
    fn catalyst_wildcard_not_applied_outside_controller() {
        // A package without the Catalyst::Controller ancestor must not get $c typed.
        let fa = build_with_catalyst(
            "package OtherPackage;\nsub index {\n    my ($self, $c) = @_;\n    my $x = $c;\n}\n1;\n",
        );
        assert_eq!(
            fa.inferred_type_via_bag("$c", Point::new(3, 12)),
            None,
            "wildcard rule must not type $c in a package that doesn't isa Catalyst::Controller",
        );
    }

    /// P1.4 — the real metacpan shape, through the actual hover query path: a
    /// controller reaches `Catalyst::Controller` through a *workspace
    /// intermediate* base that is itself a child of the role class. The leaf's
    /// local `package_parents` only knows its direct parent; reaching the role
    /// requires `class_isa` to chase the intermediate's parents through the
    /// module index. The bug was the hover path (`format_symbol_hover_at`)
    /// dropping the index — this exercises `hover_info` end-to-end so it
    /// fails on pre-fix code.
    #[test]
    fn catalyst_c_typed_through_workspace_intermediate_via_hover() {
        use crate::index::module_index::ModuleIndex;
        use std::path::PathBuf;
        let idx = ModuleIndex::new_for_test();
        idx.set_workspace_root(None);
        // Intermediate base, registered as a workspace module (like a project
        // `lib/.../Controller.pm`): its parent (`Catalyst::Controller`) lives
        // only in the index, NOT in the leaf's local `package_parents`.
        idx.register_workspace_module(
            PathBuf::from("/fake/MetaCPAN/Web/Controller.pm"),
            std::sync::Arc::new(build_fa(
                "package MetaCPAN::Web::Controller;\nuse parent 'Catalyst::Controller';\nsub pageset {\n    my ($self, $page) = @_;\n}\n1;\n",
            )),
        );
        // Leaf controller: extends only the workspace intermediate.
        let src = "package MetaCPAN::Web::Controller::Author;\nuse parent 'MetaCPAN::Web::Controller';\nsub root :Chained {\n    my ($self, $c, $id) = @_;\n    my $x = $c;\n}\n1;\n";
        let fa = build_with_catalyst(src);
        // Hover on the `$c` usage (row 4, the `my $x = $c;` line). The hover
        // path resolves the variable's type — only typed correctly if the
        // index is threaded all the way to the gated-param query.
        let hover = fa
            .hover_info(Point::new(4, 12), src, Some(&idx))
            .expect("hover should produce info for $c");
        assert!(
            hover.contains("type: Catalyst"),
            "3-hop cross-file ancestry through a workspace base must type $c in \
             hover (the path that dropped the index), got: {}",
            hover,
        );
    }

    /// P1.3 — the attribute gate: in the SAME controller, an action (`:Local`)
    /// gets `$c`, but a plain helper sub (no action attribute) must NOT get its
    /// 2nd param typed. The honest action signal is the attribute, not the
    /// parameter position.
    #[test]
    fn catalyst_non_action_helper_second_param_not_typed() {
        let fa = build_with_catalyst(
            "package MyApp::Controller::Foo;\nuse parent 'Catalyst::Controller';\nsub show :Local {\n    my ($self, $c) = @_;\n    my $a = $c;\n}\nsub pageset {\n    my ($self, $page) = @_;\n    my $b = $page;\n}\n1;\n",
        );
        // The action's $c IS typed.
        let c_ty = fa
            .inferred_type_via_bag("$c", Point::new(4, 12))
            .expect("action $c should be typed");
        assert!(
            matches!(&c_ty, InferredType::ClassName(c) if c == "Catalyst"),
            "action method's $c must type as Catalyst, got {:?}",
            c_ty,
        );
        // The plain helper's 2nd param ($page) must NOT be typed — no attribute.
        assert_eq!(
            fa.inferred_type_via_bag("$page", Point::new(8, 12)),
            None,
            "a non-action helper's 2nd param must NOT be typed Catalyst (P1.3 \
             over-application); only attribute-carrying actions receive $c",
        );
    }

    /// Catalyst private-action names (`begin`/`end`/`auto`/`default`/`index`)
    /// are dispatched by name alone — no action attribute. The rule DECLARES
    /// them (`implicit_action_names`, mirroring catalyst.rhai), so the
    /// `requires_action_attr` gate lets them through and `$c` still types.
    #[test]
    fn catalyst_private_action_names_type_c_without_attr() {
        // `sub end { my ($self,$c)=@_ }` in a controller — no attribute.
        let fa = build_with_catalyst(
            "package MyApp::Controller::Root;\nuse parent 'Catalyst::Controller';\nsub end {\n    my ($self, $c) = @_;\n    my $r = $c;\n}\n1;\n",
        );
        let ty = fa
            .inferred_type_via_bag("$c", Point::new(4, 12))
            .expect("private-action end: $c should be typed even without an attribute");
        assert!(
            matches!(&ty, InferredType::ClassName(c) if c == "Catalyst"),
            "sub end without action attr must type $c as Catalyst, got {:?}",
            ty,
        );
    }

    /// A plain helper sub whose name happens to NOT be a private-action name
    /// and carries no action attribute must still be excluded.
    #[test]
    fn catalyst_plain_helper_not_a_private_action() {
        let fa = build_with_catalyst(
            "package MyApp::Controller::Root;\nuse parent 'Catalyst::Controller';\nsub helper {\n    my ($self, $x) = @_;\n    my $r = $x;\n}\n1;\n",
        );
        assert_eq!(
            fa.inferred_type_via_bag("$x", Point::new(4, 12)),
            None,
            "non-action, non-private-action helper must NOT have $x typed as Catalyst",
        );
    }

    // The name-dispatched exemption is PER-RULE, not a core allowlist: an
    // attribute-gated rule from another framework that declares NO
    // implicit_action_names must not inherit Catalyst's private-action
    // names — `sub begin` without an attribute stays untyped.
    struct StrictAttrPlugin;
    impl FrameworkPlugin for StrictAttrPlugin {
        fn id(&self) -> &str { "strict-attr-test" }
        fn triggers(&self) -> &[Trigger] {
            static T: [Trigger; 1] = [Trigger::Always];
            &T
        }
        fn param_types(&self) -> &[ParamType] {
            use std::sync::OnceLock;
            static PT: OnceLock<Vec<ParamType>> = OnceLock::new();
            PT.get_or_init(|| {
                vec![ParamType {
                    method: None,
                    in_role: "Other::Framework::Handler".into(),
                    param: 1,
                    type_class: "Other::Framework".into(),
                    requires_action_attr: true,
                    implicit_action_names: vec![],
                    from_loader_config: false,
                }]
            })
        }
        fn on_signature_help(&self, _: &SigHelpQueryContext) -> Option<PluginSigHelpAnswer> { None }
        fn on_completion(&self, _: &CompletionQueryContext) -> Option<PluginCompletionAnswer> { None }
    }

    #[test]
    fn attr_gated_rule_without_declared_names_exempts_nothing() {
        let mut reg = PluginRegistry::new();
        reg.register(Box::new(StrictAttrPlugin));
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
        let src = "package My::Handler;\nuse parent 'Other::Framework::Handler';\nsub begin {\n    my ($self, $c) = @_;\n    my $r = $c;\n}\n1;\n";
        let tree = parser.parse(src, None).unwrap();
        let fa = crate::build::builder::build_with_plugins(&tree, src.as_bytes(), Arc::new(reg));
        assert_eq!(
            fa.inferred_type_via_bag("$c", Point::new(4, 12)),
            None,
            "`begin` is Catalyst vocabulary; a rule that declares no \
             implicit_action_names must not exempt it from the attribute gate",
        );
    }
}
