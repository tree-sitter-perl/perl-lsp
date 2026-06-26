//! Measures the overload Π-type: `sub add { $_[0] + $_[1] }` as a
//! parametric return that monomorphizes per call.

use super::*;
use crate::file_analysis::InferredType;

/// `sub add { $_[0] + $_[1] }` → arg0 + arg1.
fn add() -> PiBody {
    PiBody::BinOp("+", Box::new(PiBody::Arg(0)), Box::new(PiBody::Arg(1)))
}

#[test]
fn default_collapses_to_int() {
    // No call context (hover on the def): the Π-type collapses to the
    // operator's mono-type — the "mostly we can collapse to just int".
    let ov = OverloadTable::default();
    assert_eq!(add().monomorphize(&[], &ov), Some(InferredType::Numeric));
    // numeric args agree with the default.
    assert_eq!(
        add().monomorphize(&[InferredType::Numeric, InferredType::Numeric], &ov),
        Some(InferredType::Numeric),
    );
}

#[test]
fn overloading_arg_monomorphizes_to_the_overload_return() {
    // `add($v1, $v2)` where Vector overloads `+` to return a Vector. The
    // Π-type monomorphizes to the overload's return — the call site
    // substitutes the concrete arg class.
    let mut ov = OverloadTable::default();
    ov.set("Vector", "+", Some(InferredType::ClassName("Vector".into())));
    let args = [InferredType::ClassName("Vector".into()), InferredType::ClassName("Vector".into())];
    assert_eq!(
        add().monomorphize(&args, &ov),
        Some(InferredType::ClassName("Vector".into())),
        "monomorphizes to Vector, not Numeric",
    );
}

#[test]
fn overload_return_provided_late() {
    // The worst case you flagged: the overload's return is UNKNOWN on the
    // first pass. The Π-type returns `None` (not a wrong guess) — and a
    // later worklist pass, once the overload sub is typed, fills it. Same
    // late-binding as any forward reference; no premature commitment.
    let args = [InferredType::ClassName("BigInt".into()), InferredType::ClassName("BigInt".into())];

    let mut early = OverloadTable::default();
    early.set("BigInt", "+", None); // overloaded, return not yet resolved
    assert_eq!(add().monomorphize(&args, &early), None, "first pass: unresolved, no guess");

    let mut late = OverloadTable::default();
    late.set("BigInt", "+", Some(InferredType::ClassName("BigInt".into())));
    assert_eq!(
        add().monomorphize(&args, &late),
        Some(InferredType::ClassName("BigInt".into())),
        "provided late: resolves to BigInt",
    );
}

#[test]
fn concat_is_string_and_mixed_args_still_monomorphize() {
    // `sub j { $_[0] . $_[1] }` — string concat is mono-typed String.
    let j = PiBody::BinOp(".", Box::new(PiBody::Arg(0)), Box::new(PiBody::Arg(1)));
    let ov = OverloadTable::default();
    assert_eq!(j.monomorphize(&[], &ov), Some(InferredType::String));

    // but a class overloading `.` (stringification-ish) still dominates.
    let mut ov2 = OverloadTable::default();
    ov2.set("Path", ".", Some(InferredType::ClassName("Path".into())));
    let args = [InferredType::ClassName("Path".into()), InferredType::String];
    assert_eq!(j.monomorphize(&args, &ov2), Some(InferredType::ClassName("Path".into())));
}
