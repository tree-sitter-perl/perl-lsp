// dbic: DBIx::Class result-class DSL → column + relationship accessors.
//
// A Result class declares its table shape with class-method calls:
//     __PACKAGE__->add_columns(qw/id name/);   # or  id => { data_type => ... }
//     __PACKAGE__->has_many(comments => 'Schema::Result::Comment', 'post_id');
//     __PACKAGE__->belongs_to(author => 'Schema::Result::User');
//
// A column installs an accessor + a `Bridged` HashKeyDef, so `$row->name` and
// the condition args `$rs->search({ name => ... })` resolve it — but NOT
// `$row->{name}`: a column isn't a hash slot (it's undef in DBIC). A
// relationship installs an accessor whose return names the related shape:
// has_many / many_to_many → a ResultSet, belongs_to / has_one / might_have → row.
//
// The declarator verbs live in the pattern's #any-of?; the flat arg
// names arrive via the `list` projection on the @args capture (the
// pattern-era `arg_names`). This plugin owns the verb vocabulary.
// `ClassIsa("DBIx::Class")` scopes synthesis to result classes at
// any depth. `load_components` parent registration stays core (generic mixin
// machinery). `fluent_verbs` carry a resultset through `$rs->search`, so
// `RowOf`/`find` compose a `$rs->find->col` chain; relationship typing is plain.

fn id() { "dbic" }

fn triggers() {
    [ #{ ClassIsa: "DBIx::Class" } ]
}

fn patterns() {
    [
        #{
            name: "class_decl",
            // Any receiver matches syntactically; the class-level-call
            // gate (`__PACKAGE__->…` / `CurrentClass->…`, never a
            // runtime `$rs->add_columns({...})`) is the
            // `is_package_receiver` projection checked in on_match —
            // "receiver is the current package" isn't expressible in
            // a pattern.
            query: `
(method_call_expression
  invocant: (_) @recv
  method: (_) @verb
  (#any-of? @verb "add_columns" "add_column" "has_many" "many_to_many"
                  "belongs_to" "has_one" "might_have")
  arguments: (_) @args
) @call
`,
            projections: #{
                recv: ["is_package_receiver"],
                args: ["list"],
            },
            expect: [
                #{ src: "__PACKAGE__->add_columns(qw/id name/);", matches: 1 },
                #{ src: "__PACKAGE__->has_many(comments => 'Schema::Result::Comment', 'post_id');", matches: 1 },
                // A runtime call still MATCHES (receiver gating is
                // on_match's job); the search verb never does.
                #{ src: "$rs->add_columns({ x => 1 });", matches: 1 },
                #{ src: "$rs->search({ x => 1 });", matches: 0 },
            ],
        },
    ]
}

// Resultset/row verbs whose FIRST hashref arg is keyed by COLUMN names — the
// `\%cond` of `search(\%cond, \%attrs)`, the `\%cols` of `create`/`update`.
// Core links those keys to the class's columns (so renaming a column rewrites
// them) and, crucially, walks ONLY the first hashref: the trailing `\%attrs`
// hash (`order_by`/`rows`/`group_by`/…) is NOT column-keyed and stays out.
// Owning the verb list here (not in core) keeps core from hardcoding DBIC's API.
fn column_keyed_verbs() {
    ["search", "search_rs", "find", "find_or_create", "find_or_new",
     "update", "update_or_create", "create", "new", "count", "single"]
}

// Class-level DSL verbs inherited from DBIx::Class::Core with no visible
// `sub` anywhere the walker can see — the unresolved-method diagnostic
// consults this union so `__PACKAGE__->set_primary_key(...)` never flags.
fn meta_methods() {
    ["add_columns", "add_column", "set_primary_key", "table",
     "resultset_class", "has_many", "has_one", "belongs_to", "might_have",
     "many_to_many", "load_components"]
}

// Fluent verbs — the call returns the invocant's type UNCHANGED, so they only
// apply to an actual resultset (`$rs->search` → a same-row resultset). DBIC has
// no `Class->search`: search/find live on the resultset, which only comes from
// `$schema->resultset('X')` (typed separately). Core re-publishes the invocant's
// `Parametric(ResultSet{row})`; `RowOf`/`find` then compose the rest (`$rs->find`
// → a row). Verb→return-shape is DBIC's, so it lives here.
fn fluent_verbs() {
    ["search", "search_rs"]
}

fn is_resultset_rel(verb) {
    verb == "has_many" || verb == "many_to_many"
}

fn is_row_rel(verb) {
    verb == "belongs_to" || verb == "has_one" || verb == "might_have"
}

// A column accessor: a get/set Method plus a `Bridged` HashKeyDef (resolves the
// search-condition / constructor arg keys — never a `$row->{col}` deref).
fn column_actions(name, span, pkg) {
    let out = [];
    out += #{
        Method: #{
            name: name,
            span: span,
            selection_span: span,
            params: [ #{ name: "$val", "default": (), is_slurpy: false, is_invocant: false } ],
            is_method: true,
            return_type: (),
            doc: (),
            on_class: (),
            display: (),
            hide_in_outline: false,
            opaque_return: false,
        }
    };
    if pkg != () {
        out += #{
            HashKeyDef: #{
                name: name,
                // A column lives in its own owner namespace, NOT as a `$row->{col}`
                // hash slot (undef in DBIC) — `owner_bridged`, not `Class`.
                owner: owner_bridged(pkg),
                span: span,
                selection_span: span,
            }
        };
    }
    out
}

// A relationship accessor: a no-arg Method whose return names the
// related shape. `ret` is the InferredType (ResultSet class or row class).
fn relationship_method(name, span, ret) {
    #{
        Method: #{
            name: name,
            span: span,
            selection_span: span,
            params: [],
            is_method: true,
            return_type: ret,
            doc: (),
            on_class: (),
            display: (),
            hide_in_outline: false,
            opaque_return: false,
        }
    }
}

fn on_match(pattern, m) {
    let out = [];
    let verb = m.captures.verb.text;

    // Class-level declaration only: `__PACKAGE__->add_columns(...)`, never a
    // runtime `$rs->add_columns({...})` (a query op, not a column decl).
    let recv = m.captures.recv;
    if recv == () || recv.is_package_receiver != true { return out; }

    let args = m.captures.args;
    if args == () { return out; }
    let names = args.list;
    if names == () || names.len() < 1 { return out; }

    if verb == "add_columns" || verb == "add_column" {
        // Every flattened name is a column (the hashref option values
        // carry no name and never reach the `list` projection).
        for pair in names {
            out += column_actions(pair[0], pair[1], m["package"]);
        }
        return out;
    }

    if is_resultset_rel(verb) || is_row_rel(verb) {
        // names[0] is the accessor, names[1] (if present) the related class.
        let acc = names[0];
        let ret = ();
        if is_resultset_rel(verb) {
            ret = #{ ClassName: "DBIx::Class::ResultSet" };
        } else if names.len() >= 2 {
            ret = #{ ClassName: names[1][0] };
        }
        out += relationship_method(acc[0], acc[1], ret);
        return out;
    }

    out
}
