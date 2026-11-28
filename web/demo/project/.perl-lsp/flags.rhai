// flags: teaches the server this project's feature-flag DSL.
//
//     feature_flag new_checkout => (default => 0);    # lib/Shop/Flags.pm
//     ... if enabled('new_checkout');                 # anywhere
//
// To perl the flag's name is two unrelated strings. This plugin says they
// are one thing: the declaration defines a flag, a lookup refers to it. Go
// to definition, references and rename on the name follow from that.
//
// Edit this file and the page restarts the server with your version.
// Try it: flags.pl also calls `disabled('beta_search')`, which nothing
// here matches. Add "disabled" to the `#any-of?` list in the query below.

fn id() { "flags" }

fn api() { 1 }

// The declarations and the lookups sit in different files, so the plugin
// runs everywhere and lets its patterns do the choosing.
fn triggers() { [ "Always" ] }

// What hover names as the way a flag is read.
fn lookups() { ["enabled"] }

fn patterns() {
    [
        #{
            name: "flag_call",
            // `feature_flag name => ...` has no parens and `enabled('name')`
            // has them: two node kinds, one shape.
            query: `
([(function_call_expression
    function: (_) @verb
    arguments: [
      (list_expression . (_) @flag)
      (string_literal) @flag
      (bareword) @flag
    ])
  (ambiguous_function_call_expression
    function: (_) @verb
    arguments: [
      (list_expression . (_) @flag)
      (string_literal) @flag
      (bareword) @flag
    ])]
  (#any-of? @verb "feature_flag" "enabled")
) @call
`,
            projections: #{ flag: ["str", "content_span"] },
            expect: [
                #{ src: "feature_flag new_checkout => (default => 0);", matches: 1 },
                #{ src: "enabled('new_checkout');", matches: 1 },
                #{ src: "launch('new_checkout');", matches: 0 },
            ],
        },
    ]
}

fn on_match(pattern, m) {
    let name = m.captures.flag.str;
    if name == () { return []; }

    // Inside the quotes when there are quotes, so a rename rewrites the
    // name and leaves them alone.
    let span = m.captures.flag.content_span;
    if span == () { span = m.captures.flag.span; }

    let verb = m.captures.verb.text;
    let owner = #{ Class: "Shop::Flags" };

    // Every site is a reference to the flag ...
    let out = [ #{ DispatchCall: #{ name: name, dispatcher: verb, owner: owner, span: span, var_text: "" } } ];

    // ... and the declaration is also where it is defined.
    if verb == "feature_flag" {
        out += #{
            Handler: #{
                name: name,
                owner: owner,
                dispatchers: lookups(),
                params: [],
                span: m.span,
                selection_span: span,
            }
        };
    }
    out
}
