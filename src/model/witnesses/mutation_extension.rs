//! The mutation-extension pass: a write through a variable extends the
//! shape it lands in (`docs/adr/structural-shapes.md`).

use super::*;

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
        // First scope on the write's lookup chain whose Variable attachment has
        // witnesses; `crossed` = we climbed past the start scope (index
        // > 0) to find it.
        let mut attach: Option<(ScopeId, bool)> = None;
        for (i, sid) in crate::model::file_analysis::lookup_chain(ctx.scopes, w.scope)
            .scopes()
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
        let widen = w.conditional || scope_crossed;
        let hops: Vec<&crate::model::file_analysis::WriteKey> =
            std::iter::once(&w.key).chain(&w.path).collect();
        let slot_of = |t: InferredType| match w.rhs_slot {
            Some(n) => t.element_at(n).cloned(),
            None => Some(t),
        };
        let vtype = w.rhs_span.and_then(rhs_type).and_then(slot_of);
        let shape = match base {
            InferredType::HashWithKeys { .. } | InferredType::Sequence(_) => {
                let Some(shape) = extend_shape(&base, &hops, vtype.as_ref(), widen) else { continue };
                shape
            }
            // A nested write through an object's slot (`$self->{s}{k} = …`)
            // lands in the slot's value, which every method shares.
            ref obj if !w.path.is_empty() && obj.class_name().is_some() => {
                let crate::model::file_analysis::WriteKey::Hash(ref slot) = w.key else { continue };
                let att = WitnessAttachment::SlotType {
                    class: obj.class_name().unwrap_or_default().to_string(),
                    key: slot.clone(),
                };
                let reg = ReducerRegistry::with_defaults();
                let q = ReducerQuery {
                    attachment: &att,
                    point: None,
                    framework: FrameworkFact::Plain,
                    arity_hint: None,
                    receiver: None,
                    args: Vec::new(),
                    context: Some(ctx),
                };
                let ReducedValue::Type(current) = reg.query(bag, &q) else { continue };
                let hops: Vec<&crate::model::file_analysis::WriteKey> = w.path.iter().collect();
                let Some(shape) = extend_shape(&current, &hops, vtype.as_ref(), widen) else { continue };
                bag.push(Witness {
                    attachment: att,
                    source: WitnessSource::Builder("mutation_extension".into()),
                    payload: WitnessPayload::InferredType(shape),
                    span: Span { start: w.span.start, end: w.span.start },
                });
                continue;
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

/// `shape` after a write through `hops` (outermost key first): a static
/// key at the end joins the shape, its value `vtype`; a key on the way
/// extends that key's value; a dynamic or index key, or a write that may
/// not have run (`widen`), opens the shape it lands on. `None` when the
/// write says nothing new.
fn extend_shape(
    shape: &InferredType,
    hops: &[&crate::model::file_analysis::WriteKey],
    vtype: Option<&InferredType>,
    widen: bool,
) -> Option<InferredType> {
    use crate::model::file_analysis::WriteKey;
    let (first, rest) = hops.split_first()?;
    match shape {
        InferredType::HashWithKeys { keys, open } => {
            let opened = || (!*open).then(|| InferredType::HashWithKeys { keys: keys.clone(), open: true });
            let WriteKey::Hash(k) = first else { return opened() };
            let mut keys = keys.clone();
            // Copy-on-write: `to_mut` clones the key list only when the
            // allocation is shared, so the pass pays per actual divergence,
            // never per query.
            let keys_mut = keys.to_mut();
            match keys_mut.iter_mut().find(|(name, _)| name == k) {
                Some(entry) if rest.is_empty() => {
                    if widen {
                        return opened();
                    }
                    if vtype.is_none() || entry.1.as_deref() == vtype {
                        return None;
                    }
                    entry.1 = vtype.cloned().map(Box::new);
                }
                Some(entry) => {
                    let inner = extend_shape(entry.1.as_deref()?, rest, vtype, widen)?;
                    entry.1 = Some(Box::new(inner));
                }
                None if widen => return opened(),
                None => keys_mut.push((k.to_string(), vivify(rest, vtype).map(Box::new))),
            }
            Some(InferredType::HashWithKeys { keys, open: *open })
        }
        // A tuple slot: only the sound moves, retyping an in-bounds slot or
        // appending at exactly `len`. A write past `len` leaves holes, so the
        // tuple degrades to `ArrayRef`. A tuple has no open flag to widen
        // into, so a write that may not have run changes a slot only by
        // opening the shape below it.
        InferredType::Sequence(elems) => {
            let at = match first {
                WriteKey::Index(i) if *i >= 0 => Some(*i as usize),
                _ => None,
            };
            let Some(i) = at else {
                // A write into an element nobody can name (a dynamic index)
                // may land in any of them: each element's shape opens
                // below it.
                if rest.is_empty() {
                    return None;
                }
                let mut changed = false;
                let elems: Vec<InferredType> = elems
                    .iter()
                    .map(|e| match extend_shape(e, rest, vtype, true) {
                        Some(o) => {
                            changed = true;
                            o
                        }
                        None => e.clone(),
                    })
                    .collect();
                return changed.then_some(InferredType::Sequence(elems));
            };
            if widen && (rest.is_empty() || i >= elems.len()) {
                return None;
            }
            let mut elems = elems.clone();
            if i > elems.len() {
                return Some(InferredType::ArrayRef);
            }
            if !rest.is_empty() {
                if i == elems.len() {
                    elems.push(vivify(rest, vtype).unwrap_or(InferredType::Unknown));
                } else {
                    let inner = extend_shape(&elems[i], rest, vtype, widen)?;
                    elems[i] = inner;
                }
            } else {
                match (i.cmp(&elems.len()), vtype) {
                    (std::cmp::Ordering::Less, None) => return None,
                    (std::cmp::Ordering::Less, Some(vt)) if elems[i] == *vt => return None,
                    (std::cmp::Ordering::Less, Some(vt)) => elems[i] = vt.clone(),
                    (_, vt) => elems.push(vt.cloned().unwrap_or(InferredType::Unknown)),
                }
            }
            Some(InferredType::Sequence(elems))
        }
        _ => None,
    }
}

/// The value a write autovivifies below a missing key, as Perl does: a
/// closed shape holding just the written path, typed from the RHS at the
/// leaf. A missing array autovivifies as a tuple only at index 0, the one
/// slot a fresh array can be written at without holes (a slot the RHS can't
/// type is `Unknown`, since a tuple has no untyped slot); at a later index
/// it is an `ArrayRef`, which says nothing about elements.
/// TODO(element-typed-array): a tuple is only right while nothing else
/// writes the array; these writes move to an element-typed array when the
/// lattice has one.
fn vivify(hops: &[&crate::model::file_analysis::WriteKey], vtype: Option<&InferredType>) -> Option<InferredType> {
    use crate::model::file_analysis::{SharedKeys, WriteKey};
    let Some((first, rest)) = hops.split_first() else { return vtype.cloned() };
    match first {
        WriteKey::Hash(k) => Some(InferredType::HashWithKeys {
            keys: SharedKeys::new(vec![(k.clone(), vivify(rest, vtype).map(Box::new))]),
            open: false,
        }),
        WriteKey::Index(0) => {
            Some(InferredType::Sequence(vec![vivify(rest, vtype).unwrap_or(InferredType::Unknown)]))
        }
        WriteKey::Index(i) if *i > 0 => Some(InferredType::ArrayRef),
        WriteKey::Index(_) | WriteKey::Unknown => None,
    }
}
