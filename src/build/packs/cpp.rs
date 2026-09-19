//! C/C++'s pack.

use crate::build::query_extract::LangPack;
use crate::model::file_analysis::{canonical_template_spelling, InferredType, NameSpellings, PackSpellings};

/// C/C++ writes and displays nothing of its own: the engine's type tags are
/// its vocabulary, and it offers no import or annotation quick-fix. Its
/// members belong to the container that declares them.
const SPELLINGS: PackSpellings = PackSpellings {
    variadic_marker: "...",
    default_sep: " = ",
    members_are_package_bound: true,
    ..PackSpellings::NONE
};

pub fn cpp_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/cpp/skeleton.scm"),
        bundled_overlays: &[],
        spellings: &SPELLINGS,
        lang_id: "cpp",
        // `main` is entered over the ABI, never from a source call site.
        bundled_entry_markers: &[include_str!("../../../queries/cpp/cpp.entry.json")],
        bundled_rail_docs: &[],
        names: NameSpellings::with_separator("::"),
        // Template spellings get ONE canonical whitespace form so a
        // specialization's identity (`formatter<int, char>`) matches
        // however the source wrapped it. Identity for every non-template
        // name (no whitespace, no comma → unchanged).
        shape_name: |_, raw| canonical_template_spelling(raw),
        // an anonymous inline union has no name token of its own; the
        // synthetic container is outline structure, not an addressable
        // member (the "anonymous" attribute keeps it out of completion).
        default_name: |kind, _, _| match kind {
            "unionfield" => Some("(union)".to_string()),
            _ => None,
        },
        annot_type: cpp_annot_type,
        // A C++ return spelling is always concrete — no late-bound receiver
        // spelling exists in the language.
        declared_return: |t| cpp_annot_type(t).map(crate::model::witnesses::ReturnExpr::Concrete),
        doc_types: |_, _| vec![],
        doc_uses_method_tags: &[],
        // #include "a/b.h" / <vector>: strip the delimiters; a quoted
        // path is workspace-relative verbatim, a system header resolves
        // through include dirs (library_roots, later). Tier 1: identity.
        module_paths: |m| {
            let p = m.trim_matches(|c: char| c == '"' || c == '<' || c == '>');
            vec![p.to_string()]
        },
        import_module: |_, _| None,
        // An engaged `std::optional<T>` holds a T, so an optional spelling
        // refines to its inner class; anything else denotes the class it
        // spells (`dynamic_cast<Derived*>` → Derived), which is `annot_type`'s
        // job — a primitive or template spelling refines nothing.
        narrow_type: |ty| {
            if let Some(inner) = optional_inner(ty) {
                return Some(InferredType::ClassName(inner));
            }
            match cpp_annot_type(ty) {
                Some(InferredType::ClassName(c)) => Some(InferredType::ClassName(c)),
                _ => None,
            }
        },
        brace_scoped_members: true,
        bundled_builtin_types: &[],
        enum_members: &[],
        trigger_chars: &[".", ">", ":"],
    }
}



/// Peel `T` out of a `std::optional<T>` declared-type text, unqualified
/// (matching how `cpp_annot_type` keys classes by their last `::` segment). `None`
/// when the text isn't an optional — the type-side gate that keeps the
/// token-less `if (opt)` narrowing from firing on non-optional subjects.
fn optional_inner(ty: &str) -> Option<String> {
    let inner = ty
        .trim()
        .strip_prefix("std::optional<")
        .or_else(|| ty.trim().strip_prefix("optional<"))?
        .strip_suffix('>')?
        .trim();
    let leaf = inner.rsplit("::").next().unwrap_or(inner).trim();
    (!leaf.is_empty()
        && !leaf.contains(' ')
        && leaf.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_'))
    .then(|| leaf.to_string())
}

/// C++ declared types ARE the witness source. Primitives → the value
/// lattice; `auto`/`void` defer (None → the edge carries); anything else
/// identifier-shaped is a class instance.
fn cpp_annot_type(text: &str) -> Option<InferredType> {
    use InferredType::*;
    match text.trim() {
        "int" | "long" | "short" | "unsigned" | "size_t" | "int32_t" | "int64_t"
        | "uint32_t" | "uint64_t" | "double" | "float" | "char" => Some(Numeric),
        "bool" => Some(Bool),
        "std::string" | "string" | "std::string_view" => Some(String),
        "auto" | "void" => None,
        t => {
            // Elaborated type specifier `struct op` / `union u` /
            // `enum e` — the dominant C spelling (`struct op* o`).
            // The tag names the type; strip the keyword so it resolves
            // the same as the bare/typedef'd name.
            let tag = t
                .strip_prefix("struct ")
                .or_else(|| t.strip_prefix("union "))
                .or_else(|| t.strip_prefix("enum "))
                .unwrap_or(t)
                .trim();
            // A template spelling (`Box<Widget>`, `vector<int>`)
            // peels into the Instance flavor: dispatch keys the
            // BASE so members resolve through the plain-class
            // machinery; the args ride along for substitution.
            if let Some(p) =
                crate::model::file_analysis::ParametricType::instance_from_spelling(tag)
            {
                return Some(Parametric(p));
            }
            let typeish = !tag.is_empty()
                && !tag.contains(' ')
                && tag.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_');
            // Strip the namespace qualifier — classes/members are
            // keyed by the unqualified name (@context.class), so
            // `geo::Circle` must type as `Circle` to resolve.
            typeish.then(|| ClassName(tag.rsplit("::").next().unwrap_or(tag).to_string()))
        }
    }
}
