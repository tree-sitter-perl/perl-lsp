//! SPIKE: overload-on-param as a Π-type that monomorphizes at the call
//! site. A pure type-algebra model (no grammar) of the one thing the
//! production `ReturnExpr` doesn't yet carry.
//!
//! The engine's `ReturnExpr` (witnesses.rs) is already a dependent return
//! type — conceptually `(receiver, arity, args) -> InferredType`,
//! evaluated against `q.receiver` / `q.arity_hint`. Its free variables:
//! `Receiver` (∀R.→R, fluent), `UnionOnArgs` (arg-indexed dispatch), and
//! `Operator(ParametricOp)` (currently only `RowOf`). What's missing for
//! `sub add { $_[0] + $_[1] }` is:
//!   - a free variable for the n-th ARGUMENT's type (`Arg(n)`, the
//!     sibling of `Receiver`), and
//!   - a parametric operator for an overloadable binop
//!     (`ParametricOp::BinOp`), evaluated with the call's arg types.
//!
//! This module models exactly those, so we can watch the monomorphize
//! happen: a function whose body is `arg0 OP arg1` is a Π-type;
//!   - no call context / numeric args → the operator's mono-type
//!     (the "mostly collapse to int"), the operator-evidence default;
//!   - an argument whose class OVERLOADS the operator → that overload's
//!     return type — and if that return is unknown on the first pass, it
//!     comes back `None` and resolves LATE (the worklist edge-chase),
//!     exactly the "provided late" case.
//!
//! Not wired into the pipeline; this is the representation spike that
//! says what a real `ReturnExpr::Arg` + `ParametricOp::BinOp` would do.

use crate::file_analysis::InferredType;
use std::collections::HashMap;

/// A return body parametric over the argument types — the Π-type
/// `(arg types) -> InferredType`. Mirrors `ReturnExpr`; `Arg` is the
/// missing free variable, `BinOp` the missing `ParametricOp`.
#[derive(Debug, Clone)]
pub enum PiBody {
    Concrete(InferredType),
    /// The n-th argument's type — substituted at the call site.
    Arg(usize),
    /// An overloadable binary operator over two sub-bodies.
    BinOp(&'static str, Box<PiBody>, Box<PiBody>),
}

/// Overload facts: does class C overload operator OP, and to what return
/// type? `Some(None)` = overloaded but the return is not yet known (first
/// pass) → resolves late. `None` = C does not overload OP.
#[derive(Debug, Default)]
pub struct OverloadTable {
    ret: HashMap<(String, &'static str), Option<InferredType>>,
}

impl OverloadTable {
    pub fn set(&mut self, class: &str, op: &'static str, ret: Option<InferredType>) {
        self.ret.insert((class.to_string(), op), ret);
    }
    fn overload_return(&self, class: &str, op: &'static str) -> Option<Option<InferredType>> {
        self.ret.get(&(class.to_string(), op)).cloned()
    }
}

/// The operator's mono-type — what Perl's syntax leaks when no overload
/// is in play. Arithmetic → Numeric, concat/repeat → String. This is the
/// "mostly collapse to int" default (the operator-evidence heuristic).
fn op_mono_type(op: &str) -> InferredType {
    match op {
        "." | "x" => InferredType::String,
        _ => InferredType::Numeric, // + - * / and comparisons
    }
}

impl PiBody {
    /// Monomorphize against concrete argument types. Empty / short `args`
    /// = unknown args (e.g. hover with no call site) → the operator
    /// default. Returns `None` ONLY when an overload is known to apply
    /// but its return type hasn't resolved yet — the "provided late"
    /// signal, which a later worklist pass fills.
    pub fn monomorphize(&self, args: &[InferredType], ov: &OverloadTable) -> Option<InferredType> {
        match self {
            PiBody::Concrete(t) => Some(t.clone()),
            PiBody::Arg(n) => Some(args.get(*n).cloned().unwrap_or(InferredType::Numeric)),
            PiBody::BinOp(op, l, r) => {
                let lt = l.monomorphize(args, ov);
                let rt = r.monomorphize(args, ov);
                // an operand whose class overloads OP dominates: the
                // result is that overload's return (class-identity
                // dominates rep — same rule FrameworkAwareTypeFold uses).
                for operand in [&lt, &rt] {
                    if let Some(InferredType::ClassName(c)) = operand {
                        match ov.overload_return(c, op) {
                            Some(Some(t)) => return Some(t),     // resolved
                            Some(None) => return None,           // late
                            None => {}                            // not overloaded
                        }
                    }
                }
                // no overloading operand → the operator's mono-type.
                Some(op_mono_type(op))
            }
        }
    }
}

#[cfg(test)]
#[path = "overload_pi_tests.rs"]
mod tests;
