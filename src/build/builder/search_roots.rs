//! Search roots: the directories a file adds to `@INC` where the directory
//! is statically known, minted with their anchor. Which modules, scalars
//! and arrays are the idioms is the plugin manifest's
//! (`search_root_makers`); this reads the shapes they are written in.

use super::*;
use std::collections::HashMap;
use crate::build::plugin::SearchRootMaker;
use crate::cst::{LiteralShape, NodeExt};
use crate::model::file_analysis::{RootAnchor, SearchRoot};

/// The search-root idioms the plugins declare, baked at build start.
#[derive(Default)]
pub(super) struct SearchRootIdioms {
    /// A module whose import list is roots → their anchor.
    uses: HashMap<String, RootAnchor>,
    /// A package scalar holding a directory → the anchor it stands for.
    variables: HashMap<String, RootAnchor>,
    /// An array that is the search path → the anchor of what is added,
    /// and the calls that add to it.
    arrays: HashMap<String, (RootAnchor, Vec<String>)>,
}

impl SearchRootIdioms {
    pub(super) fn from_makers<'m>(makers: impl Iterator<Item = &'m SearchRootMaker>) -> Self {
        let mut out = SearchRootIdioms::default();
        for m in makers {
            let anchor = match m.anchor.as_str() {
                "working_dir" => RootAnchor::WorkingDir,
                "file_dir" => RootAnchor::FileDir,
                other => {
                    log::error!("search_root_makers: unknown anchor `{other}` for `{}` — entry dropped", m.name);
                    continue;
                }
            };
            match m.kind.as_str() {
                "use" => {
                    out.uses.insert(m.name.clone(), anchor);
                }
                "variable" => {
                    out.variables.insert(m.name.clone(), anchor);
                }
                "array" => {
                    out.arrays.insert(m.name.clone(), (anchor, m.verbs.clone()));
                }
                other => log::error!("search_root_makers: unknown kind `{other}` for `{}` — entry dropped", m.name),
            }
        }
        out
    }
}

impl<'a> Builder<'a> {
    /// `use MODULE LIST` where the module's import list is roots.
    pub(super) fn mint_use_roots(&mut self, node: Node<'a>, module_node: Node<'a>, module_name: &str) {
        let Some(&anchor) = self.search_root_idioms.uses.get(module_name) else { return };
        if crate::cst::is_negated_use(node) {
            return;
        }
        for arg in node.elements().filter(|c| c.id() != module_node.id()) {
            for root in self.root_values(arg, anchor) {
                self.add_search_root(root);
            }
        }
    }

    /// `unshift @INC, LIST` and its kin: a call the manifest says adds to
    /// a search-path array.
    pub(super) fn mint_search_path_edit(&mut self, node: Node<'a>) {
        if self.search_root_idioms.arrays.is_empty() {
            return;
        }
        let Some(verb) = node.child_by_field_name("function").and_then(|f| f.text(self.source)) else { return };
        let args = crate::cst::call_args(node);
        let Some((array, rest)) = args.split_first() else { return };
        if array.kind() != "array" {
            return;
        }
        let Some(name) = crate::cst::varname_child(*array).and_then(|v| v.text(self.source)) else { return };
        let Some((anchor, verbs)) = self.search_root_idioms.arrays.get(name) else { return };
        if !verbs.iter().any(|v| v == verb) {
            return;
        }
        let anchor = *anchor;
        for &arg in rest {
            for root in self.root_values(arg, anchor) {
                self.add_search_root(root);
            }
        }
    }

    fn add_search_root(&mut self, root: SearchRoot) {
        if !root.path.is_empty() || root.anchor == RootAnchor::FileDir {
            if !self.search_roots.contains(&root) {
                self.search_roots.push(root);
            }
        }
    }

    /// The roots one argument writes under an idiom anchored at `anchor`:
    /// each string a list or `qw` holds, a string or constant that folds,
    /// or an anchor scalar followed by a literal path. Anything else
    /// writes none.
    fn root_values(&self, arg: Node<'a>, anchor: RootAnchor) -> Vec<SearchRoot> {
        match crate::cst::literal_shape(arg) {
            Some(LiteralShape::List) => {
                return crate::cst::list_elements(arg).into_iter().flat_map(|e| self.root_values(e, anchor)).collect();
            }
            Some(LiteralShape::Qw) => {
                return crate::cst::string_list(arg, self.source, &mut |_| Vec::new())
                    .into_iter()
                    .map(|(w, _)| SearchRoot::written(anchor, &w))
                    .collect();
            }
            _ => {}
        }
        if let Some((var_anchor, tail)) = self.anchored_tail(arg) {
            return vec![SearchRoot::written(var_anchor, tail.trim_start_matches('/'))];
        }
        self.enumerate_string_values(arg).into_iter().map(|v| SearchRoot::written(anchor, &v)).collect()
    }

    /// An expression that is an anchor scalar followed by a literal path:
    /// `$FindBin::Bin`, `"$FindBin::Bin/../lib"`, `$FindBin::Bin . '/lib'`.
    /// The anchor the scalar stands for, and the path after it.
    fn anchored_tail(&self, expr: Node<'a>) -> Option<(RootAnchor, String)> {
        match expr.kind() {
            "scalar" => Some((self.anchor_of_scalar(expr)?, String::new())),
            "interpolated_string_literal" => {
                let content = expr.child_by_field_name("content")?;
                let mut parts = content.elements();
                let head = parts.next().filter(|h| h.kind() == "scalar" && h.start_byte() == content.start_byte())?;
                if parts.next().is_some() {
                    return None;
                }
                let anchor = self.anchor_of_scalar(head)?;
                let tail = std::str::from_utf8(&self.source[head.end_byte()..content.end_byte()]).ok()?;
                Some((anchor, tail.to_string()))
            }
            "binary_expression" => {
                if expr.child_by_field_name("operator").map(|o| o.kind()) != Some(".") {
                    return None;
                }
                let (anchor, mut tail) = self.anchored_tail(expr.child_by_field_name("left")?)?;
                let mut right = self.enumerate_string_values(expr.child_by_field_name("right")?);
                if right.len() != 1 {
                    return None;
                }
                tail.push_str(&right.remove(0));
                Some((anchor, tail))
            }
            _ => None,
        }
    }

    /// The anchor a scalar stands for: a manifest variable, spelled
    /// qualified or imported bare (`use FindBin qw($Bin)`).
    fn anchor_of_scalar(&self, scalar: Node<'a>) -> Option<RootAnchor> {
        let varname = crate::cst::varname_child(scalar)?.text(self.source)?;
        let spellings = &crate::model::conventions::PERL_SPELLINGS;
        if crate::model::file_analysis::split_qualified(varname, spellings).0.is_some() {
            return self.search_root_idioms.variables.get(varname).copied();
        }
        let local = format!("${varname}");
        let module = self
            .imports
            .iter()
            .find(|i| i.imported_symbols.iter().any(|s| s.local_name == local))?;
        let qualified = crate::model::conventions::join_qualified(&module.module_name, varname, spellings.sep()?);
        self.search_root_idioms.variables.get(&qualified).copied()
    }
}
