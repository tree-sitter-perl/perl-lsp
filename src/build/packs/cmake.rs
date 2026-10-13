//! CMake's pack.

use crate::build::query_extract::*;
use crate::model::file_analysis::NameSpellings;

pub fn cmake_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/cmake/skeleton.scm"),
        names: NameSpellings::NONE,
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
        rebind_method: |_| false,
        implicit_this_members: false,
        include_path_tokens: false,
        preprocessor_macros: false,
        entrypoint_symbols: &[],
        brace_scoped_members: false,
        trigger_chars: &["{", "("],
        receiver_names: &[],
        nested_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: true },
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
