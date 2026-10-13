//! `@flow.*`, `@expr.*`, `@obs.*`, `@domain.*`, `@shape.*`: values — where
//! they come from, what an expression evaluates to, and how a use observes
//! its operand. Lowered to type witnesses; the flow edges stay as the
//! provenance tier above them.

use super::*;
use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct State {
    /// flow.assign joins: match_id → (target name+scope, source span)
    flow_targets: HashMap<usize, (String, ScopeId, Point)>,
    flow_sources: HashMap<usize, Span>,
    /// Rebind shapes with no inflowing value (loop vars: `for x in …`,
    /// `for (auto x : …)`) — they mint a `Rebind` FlowEdge so the narrowing
    /// cutoff sees them, exactly like Perl's `foreach` var.
    pub(super) flow_rebinds: Vec<(String, ScopeId, Point)>,
    /// `@type.annot` — a declaration's type text, joined per match.
    annots: HashMap<usize, String>,
    /// expr-literal spans, for narrowing an Edge target onto the actual
    /// literal when the rhs node wraps it.
    lit_spans: Vec<(usize, usize, Span)>,
    /// keyed-shape collection: ctor + keys grouped per @expr.shape span
    shape_spans: Vec<(usize, usize, Span)>,
    shape_ctors: HashMap<(usize, usize), String>,
    shape_keys: Vec<(usize, usize, String)>,
    /// `@domain.value` — the operand a field slot is compared/assigned
    /// against. Joined to its `@domain.slot` by match_id (the slot event
    /// pushes the site); the value's own enum resolves cross-file later. Only
    /// an identifier-shaped operand can name an enumerator, so anything else
    /// (a literal, arithmetic, a call — the capture is ungated) is stored as
    /// the empty sentinel: it stays a SITE (counter-evidence in the coherence
    /// vote's denominator) without persisting arbitrary expression text.
    domain_value_by_match: HashMap<usize, String>,
}

pub(super) fn collect(st: &mut ExtractState, e: &Event) {
    match e.cap {
        Capture::TypeAnnot => {
            st.flow.annots.insert(e.match_id, e.text.clone());
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
            // declaration in into_file_analysis (goto-def + hover).
            out.var_reads.push(((pack.shape_name)(&e.text), cur_scope, span));
            out.witnesses.push(skeleton_witness(
                WitnessAttachment::Expr(span),
                "skeleton",
                WitnessPayload::Edge(WitnessAttachment::Variable {
                    name: (pack.shape_name)(&e.text),
                    scope: cur_scope,
                }),
                span,
            ));
        }
        Capture::Expr(ExprCap::ReturnValue) => {
            // The returned expression's own general-rule witness (literal
            // / var-read / member / call — whichever matched this same
            // node) already carries its type; this just records the site
            // (scope + span) so `emit_return_fuel` (language_driver.rs,
            // phase 7) can chain the enclosing function's `Symbol` onto it
            // when undeclared.
            out.return_sites.push((cur_scope, e.span()));
        }
        Capture::Flow(FlowCap::Target) => {
            f.flow_targets.insert(e.match_id, ((pack.shape_name)(&e.text), cur_scope, e.start));
            // Record the declared-type text keyed by (var, DECLARING scope)
            // for the token-less optional-engagement narrowing. cur_scope is
            // only known here (scopes mint during this walk); the type-annot
            // half was pre-collected (it precedes the declarator in source).
            if let Some(annot) = f.annots.get(&e.match_id) {
                st.narrowing.annot_text_by_var.insert(((pack.shape_name)(&e.text), cur_scope), annot.clone());
            }
        }
        Capture::Flow(FlowCap::Rebind) => {
            f.flow_rebinds.push(((pack.shape_name)(&e.text), cur_scope, e.start));
        }
        Capture::Flow(FlowCap::Source) => {
            f.flow_sources.insert(e.match_id, e.span());
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
        Capture::Shape(ShapeCap::Ctor) => {
            // belongs to the smallest enclosing expr.shape; matches
            // share the call node so byte keys line up
            f.shape_ctors
                .entry(byte_range_of(events, e.match_id, Capture::Expr(ExprCap::Shape)).unwrap_or((0, 0)))
                .or_insert_with(|| e.text.clone());
        }
        Capture::Shape(ShapeCap::Key) => {
            if let Some(range) = byte_range_of(events, e.match_id, Capture::Expr(ExprCap::Shape)) {
                f.shape_keys.push((range.0, range.1, e.text.clone()));
            }
        }
        Capture::Obs(obs) => {
            // Usage-site evidence: a mono-typed operator observes
            // its operand. Same Observation payloads the Perl
            // walker emits; the fold is the production one.
            out.witnesses.push(skeleton_witness(
                WitnessAttachment::Variable { name: (pack.shape_name)(&e.text), scope: cur_scope },
                "skeleton-obs",
                WitnessPayload::Observation(obs.observation()),
                e.span(),
            ));
        }
        Capture::Expr(ExprCap::Call) => {
            // A call's VALUE is the callee's own resolution — deferred to
            // `into_file_analysis`, where the symbol table is known: a
            // `Class` callee is a functional cast / constructor, a callable
            // flows its return, an unresolvable name types nothing. Record
            // (span, callee) and mark the span as a value-producing site so
            // an enclosing `auto x = f(..)` flow edge targets it; the type
            // witness is minted later once the callee resolves (no name-case
            // guess). `docs/adr/macro-handling.md`.
            let callee = events
                .iter()
                .find(|x| x.match_id == e.match_id && x.cap == Capture::Ref(RefKind::Call))
                .map(|x| x.text.clone());
            if let Some(callee) = callee {
                let span = e.span();
                f.lit_spans.push((e.start_byte, e.end_byte, span));
                out.call_sites.push((span, callee));
            }
        }
        // The assignment node anchors its pattern; its target and source
        // carry the meaning.
        Capture::Flow(FlowCap::Assign) => {}
        _ => {}
    }
}

/// Keyed shapes → `HashWithKeys` witnesses.
pub(super) fn emit_keyed_shapes(st: &mut ExtractState) {
    let f = &mut st.flow;
    let mut seen_spans: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    for &(sb, eb, span) in &f.shape_spans {
        if !seen_spans.insert((sb, eb)) {
            continue;
        }
        let Some(ctor) = f.shape_ctors.get(&(sb, eb)) else { continue };
        if !(st.pack.shape_ctor)(ctor) {
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
        .filter(|s| s.kind == DefKind::Field)
        .filter_map(|s| out.scopes.get(s.scope.0 as usize).map(|sc| ((s.name.clone(), s.scope), sc.span)))
        .collect();
    for mid in flow_mids {
        let (name, scope, at) = &f.flow_targets[mid];
        let var = WitnessAttachment::Variable { name: name.clone(), scope: *scope };
        // Class-wide extent for a data member; the sequential decl point for a
        // local (see `field_scope_span`).
        let annot_span = field_scope_span
            .get(&(name.clone(), *scope))
            .copied()
            .unwrap_or(Span { start: *at, end: *at });
        if let Some(annot) = f.annots.get(mid) {
            // A class-shaped declared type edges into the alias graph (it may
            // be a typedef — `U16 x;` where `typedef unsigned short U16`);
            // primitives stay leaves; `None` (auto/void) defers to the flow
            // edge as before. `TypeName` chases the typedef or falls back to
            // the same `ClassName`, so a plain struct/class is unchanged.
            let payload = match (st.pack.annot_type)(annot) {
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
        if let Some(src_span) = f.flow_sources.get(mid) {
            // Narrow onto the outermost literal the rhs wraps, when the
            // rhs node itself carries no witness (paren wrappers).
            let src_bytes = byte_range_of(events, *mid, Capture::Flow(FlowCap::Source));
            let target_span = f
                .lit_spans
                .iter()
                .filter(|&&(s, en, _)| src_bytes.is_some_and(|(ss, se)| s >= ss && en <= se))
                .max_by_key(|&&(s, en, _)| en - s)
                .map(|&(_, _, sp)| sp)
                .filter(|sp| sp != src_span)
                .unwrap_or(*src_span);
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

/// A bare identifier lexeme — the only shape that can name an enumerator.
/// Pure string test (no node-kind probe) so every language's capture text
/// routes through the same rule.
fn is_identifier_text(s: &str) -> bool {
    !s.is_empty()
        && !s.as_bytes()[0].is_ascii_digit()
        && s.bytes().all(|b| b == b'_' || b.is_ascii_alphanumeric())
}
