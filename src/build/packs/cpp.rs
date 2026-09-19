//! C/C++'s pack.

use crate::build::query_extract::{LangPack, PeelSpec, C_FIELD_DECL_PEEL};
use crate::model::file_analysis::{canonical_template_spelling, InferredType, NameSpellings, PackSpellings};

/// C/C++ writes and displays nothing of its own: the engine's type tags are
/// its vocabulary, and it offers no import or annotation quick-fix. Its
/// members belong to the container that declares them.
const SPELLINGS: PackSpellings = PackSpellings {
    variadic_marker: "...",
    default_sep: " = ",
    members_are_package_bound: true,
    // a member read and a member call are different syntax here
    member_reads_are_calls: false,
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
        default_name: |kind| match kind {
            "unionfield" => Some("(union)"),
            _ => None,
        },
        annot_type: cpp_annot_type,
        declared_return: |t| cpp_annot_type(t).map(crate::model::witnesses::ReturnExpr::Concrete),
        // #include "a/b.h" / <vector>: strip the delimiters; a quoted
        // path is workspace-relative verbatim, a system header resolves
        // through include dirs (library_roots, later). Tier 1: identity.
        module_paths: |m| {
            let p = m.trim_matches(|c: char| c == '"' || c == '<' || c == '>');
            vec![p.to_string()]
        },
        import_module: |_, _| None,
        cmd_effects: |_| vec![],
        // Two narrowings, both keyed on what the value IS, not a name allowlist:
        //   `if (dynamic_cast<Derived*>(b))` — b is a Derived inside (ty is the
        //     template arg; pointer-ness dropped for navigation, like locals).
        //   `if (opt)` / `if (opt.has_value())` — an engaged std::optional<T>
        //     holds a T inside (ty is opt's DECLARED type; peel the inner T).
        //     The bare form carries no guard token; `.has_value()` gates the
        //     method so `opt.value_or(x)` (not an engagement test) won't narrow.
        narrow_guard: |guard, ty| {
            let class = match guard {
                Some("dynamic_cast") => ty.to_string(),
                None | Some("has_value") => optional_inner(ty)?,
                _ => return None,
            };
            Some(InferredType::ClassName(class))
        },
        // Rebinding methods: a moved-from object is put back into a known state
        // by these std container/optional/smart-ptr resets, so a use after one
        // is NOT a use-after-move. (An ordinary `x.use()` is not here, so the
        // canonical bug still flags.)
        rebind_method: |m| {
            matches!(m, "clear" | "reset" | "assign" | "emplace" | "swap")
        },
        // C/C++ methods read members with an implicit `this->`.
        implicit_this_members: true,
        brace_scoped_members: true,
        bundled_builtin_types: &[],
        trigger_chars: &[".", ">", ":"],
        receiver_names: &["this"],
        // `field_identifier` only ever names a struct/class member (the
        // grammar's own distinction from a plain `identifier` local), so
        // "def.field" matches the plain (non-pointer) field pattern above.
        // Shared with the member-block synth lane (rule #10).
        // DerefKind placeholder — record_stack false, so it's never read.
        recv_peel: PeelSpec {
            wrappers: &[
                ("parenthesized_expression", crate::model::file_analysis::DerefKind::Pointer),
                ("pointer_expression", crate::model::file_analysis::DerefKind::Pointer),
            ],
            annot_kinds: &[],
            leaf_to_def: &[],
            record_stack: false,
        },
        op_map: &[
            ("->", crate::model::file_analysis::MemberOp::Arrow),
            (".", crate::model::file_analysis::MemberOp::Dot),
        ],
        simple_var_kinds: &["identifier"],
        // a templated qualifier (`Buf<T>::grow`) owns by its BASE class name
        member_kinds: &["field_expression"],
        skip_kinds: &["string_literal", "char_literal", "raw_string_literal", "comment"],
        call_kinds: &["call_expression"],
        domain_compare_kinds: &["binary_expression"],
        domain_compare_ops: &["==", "!="],
    }
}

/// Peel `T` out of a `std::optional<T>` declared-type text, unqualified
/// (matching how `annot_type` keys classes by their last `::` segment). `None`
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
