//! R's pack.

use crate::build::query_extract::*;
use crate::model::file_analysis::NameSpellings;

pub fn r_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/r/skeleton.scm"),
        names: NameSpellings::NONE,
        // A logical is a number in arithmetic (`TRUE + 1` is 2).
        lattice: crate::model::file_analysis::LatticeRules::new(&[crate::model::file_analysis::Subsumption::BoolIntoNumeric]),
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
        entrypoint_symbols: &[],
        brace_scoped_members: false,
        trigger_chars: &["$", "@", ":"],
        recv_peel: &[],
        simple_var_kinds: &[],
        member_kinds: &[],
        skip_kinds: &[],
        call_kinds: &[],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
    }
}
