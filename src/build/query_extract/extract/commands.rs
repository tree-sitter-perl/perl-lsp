//! `@cmd` / `@cmd.arg`: command-dispatched languages (CMake), where a
//! statement is a command name plus positional arguments. Every command is
//! a call of its name; what a command family declares, references or
//! imports, the document states with the def, ref and import captures.

use super::*;
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct State {
    /// per match, the command identifier and the scope it is written in
    cmd_names: BTreeMap<usize, (String, Span, ScopeId)>,
}

pub(super) fn handle(st: &mut ExtractState, e: &Event) {
    // An argument is counted through its command's match.
    if e.cap == Capture::Cmd(CmdCap::Name) {
        st.commands.cmd_names.insert(e.match_id, (e.text.clone(), e.span(), st.cur_scope));
    }
}

/// Each command's invocation ref: user functions rename through it, and a
/// builtin's name matches no def, harmlessly.
pub(super) fn finish(st: &mut ExtractState) {
    let c = &st.commands;
    for (mid, (cmd, cmd_span, scope)) in &c.cmd_names {
        st.out.refs.push(SkelRef {
            kind: RefCap::Call,
            name: cmd.clone(),
            start: cmd_span.start,
            end: cmd_span.end,
            scope: *scope,
            invocant: None,
            member_op: None,
            arg_count: Some(st.adorned.of_match(*mid).filter(|t| t.cap == Capture::Cmd(CmdCap::Arg)).count()),
        });
    }
}
