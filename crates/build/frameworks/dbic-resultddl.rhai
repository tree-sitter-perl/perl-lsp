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
// We synthesize the accessor symbol (the name is `args[0]`, autoquoted
// by the fat comma) so `$row->NAME` completes, hovers, and gotos-def
// back to the DSL line. Column return types (text → Str, integer → Int,
// …) and relationship target classes are a deliberate follow-up — the
// accessor existing is the load-bearing 80%.

fn id() { "dbic-resultddl" }

fn triggers() {
    // Fire only where the DSL is actually in scope. In a plain DBIC
    // project that's a literal `use DBIx::Class::ResultDDL`; in a kit
    // setup it's the kit's SyntheticUse of the same module, which lands
    // in package_uses identically.
    [ #{ UsesModule: "DBIx::Class::ResultDDL" } ]
}

// The relationship declarators. Each takes the accessor name as its
// first (autoquoted) arg, exactly like `col`.
fn is_relationship(name) {
    let rels = ["has_many", "belongs_to", "has_one", "might_have", "many_to_many"];
    let hit = false;
    for r in rels { if r == name { hit = true; break; } }
    hit
}

fn on_function_call(ctx) {
    let out = [];
    let fname = ctx.function_name;
    if fname == () { return out; }

    let is_col = fname == "col";
    let is_rel = is_relationship(fname);
    if !is_col && !is_rel { return out; }

    if ctx.args.len() < 1 { return out; }
    let arg0 = ctx.args[0];
    let name = arg0.string_value;
    if name == () { return out; }   // dynamic name — can't synthesize

    // Columns read as fields of the row (the table's structure);
    // relationships read as methods. The distinction only affects the
    // outline/completion icon — both are real accessors on the class.
    let display = if is_col { "Field" } else { () };

    out += #{
        Method: #{
            name: name,
            span: arg0.span,
            selection_span: arg0.span,
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
