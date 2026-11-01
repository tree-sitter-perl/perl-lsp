//! `@root` / `@root.file_dir` / `@root.path`: a statement that adds a
//! directory to the module search path, minted as a `SearchRoot` with its
//! anchor.

use super::*;
use crate::model::file_analysis::{RootAnchor, SearchRoot};

/// On the statement: its anchor is whether the match captured
/// `@root.file_dir`, its path the match's segments in source order.
pub(super) fn handle(st: &mut ExtractState, e: &Event) {
    if e.cap != Capture::Root(RootCap::Statement) {
        return;
    }
    let ad = st.adorned;
    let file_dir = ad.in_match(e.match_id, Capture::Root(RootCap::FileDir)).is_some();
    let segments: Vec<&str> =
        ad.of_match(e.match_id).filter(|c| c.cap == Capture::Root(RootCap::Path)).map(|c| c.text.as_str()).collect();
    let anchor = if file_dir { RootAnchor::FileDir } else { RootAnchor::WorkingDir };
    let root = SearchRoot::written(anchor, &segments.join("/"));
    if (!root.path.is_empty() || file_dir) && !st.out.search_roots.contains(&root) {
        st.out.search_roots.push(root);
    }
}
