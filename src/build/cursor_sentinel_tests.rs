//! Measures the sentinel re-parse seam: splice `__CURSOR__` at the
//! cursor, re-parse, recover the receiver of `box.` / `box->` / `obj.`
//! — the incomplete-input cases that have no member node in the raw
//! parse. Also pins the free-anchor property (receiver byte offsets
//! survive the splice unchanged) and the string/comment skip guard.

use super::*;

fn cpp() -> Parser {
    let mut p = Parser::new();
    p.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
    p
}

fn python() -> Parser {
    let mut p = Parser::new();
    p.set_language(&tree_sitter_python::LANGUAGE.into()).unwrap();
    p
}

/// Byte offset just past the first occurrence of `marker` in `src`.
fn after(src: &str, marker: &str) -> usize {
    src.find(marker).unwrap() + marker.len()
}

// ---- PROBE: does the sentinel parse as a clean member access? ----
//
// Run with `cargo test cursor_sentinel -- --nocapture probe` to see the
// patched CST. This is the evidence the approach rests on.

#[test]
fn probe_patched_cst_shapes() {
    let cases: &[(&str, fn() -> Parser, &str)] = &[
        ("cpp dot", cpp, "box."),
        ("cpp arrow", cpp, "box->"),
        ("python dot", python, "obj."),
    ];
    for (label, mk, frag) in cases {
        let mut p = mk();
        let src = format!("{frag}");
        let cursor = src.len();
        let raw = p.parse(&src, None).unwrap();
        let patched = patch(&src, cursor);
        let fixed = p.parse(&patched, None).unwrap();
        eprintln!("=== {label}: {frag:?} ===");
        eprintln!("  raw     : {}", raw.root_node().to_sexp());
        eprintln!("  patched : {patched:?}");
        eprintln!("  fixed   : {}", fixed.root_node().to_sexp());
    }
}

// ---- The deliverable: receiver recovery on incomplete input ----

#[test]
fn cpp_dot_receiver() {
    let src = "void f() { box. }";
    let cursor = after(src, "box."); // just past the dot
    let r = receiver_at(&mut cpp(), &crate::build::packs::cpp_pack(), src, cursor).unwrap();
    assert_eq!(r.text, "box");
    assert!(!r.arrow);
}

#[test]
fn cpp_arrow_receiver() {
    let src = "void f() { box-> }";
    let cursor = after(src, "box->");
    let r = receiver_at(&mut cpp(), &crate::build::packs::cpp_pack(), src, cursor).unwrap();
    assert_eq!(r.text, "box");
    assert!(r.arrow);
}

#[test]
fn python_dot_receiver() {
    let src = "obj.";
    let cursor = src.len();
    let r = receiver_at(&mut python(), &crate::build::packs::python_pack(), src, cursor).unwrap();
    assert_eq!(r.text, "obj");
    assert!(!r.arrow);
}

#[test]
fn cpp_chained_receiver_is_the_full_prefix() {
    // The receiver of `a.b.` is the whole `a.b`, not just `b` — the
    // sentinel completes the OUTER access, and step (c) types `a.b`.
    let src = "void f() { a.b. }";
    let cursor = after(src, "a.b.");
    let r = receiver_at(&mut cpp(), &crate::build::packs::cpp_pack(), src, cursor).unwrap();
    assert_eq!(r.text, "a.b");
    assert!(!r.arrow);
}

#[test]
fn cpp_call_receiver() {
    // Receiver is a call expression — exactly the case the engine's
    // tree-free `expr_type_at_span` then types via the call's return.
    let src = "void f() { make().  }";
    let cursor = after(src, "make().");
    let r = receiver_at(&mut cpp(), &crate::build::packs::cpp_pack(), src, cursor).unwrap();
    assert_eq!(r.text, "make()");
}

/// PHP's member operators are the document's: `->`, the nullsafe `?->`
/// and the scope operator `::` each open a member access, whose receiver is
/// the one the document's match names.
#[test]
fn php_receivers_follow_the_document() {
    let mut p = Parser::new();
    p.set_language(&tree_sitter_php::LANGUAGE_PHP.into()).unwrap();
    let pack = crate::build::packs::php_pack();
    for (src, marker, recv, arrow) in [
        ("<?php $box-> }", "$box->", "$box", true),
        ("<?php $box?-> }", "$box?->", "$box", true),
        ("<?php Foo\\Bar:: }", "Foo\\Bar::", "Foo\\Bar", false),
        ("<?php parent:: }", "parent::", "parent", false),
        ("<?php $a->b()->c-> }", "$a->b()->c->", "$a->b()->c", true),
    ] {
        let r = receiver_at(&mut p, &pack, src, after(src, marker)).unwrap_or_else(|| panic!("a receiver in {src}"));
        assert_eq!((r.text.as_str(), r.arrow), (recv, arrow), "{src}");
    }
}

// ---- The free-anchor property: receiver offsets survive the splice ----

#[test]
fn receiver_span_is_original_coordinates() {
    let src = "void f() { widget. }";
    let cursor = after(src, "widget.");
    let r = receiver_at(&mut cpp(), &crate::build::packs::cpp_pack(), src, cursor).unwrap();
    // The recovered span indexes back into the ORIGINAL source cleanly.
    assert_eq!(&src[r.start..r.end], "widget");
    assert!(r.end <= cursor, "receiver must end at/before the splice");
}

// ---- Robustness: deeper incomplete context still recovers ----

#[test]
fn cpp_receiver_inside_unbalanced_block() {
    // No closing brace — the kind of mid-edit buffer completion fires in.
    let src = "void f() {\n    auto x = box.";
    let cursor = src.len();
    let r = receiver_at(&mut cpp(), &crate::build::packs::cpp_pack(), src, cursor).unwrap();
    assert_eq!(r.text, "box");
}

// ---- The skip guard: never splice into a string / comment ----

#[test]
fn no_receiver_inside_string() {
    // A dot inside a string literal is not a member access.
    let src = r#"const char* s = "box. ";"#;
    let cursor = after(src, "box.");
    assert!(receiver_at(&mut cpp(), &crate::build::packs::cpp_pack(), src, cursor).is_none());
}

#[test]
fn no_receiver_inside_comment() {
    let src = "// box.\nint n;";
    let cursor = after(src, "box.");
    assert!(receiver_at(&mut cpp(), &crate::build::packs::cpp_pack(), src, cursor).is_none());
}

// ---- Incremental re-parse: same answer, a fraction of the work ----

#[test]
fn incremental_matches_full_and_is_cheaper() {
    // A realistically large buffer: completion fires deep inside it.
    let mut body = String::from("struct Big {\n");
    for i in 0..4000 {
        body.push_str(&format!("  int field_{i};\n"));
    }
    body.push_str("};\nvoid f() { box.");
    let cursor = body.len();
    let mut p = cpp();

    // Full path (cold): parse original + parse patched from scratch.
    let full = receiver_at(&mut p, &crate::build::packs::cpp_pack(), &body, cursor).unwrap();
    assert_eq!(full.text, "box");

    // Incremental path: document already holds the unpatched tree.
    let old = p.parse(&body, None).unwrap();
    let inc = receiver_at_incremental(&mut p, &crate::build::packs::cpp_pack(), &body, &old, cursor).unwrap();
    assert_eq!(inc, full); // identical receiver

    // Measure: incremental re-parse vs a cold full re-parse of the patch.
    let patched = patch(&body, cursor);
    let iters = 200;
    let t0 = std::time::Instant::now();
    for _ in 0..iters {
        let _ = receiver_at_incremental(&mut p, &crate::build::packs::cpp_pack(), &body, &old, cursor).unwrap();
    }
    let inc_us = t0.elapsed().as_micros() as f64 / iters as f64;
    let t1 = std::time::Instant::now();
    for _ in 0..iters {
        let _ = p.parse(&patched, None).unwrap();
    }
    let cold_us = t1.elapsed().as_micros() as f64 / iters as f64;
    eprintln!(
        "incremental re-parse {:.1}us  vs  cold full re-parse {:.1}us  ({:.1}x)",
        inc_us,
        cold_us,
        cold_us / inc_us
    );
    // Incremental (which includes the skip-guard + the InputEdit + the
    // localized re-parse) beats a single cold full parse of the patched
    // buffer. The free-anchor property means no remap cost on top.
    assert!(inc_us < cold_us, "incremental should beat a cold full parse");
}

#[test]
fn no_receiver_on_plain_identifier() {
    // `box` with no operator is not a member access — nothing to do.
    let src = "void f() { box }";
    let cursor = after(src, "box");
    assert!(receiver_at(&mut cpp(), &crate::build::packs::cpp_pack(), src, cursor).is_none());
}

// ---- Recovery: the closer the sentinel parse splices after the cursor ----

/// What the shared sentinel parse spliced after `__CURSOR__` at the `|` in
/// `marked`: the closer it retried with, or `""` when the plain splice stood.
fn spliced(parser: &mut Parser, pack: &crate::build::query_extract::LangPack, marked: &str) -> String {
    let cursor = marked.find('|').expect("a cursor mark");
    let src = marked.replacen('|', "", 1);
    let written = parser.parse(&src, None).unwrap();
    let sp = sentinel_parse(parser, pack, &src, &written, cursor).unwrap_or_else(|| panic!("a sentinel parse in {marked:?}"));
    assert_eq!(&sp.patched[..cursor], &src[..cursor], "{marked:?}: the bytes before the cursor keep their offsets");
    let insert = &sp.patched[cursor..cursor + sp.patched.len() - src.len()];
    insert.strip_prefix(SENTINEL).expect("the sentinel leads the splice").trim().to_string()
}

fn php() -> Parser {
    let mut p = Parser::new();
    p.set_language(&tree_sitter_php::LANGUAGE_PHP.into()).unwrap();
    p
}

#[test]
fn an_unclosed_bracket_before_the_cursor_is_closed_once_in_python() {
    let pack = crate::build::packs::python_pack();
    let mut p = python();
    for (marked, closer) in [
        ("local(|", ")"),
        ("local(name=1, |", ")"),
        ("local(name=1, |\nx = 2", ")"),
        ("def h():\n    x = local(name=1, |\n    return x", ")"),
        ("local(name=1, pick(|", ")"),
        ("local(name=1, pick(|\nx = 2", ")"),
        ("local([1, 2], |", ")"),
        ("local([1, |", "]"),
        ("local(name=1, {'a': |", "}"),
        ("local(name=1, (|", ")"),
        ("k.go(|", ")"),
        // The innermost pair is closed: no retry, whatever else the file did.
        ("local(na|, size=2)", ""),
        ("local(pick(|, size=2)", ""),
        ("local(name=1, |)", ""),
    ] {
        assert_eq!(spliced(&mut p, &pack, marked), closer, "{marked:?}");
    }
}

#[test]
fn an_unclosed_bracket_before_the_cursor_is_closed_once_in_php() {
    let pack = crate::build::packs::php_pack();
    let mut p = php();
    for (body, closer) in [
        ("connect(ti|", ")"),
        ("connect(host: 'h', |", ")"),
        ("connect(host: 'h', |\n$x = 2;", ")"),
        ("function h() {\n    $y = connect(host: 'h', |\n    return $y;\n}", ")"),
        ("connect(host: pick(|", ")"),
        ("connect(host: pick(|\n$x = 2;", ")"),
        ("connect([1, 2], |", ")"),
        ("$z = [connect(|", ")"),
        ("connect([1, |", "]"),
        ("connect(host: (|", ")"),
        ("connect(ho|, retry: true);", ""),
        ("connect(pick(|, retry: true);", ""),
        ("if (connect(|\n) {\n}", ""),
    ] {
        let marked = format!("<?php {body}");
        assert_eq!(spliced(&mut p, &pack, &marked), closer, "{marked:?}");
    }
}

#[test]
fn an_unclosed_bracket_before_the_cursor_is_closed_once_in_cpp() {
    let pack = crate::build::packs::cpp_pack();
    let mut p = cpp();
    for (body, closer) in [
        ("g(|", ")"),
        ("g(a, |", ")"),
        ("g(h(x), |", ")"),
        ("g(a, h(x, |", ")"),
        ("o.m(x, |", ")"),
        ("int y = g(a, |\n  int z = 2;", ")"),
        ("g(a, \"(\", |", ")"),
        ("g(a, |);", ""),
        ("box.|", ""),
    ] {
        let marked = format!("void f() {{\n  {body}\n}}");
        assert_eq!(spliced(&mut p, &pack, &marked), closer, "{marked:?}");
    }
}

/// The retry closes the innermost bracket only: the code after the cursor
/// may close the outer ones, and one closer is one reparse at any depth.
#[test]
fn a_deep_nest_splices_one_closer() {
    let pack = crate::build::packs::python_pack();
    let marked = format!("{}|", "f(".repeat(1000));
    assert_eq!(spliced(&mut python(), &pack, &marked), ")");
}

/// A bracket inside a string is the string's text, not an open token.
#[test]
fn a_bracket_in_a_string_opens_nothing() {
    let pack = crate::build::packs::python_pack();
    assert_eq!(spliced(&mut python(), &pack, "s = \"(\" + |"), "");
    assert_eq!(spliced(&mut python(), &pack, "g(\"(\", |"), ")");
}

/// A call that parsed is read on the plain splice, byte for byte.
#[test]
fn a_closed_call_reads_the_plain_splice() {
    let pack = crate::build::packs::python_pack();
    let mut p = python();
    for marked in ["local(name=1, |)\n", "local(|)\nx = 2\n", "obj.|\n"] {
        let cursor = marked.find('|').unwrap();
        let src = marked.replacen('|', "", 1);
        let written = p.parse(&src, None).unwrap();
        let sp = sentinel_parse(&mut p, &pack, &src, &written, cursor).unwrap();
        assert_eq!(sp.patched, patch(&src, cursor), "{marked:?}");
    }
}
