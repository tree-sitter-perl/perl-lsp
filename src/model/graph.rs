//! The typed-edge graph — one walker over what is morally one graph.
//!
//! A DERIVED view, no stored graph. Edges materialize on demand from
//! the stores that already exist (`PackageFacts::parents` ∪ `parents_of`'s
//! synthetic app-surface edge, the `ModuleEdgeIndexes` children map,
//! plugin-namespace bridges); `walk` is the single traversal — seen-
//! set, depth cap, edge-kind mask — that the ancestry/bridge/descendant
//! queries route through. Design: `docs/adr/graph-walking.md`.
//!
//! `GraphView` consumes `&FileAnalysis` + the `CrossFileLookup` trait
//! and answers queries; `FileAnalysis` stays the canonical model (rule
//! #2), and the builder never touches this.
//!
//! Model layer: a derived view over `&FileAnalysis` and the model-
//! defined `CrossFileLookup` trait, with zero Index-layer deps — so the
//! model-internal walkers (`for_each_ancestor_class` and the dispatch/
//! method/bridge resolution that funnels through it) call `walk`
//! directly, no up-layer import.

use crate::model::file_analysis::{CrossFileLookup, FileAnalysis};

/// One typed edge family. The CLOSED set of edge kinds — `edges_from`
/// matches on this exhaustively, so adding a kind is a compile error
/// until its derivation is written (the design doc's "one match site,
/// never a parallel walker" invariant, with compiler teeth). The
/// bitflag MASK below is set membership over this enum, not a separate
/// source of truth: `EdgeKind::ALL` + `flag()` keep them in lockstep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeKind {
    /// class → parent class/role (`use parent`/`@ISA`/`with`/…).
    /// `real_parents_of` is the single derivation — the inheritance
    /// consumers share it, so they can't disagree on the MRO. The
    /// synthetic app-surface edge is its own kind (`AppSurface`) so
    /// walks that must not treat a consumer as a descendant of the
    /// surface (isa gates, trigger views) can mask it off; full-MRO
    /// walks pass `INHERITS | APP_SURFACE`.
    Inherits,
    /// class → the synthetic `APP_SURFACE_CLASS` parent for
    /// manifest-declared app-surface consumers (the Mojo helper/plugin
    /// "app surface", `docs/adr/plugin-system.md`). Split from
    /// `Inherits` so it is maskable; `app_surface_parent` is the one
    /// speller of the edge condition.
    AppSurface,
    /// parent → direct child/composer (the `children_index` inverse;
    /// `walk` supplies the transitivity).
    InheritsInv,
    /// class → modules whose plugin namespaces bridge to it. Module
    /// nodes are terminal — bridge edges don't compose.
    Bridges,
    /// primary template → its specializations (the family view:
    /// goto-implementation enumerates them). NOT an inheritance edge —
    /// a specialization REPLACES the primary's member table wholesale,
    /// so member resolution must NEVER traverse this (a spec that also
    /// really inherits carries a separate `Inherits` edge).
    Specializes,
}

impl EdgeKind {
    /// Every variant. New kinds MUST be added here — the `edges_from`
    /// loop iterates it, so a forgotten kind is never traversed (and
    /// its `flag()` arm + match arm are compile errors meanwhile).
    pub const ALL: [EdgeKind; 5] = [
        Self::Inherits,
        Self::AppSurface,
        Self::InheritsInv,
        Self::Bridges,
        Self::Specializes,
    ];

    fn flag(self) -> EdgeKindMask {
        match self {
            EdgeKind::Inherits => EdgeKindMask::INHERITS,
            EdgeKind::AppSurface => EdgeKindMask::APP_SURFACE,
            EdgeKind::InheritsInv => EdgeKindMask::INHERITS_INV,
            EdgeKind::Bridges => EdgeKindMask::BRIDGES,
            EdgeKind::Specializes => EdgeKindMask::SPECIALIZES,
        }
    }
}

bitflags::bitflags! {
    /// A SET of [`EdgeKind`]s a walk may traverse (`INHERITS | BRIDGES`).
    /// Storage + ergonomic `|` only — `EdgeKind` is the source of truth;
    /// the consts here mirror its variants via `EdgeKind::flag()`.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct EdgeKindMask: u8 {
        const INHERITS     = 1 << 0;
        const INHERITS_INV = 1 << 1;
        const BRIDGES      = 1 << 2;
        const SPECIALIZES  = 1 << 3;
        const APP_SURFACE  = 1 << 4;
    }
}

/// A graph node. The class axis + terminal Module nodes for bridges;
/// Scope/Symbol/File nodes are future taxonomy (`adr/graph-walking.md`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Node {
    Class(String),
    Module(String),
}

/// Per-node control verdict a walk visitor returns. `PruneChildren` is
/// what makes gated gathers (the role-requires walk stops at the first
/// non-role node) and scoped views expressible as THE walk instead of a
/// bespoke BFS: it skips expanding the just-visited node's edges while
/// the rest of the traversal continues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkControl {
    /// Keep walking — expand this node's edges.
    Continue,
    /// Don't expand this node's edges; the rest of the walk proceeds.
    PruneChildren,
    /// Stop the whole walk.
    Stop,
}

/// The derived view: borrows the origin file's analysis (local edges)
/// and the index (cross-file edges). Build one per query — it holds no
/// state beyond the borrows.
pub struct GraphView<'a> {
    fa: &'a FileAnalysis,
    idx: Option<&'a dyn CrossFileLookup>,
}

/// Both bound axes of a graph walk, carried by the TYPE so a walk's
/// guarantee is declared at its call site instead of implied by which
/// walker it happened to ride. `max_depth` bounds how FAR from the origin
/// (edges); `max_visits` bounds total WORK (unique nodes visited). A deep
/// chain exhausts one axis, a wide fan-out the other, and neither alone
/// is "terminates in bounded time" for both shapes. The presets preserve
/// each walk family's pre-collapse guarantee EXACTLY — tightening either
/// is a deliberate, corpus-measured change to a preset constant, never a
/// side effect of routing (the divergent cases are pinned:
/// `deep_isa_chain_within_visit_budget_still_resolves`,
/// `wide_fanout_enumerates_completely_despite_any_visit_budget`).
#[derive(Clone, Copy)]
pub struct WalkBound {
    pub max_depth: usize,
    pub max_visits: usize,
}

impl WalkBound {
    /// The graph-verb guarantee: ancestry depth capped at 21 (the Perl
    /// MRO backstop), visits bounded only by the seen-set — a wide
    /// fan-out (implementations over a 300-child schema) enumerates
    /// completely.
    pub const GRAPH: WalkBound = WalkBound { max_depth: 21, max_visits: usize::MAX };
    /// The isa-family guarantee: 200 visited classes (set well above any
    /// real MRO), depth unbounded — a deep legitimate chain within the
    /// budget still resolves.
    pub const ISA: WalkBound = WalkBound { max_depth: usize::MAX, max_visits: 200 };
}

/// THE bounded DFS — the one loop under both `GraphView::walk` and
/// `walk_ancestry` (`docs/adr/sibling-forks.md`: the engines were the
/// duplicated half; edge derivation was already single-sourced per
/// family). Seen-set cycle-safety; visit-at-pop so a left parent's whole
/// ancestry precedes the right parent (the @ISA contract — edge sources
/// are reverse-pushed to preserve their order under LIFO); the origin is
/// never visited (depth 0 is the caller's hand). A node AT `max_depth`
/// is visited but not expanded; a visit past `max_visits` stops the walk.
///
/// Returns whether the walk was TRUNCATED — a node went unexpanded because a
/// bound cut it off, so the visitor saw less than the graph holds. A caller
/// that merely wants a best-effort enumeration ignores it; a caller whose
/// correctness rests on having seen the WHOLE reachable set must not, because
/// truncation is otherwise silent and looks exactly like a small graph.
pub(crate) fn bounded_dfs<N: std::hash::Hash + Eq + Clone>(
    origin: N,
    bound: WalkBound,
    mut edges: impl FnMut(&N, &mut Vec<N>),
    visit: &mut dyn FnMut(&N) -> WalkControl,
) -> bool {
    let mut truncated = false;
    let mut seen: std::collections::HashSet<N> = std::collections::HashSet::new();
    seen.insert(origin.clone());
    let mut stack: Vec<(N, usize)> = vec![(origin, 0)];
    let mut visits = 0usize;
    let mut next: Vec<N> = Vec::new();
    while let Some((node, depth)) = stack.pop() {
        if depth > 0 {
            if visits >= bound.max_visits {
                return true;
            }
            visits += 1;
            match visit(&node) {
                WalkControl::Continue => {}
                WalkControl::PruneChildren => continue,
                WalkControl::Stop => return truncated,
            }
        }
        if depth >= bound.max_depth {
            // Visited but not expanded: anything below it is invisible to the
            // visitor and indistinguishable from absence.
            truncated = true;
            continue;
        }
        next.clear();
        edges(&node, &mut next);
        for n in next.drain(..).rev() {
            if seen.insert(n.clone()) {
                stack.push((n, depth + 1));
            }
        }
    }
    truncated
}

impl<'a> GraphView<'a> {
    pub fn new(fa: &'a FileAnalysis, idx: Option<&'a dyn CrossFileLookup>) -> Self {
        GraphView { fa, idx }
    }

    /// THE graph walker's public face. DFS from `origin` over edges in
    /// `mask` at [`WalkBound::GRAPH`]; `visit` sees every reached node
    /// (origin excluded) in traversal order and answers with a
    /// [`WalkControl`] verdict. On INHERITS the order is Perl's
    /// left-to-right DFS MRO, so method resolution sees ancestors in the
    /// order dispatch demands.
    ///
    /// Returns whether the walk was truncated by a bound — see
    /// [`bounded_dfs`]. Best-effort callers ignore it; a caller claiming to
    /// have seen the whole reachable set must not.
    pub fn walk(
        &self,
        origin: Node,
        mask: EdgeKindMask,
        visit: &mut dyn FnMut(&Node) -> WalkControl,
    ) -> bool {
        bounded_dfs(
            origin,
            WalkBound::GRAPH,
            |node, out| self.edges_from(node, mask, out),
            visit,
        )
    }

    /// Edge derivation — the ONE place graph structure comes from. The
    /// `match` is EXHAUSTIVE over `EdgeKind`, so a new kind can't be
    /// added without writing its derivation here (never a parallel
    /// walker).
    fn edges_from(&self, node: &Node, mask: EdgeKindMask, out: &mut Vec<Node>) {
        let Node::Class(class) = node else { return };
        for kind in EdgeKind::ALL {
            if !mask.contains(kind.flag()) {
                continue;
            }
            match kind {
                EdgeKind::Inherits => {
                    for p in crate::model::file_analysis::real_parents_of(
                        class,
                        &self.fa.packages,
                        self.idx,
                    ) {
                        out.push(Node::Class(p));
                    }
                }
                EdgeKind::AppSurface => {
                    if let Some(s) = crate::model::file_analysis::app_surface_parent(
                        class,
                        &self.fa.plugin.app_surface_consumers,
                    ) {
                        out.push(Node::Class(s));
                    }
                }
                EdgeKind::InheritsInv => {
                    // Local children: a class in THIS file naming `class` as a
                    // parent (same-file hierarchies — many structs pasting one
                    // member-block role macro, a single-file Perl `@ISA` chain).
                    // Symmetric with `parents_of` (local ∪ cross-file); the walk's
                    // seen-set dedups against the cross-file index below.
                    for (child, parents) in self.fa.package_parent_edges() {
                        if parents.iter().any(|p| p == class) {
                            out.push(Node::Class(child.clone()));
                        }
                    }
                    if let Some(idx) = self.idx {
                        for (pkg, _module) in idx.direct_children_of(class).read() {
                            out.push(Node::Class(pkg));
                        }
                    }
                }
                EdgeKind::Bridges => {
                    if let Some(idx) = self.idx {
                        let mut seen_mods: std::collections::HashSet<String> = Default::default();
                        idx.for_each_entity_bridged_to(class, &mut |module, _cached, _sym| {
                            if seen_mods.insert(module.to_string()) {
                                out.push(Node::Module(module.to_string()));
                            }
                            std::ops::ControlFlow::Continue(())
                        });
                    }
                }
                EdgeKind::Specializes => {
                    // Local specs: this file's (spec → primary) map, inverted.
                    // Deterministic order — HashMap iteration is randomized.
                    let mut local: Vec<&String> = self
                        .fa
                        .pack.specializes
                        .iter()
                        .filter(|(_, primary)| primary.as_str() == class)
                        .map(|(spec, _)| spec)
                        .collect();
                    local.sort();
                    for spec in local {
                        out.push(Node::Class(spec.clone()));
                    }
                    if let Some(idx) = self.idx {
                        for (spec, _module) in idx.direct_specializations_of(class).read() {
                            out.push(Node::Class(spec));
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "graph_tests.rs"]
mod tests;
