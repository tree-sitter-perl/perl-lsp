//! The single shared query entrypoints: scope-chain walk + framework
//! lookup + reducer dispatch live here and nowhere else.

use super::*;

// ---- Single shared query entrypoints ----
//
// Both the in-builder return fold and `FileAnalysis`'s public queries go
// through these helpers, so the scope-chain walk + framework lookup +
// reducer dispatch lives in exactly one place.

/// Canonical query for "what does this sub return?". Handles local subs
/// (resolved via the `symbols` table to a `Symbol` attachment) and
/// imported / cross-file subs (resolved through
/// `BagContext.module_index`'s exporter index → recurse into the cached
/// module's bag). Callers don't branch on "is this local".
pub fn query_sub_return_type(
    bag: &WitnessBag,
    symbols: &[crate::model::file_analysis::Symbol],
    sub_name: &str,
    arity_hint: Option<u32>,
    receiver: Option<InferredType>,
    context: Option<&BagContext>,
) -> Option<InferredType> {
    let reg = ReducerRegistry::with_defaults();
    // Local-symbol query first — `ReturnExprReducer` picks up any
    // arity-discriminated `UnionOnArgs` on the matching sym.
    let local_sym = symbols.iter().find(|s| {
        s.name == sub_name
            && matches!(
                s.kind,
                crate::model::file_analysis::SymKind::Sub | crate::model::file_analysis::SymKind::Method
            )
    });
    if let Some(sym) = local_sym {
        let att = WitnessAttachment::Symbol(sym.id);
        let q = ReducerQuery {
            attachment: &att,
            point: None,
            framework: FrameworkFact::Plain,
            arity_hint,
            receiver: receiver.clone(),
            args: Vec::new(),
            context,
        };
        match reg.query(bag, &q) {
            ReducedValue::Type(t) => return Some(t),
            ReducedValue::FactMap(_) | ReducedValue::None => {}
        }
        // Cross-symbol dispatch within the sym's class (Mojo::Base
        // getter+writer share a name; at arity=1 the writer's answer is
        // required). `PackageSymbol{package, name}` carries every per-arity
        // arm synthesis published.
        if let Some(class) = sym.package.as_ref() {
            // Default receiver for class-keyed lookup: when the caller
            // didn't pass one, fall back to `ClassName(class)` — the Mojo
            // writer's `Receiver` then evaluates to the fluent return,
            // matching what `$obj->writer()` would produce. A supplied
            // receiver passes through.
            let att = WitnessAttachment::PackageSymbol {
                package: class.clone(),
                name: sub_name.to_string(),
            };
            let effective_receiver = receiver
                .clone()
                .or_else(|| Some(InferredType::ClassName(class.clone())));
            let q = ReducerQuery {
                attachment: &att,
                point: None,
                framework: FrameworkFact::Plain,
                arity_hint,
                receiver: effective_receiver,
                args: Vec::new(),
                context,
            };
            match reg.query(bag, &q) {
                ReducedValue::Type(t) => return Some(t),
                ReducedValue::FactMap(_) | ReducedValue::None => {}
            }
        }
    }
    // Cross-file: the exporters, then a pack's include closure — the same
    // ladder an unresolved `Callee::Function` walks.
    let ctx = context?;
    reg.free_function_cross_file(ctx, sub_name, None, arity_hint, receiver, &[], &mut QueryState::new())
}

/// What `place` holds at `point`. The place names its storage, so there
/// is no chain to walk; `scope` (where the question is asked) supplies only
/// the framework the fold reads under.
pub fn query_place_type(
    bag: &WitnessBag,
    ctx: &BagContext,
    place: &crate::model::file_analysis::Place,
    scope: ScopeId,
    point: Point,
) -> Option<InferredType> {
    let framework = crate::model::file_analysis::lookup_chain(ctx.scopes, scope)
        .scopes()
        .find_map(|sid| ctx.scopes[sid.0 as usize].package.as_ref())
        .and_then(|pkg| ctx.package_framework.framework_of(pkg))
        .unwrap_or(FrameworkFact::Plain);
    let att = WitnessAttachment::Place(Box::new(place.clone()));
    let q = ReducerQuery {
        attachment: &att,
        point: Some(point),
        framework,
        arity_hint: None,
        receiver: None,
        args: Vec::new(),
        context: Some(ctx),
    };
    match ReducerRegistry::with_defaults().query(bag, &q) {
        ReducedValue::Type(t) => Some(t),
        ReducedValue::FactMap(_) | ReducedValue::None => None,
    }
}

/// Walk the scope chain from `scope` upward, folding every Variable
/// witness for `var`; returns the first scope that produces a typed
/// answer, else `None`.
///
/// Public entry: starts a fresh cycle-guard set. Recursive callers
/// already inside `query_rec` must use `query_variable_with_visited`
/// instead so the shared visited set catches mutual `Edge(Variable)`
/// loops.
pub fn query_variable_type(
    bag: &WitnessBag,
    ctx: &BagContext,
    var: &str,
    scope: ScopeId,
    point: Point,
) -> Option<InferredType> {
    let reg = ReducerRegistry::with_defaults();
    let mut state = QueryState::new();
    reg.query_variable_with_visited(bag, ctx, var, scope, point, None, &mut state)
}
