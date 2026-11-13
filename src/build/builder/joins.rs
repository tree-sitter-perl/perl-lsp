//! Where control paths rejoin, a rebound variable holds what it holds at the
//! end of every path that reaches the join (`docs/adr/cfg-tier.md`). This
//! phase mints those joins once, from the control document and the bound
//! writes; the registry folds them at query time.

use super::*;
use crate::model::file_analysis::{Arm, Binding, Construct, ControlKind, JoinPlan, Place};
use crate::model::witnesses::{JoinRole, Witness, WitnessAttachment, WitnessPayload, WitnessSource, JOIN_SOURCE};
use std::collections::{HashMap, HashSet};

impl<'a> Builder<'a> {
    /// For every rebind, a join past each branch whose arm holds it, from
    /// the write out to the variable's binding (or the callable the write
    /// runs in). A construct is minted once per variable, however many of
    /// its arms write.
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
        if assignments.is_empty() {
            return;
        }
        let main = self.syntax.main().root().ts();
        let mut seen: HashSet<usize> = HashSet::new();
        let around: Vec<Node<'a>> = assignments
            .iter()
            .filter(|(n, _)| self.syntax.root_of(*n) == Some(main))
            .flat_map(|&(n, _)| std::iter::once(n).chain(self.syntax.ancestors(n)))
            .filter(|n| seen.insert(n.id()))
            .collect();
        let view = super::control::control_view_at(around, self.source);
        // Every arm by its span: a write finds the arms holding it among
        // its own ancestors.
        let mut arm_at: HashMap<Span, Vec<(usize, usize)>> = HashMap::new();
        for (ci, c) in view.constructs.iter().enumerate() {
            for (ai, a) in c.arms.iter().enumerate() {
                arm_at.entry(a.span).or_default().push((ci, ai));
            }
        }
        let plan = JoinPlan::new(&view);
        let mut minted: HashSet<(Binding, Span, JoinRole)> = HashSet::new();
        let mut witnesses: Vec<Witness> = Vec::new();
        let mut markers: Vec<(String, ScopeId, Point)> = Vec::new();
        for (node, var) in assignments {
            let span = node_to_span(node);
            let at = crate::cst::assignment_write_point(node);
            let sid = self.innermost_scope_id_at(span.start);
            let (binding, home) = self.binding_of(&var, at, sid);
            // The write's scopes, innermost first: where each join lands,
            // and the blocks the write leaves on its way to its binding.
            let chain = crate::model::file_analysis::scope_chain_of(&self.scopes, sid);
            let pos = |id: ScopeId| chain.iter().position(|&s| s == id);
            let callable = chain.iter().copied().find(|&s| self.scopes[s.0 as usize].kind.is_callable()).unwrap_or(*chain.last().unwrap_or(&sid));
            // The write's joins stop at whichever is nearer: the variable's
            // binding or the callable the write runs in.
            let home_inside = pos(home).is_some_and(|h| pos(callable).is_some_and(|c| h <= c));
            let stop = if home_inside { home } else { callable };
            let within = self.scopes[stop.0 as usize].span;
            let holding_scope = |span: Span| {
                chain
                    .iter()
                    .copied()
                    .find(|&s| {
                        let ss = self.scopes[s.0 as usize].span;
                        ss.start <= span.start && span.end <= ss.end
                    })
                    .unwrap_or(stop)
            };

            // Innermost first, inside `within`.
            let holding: Vec<(&Construct, &Arm)> = std::iter::once(node)
                .chain(self.syntax.ancestors(node))
                .map(node_to_span)
                .take_while(|a| within.start <= a.start && a.end <= within.end)
                .filter_map(|a| arm_at.get(&a))
                .flatten()
                .map(|&(ci, ai)| (&view.constructs[ci], &view.constructs[ci].arms[ai]))
                .filter(|(c, _)| within.start <= c.span.start && c.span.end <= within.end)
                .collect();
            for (c, arm) in &holding {
                // An `elsif` (a branch that is wholly another's arm) joins
                // with the construct it is the arm of.
                if plan.is_lifted(c) {
                    continue;
                }
                let scope = holding_scope(c.span);
                match c.kind {
                    ControlKind::Branch => {
                        for crate::model::file_analysis::BranchJoin { role, of, lands, arms } in plan.branch_joins(c, None, self.scopes[sid.0 as usize].span) {
                            if !minted.insert((binding.clone(), of, role)) {
                                continue;
                            }
                            let join = WitnessAttachment::Join { place: Box::new(Place::variable(binding.clone())), of, role };
                            for at in lands {
                                witnesses.push(self.join_point(&var, scope, at, WitnessPayload::Reset));
                                witnesses.push(self.join_point(&var, scope, at, WitnessPayload::Edge(join.clone())));
                            }
                            let read = WitnessAttachment::Place(Box::new(Place::variable(binding.clone())));
                            for arm_at in arms {
                                witnesses.push(Witness {
                                    attachment: join.clone(),
                                    source: WitnessSource::Builder(JOIN_SOURCE.into()),
                                    payload: WitnessPayload::Edge(read.clone()),
                                    span: Span { start: arm_at, end: arm_at },
                                });
                            }
                        }
                    }
                    // TODO(cfg-loops): a loop's head and exit joins (design
                    // PR 7); until then a write in a modifier loop leaves its
                    // variable unknown past the loop, and a loop block is a
                    // block like any other (below).
                    ControlKind::Loop { .. } => {
                        if !self.is_block_at(arm.span) && minted.insert((binding.clone(), c.span, JoinRole::After)) {
                            markers.push((var.clone(), scope, c.span.end));
                        }
                    }
                }
            }

            // A block no construct runs conditionally (a bare block, `eval`,
            // a `map` callback) still bounds the write; past it the
            // variable is unknown, as no join says what the block left.
            let upto = |end: ScopeId| chain.iter().copied().take_while(move |&s| s != end).zip(chain.iter().copied().skip(1));
            for (level, parent) in upto(stop) {
                let block = self.scopes[level.0 as usize].span;
                if !holding.iter().any(|(c, a)| c.kind == ControlKind::Branch && a.span == block) {
                    markers.push((var.clone(), parent, self.statement_end_holding(node, block)));
                }
            }

            // TODO(closure-call-writes): a write in a callable reaches its
            // outer variable only where the callable is called (design
            // decision 6, owned by whole-program analysis); until calls are
            // visible the variable is unknown from the callable on.
            if !home_inside && matches!(binding, Binding::Lexical(_)) && pos(home).is_some() {
                for (level, parent) in upto(home).skip_while(|&(l, _)| l != callable) {
                    let block = self.scopes[level.0 as usize].span;
                    markers.push((var.clone(), parent, self.statement_end_holding(node, block)));
                }
            }
        }
        for w in witnesses {
            self.push_witness(w);
        }
        for (variable, scope, at) in markers {
            self.push_reset_marker(variable, scope, at);
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

    /// Whether the node spanning exactly `span` is a block.
    fn is_block_at(&self, span: Span) -> bool {
        self.syntax
            .main()
            .root()
            .ts()
            .descendant_for_point_range(span.start, span.end)
            .is_some_and(|n| n.kind() == "block" && node_to_span(n) == span)
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
