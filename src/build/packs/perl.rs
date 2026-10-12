//! Perl's pack: the query-engine path measured against the native builder.

use crate::build::query_extract::*;

pub fn perl_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/perl/skeleton.scm"),
        names: crate::model::conventions::PERL_SPELLINGS,
        // The builder stores variable symbols WITH sigil. Nothing to
        // re-attach: def.var captures the whole `(scalar)` node, so the
        // raw text already carries it.
        shape_name: |raw| raw.to_string(),
        default_name: |kind| match kind {
            DefKind::Anon => Some("(anon)"),
            _ => None,
        },
        annot_type: |_| None,
        module_paths: |m| vec![format!("{}.pm", m.replace("::", "/"))],
        narrow_guard: |_, _| None,
        rebind_method: |_| false,
        implicit_this_members: false,
        include_path_tokens: false,
        preprocessor_macros: false,
        entrypoint_symbols: &[],
        brace_scoped_members: false,
        // Nothing reads them: Perl's trigger characters come from
        // `PerlDriver::caps`, and this pack is never registered.
        trigger_chars: &[],
        receiver_names: &[],
        nested_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: true },
        recv_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: false },
        op_map: &[],
        simple_var_kinds: &[],
        member_kinds: &[],
        skip_kinds: &[],
        call_kinds: &[],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
    }
}
