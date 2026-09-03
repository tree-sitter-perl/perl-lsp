// monkey-patch: load-time method installation.
//
// A class that builds its method surface with
// `Mojo::Util::monkey_patch $class => $name => sub {…}` exposes no static
// `sub NAME` — but the registration call site IS in the source, so the
// named methods synthesize the same way Moo's `has` synthesizes accessors
// (docs/prompt-method-resolution-residuals.md §1).
//
// The reach is whatever the candidate fold can name. A transformed name
// (`lc $name`) folds to nothing and is an honest miss — Mojo::UserAgent's own
// HTTP verbs are that shape. Modelling it is const folding through a pure
// builtin, which is in scope for the fold and not for this plugin; see
// `docs/prompt-method-resolution-residuals.md`.
//
// The Sub::Install / Class::Method::Modifiers family is the same capability
// wearing a hashref-options shape (`install_sub({ code, into, as })`); it
// rides this seam once a pattern classifies hashref args for function calls.

fn id() { "monkey-patch" }

// `monkey_patch` is Mojo::Util's export, so the importing file is the scope.
fn triggers() {
    [ #{ UsesModule: "Mojo::Util" } ]
}

fn patterns() {
    [
        #{
            name: "monkey_patch_call",
            // Both call spellings; the target class is the anchored first
            // argument and the (name => sub) tail rides the `args`
            // projection, since the pair count is open.
            query: `
([
  (ambiguous_function_call_expression
    function: (_) @verb
    arguments: (list_expression . (_) @target) @args) @call
  (function_call_expression
    function: (_) @verb
    arguments: (list_expression . (_) @target) @args) @call
]
(#eq? @verb "monkey_patch"))
`,
            projections: #{
                target: ["str", "text"],
                args: ["args"],
            },
            expect: [
                #{ src: "monkey_patch 'My::Class', get => sub { 1 };", matches: 1 },
                #{ src: "monkey_patch __PACKAGE__, put => sub { 1 }, post => sub { 2 };", matches: 1 },
                #{ src: "monkey_patch($c, $_ => sub { 1 });", matches: 1 },
                // A bare `monkey_patch;` has no argument list to anchor.
                #{ src: "monkey_patch;", matches: 0 },
                #{ src: "$obj->monkey_patch('x', sub { 1 });", matches: 0 },
            ],
        },
    ]
}

// The class receiving the methods: a literal, or `__PACKAGE__`. A runtime
// `$class` does not fold — an honest miss, never a guess.
fn target_class(m) {
    let t = m.captures.target;
    if t == () { return (); }
    if t.text == "__PACKAGE__" { return m["package"]; }
    t.str
}

fn on_match(pattern, m) {
    let out = [];
    let cls = target_class(m);
    if cls == () { return out; }

    let args = m.captures.args;
    if args == () { return out; }
    args = args.args;
    if args == () { return out; }

    // (name => sub) pairs, from the argument after the target class. The
    // shared pair walk owns the pairing, the key's candidate set and the
    // callback's signature — one speller for a shape half the plugins use.
    for pair in classified_pairs(args, 1) {
        // A loop registration (`monkey_patch $c, $_, sub {…} for qw(a b)`)
        // folds the topic's candidates into `key_values` — fan out over them;
        // otherwise the single key is the whole set.
        let names = pair.key_values;
        if names == () || names.len() == 0 { names = [pair.key]; }

        // Rename rewrites the name inside the quotes, so prefer the content
        // span; a folded name has none and falls back to its token.
        let name_span = pair.key_content_span;
        if name_span == () { name_span = pair.key_span; }

        // Installed subs are invoked as methods — their first positional is
        // the receiver, so display drops it at call sites.
        let params = as_invocant_params(pair.value_sub_params);
        if params == () { params = []; }

        for name in names {
            // The fold can hand back something no caller could invoke — an
            // unresolved interpolation, a sigil, an empty string. Ask the
            // shared name vocabulary rather than testing for one bad
            // character: a symbol named `$_` is not reachable by any call.
            if !is_callable_sub_name(name) { continue; }
            out += #{
                Method: #{
                    name: name,
                    span: name_span,
                    selection_span: name_span,
                    params: params,
                    is_method: true,
                    return_type: (),
                    doc: (),
                    on_class: cls,
                    display: (),
                    hide_in_outline: false,
                    opaque_return: false,
                    outline_label: (),
                    return_via_edge: pair.value_return_edge,
                    attr: (),
                }
            };
        }
    }
    out
}
