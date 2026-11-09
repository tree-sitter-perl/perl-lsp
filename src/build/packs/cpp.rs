//! C/C++'s pack.

use crate::build::query_extract::*;
use crate::model::file_analysis::{ClassCall, InferredType, MemberSpaces, NameSpellings};

/// C++'s scope-resolution separator, as the pack declares it.
const CPP_SEP: &str = "::";

pub fn cpp_pack() -> LangPack {
    LangPack {
        id: "cpp",
        query_source: include_str!("../../../queries/cpp/skeleton.scm"),
        // One member namespace: the nearest declaration of a name wins,
        // whether it is a data member or a member function. Calling a class
        // constructs it (a functional cast, `Foo(x)`).
        names: NameSpellings {
            class_call: ClassCall::Constructs,
            // TODO(construction-navigation): `Foo(x)` reaches no
            // constructor yet, so a constructor's references do not list it.
            ..NameSpellings::with_separator(CPP_SEP)
        },
        functions: FunctionLookup::Global,
        construction: crate::model::file_analysis::ConstructionKey::Constructor,
        // One member namespace (see `names`).
        member_spaces: MemberSpaces::SHARED,
        // A derived class reaches its bases' protected members, a base
        // never a derived class's.
        family_reach: crate::model::file_analysis::FamilyReach::Descendants,
        // `bool` promotes to `int`.
        lattice: crate::model::file_analysis::LatticeRules::new(&[crate::model::file_analysis::Subsumption::BoolIntoNumeric]),
        type_display: |_| None,
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
        // C++ declared types ARE the witness source.
        annot_type,
        doc_text: |_| None,
        // #include "a/b.h" / <vector>: strip the delimiters; a quoted
        // path is workspace-relative verbatim, a system header resolves
        // through include dirs (library_roots, later). Tier 1: identity.
        module_paths: |m| {
            let p = m.trim_matches(|c: char| c == '"' || c == '<' || c == '>');
            vec![p.to_string()]
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
        skip_kinds: &["string_literal", "char_literal", "raw_string_literal", "comment"],
        domain_compare_kinds: &["binary_expression"],
        domain_compare_ops: &["==", "!="],
    }
}

/// A declared type's value. Primitives → the value lattice; `auto`/`void`
/// defer (None → edge carries); anything else identifier-shaped is a class
/// instance.
fn annot_type(text: &str) -> Option<InferredType> {
    use InferredType::*;
    match text.trim() {
        "int" | "long" | "short" | "unsigned" | "size_t" | "int32_t" | "int64_t" | "uint32_t" | "uint64_t"
        | "double" | "float" | "char" => Some(Numeric),
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
            // `std::optional<T>` is a T or nothing, the lattice's nullable
            // value, so an engagement test strips it as `isset` does.
            // TODO(cpp-optional-wrapper-members): a `.` access reaches the
            // optional's own members (`has_value`, `reset`), but a member
            // read of a nullable reaches its value's, as `->` does.
            if let Some(inner) = tag
                .strip_prefix("std::optional<")
                .or_else(|| tag.strip_prefix("optional<"))
                .and_then(|rest| rest.strip_suffix('>'))
            {
                return annot_type(inner).map(|t| Optional(Box::new(t)));
            }
            // A template spelling (`Box<Widget>`, `vector<int>`)
            // peels into the Instance flavor: dispatch keys the
            // BASE so members resolve through the plain-class
            // machinery; the args ride along for substitution.
            if let Some(p) = crate::model::file_analysis::ParametricType::instance_from_spelling(tag) {
                return Some(Parametric(p));
            }
            let typeish = !tag.is_empty()
                && !tag.contains(' ')
                && tag.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_');
            // Strip the namespace qualifier — classes/members are
            // keyed by the unqualified name (@context.class), so
            // `geo::Circle` must type as `Circle` to resolve.
            typeish.then(|| ClassName(tag.rsplit(CPP_SEP).next().unwrap_or(tag).to_string()))
        }
    }
}
