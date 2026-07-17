//! Measures the C++ reparse seam: macro expansion → corrected re-parse
//! → recovered symbols through the production cpp_pack extractor, with
//! anchor remap back to original source.

use super::*;
use crate::query_extract::{cpp_pack, extract};

#[path = "cpp_obstacle.rs"]
mod cpp_obstacle;

fn cpp_parser() -> tree_sitter::Parser {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
    p
}

fn parse(p: &mut tree_sitter::Parser, src: &str) -> tree_sitter::Tree {
    p.parse(src, None).unwrap()
}

fn errors(node: tree_sitter::Node) -> usize {
    let mut n = 0;
    let mut cur = node.walk();
    let mut stack = vec![node];
    while let Some(x) = stack.pop() {
        if x.is_error() || x.is_missing() {
            n += 1;
        }
        for c in x.children(&mut cur) {
            stack.push(c);
        }
    }
    n
}

fn extracted_names(p: &mut tree_sitter::Parser, src: &str) -> Vec<String> {
    let tree = parse(p, src);
    extract(&tree, src.as_bytes(), &cpp_pack())
        .unwrap()
        .symbols
        .iter()
        .map(|s| s.name.clone())
        .collect()
}

fn sample(name: &str) -> &'static cpp_obstacle::Sample {
    cpp_obstacle::SAMPLES.iter().find(|s| s.name == name).unwrap()
}

#[test]
fn object_macro_recovers_corrupted_class() {
    let mut p = cpp_parser();
    let src = sample("api_export_attr").src;

    // baseline: class evaporates into a function_definition
    let before = parse(&mut p, src);
    assert!(
        before.root_node().to_sexp().contains("function_definition"),
        "baseline corruption: class parsed as a function",
    );
    assert!(!extracted_names(&mut p, src).contains(&"Widget".to_string()), "Widget lost pre-reparse");

    let tree = parse(&mut p, src);
    let (rewritten, map) = preprocess_with(&tree, src, &PreExpandedExternal::empty());

    // the corrected parse: a real class, error-free
    let after = parse(&mut p, &rewritten);
    assert_eq!(errors(after.root_node()), 0, "expansion clears the parse: {rewritten}");
    assert!(after.root_node().to_sexp().contains("class_specifier"), "real class recovered");

    // through the production extractor: Widget + its methods are back
    let names = extracted_names(&mut p, &rewritten);
    assert!(names.contains(&"Widget".to_string()), "class recovered: {names:?}");
    assert!(names.contains(&"draw".to_string()), "method recovered: {names:?}");
    assert!(names.contains(&"resize".to_string()), "method recovered: {names:?}");

    // anchor remap: `Widget` in the rewritten source maps back onto the
    // original `Widget` token (despite the inserted attribute bytes).
    let w_t = rewritten.find("Widget").unwrap();
    let w_o = map.to_original(w_t);
    assert_eq!(&src[w_o..w_o + 6], "Widget", "anchor lands on original Widget");
}

#[test]
fn function_macro_expands_to_member_declarations() {
    let mut p = cpp_parser();
    let src = sample("decl_macro").src;

    // the macro-generated members are absent pre-expansion
    assert!(!extracted_names(&mut p, src).contains(&"GetRuntimeClass".to_string()));

    let tree = parse(&mut p, src);
    let (rewritten, _map) = preprocess_with(&tree, src, &PreExpandedExternal::empty());

    // DECLARE_DYNAMIC(MyObj) expanded its body with cls→MyObj; the
    // virtual method is now a real, extractable symbol.
    assert!(rewritten.contains("GetRuntimeClass"), "body expanded: {rewritten}");
    assert!(rewritten.contains("MyObj* Ptr"), "param substituted: {rewritten}");
    let names = extracted_names(&mut p, &rewritten);
    assert!(names.contains(&"GetRuntimeClass".to_string()), "method synthesized by expansion: {names:?}");
    assert!(names.contains(&"MyObj".to_string()), "class still present: {names:?}");
}

/// The load-bearing invariant of the two-tier own/external split: the fast
/// path (locals fixpointed over a cached, pre-expanded external table) and the
/// slow path (the old single-tier merge+fixpoint) produce BYTE-IDENTICAL
/// expansion. Exercised on a macro-heavy mix that hits every tricky case the
/// split must preserve: external-referencing-external, external-referencing-
/// local, and a local `#define` shadowing an external one.
#[test]
fn two_tier_matches_single_tier_expansion() {
    let mut p = cpp_parser();

    // EXTERNAL macros (as if gathered from #included headers).
    let mut ext = std::collections::BTreeMap::new();
    let def = |m: &mut std::collections::BTreeMap<String, Macro>, n: &str, params: Option<&[&str]>, body: &str| {
        m.insert(
            n.to_string(),
            Macro {
                params: params.map(|ps| ps.iter().map(|s| s.to_string()).collect()),
                body: body.to_string(),
                guards: Vec::new(),
                def_line: 0,
            },
        );
    };
    // external → external (mutual expansion, baked once in the cache)
    def(&mut ext, "API", None, "EXPORTED");
    def(&mut ext, "EXPORTED", None, "__attribute__((visibility(\"default\")))");
    def(&mut ext, "WRAP", Some(&["x"]), "API x");
    // external body that NAMES a local macro (LOCAL_TAG) — forces the slow path
    def(&mut ext, "TAGGED", None, "LOCAL_TAG int");
    // a name the file will SHADOW with its own #define
    def(&mut ext, "MAXLEN", None, "256");

    let external = PreExpandedExternal::from_raw(std::sync::Arc::new(ext));

    let cases = [
        // clean split: only external-referencing-external, no interaction
        "class API Widget { void draw(); };\nint sz = MAXN;\n#define MAXN 8\nWRAP(class) Gadget {};\n",
        // external body references a local macro → slow path must engage
        "#define LOCAL_TAG const\nTAGGED foo;\nclass API Thing { void go(); };\n",
        // local shadows an external name → slow path must engage
        "#define MAXLEN 16\nint buf[MAXLEN];\nclass API Box { void run(); };\n",
        // both interactions at once
        "#define LOCAL_TAG volatile\n#define MAXLEN 32\nTAGGED n = MAXLEN;\nclass API Q {};\n",
    ];

    for src in cases {
        let tree = parse(&mut p, src);
        // args: (…, alias_only, force_slow, expand_region_bodies). Full mode
        // both; the force_slow flag toggles fast two-tier vs the old single-tier
        // merge — exclusion scope held at the narrow default throughout.
        let (fast, fmap) = preprocess_with_mode_inner(&tree, src, &external, false, false, true);
        let (slow, smap) = preprocess_with_mode_inner(&tree, src, &external, false, true, true);
        assert_eq!(fast, slow, "expansion drift (full) on:\n{src}\nfast:\n{fast}\nslow:\n{slow}");
        assert_eq!(fmap.edits_for_test(), smap.edits_for_test(), "splice-map drift on:\n{src}");

        // alias-only mode (the parse-damage fallback) must also agree.
        let (fa, _) = preprocess_with_mode_inner(&tree, src, &external, true, false, true);
        let (sa, _) = preprocess_with_mode_inner(&tree, src, &external, true, true, true);
        assert_eq!(fa, sa, "alias-only expansion drift on:\n{src}");
    }
}

#[test]
fn no_macros_is_identity() {
    let mut p = cpp_parser();
    let src = sample("clean_baseline").src;
    let tree = parse(&mut p, src);
    let (rewritten, map) = preprocess_with(&tree, src, &PreExpandedExternal::empty());
    assert_eq!(rewritten, src, "no expandable macros → identity");
    assert_eq!(map.to_original(42), 42);
}

#[test]
fn cpp_reparse_obstacle_delta_report() {
    let mut p = cpp_parser();
    println!("\n===== C++ reparse seam: before → after macro expansion =====");
    println!(
        "{:<18} {:>12}   {:>14}",
        "sample", "errors b→a", "Tier-1 recall b→a"
    );
    let (mut rb, mut ra, mut tot) = (0, 0, 0);
    for s in cpp_obstacle::SAMPLES {
        let tree = parse(&mut p, s.src);
        let eb = errors(tree.root_node());
        let nb = extracted_names(&mut p, s.src);
        let hb = s.expected.iter().filter(|n| nb.iter().any(|g| g == *n)).count();

        let _ = tree;
        let (rewritten, _, _) = preprocess_validated_with(&mut p, s.src, &PreExpandedExternal::empty());
        let ta = parse(&mut p, &rewritten);
        let ea = errors(ta.root_node());
        let na = extracted_names(&mut p, &rewritten);
        let ha = s.expected.iter().filter(|n| na.iter().any(|g| g == *n)).count();

        rb += eb;
        ra += ea;
        tot += s.expected.len();
        println!(
            "{:<18} {:>5} → {:<4}   {:>6}/{} → {}/{}",
            s.name,
            eb,
            ea,
            hb,
            s.expected.len(),
            ha,
            s.expected.len(),
        );
        let _ = &mut tot;
    }
    println!("----- totals: errors {rb} → {ra}; expected-symbol pool = {tot} -----\n");
}

// --- SpliceMap: binary search ≡ the former linear scan --------------------
//
// `to_original` / `replacement_at` map extracted spans back to user text;
// a wrong result silently breaks goto-def/hover/rename for C/C++. These
// reference impls ARE the old linear scan; the fuzz test asserts the
// binary-search production path returns byte-identical results for every
// transformed offset over randomized (incl. zero-width, adjacent) edits.

fn ref_to_original(edits: &[(usize, usize, usize)], transformed: usize) -> usize {
    let mut shift: isize = 0;
    for &(os, oe, nlen) in edits {
        let ts = (os as isize + shift) as usize;
        if transformed < ts {
            return (transformed as isize - shift) as usize;
        }
        if transformed < ts + nlen {
            return os;
        }
        shift += nlen as isize - (oe - os) as isize;
    }
    (transformed as isize - shift) as usize
}

fn ref_replacement_at(edits: &[(usize, usize, usize)], transformed: usize) -> Option<(usize, usize)> {
    let mut shift: isize = 0;
    for &(os, oe, nlen) in edits {
        let ts = (os as isize + shift) as usize;
        if transformed < ts {
            return None;
        }
        if transformed < ts + nlen {
            return Some((os, oe));
        }
        shift += nlen as isize - (oe - os) as isize;
    }
    None
}

/// The PERL_BITFIELD16 shape: three config-variant `#define`s under
/// `#ifdef` / `#if defined` / `#else`. Every variant is modeled with its
/// guard trail — nothing collapses to the collection-order winner.
#[test]
fn collect_macro_variants_captures_guard_trails() {
    let mut p = cpp_parser();
    let src = "\
#ifdef WIN32
#define M U16
#endif
#if defined(HAS_NON_INT_BITFIELDS)
#define M U16
#else
#define M unsigned
#endif
struct s { M x:9; };
";
    let tree = parse(&mut p, src);
    let variants = collect_macro_variants(&tree, src.as_bytes());
    let m = variants.get("M").expect("M has variants");
    assert_eq!(m.len(), 3, "all three config variants modeled, none pruned");

    let by_line: std::collections::HashMap<usize, &Macro> =
        m.iter().map(|v| (v.def_line, v)).collect();
    // win32 variant — guarded by the #ifdef.
    assert_eq!(by_line[&1].guards, vec!["defined(WIN32)".to_string()]);
    assert_eq!(by_line[&1].body, "U16");
    // the `#if defined(...)` then-branch.
    assert_eq!(by_line[&4].guards, vec!["defined(HAS_NON_INT_BITFIELDS)".to_string()]);
    // the `#else` branch — the condition is NEGATED.
    assert_eq!(by_line[&6].guards, vec!["!defined(HAS_NON_INT_BITFIELDS)".to_string()]);
    assert_eq!(by_line[&6].body, "unsigned");
}

/// Content sniff: a `.def`-style C dispatch table (no
/// recognizable extension) reads as C-family on its preprocessor/brace
/// shape; a Perl script with the same unowned extension must NOT.
#[test]
fn sniff_c_family_vs_perl_on_content() {
    let c_src = "\
/* Automatically generated */
const char *COMMAND_GROUP_STR[] = {
    \"generic\",
    \"string\",
};
#ifndef SKIP_CMD_HISTORY_TABLE
struct redisCommand cmd;
#endif
";
    assert!(looks_like_c_family(c_src), "C dispatch-table shape should sniff as C-family");

    let perl_src = "\
use strict;
use warnings;
package Foo::Bar;

sub greet {
    my ($self, $name) = @_;
    return \"hi $name\";
}
";
    assert!(!looks_like_c_family(perl_src), "a Perl script must not sniff as C-family");

    assert!(!looks_like_c_family(""), "empty content has no opinion");
}

/// The header-guard idiom (`#ifndef X` / `#define X`) is NOT a real config
/// knob: a macro nested under it must not inherit "!defined(X)" as a guard
/// term (gd on nearly every macro in a guarded header was
/// printing a bogus `(if !defined(__REDIS_H))` label). A genuinely
/// conditional macro nested in the SAME guarded region keeps its own guard.
#[test]
fn header_guard_is_not_a_config_knob() {
    let mut p = cpp_parser();
    let src = "\
#ifndef FOO_H
#define FOO_H
#define PLAIN 1
#ifdef ENABLE_X
#define GATED 2
#endif
#endif
";
    let tree = parse(&mut p, src);
    let variants = collect_macro_variants(&tree, src.as_bytes());
    assert!(variants["FOO_H"][0].guards.is_empty(), "the guard's own #define carries no terms");
    assert!(
        variants["PLAIN"][0].guards.is_empty(),
        "a plain macro nested in the header guard doesn't inherit it: {:?}",
        variants["PLAIN"][0].guards
    );
    assert_eq!(
        variants["GATED"][0].guards,
        vec!["defined(ENABLE_X)".to_string()],
        "a REAL conditional nested in the same guard keeps its own guard"
    );
}

/// A bodyless `#define FLAG` is the canonical config knob — it must enter
/// the macro universe (and the defined set when unconditional), or the
/// reachability ranking of `#ifdef FLAG` arms comes out exactly inverted.
#[test]
fn bodyless_define_joins_the_config_universe() {
    use crate::cpp_macro_model::{classify, KnownConfig, Reachability};
    let mut p = cpp_parser();
    let src = "\
#define MY_FEATURE
#ifdef MY_FEATURE
#define LIMIT 42
#else
#define LIMIT 7
#endif
";
    // Identity lane: the flag is a MacroDef (goto-def on the flag lands).
    let defs = crate::cpp_reparse::collect_macro_defs(&mut p, src);
    let flag = defs.iter().find(|d| d.name == "MY_FEATURE").expect("bodyless define collected");
    assert!(flag.body.is_empty());
    assert!(flag.guards.is_empty());
    assert!(flag.delegate.is_none());

    // Reachability: with the flag ON, the #ifdef arm is ACTIVE and the
    // #else arm provably unreachable — not the inverse.
    let tree = parse(&mut p, src);
    let variants = collect_macro_variants(&tree, src.as_bytes());
    let mut defined = std::collections::HashSet::new();
    let mut universe = std::collections::HashSet::new();
    for (name, vs) in &variants {
        universe.insert(name.clone());
        if vs.iter().any(|m| m.guards.is_empty()) {
            defined.insert(name.clone());
        }
    }
    assert!(defined.contains("MY_FEATURE"), "unconditional bodyless define is known ON");
    let cfg = KnownConfig::new(defined, universe);
    let lim = &variants["LIMIT"];
    let ifdef_arm = lim.iter().find(|m| m.def_line == 2).expect("#ifdef arm");
    let else_arm = lim.iter().find(|m| m.def_line == 4).expect("#else arm");
    assert_eq!(classify(&ifdef_arm.guards, &cfg), Reachability::Active);
    assert!(matches!(classify(&else_arm.guards, &cfg), Reachability::Unreachable { .. }));
}

/// Nested-macro-body refs: a use of a known macro inside another `#define`'s
/// body is minted at its ORIGINAL span; the macro's own params and
/// stringify/paste operands are excluded.
#[test]
fn macro_body_name_refs_mints_known_macro_uses() {
    let mut p = cpp_parser();
    let src = "\
#define FLAGS(x)  (x)->f
#define IS_OK(x)  (FLAGS(x) & 1)
#define STR(x)    #x
#define CAT(a,b)  a ## b
";
    let known: std::collections::HashSet<String> =
        ["FLAGS", "IS_OK", "STR", "CAT", "x"].iter().map(|s| s.to_string()).collect();
    let refs = crate::cpp_reparse::macro_body_name_refs(&mut p, src, &known);
    let name_refs = &refs.name_refs;
    // FLAGS used inside IS_OK's body (line index 1) is the one real ref.
    let flags: Vec<_> = name_refs.iter().filter(|(n, _)| n == "FLAGS").collect();
    assert_eq!(flags.len(), 1, "one FLAGS body use, got {name_refs:?}");
    let (_, span) = flags[0];
    assert_eq!(span.start.row, 1, "FLAGS use is on the IS_OK line");
    // `x` is a param everywhere it appears in a body — never minted, even
    // though it's in `known`. `#x` (stringify) and `a`/`b` (paste operands)
    // are not references either.
    assert!(name_refs.iter().all(|(n, _)| n != "x"), "params excluded: {name_refs:?}");
    // `(x)->f` inside FLAGS's body: `f` is a member-access token, recovered
    // into the member lane (never the name lane), untyped for the assembly
    // pass to resolve to its declaring struct.
    assert!(
        refs.member_refs.iter().any(|(n, s)| n == "f" && s.start.row == 0),
        "->f member use recovered: {:?}",
        refs.member_refs
    );
    assert!(name_refs.iter().all(|(n, _)| n != "f"), "member token not in name lane");
}

/// End-to-end reachability over the captured variants: WIN32 absent → its
/// variant is UNREACHABLE-labeled (not dropped); the HAS knob's two branches
/// are UNKNOWN.
#[test]
fn variant_model_ranks_by_reachability() {
    use crate::cpp_macro_model::{classify, KnownConfig, Reachability};
    let mut p = cpp_parser();
    let src = "\
#ifdef WIN32
#define M short
#endif
#if defined(HAS_NON_INT_BITFIELDS)
#define M unsigned
#else
#define M int
#endif
";
    let tree = parse(&mut p, src);
    let raw = collect_macro_variants(&tree, src.as_bytes());
    // Known config: nothing predefined; HAS_NON_INT_BITFIELDS is a knob we've
    // seen #defined somewhere (universe), WIN32 is not.
    let cfg = KnownConfig::new(
        Default::default(),
        ["HAS_NON_INT_BITFIELDS".to_string()].into_iter().collect(),
    );
    let mut ranked: Vec<_> = raw["M"]
        .iter()
        .map(|v| (v, classify(&v.guards, &cfg)))
        .collect();
    ranked.sort_by_key(|(_, r)| r.rank());
    assert_eq!(ranked.len(), 3, "nothing is ever pruned");
    // last one is the win32 variant, unreachable + labeled.
    let (last, r) = ranked.last().unwrap();
    assert_eq!(last.body, "short");
    assert_eq!(last.guards, vec!["defined(WIN32)".to_string()]);
    assert_eq!(r.label().as_deref(), Some("unreachable: WIN32 undefined"));
    // the two HAS branches are UNKNOWN (kept, not guessed).
    assert!(ranked[..2]
        .iter()
        .all(|(_, r)| matches!(r, Reachability::Unknown { .. })));
}

#[test]
fn splicemap_binsearch_matches_linear_scan() {
    // Deterministic LCG — no external rng dep.
    let mut state: u64 = 0x9e3779b97f4a7c15;
    let mut next = |bound: usize| {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) as usize % bound.max(1)
    };

    for _ in 0..4000 {
        let src_len = 8 + next(40);
        let src: String = std::iter::repeat('a').take(src_len).collect();
        // Random sorted, disjoint splices. Adjacent (gap 0) + empty
        // replacements are generated so the zero-width `ts`-tie path fires.
        let mut splices: Vec<Splice> = Vec::new();
        let mut cur = 0usize;
        while cur < src_len {
            let gap = next(3); // 0 → adjacent to the previous edit
            let start = cur + gap;
            if start >= src_len {
                break;
            }
            let end = (start + next(4)).min(src_len); // width 0..3
            let rep_len = next(4); // 0..3, incl. empty
            let replacement: String = std::iter::repeat('X').take(rep_len).collect();
            splices.push(Splice { start, end, replacement, name: String::new() });
            cur = end + 1;
        }
        let (out, map) = apply(&src, &mut splices);
        for t in 0..=out.len() {
            assert_eq!(
                map.to_original(t),
                ref_to_original(&map.edits, t),
                "to_original mismatch at {t} for edits {:?}",
                map.edits
            );
            assert_eq!(
                map.replacement_at(t),
                ref_replacement_at(&map.edits, t),
                "replacement_at mismatch at {t} for edits {:?}",
                map.edits
            );
        }
    }
}

/// The macro identity lane: `collect_macro_defs` captures each `#define` with
/// its def span and — for a function-like DIRECT-DELEGATION wrapper — the
/// callee it forwards to. Only a body that IS one whole call `G(args)` is a
/// delegation; `F(x) + 1` and object-like macros are not.
#[test]
fn collect_macro_defs_recognizes_delegation() {
    let mut p = cpp_parser();
    let src = "\
#define WRAP(x) realFunc(x)
#define THREADED(x) Perl_new(aTHX_ x)
#define NOTDELEG(x) realFunc(x) + 1
#define OBJLIKE 100
";
    let defs = crate::cpp_reparse::collect_macro_defs(&mut p, src);
    let by_name: std::collections::HashMap<&str, &crate::file_analysis::MacroDef> =
        defs.iter().map(|d| (d.name.as_str(), d)).collect();

    assert_eq!(by_name["WRAP"].delegate.as_deref(), Some("realFunc"));
    assert_eq!(by_name["THREADED"].delegate.as_deref(), Some("Perl_new"));
    // A call that is only PART of the body is not delegation.
    assert_eq!(by_name["NOTDELEG"].delegate, None);
    // Object-like macros never delegate (no params).
    assert_eq!(by_name["OBJLIKE"].delegate, None);
    assert!(by_name["OBJLIKE"].params.is_none());
    assert!(by_name["WRAP"].params.is_some());

    // The def span lands on the macro NAME (not the `#define` keyword).
    let wrap = by_name["WRAP"];
    assert_eq!(wrap.selection_span.start.row, 0);
    assert_eq!(wrap.selection_span.start.column, 8); // after "#define "
}

/// The parametric-return lane: an identity/projection macro body reduces to
/// one of its parameters (paren/cast wrappers transparent). `classify_param_
/// return` reports the param index; a non-param body reports None (the
/// param-independent `classify_body_type` lane handles those).
#[test]
fn classify_param_return_reads_param_index() {
    let mut p = cpp_parser();
    let params = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();

    // Bare identity.
    assert_eq!(
        crate::cpp_reparse::classify_param_return(&mut p, "(x)", &params(&["x"])),
        Some(0)
    );
    // Cast wrapper — the cast type is ignored; still the argument's value.
    assert_eq!(
        crate::cpp_reparse::classify_param_return(&mut p, "((Widget*)(x))", &params(&["x"])),
        Some(0)
    );
    // Two-param select-second.
    assert_eq!(
        crate::cpp_reparse::classify_param_return(&mut p, "(b)", &params(&["a", "b"])),
        Some(1)
    );
    // An operator body is param-INDEPENDENT, not an identity.
    assert_eq!(
        crate::cpp_reparse::classify_param_return(&mut p, "((x)*(x))", &params(&["x"])),
        None
    );
    // A bare identifier that isn't a parameter (a global) is not a param return.
    assert_eq!(
        crate::cpp_reparse::classify_param_return(&mut p, "(GLOBAL)", &params(&["x"])),
        None
    );
    // Real redis shapes: `UNUSED(x) (void)(x)` and `ANNOTATE_HAPPENS_BEFORE(v)
    // ((void) v)` are cast-to-void identities — the pervasive C spelling.
    assert_eq!(
        crate::cpp_reparse::classify_param_return(&mut p, "(void)(x)", &params(&["x"])),
        Some(0)
    );
    assert_eq!(
        crate::cpp_reparse::classify_param_return(&mut p, "((void) v)", &params(&["v"])),
        Some(0)
    );
}

// ===== Member-block macros as roles =====

#[test]
fn member_block_macro_classified_blanked_and_minted() {
    let mut p = cpp_parser();
    // BASEOP is a field-block macro pasted STANDALONE into two structs; a
    // one-member REFCNT proves roles-all-the-way. `PERL_BITFIELD16` types the
    // key field (re-sourced `TypeName` edge).
    let src = "\
#define PERL_BITFIELD16 unsigned short
#define BASEOP PERL_BITFIELD16 op_type:9; int op_flags;
#define REFCNT int op_refcnt;
struct op { BASEOP };
struct unop { BASEOP int* op_first; };
struct sv { REFCNT };
";
    let plan = crate::cpp_reparse::plan_member_blocks(&mut p, src);
    assert!(!plan.is_empty(), "member-block macros should be detected");

    // Blank mode: the use is whitespace in the parse view, so the structs parse
    // clean — but the ORIGINAL still holds the token (identity preserved).
    assert!(plan.blanked_source.contains("struct op {        };") || plan.blanked_source.contains("struct op {  "),
        "BASEOP use blanked: {:?}", plan.blanked_source);
    assert!(plan.blanked_source.contains("BASEOP") == false || !plan.blanked_source.contains("{ BASEOP"),
        "no BASEOP use survives in the parse view");
    assert_eq!(errors(parse(&mut p, &plan.blanked_source).root_node()), 0, "blanked source parses clean");

    // Parent edges — the copypasta IS inheritance.
    assert!(plan.edges.contains(&("op".to_string(), "BASEOP".to_string())));
    assert!(plan.edges.contains(&("unop".to_string(), "BASEOP".to_string())));
    assert!(plan.edges.contains(&("sv".to_string(), "REFCNT".to_string())));

    // One synthetic base per macro, members parsed from the config-active body.
    let baseop = plan.bases.iter().find(|b| b.macro_name == "BASEOP").expect("BASEOP base");
    let names: Vec<&str> = baseop.members.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, vec!["op_type", "op_flags"]);
    let op_type = &baseop.members[0];
    assert_eq!(op_type.type_text, "PERL_BITFIELD16");
    // Positioned at the real `#define` body token (line 1, not the use sites).
    assert_eq!(op_type.name_span.start.row, 1);

    // Roles all the way: even a one-member macro is a role.
    let refcnt = plan.bases.iter().find(|b| b.macro_name == "REFCNT").expect("REFCNT base");
    assert_eq!(refcnt.members.len(), 1);
    assert_eq!(refcnt.members[0].name, "op_refcnt");
}

#[test]
fn funclike_member_block_comment_truncated_body_keeps_all_fields() {
    let mut p = cpp_parser();
    // The perl5 sv.h `_SV_HEAD` shape: a FUNCTION-like member-block macro whose
    // `\`-continued body carries a trailing block comment on each field line, and
    // whose last field has no `;` (the `;` comes from the paste). tree-sitter-cpp
    // ends `preproc_arg` at the first comment, so a CST-span body kept only the
    // first field. The body must be re-derived from raw source across the
    // continuations, and the call-shaped paste handled as a role.
    let src = "\
#define _SV_HEAD(ptrtype) \\
    ptrtype  sv_any;    /* pointer to body */    \\
    unsigned sv_refcnt; /* how many refs */      \\
    unsigned sv_flags   /* what we are */

struct sv { _SV_HEAD(void*); };
";
    let plan = crate::cpp_reparse::plan_member_blocks(&mut p, src);
    assert!(!plan.is_empty(), "function-like member block should be detected");

    let base = plan.bases.iter().find(|b| b.macro_name == "_SV_HEAD").expect("_SV_HEAD base");
    let names: Vec<&str> = base.members.iter().map(|m| m.name.as_str()).collect();
    // All three fields survive — the comment-truncation no longer drops sv_flags.
    assert_eq!(names, vec!["sv_any", "sv_refcnt", "sv_flags"]);

    // The parent edge forms despite the comment-truncated def sitting right above
    // the struct (comment neutralization keeps the blanked view parsing clean).
    assert!(
        plan.edges.contains(&("sv".to_string(), "_SV_HEAD".to_string())),
        "edges: {:?}",
        plan.edges
    );
    assert_eq!(errors(parse(&mut p, &plan.blanked_source).root_node()), 0, "blanked source parses clean");
}

#[test]
fn comment_free_bitfield_block_synthesizes_every_field() {
    let mut p = cpp_parser();
    // The op.h `BASEOP` shape (comment-free): a bitfield-heavy field block. Every
    // field — plain, bitfield, function-pointer, `U8` — must synthesize; the
    // per-field split never drops one (Family M #2 was a use-site config-region
    // misattribution, not a synthesis gap).
    let src = "\
#define BASEOP \\
    OP*  op_next; \\
    OP*  (*op_ppaddr)(pTHX); \\
    PADOFFSET  op_targ; \\
    PERL_BITFIELD16 op_type:9; \\
    PERL_BITFIELD16 op_opt:1; \\
    PERL_BITFIELD16 op_slabbed:1; \\
    U8  op_flags; \\
    U8  op_private;
struct op { BASEOP };
";
    let plan = crate::cpp_reparse::plan_member_blocks(&mut p, src);
    let base = plan.bases.iter().find(|b| b.macro_name == "BASEOP").expect("BASEOP base");
    let names: Vec<&str> = base.members.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(
        names,
        vec!["op_next", "op_ppaddr", "op_targ", "op_type", "op_opt", "op_slabbed", "op_flags", "op_private"],
        "all fields synthesize, none dropped"
    );
}

#[test]
fn member_block_pointer_field_keeps_its_deref_stack() {
    let mut p = cpp_parser();
    // hitlist-4 finding 4: a pointer member declared inside a `#define BASEOP`
    // body must peel its `*` into `deref_stack` (so hover renders `OP*`, not the
    // bare `OP`), exactly as a plainly-declared field does. Non-pointer / bitfield
    // members keep an EMPTY stack.
    let src = "\
#define BASEOP OP* op_next; unsigned op_type:9; OP** op_sibparent;
struct op { BASEOP };
";
    let plan = crate::cpp_reparse::plan_member_blocks(&mut p, src);
    let base = plan.bases.iter().find(|b| b.macro_name == "BASEOP").expect("BASEOP base");
    let member = |n: &str| base.members.iter().find(|m| m.name == n).unwrap_or_else(|| panic!("member {n}"));
    use crate::file_analysis::DerefKind;
    assert_eq!(
        member("op_next").deref_stack.iter().map(|s| s.kind).collect::<Vec<_>>(),
        vec![DerefKind::Pointer],
        "single-pointer member peels one Pointer step"
    );
    assert_eq!(
        member("op_sibparent").deref_stack.iter().map(|s| s.kind).collect::<Vec<_>>(),
        vec![DerefKind::Pointer, DerefKind::Pointer],
        "double-pointer member peels two Pointer steps"
    );
    assert!(member("op_type").deref_stack.is_empty(), "bitfield member has no deref stack");
}

#[test]
fn non_member_block_macros_are_untouched() {
    let mut p = cpp_parser();
    // A value macro and a function-like macro are NOT member blocks — no plan.
    let src = "\
#define MAX 100
#define MIN(a,b) ((a)<(b)?(a):(b))
struct s { int x; };
";
    let plan = crate::cpp_reparse::plan_member_blocks(&mut p, src);
    assert!(plan.is_empty(), "no member-block macros here");
    assert_eq!(plan.blanked_source, src, "source unchanged");
}

/// Poisoned-persist lock: a header that RESOLVES (exists) but fails to read
/// (non-UTF-8, transient I/O) silently truncates the closure — its transitive
/// includes never enqueue. The BFS must report `complete=false` so the driver
/// marks the analysis degraded and `save_to_db` refuses to freeze the truncated
/// closure behind a self-validating `deps_stamp`. A fully-readable closure
/// reports `complete=true`. An UNRESOLVED include is a legitimate boundary, not
/// incompleteness.
#[test]
fn include_closure_reports_incomplete_on_unreadable_header() {
    let dir = std::env::temp_dir().join(format!("closure_trunc_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let good = dir.join("good.h");
    let bad = dir.join("bad.h");
    let nested = dir.join("nested.h");
    let main_c = dir.join("main.c");
    std::fs::write(&nested, "#define NESTED 1\n").unwrap();
    // `bad.h` resolves + canonicalizes (exists) but read_to_string fails on the
    // invalid UTF-8, so its `#include "nested.h"` is never followed.
    std::fs::write(&good, "#define OK 1\n").unwrap();
    std::fs::write(&bad, [0xff, 0xfe, b'#', b'i', b'n', b'c']).unwrap();

    let complete_src = "#include \"good.h\"\n";
    std::fs::write(&main_c, complete_src).unwrap();
    let (closure, complete) = crate::cpp_reparse::include_closure(&main_c, complete_src);
    assert!(complete, "a fully-readable closure is complete");
    assert_eq!(closure.len(), 1);

    let trunc_src = "#include \"bad.h\"\n";
    std::fs::write(&main_c, trunc_src).unwrap();
    let (_closure, complete) = crate::cpp_reparse::include_closure(&main_c, trunc_src);
    assert!(!complete, "an unreadable resolved header truncates → incomplete");

    let unresolved_src = "#include \"no_such_system_header.h\"\n";
    std::fs::write(&main_c, unresolved_src).unwrap();
    let (closure, complete) = crate::cpp_reparse::include_closure(&main_c, unresolved_src);
    assert!(complete, "an unresolved include is a boundary, not incompleteness");
    assert!(closure.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// H1 lock: the tier-1 caches are keyed by (file, include-set) and cannot
/// see header CONTENT edits; `evict_analysis_caches` is the invalidation
/// seam the save/watcher path drives. After eviction, the macro table and
/// the include closure re-gather against the new bytes.
#[test]
fn evict_analysis_caches_recovers_header_content_edits() {
    let dir = std::env::temp_dir().join(format!("evict_seam_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let hdr = dir.join("hdr.h");
    let hdr2 = dir.join("hdr2.h");
    let main_c = dir.join("main.c");
    std::fs::write(&hdr, "#define LIMIT 5\n").unwrap();
    let src = "#include \"hdr.h\"\nint x = LIMIT;\n";
    std::fs::write(&main_c, src).unwrap();

    let mut p = cpp_parser();
    let t1 = included_macros(&main_c, src, &mut p);
    assert!(t1.contains_key("LIMIT"));
    assert!(!t1.contains_key("LIMIT2"));
    assert_eq!(include_closure(&main_c, src).0.len(), 1);

    // Edit the header: new macro + a new nested include. The consuming
    // file's OWN include set is unchanged, so both caches keep serving
    // the old world by design...
    std::fs::write(&hdr2, "#define NESTED 1\n").unwrap();
    std::fs::write(&hdr, "#include \"hdr2.h\"\n#define LIMIT 5\n#define LIMIT2 7\n").unwrap();
    let t2 = included_macros(&main_c, src, &mut p);
    assert!(!t2.contains_key("LIMIT2"), "tier-1 hit: content edit invisible until evicted");
    assert_eq!(include_closure(&main_c, src).0.len(), 1, "closure cache: same");

    // ...until the invalidation seam evicts the consumer.
    let evict: std::collections::HashSet<std::path::PathBuf> =
        [main_c.canonicalize().unwrap(), hdr.canonicalize().unwrap()]
            .into_iter()
            .collect();
    evict_analysis_caches(&evict);
    let t3 = included_macros(&main_c, src, &mut p);
    assert!(t3.contains_key("LIMIT2"), "fresh gather sees the header edit");
    assert!(t3.contains_key("NESTED"), "and the new nested include");
    assert_eq!(include_closure(&main_c, src).0.len(), 2, "closure re-walked");
    let _ = std::fs::remove_dir_all(&dir);
}

/// M5 lock: a toolchain-predefined macro (`__GNUC__`) is a known-ON knob in
/// the reachability config — the arm it guards ranks ACTIVE, not
/// "unreachable: __GNUC__ undefined". `seed_predefined` is the one seeding
/// point navigation (`symbols::ranked_macro_variants`) and build-side variant
/// selection (`known_config`) share.
#[test]
fn predefined_macros_seed_reachability_config() {
    use crate::cpp_macro_model::{classify, KnownConfig, Reachability};
    let guards = vec!["defined(__GNUC__)".to_string()];
    // Unseeded: __GNUC__ is defined nowhere in the closure → provably absent.
    let bare = KnownConfig::new(Default::default(), Default::default());
    assert!(matches!(classify(&guards, &bare), Reachability::Unreachable { .. }));
    // Seeded with the toolchain's predefined set: the arm is ACTIVE.
    let mut defined = std::collections::HashSet::new();
    let mut universe = std::collections::HashSet::new();
    crate::cpp_reparse::seed_predefined(
        &mut defined,
        &mut universe,
        &[("__GNUC__".to_string(), "13".to_string())],
    );
    let seeded = KnownConfig::new(defined, universe);
    assert_eq!(classify(&guards, &seeded), Reachability::Active);
}





// --- Context-free-safe expansion verdict (docs/prompt-macro-salvage-scaling.md)
//
// A macro whose expansion is context-INDEPENDENTLY safe — an empty/whitespace
// body, i.e. a pure byte-DELETION like perl5's `pTHX_`/`aTHX_` under a
// non-multiplicity config — must never be stranded when a *sibling* macro in
// the same conditional region forces the wide→re-excluded fallback (which
// otherwise drops every conditional-region-body expansion wholesale). This is
// the exact op.c:633 dark-receiver: `Perl_op_refcnt_inc(pTHX_ OP *o)` sits in
// `#ifdef PERL_DEBUG_READONLY_OPS`, and a broken sibling macro forced the
// fallback → `pTHX_` stayed literal → `o` typed `pTHX_`, not `OP`.

fn macro_table(defs: &[(&str, Option<&[&str]>, &str)]) -> PreExpandedExternal {
    let mut ext = std::collections::BTreeMap::new();
    for (n, params, body) in defs {
        ext.insert(
            n.to_string(),
            Macro {
                params: params.map(|ps| ps.iter().map(|s| s.to_string()).collect()),
                body: body.to_string(),
                guards: Vec::new(),
                def_line: 0,
            },
        );
    }
    PreExpandedExternal::from_raw(std::sync::Arc::new(ext))
}

#[test]
fn context_free_safe_macro_survives_conditional_region_drop() {
    let mut p = cpp_parser();
    // `pTHX_` empty (safe deletion); `EVIL` breaks the parse when expanded, so
    // the wide expansion raises damage and the fallback re-excludes the region.
    let external = macro_table(&[("pTHX_", None, ""), ("EVIL", None, ")}} garbage {{(")]);
    let src = "struct OP { int x; };\n#ifdef FEATURE\nvoid refcnt(pTHX_ struct OP *o) { o->x = 1; }\nint z = EVIL;\n#endif\nvoid other(int y) { y++; }\n";

    let tree = parse(&mut p, src);
    let before = crate::cpp_reparse::parse_damage(tree.root_node());
    let (wide, _) = preprocess_with(&tree, src, &external);
    let wide_dmg = crate::cpp_reparse::parse_damage(parse(&mut p, &wide).root_node());
    assert!(wide_dmg > before, "the sibling `EVIL` must force the fallback (wide raises damage)");

    let (rw, _map, _rec) = preprocess_validated_with(&mut p, src, &external);
    // `pTHX_` is deleted even though it lives in the dropped conditional region…
    assert!(!rw.contains("pTHX_"), "context-free-safe `pTHX_` expanded despite the region drop:\n{rw}");
    assert!(
        rw.contains("refcnt( struct OP *o)") || rw.contains("refcnt(struct OP *o)"),
        "the signature parses `o` as `struct OP *`:\n{rw}"
    );
    // …while the position-DEPENDENT `EVIL` stays excluded (left literal), so the
    // exemption did not over-broaden past the provably-safe class.
    assert!(rw.contains("EVIL"), "non-safe `EVIL` must stay excluded in the fallback:\n{rw}");
    // The damage-never-rises invariant: the shipped rewrite validates.
    assert!(
        crate::cpp_reparse::parse_damage(parse(&mut p, &rw).root_node()) <= before,
        "salvage/exemption never ships a rewrite above baseline damage"
    );
}

#[test]
fn context_free_safe_macro_still_barred_from_hard_spans() {
    let mut p = cpp_parser();
    // The exemption relaxes only the conditional-region-body exclusion — a
    // safe-macro token inside a string/comment must still never be touched.
    let external = macro_table(&[("pTHX_", None, ""), ("EVIL", None, ")}} garbage {{(")]);
    let src = "struct OP { int x; };\n#ifdef FEATURE\nconst char *s = \"pTHX_ in a string\"; // pTHX_ in a comment\nint z = EVIL;\n#endif\n";
    let (rw, _m, _r) = preprocess_validated_with(&mut p, src, &external);
    assert!(rw.contains("\"pTHX_ in a string\""), "string literal bytes untouched:\n{rw}");
    assert!(rw.contains("// pTHX_ in a comment"), "comment bytes untouched:\n{rw}");
}

#[test]
fn salvage_keeps_context_free_safe_groups_without_a_probe() {
    // Direct salvage: a context-free-safe deletion (empty replacement) is kept
    // unconditionally and never enters the budgeted bisection (doc fix #1), so
    // the whole budget flows to the genuinely-ambiguous name.
    let mut p = cpp_parser();
    let src = "int a; int b; int c;\n";
    let base_tree = parse(&mut p, src);
    let base = (
        crate::cpp_reparse::parse_damage(base_tree.root_node()),
        structure_count(base_tree.root_node()),
    );
    // `a` (4..5) → deleted (safe); `b` (11..12) → broken expansion (ambiguous).
    let splices = vec![
        Splice { start: 4, end: 5, replacement: String::new(), name: "SAFE".into() },
        Splice { start: 11, end: 12, replacement: ")}}(".into(), name: "BAD".into() },
    ];
    let mut budget = SALVAGE_PARSE_BUDGET;
    let start_budget = budget;
    let kept = salvage_splices(&mut p, src, &splices, base, &mut budget);
    assert!(kept.iter().any(|s| s.name == "SAFE"), "safe deletion kept: {kept:?}");
    // The safe group cost zero probes: only the single ambiguous group could
    // have been probed (≤ 2 probes: its expansion + its blank retry).
    assert!(start_budget - budget <= 2, "safe group must not consume budget (used {})", start_budget - budget);
}

#[test]
fn object_macro_does_not_expand_uses_before_its_define() {
    // C preprocessor position semantics: `#define Simplify DontCallSimplify`
    // affects only text AT/AFTER the directive. The out-of-line def and the
    // call ABOVE it must keep the real name (re2 simplify.cc shape).
    let mut p = cpp_parser();
    let src = "\
Regexp* Regexp::Simplify() {\n\
  return this;\n\
}\n\
#define Simplify DontCallSimplify\n\
Regexp* Foo() { return Simplify(); }\n";
    let tree = parse(&mut p, src);
    let (rewritten, _map) = preprocess_with(&tree, src, &PreExpandedExternal::empty());
    // The pre-directive def keeps `Simplify`; the post-directive use expands.
    assert!(
        rewritten.contains("Regexp::Simplify()"),
        "pre-directive def keeps real name: {rewritten}"
    );
    let after_directive = rewritten.split("#define").nth(1).unwrap_or("");
    assert!(
        after_directive.contains("DontCallSimplify"),
        "post-directive use still expands: {rewritten}"
    );
    // And the def is never renamed away.
    assert!(
        !rewritten.starts_with("Regexp* Regexp::DontCallSimplify"),
        "pre-directive def NOT expanded: {rewritten}"
    );
}

#[test]
fn macro_still_expands_uses_after_its_define() {
    // The position guard must not suppress the normal case: a use after the
    // `#define` still expands.
    let mut p = cpp_parser();
    let src = "#define FOO bar\nint FOO;\n";
    let tree = parse(&mut p, src);
    let (rewritten, _map) = preprocess_with(&tree, src, &PreExpandedExternal::empty());
    assert!(rewritten.contains("int bar;"), "post-directive use expands: {rewritten}");
}

#[test]
fn include_guard_names_are_recognized() {
    let mut p = cpp_parser();
    let src = "\
#ifndef LEVELDB_FOO_H_\n\
#define LEVELDB_FOO_H_\n\
int real_symbol;\n\
#endif\n";
    let guards = crate::cpp_reparse::collect_include_guard_names(&mut p, src);
    assert!(guards.contains("LEVELDB_FOO_H_"), "include guard recognized: {guards:?}");
}

#[test]
fn valued_and_functional_ifndef_defines_are_not_guards() {
    // A bodyless `#define X` inside `#ifndef X` is the guard; a valued or
    // function-like conditional definition is a real entity, not plumbing.
    let mut p = cpp_parser();
    let src = "\
#ifndef MAXVAL\n\
#define MAXVAL 100\n\
#endif\n\
#ifndef MIN\n\
#define MIN(a,b) ((a)<(b)?(a):(b))\n\
#endif\n";
    let guards = crate::cpp_reparse::collect_include_guard_names(&mut p, src);
    assert!(!guards.contains("MAXVAL"), "valued define is not a guard: {guards:?}");
    assert!(!guards.contains("MIN"), "function-like define is not a guard: {guards:?}");
}
