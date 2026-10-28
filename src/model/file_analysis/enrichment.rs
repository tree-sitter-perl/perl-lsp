//! Query-time dispatch/guard resolution, and the index rebuild after a
//! decided view installs its readings.

use super::*;

impl FileAnalysis {
    /// Test seam for an origin built outside an `OpenDocs`: install the
    /// readings its questions pick, as `OpenDocs::condition_open` does for
    /// an open doc.
    #[cfg(test)]
    pub fn condition_with(&mut self, module_index: &dyn CrossFileLookup) {
        if let Some(view) = self.decided_with(module_index) {
            *self = view;
        }
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
            if let Some(c) = gated.pick(self, module_index).reading {
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
            if let Some(c) = gated.pick(self, module_index).reading {
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
            if gated.pick(self, module_index).verdict == Verdict::Unknown {
                out.push(UntypedDispatch {
                    call_span: gated.call_span(),
                    dispatcher: gated.dispatcher().to_string(),
                    gate: gated.gate().to_string(),
                });
            }
        }
        out
    }


    /// Rebuild the table indices after a view installed its readings, and
    /// link its installed HashKeyAccess refs to the HashKeyDefs it installed
    /// — the same `(target_name, owner)` linker `build_indices` runs.
    pub fn rebuild_decided_indices(&mut self, first_installed_ref: usize) {
        self.symbols.rebuild_indices();

        // Link installed HashKeyAccess refs to installed HashKeyDef symbols,
        // mirroring build_indices's logic. Base refs keep the links the
        // build gave them: a base ref linked to an installed symbol would
        // name an id the next view's cut removes.
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
        for (i, r) in self.refs.iter().enumerate().skip(first_installed_ref) {
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

        self.refs.rebuild_indices(&self.pack.names);
    }


}
