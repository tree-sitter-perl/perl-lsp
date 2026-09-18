//! R's pack.

use crate::build::query_extract::{LangPack, OutOfLineSpec, PeelSpec};
use crate::model::file_analysis::NameSpellings;

// Live only under `feature = "r"` (or the pack tests); see `python_pack`.
#[allow(dead_code)]
pub fn r_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/r/skeleton.scm"),
        bundled_overlays: &[],
        lang_id: "r",
        bundled_entry_markers: &[],
        bundled_rail_docs: &[],
        names: NameSpellings::NONE,
        shape_name: |_, raw| raw.to_string(),
        default_name: |_| None,
        annot_type: |_| None,
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
        rebind_method: |_| false,
        implicit_this_members: false,
        entrypoint_symbols: &[],
        brace_scoped_members: false,
        bundled_builtin_types: &[],
        trigger_chars: &["$", "@", ":"],
        receiver_names: &[],
        nested_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: true },
        recv_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: false },
        op_map: &[],
        simple_var_kinds: &[],
        qualifier_peel: &[],
        member_kinds: &[],
        skip_kinds: &[],
        call_kinds: &[],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
        oolfn: OutOfLineSpec::OFF,
    }
}
