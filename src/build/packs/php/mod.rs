//! php's pack: the declaration, its phpdoc type reader, and composer's
//! install-map discovery (php's dependency-root source).

// No caller outside the pack tests when php is compiled out.
#[cfg_attr(not(feature = "php"), allow(dead_code))]
pub mod composer;
mod doc;

use doc::{php_annot_type, php_doc_types};

use crate::build::query_extract::{EnumMember, LangPack};
use crate::model::file_analysis::{InferredType, NameSpellings, PackSpellings};

/// php's write and display spellings. `type_display` is what a human
/// surface renders (a php array is one type whichever rep the engine
/// inferred); `native_type_spellings` is what a quick-fix WRITES, which is
/// a smaller map — an engine tag with no unambiguous php spelling is absent
/// rather than guessed.
#[cfg_attr(not(feature = "php"), allow(dead_code))]
const SPELLINGS: PackSpellings = PackSpellings {
    type_display: &[
        ("String", "string"),
        ("Numeric", "int|float"),
        ("Bool", "bool"),
        ("HashRef", "array"),
        ("ArrayRef", "array"),
        ("Undef", "null"),
        ("CodeRef", "callable"),
        ("Sequence", "list"),
    ],
    native_type_spellings: &[
        ("String", "string"),
        ("Bool", "bool"),
        ("HashRef", "array"),
        ("ArrayRef", "array"),
        ("Sequence", "array"),
        ("CodeRef", "callable"),
    ],
    class_literal_member: "class",
    import_template: "use {};\n",
    contract_stub: "public function {}\n{\n    // TODO: implement\n}",
    return_annotation_template: ": {}",
    static_property_sigil: "$",
    variadic_marker: "...",
    default_sep: " = ",
    members_are_package_bound: true,
    // `implements` is checked where the class is declared: `__call` catches
    // calls a compile error would never let happen
    catch_all_satisfies_contracts: false,
};

// Registered by `php_driver` only under `feature = "php"` (and driven by the
// pack tests); dead weight in a single-language build like `cpp`-only.
#[cfg_attr(not(feature = "php"), allow(dead_code))]
pub fn php_pack() -> LangPack {
    LangPack {
        // Base skeleton + the bundled framework overlays (pure query
        // vocabulary — see each overlay's header for the doctrine note).
        query_source: include_str!("../../../../queries/php/skeleton.scm"),
        bundled_overlays: &[
            ("stdlib.scm", include_str!("../../../../queries/php/stdlib.scm")),
        ],
        spellings: &SPELLINGS,
        lang_id: "php",
        bundled_entry_markers: &[
            // The language's OWN runtime surface: magic methods and the SPL
            // interface contracts the engine calls structurally, so zero
            // in-repo call sites is the expected state.
            include_str!("../../../../queries/php/php.entry.json"),
        ],
        bundled_rail_docs: &[
        ],
        // `\\` qualifies, `$` leads every variable, and a written class
        // spelling resolves through the file's `use` rows and namespace
        // (`Collection` after `use A\\B\\Collection;`). The class-to-member
        // qualifier is php's own `::`, not the namespace separator.
        names: NameSpellings {
            namespace_sep: Some(std::borrow::Cow::Borrowed("\\")),
            sigils: std::borrow::Cow::Borrowed(&['$']),
            class_spelling: crate::model::file_analysis::ClassSpelling::UseMap,
            member_sep: Some(std::borrow::Cow::Borrowed("::")),
        },
        // variable_name captures carry the `$` (PHP spells it at every
        // use, like Perl); names/classes pass through verbatim. Nothing is
        // canonicalized here: which receiver spellings name the enclosing
        // class is the skeleton's `@receiver.self` / `@receiver.this`, and
        // the extractor mints the class itself from the class-body scope.
        shape_name: |_, raw| raw.to_string(),
        default_name: |kind, row, col| match kind {
            "anon" => Some("(anon)".to_string()),
            // `new class(...) {...}` — PHP's own runtime spelling is
            // `class@anonymous<file>:<line>`; ours stays identifier-shaped
            // and file-local by construction (nothing else spells it).
            "class" => Some(format!("class_anonymous_{}_{}", row + 1, col + 1)),
            _ => None,
        },
        // Declared types (params, properties, returns) ARE the witness
        // source — PHP's gradual typing seeds the bag, inference covers
        // the untyped legacy tier. `?T` peels to T (nullability is not a
        // navigation fact); unions/intersections defer (None → the flow
        // edge carries). The `self`/`static` receiver spellings are a
        // RETURN shape, answered by `declared_return`.
        annot_type: php_annot_type,
        // `: static` / `: $this` are late-bound to the call's receiver —
        // fluent builders chain through `ReturnExpr::Receiver`. `self`
        // strictly means the defining class; substituting the receiver
        // over-approximates only for inherited methods (accepted). Every
        // other spelling is whatever the declared-type reader makes of it.
        declared_return: |text| {
            use crate::model::witnesses::ReturnExpr;
            match text.trim().trim_start_matches('?') {
                "static" | "$this" | "self" => Some(ReturnExpr::Receiver),
                t => php_annot_type(t).map(ReturnExpr::Concrete),
            }
        },
        // phpdoc: the type vocabulary of REAL PHP — most of WordPress and
        // half of Laravel's public API type only here.
        doc_types: php_doc_types,
        doc_uses_method_tags: &["dataProvider"],
        // PSR-4's real map lives in composer.json (autoload roots); the
        // one executable line is the namespace-mirrors-directories shape.
        module_paths: |m| {
            let base = m.trim_start_matches('\\').replace('\\', "/");
            vec![format!("{base}.php")]
        },
        import_module: |_, _| None,
        // `$x instanceof User` refines $x to User: the class token leafs like
        // every other class spelling (`Op\Install` → `Install`; classes are
        // filed by leaf).
        narrow_type: |ty| {
            php_annot_type(ty).filter(|t| matches!(t, InferredType::ClassName(_)))
        },
        // class/trait/interface bodies are brace-delimited, so a member
        // orphaned by a misparse can re-anchor positionally.
        brace_scoped_members: true,
        bundled_builtin_types: &[include_str!("../../../../queries/php/builtins.txt")],
        enum_members: &[
            EnumMember { name: "value", callable: false },
            EnumMember { name: "name", callable: false },
            EnumMember { name: "cases", callable: true },
            EnumMember { name: "from", callable: true },
            EnumMember { name: "tryFrom", callable: true },
        ],
        trigger_chars: &["$", ">", ":"],
    }
}

