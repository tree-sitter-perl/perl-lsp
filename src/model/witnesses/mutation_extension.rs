//! The mutation-extension pass: a write through a variable extends the
//! shape it lands in (`docs/adr/structural-shapes.md`).
//!
//! A write is a DELTA (`WitnessPayload::Extend`): the key it adds and the
//! type of the value it writes. The shape a read sees is the value standing
//! at each write with that write's delta laid over it, folded when the place
//! is read (`ReducerRegistry::lower_extensions`) — never a copy of the grown
//! shape per write, which is N² keys for N writes to one hash.

use super::*;
use crate::model::file_analysis::{KeyWrite, WriteKey};

/// Push one `Extend` delta per `KeyWrite` that lands in a shape (a hash or
/// tuple standing at the write, or an object slot's value). For each:
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
/// `Member{class, s, Holds}`; every other delta goes on the variable's place, with
/// a zero-width span at the write — invisible to reads before it.
///
/// `bindings[i]` is `key_writes[i]`'s variable binding and its home scope,
/// resolved by the caller's binder before the bag is borrowed.
///
/// Re-emittable: the fold passes `clear = true` (clear-and-emit per
/// iteration, tag `mutation_extension`) since a write's value and its
/// object-slot route read types the fold is still settling. Enrichment
/// passes `false` — post-finalize the bag is append-only — so values and
/// routes that imported types sharpen land as later deltas at the same
/// sites, which re-type what the build's deltas said.
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
    // Per-var doc order: each write's value is read with the earlier
    // writes' deltas already in the bag and its own not yet, as Perl
    // evaluates a right-hand side before it stores.
    let mut writes: Vec<(&KeyWrite, &(crate::model::file_analysis::Binding, ScopeId))> =
        key_writes.iter().zip(bindings).collect();
    writes.sort_by_key(|(w, _)| (&w.var_text, w.span.start));
    let mut routes = SlotRoutes::default();
    for (w, (binding, home)) in writes {
        let place = crate::model::file_analysis::Place::variable(binding.clone());
        // A delta only grows a shape, so a write where none stands — an
        // object's own slot, a scalar, a value nothing types — has nothing
        // to say to the place, and its delta would be dead weight in every
        // bag that carries it. A nested write through an object's slot
        // lands in the slot instead.
        let owner = match (routes.standing_at(bag, ctx, &place, w.span.start), &w.key) {
            (Standing::Shape, _) => None,
            (Standing::Object(class), WriteKey::Hash(_)) if !w.path.is_empty() => Some(class),
            _ => continue,
        };
        // Attach where the value the write extends is bound — the scope a
        // read takes it from — never where the write merely uses it (a
        // block's own `$v->{k}` access would hide the shape from every read
        // after the block). Climbing past the write's own scope means the
        // write may not have run by read time.
        let widen = w.conditional || *home != w.scope;
        let value = w.rhs_span.and_then(|s| rhs_type(bag, ctx, s)).and_then(|t| match w.rhs_slot {
            Some(n) => t.element_at(n).cloned(),
            None => Some(t),
        });
        let (attachment, hops) = match (owner, &w.key) {
            (Some(owner), WriteKey::Hash(slot)) => {
                (WitnessAttachment::holds_of(owner, ctx.member_spaces.values, slot.clone()), w.path.clone())
            }
            _ => (
                WitnessAttachment::Place(Box::new(place)),
                std::iter::once(w.key.clone()).chain(w.path.iter().cloned()).collect(),
            ),
        };
        bag.push(Witness {
            attachment,
            source: WitnessSource::Builder("mutation_extension".into()),
            payload: WitnessPayload::Extend(KeyExtension { hops, value, widen }),
            span: Span { start: w.span.start, end: w.span.start },
        });
    }
}

fn rhs_type(bag: &WitnessBag, ctx: &BagContext, s: Span) -> Option<InferredType> {
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
}

/// What stands at a write: a shape the write grows, an object whose slot a
/// nested write lands in, or neither. A delta never changes which (it grows
/// a shape or does nothing), so the answer moves only where some other
/// witness of the place does — the place is re-read at a segment boundary
/// (`SpanIndex::boundary`), never once per write.
#[derive(Clone)]
enum Standing {
    Shape,
    Object(String),
    Neither,
}

impl Standing {
    fn of(t: Option<InferredType>) -> Standing {
        match t {
            Some(InferredType::HashWithKeys { .. } | InferredType::Sequence(_)) => Standing::Shape,
            Some(t) => t.class_name().map_or(Standing::Neither, |c| Standing::Object(c.to_string())),
            None => Standing::Neither,
        }
    }
}

#[derive(Default)]
struct SlotRoutes {
    current: Option<RouteSegment>,
}

struct RouteSegment {
    place: crate::model::file_analysis::Place,
    others: SpanIndex,
    at: Point,
    standing: Standing,
}

impl SlotRoutes {
    fn standing_at(
        &mut self,
        bag: &WitnessBag,
        ctx: &BagContext,
        place: &crate::model::file_analysis::Place,
        at: Point,
    ) -> Standing {
        let same_place = self.current.as_ref().is_some_and(|seg| seg.place == *place);
        if !same_place {
            let att = WitnessAttachment::Place(Box::new(place.clone()));
            // A use's observation (a deref, a numeric context) says how the
            // value is used, never which class it is, so only a witness that
            // can bind a value moves the route.
            let others = SpanIndex::of(bag.iter_attachment_indexed(&att).map(|(_, w)| w).filter(|w| match &w.payload {
                WitnessPayload::Extend(_) => false,
                WitnessPayload::Observation(_) => w.payload.binds_value(),
                _ => true,
            }));
            let standing = Standing::of(query_variable_place(bag, ctx, place, ReadPoint::position(at)));
            self.current = Some(RouteSegment { place: place.clone(), others, at, standing });
        }
        let seg = self.current.as_mut().expect("set above");
        if same_place && seg.others.boundary(seg.at, at) {
            seg.standing = Standing::of(query_variable_place(bag, ctx, place, ReadPoint::position(at)));
        }
        seg.at = at;
        seg.standing.clone()
    }
}

/// The sites of a place's witnesses other than its deltas, sorted for
/// the two questions `boundary` asks.
pub(super) struct SpanIndex {
    starts: Vec<Point>,
    region_ends: Vec<Point>,
}

impl SpanIndex {
    pub(super) fn of<'a>(others: impl Iterator<Item = &'a Witness>) -> Self {
        let mut starts = Vec::new();
        let mut region_ends = Vec::new();
        for w in others {
            starts.push(w.span.start);
            if w.span.start != w.span.end {
                region_ends.push(w.span.end);
            }
        }
        starts.sort();
        region_ends.sort();
        SpanIndex { starts, region_ends }
    }

    /// Did the value standing at the place move between two of its writes
    /// (`prev` < `at`) other than by the writes themselves? Some other
    /// witness opens in `(prev, at]`, or a region holding at `prev` ends
    /// before `at`. A region holding at both cannot: the write at `prev` is a
    /// point fact after the region opened, and the fold lets that outrank it.
    pub(super) fn boundary(&self, prev: Point, at: Point) -> bool {
        let opens = self.starts.partition_point(|p| *p <= prev) < self.starts.partition_point(|p| *p <= at);
        let closes = self.region_ends.partition_point(|p| *p < prev) < self.region_ends.partition_point(|p| *p < at);
        opens || closes
    }
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
                // An open shape may already hold the key, so what lies below
                // it is not just this write's path.
                keys.to_mut().push((k.to_string(), vivify(rest, vtype, *open).map(Box::new)));
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
                    elems.push(vivify(rest, vtype, false).unwrap_or(InferredType::Unknown));
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
/// it is an `ArrayRef`, which says nothing about elements. `open` opens
/// every vivified hash: the write may land on a value that already stood.
/// TODO(element-typed-array): a tuple is only right while nothing else
/// writes the array; these writes move to an element-typed array when the
/// lattice has one.
pub(super) fn vivify(hops: &[WriteKey], vtype: Option<&InferredType>, open: bool) -> Option<InferredType> {
    use crate::model::file_analysis::SharedKeys;
    let Some((first, rest)) = hops.split_first() else { return vtype.cloned() };
    match first {
        WriteKey::Hash(k) => Some(InferredType::HashWithKeys {
            keys: SharedKeys::new(vec![(k.clone(), vivify(rest, vtype, open).map(Box::new))]),
            open,
        }),
        WriteKey::Index(0) => {
            Some(InferredType::Sequence(vec![vivify(rest, vtype, open).unwrap_or(InferredType::Unknown)]))
        }
        WriteKey::Index(i) if *i > 0 => Some(InferredType::ArrayRef),
        // A computed key still names the container it writes into.
        WriteKey::AnyKey => Some(InferredType::HashWithKeys { keys: SharedKeys::new(Vec::new()), open: true }),
        WriteKey::AnyIndex => Some(InferredType::ArrayRef),
        WriteKey::Index(_) | WriteKey::Unknown => None,
    }
}
