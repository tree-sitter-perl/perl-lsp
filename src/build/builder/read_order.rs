//! Where a read is evaluated against the order its source is written in:
//! a statement modifier's head (`STMT if G`, `STMT for LIST`) runs before
//! the statement written ahead of it (`docs/adr/flow-narrowing.md`
//! §Evaluation order). Minted once per file from the modifier nodes, before
//! the walk, so every reader of a node in a head asks the same table
//! whenever it reads it: the walk at the node, the narrowing at the
//! modifier, a post-walk pass over the refs.

use super::*;

/// One region of the table: a modifier's head, or a callable whose body
/// runs when called and so is evaluated before nothing around it.
struct Region {
    span: Span,
    /// The statement the head runs before; `None` for a callable.
    before: Option<Span>,
    /// The innermost region holding this one.
    holder: Option<u32>,
}

/// The modifier heads and callables of one file, sorted by start (outer
/// first on a tie), each linked to the innermost region holding it. CST
/// spans nest or are disjoint, so the regions holding a span are the
/// ancestors of the last region that starts at or before it.
pub(super) struct ReadOrder {
    regions: Vec<Region>,
}

impl ReadOrder {
    pub(super) fn of(syn: &crate::cst::SyntaxForest<'_>) -> Self {
        static KINDS: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
        let kinds = KINDS.get_or_init(|| {
            let mut names: Vec<&str> = crate::cst::MODIFIER_KINDS.to_vec();
            names.extend_from_slice(crate::cst::CALLABLE_KINDS);
            perl_kind_ids(&names)
        });
        let mut regions: Vec<Region> = Vec::new();
        for n in syn.file_nodes_of_kind(kinds) {
            let n = n.ts();
            if crate::cst::is_callable_boundary(n.kind()) {
                regions.push(Region { span: node_to_span(n), before: None, holder: None });
            } else if let Some((stmt, head)) = crate::cst::modifier_runs_head_first(n) {
                regions.push(Region { span: node_to_span(head), before: Some(node_to_span(stmt)), holder: None });
            }
        }
        // A callable no head holds changes no answer; dropping it keeps the
        // table to the files that have modifiers.
        if regions.iter().all(|r| r.before.is_none()) {
            return ReadOrder { regions: Vec::new() };
        }
        regions.sort_by(|a, b| a.span.start.cmp(&b.span.start).then(b.span.end.cmp(&a.span.end)));
        let mut open: Vec<u32> = Vec::new();
        for i in 0..regions.len() {
            while let Some(&top) = open.last() {
                if regions[top as usize].span.end >= regions[i].span.end {
                    break;
                }
                open.pop();
            }
            regions[i].holder = open.last().copied();
            open.push(i as u32);
        }
        ReadOrder { regions }
    }

    /// The statement a read at `span` is evaluated before though it follows
    /// it: the innermost head holding `span`, unless a callable sits
    /// between them.
    // TODO(nested-modifier-skips): a read in a head nested in another head
    // (`$o = Bar->new if do { 1 if $o->alive }`) is evaluated before both
    // statements; a `ReadPoint` skips one, so the outer one still reaches it.
    pub(super) fn before(&self, span: Span) -> Option<Span> {
        let last = self.regions.partition_point(|r| r.span.start <= span.start).checked_sub(1)?;
        let mut at = Some(last as u32);
        while let Some(i) = at {
            let r = &self.regions[i as usize];
            if r.span.start <= span.start && span.end <= r.span.end {
                return r.before;
            }
            at = r.holder;
        }
        None
    }
}

impl<'a> Builder<'a> {
    /// The order fact a ref or deref site minted at `span` carries.
    pub(super) fn evaluated_before(&self, span: Span) -> Option<Box<Span>> {
        self.read_order.before(span).map(Box::new)
    }

    /// Where a read of `node` asks.
    pub(super) fn read_point_of(&self, node: Node<'_>) -> crate::model::witnesses::ReadPoint {
        crate::model::witnesses::ReadPoint { at: node.start_position(), skips: self.read_order.before(node_to_span(node)) }
    }
}
