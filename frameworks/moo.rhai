// moo: the Moo/Moose `has` accessor-option vocabulary. A `has` declaration's
// non-default options each synthesize a method whose NAME follows a per-option
// convention:
//
//   predicate => 1   -> has_<attr>     (private attr `_x` -> `_has_x`)
//   clearer   => 1   -> clear_<attr>   (private attr `_x` -> `_clear_x`)
//   builder   => 1   -> _build_<attr>
//   writer    => 'set_x'   -- explicit name, takes one value arg
//   reader    => 'get_x'   -- explicit name
//   handles => { local => 'remote', ... } / [qw/m.../]
//                    -> one method per local name; when the attribute's isa is
//                       a class, the delegated method's return edges to the
//                       remote method on that class
//
// Core peels the `has` arg list flat (rule #1 — only the builder touches
// tree-sitter) and per-arg classifies its `value_shape`; this plugin pairs
// the option tail with the shared `classified_pairs` host fn (the same
// primitive `to` uses) and reads each value's shape. No keyword allowlist in
// core, so a new keyword needs no core edit. What lives HERE is the
// vocabulary: which keyword maps to which name pattern and return behavior;
// arms below ignore keywords/shapes
// they don't act on. The default accessor / isa / constructor-key synthesis is
// still native in `visit_has_call`; this plugin is the seam beyond it.

fn id() { "moo" }

// The base role engines this family ships: `use`-ing one makes the
// consuming package a ROLE (is_role_package; the composer-mismatch
// diagnostic skips roles — their requires obligations transfer to the
// eventual class composer). Core holds NO list of its own — this
// manifest and any third-party plugin's are the whole set. House kits
// that re-export one of these can instead emit SyntheticUse (see
// adr/role-contracts.md). Role::Tiny::With is deliberately absent: it
// grants `with` to plain classes.
fn role_makers() {
    ["Moo::Role", "Moose::Role", "Mouse::Role", "Role::Tiny"]
}

// The `has`/`option` declaration shape, query-declared. The attr-name
// head arrives via the `list` projection on @attrs (covers `has 'x'`,
// `has [qw/a b/]`, `has \@attrs` through the constant fold); the option
// tail arrives as full ArgInfos via the `args` transition projection so
// the shared `classified_pairs` host fn keeps pairing it; `isa` is the
// one Moo-semantic resolution core performs (string vocabulary +
// constraint-constructor fold). A bare `has 'x';` (no option tail, no
// list_expression) doesn't match — nothing to synthesize beyond the
// native accessor.
fn patterns() {
    [
        #{
            name: "has_decl",
            query: `
([
  (ambiguous_function_call_expression
    function: (_) @verb
    arguments: (list_expression . (_) @attrs) @args) @call
  (function_call_expression
    function: (_) @verb
    arguments: (list_expression . (_) @attrs) @args) @call
]
(#any-of? @verb "has" "option"))
`,
            projections: #{
                attrs: ["list"],
                args: ["args", "isa"],
            },
            expect: [
                #{ src: "has token => (is => 'ro', predicate => 1);", matches: 1,
                   captures: #{ verb: "has", attrs: "token" } },
                #{ src: "has '_secret', is => 'ro', clearer => 1;", matches: 1 },
                #{ src: "option verbose => (is => 'ro', format => 'i');", matches: 1 },
                #{ src: "has 'plain';", matches: 0 },
                #{ src: "$obj->has('x', 1);", matches: 0 },
            ],
        },
    ]
}

// The modules whose `use` grants the consuming package Moo-family `has`
// semantics: native accessor/constructor-key synthesis, the flavor's isa
// vocabulary, and the keyword surface each module exports into the
// consumer. Core holds NO module list — this manifest (plus any
// third-party plugin's) is the whole set; the builder bakes
// module → (flavor, keywords) at plugin load. `flavor` selects which of
// core's rule sets applies ("Moo" / "Moose"); Mojo::Base is absent by
// design — its `-base` gate is structural, not a name match.
fn moo_keywords() { ["has", "with", "extends", "around", "before", "after"] }
fn moose_keywords() {
    moo_keywords() + ["override", "super", "inner", "augment", "confess", "blessed"]
}

fn framework_mode_makers() {
    [
        #{ "module": "Moo", flavor: "Moo", imports: moo_keywords() },
        // `requires` is role-only sugar; a plain class never calls it.
        #{ "module": "Moo::Role", flavor: "Moo", imports: moo_keywords() + ["requires"] },
        // Dancer2::Plugin re-exports Moo's `has`/`with`/... into the consuming
        // plugin package, so `has my_setting => (...)` there wants the same
        // accessor synthesis.
        #{ "module": "Dancer2::Plugin", flavor: "Moo", imports: moo_keywords() },
        // Role::Tiny / Role::Tiny::With behave like Moo::Role for our
        // purposes: they export `with` (and `requires`, via the role sugar)
        // into the consuming package.
        #{ "module": "Role::Tiny", flavor: "Moo", imports: moo_keywords() + ["requires"] },
        #{ "module": "Role::Tiny::With", flavor: "Moo", imports: moo_keywords() + ["requires"] },
        // MooX::Options' `option` is a `has` with extra option-parsing keys;
        // it routes through the shared `has` path at the call site, gated on
        // the `option` keyword being in this import surface.
        #{ "module": "MooX::Options", flavor: "Moo", imports: moo_keywords() + ["option"] },
        #{ "module": "Moose", flavor: "Moose", imports: moose_keywords() },
        #{ "module": "Moose::Role", flavor: "Moose", imports: moose_keywords() + ["requires"] },
        // Mouse mimics Moose's keyword and `has` surface.
        #{ "module": "Mouse", flavor: "Moose", imports: moose_keywords() },
        #{ "module": "Mouse::Role", flavor: "Moose", imports: moose_keywords() + ["requires"] },
    ]
}

// Derived from the maker manifest, so the plugin's accessor-option
// vocabulary can never fire for a module core doesn't grant `has`
// semantics (or vice versa) — one declaration feeds both gates.
fn triggers() {
    let out = [];
    for m in framework_mode_makers() {
        out.push(#{ UsesModule: m["module"] });
    }
    out
}

// Keywords whose value names a method to synthesize. Any OTHER option keyword
// (`is`/`isa`/`default`/`lazy`/`coerce`/`required`/`trigger`/…) carries data,
// not a method name — its value (`'ro'`, `'Str'`, …) must NOT mint a symbol.
// This gate is the whole fix for the phantom-`ro`/`rw`/`lazy` regression: core
// hands every fat-comma option through by value shape (rule #1), and the
// vocabulary of "which keyword produces a name" lives here (rule #8/#10).
fn names_a_method(kw) {
    kw == "reader" || kw == "writer" || kw == "accessor"
        || kw == "predicate" || kw == "clearer" || kw == "builder" || kw == "handles"
}

// Derive a `=> 1` shorthand name: `prefix + attr`, with the Moo/Moose private
// convention that a leading `_` on the attribute moves to the front of the
// derived name (`_token` + predicate -> `_has_token`, not `has__token`).
fn derive_name(prefix, attr) {
    if attr.starts_with("_") {
        "_" + prefix + attr.sub_string(1)
    } else {
        prefix + attr
    }
}

// A plain method symbol with no return type — predicate (returns bool),
// clearer (returns undef), builder (return is whatever the user's sub yields).
// `attr` enrolls the method in the attribute's projection group: rename
// of the attr re-derives this method's name (when it embeds the attr) and
// references from any group spelling include its call sites. Pass () for
// methods whose identity is NOT the attr's (handles delegations).
fn plain_method(name, span, params, attr) {
    #{
        Method: #{
            name: name,
            span: span,
            selection_span: span,
            params: params,
            is_method: true,
            return_type: (),
            doc: (),
            on_class: (),
            display: (),
            hide_in_outline: false,
            opaque_return: false,
            outline_label: (),
            return_via_edge: (),
            attr: attr,
        }
    }
}

// Pull the class name out of an `isa_type` map, or () if the isa isn't a
// plain class (Str/Int/HashRef/etc. can't back a delegation return edge).
fn isa_class(isa_type) {
    if isa_type == () { return (); }
    if isa_type.contains("ClassName") { return isa_type.ClassName; }
    ()
}

// The `=> 1` shorthand: the generic value-shape is `Num("1")`.
fn is_shorthand(val) {
    val.Num == "1"
}

// `handles => {l => r}` / `[qw/m/]` as `[local, remote]` pairs. The
// "arrayref entry is its own remote" rule is Moo's, so it lives here:
// the generic primitive just hands us `HashPairs` / `ArrayItems`.
fn handles_pairs(val) {
    if val.HashPairs != () { return val.HashPairs; }
    let out = [];
    if val.ArrayItems != () {
        for n in val.ArrayItems { out.push([n, n]); }
    }
    out
}

fn on_match(pattern, m) {
    let out = [];
    let attrs_cap = m.captures.attrs;
    let args_cap = m.captures.args;
    if attrs_cap == () || args_cap == () { return out; }

    let attr_names = attrs_cap.list;
    if attr_names == () || attr_names.len() < 1 { return out; }

    let cls = isa_class(args_cap.isa);
    // The accessor options are the keyval tail after the attr-name head
    // (always one arg — a name or `[qw/…/]`), paired by the shared host fn
    // over the flattened arg list. Each `opt.value` is a `value_shape`.
    let pairs = classified_pairs(args_cap.args, 1);

    for attr_pair in attr_names {
        let attr = attr_pair[0];
        // `+attr` modifies a parent-declared attribute — its options
        // apply to the parent's attr; don't mint methods under `+x`.
        if attr.starts_with("+") { continue; }
        let span = attr_pair[1];

        for opt in pairs {
            let kw = opt.key;
            let val = opt.value;

            // `lazy_build => 1` (Moose) implies a builder/clearer/predicate
            // trio at runtime; expand it to the three companion methods.
            if kw == "lazy_build" && is_shorthand(val) {
                out += plain_method("_build_" + attr, span, [], attr);
                out += plain_method(derive_name("clear_", attr), span, [], attr);
                out += plain_method(derive_name("has_", attr), span, [], attr);
                continue;
            }

            // Only name-producing keywords mint a method. Everything else
            // (is/isa/default/lazy/…) carries data, not a method name.
            if !names_a_method(kw) { continue; }

            if kw == "handles" {
                // One method per delegated name. With a class-typed isa, edge
                // the method's return through MethodOnClass{class, remote} so
                // `$self->log(...)` chains to the remote method's return.
                for pair in handles_pairs(val) {
                    let local = pair[0];
                    let remote = pair[1];
                    let edge = ();
                    if cls != () {
                        edge = #{ PackageSymbol: #{ "package": cls, name: remote } };
                    }
                    out += #{
                        Method: #{
                            name: local,
                            span: span,
                            selection_span: span,
                            params: [],
                            is_method: true,
                            return_type: (),
                            doc: (),
                            on_class: (),
                            display: (),
                            hide_in_outline: false,
                            opaque_return: false,
                            outline_label: (),
                            return_via_edge: edge,
                            attr: (),
                        }
                    };
                }
                continue;
            }

            // Resolve the method name: shorthand derives from the attr, an
            // explicit string is used verbatim. Anchor point matters for rename:
            // an explicit string IS where the method name is written, so the
            // method's decl span is that string — renaming the attr (when the
            // name embeds it) then rewrites the defining string too, not just
            // the call sites (else Moo keeps minting `has_size` while callers
            // say `has_dim`). The `=> 1` derived name has no string; Moo
            // regenerates it from the attr at runtime, so the attr span is right.
            let name = ();
            let name_span = span;
            if is_shorthand(val) {
                if kw == "predicate" { name = derive_name("has_", attr); }
                else if kw == "clearer" { name = derive_name("clear_", attr); }
                else if kw == "builder" { name = "_build_" + attr; }
                // writer/reader have no `=> 1` shorthand in Moo.
            } else if val.Str != () {
                name = val.Str;
                name_span = if opt.value_content_span != () { opt.value_content_span }
                            else { opt.value_span };
            }
            if name == () { continue; }

            // writer/accessor take a value arg; the rest take none.
            let params = [];
            if kw == "writer" || kw == "accessor" {
                params = [ #{ name: "$val", "default": (), is_slurpy: false, is_invocant: false } ];
            }
            out += plain_method(name, name_span, params, attr);
        }
    }

    out
}
