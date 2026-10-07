//! `@cmd` / `@cmd.arg`: command-dispatched languages (CMake), where a
//! statement is a command name plus positional arguments and the pack's
//! `cmd_effects` says what the command does with them.

use super::*;
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct State {
    /// per match, the command identifier and its ordered arguments
    cmd_names: BTreeMap<usize, (String, Span, ScopeId)>,
    cmd_args: BTreeMap<usize, Vec<(String, Span)>>,
}

pub(super) fn handle(st: &mut ExtractState, e: &Event) {
    match e.cap {
        Capture::Cmd(CmdCap::Name) => {
            st.commands.cmd_names.insert(e.match_id, (e.text.clone(), e.span(), st.cur_scope));
        }
        Capture::Cmd(CmdCap::Arg) => {
            st.commands.cmd_args.entry(e.match_id).or_default().push((e.text.clone(), e.span()));
        }
        _ => {}
    }
}

/// Classify each command's effects into refs, defs and imports.
pub(super) fn finish(st: &mut ExtractState) {
    let c = &st.commands;
    let out = &mut st.out;
    for (mid, (cmd, cmd_span, scope)) in &c.cmd_names {
        let args = c.cmd_args.get(mid).cloned().unwrap_or_default();
        // every invocation identifier is a call ref (user functions
        // rename through it; builtin names match no defs, harmlessly)
        out.refs.push(SkelRef {
            kind: RefKind::Call,
            name: cmd.clone(),
            start: cmd_span.start,
            end: cmd_span.end,
            scope: *scope,
            invocant: None,
            member_op: None,
            arg_count: Some(args.len()),
        });
        for effect in (st.pack.cmd_effects)(cmd) {
            match effect {
                CmdEffect::Def { kind, name_arg } => {
                    if let Some((name, span)) = args.get(name_arg) {
                        out.symbols.push(SkelSymbol {
                            kind,
                            name: name.clone(),
                            start: cmd_span.start,
                            end: span.end,
                            name_start: span.start,
                            name_end: span.end,
                            package: None,
                            scope: *scope,
                            return_type: None,
                            deref_stack: Vec::new(),
                            attributes: Vec::new(),
                            arity: None,
                            qualifier_owned: false,
                        });
                    }
                }
                CmdEffect::RefArgsFrom { from } => {
                    for (name, span) in args.iter().skip(from) {
                        let is_keyword =
                            !name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c == '_');
                        if !is_keyword && !name.contains("${") {
                            out.refs.push(SkelRef {
                                kind: RefKind::Call,
                                name: name.clone(),
                                start: span.start,
                                end: span.end,
                                scope: *scope,
                                invocant: None,
                                member_op: None,
                                arg_count: None,
                            });
                        }
                    }
                }
                CmdEffect::Import { arg } => {
                    if let Some((name, _)) = args.get(arg) {
                        if !out.imports.contains(name) {
                            out.imports.push(name.clone());
                        }
                    }
                }
            }
        }
    }
}
