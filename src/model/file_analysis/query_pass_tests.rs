//! A pass over one analysis answers each link of a flow chain once
//! (`FileAnalysis::query_pass`): every call on a long `$x = $x->add()` chain
//! gets its receiver, and the pass's work grows with the chain, not with its
//! square.

use super::*;
use crate::index::module_index::ModuleIndex;

/// Under `PERL_LSP_FOLD_MEMO_EQUIV` the build runs with its memos off: the
/// net compares a build against its memo-off twin, and past the depth cap
/// that twin is the truncated stamp under test, so the two cannot agree on
/// this input. The re-stamp is what the tests assert, with the memos on.
fn perl_chain(links: usize) -> FileAnalysis {
    let mut src = String::from(
        "package A; sub new { bless {}, shift } sub add { my $s = shift; return $s }\n\
         package main; sub f { my $x = A->new;\n",
    );
    for _ in 0..links {
        src.push_str("$x = $x->add;\n");
    }
    src.push_str("}\n1;\n");
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let tree = parser.parse(&src, None).unwrap();
    let build = || crate::build::builder::build(&tree, src.as_bytes());
    if crate::model::witnesses::fold_memo_equiv_enabled() {
        crate::model::witnesses::without_fold_memo(build)
    } else {
        build()
    }
}

/// The class each `add` call dispatches on, as a reader under `idx` sees it.
fn add_receivers(fa: &FileAnalysis, idx: &ModuleIndex) -> Vec<Vec<String>> {
    fa.refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::MethodCall { .. }) && r.target_name == "add")
        .map(|r| fa.receiver_classes(r, Some(idx), ReceiverPolicy::Navigate).into_iter().collect())
        .collect()
}

/// The enrichment re-stamp runs under an index, which the build's fold memo
/// never admits, so before the pass memo each call re-walked the chain from
/// scratch and every call past the registry's depth cap lost its receiver.
#[test]
fn every_call_on_a_long_chain_keeps_its_receiver_under_an_index() {
    let mut fa = perl_chain(300);
    let idx = ModuleIndex::new_for_test();
    fa.stamp_method_call_targets(Some(&idx));
    let receivers = add_receivers(&fa, &idx);
    assert_eq!(receivers.len(), 300);
    let lost: Vec<usize> =
        receivers.iter().enumerate().filter(|(_, c)| c.as_slice() != ["A"]).map(|(i, _)| i).collect();
    assert!(lost.is_empty(), "{} of 300 calls lost their receiver; first {:?}", lost.len(), lost.first());
}

/// Deterministic cost pin: the attachments the re-stamp reduces. A chain
/// twice as long costs about twice as much; re-walking the prefix per call
/// would be four times.
#[test]
fn a_restamp_reduces_each_link_once() {
    let idx = ModuleIndex::new_for_test();
    let cost = |links| {
        let mut fa = perl_chain(links);
        let before = crate::model::witnesses::query_rec_bodies();
        fa.stamp_method_call_targets(Some(&idx));
        crate::model::witnesses::query_rec_bodies() - before
    };
    let (short, long) = (cost(60), cost(120));
    assert!(long < short * 3, "60 links reduced {short} attachments, 120 reduced {long}: superlinear");
}

/// `x = x.add()` with the write landing at its target: each read of `x`
/// reaches its own statement's write, so every chase cuts a cycle at the
/// read it started from. That cut is part of the read's own answer, and
/// sharing the answer keeps the stamp linear in the chain rather than cubic.
#[cfg(feature = "cpp")]
#[test]
fn a_cpp_reassignment_chain_types_every_call_at_linear_cost() {
    // Built and stamped, with the attachments that cost.
    let build = |src: &str| {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
        let tree = parser.parse(src, None).unwrap();
        let before = crate::model::witnesses::query_rec_bodies();
        let skel = crate::build::query_extract::extract(&tree, src.as_bytes(), &crate::build::packs::cpp_pack());
        (skel.unwrap().into_file_analysis(), crate::model::witnesses::query_rec_bodies() - before)
    };
    let receivers = |fa: &FileAnalysis| -> Vec<Vec<String>> {
        fa.refs()
            .iter()
            .filter(|r| matches!(r.kind, RefKind::MethodCall { .. }) && r.target_name == "add")
            .map(|r| fa.receiver_classes(r, None, ReceiverPolicy::Navigate).into_iter().collect())
            .collect()
    };
    let chain = |links| {
        let mut src = String::from("struct A { A add(); };\nvoid f() {\n  A x;\n");
        for _ in 0..links {
            src.push_str("  x = x.add();\n");
        }
        src + "}\n"
    };
    let src = chain(60);
    let (fa, short) = build(&src);
    // The memos change cost, never an answer.
    let (control, _) = crate::model::witnesses::without_fold_memo(|| build(&src));
    assert_eq!(receivers(&fa), receivers(&control));
    let got = receivers(&fa);
    assert_eq!(got.len(), 60);
    assert!(got.iter().all(|c| c.as_slice() == ["A"]), "{got:?}");
    let (_, long) = build(&chain(120));
    assert!(long < short * 3, "60 links reduced {short} attachments, 120 reduced {long}: superlinear");
}

/// `x = x.add()` writes x at the assignment's end, after its right side has
/// read the x before it, so each call's receiver is the previous link's
/// value, one hop back. A chain far past the registry's depth cap still
/// types every call, and the build reduces each link a bounded number of
/// times.
#[cfg(feature = "cpp")]
#[test]
fn a_cpp_chain_past_the_depth_cap_types_every_call_at_linear_cost() {
    let chain = |links| {
        let mut src = String::from("struct A { A add(); };\nvoid f() {\n  A x;\n");
        for _ in 0..links {
            src.push_str("  x = x.add();\n");
        }
        src + "}\n"
    };
    // Built, with the attachments the build reduced.
    let build = |links| {
        let src = chain(links);
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
        let tree = parser.parse(&src, None).unwrap();
        let before = crate::model::witnesses::query_rec_bodies();
        let skel = crate::build::query_extract::extract(&tree, src.as_bytes(), &crate::build::packs::cpp_pack());
        (skel.unwrap().into_file_analysis(), crate::model::witnesses::query_rec_bodies() - before)
    };
    let links = 600;
    let (fa, _) = build(links);
    let receivers: Vec<Vec<String>> = fa
        .refs()
        .iter()
        .filter(|r| matches!(r.kind, RefKind::MethodCall { .. }) && r.target_name == "add")
        .map(|r| fa.receiver_classes(r, None, ReceiverPolicy::Navigate).into_iter().collect())
        .collect();
    assert_eq!(receivers.len(), links);
    let lost: Vec<usize> =
        receivers.iter().enumerate().filter(|(_, c)| c.as_slice() != ["A"]).map(|(i, _)| i).collect();
    assert!(lost.is_empty(), "{} of {links} calls lost their receiver; first {:?}", lost.len(), lost.first());
    let (short, long) = (build(150).1, build(300).1);
    assert!(long < short * 3, "150 links reduced {short} attachments, 300 reduced {long}: superlinear");
}
