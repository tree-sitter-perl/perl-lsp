//! `@import.*`: import statements, and import CALLS (R's
//! `library()`/`source()`) whose callee + argument the pack classifies.

use super::*;
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct State {
    /// import-call halves, joined per match (BTreeMap: match ids are
    /// source-ordered, so imports come out deterministic)
    import_fns: BTreeMap<usize, String>,
    import_args: BTreeMap<usize, String>,
}

pub(super) fn handle(st: &mut ExtractState, e: &Event) {
    match e.cap {
        Capture::Import(ImportCap::Name) => {
            st.out.import_sites.push((e.text.clone(), e.span()));
            st.out.imports.push(e.text.clone());
        }
        Capture::Import(ImportCap::Fn) => {
            st.imports.import_fns.insert(e.match_id, e.text.clone());
        }
        Capture::Import(ImportCap::Arg) => {
            st.imports.import_args.insert(e.match_id, e.text.clone());
        }
        // The statement anchors its pattern; its name carries the meaning.
        Capture::Import(ImportCap::Statement) => {}
        _ => {}
    }
}

/// Import CALLS → imports.
pub(super) fn finish(st: &mut ExtractState) {
    let i = &st.imports;
    for (mid, f) in &i.import_fns {
        if let Some(arg) = i.import_args.get(mid) {
            if let Some(module) = (st.pack.import_call)(f, arg) {
                if !st.out.imports.contains(&module) {
                    st.out.imports.push(module);
                }
            }
        }
    }
}
