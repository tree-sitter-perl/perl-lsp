//! `@flow.*`, `@expr.*`, `@obs.*`, `@domain.*`, `@shape.*`: values — where
//! they come from, what an expression evaluates to, and how a use observes
//! its operand. Lowered to type witnesses; the flow edges stay as the
//! provenance tier above them.

use super::*;
use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct State {
    /// flow.assign joins: match_id → (target name, the scopes it binds
    /// in, its site)
    flow_targets: HashMap<usize, (String, Vec<ScopeId>, Point)>,
    flow_sources: HashMap<usize, Span>,
    /// Writes to a place: match_id → the place's span.
    flow_places: HashMap<usize, Span>,
    /// Member reads by byte range → (receiver span, member): what a
    /// place's own read projects, so a write lands where the read looks.
    member_reads: HashMap<(usize, usize), (Span, String)>,
    /// Rebind shapes with no inflowing value (loop vars: `for x in …`,
    /// `for (auto x : …)`) — they mint a `Rebind` FlowEdge so the narrowing
    /// cutoff sees them, exactly like Perl's `foreach` var.
    pub(super) flow_rebinds: Vec<(String, ScopeId, Point)>,
    /// `@type.annot` — a declaration's type text and span, joined per match.
    annots: HashMap<usize, (String, Point, Point)>,
    /// The class each `@type.enclosing` annotation names: the one in force
    /// where it is written, known only once the walk reaches it.
    enclosing_annots: HashMap<usize, String>,
    /// `@expr.read.var.name` — a read's name token, joined to its node.
    read_names: HashMap<usize, (String, Span)>,
    /// expr-literal spans, for narrowing an Edge target onto the actual
    /// literal when the rhs node wraps it.
    lit_spans: Vec<(usize, usize, Span)>,
    /// keyed-shape collection: keys grouped per @expr.shape span
    shape_spans: Vec<(usize, usize, Span)>,
    shape_keys: Vec<(usize, usize, String)>,
    /// `@domain.value` — the operand a field slot is compared/assigned
    /// against. Joined to its `@domain.slot` by match_id (the slot event
    /// pushes the site); the value's own enum resolves cross-file later. Only
    /// an identifier-shaped operand can name an enumerator, so anything else
    /// (a literal, arithmetic, a call — the capture is ungated) is stored as
    /// the empty sentinel: it stays a SITE (counter-evidence in the coherence
    /// vote's denominator) without persisting arbitrary expression text.
    domain_value_by_match: HashMap<usize, String>,
    /// `@arm` spans not yet known closed, in start order. Events arrive by
    /// start, so every arm around an observation is here when it arrives,
    /// and an arm that ends before an event's start holds nothing later.
    open_arms: Vec<Span>,
    /// Usage-site evidence awaiting the rebind cutoff, which reads the
    /// finished flow edges (`narrowing::finish`): the observed name, its
    /// scope, the observation, and its region before the cutoff.
    pub(super) observations: Vec<(String, ScopeId, crate::model::witnesses::TypeObservation, Span)>,
}

pub(super) fn collect(st: &mut ExtractState, e: &Event) {
    match e.cap {
        Capture::TypeAnnot => {
            st.flow.annots.insert(e.match_id, (e.text.clone(), e.start, e.end));
        }
        Capture::Expr(ExprCap::ReadVarName) => {
            st.flow.read_names.insert(e.match_id, (e.text.clone(), e.span()));
        }
        Capture::Domain(DomainCap::Value) => {
            let v = if is_identifier_text(&e.text) { e.text.clone() } else { String::new() };
            st.flow.domain_value_by_match.insert(e.match_id, v);
        }
        _ => {}
    }
}

fn skeleton_witness(attachment: WitnessAttachment, source: &str, payload: WitnessPayload, span: Span) -> Witness {
    Witness { attachment, source: WitnessSource::Builder(source.into()), payload, span }
}

pub(super) fn handle(st: &mut ExtractState, e: &Event, events: &[Event]) {
    let pack = st.pack;
    let cur_scope = st.cur_scope;
    let bind_scope = st.bind_scope();
    // A target that is the name defs in its match declare binds where each
    // of them binds; any other binds in the innermost scope, or the def
    // scope of the header it is written in (an annotated parameter).
    let mut target_scopes: Vec<ScopeId> = st.defs.declared_by(e).iter().map(|&kind| st.def_scope(kind)).collect();
    target_scopes.dedup();
    if target_scopes.is_empty() {
        target_scopes.push(bind_scope);
    }
    let f = &mut st.flow;
    let out = &mut st.out;
    match e.cap {
        Capture::Expr(ExprCap::Lit(lit)) => {
            let span = e.span();
            f.lit_spans.push((e.start_byte, e.end_byte, span));
            out.witnesses.push(skeleton_witness(
                WitnessAttachment::Expr(span),
                "skeleton",
                WitnessPayload::InferredType(lit.value_type()),
                span,
            ));
        }
        Capture::Expr(ExprCap::ReadVar) => {
            // a variable READ is an edge: Expr(span) resolves to
            // whatever the Variable resolves to — same shape the
            // builder's emit_expr_witness uses.
            let span = e.span();
            // …and a candidate local-var reference, resolved to its
            // declaration in into_file_analysis (goto-def + hover). Where the
            // document names the read's name token, that token is the
            // reference and the name; the node stays the value.
            let (name, ref_span) = match f.read_names.get(&e.match_id) {
                Some((text, token)) => ((pack.shape_name)(text), *token),
                None => ((pack.shape_name)(&e.text), span),
            };
            out.var_reads.push((name.clone(), cur_scope, ref_span));
            out.witnesses.push(skeleton_witness(
                WitnessAttachment::Expr(span),
                "skeleton",
                WitnessPayload::Edge(WitnessAttachment::Variable {
                    name,
                    scope: cur_scope,
                }),
                span,
            ));
        }
        Capture::Receiver => {
            out.receivers.push((cur_scope, e.span()));
        }
        Capture::Expr(ExprCap::ReturnValue) => {
            // The returned expression's own general-rule witness (literal
            // / var-read / member / call — whichever matched this same
            // node) already carries its type; this just records the site
            // (scope + span) so `into_file_analysis` can chain the enclosing
            // function's `Symbol` onto it when undeclared.
            out.return_sites.push((cur_scope, e.span()));
        }
        Capture::Flow(FlowCap::Target) => {
            // Record the declared-type text keyed by (var, DECLARING scope)
            // for the token-less optional-engagement narrowing. cur_scope is
            // only known here (scopes mint during this walk); the type-annot
            // half was pre-collected (it precedes the declarator in source).
            if let Some((annot, s, en)) = f.annots.get(&e.match_id) {
                for &scope in &target_scopes {
                    st.narrowing.annot_text_by_var.insert(((pack.shape_name)(&e.text), scope), annot.clone());
                }
                if let Some(class) = st.package.as_ref().filter(|_| st.defs.names_enclosing(*s, *en)) {
                    f.enclosing_annots.insert(e.match_id, class.clone());
                }
            }
            f.flow_targets.insert(e.match_id, ((pack.shape_name)(&e.text), target_scopes, e.start));
        }
        Capture::Flow(FlowCap::Rebind) => {
            f.flow_rebinds.push(((pack.shape_name)(&e.text), cur_scope, e.start));
        }
        Capture::Flow(FlowCap::Source) => {
            f.flow_sources.insert(e.match_id, e.span());
        }
        Capture::Flow(FlowCap::Place) => {
            f.flow_places.insert(e.match_id, e.span());
        }
        Capture::AnonAggMember => {
            // A field typed by an anonymous aggregate: its members are
            // flattened onto the enclosing named container, so the field's
            // own type IS that container (the anon hop is identity) —
            // `u->data.ping` types `u->data` as U and finds `ping` there.
            // TypeName (not ClassName) so a typedef'd container chases.
            if let Some(owner) = &st.package {
                out.witnesses.push(skeleton_witness(
                    WitnessAttachment::Variable { name: (pack.shape_name)(&e.text), scope: cur_scope },
                    "skeleton-anon-agg",
                    WitnessPayload::Edge(WitnessAttachment::TypeName(owner.clone())),
                    Span { start: e.start, end: e.start },
                ));
            }
        }
        Capture::Domain(DomainCap::Slot) => {
            // A field slot used against a typed value — one domain-typing
            // site. The value is joined by match_id; its enum resolves
            // cross-file at query time. The span is the SLOT's, so a
            // find-references on the enum surfaces the field's own uses.
            if let Some(value) = f.domain_value_by_match.get(&e.match_id) {
                out.domain_sites.push(crate::model::file_analysis::DomainSite {
                    slot: (pack.shape_name)(&e.text),
                    value: value.clone(),
                    slot_span: e.span(),
                });
            }
        }
        Capture::Expr(ExprCap::Shape) => {
            f.shape_spans.push((e.start_byte, e.end_byte, e.span()));
        }
        Capture::Shape(ShapeCap::Key) => {
            if let Some(range) = byte_range_of(events, e.match_id, Capture::Expr(ExprCap::Shape)) {
                f.shape_keys.push((range.0, range.1, e.text.clone()));
            }
        }
        Capture::Arm => f.open_arms.push(e.span()),
        Capture::Obs(obs) => {
            // Usage-site evidence: a mono-typed operator observes
            // its operand. Same Observation payloads the Perl
            // walker emits; the fold is the production one. It holds from
            // the site to the end of the innermost `@arm` around it inside
            // its scope, else to the end of the scope, cut at the next
            // rebind like a guard's narrowing.
            let site = e.span();
            let scope = out.scopes[cur_scope.0 as usize].span;
            let before = |a: Point, b: Point| (a.row, a.column) < (b.row, b.column);
            while f.open_arms.last().is_some_and(|a| before(a.end, site.start)) {
                f.open_arms.pop();
            }
            // Arms nest, so the innermost around the site is the last one
            // holding it; an arm that opens outside the scope holds the
            // whole scope, and so does every arm beneath it.
            let end = f
                .open_arms
                .iter()
                .rev()
                .find(|a| !before(site.start, a.start) && !before(a.end, site.end))
                .filter(|a| !before(a.start, scope.start))
                .map_or(scope.end, |a| a.end);
            f.observations.push((
                (pack.shape_name)(&e.text),
                cur_scope,
                obs.observation(),
                Span { start: site.start, end },
            ));
        }
        Capture::Expr(ExprCap::Call) => {
            // A call's VALUE is an `Invoke` of its callee, minted in
            // `into_file_analysis`, where the symbol table resolves a free
            // callee (`docs/adr/calls-are-invokes.md`). Record the site and
            // mark its span as value-producing so an enclosing `auto x =
            // f(..)` flow edge targets it. A member call (`recv.m(..)`)
            // carries its receiver, the `@member.recv` of the same match.
            let in_match = |cap: Capture| events.iter().find(|x| x.match_id == e.match_id && x.cap == cap);
            let callee = in_match(Capture::Ref(RefCap::Call))
                .map(|x| (x, None))
                .or_else(|| {
                    let name = in_match(Capture::Ref(RefCap::MemberCall))?;
                    let recv = in_match(Capture::Member(MemberCap::Recv))?;
                    Some((name, Some(recv.span())))
                });
            if let Some((token, receiver)) = callee {
                let (callee, callee_at) = ((pack.shape_name)(&token.text), token.start);
                let span = e.span();
                f.lit_spans.push((e.start_byte, e.end_byte, span));
                let args = st.members.call_args(e.match_id);
                out.call_sites.push(crate::build::query_extract::CallSite {
                    span,
                    callee,
                    callee_at,
                    receiver,
                    scope: cur_scope,
                    args,
                });
            }
        }
        Capture::Expr(ExprCap::Member) => {
            // A member read's value is the receiver's slot of that name:
            // a `ValueHop` dispatched on the receiver's type at query time,
            // so a chain (`a.b.c`) hops through each receiver's own `Expr`.
            let in_match = |cap: Capture| events.iter().find(|x| x.match_id == e.match_id && x.cap == cap);
            if let (Some(name), Some(recv)) =
                (in_match(Capture::Ref(RefCap::MemberValue)), in_match(Capture::Member(MemberCap::Recv)))
            {
                let span = e.span();
                let member = (pack.shape_name)(&name.text);
                f.lit_spans.push((e.start_byte, e.end_byte, span));
                f.member_reads.insert((e.start_byte, e.end_byte), (recv.span(), member.clone()));
                out.witnesses.push(skeleton_witness(
                    WitnessAttachment::Expr(span),
                    "skeleton-member",
                    WitnessPayload::Projected {
                        base: WitnessAttachment::Expr(recv.span()),
                        step: crate::model::witnesses::ProjectionStep::ValueHop { member },
                    },
                    span,
                ));
            }
        }
        Capture::NamedReceiver(names) => {
            // Which class the token names is answered once the use-map and
            // the callables' owners are in (`into_file_analysis`).
            out.receiver_sites.push(crate::build::query_extract::ReceiverSite {
                span: e.span(),
                scope: cur_scope,
                names,
                text: e.text.clone(),
            });
        }
        // The assignment node anchors its pattern; its target and source
        // carry the meaning.
        Capture::Flow(FlowCap::Assign) => {}
        _ => {}
    }
}

/// Keyed shapes → `HashWithKeys` witnesses. The document captures
/// `@expr.shape` only on a constructor call, so every span is one.
pub(super) fn emit_keyed_shapes(st: &mut ExtractState) {
    let f = &mut st.flow;
    let mut seen_spans: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    for &(sb, eb, span) in &f.shape_spans {
        if !seen_spans.insert((sb, eb)) {
            continue;
        }
        let mut keys: Vec<(String, Option<Box<InferredType>>)> = f
            .shape_keys
            .iter()
            .filter(|&&(s2, e2, _)| s2 == sb && e2 == eb)
            .map(|(_, _, k)| (k.clone(), None))
            .collect();
        keys.dedup_by(|a, b| a.0 == b.0);
        f.lit_spans.push((sb, eb, span));
        st.out.witnesses.push(skeleton_witness(
            WitnessAttachment::Expr(span),
            "skeleton-shape",
            WitnessPayload::InferredType(InferredType::HashWithKeys {
                keys: crate::model::file_analysis::SharedKeys::new(keys),
                open: false,
            }),
            span,
        ));
    }
}

/// Join flow captures into value-flow edges, then lower the edges to
/// Variable witnesses.
pub(super) fn finish(st: &mut ExtractState, events: &[Event]) {
    let f = &mut st.flow;
    let out = &mut st.out;
    // Match-id order (deterministic) — two captures targeting the same
    // `Variable{name, scope}` slot would otherwise land witnesses in
    // HashMap-iteration order, flipping the latest-wins winner per process.
    let mut flow_mids: Vec<&usize> = f.flow_targets.keys().collect();
    flow_mids.sort_unstable();
    // A class/struct DATA MEMBER is visible throughout its class body
    // regardless of declaration order (C++ member lookup is not sequential:
    // a method reads a field declared later in a `private:` section below
    // it). The type-witness temporal filter is position-based — it admits a
    // witness only at/after its span — so a zero-width witness pinned to the
    // field's decl point would be REJECTED for every read textually above it.
    // Give the field's declared-type witness its class-body scope's span so
    // "this type holds class-wide" is what the filter sees. Locals keep their
    // decl-point span (flow narrowing is sequential and correct for them).
    // Scopes are pushed in id order (`id = ScopeId(scopes.len())`), so a
    // ScopeId indexes its own scope directly.
    let field_scope_span: HashMap<(String, ScopeId), Span> = out
        .symbols
        .iter()
        .filter(|s| s.kind == DefCap::Field)
        .filter_map(|s| out.scopes.get(s.scope.0 as usize).map(|sc| ((s.name.clone(), s.scope), sc.span)))
        .collect();
    for (mid, scope) in flow_mids.into_iter().flat_map(|mid| f.flow_targets[mid].1.iter().map(move |s| (mid, s))) {
        let (name, _, at) = &f.flow_targets[mid];
        let var = WitnessAttachment::Variable { name: name.clone(), scope: *scope };
        // Class-wide extent for a data member; the sequential decl point for a
        // local (see `field_scope_span`).
        let annot_span = field_scope_span
            .get(&(name.clone(), *scope))
            .copied()
            .unwrap_or(Span { start: *at, end: *at });
        if let Some((annot, ..)) = f.annots.get(mid) {
            // A class-shaped declared type edges into the alias graph (it may
            // be a typedef — `U16 x;` where `typedef unsigned short U16`);
            // primitives stay leaves; `None` (auto/void) defers to the flow
            // edge as before. `TypeName` chases the typedef or falls back to
            // the same `ClassName`, so a plain struct/class is unchanged.
            // A type naming the class it is written in is that class, already
            // an identity; any other spelling resolves at its site.
            let annot_type = match f.enclosing_annots.get(mid) {
                Some(class) => Some(InferredType::ClassName(class.clone())),
                None => (st.pack.annot_type)(annot).map(|t| written_type_at(out, &st.pack.names, t, annot_span.start)),
            };
            if let (Some(t), true) = (&annot_type, field_scope_span.contains_key(&(name.clone(), *scope))) {
                out.field_types.push((name.clone(), *scope, t.clone()));
            }
            let payload = match annot_type {
                Some(InferredType::ClassName(cn)) => Some(WitnessPayload::Edge(WitnessAttachment::TypeName(cn))),
                Some(t) => Some(WitnessPayload::InferredType(t)),
                None => None,
            };
            if let Some(payload) = payload {
                out.witnesses.push(Witness {
                    attachment: var.clone(),
                    source: WitnessSource::Annotation(crate::model::witnesses::AnnotationKind::Declared),
                    payload,
                    span: annot_span,
                });
            }
        }
        if let Some(target_span) = source_value_span(f, events, *mid) {
            // Mint a value-flow edge (cpp init is `Whole`); the witness is its
            // lowering, so type inference sees the same `Variable → Edge(Expr)`
            // it always did — now with the source span kept for provenance.
            out.flow_edges.push(crate::model::file_analysis::FlowEdge {
                target_name: name.clone(),
                target_scope: *scope,
                target_at: *at,
                source: target_span,
                extraction: crate::model::file_analysis::Extraction::Whole,
                reassigns: false,
            });
        }
    }
    // A place is whatever its own read projects. A member read's target is
    // decided by its receiver's type, which only the assembled analysis
    // answers (`skeleton::settle_member_writes`). A name's place is its
    // binding, which `@flow.target` writes and rebinds; a place no read
    // projects (a call, a literal) takes nothing.
    let mut place_mids: Vec<&usize> = f.flow_places.keys().collect();
    place_mids.sort_unstable();
    for mid in place_mids {
        let read = byte_range_of(events, *mid, Capture::Flow(FlowCap::Place)).and_then(|r| f.member_reads.get(&r));
        if let (Some((receiver, member)), Some(value)) = (read, source_value_span(f, events, *mid)) {
            out.member_writes.push(crate::build::query_extract::MemberWrite {
                receiver: *receiver,
                member: member.clone(),
                value,
            });
        }
    }
    // Bind-shape rebinds (loop vars): no inflowing value, recorded for the
    // narrowing cutoff (`Rebind` lowers to nothing — provenance only).
    for (name, scope, at) in std::mem::take(&mut f.flow_rebinds) {
        out.flow_edges.push(crate::model::file_analysis::FlowEdge {
            target_name: name,
            target_scope: scope,
            target_at: at,
            source: Span { start: at, end: at },
            extraction: crate::model::file_analysis::Extraction::Rebind,
            reassigns: false,
        });
    }
    // Lower the value-flow edges to type-tier witnesses (the bag is canonical
    // for types; the edges are the provenance tier above it).
    for fe in &out.flow_edges {
        if let Some(w) = fe.lower_to_witness() {
            out.witnesses.push(w);
        }
    }
}

/// The span whose witness is match `mid`'s inflowing value: its
/// `@flow.source`, narrowed onto the outermost literal the source wraps
/// when the source node itself carries no witness (paren wrappers).
fn source_value_span(f: &State, events: &[Event], mid: usize) -> Option<Span> {
    let src_span = f.flow_sources.get(&mid)?;
    let src_bytes = byte_range_of(events, mid, Capture::Flow(FlowCap::Source));
    Some(
        f.lit_spans
            .iter()
            .filter(|&&(s, en, _)| src_bytes.is_some_and(|(ss, se)| s >= ss && en <= se))
            .max_by_key(|&&(s, en, _)| en - s)
            .map(|&(_, _, sp)| sp)
            .filter(|sp| sp != src_span)
            .unwrap_or(*src_span),
    )
}

/// A bare identifier lexeme — the only shape that can name an enumerator.
/// Pure string test (no node-kind probe) so every language's capture text
/// routes through the same rule.
fn is_identifier_text(s: &str) -> bool {
    !s.is_empty()
        && !s.as_bytes()[0].is_ascii_digit()
        && s.bytes().all(|b| b == b'_' || b.is_ascii_alphanumeric())
}
