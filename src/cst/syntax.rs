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

/// A node's preorder index in its [`Structure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NodeId(u32);

const NO_PARENT: u32 = u32::MAX;

struct Entry<'t> {
    // The raw node answers everything the entry does not store yet; a field
    // moves here when the readers that need it port, and the node goes when
    // the last raw reader does.
    ts: Node<'t>,
    parent: u32,
    start_byte: u32,
}

/// The structure of one parse.
pub(crate) struct Structure<'t> {
    entries: Vec<Entry<'t>>,
}

impl<'t> Structure<'t> {
    fn build(root: Node<'t>) -> Self {
        let mut entries: Vec<Entry<'t>> = Vec::with_capacity(root.descendant_count());
        let mut cursor = root.walk();
        // Explicit stack, like every tree pass here: a recursive descent
        // costs one frame per CST level.
        let mut open: Vec<u32> = Vec::new();
        loop {
            let node = cursor.node();
            entries.push(Entry {
                ts: node,
                parent: open.last().copied().unwrap_or(NO_PARENT),
                start_byte: node.start_byte() as u32,
            });
            if cursor.goto_first_child() {
                open.push((entries.len() - 1) as u32);
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Structure { entries };
                }
                open.pop();
            }
        }
    }

    fn root(&self) -> SyntaxNode<'_, 't> {
        SyntaxNode { tree: self, id: NodeId(0) }
    }

    /// The id of a node of THIS parse. `None` for a node of another tree —
    /// identity is checked, never inferred from position, so a zero-width
    /// MISSING node and its same-start neighbours stay distinct.
    fn id_of(&self, node: Node<'t>) -> Option<NodeId> {
        let start = node.start_byte() as u32;
        let first = self.entries.partition_point(|e| e.start_byte < start);
        let want = node.id();
        self.entries[first..]
            .iter()
            .take_while(|e| e.start_byte == start)
            .position(|e| e.ts.id() == want)
            .map(|i| NodeId((first + i) as u32))
    }

    fn lift(&self, node: Node<'t>) -> Option<SyntaxNode<'_, 't>> {
        self.id_of(node).map(|id| SyntaxNode { tree: self, id })
    }
}

/// The parses one build reads: a file's main tree plus the trees parsed
/// over its own bytes (`s///e` replacement code). A node is answered by the
/// parse it belongs to, found by identity.
pub(crate) struct SyntaxForest<'t> {
    trees: Vec<Structure<'t>>,
}

impl<'t> SyntaxForest<'t> {
    pub(crate) fn new(roots: impl IntoIterator<Item = Node<'t>>) -> Self {
        SyntaxForest { trees: roots.into_iter().map(Structure::build).collect() }
    }

    /// The handle for `node`, from whichever parse holds it. `None` is a
    /// node from a parse this build never registered — a bug at the caller,
    /// which debug builds stop on.
    pub(crate) fn lift(&self, node: Node<'t>) -> Option<SyntaxNode<'_, 't>> {
        let found = self.trees.iter().find_map(|t| t.lift(node));
        debug_assert!(found.is_some(), "node {node:?} is from a parse outside this build");
        found
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

/// A node of a [`Structure`]: a borrowed tree plus an id.
#[derive(Clone, Copy)]
pub(crate) struct SyntaxNode<'a, 't> {
    tree: &'a Structure<'t>,
    id: NodeId,
}

impl PartialEq for SyntaxNode<'_, '_> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && std::ptr::eq(self.tree, other.tree)
    }
}
impl Eq for SyntaxNode<'_, '_> {}

impl std::fmt::Debug for SyntaxNode<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SyntaxNode({:?}, {})", self.id, self.kind())
    }
}

impl<'a, 't> SyntaxNode<'a, 't> {
    fn entry(&self) -> &'a Entry<'t> {
        &self.tree.entries[self.id.0 as usize]
    }
    /// The raw tree-sitter node, for readers not yet on the tree.
    pub(crate) fn ts(&self) -> Node<'t> {
        self.entry().ts
    }
    pub(crate) fn kind(&self) -> &'static str {
        self.entry().ts.kind()
    }
    pub(crate) fn parent(&self) -> Option<SyntaxNode<'a, 't>> {
        let p = self.entry().parent;
        (p != NO_PARENT).then_some(SyntaxNode { tree: self.tree, id: NodeId(p) })
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
        if parent == NO_PARENT || self.id.0 == parent + 1 {
            return None;
        }
        let mut cur = SyntaxNode { tree: self.tree, id: NodeId(self.id.0 - 1) };
        while cur.entry().parent != parent {
            cur = cur.parent()?;
        }
        Some(cur)
    }
}

#[cfg(test)]
#[path = "syntax_tests.rs"]
mod tests;
