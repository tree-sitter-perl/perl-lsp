//! Perl's pack: the query-engine path measured against the native builder.

use crate::build::query_extract::*;

pub fn perl_pack() -> LangPack {
    LangPack {
        id: "perl",
        query_source: include_str!("../../../queries/perl/skeleton.scm"),
        names: crate::model::conventions::PERL_SPELLINGS,
        lattice: crate::model::conventions::PERL_LATTICE.clone(),
        // The builder stores variable symbols WITH sigil. Nothing to
        // re-attach: def.var captures the whole `(scalar)` node, so the
        // raw text already carries it.
        shape_name: |raw| raw.to_string(),
        default_name: |kind, _| match kind {
            DefCap::Anon => Some("(anon)".to_string()),
            _ => None,
        },
        annot_type: |_| None,
        module_paths: crate::model::conventions::perl_module_paths,
        narrow_guard: |_, _| None,
        entrypoint_symbols: &[],
        brace_scoped_members: false,
        // Nothing reads them: Perl's trigger characters come from
        // `PerlDriver::caps`, and this pack is never registered.
        trigger_chars: &[],
        recv_peel: &[],
        simple_var_kinds: &[],
        member_kinds: &[],
        skip_kinds: &[],
        call_kinds: &[],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
    }
}
