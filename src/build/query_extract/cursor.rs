//! The pack document read at cursor time: the member accesses it states
//! over a tree the extractor never saw (the cursor sentinel's reparse,
//! `cursor_sentinel.rs`). Which nodes are member accesses, which is the
//! receiver and which token the operator, is the document's to say; the
//! sentinel asks here instead of keeping a table of node kinds.

use super::*;
use crate::model::file_analysis::{MemberMode, MemberOp, MemberReach, SpaceAsk, SpaceSet, Span};
use std::cell::OnceCell;
use tree_sitter::Node;

/// One member access the document states: a `@ref.member.value` or
/// `@ref.member.call` token and the `@member.recv` its match joins.
#[derive(Clone, Copy)]
pub struct MemberSite<'t> {
    /// The access's own node: its read (`@expr.member`) or its call
    /// (`@expr.call`).
    pub node: Node<'t>,
    pub recv: Node<'t>,
    pub name: Node<'t>,
    pub mode: MemberMode,
    /// The operator token the match names (`@member.op`).
    pub op: Option<Node<'t>>,
}

/// What the document says of an operator token and the member after it.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MemberFacts {
    /// Which operator the token is (`@member.op.<which>`).
    pub op: Option<MemberOp>,
    /// What the operator reaches (`@space.<name>[.<mark>]` on the token);
    /// `None` when it states none.
    pub offers: Option<MemberReach>,
    /// Words written after the operator that name no member
    /// (`#set! member.word`).
    pub words: Vec<String>,
    /// The namespaces the access's own name token searches.
    pub spaces: Option<SpaceAsk>,
    /// The member as written, where that is more than its name
    /// (`@member.spelled`: php's `$x`).
    pub spelled: Option<Span>,
}

/// A pack's document over one parsed buffer.
pub struct CursorDocument<'t, 's> {
    compiled: &'static CompiledPack,
    root: Node<'t>,
    source: &'s [u8],
    /// Built on the first structural predicate, over the whole tree, so a
    /// match rooted anywhere reads its real parent.
    syntax: OnceCell<crate::cst::SyntaxForest<'t>>,
}

impl<'t, 's> CursorDocument<'t, 's> {
    /// `None` when the document does not compile, which extraction reports.
    pub fn new(tree: &'t Tree, source: &'s [u8], pack: &LangPack) -> Option<Self> {
        let compiled = cached_query(&tree.language(), pack).ok()?;
        Some(CursorDocument { compiled, root: tree.root_node(), source, syntax: OnceCell::new() })
    }

    /// Every admitted match rooted in `under`'s subtree, or, with `bytes`,
    /// every one over the whole tree that touches that range.
    fn each_match(
        &self,
        under: Node<'t>,
        bytes: Option<std::ops::Range<usize>>,
        mut f: impl FnMut(usize, &[(Node<'t>, Capture)]),
    ) {
        let mut cursor = QueryCursor::new();
        if let Some(r) = bytes {
            cursor.set_byte_range(r);
        }
        let mut matches = cursor.matches(&self.compiled.query, under, self.source);
        let mut caps: Vec<(Node<'t>, Capture)> = Vec::new();
        while let Some(m) = matches.next() {
            let syntax = || self.syntax.get_or_init(|| crate::cst::SyntaxForest::new(self.root));
            if !crate::build::query_predicates::admits(&self.compiled.query, m, syntax) {
                continue;
            }
            caps.clear();
            caps.extend(m.captures.iter().filter_map(|c| Some((c.node, self.compiled.captures[c.index as usize]?))));
            f(m.pattern_index, &caps);
        }
    }

    /// The member access a match states, if it states one.
    fn site_of(caps: &[(Node<'t>, Capture)]) -> Option<MemberSite<'t>> {
        let find = |want: &dyn Fn(Capture) -> bool| caps.iter().find(|(_, c)| want(*c)).map(|(n, _)| *n);
        let (name, mode) = caps.iter().find_map(|(n, c)| match c {
            Capture::Ref(RefCap::MemberValue) => Some((*n, MemberMode::Value)),
            Capture::Ref(RefCap::MemberCall) => Some((*n, MemberMode::Callable)),
            _ => None,
        })?;
        let recv = find(&|c| matches!(c, Capture::Member(MemberCap::Recv | MemberCap::RecvName)))?;
        let node = find(&|c| matches!(c, Capture::Expr(ExprCap::Member | ExprCap::Call)))?;
        let op = find(&|c| c == Capture::Member(MemberCap::Op));
        Some(MemberSite { node, recv, name, mode, op })
    }

    /// The member access whose name token holds byte `at`.
    pub fn member_named_at(&self, at: usize) -> Option<MemberSite<'t>> {
        let mut found = None;
        self.each_match(self.root, Some(at..at + 1), |_, caps| {
            if found.is_none() {
                found = Self::site_of(caps).filter(|s| s.name.start_byte() <= at && at < s.name.end_byte());
            }
        });
        found
    }

    /// The member access whose own node is `node`.
    pub fn member_at(&self, node: Node<'t>) -> Option<MemberSite<'t>> {
        let mut found = None;
        self.each_match(node, None, |_, caps| {
            if found.is_none() {
                found = Self::site_of(caps).filter(|s| s.node == node);
            }
        });
        found
    }

    /// What the document says of `site`'s operator and name token.
    pub fn facts(&self, site: &MemberSite<'t>) -> MemberFacts {
        let mut facts = MemberFacts::default();
        let query = &self.compiled.query;
        self.each_match(site.node, None, |pattern, caps| {
            for (node, cap) in caps {
                if Some(*node) == site.op {
                    match cap {
                        Capture::MemberOp(which) => facts.op = Some(which.op()),
                        Capture::Space { id, mark } => {
                            let r = facts.offers.unwrap_or_default();
                            let marked = |set: SpaceSet, m| if *mark == Some(m) { set.with(*id) } else { set };
                            facts.offers = Some(MemberReach {
                                spaces: r.spaces.with(*id),
                                forward: marked(r.forward, SpaceMark::Forward),
                                spelled: marked(r.spelled, SpaceMark::Spelled),
                            });
                        }
                        _ => {}
                    }
                    for s in query.property_settings(pattern) {
                        if *s.key == *SettingKey::MemberWord.spelling() {
                            if let Some(w) = s.value.as_deref().filter(|w| !facts.words.iter().any(|x| x == w)) {
                                facts.words.push(w.to_string());
                            }
                        }
                    }
                }
                if *node == site.name {
                    if let Capture::Space { id, mark } = cap {
                        let a = facts.spaces.unwrap_or_default();
                        facts.spaces = Some(SpaceAsk {
                            search: a.search.with(*id),
                            illegal: if *mark == Some(SpaceMark::Illegal) { a.illegal.with(*id) } else { a.illegal },
                        });
                    }
                }
                if *cap == Capture::Member(MemberCap::Spelled)
                    && node.start_byte() <= site.name.start_byte()
                    && site.name.end_byte() <= node.end_byte()
                {
                    facts.spelled = Some(Span { start: node.start_position(), end: node.end_position() });
                }
            }
        });
        facts
    }
}
