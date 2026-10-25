//! `@import.*`: import statements, and the module argument of an import
//! CALL (R's `library()`/`source()`, CMake's `include()`), which callees
//! the document's own predicate picks.

use super::*;
use crate::model::file_analysis::{ImportPrefix, ImportReach};
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
        Capture::Import(ImportCap::Module) => push_import(st, &e.text),
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
    let prefix = st.imports.prefix_by_match.get(&e.match_id).map(|p| module_prefix(st, p, sep));
    let namespace = match (prefix.as_deref(), qualifier) {
        (Some(p), Some(q)) => crate::model::conventions::join_qualified(p, q, sep),
        (p, q) => p.or(q).unwrap_or("").to_string(),
    };
    // the enclosing scope; `bind_reach` narrows it to the namespace
    // section once every section's end is known
    let mut reach = st.out.scopes[st.cur_scope.0 as usize].span;
    if names.imports.reach == ImportReach::FromImport {
        reach.start = e.span().start;
    }
    let row = crate::model::file_analysis::UseRow {
        span: e.span(),
        name: crate::model::file_analysis::QualifiedName { namespace, leaf: leaf.to_string() },
        alias: st.imports.alias_by_match.get(&e.match_id).cloned(),
        reach,
    };
    let identity = row.name.identity(sep);
    push_import(st, &identity);
    st.out.use_rows.push(row);
}

fn push_import(st: &mut ExtractState, module: &str) {
    if !st.out.imports.iter().any(|m| m == module) {
        st.out.imports.push(module.to_string());
    }
}

/// The namespace an import's module prefix names. A leading separator is
/// the language's to define: absolute (`use \A\B\{C}`), or relative to the
/// file's own package, one level up per separator past the first (`from
/// ..x import C` in `pkg.sub.mod` is `pkg.x`).
fn module_prefix(st: &ExtractState, written: &str, sep: &str) -> String {
    let absolute = written.trim_start_matches(sep);
    match st.pack.names.imports.prefix {
        ImportPrefix::Absolute => absolute.to_string(),
        ImportPrefix::PackageRelative if absolute.len() == written.len() => absolute.to_string(),
        ImportPrefix::PackageRelative => {
            let levels = (written.len() - absolute.len()) / sep.len();
            let package = st.file_module.as_ref().map(|m| m.package()).unwrap_or_default();
            let base = &package[..package.len().saturating_sub(levels - 1)];
            base.iter()
                .map(String::as_str)
                .chain((!absolute.is_empty()).then_some(absolute))
                .fold(String::new(), |ns, seg| crate::model::conventions::join_qualified(&ns, seg, sep))
        }
    }
}

/// Each row binds in its scope AND its namespace section, whichever is
/// narrower: `namespace A; use X\M;` reaches to the next `namespace`, a
/// row in a braced namespace to its closing brace.
pub(super) fn bind_reach(st: &mut ExtractState) {
    let sections = &st.out.namespace_ranges;
    for row in st.out.use_rows.iter_mut() {
        if let Some(sec) = crate::model::file_analysis::namespace_range_at(sections, row.span.start) {
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
