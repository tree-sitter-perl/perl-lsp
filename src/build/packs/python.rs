//! Python's pack.

use crate::build::query_extract::*;
use crate::model::file_analysis::{join_all, InferredType, MemberNamespace, NameSpellings, Unordered};

pub fn python_pack() -> LangPack {
    LangPack {
        query_source: include_str!("../../../queries/python/skeleton.scm"),
        // One member namespace: the nearest declaration of a name wins,
        // whether it is a data member or a member function.
        names: NameSpellings { member_namespace: MemberNamespace::Shared, ..NameSpellings::NONE },
        // `bool` is a subclass of `int`.
        lattice: crate::model::file_analysis::LatticeRules::new(&[crate::model::file_analysis::Subsumption::BoolIntoNumeric]),
        shape_name: |raw| raw.to_string(),
        default_name: |_| None,
        annot_type: annotation_type,
        module_paths: |m| {
            let base = m.replace('.', "/");
            vec![format!("{base}.py"), format!("{base}/__init__.py")]
        },
        // `isinstance(x, Foo)` narrows x to Foo inside the guard.
        narrow_guard: |guard, ty| (guard == Some("isinstance")).then(|| InferredType::ClassName(ty.to_string())),
        entrypoint_symbols: &[],
        brace_scoped_members: false,
        trigger_chars: &["."],
        recv_peel: &["parenthesized_expression"],
        simple_var_kinds: &["identifier"],
        member_kinds: &["attribute"],
        skip_kinds: &["string", "string_content", "comment", "concatenated_string"],
        call_kinds: &["call"],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
    }
}

/// An annotation's type. `A | B` and `Union[A, B]` are the join of their
/// arms, `Optional[A]` is `A | None`; a spelling with an arm this can't
/// read types nothing, rather than claiming the arms it can.
fn annotation_type(text: &str) -> Option<InferredType> {
    let t = text.trim();
    let arms: Vec<&str> = if let Some(inner) = subscript(t, "Optional") {
        vec![inner, "None"]
    } else if let Some(inner) = subscript(t, "Union") {
        split_top_level(inner, ',')
    } else {
        split_top_level(t, '|')
    };
    if arms.len() > 1 {
        let types = arms.into_iter().map(annotation_type).collect::<Option<Vec<_>>>()?;
        return join_all(&types, &Unordered);
    }
    match t {
        "str" => Some(InferredType::String),
        "int" | "float" => Some(InferredType::Numeric),
        "None" => Some(InferredType::Undef),
        "list" => Some(InferredType::ArrayRef),
        "dict" => Some(InferredType::HashRef),
        t if t.chars().next().is_some_and(|c| c.is_uppercase()) => {
            Some(InferredType::ClassName(t.to_string()))
        }
        _ => None,
    }
}

/// `inner` of `name[inner]`, also spelled `typing.name[inner]`.
fn subscript<'a>(t: &'a str, name: &str) -> Option<&'a str> {
    let rest = t.strip_prefix("typing.").unwrap_or(t).strip_prefix(name)?;
    rest.trim_start().strip_prefix('[')?.strip_suffix(']')
}

/// `t` split on `sep` outside any brackets.
fn split_top_level(t: &str, sep: char) -> Vec<&str> {
    let (mut depth, mut start, mut out) = (0i32, 0, Vec::new());
    for (i, c) in t.char_indices() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth -= 1,
            c if c == sep && depth == 0 => {
                out.push(t[start..i].trim());
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(t[start..].trim());
    out
}
