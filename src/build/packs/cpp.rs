//! C/C++'s pack.

use crate::build::query_extract::*;
use crate::model::file_analysis::{ClassCall, InferredType, MemberNamespace, NameSpellings};

pub fn cpp_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/cpp/skeleton.scm"),
        // One member namespace: the nearest declaration of a name wins,
        // whether it is a data member or a member function. Calling a class
        // constructs it (a functional cast, `Foo(x)`).
        names: NameSpellings {
            member_namespace: MemberNamespace::Shared,
            class_call: ClassCall::Constructs,
            ..NameSpellings::with_separator("::")
        },
        // Template spellings get ONE canonical whitespace form so a
        // specialization's identity (`formatter<int, char>`) matches
        // however the source wrapped it. Identity for every non-template
        // name (no whitespace, no comma → unchanged).
        shape_name: |raw| canonical_template_spelling(raw),
        // an anonymous inline union has no name token of its own; the
        // synthetic container is outline structure, not an addressable
        // member (the "anonymous" attribute keeps it out of completion).
        default_name: |kind, _| match kind {
            DefCap::UnionField => Some("(union)".to_string()),
            _ => None,
        },
        // C++ declared types ARE the witness source. Primitives → the
        // value lattice; `auto`/`void` defer (None → edge carries);
        // anything else identifier-shaped is a class instance.
        annot_type: |text| {
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
        },
        // #include "a/b.h" / <vector>: strip the delimiters; a quoted
        // path is workspace-relative verbatim, a system header resolves
        // through include dirs (library_roots, later). Tier 1: identity.
        module_paths: |m| {
            let p = m.trim_matches(|c: char| c == '"' || c == '<' || c == '>');
            vec![p.to_string()]
        },
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
        entrypoint_symbols: &["main"],
        brace_scoped_members: true,
        trigger_chars: &[".", ">", ":"],
        // `field_identifier` only ever names a struct/class member (the
        // grammar's own distinction from a plain `identifier` local), so
        // `Field` matches the plain (non-pointer) field pattern above.
        // Shared with the member-block synth lane (rule #10).
        recv_peel: &["parenthesized_expression", "pointer_expression"],
        simple_var_kinds: &["identifier"],
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
