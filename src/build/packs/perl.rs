//! Perl's pack: the query-engine path measured against the native builder.

use crate::build::query_extract::*;

pub fn perl_pack() -> LangPack {
    LangPack {
        id: "perl",
        query_source: include_str!("../../../queries/perl/skeleton.scm"),
        names: crate::model::conventions::PERL_SPELLINGS,
        functions: FunctionLookup::Global,
        construction: crate::model::file_analysis::ConstructionKey::Constructor,
        member_spaces: crate::model::conventions::PERL_CONVENTIONS.member_spaces.clone(),
        lattice: crate::model::conventions::PERL_CONVENTIONS.lattice.clone(),
        type_display: |_| None,
        // The builder stores variable symbols WITH sigil. Nothing to
        // re-attach: def.var captures the whole `(scalar)` node, so the
        // raw text already carries it.
        shape_name: |raw| raw.to_string(),
        default_name: |kind, _| match kind {
            DefCap::Anon => Some("(anon)".to_string()),
            _ => None,
        },
        annot_type: |_| None,
        module_paths: |m| vec![crate::model::conventions::perl_module_rel_path(m)],
        narrow_guard: |_| None,
        entrypoint_symbols: &[],
        brace_scoped_members: false,
        // Nothing reads them: Perl's trigger characters come from
        // `PerlDriver::caps`, and this pack is never registered.
        trigger_chars: &[],
        recv_peel: &[],
        simple_var_kinds: &[],
        skip_kinds: &[],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
    }
}
