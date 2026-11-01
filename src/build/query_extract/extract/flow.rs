//! `@flow.*`, `@expr.*`, `@obs.*`, `@domain.*`, `@shape.*`: values — where
//! they come from, what an expression evaluates to, and how a use observes
//! its operand. Lowered to type witnesses; the flow edges stay as the
//! provenance tier above them.

use super::*;
use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
use std::collections::{BTreeMap, HashMap};

/// A `@flow.target` as the walk found it.
struct Target {
    name: String,
    /// The scopes it binds in.
    scopes: Vec<ScopeId>,
    /// Where the write lands (`written_at`).
    at: Point,
    /// The target token's own start.
    token: Point,
    /// The class a `@type.enclosing` annotation names: the one in force
    /// where it is written.
    enclosing: Option<String>,
}

#[derive(Default)]
pub(super) struct State {
    /// Each `@flow.target`'s binding, by its match: what only the walk at
    /// it knows.
    flow_targets: BTreeMap<usize, Target>,
    /// Rebind shapes with no inflowing value (loop vars: `for x in …`,
    /// `for (auto x : …)`) — they mint a `Rebind` FlowEdge so the narrowing
    /// cutoff sees them, exactly like Perl's `foreach` var.
    pub(super) flow_rebinds: Vec<(String, ScopeId, Point, Point)>,
    /// Usage-site evidence awaiting the rebind cutoff, which reads the
    /// finished flow edges (`narrowing::finish`): the observed name, its
    /// scope, the observation, and its region before the cutoff.
    pub(super) observations: Vec<(String, ScopeId, crate::model::witnesses::TypeObservation, Span)>,
}

/// Where a target event's write lands. A `@flow.assign` match writes at its
/// assignment's end, after every read the assignment makes, and resets the
/// variable there (`docs/adr/flow-narrowing.md`); any other target is
/// bound at its own token.
fn written_at(ad: &Adorned, out: &mut SkeletonAnalysis, name: String, scope: ScopeId, e: &Event) -> Point {
    let Some(at) = ad.in_match(e.match_id, Capture::Flow(FlowCap::Assign)).map(|a| a.end) else { return e.start };
    out.witnesses.push(Witness {
        attachment: WitnessAttachment::Variable { name, scope },
        source: WitnessSource::Builder(crate::model::witnesses::RESET_SOURCE.into()),
        payload: WitnessPayload::Reset,
        span: Span { start: at, end: at },
    });
    at
}

fn skeleton_witness(attachment: WitnessAttachment, source: &str, payload: WitnessPayload, span: Span) -> Witness {
    Witness { attachment, source: WitnessSource::Builder(source.into()), payload, span }
}

pub(super) fn handle(st: &mut ExtractState, ix: usize, e: &Event) {
    let pack = st.pack;
    let ad = st.adorned;
    let cur_scope = st.cur_scope;
    let target_scopes = match e.cap {
        Capture::Flow(FlowCap::Target) => super::defs::binding_scopes(st, e),
        _ => Vec::new(),
    };
    let f = &mut st.flow;
    let out = &mut st.out;
    match e.cap {
        Capture::Expr(ExprCap::Lit(lit)) => {
            let span = e.span();
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
            let (name, ref_span) = match ad.in_match(e.match_id, Capture::Expr(ExprCap::ReadVarName)) {
                Some(token) => ((pack.shape_name)(&token.text), token.span()),
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
            let name = (pack.shape_name)(&e.text);
            let mut at = e.start;
            for &scope in &target_scopes {
                at = written_at(ad, out, name.clone(), scope, e);
            }
            // Record the declared-type text keyed by (var, DECLARING scope)
            // for the token-less optional-engagement narrowing. cur_scope is
            // only known here (scopes mint during this walk).
            let mut enclosing = None;
            if let Some(annot) = ad.in_match(e.match_id, Capture::TypeAnnot) {
                for &scope in &target_scopes {
                    st.narrowing.annot_text_by_var.insert(((pack.shape_name)(&e.text), scope), annot.text.clone());
                }
                if super::defs::names_enclosing(ad, annot) {
                    enclosing = st.package.clone();
                }
            }
            let target = Target { name, scopes: target_scopes, at, token: e.start, enclosing };
            f.flow_targets.insert(e.match_id, target);
        }
        Capture::Flow(FlowCap::Rebind) => {
            let at = written_at(ad, out, (pack.shape_name)(&e.text), cur_scope, e);
            f.flow_rebinds.push(((pack.shape_name)(&e.text), cur_scope, at, e.start));
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
            // site. The value is its match's `@domain.value`; its enum
            // resolves cross-file at query time. The span is the SLOT's, so a
            // find-references on the enum surfaces the field's own uses.
            // Only an identifier-shaped operand can name an enumerator, so
            // anything else (a literal, arithmetic, a call — the capture is
            // ungated) is the empty sentinel: it stays a SITE
            // (counter-evidence in the coherence vote's denominator) without
            // persisting arbitrary expression text.
            if let Some(value) = ad.in_match(e.match_id, Capture::Domain(DomainCap::Value)) {
                out.domain_sites.push(crate::model::file_analysis::DomainSite {
                    slot: (pack.shape_name)(&e.text),
                    value: if is_identifier_text(&value.text) { value.text.clone() } else { String::new() },
                    slot_span: e.span(),
                });
            }
        }
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
            // Arms nest, so the innermost around the site is the smallest
            // holding it; an arm that opens outside the scope holds the
            // whole scope, and so does every arm beneath it.
            let end = ad
                .around(e)
                .map(|a| &ad.events()[a])
                .filter(|a| a.cap == Capture::Arm)
                .min_by_key(|a| ad.width(a))
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
            let in_match = |cap: Capture| ad.in_match(e.match_id, cap);
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
                let args = super::members::call_args(ad, e.match_id);
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
        Capture::Expr(ExprCap::Wrapper) => {
            // A wrapper's value is its content's: an edge, so a source or
            // receiver written in parens reads what it wraps.
            if let Some(inner) = ad.in_match(e.match_id, Capture::Expr(ExprCap::WrapperInner)) {
                out.witnesses.push(skeleton_witness(
                    WitnessAttachment::Expr(e.span()),
                    "skeleton",
                    WitnessPayload::Edge(WitnessAttachment::Expr(inner.span())),
                    e.span(),
                ));
            }
        }
        Capture::Expr(ExprCap::Member) => {
            // A member read's value is the receiver's slot of that name:
            // a `ValueHop` dispatched on the receiver's type at query time,
            // so a chain (`a.b.c`) hops through each receiver's own `Expr`.
            let in_match = |cap: Capture| ad.in_match(e.match_id, cap);
            if let (Some(name), Some(recv)) =
                (in_match(Capture::Ref(RefCap::MemberValue)), in_match(Capture::Member(MemberCap::Recv)))
            {
                let span = e.span();
                let member = (pack.shape_name)(&name.text);
                st.minted[ix].projects =
                    Some(Box::new(MemberRead { receiver: recv.span(), member: member.clone(), token: name.span() }));
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
/// `@expr.shape` only on a constructor call, so every span is one; its keys
/// are the `@shape.key`s of every match that tagged that node a shape.
pub(super) fn emit_keyed_shapes(st: &mut ExtractState) {
    let ad = st.adorned;
    let shape = Capture::Expr(ExprCap::Shape);
    let mut seen = std::collections::HashSet::new();
    for e in ad.events() {
        if e.cap != shape || !seen.insert(e.site) {
            continue;
        }
        let mut key_ixs: Vec<usize> = ad
            .on(e)
            .filter(|s| s.cap == shape)
            .flat_map(|s| ad.of_match_ixs(s.match_id))
            .filter(|&k| ad.events()[k].cap == Capture::Shape(ShapeCap::Key))
            .collect();
        key_ixs.sort_unstable();
        key_ixs.dedup();
        let mut keys: Vec<(String, Option<Box<InferredType>>)> =
            key_ixs.iter().map(|&k| (ad.events()[k].text.clone(), None)).collect();
        keys.dedup_by(|a, b| a.0 == b.0);
        let span = e.span();
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
pub(super) fn finish(st: &mut ExtractState) {
    let ad = st.adorned;
    let f = &mut st.flow;
    let out = &mut st.out;
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
    // A write of a name some earlier def declares replaces its value.
    let reassigned: std::collections::HashSet<(&str, Point)> =
        out.var_writes.iter().map(|(n, _, sp)| (n.as_str(), sp.start)).collect();
    let reassigns = |name: &str, token: &Point| reassigned.contains(&(name, *token));
    let mut edges = Vec::new();
    // Match-id order (deterministic) — two captures targeting the same
    // `Variable{name, scope}` slot land witnesses in this order, and the
    // latest wins.
    let targets = f.flow_targets.iter().flat_map(|(&mid, t)| t.scopes.iter().map(move |scope| (mid, t, scope)));
    for (mid, Target { name, at, token, enclosing, .. }, scope) in targets {
        let var = WitnessAttachment::Variable { name: name.clone(), scope: *scope };
        // Class-wide extent for a data member; the sequential decl point for a
        // local (see `field_scope_span`).
        let annot_span = field_scope_span
            .get(&(name.clone(), *scope))
            .copied()
            .unwrap_or(Span { start: *at, end: *at });
        if let Some(annot) = ad.in_match(mid, Capture::TypeAnnot).map(|a| &a.text) {
            // A class-shaped declared type edges into the alias graph (it may
            // be a typedef — `U16 x;` where `typedef unsigned short U16`);
            // primitives stay leaves; `None` (auto/void) defers to the flow
            // edge as before. `TypeName` chases the typedef or falls back to
            // the same `ClassName`, so a plain struct/class is unchanged.
            // A type naming the class it is written in is that class, already
            // an identity; any other spelling resolves at its site.
            let annot_type = match enclosing {
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
        if let Some(target_span) = source_value_span(ad, mid) {
            // Mint a value-flow edge (cpp init is `Whole`); the witness is its
            // lowering, so type inference sees the same `Variable → Edge(Expr)`
            // it always did — now with the source span kept for provenance.
            edges.push(crate::model::file_analysis::FlowEdge {
                target_name: name.clone(),
                target_scope: *scope,
                target_at: *at,
                source: target_span,
                extraction: crate::model::file_analysis::Extraction::Whole,
                reassigns: reassigns(name, token),
            });
        }
    }
    // A place is whatever its own read projects. A member read's target is
    // decided by its receiver's type, which only the assembled analysis
    // answers (`skeleton::settle_member_writes`). A name's place is its
    // binding, which `@flow.target` writes and rebinds; a place no read
    // projects (a call, a literal) takes nothing. The member token of a
    // projected place is written, not read, whether or not a value flows in
    // (`$o->n += 1`).
    for place in ad.by_match().filter(|e| e.cap == Capture::Flow(FlowCap::Place)) {
        let Some(read) = ad.on_ixs(place).filter_map(|i| st.minted[i].projects.as_deref()).last() else { continue };
        out.write_sites.push(read.token);
        if let Some(value) = source_value_span(ad, place.match_id) {
            out.member_writes.push(crate::build::query_extract::MemberWrite {
                receiver: read.receiver,
                member: read.member.clone(),
                value,
            });
        }
    }
    // Bind-shape rebinds (loop vars): no inflowing value, recorded for the
    // narrowing cutoff (`Rebind` lowers to nothing — provenance only).
    for (name, scope, at, token) in std::mem::take(&mut f.flow_rebinds) {
        edges.push(crate::model::file_analysis::FlowEdge {
            reassigns: reassigns(&name, &token),
            target_name: name,
            target_scope: scope,
            target_at: at,
            source: Span { start: at, end: at },
            extraction: crate::model::file_analysis::Extraction::Rebind,
        });
    }
    out.flow_edges.extend(edges);
    // Lower the value-flow edges to type-tier witnesses (the bag is canonical
    // for types; the edges are the provenance tier above it).
    for fe in &out.flow_edges {
        if let Some(w) = fe.lower_to_witness() {
            out.witnesses.push(w);
        }
    }
}

/// The span whose witness is match `mid`'s inflowing value: its
/// `@flow.source` node. A wrapper (`(5)`) carries its content's value as an
/// edge (`@expr.wrapper`), and a value merely inside the source — an
/// argument, an operand — is not its value.
fn source_value_span(ad: &Adorned, mid: usize) -> Option<Span> {
    ad.in_match(mid, Capture::Flow(FlowCap::Source)).map(Event::span)
}

/// A bare identifier lexeme — the only shape that can name an enumerator.
/// Pure string test (no node-kind probe) so every language's capture text
/// routes through the same rule.
fn is_identifier_text(s: &str) -> bool {
    !s.is_empty()
        && !s.as_bytes()[0].is_ascii_digit()
        && s.bytes().all(|b| b == b'_' || b.is_ascii_alphanumeric())
}
