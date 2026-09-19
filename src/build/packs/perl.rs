//! Perl's pack — the query-engine seam for the language the native
//! builder owns.

use crate::build::query_extract::{LangPack, PeelSpec};

/// The Perl-on-query-engine seam (go-live map ARC 3, the builder.rs shrink):
/// not registered as a driver — the native builder still owns Perl — but the
/// parity tests in query_extract_tests.rs measure it against the builder so
/// the migration path stays proven.
#[allow(dead_code)]
pub fn perl_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/perl/skeleton.scm"),
        bundled_overlays: &[],
        // Perl's spellings have one home, and it is not here.
        spellings: &crate::model::conventions::PERL_SPELLINGS_PACK,
        lang_id: "perl",
        bundled_entry_markers: &[],
        bundled_rail_docs: &[],
        names: crate::model::conventions::PERL_SPELLINGS,
        shape_name: |kind, raw| match kind {
            // The builder stores variable symbols WITH sigil; varname
            // captures are sigil-less. Predicate re-attaches nothing —
            // def.var captures the whole `(scalar)` node so raw text
            // already carries the sigil.
            _ => raw.to_string(),
        },
        default_name: |kind| match kind {
            "anon" => Some("(anon)"),
            _ => None,
        },
        annot_type: |_| None,
        declared_return: |_| None,
        module_paths: |m| vec![format!("{}.pm", m.replace("::", "/"))],
        import_module: |_, _| None,
        narrow_guard: |_, _| None,
        rebind_method: |_| false,
        implicit_this_members: false,
        brace_scoped_members: false,
        bundled_builtin_types: &[],
        trigger_chars: &["$", "@", "%", ">", ":", "{"],
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
