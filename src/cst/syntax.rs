//! The core syntax tree: one flat preorder array per parse, built by a
//! single `TreeCursor` pass. Design, measurements and the migration plan:
//! `docs/adr/core-syntax-tree.md`.
//!
//! tree-sitter's own navigation is the wrong shape for analysis: `Node::parent`
//! (and the sibling steps, which go through it) descends from the root on
//! every call, linear in each level's width — a flat 750-statement file pays
//! hundreds of sibling steps per climb. Here a node's id is its preorder
//! index, so parent is a load.
//!
//! The preorder IS tree-sitter's descendant order, and start bytes never
//! decrease along it — that is what makes [`Structure::id_of`] a binary
//! search instead of a map.

use tree_sitter::Node;

/// A node's position in its [`SyntaxForest`]: the preorder index in its
/// parse, offset by every parse registered before it, so one id names a node
/// anywhere in the forest and one [`NodeFacts`] covers every parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub(crate) struct NodeId(u32);

impl NodeId {
    pub(crate) fn index(self) -> usize {
        self.0 as usize
    }
}

const NO_PARENT: u32 = u32::MAX;

struct Entry<'t> {
    // The raw node answers everything the entry does not store yet; a field
    // moves here when the readers that need it port, and the node goes when
    // the last raw reader does.
    ts: Node<'t>,
    parent: u32,
    /// One past the last id of the subtree: a subtree is `id..end`.
    end: u32,
    start_byte: u32,
    end_byte: u32,
}

/// The structure of one parse.
pub(crate) struct Structure<'t> {
    /// The forest id of this parse's root.
    base: u32,
    entries: Vec<Entry<'t>>,
    /// Each node's kind id, a column of its own: a kind scan reads two
    /// bytes per node instead of a 48-byte entry.
    kinds: Vec<u16>,
    /// The nodes another parse replaces, by local index, ascending: each
    /// with the forest index of the parse that holds its code.
    injections: Vec<(u32, usize)>,
}

impl<'t> Structure<'t> {
    fn build(root: Node<'t>, base: u32) -> Self {
        let mut entries: Vec<Entry<'t>> = Vec::with_capacity(root.descendant_count());
        let mut kinds: Vec<u16> = Vec::with_capacity(root.descendant_count());
        let mut cursor = root.walk();
        // Explicit stack, like every tree pass here: a recursive descent
        // costs one frame per CST level.
        let mut open: Vec<u32> = Vec::new();
        loop {
            let node = cursor.node();
            entries.push(Entry {
                ts: node,
                parent: open.last().copied().unwrap_or(NO_PARENT),
                end: 0,
                start_byte: node.start_byte() as u32,
                end_byte: node.end_byte() as u32,
            });
            kinds.push(node.kind_id());
            let id = entries.len() - 1;
            if cursor.goto_first_child() {
                open.push(id as u32);
                continue;
            }
            entries[id].end = entries.len() as u32;
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Structure { base, entries, kinds, injections: Vec::new() };
                }
                if let Some(closed) = open.pop() {
                    entries[closed as usize].end = entries.len() as u32;
                }
            }
        }
    }

    /// How many nodes the parse has: every id is below it.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn node(&self, id: NodeId) -> SyntaxNode<'_, 't> {
        SyntaxNode { tree: self, local: id.0 - self.base }
    }

    pub(crate) fn root(&self) -> SyntaxNode<'_, 't> {
        SyntaxNode { tree: self, local: 0 }
    }

    /// The id of a node of THIS parse. `None` for a node of another tree —
    /// identity is checked, never inferred from position, so a zero-width
    /// MISSING node and its same-start neighbours stay distinct.
    fn local_of(&self, node: Node<'t>) -> Option<u32> {
        let start = node.start_byte() as u32;
        let first = self.entries.partition_point(|e| e.start_byte < start);
        let want = node.id();
        self.entries[first..]
            .iter()
            .take_while(|e| e.start_byte == start)
            .position(|e| e.ts.id() == want)
            .map(|i| (first + i) as u32)
    }

    pub(crate) fn lift(&self, node: Node<'t>) -> Option<SyntaxNode<'_, 't>> {
        self.local_of(node).map(|local| SyntaxNode { tree: self, local })
    }

    /// The outermost node spanning exactly `range`: preorder puts a
    /// same-range chain's head first.
    fn local_spanning(&self, range: std::ops::Range<usize>) -> Option<u32> {
        let start = range.start as u32;
        let first = self.entries.partition_point(|e| e.start_byte < start);
        self.entries[first..]
            .iter()
            .take_while(|e| e.start_byte == start)
            .position(|e| e.end_byte as usize == range.end)
            .map(|i| (first + i) as u32)
    }

    /// The injection on the subtree holding local index `local`, if any.
    fn injection_over(&self, local: u32) -> Option<(u32, usize)> {
        let i = self.injections.partition_point(|(at, _)| *at <= local);
        let (at, tree) = *self.injections[..i].last()?;
        (local < self.entries[at as usize].end).then_some((at, tree))
    }
}

/// A parse that replaces part of another: the code of an `s///e`
/// replacement, which the host parse reads as a string.
pub(crate) struct Injection<'t> {
    /// The forest index of the parse holding the replaced node.
    pub(crate) host: usize,
    /// The replaced node's byte range in the host.
    pub(crate) replaces: std::ops::Range<usize>,
    pub(crate) root: Node<'t>,
}

/// The parses one build reads: a file's main tree plus the trees parsed
/// over its own bytes (`s///e` replacement code). A node is answered by the
/// parse it belongs to, found by identity. Read in file order
/// ([`SyntaxForest::file_nodes_of_kind`]) it is one tree: each injected parse
/// stands where the node it replaces stood.
pub(crate) struct SyntaxForest<'t> {
    trees: Vec<Structure<'t>>,
}

impl<'t> SyntaxForest<'t> {
    pub(crate) fn new(root: Node<'t>) -> Self {
        Self::with_injections(root, Vec::new())
    }

    /// `injections` in registration order: a host is registered before
    /// anything it hosts.
    pub(crate) fn with_injections(main: Node<'t>, injections: Vec<Injection<'t>>) -> Self {
        let mut trees = vec![Structure::build(main, 0)];
        for inj in injections {
            let base = trees.last().map_or(0, |t| t.base + t.len() as u32);
            let index = trees.len();
            debug_assert!(inj.host < index, "an injection's host registers first");
            let at = trees.get(inj.host).and_then(|h| h.local_spanning(inj.replaces.clone()));
            debug_assert!(at.is_some(), "no node spans the replaced range {:?}", inj.replaces);
            if let Some(at) = at {
                let host = &mut trees[inj.host];
                let pos = host.injections.partition_point(|(a, _)| *a < at);
                host.injections.insert(pos, (at, index));
            }
            trees.push(Structure::build(inj.root, base));
        }
        SyntaxForest { trees }
    }

    /// How many nodes the forest holds: every id is below it.
    pub(crate) fn len(&self) -> usize {
        self.trees.last().map_or(0, |t| t.base as usize + t.len())
    }

    /// The root of the parse that holds `node`'s code, when another parse
    /// replaces it.
    pub(crate) fn injected_at(&self, node: Node<'t>) -> Option<Node<'t>> {
        let n = self.lift(node)?;
        let (_, tree) = n.tree.injections.iter().find(|(at, _)| *at == n.local)?;
        Some(self.trees[*tree].root().ts())
    }

    /// Whether `node` is inside a node another parse replaces: the host's
    /// reading of text that is really code.
    pub(crate) fn is_replaced(&self, node: Node<'t>) -> bool {
        self.lift(node).is_some_and(|n| n.tree.injection_over(n.local).is_some())
    }

    /// Each injected parse's root with the byte offset of the node it
    /// replaces, in registration order (hosts first).
    pub(crate) fn injections(&self) -> impl Iterator<Item = (Node<'t>, usize)> + '_ {
        self.trees.iter().flat_map(move |h| {
            h.injections.iter().map(move |(at, tree)| {
                (self.trees[*tree].root().ts(), h.entries[*at as usize].start_byte as usize)
            })
        })
    }

    /// Every node whose kind id is in `kinds`, in file order: the main
    /// parse's preorder, with each injected parse standing in for the
    /// subtree it replaces.
    pub(crate) fn file_nodes_of_kind(&self, kinds: &[u16]) -> Vec<SyntaxNode<'_, 't>> {
        let mut out = Vec::new();
        self.scan_in_file_order(0, kinds, &mut out);
        out
    }

    fn scan_in_file_order<'a>(&'a self, tree: usize, kinds: &[u16], out: &mut Vec<SyntaxNode<'a, 't>>) {
        let t = &self.trees[tree];
        let mut injections = t.injections.iter().peekable();
        let mut local = 0u32;
        while (local as usize) < t.len() {
            if let Some(&(at, inner)) = injections.next_if(|(at, _)| *at == local) {
                self.scan_in_file_order(inner, kinds, out);
                local = t.entries[at as usize].end;
                continue;
            }
            if kinds.contains(&t.kinds[local as usize]) {
                out.push(SyntaxNode { tree: t, local });
            }
            local += 1;
        }
    }

    /// The handle for `node`, from whichever parse holds it. `None` is a
    /// node from a parse this build never registered — a bug at the caller,
    /// which debug builds stop on.
    pub(crate) fn lift(&self, node: Node<'t>) -> Option<SyntaxNode<'_, 't>> {
        let found = self.trees.iter().find_map(|t| t.lift(node));
        debug_assert!(found.is_some(), "node {node:?} is from a parse outside this build");
        found
    }

    /// The file's own parse, the first registered.
    pub(crate) fn main(&self) -> &Structure<'t> {
        &self.trees[0]
    }

    /// The root of each parse, main tree first.
    pub(crate) fn roots(&self) -> impl Iterator<Item = Node<'t>> + '_ {
        self.trees.iter().map(|t| t.root().ts())
    }

    /// The root of the parse `node` belongs to.
    pub(crate) fn root_of(&self, node: Node<'t>) -> Option<Node<'t>> {
        Some(self.lift(node)?.tree.root().ts())
    }

    pub(crate) fn parent(&self, node: Node<'t>) -> Option<Node<'t>> {
        self.lift(node)?.parent().map(|p| p.ts())
    }

    pub(crate) fn prev_sibling(&self, node: Node<'t>) -> Option<Node<'t>> {
        self.lift(node)?.prev_sibling().map(|p| p.ts())
    }

    /// Strict ancestors of `node`, nearest first.
    pub(crate) fn ancestors(&self, node: Node<'t>) -> impl Iterator<Item = Node<'t>> + '_ {
        self.lift(node).into_iter().flat_map(|n| n.ancestors()).map(|n| n.ts())
    }
}

/// Every kind id `lang` gives any of `names` as a named node. A grammar can
/// carry one name on several symbols (aliases), and a kind scan has to match
/// them all.
pub(crate) fn kind_ids(lang: &tree_sitter::Language, names: &[&str]) -> Vec<u16> {
    (0..lang.node_kind_count() as u16)
        .filter(|&id| {
            lang.node_kind_is_named(id)
                && lang.node_kind_for_id(id).is_some_and(|k| names.contains(&k))
        })
        .collect()
}

/// A node of a [`Structure`]: a borrowed tree plus the node's index in it.
#[derive(Clone, Copy)]
pub(crate) struct SyntaxNode<'a, 't> {
    tree: &'a Structure<'t>,
    local: u32,
}

impl PartialEq for SyntaxNode<'_, '_> {
    fn eq(&self, other: &Self) -> bool {
        self.local == other.local && std::ptr::eq(self.tree, other.tree)
    }
}
impl Eq for SyntaxNode<'_, '_> {}

impl std::fmt::Debug for SyntaxNode<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SyntaxNode({:?}, {})", self.id(), self.kind())
    }
}

impl<'a, 't> SyntaxNode<'a, 't> {
    fn entry(&self) -> &'a Entry<'t> {
        &self.tree.entries[self.local as usize]
    }
    fn at(&self, local: u32) -> SyntaxNode<'a, 't> {
        SyntaxNode { tree: self.tree, local }
    }
    /// The raw tree-sitter node, for readers not yet on the tree.
    pub(crate) fn ts(&self) -> Node<'t> {
        self.entry().ts
    }
    pub(crate) fn id(&self) -> NodeId {
        NodeId(self.tree.base + self.local)
    }
    pub(crate) fn start_byte(&self) -> usize {
        self.entry().start_byte as usize
    }
    pub(crate) fn end_byte(&self) -> usize {
        self.entry().end_byte as usize
    }
    /// The ids of this node and everything under it, in preorder.
    pub(crate) fn subtree(&self) -> std::ops::Range<usize> {
        let base = self.tree.base as usize;
        base + self.local as usize..base + self.entry().end as usize
    }
    /// The outermost node sharing this node's range: the head of its
    /// same-range chain (a node and the parent it fills exactly), so every
    /// node of one chain answers the same site. O(chain length).
    pub(crate) fn site(&self) -> SyntaxNode<'a, 't> {
        let mut cur = *self;
        while let Some(p) = cur.parent().filter(|p| p.range() == self.range()) {
            cur = p;
        }
        cur
    }
    /// The sites holding this node's, nearest first.
    pub(crate) fn enclosing_sites(&self) -> impl Iterator<Item = SyntaxNode<'a, 't>> {
        self.site().ancestors().filter(|a| a.is_site())
    }
    /// Whether this node heads its same-range chain.
    fn is_site(&self) -> bool {
        self.parent().is_none_or(|p| p.range() != self.range())
    }
    fn range(&self) -> (u32, u32) {
        (self.entry().start_byte, self.entry().end_byte)
    }
    pub(crate) fn kind(&self) -> &'static str {
        self.entry().ts.kind()
    }
    pub(crate) fn kind_id(&self) -> u16 {
        self.tree.kinds[self.local as usize]
    }
    /// Every node strictly under this one whose kind id is in `kinds`, in
    /// preorder: a scan of the kind column over the subtree's interval.
    pub(crate) fn descendants_of_kind<'k>(
        &self,
        kinds: &'k [u16],
    ) -> impl Iterator<Item = SyntaxNode<'a, 't>> + 'k
    where
        'a: 'k,
    {
        let tree = self.tree;
        let ids = self.local as usize + 1..self.entry().end as usize;
        tree.kinds[ids.clone()]
            .iter()
            .zip(ids)
            .filter(|(k, _)| kinds.contains(k))
            .map(move |(_, local)| SyntaxNode { tree, local: local as u32 })
    }
    pub(crate) fn parent(&self) -> Option<SyntaxNode<'a, 't>> {
        let p = self.entry().parent;
        (p != NO_PARENT).then(|| self.at(p))
    }
    /// Strict ancestors, nearest first.
    pub(crate) fn ancestors(&self) -> impl Iterator<Item = SyntaxNode<'a, 't>> {
        std::iter::successors(self.parent(), |n| n.parent())
    }
    /// The previous child of the same parent: the node just before this one
    /// in preorder, climbed back up to the parent's level — O(depth), never
    /// a scan of the parent's children.
    pub(crate) fn prev_sibling(&self) -> Option<SyntaxNode<'a, 't>> {
        let parent = self.entry().parent;
        if parent == NO_PARENT || self.local == parent + 1 {
            return None;
        }
        let mut cur = self.at(self.local - 1);
        while cur.entry().parent != parent {
            cur = cur.parent()?;
        }
        Some(cur)
    }
}

/// The facts a consumer keeps per node, apart from the structure so a walk
/// can hold node handles while it writes: every fact in node order, one
/// slice per node. A consumer's own mutable state per fact is a `Vec`
/// indexed by the fact's position here.
pub(crate) struct NodeFacts<T> {
    /// `first[id]..first[id + 1]` are node `id`'s facts.
    first: Vec<u32>,
    facts: Vec<T>,
}

impl<T> NodeFacts<T> {
    /// Facts for a parse of `len` nodes. Facts on one node keep the order
    /// they come in.
    pub(crate) fn new(len: usize, mut items: Vec<(NodeId, T)>) -> Self {
        items.sort_by_key(|(id, _)| id.0);
        let mut first = vec![0u32; len + 1];
        for (id, _) in &items {
            first[id.0 as usize + 1] += 1;
        }
        for i in 1..first.len() {
            first[i] += first[i - 1];
        }
        NodeFacts { first, facts: items.into_iter().map(|(_, t)| t).collect() }
    }

    /// Every fact, nodes in preorder.
    pub(crate) fn all(&self) -> &[T] {
        &self.facts
    }

    /// The positions of the facts on the nodes `ids` (a subtree is one
    /// such interval).
    pub(crate) fn positions(&self, ids: std::ops::Range<usize>) -> std::ops::Range<usize> {
        self.first[ids.start] as usize..self.first[ids.end] as usize
    }

    /// The positions of the facts on node `id`.
    pub(crate) fn on(&self, id: NodeId) -> std::ops::Range<usize> {
        self.positions(id.index()..id.index() + 1)
    }
}

#[cfg(test)]
#[path = "syntax_tests.rs"]
mod tests;
