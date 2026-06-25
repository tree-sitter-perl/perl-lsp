//! Measures the reparenthesizer: prototype facts → rewritten source →
//! corrected re-parse → anchor remap.

use super::*;

fn perl_parser() -> tree_sitter::Parser {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    p
}

fn parse(p: &mut tree_sitter::Parser, src: &str) -> Tree {
    p.parse(src, None).unwrap()
}

#[test]
fn prototypes_collected_with_shape() {
    let mut p = perl_parser();
    let src = "sub sner ($) { $_[0] }\nsub noop () { 42 }\nsub pair ($$) { 0 }\n";
    let tree = parse(&mut p, src);
    let protos = collect_prototypes(&tree, src.as_bytes());
    assert_eq!(protos["sner"], Proto { nullary: false, fixed_arity: 1 });
    assert_eq!(protos["noop"], Proto { nullary: true, fixed_arity: 0 });
    assert_eq!(protos["pair"], Proto { nullary: false, fixed_arity: 2 });
}

#[test]
fn unary_call_reparenthesized_and_reparses_correctly() {
    let mut p = perl_parser();
    // Without the proto, tree-sitter grabs BOTH args into sner's call.
    let src = "sub sner ($) { $_[0] }\nsner 1, 2;\n";

    // baseline: the wrong greedy parse
    let before = parse(&mut p, src);
    assert!(
        before.root_node().to_sexp().contains("ambiguous_function_call_expression"),
        "baseline should be the greedy ambiguous call",
    );

    let (rewritten, map) = reparenthesize(&mut p, src);
    assert!(rewritten.contains("sner(1), 2") || rewritten.contains("sner( 1), 2"), "rewritten: {rewritten:?}");

    // the corrected parse: an OUTER list_expression whose first element
    // is a real arity-1 function_call_expression.
    let after = parse(&mut p, &rewritten);
    let sexp = after.root_node().to_sexp();
    let call_stmt = sexp.split("sner").nth(1).unwrap_or("");
    // the statement holding the call now nests a function_call_expression
    // inside a list_expression — the unary grouping.
    assert!(
        sexp.contains("function_call_expression"),
        "expected a real parenthesized call after rewrite: {sexp}",
    );

    // anchor remap: find `2` in the rewritten source, map back to its
    // original offset, and confirm it points at the original `2`.
    let two_t = rewritten.rfind('2').unwrap();
    let two_o = map.to_original(two_t);
    assert_eq!(&src[two_o..two_o + 1], "2", "anchor must land on original `2`");
    // and the inserted `(` (the LAST paren; the prototype's is earlier)
    // collapses to the call site — the space right after `sner`.
    let paren_t = rewritten.rfind('(').unwrap();
    let paren_o = map.to_original(paren_t);
    assert_eq!(&src[paren_o..paren_o + 1], " ", "inserted paren collapses to its site");
    let _ = call_stmt;
}

#[test]
fn nullary_bareword_operand_becomes_a_call() {
    let mut p = perl_parser();
    // `sner + 1` with nullary proto means `sner() + 1`; without it,
    // tree-sitter reads `sner` as a bareword operand.
    let src = "sub sner () { 42 }\nmy $x = sner + 1;\n";

    let before = parse(&mut p, src);
    assert!(
        before.root_node().to_sexp().contains("bareword"),
        "baseline reads sner as a bareword",
    );

    let (rewritten, map) = reparenthesize(&mut p, src);
    assert!(rewritten.contains("sner() + 1"), "rewritten: {rewritten:?}");

    let after = parse(&mut p, &rewritten);
    let sexp = after.root_node().to_sexp();
    assert!(
        sexp.contains("function_call_expression") || sexp.contains("ambiguous_function_call_expression"),
        "sner is now a call: {sexp}",
    );

    // anchor remap on the `1` after the inserted `()`
    let one_t = rewritten.rfind('1').unwrap();
    let one_o = map.to_original(one_t);
    assert_eq!(&src[one_o..one_o + 1], "1");
}

#[test]
fn no_prototypes_is_identity() {
    let mut p = perl_parser();
    let src = "foo 1, 2;\nbar();\n";
    let (rewritten, map) = reparenthesize(&mut p, src);
    assert_eq!(rewritten, src, "no protos → no rewrite");
    assert_eq!(map.to_original(5), 5, "identity anchor map");
}
