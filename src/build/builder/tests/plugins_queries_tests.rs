use crate::model::file_analysis::ReceiverPolicy;
use super::*;

/// Cross-file helper chain completion: Users.pm inherits from
/// Mojolicious::Controller; helpers declared in a sibling Lite
/// file register Methods on Controller. From Users.pm, cursor at
/// `$c->`, `$c->users->`, `$c->admin->` must all resolve through
/// the proxy classes even though the methods live in another
/// file and the CPAN-cached Controller doesn't know about them.
///
/// Regression trigger: `resolve_method_in_ancestors` used to scan
/// only `get_cached(class)` cross-file, missing plugin-emitted
/// methods that live in other modules under the same `package`.
/// `detect_cursor_context_tree` also only called `resolve_expression_type`
/// without a module_index, so chain resolution of `$c->users->`
/// fell through to the untyped fallback and returned Users's own
/// methods (list, create) instead of the proxy chain's leaves.
#[test]
fn plugin_mojo_helpers_cross_file_chain_completion() {
    use tower_lsp::lsp_types::Position;
    use tree_sitter::Parser;

    // The Lite file — declares the helpers.
    let lite_src = r#"package MyApp;
use strict;
use warnings;
use Mojolicious::Lite;

my $app = Mojolicious->new;

$app->helper(current_user => sub { my ($c, $fallback) = @_; });
$app->helper('users.create' => sub { my ($c, $name, $email) = @_; });
$app->helper('users.delete' => sub { my ($c, $id) = @_; });
$app->helper('admin.users.purge' => sub { my ($c, $force) = @_; });
"#;
    let lite_fa = build_fa(lite_src);

    // The controller file — inherits from Mojolicious::Controller
    // and expects to reach the helpers cross-file. This is where
    // the user's `$c->` completion is happening in real life.
    let src = r#"package Users;
use strict;
use warnings;
use parent 'Mojolicious::Controller';

sub list {
    my ($c) = @_;
    $c->;
    $c->users->;
    $c->admin->;
}
"#;
    let fa = build_fa(src);

    // Sanity — Users.pm's own analysis has `list` but not the
    // helpers (they're declared in the Lite file).
    let users_subs: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, SymKind::Method | SymKind::Sub))
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(users_subs, vec!["list"], "Users.pm owns only `list`");

    // Now simulate the nvim completion pipeline at `$c->` position.
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();

    // Populate a ModuleIndex with a mock Mojolicious::Controller
    // that has a few native-looking methods (render, stash, etc.).
    // Matches the user's env where CPAN Mojolicious is installed
    // and its Controller is cached cross-file. Register the Lite
    // script itself too — workspace indexer would.
    // Workspace has BOTH files registered — mirrors nvim startup
    // after Rayon indexes the .pm/.pl set.
    let idx = std::sync::Arc::new(crate::index::module_index::ModuleIndex::new_for_test());
    let lite_fa = std::sync::Arc::new(lite_fa);
    idx.register_workspace_module(std::path::PathBuf::from("/tmp/MyApp.pm"), lite_fa.clone());
    let users_fa = std::sync::Arc::new(build_fa(src));
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/lib/Users.pm"),
        users_fa.clone(),
    );

    let ctrl_src = r#"package Mojolicious::Controller;
sub render { my ($self, %args) = @_; }
sub stash { my ($self, $key) = @_; }
sub req { my ($self) = @_; }
sub res { my ($self) = @_; }
sub session { my ($self, $key) = @_; }
1;
"#;
    let ctrl_fa = std::sync::Arc::new(build_fa(ctrl_src));
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Mojolicious/Controller.pm"),
        ctrl_fa,
    );

    // The workspace knows the Lite file declares a namespace bridged
    // to the app surface (the mojo-helpers app namespace emits
    // `Bridge::Class(APP_SURFACE_CLASS)`). The controller reaches it
    // through the synthetic-parent edge in the ancestor walk.
    let mods = idx.modules_bridging_to(crate::model::file_analysis::APP_SURFACE_CLASS);
    assert!(
        mods.iter().any(|m| m == "MyApp"),
        "workspace index must list MyApp.pm bridged to the app surface; got: {:?}",
        mods
    );

    // Part 1: `$c->` completion in Users.pm surfaces both the
    // inherited native methods AND the plugin-emitted helpers
    // (cross-file, via the app namespace's Class(Controller) bridge).
    let pos = |row: u32, col: u32| Position {
        line: row,
        character: col,
    };
    // The language root's members (UNIVERSAL's `can`, …) end every class's
    // list; these assertions are about the classes' own.
    let roots = crate::model::roots::installed();
    let root_member = |l: &str| roots.surface("perl").is_some_and(|s| s.members.iter().any(|m| m.name == l));
    let call_label_set = |items: &[tower_lsp::lsp_types::CompletionItem]| -> Vec<String> {
        items.iter().map(|it| it.label.clone()).filter(|l| !root_member(l)).collect()
    };

    let items = crate::lsp::symbols::completion_items_for_test(&fa, &tree, src, pos(7, 8), &idx, None);
    let labels = call_label_set(&items);
    for expected in &["list", "render", "stash", "current_user", "users", "admin"] {
        assert!(
            labels.iter().any(|l| l == expected),
            "$c-> must offer `{}`; got: {:?}",
            expected,
            labels
        );
    }

    // Part 2: `$c->users->` (chained cross-file) resolves to the
    // _Helper::users proxy and surfaces its leaves. Before the
    // fix: cursor_context couldn't resolve the chain without a
    // module_index, so completion fell through to Users's own
    // methods (`list`).
    let items = crate::lsp::symbols::completion_items_for_test(&fa, &tree, src, pos(8, 15), &idx, None);
    let labels = call_label_set(&items);
    assert_eq!(
        labels.iter().collect::<std::collections::HashSet<_>>(),
        ["create", "delete"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .iter()
            .collect::<std::collections::HashSet<_>>(),
        "$c->users-> must offer exactly the helper chain leaves (create/delete); got: {:?}",
        labels,
    );
    assert!(
        !labels.iter().any(|l| l == "list"),
        "$c->users-> must NOT fall back to Users.pm's own `list`; got: {:?}",
        labels
    );

    // Part 3: `$c->admin->` resolves through the first-level proxy
    // to the innermost `users` step.
    let items = crate::lsp::symbols::completion_items_for_test(&fa, &tree, src, pos(9, 15), &idx, None);
    let labels = call_label_set(&items);
    assert_eq!(
        labels,
        vec!["users"],
        "$c->admin-> must offer exactly `users`; got: {:?}",
        labels
    );

    // Part 4: the proxy's detail is suppressed (opaque_return).
    // No `_Helper::...` string should leak into the user-facing
    // detail of a helper-root completion entry, even cross-file.
    let items = crate::lsp::symbols::completion_items_for_test(&fa, &tree, src, pos(7, 8), &idx, None);
    let users_item = items.iter().find(|it| it.label == "users").unwrap();
    let admin_item = items.iter().find(|it| it.label == "admin").unwrap();
    for (name, item) in [("users", users_item), ("admin", admin_item)] {
        let d = item.detail.as_deref().unwrap_or("");
        assert!(
            !d.contains("_Helper"),
            "opaque_return must suppress proxy class in `{}`'s detail cross-file; got: {:?}",
            name,
            d
        );
    }

    // Part 5: helper calls resolve at the model seam. The ancestor walk
    // picks up plugin-emitted methods on parent classes declared elsewhere
    // in the workspace; the unresolved-method diagnostic consults this same
    // walk, so it stays silent for these by construction.
    for helper in ["users", "admin", "current_user"] {
        assert!(
            fa.resolve_method_in_ancestors("Users", helper, Some(&*idx)).is_some(),
            "helper `{}` must resolve on Users through the app-surface bridge",
            helper
        );
    }
}

/// documentHighlight on a method-call identifier must highlight
/// JUST the method name, not the whole `$obj->method(...)` span.
/// Before this pin: hovering `helper` on one `$app->helper(NAME =>
/// sub { ... })` site underlined every other registration's full
/// multi-line call expression — args, sub bodies, closing `);`
/// all included. Regression trigger: MethodCall ref.span covers
/// the whole call (needed for gd/ref_at inside-args lookup);
/// highlight path now uses `method_name_span` from the ref kind.
#[test]
fn method_call_highlight_uses_method_name_span_only() {
    let src = r#"package MyApp;
sub do_thing { }
sub run {
    my ($self, $x) = @_;
    $self->do_thing($x, 1, 2);
    $self->do_thing(3);
}
"#;
    let fa = build_fa(src);

    // Cursor on `do_thing` at the first call site. Highlight
    // must return ranges whose width == len("do_thing"), never
    // a range that spans past the closing `)` or crosses into
    // the next line.
    let row = 4; // 0-indexed: `    $self->do_thing($x, 1, 2);`
    let col = src.lines().nth(row).unwrap().find("do_thing").unwrap();
    let point = tree_sitter::Point::new(row, col + 1);

    let hits = fa.find_occurrences(point, None);
    assert!(!hits.is_empty(), "should highlight at least one occurrence");

    for (span, _access) in &hits {
        // Must be single-line + width exactly 8 ("do_thing").
        assert_eq!(
            span.start.row, span.end.row,
            "highlight must not span multiple lines; got: {:?}",
            span
        );
        let width = span.end.column - span.start.column;
        assert_eq!(
            width,
            "do_thing".len(),
            "highlight width must match method identifier; got {}: {:?}",
            width,
            span
        );
    }
}

/// `$app->admin->` (chained helper call) completion returns the
/// proxy class's methods — not the fallback full-file list.
/// Validates that `resolve_expression_type` chains through the
/// plugin-synthesized opaque return and
/// `complete_methods_for_class` finds methods on the proxy.
#[test]
fn plugin_mojo_helpers_chained_proxy_completion() {
    let src = r#"
package MyApp;
use Mojolicious::Lite;

my $app = Mojolicious->new;
$app->helper('admin.users.purge' => sub { my ($c, $force) = @_; });
"#;
    let fa = build_fa(src);

    // 1. `$app->admin` resolves to the first-level proxy.
    let admin_proxy = fa
        .find_method_return_type("Mojolicious", "admin", None, None)
        .expect("admin on Mojolicious has a return_type");
    let admin_class = admin_proxy
        .class_name()
        .expect("proxy return_type is a ClassName");
    assert_eq!(admin_class, "Mojolicious::Controller::_Helper::admin");

    // 2. `$app->admin->` completion shows the `users` proxy step.
    let candidates = fa.complete_methods_for_class(admin_class, None);
    let labels: Vec<&str> = candidates.iter().map(|c| c.label.as_str()).collect();
    assert!(
        labels.contains(&"users"),
        "chain completion on admin proxy must surface `users`; got: {:?}",
        labels
    );
    // And the `users` step's detail must NOT leak the internal
    // `_Helper::admin::users` proxy class name — the plugin
    // declared the return type opaque.
    let users_cand = candidates.iter().find(|c| c.label == "users").unwrap();
    assert!(
        !users_cand
            .detail
            .as_deref()
            .unwrap_or("")
            .contains("_Helper"),
        "opaque_return must hide the proxy class from detail: {:?}",
        users_cand.detail,
    );

    // 3. Two levels in — `$app->admin->users` → the innermost proxy.
    let users_proxy = fa
        .find_method_return_type(admin_class, "users", None, None)
        .expect("users on admin proxy has a return_type");
    let users_class = users_proxy.class_name().unwrap();
    assert_eq!(
        users_class,
        "Mojolicious::Controller::_Helper::admin::users"
    );

    // 4. Leaf completion shows `purge`.
    let leaf_candidates = fa.complete_methods_for_class(users_class, None);
    let leaf_labels: Vec<&str> = leaf_candidates.iter().map(|c| c.label.as_str()).collect();
    assert!(
        leaf_labels.contains(&"purge"),
        "leaf proxy must offer `purge`; got: {:?}",
        leaf_labels
    );
}

// ==== Three tests pinning this round's user-facing contracts. ====
//
// They begin RED and get fixed one at a time below. Shape of each is
// "source code + cursor position + real-pipeline assertion" so we
// can't lie about internal function results passing while the LSP
// experience breaks.

/// Outline detail names the semantic kind, LSP kind stays FUNCTION
/// (user config can render an icon for the domain word). Terminal
/// URL handlers (mojo-lite `get '/x' => sub {}`) are `<route>`;
/// routing hops (`->to('Users#list')`) are `<dispatch>` — those
/// two are semantically different and must not collapse. Tasks
/// stay `<task>`, helpers stay `<helper>`, events stay EVENT.
#[test]
fn outline_detail_names_the_semantic_kind() {
    use tower_lsp::lsp_types::SymbolKind;
    let src = r#"package MyApp;
use Mojolicious::Lite;

my $app = Mojolicious->new;
$app->helper(current_user => sub { my ($c) = @_; });

my $r = app->routes;
$r->get('/x')->to('Users#list');
get '/home' => sub { my $c = shift; };

use Minion;
my $minion = Minion->new;
$minion->add_task(send_email => sub { my ($job) = @_; });

package MyEmitter;
use parent 'Mojo::EventEmitter';
sub new {
    my $self = bless {}, shift;
    $self->on('ready', sub { my ($s) = @_; });
    $self;
}
"#;
    let fa = build_fa(src);
    let outline = fa.document_symbols();

    fn flatten<'a>(
        out: &'a [crate::model::file_analysis::OutlineSymbol],
        acc: &mut Vec<&'a crate::model::file_analysis::OutlineSymbol>,
    ) {
        for s in out {
            acc.push(s);
            flatten(&s.children, acc);
        }
    }
    let mut all = Vec::new();
    flatten(&outline, &mut all);

    let lsp_kind = |os: &crate::model::file_analysis::OutlineSymbol| -> SymbolKind {
        crate::lsp::symbols::outline_lsp_kind(os)
    };

    let helper = all
        .iter()
        .find(|s| s.name.contains("current_user"))
        .expect("helper must be in outline of its declaring file");
    assert_eq!(lsp_kind(helper), SymbolKind::FUNCTION);
    assert!(
        helper.detail.as_deref().unwrap_or("").contains("helper"),
        "helper outline detail must contain 'helper'; got: {:?}",
        helper.detail
    );

    // Terminal route: body lives here, `<route>` word.
    let term_route = all
        .iter()
        .find(|s| s.name.contains("/home"))
        .expect("mojo-lite terminal route must be in outline");
    assert_eq!(lsp_kind(term_route), SymbolKind::FUNCTION);
    assert_eq!(
        term_route.detail.as_deref(),
        Some("route"),
        "terminal mojo-lite route word is 'route'; got: {:?}",
        term_route.detail
    );

    // Controller action (`->to('Users#list')`): no body at this
    // site, just a cross-reference into Users::list. Word must be
    // `action`, not `route` — `<route> GET /x` and `<action>
    // Users#list` are semantically different line items.
    let action = all
        .iter()
        .find(|s| s.name.contains("Users#list"))
        .expect("->to('Users#list') action must be in outline");
    assert_eq!(lsp_kind(action), SymbolKind::FUNCTION);
    assert_eq!(
        action.detail.as_deref(),
        Some("action"),
        "->to(...) word is 'action' (distinct from a terminal route); got: {:?}",
        action.detail
    );

    let task = all
        .iter()
        .find(|s| s.name.contains("send_email"))
        .expect("task must be in outline of its declaring file");
    assert_eq!(lsp_kind(task), SymbolKind::FUNCTION);
    assert!(
        task.detail.as_deref().unwrap_or("").contains("task"),
        "task outline detail must contain 'task'; got: {:?}",
        task.detail
    );

    let event = all
        .iter()
        .find(|s| s.name.contains("ready"))
        .expect("event must be in outline of its declaring file");
    assert_eq!(
        lsp_kind(event),
        SymbolKind::EVENT,
        "events stay EVENT — the one LSP kind that fits"
    );
}

/// `sub get { shift->_generate_route(GET => @_) }` — the Mojo
/// Routes::Route pattern. `shift` in the invocant position of a
/// method call within a method body means `$self`, so the chain
/// invocant class must resolve to the enclosing package.
///
/// Without this, every HTTP-verb method on Mojolicious::Routes::Route
/// has an unknowable chain and `$r->get(...)->to(...)` loses
/// intelligence at the `->to` hop.
#[test]
fn shift_as_self_in_method_body_resolves_to_current_package() {
    let src = r#"
package Mojolicious::Routes::Route;

sub get { shift->_generate_route(GET => @_) }

sub _generate_route {
    my $self = shift;
    return $self;
}
"#;
    let fa = build_fa(src);

    // The MethodCall ref for `_generate_route` (inside `get`'s body)
    // must carry `invocant_class = Mojolicious::Routes::Route` —
    // proving the build-time chain resolver treated `shift` as
    // `$self` and looked up the enclosing package.
    let gr_ref = fa
        .refs()
        .iter()
        .find(|r| {
            matches!(r.kind, RefKind::MethodCall { .. }) && r.target_name == "_generate_route"
        })
        .expect("MethodCall ref for `_generate_route`");

    if matches!(gr_ref.kind, RefKind::MethodCall { .. }) {
        let invocant_class = fa.receiver_classes(gr_ref, None, ReceiverPolicy::Navigate).single().map(str::to_string);
        assert_eq!(
            invocant_class.as_deref(),
            Some("Mojolicious::Routes::Route"),
            "`shift->_generate_route` must resolve its invocant to \
                 the enclosing package. got invocant_class: {:?}",
            invocant_class,
        );
    } else {
        panic!("expected MethodCall ref");
    }
}

/// `sub is_endpoint { $_[0]->inline ? undef : ... }` — Mojo uses
/// `$_[0]` instead of `shift` on hot paths where the shift's arg-
/// list mutation is expensive. Same self-tell as `shift`.
#[test]
fn dollar_underscore_zero_as_self_resolves_to_current_package() {
    let src = r#"
package Mojolicious::Routes::Route;

sub is_endpoint {
    $_[0]->inline;
}

sub inline {
    my $self = shift;
    return $self;
}
"#;
    let fa = build_fa(src);

    let inline_ref = fa
        .refs()
        .iter()
        .find(|r| matches!(r.kind, RefKind::MethodCall { .. }) && r.target_name == "inline")
        .expect("MethodCall ref for `inline`");

    if matches!(inline_ref.kind, RefKind::MethodCall { .. }) {
        let invocant_class = fa.receiver_classes(inline_ref, None, ReceiverPolicy::Navigate).single().map(str::to_string);
        assert_eq!(
            invocant_class.as_deref(),
            Some("Mojolicious::Routes::Route"),
            "`$$_[0]->inline` must resolve its invocant to the \
                 enclosing package. got invocant_class: {:?}",
            invocant_class,
        );
    } else {
        panic!("expected MethodCall ref");
    }
}

/// Regression for the crash reported in the nvim LSP log:
/// `thread 'tokio-rt-worker' panicked at src/file_analysis.rs:1164:44:
/// index out of bounds: the len is 17 but the index is 17`.
///
/// Root cause: `enrich_imported_types_with_keys` truncated the constraint
/// store back to baseline but left stale per-var indices behind; the next
/// enrichment's MCB pass indexed past the truncated length. The MCB pass
/// is now `emit_method_call_binding_edges` (bag edges, no index maps),
/// but the double-enrichment repro stays as the idempotency net.
///
/// Repro: enrich the same FileAnalysis twice with a module_index.
/// The second call must not panic.
#[test]
fn enrichment_twice_does_not_crash_on_stale_indices() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    let app_src = r#"
package main;
use Mojolicious::Lite;

my $r = app->routes;
$r->get('/users')->to('Users#list');
"#;
    let mojolicious_pm = r#"
package Mojolicious;
use Mojo::Base -base;
has routes => sub { Mojolicious::Routes->new };
1;
"#;
    let routes_pm = r#"
package Mojolicious::Routes;
use Mojo::Base 'Mojolicious::Routes::Route';
1;
"#;
    let route_pm = r#"
package Mojolicious::Routes::Route;
use Mojo::Base -base;
sub get { my $self = shift; return $self; }
sub to  { my $self = shift; return $self; }
1;
"#;

    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Mojolicious.pm"),
        Arc::new(build_fa(mojolicious_pm)),
    );
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Mojolicious/Routes.pm"),
        Arc::new(build_fa(routes_pm)),
    );
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Mojolicious/Routes/Route.pm"),
        Arc::new(build_fa(route_pm)),
    );

    let mut fa = build_fa(app_src);
    // First enrichment — simulates publish_diagnostics after module
    // resolution. Populates type_constraints + type_constraints_by_var.
    fa.enrich_imported_types_with_keys(Some(&idx));
    // Second enrichment — simulates a subsequent change or refresh.
    // Must not panic, and the re-derived state must stay usable.
    fa.enrich_imported_types_with_keys(Some(&idx));

    // Sanity: `$r` is still typed after the second run (not just
    // "didn't crash" — the state is actually usable).
    let r_type = fa.inferred_type_via_bag("$r", tree_sitter::Point { row: 5, column: 0 });
    assert!(
        r_type.as_ref().and_then(|t| t.class_name()) == Some("Mojolicious::Routes"),
        "after two enrichments, $$r should still be typed as Mojolicious::Routes; got: {:?}",
        r_type,
    );
}

/// Real-file invariant: every meaningful token on the
/// `app->routes` / `$r->get(...)->to(...)` lines of the mojo demo
/// must surface a useful hover AND a useful goto-def. This is the
/// exact scenario the user reports dead in nvim — hover returns
/// nothing, gd has nowhere to go.
///
/// Probes (all on the actual demo file, not a synthetic snippet):
///   * `app`    in `my $r = app->routes;`         → hover mentions Mojolicious; gd lands somewhere
///   * `routes` in `app->routes`                  → hover mentions routes / Mojolicious::Routes; gd into Mojolicious.pm
///   * `$r`     in `$r->get(...)`                 → hover shows the declaration line
///   * `get`    in `$r->get(...)`                 → hover mentions the real Route::get POD; gd into Route.pm
///   * `to`     in `->to('Users#list')`           → hover mentions Route::to; gd into Route.pm
///
/// Any probe returning `None` for BOTH hover and gd is a bug. The
/// test enumerates each probe independently so failures pinpoint
/// which hop of the chain is broken, not "something somewhere".
#[test]
fn mojo_demo_lines_70_71_all_tokens_intelligent() {
    use crate::index::module_index::ModuleIndex;
    use std::sync::Arc;

    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test_files/plugin_mojo_demo.pl");
    let src = std::fs::read_to_string(&path).unwrap();
    let fa = build_fa(&src);
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let _tree = parser.parse(&src, None).unwrap();

    // Stub the three Mojo modules the chain walks through so
    // cross-file resolution has something to reach. Shapes mirror
    // the real @INC modules' method signatures.
    let mojolicious_pm = r#"
package Mojolicious;
use Mojo::Base -base;

=head2 routes

Returns the router.

=cut

has routes => sub { Mojolicious::Routes->new };

=head2 helper

Register a helper.

=cut

sub helper { my $self = shift; }
1;
"#;
    let routes_pm = r#"
package Mojolicious::Routes;
use Mojo::Base 'Mojolicious::Routes::Route';
1;
"#;
    let route_pm = r#"
package Mojolicious::Routes::Route;
use Mojo::Base -base;

=head2 get

  my $route = $r->get('/:foo' => sub ($c) {...});

Generate route matching only C<GET> requests.

=cut

sub get { my $self = shift; return $self; }

=head2 to

  $r->to('Users#list');

Set the route's target.

=cut

sub to { my $self = shift; return $self; }
1;
"#;

    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Mojolicious.pm"),
        Arc::new(build_fa(mojolicious_pm)),
    );
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Mojolicious/Routes.pm"),
        Arc::new(build_fa(routes_pm)),
    );
    idx.register_workspace_module(
        std::path::PathBuf::from("/tmp/Mojolicious/Routes/Route.pm"),
        Arc::new(build_fa(route_pm)),
    );

    // Cross-file enrichment — mirrors `OpenDocs::enrich_open`.
    // Without this pass, MethodCallBindings whose resolution needs
    // a cross-file return type (e.g. `$r = app->routes` needs real
    // Mojolicious.pm's `routes` accessor) don't land in
    // `type_constraints`, and `$r` stays untyped.
    let mut fa = fa;
    fa.enrich_imported_types_with_keys(Some(&idx));
    let fa = fa;

    // Locate the two target lines by content — decoupled from
    // absolute row numbers so reformats don't invalidate the test.
    let (row_app_routes, line_app_routes) = src
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("my $r = app->routes;"))
        .map(|(i, l)| (i, l))
        .expect("demo must contain `my $r = app->routes;`");
    let (row_r_get_to, line_r_get_to) = src
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("$r->get('/users')->to('Users#list');"))
        .map(|(i, l)| (i, l))
        .expect("demo must contain `$r->get('/users')->to('Users#list');`");

    // Column helper — cursor one char into the token, not at its start,
    // so `ref_at` / `symbol_at` hit the token reliably.
    let col_of =
        |line: &str, needle: &str| -> usize { line.find(needle).expect("needle in line") + 1 };
    let probe = |row: usize, col: usize| tree_sitter::Point { row, column: col };

    // Per-probe assertion. Any probe where BOTH hover and gd come
    // back empty is a dead token — the user's reported symptom.
    // Print detailed per-probe status so failures pinpoint the hop.
    let check = |label: &str, point: tree_sitter::Point| {
        let hover = fa.hover_info(point, &src, Some(&idx));
        let def = fa.find_definition(point, Some(&idx));
        assert!(
            hover.is_some() || def.is_some(),
            "[{label}] @ ({},{}) is a dead token — NO hover AND NO gd. \
                 Chain-resolution hit a wall here. src: {:?}",
            point.row,
            point.column,
            src.lines().nth(point.row).unwrap_or("<oob>"),
        );
    };

    // Line 70 probes.
    check(
        "app bareword",
        probe(row_app_routes, col_of(line_app_routes, "app")),
    );
    check(
        "routes accessor",
        probe(row_app_routes, col_of(line_app_routes, "routes")),
    );

    // Line 71 probes.
    check(
        "$r receiver",
        probe(row_r_get_to, col_of(line_r_get_to, "$r")),
    );
    check(
        "get method",
        probe(row_r_get_to, col_of(line_r_get_to, "->get") + 2),
    ); // skip "->"
    check(
        "to method",
        probe(row_r_get_to, col_of(line_r_get_to, "->to") + 2),
    );

    // Focused assertions on `app`:
    //   1. Hover surfaces the plugin's `app` Sub doc — i.e. ref_at
    //      resolves to the narrow FunctionCall ref for the bareword,
    //      NOT the wider MethodCall ref that would describe `routes`.
    //   2. A semantic token lands on the bareword span — the user
    //      reported no highlight on `app->` in nvim; the narrow
    //      FunctionCall ref is what feeds semantic tokens.
    let app_point = probe(row_app_routes, col_of(line_app_routes, "app"));
    let app_hover = fa.hover_info(app_point, &src, Some(&idx));
    let app_hover_text = app_hover.as_deref().unwrap_or("");
    assert!(
        app_hover_text.contains("The Mojolicious application instance"),
        "hover on `app` must surface the plugin-emitted Sub's doc \
             — proving ref_at picked the narrow FunctionCall ref, not \
             the outer MethodCall for `routes`. got: {:?}",
        app_hover,
    );

    // Semantic token on the bareword — any token kind is fine, the
    // point is SOMETHING lights it up.
    let tokens = fa.semantic_tokens();
    let app_row = row_app_routes;
    let app_col_start = line_app_routes.find("app").unwrap();
    let app_col_end = app_col_start + "app".len();
    let app_has_token = tokens.iter().any(|t| {
        t.span.start.row == app_row
            && t.span.start.column == app_col_start
            && t.span.end.column == app_col_end
    });
    assert!(
        app_has_token,
        "semantic token must fire on the `app` bareword span — \
             user reported no highlight and traced it to a missing \
             Ref at the invocant. tokens near row {}: {:?}",
        app_row,
        tokens
            .iter()
            .filter(|t| t.span.start.row == app_row)
            .collect::<Vec<_>>(),
    );

    // Headline chain assertion: `$r` MUST be typed as
    // Mojolicious::Routes after the `my $r = app->routes;` line.
    // This is the single most important observable — without it,
    // every `$r->...` downstream loses intelligence (precisely
    // the user's report). `inferred_type` is the same query
    // invocant_classes_in_scope uses for method resolution, so if
    // this says None, nothing on line 71 can work.
    let r_point = probe(row_r_get_to, col_of(line_r_get_to, "$r"));
    let r_type = fa.inferred_type_via_bag("$r", r_point);
    assert_eq!(
        r_type.as_ref().and_then(|t| t.class_name()),
        Some("Mojolicious::Routes"),
        "`$$r` must be typed as Mojolicious::Routes at the `$$r->get` \
             call site. Without this, the rest of line 71 is dead. got: {:?}",
        r_type,
    );

    // `$r->get` must resolve via inheritance (Mojolicious::Routes
    // ISA Mojolicious::Routes::Route) to the real `get` method.
    // Return type is fluent — stays on Route for `->to` to work.
    let get_rt = fa.find_method_return_type("Mojolicious::Routes", "get", Some(&idx), None);
    assert_eq!(
        get_rt.as_ref().and_then(|t| t.class_name()),
        Some("Mojolicious::Routes::Route"),
        "`$$r->get` must resolve to Mojolicious::Routes::Route::get \
             via inheritance. got: {:?}",
        get_rt,
    );
}

/// Real-file invariant pinning the original nvim repro: line 118
/// of plugin_mojo_demo.pl, which sits textually in `package MyApp`
/// but before the fix was reported as `MyApp::Progress` (the LAST-
/// declared package in the file) — so Minion's trigger didn't
/// match, the plugin hook didn't fire, and the native path
/// mis-keyed the task's sig off the enqueue parens.
///
/// Pinned points:
///   * cursor inside `'alice@example.com'` → $to   (slot 0)
///   * cursor inside `'hi'`                → $subject (slot 1)
///   * cursor inside `'body'`              → $body   (slot 2)
///   * cursor past the closing `]`          → NOT the task sig
#[test]
fn enqueue_sighelp_line_118_of_demo() {
    use tower_lsp::lsp_types::Position;
    use tree_sitter::Parser;

    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("test_files/plugin_mojo_demo.pl");
    let src = std::fs::read_to_string(&path).unwrap();
    let fa = build_fa(&src);
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(&src, None).unwrap();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();

    // Locate the enqueue call by content — line numbers in the
    // demo file shift whenever it's edited, and the test's value
    // is the signature-help behavior, not a literal row.
    let (line_idx, line) = src
        .lines()
        .enumerate()
        .find(|(_, l)| l.contains("$minion->enqueue(send_email"))
        .map(|(i, l)| (i as u32, l))
        .expect("demo must contain the send_email enqueue site");

    let cases: &[(&str, &str, Option<u32>)] = &[
        ("alice@example.com", "'alice", Some(0)),
        ("hi", "'hi'", Some(1)),
        ("body", "'body'", Some(2)),
    ];
    for (slot_label, needle, expected) in cases {
        let col = (line.find(needle).unwrap() + 2) as u32;
        let pos = Position {
            line: line_idx,
            character: col,
        };
        let sig = crate::lsp::symbols::signature_help(&fa, &tree, &src, pos, &idx)
            .unwrap_or_else(|| panic!("[{slot_label}] sig help must fire"));
        assert!(
            sig.signatures[0].label.contains("send_email"),
            "[{slot_label}] task sig expected; got {:?}",
            sig.signatures[0].label
        );
        assert_eq!(
            sig.active_parameter, *expected,
            "[{slot_label}] wrong slot; got {:?}",
            sig.active_parameter
        );
    }

    // Past the closing `]` — must NOT show the task sig.
    let col = (line.rfind(']').unwrap() + 1) as u32;
    let pos = Position {
        line: line_idx,
        character: col,
    };
    if let Some(s) = crate::lsp::symbols::signature_help(&fa, &tree, &src, pos, &idx) {
        let lbl = &s.signatures[0].label;
        assert!(
            !lbl.contains("send_email"),
            "past `]`: task sig must not leak; got {lbl:?}"
        );
    }
}

/// Pinned invariant for the real-nvim Minion sig-help bug:
///
///   * Fat commas and literal commas must produce the SAME
///     signature-help behavior at identical cursor positions.
///     Two cases before the fix: (a) inside the arrayref, both
///     variants routed to the task sig — that worked. (b) once
///     the cursor left the arrayref, the native string-dispatch
///     path keyed the task's active_param off the outer call's
///     literal-comma count, surfacing `$subject` at the options-
///     hash slot, and `$body` several slots into a run of
///     trailing commas. Both wrong in obviously different ways.
///
///   * Cursor inside the arrayref → task sig, correct slot.
///   * Cursor outside the arrayref but still in the enqueue call
///     → NEVER the task sig. Falls through to enqueue's own
///     method sig (none here, since Minion.pm isn't indexed in
///     the test — `None` is the acceptable outcome).
///
/// If this regresses, the sweep-style bug is back: flip to
/// `DUMP_SWEEP=1 cargo test` to get a per-column dump.
#[test]
fn enqueue_sighelp_separator_agnostic() {
    use tower_lsp::lsp_types::Position;
    use tree_sitter::Parser;

    let cases: &[(&str, &str)] = &[
        (
            "literal-comma",
            "$minion->enqueue('send_email', [ 'alice' ], {})",
        ),
        (
            "fat-comma",
            "$minion->enqueue(send_email => [ 'alice' ], , , , )",
        ),
    ];

    let header = "package MyApp;\nuse Minion;\nmy $minion = Minion->new;\n\
             $minion->add_task(send_email => sub { my ($job, $to, $subject, $body) = @_; });\n";

    let dump = std::env::var("DUMP_SWEEP").is_ok();
    let mut dump_out = String::new();

    for (label, call_line) in cases {
        let src = format!("{}{};\n", header, call_line);
        let fa = build_fa(&src);
        let mut parser = Parser::new();
        parser
            .set_language(&ts_parser_perl::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(&src, None).unwrap();
        let idx = crate::index::module_index::ModuleIndex::new_for_test();

        let line_idx = src
            .lines()
            .position(|l| l.starts_with("$minion->enqueue"))
            .unwrap();
        let line = src.lines().nth(line_idx).unwrap();

        // Cursor inside 'alice' → task sig, slot 0 ($to).
        let in_alice = line.find("'alice'").unwrap() + 3;
        let pos = Position {
            line: line_idx as u32,
            character: in_alice as u32,
        };
        let sig = crate::lsp::symbols::signature_help(&fa, &tree, &src, pos, &idx)
            .unwrap_or_else(|| panic!("[{label}] cursor in 'alice' must fire task sig"));
        assert!(
            sig.signatures[0].label.contains("send_email"),
            "[{label}] in 'alice' → task sig; got: {:?}",
            sig.signatures[0].label
        );
        assert_eq!(
            sig.active_parameter,
            Some(0),
            "[{label}] in 'alice' → $to (slot 0); got {:?}",
            sig.active_parameter
        );

        // Cursor past the `]` but still inside the enqueue parens
        // → the options-hash slot / trailing-comma space. MUST NOT
        // show the task sig. `None` is acceptable (enqueue's own
        // method isn't indexed in this test).
        let past_bracket = line.find(']').unwrap() + 2;
        let pos = Position {
            line: line_idx as u32,
            character: past_bracket as u32,
        };
        let sig = crate::lsp::symbols::signature_help(&fa, &tree, &src, pos, &idx);
        if let Some(s) = &sig {
            let lbl = &s.signatures[0].label;
            assert!(
                !lbl.contains("send_email"),
                "[{label}] past `]`: task sig must NOT show; got: {:?}",
                lbl
            );
        }

        // Fat-comma specific: sweep the trailing-commas region
        // and ensure NONE of those columns surface the task sig.
        // Before the fix, each literal comma bumped active_param
        // and produced $subject / $body at arbitrary positions.
        if *label == "fat-comma" {
            let start = line.find(']').unwrap() + 1;
            let end = line.rfind(')').unwrap();
            for col in start..=end {
                let pos = Position {
                    line: line_idx as u32,
                    character: col as u32,
                };
                let sig = crate::lsp::symbols::signature_help(&fa, &tree, &src, pos, &idx);
                if let Some(s) = &sig {
                    let lbl = &s.signatures[0].label;
                    assert!(!lbl.contains("send_email"),
                            "[{label}] col {col}: task sig leaked into trailing-comma region; got: {:?}",
                            lbl);
                }
            }
        }

        if dump {
            dump_out.push_str(&format!("\n=== {} ===\n{}\n", label, line));
            for col in 0..=line.len() {
                let pos = Position {
                    line: line_idx as u32,
                    character: col as u32,
                };
                let sig = crate::lsp::symbols::signature_help(&fa, &tree, &src, pos, &idx);
                let label_str = match &sig {
                    None => "<none>".to_string(),
                    Some(s) => format!(
                        "ap={:?} sig={}",
                        s.active_parameter,
                        s.signatures
                            .first()
                            .map(|si| si.label.as_str())
                            .unwrap_or("")
                    ),
                };
                let ch = line
                    .chars()
                    .nth(col)
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "<eol>".into());
                dump_out.push_str(&format!("col {:>3} ({:<5}): {}\n", col, ch, label_str));
            }
        }
    }

    if dump {
        panic!("{}", dump_out);
    }
}

/// Sanity: the minion plugin registers a task Handler with the
/// expected shape. The arrayref-sig-help behavior itself lives in
/// the plugin's `on_signature_help` IoC hook (tested end-to-end
/// below) — no data flag on the Handler.
#[test]
fn minion_registers_task_handler() {
    let src = r#"package MyApp;
use Minion;
my $minion = Minion->new;
$minion->add_task(send_email => sub { my ($job, $to) = @_; });
"#;
    let fa = build_fa(src);
    let h = fa
        .symbols()
        .iter()
        .find(|s| s.kind == SymKind::Handler && s.name == "send_email")
        .expect("handler exists");
    let SymbolDetail::Handler { dispatchers, .. } = &h.detail
    else {
        panic!("detail shape");
    };
    assert!(
        dispatchers.iter().any(|d| d == "enqueue"),
        "must list enqueue as a dispatcher; got: {:?}",
        dispatchers
    );
    assert!(
        matches!(h.presentation.display, Some(HandlerDisplay::Task)),
        "task handlers display as Task; got: {:?}",
        h.presentation.display
    );
}

/// Test 2 — arrayref sig help, through the REAL LSP pipeline.
/// Cursor sits INSIDE the middle string literal `'hi'` — the
/// shape a user actually produces in nvim. active_parameter must
/// be 1 (= $subject). Earlier version of this test used a
/// cursor-right-after-comma position that nobody types at, and
/// passed while the real nvim experience was broken.
#[test]
fn enqueue_arrayref_sig_help_active_param_inside_string() {
    use tower_lsp::lsp_types::Position;
    use tree_sitter::Parser;

    let src = r#"package MyApp;
use Minion;
my $minion = Minion->new;
$minion->add_task(send_email => sub {
    my ($job, $to, $subject, $body) = @_;
});
$minion->enqueue(send_email => ['alice', 'hi', 'body']);
"#;
    let fa = build_fa(src);
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();

    // Cursor between `h` and `i` of `'hi'` — the middle slot of
    // the arrayref, which is $subject.
    let line_idx = src
        .lines()
        .position(|l| l.contains("enqueue(send_email"))
        .expect("enqueue line present");
    let line = src.lines().nth(line_idx).unwrap();
    let col = line.find("'hi'").unwrap() + 2; // between h and i
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let sig = crate::lsp::symbols::signature_help(&fa, &tree, src, pos, &idx)
        .expect("sig help must fire inside a string-literal arrayref arg");

    let info = &sig.signatures[0];
    assert!(
        info.label.contains("send_email"),
        "label references the task, not enqueue; got: {:?}",
        info.label
    );
    assert!(
        info.label.contains("$subject"),
        "label surfaces the task's params; got: {:?}",
        info.label
    );
    assert_eq!(
        sig.active_parameter,
        Some(1),
        "cursor inside `'hi'` → $subject (index 1), NOT $to. \
             If you see 0 here, sig help isn't recognizing it's inside \
             the arrayref at slot 1; got: {:?}",
        sig.active_parameter
    );
}

/// Sig help must also land on the LAST arrayref slot when the
/// cursor is inside its string literal. Pinned separately from
/// the middle-slot test because count_commas can off-by-one on
/// the last slot if the walker breaks wrong.
#[test]
fn enqueue_arrayref_sig_help_active_param_inside_last_string() {
    use tower_lsp::lsp_types::Position;
    use tree_sitter::Parser;

    let src = r#"package MyApp;
use Minion;
my $minion = Minion->new;
$minion->add_task(send_email => sub {
    my ($job, $to, $subject, $body) = @_;
});
$minion->enqueue(send_email => ['alice', 'hi', 'body']);
"#;
    let fa = build_fa(src);
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();

    let line_idx = src
        .lines()
        .position(|l| l.contains("enqueue(send_email"))
        .unwrap();
    let line = src.lines().nth(line_idx).unwrap();
    let col = line.find("'body'").unwrap() + 3; // inside "body"
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let sig = crate::lsp::symbols::signature_help(&fa, &tree, src, pos, &idx)
        .expect("sig help fires inside the last string too");

    assert_eq!(
        sig.active_parameter,
        Some(2),
        "cursor inside `'body'` → $body (index 2); got: {:?}",
        sig.active_parameter
    );
}

/// Test 3a — hash-key completion on an empty enqueue options hash
/// in a file that ALSO has a matching add_task. The earlier
/// version of this test used an enqueue for an unknown task name,
/// which accidentally sidestepped the dispatch-args short-circuit
/// — nvim's real experience (task registered, enqueue at 3rd arg)
/// was silently broken. Pin the real shape.
#[test]
fn enqueue_options_hash_completion_empty() {
    use tower_lsp::lsp_types::Position;
    use tree_sitter::Parser;

    let src = r#"package MyApp;
use Minion;
my $minion = Minion->new;
$minion->add_task(task_x => sub { my ($job, $a) = @_; });
$minion->enqueue(task_x => ['a'], {  });
"#;
    let fa = build_fa(src);
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();

    // Cursor inside the enqueue options hash — `{  }` on the
    // enqueue line. Can't just search for "{ " globally because
    // the sub body `sub { my ($job` matches first.
    let line_idx = src
        .lines()
        .position(|l| l.contains("enqueue(task_x"))
        .expect("enqueue line");
    let line = src.lines().nth(line_idx).unwrap();
    let col = line.find("{  }").unwrap() + 2; // halfway between `{` and `}`
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let items = crate::lsp::symbols::completion_items_for_test(&fa, &tree, src, pos, &idx, None);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();

    for expected in &["priority", "queue", "delay", "attempts"] {
        assert!(
            labels.contains(expected),
            "empty-hash: `{}` must complete; got: {:?}",
            expected,
            labels
        );
    }
}

/// Test 3b — with an existing key in the hash, it must NOT be
/// offered again; the rest of the options must still appear.
/// Same task-registered shape as 3a so the dispatch-args
/// short-circuit IS active and gets properly bypassed on HashKey.
#[test]
fn enqueue_options_hash_completion_with_existing_keys() {
    use tower_lsp::lsp_types::Position;
    use tree_sitter::Parser;

    let src = r#"package MyApp;
use Minion;
my $minion = Minion->new;
$minion->add_task(task_x => sub { my ($job, $a) = @_; });
$minion->enqueue(task_x => ['a'], { priority => 10,  });
"#;
    let fa = build_fa(src);
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();

    // Scope the anchor to the enqueue line so the sub body's own
    // brace/comma pattern doesn't claim the match first.
    let line_idx = src
        .lines()
        .position(|l| l.contains("enqueue(task_x"))
        .expect("enqueue line");
    let line = src.lines().nth(line_idx).unwrap();
    let col = line.find("priority => 10, ").unwrap() + "priority => 10, ".len();
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let items = crate::lsp::symbols::completion_items_for_test(&fa, &tree, src, pos, &idx, None);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();

    assert!(
        labels.contains(&"queue"),
        "with-existing: `queue` must still complete; got: {:?}",
        labels
    );
    assert!(
        labels.contains(&"delay"),
        "with-existing: `delay` must still complete; got: {:?}",
        labels
    );
    assert!(
        !labels.contains(&"priority"),
        "with-existing: `priority` is already used — must NOT re-appear; got: {:?}",
        labels
    );
}

/// mojo-helpers emits a PluginNamespace for the app, bridging to the
/// single fictional app surface (docs/adr/plugin-system.md). Each
/// registered helper's name is an entity. The consumer classes reach
/// the surface via the synthetic-parent edge in core. Multi-app
/// workspaces get one namespace per app.
#[test]
fn mojo_helpers_emits_app_plugin_namespace() {
    use crate::model::file_analysis::Bridge;
    let src = r#"package MyApp;
use Mojolicious::Lite;
my $app = Mojolicious->new;
$app->helper(current_user => sub { my ($c) = @_; });
$app->helper('users.create' => sub { my ($c) = @_; });
"#;
    let fa = build_fa(src);

    // Identify by semantic shape (kind + bridge to the app surface),
    // not by plugin id — the contract is "there's an 'app' namespace
    // bridging to the app surface", not "a plugin literally called
    // mojo-helpers emits it".
    let ns = fa
        .plugin.namespaces
        .iter()
        .find(|n| {
            n.kind == "app"
                && n.bridges
                    .contains(&Bridge::Class(crate::model::file_analysis::APP_SURFACE_CLASS.into()))
        })
        .expect("an `app` namespace must bridge the app surface");

    // Entities cover both registered helpers, through the
    // name-keyed resolution that expands fan-out Methods.
    let entity_names: Vec<&str> = ns
        .entities
        .iter()
        .map(|id| fa.symbol(*id).name.as_str())
        .collect();
    assert!(
        entity_names.contains(&"current_user"),
        "simple helper must land in the namespace; got: {:?}",
        entity_names
    );
    assert!(
        entity_names.contains(&"users"),
        "dotted-helper root must land in the namespace; got: {:?}",
        entity_names
    );

    // Namespace ID is stable per enclosing package — one namespace
    // for MyApp regardless of how many helpers it registers. Scope
    // the count to this namespace's own (plugin_id, id) pair.
    let count = fa
        .plugin.namespaces
        .iter()
        .filter(|n| n.plugin_id == ns.plugin_id && n.id == ns.id)
        .count();
    assert_eq!(count, 1, "one namespace per app, not one per helper");
}

/// Plugin namespaces are a structural concept (bridges into class
/// lookups via `for_each_entity_bridged_to`) — they are deliberately
/// NOT surfaced in the document outline. The entities inside (helpers,
/// routes, tasks) already render as individual entries with their
/// `<word>` kind prefix; a separate "this file hosts a mojo app" row
/// is noise the user can't act on. The namespace data still has to
/// be populated for cross-file bridge lookups to work — that's what
/// this test pins.
#[test]
fn plugin_namespaces_are_populated_but_not_in_outline() {
    let src = r#"package MyApp;
use Mojolicious::Lite;
app->helper(current_user => sub { my ($c) = @_; });
get '/home' => sub { my $c = shift; };
"#;
    let fa = build_fa(src);

    // The namespace data is still there for bridge queries — that's
    // how `$c->current_user` resolves to the helper across files.
    assert!(
        fa.plugin.namespaces.iter().any(|n| n.kind == "app"),
        "app namespace should still exist in FileAnalysis; got: {:?}",
        fa.plugin.namespaces
            .iter()
            .map(|n| &n.id)
            .collect::<Vec<_>>()
    );

    // Outline must NOT contain any Namespace kind entries from the
    // plugin namespaces. Packages (`MyApp`) are Namespace-kind too
    // but come from SymKind::Package symbols, which are fine.
    let outline = fa.document_symbols();
    let plugin_ns_in_outline: Vec<&str> = outline
        .iter()
        .filter(|o| o.kind == SymKind::Namespace)
        .map(|o| o.name.as_str())
        .filter(|n| n.starts_with('['))
        .collect();
    assert!(
        plugin_ns_in_outline.is_empty(),
        "plugin namespaces must not surface in outline; leaked: {:?}",
        plugin_ns_in_outline,
    );

    // The actual entries (helper, route) still show flat.
    fn walk<'a>(xs: &'a [crate::model::file_analysis::OutlineSymbol], out: &mut Vec<&'a str>) {
        for x in xs {
            out.push(x.name.as_str());
            walk(&x.children, out);
        }
    }
    let mut all = Vec::new();
    walk(&outline, &mut all);
    assert!(
        all.iter().any(|n| n.contains("current_user")),
        "helper must still appear flat in outline; got: {:?}",
        all
    );
    assert!(
        all.iter().any(|n| n.contains("/home")),
        "route must still appear flat in outline; got: {:?}",
        all
    );
}

/// mojo-events emits a PluginNamespace per emitter class. Bridges to
/// the emitter class; entity_names are the event Handler names.
/// Multiple `->on/->once/->subscribe` wire-ups on the same emitter
/// accumulate under the same namespace id.
#[test]
fn mojo_events_emits_emitter_plugin_namespace() {
    use crate::model::file_analysis::Bridge;
    let src = r#"package My::Emitter;
use parent 'Mojo::EventEmitter';
sub register {
    my $self = shift;
    $self->on(connect => sub { my ($e) = @_; });
    $self->on(disconnect => sub { my ($e) = @_; });
    $self->once(ready => sub { my ($e) = @_; });
}
"#;
    let fa = build_fa(src);

    let ns = fa
        .plugin.namespaces
        .iter()
        .find(|n| n.kind == "events" && n.bridges.contains(&Bridge::Class("My::Emitter".into())))
        .expect("an `events` namespace must bridge My::Emitter");

    let entity_names: Vec<&str> = ns
        .entities
        .iter()
        .map(|id| fa.symbol(*id).name.as_str())
        .collect();
    for ev in ["connect", "disconnect", "ready"] {
        assert!(
            entity_names.contains(&ev),
            "event `{}` must land in the namespace; got: {:?}",
            ev,
            entity_names
        );
    }

    let count = fa
        .plugin.namespaces
        .iter()
        .filter(|n| n.plugin_id == ns.plugin_id && n.id == ns.id)
        .count();
    assert_eq!(count, 1, "one namespace per emitter, not one per wire-up");
}

/// mojo-routes emits a PluginNamespace per declaring package. Each
/// `->to('Ctrl#action')` call's Handler lands as a namespace entity;
/// the bridge points at `Mojolicious::Controller` (not the declaring
/// package) so `$c->url_for('|')` from any controller resolves via
/// `for_each_entity_bridged_to` walking through Controller in its
/// ancestor chain. Namespace id still keys on the declaring package
/// so future app-scoping has per-app buckets to narrow to.
#[test]
fn mojo_routes_emits_app_plugin_namespace() {
    use crate::model::file_analysis::Bridge;
    let src = r#"package MyApp;
use Mojolicious;
sub startup {
    my $self = shift;
    my $r = $self->routes;
    $r->get('/users')->to('Users#list');
    $r->post('/users')->to('Users#create');
}
"#;
    let fa = build_fa(src);

    // Identify by semantic shape — a `routes` namespace that
    // bridges to Mojolicious::Controller (the happy-path owner
    // for the workspace-wide url_for lookup). Entity names are
    // the Controller#action form, distinguishing from the Lite
    // path-based flavor.
    let ns = fa
        .plugin.namespaces
        .iter()
        .find(|n| {
            n.kind == "routes"
                && n.bridges
                    .contains(&Bridge::Class("Mojolicious::Controller".into()))
                && n.entities
                    .iter()
                    .any(|id| fa.symbol(*id).name.contains('#'))
        })
        .expect(
            "a `routes` namespace must bridge Mojolicious::Controller with Ctrl#action entities",
        );

    let entity_names: Vec<&str> = ns
        .entities
        .iter()
        .map(|id| fa.symbol(*id).name.as_str())
        .collect();
    assert!(
        entity_names.contains(&"Users#list"),
        "route Users#list must land in the namespace; got: {:?}",
        entity_names
    );
    assert!(
        entity_names.contains(&"Users#create"),
        "route Users#create must land in the namespace; got: {:?}",
        entity_names
    );

    let count = fa
        .plugin.namespaces
        .iter()
        .filter(|n| n.plugin_id == ns.plugin_id && n.id == ns.id)
        .count();
    assert_eq!(
        count, 1,
        "one namespace per declaring package, not one per route"
    );
}

/// mojo-lite emits a PluginNamespace per Lite app. Entity names are
/// the route paths (the same string that mojo-lite stamps into the
/// Handler). Bridge is `Mojolicious::Controller` so `$c->url_for(|)`
/// inside any controller picks up these Lite routes too — mirrors
/// mojo-routes; the Lite script package lives on in the namespace
/// id (`mojo-lite:<pkg>`) for future app-scoping.
#[test]
fn mojo_lite_emits_app_plugin_namespace() {
    use crate::model::file_analysis::Bridge;
    let src = r#"package main;
use Mojolicious::Lite;
get '/users' => sub { my $c = shift; };
post '/login' => sub { my $c = shift; };
"#;
    let fa = build_fa(src);

    let ns = fa
        .plugin.namespaces
        .iter()
        .find(|n| {
            n.kind == "routes"
                && n.bridges
                    .contains(&Bridge::Class("Mojolicious::Controller".into()))
                && n.entities
                    .iter()
                    .any(|id| fa.symbol(*id).name.starts_with('/'))
        })
        .expect(
            "a Lite `routes` namespace must bridge Mojolicious::Controller with /path entities",
        );

    let entity_names: Vec<&str> = ns
        .entities
        .iter()
        .map(|id| fa.symbol(*id).name.as_str())
        .collect();
    assert!(
        entity_names.contains(&"/users"),
        "route /users must land in the namespace; got: {:?}",
        entity_names
    );
    assert!(
        entity_names.contains(&"/login"),
        "route /login must land in the namespace; got: {:?}",
        entity_names
    );
}

/// minion emits a PluginNamespace per enclosing package. Tasks land
/// as entities; bridge is `Class(Minion)` so the namespace feeds the
/// same cross-file lookup primitive used by the other plugins.
/// (The `dispatch_targets_for` completion-hook path is independent —
/// the namespace here is for outline/workspace-symbol and future
/// consolidation of the task-lookup path.)
#[test]
fn minion_emits_tasks_plugin_namespace() {
    use crate::model::file_analysis::Bridge;
    let src = r#"package MyApp;
use Minion;
my $minion = Minion->new;
$minion->add_task(send_email => sub { my ($job) = @_; });
$minion->add_task(resize_image => sub { my ($job) = @_; });
"#;
    let fa = build_fa(src);

    let ns = fa
        .plugin.namespaces
        .iter()
        .find(|n| n.kind == "tasks" && n.bridges.contains(&Bridge::Class("Minion".into())))
        .expect("a `tasks` namespace must bridge Minion");

    let entity_names: Vec<&str> = ns
        .entities
        .iter()
        .map(|id| fa.symbol(*id).name.as_str())
        .collect();
    assert!(
        entity_names.contains(&"send_email"),
        "task send_email must land in the namespace; got: {:?}",
        entity_names
    );
    assert!(
        entity_names.contains(&"resize_image"),
        "task resize_image must land in the namespace; got: {:?}",
        entity_names
    );

    let count = fa
        .plugin.namespaces
        .iter()
        .filter(|n| n.plugin_id == ns.plugin_id && n.id == ns.id)
        .count();
    assert_eq!(count, 1, "one namespace per package, not one per add_task");
}

/// RED — sig help at the OPTIONS hash of enqueue should show
/// enqueue's own signature, not the task's. Currently broken:
/// the string-dispatch sig help fires whenever the cursor is past
/// arg-0 of a dispatcher call, regardless of whether the cursor
/// is actually inside the handler-args slot. For `enqueue`,
/// handler args live INSIDE the arrayref at slot 1 — slot 2 is
/// enqueue's own options hash.
///
/// Proper fix: plugin-controlled dispatch (see
/// `docs/adr/plugin-system.md` — IoC query hooks).
/// The plugin decides when sig help applies to the handler vs
/// when it applies to the dispatcher itself. Core-side fix is
/// possible (narrow the string-dispatch path to the declared
/// handler-args slot) but fragile; leaving as RED until the
/// IoC hook lands.
#[test]
fn enqueue_options_hash_sig_help_is_enqueue_not_task() {
    use tower_lsp::lsp_types::Position;
    use tree_sitter::Parser;

    let src = r#"package MyApp;
use Minion;
my $minion = Minion->new;
$minion->add_task(send_email => sub {
    my ($job, $to, $subject) = @_;
});
$minion->enqueue(send_email => ['a', 'b'], {  });
"#;
    let fa = build_fa(src);
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();

    let line_idx = src
        .lines()
        .position(|l| l.contains("enqueue(send_email"))
        .unwrap();
    let line = src.lines().nth(line_idx).unwrap();
    let col = line.find("{  }").unwrap() + 2;
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let sig = crate::lsp::symbols::signature_help(&fa, &tree, src, pos, &idx);

    // Tight contract: `PluginSigHelpAnswer::Silent` returns None
    // from `signature_help` — full stop. The plugin explicitly
    // claims the slot to block the native string-dispatch path
    // that would mis-show the task's sig. Anything else means
    // either the plugin stopped claiming, or the core's Silent
    // handler regressed.
    assert!(
        sig.is_none(),
        "plugin `Silent` on the options-hash slot must suppress native \
             sig help entirely; got: {:?}",
        sig
    );
}

/// RED — completion at arg-0 of enqueue should offer ONLY
/// registered task names (Handler dispatch targets), not a
/// union of tasks + every other `Minion` instance method.
/// Matches the real nvim env where CPAN-installed Minion brings
/// ~30 instance methods cross-file, which leak in when a
/// user types `$minion->enqueue(|)`.
///
/// Same arch gap as the sig-help one above: the core doesn't
/// know that `enqueue`'s arg-0 is semantically "pick a task
/// name", so `dispatch_target_completions` contributes task
/// names but instance methods reach in through completion of
/// the receiver's class methods on the `$minion->` receiver.
///
/// Proper fix: plugin-controlled `on_completion` hook + the
/// PluginNamespace entities indexed for fast "names of kind
/// `task` on this minion" lookup. See the arch doc.
#[test]
fn enqueue_arg0_offers_task_names_only() {
    use tower_lsp::lsp_types::Position;
    use tree_sitter::Parser;

    // Task-declaring file.
    let src = r#"package MyApp;
use Minion;
my $minion = Minion->new;
$minion->add_task(send_email => sub { my ($job) = @_; });
$minion->add_task(resize_image => sub { my ($job) = @_; });
$minion->enqueue();
"#;
    let fa = build_fa(src);
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();

    // Mock CPAN Minion with realistic instance methods that
    // would otherwise leak into `$minion->enqueue(|)`. Uses the
    // same workspace-module-registration path nvim startup uses.
    let minion_src = r#"package Minion;
sub new { my $class = shift; bless {}, $class }
sub enqueue     { my ($self, $task, $args, $opts) = @_; }
sub enqueue_p   { my ($self, $task, $args, $opts) = @_; }
sub perform_jobs { my ($self) = @_; }
sub backend     { my ($self) = @_; }
sub reset       { my ($self) = @_; }
sub stats       { my ($self) = @_; }
sub worker      { my ($self) = @_; }
sub repair      { my ($self) = @_; }
sub foreground  { my ($self, $id) = @_; }
1;
"#;
    let minion_fa = std::sync::Arc::new(build_fa(minion_src));
    let idx = std::sync::Arc::new(crate::index::module_index::ModuleIndex::new_for_test());
    idx.register_workspace_module(std::path::PathBuf::from("/tmp/Minion.pm"), minion_fa);

    // Cursor inside `enqueue(|)` — just after the `(`.
    let line_idx = src.lines().position(|l| l.ends_with("enqueue();")).unwrap();
    let line = src.lines().nth(line_idx).unwrap();
    let col = line.find("enqueue(").unwrap() + "enqueue(".len();
    let pos = Position {
        line: line_idx as u32,
        character: col as u32,
    };

    let items = crate::lsp::symbols::completion_items_for_test(&fa, &tree, src, pos, &idx, None);
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();

    assert!(
        labels.contains(&"send_email"),
        "task names must appear at enqueue's arg 0; got: {:?}",
        labels
    );
    assert!(
        labels.contains(&"resize_image"),
        "every registered task name must be offered; got: {:?}",
        labels
    );

    // The tight contract — only tasks, nothing else. When this
    // goes green we'll know the plugin owns the completion shape
    // at this position and the Minion-method firehose is gone.
    for label in &labels {
        assert!(
            *label == "send_email" || *label == "resize_image",
            "only task names should appear at enqueue's arg 0; \
                 got unexpected `{}` in {:?}",
            label,
            labels,
        );
    }
}

/// Sig help on a helper call strips `$c` like it strips `$self`.
/// The helper plugin flags its callback's first param as invocant
/// via `as_invocant_params`; the core sig help path drops any
/// invocant-flagged first positional instead of name-matching
/// `$self`/`$class` only.
#[test]
fn plugin_mojo_helpers_sig_help_strips_invocant() {
    use tower_lsp::lsp_types::Position;
    use tree_sitter::Parser;

    let src = r#"package MyApp;
use Mojolicious::Lite;

my $app = Mojolicious->new;
$app->helper(current_user => sub {
    my ($c, $fallback) = @_;
});

sub act {
    my ($c) = @_;
    $c->current_user();
}
"#;
    let fa = build_fa(src);
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();

    // Cursor inside `$c->current_user(|)` — between the parens.
    let (row, col) = src
        .lines()
        .enumerate()
        .find_map(|(r, l)| {
            l.find("current_user()")
                .map(|c| (r, c + "current_user(".len()))
        })
        .expect("find call site");
    let pos = Position {
        line: row as u32,
        character: col as u32,
    };

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let sig = crate::lsp::symbols::signature_help(&fa, &tree, src, pos, &idx)
        .expect("sig help fires on helper call");

    let info = &sig.signatures[0];
    assert!(
        info.label.contains("current_user"),
        "label: {:?}",
        info.label
    );
    assert!(
        info.label.contains("$fallback"),
        "sig should show declared param `$fallback`; got: {:?}",
        info.label
    );
    assert!(
        !info.label.contains("$c"),
        "`$c` must be stripped as invocant; got: {:?}",
        info.label
    );
}

/// Sig help when the cursor sits inside the arrayref at position 1
/// of `enqueue` — the core routes via the Handler's
/// `args_in_arrayref_at` declaration (set by the minion plugin)
/// and shows the task's params (invocant-stripped). Plugin-agnostic
/// on the sig-help side: all the core needs is the declaration.
#[test]
fn plugin_minion_sig_help_on_enqueue_array_args() {
    use tower_lsp::lsp_types::Position;
    use tree_sitter::{Parser, Point};

    let src = r#"package MyApp;
use Minion;

my $minion = Minion->new;
$minion->add_task(send_email => sub {
    my ($job, $to, $subject, $body) = @_;
});
$minion->enqueue(send_email => [ ]);
"#;
    let fa = build_fa(src);

    // Cursor inside the enqueue call's arrayref: point at the
    // single space between the `[` and `]` on the last line.
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();

    // Find the `[ ]` — cursor at col AFTER `[`.
    let mut cursor_point: Option<Point> = None;
    for (row, line) in src.lines().enumerate() {
        if line.contains("enqueue(send_email") {
            if let Some(col) = line.find("[ ") {
                cursor_point = Some(Point::new(row, col + 1));
            }
        }
    }
    let cursor_point = cursor_point.expect("locate cursor inside [ ]");
    let pos = Position {
        line: cursor_point.row as u32,
        character: cursor_point.column as u32,
    };

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let sig = crate::lsp::symbols::signature_help(&fa, &tree, src, pos, &idx)
        .expect("sig help must fire inside enqueue's arrayref");

    // At least one signature, matching the `send_email` handler
    // (the task's params minus $job).
    assert!(!sig.signatures.is_empty(), "at least one signature");
    let info = &sig.signatures[0];
    let label = &info.label;
    assert!(
        label.contains("send_email"),
        "sig label must reference the handler name: {:?}",
        label
    );
    assert!(
        label.contains("$to"),
        "sig should surface task params (`$to`): {:?}",
        label
    );
    assert!(
        !label.contains("$job"),
        "invocant `$job` must be stripped from display: {:?}",
        label
    );
}

/// Hash-key completion on the enqueue options hash
/// (`$minion->enqueue('task', [args], { | })`) — the cursor_context
/// layer now recognizes a nested hash literal as a positional
/// argument and routes it to `HashKeyOwner::Sub { name: enqueue }`.
#[test]
fn plugin_minion_hashkey_help_on_enqueue_options() {
    use tree_sitter::{Parser, Point};
    let src = r#"package MyApp;
use Minion;

my $minion = Minion->new;
$minion->enqueue(task_x => ['arg'] => { });
"#;
    // Build + parse
    let fa = build_fa(src);
    let mut parser = Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();

    // Cursor inside the empty options hash literal `{ | }`.
    // Line 4 (0-indexed) column after "{ " — aim at the middle
    // of the hash's interior.
    let src_bytes = src.as_bytes();
    let mut cursor: Option<Point> = None;
    for (row, line) in src.lines().enumerate() {
        if let Some(col) = line.find("{ ") {
            cursor = Some(Point::new(row, col + 2));
        }
    }
    let cursor = cursor.expect("find the `{ ` in the source");

    let ctx = crate::lsp::cursor_context::detect_cursor_context_tree(&tree, src_bytes, cursor, &fa)
        .expect("context should be detected inside hash literal");
    match ctx {
        crate::lsp::cursor_context::CursorContext::HashKey { source_sub, .. } => {
            assert_eq!(
                source_sub.as_deref(),
                Some("enqueue"),
                "nested {{ }} at call-arg position routes to the callee"
            );
        }
        other => panic!("expected HashKey context, got {:?}", other),
    }

    // Completion path surfaces the plugin's HashKeyDefs.
    let candidates = fa.complete_hash_keys_for_sub("enqueue", cursor, None);
    let labels: Vec<&str> = candidates.iter().map(|c| c.label.as_str()).collect();
    for expected in &["priority", "queue", "delay", "attempts"] {
        assert!(
            labels.contains(expected),
            "enqueue option `{}` must complete; got: {:?}",
            expected,
            labels
        );
    }
}
