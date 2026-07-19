//! The layer DAG, enforced. CLAUDE.md's architecture rules #1/#2 say
//! data flows down only and the model never touches the tree; this
//! suite makes a violation a red `cargo test` instead of a review
//! catch. (The alternative — a crate-per-layer workspace — buys the
//! same guarantee from the compiler at the price of five published
//! crates; the executed-and-rejected split lives on branch `workspace-split`.)

use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

/// Layer order — an import may only point at the same layer or lower.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Layer {
    Model = 0,
    Cst = 1,
    Build = 2,
    Index = 3,
    Lsp = 4,
}

/// Module → layer. Every non-test module must be assigned: the
/// `unassigned module` assertion below makes adding a file without
/// placing it in the architecture a test failure, not a drift.
fn layer_map() -> HashMap<&'static str, Layer> {
    use Layer::*;
    HashMap::from([
        ("file_analysis", Model),
        ("witnesses", Model),
        ("surface", Model),
        ("conventions", Model),
        ("graph", Model),
        ("cst", Cst),
        ("builder", Build),
        ("plugin", Build),
        ("pod", Build),
        ("cpanfile", Build),
        ("query_cache", Build),
        ("query_extract", Build),
        // multi-language serving seam (LanguageDriver keystone)
        ("language_driver", Build),
        // test-only C++ macro corpus consumed by query_extract_tests
        ("cpp_obstacle", Build),
        // stratified reparse seam (Perl prototype reparenthesizer spike)
        ("reparse", Build),
        // C++ reparse seam (macro expansion before extraction spike)
        ("cpp_reparse", Build),
        // config-variant macro model: guard trail + reachability + join
        ("cpp_macro_model", Build),
        // zero-config toolchain probe: shell out to cc for stdlib
        // include roots + predefined macros + resource dir (spike)
        ("cpp_toolchain", Build),
        // sentinel re-parse for member-access cursor context
        ("cursor_sentinel", Build),
        // the shared metaprogram-projection engine (worklist + seen-set +
        // root-chained provenance); pure std, importable from any layer
        ("module_index", Index),
        ("module_resolver", Index),
        ("module_cache", Index),
        ("pack_bag_cache", Index),
        ("file_store", Index),
        ("resolve", Index),
        ("document", Index),
        // Leaf instrumentation util (std-only, no crate imports): lives at the
        // bottom so every layer — builder included — may import it downward.
        ("timings", Model),
        ("builtins_pod", Index),
        ("backend", Lsp),
        // process-survival service wrapper: catches handler panics at the
        // request/notification boundary (no crate:: imports, DAG-neutral)
        ("panic_guard", Lsp),
        ("symbols", Lsp),
        ("cursor_context", Lsp),
        // one Slot vocabulary over cursor_context (Perl) + cursor_sentinel
        // (pack); consumers switch on Slot, never on language
        ("cursor_slot", Lsp),
        ("plugin_cli", Lsp),
        ("main", Lsp),
        ("layering_tests", Lsp),
    ])
}

/// Source files per module, relative to `src/`. Test suites are
/// exempt — they deliberately drive lower layers through upper ones.
fn module_sources() -> Vec<(&'static str, Vec<PathBuf>)> {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out: Vec<(&'static str, Vec<PathBuf>)> = Vec::new();
    let map = layer_map();
    for entry in fs::read_dir(&src).expect("read src/") {
        let path = entry.expect("dir entry").path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()).map(str::to_string)
        else {
            continue;
        };
        if path.is_dir() {
            // A module directory (`plugin/`, `builder/`) contributes its
            // `.rs` files to that module's layer — same enforcement as the
            // top-level `<module>.rs`, so a submodule can't dodge the DAG.
            if let Some(name) = map.keys().copied().find(|k| *k == stem) {
                let files = fs::read_dir(&path)
                    .unwrap_or_else(|_| panic!("read {stem}/"))
                    .filter_map(|e| {
                        let p = e.ok()?.path();
                        let s = p.file_stem()?.to_str()?;
                        (p.extension()? == "rs" && !s.ends_with("_tests") && !s.ends_with("_test"))
                            .then_some(p)
                    })
                    .collect();
                out.push((name, files));
            }
            continue;
        }
        if path.extension().is_none_or(|e| e != "rs") || stem.ends_with("_tests")
            || stem.ends_with("_test")
        {
            continue;
        }
        let name: &'static str = map
            .keys()
            .copied()
            .find(|k| *k == stem)
            .unwrap_or_else(|| panic!("unassigned module src/{stem}.rs — add it to layer_map()"));
        out.push((name, vec![path]));
    }
    out
}

/// `crate::xxx` references in non-test code, with `use` lines and
/// inline paths both counted. Lines inside `#[cfg(test)]` regions are
/// NOT excluded — test modules live in `_tests.rs` files here, which
/// the walker already skips.
fn crate_refs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let needle = b"crate::";
    let mut i = 0;
    while let Some(j) = text[i..].find("crate::").map(|j| i + j) {
        i = j + needle.len();
        // skip `::crate::` false positives and doc-comment mentions in
        // strings is overkill; module names are what we extract.
        let rest = &bytes[i..];
        let end = rest
            .iter()
            .position(|c| !(c.is_ascii_alphanumeric() || *c == b'_'))
            .unwrap_or(rest.len());
        if end > 0 {
            out.push(text[i..i + end].to_string());
        }
    }
    out
}

/// Rule: every `crate::X` reference points at the same layer or lower.
#[test]
fn imports_flow_down_only() {
    let map = layer_map();
    let mut violations = Vec::new();
    for (module, files) in module_sources() {
        let my_layer = map[module];
        for f in &files {
            let text = fs::read_to_string(f).expect("read source");
            for target in crate_refs(&text) {
                let Some(&target_layer) = map.get(target.as_str()) else {
                    continue; // not a module path (a type/fn at crate root, etc.)
                };
                if target_layer > my_layer {
                    violations.push(format!(
                        "{} ({:?}) imports crate::{} ({:?}) — data flows down only",
                        f.display(),
                        my_layer,
                        target,
                        target_layer,
                    ));
                }
            }
        }
    }
    assert!(violations.is_empty(), "layer violations:\n{}", violations.join("\n"));
}

/// Rule #2's teeth: the model layer never touches the tree. The only
/// tree-sitter name it may utter is `Point` (plus the serde shim that
/// wraps it). `cst` may not appear at all — the typed view is for
/// sanctioned tree consumers, and the model is not one.
#[test]
fn model_layer_cannot_walk_trees() {
    let map = layer_map();
    let mut violations = Vec::new();
    for (module, files) in module_sources() {
        if map[module] != Layer::Model {
            continue;
        }
        for f in &files {
            let text = fs::read_to_string(f).expect("read source");
            for (ln, line) in text.lines().enumerate() {
                let mut i = 0;
                while let Some(j) = line[i..].find("tree_sitter::").map(|j| i + j) {
                    i = j + "tree_sitter::".len();
                    let rest = &line[i..];
                    let end = rest
                        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                        .unwrap_or(rest.len());
                    let name = &rest[..end];
                    if name != "Point" {
                        violations.push(format!(
                            "{}:{}: tree_sitter::{} — the model is Point-only",
                            f.display(),
                            ln + 1,
                            name,
                        ));
                    }
                }
                for forbidden in ["TreeCursor", "child_by_field_name", "named_child("] {
                    if line.contains(forbidden) {
                        violations.push(format!(
                            "{}:{}: `{}` — tree walking belongs in the builder",
                            f.display(),
                            ln + 1,
                            forbidden,
                        ));
                    }
                }
            }
            if text.contains("crate::cst") {
                violations.push(format!(
                    "{}: imports crate::cst — the typed view is for tree consumers",
                    f.display(),
                ));
            }
        }
    }
    assert!(violations.is_empty(), "rule #2 violations:\n{}", violations.join("\n"));
}

/// Only the builder layer (and `cst` itself) may speak the grammar:
/// `ts_parser_perl::` anywhere above `build` means a second parser
/// entry point is growing. The index layer gets a pass for parsing
/// (resolver/document call `builder::create_parser`), so the check is
/// on the grammar crate, not `tree_sitter` generally.
#[test]
fn grammar_stays_in_the_builder_layer() {
    let map = layer_map();
    let mut violations = Vec::new();
    for (module, files) in module_sources() {
        let layer = map[module];
        if layer == Layer::Build || layer == Layer::Cst {
            continue;
        }
        // main.rs hosts --parse; backend/document parse via
        // builder::create_parser. Direct grammar naming outside
        // build/cst is the smell.
        for f in &files {
            let text = fs::read_to_string(f).expect("read source");
            for (ln, line) in text.lines().enumerate() {
                if line.contains("ts_parser_perl::") {
                    violations.push(format!(
                        "{}:{}: names the grammar directly — route through builder::create_parser",
                        f.display(),
                        ln + 1,
                    ));
                }
            }
        }
    }
    assert!(violations.is_empty(), "grammar violations:\n{}", violations.join("\n"));
}

/// Whole-copy registration is BUDGETED, not free: every call site of an API
/// that pins an unstripped `FileAnalysis` resident must appear here with a
/// reason its residency is bounded. The stripped alternatives
/// (`register_symbols_stripping` / `register_workspace_stripping` /
/// `prepare_pack_parts` / `prepare_workspace_parts` + the deferred writer
/// halves) are the DEFAULT for anything bulk — a new call site of the APIs
/// below compiles and passes every functional test while silently
/// re-pinning the gigabytes the eviction axes strip (the chromium 20 GB
/// wall), so this test is the tripwire: to add one, add the (file, count)
/// here WITH a bounded-residency justification in the code.
#[test]
fn whole_copy_registration_sites_are_allowlisted() {
    // fn name → (file stem, expected call-site count, why it's bounded)
    let allow: Vec<(&str, Vec<(&str, usize, &str)>)> = vec![
        (
            "register_symbols",
            vec![
                // 1 shared writer fallback (commit-fail + panic, via
                // run_persist_writer — bounded by failure, tripwire-
                // counted), 1 degraded/unpersisted worker arm (tripwire-
                // counted), 1 pack_file_changed unpersisted fallback.
                ("module_resolver", 3, "failure fallbacks + NO_EVICT arm, tripwire-counted"),
            ],
        ),
        (
            "register_symbols_inner",
            vec![
                ("module_index", 2, "the two registration front doors"),
                (
                    "module_resolver",
                    3,
                    "stub/full warm lanes + deferred writer — all take prepare_pack_parts output",
                ),
            ],
        ),
        // register_workspace_module: TEST-ONLY today (fixtures build whole
        // copies directly). Its first production caller lands here.
        ("register_workspace_module", vec![]),
        (
            "register_workspace_resident",
            vec![
                ("backend", 1, "watcher re-register — bounded by external change volume"),
                ("module_index", 1, "register_workspace_module's residency half"),
                ("module_resolver", 1, "shared writer failure fallback (run_persist_writer)"),
            ],
        ),
        (
            "register_workspace_residency",
            vec![
                ("module_index", 1, "register_workspace_stripping's residency half"),
                ("module_resolver", 2, "deferred writer halves — stripped arcs only"),
            ],
        ),
        (
            "register_materialized_whole",
            vec![(
                "module_index",
                1,
                "gated-emission CLI/batch materialization — plugin-triggered \
                 files only (sparse by construction), one-shot startup, whole \
                 copy deliberate so whole_present sees the emissions",
            )],
        ),
    ];
    let mut violations: Vec<String> = Vec::new();
    for (name, files) in &allow {
        let mut seen: HashMap<String, usize> = HashMap::new();
        for (_module, paths) in module_sources() {
            for path in paths {
                let stem = path.file_stem().unwrap().to_str().unwrap().to_string();
                let text = fs::read_to_string(&path).unwrap();
                let needle = format!("{name}(");
                for line in text.lines() {
                    let t = line.trim_start();
                    if t.starts_with("//") {
                        continue;
                    }
                    let mut rest = t;
                    while let Some(pos) = rest.find(&needle) {
                        // A call site, not the definition, and not a
                        // longer-named sibling (`register_symbols_inner(`
                        // must not count as `register_symbols(`).
                        let before = &rest[..pos];
                        let defn = before.trim_end().ends_with("fn");
                        let word_start = pos == 0
                            || !rest[..pos]
                                .chars()
                                .next_back()
                                .is_some_and(|c| c.is_alphanumeric() || c == '_');
                        if !defn && word_start {
                            *seen.entry(stem.clone()).or_default() += 1;
                        }
                        rest = &rest[pos + needle.len()..];
                    }
                }
            }
        }
        let expected: HashMap<String, usize> =
            files.iter().map(|(f, n, _)| (f.to_string(), *n)).collect();
        for (file, n) in &seen {
            match expected.get(file) {
                Some(exp) if exp == n => {}
                Some(exp) => violations.push(format!(
                    "{name}() call-site count changed in src/{file}.rs: {n} (allowlisted {exp}) — \
                     if the new site registers WHOLE copies, justify its residency bound here; \
                     bulk paths use the stripping/parts APIs"
                )),
                None => violations.push(format!(
                    "{name}() called from src/{file}.rs ({n} site(s)) — not allowlisted. Bulk \
                     registration must go through the stripping/parts APIs; a deliberate \
                     whole-copy site needs an entry here with its residency bound"
                )),
            }
        }
        for (file, exp) in &expected {
            if !seen.contains_key(file) {
                violations.push(format!(
                    "{name}() allowlisted in src/{file}.rs ({exp}) but no call site found — \
                     update the allowlist"
                ));
            }
        }
    }
    assert!(violations.is_empty(), "whole-copy registration drift:\n{}", violations.join("\n"));
}
