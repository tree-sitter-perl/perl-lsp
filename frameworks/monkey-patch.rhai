// monkey-patch: load-time method installation. Classes that build their
// method surface with `Mojo::Util::monkey_patch $class => $name => sub {…}`
// expose no static `sub NAME` — but the registration call site IS visible
// in source, so synthesize the named methods the same way Moo `has`
// synthesizes accessors (docs/prompt-method-resolution-residuals.md §1).
// Mojo::UserAgent's HTTP verbs are the canonical case.
//
// The Sub::Install / Class::Method::Modifiers family is the same capability
// with a hashref-options shape (`install_sub({ code, into, as })`); it rides
// this seam once CallContext classifies hashref args for function calls.

fn id() { "monkey-patch" }

fn triggers() {
    [ #{ UsesModule: "Mojo::Util" } ]
}

// monkey_patch $class, name => sub {…}, name2 => sub {…};
fn on_function_call(ctx) {
    let out = [];
    if ctx.function_name != "monkey_patch" { return out; }
    let args = ctx.args;
    if args.len() < 3 { return out; }

    // Target class: a literal string/bareword, or __PACKAGE__. A runtime
    // `$class` doesn't fold — honest miss, never a guess.
    let cls = args[0].string_value;
    if args[0].text == "__PACKAGE__" { cls = ctx.current_package; }
    if cls == () { return out; }

    // (name => sub) pairs from arg 1 on. Loop registrations
    // (`monkey_patch $c, $_, sub {…} for qw(a b)`) fold the topic's
    // candidates into `string_values` — fan out over them.
    let i = 1;
    while i + 1 < args.len() {
        let name_arg = args[i];
        let code_arg = args[i + 1];
        i += 2;

        let names = name_arg.string_values;
        if names == () { names = []; }
        if names.len() == 0 {
            let single = name_arg.string_value;
            if single == () { continue; }
            names = [single];
        }

        let name_span = name_arg.content_span;
        if name_span == () { name_span = name_arg.span; }
        // Installed subs are invoked as methods; their first positional
        // is the receiver — flag it so display drops it at call sites.
        let params = as_invocant_params(code_arg.sub_params);
        if params == () { params = []; }

        for name in names {
            // Unfolded dynamics (`lc $name`, interpolations) are honest
            // misses, not methods named "$_".
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
