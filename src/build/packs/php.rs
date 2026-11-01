//! PHP's pack.

use crate::build::query_extract::*;
use crate::model::file_analysis::{
    ClassCall, ClassSpelling, ConstructionKey, FunctionLookup, InferredType, NameSpellings,
};

pub fn php_pack() -> LangPack {
    LangPack {
        id: "php",
        query_source: include_str!("../../../queries/php/skeleton.scm"),
        // `\` qualifies, `$` leads every variable, and a written class
        // spelling names the class the file's `use` rows and namespace say
        // it does (`Collection` after `use A\B\Collection;`). `Foo()` calls
        // a function even where a class `Foo` is in scope: construction is
        // `new Foo`.
        names: NameSpellings {
            namespace_sep: Some(std::borrow::Cow::Borrowed("\\")),
            sigils: std::borrow::Cow::Borrowed(&['$']),
            class_spelling: ClassSpelling::UseMap,
            class_call: ClassCall::FunctionCall,
            construction: ConstructionKey::Class,
            functions: FunctionLookup::NamespaceThenGlobal,
            ..NameSpellings::NONE
        },
        // `bool` is not an `int`: `bool|int` is a union a signature spells.
        lattice: crate::model::file_analysis::LatticeRules::new(&[]),
        shape_name: |raw| raw.to_string(),
        // An anonymous class is one class per place it is written: PHP
        // names it `class@anonymous` and where it stands.
        default_name: |kind, at| match kind {
            DefCap::Class => Some(format!("class@anonymous:{}:{}", at.row + 1, at.column + 1)),
            _ => None,
        },
        annot_type: declared_type,
        // Where a namespace lives is composer's autoload map, not a rule
        // over the name.
        module_paths: |_| Vec::new(),
        // Composer's map lists php's names (`ComposerLocator::for_each_name`).
        module_of: |_| None,
        narrow_guard: |_, _| None,
        entrypoint_symbols: &[],
        brace_scoped_members: true,
        trigger_chars: &[],
        recv_peel: &[],
        simple_var_kinds: &[],
        member_kinds: &[],
        skip_kinds: &[],
        call_kinds: &[],
        domain_compare_kinds: &[],
        domain_compare_ops: &[],
    }
}

/// A single declared type as written (`int`, `?Foo`, `\App\User`): the
/// document captures no union or intersection, and marks `self`, `static`
/// and `parent` `@type.enclosing` / `@type.super`, so none reaches here. A
/// class spelling resolves through the use-map at its site.
fn declared_type(text: &str) -> Option<InferredType> {
    let t = text.trim().trim_start_matches('?');
    match t.to_ascii_lowercase().as_str() {
        "int" | "float" => Some(InferredType::Numeric),
        "string" => Some(InferredType::String),
        "bool" | "true" | "false" => Some(InferredType::Bool),
        "array" | "iterable" | "callable" | "object" | "mixed" | "null" | "void" | "never" => None,
        _ => Some(InferredType::ClassName(t.to_string())),
    }
}
