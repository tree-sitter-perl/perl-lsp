//! Python's pack.

use crate::build::query_extract::LangPack;
use crate::model::file_analysis::{InferredType, NameSpellings, PackSpellings};

/// Python writes and displays nothing of its own: the engine's type tags are
/// its vocabulary, and it offers no import or annotation quick-fix. Its
/// members belong to the container that declares them.
const SPELLINGS: PackSpellings = PackSpellings {
    variadic_marker: "*",
    default_sep: "=",
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
        annot_type: python_annot_type,
        declared_return: |t| {
            python_annot_type(t).map(crate::model::witnesses::ReturnExpr::Concrete)
        },
        doc_types: |_, _| vec![],
        doc_uses_method_tags: &[],
        module_paths: |m| {
            let base = m.replace('.', "/");
            vec![format!("{base}.py"), format!("{base}/__init__.py")]
        },
        import_module: |_, _| None,
        // A guard's type token is a class name verbatim (`isinstance(x, Foo)`).
        narrow_type: |ty| Some(InferredType::ClassName(ty.to_string())),
        brace_scoped_members: false,
        bundled_builtin_types: &[],
        enum_members: &[],
        trigger_chars: &["."],
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
