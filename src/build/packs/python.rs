//! Python's pack.

use crate::build::query_extract::*;
use crate::model::file_analysis::{
    join_all, ClassCall, ClassSpelling, ImportPrefix, InstanceSlots, ImportReach, ImportRule, InferredType, MemberNamespace,
    NameSpellings,
};
use crate::build::walk_markers::MarkedDirs;
use std::path::Path;

/// Python's subsumption rules: `bool <: int`.
const PYTHON_LATTICE: crate::model::file_analysis::LatticeRules =
    crate::model::file_analysis::LatticeRules::new(&[crate::model::file_analysis::Subsumption::BoolIntoNumeric]);

pub fn python_pack() -> LangPack {
    LangPack {
        id: "python",
        query_source: include_str!("../../../queries/python/skeleton.scm"),
        // `.` qualifies, and a class spelling names what the file's imports
        // and its own module say it does (`User` after `from pkg.models
        // import User`). An import is a statement that runs, so it binds
        // from where it is written; a leading `.` climbs from the file's
        // package. Calling a class constructs it. One member namespace: the
        // nearest declaration of a name wins, whether it is a data member
        // or a member function. An instance holds its own attributes (the
        // slots `self.x = …` writes), read before its class's members.
        names: NameSpellings {
            namespace_sep: Some(std::borrow::Cow::Borrowed(".")),
            class_spelling: ClassSpelling::UseMap,
            member_namespace: MemberNamespace::Shared,
            imports: ImportRule { reach: ImportReach::FromImport, prefix: ImportPrefix::PackageRelative },
            class_call: ClassCall::Constructs,
            instance_slots: InstanceSlots::BeforeClass,
            ..NameSpellings::NONE
        },
        // `bool` is a subclass of `int`.
        lattice: PYTHON_LATTICE,
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
        let types = arms
            .into_iter()
            .map(|arm| if arm == "None" { Some(InferredType::Undef) } else { annotation_type(arm) })
            .collect::<Option<Vec<_>>>()?;
        // An annotation names no classes' relations, only the language's.
        return join_all(&types, &PYTHON_LATTICE);
    }
    match t {
        "str" => Some(InferredType::String),
        "int" | "float" => Some(InferredType::Numeric),
        // `None` is the null value's type: an arm of a union, no type alone.
        "None" => None,
        "list" => Some(InferredType::ArrayRef),
        "dict" => Some(InferredType::HashRef),
        // A subscript or string annotation is no class spelling.
        t if t.chars().next().is_some_and(|c| c.is_uppercase())
            && t.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') =>
        {
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

/// The module `path` is: its stem under the chain of package directories
/// above it (walk up while the directory is marked — holds `__init__.py`;
/// the first unmarked one is the import root), `__init__` naming its
/// package. A file whose own directory is no package (a namespace package,
/// a script dir) is named from the root instead, when it lies under one.
pub fn python_file_module(path: &Path, dirs: &MarkedDirs<'_>) -> Option<FileModule> {
    let stem = path.file_stem()?.to_str()?;
    let is_package = stem == "__init__";
    let mut segments: Vec<String> = Vec::new();
    if !is_package {
        segments.push(stem.to_string());
    }
    let mut dir = path.parent();
    let in_package = dir.is_some_and(|d| dirs.is_marked(d));
    if in_package {
        while let Some(d) = dir.filter(|d| dirs.is_marked(d)) {
            segments.push(d.file_name()?.to_str()?.to_string());
            dir = d.parent();
        }
    } else if let Some(rel) = path.parent().and_then(|d| dirs.root_relative(d)) {
        for c in rel.components().rev() {
            segments.push(c.as_os_str().to_str()?.to_string());
        }
    }
    segments.reverse();
    (!segments.is_empty()).then_some(FileModule { segments, is_package })
}
