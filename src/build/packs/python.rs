//! Python's pack.

use crate::build::query_extract::*;
use crate::model::file_analysis::{
    ClassCall, ClassSpelling, ImportPrefix, InstanceSlots, ImportReach, ImportRule, InferredType, MemberNamespace, NameSpellings,
};
use crate::build::walk_markers::MarkedDirs;
use std::path::Path;

pub fn python_pack() -> LangPack {
    LangPack {
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
        shape_name: |raw| raw.to_string(),
        default_name: |_, _| None,
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
