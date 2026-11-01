//! Differential harness: the query-skeleton vs the real builder, over
//! the repo's test corpus (+ a substrate sample when present). The
//! numbers this prints ARE the spike's findings — run with
//! `cargo test query_skeleton -- --nocapture`.

use super::*;
use crate::build::packs::*;
use std::collections::HashSet;
use std::path::PathBuf;

fn corpus_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_files")];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if matches!(p.extension().and_then(|s| s.to_str()), Some("pl" | "pm")) {
                out.push(p);
            }
        }
    }
    // Substrate sample: realism check against real CPAN code. The
    // snapshot lives in the main checkout; absent (CI) we just skip.
    let substrate = PathBuf::from("/home/veesh/personal/perl-tree-sitter-lsp/gold-corpus/local/lib/perl5");
    if substrate.is_dir() {
        let mut subs: Vec<PathBuf> = Vec::new();
        let mut stack = vec![substrate];
        while let Some(dir) = stack.pop() {
            if subs.len() >= 60 {
                break;
            }
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().and_then(|s| s.to_str()) == Some("pm")
                    && std::fs::metadata(&p).map(|m| m.len() < 100_000).unwrap_or(false)
                {
                    subs.push(p);
                    if subs.len() >= 60 {
                        break;
                    }
                }
            }
        }
        out.extend(subs);
    }
    out.sort();
    out
}

#[derive(Default)]
struct Tally {
    builder: usize,
    matched: usize,
    misses: Vec<String>,
}

impl Tally {
    fn recall(&self) -> f64 {
        if self.builder == 0 {
            return 1.0;
        }
        self.matched as f64 / self.builder as f64
    }
}

#[test]
fn query_skeleton_differential_report() {
    use crate::model::file_analysis::SymKind;
    let pack = perl_pack();
    let mut parser = crate::build::builder::create_parser();

    // builder kind → skeleton kinds that may legitimately answer for it
    let kind_map: &[(SymKind, &[DefCap], &str)] = &[
        (SymKind::Package, &[DefCap::Package], "package"),
        (SymKind::Class, &[DefCap::Package, DefCap::Class], "class"),
        (SymKind::Sub, &[DefCap::Sub, DefCap::Method, DefCap::Anon, DefCap::Constant], "sub"),
        (SymKind::Method, &[DefCap::Sub, DefCap::Method, DefCap::Anon], "method"),
        (SymKind::Variable, &[DefCap::Var], "variable"),
    ];

    let mut tallies: std::collections::HashMap<&str, Tally> = Default::default();
    let mut synthesized_unmatched: std::collections::HashMap<String, usize> = Default::default();
    let mut ref_tally = Tally::default();
    let mut skel_ref_extras = 0usize;
    let mut skel_ref_total = 0usize;
    let mut files = 0usize;
    let mut query_errors = 0usize;

    for path in corpus_files() {
        let Ok(source) = std::fs::read_to_string(&path) else { continue };
        let Some(tree) = parser.parse(&source, None) else { continue };
        let fa = crate::build::builder::build(&tree, source.as_bytes());
        let skel = match extract(&tree, source.as_bytes(), &pack) {
            Ok(s) => s,
            Err(e) => {
                query_errors += 1;
                eprintln!("query error on {}: {e}", path.display());
                continue;
            }
        };
        files += 1;

        let skel_defs: HashSet<(DefCap, usize, usize)> = skel
            .symbols
            .iter()
            .map(|s| (s.kind, s.name_start.row, s.name_start.column))
            .collect();

        for (bkind, skel_kinds, label) in kind_map {
            let t = tallies.entry(label).or_default();
            for sym in fa.symbols().iter().filter(|s| s.kind == *bkind) {
                t.builder += 1;
                let pos = (sym.selection_span.start.row, sym.selection_span.start.column);
                // Exact column: the builder anchors a variable's
                // selection on the sigil, and the skeleton captures the
                // whole sigiled node — both start at the sigil, so no
                // anchor fudge is needed (an earlier `col - 1` tolerance
                // papered over a mis-stated anchor; see skeleton.scm).
                let hit = skel_kinds
                    .iter()
                    .any(|sk| skel_defs.contains(&(*sk, pos.0, pos.1)));
                if hit {
                    t.matched += 1;
                } else {
                    // SEMANTIC synthesis can never come from a def
                    // pattern — bucket it by the recorded facts so the
                    // recall number speaks only about syntactic defs:
                    // plugin namespace, requires markers, and the
                    // has-accessor projection families.
                    let plugin_or_marker = !matches!(sym.namespace, crate::model::file_analysis::Namespace::Language)
                        || sym.flags.contains(crate::model::file_analysis::SymbolFlags::CONTRACT)
                        || fa.attr_projections.iter().any(|pr| {
                            pr.attr == sym.name
                                && Some(pr.class.as_str()) == sym.package.as_deref()
                        })
                        || sym.span.start == sym.span.end;
                    // Three-way bucket. A SYNTACTIC def in Perl is
                    // exactly `sub NAME` / `method NAME`; a name token
                    // present at the site without that keyword is
                    // syntax a PATTERN can't pair (use-constant tables,
                    // codegen loops); no name at the site at all is
                    // semantic synthesis.
                    let line = source.lines().nth(pos.0).unwrap_or("");
                    let before = &line[..pos.1.min(line.len())];
                    // the keyword probe only means something for subs/
                    // methods; other kinds are syntactic by default
                    let keyword_def = !matches!(bkind, SymKind::Sub | SymKind::Method)
                        || before.trim_end().ends_with("sub")
                        || before.trim_end().ends_with("method");
                    if plugin_or_marker || !keyword_def {
                        *synthesized_unmatched.entry(sym.name.clone()).or_default() += 1;
                    } else if t.misses.len() < 12 {
                        t.misses.push(format!(
                            "{}:{}:{} {} ({:?})",
                            path.file_name().unwrap().to_string_lossy(),
                            pos.0 + 1,
                            pos.1,
                            sym.name,
                            bkind,
                        ));
                    }
                }
            }
        }

        // refs: positions of call/method/var reads the builder records
        use crate::model::file_analysis::RefKind;
        let skel_ref_pos: HashSet<(usize, usize)> =
            skel.refs.iter().map(|r| (r.start.row, r.start.column)).collect();
        skel_ref_total += skel.refs.len();
        let mut builder_ref_pos: HashSet<(usize, usize)> = HashSet::new();
        for r in fa.refs() {
            if matches!(
                r.kind,
                RefKind::FunctionCall { .. } | RefKind::MethodCall { .. } | RefKind::FieldAccess { .. } | RefKind::Variable
            ) {
                builder_ref_pos.insert((r.span.start.row, r.span.start.column));
            }
        }
        for pos in &builder_ref_pos {
            ref_tally.builder += 1;
            // skeleton var refs anchor on the varname (post-sigil): a
            // builder `$x` ref at col c matches a skeleton ref at c+1.
            if skel_ref_pos.contains(pos) || skel_ref_pos.contains(&(pos.0, pos.1 + 1)) {
                ref_tally.matched += 1;
            }
        }
        skel_ref_extras += skel
            .refs
            .iter()
            .filter(|r| {
                !builder_ref_pos.contains(&(r.start.row, r.start.column))
                    && !(r.start.column > 0
                        && builder_ref_pos.contains(&(r.start.row, r.start.column - 1)))
            })
            .count();
    }

    println!("\n===== query-skeleton differential ({files} files) =====");
    for (_, _, label) in kind_map {
        let t = &tallies[label];
        println!(
            "  {:<9} builder {:>5}  matched {:>5}  recall {:>6.1}%",
            label,
            t.builder,
            t.matched,
            t.recall() * 100.0,
        );
        for m in &t.misses {
            println!("      miss: {m}");
        }
    }
    let synth_total: usize = synthesized_unmatched.values().sum();
    println!("  zero-syntax (synthesized) builder symbols, unreachable by ANY pattern: {synth_total}");
    println!(
        "  refs      builder {:>5}  matched {:>5}  recall {:>6.1}%  (skeleton extras: {} of {})",
        ref_tally.builder,
        ref_tally.matched,
        ref_tally.recall() * 100.0,
        skel_ref_extras,
        skel_ref_total,
    );
    assert_eq!(query_errors, 0, "query failed to compile/run somewhere");

    // Spike floor: the purely syntactic skeleton should all but
    // reproduce the builder's package/sub rows. These asserts make the
    // claim falsifiable rather than narrative.
    assert!(tallies["package"].recall() >= 0.95, "package recall");
    assert!(tallies["sub"].recall() >= 0.90, "sub recall");
}

#[test]
fn field_queryability_must_be_probed_per_node() {
    // The story behind skeleton.scm's variable-def pattern, with numbers.
    // Every field on `variable_declaration` resolves to a NAMED node in the
    // query engine: the single `variable:` to its var node, and the
    // paren-list `variables:` to one var node per slot (the parens and
    // commas carry no field). A `.scm` may lean on either; the field-less
    // `(_ (varname))` discriminator below stays the simplest single form
    // that covers both spellings.
    use tree_sitter::{Query, QueryCursor, StreamingIterator};
    let src = "my $x = 1;\nmy ($a, $b) = @_;\n";
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(src, None).unwrap();
    let count = |pat: &str| -> usize {
        let q = Query::new(&tree.language(), pat).unwrap();
        let mut c = QueryCursor::new();
        let mut ms = c.matches(&q, tree.root_node(), src.as_bytes());
        let mut n = 0;
        while let Some(m) = ms.next() {
            n += m.captures.len();
        }
        n
    };
    // single field: resolves to the named var node.
    assert_eq!(count("(variable_declaration variable: (scalar (varname) @v))"), 1);
    // paren-list field: one named var node per slot, no punctuation.
    assert_eq!(count("(variable_declaration variables: (scalar) @v)"), 2);
    assert_eq!(count("(variable_declaration variables: (_) @v)"), 2);
    assert_eq!(count("(variable_declaration variables: _ @v)"), 2);
    // The field-less discriminator covers both spellings in one pattern.
    assert_eq!(count("(variable_declaration variable: (_ (varname) @v))"), 1);
    assert_eq!(count("(variable_declaration variables: (_ (varname) @v))"), 2);
    assert_eq!(count("(variable_declaration (_ (varname) @v))"), 3);
}

// ---- spike 2: the witness bag fed from captures alone ----

#[test]
fn walker_free_file_analysis_answers_type_queries() {
    // No builder anywhere in this test: captures → witnesses → a real
    // FileAnalysis → the production reducer registry answers.
    let src = "my $x = \"hello\";\nmy $n = 42;\nmy $h = {};\nmy $y = $x;\n";
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(src, None).unwrap();
    let skel = extract(&tree, src.as_bytes(), &perl_pack()).unwrap();
    let fa = skel.into_file_analysis();

    let end = tree_sitter::Point { row: 4, column: 0 };
    use crate::model::file_analysis::InferredType;
    assert_eq!(fa.inferred_type_via_bag("$x", end), Some(InferredType::String));
    assert_eq!(fa.inferred_type_via_bag("$n", end), Some(InferredType::Numeric));
    assert_eq!(fa.inferred_type_via_bag("$h", end), Some(InferredType::HashRef));
    // THE edge chase: $y → Variable($x) → Expr(literal) — three hops
    // through the production registry, zero walker code.
    assert_eq!(fa.inferred_type_via_bag("$y", end), Some(InferredType::String));
}

#[test]
fn python_pack_same_driver_same_engine() {
    // The cross-language existence proof: a different grammar, a
    // ~40-line query pack, the SAME driver and the SAME engine.
    let src = "\
import os

class Greeter:
    def greet(self, name):
        msg = \"hi\"
        return msg

x = \"hello\"
n: int = compute()
y = x
";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(src, None).unwrap();
    let skel = extract(&tree, src.as_bytes(), &python_pack()).unwrap();

    // outline: class + method + module-level vars
    let names: Vec<(DefCap, String)> = skel
        .symbols
        .iter()
        .map(|s| (s.kind, s.name.clone()))
        .collect();
    assert!(names.contains(&(DefCap::Class, "Greeter".into())), "{names:?}");
    assert!(names.contains(&(DefCap::Sub, "greet".into())), "{names:?}");
    assert!(names.contains(&(DefCap::Assigned, "x".into())), "{names:?}");
    assert_eq!(skel.imports, vec!["os"]);

    let fa = skel.into_file_analysis();
    let end = tree_sitter::Point { row: 10, column: 0 };
    use crate::model::file_analysis::InferredType;
    // literal
    assert_eq!(fa.inferred_type_via_bag("x", end), Some(InferredType::String));
    // annotation — ring 3 in the tree, emitted by the pack predicate
    assert_eq!(fa.inferred_type_via_bag("n", end), Some(InferredType::Numeric));
    // edge chase across variables
    assert_eq!(fa.inferred_type_via_bag("y", end), Some(InferredType::String));
    // scoped local inside the method body
    let inside = tree_sitter::Point { row: 5, column: 8 };
    assert_eq!(fa.inferred_type_via_bag("msg", inside), Some(InferredType::String));
}

// ---- spike 3: cross-file through the production engine ----
//
// `resolve_imports_with_pack` lives HERE because the layering test
// (correctly) rejected it from query_extract.rs: import resolution is
// Index-layer work — in a real implementation it sits beside
// module_resolver (Index imports Build, never the reverse). The spike
// keeps it in the test suite, which is exempt by design.

/// Resolve a file's imports into a `ModuleIndex` — the GENERIC
/// cross-file loop. The only per-language inputs are the pack's
/// `module_paths` predicate and a parser factory; registration rides
/// `insert_cache`, which feeds the production reverse indexes
/// (`ModuleEdgeIndexes`), so `modules_with_symbol` /
/// `module_declaring_method_in_package` / the MRO walk all work
/// unchanged on pack-built modules.
fn resolve_imports_with_pack(
    imports: &[String],
    root: &std::path::Path,
    pack: &LangPack,
    mk_parser: &mut dyn FnMut() -> tree_sitter::Parser,
    idx: &crate::index::module_index::ModuleIndex,
) -> Result<(), String> {
    for module in imports {
        for rel in (pack.module_paths)(module) {
            let path = root.join(&rel);
            let Ok(source) = std::fs::read_to_string(&path) else { continue };
            let mut parser = mk_parser();
            let Some(tree) = parser.parse(&source, None) else { continue };
            let fa = extract(&tree, source.as_bytes(), pack)?.into_file_analysis();
            idx.insert_cache(
                module,
                Some(std::sync::Arc::new(crate::index::module_index::CachedModule::new(
                    path,
                    std::sync::Arc::new(fa),
                ))),
            );
            break;
        }
    }
    Ok(())
}



fn python_parser() -> tree_sitter::Parser {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_python::LANGUAGE.into()).unwrap();
    p
}

fn python_fa(src: &str) -> (crate::model::file_analysis::FileAnalysis, Vec<String>) {
    let mut parser = python_parser();
    let tree = parser.parse(src, None).unwrap();
    let skel = extract(&tree, src.as_bytes(), &python_pack()).unwrap();
    let imports = skel.imports.clone();
    (skel.into_file_analysis(), imports)
}

#[test]
fn a_python_receiver_is_a_flag_the_document_mints() {
    use crate::model::file_analysis::{OutlineSymbol, SymbolFlags};
    // The leading `self` / `cls` is the receiver; a later parameter that
    // happens to share the spelling is an ordinary parameter.
    let src = "\
class Greeter:
    def greet(self, name):
        return name

    @classmethod
    def make(cls, self):
        return cls
";
    let (fa, _) = python_fa(src);
    let receivers: Vec<(String, usize)> = fa
        .symbols()
        .iter()
        .filter(|s| s.flags.contains(SymbolFlags::RECEIVER))
        .map(|s| (s.name.clone(), s.selection_span.start.row))
        .collect();
    assert_eq!(receivers, vec![("self".to_string(), 1), ("cls".to_string(), 5)]);

    fn names(syms: &[OutlineSymbol], out: &mut Vec<String>) {
        for s in syms {
            out.push(s.name.clone());
            names(&s.children, out);
        }
    }
    let mut outline = Vec::new();
    names(&fa.document_symbols(), &mut outline);
    assert!(!outline.iter().any(|n| n == "cls"), "{outline:?}");
}

#[test]
fn python_cross_file_function_refs_through_refs_to() {
    // Two pack-built FileAnalyses in the production OpenDocs; the
    // production refs_to walks them — declaration in a.py, call in
    // b.py — with zero Perl and zero engine edits.
    let (fa_a, _) = python_fa("def helper(x):\n    return x\n");
    let (fa_b, _) = python_fa("from a import helper\n\nz = helper(1)\n");

    let target = crate::index::resolve::TargetRef::new(
        "helper".into(),
        crate::index::resolve::TargetKind::Sub { package: None },
        &fa_b,
    );
    let store = crate::index::open_docs::OpenDocs::new();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let pa = std::path::PathBuf::from("/fake/py/a.py");
    let pb = std::path::PathBuf::from("/fake/py/b.py");
    idx.insert_workspace_for_test(pa.clone(), fa_a);
    idx.insert_workspace_for_test(pb.clone(), fa_b);

    let locs = crate::index::resolve::refs_to(&store, Some(&idx), &target, crate::index::resolve::RoleMask::EDITABLE);
    let by_file: Vec<(String, crate::model::file_analysis::AccessKind)> = locs
        .iter()
        .map(|l| {
            let f = match &l.key {
                crate::index::open_docs::FileKey::Path(p) => {
                    p.file_name().unwrap().to_string_lossy().to_string()
                }
                crate::index::open_docs::FileKey::Url(u) => u.to_string(),
            };
            (f, l.access)
        })
        .collect();
    assert!(
        by_file.contains(&("a.py".into(), crate::model::file_analysis::AccessKind::Declaration)),
        "expected the def in a.py, got {by_file:?}",
    );
    assert!(
        by_file.contains(&("b.py".into(), crate::model::file_analysis::AccessKind::Read)),
        "expected the call in b.py, got {by_file:?}",
    );
}

#[test]
fn python_cross_file_method_dispatch_through_mro_walk() {
    // The full chain: pack resolver (module_paths predicate) registers
    // a.py in the production ModuleIndex; resolve_method_in_ancestors finds
    // greet on Greeter CROSS-FILE through the production index arms. The
    // constructor convention that once typed `g` from the callee's name
    // case is retired — a call's value is the callee's own resolution
    // (`docs/adr/macro-handling.md`). `Greeter` is a cross-file class NOT
    // registered under its own name (its module is `a`), so it resolves to
    // no local/cross-file symbol here and `g` stays honestly untyped rather
    // than guessed. The cross-file constructor-typing gap is ledgered in
    // `docs/PARKED.md`; the dispatch below is keyed on the class name
    // directly and is unaffected.
    let dir = std::env::temp_dir().join(format!("qx-spike-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("a.py"),
        "class Greeter:\n    def greet(self, name):\n        return name\n",
    )
    .unwrap();

    let src_b = "from a import Greeter\n\ng = Greeter()\nr = g.greet(\"x\")\n";
    let (fa_b, imports_b) = python_fa(src_b);

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    resolve_imports_with_pack(&imports_b, &dir, &python_pack(), &mut python_parser, &idx)
        .unwrap();

    let end = tree_sitter::Point { row: 4, column: 0 };
    assert_eq!(
        fa_b.inferred_type_via_bag("g", end),
        None,
        "no name-case guess: `g` is untyped until the callee resolves to a known type",
    );
    match fa_b.resolve_method_in_ancestors("Greeter", "greet", Some(&idx)) {
        Some(crate::model::file_analysis::MethodResolution::CrossFile { class, def_module, .. }) => {
            assert_eq!(class, "Greeter");
            assert_eq!(def_module.as_deref(), Some("a"));
        }
        other => panic!("expected cross-file resolution of greet, got {other:?}"),
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dbg_operator_patterns() {
    use tree_sitter::{Query, QueryCursor, StreamingIterator};
    let src = "my $y = $x + 1;\nmy $s = $a . \"b\";\n";
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(src, None).unwrap();
    for pat in [
        r#"(binary_expression left: (scalar) @v "+")"#,
        r#"(binary_expression left: (scalar) @v operator: "+")"#,
        r#"(binary_expression left: (scalar) @v ".")"#,
    ] {
        match Query::new(&tree.language(), pat) {
            Ok(q) => {
                let mut c = QueryCursor::new();
                let mut ms = c.matches(&q, tree.root_node(), src.as_bytes());
                let mut n = 0;
                while let Some(m) = ms.next() { n += m.captures.len(); }
                println!("PAT {pat} -> {n}");
            }
            Err(e) => println!("PAT {pat} -> ERR {e}"),
        }
    }
}

#[test]
fn operator_evidence_types_through_usage() {
    // The operator-orientation answer: $x's initializer is unknowable,
    // but `$x + 1` observes it numeric and `$s . "!"` observes $s
    // stringy — usage-site evidence through the production fold.
    let src = "my $x = f();\nmy $s = g();\nmy $y = $x + 1;\nmy $t = $s . \"!\";\n";
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(src, None).unwrap();
    let fa = extract(&tree, src.as_bytes(), &perl_pack())
        .unwrap()
        .into_file_analysis();
    let end = tree_sitter::Point { row: 4, column: 0 };
    use crate::model::file_analysis::InferredType;
    assert_eq!(fa.inferred_type_via_bag("$x", end), Some(InferredType::Numeric));
    assert_eq!(fa.inferred_type_via_bag("$s", end), Some(InferredType::String));
}

/// A use inside a conditionally-run arm observes its operand within the
/// arm only, as the walker's does.
#[test]
fn operator_evidence_holds_within_its_arm() {
    let src = "my $x = f();\nmy $c = g();\nmy $y = $c ? $x + 1 : 0;\nmy $z = 1;\n";
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(src, None).unwrap();
    let fa = extract(&tree, src.as_bytes(), &perl_pack())
        .unwrap()
        .into_file_analysis();
    use crate::model::file_analysis::InferredType;
    let at = |row, column| fa.inferred_type_via_bag("$x", tree_sitter::Point { row, column });
    assert_eq!(at(2, 19), Some(InferredType::Numeric), "inside the arm");
    assert_eq!(at(4, 0), None, "past the arm");
}

/// A use's evidence ends at the subject's next rebind, the same edge-driven
/// cutoff a guard's narrowing takes.
#[test]
fn operator_evidence_ends_at_a_rebind() {
    let src = "my $x = f();\nmy $y = $x + 1;\n$x = g();\nmy $z = 1;\n";
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(src, None).unwrap();
    let fa = extract(&tree, src.as_bytes(), &perl_pack())
        .unwrap()
        .into_file_analysis();
    use crate::model::witnesses::{WitnessAttachment, WitnessPayload, WitnessSource};
    let obs: Vec<_> = fa
        .witnesses
        .all()
        .iter()
        .filter(|w| matches!(&w.attachment, WitnessAttachment::Variable { name, .. } if name == "$x"))
        .filter(|w| matches!(&w.source, WitnessSource::Builder(t) if t == "skeleton-obs"))
        .filter(|w| matches!(w.payload, WitnessPayload::Observation(_)))
        .collect();
    assert_eq!(obs.len(), 1);
    // A write lands at its assignment's end, once the right side is read.
    assert_eq!(obs[0].span.end, tree_sitter::Point { row: 2, column: 8 }, "cut where `$x = g()` lands");
}

#[test]
fn python_workspace_rename_for_free() {
    // The exact LSP rename path — resolve_symbol at the cursor, then
    // refs_to, then text edits — over two pack-built Python files.
    let src_a = "def helper(x):\n    return x\n";
    let src_b = "from a import helper\n\nz = helper(1)\n";
    let (fa_a, _) = python_fa(src_a);
    let (fa_b, _) = python_fa(src_b);

    let store = crate::index::open_docs::OpenDocs::new();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let pa = std::path::PathBuf::from("/fake/py/a.py");
    let pb = std::path::PathBuf::from("/fake/py/b.py");
    idx.insert_workspace_for_test(pa.clone(), fa_a);
    idx.insert_workspace_for_test(pb.clone(), fa_b);

    // Cursor on the CALL in b.py (line 2, inside "helper").
    let fa_b_view = idx.workspace_file_for_test(&pb).unwrap();
    let point = tree_sitter::Point { row: 2, column: 6 };
    let resolved = crate::index::resolve::resolve_symbol(&fa_b_view, point, None);
    let target = match resolved {
        Some(crate::index::resolve::ResolvedTarget::Target(t)) => t,
        other => panic!("expected a walkable target at the call, got {other:?}"),
    };
    assert_eq!(target.name, "helper");

    let locs = crate::index::resolve::refs_to(&store, Some(&idx), &target, crate::index::resolve::RoleMask::EDITABLE);
    assert_eq!(locs.len(), 3, "decl + import spelling + call, got {locs:?}");

    // Apply the edits the way backend::rename does: replace each span
    // with the new name, per file, right-to-left.
    let mut texts: std::collections::HashMap<std::path::PathBuf, String> =
        [(pa.clone(), src_a.to_string()), (pb.clone(), src_b.to_string())].into();
    for loc in &locs {
        let crate::index::open_docs::FileKey::Path(p) = &loc.key else { panic!() };
        let text = texts.get_mut(p).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let line = lines[loc.span.start.row];
        let mut new_line = line.to_string();
        new_line.replace_range(loc.span.start.column..loc.span.end.column, "fetch_all");
        let mut new_lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        new_lines[loc.span.start.row] = new_line;
        *text = new_lines.join("\n") + "\n";
    }
    assert_eq!(texts[&pa], "def fetch_all(x):\n    return x\n");
    assert_eq!(texts[&pb], "from a import fetch_all\n\nz = fetch_all(1)\n");
}


#[test]
fn dbg_r_cst() {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_r::LANGUAGE.into()).unwrap();
    let src = "library(dplyr)\nsource(\"util.R\")\nf <- function(x) {\n  y <- x + 1\n  y\n}\ndf <- data.frame(age = c(30), name = c(\"ada\"))\nm <- df$age\nprint.myclass <- function(obj) obj\n";
    let tree = p.parse(src, None).unwrap();
    println!("{}", tree.root_node().to_sexp());
}

// ---- spike 4: the R pack ----

fn r_parser() -> tree_sitter::Parser {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_r::LANGUAGE.into()).unwrap();
    p
}

fn r_fa(src: &str) -> (crate::model::file_analysis::FileAnalysis, Vec<String>, SkeletonAnalysis) {
    let mut parser = r_parser();
    let tree = parser.parse(src, None).unwrap();
    let skel = extract(&tree, src.as_bytes(), &r_pack()).unwrap();
    let imports = skel.imports.clone();
    let symbols = SkeletonAnalysis {
        symbols: skel.symbols.clone(),
        ..Default::default()
    };
    (skel.into_file_analysis(), imports, symbols)
}

#[test]
fn r_outline_imports_and_s3_names() {
    let src = "library(dplyr)\nsource(\"util.R\")\n\nadd_one <- function(x) {\n  y <- x + 1\n  y\n}\n\nprint.myclass <- function(obj) obj\n";
    let (_fa, imports, skel) = r_fa(src);
    assert_eq!(imports, vec!["dplyr", "util.R"]);
    let subs: Vec<&str> = skel
        .symbols
        .iter()
        .filter(|s| s.kind == DefCap::Sub)
        .map(|s| s.name.as_str())
        .collect();
    // S3 method names fall out of the def pattern verbatim — the
    // convention layer (print.myclass IS print on myclass) is a later
    // pack predicate, but the outline is already right.
    assert_eq!(subs, vec!["add_one", "print.myclass"]);
    // params + locals are variables, the function names are NOT doubled
    let vars: Vec<&str> = skel
        .symbols
        .iter()
        .filter(|s| s.kind.sym_kind() == crate::model::file_analysis::SymKind::Variable)
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(vars, vec!["x", "y", "obj"]);
}

#[test]
fn r_data_frame_columns_are_static_keys() {
    // THE feature no R tooling has statically: df's columns as part of
    // its TYPE, from the data.frame literal, through the production
    // witness engine.
    let src = "df <- data.frame(age = c(30, 41), name = c(\"ada\", \"grace\"))\nlater <- 1\n";
    let (fa, _, _) = r_fa(src);
    let end = tree_sitter::Point { row: 1, column: 0 };
    use crate::model::file_analysis::InferredType;
    match fa.inferred_type_via_bag("df", end) {
        Some(InferredType::HashWithKeys { keys, open }) => {
            let names: Vec<&str> = keys.iter().map(|(k, _)| k.as_str()).collect();
            assert_eq!(names, vec!["age", "name"]);
            assert!(!open);
        }
        other => panic!("expected the column shape, got {other:?}"),
    }
}

/// Which callees construct a keyed value and which import is the
/// document's word: a named argument to any other call is no key, and a
/// call of any other name imports nothing.
#[test]
fn r_only_the_named_constructors_and_importers_count() {
    let src = "library(\"dplyr\")\nhelper(pkg)\nopts <- configure(width = 3)\nl <- list(width = 3)\n";
    let (fa, imports, _) = r_fa(src);
    assert_eq!(imports, vec!["dplyr"], "a quoted library name imports; helper() does not");
    let end = tree_sitter::Point { row: 4, column: 0 };
    use crate::model::file_analysis::InferredType;
    assert!(
        matches!(fa.inferred_type_via_bag("l", end), Some(InferredType::HashWithKeys { .. })),
        "list() constructs a keyed value"
    );
    assert!(
        !matches!(fa.inferred_type_via_bag("opts", end), Some(InferredType::HashWithKeys { .. })),
        "configure() is no constructor"
    );
}

#[test]
fn r_cross_file_refs_and_workspace_rename() {
    // util.R defines fetch_data; analysis.R sources it and calls it.
    // refs_to + rename across files, R-flavored resolution = the
    // source() path string, verbatim.
    let dir = std::env::temp_dir().join(format!("qx-r-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src_util = "fetch_data <- function(path) {\n  path\n}\n";
    std::fs::write(dir.join("util.R"), src_util).unwrap();
    let src_main = "source(\"util.R\")\n\nresult <- fetch_data(\"db.csv\")\n";

    let (fa_main, imports, _) = r_fa(src_main);
    let (fa_util, _, _) = r_fa(src_util);

    // cross-file registration through the pack resolver
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    resolve_imports_with_pack(&imports, &dir, &r_pack(), &mut r_parser, &idx).unwrap();
    assert!(
        idx.get_cached("util.R").is_some(),
        "source() path registered as a module",
    );

    // rename across the two files via the production path
    let store = crate::index::open_docs::OpenDocs::new();
    let pm = std::path::PathBuf::from("/fake/r/analysis.R");
    let pu = std::path::PathBuf::from("/fake/r/util.R");
    idx.insert_workspace_for_test(pm.clone(), fa_main);
    idx.insert_workspace_for_test(pu.clone(), fa_util);

    let point = tree_sitter::Point { row: 2, column: 12 }; // on fetch_data call
    let fa_view = idx.workspace_file_for_test(&pm).unwrap();
    let target = match crate::index::resolve::resolve_symbol(&fa_view, point, None) {
        Some(crate::index::resolve::ResolvedTarget::Target(t)) => t,
        other => panic!("expected target, got {other:?}"),
    };
    assert_eq!(target.name, "fetch_data");
    let locs = crate::index::resolve::refs_to(&store, Some(&idx), &target, crate::index::resolve::RoleMask::EDITABLE);
    let files: Vec<String> = locs
        .iter()
        .map(|l| match &l.key {
            crate::index::open_docs::FileKey::Path(p) => {
                p.file_name().unwrap().to_string_lossy().to_string()
            }
            _ => unreachable!(),
        })
        .collect();
    assert!(files.contains(&"util.R".to_string()), "decl found: {files:?}");
    assert!(files.contains(&"analysis.R".to_string()), "call found: {files:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn dbg_cmake_cst() {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_cmake::LANGUAGE.into()).unwrap();
    let src = "set(MY_FLAG ON)\ninclude(util.cmake)\nadd_subdirectory(src)\nfunction(register_widget name)\n  message(STATUS \"${name} ${MY_FLAG}\")\nendfunction()\nadd_library(widgets STATIC a.c)\ntarget_link_libraries(widgets PRIVATE core)\nregister_widget(button)\n";
    let tree = p.parse(src, None).unwrap();
    println!("{}", tree.root_node().to_sexp());
}

// ---- spike 5: the CMake pack ----

fn cmake_parser() -> tree_sitter::Parser {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_cmake::LANGUAGE.into()).unwrap();
    p
}

fn cmake_skel(src: &str) -> SkeletonAnalysis {
    let mut parser = cmake_parser();
    let tree = parser.parse(src, None).unwrap();
    extract(&tree, src.as_bytes(), &cmake_pack()).unwrap()
}

#[test]
fn cmake_outline_targets_vars_and_interpolated_refs() {
    let src = "set(MY_FLAG ON)\ninclude(util.cmake)\nadd_subdirectory(src)\n\
               function(register_widget name)\n  message(STATUS \"${name} ${MY_FLAG}\")\nendfunction()\n\
               add_library(widgets STATIC a.c)\ntarget_link_libraries(widgets PRIVATE core)\nregister_widget(button)\n";
    let skel = cmake_skel(src);

    let defs: Vec<(DefCap, String)> = skel
        .symbols
        .iter()
        .map(|s| (s.kind, s.name.clone()))
        .collect();
    assert!(defs.contains(&(DefCap::Var, "MY_FLAG".into())), "{defs:?}");
    assert!(defs.contains(&(DefCap::Sub, "register_widget".into())), "{defs:?}");
    assert!(defs.contains(&(DefCap::Sub, "widgets".into())), "target def: {defs:?}");
    assert!(defs.contains(&(DefCap::Var, "name".into())), "param def: {defs:?}");
    assert_eq!(skel.imports, vec!["util.cmake", "src"]);

    // ${MY_FLAG} INSIDE a quoted string is a real var ref — Perl's
    // regex-interpolation work, free.
    assert!(
        skel.refs.iter().any(|r| r.kind == RefCap::Var && r.name == "MY_FLAG"),
        "interpolated var ref: {:?}",
        skel.refs.iter().filter(|r| r.kind == RefCap::Var).collect::<Vec<_>>(),
    );
    // target_link_libraries args reference targets; PRIVATE is a
    // keyword and must not become a ref
    assert!(skel.refs.iter().any(|r| r.kind == RefCap::Call && r.name == "widgets"));
    assert!(skel.refs.iter().any(|r| r.kind == RefCap::Call && r.name == "core"));
    assert!(!skel.refs.iter().any(|r| r.name == "PRIVATE"));
}

/// CMake command names are case-insensitive, so the family patterns are
/// too: `SET` and `Add_Library` declare what their lower-case spellings do.
#[test]
fn cmake_command_families_match_however_the_command_is_cased() {
    let skel = cmake_skel(
        "SET(MY_FLAG ON)\nAdd_Library(widgets STATIC a.c)\nTARGET_LINK_LIBRARIES(widgets PUBLIC core)\nINCLUDE(util.cmake)\n",
    );
    let defs: Vec<(DefCap, String)> = skel.symbols.iter().map(|s| (s.kind, s.name.clone())).collect();
    assert!(defs.contains(&(DefCap::Var, "MY_FLAG".into())), "{defs:?}");
    assert!(defs.contains(&(DefCap::Sub, "widgets".into())), "{defs:?}");
    assert!(skel.refs.iter().any(|r| r.kind == RefCap::Call && r.name == "core"));
    assert!(!skel.refs.iter().any(|r| r.name == "PUBLIC"));
    assert_eq!(skel.imports, vec!["util.cmake"]);
}

/// A command family's def spans the whole command, so the name it declares
/// sits inside its range; an interpolated argument names no target.
#[test]
fn cmake_a_command_def_spans_its_command() {
    let skel = cmake_skel("set(MY_FLAG ON)\ntarget_sources(app PRIVATE ${GEN} main.c)\n");
    let flag = skel.symbols.iter().find(|s| s.name == "MY_FLAG").expect("the set def");
    assert_eq!((flag.start.column, flag.end.column), (0, 15), "the def is `set(MY_FLAG ON)`");
    assert_eq!((flag.name_start.column, flag.name_end.column), (4, 11));
    let calls: Vec<&str> =
        skel.refs.iter().filter(|r| r.kind == RefCap::Call).map(|r| r.name.as_str()).collect();
    assert!(calls.contains(&"app") && calls.contains(&"main.c"), "{calls:?}");
    assert!(!calls.iter().any(|c| c.contains("${")), "{calls:?}");
}

// ---- C++ obstacle course: measure macro-induced parse damage ----

#[path = "cpp_obstacle_test_corpus.rs"]
mod cpp_obstacle;

fn cpp_parser() -> tree_sitter::Parser {
    let mut p = tree_sitter::Parser::new();
    p.set_language(&tree_sitter_cpp::LANGUAGE.into()).unwrap();
    p
}

/// (error_nodes, missing_nodes, deepest_error_byte_span_total)
fn count_damage(node: tree_sitter::Node) -> (usize, usize, usize) {
    let mut errors = 0;
    let mut missing = 0;
    let mut err_bytes = 0;
    let mut cursor = node.walk();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.is_error() {
            errors += 1;
            err_bytes += n.end_byte() - n.start_byte();
        }
        if n.is_missing() {
            missing += 1;
        }
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    (errors, missing, err_bytes)
}

/// Does the identifier `name` appear ANYWHERE as a named leaf in the
/// tree? (i.e. is there at least a token the skeleton could capture).
fn name_present(node: tree_sitter::Node, src: &[u8], name: &str) -> bool {
    let mut cursor = node.walk();
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.named_child_count() == 0 && n.utf8_text(src).ok() == Some(name) {
            return true;
        }
        for c in n.children(&mut cursor) {
            stack.push(c);
        }
    }
    false
}

#[test]
fn cpp_obstacle_course_damage_report() {
    use cpp_obstacle::SAMPLES;
    let mut parser = cpp_parser();
    println!("\n===== C++ macro obstacle course: parse-damage report =====");
    println!(
        "{:<18} {:>6} {:>7} {:>8}  {:>5}  expected-names-reachable",
        "sample", "errors", "missing", "err-byte", "names"
    );
    let mut total_err = 0;
    let mut total_reach = 0;
    let mut total_names = 0;
    for s in SAMPLES {
        let tree = parser.parse(s.src, None).unwrap();
        let root = tree.root_node();
        let (errors, missing, err_bytes) = count_damage(root);
        let reachable: Vec<&str> = s
            .expected
            .iter()
            .copied()
            .filter(|n| name_present(root, s.src.as_bytes(), n))
            .collect();
        let lost: Vec<&str> = s
            .expected
            .iter()
            .copied()
            .filter(|n| !reachable.contains(n))
            .collect();
        total_err += errors;
        total_reach += reachable.len();
        total_names += s.expected.len();
        println!(
            "{:<18} {:>6} {:>7} {:>8}  {:>2}/{:<2}  lost: {:?}",
            s.name,
            errors,
            missing,
            err_bytes,
            reachable.len(),
            s.expected.len(),
            lost,
        );
    }
    println!(
        "----- totals: {total_err} error nodes; {total_reach}/{total_names} \
         expected names reachable as tokens -----\n"
    );
    // The report is the deliverable; this guard just ensures the corpus
    // parses at all (no panic) and the baseline is clean.
    let clean = &SAMPLES[0];
    let tree = parser.parse(clean.src, None).unwrap();
    let (errors, _, _) = count_damage(tree.root_node());
    assert_eq!(errors, 0, "clean baseline must parse error-free");
}


#[test]
fn dbg_cpp_attr_probe() {
    let mut parser = cpp_parser();
    let cases = [
        ("expand-to-attr", "class __attribute__((visibility(\"default\"))) Widget {\npublic:\n  void draw();\n};\n"),
        ("strip-empty",     "class  Widget {\npublic:\n  void draw();\n};\n"),
        ("declspec-attr",   "class __declspec(dllexport) Widget {\npublic:\n  void draw();\n};\n"),
    ];
    for (name, src) in cases {
        let tree = parser.parse(src, None).unwrap();
        let (errors, missing, _) = count_damage(tree.root_node());
        println!("\n--- {name}: errors={errors} missing={missing} ---\n{}", tree.root_node().to_sexp());
    }
}

#[test]
fn dbg_cpp_cst() {
    use cpp_obstacle::SAMPLES;
    let mut parser = cpp_parser();
    for s in SAMPLES {
        let tree = parser.parse(s.src, None).unwrap();
        println!("\n========== {} ==========\n{}", s.name, tree.root_node().to_sexp());
    }
}

/// A `Point` for `needle`'s first byte at the given occurrence — tree-sitter
/// columns ARE byte offsets, matching `reanchor_truncated_containers`.
fn tok(src: &str, needle: &str, occ: usize) -> Point {
    let mut idx = 0;
    for _ in 0..occ {
        idx += src[idx..].find(needle).expect("needle") + needle.len();
    }
    let f = idx + src[idx..].find(needle).expect("needle");
    let row = src[..f].bytes().filter(|&b| b == b'\n').count();
    let col = f - src[..f].rfind('\n').map(|n| n + 1).unwrap_or(0);
    Point { row, column: col }
}

/// Build a bare `SkelSymbol` at the named token, with the given kind/package —
/// enough surface for `reanchor_truncated_containers` (name span + start).
fn sksym(src: &str, kind: DefCap, name: &str, occ: usize, package: Option<&str>) -> super::SkelSymbol {
    let ns = tok(src, name, occ);
    super::SkelSymbol {
        kind,
        name: name.to_string(),
        start: ns,
        end: ns,
        name_start: ns,
        name_end: Point { row: ns.row, column: ns.column + name.len() },
        package: package.map(str::to_string),
        scope: crate::model::file_analysis::ScopeId(0),
        spelled: None,
        pair: None,
        return_type: None,
        deref_stack: Vec::new(),
        attributes: Vec::new(),
        arity: None,
        qualifier_owned: false,
        flags: Default::default(),
    }
}

/// The re-anchor invariant (`docs/adr/config-superposition-declarations.md`): when a deep
/// misparse truncates a `class_specifier`, its late members become siblings in
/// the enclosing scope and lose their `package` (json.hpp `basic_json`: ~4400
/// lines fall through to `nlohmann`). The recovery brace-matches the ORIGINAL
/// source (balanced — each `#if`/`#else` arm is individually brace-balanced C++,
/// so both-arms-present text still balances; only the macro-expanded transform
/// unbalances) and re-attributes each member to the innermost container that
/// textually encloses it.
#[test]
fn reanchor_recovers_members_after_truncated_class() {
    // The class body braces are REAL (as in the original source); the fall-
    // through is simulated by giving late members the enclosing scope's name —
    // exactly the post-truncation skeleton the extractor produces.
    let src = "namespace ns {\nclass Widget {\n  int early;\n  int mid;\n  int late;\n  void tail() {}\n};\n}\n";
    let mut skel = SkeletonAnalysis::default();
    skel.symbols = vec![
        sksym(src, DefCap::Package, "ns", 0, None),
        sksym(src, DefCap::Class, "Widget", 0, Some("ns")),
        // early is correctly attributed; mid/late/tail fell through to `ns`
        // (a literal namespace = an ancestor container) — the truncation shape.
        sksym(src, DefCap::Field, "early", 0, Some("Widget")),
        sksym(src, DefCap::Field, "mid", 0, Some("ns")),
        sksym(src, DefCap::Field, "late", 0, Some("ns")),
        sksym(src, DefCap::Method, "tail", 0, Some("ns")),
    ];

    skel.reanchor_truncated_containers(src);
    let pkg = |name: &str| skel.symbols.iter().find(|s| s.name == name).and_then(|s| s.package.clone());

    // Every member — including the late fall-through ones — lands in Widget.
    assert_eq!(pkg("early").as_deref(), Some("Widget"));
    assert_eq!(pkg("mid").as_deref(), Some("Widget"));
    assert_eq!(pkg("late").as_deref(), Some("Widget"));
    assert_eq!(pkg("tail").as_deref(), Some("Widget"));
    // The class itself stays in its namespace (its name is before the body).
    assert_eq!(pkg("Widget").as_deref(), Some("ns"));
    assert_eq!(pkg("ns"), None);
}

/// Non-computable enclosing scope (a MACRO-defined namespace like json.hpp's
/// `nlohmann`, whose name span covers the macro token, not `nlohmann`): the
/// fallen-through member still recovers, because the current package names no
/// computable container.
#[test]
fn reanchor_recovers_through_macro_namespace() {
    let src = "class Widget {\n  int early;\n  int late;\n};\n";
    let mut skel = SkeletonAnalysis::default();
    skel.symbols = vec![
        sksym(src, DefCap::Class, "Widget", 0, Some("nlohmann")),
        sksym(src, DefCap::Field, "early", 0, Some("Widget")),
        // `late` fell through to `nlohmann` — a name with no computable
        // container symbol in this file (macro-synthesized namespace).
        sksym(src, DefCap::Field, "late", 0, Some("nlohmann")),
    ];
    skel.reanchor_truncated_containers(src);
    let pkg = |name: &str| skel.symbols.iter().find(|s| s.name == name).and_then(|s| s.package.clone());
    assert_eq!(pkg("late").as_deref(), Some("Widget"));
    // Widget's own name is before its body brace → not inside any computable
    // container → its (macro-namespace) package is left as-is.
    assert_eq!(pkg("Widget").as_deref(), Some("nlohmann"));
}

/// Upgrade-only guard: a `::`-qualifier attribution (out-of-line def) names a
/// container that does NOT textually enclose the symbol — leave it alone, never
/// overwrite qualifier knowledge with the textual scope.
#[test]
fn reanchor_preserves_out_of_line_qualifier() {
    // `run` is written at namespace scope (outside Buf's braces) but attributed
    // to Buf via the `Buf::` qualifier.
    let src = "namespace ns {\nclass Buf {\n  void run();\n};\nvoid Buf::run() { work(); }\n}\n";
    let mut skel = SkeletonAnalysis::default();
    skel.symbols = vec![
        sksym(src, DefCap::Package, "ns", 0, None),
        sksym(src, DefCap::Class, "Buf", 0, Some("ns")),
        // the out-of-line def (2nd `run`) attributed to Buf, sitting in `ns`.
        sksym(src, DefCap::Method, "run", 1, Some("Buf")),
    ];
    skel.reanchor_truncated_containers(src);
    let run = skel.symbols.iter().find(|s| s.name == "run").unwrap();
    assert_eq!(run.package.as_deref(), Some("Buf"), "qualifier preserved, not re-anchored to ns");
}

/// A forward declaration (`class X;` — no body) is not a computable container:
/// it must not seed a bogus extent that swallows a following symbol.
#[test]
fn reanchor_ignores_forward_declarations() {
    let src = "namespace ns {\nclass Fwd;\nint free_var;\n}\n";
    let mut skel = SkeletonAnalysis::default();
    skel.symbols = vec![
        sksym(src, DefCap::Class, "Fwd", 0, Some("ns")),
        sksym(src, DefCap::Field, "free_var", 0, Some("ns")),
    ];
    skel.reanchor_truncated_containers(src);
    let fv = skel.symbols.iter().find(|s| s.name == "free_var").unwrap();
    assert_eq!(fv.package.as_deref(), Some("ns"), "forward decl has no body → free_var untouched");
}

fn cpp_skel(src: &str) -> SkeletonAnalysis {
    let mut parser = cpp_parser();
    let tree = parser.parse(src, None).unwrap();
    extract(&tree, src.as_bytes(), &cpp_pack()).unwrap()
}

/// The `deref` walk, spelling by spelling: the per-level stack a pointer /
/// reference chain flattens to, each level carrying its own cv-qualifiers,
/// and the same chain reading the same whether it declares a member or a
/// local, initialized or not.
#[test]
fn cpp_declarator_peel_stacks_every_spelling() {
    use crate::model::file_analysis::DerefKind::{Pointer, Reference};
    let src = "\
struct S {
  Box** a;
  char* const& b;
  Box* const& c;
  Node*** d;
};
void f() {
  Box** x;
  char* const& y = q;
}
";
    let skel = cpp_skel(src);
    let stack = |name: &str| -> Vec<(crate::model::file_analysis::DerefKind, Vec<String>)> {
        skel.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("{name} is extracted"))
            .deref_stack
            .iter()
            .map(|st| (st.kind, st.annotations.clone()))
            .collect()
    };
    let plain = |k| (k, Vec::<String>::new());
    let cnst = |k| (k, vec!["const".to_string()]);
    assert_eq!(stack("a"), vec![plain(Pointer), plain(Pointer)], "Box**");
    assert_eq!(stack("b"), vec![cnst(Pointer), plain(Reference)], "char* const&");
    assert_eq!(stack("c"), vec![cnst(Pointer), plain(Reference)], "Box* const&");
    assert_eq!(stack("d"), vec![plain(Pointer); 3], "Node***");
    assert_eq!(stack("x"), vec![plain(Pointer), plain(Pointer)], "a local reads the same");
    assert_eq!(stack("y"), vec![cnst(Pointer), plain(Reference)], "an initialized local too");
}

/// A pointer member is minted as a field and a pointer local as a local:
/// the leaf mark's suffix names the def.
#[test]
fn cpp_declarator_leaf_names_its_def() {
    let skel = cpp_skel("struct S { Box* m; };\nvoid f() { Box* l; }\n");
    let kind = |name: &str| skel.symbols.iter().find(|s| s.name == name).map(|s| s.kind);
    assert_eq!(kind("m"), Some(DefCap::Field));
    assert_eq!(kind("l"), Some(DefCap::Local));
}

#[test]
fn cpp_tier1_extraction_report() {
    use cpp_obstacle::SAMPLES;
    println!("\n===== C++ Tier-1 skeleton extraction (pack through driver) =====");
    let mut got_total = 0;
    let mut exp_total = 0;
    for s in SAMPLES {
        let skel = cpp_skel(s.src);
        let names: Vec<String> = skel.symbols.iter().map(|s| s.name.clone()).collect();
        let hit: Vec<&str> = s.expected.iter().copied().filter(|n| names.iter().any(|g| g == n)).collect();
        let miss: Vec<&str> = s.expected.iter().copied().filter(|n| !hit.contains(n)).collect();
        got_total += hit.len();
        exp_total += s.expected.len();
        let kinds: Vec<String> = skel
            .symbols
            .iter()
            .map(|s| format!("{}:{}", s.kind, s.name))
            .collect();
        println!(
            "{:<18} {:>2}/{:<2}  miss: {:<28}  extracted: {:?}",
            s.name,
            hit.len(),
            s.expected.len(),
            format!("{miss:?}"),
            kinds,
        );
    }
    println!("----- Tier-1 recall: {got_total}/{exp_total} expected symbols extracted -----\n");
}

#[test]
fn cpp_clean_baseline_outline() {
    let skel = cpp_skel(cpp_obstacle::SAMPLES[0].src);
    let defs: Vec<(DefCap, String)> =
        skel.symbols.iter().map(|s| (s.kind, s.name.clone())).collect();
    assert!(defs.contains(&(DefCap::Class, "Shape".into())), "{defs:?}");
    assert!(defs.contains(&(DefCap::Class, "Circle".into())), "{defs:?}");
    assert!(defs.contains(&(DefCap::Method, "area".into())), "{defs:?}");
    assert!(defs.contains(&(DefCap::Field, "radius".into())), "{defs:?}");
    assert!(defs.contains(&(DefCap::Sub, "main".into())), "{defs:?}");
    // namespace stickiness: Shape/Circle carry package=geo
    let shape = skel.symbols.iter().find(|s| s.name == "Shape").unwrap();
    assert_eq!(shape.package.as_deref(), Some("geo"), "namespace context");
}

#[test]
fn cpp_walker_free_type_queries() {
    // The same proof the Python pack gives, on C++ syntax: a REAL
    // FileAnalysis built from capture events alone answers type queries
    // through the production witness bag + reducer registry. C++'s leak
    // is declared types — pervasive, so annot_type carries the load.
    let src = "\
namespace geo { class Circle { public: double area(); }; }
int main() {
    int n = 5;
    std::string s = \"hi\";
    geo::Circle c;
    auto m = n;
    return 0;
}
";
    let fa = cpp_skel(src).into_file_analysis();
    use crate::model::file_analysis::InferredType;
    // cursor on `return 0;` (row 6) — past every declaration. Queries
    // are TEMPORAL: a variable has no type before its declaration point,
    // which is exactly the production rule (no guessing the future).
    let inside = tree_sitter::Point { row: 6, column: 4 };
    assert_eq!(fa.inferred_type_via_bag("n", inside), Some(InferredType::Numeric), "int decl");
    assert_eq!(fa.inferred_type_via_bag("s", inside), Some(InferredType::String), "std::string decl");
    // The namespace qualifier is stripped — classes/members are keyed by
    // the unqualified name (@context.class), so `geo::Circle` types as
    // `Circle` and member completion resolves through the hierarchy.
    assert_eq!(
        fa.inferred_type_via_bag("c", inside),
        Some(InferredType::ClassName("Circle".into())),
        "class-instance decl",
    );
    // `auto m = n` — the cross-variable edge chase: m → Expr(n) →
    // Variable(n) → Numeric, through the production registry untouched.
    assert_eq!(fa.inferred_type_via_bag("m", inside), Some(InferredType::Numeric), "auto edge chase");
    // and temporality holds: before its declaration, n has no type
    assert_eq!(
        fa.inferred_type_via_bag("n", tree_sitter::Point { row: 1, column: 0 }),
        None,
        "no type before declaration",
    );
}

#[test]
fn cpp_type_inference_through_macro_reparse() {
    // The whole stack composes. A declarator-position macro DESTROYS the
    // class (it reparses as a function), so the class SYMBOL — the thing
    // goto-def / members / inheritance need — does not exist. `b`'s
    // *nominal* type (`ClassName("Box")`) survives, because it comes from
    // b's own declaration, but it points at nothing. Reparse via
    // expansion recovers the class symbol and its fields, making that
    // nominal type RESOLVABLE; the bag then types `b` and the edge chase
    // `auto same = b` on the reparsed tree.
    let mut parser = cpp_parser();
    let src = "\
#define API_EXPORT __attribute__((visibility(\"default\")))
class API_EXPORT Box {
public:
    int width;
};
int main() {
    Box b;
    auto same = b;
    return 0;
}
";
    // pre-reparse: the class SYMBOL evaporated (only its corrupted
    // remains), so navigation to `Box` and its `width` field is dead.
    let raw_defs: Vec<(DefCap, String)> = cpp_skel(src)
        .symbols
        .iter()
        .map(|s| (s.kind, s.name.clone()))
        .collect();
    assert!(!raw_defs.contains(&(DefCap::Class, "Box".into())), "class lost pre-reparse: {raw_defs:?}");

    // reparse (validated macro expansion), then extract + query
    let (rewritten, _map, _) = crate::build::cpp_reparse::preprocess_validated_with(
        &mut parser,
        src,
        &crate::build::cpp_reparse::PreExpandedExternal::empty(),
    );
    let skel = cpp_skel(&rewritten);
    let defs: Vec<(DefCap, String)> = skel.symbols.iter().map(|s| (s.kind, s.name.clone())).collect();
    assert!(defs.contains(&(DefCap::Class, "Box".into())), "class recovered: {defs:?}");
    assert!(defs.contains(&(DefCap::Field, "width".into())), "field recovered: {defs:?}");

    use crate::model::file_analysis::InferredType;
    let fa = skel.into_file_analysis();
    let pt = tree_sitter::Point { row: 8, column: 4 }; // on `return 0;`
    assert_eq!(
        fa.inferred_type_via_bag("b", pt),
        Some(InferredType::ClassName("Box".into())),
        "b types as the recovered class through the bag",
    );
    // the edge chase rides on top of the macro-reparsed tree
    assert_eq!(
        fa.inferred_type_via_bag("same", pt),
        Some(InferredType::ClassName("Box".into())),
        "auto edge chase on a macro-recovered class",
    );
}

#[test]
fn cpp_cross_file_include_resolution_and_rename() {
    let dir = std::env::temp_dir().join(format!("qx-cpp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let header = "int compute(int x);\n";
    std::fs::write(dir.join("util.h"), header).unwrap();
    let main_src = "#include \"util.h\"\n\nint main() {\n    return compute(41);\n}\n";

    let main_skel = cpp_skel(main_src);
    let imports = main_skel.imports.clone();
    assert_eq!(imports, vec!["util.h"], "quoted include captured clean: {imports:?}");

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    resolve_imports_with_pack(&imports, &dir, &cpp_pack(), &mut cpp_parser, &idx).unwrap();
    assert!(idx.get_cached("util.h").is_some(), "#include must resolve util.h");

    // cross-file rename: cursor on the call `compute` in main → def in
    // util.h + call in main, through the production resolve/refs path
    let store = crate::index::open_docs::OpenDocs::new();
    let pm = std::path::PathBuf::from("/fake/cpp/main.cpp");
    let ph = std::path::PathBuf::from("/fake/cpp/util.h");
    idx.insert_workspace_for_test(pm.clone(), main_skel.into_file_analysis());
    idx.insert_workspace_for_test(ph.clone(), cpp_skel(header).into_file_analysis());

    let fa_view = idx.workspace_file_for_test(&pm).unwrap();
    let point = tree_sitter::Point { row: 3, column: 11 };
    let target = match crate::index::resolve::resolve_symbol(&fa_view, point, None) {
        Some(crate::index::resolve::ResolvedTarget::Target(t)) => t,
        other => panic!("expected target, got {other:?}"),
    };
    assert_eq!(target.name, "compute");
    let locs = crate::index::resolve::refs_to(&store, Some(&idx), &target, crate::index::resolve::RoleMask::EDITABLE);
    let files: Vec<String> = locs
        .iter()
        .map(|l| match &l.key {
            crate::index::open_docs::FileKey::Path(p) => {
                p.file_name().unwrap().to_string_lossy().to_string()
            }
            _ => unreachable!(),
        })
        .collect();
    assert!(files.contains(&"util.h".to_string()), "def in header: {files:?}");
    assert!(files.contains(&"main.cpp".to_string()), "call in main: {files:?}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn cpp_cross_file_enum_variant_goto_def() {
    // A bare enum-constant use (`OP_SCOPE`) resolves cross-file to its
    // enumerator def, even though the enum-hover pass stamps the parent enum
    // as the constant's `package` (a type annotation, for `RED: Color` hover).
    // Two mechanisms cooperate: register_symbols keys file-scope values on
    // File scope (not package absence), so the constant is findable by name;
    // and the use mints an unresolved `Variable` ref that the query-time
    // cross-file tail chases by that name.
    use crate::model::file_analysis::RefKind;
    let header = "enum opcode {\n    OP_NULL,\n    OP_SCOPE,\n};\n";
    let header_fa = cpp_skel(header).into_file_analysis();
    let op = header_fa.symbols().iter().find(|s| s.name == "OP_SCOPE").unwrap();
    assert_eq!(op.package.as_deref(), Some("opcode"),
        "enum constant carries its enum as the (type) package");

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let hpath = std::path::PathBuf::from("/fake/cpp/opcodes.h");
    idx.register_symbols(hpath, std::sync::Arc::new(header_fa));
    assert!(idx.get_cached("OP_SCOPE").is_some(),
        "packaged file-scope enum constant is registered by name");

    let use_src = "int is_scope(int t) {\n    return t == OP_SCOPE;\n}\n";
    let use_fa = cpp_skel(use_src).into_file_analysis();
    assert!(
        use_fa.refs().iter().any(|r| matches!(r.kind, RefKind::Variable)
            && r.target_name == "OP_SCOPE"
            && r.resolved_symbol().is_none()),
        "a use with no LOCAL decl still mints an unresolved Variable ref"
    );

    let uri = tower_lsp::lsp_types::Url::from_file_path("/fake/cpp/use.c").unwrap();
    let pos = tower_lsp::lsp_types::Position { line: 1, character: 16 };
    let store = crate::index::open_docs::OpenDocs::new();
    let resp = crate::lsp::symbols::find_definition(&store, &use_fa, pos, &uri, &idx)
        .expect("cross-file enum-variant goto-def resolves");
    let loc = match resp {
        tower_lsp::lsp_types::GotoDefinitionResponse::Scalar(l) => l,
        other => panic!("expected a single location, got {other:?}"),
    };
    assert!(loc.uri.path().ends_with("opcodes.h"), "lands in the header: {}", loc.uri);
    assert_eq!(loc.range.start.line, 2, "lands on the OP_SCOPE enumerator (line 3)");
}

#[test]
fn cmake_cross_file_function_rename_and_subdirectory_resolution() {
    let dir = std::env::temp_dir().join(format!("qx-cmake-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let src_util = "function(register_widget name)\n  message(STATUS \"${name}\")\nendfunction()\n";
    std::fs::write(dir.join("util.cmake"), src_util).unwrap();
    std::fs::write(dir.join("src/CMakeLists.txt"), "add_library(inner STATIC b.c)\n").unwrap();
    let src_root = "include(util.cmake)\nadd_subdirectory(src)\n\nregister_widget(button)\n";

    let root_skel = cmake_skel(src_root);
    let imports = root_skel.imports.clone();
    let fa_root = root_skel.into_file_analysis();
    let fa_util = cmake_skel(src_util).into_file_analysis();

    // resolution: util.cmake verbatim, src → src/CMakeLists.txt
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    resolve_imports_with_pack(&imports, &dir, &cmake_pack(), &mut cmake_parser, &idx).unwrap();
    assert!(idx.get_cached("util.cmake").is_some());
    assert!(
        idx.get_cached("src").is_some(),
        "add_subdirectory(src) must resolve src/CMakeLists.txt",
    );

    // workspace rename of the function, cursor on the call site
    let store = crate::index::open_docs::OpenDocs::new();
    let pr = std::path::PathBuf::from("/fake/cmake/CMakeLists.txt");
    let pu = std::path::PathBuf::from("/fake/cmake/util.cmake");
    idx.insert_workspace_for_test(pr.clone(), fa_root);
    idx.insert_workspace_for_test(pu.clone(), fa_util);

    let fa_view = idx.workspace_file_for_test(&pr).unwrap();
    let point = tree_sitter::Point { row: 3, column: 4 };
    let target = match crate::index::resolve::resolve_symbol(&fa_view, point, None) {
        Some(crate::index::resolve::ResolvedTarget::Target(t)) => t,
        other => panic!("expected target, got {other:?}"),
    };
    assert_eq!(target.name, "register_widget");
    let locs = crate::index::resolve::refs_to(&store, Some(&idx), &target, crate::index::resolve::RoleMask::EDITABLE);
    let files: Vec<String> = locs
        .iter()
        .map(|l| match &l.key {
            crate::index::open_docs::FileKey::Path(p) => {
                p.file_name().unwrap().to_string_lossy().to_string()
            }
            _ => unreachable!(),
        })
        .collect();
    assert!(files.contains(&"util.cmake".to_string()), "def: {files:?}");
    assert!(files.contains(&"CMakeLists.txt".to_string()), "call: {files:?}");
    std::fs::remove_dir_all(&dir).ok();
}




#[test]
fn python_union_annotations_are_unions() {
    use crate::model::file_analysis::format_inferred_type;
    let at = python_pack().annot_type;
    let spelled = |t: &str| at(t).map(|t| format_inferred_type(&t));
    for (src, want) in [
        ("Cat | Bird", "Bird|Cat"),
        ("Union[Cat, Bird]", "Bird|Cat"),
        ("typing.Union[Cat, Bird]", "Bird|Cat"),
        ("Cat | None", "Maybe<Cat>"),
        ("Optional[Cat]", "Maybe<Cat>"),
        ("Optional[Cat | Bird]", "Maybe<Bird|Cat>"),
        ("int | float", "Numeric"),
    ] {
        assert_eq!(spelled(src).as_deref(), Some(want), "{src}");
    }
    // An arm nothing reads types the whole spelling as nothing.
    assert_eq!(at("Cat | 3"), None);
    // `None` alone types nothing; it is only ever an arm.
    assert_eq!(at("None"), None);
}

#[test]
fn python_a_union_annotation_types_the_variable() {
    let src = "\
class Cat:
    def sound(self):
        return 'meow'
class Bird:
    def sound(self):
        return 'tweet'
pet: Cat | Bird = adopt()
y = 1
";
    let mut parser = python_parser();
    let tree = parser.parse(src, None).unwrap();
    let skel = extract(&tree, src.as_bytes(), &python_pack()).unwrap();
    let fa = skel.into_file_analysis();
    let t = fa.inferred_type_via_bag("pet", tree_sitter::Point { row: 7, column: 0 });
    assert_eq!(t.map(|t| crate::model::file_analysis::format_inferred_type(&t)).as_deref(), Some("Bird|Cat"));
}

#[test]
fn python_isinstance_narrows_within_the_guard() {
    // `if isinstance(x, Foo): ...` refines x to Foo INSIDE the block and
    // nowhere else — guard narrowing via scoped witness.
    let src = "\
class Foo:
    def run(self):
        pass
x = maybe()
if isinstance(x, Foo):
    x.run()
y = 1
";
    let mut parser = python_parser();
    let tree = parser.parse(src, None).unwrap();
    let skel = extract(&tree, src.as_bytes(), &python_pack()).unwrap();
    let fa = skel.into_file_analysis();
    use crate::model::file_analysis::InferredType;
    // inside the guard (row 5, `x.run()`): x is Foo
    assert_eq!(
        fa.inferred_type_via_bag("x", tree_sitter::Point { row: 5, column: 4 }),
        Some(InferredType::ClassName("Foo".into())),
        "narrowed inside the guard",
    );
    // after the guard (row 6): the refinement does not leak
    assert_eq!(
        fa.inferred_type_via_bag("x", tree_sitter::Point { row: 6, column: 0 }),
        None,
        "narrowing scoped to the block — gone after",
    );
}

#[test]
fn python_narrowing_truncates_at_reassignment() {
    // THE cross-language cutoff: `isinstance` narrows x to Foo, then a
    // reassignment INSIDE the block rebinds it — x must stop being Foo after
    // the rebind (the edge-driven cutoff, same as Perl). Pre-lift, the scoped
    // witness leaked Foo to the whole block.
    let src = "\
class Foo:
    def run(self):
        pass
x = maybe()
if isinstance(x, Foo):
    x.run()
    x = other()
    x.run()
y = 1
";
    let mut parser = python_parser();
    let tree = parser.parse(src, None).unwrap();
    let skel = extract(&tree, src.as_bytes(), &python_pack()).unwrap();
    let fa = skel.into_file_analysis();
    use crate::model::file_analysis::InferredType;
    assert_eq!(
        fa.inferred_type_via_bag("x", tree_sitter::Point { row: 5, column: 4 }),
        Some(InferredType::ClassName("Foo".into())),
        "narrowed before the reassignment",
    );
    assert_ne!(
        fa.inferred_type_via_bag("x", tree_sitter::Point { row: 7, column: 4 }),
        Some(InferredType::ClassName("Foo".into())),
        "reassignment rebinds x → narrowing truncated by the cross-language cutoff",
    );
}

#[test]
fn python_for_loop_var_rebind_truncates_narrowing() {
    // A `for x in …` loop INSIDE the guard rebinds x (the loop var leaks in
    // Python) — so the narrowing ends at the loop, via the bind-shape Rebind
    // edge feeding the cutoff.
    let src = "\
class Foo:
    def run(self):
        pass
x = maybe()
if isinstance(x, Foo):
    x.run()
    for x in items:
        pass
    x.run()
";
    let mut parser = python_parser();
    let tree = parser.parse(src, None).unwrap();
    let skel = extract(&tree, src.as_bytes(), &python_pack()).unwrap();
    let fa = skel.into_file_analysis();
    use crate::model::file_analysis::InferredType;
    assert_eq!(
        fa.inferred_type_via_bag("x", tree_sitter::Point { row: 5, column: 4 }),
        Some(InferredType::ClassName("Foo".into())),
        "narrowed before the loop",
    );
    assert_ne!(
        fa.inferred_type_via_bag("x", tree_sitter::Point { row: 8, column: 4 }),
        Some(InferredType::ClassName("Foo".into())),
        "the loop var rebinds x → narrowing truncated",
    );
}

#[test]
fn cpp_use_after_move_flags_read_before_reassign() {
    // `std::move(x)` leaves x moved-from: the row-3 read is a bug; the row-5
    // read is fine because `x = other()` reassigns it (the FlowEdge rebind
    // ends the moved-from region, same cutoff as narrowing).
    let src = "\
void f() {
  Widget x;
  sink(std::move(x));
  x.use();
  x = other();
  x.use();
}
";
    let fa = cpp_skel(src).into_file_analysis();
    let diags = crate::lsp::symbols::pack_use_after_move_diagnostics(&fa);
    assert_eq!(diags.len(), 1, "exactly one use-after-move: {diags:?}");
    // the flagged read is the row-3 `x.use()` receiver, not the row-5 one.
    assert_eq!(diags[0].range.start.line, 3, "row-3 read flagged: {diags:?}");
    assert!(
        diags.iter().all(|d| d.range.start.line != 5),
        "row-5 read (post-reassign) is clean: {diags:?}",
    );
}

#[test]
fn cpp_conditional_move_does_not_flag_sibling_arm() {
    // GOAL-2.1: if/else arms are their own @scope now, so a `std::move` in the
    // `if` arm bounds its moved-from region to that arm — the read of `x` in the
    // `else` arm (and after the if) is a different scope subtree, not flagged.
    let src = "\
void f(bool c) {
  Widget x;
  if (c) {
    sink(std::move(x));
  } else {
    x.use();
  }
  x.use();
}
";
    let fa = cpp_skel(src).into_file_analysis();
    let diags = crate::lsp::symbols::pack_use_after_move_diagnostics(&fa);
    assert_eq!(
        diags.len(),
        0,
        "the move is scoped to the if-arm — sibling else / post-if reads are clean: {diags:?}",
    );
}

#[test]
fn cpp_conditional_move_still_flags_same_arm_read() {
    // The arm scoping must not over-suppress: a read AFTER the move IN THE SAME
    // arm is still the real use-after-move bug.
    let src = "\
void f(bool c) {
  Widget x;
  if (c) {
    sink(std::move(x));
    x.use();
  }
}
";
    let fa = cpp_skel(src).into_file_analysis();
    let diags = crate::lsp::symbols::pack_use_after_move_diagnostics(&fa);
    assert_eq!(diags.len(), 1, "same-arm read after move is still flagged: {diags:?}");
    assert_eq!(diags[0].range.start.line, 4, "the row-4 same-arm read: {diags:?}");
}

#[test]
fn cpp_reset_via_method_ends_moved_from_region() {
    // GOAL-2.2: a rebinding method call (`x.clear()`) puts the moved-from object
    // back into a known state — the moved-from region ends at the reset, so the
    // reset's own receiver read AND the later `x.use()` are clean.
    let src = "\
void f() {
  Widget x;
  sink(std::move(x));
  x.clear();
  x.use();
}
";
    let fa = cpp_skel(src).into_file_analysis();
    let diags = crate::lsp::symbols::pack_use_after_move_diagnostics(&fa);
    assert_eq!(
        diags.len(),
        0,
        "x.clear() rebinds x — the reset and the post-reset use are clean: {diags:?}",
    );
}

#[test]
fn cpp_non_reset_method_after_move_still_flags() {
    // The rebind-method set is specific: an ordinary method call (`x.use()`) is
    // NOT a reset, so the canonical use-after-move still flags.
    let src = "\
void f() {
  Widget x;
  sink(std::move(x));
  x.use();
}
";
    let fa = cpp_skel(src).into_file_analysis();
    let diags = crate::lsp::symbols::pack_use_after_move_diagnostics(&fa);
    assert_eq!(diags.len(), 1, "non-reset use after move still flags: {diags:?}");
}

// ---- honesty gates B / C / E: the silence side (false positives are worse
// than false negatives; each negative below is a documented gate). Verified to
// take the FPs on real spdlog/fmt/onednn headers to zero.

#[test]
fn cpp_uam_braceless_conditional_move_is_silent() {
    // GATE C: the move is in a braceless `if` arm (no compound → no @scope), so
    // the post-if read may be reached WITHOUT the move. Not straight-line ⇒
    // silent. (Braced arms are their own scope and stay flaggable — see
    // `cpp_conditional_move_still_flags_same_arm_read`.)
    let src = "\
void f(bool c) {
  Widget x;
  if (c) sink(std::move(x));
  x.use();
}
";
    let fa = cpp_skel(src).into_file_analysis();
    let diags = crate::lsp::symbols::pack_use_after_move_diagnostics(&fa);
    assert_eq!(diags.len(), 0, "braceless conditional move is not straight-line: {diags:?}");
}

#[test]
fn cpp_uam_switch_case_move_is_silent() {
    // GATE C: a move in one `case` and a read in another share the switch-body
    // scope (case labels aren't scopes); which case runs is path-sensitive.
    let src = "\
void f(int k) {
  Widget x;
  switch (k) {
    case 1: sink(std::move(x)); break;
    case 2: x.use(); break;
  }
}
";
    let fa = cpp_skel(src).into_file_analysis();
    let diags = crate::lsp::symbols::pack_use_after_move_diagnostics(&fa);
    assert_eq!(diags.len(), 0, "switch-case conditional move is silent: {diags:?}");
}

#[test]
fn cpp_uam_ternary_move_is_silent() {
    // GATE C: the move is inside a ternary — a conditionally-evaluated operand.
    let src = "\
void f(bool c) {
  Widget x;
  int n = c ? consume(std::move(x)) : 0;
  x.use();
}
";
    let fa = cpp_skel(src).into_file_analysis();
    let diags = crate::lsp::symbols::pack_use_after_move_diagnostics(&fa);
    assert_eq!(diags.len(), 0, "ternary conditional move is silent: {diags:?}");
}

#[test]
fn cpp_uam_loop_body_move_is_silent() {
    // GATE C: a move in a loop body may run every iteration; a same-body read is
    // path-sensitive across the back-edge. Loop bodies aren't scopes, so the
    // whole loop gates the move.
    let src = "\
void f(int n) {
  Widget x;
  while (n-- > 0) {
    sink(std::move(x));
    x.use();
  }
}
";
    let fa = cpp_skel(src).into_file_analysis();
    let diags = crate::lsp::symbols::pack_use_after_move_diagnostics(&fa);
    assert_eq!(diags.len(), 0, "loop-carried move is silent: {diags:?}");
}

#[test]
fn cpp_uam_member_init_list_move_is_silent() {
    // GATE B: a move in a member-initializer list lands in the CLASS scope, not
    // a function body (the init list is outside the ctor's `{}`), so it can't be
    // bounded — and the moved value only initializes ONE subobject. This is the
    // move-constructor flood the real headers exhibit.
    let src = "\
struct W {
  Widget a_;
  int b_;
  W(Widget other) : a_(std::move(other)), b_(other.size()) {}
};
";
    let fa = cpp_skel(src).into_file_analysis();
    let diags = crate::lsp::symbols::pack_use_after_move_diagnostics(&fa);
    assert_eq!(diags.len(), 0, "member-initializer-list move is silent: {diags:?}");
}

#[test]
fn cpp_uam_parameter_move_is_silent() {
    // GATE E: moving a PARAMETER then reading it is the forwarding / subobject
    // idiom (move-ctors, operator=), which this tier can't tell from a bug.
    // Only moves of LOCALS are flagged.
    let src = "\
void f(Widget w) {
  sink(std::move(w));
  w.use();
}
";
    let fa = cpp_skel(src).into_file_analysis();
    let diags = crate::lsp::symbols::pack_use_after_move_diagnostics(&fa);
    assert_eq!(diags.len(), 0, "moved parameter is not flagged: {diags:?}");
}

#[test]
fn cpp_uam_local_in_nested_block_still_flags() {
    // The gates don't over-suppress: a straight-line move + read of a LOCAL in a
    // plain nested block (not a conditional/loop) is a real bug and still flags.
    let src = "\
void f() {
  Widget x;
  {
    sink(std::move(x));
    x.use();
  }
}
";
    let fa = cpp_skel(src).into_file_analysis();
    let diags = crate::lsp::symbols::pack_use_after_move_diagnostics(&fa);
    assert_eq!(diags.len(), 1, "straight-line local move in a bare block still flags: {diags:?}");
}

#[test]
fn cpp_uam_toggle_gates_pack_diagnostics() {
    // The diagnostic is opt-in: `pack_diagnostics` emits it only when the toggle
    // is set, and never for the default (off) options.
    let src = "\
void f() {
  Widget x;
  sink(std::move(x));
  x.use();
}
";
    let fa = cpp_skel(src).into_file_analysis();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let off = crate::lsp::symbols::pack_diagnostics(&fa, &idx, crate::lsp::symbols::DiagnosticOptions::default());
    assert!(
        !off.iter().any(|d| matches!(&d.code, Some(tower_lsp::lsp_types::NumberOrString::String(s)) if s == "use-after-move")),
        "off by default: {off:?}",
    );
    let on = crate::lsp::symbols::pack_diagnostics(
        &fa,
        &idx,
        crate::lsp::symbols::DiagnosticOptions { use_after_move: true, ..Default::default() },
    );
    assert!(
        on.iter().any(|d| matches!(&d.code, Some(tower_lsp::lsp_types::NumberOrString::String(s)) if s == "use-after-move")),
        "on when toggled: {on:?}",
    );
}

#[test]
fn cpp_dynamic_cast_guard_narrows() {
    // `if (dynamic_cast<Derived*>(b))` refines b to Derived inside the block —
    // the cpp analog of python isinstance, via the now-wired narrow_guard.
    let src = "\
void f(Base* b) {
    if (dynamic_cast<Derived*>(b)) {
        b->run();
    }
}
";
    let fa = cpp_skel(src).into_file_analysis();
    use crate::model::file_analysis::InferredType;
    assert_eq!(
        fa.inferred_type_via_bag("b", tree_sitter::Point { row: 2, column: 8 }),
        Some(InferredType::ClassName("Derived".into())),
        "dynamic_cast narrows b to Derived inside the guard",
    );
}

#[test]
fn cpp_optional_engaged_guard_narrows_to_inner_type() {
    // Guard-testing a `std::optional<Widget>` as engaged proves it holds a
    // Widget inside the block, so `opt->` / `*opt` resolve on Widget there.
    // Two engagement shapes — bare truthiness `if (opt)` and `if
    // (opt.has_value())` — both peel T off the DECLARED optional type; the
    // refinement is gone after the block and dies at a reassignment (the same
    // edge-driven cutoff as dynamic_cast / isinstance).
    let src = "\
void f(std::optional<Widget> a, std::optional<Widget> b) {
    if (a) {
        a->run();
        a = other();
        a->run();
    }
    a->run();
    if (b.has_value()) {
        b->go();
    }
}
";
    let fa = cpp_skel(src).into_file_analysis();
    use crate::model::file_analysis::InferredType;
    let widget = || Some(InferredType::ClassName("Widget".into()));
    // bare `if (a)`: a narrows to Widget inside (row 2)…
    assert_eq!(
        fa.inferred_type_via_bag("a", tree_sitter::Point { row: 2, column: 8 }),
        widget(),
        "bare `if (a)` narrows the engaged optional to its inner Widget",
    );
    // …but the reassignment inside the block ends it (row 4, post-rebind)…
    assert_ne!(
        fa.inferred_type_via_bag("a", tree_sitter::Point { row: 4, column: 8 }),
        widget(),
        "reassignment rebinds a → narrowing truncated by the cutoff",
    );
    // …and it does not leak past the block (row 6).
    assert_ne!(
        fa.inferred_type_via_bag("a", tree_sitter::Point { row: 6, column: 4 }),
        widget(),
        "narrowing scoped to the guard block — gone after",
    );
    // `if (b.has_value())`: same narrowing via the method-form guard (row 8).
    assert_eq!(
        fa.inferred_type_via_bag("b", tree_sitter::Point { row: 8, column: 8 }),
        widget(),
        "`has_value()` narrows the engaged optional to its inner Widget",
    );
}

#[test]
fn a_pack_guard_narrows_every_arm_it_enters() {
    // One region rule for every pack guard: the arm the pattern tags
    // `@narrow.arm`, whatever construct holds it, and nothing outside it.
    let src = "\
void f(Base* b, std::optional<Widget> o) {
    if (o) return o->run();
    int i = dynamic_cast<Derived*>(b) ? b->id() : 0;
    bool ok = o.has_value() && o->ok();
    b->tail();
}
";
    let fa = cpp_skel(src).into_file_analysis();
    use crate::model::file_analysis::InferredType;
    let at = |v: &str, row, column| fa.inferred_type_via_bag(v, tree_sitter::Point { row, column });
    let class = |c: &str| Some(InferredType::ClassName(c.into()));
    assert_eq!(at("o", 1, 18), class("Widget"), "an `if` arm that is no block");
    assert_eq!(at("b", 2, 40), class("Derived"), "a ternary's consequence");
    assert_ne!(at("b", 2, 50), class("Derived"), "not its alternative");
    assert_eq!(at("o", 3, 31), class("Widget"), "the right of `&&`");
    assert_ne!(at("b", 4, 4), class("Derived"), "nothing after the statement");

    let src = "\
class Foo:
    pass
x = maybe()
a = x.run() if isinstance(x, Foo) else x.other()
b = isinstance(x, Foo) and x.run()
if cond():
    pass
elif isinstance(x, Foo):
    x.run()
x.tail()
";
    let mut parser = python_parser();
    let tree = parser.parse(src, None).unwrap();
    let fa = extract(&tree, src.as_bytes(), &python_pack()).unwrap().into_file_analysis();
    let at = |row, column| fa.inferred_type_via_bag("x", tree_sitter::Point { row, column });
    assert_eq!(at(3, 4), class("Foo"), "the value a conditional expression yields when the test holds");
    assert_ne!(at(3, 39), class("Foo"), "not its else value");
    assert_eq!(at(4, 27), class("Foo"), "the right of `and`");
    assert_eq!(at(8, 4), class("Foo"), "an `elif` body");
    assert_ne!(at(9, 0), class("Foo"), "nothing after");
}

#[test]
fn cpp_optional_same_name_narrows_to_own_inner_type_per_function() {
    // GOAL-3 regression: `annot_text_by_var` is keyed by (name, SCOPE), so two
    // functions each with a `std::optional<...> opt` of a DIFFERENT inner type
    // peel the RIGHT T. Pre-fix it was keyed by bare name — last-declaration-
    // wins gave BOTH functions the last one's inner type.
    let src = "\
void f(std::optional<Widget> opt) {
    if (opt) {
        opt->run();
    }
}
void g(std::optional<Gadget> opt) {
    if (opt) {
        opt->go();
    }
}
";
    let fa = cpp_skel(src).into_file_analysis();
    use crate::model::file_analysis::InferredType;
    // f's opt narrows to its own Widget (row 2)…
    assert_eq!(
        fa.inferred_type_via_bag("opt", tree_sitter::Point { row: 2, column: 8 }),
        Some(InferredType::ClassName("Widget".into())),
        "f's opt peels Widget from its OWN std::optional<Widget>",
    );
    // …and g's opt narrows to its own Gadget (row 7), not a sibling's inner type.
    assert_eq!(
        fa.inferred_type_via_bag("opt", tree_sitter::Point { row: 7, column: 8 }),
        Some(InferredType::ClassName("Gadget".into())),
        "g's opt peels Gadget from its OWN std::optional<Gadget> — no cross-fn leak",
    );
}

#[test]
fn cpp_optional_guard_does_not_narrow_non_optional_subject() {
    // The narrowing keys on the subject BEING a std::optional (rule #10, on the
    // type not a name): a bare `if (p)` over a non-optional pointer must not
    // invent an inner type, and `opt.value_or(x)` (not an engagement test) must
    // not narrow even though opt IS optional.
    let src = "\
void f(Widget* p, std::optional<Widget> opt) {
    if (p) {
        p->run();
    }
    if (opt.value_or(0)) {
        opt->run();
    }
}
";
    let fa = cpp_skel(src).into_file_analysis();
    use crate::model::file_analysis::InferredType;
    // `p` stays its declared pointee type (Widget) — NOT re-narrowed to some
    // peeled inner — but critically the bare-if didn't fabricate a bogus type.
    // The load-bearing assertion: opt is not narrowed by a non-engagement call.
    assert_ne!(
        fa.inferred_type_via_bag("opt", tree_sitter::Point { row: 5, column: 8 }),
        Some(InferredType::ClassName("Widget".into())),
        "value_or() is not an engagement guard → opt not narrowed to inner",
    );
    // sanity: `p` is a pointer local, so it types as its pointee Widget from the
    // declaration (unchanged by the bare-if), proving the guard added nothing.
    assert_eq!(
        fa.inferred_type_via_bag("p", tree_sitter::Point { row: 2, column: 8 }),
        Some(InferredType::ClassName("Widget".into())),
        "non-optional pointer keeps its declared pointee type; bare-if is a no-op",
    );
}

#[test]
fn cpp_pointer_declared_vars_get_their_pointee_type() {
    // `T* p;` and the dynamic_cast condition-form both type the var to
    // the pointee class (pointer-ness dropped for navigation).
    let src = "\
void f(Base* b) {
    Widget* w;
    if (Derived* d = dynamic_cast<Derived*>(b)) {
        d->go();
    }
    w->run();
}
";
    let fa = cpp_skel(src).into_file_analysis();
    use crate::model::file_analysis::InferredType;
    // w at its use (row 5)
    assert_eq!(
        fa.inferred_type_via_bag("w", tree_sitter::Point { row: 5, column: 4 }),
        Some(InferredType::ClassName("Widget".into())),
        "T* w typed to Widget",
    );
    // d declared in the if-condition, used in the block (row 3)
    assert_eq!(
        fa.inferred_type_via_bag("d", tree_sitter::Point { row: 3, column: 8 }),
        Some(InferredType::ClassName("Derived".into())),
        "dynamic_cast'd d typed to Derived",
    );
}

#[test]
fn cpp_reference_declared_var_types_to_referent() {
    let src = "void f(Widget& src) {\n    Widget& r = src;\n    r.run();\n}\n";
    let fa = cpp_skel(src).into_file_analysis();
    use crate::model::file_analysis::InferredType;
    assert_eq!(
        fa.inferred_type_via_bag("r", tree_sitter::Point { row: 2, column: 4 }),
        Some(InferredType::ClassName("Widget".into())),
    );
}

#[test]
fn cpp_member_op_mismatches_drive_off_deref_depth() {
    use crate::model::file_analysis::MemberOp;
    // p: Box* (wants ->) accessed with `.` → mismatch.
    // b: Box   (wants .)  accessed with `->` → mismatch.
    // pp: Box** (DEEP — `(*pp)->`) accessed with `.` → NO fix (show-only).
    let src = "\
struct Box { int w; };
void f() {
    Box* p;
    Box b;
    Box** pp;
    p.w;
    b->w;
    pp.w;
}
";
    let fa = cpp_skel(src).into_file_analysis();
    // op-DX rides the MethodCall refs now (p.w, b->w, pp.w each mint one with
    // a `member_op`); the mismatch query joins each with its receiver's stack.
    let mm = fa.member_op_mismatches();
    assert_eq!(mm.len(), 2, "p and b mismatch; pp is DEEP/show-only: {mm:?}");

    // p.w → expected Arrow, typed Dot
    let p = mm.iter().find(|m| m.op_span.start.row == 5).expect("p.w mismatch");
    assert_eq!(p.typed, MemberOp::Dot);
    assert_eq!(p.expected, MemberOp::Arrow);

    // b->w → expected Dot, typed Arrow
    let b = mm.iter().find(|m| m.op_span.start.row == 6).expect("b->w mismatch");
    assert_eq!(b.typed, MemberOp::Arrow);
    assert_eq!(b.expected, MemberOp::Dot);

    // pp.w (row 7) yields no mismatch — DEEP.
    assert!(mm.iter().all(|m| m.op_span.start.row != 7), "pp is show-only");
}

#[test]
fn cpp_dangling_arrow_keeps_provable_mismatches() {
    // A mid-edit dangling `q->` (nothing after) mints no member ref of its own.
    // The still-PROVABLE mismatches must survive — a prior line in the same
    // function AND a later, separate function whose recovery is anchored by the
    // intervening `}`. (A mismatch whose receiver DECLARATION the dangling
    // expression greedily consumes is genuinely unprovable in the recovered
    // tree — its type is gone — and is left out rather than guessed: that
    // narrow loss is a recovered-tree limitation, not a bug.)
    let src = "\
struct Box { int w; };
void f() {
    Box* p;
    p.w;
    Box* q;
    q->
}
void g() {
    Box* r;
    r.w;
}
";
    let fa = cpp_skel(src).into_file_analysis();
    let mm = fa.member_op_mismatches();
    assert!(
        mm.iter().any(|m| m.op_span.start.row == 3),
        "prior-line p.w mismatch survives the dangling `q->`: {mm:?}",
    );
    assert!(
        mm.iter().any(|m| m.op_span.start.row == 9),
        "next-function r.w mismatch survives (recovery anchored by `}}`): {mm:?}",
    );
}

#[test]
fn cpp_deep_receiver_gets_peel_hint() {
    // `OP** op_p; op_p->m` (and `op_p.m`) can't be fixed by a token swap —
    // `op_p->` dereferences one level to an `OP*`, still not a struct. The
    // hint suggests the peeled receiver `(*op_p)`. Show-only, no mismatch entry.
    let src = "\
struct Box { int w; };
void f() {
    Box** pp;
    Box*** ppp;
    Box* p;
    pp->w;
    ppp.w;
    p->w;
}
";
    let fa = cpp_skel(src).into_file_analysis();

    let peels = fa.member_op_deep_accesses();
    // pp (depth 2) and ppp (depth 3) each get a peel; p (depth 1) does not.
    assert_eq!(peels.len(), 2, "pp + ppp peel; p is single-level: {peels:?}");

    let pp = peels.iter().find(|p| p.op_span.start.row == 5).expect("pp->w peel");
    assert_eq!(pp.wrap, "(*pp)");
    assert_eq!(pp.depth, 2);

    let ppp = peels.iter().find(|p| p.op_span.start.row == 6).expect("ppp.w peel");
    assert_eq!(ppp.wrap, "(**ppp)");
    assert_eq!(ppp.depth, 3);

    // The peel partition is disjoint from the swap partition: neither pp nor
    // ppp appears as a mismatch, and p (a real single-level swap needing `->`,
    // written `->` correctly here) yields neither.
    let mm = fa.member_op_mismatches();
    assert!(mm.is_empty(), "no single-level mismatch in this fixture: {mm:?}");

    // The LSP projection carries the peel code + no quick-fix data.
    let diags = crate::lsp::symbols::pack_member_op_peel_diagnostics(&fa);
    assert_eq!(diags.len(), 2);
    assert!(diags.iter().all(|d| matches!(
        &d.code,
        Some(tower_lsp::lsp_types::NumberOrString::String(s)) if s == "member-access-peel"
    )));
    assert!(diags.iter().all(|d| d.data.is_none()), "show-only: no auto-fix data");
}

#[test]
fn cpp_move_in_scopeless_operator_body_does_not_leak_to_sibling() {
    // GOAL-1 regression: the `operator[]` body mints its OWN @scope now (the
    // universal `(function_definition) @scope`). Before, operator/cast/dtor
    // shapes minted none, so a `std::move` inside one attributed to the
    // enclosing CLASS scope — and its moved-from region covered every sibling
    // method, false-flagging their reads of a same-named var. The move here is
    // inside `operator[]` (a shape that had no scope); `b()`'s read of `x` must
    // NOT be flagged.
    let src = "\
struct S {
  int operator[](int i) {
    Widget x;
    sink(std::move(x));
    return 0;
  }
  void b() {
    x.use();
  }
};
";
    let fa = cpp_skel(src).into_file_analysis();
    let diags = crate::lsp::symbols::pack_use_after_move_diagnostics(&fa);
    assert_eq!(
        diags.len(),
        0,
        "operator[]'s move is scoped to its own body — no leak to sibling b(): {diags:?}",
    );
}

// ---- Domain typing (int-used-as-enum): a storage slot's DOMAIN recovered
// from usage. `op_type` is stored `uint16_t` but always compared against
// `enum opcode` values — the sites fold onto the language-generic
// `Field{owner, name}` subject via `DomainCoherenceFold`. ----

#[test]
fn cpp_domain_typing_field_folds_to_enum() {
    let src = "\
enum opcode { OP_NULL, OP_CONST, OP_SCOPE };
struct op { enum opcode op_type; };
void a(struct op* o) { if (o->op_type == OP_CONST) { } }
void b(struct op* o) { if (o->op_type == OP_SCOPE) { } }
";
    let fa = cpp_skel(src).into_file_analysis();
    // Both comparison sites captured as raw domain evidence.
    assert_eq!(fa.pack.domain_sites.len(), 2, "sites: {:?}", fa.pack.domain_sites);
    // The slot's usage-recovered domain is the enum, folded over ≥2 functions,
    // owner resolved to the declaring struct.
    let dom = fa
        .field_domain_for_owner("op", "op_type", None)
        .expect("op_type has a recovered domain");
    assert_eq!(dom.domain, "opcode");
    assert!(dom.confidence > 0.99, "coherent: {}", dom.confidence);
    // Same answer through the ancestor-walked owner (the hover/goto-def path).
    assert_eq!(
        fa.field_domain("op", "op_type", None).map(|d| d.domain),
        Some("opcode".to_string()),
    );
    // Reverse bridge: the enum surfaces the field's sites (gd/gr symmetry).
    assert_eq!(fa.field_sites_for_enum("opcode", None).len(), 2);
    // An unrelated enum surfaces nothing.
    assert!(fa.field_sites_for_enum("nonesuch", None).is_empty());
}

#[test]
fn cpp_domain_typing_mixed_slot_stays_none() {
    // A slot compared mostly against non-enum values (raw ints) has no
    // coherent domain — the majority vote refuses to commit.
    let src = "\
enum opcode { OP_NULL, OP_CONST };
struct op { int op_type; };
void a(struct op* o) { if (o->op_type == 0) { } }
void b(struct op* o) { if (o->op_type == 1) { } }
void c(struct op* o) { if (o->op_type == OP_CONST) { } }
";
    let fa = cpp_skel(src).into_file_analysis();
    // ALL three interactions are sites — the `== 0` / `== 1` number-literal
    // operands are counter-evidence (value capture is ungated), so the one
    // enum site is 1/3: below the strict majority over the honest total.
    assert_eq!(fa.pack.domain_sites.len(), 3, "sites: {:?}", fa.pack.domain_sites);
    assert_eq!(fa.field_domain_for_owner("op", "op_type", None), None);
}

#[test]
fn cpp_domain_typing_owner_gates_same_named_fields() {
    // Two structs each declare an `int kind`; only basket's is used against
    // the enum. The vote is owner-gated at gather time — crate's kind must
    // not inherit basket's domain through the shared slot NAME.
    let src = "\
enum fruit { APPLE, BANANA, CHERRY };
struct basket { int kind; };
struct crate { int kind; };
int a(struct basket* b) { return b->kind == APPLE; }
int b2(struct basket* b) { return b->kind == BANANA; }
int c(struct crate* c) { return c->kind == 3; }
int d(struct crate* c) { return c->kind == 7; }
";
    let fa = cpp_skel(src).into_file_analysis();
    assert_eq!(
        fa.field_domain("basket", "kind", None).map(|d| d.domain),
        Some("fruit".to_string()),
    );
    assert_eq!(fa.field_domain("crate", "kind", None), None, "no cross-struct pooling");
}

#[test]
fn cpp_domain_vote_never_answers_the_slot_value() {
    // A slot with no usage sites has no domain, even with an index that
    // reaches the slot's declared value: hover showed `argv: robj` (the
    // value, minus its stars) as a "domain" for `robj **argv`.
    let src = "\
struct robj { int type; };
struct client { struct robj **argv; };
void f(struct client* c) { struct robj *o = c->argv[1]; }
";
    let fa = std::sync::Arc::new(cpp_skel(src).into_file_analysis());
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.register_symbols(std::path::PathBuf::from("/t/client.cpp"), fa.clone());
    assert_eq!(fa.field_domain("client", "argv", Some(&idx)), None);
}

// ---- C++ template extraction hygiene (specializations, instantiations,
// out-of-line members, aliases, concepts, unions) ----

fn cpp_fa(src: &str) -> crate::model::file_analysis::FileAnalysis {
    let mut parser = cpp_parser();
    let tree = parser.parse(src, None).unwrap();
    extract(&tree, src.as_bytes(), &cpp_pack())
        .unwrap()
        .into_file_analysis()
}

/// An analysis is of its pack's language however it was assembled — through
/// the driver, straight from extraction, or as the stand-in for a file that
/// never parsed — because `resolve()` routes visibility on that stamp.
#[test]
fn assembly_stamps_the_packs_language() {
    assert_eq!(cpp_fa("int f();").language, "cpp");
    let empty = SkeletonAnalysis::for_pack(&cpp_pack()).into_file_analysis();
    assert_eq!(empty.language, "cpp");
}

/// A member token with an argument list beside it is a call; without one
/// it reads a value — spaced or not, the extractor states which.
#[test]
fn cpp_member_read_is_a_field_access_and_a_member_call_is_a_method_call() {
    use crate::model::file_analysis::RefKind;
    let fa = cpp_fa(
        "struct S { int n; int f(int); };\n\
         int g(S s) { return s.n + s.f(1) + s.f (2); }\n",
    );
    let kinds = |name: &str| -> Vec<(bool, Option<usize>)> {
        fa.refs()
            .iter()
            .filter(|r| r.target_name == name && r.member_site().is_some())
            .map(|r| (matches!(r.kind, RefKind::FieldAccess { .. }), r.arg_count))
            .collect()
    };
    assert_eq!(kinds("n"), vec![(true, None)], "an uncalled member reads a value");
    assert_eq!(
        kinds("f"),
        vec![(false, Some(1)), (false, Some(1))],
        "a called member is a call, with or without space before its arguments"
    );
}

/// Each call's `Invoke`: (callee, defined in this file, which arguments
/// spread), in source order; `None` args when the call captured no list.
fn call_invokes(fa: &crate::model::file_analysis::FileAnalysis) -> Vec<(String, bool, Option<Vec<bool>>)> {
    use crate::model::witnesses::{CallArg, Callee, WitnessPayload};
    fa.witnesses
        .all()
        .iter()
        .filter_map(|w| match &w.payload {
            WitnessPayload::Invoke { callee: Callee::Function { name, local, .. }, args, .. } => Some((
                name.clone(),
                local.is_some(),
                args.as_ref().map(|a| a.iter().map(|a| matches!(a, CallArg::Spread(_))).collect()),
            )),
            _ => None,
        })
        .collect()
}

/// A call's value is an `Invoke` of its callee on its written arguments: a
/// comment between two arguments is not one, a pack expansion spreads, and a
/// callee this file doesn't define is left for the cross-file lookup.
#[test]
fn cpp_call_is_an_invoke_on_its_arguments() {
    let fa = cpp_fa(
        "int f(int a, int b);\n\
         template <class... T> int g(T... args) { return f(1 /* one */, 2) + f(args...) + h(3); }\n",
    );
    assert_eq!(
        call_invokes(&fa),
        vec![
            ("f".into(), true, Some(vec![false, false])),
            ("f".into(), true, Some(vec![true])),
            ("h".into(), false, Some(vec![false])),
        ]
    );
}

#[test]
fn python_call_is_an_invoke_on_its_arguments() {
    let (fa, _) = python_fa("def f(*a, **k):\n    return 1\n\nx = f(1, *rest, **kw)\ny = f(i for i in rest)\n");
    assert_eq!(
        call_invokes(&fa),
        vec![("f".into(), true, Some(vec![false, true, true])), ("f".into(), true, None)],
        "a starred argument spreads; a generator argument is no argument list"
    );
}

/// A member call is an `Invoke` of the member on its receiver, and a member
/// read projects the receiver's slot, so a chain types hop by hop through
/// a flow edge with no intermediate variable.
#[test]
fn cpp_member_chains_type_through_their_receivers() {
    use crate::model::file_analysis::InferredType;
    let fa = cpp_fa(
        "\
struct Engine {
    int spin() { return 7; }
};
struct Widget {
    Engine get() { return Engine(); }
    Engine motor;
};
template <typename T>
struct Box { T item_; };
int f(Widget w, Box<int> b) {
    auto x = w.get().spin();
    auto e = w.get();
    auto m = w.motor;
    auto n = w.motor.spin();
    auto i = b.item_;
    return x;
}
",
    );
    let at = tree_sitter::Point { row: 15, column: 4 };
    let ty = |v: &str| fa.inferred_type_via_bag(v, at);
    assert_eq!(ty("e"), Some(InferredType::ClassName("Engine".into())), "one call hop");
    assert_eq!(ty("x"), Some(InferredType::Numeric), "two call hops");
    assert_eq!(ty("m"), Some(InferredType::ClassName("Engine".into())), "a field read");
    assert_eq!(ty("n"), Some(InferredType::Numeric), "a call on a field read");
    assert_eq!(ty("i"), Some(InferredType::ClassName("int".into())), "a template-typed field substitutes");
}

/// A bare name in a member body is the class's member when one answers,
/// inherited ones included, and the member hides a free function of the
/// same name.
#[test]
fn cpp_bare_names_in_a_member_body_reach_inherited_members() {
    use crate::model::file_analysis::InferredType;
    let fa = cpp_fa(
        "\
struct Engine { int spin() { return 7; } };
struct Base {
    Engine motor;
    Engine make() { return Engine(); }
};
int make();
struct Car : Base {
    int drive() {
        auto m = motor;
        auto e = make();
        auto n = make().spin();
        return n;
    }
};
",
    );
    let at = tree_sitter::Point { row: 11, column: 8 };
    let ty = |v: &str| fa.inferred_type_via_bag(v, at);
    assert_eq!(ty("m"), Some(InferredType::ClassName("Engine".into())), "an inherited field");
    assert_eq!(ty("e"), Some(InferredType::ClassName("Engine".into())), "the member hides `int make()`");
    assert_eq!(ty("n"), Some(InferredType::Numeric), "a chain off an implicit call");
}

#[test]
fn python_member_call_is_an_invoke_on_its_receiver() {
    use crate::model::witnesses::{CallArg, Callee, ProjectionStep, WitnessPayload};
    let (fa, _) = python_fa("w = make()\nx = w.get(1, *rest)\ny = w.size\n");
    let calls: Vec<_> = fa
        .witnesses
        .all()
        .iter()
        .filter_map(|w| match &w.payload {
            WitnessPayload::Invoke { callee: Callee::Method { name, .. }, receiver, args } => Some((
                name.clone(),
                receiver.is_some(),
                args.as_ref().map(|a| a.iter().map(|a| matches!(a, CallArg::Spread(_))).collect::<Vec<_>>()),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(calls, vec![("get".into(), true, Some(vec![false, true]))]);
    let reads: Vec<_> = fa
        .witnesses
        .all()
        .iter()
        .filter_map(|w| match &w.payload {
            WitnessPayload::Projected { step: ProjectionStep::ValueHop { member }, .. } => Some(member.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(reads, vec!["size".to_string()]);
}

/// C++ classes hold ONE member namespace: the nearest declaration of a
/// name answers a call and a value read alike, so a derived member
/// function hides a base data member of its name for both asks, and a
/// call reaches a data member (`d.hook()` calls what `hook` holds).
#[test]
fn cpp_member_asks_answer_the_nearest_declaration_of_either_family() {
    use crate::model::file_analysis::MethodResolution;
    let fa = cpp_fa(
        "struct Base { int hook; int count; };\n\
         struct Derived : Base { void hook(); };\n",
    );
    let class_of = |r: Option<MethodResolution>| match r {
        Some(MethodResolution::Local { class, .. }) => Some(class),
        other => panic!("expected a local hit, got {other:?}"),
    };
    assert_eq!(class_of(fa.resolve_field_in_ancestors("Derived", "hook", None)), Some("Derived".into()));
    assert_eq!(class_of(fa.resolve_method_in_ancestors("Derived", "hook", None)), Some("Derived".into()));
    assert_eq!(class_of(fa.resolve_method_in_ancestors("Derived", "count", None)), Some("Base".into()));
}

/// An EXPLICIT declared type governs member dispatch for EVERY C++
/// initializer shape — `T x = {…}`, `T x{…}`, `T x;` — the braced-init
/// twin of the annotation-priority fix. The initializer's literals mint a
/// `Numeric` flow witness (priority 10); the declared container mints an
/// `Annotation(Declared)` witness (priority 20). Before the plain-`InferredType`
/// axis learned to break ties on source priority, the later flow witness
/// clobbered the annot (latest-wins) and the variable hovered `Numeric`.
#[test]
fn cpp_braced_init_declared_type_governs_over_flow() {
    use crate::model::file_analysis::InferredType;
    let fa = cpp_fa(
        "template <class K, class V> struct FlatMap { V at(K k); };\n\
         int main() {\n\
         FlatMap<int, int> m = {{1, 7}, {2, 9}};\n\
         FlatMap<int, int> d{{3, 4}};\n\
         FlatMap<int, int> n;\n\
         return 0;\n\
         }\n",
    );
    let pt = tree_sitter::Point { row: 6, column: 0 };
    for var in ["m", "d", "n"] {
        let t = fa.inferred_type_via_bag(var, pt);
        assert!(
            matches!(t, Some(InferredType::Parametric(_))),
            "{var}: the declared container must govern every initializer shape, got {t:?}"
        );
        assert_ne!(
            t,
            Some(InferredType::Numeric),
            "{var}: braced-init flow must not clobber the declared type"
        );
    }
}

/// Class dedup keys on (name, span), not name alone: an
/// EXACT re-capture of the same node (a class with a base clause matches
/// both the bodied pattern and the inheritance pattern in skeleton.scm)
/// still collapses to one entry, but two GENUINELY DISTINCT classes that
/// happen to share a bare name in different namespaces must both survive —
/// the old name-only dedup silently dropped the second one.
#[test]
fn class_dedup_keys_on_name_and_span_not_name_alone() {
    // same-span double-capture (bodied + inheritance patterns, one node)
    // still collapses to a single `Circle`.
    let fa = cpp_fa(
        "class Shape { public: int x; };\n\
         class Circle : public Shape { public: int r; };\n",
    );
    let circles: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, crate::model::file_analysis::SymKind::Class) && s.name == "Circle")
        .collect();
    assert_eq!(circles.len(), 1, "one node, two matching patterns, still one symbol");

    // two DIFFERENT classes sharing a bare name in different namespaces
    // both survive (previously the second `Node` vanished).
    let fa2 = cpp_fa("namespace A { struct Node { int a; }; }\nnamespace B { struct Node { int b; }; }\n");
    let nodes: Vec<_> = fa2
        .symbols()
        .iter()
        .filter(|s| matches!(s.kind, crate::model::file_analysis::SymKind::Class) && s.name == "Node")
        .collect();
    assert_eq!(nodes.len(), 2, "distinct classes at distinct spans must both survive: {nodes:?}");
}

#[test]
fn canonical_template_spelling_normalizes_whitespace() {
    assert_eq!(canonical_template_spelling("formatter"), "formatter");
    assert_eq!(canonical_template_spelling("formatter<int,char>"), "formatter<int, char>");
    assert_eq!(
        canonical_template_spelling("formatter< int ,\n  char >"),
        "formatter<int, char>"
    );
    assert_eq!(canonical_template_spelling("Buf<T *>"), "Buf<T*>");
    // a load-bearing space between word tokens survives
    assert_eq!(
        canonical_template_spelling("Buf<unsigned long>"),
        "Buf<unsigned long>"
    );
    assert_eq!(canonical_template_spelling("Buf< Buf<int> >"), "Buf<Buf<int>>");
}

#[test]
fn cpp_annot_type_peels_template_spellings_to_instance() {
    use crate::model::file_analysis::{InferredType, ParametricType};
    let at = cpp_pack().annot_type;
    match at("Box<Widget>") {
        Some(InferredType::Parametric(ParametricType::Instance { base, .. })) => {
            assert_eq!(base, "Box")
        }
        other => panic!("expected Instance, got {other:?}"),
    }
    // an embedded-space arg used to fail the typeish gate entirely
    assert!(matches!(at("Buf<unsigned long>"), Some(InferredType::Parametric(_))));
    // non-template spellings unchanged
    assert!(matches!(at("Widget"), Some(InferredType::ClassName(c)) if c == "Widget"));
    assert!(matches!(at("int"), Some(InferredType::Numeric)));
}

#[test]
fn cpp_template_instance_member_gd_dispatches_on_base_or_exact_spec() {
    // Row 11 of the template arc: `Box<Widget> b; b.size()` resolves the
    // member through the Instance's BASE with zero template-specific
    // resolution code; an instance whose exact canonical spelling names a
    // per-spec class dispatches THERE instead (exact-or-primary only).
    let src = "\
template <typename T>
class Box {
public:
    T get();
    int size();
    T v_;
};
template <typename T> struct codec {
    void parse();
};
template <> struct codec<int> {
    void pack_int();
};
struct Widget { int w_; };
void use_box() {
    Box<Widget> b;
    b.size();
    codec<int> ci;
    ci.pack_int();
    codec<char> cc;
    cc.parse();
}
";
    let fa = cpp_fa(src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let uri = tower_lsp::lsp_types::Url::from_file_path("/fake/cpp/box.cpp").unwrap();
    let store = crate::index::open_docs::OpenDocs::new();
    let gd = |line: u32, character: u32| {
        match crate::lsp::symbols::find_definition(
            &store,
            &fa,
            tower_lsp::lsp_types::Position { line, character },
            &uri,
            &idx,
        ) {
            Some(tower_lsp::lsp_types::GotoDefinitionResponse::Scalar(l)) => {
                Some((l.range.start.line, l.range.start.character))
            }
            None => None,
            other => panic!("expected a single location, got {other:?}"),
        }
    };
    assert_eq!(gd(16, 6), Some((4, 8)), "b.size() lands on Box::size via the base");
    assert_eq!(gd(18, 7), Some((11, 9)), "codec<int> instance keys the exact-spelling spec");
    assert_eq!(gd(20, 7), Some((8, 9)), "codec<char> (no spec) falls to the primary");
}

#[test]
fn cpp_class_specialization_mints_per_spec_class_with_owned_members() {
    let fa = cpp_fa(
        r#"
template <typename T, typename Char> struct formatter { int parse(int ctx); };
template <> struct formatter<int, char> { int fmt_full(); };
template <typename T> struct formatter<T*, char> { int fmt_partial(); };
"#,
    );
    let class = |n: &str| {
        fa.symbols()
            .iter()
            .find(|s| s.name == n && matches!(s.kind, crate::model::file_analysis::SymKind::Class))
    };
    assert!(class("formatter").is_some(), "primary still extracts");
    assert!(class("formatter<int, char>").is_some(), "full spec is its own Class");
    assert!(class("formatter<T*, char>").is_some(), "partial spec is its own Class");
    // members are OWNED by the spec (package = the canonical spec name)
    let member = |n: &str| fa.symbols().iter().find(|s| s.name == n).unwrap();
    assert_eq!(member("fmt_full").package.as_deref(), Some("formatter<int, char>"));
    assert_eq!(member("fmt_partial").package.as_deref(), Some("formatter<T*, char>"));
    assert_eq!(member("parse").package.as_deref(), Some("formatter"));
    // the family edges (spec → primary), NEVER an inheritance edge
    assert_eq!(fa.pack.specializes.get("formatter<int, char>").map(String::as_str), Some("formatter"));
    assert_eq!(fa.pack.specializes.get("formatter<T*, char>").map(String::as_str), Some("formatter"));
    assert!(
        fa.declared_parents("formatter<int, char>").is_empty(),
        "specialization is not inheritance — member resolution must not fall through"
    );
}

#[test]
fn cpp_out_of_line_template_member_joins_base_class() {
    let fa = cpp_fa(
        r#"
template <typename T> struct Buf { void grow(int n); };
template <typename T> void Buf<T>::grow(int n) { int local_g = n; }
"#,
    );
    let grows: Vec<_> = fa.symbols().iter().filter(|s| s.name == "grow").collect();
    assert_eq!(grows.len(), 2, "in-class decl + out-of-line def");
    for g in &grows {
        assert_eq!(
            g.package.as_deref(),
            Some("Buf"),
            "the template qualifier peels to the base class — decl and def unify"
        );
    }
}

/// Out-of-line definitions whose declarator or qualifier the narrow per-shape
/// patterns missed (hitlist H7-2): a pointer/reference return wraps the
/// function_declarator in a `pointer_declarator`; a nested class owner nests the
/// `qualified_identifier`; a constructor/destructor has no return type at all.
/// The general `@ool.def` capture + the driver's canonical declarator unwrap +
/// qualifier walk mint the method with its OWNING class (the innermost `::`
/// segment), not the enclosing namespace.
#[test]
fn cpp_out_of_line_pointer_return_def_extracted() {
    let skel = cpp_skel("Regexp* Regexp::Simplify() { return 0; }\n");
    let m = skel
        .symbols
        .iter()
        .find(|s| s.name == "Simplify")
        .expect("pointer-returning out-of-line def is extracted");
    assert_eq!(m.kind, DefCap::Method);
    assert_eq!(m.package.as_deref(), Some("Regexp"));
}

#[test]
fn cpp_out_of_line_multilevel_qualifier_def_owns_inner_class() {
    // `Prog::Inst::InitAlt` — the qualified_identifier nests (scope: Prog,
    // name: (Inst::InitAlt)); the owner is the INNERMOST class `Inst`.
    let skel = cpp_skel("void Prog::Inst::InitAlt(int a) { }\n");
    let m = skel
        .symbols
        .iter()
        .find(|s| s.name == "InitAlt")
        .expect("multi-level-qualified out-of-line def is extracted");
    assert_eq!(m.package.as_deref(), Some("Inst"));
    assert!(m.qualifier_owned, "package came from the `::` qualifier");
}

#[test]
fn cpp_out_of_line_constructors_extracted() {
    // A constructor has NO return type (`!type`); the multi-level form owns the
    // inner class.
    let skel = cpp_skel("RE2::RE2(const char* p) { }\nRE2::Options::Options(int x) { }\n");
    let ctor = skel.symbols.iter().find(|s| s.name == "RE2" && s.kind == DefCap::Method);
    assert_eq!(ctor.map(|s| s.package.as_deref()), Some(Some("RE2")));
    let nested = skel.symbols.iter().find(|s| s.name == "Options" && s.kind == DefCap::Method);
    assert_eq!(nested.map(|s| s.package.as_deref()), Some(Some("Options")));
}

#[test]
fn cpp_out_of_line_arbitrary_declarator_nesting_and_qualifier_depth() {
    // Double-pointer return (`Foo**`) + 3-level qualifier — the S-query can't
    // enumerate this shape; the driver's unwrap + walk reach it by construction.
    let skel = cpp_skel("Foo** Outer::Mid::Inner::make() { return 0; }\n");
    let m = skel
        .symbols
        .iter()
        .find(|s| s.name == "make")
        .expect("nested-pointer + 3-level qualifier out-of-line def is extracted");
    assert_eq!(m.package.as_deref(), Some("Inner"));
}

/// F8: an out-of-line method whose owning class is declared in a HEADER (absent
/// here) is NOT a truncation fall-through — the `::` qualifier is authoritative,
/// so re-anchoring must leave `RE2::Init`'s package as `RE2`, not upgrade it to
/// the enclosing `re2` namespace (the bug: an absent local container read as
/// "non-computable scope → recover").
#[test]
fn reanchor_keeps_qualifier_owner_when_class_body_absent() {
    let src = "namespace re2 {\nvoid RE2::Init(const char* p) { }\n}\n";
    let mut init = sksym(src, DefCap::Method, "Init", 0, Some("RE2"));
    init.qualifier_owned = true;
    let mut skel = SkeletonAnalysis::default();
    skel.symbols = vec![sksym(src, DefCap::Package, "re2", 0, None), init];
    skel.reanchor_truncated_containers(src);
    let got = skel.symbols.iter().find(|s| s.name == "Init").unwrap();
    assert_eq!(got.package.as_deref(), Some("RE2"), "qualifier owner survives re-anchor");
}

#[test]
fn cpp_explicit_instantiation_outline_entry_and_no_param_leak() {
    let fa = cpp_fa(
        r#"
template <typename T> struct Buf { void grow(int n); };
template struct Buf<int>;
template void Buf<float>::grow(int n2);
template auto thousands_sep_impl(int loc) -> int;
"#,
    );
    use crate::model::file_analysis::SymKind;
    // enumerable outline items (fork 2): class + method + function forms
    assert!(fa
        .symbols()
        .iter()
        .any(|s| s.name == "Buf<int>" && matches!(s.kind, SymKind::Class)));
    assert!(fa
        .symbols()
        .iter()
        .any(|s| s.name == "grow"
            && matches!(s.kind, SymKind::Method)
            && s.package.as_deref() == Some("Buf")));
    assert!(fa
        .symbols()
        .iter()
        .any(|s| s.name == "thousands_sep_impl" && matches!(s.kind, SymKind::Sub)));
    // an instantiation is NOT a specialization — no family edge
    assert!(!fa.pack.specializes.contains_key("Buf<int>"));
    // signature params live in a sub-body scope, out of the outline
    for leak in ["n2", "loc"] {
        let sym = fa.symbols().iter().find(|s| s.name == leak).unwrap();
        assert!(
            fa.scope_within_sub_body(sym.scope),
            "{leak} must not float to the outline"
        );
    }
}

#[test]
fn cpp_function_scopes_shield_params_and_locals_from_outline() {
    let fa = cpp_fa(
        r#"
struct Point { int x; int y; };
int compute(int a) { int b = a; return b; }
struct W { int parse(int ctx); };
"#,
    );
    for local in ["a", "b", "ctx"] {
        let sym = fa.symbols().iter().find(|s| s.name == local).unwrap();
        assert!(
            fa.scope_within_sub_body(sym.scope),
            "{local} is sub-body content"
        );
    }
    // fields still surface (class-body scope is NOT a sub body)
    for field in ["x", "y"] {
        let sym = fa.symbols().iter().find(|s| s.name == field).unwrap();
        assert!(!fa.scope_within_sub_body(sym.scope));
        assert!(fa.symbol_is_class_content(sym), "{field} is class content");
    }
    // a method-body local never reads as class content, and a prototype
    // param (sticky class package) doesn't either
    let ctx = fa.symbols().iter().find(|s| s.name == "ctx").unwrap();
    assert!(!fa.symbol_is_class_content(ctx));
}

#[test]
fn cpp_using_alias_and_concept_mint_symbols() {
    let fa = cpp_fa(
        r#"
using byte_alias = unsigned char;
template <typename T> struct Buf { int n; };
template <typename T> using vec_alias = Buf<T>;
template <typename T> concept Addable = requires(T a) { a + a; };
"#,
    );
    use crate::model::file_analysis::SymKind;
    for name in ["byte_alias", "vec_alias", "Addable"] {
        assert!(
            fa.symbols()
                .iter()
                .any(|s| s.name == name && matches!(s.kind, SymKind::Class)),
            "{name} should be a findable type symbol"
        );
    }
    // requires-expr params stop leaking to top level
    let a = fa.symbols().iter().find(|s| s.name == "a").unwrap();
    assert!(fa.scope_within_sub_body(a.scope));
}

#[test]
fn cpp_template_base_inheritance_edges() {
    let fa = cpp_fa(
        r#"
template <typename T> struct base { int common; };
template <typename T> struct D : base<T> { int own; };
"#,
    );
    let parents = fa.declared_parents("D");
    assert!(!parents.is_empty(), "D records parents");
    assert!(
        parents.iter().any(|p| p == "base"),
        "the bare base name joins the primary; got {parents:?}"
    );
    assert!(
        parents.iter().any(|p| p == "base<T>"),
        "the canonical spelling rides along for per-spec joins; got {parents:?}"
    );
}

#[test]
fn cpp_union_members_nest_and_overlay() {
    let fa = cpp_fa(
        r#"
struct pm {
  int op_first;
  union {
    long op_pmreplroot;
    void* op_pmtargetgv;
  };
  union {
    long u2a;
    char u2b;
  } named_u;
};
typedef union {
  int any_i32;
  long any_iv;
} ANY;
"#,
    );
    use crate::model::file_analysis::SymKind;
    let sym = |n: &str| fa.symbols().iter().find(|s| s.name == n).unwrap();
    // containers carry the union attribute; the anonymous one is synthetic
    let anon = sym("(union)");
    assert!(anon.attributes.iter().any(|a| a == "union"));
    assert!(anon.attributes.iter().any(|a| a == "anonymous"));
    let named = sym("named_u");
    assert!(named.attributes.iter().any(|a| a == "union"));
    assert!(matches!(sym("ANY").kind, SymKind::Class));
    assert!(sym("ANY").attributes.iter().any(|a| a == "union"));
    // members keep the STRUCT as package (flat completion/refs identity) …
    assert_eq!(sym("op_pmreplroot").package.as_deref(), Some("pm"));
    assert_eq!(sym("u2a").package.as_deref(), Some("pm"));
    // … but nest under their container structurally
    assert_eq!(fa.union_container_of(sym("op_pmreplroot")).unwrap().name, "(union)");
    assert_eq!(fa.union_container_of(sym("u2b")).unwrap().name, "named_u");
    assert_eq!(fa.union_container_of(sym("any_i32")).unwrap().name, "ANY");
    assert!(fa.union_container_of(sym("op_first")).is_none());
    // the overlay = the sibling group
    let overlay = fa.union_overlay(sym("op_pmreplroot")).unwrap();
    assert_eq!(overlay.len(), 1);
    assert!(overlay[0].starts_with("op_pmtargetgv"), "{overlay:?}");
    // completion: real members offered flat on pm; the synthetic container never
    let cands = fa.complete_members_for_class("pm", None, None);
    let labels: Vec<&str> = cands.iter().map(|c| c.label.as_str()).collect();
    for want in ["op_first", "op_pmreplroot", "op_pmtargetgv", "u2a", "named_u"] {
        assert!(labels.contains(&want), "{want} missing from {labels:?}");
    }
    assert!(!labels.contains(&"(union)"), "synthetic container leaked: {labels:?}");
}



// ---- Instantiation-aware typing (template arc slice (c)) ----

#[test]
fn cpp_template_params_extract_in_declaration_order() {
    let fa = cpp_fa(
        "template <typename T, class U = int>\nclass Box { T get(); };\n\
         template <typename T> struct fmtr<vector<T>, char> { T front(); };\n",
    );
    assert_eq!(
        fa.pack.template_params.get("Box"),
        Some(&vec!["T".to_string(), "U".to_string()]),
        "primary params, declaration order (defaulted param included)"
    );
    assert_eq!(
        fa.pack.template_params.get("fmtr<vector<T>, char>"),
        Some(&vec!["T".to_string()]),
        "a partial spec keys its params under the canonical spelling"
    );
}

#[test]
fn cpp_instance_member_types_substitute_lazily() {
    // The typing ladder, rungs 1 + 2: a member whose type IS a param
    // (`T get()` / `T v_`), and a param one hop under a template spelling
    // (`vector<T> all()`) / behind a trailing return (`-> T*`). All lazy:
    // nothing per-instantiation is materialized — `ParamOf` (methods) and
    // `substitute_type_params` (fields) read the receiver's args at query
    // time.
    use crate::model::file_analysis::{InferredType, ParametricType};
    let fa = cpp_fa(
        "\
template <typename T>
class Box {
public:
    T get();
    vector<T> all();
    auto tail() -> T*;
    T v_;
    int size();
};
",
    );
    let recv = InferredType::Parametric(
        ParametricType::instance_from_spelling("Box<int>").unwrap(),
    );
    let mvt = |m: &str| fa.member_value_type(&recv, m, None, None);
    assert_eq!(mvt("get"), Some(InferredType::ClassName("int".into())), "bare param return");
    assert_eq!(mvt("v_"), Some(InferredType::ClassName("int".into())), "bare param field");
    assert_eq!(
        mvt("all"),
        Some(InferredType::Parametric(
            ParametricType::instance_from_spelling("vector<int>").unwrap()
        )),
        "param one hop under a template spelling"
    );
    assert_eq!(
        mvt("tail"),
        Some(InferredType::ClassName("int".into())),
        "trailing return -> T* substitutes (pointer dropped for navigation)"
    );
    assert_eq!(mvt("size"), Some(InferredType::Numeric), "concrete member unchanged");
    // No receiver args → no invented answer for param-shaped members.
    let bare = InferredType::ClassName("Box".into());
    assert_eq!(fa.member_value_type(&bare, "get", None, None), None);
    assert_eq!(fa.member_value_type(&bare, "size", None, None), Some(InferredType::Numeric));
}

#[test]
fn cpp_field_value_type_never_answers_a_callable() {
    // `field_value_type` is what a `FieldAccess` ref asks: the class's
    // value members only. `member_value_type` is the kind-less ladder for
    // an asker with no ref in hand (the sentinel mid-keystroke).
    use crate::model::file_analysis::InferredType;
    let fa = cpp_fa(
        "\
struct S {
    int v_;
    int size();
};
",
    );
    let s = InferredType::ClassName("S".into());
    assert_eq!(fa.field_value_type(&s, "v_", None), Some(InferredType::Numeric));
    assert_eq!(fa.field_value_type(&s, "size", None), None, "a callable is not a value member");
    assert_eq!(fa.member_value_type(&s, "size", None, None), Some(InferredType::Numeric));
}

#[test]
fn cpp_partial_pattern_spec_dispatch_binds_params() {
    // The selection ladder (fork 4): exact-spelling spec > partial-pattern
    // spec (structural match, params bind) > base primary — and a member
    // query on a partial spec substitutes the PATTERN's bindings, not the
    // primary's positional args.
    use crate::model::file_analysis::{InferredType, ParametricType};
    let fa = cpp_fa(
        "\
template <typename T, typename C> struct codec {
    int parse();
};
template <typename T> struct codec<T*, char> {
    T deref();
};
template <typename T> struct codec<vector<T>, char> {
    T front();
};
template <> struct codec<int, char> {
    int whole();
};
",
    );
    let inst = |s: &str| {
        InferredType::Parametric(ParametricType::instance_from_spelling(s).unwrap())
    };
    // exact beats partial beats primary
    assert_eq!(
        fa.dispatch_class_of(&inst("codec<int, char>"), None).as_deref(),
        Some("codec<int, char>")
    );
    assert_eq!(
        fa.dispatch_class_of(&inst("codec<Widget*, char>"), None).as_deref(),
        Some("codec<T*, char>")
    );
    assert_eq!(
        fa.dispatch_class_of(&inst("codec<vector<int>, char>"), None).as_deref(),
        Some("codec<vector<T>, char>"),
        "nested pattern (the formatter<vector<T>> shape)"
    );
    assert_eq!(
        fa.dispatch_class_of(&inst("codec<double, double>"), None).as_deref(),
        Some("codec"),
        "no spec matches → primary"
    );
    // pattern bindings feed member substitution: T bound THROUGH the shape
    assert_eq!(
        fa.member_value_type(&inst("codec<Widget*, char>"), "deref", None, None),
        Some(InferredType::ClassName("Widget".into()))
    );
    assert_eq!(
        fa.member_value_type(&inst("codec<vector<Widget>, char>"), "front", None, None),
        Some(InferredType::ClassName("Widget".into()))
    );
    // a member the spec doesn't define falls through the ladder to the primary
    assert_eq!(
        fa.member_value_type(&inst("codec<Widget*, char>"), "parse", None, None),
        Some(InferredType::Numeric)
    );
    // the ladder itself is ranked and never pruned
    let ladder: Vec<String> = fa
        .dispatch_ladder_of(&inst("codec<Widget*, char>"), None)
        .into_iter()
        .map(|(c, _)| c)
        .collect();
    assert_eq!(ladder, vec!["codec<T*, char>".to_string(), "codec".to_string()]);
}

#[test]
fn cpp_member_chain_types_through_substituted_returns() {
    // `w.get().spin()` — the invocant `w.get()` types via the pack
    // member-chain arm of `expr_type_at_span` (tree-free), so gd on `spin`
    // resolves on Widget. Chains compose because `get()` answers Widget.
    let src = "\
struct Widget {
    void spin();
};
template <typename T>
class Box {
public:
    T get();
    T v_;
};
void go() {
    Box<Widget> w;
    w.get().spin();
    w.v_.spin();
}
";
    let fa = cpp_fa(src);
    let gd = |line: u32, character: u32| {
        let uri = tower_lsp::lsp_types::Url::from_file_path("/fake/cpp/chain.cpp").unwrap();
        let store = crate::index::open_docs::OpenDocs::new();
        let idx = crate::index::module_index::ModuleIndex::new_for_test();
        match crate::lsp::symbols::find_definition(
            &store,
            &fa,
            tower_lsp::lsp_types::Position { line, character },
            &uri,
            &idx,
        ) {
            Some(tower_lsp::lsp_types::GotoDefinitionResponse::Scalar(l)) => {
                Some((l.range.start.line, l.range.start.character))
            }
            None => None,
            other => panic!("expected a single location, got {other:?}"),
        }
    };
    assert_eq!(gd(11, 13), Some((1, 9)), "w.get().spin() resolves spin on Widget");
    assert_eq!(gd(12, 9), Some((1, 9)), "w.v_.spin() resolves through the field's type");
}

// A comment inside a C++ argument list is not an argument.
#[test]
fn cpp_call_arg_count_skips_comments() {
    let skel = cpp_skel("void f(int, int);\nvoid g() { f(a, /* c */ b); }\n");
    let call = skel.refs.iter().find(|r| r.name == "f").expect("f call ref");
    assert_eq!(call.arg_count, Some(2));
}

// A comment ahead of a parenthesized C++ receiver is peeled with the parens.
// The receiver stays an expression: the parens are how it was written.
#[test]
fn cpp_receiver_peel_skips_comments() {
    let fa = cpp_fa("struct W { void spin(); };\nvoid g() { W w; (/* c */ w).spin(); }\n");
    let call = fa.refs().iter().find(|r| r.target_name == "spin" && r.member_site().is_some()).expect("spin member ref");
    assert_eq!(fa.method_call_invocant_type(call, None), Some(InferredType::ClassName("W".into())));
}

// A receiver in parentheses (or behind `*` / `&`) is the wrapper node
// itself, read through the wrapper's edge to what it holds: the document
// states which nodes hold their content's value, and no Rust table peels
// them a second time. Every pack places the receiver on the same node.
#[test]
fn a_wrapped_receiver_is_read_through_its_wrapper() {
    let fa = cpp_fa("struct W { void spin(); };\nvoid g() { W w; W* p; (w).spin(); (*p).spin(); (&w)->spin(); }\n");
    let calls: Vec<_> = fa.refs().iter().filter(|r| r.target_name == "spin" && r.member_site().is_some()).collect();
    assert_eq!(calls.len(), 3);
    for (call, col) in calls.iter().zip([22, 34, 47]) {
        assert_eq!(call.member_site().unwrap().invocant_span.unwrap().start, Point::new(1, col), "the wrapper is the receiver");
        assert_eq!(fa.method_call_invocant_type(call, None), Some(InferredType::ClassName("W".into())), "col {col}");
    }
    let (fa, _) = python_fa("class W:\n    def spin(self):\n        pass\n\nw = W()\n(w).spin()\n");
    let call = fa.refs().iter().find(|r| r.target_name == "spin" && r.member_site().is_some()).expect("spin");
    assert_eq!(call.member_site().unwrap().invocant_span.unwrap().start, Point::new(5, 0), "the wrapper is the receiver");
    assert_eq!(fa.method_call_invocant_type(call, None), Some(InferredType::ClassName("W".into())));
}

// `this` is the object of the class whose body it is in, not a class named
// `this`: its value types from the eliding body, never from its spelling.
#[test]
fn cpp_this_reads_the_enclosing_class() {
    use crate::build::language_driver::LanguageRegistry;
    let p = std::path::PathBuf::from("/x/this.cpp");
    let src = "class Status {\n void Update() {\n  this->Ref();\n }\n void Ref();\n};\nvoid Status::Ref() { this->Update(); }\n";
    let fa = LanguageRegistry::compiled().for_path(&p).unwrap().analyze_with_path(src, Some(&p));
    for at in [Point::new(2, 8), Point::new(6, 27)] {
        let r = fa.ref_at(at).expect("member call");
        assert_eq!(fa.method_call_invocant_type(r, None), Some(InferredType::ClassName("Status".into())), "{at:?}");
    }
}

// One system types `this`: the token is stated once (`@receiver.enclosing`)
// and its class is the method-class table's, so its value is one witness.
#[test]
fn cpp_this_is_typed_once() {
    use crate::model::witnesses::WitnessAttachment as WA;
    let src = "class S {\n void f() { this->g(); }\n void g();\n};\n";
    let fa = cpp_fa(src);
    let this = crate::model::file_analysis::Span { start: Point::new(1, 12), end: Point::new(1, 16) };
    let typed: Vec<_> = fa.witnesses.for_attachment(&WA::Expr(this)).into_iter().map(|w| w.source.clone()).collect();
    assert_eq!(typed.len(), 1, "one witness types `this`: {typed:?}");
    assert_eq!(fa.expr_type_at_span(this, None), Some(InferredType::ClassName("S".into())));
}

// A comment inside a Python argument list is not an argument.
#[test]
fn python_call_arg_count_skips_comments() {
    let src = "o.f(a,  # c\n    b)\n";
    let mut parser = python_parser();
    let tree = parser.parse(src, None).unwrap();
    let skel = extract(&tree, src.as_bytes(), &python_pack()).unwrap();
    let call = skel.refs.iter().find(|r| r.name == "f" && r.invocant.is_some()).expect("f member call ref");
    assert_eq!(call.arg_count, Some(2));
}
