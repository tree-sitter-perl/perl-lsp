//! R's pack.

use crate::build::query_extract::*;
use crate::model::file_analysis::NameSpellings;

pub fn r_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/r/skeleton.scm"),
        names: NameSpellings::NONE,
        shape_name: |raw| raw.to_string(),
        default_name: |_| None,
        annot_type: |_| None,
        // No reliable lexical ctor convention in R (S4/R5 exist but
        // rare); class typing arrives via shapes and S3 later.
        // source("util.R") hands us the path verbatim; library(pkg)
        // resolves into the installed-library tree (a real install
        // would consult .libPaths() — not modeled here).
        module_paths: |m| vec![m.to_string()],
        narrow_guard: |_, _| None,
        rebind_method: |_| false,
        implicit_this_members: false,
        entrypoint_symbols: &[],
        brace_scoped_members: false,
        trigger_chars: &["$", "@", ":"],
        receiver_names: &[],
        recv_peel: &[],
        op_map: &[],
        simple_var_kinds: &[],
        member_kinds: &[],
        skip_kinds: &[],
        call_kinds: &[],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
    }
}
