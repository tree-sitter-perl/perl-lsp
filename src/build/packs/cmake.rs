//! CMake's pack — the command-dispatched language.

use crate::build::query_extract::{CmdEffect, LangPack, OutOfLineSpec, PeelSpec};
use crate::model::file_analysis::{NameSpellings, PackSpellings};

/// CMake writes and displays nothing of its own: the engine's type tags are
/// its vocabulary, and it offers no import or annotation quick-fix. Its
/// members belong to the container that declares them.
const SPELLINGS: PackSpellings = PackSpellings {
    members_are_package_bound: true,
    ..PackSpellings::NONE
};

// Live only under `feature = "cmake"` (or the pack tests); see `python_pack`.
// Sole constructor of the `CmdEffect` variants.
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
        rettype_receiver: |_| false,
        field_registry_edges: false,
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
        shape_ctor: |_| false,
        import_call: |_, _| None,
        cmd_effects: |cmd| match cmd.to_ascii_lowercase().as_str() {
            "set" | "option" => vec![CmdEffect::Def { kind: "var", name_arg: 0 }],
            "add_library" | "add_executable" | "add_custom_target" => {
                // Targets. SymKind::Target is the real future; "sub"
                // rides the full rename/refs machinery today.
                vec![CmdEffect::Def { kind: "sub", name_arg: 0 }]
            }
            "target_link_libraries" | "target_include_directories"
            | "target_compile_definitions" | "target_sources" => vec![
                CmdEffect::RefArgsFrom { from: 0 },
            ],
            "include" | "add_subdirectory" => vec![CmdEffect::Import { arg: 0 }],
            _ => vec![],
        },
        narrow_type: |_| None,
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
        pair_arrow: "",
        spread_arg_kind: "",
        named_arg_field: "",
        imports_bind_names: false,
        deprecated_attribute: "",
        builtin_types: &[],
        enum_members: &[],
        trigger_chars: &["{", "("],
        receiver_names: &[],
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
