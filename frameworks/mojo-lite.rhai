// mojo-lite: top-level route declarations in Mojolicious::Lite scripts.
//
//   get  '/users' => sub { my $c = shift; ... };
//   post '/login' => sub { ... };
//   any  '/fallback' => sub { ... };
//
// Each route registers a handler keyed by the URL path. Modeling:
//   * Handler symbol named = URL path
//   * owner = Class(current package)  (Lite scripts are usually `main`)
//   * dispatchers = ["url_for"]  (Mojo's route-name-to-URL helper)
//   * params = handler sub's signature (first param is $c:
//     Mojolicious::Controller; the handler sig stripping already
//     collapses invocant-like first args at display time)
//
// Outline surfaces every route as an EVENT symbol with its path +
// handler params. Completion for `url_for('|')` shows every known
// route in the workspace at top priority. The plugin never has to
// wire any of those features — they fall out of `SymKind::Handler`
// machinery.

fn id() { "mojo-lite" }

fn triggers() {
    [
        #{ UsesModule: "Mojolicious::Lite" },
    ]
}

// `use Mojolicious::Lite` monkey-patches a fixed verb set into the
// caller's package at import time. Modeling:
//
//   * Verbs that delegate to a real method elsewhere — `get`, `post`,
//     `helper`, etc. — land as synthetic `Import`s pointing at the real
//     source module (Mojolicious::Routes::Route for route verbs,
//     Mojolicious for helper/hook/plugin). Hover, gd, and sig-help
//     flow through the normal imported-function path to the real sub's
//     POD/signature/return type.
//
//   * `del` is a rename: `del '/x' => sub {}` calls `$routes->delete(...)`.
//     `ImportedSymbol::renamed("del", "delete")` gives the local name
//     the user types + the remote name the LSP looks up cross-file.
//
//   * `app` and `group` are pure closures with nowhere real to point to
//     (`sub { $app }`, a control-flow block). They stay as plugin-emitted
//     Sub symbols — `app` typed as Mojolicious so the `app->routes` chain
//     starts with intelligence; `group` bare.
//
//   * Every verb also emits `FrameworkImport` so the unresolved-function
//     diagnostic skips them regardless of whether the real source module
//     is resolved cross-file yet.

// The lite topic-route DSL: file-level verbs whose `->to('#action')`
// partials inherit the controller the nearest `under(...)->to('ctrl#…')`
// set, scoped by `group { }`. Core keeps the generic frame stack; every
// NAME in the mechanism is declared right here. The SetRouteBase write
// itself is emitted by mojo-routes (the plugin that parses to-specs).
fn topic_route_dsl() {
    #{
        "module": "Mojolicious::Lite",
        "verbs": ["get", "post", "put", "delete", "del", "patch", "options",
                  "any", "websocket", "under"],
        "group_fn": "group",
    }
}


// Mojo's `Mojo::Util::camelize` (a plugin name `'foo_bar'` → class tail
// `Foo_Bar`... actually `-`→`::`, `_`→CamelCase): the convention that maps
// a `plugin 'X'` token to its class-name tail. Pure string work, owned
// here; core resolves the resulting key generically by `::`-tail. Mojo
// plugin names are already CamelCase, so this usually passes through.
fn camelize(token) {
    if token == "" { return ""; }
    let first = token[0];
    if first >= 'A' && first <= 'Z' { return token; }
    let segs = [];
    for dash_seg in token.split('-') {
        let parts = [];
        for piece in dash_seg.split('_') {
            if piece == "" { continue; }
            let lower = piece.to_lower();
            parts.push(lower[0].to_upper().to_string() + lower.sub_string(1));
        }
        segs.push(parts.reduce(|a, p| a + p, ""));
    }
    segs.reduce(|a, s| if a == "" { s } else { a + "::" + s }, "")
}

fn on_use(ctx) {
    if ctx.module_name != "Mojolicious::Lite" { return []; }
    let out = [];
    // The framework loads these unconditionally at startup — no `use`
    // ever names them, so the import-driven resolver would never pull
    // them in and their helper registrations would stay invisible.
    // Declaring the implication here (rule #8: the plugin owns
    // framework behavior) routes them through the normal import →
    // resolve → bridge pipeline.
    out += #{ "SyntheticUse": #{ "module": "Mojolicious::Plugin::DefaultHelpers", "args": [], "imports": [], "span": ctx.span }};
    out += #{ "SyntheticUse": #{ "module": "Mojolicious::Plugin::TagHelpers", "args": [], "imports": [], "span": ctx.span }};

    // `app` — no real sub to link to; monkey-patched `sub { $app }`.
    // Typed Sub is the one acknowledged stub; downstream chain flow
    // (`app->routes->get->to`) depends on this return type.
    out += #{ FrameworkImport: #{ keyword: "app" } };
    out += #{
        Symbol: #{
            name: "app",
            kind: "Sub",
            span: ctx.span,
            selection_span: ctx.span,
            detail: #{
                Sub: #{
                    params: [],
                    is_method: false,
                    doc: "The Mojolicious application instance.",
                    display: "Function",
                    hide_in_outline: true,
                }
            },
            return_type: #{ ClassName: "Mojolicious" },
        }
    };

    // `group` — control-flow block. No corresponding method.
    out += #{ FrameworkImport: #{ keyword: "group" } };
    out += #{
        Symbol: #{
            name: "group",
            kind: "Sub",
            span: ctx.span,
            selection_span: ctx.span,
            detail: #{
                Sub: #{
                    params: [],
                    is_method: false,
                    doc: "Group routes so common prefixes/middlewares apply to all of them.",
                    display: "Function",
                    hide_in_outline: true,
                }
            },
        }
    };

    // Route verbs → Mojolicious::Routes::Route. Each `$routes->$verb(@_)`
    // internally, so the real POD/signature/return type live on Route.
    let route_same_name_verbs = [
        "get", "post", "put", "patch", "options", "any",
        "under", "websocket",
    ];
    let route_imports = [];
    for v in route_same_name_verbs {
        out += #{ FrameworkImport: #{ keyword: v } };
        route_imports += #{ local_name: v };
    }
    // `del` is the rename: local `del` → remote `delete` on Route.
    out += #{ FrameworkImport: #{ keyword: "del" } };
    route_imports += #{ local_name: "del", remote_name: "delete" };

    out += #{
        Import: #{
            module_name: "Mojolicious::Routes::Route",
            imported_symbols: route_imports,
            span: ctx.span,
        }
    };

    // Helper/hook/plugin verbs → Mojolicious. Each `$app->$name(@_)`
    // internally; the real method is on the app class.
    let mojo_verbs = ["helper", "hook", "plugin"];
    let mojo_imports = [];
    for v in mojo_verbs {
        out += #{ FrameworkImport: #{ keyword: v } };
        mojo_imports += #{ local_name: v };
    }
    out += #{
        Import: #{
            module_name: "Mojolicious",
            imported_symbols: mojo_imports,
            span: ctx.span,
        }
    };

    out
}

fn patterns() {
    [
        // `plugin 'Thing'` — goto-def on the name lands on the plugin
        // class's `register`. Same machinery as go-to-controller: the
        // MethodCallRef's invocant token camelizes and tail-matches
        // workspace modules, with ownership of the action (`register` —
        // every Mojolicious plugin has one) as the disambiguator.
        // Namespace-agnostic, so app-specific plugin namespaces
        // (Clove::App::Plugin::*) resolve the same as Mojolicious::Plugin::*.
        #{
            name: "plugin_load",
            query: `
([
  (function_call_expression
    function: (_) @verb
    arguments: (list_expression . (_) @name . (_)? @config)) @call
  (ambiguous_function_call_expression
    function: (_) @verb
    arguments: (list_expression . (_) @name . (_)? @config)) @call
  (function_call_expression
    function: (_) @verb
    arguments: [(string_literal) (interpolated_string_literal) (scalar) (bareword)] @name) @call
  (ambiguous_function_call_expression
    function: (_) @verb
    arguments: [(string_literal) (interpolated_string_literal) (scalar) (bareword)] @name) @call
]
(#eq? @verb "plugin"))
`,
            projections: #{
                name: ["str", "strs", "content_span"],
            },
            expect: [
                #{ src: "plugin 'Minion';", matches: 1, captures: #{ name: "'Minion'" } },
                #{ src: "plugin Minion => { Pg => $url };", matches: 1 },
                #{ src: "plugin($_) for qw/A B/;", matches: 1 },
                #{ src: "plugins('x');", matches: 0 },
            ],
        },
        // Route-declaration verbs. `under` is a middleware-ish route
        // wrapper; `websocket` registers a WS handler. Both behave like
        // handler registrations for our purposes.
        #{
            name: "route_decl",
            query: `
([
  (function_call_expression
    function: (_) @verb
    arguments: (list_expression . (_) @path (anonymous_subroutine_expression)? @handler)) @call
  (ambiguous_function_call_expression
    function: (_) @verb
    arguments: (list_expression . (_) @path (anonymous_subroutine_expression)? @handler)) @call
  (function_call_expression
    function: (_) @verb
    arguments: [(string_literal) (interpolated_string_literal) (scalar)] @path) @call
  (ambiguous_function_call_expression
    function: (_) @verb
    arguments: [(string_literal) (interpolated_string_literal) (scalar)] @path) @call
]
(#any-of? @verb "get" "post" "put" "del" "patch" "any" "under" "websocket"))
`,
            projections: #{
                path: ["str"],
                handler: ["sub_params"],
            },
            expect: [
                #{ src: "get '/users/profile' => sub { my $c = shift; };", matches: 1 },
                #{ src: "under '/admin';", matches: 1 },
                #{ src: "put('/x', sub {});", matches: 1 },
                // Intermediate default-values hash between path and handler:
                // @handler must still bind the trailing sub, not the hash.
                #{ src: "any '/*w' => {w => ''} => sub ($c) { $c->render; };", matches: 1 },
                #{ src: "getter '/x';", matches: 0 },
            ],
        },
        // Dispatchers that take a URL string and resolve to a Lite
        // route. `url_for` generates a URL for a named route;
        // `redirect_to` issues a 302 to the given path. Both make the
        // string argument a real reference to the route's Handler.
        #{
            name: "url_dispatch",
            query: `
(method_call_expression
  method: (_) @verb
  (#any-of? @verb "url_for" "redirect_to")
  arguments: [
    (list_expression . (_) @path)
    (string_literal) @path
    (interpolated_string_literal) @path
    (scalar) @path
  ]
) @call
`,
            projections: #{
                path: ["str"],
            },
            expect: [
                #{ src: "$c->redirect_to('/login');", matches: 1 },
                #{ src: "$c->url_for('user_show');", matches: 1 },
                #{ src: "$c->render(text => 'x');", matches: 0 },
            ],
        },
    ]
}

fn on_match(pattern, m) {
    if pattern == "plugin_load" { return plugin_load_emissions(m); }
    if pattern == "url_dispatch" { return url_dispatch_emissions(m); }
    route_decl_emissions(m)
}

fn plugin_load_emissions(m) {
    let out = [];
    let name_cap = m.captures.name;
    if name_cap == () { return out; }
    let name = name_cap.str;
    if name == () || name == "" { return out; }
    let span = name_cap.content_span;
    if span == () { span = name_cap.span; }
    out += #{
        MethodCallRef: #{
            method_name: "register",
            invocant: camelize(name),
            span: span,
            invocant_span: (),
            bridged: "tail",
        }
    };
    // "You loaded it" — make the resolver pull the plugin in
    // so its helpers register. Default namespace guess only;
    // app-custom namespaces (Clove::App::Plugin::*) are
    // workspace modules and already indexed.
    if !name.contains("::") {
        out += #{ "SyntheticUse": #{ "module": "Mojolicious::Plugin::" + name, "args": [], "imports": [], "span": span }};
    }
    // Multi-value: a lite `plugin($_) for qw/A B/` and a
    // folded constant both surface in `strs`
    // (the const-folding machinery), so record every name.
    let cfg = ();
    let config_cap = m.captures.config;
    if config_cap != () { cfg = config_cap.span; }
    for nm in name_cap.strs {
        out += #{ "PluginLoad": #{ "name": nm, "config_span": cfg }};
    }
    out
}

fn route_decl_emissions(m) {
    let out = [];
    let fname = m.captures.verb.text;

    let path_cap = m.captures.path;
    if path_cap == () { return out; }

    // Path is the first literal string. Dynamic patterns skip.
    let path = path_cap.str;
    if path == () { return out; }

    // Owner: the current package is the Lite app's namespace (often
    // `main` for Lite scripts). That's what url_for resolves against
    // at the framework level.
    let owner_class = m["package"];
    if owner_class == () { return out; }

    // Find the handler sub arg. In Lite syntax `get '/x' => sub {...}`
    // the sub is the @handler capture; other forms (e.g. `get '/x' =>
    // 'Ctrl#act'` with string target) are handled by mojo-routes.
    // The route handler's first positional is always the Mojolicious
    // controller; flag it so the call-site display drops it.
    let handler = m.captures.handler;
    let params = [];
    if handler != () {
        params = as_invocant_params(handler.sub_params);
    }

    // Lite routes are framework-synthesized URL callables. The
    // HTTP verb gets promoted into `outline_label` ("GET /path",
    // "POST /path") so two handlers on the same URL (the common
    // form+submit pair) don't collapse into two visually-identical
    // outline entries. Dispatch still keys on `name: path` — url_for
    // matches on path, not verb.
    //
    // Owner is `Mojolicious::Controller` (not `main` / the Lite
    // script's package): url_for/redirect_to are Controller methods,
    // and routes should be reachable from any controller in the
    // workspace. Same reasoning as mojo-routes.
    out += #{
        Handler: #{
            name: path,
            owner: #{ Class: "Mojolicious::Controller" },
            dispatchers: ["url_for", "redirect_to"],
            params: params,
            // Extent covers the whole `get '/path' => sub {…}` so the outline
            // node encloses the route body — editor sticky-scroll / breadcrumb
            // pin the route while the cursor is inside it. Selection stays the
            // path string for a precise goto anchor.
            span: m.span,
            selection_span: path_cap.span,
            display: "Route",
            hide_in_outline: false,
            outline_label: fname.to_upper() + " " + path,
        }
    };

    // Bridge to Mojolicious::Controller so `for_each_entity_bridged_to(
    // "Mojolicious::Controller", ...)` — the cross-file lookup that
    // powers url_for completion in any controller — finds these route
    // Handlers. Namespace id stays keyed on the declaring package so
    // future app-scoping can still tell Lite scripts apart; the bridge
    // controls visibility.
    out += #{
        PluginNamespace: #{
            id: "mojo-lite:" + owner_class,
            kind: "routes",
            bridges: [ #{ Class: "Mojolicious::Controller" } ],
            entity_names: [path],
            decl_span: path_cap.span,
        }
    };

    // Type the route-handler's first positional as Mojolicious::Controller
    // so `$c->...` inside `get '/path' => sub { my $c = shift; $c->... }`
    // resolves to controller methods + helpers.
    if params.len() >= 1 && handler != () {
        out += #{
            VarType: #{
                variable: params[0].name,
                at: handler.span,
                inferred_type: #{ ClassName: "Mojolicious::Controller" },
            }
        };
    }

    out
}

fn url_dispatch_emissions(m) {
    let out = [];
    let mname = m.captures.verb.text;

    let path_cap = m.captures.path;
    if path_cap == () { return out; }
    let path = path_cap.str;
    if path == () { return out; }

    // Owner: routes are registered under the Lite app's own package.
    // The match-site package is usually `main` (Lite) or whichever
    // controller is calling back into url_for — either way, the
    // Handlers we're pointing at live under the calling package.
    let owner_class = m["package"];
    if owner_class == () { return out; }

    out += #{
        DispatchCall: #{
            name: path,
            dispatcher: mname,
            owner: #{ Class: owner_class },
            span: path_cap.span,
            var_text: "",
        }
    };

    out
}
