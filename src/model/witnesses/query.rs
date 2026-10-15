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
    // Cross-file imports: walk the module_index for exporters of
    // `sub_name` and recurse into each cached bag for the matching
    // `Symbol`. The recursion shares the registry — same arity dispatch,
    // overrides, and fold rules; only the bag and symbols change.
    if let Some(ctx) = context {
        if let Some(idx) = ctx.module_index {
            let try_in = |full: &std::sync::Arc<crate::model::file_analysis::FileAnalysis>|
             -> Option<Option<InferredType>> {
                // Outer None = no matching symbol (an enriched retry can't
                // help — enrichment adds no symbols of these kinds);
                // Some(None) = symbol present, type unresolved (retryable).
                let sym = full.symbols().iter().find(|s| {
                    s.name == sub_name
                        && matches!(
                            s.kind,
                            crate::model::file_analysis::SymKind::Sub
                                | crate::model::file_analysis::SymKind::Method
                        )
                })?;
                let cached_ctx = BagContext {
                    scopes: &full.scopes,
                    package_framework: &full.packages,
                    module_index: Some(idx),
                    package_parents: &full.packages,
                    app_surface_consumers: &full.plugin.app_surface_consumers,
                };
                let att = WitnessAttachment::Symbol(sym.id);
                let q = ReducerQuery {
                    attachment: &att,
                    point: None,
                    framework: FrameworkFact::Plain,
                    arity_hint,
                    receiver: receiver.clone(),
                    args: Vec::new(),
                    context: Some(&cached_ctx),
                };
                match reg.query(&full.witnesses, &q) {
                    ReducedValue::Type(t) => Some(Some(t)),
                    ReducedValue::FactMap(_) | ReducedValue::None => Some(None),
                }
            };
            // Two passes so the R4 retry can never SHADOW a later
            // exporter's raw answer: every exporter answers from its raw
            // bag first; only when ALL raw bags miss do the retryable ones
            // consult the enrichment overlay (fallback-on-miss — the raw
            // bag dead-ends when the closed file's sub return chains
            // through ITS OWN imports; the walker pins no edge for
            // imported calls).
            let mut retryable: Vec<std::sync::Arc<crate::model::file_analysis::CachedModule>> =
                Vec::new();
            for module_name in idx.find_exporters(sub_name) {
                // Every candidate file registered under the exporter's name —
                // a split exporter's sub lives in whichever file defines it.
                for cached in idx.visible_def_candidates(&module_name) {
                    let full = idx.bag_present(&cached);
                    match try_in(&full) {
                        Some(Some(t)) => return Some(t),
                        Some(None) => retryable.push(cached),
                        None => {}
                    }
                }
            }
            for cached in retryable {
                crate::util::ghost_stats::count("consult.imported_sub_return");
                if !idx.serves_enriched() {
                    break;
                }
                let full = idx.bag_present(&cached);
                let enriched = idx.enriched_present(&cached);
                if !std::sync::Arc::ptr_eq(&enriched, &full) {
                    if let Some(Some(t)) = try_in(&enriched) {
                        return Some(t);
                    }
                }
            }
            // Pack cross-file: a call whose callee is defined in an INCLUDED
            // header carries no Perl export edge (C free functions never
            // populate `@EXPORT`). Its return type crosses the boundary by the
            // SAME include-closure visibility goto-def uses (`pack_def_paths`):
            // a candidate def is reachable iff the querying file sees its path
            // or the candidate includes the querying file. Gate on that
            // resolved-target identity — never a bare name match — so a
            // same-named free function in an unrelated TU can't contaminate.
            // Distinct return types across reachable candidates (a genuine
            // ambiguity) collapse to silence; agreeing decls (prototype +
            // definition of the one function) fold to their shared answer.
            // A Flat scope (name-keyed pack — PHP) has no closure to gate by:
            // every candidate is reachable BY RULE, and the same agreement
            // fold below keeps a genuinely duplicated name silent. Transparent
            // hosts (no rule known yet) still skip the arm entirely.
            let scope = idx
                .visibility_scope()
                .map(|(p, v)| (p.to_string_lossy().into_owned(), v));
            if scope.is_some() || idx.flat_scope() {
                let mut answer: Option<InferredType> = None;
                let mut ambiguous = false;
                for cached in idx.def_candidates(sub_name) {
                    let p = cached.path.to_string_lossy();
                    // Reachability reads pinned fields (path + include_closure),
                    // so gate BEFORE rehydrating — an unreachable candidate
                    // never pays a bag decode.
                    let reachable = match &scope {
                        Some((self_str, visible)) => visible.contains(p.as_ref())
                            || cached.analysis.pack.include_closure.contains(self_str),
                        None => true,
                    };
                    if !reachable {
                        continue;
                    }
                    let full = idx.bag_present(&cached);
                    let Some(sym) = full.symbols().iter().find(|s| {
                        s.name == sub_name
                            && matches!(
                                s.kind,
                                crate::model::file_analysis::SymKind::Sub
                                    | crate::model::file_analysis::SymKind::Method
                            )
                            && full.is_linkage_visible(s)
                    }) else {
                        continue;
                    };
                    let cached_ctx = BagContext {
                        scopes: &full.scopes,
                        package_framework: &full.packages,
                        module_index: Some(idx),
                        package_parents: &full.packages,
                        app_surface_consumers: &full.plugin.app_surface_consumers,
                    };
                    let att = WitnessAttachment::Symbol(sym.id);
                    let q = ReducerQuery {
                        attachment: &att,
                        point: None,
                        framework: FrameworkFact::Plain,
                        arity_hint,
                        receiver: receiver.clone(),
                        args: Vec::new(),
                        context: Some(&cached_ctx),
                    };
                    match reg.query(&full.witnesses, &q) {
                        ReducedValue::Type(t) => match &answer {
                            Some(existing) if *existing != t => ambiguous = true,
                            None => answer = Some(t),
                            _ => {}
                        },
                        ReducedValue::FactMap(_) | ReducedValue::None => {}
                    }
                }
                if !ambiguous {
                    if let Some(t) = answer {
                        return Some(t);
                    }
                }
            }
        }
    }
    None
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

/// Fold `KeyWrite`s into variable shape witnesses — the mutation-
/// extension pass. For each write on a variable whose shape at the
/// write point is `HashWithKeys`:
///
/// - unconditional static-key write → push the EXTENDED shape (key
///   joins the list, value typed from the RHS expression, `open`
///   preserved). A write to an already-known key retypes its value.
/// - dynamic key, syntactically conditional write, or a write whose
///   scope chain crosses a boundary before reaching the attachment
///   scope (nested block / closure — execution unknowable) → push the
///   same keys with `open: true`.
///
/// Witnesses attach to the scope the variable's existing witnesses
/// live on (so the read-side scope walk finds them) with a zero-width
/// span at the write position — the same temporal contract as TC
/// mirrors: invisible to reads before the write, latest-wins after
/// (`HashWithKeys` subsumption is equality-only, so a different shape
/// legitimately replaces the standing one).
///
/// Re-emittable: fold callers pass `clear = true` (clear-and-emit per
/// iteration, tag `mutation_extension`). Enrichment passes `false` —
/// post-finalize the bag is append-only (removal would shift the
/// sealed `base_witness_count`); duplicate pushes are idempotent under
/// latest-wins and truncated away by the next enrichment cycle.
pub(crate) fn emit_mutation_extension_witnesses(
    bag: &mut WitnessBag,
    ctx: &BagContext,
    key_writes: &[crate::model::file_analysis::KeyWrite],
    clear: bool,
) {
    if clear {
        bag.remove_by_source_tag("mutation_extension");
    }
    // Per-var doc order so later writes see earlier extensions.
    let mut writes: Vec<&crate::model::file_analysis::KeyWrite> = key_writes.iter().collect();
    writes.sort_by_key(|w| (&w.var_text, w.span.start));
    for w in writes {
        // Attach where the variable's existing witnesses live; note
        // whether getting there crosses a scope boundary (nested block
        // or closure — the write may not have executed by read time).
        // First scope up the shared chain whose Variable attachment has
        // witnesses; `crossed` = we climbed past the start scope (index
        // > 0) to find it.
        let mut attach: Option<(ScopeId, bool)> = None;
        for (i, &sid) in crate::model::file_analysis::scope_chain_of(ctx.scopes, w.scope)
            .iter()
            .enumerate()
        {
            let att = WitnessAttachment::Variable { name: w.var_text.clone(), scope: sid };
            if !bag.for_attachment(&att).is_empty() {
                attach = Some((sid, i > 0));
                break;
            }
        }
        let Some((attach_sid, scope_crossed)) = attach else { continue };
        let Some(base) = query_variable_type(bag, ctx, &w.var_text, w.scope, w.span.start)
        else {
            continue;
        };
        let rhs_type = |s: Span| {
            let reg = ReducerRegistry::with_defaults();
            let att = WitnessAttachment::Expr(s);
            let q = ReducerQuery {
                attachment: &att,
                point: None,
                framework: FrameworkFact::Plain,
                arity_hint: None,
                receiver: None,
                args: Vec::new(),
                context: Some(ctx),
            };
            match reg.query(bag, &q) {
                ReducedValue::Type(t) => Some(t),
                ReducedValue::FactMap(_) | ReducedValue::None => None,
            }
        };
        let shape = match base {
            InferredType::HashWithKeys { mut keys, open } => {
                // An Index write on a hash-shaped var is contradictory
                // evidence — widen like any unknowable write.
                let widen = !matches!(w.key, crate::model::file_analysis::WriteKey::Hash(_))
                    || w.conditional
                    || scope_crossed;
                if widen {
                    if open {
                        continue; // already open — nothing to add
                    }
                    InferredType::HashWithKeys { keys, open: true }
                } else {
                    let crate::model::file_analysis::WriteKey::Hash(ref k) = w.key else {
                        unreachable!()
                    };
                    let vtype = w.rhs_span.and_then(rhs_type);
                    // Copy-on-write: `to_mut` clones the key list only when
                    // the allocation is shared — the extension path pays
                    // O(S) per ACTUAL divergence, never per query.
                    let keys_mut = keys.to_mut();
                    match keys_mut.iter_mut().find(|(name, _)| name == k) {
                        Some(entry) => {
                            if vtype.is_none() || entry.1.as_deref() == vtype.as_ref() {
                                continue; // no new information
                            }
                            entry.1 = vtype.map(Box::new);
                        }
                        None => keys_mut.push((k.to_string(), vtype.map(Box::new))),
                    }
                    InferredType::HashWithKeys { keys, open }
                }
            }
            // Sequence slot write: only the sound moves — retype an
            // in-bounds slot, append at exactly len. Everything else
            // (out-of-bounds, conditional, crossed, Unknown) is
            // unmodeled: Sequence has no open flag to widen into, and
            // a bare-ArrayRef downgrade loses to structure-dominates-
            // rep subsumption. No array-index diagnostic exists, so
            // `element_at`'s honest None covers the residual.
            InferredType::Sequence(mut elems) => {
                let crate::model::file_analysis::WriteKey::Index(i) = w.key else { continue };
                if w.conditional || scope_crossed || i < 0 {
                    continue;
                }
                let Some(vt) = w.rhs_span.and_then(rhs_type) else { continue };
                let i = i as usize;
                if i < elems.len() {
                    if elems[i] == vt {
                        continue; // no new information
                    }
                    elems[i] = vt;
                } else if i == elems.len() {
                    elems.push(vt);
                } else {
                    continue;
                }
                InferredType::Sequence(elems)
            }
            _ => continue,
        };
        bag.push(Witness {
            attachment: WitnessAttachment::Variable {
                name: w.var_text.clone(),
                scope: attach_sid,
            },
            source: WitnessSource::Builder("mutation_extension".into()),
            payload: WitnessPayload::InferredType(shape),
            span: Span { start: w.span.start, end: w.span.start },
        });
    }
}
