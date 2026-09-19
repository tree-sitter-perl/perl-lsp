//! R's pack.

use crate::build::query_extract::{LangPack, PeelSpec};
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
        default_name: |_| None,
        annot_type: |_| None,
        declared_return: |_| None,
        // No reliable lexical ctor convention in R (S4/R5 exist but
        // rare); class typing arrives via shapes and S3 later.
        // source("util.R") hands us the path verbatim; library(pkg)
        // resolves into the installed-library tree (a real install
        // would consult .libPaths() — not modeled here).
        module_paths: |m| vec![m.to_string()],
        // Whichever call imports, R names the module in the ARGUMENT: a
        // sourced path verbatim, a library name into the installed tree.
        import_module: |_, arg| Some(arg.to_string()),
        narrow_guard: |_, _| None,
        implicit_this_members: false,
        brace_scoped_members: false,
        bundled_builtin_types: &[],
        trigger_chars: &["$", "@", ":"],
        recv_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: false },
        simple_var_kinds: &[],
        member_kinds: &[],
        skip_kinds: &[],
        call_kinds: &[],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
    }
}
