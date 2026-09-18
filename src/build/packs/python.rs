//! Python's pack.

use crate::build::query_extract::{LangPack, OutOfLineSpec, PeelSpec};
use crate::model::file_analysis::{InferredType, NameSpellings, PackSpellings};

/// Python writes and displays nothing of its own: the engine's type tags are
/// its vocabulary, and it offers no import or annotation quick-fix. Its
/// members belong to the container that declares them.
const SPELLINGS: PackSpellings = PackSpellings {
    members_are_package_bound: true,
    ..PackSpellings::NONE
};

// Registered by `python_driver` only under `feature = "python"` (and driven by
// the pack tests); dead weight in a single-language build like `cpp`-only.
#[allow(dead_code)]
pub fn python_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/python/skeleton.scm"),
        bundled_overlays: &[],
        spellings: &SPELLINGS,
        lang_id: "python",
        bundled_entry_markers: &[],
        bundled_rail_docs: &[],
        names: NameSpellings::NONE,
        shape_name: |_, raw| raw.to_string(),
        default_name: |_, _, _| None,
        annot_type: |text| match text.trim() {
            "str" => Some(InferredType::String),
            "int" | "float" => Some(InferredType::Numeric),
            "list" => Some(InferredType::ArrayRef),
            "dict" => Some(InferredType::HashRef),
            t if t.chars().next().is_some_and(|c| c.is_uppercase()) => {
                Some(InferredType::ClassName(t.to_string()))
            }
            _ => None,
        },
        rettype_receiver: |_| false,
        field_registry_edges: false,
        super_receiver: |_| false,
        self_class_tokens: &[],
        class_token_kinds: &[],
        function_scoped_vars: false,
        constructor_names: &[],
        doc_types: |_, _| vec![],
        doc_uses_method_tags: &[],
        module_paths: |m| {
            let base = m.replace('.', "/");
            vec![format!("{base}.py"), format!("{base}/__init__.py")]
        },
        shape_ctor: |_| false,
        import_call: |_, _| None,
        cmd_effects: |_| vec![],
        // `isinstance(x, Foo)` narrows x to Foo inside the guard.
        narrow_guard: |guard, ty| (guard == Some("isinstance")).then(|| InferredType::ClassName(ty.to_string())),
        narrow_assertions: &[],
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
        trigger_chars: &["."],
        receiver_names: &["self", "cls"],
        nested_peel: PeelSpec { wrappers: &[], annot_kinds: &[], leaf_to_def: &[], record_stack: true },
        recv_peel: PeelSpec {
            wrappers: &[("parenthesized_expression", crate::model::file_analysis::DerefKind::Pointer)],
            annot_kinds: &[],
            leaf_to_def: &[],
            record_stack: false,
        },
        // Python has one member operator (`.`), so no op-DX (op_map empty).
        op_map: &[],
        simple_var_kinds: &["identifier"],
        dynamic_arg_markers: &[],
        dynamic_var_markers: &[],
        qualifier_peel: &[],
        member_kinds: &["attribute"],
        skip_kinds: &["string", "string_content", "comment", "concatenated_string"],
        call_kinds: &["call"],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
        oolfn: OutOfLineSpec::OFF,
    }
}
