//! `@import.*`: import statements, and the module argument of an import
//! CALL (R's `library()`/`source()`, CMake's `include()`), which callees
//! the document's own predicate picks.

use super::*;
use std::collections::{BTreeMap, HashMap};

#[derive(Default)]
pub(super) struct State {
    /// import-call arguments per match (BTreeMap: match ids are
    /// source-ordered, so imports come out deterministic)
    import_args: BTreeMap<usize, String>,
    /// A group import's shared namespace and an import's alias, joined to
    /// the match's import name. The alias is written after the name, so
    /// both are collected ahead of it.
    prefix_by_match: HashMap<usize, String>,
    alias_by_match: HashMap<usize, String>,
}

pub(super) fn collect(st: &mut ExtractState, e: &Event) {
    match e.cap {
        Capture::Import(ImportCap::Prefix) => {
            st.imports.prefix_by_match.insert(e.match_id, e.text.clone());
        }
        Capture::Import(ImportCap::Alias) => {
            st.imports.alias_by_match.insert(e.match_id, e.text.clone());
        }
        _ => {}
    }
}

pub(super) fn handle(st: &mut ExtractState, e: &Event) {
    match e.cap {
        // One import row either way; the spellings differ in what the
        // document claims about the token, which the registry reads.
        Capture::Import(ImportCap::Path) => {
            st.out.import_sites.push((e.text.clone(), e.span()));
            st.out.imports.push(e.text.clone());
        }
        Capture::Import(ImportCap::Name) => name_row(st, e),
        Capture::Import(ImportCap::Arg) => {
            st.imports.import_args.insert(e.match_id, e.text.clone());
        }
        // The statement anchors its pattern; its name carries the meaning.
        // A prefix and an alias are read through their import name.
        Capture::Import(ImportCap::Statement | ImportCap::Prefix | ImportCap::Alias) => {}
        _ => {}
    }
}

/// An imported name's row: the name in full (a group clause joins its
/// prefix), spanning the name token. An aliased import also records what
/// the alias stands for, as the parts the producer has in hand.
fn name_row(st: &mut ExtractState, e: &Event) {
    let names = &st.pack.names;
    let prefix = st.imports.prefix_by_match.get(&e.match_id);
    let (raw, parts) = match (names.sep(), prefix) {
        (Some(sep), Some(prefix)) => {
            let ns = prefix.trim_start_matches(sep);
            (crate::model::conventions::join_qualified(ns, &e.text, sep), (ns, e.text.as_str()))
        }
        _ => {
            let written = names.sep().map_or(e.text.as_str(), |sep| e.text.trim_start_matches(sep));
            let (ns, leaf) = crate::model::conventions::split_qualified(written, names);
            (e.text.clone(), (ns.unwrap_or(""), leaf))
        }
    };
    if let Some(alias) = st.imports.alias_by_match.get(&e.match_id) {
        st.out.use_aliases.push((alias.clone(), parts.0.to_string(), parts.1.to_string()));
    }
    st.out.import_sites.push((raw.clone(), e.span()));
    st.out.imports.push(raw);
}

/// Import-call arguments → imports, the module as written.
pub(super) fn finish(st: &mut ExtractState) {
    for module in st.imports.import_args.values() {
        if !st.out.imports.contains(module) {
            st.out.imports.push(module.clone());
        }
    }
}
