// autovivification: the CPAN pragma's lexical bits. `no autovivification`
// turns vivification off for the categories it names (`fetch`, `exists`
// and `delete` when it names none), and `'strict'` makes an avoided
// vivification die. `use autovivification` turns the named bits back off,
// or every bit when it names none. `warn` changes no outcome.

fn id() { "autovivification" }

fn triggers() { [ #{ UsesModule: "autovivification" } ] }

fn categories() { #{ fetch: "Fetch", store: "Store", exists: "Exists", delete: "Delete" } }

fn hint(h, on, span) { #{ Hint: #{ hint: h, on: on, span: span } } }

fn on_use(ctx) {
    if ctx.module_name != "autovivification" { return []; }
    let cats = categories();
    let named = ctx.imports;
    if named.len() == 0 {
        named = if ctx.negated { ["fetch", "exists", "delete"] }
                else { ["fetch", "store", "exists", "delete", "strict"] };
    }
    let out = [];
    for n in named {
        if n == "strict" {
            out.push(hint("VivifyStrict", ctx.negated, ctx.span));
        } else if n in cats {
            out.push(hint(#{ NoVivify: cats[n] }, ctx.negated, ctx.span));
        }
    }
    out
}
