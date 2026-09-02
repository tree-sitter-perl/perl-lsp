// monkey-patch: load-time method installation.
//
// A class that builds its method surface with
// `Mojo::Util::monkey_patch $class => $name => sub {…}` exposes no static
// `sub NAME` — but the registration call site IS in the source, so the
// named methods synthesize the same way Moo's `has` synthesizes accessors
// (docs/prompt-method-resolution-residuals.md §1).
//
// The reach is LITERAL registrations. Mojo::UserAgent's own HTTP verbs —
// `monkey_patch __PACKAGE__, lc $name, sub {…}` inside a `for` over a qw
// list — are deliberately NOT covered: the candidate fold resolves the
// variables inside a name expression without applying the transformation
// around them, so trusting it there installs `GET` where the real method is
// `get`. A missing method is recoverable; a method that does not exist makes
// goto-def and the unresolved-method lint both confidently wrong.
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

    // (name => sub) pairs, from the argument after the target class.
    let i = 1;
    while i + 1 < args.len() {
        let name_arg = args[i];
        let code_arg = args[i + 1];
        i += 2;

        // The name must be ATOMIC: a literal, a bareword, or a plain scalar.
        // A compound expression is an honest miss, because the candidate fold
        // resolves the variables inside it and not the transformation around
        // them — `lc $n` over `qw(lc_a LC_B)` folds to the raw list and would
        // install `LC_B`, a method that does not exist. A method name never
        // contains whitespace, so an atomic token is the check.
        let name_text = name_arg.text;
        if name_text == () { continue; }
        if name_text.contains(" ") || name_text.contains("\t") { continue; }

        // A loop registration (`monkey_patch $c, $_, sub {…} for qw(a b)`)
        // folds the topic's candidates into `string_values` — fan out over
        // them; otherwise it is the single literal.
        let names = name_arg.string_values;
        if names == () { names = []; }
        if names.len() == 0 {
            let single = name_arg.string_value;
            if single == () { continue; }
            names = [single];
        }

        // Rename rewrites the name inside the quotes, so prefer the content
        // span; a folded name has none and falls back to its token.
        let name_span = name_arg.content_span;
        if name_span == () { name_span = name_arg.span; }

        // Installed subs are invoked as methods — their first positional is
        // the receiver, so display drops it at call sites.
        let params = as_invocant_params(code_arg.sub_params);
        if params == () { params = []; }

        for name in names {
            // An unfolded dynamic (`lc $name`, an interpolation) is an
            // honest miss, not a method literally named `$_`.
            if name.contains("$") { continue; }
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
                    return_via_edge: code_arg.callable_return_edge,
                    attr: (),
                }
            };
        }
    }
    out
}
