// strict: where every variable must be declared. `use strict` with no list
// or a list naming `vars` turns it on for the rest of the enclosing scope,
// `no strict` likewise turns it off, and a module that imports strict into
// its caller turns it on at its `use`.

fn id() { "strict" }

fn triggers() { [ #{ UsesModule: "strict" } ] }

// Modules whose `import` turns strict on in the importing scope.
fn strict_importers() {
    [
        "Moo", "Moo::Role", "Moose", "Moose::Role", "Mouse", "Mouse::Role",
        "Role::Tiny", "Mojo::Base", "Mojolicious::Lite", "Dancer", "Dancer2",
        "common::sense", "Modern::Perl", "strictures", "Test2::V0",
    ]
}

fn rule(required, span) {
    [ #{ RequireDeclaration: #{ required: required, span: span } } ]
}

fn on_use(ctx) {
    if ctx.module_name == "strict" {
        if ctx.imports.len() == 0 || "vars" in ctx.imports {
            return rule(!ctx.negated, ctx.span);
        }
        return [];
    }
    if !ctx.negated && ctx.module_name in strict_importers() {
        return rule(true, ctx.span);
    }
    []
}
