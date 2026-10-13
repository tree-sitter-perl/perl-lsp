//! `@import.*`: import statements, and the module argument of an import
//! CALL (R's `library()`/`source()`, CMake's `include()`), which callees
//! the document's own predicate picks.

use super::*;
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct State {
    /// import-call arguments per match (BTreeMap: match ids are
    /// source-ordered, so imports come out deterministic)
    import_args: BTreeMap<usize, String>,
}

pub(super) fn handle(st: &mut ExtractState, e: &Event) {
    match e.cap {
        Capture::Import(ImportCap::Name) => {
            st.out.import_sites.push((e.text.clone(), e.span()));
            st.out.imports.push(e.text.clone());
        }
        Capture::Import(ImportCap::Arg) => {
            st.imports.import_args.insert(e.match_id, e.text.clone());
        }
        // The statement anchors its pattern; its name carries the meaning.
        Capture::Import(ImportCap::Statement) => {}
        _ => {}
    }
}

/// Import-call arguments → imports, the module as written.
pub(super) fn finish(st: &mut ExtractState) {
    for module in st.imports.import_args.values() {
        if !st.out.imports.contains(module) {
            st.out.imports.push(module.clone());
        }
    }
}
