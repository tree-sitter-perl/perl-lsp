//! `@scope`, `@context.*`, `@parent`: the lexical tree, the sticky
//! container context, and the inheritance edges declared with a class.

use super::*;
use crate::model::file_analysis::{OpenSections, SectionKind};
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct State {
    /// A context whose same-match `@scope` starts AFTER it (C++ namespace:
    /// `@context` is on the name, `@scope` on the body `{`) must be
    /// registered at the BODY depth, not the file depth — else it goes
    /// sticky and leaks past the closing brace; it waits here for the
    /// scope push.
    pending_context: HashMap<usize, (ContextKind, String)>,
    /// Parent edges whose parent is still the written spelling: its index
    /// in `out.parents`, where it was written, and the scope it is written
    /// in. Resolved through the use-map once every import row and class is
    /// in.
    written_parents: Vec<(usize, Point, ScopeId)>,
    /// Each `@scope.header` scope, and the def scope whose header it is:
    /// the scope a name the header declares binds in.
    pub(super) headed: HashMap<ScopeId, ScopeId>,
    /// The scopes that are a class's own, opened by the match whose
    /// `@context.class` names it, and the class's identity.
    class_scopes: HashMap<ScopeId, String>,
    /// Statement-form namespace sections still running, by the scope
    /// depth they were declared at.
    open_sections: OpenSections,
}

/// Record a namespace section under a use-map: a braced one is its body,
/// a statement-form one runs to the end of the enclosing scope until the
/// next one at its depth cuts it short.
fn open_section(st: &mut ExtractState, namespace: &str, span: Span, kind: SectionKind) {
    if st.pack.names.use_map_sep().is_none() {
        return;
    }
    let lane = &mut st.out.namespace_ranges;
    match kind {
        SectionKind::Statement => {
            let depth = st.scope_stack.len();
            st.scopes.open_sections.open_statement(lane, namespace.to_string(), span.start, span.end, depth);
        }
        SectionKind::Block => OpenSections::push_block(lane, namespace.to_string(), span),
    }
}

/// The home a declaration in `from` at `at` joins: the class whose own
/// scope is on its `callable_reach`, else the namespace in force there. A
/// class declared in `class Outer:`'s body is `Outer.Inner`; one declared
/// in a method of `Outer` joins the namespace.
pub(super) fn home_of<'s>(st: &'s ExtractState, from: ScopeId, at: Point) -> Option<&'s str> {
    crate::model::file_analysis::callable_reach(&st.out.scopes, from)
        .find_map(|id| st.scopes.class_scopes.get(&id))
        .map(String::as_str)
        .or_else(|| st.namespace_at(at))
}

/// The scope each class declared in this file opened as its own (its
/// first, where a class reopens), by the class's identity.
pub(super) fn class_scope_rows(st: &ExtractState) -> Vec<(String, ScopeId)> {
    let mut first: HashMap<&str, ScopeId> = HashMap::new();
    for (&id, class) in &st.scopes.class_scopes {
        let at = first.entry(class.as_str()).or_insert(id);
        *at = (*at).min(id);
    }
    let mut rows: Vec<(String, ScopeId)> = first.into_iter().map(|(c, id)| (c.to_string(), id)).collect();
    rows.sort_by_key(|&(_, id)| id);
    rows
}

pub(super) fn handle(st: &mut ExtractState, e: &Event) {
    let pack = st.pack;
    match e.cap {
        // `@scope` = a plain lexical Block; `@scope.sub` = sub-body
        // content (function bodies, prototype signatures, explicit
        // instantiations, requires-expressions) — the kind
        // `scope_within_sub_body` reads to shield params/locals from the
        // outline and the class-content lane. Pack subs carry no name on
        // the scope (the Symbol holds identity); the callable it is the body
        // of is joined as its `owner` at assembly.
        Capture::Scope(scope_cap) => {
            let id = ScopeId(st.out.scopes.len() as u32);
            // A header is code of the scope around its def's own (the file,
            // for a header the file scope holds).
            let around = match scope_cap {
                ScopeCap::Header => st.out.scopes[st.cur_scope.0 as usize].parent,
                _ => None,
            };
            let (parent, package) = match around {
                Some(around) => {
                    st.scopes.headed.insert(id, st.cur_scope);
                    (around, st.out.scopes[around.0 as usize].package.clone())
                }
                None => (st.cur_scope, st.package.clone()),
            };
            st.out.scopes.push(Scope {
                id,
                parent: Some(parent),
                kind: match scope_cap {
                    ScopeCap::Sub | ScopeCap::SubImplicitReceiver | ScopeCap::SubClosed => {
                        ScopeKind::Sub { name: String::new() }
                    }
                    ScopeCap::Block | ScopeCap::Direct | ScopeCap::Header => ScopeKind::Block,
                },
                span: e.span(),
                package,
                owner: None,
                implicit_receiver: scope_cap == ScopeCap::SubImplicitReceiver,
                closed: scope_cap == ScopeCap::SubClosed,
                captures: Vec::new(),
                direct: scope_cap == ScopeCap::Direct,
                declaration_rules: Vec::new(),
            });
            st.scope_stack.push((st.adorned.end_byte(e), id));
            st.out.scope_count += 1;
            // a context deferred to THIS scope (C++ namespace) →
            // register at the body depth so it pops with the block.
            if let Some((kind, text)) = st.scopes.pending_context.remove(&e.match_id) {
                while st.context_stack.last().is_some_and(|&(d, ..)| d >= st.scope_stack.len()) {
                    st.context_stack.pop();
                }
                match kind {
                    ContextKind::Namespace => open_section(st, &text, e.span(), SectionKind::Block),
                    ContextKind::Class => {
                        st.scopes.class_scopes.insert(id, text.clone());
                    }
                    _ => {}
                }
                st.context_stack.push((st.scope_stack.len(), kind, text));
            }
        }
        Capture::Context(kind) => {
            // Shape the context like a def name (cpp canonicalizes a
            // spec's template spelling) so members' `package` matches
            // the container Symbol's identity exactly.
            let text = (pack.shape_name)(&e.text);
            // A class body's members file under the class's identity, as
            // the class's def minted it.
            let identity = (kind == ContextKind::Class && pack.names.use_map_sep().is_some()).then(|| {
                super::defs::class_of_match(st, e.match_id)
                    .map(str::to_string)
                    .unwrap_or_else(|| st.class_identity(&text, st.cur_scope, e.start))
            });
            // If this match's `@scope` starts AFTER this context, the
            // context belongs to that (not-yet-pushed) body — defer it
            // so it registers at the body depth and pops with the block.
            let own_scope = st.adorned.of_match(e.match_id).find(|t| matches!(t.cap, Capture::Scope(_)));
            if own_scope.is_some_and(|s| st.adorned.start_byte(s) > st.adorned.start_byte(e)) {
                st.scopes.pending_context.insert(e.match_id, (kind, identity.unwrap_or(text)));
            } else {
                // Replace any context at the same depth; deeper ones
                // were already popped with their scopes.
                while st.context_stack.last().is_some_and(|&(d, ..)| d >= st.scope_stack.len()) {
                    st.context_stack.pop();
                }
                let value = identity.unwrap_or_else(|| e.text.clone());
                if kind == ContextKind::Namespace {
                    let end = st.out.scopes[st.cur_scope.0 as usize].span.end;
                    open_section(st, &value, Span { start: e.span().start, end }, SectionKind::Statement);
                }
                // A scope opened by this same match is the container the
                // context names (php's class node), so a point anywhere in
                // it, header included, is in that package.
                if own_scope.is_some() {
                    if let Some(&(_, id)) = st.scope_stack.last() {
                        st.out.scopes[id.0 as usize].package = Some(value.clone());
                        if kind == ContextKind::Class {
                            st.scopes.class_scopes.insert(id, value.clone());
                        }
                    }
                }
                st.context_stack.push((st.scope_stack.len(), kind, value));
            }
        }
        Capture::ClosureCapture => {
            let name = (pack.shape_name)(&e.text);
            let captures = &mut st.out.scopes[st.cur_scope.0 as usize].captures;
            if !captures.contains(&name) {
                captures.push(name);
            }
        }
        Capture::Parent => {
            // `@parent` (a base class) pairs with the `@def.class.name`
            // in the same match — record the inheritance edge from the
            // class as its def minted it. The base is shaped like a def
            // name (cpp canonicalizes a template-spelled base) so the edge
            // joins the identity the target class was filed under.
            if let Some(child) = super::defs::class_of_match(st, e.match_id).map(str::to_string) {
                if pack.names.use_map_sep().is_some() {
                    st.scopes.written_parents.push((st.out.parents.len(), e.span().start, st.cur_scope));
                }
                st.out.parents.push((child, (pack.shape_name)(&e.text)));
            }
        }
        _ => {}
    }
}

/// Resolve each written parent spelling to the identity it names, through
/// the file's use-map where it was written, from the scope it is written
/// in (a header's, where the document marks one): `extends Model` after `use
/// Illuminate\Database\Eloquent\Model;` is that class, an unimported leaf
/// is the namespace's own, a sibling the enclosing class body declares is
/// that nested class. Runs after every class is minted.
pub(super) fn resolve_parents(st: &mut ExtractState) {
    let out = &st.out;
    let resolved: Vec<(usize, String)> = std::mem::take(&mut st.scopes.written_parents)
        .into_iter()
        .filter_map(|(i, site, from)| {
            let map = crate::model::file_analysis::UseMap::in_scope(&out.use_rows, &out.namespace_ranges, &out.scopes, &st.pack.names, site, from, &out.symbols)?;
            Some((i, map.resolve(&out.parents[i].1).identity(map.sep)))
        })
        .collect();
    for (i, parent) in resolved {
        st.out.parents[i].1 = parent;
    }
}
