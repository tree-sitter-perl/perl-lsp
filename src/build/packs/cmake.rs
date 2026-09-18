//! CMake's pack — the command-dispatched language.

use crate::build::query_extract::{LangPack, PeelSpec};
use crate::model::file_analysis::{NameSpellings, PackSpellings};

/// CMake writes and displays nothing of its own: the engine's type tags are
/// its vocabulary, and it offers no import or annotation quick-fix. Its
/// members belong to the container that declares them.
const SPELLINGS: PackSpellings = PackSpellings {
    members_are_package_bound: true,
    ..PackSpellings::NONE
};

// Live only under `feature = "cmake"` (or the pack tests); see `python_pack`.
#[allow(dead_code)]
pub fn cmake_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/cmake/skeleton.scm"),
        bundled_overlays: &[],
        spellings: &SPELLINGS,
        lang_id: "cmake",
        bundled_entry_markers: &[],
        bundled_rail_docs: &[],
        names: NameSpellings::NONE,
        shape_name: |_, raw| raw.to_string(),
        default_name: |_, _, _| None,
        annot_type: |_| None,
        declared_return: |_| None,
        super_receiver: |_| false,
        self_class_tokens: &[],
        class_token_kinds: &[],
        function_scoped_vars: false,
        constructor_names: &[],
        doc_types: |_, _| vec![],
        doc_uses_method_tags: &[],
        // include(util.cmake) is a literal path; add_subdirectory(src)
        // means src/CMakeLists.txt. The whole resolution strategy.
        module_paths: |m| {
            if m.ends_with(".cmake") {
                vec![m.to_string()]
            } else {
                vec![format!("{m}/CMakeLists.txt"), format!("{m}.cmake")]
            }
        },
        import_module: |_, _| None,
        narrow_type: |_| None,
        implicit_this_members: false,
        brace_scoped_members: false,
        call_shapes: &[],
        arg_kind: "",
        implicit_variables: &[],
        throwaway_names: &[],
        catch_all_methods: &[],
        callable_placeholder_kind: "",
        pair_arrow: "",
        spread_arg_kind: "",
        named_arg_field: "",
        imports_bind_names: false,
        deprecated_attribute: "",
        bundled_builtin_types: &[],
        enum_members: &[],
        trigger_chars: &["{", "("],
        receiver_names: &[],
        recv_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: false },
        op_map: &[],
        simple_var_kinds: &[],
        dynamic_arg_markers: &[],
        dynamic_var_markers: &[],
        member_kinds: &[],
        skip_kinds: &[],
        call_kinds: &[],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
    }
}
