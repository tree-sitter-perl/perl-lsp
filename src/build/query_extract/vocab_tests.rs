use super::*;
use crate::build::packs::{cmake_pack, cpp_pack, perl_pack, php_pack, python_pack, r_pack};
use crate::build::query_extract::{extract, LangPack};

fn bundled() -> Vec<(&'static str, LangPack, tree_sitter::Language)> {
    vec![
        ("perl", perl_pack(), ts_parser_perl::LANGUAGE.into()),
        ("cpp", cpp_pack(), tree_sitter_cpp::LANGUAGE.into()),
        ("python", python_pack(), tree_sitter_python::LANGUAGE.into()),
        ("r", r_pack(), tree_sitter_r::LANGUAGE.into()),
        ("cmake", cmake_pack(), tree_sitter_cmake::LANGUAGE.into()),
        ("php", php_pack(), tree_sitter_php::LANGUAGE_PHP.into()),
    ]
}

/// Every capture a bundled document uses is in the vocabulary, and the
/// enum spells it back exactly: `parse` and `Display` are one table.
#[test]
fn bundled_documents_speak_the_vocabulary() {
    for (lang, pack, language) in bundled() {
        let query = tree_sitter::Query::new(&language, pack.query_source).unwrap();
        for name in query.capture_names() {
            if name.starts_with('_') {
                continue;
            }
            let cap = Capture::parse(name).unwrap_or_else(|e| panic!("{lang}: {e}"));
            assert_eq!(cap.to_string(), *name, "{lang}: @{name} round-trips");
        }
    }
}

/// A capture outside the vocabulary fails the compile, so a document typo
/// is an error instead of a silently inert pattern.
#[test]
fn unknown_capture_fails_the_load() {
    let pack = LangPack {
        query_source: "(subroutine_declaration_statement name: (_) @def.subroutine.name) @def.subroutine",
        ..perl_pack()
    };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let tree = parser.parse("sub f {}", None).unwrap();
    let err = extract(&tree, b"sub f {}", &pack).unwrap_err();
    assert!(err.contains("unknown capture @def.subroutine"), "{err}");
}

/// `_`-prefixed captures are predicate-only: they map to nothing and the
/// rest of the pattern extracts as usual.
#[test]
fn predicate_only_captures_are_skipped() {
    let table = capture_table(&["def.sub", "_const", "def.sub.name"]).unwrap();
    assert_eq!(
        table,
        vec![
            Some(Capture::Def { kind: DefCap::Sub, part: DefPart::Node }),
            None,
            Some(Capture::Def { kind: DefCap::Sub, part: DefPart::Name }),
        ]
    );
}

/// A mark spelled like an attribute means that attribute's flag, so the
/// two ways a document states one fact cannot drift apart.
#[test]
fn a_mark_spelled_as_an_attribute_mints_that_attribute() {
    use crate::model::file_analysis::SymbolFlags;
    for &mark in MarkCap::ALL {
        if let Ok(flag) = SymbolFlags::try_from(mark.spelling()) {
            assert_eq!(mark.flag(), flag, "@mark.{mark}");
        }
    }
}

#[test]
fn malformed_names_do_not_parse() {
    for name in ["def", "def.sub.name.x", "scope.", "import.", "ref", "expr.lit.float", "flow"] {
        assert!(Capture::parse(name).is_err(), "@{name} must not parse");
    }
}

/// A link names a node one level from its pattern's `@link.from.<walk>`; a
/// pattern that links without one fails the compile instead of recording
/// nothing.
#[test]
fn a_link_without_its_from_fails_the_load() {
    let err = load_err("(subroutine_declaration_statement)\n(subroutine_declaration_statement name: (_) @link.name)");
    assert!(err.contains("line 2: a pattern with @link.<rel> needs exactly one @link.from.<walk>"), "{err}");
}

/// A walk starts at its root capture, so links for a walk whose root the
/// document never captures have no reader and fail the compile.
#[test]
fn a_walk_without_its_root_fails_the_load() {
    let err = load_err("(subroutine_declaration_statement name: (_) @link.name) @link.from.ool");
    assert!(err.contains("@link.from.ool links for a walk whose root @ool.def"), "{err}");
}

/// The bare `@link.from` no longer parses: every link names its walk.
#[test]
fn a_from_names_its_walk() {
    assert!(Capture::parse("link.from").is_err());
    assert_eq!(Capture::parse("link.from.ool"), Ok(Capture::LinkFrom(LinkWalk::Ool)));
}

fn load_err(query_source: &'static str) -> String {
    let pack = LangPack { query_source, ..perl_pack() };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let tree = parser.parse("sub f {}", None).unwrap();
    extract(&tree, b"sub f {}", &pack).unwrap_err()
}

/// A node written with a fourth capture fails the load: tree-sitter would
/// drop it without a word.
#[test]
fn a_fourth_capture_on_one_node_fails_the_load() {
    let err = load_err("(subroutine_declaration_statement\n  name: (_) @def.sub.name @_a @_b @_c) @def.sub");
    assert!(err.contains("line 2: a node holds at most three captures"), "{err}");
    assert!(crate::build::query_predicates::check_source("(x) @a @b @c ; @d @e\n(y \"@q @r\" @s) @t").is_ok());
}

/// Every arm capture parses back to itself.
#[test]
fn every_cond_capture_round_trips() {
    for &cap in CondCap::ALL {
        let name = format!("cond.{}", cap.spelling());
        assert_eq!(Capture::parse(&name).ok(), Some(Capture::Cond(cap)), "@{name}");
    }
}
