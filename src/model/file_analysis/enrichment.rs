//! Deferred plugin emissions (applied by the index's gate-applied view),
//! query-time dispatch/guard resolution, and the index rebuild after symbols
//! are appended.

use super::*;

impl FileAnalysis {
    /// Which deferred plugin emissions fire NOW: `fired[i]` when any of
    /// `gated_emissions[i]`'s `ClassIsa` prefixes holds through the index's
    /// ancestry. Asked at every read, never stored, so an ancestry edit
    /// anywhere moves it with nothing to invalidate.
    pub fn fired_gates(&self, module_index: &dyn CrossFileLookup) -> Vec<bool> {
        self.plugin
            .gated_emissions
            .iter()
            .map(|em| {
                em.gate_prefixes.iter().any(|prefix| {
                    class_isa_prefix(&em.package, prefix, &self.packages, Some(module_index))
                })
            })
            .collect()
    }

    /// This analysis with the emissions whose gate holds applied, or `None`
    /// when none fire — the open-document twin of the index's gate-applied
    /// view, for an analysis the index does not hold.
    pub fn with_holding_gates(&self, module_index: &dyn CrossFileLookup) -> Option<FileAnalysis> {
        if self.plugin.gated_emissions.is_empty() {
            return None;
        }
        let fired = self.fired_gates(module_index);
        if !fired.iter().any(|f| *f) {
            return None;
        }
        let mut copy = self.clone();
        copy.apply_fired_gated_emissions(&fired);
        Some(copy)
    }

    /// Test seam for an origin built outside a `FileStore`: apply the gates
    /// that hold, as `FileStore::condition_open` does for an open doc.
    #[cfg(test)]
    pub fn condition_with(&mut self, module_index: &dyn CrossFileLookup) {
        if let Some(applied) = self.with_holding_gates(module_index) {
            *self = applied;
        }
    }

    /// Apply the deferred plugin emissions whose gate the caller found
    /// holding (`fired[i]` for `gated_emissions[i]`), then rebuild the
    /// name/scope indices so the new symbols are findable. The index's
    /// gate-applied view is the one caller: it asks the gates, this only
    /// mints.
    pub fn apply_fired_gated_emissions(&mut self, fired: &[bool]) {
        self.apply_fired_gated_emissions_raw(fired);
        self.rebuild_appended_indices();
    }

    fn apply_fired_gated_emissions_raw(&mut self, fired: &[bool]) {
        if self.plugin.gated_emissions.is_empty() {
            return;
        }
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
        // Snapshot: the borrow of `self.plugin.gated_emissions` can't overlap the
        // `&mut self` symbol/ref/witness pushes below.
        let emissions = std::mem::take(&mut self.plugin.gated_emissions);
        for (i, em) in emissions.iter().enumerate() {
            if !fired.get(i).copied().unwrap_or(false) {
                continue;
            }
            let scope = self.scope_at(em.scope_point).unwrap_or(ScopeId(0));
            let ns = Namespace::framework(em.plugin_id.clone());
            for gs in &em.symbols {
                let pkg = gs.on_class.clone().or_else(|| Some(em.package.clone()));
                // Same dedup the builder's `apply_emit_action` runs: never
                // stack a second identical synthesized symbol.
                let dup = self.symbols.iter().any(|s| {
                    s.name == gs.name
                        && s.kind == gs.kind
                        && s.package == pkg
                        && s.namespace == ns
                });
                if dup {
                    continue;
                }
                let id = SymbolId(self.symbols.len() as u32);
                self.symbols.push(Symbol {
                    id,
                    name: gs.name.clone(),
                    kind: gs.kind,
                    span: gs.span,
                    selection_span: gs.selection_span,
                    scope,
                    package: pkg.clone(),
                    detail: gs.detail.clone(),
                    namespace: ns.clone(),
                    presentation: gs.presentation.clone(),
                    attributes: Vec::new(),
                    flags: Default::default(),
                    declared_with: None,
                    deref_stack: Vec::new(),
                    arity: None,
                });
                if let Some(rt) = &gs.return_type {
                    // Plugin-priority `Symbol(sid) → InferredType`, matching
                    // the builder's Method emit. Class-scoped methods also get
                    // the `PackageSymbol{package,name} → Edge(Symbol(sid))`
                    // mirror the fold writeback would have pushed, so cross-
                    // file return-type queries resolve the relationship shape.
                    self.witnesses.push(Witness {
                        attachment: WitnessAttachment::Symbol(id),
                        source: WitnessSource::Plugin(em.plugin_id.clone()),
                        payload: WitnessPayload::InferredType(rt.clone()),
                        span: gs.span,
                    });
                    if matches!(gs.kind, SymKind::Method | SymKind::Sub) {
                        if let Some(class) = &pkg {
                            self.witnesses.push(Witness {
                                attachment: WitnessAttachment::PackageSymbol {
                                    package: class.clone(),
                                    name: gs.name.clone(),
                                },
                                source: WitnessSource::Plugin(em.plugin_id.clone()),
                                payload: WitnessPayload::Edge(WitnessAttachment::Symbol(id)),
                                span: gs.span,
                            });
                        }
                    }
                }
            }
            for gr in &em.refs {
                self.refs.push(Ref {
                    kind: gr.kind.clone(),
                    span: gr.span,
                    scope,
                    target_name: gr.target_name.clone(),
                    access: gr.access,
                    binding: gr.binding.clone(),
                    folded_from: None,
                    arg_count: None,
                });
            }
        }
        self.plugin.gated_emissions = emissions;
    }


}

impl FileAnalysis {
    /// Push a `TypeConstraint` shape into the witness bag — a Variable
    /// `InferredType` witness plus a class-assertion observation when
    /// the type is a class identity. The bag is the single store; this
    /// helper exists so callers can keep the legible "I'm seeding a
    /// type constraint on $X" call shape rather than open-coding the
    /// witness construction. Builder has a parallel helper that does
    /// the same thing during the walk.
    #[cfg(test)]
    pub(crate) fn push_type_constraint(&mut self, tc: TypeConstraint) {
        use crate::model::witnesses::{Witness, WitnessAttachment, WitnessPayload, WitnessSource};
        let TypeConstraint { variable, scope, constraint_span: span, inferred_type: ty } = tc;
        self.witnesses.push(Witness {
            attachment: WitnessAttachment::Variable { name: variable, scope },
            source: WitnessSource::Builder("type_constraint".into()),
            payload: WitnessPayload::InferredType(ty),
            span: Span { start: span.start, end: span.start },
        });
    }

    /// Resolve one gated dispatch candidate against its receiver, AT QUERY
    /// TIME. Receiver resolution is two-tier: the build-time `receiver_class`
    /// hint (a locally-constructed `My::Minion->new`, a typed `has`-attribute)
    /// when present, else cross-file resolution of the call's invocant via
    /// `method_call_invocant_class` with the module index — which lights up
    /// helper-/attribute-returned receivers (`$c->minion->enqueue`,
    /// `$self->_minion->enqueue`) that only type once other modules are in
    /// scope. The gate (`isa target_class`) is applied by `resolve_for`; the
    /// caller never reads the inner candidate without it.
    fn resolve_dispatch_candidate<'a>(
        &'a self,
        gated: &'a ProvisionalDispatch,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> GateResult<&'a DispatchCandidate> {
        let recv = self.dispatch_receiver_class(gated, module_index);
        gated.resolve_for(recv.as_deref(), &self.packages, module_index)
    }

    /// Resolve the receiver class for a gated dispatch candidate: the
    /// build-time hint, else the call's invocant via the MethodCall ref at
    /// `call_span` (cross-file aware through the bag). Uses only the gate-input
    /// accessors, never the gated handler payload.
    fn dispatch_receiver_class(
        &self,
        gated: &ProvisionalDispatch,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<String> {
        gated.receiver_hint().cloned().or_else(|| {
            let call_span = gated.call_span();
            let dispatcher = gated.dispatcher();
            self.refs
                .iter()
                .find(|r| {
                    r.span == call_span
                        && r.target_name == dispatcher
                        && matches!(r.kind, RefKind::MethodCall { .. })
                })
                .and_then(|r| self.method_call_invocant_class(r, module_index))
        })
    }

    /// Query-time handler call-sites in THIS file: every gated dispatch
    /// candidate whose receiver isa-resolves the gate, projected to the data
    /// `refs_to` and goto-def need. The single seam for both — `resolve.rs`
    /// (handler references) and dispatch goto-def call it, so they can't
    /// drift. Candidates ride the cache; resolution is lazy, so non-open
    /// workspace/dependency files surface exactly like open ones
    /// (`docs/adr/receiver-gated-dispatch.md`).
    pub fn applicable_dispatches(
        &self,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<AppliedDispatch> {
        // Avoid double-counting a site the emit-hook path already materialized
        // as a real `DispatchCall` ref (files whose triggers fired).
        let materialized: HashSet<(Point, Point, String)> = self
            .refs
            .iter()
            .filter_map(|r| match &r.kind {
                RefKind::DispatchCall { dispatcher, .. } => {
                    Some((r.span.start, r.span.end, dispatcher.clone()))
                }
                _ => None,
            })
            .collect();
        let mut out = Vec::new();
        for gated in &self.provisional_dispatches {
            if let GateResult::Applies(c) = self.resolve_dispatch_candidate(gated, module_index) {
                if materialized.contains(&(c.span.start, c.span.end, c.dispatcher.clone())) {
                    continue;
                }
                out.push(AppliedDispatch {
                    name: c.name.clone(),
                    span: c.span,
                    owner: HandlerOwner::Class(c.owner_class.clone()),
                });
            }
        }
        out
    }

    /// The applicable dispatch at a cursor point, if the cursor sits on a
    /// gated dispatch verb call whose receiver isa-resolves. Drives
    /// query-time dispatch goto-def — the same gate as `applicable_dispatches`,
    /// so an open file with a cross-file receiver resolves the handler without
    /// any eagerly-materialized `DispatchCall` ref. Matches on either the
    /// name-arg span or the whole-call span so the cursor anywhere on the
    /// `verb('name')` call lands.
    pub fn dispatch_at(
        &self,
        point: Point,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<AppliedDispatch> {
        for gated in &self.provisional_dispatches {
            // `call_span` (a gate-input accessor) spans the whole `verb('name')`
            // call, so it covers the name-arg too — cheap cursor pre-filter
            // before resolving the gate.
            if !contains_point(&gated.call_span(), point) {
                continue;
            }
            if let GateResult::Applies(c) = self.resolve_dispatch_candidate(gated, module_index) {
                return Some(AppliedDispatch {
                    name: c.name.clone(),
                    span: c.span,
                    owner: HandlerOwner::Class(c.owner_class.clone()),
                });
            }
        }
        None
    }

    /// Every scalar-receiver dereference in this file, paired with the
    /// receiver's **narrowed** type at the use point. The one lattice read
    /// the undef/Optional/shape diagnostics (D1/D2/D6,
    /// `docs/adr/narrowing-diagnostics.md`) consume — each is a filter
    /// over this stream that asks the type, never the syntax (rule #10).
    ///
    /// A site is included only when the receiver's type resolves; an
    /// unresolvable receiver is omitted (honest silence — the diagnostics
    /// built on top miss it rather than guess). All four arrow forms are
    /// covered: method (`$x->m`) and hash (`$x->{k}`) carry a receiver-typed
    /// ref; array (`$x->[i]`) and code (`$x->()`) carry none, so the builder
    /// records them as `arrow_deref_sites` and this query merges the two
    /// sources. The residual is the receiver *shape*, not the arrow form: only
    /// a plain scalar operand is recorded, so a chain receiver (`f()->[0]`,
    /// `$x->{k}->()`) is skipped — only a plain scalar can be provably
    /// `Undef`/`Optional`.
    pub fn deref_receiver_sites(
        &self,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<DerefSite> {
        use crate::model::conventions::InvocantText;
        let mut out = Vec::new();
        for r in self.refs() {
            let (receiver, form) = match &r.kind {
                RefKind::MethodCall { invocant, .. } | RefKind::FieldAccess { invocant, .. } => {
                    // Only a scalar invocant can be undef/Optional; a
                    // bareword/`__PACKAGE__`/chain/bridged receiver never
                    // narrows here.
                    let Some(name) = invocant.as_name() else { continue };
                    if !matches!(name.classify(), InvocantText::Scalar(_)) {
                        continue;
                    }
                    (invocant.text().to_string(), DerefForm::Method(r.target_name.clone()))
                }
                // A scalar `var_text` is the arrow form `$x->{k}`; the direct
                // `$h{k}` form's base is the named hash, which can't be an
                // undef *receiver* (and won't type as Undef/Optional anyway).
                RefKind::HashKeyAccess { var_text, .. } if var_text.starts_with('$') => {
                    (var_text.clone(), DerefForm::HashKey)
                }
                _ => continue,
            };
            if let Some(ty) =
                self.inferred_type_via_bag_ctx(&receiver, r.span.start, module_index)
            {
                out.push(DerefSite {
                    span: r.span,
                    receiver,
                    receiver_ty: ty,
                    form,
                });
            }
        }
        // Array (`$x->[i]`) / code (`$x->()`) derefs have no typed ref; the
        // builder records them so they join the same stream.
        for s in &self.arrow_deref_sites {
            if let Some(ty) =
                self.inferred_type_via_bag_ctx(&s.receiver, s.span.start, module_index)
            {
                out.push(DerefSite {
                    span: s.span,
                    receiver: s.receiver.clone(),
                    receiver_ty: ty,
                    form: s.form.clone(),
                });
            }
        }
        out
    }

    /// True if `ancestor` is on `descendant`'s inheritance chain (or they are
    /// the same class) — the relatedness test D4 uses to avoid flagging a
    /// legitimate downcast as a contradiction.
    fn is_ancestor_of(
        &self,
        ancestor: &str,
        descendant: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> bool {
        let mut found = false;
        self.for_each_ancestor_class(descendant, module_index, |c| {
            if c == ancestor {
                found = true;
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        });
        found
    }

    /// D3/D4: each recorded guard whose outcome is constant given its
    /// subject's prior (pre-guard) type — redundant (always-true) or
    /// contradictory (always-false). Only fires on a *confident* prior type
    /// (a concrete class / rep / `Undef`); an absent or merely-Optional prior
    /// leaves the guard meaningful and is skipped (rule #10 — ask the type).
    /// Scoped to `isa`/`DOES` (class) and `defined`/`blessed` guards; rep
    /// `ref…eq` guards are the documented residual.
    pub fn guard_redundancies(
        &self,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<GuardRedundancy> {
        let mut out = Vec::new();
        for g in &self.guard_sites {
            let Some(prior) = self
                .inferred_type_via_bag_ctx(&g.subject, g.before_point, module_index)
                .filter(InferredType::is_known)
            else {
                continue;
            };
            // (definitely satisfies the raw predicate, definitely contradicts it)
            let (satisfied, contradicted) = match &g.predicate {
                GuardPredicate::Defined => match &prior {
                    InferredType::Undef => (false, true),
                    // The genuine "maybe undef" — the guard is doing its job.
                    InferredType::Optional(_) => continue,
                    // Any other concrete type is definitely defined.
                    _ => (true, false),
                },
                GuardPredicate::IsType(InferredType::ClassName(target)) => {
                    let Some(prior_cls) = prior.class_name() else { continue };
                    if prior_cls == target
                        || self.is_ancestor_of(target, prior_cls, module_index)
                    {
                        (true, false) // prior is-a target
                    } else if self.is_ancestor_of(prior_cls, target, module_index) {
                        continue; // target is a subclass of prior — a legit downcast
                    } else {
                        (false, true) // unrelated concrete classes
                    }
                }
                // Rep `ref…eq` guards (HASH/ARRAY/CODE) — residual.
                GuardPredicate::IsType(_) => continue,
            };
            let always_true =
                (satisfied && g.asserts_when_true) || (contradicted && !g.asserts_when_true);
            let always_false =
                (satisfied && !g.asserts_when_true) || (contradicted && g.asserts_when_true);
            let verdict = if always_true {
                GuardVerdict::AlwaysTrue
            } else if always_false {
                GuardVerdict::AlwaysFalse
            } else {
                continue;
            };
            out.push(GuardRedundancy {
                span: g.span,
                verdict,
                subject: g.subject.clone(),
                predicate: g.predicate.clone(),
            });
        }
        out
    }

    /// The rep a `ref…eq` / `isa` GUARD established for `subject` at `point`,
    /// read from **narrowing-sourced witnesses only**. Distinct from
    /// `inferred_type_via_bag`: a `$x->{k}` deref pushes a zero-extent
    /// `HashRef` belief sitting exactly at the use point, which would mask the
    /// guard's rep under the merged query — so D6 reads the guard's assertion
    /// directly. Innermost (narrowest) containing region wins. `None` when no
    /// guard narrows the subject here (so D6 fires only on guard-narrowed
    /// reps, per the plan).
    pub fn guard_narrowed_rep(&self, subject: &str, point: Point) -> Option<InferredType> {
        use crate::model::witnesses::{WitnessAttachment, WitnessPayload, WitnessSource};
        let mut best: Option<&crate::model::witnesses::Witness> = None;
        for w in self.witnesses.filter(|w| {
            matches!(&w.source, WitnessSource::Builder(s) if s == "narrowing" || s == "defined_narrowing")
                && matches!(&w.attachment, WitnessAttachment::Variable { name, .. } if name == subject)
        }) {
            if !contains_point(&w.span, point) {
                continue;
            }
            if !matches!(&w.payload, WitnessPayload::InferredType(_)) {
                continue;
            }
            let innermost = match best {
                None => true,
                Some(b) => (w.span.start.row, w.span.start.column) >= (b.span.start.row, b.span.start.column),
            };
            if innermost {
                best = Some(w);
            }
        }
        match best.map(|w| &w.payload) {
            Some(WitnessPayload::InferredType(t)) => Some(t.clone()),
            _ => None,
        }
    }

    /// Gated dispatch candidates in THIS file whose receiver couldn't be
    /// typed (`ReceiverUntyped`) — the genuine typing gaps the opt-in
    /// `unresolved-dispatch` diagnostic surfaces. `DoesNotApply` is a settled
    /// negative and never appears here.
    pub fn untyped_dispatches(
        &self,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<UntypedDispatch> {
        let mut out = Vec::new();
        for gated in &self.provisional_dispatches {
            if let GateResult::ReceiverUntyped =
                self.resolve_dispatch_candidate(gated, module_index)
            {
                out.push(UntypedDispatch {
                    call_span: gated.call_span(),
                    dispatcher: gated.dispatcher().to_string(),
                    gate: gated.gate().to_string(),
                });
            }
        }
        out
    }


    /// Rebuild the indices affected by enrichment (symbols, the ref
    /// name/target lookups, HashKeyAccess linkage).
    ///
    /// Enrichment re-owns HashKeyAccess refs (dropping their stale
    /// HashKeyDef links) and gated emissions can mint symbols. This
    /// method re-runs the same `(target_name, owner)` linker that
    /// `build_indices` uses, so the ref→target index stays accurate after a
    /// cross-file hash-key binding resolves.
    fn rebuild_appended_indices(&mut self) {
        self.symbols.rebuild_indices();

        // Re-link HashKeyAccess refs to (possibly newly-injected) HashKeyDef
        // symbols, mirroring build_indices's logic.
        let hashkey_defs: HashMap<(String, HashKeyOwner), SymbolId> = self.symbols.iter()
            .filter_map(|sym| {
                if let SymbolDetail::HashKeyDef { owner, .. } = &sym.detail {
                    Some(((sym.name.clone(), owner.clone()), sym.id))
                } else {
                    None
                }
            })
            .collect();
        let mut hashkey_resolutions: Vec<(usize, SymbolId)> = Vec::new();
        for (i, r) in self.refs.iter().enumerate() {
            if r.resolved_symbol().is_some() {
                continue;
            }
            if let Some(owner) = r.hash_key_owner() {
                if let Some(&sid) = hashkey_defs.get(&(r.target_name.clone(), owner.clone())) {
                    hashkey_resolutions.push((i, sid));
                }
            }
        }
        for (idx, sid) in hashkey_resolutions {
            self.refs[idx].link_owned_symbol(sid);
        }

        self.refs.refresh_name_target_indices(&self.pack.names);
    }


}
