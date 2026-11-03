//! The mutation-extension pass: a write through a variable extends the
//! shape it lands in (`docs/adr/structural-shapes.md`).
//!
//! A write is a DELTA (`WitnessPayload::Extend`): the key it adds and an
//! edge to the value it writes. The shape a read sees is the value standing
//! at each write with that write's delta laid over it, folded when the place
//! is read (`ReducerRegistry::lower_extensions`) — never a copy of the grown
//! shape per write, which is N² keys for N writes to one hash.

use super::*;
use crate::model::file_analysis::{KeyWrite, WriteKey};

/// Push one `Extend` delta per `KeyWrite`. For each write:
///
/// - unconditional static-key write → the key joins the shape it lands in,
///   its value the RHS expression (a write to an already-known key retypes
///   its value);
/// - dynamic key, syntactically conditional write, or a write whose scope
///   chain crosses a boundary before reaching the binding's scope (nested
///   block / closure — execution unknowable) → `widen`: the shape opens.
///
/// A nested write through an object's slot (`$self->{s}{k} = …`) lands in
/// the slot's value, which every method shares, so its delta goes on
/// `Field{class, s}`; every other delta goes on the variable's place, with
/// a zero-width span at the write — invisible to reads before it.
///
/// `bindings[i]` is `key_writes[i]`'s variable binding and its home scope,
/// resolved by the caller's binder before the bag is borrowed.
///
/// The fold passes `clear = true` (clear-and-emit per iteration, tag
/// `mutation_extension`: the object-slot routing reads the root's type).
/// Enrichment passes `false` — post-finalize the bag is append-only — and
/// adds only the object-slot deltas: a place's deltas are already in the bag
/// and fold onto whatever imported value stands under them at read time.
pub(crate) fn emit_mutation_extension_witnesses(
    bag: &mut WitnessBag,
    ctx: &BagContext,
    key_writes: &[KeyWrite],
    clear: bool,
    bindings: &[(crate::model::file_analysis::Binding, ScopeId)],
) {
    debug_assert_eq!(bindings.len(), key_writes.len());
    if clear {
        bag.remove_by_source_tag("mutation_extension");
    }
    // Per-var doc order so a place's deltas land in the order they apply.
    let mut writes: Vec<(&KeyWrite, &(crate::model::file_analysis::Binding, ScopeId))> =
        key_writes.iter().zip(bindings).collect();
    writes.sort_by_key(|(w, _)| (&w.var_text, w.span.start));
    // Route every write before pushing any delta, so the routing reads see
    // the bag the fold handed in.
    let mut routes = SlotRoutes::default();
    let routed: Vec<Option<String>> = writes
        .iter()
        .map(|(w, (binding, _))| {
            if w.path.is_empty() || !matches!(w.key, WriteKey::Hash(_)) {
                return None;
            }
            let place = crate::model::file_analysis::Place::variable(binding.clone());
            routes.owner_at(bag, ctx, &place, w.span.start)
        })
        .collect();
    for ((w, (binding, home)), owner) in writes.into_iter().zip(routed) {
        // Attach where the value the write extends is bound — the scope a
        // read takes it from — never where the write merely uses it (a
        // block's own `$v->{k}` access would hide the shape from every read
        // after the block). Climbing past the write's own scope means the
        // write may not have run by read time.
        let widen = w.conditional || *home != w.scope;
        let value = w.rhs_span.map(WitnessAttachment::Expr);
        let span = Span { start: w.span.start, end: w.span.start };
        let source = WitnessSource::Builder("mutation_extension".into());
        match (owner, &w.key) {
            (Some(owner), WriteKey::Hash(slot)) => {
                bag.push(Witness {
                    attachment: WitnessAttachment::Field { owner, name: slot.clone() },
                    source,
                    payload: WitnessPayload::Extend(KeyExtension {
                        hops: w.path.clone(),
                        value,
                        slot: w.rhs_slot,
                        widen,
                    }),
                    span,
                });
            }
            _ if clear => {
                bag.push(Witness {
                    attachment: WitnessAttachment::Place(Box::new(
                        crate::model::file_analysis::Place::variable(binding.clone()),
                    )),
                    source,
                    payload: WitnessPayload::Extend(KeyExtension {
                        hops: std::iter::once(w.key.clone()).chain(w.path.iter().cloned()).collect(),
                        value,
                        slot: w.rhs_slot,
                        widen,
                    }),
                    span,
                });
            }
            _ => {}
        }
    }
}

/// Which class's slot a nested write through a place lands in: the class of
/// the value standing at the write. A delta never changes whether a place
/// holds an object (it grows a shape or does nothing), so the answer moves
/// only where some other witness of the place does — the place is re-read
/// at a segment boundary (`segment_boundary`), never once per write.
#[derive(Default)]
struct SlotRoutes {
    current: Option<RouteSegment>,
}

struct RouteSegment {
    place: crate::model::file_analysis::Place,
    others: Vec<Witness>,
    at: Point,
    owner: Option<String>,
}

impl SlotRoutes {
    fn owner_at(
        &mut self,
        bag: &WitnessBag,
        ctx: &BagContext,
        place: &crate::model::file_analysis::Place,
        at: Point,
    ) -> Option<String> {
        let reuse = self.current.as_ref().is_some_and(|seg| {
            seg.place == *place && !segment_boundary(seg.others.iter(), seg.at, at)
        });
        if !reuse {
            let att = WitnessAttachment::Place(Box::new(place.clone()));
            let others = bag
                .iter_attachment_indexed(&att)
                .map(|(_, w)| w)
                .filter(|w| !matches!(w.payload, WitnessPayload::Extend(_)))
                .cloned()
                .collect();
            let owner = query_variable_place(bag, ctx, place, at)
                .and_then(|t| t.class_name().map(str::to_string));
            self.current = Some(RouteSegment { place: place.clone(), others, at, owner });
        }
        let seg = self.current.as_mut().expect("set above");
        seg.at = at;
        seg.owner.clone()
    }
}

/// Did the value standing at a place move between two of its writes
/// (`prev` < `at`) other than by the writes themselves? Some other witness
/// of the place opens in `(prev, at]`, or a region holding at `prev` ends
/// before `at`. A region holding at both cannot: the write at `prev` is a
/// point fact after the region opened, and the fold lets that outrank it.
pub(super) fn segment_boundary<'a>(others: impl Iterator<Item = &'a Witness>, prev: Point, at: Point) -> bool {
    others.into_iter().any(|w| {
        let starts = prev < w.span.start && w.span.start <= at;
        let region_ends = w.span.start != w.span.end && prev <= w.span.end && w.span.end < at;
        starts || region_ends
    })
}

/// `shape` after a write through `hops` (outermost key first), in place: a
/// static key at the end joins the shape, its value `vtype`; a key on the
/// way extends that key's value; a dynamic or index key, or a write that may
/// not have run (`widen`), opens the shape it lands on. `false` when the
/// write says nothing new.
///
/// In place because the fold owns the shape it grows: a key list is copied
/// only while it is still shared with the value it started from, so N writes
/// copy it once, not N times.
pub(super) fn extend_in_place(
    shape: &mut InferredType,
    hops: &[WriteKey],
    vtype: Option<&InferredType>,
    widen: bool,
) -> bool {
    let Some((first, rest)) = hops.split_first() else { return false };
    match shape {
        InferredType::HashWithKeys { keys, open } => {
            let WriteKey::Hash(k) = first else { return open_shape(open) };
            let Some(i) = keys.iter().position(|(name, _)| name == k) else {
                if widen {
                    return open_shape(open);
                }
                keys.to_mut().push((k.to_string(), vivify(rest, vtype).map(Box::new)));
                return true;
            };
            if rest.is_empty() {
                if widen {
                    return open_shape(open);
                }
                if vtype.is_none() || keys[i].1.as_deref() == vtype {
                    return false;
                }
                keys.to_mut()[i].1 = vtype.cloned().map(Box::new);
                return true;
            }
            if keys[i].1.is_none() {
                return false;
            }
            // Grown where it stands: a chain of writes under one key copies
            // the inner list once, not once per write.
            let inner = keys.to_mut()[i].1.as_deref_mut().expect("checked above");
            extend_in_place(inner, rest, vtype, widen)
        }
        // A tuple slot: only the sound moves, retyping an in-bounds slot or
        // appending at exactly `len`. A write past `len` leaves holes, so the
        // tuple degrades to `ArrayRef`. A tuple has no open flag to widen
        // into, so a conditional or dynamic write changes nothing.
        InferredType::Sequence(elems) => {
            let WriteKey::Index(i) = first else { return false };
            if widen || *i < 0 {
                return false;
            }
            let i = *i as usize;
            if i > elems.len() {
                *shape = InferredType::ArrayRef;
                return true;
            }
            if !rest.is_empty() {
                if i == elems.len() {
                    elems.push(vivify(rest, vtype).unwrap_or(InferredType::Unknown));
                    return true;
                }
                return extend_in_place(&mut elems[i], rest, vtype, widen);
            }
            match (i.cmp(&elems.len()), vtype) {
                (std::cmp::Ordering::Less, None) => false,
                (std::cmp::Ordering::Less, Some(vt)) if elems[i] == *vt => false,
                (std::cmp::Ordering::Less, Some(vt)) => {
                    elems[i] = vt.clone();
                    true
                }
                (_, vt) => {
                    elems.push(vt.cloned().unwrap_or(InferredType::Unknown));
                    true
                }
            }
        }
        _ => false,
    }
}

fn open_shape(open: &mut bool) -> bool {
    !std::mem::replace(open, true)
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
fn vivify(hops: &[WriteKey], vtype: Option<&InferredType>) -> Option<InferredType> {
    use crate::model::file_analysis::SharedKeys;
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
