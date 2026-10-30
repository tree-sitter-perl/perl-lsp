//! `@root` / `@root.file_dir` / `@root.path`: a statement that adds a
//! directory to the module search path, minted as a `SearchRoot` with its
//! anchor.

use super::*;
use crate::model::file_analysis::{RootAnchor, SearchRoot};
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct State {
    /// per match: whether it is anchored at the file, and its segments in
    /// source order
    roots: BTreeMap<usize, (bool, Vec<String>)>,
}

pub(super) fn collect(st: &mut ExtractState, e: &Event) {
    let entry = st.roots.roots.entry(e.match_id).or_default();
    match e.cap {
        Capture::Root(RootCap::FileDir) => entry.0 = true,
        Capture::Root(RootCap::Path) => entry.1.push(e.text.clone()),
        _ => {}
    }
}

pub(super) fn finish(st: &mut ExtractState) {
    for (file_dir, segments) in std::mem::take(&mut st.roots.roots).into_values() {
        let anchor = if file_dir { RootAnchor::FileDir } else { RootAnchor::WorkingDir };
        let root = SearchRoot::written(anchor, &segments.join("/"));
        if (!root.path.is_empty() || file_dir) && !st.out.search_roots.contains(&root) {
            st.out.search_roots.push(root);
        }
    }
}
