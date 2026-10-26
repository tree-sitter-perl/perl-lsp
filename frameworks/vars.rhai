// vars: `use vars qw($x @y)` is the pre-`our` declaration form. Each word
// declares a package global of the current package, at the word itself.

fn id() { "vars" }

fn triggers() { [ #{ UsesModule: "vars" } ] }

fn on_use(ctx) {
    if ctx.module_name != "vars" { return []; }
    ctx.import_words.map(|w| #{ PackageVariable: #{ name: w.text, span: w.span } })
}
