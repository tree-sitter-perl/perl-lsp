//! `@flow.*`, `@expr.*`, `@obs.*`, `@domain.*`, `@shape.*`: values — where
//! they come from, what an expression evaluates to, and how a use observes
//! its operand. Lowered to type witnesses; the flow edges stay as the
//! provenance tier above them.

use super::*;
use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
use std::collections::{BTreeMap, HashMap};

#[derive(Default)]
pub(super) struct State {
    /// Each `@flow.target`'s binding, by its match: the scopes it binds in
    /// are known only while the walk is at it.
    flow_targets: BTreeMap<usize, (String, Vec<ScopeId>, Point)>,
    /// Rebind shapes with no inflowing value (loop vars: `for x in …`,
    /// `for (auto x : …)`) — they mint a `Rebind` FlowEdge so the narrowing
    /// cutoff sees them, exactly like Perl's `foreach` var.
    pub(super) flow_rebinds: Vec<(String, ScopeId, Point)>,
    /// Usage-site evidence awaiting the rebind cutoff, which reads the
    /// finished flow edges (`narrowing::finish`): the observed name, its
    /// scope, the observation, and its region before the cutoff.
    pub(super) observations: Vec<(String, ScopeId, crate::model::witnesses::TypeObservation, Span)>,
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
            st.minted[ix].value = true;
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
            // Record the declared-type text keyed by (var, DECLARING scope)
            // for the token-less optional-engagement narrowing. cur_scope is
            // only known here (scopes mint during this walk).
            if let Some(annot) = ad.in_match(e.match_id, Capture::TypeAnnot) {
                for &scope in &target_scopes {
                    st.narrowing.annot_text_by_var.insert(((pack.shape_name)(&e.text), scope), annot.text.clone());
                }
            }
            f.flow_targets.insert(e.match_id, ((pack.shape_name)(&e.text), target_scopes, e.start));
        }
        Capture::Flow(FlowCap::Rebind) => {
            f.flow_rebinds.push(((pack.shape_name)(&e.text), cur_scope, e.start));
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
                .map(|x| (x.text.clone(), None))
                .or_else(|| {
                    let name = in_match(Capture::Ref(RefCap::MemberCall))?;
                    let recv = in_match(Capture::Member(MemberCap::Recv))?;
                    Some(((pack.shape_name)(&name.text), Some(recv.span())))
                });
            if let Some((callee, receiver)) = callee {
                let span = e.span();
                st.minted[ix].value = true;
                let args = super::members::call_args(ad, e.match_id);
                out.call_sites.push(crate::build::query_extract::CallSite {
                    span,
                    callee,
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
            let in_match = |cap: Capture| ad.in_match(e.match_id, cap);
            if let (Some(name), Some(recv)) =
                (in_match(Capture::Ref(RefCap::MemberValue)), in_match(Capture::Member(MemberCap::Recv)))
            {
                let span = e.span();
                let member = (pack.shape_name)(&name.text);
                st.minted[ix].value = true;
                st.minted[ix].projects = Some(Box::new((recv.span(), member.clone())));
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
    for (ix, e) in ad.events().iter().enumerate() {
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
        st.minted[ix].value = true;
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
    // Match-id order (deterministic) — two captures targeting the same
    // `Variable{name, scope}` slot land witnesses in this order, and the
    // latest wins.
    let targets = f.flow_targets.iter().flat_map(|(&mid, (name, scopes, at))| scopes.iter().map(move |scope| (mid, name, scope, at)));
    for (mid, name, scope, at) in targets {
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
            let annot_type =
                (st.pack.annot_type)(annot).map(|t| written_type_at(out, &st.pack.names, t, annot_span.start));
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
        if let Some(target_span) = source_value_span(ad, &st.minted, mid) {
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
    for place in ad.by_match().filter(|e| e.cap == Capture::Flow(FlowCap::Place)) {
        let read = ad.on_ixs(place).filter_map(|i| st.minted[i].projects.as_deref()).last();
        if let (Some((receiver, member)), Some(value)) = (read, source_value_span(ad, &st.minted, place.match_id)) {
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
/// `@flow.source`, narrowed onto the outermost value the source holds when
/// the source node itself carries no witness (paren wrappers).
fn source_value_span(ad: &Adorned, minted: &[Minted], mid: usize) -> Option<Span> {
    let src = ad.in_match(mid, Capture::Flow(FlowCap::Source))?;
    Some(
        ad.within(src)
            .filter(|&ix| minted[ix].value)
            .map(|ix| &ad.events()[ix])
            .max_by_key(|v| ad.width(v))
            .map_or(src.span(), Event::span),
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
