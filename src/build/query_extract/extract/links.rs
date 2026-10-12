//! `@link.*`: structure the document states one level at a time, chained
//! here to any depth. A tree-sitter pattern has a fixed depth, so a
//! declarator wrapped any number of times or a name qualified any number of
//! times cannot be one pattern; the document links each level to the next,
//! and the driver follows the links by node identity. Nothing here reads a
//! node kind or a field: which node links to which is the document's call.

use super::*;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct Links<'t> {
    to: HashMap<(usize, LinkCap), tree_sitter::Node<'t>>,
}

impl<'t> Links<'t> {
    pub(super) fn record(&mut self, from: tree_sitter::Node<'t>, rel: LinkCap, to: tree_sitter::Node<'t>) {
        self.to.insert((from.id(), rel), to);
    }

    fn get(&self, node: tree_sitter::Node<'t>, rel: LinkCap) -> Option<tree_sitter::Node<'t>> {
        self.to.get(&(node.id(), rel)).copied()
    }

    /// The name a scope segment contributes: a templated name's base
    /// (`Buf<T>::grow` files under `Buf`), else the node's own text.
    pub(super) fn base_text(&self, node: tree_sitter::Node<'t>, src: &[u8]) -> String {
        let named = self.get(node, LinkCap::Base).unwrap_or(node);
        named.utf8_text(src).unwrap_or("").to_string()
    }

    /// An out-of-line definition's owner and member name: the definition's
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
        let mut decl = self.get(def, LinkCap::Declares)?;
        let mut node = loop {
            if let Some(declared) = self.get(decl, LinkCap::Declares) {
                break declared;
            }
            decl = self.get(decl, LinkCap::Inner)?;
        };
        self.get(node, LinkCap::Name)?;
        let mut owner = String::new();
        while let Some(name) = self.get(node, LinkCap::Name) {
            if let Some(scope) = self.get(node, LinkCap::Scope) {
                owner = self.base_text(scope, src);
            }
            node = name;
        }
        Some((owner, node))
    }
}
