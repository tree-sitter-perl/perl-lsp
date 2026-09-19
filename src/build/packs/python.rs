//! Python's pack.

use crate::build::query_extract::{LangPack, PeelSpec};
use crate::model::file_analysis::{InferredType, NameSpellings, PackSpellings};

/// Python writes and displays nothing of its own: the engine's type tags are
/// its vocabulary, and it offers no import or annotation quick-fix. Its
/// members belong to the container that declares them.
const SPELLINGS: PackSpellings = PackSpellings {
    variadic_marker: "*",
    default_sep: "=",
    members_are_package_bound: true,
    // a member read and a member call are different syntax here
    member_reads_are_calls: false,
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
        default_name: |_| None,
        annot_type: python_annot_type,
        declared_return: |t| {
            python_annot_type(t).map(crate::model::witnesses::ReturnExpr::Concrete)
        },
        module_paths: |m| {
            let base = m.replace('.', "/");
            vec![format!("{base}.py"), format!("{base}/__init__.py")]
        },
        import_module: |_, _| None,
        // `isinstance(x, Foo)` narrows x to Foo inside the guard.
        narrow_guard: |guard, ty| (guard == Some("isinstance")).then(|| InferredType::ClassName(ty.to_string())),
        implicit_this_members: false,
        brace_scoped_members: false,
        bundled_builtin_types: &[],
        trigger_chars: &["."],
        recv_peel: PeelSpec {
            wrappers: &[("parenthesized_expression", crate::model::file_analysis::DerefKind::Pointer)],
            annot_kinds: &[],
            leaf_to_def: &[],
            record_stack: false,
        },
        simple_var_kinds: &["identifier"],
        member_kinds: &["attribute"],
        skip_kinds: &["string", "string_content", "comment", "concatenated_string"],
        call_kinds: &["call"],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
    }
}

fn python_annot_type(text: &str) -> Option<InferredType> {
    match text.trim() {
        "str" => Some(InferredType::String),
        "int" | "float" => Some(InferredType::Numeric),
        "list" => Some(InferredType::ArrayRef),
        "dict" => Some(InferredType::HashRef),
        t if t.chars().next().is_some_and(|c| c.is_uppercase()) => {
            Some(InferredType::ClassName(t.to_string()))
        }
        _ => None,
    }
}
