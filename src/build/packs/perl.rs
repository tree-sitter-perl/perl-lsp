//! Perl's pack — the query-engine seam for the language the native
//! builder owns.

use crate::build::query_extract::{LangPack, OutOfLineSpec, PeelSpec};
use crate::model::file_analysis::PackSpellings;

/// Perl writes and displays nothing of its own: the engine's type tags are
/// its vocabulary, and it offers no import or annotation quick-fix. Its
/// members belong to the container that declares them.
const SPELLINGS: PackSpellings = PackSpellings {
    members_are_package_bound: true,
    ..PackSpellings::NONE
};

/// The Perl-on-query-engine seam (go-live map ARC 3, the builder.rs shrink):
/// not registered as a driver — the native builder still owns Perl — but the
/// parity tests in query_extract_tests.rs measure it against the builder so
/// the migration path stays proven.
#[allow(dead_code)]
pub fn perl_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/perl/skeleton.scm"),
        bundled_overlays: &[],
        spellings: &SPELLINGS,
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
        default_name: |kind, _, _| match kind {
            "anon" => Some("(anon)".to_string()),
            _ => None,
        },
        annot_type: |_| None,
        rettype_receiver: |_| false,
        field_registry_edges: false,
        function_scoped_vars: false,
        constructor_names: &[],
        doc_types: |_, _| vec![],
        doc_uses_method_tags: &[],
        module_paths: |m| vec![format!("{}.pm", m.replace("::", "/"))],
        shape_ctor: |_| false,
        import_call: |_, _| None,
        cmd_effects: |_| vec![],
        narrow_guard: |_, _| None,
        narrow_assertions: &[],
        rebind_method: |_| false,
        implicit_this_members: false,
        include_path_tokens: false,
        preprocessor_macros: false,
        entrypoint_symbols: &[],
        runtime_invoked_methods: &[],
        brace_scoped_members: false,
        call_shapes: &[],
        arg_kind: "",
        implicit_variables: &[],
        throwaway_names: &[],
        catch_all_methods: &[],
        callable_placeholder_kind: "",
        pair_arrow: "=>",
        spread_arg_kind: "",
        named_arg_field: "",
        imports_bind_names: false,
        deprecated_attribute: "",
        builtin_types: &[],
        enum_members: &[],
        trigger_chars: &["$", "@", "%", ">", ":", "{"],
        nested_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: true },
        recv_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: false },
        op_map: &[],
        simple_var_kinds: &[],
        dynamic_arg_markers: &[],
        dynamic_var_markers: &[],
        qualifier_peel: &[],
        member_kinds: &[],
        skip_kinds: &[],
        call_kinds: &[],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
        oolfn: OutOfLineSpec::OFF,
    }
}
