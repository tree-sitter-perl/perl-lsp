//! PHP's pack.

use crate::build::query_extract::*;
use crate::model::file_analysis::{ClassCall, ClassSpelling, NameSpellings};

pub fn php_pack() -> LangPack {
    LangPack {
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
        // `bool` is not an `int`: `bool|int` is a union a signature spells.
        lattice: crate::model::file_analysis::LatticeRules::new(&[]),
        shape_name: |raw| raw.to_string(),
        default_name: |_| None,
        annot_type: |_| None,
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
