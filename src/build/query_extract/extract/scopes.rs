//! `@scope`, `@context.*`, `@parent`: the lexical tree, the sticky
//! container context, and the inheritance edges declared with a class.

use super::*;
use crate::model::file_analysis::{PackageKind, PackageRange};
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct State {
    /// A context whose same-match `@scope` starts AFTER it (C++ namespace:
    /// `@context` is on the name, `@scope` on the body `{`) must be
    /// registered at the BODY depth, not the file depth — else it goes
    /// sticky and leaks past the closing brace. Each match's scope start,
    /// pre-indexed; such contexts defer to the scope push.
    scope_start_by_match: HashMap<usize, usize>,
    pending_context: HashMap<usize, (ContextKind, String)>,
    /// Parent edges whose parent is still the written spelling: its index
    /// in `out.parents`, the namespace in force where it was written, and
    /// where. Resolved through the use-map once every import row is in.
    written_parents: Vec<(usize, Option<String>, Point)>,
    /// Statement-form namespace sections still running, by the depth they
    /// were declared at: the next one at that depth ends them.
    open_sections: Vec<(usize, usize)>,
}

/// Record a namespace section under a use-map: a braced one is its body,
/// a statement-form one runs to the end of the enclosing scope until the
/// next one at its depth cuts it short.
fn open_section(st: &mut ExtractState, namespace: &str, span: Span, kind: PackageKind) {
    if st.pack.names.use_map_sep().is_none() {
        return;
    }
    let depth = st.scope_stack.len();
    if kind == PackageKind::Statement {
        while st.scopes.open_sections.last().is_some_and(|&(d, _)| d >= depth) {
            let (_, i) = st.scopes.open_sections.pop().unwrap();
            st.out.namespace_sections[i].span.end = span.start;
        }
        st.scopes.open_sections.push((depth, st.out.namespace_sections.len()));
    }
    st.out.namespace_sections.push(PackageRange { package: namespace.to_string(), span, kind });
}

pub(super) fn collect(st: &mut ExtractState, e: &Event) {
    if let Capture::Scope(_) = e.cap {
        st.scopes.scope_start_by_match.entry(e.match_id).or_insert(e.start_byte);
    }
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
            st.out.scopes.push(Scope {
                id,
                parent: Some(st.cur_scope),
                kind: match scope_cap {
                    ScopeCap::Sub | ScopeCap::SubImplicitReceiver => ScopeKind::Sub { name: String::new() },
                    ScopeCap::Block => ScopeKind::Block,
                },
                span: e.span(),
                package: st.package.clone(),
                owner: None,
                implicit_receiver: scope_cap == ScopeCap::SubImplicitReceiver,
            });
            st.scope_stack.push((e.end_byte, id));
            st.out.scope_count += 1;
            // a context deferred to THIS scope (C++ namespace) →
            // register at the body depth so it pops with the block.
            if let Some((kind, text)) = st.scopes.pending_context.remove(&e.match_id) {
                while st.context_stack.last().is_some_and(|&(d, ..)| d >= st.scope_stack.len()) {
                    st.context_stack.pop();
                }
                if kind == ContextKind::Namespace {
                    open_section(st, &text, e.span(), PackageKind::Block);
                }
                st.context_stack.push((st.scope_stack.len(), kind, text));
            }
            narrowing::on_scope_push(st, e, id);
        }
        Capture::Context(kind) => {
            // Shape the context like a def name (cpp canonicalizes a
            // spec's template spelling) so members' `package` matches
            // the container Symbol's identity exactly.
            let text = (pack.shape_name)(&e.text);
            // A class body's members file under the class's identity.
            let identity = (kind == ContextKind::Class && pack.names.use_map_sep().is_some())
                .then(|| st.class_identity(&text));
            // If this match's `@scope` starts AFTER this context, the
            // context belongs to that (not-yet-pushed) body — defer it
            // so it registers at the body depth and pops with the block.
            if st.scopes.scope_start_by_match.get(&e.match_id).is_some_and(|&s| s > e.start_byte) {
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
                    open_section(st, &value, Span { start: e.span().start, end }, PackageKind::Statement);
                }
                // A scope opened by this same match is the container the
                // context names (php's class node), so a point anywhere in
                // it, header included, is in that package.
                if st.scopes.scope_start_by_match.contains_key(&e.match_id) {
                    if let Some(&(_, id)) = st.scope_stack.last() {
                        st.out.scopes[id.0 as usize].package = Some(value.clone());
                    }
                }
                st.context_stack.push((st.scope_stack.len(), kind, value));
            }
        }
        Capture::Parent => {
            // `@parent` (a base class) pairs with the `@def.class.name`
            // in the same match — record the inheritance edge.
            if let Some(child) = st.defs.name_of(e.match_id, DefCap::Class) {
                // Shaped like the child's def name (cpp canonicalizes a
                // template-spelled base) so the edge joins the identity
                // the target class was filed under. The parent pattern is a
                // match of its own, so the child's identity is minted here
                // the way the def handler mints it.
                let edge = (st.class_identity(child), (pack.shape_name)(&e.text));
                if pack.names.use_map_sep().is_some() {
                    let ns = st.namespace().map(str::to_string);
                    st.scopes.written_parents.push((st.out.parents.len(), ns, e.span().start));
                }
                st.out.parents.push(edge);
            }
        }
        _ => {}
    }
}

/// Resolve each written parent spelling to the identity it names, through
/// the file's use-map at the namespace it was written in: `extends Model`
/// after `use Illuminate\Database\Eloquent\Model;` is that class, an
/// unimported leaf is the namespace's own.
pub(super) fn resolve_parents(st: &mut ExtractState) {
    let Some(sep) = st.pack.names.use_map_sep() else { return };
    for (i, ns, site) in std::mem::take(&mut st.scopes.written_parents) {
        let map = crate::model::file_analysis::UseMap {
            rows: &st.out.use_rows,
            own_namespace: ns.as_deref(),
            sep,
            site: Some(site),
        };
        st.out.parents[i].1 = map.resolve(&st.out.parents[i].1).identity(sep);
    }
}
