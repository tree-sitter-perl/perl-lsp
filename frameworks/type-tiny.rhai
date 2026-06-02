// type-tiny: the Type::Tiny / Types::Standard constraint vocabulary used in
// `isa` (`has minion => (isa => InstanceOf['Clove::Minion'])`).
//
// A constraint constructor (`InstanceOf[...]`) is a *value* — a Type::Tiny
// object — not a class. The core types its call expression as
// `TypeConstraintOf(inner)` and an `isa => …` projects the inner (the
// constrained type) onto the accessor. This plugin owns the vocabulary:
// which names are constraints, and how each folds its params into the inner
// type. The core does the node-walking (rule #1) and hands over a flat param
// list; arity lives here.
//
// Scope (first cut): the "first string param is a class" shape — InstanceOf
// and its role twin ConsumerOf. That's what unlocks `$self->attr->method`
// chains where attr is `isa => InstanceOf['X']`.

fn id() { "type-tiny" }

// Manifest hooks are global (consulted by name), so triggers are irrelevant.
fn triggers() { [] }

// Cheap dispatch gate: which call names are constraint constructors.
fn type_constraint_names() { ["InstanceOf", "ConsumerOf"] }

// Fold the constructor's params into the constrained INNER type, or () to
// decline. `params` is a list of #{ string, ty }. Arity is ours to handle.
fn type_constraint_inner(name, params) {
    if params.is_empty() { return (); }
    // InstanceOf['Foo'] / ConsumerOf['Role'] — first string param is the class.
    let cls = params[0].string;
    if cls == () { return (); }
    type_class(cls)
}
