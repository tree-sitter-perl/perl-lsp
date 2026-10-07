//! `@scope`, `@context.*`, `@parent`: the lexical tree, the sticky
//! container context, and the inheritance edges declared with a class.

use super::*;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct State {
    /// A context whose same-match `@scope` starts AFTER it (C++ namespace:
    /// `@context` is on the name, `@scope` on the body `{`) must be
    /// registered at the BODY depth, not the file depth — else it goes
    /// sticky and leaks past the closing brace. Each match's scope start,
    /// pre-indexed; such contexts defer to the scope push.
    scope_start_by_match: HashMap<usize, usize>,
    pending_context: HashMap<usize, String>,
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
        // the scope (the Symbol holds identity).
        Capture::Scope(scope_cap) => {
            let id = ScopeId(st.out.scopes.len() as u32);
            st.out.scopes.push(Scope {
                id,
                parent: Some(st.cur_scope),
                kind: match scope_cap {
                    ScopeCap::Sub => ScopeKind::Sub { name: String::new() },
                    ScopeCap::Block => ScopeKind::Block,
                },
                span: e.span(),
                package: st.package.clone(),
                owner: None,
            });
            st.scope_stack.push((e.end_byte, id));
            st.out.scope_count += 1;
            // a context deferred to THIS scope (C++ namespace) →
            // register at the body depth so it pops with the block.
            if let Some(text) = st.scopes.pending_context.remove(&e.match_id) {
                while st.context_stack.last().is_some_and(|&(d, _)| d >= st.scope_stack.len()) {
                    st.context_stack.pop();
                }
                st.context_stack.push((st.scope_stack.len(), text));
            }
            narrowing::on_scope_push(st, e, id);
        }
        Capture::Context(_) => {
            // Shape the context like a def name (cpp canonicalizes a
            // spec's template spelling) so members' `package` matches
            // the container Symbol's identity exactly.
            let text = (pack.shape_name)(&e.text);
            // If this match's `@scope` starts AFTER this context, the
            // context belongs to that (not-yet-pushed) body — defer it
            // so it registers at the body depth and pops with the block.
            if st.scopes.scope_start_by_match.get(&e.match_id).is_some_and(|&s| s > e.start_byte) {
                st.scopes.pending_context.insert(e.match_id, text);
            } else {
                // Replace any context at the same depth; deeper ones
                // were already popped with their scopes.
                while st.context_stack.last().is_some_and(|&(d, _)| d >= st.scope_stack.len()) {
                    st.context_stack.pop();
                }
                st.context_stack.push((st.scope_stack.len(), e.text.clone()));
            }
        }
        Capture::Parent => {
            // `@parent` (a base class) pairs with the `@def.class.name`
            // in the same match — record the inheritance edge.
            if let Some(child) = st.defs.name_of(e.match_id, DefKind::Class) {
                // Shaped like the child's def name (cpp canonicalizes a
                // template-spelled base) so the edge joins the identity
                // the target class was filed under.
                let edge = (child.to_string(), (pack.shape_name)(&e.text));
                st.out.parents.push(edge);
            }
        }
        _ => {}
    }
}
