// scalar-util: Scalar::Util's `reftype` and `blessed` report what a
// reference is. Their result is a tag naming the operand, so
// `blessed($x) eq 'Foo'`, `my $t = reftype $x; ... if $t eq 'HASH'` and
// `return unless blessed $x` guard `$x` (docs/adr/flow-narrowing.md).
// Core fires an entry only where the file imported the name from `module`
// (renames included) or called it fully qualified; `ref` is a builtin and
// stays in core.

fn id() { "scalar-util" }

// Manifest hooks are global (consulted by name), so triggers are irrelevant.
fn triggers() { [] }

fn ref_tag_functions() {
    [
        #{ "module": "Scalar::Util", name: "reftype", tag: "RefType" },
        #{ "module": "Scalar::Util", name: "blessed", tag: "Blessed" },
    ]
}
