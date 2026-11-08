//! `@link.*`: structure the document states one level at a time, chained
//! here to any depth. A tree-sitter pattern has a fixed depth, so a
//! declarator wrapped any number of times or a name qualified any number of
//! times cannot be one pattern; the document links each level to the next,
//! and the driver follows the links by node identity. Nothing here reads a
//! node kind or a field: which node links to which is the document's call.
//! Each link belongs to the walk its `@link.from.<walk>` names, and each walk
//! here reads only its own.

use super::*;
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct Links<'t> {
    to: HashMap<(LinkWalk, usize, LinkCap), tree_sitter::Node<'t>>,
    /// What the document says a `deref` level or leaf is, by node id.
    marks: HashMap<usize, Mark>,
}

/// A `@deref.*` capture on one node.
#[derive(Clone, Copy)]
enum Mark {
    Level(DerefCap),
    Leaf(DefCap),
}

/// A declarator chain read to its end: the name token, one step per level
/// (outermost first, `Box*&` → `[Pointer, Reference]`), and the def the
/// leaf mints.
pub(crate) struct Peeled<'t> {
    pub(crate) leaf: tree_sitter::Node<'t>,
    pub(crate) stack: Vec<crate::model::file_analysis::DerefStep>,
    pub(crate) def: DefCap,
}

impl<'t> Links<'t> {
    pub(super) fn record(
        &mut self,
        walk: LinkWalk,
        from: tree_sitter::Node<'t>,
        rel: LinkCap,
        to: tree_sitter::Node<'t>,
    ) {
        self.to.insert((walk, from.id(), rel), to);
    }

    pub(super) fn mark_level(&mut self, node: tree_sitter::Node<'t>, cap: DerefCap) {
        self.marks.insert(node.id(), Mark::Level(cap));
    }

    pub(super) fn mark_leaf(&mut self, node: tree_sitter::Node<'t>, def: DefCap) {
        self.marks.insert(node.id(), Mark::Leaf(def));
    }

    fn get(&self, walk: LinkWalk, node: tree_sitter::Node<'t>, rel: LinkCap) -> Option<tree_sitter::Node<'t>> {
        self.to.get(&(walk, node.id(), rel)).copied()
    }

    /// The name a scope segment contributes: a templated name's base
    /// (`Buf<T>::grow` files under `Buf`), else the node's own text.
    fn base_text(&self, walk: LinkWalk, node: tree_sitter::Node<'t>, src: &[u8]) -> String {
        let named = self.get(walk, node, LinkCap::Base).unwrap_or(node);
        named.utf8_text(src).unwrap_or("").to_string()
    }

    /// The `qualifier` walk: a `@qualifier` owner's name.
    pub(super) fn qualifier_text(&self, node: tree_sitter::Node<'t>, src: &[u8]) -> String {
        self.base_text(LinkWalk::Qualifier, node, src)
    }

    /// The `ool` walk: an out-of-line definition's owner and member name: the definition's
    /// declarator, through any transparent wrappers, to the function
    /// declarator, whose declared name is qualified (`A::B::m`). Returns the
    /// innermost scope's name (the owning class) and the leaf name node.
    /// `None` when the declared name is unqualified: a free function or an
    /// in-class method, which its own pattern owns.
    pub(super) fn qualified_def(
        &self,
        def: tree_sitter::Node<'t>,
        src: &[u8],
    ) -> Option<(String, tree_sitter::Node<'t>)> {
        const W: LinkWalk = LinkWalk::Ool;
        let mut decl = self.get(W, def, LinkCap::Declares)?;
        let mut node = loop {
            if let Some(declared) = self.get(W, decl, LinkCap::Declares) {
                break declared;
            }
            decl = self.get(W, decl, LinkCap::Inner)?;
        };
        self.get(W, node, LinkCap::Name)?;
        let mut owner = String::new();
        while let Some(name) = self.get(W, node, LinkCap::Name) {
            if let Some(scope) = self.get(W, node, LinkCap::Scope) {
                owner = self.base_text(W, scope, src);
            }
            node = name;
        }
        Some((owner, node))
    }

    /// The `spelled` walk: a reference named by its whole spelling, from the
    /// node its `@ref.<kind>` tags down each level's `name` (and a templated
    /// name's `base`) to the token the reference rewrites. The scopes the
    /// qualified levels name on the way come with it, outermost first
    /// (`a::b::f` → `f`, `[a, b]`; `::f` → `f`, `[]`), or `None` when no
    /// level was qualified. `None` when the node has no level below it: the
    /// document already put the reference on its token.
    pub(super) fn spelled_leaf(
        &self,
        mut node: tree_sitter::Node<'t>,
        src: &[u8],
    ) -> Option<(tree_sitter::Node<'t>, Option<Vec<String>>)> {
        const W: LinkWalk = LinkWalk::Spelled;
        let tagged = node.id();
        let mut scopes: Option<Vec<String>> = None;
        loop {
            if let Some(name) = self.get(W, node, LinkCap::Name) {
                let level = scopes.get_or_insert_with(Vec::new);
                if let Some(scope) = self.get(W, node, LinkCap::Scope) {
                    level.push(scope.utf8_text(src).unwrap_or("").to_string());
                }
                node = name;
            } else if let Some(base) = self.get(W, node, LinkCap::Base) {
                node = base;
            } else {
                break;
            }
        }
        (node.id() != tagged).then_some((node, scopes))
    }

    /// The `deref` walk: a declarator chain from its `@nested.target` to the
    /// leaf the document marked, one step per pointer or reference level,
    /// each carrying the cv-qualifiers marked among its children. `None` when
    /// the chain reaches no marked leaf, which mints nothing rather than a
    /// half-read shape.
    pub(crate) fn peel_declarator(&self, mut node: tree_sitter::Node<'t>, src: &[u8]) -> Option<Peeled<'t>> {
        use crate::model::file_analysis::{DerefKind, DerefStep};
        let mut stack = Vec::new();
        for _ in 0..32 {
            let kind = match self.marks.get(&node.id()) {
                Some(Mark::Leaf(def)) => return Some(Peeled { leaf: node, stack, def: *def }),
                Some(Mark::Level(DerefCap::Pointer)) => DerefKind::Pointer,
                Some(Mark::Level(DerefCap::Ref)) => DerefKind::Reference,
                Some(Mark::Level(DerefCap::Pass)) => {
                    node = self.get(LinkWalk::Deref, node, LinkCap::Inner)?;
                    continue;
                }
                _ => return None,
            };
            let mut cur = node.walk();
            let annotations = node
                .children(&mut cur)
                .filter(|ch| matches!(self.marks.get(&ch.id()), Some(Mark::Level(DerefCap::Annot))))
                .filter_map(|ch| ch.utf8_text(src).ok().map(str::to_string))
                .collect();
            stack.push(DerefStep { kind, annotations });
            node = self.get(LinkWalk::Deref, node, LinkCap::Inner)?;
        }
        None
    }
}
