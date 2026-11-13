//! CMake's pack.

use crate::build::query_extract::*;
use crate::model::file_analysis::NameSpellings;

pub fn cmake_pack() -> LangPack {
    LangPack {
        id: "cmake",
        query_source: include_str!("../../../queries/cmake/skeleton.scm"),
        names: NameSpellings::NONE,
        free_names: FreeNameLookup::Global,
        construction: crate::model::file_analysis::ConstructionKey::Constructor,
        member_spaces: crate::model::file_analysis::MemberSpaces::PER_FAMILY,
        family_reach: crate::model::file_analysis::FamilyReach::Descendants,
        lattice: crate::model::file_analysis::LatticeRules::new(&[]),
        type_display: |_| None,
        shape_name: |raw| raw.to_string(),
        default_name: |_, _| None,
        annot_type: |_| None,
        annot_enforcement: crate::model::witnesses::Enforcement::Advisory,
        doc_text: |_| None,
        // include(util.cmake) is a literal path; add_subdirectory(src)
        // means src/CMakeLists.txt. The whole resolution strategy.
        module_paths: |m| {
            if m.ends_with(".cmake") {
                vec![m.to_string()]
            } else {
                vec![format!("{m}/CMakeLists.txt"), format!("{m}.cmake")]
            }
        },
        narrow_guard: |_| None,
        entrypoint_symbols: &[],
        brace_scoped_members: false,
        trigger_chars: &["{", "("],
        recv_peel: &[],
        simple_var_kinds: &[],
        skip_kinds: &[],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
    }
}
