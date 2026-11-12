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
    /// Whether the write resets the binding where it lands.
    resets: bool,
    /// The target token.
    token: Span,
    /// A `@type.<relative>` annotation's mark and the class body it is
    /// written in; outside every class body the mark names none.
    relative: Option<(RelativeClass, Option<String>)>,
    /// The namespace the token's `@space.*` names: a data member's
    /// declaration is its member identity's, so two members of one name in
    /// one class (php's `const X` and `$X`) never share a fact.
    space: Option<crate::model::file_analysis::NamespaceId>,
    /// The token declares a free constant (`@mark.constant`): its value is
    /// the constant's own, never a variable's of its name.
    constant: bool,
}

#[derive(Default)]
pub(super) struct State {
    /// Each `@flow.target`'s binding, by its match: what only the walk at
    /// it knows.
    flow_targets: BTreeMap<usize, Target>,
    /// Rebind shapes with no inflowing value (loop vars: `for x in …`,
    /// `for (auto x : …)`) — they mint a `Rebind` FlowEdge so the narrowing
    /// cutoff sees them, exactly like Perl's `foreach` var. By name, scope,
    /// where the write lands, and the token.
    pub(super) flow_rebinds: Vec<(String, ScopeId, Point, Span)>,
    /// Usage-site evidence awaiting the rebind cutoff, which reads the
    /// finished flow edges (`narrowing::finish`): the observed name, its
    /// scope, the observation, and its region before the cutoff.
    pub(super) observations: Vec<(String, ScopeId, crate::model::witnesses::TypeObservation, Span)>,
}

/// Where a target event's write lands, and whether it lands there as a
/// reset. A `@flow.assign` match writes at its assignment's end, after every
/// read the assignment makes, and resets the variable there
/// (`docs/adr/flow-narrowing.md`); any other target is bound at its own token.
fn written_at(ad: &Adorned, e: &Event) -> (Point, bool) {
    match ad.in_match(e.match_id, Capture::Flow(FlowCap::Assign)) {
        Some(a) => (a.end, true),
        None => (e.start, false),
    }
}

fn reset(witnesses: &mut Vec<Witness>, name: String, scope: ScopeId, at: Point) {
    witnesses.push(Witness {
        attachment: WitnessAttachment::Variable { name, scope },
        source: WitnessSource::Builder(crate::model::witnesses::RESET_SOURCE.into()),
        payload: WitnessPayload::Reset,
        span: Span { start: at, end: at },
    });
}

fn skeleton_witness(attachment: WitnessAttachment, source: &str, payload: WitnessPayload, span: Span) -> Witness {
    Witness { attachment, source: WitnessSource::Builder(source.into()), payload, span }
}

pub(super) fn handle(st: &mut ExtractState, ix: usize, e: &Event) {
    let pack = st.pack;
    let ad = st.adorned;
    let cur_scope = st.cur_scope;
    let (target_scopes, enclosing) = match e.cap {
        Capture::Flow(FlowCap::Target) => {
            (super::defs::binding_scopes(st, e), super::scopes::enclosing_class(st).map(str::to_string))
        }
        _ => (Vec::new(), None),
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
            out.var_reads.push(crate::build::query_extract::VarRead { name: name.clone(), scope: cur_scope, span, token: ref_span });
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
        Capture::Expr(ExprCap::ReturnClass) => {
            if !e.text.is_empty() {
                out.class_returns.push((cur_scope, e.span(), e.text.clone()));
            }
        }
        Capture::Flow(FlowCap::Target) => {
            let name = (pack.shape_name)(&e.text);
            let (at, resets) = written_at(ad, e);
            // Record the declared-type text keyed by (var, DECLARING scope)
            // for the token-less optional-engagement narrowing. cur_scope is
            // only known here (scopes mint during this walk).
            let mut relative = None;
            if let Some((annot, _)) = ad.annot_in_match(e.match_id) {
                for &scope in &target_scopes {
                    st.narrowing.annot_text_by_var.insert(((pack.shape_name)(&e.text), scope), annot.text.clone());
                }
                relative = super::defs::names_relative(ad, annot).map(|rel| (rel, enclosing));
            }
            let space = ad.on(e).find_map(|t| match t.cap {
                Capture::Space { id, .. } => Some(id),
                _ => None,
            });
            let constant = ad.on(e).any(|t| t.cap == Capture::Mark(MarkCap::Constant));
            let target = Target { name, scopes: target_scopes, at, resets, token: e.span(), relative, space, constant };
            f.flow_targets.insert(e.match_id, target);
        }
        Capture::Flow(FlowCap::Rebind) => {
            let (at, resets) = written_at(ad, e);
            if resets {
                reset(&mut out.witnesses, (pack.shape_name)(&e.text), cur_scope, at);
            }
            f.flow_rebinds.push(((pack.shape_name)(&e.text), cur_scope, at, e.span()));
        }
        Capture::AnonAggMember => {
            // A field typed by an anonymous aggregate: its members are
            // flattened onto the enclosing named container, so the field's
            // own type IS that container (the anon hop is identity) —
            // `u->data.ping` types `u->data` as U and finds `ping` there.
            // TypeName (not ClassName) so a typedef'd container chases.
            // It is the field's declared type, so a write never retypes it,
            // and the declaring token's fact (`decl_facts`), so a
            // flattened member of the same name (redis `struct { … *lp; }
            // lp;`) never takes it.
            if let Some(owner) = &st.package {
                let name = (pack.shape_name)(&e.text);
                let w = Witness {
                    attachment: WitnessAttachment::Variable { name: name.clone(), scope: cur_scope },
                    source: WitnessSource::Builder("declared_type".into()),
                    payload: WitnessPayload::Bound {
                        ty: crate::model::witnesses::BoundType::Edge(WitnessAttachment::TypeName(owner.clone())),
                        // The member holds the container itself, so nothing
                        // converts and nothing narrows past it either way.
                        evidence: crate::model::witnesses::Evidence::Declared(st.pack.annot_enforcement),
                        extent: crate::model::witnesses::Extent::Lifetime,
                    },
                    span: Span { start: e.start, end: e.start },
                };
                out.decl_facts.push((cur_scope, e.start, name, w));
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
            // A callee named by its spelling (`ns::f(..)`) resolves by
            // that spelling only under a use-map (`function_identity`);
            // elsewhere a free callee's lookup does not read it yet, and it
            // mints no call value (`docs/adr/calls-are-invokes.md`).
            let callee = in_match(Capture::Ref(RefCap::Call))
                .filter(|_| pack.names.use_map_sep().is_some() || in_match(Capture::RefSpelled).is_none())
                .map(|x| (x, None))
                .or_else(|| {
                    let name = in_match(Capture::Ref(RefCap::MemberCall))?;
                    let recv = in_match(Capture::Member(MemberCap::Recv))?;
                    Some((name, Some(recv.span())))
                });
            if let Some((token, receiver)) = callee {
                // the callee as the match spells it, as its reference reads it
                let written = in_match(Capture::RefSpelled).map_or(&token.text, |s| &s.text);
                let (callee, callee_at) = ((pack.shape_name)(written), token.start);
                let span = e.span();
                let args = super::members::call_args(ad, e.match_id);
                out.call_sites.push(crate::build::query_extract::CallSite {
                    span,
                    callee,
                    callee_at,
                    receiver,
                    scope: cur_scope,
                    args,
                    constructs: false,
                    forwards: in_match(Capture::Member(MemberCap::Forward)).is_some(),
                });
            }
        }
        Capture::Expr(ExprCap::Construct) => {
            // The site calls the constructor on the class its receiver
            // names, and the call's own reference sits on that token: it
            // spells the class, so the class's own reference keeps the
            // token for the cursor and for a class rename. Which member is
            // the constructor is the class's `@mark.constructor` flag, asked
            // where the class is in hand; the token names only the class.
            if let Some(recv) = ad.in_match(e.match_id, Capture::Member(MemberCap::Recv)) {
                let span = e.span();
                let args = super::members::call_args(ad, e.match_id);
                let arg_count = args.as_ref().map(Vec::len);
                out.call_sites.push(crate::build::query_extract::CallSite {
                    span,
                    callee: recv.text.clone(),
                    callee_at: recv.start,
                    receiver: Some(recv.span()),
                    scope: cur_scope,
                    args,
                    constructs: true,
                    forwards: false,
                });
                let invocant = match recv.payload.as_deref() {
                    Some(Payload::Recv(i)) => Some((recv.span(), i.clone())),
                    _ => None,
                };
                out.refs.push(SkelRef {
                    kind: RefCap::MemberCall,
                    name: recv.text.clone(),
                    start: recv.start,
                    end: recv.end,
                    scope: cur_scope,
                    invocant,
                    member_op: None,
                    arg_count,
                    constructs: true,
                    spaces: None,
                    qualifier: None,
                    spelled: None,
                });
            }
        }
        Capture::Expr(ExprCap::Class) => {
            // The value is the class the match's receiver token names.
            let recv = ad.of_match(e.match_id).find(|x| matches!(x.cap, Capture::NamedReceiver(_)));
            if let Some(recv) = recv {
                let span = e.span();
                out.witnesses.push(skeleton_witness(
                    WitnessAttachment::Expr(span),
                    "skeleton-class",
                    WitnessPayload::Edge(WitnessAttachment::Expr(recv.span())),
                    span,
                ));
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
                // The namespaces the member token's `@space.*` captures
                // state; untagged, where a value read lands.
                let spaces = super::refs::space_ask(ad.on(name))
                    .unwrap_or(crate::model::file_analysis::SpaceAsk::only(pack.member_spaces.values));
                st.minted[ix].projects =
                    Some(Box::new(MemberRead { receiver: recv.span(), member: member.clone(), token: name.span(), spaces }));
                out.member_reads.push(crate::build::query_extract::MemberRead {
                    span,
                    receiver: recv.span(),
                    member: member.clone(),
                    token: name.span(),
                    written: false,
                });
                out.witnesses.push(skeleton_witness(
                    WitnessAttachment::Expr(span),
                    "skeleton-member",
                    WitnessPayload::Projected {
                        base: WitnessAttachment::Expr(recv.span()),
                        step: crate::model::witnesses::ProjectionStep::ValueHop { member, spaces },
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
                object: ad.in_match(e.match_id, Capture::Ref(RefCap::Receiver)).is_some(),
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
    let classes = &st.scopes;
    // A class/struct DATA MEMBER is visible throughout its class body
    // regardless of declaration order (C++ member lookup is not sequential:
    // a method reads a field declared later in a `private:` section below
    // it). The type-witness temporal filter is position-based — it admits a
    // witness only at/after its span — so a zero-width witness pinned to the
    // field's decl point would be REJECTED for every read textually above it.
    // Give the field's declared bound its class-body scope's span so
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
    // A member's or constant's declaration facts are its declaring token's
    // (`SkeletonAnalysis::decl_facts`): `into_file_analysis` lowers
    // them to the place of the symbol that token declares. Their fallback
    // attachment is the member, owned by the class its scope names — the one
    // the declaring symbol is filed under (`defs::handle`).
    let mut decl_sites: HashMap<(ScopeId, Point), (ScopeId, Point, String, WitnessAttachment)> = HashMap::new();
    for (mid, Target { name, at, resets, token, relative, space, constant, .. }, scope) in targets {
        let owner = classes.class_of(*scope).filter(|_| field_scope_span.contains_key(&(name.clone(), *scope)));
        let decl = (owner.is_some() || *constant).then(|| (*scope, token.start, name.clone()));
        let var = match owner {
            Some(owner) => {
                let space = st.pack.member_spaces.of_decl(crate::model::file_analysis::SymKind::Field, *space);
                WitnessAttachment::holds_of(owner, space, name.clone())
            }
            _ => WitnessAttachment::Variable { name: name.clone(), scope: *scope },
        };
        if decl.is_some() {
            decl_sites.insert((*scope, *at), (*scope, token.start, name.clone(), var.clone()));
        }
        // A data member's write in its class body is its initializer, not a
        // step in a sequence: its declared type holds class-wide, so nothing
        // there retires it.
        if *resets && !field_scope_span.contains_key(&(name.clone(), *scope)) {
            reset(&mut out.witnesses, name.clone(), *scope, *at);
        }
        // Class-wide extent for a data member; the sequential decl point for a
        // local (see `field_scope_span`).
        let annot_span = field_scope_span
            .get(&(name.clone(), *scope))
            .copied()
            .unwrap_or(Span { start: *at, end: *at });
        if let Some((annot, extent)) = ad.annot_in_match(mid).map(|(a, x)| (&a.text, x)) {
            // A class-shaped declared type edges into the alias graph (it may
            // be a typedef — `U16 x;` where `typedef unsigned short U16`);
            // primitives stay leaves; `None` (auto/void) defers to the flow
            // edge as before. `TypeName` chases the typedef or falls back to
            // the same `ClassName`, so a plain struct/class is unchanged.
            // A type naming a class by where it is written is that class,
            // already an identity; any other spelling resolves at its site.
            let annot_type = match relative {
                Some((rel, class)) => class.as_ref().and_then(|c| out.relative_class(*rel, c)).map(InferredType::ClassName),
                None => (st.pack.annot_type)(annot).map(|t| written_type_at(out, &st.pack.names, t, annot_span.start)),
            };
            if let (Some(t), true) = (&annot_type, field_scope_span.contains_key(&(name.clone(), *scope))) {
                out.field_types.push((name.clone(), *scope, t.clone()));
            }
            let ty = match annot_type {
                Some(InferredType::ClassName(cn)) => {
                    Some(crate::model::witnesses::BoundType::Edge(WitnessAttachment::TypeName(cn)))
                }
                Some(t) if t.is_known() => Some(crate::model::witnesses::BoundType::Type(t)),
                Some(_) | None => None,
            };
            if let Some(ty) = ty {
                // A binding through a pointer or reference holds the
                // referent, whose type no write converts.
                let through_reference = ad.of_match(mid).any(|t| {
                    matches!(t.payload.as_deref(), Some(super::Payload::Deref(stack)) if !stack.is_empty())
                });
                let enforcement = match through_reference {
                    true => st.pack.annot_enforcement.through_reference(),
                    false => st.pack.annot_enforcement,
                };
                // A member declaration declares the slot: no write retires it.
                let extent = match field_scope_span.contains_key(&(name.clone(), *scope)) {
                    true => crate::model::witnesses::Extent::Lifetime,
                    false => extent,
                };
                let w = Witness {
                    attachment: var.clone(),
                    source: WitnessSource::Builder("declared_type".into()),
                    payload: WitnessPayload::Bound {
                        ty,
                        evidence: crate::model::witnesses::Evidence::Declared(enforcement),
                        extent,
                    },
                    span: annot_span,
                };
                match decl {
                    Some((scope, token, name)) => out.decl_facts.push((scope, token, name, w)),
                    None => out.witnesses.push(w),
                }
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
                target_token: Some(*token),
                source: target_span,
                extraction: crate::model::file_analysis::Extraction::Whole,
                reassigns: reassigns(name, &token.start),
            });
        }
    }
    // A name a value flows into is written where the write lands; its token
    // denotes the place written.
    for (&mid, t) in &f.flow_targets {
        if source_value_span(ad, mid).is_some() {
            out.place_writes.push((t.token, t.at));
        }
    }
    // A place is whatever its own read projects. A member read's target is
    // decided by its receiver's type, which only the assembled analysis
    // answers (`skeleton::settle_member_writes`). A name's place is its
    // binding, which `@flow.target` writes and rebinds; a place no read
    // projects (a call, a literal) takes nothing. The member token of a
    // projected place is written, not read, whether or not a value flows in
    // (a compound assignment states none: `$o->n += 1`), and once however
    // many patterns name it: a plain `=` is both a write and a write of a
    // value. The read itself is marked written here, while its span is still
    // its own node's: a macro remap can fold the spans of an expansion onto
    // one another.
    let mut written = std::collections::HashSet::new();
    let reads: std::collections::HashMap<Span, usize> =
        out.member_reads.iter().enumerate().map(|(i, m)| (m.span, i)).collect();
    for place in ad.by_match().filter(|e| e.cap == Capture::Flow(FlowCap::Place)) {
        let Some(read) = ad.on_ixs(place).filter_map(|i| st.minted[i].projects.as_deref()).last() else { continue };
        if written.insert(place.span()) {
            out.write_sites.push(read.token);
            out.place_writes.push((place.span(), written_at(ad, place).0));
            if let Some(&i) = reads.get(&place.span()) {
                out.member_reads[i].written = true;
            }
        }
        if let Some(value) = source_value_span(ad, place.match_id) {
            out.member_writes.push(crate::build::query_extract::MemberWrite {
                receiver: read.receiver,
                member: read.member.clone(),
                spaces: read.spaces,
                value,
            });
        }
    }
    for init in ad.by_match().filter(|e| e.cap == Capture::Flow(FlowCap::Init)) {
        if let Some(read) = ad.on_ixs(init).filter_map(|i| st.minted[i].projects.as_deref()).last() {
            out.init_sites.push(read.token);
        }
    }
    // Bind-shape rebinds (loop vars): no inflowing value, recorded for the
    // narrowing cutoff (`Rebind` lowers to nothing — provenance only).
    for (name, scope, at, token) in std::mem::take(&mut f.flow_rebinds) {
        out.place_writes.push((token, at));
        edges.push(crate::model::file_analysis::FlowEdge {
            reassigns: reassigns(&name, &token.start),
            target_name: name,
            target_scope: scope,
            target_at: at,
            target_token: Some(token),
            source: Span { start: at, end: at },
            extraction: crate::model::file_analysis::Extraction::Rebind,
        });
    }
    out.flow_edges.extend(edges);
    // Lower the value-flow edges to type-tier witnesses (the bag is canonical
    // for types; the edges are the provenance tier above it).
    // A constant holds its value wherever it is read, a read written above
    // its declaration included.
    let file_span = out.scopes.first().map(|s| s.span);
    let constants: std::collections::HashSet<(ScopeId, Point)> =
        f.flow_targets.values().filter(|t| t.constant).flat_map(|t| t.scopes.iter().map(|s| (*s, t.at))).collect();
    for fe in &out.flow_edges {
        if let Some(mut w) = fe.lower_to_witness() {
            match decl_sites.get(&(fe.target_scope, fe.target_at)) {
                Some((scope, token, name, att)) => {
                    w.attachment = att.clone();
                    if let Some(file) = file_span.filter(|_| constants.contains(&(fe.target_scope, fe.target_at))) {
                        w.span = file;
                    }
                    out.decl_facts.push((*scope, *token, name.clone(), w));
                }
                None => out.witnesses.push(w),
            }
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
