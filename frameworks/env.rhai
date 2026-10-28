// env: `use Env qw(HOME @PATH)` ties package variables of the caller to
// %ENV. Env has its own `import` and no export list, so each word declares
// a package global of the current package, at the word itself. A word
// without a sigil is a scalar.
//
// A bare `use Env` ties every %ENV key present at runtime; that set is not
// knowable statically, so the plugin speaks only for an explicit list.

fn id() { "env" }

fn triggers() { [ #{ UsesModule: "Env" } ] }

fn on_use(ctx) {
    if ctx.module_name != "Env" || ctx.negated { return []; }
    let out = [];
    for w in ctx.import_words {
        let name = w.text;
        if !name.starts_with("$") && !name.starts_with("@") { name = "$" + name; }
        out.push(#{ PackageVariable: #{ name: name, span: w.span } });
    }
    out
}
