//! CMake's pack.

use crate::build::query_extract::*;
use crate::model::file_analysis::NameSpellings;

pub fn cmake_pack() -> LangPack {
    LangPack {
        id: "cmake",
        query_source: include_str!("../../../queries/cmake/skeleton.scm"),
        names: NameSpellings::NONE,
        lattice: crate::model::file_analysis::LatticeRules::new(&[]),
        shape_name: |raw| raw.to_string(),
        default_name: |_| None,
        annot_type: |_| None,
        // include(util.cmake) is a literal path; add_subdirectory(src)
        // means src/CMakeLists.txt. The whole resolution strategy.
        module_paths: |m| {
            if m.ends_with(".cmake") {
                vec![m.to_string()]
            } else {
                vec![format!("{m}/CMakeLists.txt"), format!("{m}.cmake")]
            }
        },
        narrow_guard: |_, _| None,
        entrypoint_symbols: &[],
        brace_scoped_members: false,
        trigger_chars: &["{", "("],
        recv_peel: &[],
        simple_var_kinds: &[],
        member_kinds: &[],
        skip_kinds: &[],
        call_kinds: &[],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
    }
}
