//! Where control paths rejoin, a place holds what it holds at the end of
//! every path that reaches the join (`docs/adr/cfg-tier.md`). This phase
//! mints those joins once, from the control document, the bound writes and
//! the tests that prove something of a place; the registry folds them at
//! query time.

use super::*;
use crate::model::file_analysis::{guard_region_witnesses, join_arms, Binding, BranchJoin, Construct, ControlKind, JoinPlan, Place, Proofs, Reach};
use crate::model::witnesses::{JoinRole, Witness, WitnessAttachment, WitnessPayload, WitnessSource, EFFECT_ARM_SOURCE, JOIN_SOURCE};
use std::collections::{HashMap, HashSet};

impl<'a> Builder<'a> {
    /// For every rebind, a join past each branch whose arm holds it, from
    /// the write out to the variable's binding (or the callable the write
    /// runs in); for every test that proves something of a place, where an
    /// arm of its construct leaves, a join past that construct and each
    /// branch around it. A construct is minted once per place, however many
    /// of its arms write.
    pub(super) fn assemble_joins(&mut self) {
        static ASSIGNMENT: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
        let kinds = ASSIGNMENT.get_or_init(|| perl_kind_ids(&["assignment_expression"]));
        // A declaration binds fresh: there is no old value to join. Only a
        // variable's rebind joins; a write to a part of one is a place's.
        let assignments: Vec<(Node<'a>, String)> = self
            .syntax
            .file_nodes_of_kind(kinds)
            .into_iter()
            .map(|n| n.ts())
            .filter_map(|n| {
                let left = n.child_by_field_name("left").filter(|l| l.kind() != "variable_declaration")?;
                Some((n, self.get_var_text_from_lhs(left)?))
            })
            .collect();
        let seeds = std::mem::take(&mut self.guard_seeds);
        if assignments.is_empty() && seeds.is_empty() {
            return;
        }
        let main = self.syntax.main().root().ts();
        let mut seen: HashSet<usize> = HashSet::new();
        let around: Vec<Node<'a>> = assignments
            .iter()
            .map(|&(n, _)| n)
            .chain(seeds.iter().map(|g| g.construct))
            .filter(|&n| self.syntax.root_of(n) == Some(main))
            .flat_map(|n| std::iter::once(n).chain(self.syntax.ancestors(n)))
            .filter(|n| seen.insert(n.id()))
            .collect();
        let source = self.source;
        // A statement calling a sub of this file comes back only if the sub
        // does, which its own body decides: the walk follows each such call
        // into the body it runs, and every body it reaches mints how it ends.
        let local = self.local_callables();
        let callee = |n: Node<'a>| self.local_callee(&local, n);
        let gate = |n: Node<'a>| callee(n).is_some();
        let exits = |n: Node<'a>| narrowing::is_exit_expression(n, source);
        let mut walk = super::control::ViewWalk::default();
        let mut gates = walk.walk(around, source, &exits, &gate);
        let mut bodies: Vec<(SymbolId, Node<'a>)> = Vec::new();
        let mut followed: HashSet<SymbolId> = HashSet::new();
        let mut i = 0;
        while i < gates.len() {
            let g = gates[i];
            i += 1;
            let Some(sid) = callee(g).filter(|&sid| followed.insert(sid)) else { continue };
            let Some(body) = self.callable_body(sid, main) else { continue };
            bodies.push((sid, body));
            gates.extend(walk.walk(super::control::statements_of(body), source, &exits, &gate));
        }
        let view = walk.finish();
        drop(local);
        for g in gates {
            self.emit_expr_witness(g);
        }
        let plan = JoinPlan::new(&view);
        let mut effect_arms: Vec<Witness> = Vec::new();
        if !bodies.is_empty() {
            static RETURNS: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
            let kinds = RETURNS.get_or_init(|| perl_kind_ids(&["return_expression", "goto_expression"]));
            let returns: Vec<Span> = self.syntax.file_nodes_of_kind(kinds).into_iter().map(|n| node_to_span(n.ts())).collect();
            for (sid, body) in bodies {
                effect_arms.extend(self.effect_arms(&plan, sid, body, &returns));
            }
        }
        let untaken = std::mem::take(&mut self.untaken_proofs);
        let mut proofs: Proofs<'_> = HashMap::new();
        for p in &untaken {
            proofs.entry(p.of).or_default().push((&p.place, &p.payload));
        }
        let mut minted: HashSet<(Place, Span, JoinRole)> = HashSet::new();
        let mut witnesses: Vec<Witness> = Vec::new();
        let mut markers: Vec<(String, ScopeId, Point)> = Vec::new();

        for (node, var) in assignments {
            let span = node_to_span(node);
            let at = crate::cst::assignment_write_point(node);
            let sid = self.innermost_scope_id_at(span.start);
            let (binding, home) = self.binding_of(&var, at, sid);
            let reach = self.reach(sid, home);
            let place = Place::variable(binding.clone());

            // Innermost first, inside the reach.
            // An arm ending where the write lands (`$x = $c ? 1 : 2`'s `2`)
            // is part of the value written, not around the write.
            let born = match &binding {
                Binding::Lexical(sym) => Some(self.symbols[sym.0 as usize].span.start),
                _ => None,
            };
            let holding: Vec<&Construct> = plan
                .arms_holding(at)
                .filter(|(_, a)| contains(a.span, span))
                .take_while(|(_, a)| contains(reach.within, a.span))
                .filter(|(c, _)| contains(reach.within, c.span))
                .filter(|(c, _)| born.is_none_or(|b| !plan.binds_each_pass(c, b)))
                .map(|(c, _)| c)
                .collect();
            for c in &holding {
                // An `elsif` (a branch that is wholly another's arm) joins
                // with the construct it is the arm of.
                if plan.is_lifted(c) {
                    continue;
                }
                let joins = match c.kind {
                    ControlKind::Branch => plan.branch_joins(c, None, self.scopes[sid.0 as usize].span),
                    ControlKind::Loop { .. } => plan.loop_joins(c, None),
                    ControlKind::Block { .. } => continue,
                };
                // Held to the scope around the construct, so a sibling arm
                // never sees them.
                let scope = reach.joins_scope(&self.scopes, c.span, &joins);
                self.mint(&var, scope, &place, joins, &proofs, &mut minted, &mut witnesses);
            }

            // A block no construct runs conditionally (a bare block, `eval`,
            // a `map` callback) still bounds the write; past it the
            // variable is unknown, as no join says what the block left.
            let upto = |end: ScopeId| reach.chain.iter().copied().take_while(move |&s| s != end).zip(reach.chain.iter().copied().skip(1));
            for (level, parent) in upto(reach.stop) {
                let block = self.scopes[level.0 as usize].span;
                // A block a construct joins (its arm, or the construct when it
                // is the scope, as `for my $i` is) is already accounted for.
                if !holding.iter().any(|c| c.span == block || c.arms.iter().any(|a| a.span == block)) {
                    markers.push((var.clone(), parent, self.statement_end_holding(node, block)));
                }
            }

            // TODO(closure-call-writes): a write in a callable reaches its
            // outer variable only where the callable is called (design
            // decision 6, owned by whole-program analysis); until calls are
            // visible the variable is unknown from the callable on.
            let pos = |id: ScopeId| reach.chain.iter().position(|&s| s == id);
            if !reach.home_inside && matches!(binding, Binding::Lexical(_)) && pos(home).is_some() {
                for (level, parent) in upto(home).skip_while(|&(l, _)| l != reach.callable) {
                    let block = self.scopes[level.0 as usize].span;
                    markers.push((var.clone(), parent, self.statement_end_holding(node, block)));
                }
            }
        }

        // A test changes nothing a place holds, so its joins never replace
        // a value: each holds as a region past its construct, and where it
        // answers nothing the value coming in still stands. One is minted
        // only where it narrows: every path out of the construct carries a
        // proof about the place, and some path is gone.
        let narrowed = std::mem::take(&mut self.narrowed_arms);
        for seed in &seeds {
            for (place, key_vars) in &seed.places {
                let home = self.home_of(&place.root, seed.scope);
                let reach = self.reach(seed.scope, home);
                let bound_from = match &place.root {
                    Binding::Lexical(sym) => Some(self.symbols[sym.0 as usize].span.start),
                    _ => None,
                };
                let joins = plan.guard_joins(
                    node_to_span(seed.construct),
                    reach.within,
                    bound_from,
                    |of| proofs.get(&of).is_some_and(|ps| ps.iter().any(|(p, _)| *p == place)),
                    |arm| narrowed.iter().any(|(p, over)| p == place && contains(*over, arm)),
                );
                for (k, after) in joins {
                    if !minted.insert((place.clone(), after.of, after.role)) {
                        continue;
                    }
                    let scope = self.holding_scope(&reach, k.span);
                    witnesses.extend(self.guard_region(place, key_vars, scope, after, &proofs));
                }
            }
        }

        for w in witnesses.into_iter().chain(effect_arms) {
            self.push_witness(w);
        }
        for (variable, scope, at) in markers {
            self.push_reset_marker(variable, scope, at);
        }
    }

    /// The file's subs by name, for [`Self::local_callee`].
    fn local_callables(&self) -> HashMap<String, Vec<(SymbolId, Option<String>)>> {
        let mut out: HashMap<String, Vec<(SymbolId, Option<String>)>> = HashMap::new();
        for s in self.symbols.iter().filter(|s| matches!(s.kind, SymKind::Sub | SymKind::Method)) {
            out.entry(s.name.clone()).or_default().push((s.id, s.package.clone()));
        }
        out
    }

    /// The sub of this file a plain call at `node` runs, by the rule its
    /// `Invoke` resolves by (`find_callee_symbol`).
    fn local_callee(&self, local: &HashMap<String, Vec<(SymbolId, Option<String>)>>, node: Node<'a>) -> Option<SymbolId> {
        if !matches!(node.kind(), "function_call_expression" | "ambiguous_function_call_expression")
            || self.is_bless_call(node)
            || self.indirect_method_call(node).is_some()
        {
            return None;
        }
        let name = self.forward_callee_name(node)?;
        let (qualifier, bare) = crate::model::file_analysis::split_qualified(&name, &crate::model::conventions::PERL_SPELLINGS);
        local.get(bare)?.iter().find(|(_, pkg)| super::visit_use::qualifier_admits_local_sub(qualifier, pkg.as_deref())).map(|(id, _)| *id)
    }

    /// The body block of the sub `sid` declares, where `root`'s tree holds it.
    fn callable_body(&self, sid: SymbolId, root: Node<'a>) -> Option<Node<'a>> {
        let scope = &self.scopes[self.owner_scope.get(&sid)?.0 as usize];
        let decl = root.descendant_for_point_range(scope.span.start, scope.span.end)?;
        decl.child_by_field_name("body").filter(|b| b.kind() == "block")
    }

    /// How a call to `sid` ends, from its body: a way out per leave and,
    /// where control reaches it, the body's end past the calls it makes
    /// there (`JoinPlan::body_ends`). A `return` (or `goto`) anywhere in it,
    /// in a nested sub's body too, comes back: one too many only says a call
    /// may come back. A body the parser could not read states nothing, the top.
    // TODO(effect-leaving-bits): how a body leaves through a call that does
    // not come back (unwinds or exits) is not minted, so a pure wrapper ends
    // in no named way; the first reader of the split is try arms (PR 13).
    fn effect_arms(&self, plan: &JoinPlan<'_>, sid: SymbolId, body: Node<'a>, returns: &[Span]) -> Vec<Witness> {
        use crate::model::witnesses::{Effect, Ends};
        if body.has_error() {
            return Vec::new();
        }
        let attachment = WitnessAttachment::SymbolEffects(sid);
        let arm = |payload: WitnessPayload, at: Point| Witness {
            attachment: attachment.clone(),
            source: WitnessSource::Builder(EFFECT_ARM_SOURCE.into()),
            payload,
            span: Span { start: at, end: at },
        };
        let mut out: Vec<Witness> = plan
            .body_ends(node_to_span(body))
            .into_iter()
            .map(|(ends, at, gates)| {
                let effect = WitnessPayload::Effect(Effect::Ends(ends));
                let payload = if gates.is_empty() {
                    effect
                } else {
                    WitnessPayload::Gated { gates: gates.into_iter().map(WitnessAttachment::Expr).collect(), arm: Some(Box::new(effect)) }
                };
                arm(payload, at)
            })
            .collect();
        let within = node_to_span(body);
        if let Some(ret) = returns.iter().find(|&&r| contains(within, r)) {
            out.push(arm(WitnessPayload::Effect(Effect::Ends(Ends::COMPLETES)), ret.start));
        }
        out
    }

    /// The joins of a variable, each landing as a write (`Reset` and its
    /// edge), held to `scope` so a sibling arm never sees it.
    #[allow(clippy::too_many_arguments)]
    fn mint(
        &self,
        var: &str,
        scope: ScopeId,
        place: &Place,
        joins: Vec<BranchJoin>,
        proofs: &Proofs<'_>,
        minted: &mut HashSet<(Place, Span, JoinRole)>,
        out: &mut Vec<Witness>,
    ) {
        for BranchJoin { role, of, lands, arms } in joins {
            if !minted.insert((place.clone(), of, role)) {
                continue;
            }
            let join = WitnessAttachment::Join { place: Box::new(place.clone()), of, role };
            for at in lands {
                out.push(self.join_point(var, scope, at, WitnessPayload::Reset));
                out.push(self.join_point(var, scope, at, WitnessPayload::Edge(join.clone())));
            }
            out.extend(join_arms(&join, place, &arms, proofs));
        }
    }

    /// The join a test makes, holding as a region from past its construct
    /// to the end of `scope`, cut where the place (or a key its subscripts
    /// read) is first written or disturbed.
    fn guard_region(&self, place: &Place, key_vars: &[String], scope: ScopeId, join: BranchJoin, proofs: &Proofs<'_>) -> Vec<Witness> {
        let end = self.scopes[scope.0 as usize].span.end;
        guard_region_witnesses(place, join, end, proofs, |region| {
            let disturbed = match place.is_variable() {
                true => self.rebinds.earliest_in(place, region),
                false => self.place_disturbances.as_ref().and_then(|idx| idx.earliest_in(place, region)),
            };
            let at = region.start;
            let rebound = key_vars.iter().map(|kv| {
                let var = Place::variable(self.binding_of(kv, at, scope).0);
                self.rebinds.earliest_in(&var, region)
            });
            crate::model::file_analysis::cut_region(region, std::iter::once(disturbed).chain(rebound))
        })
    }

    /// The scopes from `sid` out, and where joins minted from there stop.
    fn reach(&self, sid: ScopeId, home: ScopeId) -> Reach {
        Reach::of(&self.scopes, sid, home)
    }

    /// The innermost scope of the reach holding `span`.
    fn holding_scope(&self, reach: &Reach, span: Span) -> ScopeId {
        reach.holding_scope(&self.scopes, span)
    }

    /// The scope a binding lives in, as `binding_of` answers it.
    fn home_of(&self, binding: &Binding, scope: ScopeId) -> ScopeId {
        match binding {
            Binding::Lexical(sym) | Binding::Receiver(sym) => self.symbols[sym.0 as usize].scope,
            Binding::Arguments(callable) => *callable,
            Binding::Package(..) => crate::model::file_analysis::outermost_scope(&self.scopes, scope),
        }
    }

    /// One witness on the variable at a join point, held to `scope` so a
    /// sibling arm never sees it.
    fn join_point(&self, var: &str, scope: ScopeId, at: Point, payload: WitnessPayload) -> Witness {
        Witness {
            attachment: WitnessAttachment::Variable { name: var.to_string(), scope },
            source: WitnessSource::Builder(JOIN_SOURCE.into()),
            payload,
            span: Span { start: at, end: at },
        }
    }

    /// The end of the statement that holds `block`, seen from `node` inside
    /// it: where the block rejoins its parent.
    fn statement_end_holding(&self, node: Node<'a>, block: Span) -> Point {
        self.syntax
            .ancestors(node)
            .find(|a| a.kind().ends_with("_statement") && a.start_position() <= block.start && a.end_position() >= block.end)
            .map_or(block.end, |a| a.end_position())
    }
}

fn contains(outer: Span, inner: Span) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}
