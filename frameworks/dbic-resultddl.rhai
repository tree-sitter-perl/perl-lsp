// dbic-resultddl: DBIx::Class::ResultDDL DSL → column + relationship
// accessors.
//
// ResultDDL is a sugar layer for declaring DBIC Result classes with
// bareword function calls instead of `__PACKAGE__->add_columns(...)`:
//
//     use DBIx::Class::ResultDDL -V2;
//     table 'embeddings';
//     col text      => text;
//     col embedding => data_type => 'vector(1536)';
//     primary_key qw/text/;
//     has_many searches  => { text => 'SearchTerm.text' };
//     belongs_to product => 'My::Schema::Result::Product';
//
// Each `col NAME` and each relationship (`has_many` / `belongs_to` /
// `has_one` / `might_have` / `many_to_many`) installs an accessor method
// NAME on the Result class. Native DBIC handling only sees the
// `add_columns` / `has_many` METHOD-call form; the ResultDDL barewords
// are invisible to it, so `$row->text` doesn't resolve without this.
//
// We synthesize the accessor symbol (the name is the first arg,
// autoquoted by the fat comma) so `$row->NAME` completes, hovers, and
// gotos-def back to the DSL line. Column return types (text → Str,
// integer → Int, …) and relationship target classes are a deliberate
// follow-up — the accessor existing is the load-bearing 80%.
//
// PORTED to the query-declared capture seam (docs/prompt-plugin-queries.md):
// the verb set lives in the pattern's #any-of?, and the name arg is the
// @name capture with a `str` projection — no positional arg digging.

fn id() { "dbic-resultddl" }

fn api() { 1 }

fn triggers() {
    // Fire only where the DSL is actually in scope. In a plain DBIC
    // project that's a literal `use DBIx::Class::ResultDDL`; in a kit
    // setup it's the kit's SyntheticUse of the same module, which lands
    // in package_uses identically.
    [ #{ UsesModule: "DBIx::Class::ResultDDL" } ]
}

fn patterns() {
    [
        #{
            name: "ddl_decl",
            // DSL calls are usually paren-less (`col text => text;` →
            // ambiguous_function_call_expression); the paren form also
            // occurs. Multi-arg calls wrap a list_expression; a lone
            // name-shaped arg sits directly under `arguments:`.
            // NOTE the group wrap `([…] (#any-of? …))`: a predicate
            // written top-level after a bracketed alternation attaches
            // to NOTHING (the alternation runs unfiltered). The core
            // rejects that spelling at pattern-compile time.
            query: `
([
  (ambiguous_function_call_expression
    function: (_) @verb
    arguments: (list_expression . (_) @name)) @call
  (function_call_expression
    function: (_) @verb
    arguments: (list_expression . (_) @name)) @call
  (ambiguous_function_call_expression
    function: (_) @verb
    arguments: [(autoquoted_bareword) (bareword) (string_literal)] @name) @call
  (function_call_expression
    function: (_) @verb
    arguments: [(autoquoted_bareword) (bareword) (string_literal)] @name) @call
]
(#any-of? @verb "col" "has_many" "belongs_to" "has_one" "might_have" "many_to_many"))
`,
            projections: #{
                name: ["str"],
            },
            expect: [
                #{ src: "col text => text;", matches: 1, captures: #{ name: "text" } },
                #{ src: "has_many searches => { text => 'SearchTerm.text' };", matches: 1,
                   captures: #{ name: "searches" } },
                #{ src: "belongs_to product => 'My::Schema::Result::Product';", matches: 1 },
                #{ src: "col('paren_form', text);", matches: 1 },
                #{ src: "table 'embeddings';", matches: 0 },
                #{ src: "primary_key qw/text/;", matches: 0 },
            ],
        },
    ]
}

fn on_match(pattern, m) {
    let out = [];
    let verb = m.captures.verb.text;
    let name_cap = m.captures.name;
    if name_cap == () { return out; }

    let name = name_cap.str;
    if name == () { return out; }   // dynamic name — can't synthesize

    // Columns read as fields of the row (the table's structure);
    // relationships read as methods. The distinction only affects the
    // outline/completion icon — both are real accessors on the class.
    let display = if verb == "col" { "Field" } else { () };

    out += #{
        Method: #{
            name: name,
            span: name_cap.span,
            selection_span: name_cap.span,
            params: [],
            is_method: true,
            return_type: (),
            doc: (),
            on_class: (),
            display: display,
            hide_in_outline: false,
            opaque_return: false,
            outline_label: (),
        }
    };
    out
}
