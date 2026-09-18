//! R's pack.

use crate::build::query_extract::{LangPack, OutOfLineSpec, PeelSpec};
use crate::model::file_analysis::{NameSpellings, PackSpellings};

/// R writes and displays nothing of its own: the engine's type tags are
/// its vocabulary, and it offers no import or annotation quick-fix. Its
/// members belong to the container that declares them.
const SPELLINGS: PackSpellings = PackSpellings {
    members_are_package_bound: true,
    ..PackSpellings::NONE
};

// Live only under `feature = "r"` (or the pack tests); see `python_pack`.
#[allow(dead_code)]
pub fn r_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/r/skeleton.scm"),
        bundled_overlays: &[],
        spellings: &SPELLINGS,
        lang_id: "r",
        bundled_entry_markers: &[],
        bundled_rail_docs: &[],
        names: NameSpellings::NONE,
        shape_name: |_, raw| raw.to_string(),
        default_name: |_, _, _| None,
        annot_type: |_| None,
        rettype_receiver: |_| false,
        field_registry_edges: false,
        function_scoped_vars: false,
        doc_types: |_, _| vec![],
        doc_uses_method_tags: &[],
        // No reliable lexical ctor convention in R (S4/R5 exist but
        // rare); class typing arrives via shapes and S3 later.
        // source("util.R") hands us the path verbatim; library(pkg)
        // resolves into the installed-library tree (a real install
        // would consult .libPaths() — not modeled here).
        module_paths: |m| vec![m.to_string()],
        shape_ctor: |callee| matches!(callee, "list" | "data.frame" | "tibble"),
        import_call: |callee, arg| match callee {
            "library" | "require" | "source" => Some(arg.to_string()),
            _ => None,
        },
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
        implicit_variables: &[],
        throwaway_names: &[],
        catch_all_methods: &[],
        pair_arrow: "",
        imports_bind_names: false,
        deprecated_attribute: "",
        builtin_types: &[],
        enum_members: &[],
        trigger_chars: &["$", "@", ":"],
        nested_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: true },
        dynamic_arg_markers: &[],
        dynamic_var_markers: &[],
        qualifier_peel: &[],
        oolfn: OutOfLineSpec::OFF,
    }
}
