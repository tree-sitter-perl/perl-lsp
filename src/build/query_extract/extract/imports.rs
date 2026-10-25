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

/// An imported name's row, spanning the name token. Under a use-map it is
/// a `UseRow` of the parts as written — the group clause's prefix and the
/// name's own qualifier are its namespace, the name's last segment its
/// leaf, the alias what it is bound under — so nothing downstream splits a
/// joined name back apart. Elsewhere the name as written is the row.
fn name_row(st: &mut ExtractState, e: &Event) {
    let names = &st.pack.names;
    let Some(sep) = names.use_map_sep() else {
        st.out.import_sites.push((e.text.clone(), e.span()));
        st.out.imports.push(e.text.clone());
        return;
    };
    let (qualifier, leaf) =
        crate::model::conventions::split_qualified(e.text.trim_start_matches(sep), names);
    let prefix = st.imports.prefix_by_match.get(&e.match_id).map(|p| p.trim_start_matches(sep));
    let namespace = match (prefix, qualifier) {
        (Some(p), Some(q)) => crate::model::conventions::join_qualified(p, q, sep),
        (p, q) => p.or(q).unwrap_or("").to_string(),
    };
    let row = crate::model::file_analysis::UseRow {
        span: e.span(),
        name: crate::model::file_analysis::QualifiedName { namespace, leaf: leaf.to_string() },
        alias: st.imports.alias_by_match.get(&e.match_id).cloned(),
        // the enclosing scope; `bind_reach` narrows it to the namespace
        // section once every section's end is known
        reach: st.out.scopes[st.cur_scope.0 as usize].span,
    };
    st.out.imports.push(row.name.identity(sep));
    st.out.use_rows.push(row);
}

/// Each row binds in its scope AND its namespace section, whichever is
/// narrower: `namespace A; use X\M;` reaches to the next `namespace`, a
/// row in a braced namespace to its closing brace.
pub(super) fn bind_reach(st: &mut ExtractState) {
    let sections = &st.out.namespace_sections;
    for row in st.out.use_rows.iter_mut() {
        if let Some(sec) = crate::model::file_analysis::section_at(sections, row.span.start) {
            row.reach.start = row.reach.start.max(sec.span.start);
            row.reach.end = row.reach.end.min(sec.span.end);
        }
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
