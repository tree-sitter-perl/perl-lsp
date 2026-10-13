//! Python's pack.

use crate::build::query_extract::*;
use crate::model::file_analysis::{InferredType, MemberNamespace, NameSpellings};

pub fn python_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/python/skeleton.scm"),
        // One member namespace: the nearest declaration of a name wins,
        // whether it is a data member or a member function.
        names: NameSpellings { member_namespace: MemberNamespace::Shared, ..NameSpellings::NONE },
        shape_name: |raw| raw.to_string(),
        default_name: |_| None,
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
        module_paths: |m| {
            let base = m.replace('.', "/");
            vec![format!("{base}.py"), format!("{base}/__init__.py")]
        },
        // `isinstance(x, Foo)` narrows x to Foo inside the guard.
        narrow_guard: |guard, ty| (guard == Some("isinstance")).then(|| InferredType::ClassName(ty.to_string())),
        rebind_method: |_| false,
        implicit_this_members: false,
        entrypoint_symbols: &[],
        brace_scoped_members: false,
        trigger_chars: &["."],
        receiver_names: &["self", "cls"],
        recv_peel: &["parenthesized_expression"],
        // Python has one member operator (`.`), so no op-DX (op_map empty).
        op_map: &[],
        simple_var_kinds: &["identifier"],
        member_kinds: &["attribute"],
        skip_kinds: &["string", "string_content", "comment", "concatenated_string"],
        call_kinds: &["call"],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
    }
}
