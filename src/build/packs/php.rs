//! PHP's pack.

use crate::build::query_extract::*;
use crate::model::file_analysis::{join_all, ClassCall, ClassSpelling, InferredType, LatticeRules, NameSpellings};

/// PHP's subsumption rules: none. `bool` is not an `int`, so `bool|int`
/// is a union a signature spells.
const PHP_LATTICE: LatticeRules = LatticeRules::new(&[]);

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
            ..NameSpellings::NONE
        },
        // A bare call names the function its namespace declares, else the
        // global one.
        functions: FunctionLookup::NamespaceThenGlobal,
        lattice: PHP_LATTICE,
        type_display: display_type,
        shape_name: |raw| raw.to_string(),
        default_name: |_| None,
        annot_type: declared_type,
        // Where a namespace lives is composer's autoload map, not a rule
        // over the name.
        module_paths: |_| Vec::new(),
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

/// PHP's word for an engine type tag: the keyword a declaration would read
/// (`declared_type` read backwards). `int` and `float` share the engine's
/// `Numeric`, so it reads as both.
fn display_type(tag: &str) -> Option<&'static str> {
    match tag {
        "Numeric" => Some("int|float"),
        "String" => Some("string"),
        "Bool" => Some("bool"),
        "HashRef" | "ArrayRef" => Some("array"),
        "CodeRef" => Some("callable"),
        _ => None,
    }
}

/// A declared type as written (`int`, `?Foo`, `\App\User`, `int|string`).
/// A union is the join of its arms and `?T` is `T|null`; a spelling with
/// an arm this can't read (an intersection, `array`) types nothing, rather
/// than claiming the arms it can. A lone `self` or `static` is marked
/// `@type.enclosing` and never reaches here; inside a union it is an arm
/// this can't read. A class spelling resolves through the use-map at its
/// site.
fn declared_type(text: &str) -> Option<InferredType> {
    let t = text.trim();
    let arms: Vec<&str> = match t.strip_prefix('?') {
        Some(inner) => vec![inner.trim(), "null"],
        None => t.split('|').map(str::trim).collect(),
    };
    if arms.len() > 1 {
        let types = arms.into_iter().map(arm_type).collect::<Option<Vec<_>>>()?;
        return join_all(&types, &PHP_LATTICE);
    }
    match arm_type(t)? {
        // `null` is the null value's type: an arm of a union, no type alone.
        InferredType::Undef => None,
        one => Some(one),
    }
}

/// One arm of a declared type.
fn arm_type(arm: &str) -> Option<InferredType> {
    match arm.to_ascii_lowercase().as_str() {
        "int" | "float" => Some(InferredType::Numeric),
        "string" => Some(InferredType::String),
        "bool" | "true" | "false" => Some(InferredType::Bool),
        "null" => Some(InferredType::Undef),
        // A callable's body is not in its type.
        "callable" => Some(InferredType::CodeRef { return_edge: None }),
        "array" | "iterable" | "object" | "mixed" | "void" | "never" | "self" | "static" => None,
        // TODO(parent-type): `parent` names the class's parent by where it
        // is written; it goes through `Lookup::Super` with every other
        // `parent` site (the `new` layer).
        "parent" => None,
        // A class spelling is a name, possibly qualified; anything else
        // (an intersection or DNF group) names no single class.
        _ if !arm.is_empty() && arm.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '\\') => {
            Some(InferredType::ClassName(arm.to_string()))
        }
        _ => None,
    }
}
