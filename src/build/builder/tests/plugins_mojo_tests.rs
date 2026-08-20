use super::*;

// ---- Framework plugin integration ----

/// End-to-end: the bundled `mojo-events` Rhai plugin should synthesize a
/// `HashKeyDef` for every literal event name passed to `->on(...)`
/// inside a class that inherits from `Mojo::EventEmitter`. This is the
/// proof that Rhai scripts emit real symbols that land in FileAnalysis
/// with the `Framework` namespace stamp.
#[test]
fn plugin_mojo_events_on_literal_emits_handler_symbol() {
    let src = r#"
package My::Emitter;
use parent 'Mojo::EventEmitter';

sub new {
    my $class = shift;
    my $self = bless {}, $class;
    $self->on('connect', sub { ... });
    $self->on('message', sub { ... });
    $self;
}

1;
"#;
    let fa = build_fa(src);

    let handlers: Vec<&Symbol> = fa
        .symbols()
        .iter()
        .filter(|s| {
            s.kind == SymKind::Handler
                && matches!(&s.namespace, Namespace::Framework { id } if id == "mojo-events")
        })
        .collect();

    let names: std::collections::HashSet<&str> = handlers.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains("connect"),
        "mojo-events should emit Handler for 'connect'; got: {:?}",
        names
    );
    assert!(
        names.contains("message"),
        "mojo-events should emit Handler for 'message'; got: {:?}",
        names
    );

    // Each Handler should also carry the dispatcher set — at minimum
    // 'emit' (the canonical Mojo dispatch method).
    for h in &handlers {
        if let SymbolDetail::Handler { dispatchers, .. } = &h.detail {
            assert!(
                dispatchers.iter().any(|d| d == "emit"),
                "Handler for {} should declare `emit` dispatcher",
                h.name
            );
        } else {
            panic!("expected Handler detail on {}", h.name);
        }
    }
}

/// Dynamic event names must not produce spurious HashKeyDefs.
#[test]
fn plugin_mojo_events_dynamic_name_does_not_emit() {
    let src = r#"
package My::Emitter;
use parent 'Mojo::EventEmitter';

sub wire {
    my ($self, $name) = @_;
    $self->on($name, sub { ... });
}

1;
"#;
    let fa = build_fa(src);
    let plugin_handlers: Vec<&Symbol> = fa
        .symbols()
        .iter()
        .filter(|s| {
            s.kind == SymKind::Handler
                && matches!(&s.namespace, Namespace::Framework { id } if id == "mojo-events")
        })
        .collect();
    assert!(
        plugin_handlers.is_empty(),
        "dynamic event name must not emit handlers; got: {:?}",
        plugin_handlers.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

/// Const folding through the plugin: `my $name = 'connect'; ...` means
/// the plugin receives `arg.string_value == "connect"` and emits a
/// symbol named "connect" — not "$name". The plugin itself contains no
/// folding logic; the builder does it once in `arg_info_for` and every
/// plugin gets folded values for free.
#[test]
fn plugin_mojo_events_const_folds_scalar_event_name() {
    let src = r#"
package My::Emitter;
use parent 'Mojo::EventEmitter';

sub wire {
    my $self = shift;
    my $evt = 'disconnect';
    $self->on($evt, sub { ... });
}

1;
"#;
    let fa = build_fa(src);

    let names: std::collections::HashSet<&str> = fa
        .symbols()
        .iter()
        .filter(|s| {
            s.kind == SymKind::Handler
                && matches!(&s.namespace, Namespace::Framework { id } if id == "mojo-events")
        })
        .map(|s| s.name.as_str())
        .collect();
    assert!(
        names.contains("disconnect"),
        "const-folded event name should emit 'disconnect'; got: {:?}",
        names
    );
    assert!(
        !names.contains("$evt"),
        "variable text must not leak through as symbol name"
    );
}

/// Transitive inheritance: a class whose parent (in the same file)
/// extends Mojo::EventEmitter should still trigger the plugin. Proves
/// the builder's transitive_parents walk composes with `ClassIsa`.
#[test]
fn plugin_mojo_events_triggers_through_transitive_parent() {
    let src = r#"
package Mid;
use parent 'Mojo::EventEmitter';

package Leaf;
use parent 'Mid';

sub wire {
    my $self = shift;
    $self->on('ready', sub { ... });
}

1;
"#;
    let fa = build_fa(src);
    let ready: Vec<&Symbol> = fa
        .symbols()
        .iter()
        .filter(|s| {
            s.kind == SymKind::Handler
                && s.name == "ready"
                && matches!(&s.namespace, Namespace::Framework { id } if id == "mojo-events")
        })
        .collect();
    assert_eq!(
        ready.len(),
        1,
        "Leaf extends Mid extends Mojo::EventEmitter — plugin must fire transitively"
    );
}

/// Cross-file def/ref pairing. Producer.pm wires events via ->on, Consumer.pm
/// calls ->emit on a producer instance. Both plugin emissions end up with
/// `HashKeyOwner::Class("Producer")`, so `resolve::refs_to` finds the
/// consumer's access ref from the producer's def query — no LSP code is
/// plugin-aware.
#[test]
fn plugin_mojo_events_cross_file_ref_pairing() {
    use crate::model::file_analysis::HandlerOwner;
    use crate::index::file_store::FileStore;
    use crate::index::resolve::{refs_to, RoleMask, TargetKind, TargetRef};
    use std::path::PathBuf;

    let producer_src = r#"
package Producer;
use parent 'Mojo::EventEmitter';

sub new {
    my $class = shift;
    my $self = bless {}, $class;
    $self->on('ready', sub { warn "ready" });
    return $self;
}
1;
"#;
    let consumer_src = r#"
package Consumer;
use parent 'Mojo::EventEmitter';

sub run {
    my $p = Producer->new;
    $p->emit('ready');
    $p->unsubscribe('ready');
}
1;
"#;

    let store = FileStore::new();
    let producer_path = PathBuf::from("/tmp/plugin_producer.pm");
    let consumer_path = PathBuf::from("/tmp/plugin_consumer.pm");

    store.insert_workspace(producer_path.clone(), build_fa(producer_src));
    store.insert_workspace(consumer_path.clone(), build_fa(consumer_src));

    let results = refs_to(
        &store,
        None,
        &TargetRef {
            name: "ready".to_string(),
            kind: TargetKind::Handler {
                owner: HandlerOwner::Class("Producer".to_string()),
                name: "ready".to_string(),
            },
            method_classes: Vec::new(), scope: crate::index::resolve::OverrideScope::Dispatch, def_paths: Vec::new(), bare_constant: false,
        },
        RoleMask::EDITABLE,
    );

    let producer_hits = results
        .iter()
        .filter(|r| matches!(&r.key, crate::index::file_store::FileKey::Path(p) if p == &producer_path))
        .count();
    let consumer_hits = results
        .iter()
        .filter(|r| matches!(&r.key, crate::index::file_store::FileKey::Path(p) if p == &consumer_path))
        .count();

    assert!(
        producer_hits >= 1,
        "producer should have ≥1 hit (the ->on Handler def); results: {:?}",
        results
    );
    assert!(
        consumer_hits >= 1,
        "consumer should have ≥1 hit (the ->emit DispatchCall); results: {:?}",
        results
    );
}

/// mojo-helpers: Phase-2 architecture emits ONE Method per helper,
/// owned by `Mojolicious::Controller` — the canonical home for
/// controller-callable helpers. The PluginNamespace's bridges cover
/// both Controller and Mojolicious so `$c->name` AND `$app->name`
/// both resolve through namespace lookup (no Symbol fan-out).
#[test]
fn plugin_mojo_helpers_registers_method_on_controller() {
    let src = r#"
package MyApp::Lite;
use Mojolicious::Lite;

my $app = Mojolicious->new;
$app->helper(current_user => sub {
    my ($c, $extra) = @_;
    return { id => 1 };
});
"#;
    let fa = build_fa(src);

    let helpers: Vec<&Symbol> = fa
        .symbols()
        .iter()
        .filter(|s| {
            s.kind == SymKind::Method
                && s.name == "current_user"
                && matches!(&s.namespace, Namespace::Framework { id } if id == "mojo-helpers")
        })
        .collect();

    assert_eq!(
        helpers.len(),
        1,
        "one Method per helper (no fan-out — Phase 2)"
    );
    let helper = helpers[0];
    assert_eq!(
        helper.package.as_deref(),
        Some(crate::model::file_analysis::APP_SURFACE_CLASS),
        "canonical home is the fictional app surface"
    );

    if let SymbolDetail::Sub { params, .. } = &helper.detail {
        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["$c", "$extra"],
            "helper's sub params flow through to the Method signature"
        );
    } else {
        panic!("helper detail should be Sub");
    }
    assert_eq!(
        helper.presentation.display,
        Some(crate::model::file_analysis::HandlerDisplay::Helper),
        "helpers render as HandlerDisplay::Helper — the LSP kind is \
             FUNCTION (the enum doesn't have Helper), the outline word \
             is 'helper'. See HandlerDisplay::outline_word.",
    );

    // The PluginNamespace owns the bridge visibility: a SINGLE bridge
    // to the fictional app surface (docs/adr/plugin-system.md). The
    // consumer classes reach it via the synthetic-parent edge in core,
    // not via a per-helper bridge list.
    let ns = fa
        .plugin.namespaces
        .iter()
        .find(|n| n.plugin_id == "mojo-helpers" && n.entities.contains(&helper.id))
        .expect("helper belongs to a mojo-helpers namespace");
    let bridge_classes: std::collections::HashSet<&str> = ns
        .bridges
        .iter()
        .map(|Bridge::Class(c)| c.as_str())
        .collect();
    assert_eq!(
        bridge_classes,
        std::iter::once(crate::model::file_analysis::APP_SURFACE_CLASS).collect(),
        "namespace bridges ONLY the app surface — open consumer set lives in core"
    );

    // The synthetic-ancestor edge: the helper resolves from BOTH the
    // app class and the controller class through the SAME ancestor walk,
    // even though neither is the helper's home package.
    for consumer in ["Mojolicious", "Mojolicious::Controller"] {
        let res = fa.resolve_method_in_ancestors(consumer, "current_user", None);
        match res {
            Some(crate::model::file_analysis::MethodResolution::Local { sym_id, .. }) => {
                assert_eq!(
                    sym_id, helper.id,
                    "{consumer}->current_user must resolve to the helper via the app surface"
                );
            }
            other => panic!(
                "{consumer}->current_user should resolve to the helper Local; got {other:?}"
            ),
        }
    }
}

/// Helper-fn `at` for `inferred_type_via_bag`: column past `my $c = shift;`
/// (or anywhere inside the sub body) so the TC's scope contains the point.
fn first_param_type(fa: &FileAnalysis, var: &str, body_line: usize, col: usize) -> Option<InferredType> {
    fa.inferred_type_via_bag(var, Point::new(body_line, col))
}

/// Named-sub helper registration (`->helper(greet => \&_greet)`): the
/// referenced sub's first positional is the controller, exactly like the
/// inline `sub ($c) {...}` form. `my $c = shift` unpacking.
#[test]
fn plugin_mojo_helpers_named_sub_typed_via_shift() {
    let src = r#"
package MyApp::Lite;
use Mojolicious::Lite;

my $app = Mojolicious->new;
$app->helper(greet => \&_greet);

sub _greet {
    my $c = shift;
    $c->render(text => 'hi');
}
"#;
    let fa = build_fa(src);
    // `$c` is declared on line 8 (`my $c = shift;`); query just after it.
    let ty = first_param_type(&fa, "$c", 8, 14);
    assert_eq!(
        ty,
        Some(InferredType::ClassName("Mojolicious::Controller".into())),
        "named-sub helper's first positional types as the controller"
    );
}

/// Same, signature form `sub _greet ($c, $name) {...}`.
#[test]
fn plugin_mojo_helpers_named_sub_typed_via_signature() {
    let src = r#"
package MyApp::Lite;
use Mojolicious::Lite;

my $app = Mojolicious->new;
$app->helper(greet => \&_greet);

sub _greet ($c, $name) {
    $c->render(text => $name);
}
"#;
    let fa = build_fa(src);
    let ty = first_param_type(&fa, "$c", 8, 8);
    assert_eq!(
        ty,
        Some(InferredType::ClassName("Mojolicious::Controller".into())),
        "signature-form named-sub helper's first positional types as the controller"
    );
}

/// Plain-comma spelling `helper('greet', \&_greet)` is identical to the
/// fat-comma form — fat-comma carries no code semantics (CLAUDE.md).
#[test]
fn plugin_mojo_helpers_named_sub_plain_comma() {
    let src = r#"
package MyApp::Lite;
use Mojolicious::Lite;

my $app = Mojolicious->new;
$app->helper('greet', \&_greet);

sub _greet {
    my $c = shift;
    $c->render;
}
"#;
    let fa = build_fa(src);
    let ty = first_param_type(&fa, "$c", 8, 14);
    assert_eq!(
        ty,
        Some(InferredType::ClassName("Mojolicious::Controller".into())),
        "plain-comma helper registration types the named sub identically"
    );
}

/// Regression: the inline-callback form still types `$c`.
#[test]
fn plugin_mojo_helpers_inline_callback_still_typed() {
    let src = r#"
package MyApp::Lite;
use Mojolicious::Lite;

my $app = Mojolicious->new;
$app->helper(greet => sub {
    my $c = shift;
    $c->render;
});
"#;
    let fa = build_fa(src);
    let ty = first_param_type(&fa, "$c", 6, 14);
    assert_eq!(
        ty,
        Some(InferredType::ClassName("Mojolicious::Controller".into())),
        "inline-callback helper still types its first positional"
    );
}

/// A named sub NOT registered as a helper (referenced via `\&` elsewhere,
/// plus a plain `sub`) gets no spurious controller typing.
#[test]
fn plugin_mojo_helpers_non_helper_named_sub_unaffected() {
    let src = r#"
package MyApp::Lite;
use Mojolicious::Lite;

my $cb = \&_other;

sub _other {
    my $c = shift;
    return $c;
}
"#;
    let fa = build_fa(src);
    let ty = first_param_type(&fa, "$c", 7, 14);
    assert_ne!(
        ty,
        Some(InferredType::ClassName("Mojolicious::Controller".into())),
        "a non-helper named sub must not be typed as a controller"
    );
}

/// Dotted helpers chain into namespace methods: `users.create` means
/// `$c->users->create`. Each non-leaf segment emits a parameterless
/// Method returning a synthetic proxy class; the leaf emits on the
/// innermost proxy with the helper's real params. Shared prefixes
/// dedup — `thing.hi` and `thing.there` must only ever produce one
/// `thing` symbol (not two), so completion + outline stay clean.
#[test]
fn plugin_mojo_helpers_dotted_chain_with_shared_prefix_dedup() {
    let src = r#"
package MyApp::Lite;
use Mojolicious::Lite;

my $app = Mojolicious->new;
$app->helper('thing.hi'    => sub { my ($c, $arg_a) = @_; });
$app->helper('thing.there' => sub { my ($c, $arg_b) = @_; });
"#;
    let fa = build_fa(src);

    // Exactly one `thing` method on the app surface (the chain root's
    // home; consumer classes reach it via the synthetic-parent edge),
    // despite two dotted helpers sharing that prefix.
    let thing_syms: Vec<&Symbol> = fa
        .symbols()
        .iter()
        .filter(|s| {
            s.name == "thing"
                && s.kind == SymKind::Method
                && s.package.as_deref() == Some(crate::model::file_analysis::APP_SURFACE_CLASS)
        })
        .collect();
    assert_eq!(
        thing_syms.len(),
        1,
        "shared prefix must dedup: one `thing` method, got {}",
        thing_syms.len()
    );

    // Its return_type is the shared proxy class.
    match fa.symbol_return_type_via_bag(thing_syms[0].id, None) {
        Some(InferredType::ClassName(n)) => {
            assert_eq!(n, "Mojolicious::Controller::_Helper::thing");
        }
        _ => panic!("thing's return type should be the shared proxy class"),
    }

    // Both leaves exist on the shared proxy class, each with its own params.
    let hi = fa
        .symbols()
        .iter()
        .find(|s| s.name == "hi" && s.kind == SymKind::Method)
        .expect("hi leaf emitted");
    let there = fa
        .symbols()
        .iter()
        .find(|s| s.name == "there" && s.kind == SymKind::Method)
        .expect("there leaf emitted");
    assert_eq!(
        hi.package.as_deref(),
        Some("Mojolicious::Controller::_Helper::thing")
    );
    assert_eq!(
        there.package.as_deref(),
        Some("Mojolicious::Controller::_Helper::thing")
    );
    if let SymbolDetail::Sub { params, .. } = &hi.detail {
        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["$c", "$arg_a"]);
    }
    if let SymbolDetail::Sub { params, .. } = &there.detail {
        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["$c", "$arg_b"]);
    }
}

/// Three-level dotted helper chains: `admin.users.purge` synthesizes
/// two intermediate proxies, each with the right return_type, and
/// the leaf lands on the innermost proxy.
#[test]
fn plugin_mojo_helpers_three_level_dotted_chain() {
    let src = r#"
package MyApp::Lite;
use Mojolicious::Lite;

my $app = Mojolicious->new;
$app->helper('admin.users.purge' => sub { my ($c, $force) = @_; });
"#;
    let fa = build_fa(src);

    let admin = fa
        .symbols()
        .iter()
        .find(|s| {
            s.name == "admin"
                && s.kind == SymKind::Method
                && s.package.as_deref() == Some(crate::model::file_analysis::APP_SURFACE_CLASS)
        })
        .expect("admin on app surface (chain root)");
    let users = fa
        .symbols()
        .iter()
        .find(|s| {
            s.name == "users"
                && s.kind == SymKind::Method
                && s.package.as_deref() == Some("Mojolicious::Controller::_Helper::admin")
        })
        .expect("users on admin proxy");
    let purge = fa
        .symbols()
        .iter()
        .find(|s| {
            s.name == "purge"
                && s.kind == SymKind::Method
                && s.package.as_deref() == Some("Mojolicious::Controller::_Helper::admin::users")
        })
        .expect("purge leaf on admin.users proxy");

    // Each non-leaf returns the next proxy in the chain.
    if let Some(InferredType::ClassName(n)) =
        fa.symbol_return_type_via_bag(admin.id, None)
    {
        assert_eq!(n, "Mojolicious::Controller::_Helper::admin");
    }
    if let Some(InferredType::ClassName(n)) =
        fa.symbol_return_type_via_bag(users.id, None)
    {
        assert_eq!(n, "Mojolicious::Controller::_Helper::admin::users");
    }
    // Leaf carries the helper's actual params.
    if let SymbolDetail::Sub { params, .. } = &purge.detail {
        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["$c", "$force"]);
    }
}

/// `use Mojolicious::Lite` autoimports a fixed verb set — our
/// unresolved-function diagnostic must skip them. The plugin's
/// `on_use` hook emits FrameworkImport actions for each; the
/// builder stashes them in framework_imports so the diagnostic
/// filter drops matching FunctionCall refs.
#[test]
fn plugin_mojo_lite_autoimports_verbs() {
    let src = r#"
package main;
use Mojolicious::Lite;

get '/x' => sub {};
post '/y' => sub {};
helper foo => sub {};
"#;
    let fa = build_fa(src);
    for verb in &[
        "get",
        "post",
        "put",
        "del",
        "patch",
        "any",
        "under",
        "websocket",
        "app",
        "helper",
        "hook",
        "plugin",
        "group",
    ] {
        assert!(
            fa.framework_imports.contains(*verb),
            "{} must be autoimported by use Mojolicious::Lite",
            verb
        );
    }
}

/// mojo-lite: top-level route verbs (`get`, `post`, etc.) register
/// Handlers keyed by URL path, with ["url_for"] as the dispatcher so
/// `url_for('/users')` can find them. Exercises the function-call
/// pattern shape that mojo-events doesn't use.
#[test]
fn plugin_mojo_lite_registers_handlers_for_routes() {
    let src = r#"
package main;
use Mojolicious::Lite;

get '/users' => sub {
    my ($c, $arg) = @_;
    $c->render(text => 'hi');
};

post '/login' => sub {
    my ($c, $user, $pw) = @_;
};

app->start;
"#;
    let fa = build_fa(src);

    let route_handlers: Vec<&Symbol> = fa
        .symbols()
        .iter()
        .filter(|s| {
            s.kind == SymKind::Handler
                && matches!(&s.namespace, Namespace::Framework { id } if id == "mojo-lite")
        })
        .collect();

    let names: std::collections::HashSet<&str> =
        route_handlers.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains("/users"),
        "GET /users handler emitted; got: {:?}",
        names
    );
    assert!(
        names.contains("/login"),
        "POST /login handler emitted; got: {:?}",
        names
    );

    // Each handler declares url_for as its dispatcher so completion
    // inside `url_for('|')` surfaces every route.
    for h in &route_handlers {
        if let SymbolDetail::Handler { dispatchers, .. } = &h.detail {
            assert!(
                dispatchers.iter().any(|d| d == "url_for"),
                "handler {} should dispatch via url_for",
                h.name
            );
        }
    }

    // Handler params come from the handler sub's signature —
    // different per route, so they round-trip correctly.
    let login = route_handlers.iter().find(|h| h.name == "/login").unwrap();
    if let SymbolDetail::Handler { params, .. } = &login.detail {
        let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["$c", "$user", "$pw"]);
    }
}

/// Routes are first-class things, not just refs. Every `->to(...)`
/// emits BOTH a MethodCallRef (cross-file target link) AND a
/// Handler symbol (route-as-entity — outline-visible, workspace-
/// searchable, discoverable via url_for completion). Mirrors the
/// mojo-lite model so route symbols are symmetric regardless of
/// declaration flavor.
#[test]
fn plugin_mojo_routes_emits_both_ref_and_handler_symbol() {
    let src = r#"
package MyApp;
use Mojolicious::Lite;

my $r = app->routes;
$r->get('/users')->to('Users#list');
$r->post('/users')->to(controller => 'Users', action => 'create');
"#;
    let fa = build_fa(src);

    // Each route: one MethodCallRef + one Handler symbol.
    let method_refs: Vec<&Ref> = fa
        .refs()
        .iter()
        .filter(|r| {
            matches!(r.kind, RefKind::MethodCall { .. })
                && (r.target_name == "list" || r.target_name == "create")
        })
        .collect();
    assert_eq!(method_refs.len(), 2, "one MethodCallRef per route");

    let route_syms: Vec<&Symbol> = fa
        .symbols()
        .iter()
        .filter(|s| {
            s.kind == SymKind::Handler
                && matches!(&s.namespace, Namespace::Framework { id } if id == "mojo-routes")
        })
        .collect();
    assert_eq!(
        route_syms.len(),
        2,
        "one Handler symbol per route so outline + workspace-symbol find them"
    );

    let names: std::collections::HashSet<&str> =
        route_syms.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains("Users#list"),
        "route identity `Users#list` present; got: {:?}",
        names
    );
    assert!(
        names.contains("Users#create"),
        "route identity `Users#create` present; got: {:?}",
        names
    );

    // Dispatcher is url_for so completion inside `url_for('|')`
    // offers every registered route.
    for s in &route_syms {
        if let SymbolDetail::Handler {
            dispatchers, owner, ..
        } = &s.detail
        {
            assert!(
                dispatchers.iter().any(|d| d == "url_for"),
                "route {} should dispatch via url_for",
                s.name
            );
            // Owner is `Mojolicious::Controller` — url_for is a
            // Controller method and the routes table is global
            // per Mojo's runtime model. Owning on Controller lets
            // `$c->url_for` in any controller resolve routes
            // declared in any app file through ancestor walking.
            // Not target-class (Users): routes exist independent
            // of their target; two routes can target the same
            // action (paginated/json/etc.).
            assert!(matches!(owner, HandlerOwner::Class(c) if c == "Mojolicious::Controller"),
                    "route owner is Mojolicious::Controller (shared base for url_for), not declaring package");
        }
    }
}

/// End-to-end cross-file gd for `->to('Users#list')`. Users.pm
/// is registered as a workspace module in ModuleIndex (via the
/// `register_workspace_module` bridge), so
/// `resolve_method_in_ancestors` finds it on lookup — and the
/// cross-file MethodCall path in `symbols::find_definition`
/// surfaces the Users::list def's location.
///
/// Before the fix, workspace modules lived only in FileStore so
/// lookups that key on module name (all cross-file method
/// resolution) missed them and gd fell through to noise.
#[test]
fn plugin_mojo_routes_gd_reaches_workspace_target() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;
    use tower_lsp::lsp_types::Position;

    let app_src = r#"
package MyApp;
use Mojolicious::Lite;

my $r = app->routes;
$r->get('/users')->to('Users#list');
"#;
    let users_src = r#"
package Users;
sub list { my ($c) = @_; }
1;
"#;

    let app_fa = build_fa(app_src);
    let users_fa = build_fa(users_src);

    let idx = ModuleIndex::new_for_test();
    // Simulate workspace indexing registering Users.pm under its
    // primary package name.
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Users.pm"),
        Arc::new(users_fa),
    );

    // Sanity: cross-file resolution on "Users"::"list" must succeed.
    let res = app_fa.resolve_method_in_ancestors("Users", "list", Some(&idx));
    assert!(
        res.is_some(),
        "Users::list must resolve cross-file after workspace register"
    );

    // And the MethodCallRef emitted by mojo-routes on the 'list'
    // portion of 'Users#list' should be at a span matching the
    // text 'list'.
    let route_ref = app_fa
        .refs()
        .iter()
        .find(|r| matches!(r.kind, RefKind::MethodCall { .. }) && r.target_name == "list")
        .expect("mojo-routes MethodCallRef for 'list'");

    // The ref's span is tight on the action name — mid-string
    // completion + goto-def both rely on that precision.
    let _ = route_ref;
    let _ = Position::default();
}

/// mojo-routes short form: `->to('Users#list')` emits a MethodCall
/// ref pointing to `Users::list`. Cursor on the string → gd jumps
/// cross-file to the Users controller's list method, same as any
/// regular method call. No routes-specific resolution code; it's
/// just a Ref that happens to live inside a string literal.
#[test]
fn plugin_mojo_routes_short_form_emits_method_call_ref() {
    let src = r#"
package MyApp;
use Mojolicious::Lite;

my $r = app->routes;
$r->get('/users')->to('Users#list');
"#;
    let fa = build_fa(src);

    let route_refs: Vec<&Ref> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::MethodCall { .. }) && r.target_name == "list")
        .collect();

    assert!(
        !route_refs.is_empty(),
        "at least one MethodCall ref for 'list'"
    );
    let r = route_refs
        .iter()
        .find(|r| matches!(&r.kind, RefKind::MethodCall { invocant, .. } if invocant.text() == "Users"))
        .expect("MethodCall with invocant=Users");

    // Sanity: ref span covers the string literal so cursor anywhere
    // in the 'Users#list' range lands on the ref.
    assert!(
        r.span.end.column > r.span.start.column,
        "method ref has non-empty span"
    );
}

/// mojo-routes long form: `->to(controller => 'Users', action => 'list')`.
/// Walks kwarg pairs, pairs up controller+action, emits the ref
/// with span on the action value.
#[test]
fn plugin_mojo_routes_long_form_emits_method_call_ref() {
    let src = r#"
package MyApp;
use Mojolicious::Lite;

my $r = app->routes;
$r->get('/users')->to(controller => 'Users', action => 'list');
"#;
    let fa = build_fa(src);

    let has_ref = fa.refs().iter().any(|r| {
        matches!(&r.kind, RefKind::MethodCall { invocant, .. } if invocant.text() == "Users")
            && r.target_name == "list"
    });
    assert!(
        has_ref,
        "long-form ->to(controller=>, action=>) must produce MethodCall ref"
    );
}

/// `$r->get('/users')->to('Users#list')->name('users_list')` — the
/// `->name()` call registers a symbolic handle. `url_for('users_list')`
/// and `redirect_to('users_list')` must resolve to it, the same way
/// they resolve to `'Users#list'`. Without `->name()`, calls like
/// `url_for('users_list')` sit unresolved.
#[test]
fn plugin_mojo_routes_name_registers_url_for_handle() {
    let src = r#"
package MyApp;
use Mojolicious::Lite;

my $r = app->routes;
$r->get('/users')->to('Users#list')->name('users_list');
"#;
    let fa = build_fa(src);

    let route_name_handler = fa
        .symbols()
        .iter()
        .find(|s| s.kind == SymKind::Handler && s.name == "users_list");
    assert!(
        route_name_handler.is_some(),
        "->name('users_list') must emit a Handler; handlers: {:?}",
        fa.symbols()
            .iter()
            .filter(|s| s.kind == SymKind::Handler)
            .map(|s| &s.name)
            .collect::<Vec<_>>()
    );

    let sym = route_name_handler.unwrap();
    if let SymbolDetail::Handler { dispatchers, .. } = &sym.detail {
        assert!(
            dispatchers.iter().any(|d| d == "url_for"),
            "named route must dispatch via url_for"
        );
        assert!(
            dispatchers.iter().any(|d| d == "redirect_to"),
            "named route must dispatch via redirect_to"
        );
    } else {
        panic!("route-name symbol should be Handler; got {:?}", sym.detail);
    }

    // The route name should be in a mojo-routes namespace bridged
    // to the declaring package, so cross-file `url_for('users_list')`
    // from other files in the workspace resolves.
    let ns = fa
        .plugin.namespaces
        .iter()
        .find(|n| n.plugin_id == "mojo-routes" && n.entities.contains(&sym.id));
    assert!(
        ns.is_some(),
        "named route must belong to a mojo-routes namespace"
    );
}

/// `->to('X#y')` routes dispatch via both `url_for` and `redirect_to`
/// (Phase-2 follow-up — `redirect_to` used to be Lite-only). Matches
/// Mojolicious's actual API where redirect_to on a controller resolves
/// named routes identically to url_for.
#[test]
fn plugin_mojo_routes_to_dispatches_via_redirect_to_too() {
    let src = r#"
package MyApp;
use Mojolicious::Lite;

my $r = app->routes;
$r->get('/users')->to('Users#list');
"#;
    let fa = build_fa(src);

    let route_handler = fa
        .symbols()
        .iter()
        .find(|s| s.kind == SymKind::Handler && s.name == "Users#list")
        .expect("Users#list Handler");

    if let SymbolDetail::Handler { dispatchers, .. } = &route_handler.detail {
        assert!(
            dispatchers.iter().any(|d| d == "url_for"),
            "->to route must dispatch via url_for"
        );
        assert!(
            dispatchers.iter().any(|d| d == "redirect_to"),
            "->to route must dispatch via redirect_to"
        );
    } else {
        panic!("route symbol should be Handler");
    }
}

// ==== AliasTo: DSL verbs delegate to real methods, not imaginary ones. ====
//
// `Mojolicious::Lite` monkey-patches `get`, `post`, `helper`, `app`, …
// into the caller at import time. At the Perl level each verb is just
// a thin pass-through to a real method:
//
//     sub { $routes->get(@_) }                  # get, post, put, any,
//                                               # options, patch, websocket
//     sub { $routes->delete(@_) }               # del
//     sub { $app->helper(@_) }                  # helper, hook, plugin
//     sub { $app }                              # app (returns the app)
//
// Previously the plugin fabricated a `SymbolDetail::Sub` per verb with a
// hand-written one-line `doc:` and, for `app`, a typed return. That's
// the "imaginary methods" the user pointed at: hover shows stub text,
// gd lands on the use statement, signature help has no params, and —
// worst — the synthesized Sub shadows chain resolution on real Mojo
// objects (`$routes->get('/x')->to(...)` loses the `to` intelligence).
//
// The fix: emit an *alias* that points at the real cross-file method.
// Hover, gd, sig help, and return-type inference all dereference the
// alias and use the real method's data. The tests below pin the
// expected behavior on real cross-file methods — they fail until
// `FunctionAlias` / `alias_to` lands in the data model and the
// resolution paths dereference it.

/// The user-visible "stomping" case: `$routes->get('/x')->to('X#y')` on
/// a real `Mojolicious::Routes` should chain through
/// `Mojolicious::Routes::Route::get` (fluent, returns its own class)
/// so `->to` resolves on `Mojolicious::Routes::Route`. The plugin-
/// synthesized top-level `get` Sub in the Lite script must NOT
/// intercept a method call on a real object of a different class.
#[test]
fn mojo_lite_chain_off_real_routes_preserves_real_method_chain() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    let app_src = r#"
package main;
use Mojolicious::Lite;
use Mojolicious;

my $routes = Mojolicious::Routes->new;
$routes->get('/users')->to('Users#list');
"#;
    // Stub Mojolicious::Routes::Route with a fluent `get` (returns
    // its own class) so chain resolution can carry the type forward.
    let route_pm_src = r#"
package Mojolicious::Routes::Route;
use Mojo::Base -base;

sub get {
    my $self = shift;
    return $self;
}

sub to {
    my $self = shift;
    return $self;
}
1;
"#;
    // Mojolicious::Routes ISA Mojolicious::Routes::Route in real
    // Mojo, so methods invoked on $routes (typed Mojolicious::Routes)
    // flow up to the parent class via the normal inheritance walk.
    let routes_pm_src = r#"
package Mojolicious::Routes;
use Mojo::Base 'Mojolicious::Routes::Route';
1;
"#;

    let app_fa = build_fa(app_src);
    let route_fa = build_fa(route_pm_src);
    let routes_fa = build_fa(routes_pm_src);

    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Mojolicious/Routes/Route.pm"),
        Arc::new(route_fa),
    );
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Mojolicious/Routes.pm"),
        Arc::new(routes_fa),
    );

    // `$routes->get` must resolve to the real Mojolicious::Routes::Route::get
    // via inheritance (Mojolicious::Routes → Mojolicious::Routes::Route) —
    // NOT to the plugin's top-level `get` Sub emitted by mojo-lite.
    // `class_name()` unifies ClassName and FirstParam — both are
    // usable for downstream chain resolution.
    let get_rt = app_fa.find_method_return_type("Mojolicious::Routes", "get", Some(&idx), None);
    assert_eq!(
        get_rt.as_ref().and_then(|t| t.class_name()),
        Some("Mojolicious::Routes::Route"),
        "`$$routes->get` must chain through the REAL Mojolicious::Routes::Route::get — \
             not the plugin's imaginary top-level `get` Sub. got: {:?}",
        get_rt,
    );

    // Second hop: `->to(...)` on the Route object returned by `get`.
    // User's wording: "get should return a to which is intelligent".
    // Fluent Route — `to` stays on Mojolicious::Routes::Route so the
    // chain can keep going (`->name(...)`, `->via(...)`, ...).
    let to_rt =
        app_fa.find_method_return_type("Mojolicious::Routes::Route", "to", Some(&idx), None);
    assert_eq!(
        to_rt.as_ref().and_then(|t| t.class_name()),
        Some("Mojolicious::Routes::Route"),
        "`->get('/x')->to(...)` must stay intelligent — Route::to is fluent, and \
             further hops depend on it. got: {:?}",
        to_rt,
    );
}

/// Hover on the DSL verb `get` in `get '/x' => sub {}` must surface
/// the real `Mojolicious::Routes::Route::get` POD, not the plugin's
/// hand-written one-liner. Pins that the plugin's Sub symbol for
/// `get` is an alias — its hover dereferences to the real method.
#[test]
fn mojo_lite_dsl_verb_hover_uses_real_method_doc() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    let app_src = r#"
package main;
use Mojolicious::Lite;

get '/users' => sub { my $c = shift; };
"#;
    // Real method carries a recognizable POD line. The test matches
    // on a substring that no hand-written plugin doc uses.
    let route_pm_src = r#"
package Mojolicious::Routes::Route;

=head2 get

  my $route = $r->get('/:foo' => sub ($c) {...});

Generate route matching only GET requests. Shortcut for
L<Mojolicious::Routes::Route/"any">.

=cut

sub get { my $self = shift; return $self; }
1;
"#;

    let app_fa = build_fa(app_src);
    let route_fa = build_fa(route_pm_src);

    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Mojolicious/Routes/Route.pm"),
        Arc::new(route_fa),
    );

    // Cursor on the `get` bareword at the call site.
    let (row, line) = app_src
        .lines()
        .enumerate()
        .find(|(_, l)| l.starts_with("get "))
        .expect("`get` call line");
    let col = line.find("get").unwrap() + 1;
    let point = tree_sitter::Point { row, column: col };

    let hover = app_fa
        .hover_info(point, app_src, Some(&idx))
        .expect("hover on DSL verb `get` returns text");

    assert!(
        hover.contains("Generate route matching only GET requests"),
        "hover on `get` must surface the real Mojolicious::Routes::Route::get POD \
             (verb is an alias, not an imaginary stub). got: {:?}",
        hover,
    );
}

/// `app` parses as a bareword invocant (`app->routes`). The plugin's
/// typed `app` Sub must make that bareword resolve to Mojolicious so
/// the chain can flow. Pins the bareword edge case explicitly — the
/// regression shape the user called out.
#[test]
fn mojo_lite_app_bareword_invocant_types_as_mojolicious() {
    let src = r#"
package main;
use Mojolicious::Lite;

my $x = app;
"#;
    let fa = build_fa(src);

    // `$x = app` — $x should pick up the return type of the plugin's
    // `app` Sub (ClassName("Mojolicious")).
    let ty = fa
        .inferred_type("$x", tree_sitter::Point::new(4, 0))
        .expect("$x must carry a type sourced from `app`'s return type");
    assert!(
        matches!(ty, InferredType::ClassName(c) if c == "Mojolicious"),
        "`$$x = app` must type as Mojolicious — bareword `app` resolves to the \
             plugin's typed Sub. got: {:?}",
        ty,
    );
}

/// The headline case: the full `app->routes->get('/x')->to('X#y')`
/// chain must be fully intelligent at every hop. One plugin stub
/// at the head (`app` → Mojolicious) — everything else is real
/// cross-file method resolution.
///
/// Every arrow is a separate assertion so a regression at any hop
/// points at the specific broken link:
///
///   app                                          → Mojolicious              (plugin-typed Sub)
///   Mojolicious::routes                          → Mojolicious::Routes       (real Mojo::Base accessor)
///   Mojolicious::Routes::get  (via parent Route) → Mojolicious::Routes::Route (fluent)
///   Mojolicious::Routes::Route::to               → Mojolicious::Routes::Route (fluent)
///
/// If any hop returns None the chain's "intelligence" collapses —
/// completion, hover, gd, sig-help all lose context from that point
/// forward. That collapse is the "hardcoded list" symptom the user
/// reported, flipped around.
#[test]
fn mojo_lite_app_routes_chain_is_fully_intelligent_to_the_end() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    let app_src = r#"
package main;
use Mojolicious::Lite;

app->routes->get('/users')->to('Users#list');
"#;
    let mojolicious_pm_src = r#"
package Mojolicious;
use Mojo::Base -base;

has routes => sub { Mojolicious::Routes->new };
1;
"#;
    let routes_pm_src = r#"
package Mojolicious::Routes;
use Mojo::Base 'Mojolicious::Routes::Route';
1;
"#;
    let route_pm_src = r#"
package Mojolicious::Routes::Route;
use Mojo::Base -base;

sub get { my $self = shift; return $self; }
sub to  { my $self = shift; return $self; }
1;
"#;

    let app_fa = build_fa(app_src);
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Mojolicious.pm"),
        Arc::new(build_fa(mojolicious_pm_src)),
    );
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Mojolicious/Routes.pm"),
        Arc::new(build_fa(routes_pm_src)),
    );
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Mojolicious/Routes/Route.pm"),
        Arc::new(build_fa(route_pm_src)),
    );

    // Hop 1: `app` → Mojolicious. The plugin's typed Sub seeds the
    // chain. This is the single sanctioned plugin stub.
    let app_sym = app_fa
        .symbols()
        .iter()
        .find(|s| {
            s.name == "app"
                && matches!(&s.namespace, Namespace::Framework { id } if id == "mojo-lite")
        })
        .expect("mojo-lite plugin must synthesize `app`");
    let rt = app_fa
        .symbol_return_type_via_bag(app_sym.id, None)
        .expect("hop 1: `app` must carry a typed return");
    assert_eq!(
        rt.class_name(),
        Some("Mojolicious"),
        "hop 1: `app` must type as Mojolicious — the one plugin stub the chain leans on"
    );

    // Hop 2: Mojolicious::routes → Mojolicious::Routes. Real
    // cross-file Mojo::Base accessor; the anon-sub default's
    // `Mojolicious::Routes->new` is lifted as the return type.
    let routes_rt = app_fa.find_method_return_type("Mojolicious", "routes", Some(&idx), None);
    assert_eq!(
        routes_rt.as_ref().and_then(|t| t.class_name()),
        Some("Mojolicious::Routes"),
        "hop 2: `Mojolicious::routes` must resolve cross-file to the real Mojo::Base \
             accessor and return Mojolicious::Routes. got: {:?}",
        routes_rt,
    );

    // Hop 3: Mojolicious::Routes::get → Mojolicious::Routes::Route.
    // Resolves via inheritance (Routes ISA Route) to the real fluent
    // method on the parent class. This is where plugin-synthesized
    // `get` from mojo-lite MUST NOT stomp.
    let get_rt = app_fa.find_method_return_type("Mojolicious::Routes", "get", Some(&idx), None);
    assert_eq!(
        get_rt.as_ref().and_then(|t| t.class_name()),
        Some("Mojolicious::Routes::Route"),
        "hop 3: `$$routes->get` must chain through the REAL \
             Mojolicious::Routes::Route::get (fluent) — not the plugin's \
             imaginary top-level `get` Sub. got: {:?}",
        get_rt,
    );

    // Hop 4: Mojolicious::Routes::Route::to → Mojolicious::Routes::Route.
    // Fluent. After this, `->name(...)`/`->via(...)`/etc. must still
    // resolve on Route — i.e. the chain keeps going, not collapses.
    let to_rt =
        app_fa.find_method_return_type("Mojolicious::Routes::Route", "to", Some(&idx), None);
    assert_eq!(
        to_rt.as_ref().and_then(|t| t.class_name()),
        Some("Mojolicious::Routes::Route"),
        "hop 4: `->to(...)` must chain through the real fluent `to` on \
             Mojolicious::Routes::Route — preserving intelligence for further \
             hops (->name, ->via, ...). got: {:?}",
        to_rt,
    );
}

/// Adversarial: a dotted helper `users.create` and a route whose
/// action is `Users#create` both end up with a Perl-level symbol
/// named `create`. They are UNRELATED:
///
///   * `users.create` lives on `Mojolicious::Controller::_Helper::users`
///     — a synthetic proxy class invented by the plugin. It's called
///     as `$c->users->create(...)`.
///   * `Users#create` points at a method `create` on the user's
///     `Users` controller class. It's called via dispatch, not
///     chained off a helper.
///
/// Name-based resolution would cross-link them (goto-def on either
/// jumps to the other, find-references unions the two unrelated
/// call sites). Class-aware resolution must keep them apart: the
/// route's MethodCallRef targets class `Users`, the helper's leaf
/// lives on `_Helper::users`.
#[test]
fn helper_and_route_with_same_leaf_name_do_not_cross_link() {
    let src = r#"
package MyApp;
use Mojolicious::Lite;

$app->helper('users.create', sub ($c, $user) {});
$app->routes->post('/users')->to(controller => 'Users', action => 'create');
"#;
    let fa = build_fa(src);

    // --- Fact-finding: what actually got emitted? ---

    // The helper leaf `create` should live on the proxy class.
    let helper_create: Vec<&Symbol> = fa
        .symbols()
        .iter()
        .filter(|s| {
            s.name == "create"
                && matches!(&s.namespace, Namespace::Framework { id } if id == "mojo-helpers")
        })
        .collect();
    assert_eq!(helper_create.len(), 1, "one helper-leaf named 'create'");
    let helper_create = helper_create[0];
    assert_eq!(
        helper_create.package.as_deref(),
        Some("Mojolicious::Controller::_Helper::users"),
        "helper leaf lives on the proxy class, NOT on Users"
    );

    // The route emits a MethodCallRef method_name=create invocant=Users.
    let route_ref = fa
        .refs()
        .iter()
        .find(|r| {
            matches!(&r.kind, RefKind::MethodCall { invocant, .. } if invocant.text() == "Users")
                && r.target_name == "create"
        })
        .expect("route should emit MethodCall create@Users");

    // --- The bug: does the route's ref resolve to the helper? ---

    // If resolves_to is Some(sym_id), it MUST NOT point to the
    // helper — the helper lives on a different class.
    if let Some(target_sid) = route_ref.resolved_symbol() {
        assert_ne!(
            target_sid, helper_create.id,
            "route MethodCall(create @ Users) must NOT resolve to the \
                 helper-leaf on _Helper::users — they share a name only"
        );
    }

    // Cross-resolution via the public API: refs_to_symbol(helper)
    // must NOT include the route's ref.
    let refs_to_helper = fa.refs_to(helper_create.id);
    for r in &refs_to_helper {
        assert_ne!(
            (r.span.start.row, r.span.start.column),
            (route_ref.span.start.row, route_ref.span.start.column),
            "route ref showed up as a reference to the helper — cross-link bug. \
                 Helper is on _Helper::users, route targets Users, they shouldn't mix."
        );
    }

    // And the mirror: resolve_method_in_ancestors on class `Users`
    // for method `create` must NOT return the helper-leaf. The
    // helper's class is _Helper::users, not Users.
    let resolution = fa.resolve_method_in_ancestors("Users", "create", None);
    if let Some(crate::model::file_analysis::MethodResolution::Local { sym_id, .. }) = resolution {
        assert_ne!(
            sym_id, helper_create.id,
            "resolve_method_in_ancestors(Users, create) returned the helper — \
                 class-awareness broken"
        );
    }
}

/// Helpers emitted by mojo-helpers land on Mojolicious::Controller.
/// A controller subclass in ANOTHER file (standard workspace layout)
/// must see them when walking methods — the class_content_index
/// bridges the lookup because the synthesizing module's primary
/// package isn't Mojolicious::Controller.
#[test]
fn plugin_mojo_helpers_reachable_cross_file_from_controller() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    // Lite script with a helper.
    let lite_src = r#"
package MyApp;
use Mojolicious::Lite;

my $app = Mojolicious->new;
$app->helper(greet => sub { my ($c, $who) = @_; });
"#;
    // Controller subclass in another file.
    let ctrl_src = r#"
package MyApp::Controller::Home;
use parent 'Mojolicious::Controller';
1;
"#;

    let lite_fa = Arc::new(build_fa(lite_src));
    let ctrl_fa = build_fa(ctrl_src);

    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(std::path::PathBuf::from("/tmp/MyApp.pm"), lite_fa.clone());

    // bridges_index knows MyApp.pm declares a namespace bridged to the
    // app surface (mojo-helpers' app namespace).
    let mods = idx.modules_bridging_to(crate::model::file_analysis::APP_SURFACE_CLASS);
    assert!(
        mods.iter().any(|m| m == "MyApp"),
        "MyApp module should be listed as bridged to the app surface; got: {:?}",
        mods
    );

    // Completion on MyApp::Controller::Home inheriting from
    // Mojolicious::Controller should walk up to the controller, cross
    // the synthetic-parent edge to the app surface, and find `greet`.
    let candidates = ctrl_fa.complete_methods_for_class("MyApp::Controller::Home", Some(&idx));
    let labels: Vec<&str> = candidates.iter().map(|c| c.label.as_str()).collect();
    assert!(
        labels.contains(&"greet"),
        "helper `greet` on the app surface in MyApp.pm should complete on \
             controller subclasses via the synthetic-parent edge; got: {:?}",
        labels
    );
}

/// mojo-helpers cross-file: when a Lite script registers a helper
/// `greet`, the resulting Method symbol's `package` is the fictional
/// app surface. Any consumer file — controller subclass, the app, or
/// otherwise — finds it via the standard workspace walk + the
/// synthetic-parent edge, without a single mojo-helpers-aware line in
/// the consumer-side code path.
#[test]
fn plugin_mojo_helpers_land_on_controller_package() {
    let src = r#"
package MyApp::Lite;
use Mojolicious::Lite;

app->helper(greet => sub {
    my ($c, $name) = @_;
    return "hello, $name";
});
1;
"#;
    let fa = build_fa(src);
    let greet = fa
        .symbols()
        .iter()
        .find(|s| s.name == "greet" && s.kind == SymKind::Method)
        .expect("helper must emit a Method named greet");
    assert_eq!(
        greet.package.as_deref(),
        Some(crate::model::file_analysis::APP_SURFACE_CLASS),
        "helper Method is packaged on the app surface; the synthetic-parent \
             edge lets every consumer class pick it up via the inheritance walk"
    );
    assert!(matches!(&greet.namespace, Namespace::Framework { id } if id == "mojo-helpers"));
}

/// Synthetic-ancestor app surface (docs/adr/plugin-system.md): a helper
/// that returns a concrete class resolves its RETURN type identically
/// from the app, the controller, AND a user-written app subclass —
/// proving the single bridge target + synthetic-parent edge composes with
/// the PackageSymbol type-resolution walk and that subclasses inherit the
/// surface for free.
#[test]
fn plugin_mojo_helpers_return_type_via_app_surface() {
    let src = r#"
package MyApp;
use Mojolicious::Lite;

my $app = Mojolicious->new;
$app->helper(model => sub { my ($c) = @_; return MyApp::Model->new; });
"#;
    // Declare a user app subclass in the SAME file so its
    // `package_parents` edge (MyApp::Web -> Mojolicious) is present;
    // it must inherit the surface for free.
    let src = format!("{src}\npackage MyApp::Web;\nuse parent 'Mojolicious';\n1;\n");
    let fa = build_fa(&src);

    // The helper resolves its return type from the app class, the
    // controller class, AND the user app subclass.
    for class in ["Mojolicious", "Mojolicious::Controller", "MyApp::Web"] {
        let rt = fa.find_method_return_type(class, "model", None, None);
        assert_eq!(
            rt,
            Some(crate::model::file_analysis::InferredType::ClassName("MyApp::Model".into())),
            "`{class}->model` must return MyApp::Model via the app surface; got {rt:?}",
        );
    }
}

/// App surface (docs/adr/plugin-system.md): `$app->minion->enqueue`
/// resolves once `$app` is typed. The `minion` helper (return type a
/// Minion subclass) is reached from the locally-typed `$app` via the app
/// surface; enrichment then resolves the `->enqueue` receiver and promotes
/// the dispatch. Proves the surface composes with dispatch-verb promotion.
#[test]
fn plugin_app_surface_minion_enqueue_resolves_when_app_typed() {
    use crate::model::file_analysis::HandlerOwner;
    use std::path::PathBuf;
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.register_workspace_module(
        PathBuf::from("/tmp/as_acme_minion.pm"),
        std::sync::Arc::new(build_fa("package Acme::Minion;\nuse Mojo::Base 'Minion';\n1;\n")),
    );

    // `$app` is locally typed (Mojolicious->new); the `minion` helper
    // returns an Acme::Minion. `$app->minion` reaches the helper via the
    // synthetic app-surface edge, so its return type is Acme::Minion.
    let mut fa = build_fa(
        "package MyApp;\nuse Mojolicious::Lite;\n\
         my $app = Mojolicious->new;\n\
         $app->helper(minion => sub { my ($c) = @_; return Acme::Minion->new; });\n\
         $app->minion->enqueue('send_email' => ['alice']);\n1;\n",
    );

    let mref = fa.refs().iter().find(|r| {
        matches!(&r.kind, RefKind::MethodCall { .. }) && r.target_name == "minion"
    });
    assert!(mref.is_some(), "an `$app->minion` MethodCall ref must exist");

    fa.enrich_imported_types_with_keys(Some(&idx));

    // `Mojolicious::Lite` is a trigger, so the emit-hook materializes the
    // DispatchCall directly; `applicable_dispatches` de-dups the gated
    // candidate against it. Either path surfaces the handler — exactly once.
    let has_materialized = fa.refs().iter().any(|r|
        matches!(&r.kind, RefKind::DispatchCall { dispatcher } if dispatcher == "enqueue")
            && matches!(r.handler_owner(), Some(HandlerOwner::Class(c)) if c == "Minion")
            && r.target_name == "send_email");
    let has_gated = fa.applicable_dispatches(Some(&idx)).iter().any(|a|
        a.name == "send_email" && a.owner == HandlerOwner::Class("Minion".into()));
    assert!(
        has_materialized ^ has_gated,
        "`$app->minion->enqueue` must surface as a Minion dispatch exactly once — \
         via the emit-hook ref OR the gated candidate; materialized={has_materialized} \
         gated={has_gated}",
    );
}

/// Helpers complete on both `$c` (Controller) and `$app` (the
/// Mojolicious app class). Every helper registers a Method on each
/// entry class, so `complete_methods_for_class` for either class
/// surfaces the helper. Dotted chain roots also land on both
/// classes; the deeper proxies stay on the shared prefix.
#[test]
fn plugin_mojo_helpers_complete_on_app_class_too() {
    let src = r#"
package MyApp;
use Mojolicious::Lite;

my $app = Mojolicious->new;
$app->helper(current_user => sub { my ($c) = @_; });
$app->helper('users.create' => sub { my ($c, $name) = @_; });
"#;
    let fa = build_fa(src);

    for class in ["Mojolicious::Controller", "Mojolicious"] {
        let candidates = fa.complete_methods_for_class(class, None);
        let labels: Vec<&str> = candidates.iter().map(|c| c.label.as_str()).collect();
        assert!(
            labels.contains(&"current_user"),
            "`current_user` must complete on {}; got: {:?}",
            class,
            labels,
        );
        assert!(
            labels.contains(&"users"),
            "`users` (dotted-helper root) must complete on {}; got: {:?}",
            class,
            labels,
        );
    }
}

/// Diagnostic pin: inside a controller action, `$c->url_for('|')`
/// must offer every named route declared in the workspace —
/// Lite paths, `Ctrl#action` pairs from `->to(...)`, and symbolic
/// `->name('foo')` handles. This is the completion side that the
/// `_emits_refs` / `_registers_url_for_handle` tests don't cover.
///
/// Discovers two separate bugs at the same time:
///   (1) Handler.owner is the *declaring* package (`MyApp`), so
///       `$c->url_for(...)` on a `Users` controller fails the
///       `owner_class == invocant_class` filter in
///       `dispatch_target_completions`.
///   (2) No coverage for "does url_for completion work at all".
#[test]
fn plugin_mojo_routes_url_for_completion_offers_route_names() {
    use tower_lsp::lsp_types::Position;
    use tree_sitter::Parser;

    let app_src = r#"package MyApp;
use Mojolicious::Lite;

my $r = app->routes;
$r->get('/users')->to('Users#list')->name('users_list');
$r->post('/users')->to(controller => 'Users', action => 'create');

get '/hello' => sub { my ($c) = @_; };
"#;
    let app_fa = std::sync::Arc::new(build_fa(app_src));

    let ctrl_src = r#"package Users;
use parent 'Mojolicious::Controller';

sub list {
    my ($c) = @_;
    my $u = $c->url_for('x');
}
"#;
    let ctrl_fa = build_fa(ctrl_src);

    let idx = std::sync::Arc::new(crate::index::module_index::ModuleIndex::new_for_test());
    idx.register_workspace_module(std::path::PathBuf::from("/tmp/app.pl"), app_fa);
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Users.pm"),
        std::sync::Arc::new(build_fa(ctrl_src)),
    );

    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(ctrl_src, None).unwrap();

    // Cursor on the `x` inside `url_for('x')` — `active_param == 0`.
    let pos = Position {
        line: 5,
        character: 25,
    };
    let items = crate::lsp::symbols::completion_items_for_test(&ctrl_fa, &tree, ctrl_src, pos, &idx, None);
    let labels: Vec<String> = items.iter().map(|it| it.label.clone()).collect();

    for expected in &["users_list", "Users#list", "/hello"] {
        assert!(
            labels.iter().any(|l| l == expected),
            "url_for('|') inside Users::list must offer `{}` (route declared in MyApp); got: {:?}",
            expected,
            labels
        );
    }
}

/// Red pin (user-reported): starting to type inside
/// `$c->url_for('|')` must not kill completion — the string
/// content should feed the prefix filter, not suppress it.
/// Covers two realistic live-editing shapes:
///
/// 1. `url_for('|')` — cursor between the quotes, string body
///    empty. Every route should appear (no prefix yet).
/// 2. `url_for('adm|')` — user has typed `adm`, cursor inside
///    the string, closing quote already in place (what you get
///    after auto-paired quotes). The returned list must be
///    prefix-filterable by `adm` via either `filter_text` or a
///    server-side restriction — `admin.users.purge`-style
///    named routes should survive the filter, Lite `/hello`
///    should drop out client-side.
///
/// Existing work that this pin must use, NOT re-roll:
///   * `candidate_to_completion_item` already sets
///     `filter_text = Some(label)` so the quoted `insert_text`
///     doesn't defeat client-side matching — covered by
///     `completion_dispatch_filter_text_matches_bare_name`.
///   * `mid_string_methodref_completions` handles the same
///     shape for MethodCallRefs (`->to('Users#li|')`), slicing
///     `source[span_start..cursor]` as the prefix.
#[test]
fn plugin_mojo_routes_url_for_completion_survives_typed_prefix() {
    use tower_lsp::lsp_types::Position;
    use tree_sitter::Parser;

    let app_src = r#"package MyApp;
use Mojolicious::Lite;

my $r = app->routes;
$r->get('/users')->to('Users#list')->name('users_list');
$r->get('/admin/users/purge')->to('Admin#purge')->name('admin_users_purge');

get '/hello' => sub { my ($c) = @_; };
"#;
    let app_fa = std::sync::Arc::new(build_fa(app_src));

    let idx = std::sync::Arc::new(crate::index::module_index::ModuleIndex::new_for_test());
    idx.register_workspace_module(std::path::PathBuf::from("/tmp/app.pl"), app_fa);

    // Case 1: empty string, cursor between the quotes.
    // `    my $u = $c->url_for('');`
    //                          ^ char 24 (opening quote)
    //                           ^ char 25 (cursor here)
    //                           ^ char 25 (closing quote)
    let empty_src = r#"package Users;
use parent 'Mojolicious::Controller';

sub list {
    my ($c) = @_;
    my $u = $c->url_for('');
}
"#;
    let empty_fa = build_fa(empty_src);
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Users_empty.pm"),
        std::sync::Arc::new(build_fa(empty_src)),
    );
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(empty_src, None).unwrap();
    let items = crate::lsp::symbols::completion_items_for_test(
        &empty_fa,
        &tree,
        empty_src,
        Position {
            line: 5,
            character: 25,
        },
        &idx,
        None,
    );
    let labels: Vec<String> = items.iter().map(|it| it.label.clone()).collect();
    for expected in &["users_list", "admin_users_purge", "Users#list", "/hello"] {
        assert!(
            labels.iter().any(|l| l == expected),
            "empty url_for('|') must offer `{}`; got: {:?}",
            expected,
            labels
        );
    }

    // Case 2: user has typed `adm`, closing quote in place.
    // `    my $u = $c->url_for('adm');`
    //                          ^ 24 opening quote
    //                           ^ 25 a
    //                            ^ 26 d
    //                             ^ 27 m — cursor here after typing
    //                              ^ 28 closing quote
    let typed_src = r#"package Users;
use parent 'Mojolicious::Controller';

sub list {
    my ($c) = @_;
    my $u = $c->url_for('adm');
}
"#;
    let typed_fa = build_fa(typed_src);
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Users_typed.pm"),
        std::sync::Arc::new(build_fa(typed_src)),
    );
    let tree = parser.parse(typed_src, None).unwrap();
    let items = crate::lsp::symbols::completion_items_for_test(
        &typed_fa,
        &tree,
        typed_src,
        Position {
            line: 5,
            character: 27,
        },
        &idx,
        None,
    );

    // Server returns the dispatch-handler set (all routes). The
    // client narrows by `filter_text` (bare label) against the
    // typed prefix `adm`. For this pin we assert the two things
    // the server owes us:
    //
    //  (a) the set still includes routes whose LABEL starts with
    //      `adm` — if the server dropped them before we got a
    //      chance to filter, completion is "dead" as the user
    //      described.
    //  (b) every returned handler's `filter_text` is set to the
    //      bare label so the client's prefix match keys on the
    //      route name, not on the quoted insert_text (`'admin...'`
    //      starts with `'`, not `a`).
    let labels: Vec<String> = items.iter().map(|it| it.label.clone()).collect();
    assert!(
        labels.iter().any(|l| l == "admin_users_purge"),
        "typed prefix `adm` must still surface `admin_users_purge` from the \
             server so client-side filter_text matching can narrow to it; got: {:?}",
        labels
    );

    let adm_item = items
        .iter()
        .find(|it| it.label == "admin_users_purge")
        .expect("admin_users_purge must be in returned items");
    assert_eq!(
        adm_item.filter_text.as_deref(),
        Some("admin_users_purge"),
        "dispatch handler `filter_text` must be the bare label so the \
             typed `adm` (no quote) matches — otherwise starting to type the \
             string kills completion",
    );
}

/// mojo-lite route URLs are referenced from `->url_for(...)` and
/// `->redirect_to(...)`. Both emit `DispatchCall` refs tight to
/// the URL string so gd/gr compose via the standard Handler
/// resolution path — no Lite-aware code in the core.
#[test]
fn plugin_mojo_lite_url_dispatch_emits_refs() {
    let src = r#"
package MyApp;
use Mojolicious::Lite;

get '/hello' => sub {
    my ($c) = @_;
    $c->render(text => 'hi');
};

sub after {
    my ($c) = @_;
    $c->redirect_to('/hello');
    my $u = $c->url_for('/hello');
}
"#;
    let fa = build_fa(src);

    let dispatch_refs: Vec<&crate::model::file_analysis::Ref> = fa
        .refs()
        .iter()
        .filter(|r| matches!(&r.kind, RefKind::DispatchCall { .. }))
        .filter(|r| r.target_name == "/hello")
        .collect();

    let dispatchers: Vec<&str> = dispatch_refs
        .iter()
        .map(|r| match &r.kind {
            RefKind::DispatchCall { dispatcher, .. } => dispatcher.as_str(),
            _ => unreachable!(),
        })
        .collect();

    assert!(
        dispatchers.contains(&"redirect_to"),
        "redirect_to('/hello') must emit a DispatchCall ref; got: {:?}",
        dispatchers,
    );
    assert!(
        dispatchers.contains(&"url_for"),
        "url_for('/hello') must emit a DispatchCall ref; got: {:?}",
        dispatchers,
    );
}

/// Plugin triggers must gate emission. A class that doesn't inherit from
/// Mojo::EventEmitter should see no mojo-events emissions even if it
/// happens to call a method named `->on(...)`.
#[test]
fn plugin_mojo_events_triggers_gate_emission() {
    let src = r#"
package My::Unrelated;

sub new {
    my $class = shift;
    my $self = bless {}, $class;
    $self->on('connect', sub { ... });
    $self;
}

1;
"#;
    let fa = build_fa(src);
    let plugin_syms: Vec<&Symbol> = fa
        .symbols()
        .iter()
        .filter(|s| {
            matches!(&s.namespace,
                Namespace::Framework { id } if id == "mojo-events")
        })
        .collect();
    assert!(
        plugin_syms.is_empty(),
        "untriggered package must not get plugin emissions; got: {:?}",
        plugin_syms.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

/// Minion plugin: `$minion->add_task(NAME, sub { ... })` emits a
/// Handler (owner: Minion) with the task's sub params, typed $job
/// in the callback body, and a DispatchCall ref on the name.
#[test]
fn plugin_minion_add_task_registers_handler() {
    let src = r#"
package MyApp;
use Minion;

my $minion = Minion->new;
$minion->add_task(send_email => sub {
    my ($job, $to, $subject) = @_;
    $job->finish;
});
"#;
    let fa = build_fa(src);

    let handler = fa
        .symbols()
        .iter()
        .find(|s| {
            s.kind == SymKind::Handler
                && s.name == "send_email"
                && matches!(&s.namespace, Namespace::Framework { id } if id == "minion")
        })
        .expect("add_task must emit a Handler named send_email");

    let SymbolDetail::Handler {
        ref owner,
        ref dispatchers,
        ref params,
    } = handler.detail
    else {
        panic!("handler detail should be Handler")
    };
    assert!(matches!(owner, HandlerOwner::Class(c) if c == "Minion"));
    assert!(dispatchers.iter().any(|d| d == "enqueue"));
    assert!(
        matches!(handler.presentation.display, Some(HandlerDisplay::Task)),
        "minion tasks render as HandlerDisplay::Task (LSP kind FUNCTION, outline word 'task')"
    );
    // Callback params: $job flagged as invocant, then the rest.
    let names: Vec<&str> = params.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["$job", "$to", "$subject"]);
    assert!(
        params[0].is_invocant,
        "Minion::Job is the callback's invocant"
    );

    // DispatchCall on the name (registration itself is a reference).
    let dc = fa.refs().iter()
            .find(|r| matches!(&r.kind, RefKind::DispatchCall { dispatcher, .. } if dispatcher == "add_task"))
            .expect("add_task must emit a DispatchCall ref");
    assert_eq!(dc.target_name, "send_email");
}

/// `$minion->enqueue(NAME, ...)` emits a DispatchCall for the name
/// so gd/gr compose against the add_task Handler.
#[test]
fn plugin_minion_enqueue_emits_dispatch_call() {
    let src = r#"
package MyApp;
use Minion;

my $minion = Minion->new;
$minion->add_task(send_email => sub { my ($job) = @_; });
$minion->enqueue(send_email => ['alice']);
$minion->enqueue_p(send_email => ['bob']);
"#;
    let fa = build_fa(src);

    let dispatchers: Vec<&str> = fa
        .refs()
        .iter()
        .filter_map(|r| match &r.kind {
            RefKind::DispatchCall { dispatcher, .. } if r.target_name == "send_email" => {
                Some(dispatcher.as_str())
            }
            _ => None,
        })
        .collect();
    assert!(
        dispatchers.contains(&"enqueue"),
        "enqueue('send_email', ...) must emit a DispatchCall; got: {:?}",
        dispatchers
    );
    assert!(
        dispatchers.contains(&"enqueue_p"),
        "enqueue_p must emit a DispatchCall too; got: {:?}",
        dispatchers
    );
}

/// Option B: a `$minion->enqueue('T')` lights up by the RECEIVER's type,
/// not the file's `use`s. `Worker` never `use`s Minion and isn't a Mojo
/// app — so the bundled minion plugin's triggers never fire, and there's no
/// `DispatchCall` after the plain build. But `$m` is a locally-constructed
/// `Acme::Minion` (isa Minion, declared cross-file), so the builder records
/// a gated candidate and `applicable_dispatches` — which has the module
/// index, hence the cross-file `isa` — resolves it at QUERY time, with no
/// enrichment. This is the "wherever the minion came from provides the magic"
/// path the file-trigger model couldn't reach.
#[test]
fn gated_dispatch_resolves_on_subclass_receiver_query_time() {
    use crate::model::file_analysis::HandlerOwner;
    use std::path::PathBuf;
    let base = build_fa("package Acme::Minion;\nuse Mojo::Base 'Minion';\n1;\n");
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.register_workspace_module(
        PathBuf::from("/tmp/b_acme_minion.pm"),
        std::sync::Arc::new(base),
    );

    let fa = build_fa(
        "package Worker;\nsub go {\n  my $m = Acme::Minion->new;\n  $m->enqueue('send_email' => ['a']);\n}\n1;\n",
    );
    // The triggers never fired, so nothing was materialized at parse time.
    assert!(
        !fa.refs().iter().any(|r| matches!(&r.kind, RefKind::DispatchCall { .. })),
        "no DispatchCall ref should exist (plugin trigger didn't fire)",
    );

    // No enrichment — query-time resolution alone surfaces the dispatch,
    // exactly as a non-open workspace file would be served.
    let applied = fa.applicable_dispatches(Some(&idx));
    assert_eq!(
        applied.iter().filter(|a|
            a.name == "send_email" && a.owner == HandlerOwner::Class("Minion".into())).count(),
        1,
        "query-time resolution must surface exactly one Minion dispatch for \
         enqueue on a Minion-subclass receiver, even with no enrichment; got {:?}",
        applied,
    );
}

/// The receiver isn't locally typed — it's a cross-file method-call return
/// (`$b->minion` where `Box::minion` returns an `Acme::Minion`). The
/// build-time hint is `None`; query-time resolution resolves the invocant
/// cross-file (via the module index) and, finding it isa Minion, surfaces the
/// dispatch. This is the `$self->_minion->enqueue(...)` shape — works whenever
/// the receiver's type is actually resolvable.
#[test]
fn gated_dispatch_resolves_cross_file_receiver_query_time() {
    use crate::model::file_analysis::HandlerOwner;
    use std::path::PathBuf;
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.register_workspace_module(
        PathBuf::from("/tmp/b_acme_minion.pm"),
        std::sync::Arc::new(build_fa("package Acme::Minion;\nuse Mojo::Base 'Minion';\n1;\n")),
    );
    idx.register_workspace_module(
        PathBuf::from("/tmp/b_box.pm"),
        std::sync::Arc::new(build_fa(
            "package Box;\nsub new { bless {}, shift }\nsub minion ($self) { return Acme::Minion->new; }\n1;\n",
        )),
    );

    let fa = build_fa(
        "package Worker;\nsub go {\n  my $b = Box->new;\n  $b->minion->enqueue('send_email' => ['a']);\n}\n1;\n",
    );

    // No enrichment: the gate resolves the cross-file receiver lazily.
    let applied = fa.applicable_dispatches(Some(&idx));
    assert!(
        applied.iter().any(|a|
            a.name == "send_email" && a.owner == HandlerOwner::Class("Minion".into())),
        "query-time resolution must resolve the cross-file receiver `$b->minion` \
         (Acme::Minion isa Minion) and surface the dispatch; got {:?}",
        applied,
    );
}

/// A Minion SUBCLASS receiver (`Acme::Minion` isa Minion, the crm
/// `Clove::Minion` shape) must still register + dispatch tasks. The
/// receiver types to `ClassName("Acme::Minion")`, which a name-prefix
/// allowlist (`== "Minion" || starts_with("Minion::")`) silently rejects —
/// the rule-#10 trap. The plugin no longer gates on receiver class, so the
/// Handler (owner Minion) and the enqueue DispatchCall pair as usual.
#[test]
fn plugin_minion_subclass_receiver_still_wires() {
    let src = r#"
package MyApp;
use Minion;

my $minion = Acme::Minion->new;
$minion->add_task(send_email => sub { my ($job) = @_; });
$minion->enqueue(send_email => ['alice']);
"#;
    let fa = build_fa(src);

    let handler = fa.symbols().iter().find(|s| {
        s.kind == SymKind::Handler
            && s.name == "send_email"
            && matches!(&s.detail, SymbolDetail::Handler { owner: HandlerOwner::Class(c), .. } if c == "Minion")
    });
    assert!(
        handler.is_some(),
        "add_task on a Minion subclass receiver must still register a Minion-owned Handler",
    );

    let has_enqueue_dc = fa.refs().iter().any(|r| matches!(
        &r.kind, RefKind::DispatchCall { dispatcher, .. }
        if dispatcher == "enqueue" && r.target_name == "send_email"
    ));
    assert!(
        has_enqueue_dc,
        "enqueue on a Minion subclass receiver must still emit a DispatchCall",
    );
}

/// $job inside an add_task callback is typed as Minion::Job so
/// completion on $job-> resolves to Minion::Job methods.
#[test]
fn plugin_minion_types_job_inside_task_body() {
    let src = r#"
package MyApp;
use Minion;

my $minion = Minion->new;
$minion->add_task(send_email => sub {
    my ($job) = @_;
    $job->finish;
});
"#;
    let fa = build_fa(src);

    // `$job` should be typed Minion::Job inside the callback —
    // plugin-declared ClassName, not builder's FirstParam.
    let ty = fa
        .inferred_type("$job", tree_sitter::Point::new(8, 0))
        .expect("$job must carry a type inside add_task callback");
    assert!(
        matches!(ty, InferredType::ClassName(c) if c == "Minion::Job"),
        "type should be plugin-declared ClassName(Minion::Job), got {:?}",
        ty,
    );
}

/// Minion's `enqueue` options go in a hashref at position 3
/// (`enqueue(task, [args], {priority => 10})`). The plugin emits
/// HashKeyDefs for the common keys owned by Sub{Minion,enqueue}
/// — what's missing is cursor-context routing for "hash literal
/// as positional arg" → `HashKey { source_sub: "enqueue" }`.
/// Skipped until the core learns that shape; the emission side is
/// pinned here so regressing it trips.
#[test]
fn plugin_minion_enqueue_options_hashkeys_emitted() {
    let src = r#"
package MyApp;
use Minion;

my $minion = Minion->new;
$minion->enqueue(task_x => ['arg'] => { priority => 10 });
"#;
    let fa = build_fa(src);

    // Options emitted as HashKeyDef symbols owned by Sub{Minion, enqueue}.
    let option_names: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| {
            s.kind == SymKind::HashKeyDef
                && matches!(&s.namespace, Namespace::Framework { id } if id == "minion")
        })
        .map(|s| s.name.as_str())
        .collect();
    for expected in &[
        "priority", "queue", "delay", "attempts", "notes", "parents", "expire", "lax",
    ] {
        assert!(
            option_names.contains(expected),
            "enqueue option `{}` must be emitted; got: {:?}",
            expected,
            option_names
        );
    }
}
