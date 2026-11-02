//! The ADR's projection contracts, held against EVERY cursor of a real
//! corpus instead of hand-picked ones (`docs/adr/resolution-candidate-set.md`).
//!
//! Four of this round's bugs were two sibling code paths disagreeing about
//! one question, each defensible alone — the defect only exists in the
//! comparison. The set's projections are the one place such pairs are a
//! CONTRACT rather than a hunch: the ADR promises relations between them,
//! so comparing projections against each other needs no oracle. A cursor
//! where the relations break is a bug by the ADR's own words, found on a
//! schedule instead of by accident.
//!
//! Checked per cursor:
//!   I1  highlights() == references() ∩ origin file (span sets). Both
//!       sides are now ONE driver (`walk_refs`, scope-parameterized), so
//!       this no longer polices a sibling implementation — it tests the
//!       residual claim the scope split leaves: that the origin scope's
//!       smaller enumeration (and its two stated asymmetries — no closure
//!       gate, no matcher_view routing) commutes with filtering the
//!       workspace walk to the origin file
//!   I2  linked_editing_spans() ⊆ highlights() spans (the co-edit subset)
//!   I3  rename_edits() ⊆ references() image (edits outside it are
//!       "unrepresentable" — hold the word to it), when renameable()
//!   I4  references() names a Declaration ⇒ definitions() is non-empty
//!       (the decl-axis backstop: gd cannot come up empty where gr names
//!       a declaration)

use super::*;
use refs_tests::parse;

fn corpus_files() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_files");
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                // Perl corpus only — the pack lanes get their own instance
                // once this net proves cheap enough.
                if p.file_name().is_some_and(|n| n == "cpp" || n == "python") {
                    continue;
                }
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "pl" || x == "pm") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

/// Per-file cursor budget. A cap is a coverage decision, so it is LOUD:
/// the test names how many cursors each capped file dropped in its
/// pass-line, never silently.
const CURSORS_PER_FILE: usize = 250;

/// The four contracts at one cursor — ONE speller, shared by every corpus
/// instance (a drifted copy of the drift-detector would be the joke that
/// writes itself). Returns the violations found at this cursor.
fn check_cursor_contracts(
    store: &OpenDocs,
    fa: &std::sync::Arc<FileAnalysis>,
    path: &std::path::Path,
    r: &crate::model::file_analysis::Ref,
    idx: &crate::index::module_index::ModuleIndex,
) -> Vec<String> {
    let mut violations = Vec::new();
    let key = FileKey::Path(crate::util::canonical_path::CanonicalPath::resolve(path.to_path_buf()));
    let cs = resolve(
        store,
        fa,
        key.clone(),
        r.span.start,
        Some(idx),
        OverrideScope::default(),
    );

    let refs_img = cs.references();
    let highlights = cs.highlights();

    // I1: highlights == references ∩ origin, as span SETS. Post-collapse
    // (one `walk_refs` driver) this tests only the scope-enumeration
    // commutation — see the module header.
    let mut gr_origin: Vec<Span> = refs_img
        .iter()
        .filter(|l| file_key_eq(&l.key, &key))
        .map(|l| l.span)
        .collect();
    let mut hl: Vec<Span> = highlights.iter().map(|l| l.span).collect();
    gr_origin.sort_by_key(|s| (s.start.row, s.start.column, s.end.row, s.end.column));
    gr_origin.dedup();
    hl.sort_by_key(|s| (s.start.row, s.start.column, s.end.row, s.end.column));
    hl.dedup();
    if gr_origin != hl {
        violations.push(format!(
            "I1 {}:{}:{} `{}` — highlights ({}) != references∩origin ({}): hl={:?} gr={:?}",
            path.display(), r.span.start.row, r.span.start.column, r.target_name,
            hl.len(), gr_origin.len(), hl, gr_origin,
        ));
    }

    // I2: linked editing spans ⊆ highlight spans.
    for s in cs.linked_editing_spans() {
        if !hl.contains(&s) {
            violations.push(format!(
                "I2 {}:{}:{} `{}` — linked-editing span {:?} not in highlights",
                path.display(), r.span.start.row, r.span.start.column, r.target_name, s,
            ));
        }
    }

    // I3: rename edits ⊆ references image. CONTAINMENT, not span equality:
    // a variable rename edits the bare name inside the sigil'd reference
    // span, so the edit is a sub-span of its reference site — outside ANY
    // reference span is the violation.
    if cs.renameable() == Ok(true) {
        if let Ok(edits) = cs.rename_edits("zzz_consistency_probe") {
            for (loc, _text) in &edits {
                let in_refs = refs_img.iter().any(|l| {
                    file_key_eq(&l.key, &loc.key)
                        && (l.span.start.row, l.span.start.column)
                            <= (loc.span.start.row, loc.span.start.column)
                        && (loc.span.end.row, loc.span.end.column)
                            <= (l.span.end.row, l.span.end.column)
                });
                if !in_refs {
                    violations.push(format!(
                        "I3 {}:{}:{} `{}` — rename edit at {:?} {:?} outside references",
                        path.display(), r.span.start.row, r.span.start.column,
                        r.target_name, loc.key, loc.span,
                    ));
                }
            }
        }
    }

    // I5: hover presents the top-ranked definitions() candidate — the
    // ADR's words. Two implementations of "the best definition" that
    // drift is exactly the sibling-pair class this net exists for.
    if let Some(h) = cs.hover_candidate() {
        if !cs
            .definitions()
            .iter()
            .any(|d| file_key_eq(&d.key, &h.key) && d.span == h.span)
        {
            violations.push(format!(
                "I5 {}:{}:{} `{}` — hover_candidate {:?} {:?} not in definitions()",
                path.display(), r.span.start.row, r.span.start.column,
                r.target_name, h.key, h.span,
            ));
        }
    }

    // I7: every implementations() location is a Declaration-access site.
    // Promoted from provisional after holding at ZERO violations over
    // ~9,600 cursors (fixtures + substrate) — the ADR sentence this
    // enforces was written AFTER the measurement, not before (measured-
    // then-promised, the gold harness's provisional→gold rule). A future
    // candidate invariant enters the same way: prefixed "P1", partitioned
    // into the reported-never-failing lane below, promoted only once the
    // corpora back it.
    for l in cs.implementations() {
        if l.access != AccessKind::Declaration {
            violations.push(format!(
                "I7 {}:{}:{} `{}` — implementations() location {:?} {:?} has access {:?}, not Declaration",
                path.display(), r.span.start.row, r.span.start.column,
                r.target_name, l.key, l.span, l.access,
            ));
        }
    }

    // I4: gr names a declaration ⇒ gd answers.
    if refs_img.iter().any(|l| l.access == AccessKind::Declaration)
        && cs.definitions().is_empty()
    {
        violations.push(format!(
            "I4 {}:{}:{} `{}` — references name a Declaration but definitions() is empty",
            path.display(), r.span.start.row, r.span.start.column, r.target_name,
        ));
    }
    violations
}

#[test]
fn projection_contracts_hold_at_every_corpus_cursor() {
    let files = corpus_files();
    assert!(files.len() >= 20, "corpus went missing: {} files", files.len());

    let store = OpenDocs::new();
    // Production wiring: the workspace bulk index feeds both the OpenDocs
    // AND the module index, so the forward lanes (method resolution,
    // package->file) see the same universe the backward walk does. A
    // store-only harness flags gd-empty asymmetries production never has.
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let mut analyses: Vec<(PathBuf, std::sync::Arc<FileAnalysis>)> = Vec::new();
    for p in &files {
        let src = std::fs::read_to_string(p).unwrap();
        let fa = std::sync::Arc::new(parse(&src));
        idx.insert_workspace_arc_for_test(p.clone(), fa.clone());
        idx.register_workspace_module(p.clone(), fa.clone());
        analyses.push((p.clone(), fa));
    }

    let mut violations: Vec<String> = Vec::new();
    let mut cursors = 0usize;
    let mut dropped = 0usize;
    for (path, fa) in &analyses {
        let refs = fa.refs();
        let stride = refs.len().div_ceil(CURSORS_PER_FILE).max(1);
        dropped += refs.len() - refs.len().div_ceil(stride);
        for r in refs.iter().step_by(stride) {
            cursors += 1;
            violations.extend(check_cursor_contracts(&store, fa, path, r, &idx));
        }
    }

    let (provisional, violations): (Vec<String>, Vec<String>) =
        violations.into_iter().partition(|v| v.starts_with("P1"));
    for v in &provisional {
        eprintln!("provisional: {v}");
    }
    if !provisional.is_empty() {
        eprintln!("({} provisional reports — never failing)", provisional.len());
    }
    assert!(
        violations.is_empty(),
        "{} projection-contract violations over {} cursors ({} capped out) — first 20:\n{}",
        violations.len(),
        cursors,
        dropped,
        violations.iter().take(20).cloned().collect::<Vec<_>>().join("\n"),
    );
    eprintln!(
        "projection contracts held at {cursors} cursors across {} files ({dropped} capped out)",
        files.len(),
    );
}

/// The pack instance of the same net — #141 and #142 (goto-def vs
/// references vs implementations disagreeing) both lived in this lane.
/// Corpus: the real cpp fixture trees, analyzed with their real paths so
/// include closures gather from disk; analyses register into the index, the
/// production topology — the index is the one home for project files, so
/// the backward walk sees every queried origin there.
#[cfg(feature = "cpp")]
#[test]
fn projection_contracts_hold_at_every_pack_cursor() {
    let reg = crate::build::language_driver::LanguageRegistry::compiled();
    let driver = reg.for_id("cpp").expect("cpp driver");

    let mut files: Vec<PathBuf> = Vec::new();
    for root in ["gold-corpus/cpp-fixture", "test_files/cpp"] {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(root);
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| {
                x == "c" || x == "cpp" || x == "cc" || x == "h" || x == "hpp"
            }) {
                files.push(p);
            }
        }
    }
    files.sort();
    assert!(files.len() >= 40, "cpp corpus went missing: {} files", files.len());

    let store = OpenDocs::new();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let mut analyses: Vec<(PathBuf, std::sync::Arc<FileAnalysis>)> = Vec::new();
    for p in &files {
        let Ok(src) = std::fs::read_to_string(p) else { continue };
        let fa = driver.analyze_with_path(&src, Some(p));
        // The bulk-index registration shape: parts carry the edge-index
        // FEED (extracted pre-strip) — `register_symbols` alone publishes
        // no name records, and the backward walk enumerates candidates
        // from them, so a feed-less harness sees empty references.
        let parts = crate::index::module_index::ModuleIndex::prepare_pack_parts(
            fa,
            crate::model::file_analysis::Residency::Whole,
        );
        let arc = std::sync::Arc::clone(parts.arc());
        idx.register_symbols_inner(p.clone(), parts);
        analyses.push((p.clone(), arc));
    }

    let mut violations: Vec<String> = Vec::new();
    let mut cursors = 0usize;
    let mut dropped = 0usize;
    for (path, fa) in &analyses {
        let refs = fa.refs();
        let stride = refs.len().div_ceil(CURSORS_PER_FILE).max(1);
        dropped += refs.len() - refs.len().div_ceil(stride);
        for r in refs.iter().step_by(stride) {
            cursors += 1;
            violations.extend(check_cursor_contracts(&store, fa, path, r, &idx));
        }
    }

    let (provisional, violations): (Vec<String>, Vec<String>) =
        violations.into_iter().partition(|v| v.starts_with("P1"));
    for v in &provisional {
        eprintln!("provisional: {v}");
    }
    if !provisional.is_empty() {
        eprintln!("({} provisional reports — never failing)", provisional.len());
    }
    assert!(
        violations.is_empty(),
        "{} pack projection-contract violations over {} cursors ({} capped out) — first 20:\n{}",
        violations.len(),
        cursors,
        dropped,
        violations.iter().take(20).cloned().collect::<Vec<_>>().join("\n"),
    );
    eprintln!(
        "pack projection contracts held at {cursors} cursors across {} files ({dropped} capped out)",
        analyses.len(),
    );
}

/// The REACH instance: the same contracts over a big real corpus — the
/// snapshot-pinned CPAN substrate (`gold-corpus/local`, ~3.5k files of
/// real CPAN code) by default. `PERL_LSP_CONSISTENCY_CORPUS=<root>` points
/// the net at ANY tree — this is the highest-leverage knob here: the net
/// travels to whatever corpus is worrying us this week (Koha, crm, a bug
/// reporter's repo) instead of staying pinned to the fixtures it was born
/// with. Nothing else changes: same contracts, same known-list, same
/// verdict.
/// Answers whether the fixture-tree run's near-cleanliness means the set
/// is clean or the corpora are tame. `#[ignore]` because it runs minutes,
/// not seconds — opt in with `cargo test -- --ignored` or by name.
/// Both caps are LOUD in the pass line; a cap is a coverage decision.
#[test]
#[ignore = "broad-corpus reach run (minutes) — run by name or --ignored"]
fn projection_contracts_broad_corpus() {
    const FILE_CAP: usize = 700;
    const BROAD_CURSORS_PER_FILE: usize = 25;

    let root = std::env::var("PERL_LSP_CONSISTENCY_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("gold-corpus/local/lib/perl5")
        });
    let mut files: Vec<PathBuf> = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "pm" || x == "pl") {
                files.push(p);
            }
        }
    }
    files.sort();
    assert!(!files.is_empty(), "no corpus at {} — set PERL_LSP_CONSISTENCY_CORPUS", root.display());
    let total_files = files.len();
    let fstride = total_files.div_ceil(FILE_CAP).max(1);
    let files: Vec<PathBuf> = files.into_iter().step_by(fstride).collect();

    let store = OpenDocs::new();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let mut analyses: Vec<(PathBuf, std::sync::Arc<FileAnalysis>)> = Vec::new();
    for p in &files {
        let Ok(src) = std::fs::read_to_string(p) else { continue };
        if src.len() > 1_000_000 {
            continue; // the walk's own 1MB cap
        }
        let fa = std::sync::Arc::new(parse(&src));
        idx.insert_workspace_arc_for_test(p.clone(), fa.clone());
        idx.register_workspace_module(p.clone(), fa.clone());
        analyses.push((p.clone(), fa));
    }

    let mut violations: Vec<String> = Vec::new();
    let mut cursors = 0usize;
    let mut dropped = 0usize;
    let t0 = std::time::Instant::now();
    for (path, fa) in &analyses {
        let refs = fa.refs();
        let stride = refs.len().div_ceil(BROAD_CURSORS_PER_FILE).max(1);
        dropped += refs.len() - refs.len().div_ceil(stride);
        for r in refs.iter().step_by(stride) {
            cursors += 1;
            violations.extend(check_cursor_contracts(&store, fa, path, r, &idx));
        }
    }

    // Gold-harness xfail discipline: adjudication-pending residuals are
    // KNOWN — reported, never silently failing — and a known that stops
    // firing is flagged for promotion, exactly like an XPASS. (The
    // original pair — invocant-position cursors inside SUPER calls —
    // promoted out when the rule-7 builtin-keyword emission landed:
    // builtin calls now mint CORE-bound refs, so the cursor resolves the
    // builtin instead of the enclosing MethodCall.)
    //
    // Current entry: HARNESS-REACH, not a code bug — production gd AND
    // references both answer correctly at this cursor (`$self->plugin(
    // 'Koha::…::Objects')` → the plugin module's `register`). The
    // in-process net lacks the resolver thread that serves name-keyed
    // module resolution at query time, so ITS gd comes up empty while its
    // references finds the decl by walking files directly. This maps the
    // net's reach boundary; wiring a test resolver lane would promote it.
    let (provisional, violations): (Vec<String>, Vec<String>) =
        violations.into_iter().partition(|v| v.starts_with("P1"));
    for v in provisional.iter().take(40) {
        eprintln!("provisional: {v}");
    }
    if !provisional.is_empty() {
        eprintln!("({} provisional reports — never failing; first 40 shown)", provisional.len());
    }
    const KNOWN: &[(&str, usize, usize)] = &[
        ("Koha/REST/V1.pm", 223, 19),
    ];
    let is_known = |v: &str| {
        KNOWN.iter().any(|(f, row, col)| v.contains(&format!("{f}:{row}:{col} ")))
    };
    let (known, violations): (Vec<String>, Vec<String>) =
        violations.into_iter().partition(|v| is_known(&v[..]));
    for v in &known {
        eprintln!("KNOWN (adjudication pending): {v}");
    }
    if known.len() < KNOWN.len() {
        eprintln!(
            "NOTE: only {} of {} KNOWN residuals fired — a fixed one should be \
             promoted out of the list (the xfail→XPASS rule)",
            known.len(),
            KNOWN.len(),
        );
    }
    // Report-first: on a broad corpus the FINDINGS are the product; print
    // every distinct violation before the verdict so a red run carries its
    // own triage list.
    for v in &violations {
        eprintln!("{v}");
    }
    eprintln!(
        "broad corpus: {} violations over {cursors} cursors, {} of {total_files} files \
         (file stride {fstride}; {dropped} cursors capped out) in {:.1}s",
        violations.len(),
        analyses.len(),
        t0.elapsed().as_secs_f32(),
    );
    assert!(
        violations.is_empty(),
        "{} projection-contract violations on the broad corpus (see stderr)",
        violations.len(),
    );
}

/// I6 — the `requires` edge round-trip, RESTATED after [B]'s counterexample
/// killed the symmetric draft (#120): `implementations()` answers DISPATCH
/// REACHABILITY (which concrete def satisfies this contract) while
/// `references()` answers RENAME CORRECTNESS (which tokens must change
/// together), and the rename set is legitimately wider — Composer never
/// composes SubRole, so SubRole's atom reaches Composer::fetch in
/// references and must NOT in implementations. Demanding symmetry would
/// force rename to emit an incomplete edit set to satisfy the net. The
/// net was built to test the code; here the code tested the net.
///
///   leg 1 (strict):   for every L in implementations(atom): references
///                     from L contains the atom — an implementation the
///                     inverse forgets is exactly this net's bug class.
///   leg 2 (weakened): for every fulfilling method M: at least one atom
///                     in references(M) has M in its implementations —
///                     catches an inverse naming unrelated atoms without
///                     asserting a symmetry the two verbs do not have.
///
/// The inverse direction IS `references()` — no new API, no new
/// vocabulary; the round-trip is two existing projections disagreeing
/// or not.
#[test]
fn requires_round_trip_holds_on_the_contract_fixtures() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_files/lib/Contract");
    let store = OpenDocs::new();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let mut by_path: Vec<(PathBuf, std::sync::Arc<FileAnalysis>, String)> = Vec::new();
    for name in ["Role.pm", "SubRole.pm", "Composer.pm", "Deep.pm", "Broken.pm"] {
        let p = root.join(name);
        let src = std::fs::read_to_string(&p).unwrap();
        let fa = std::sync::Arc::new(parse(&src));
        idx.insert_workspace_arc_for_test(p.clone(), fa.clone());
        idx.register_workspace_module(p.clone(), fa.clone());
        by_path.push((p, fa, src));
    }
    let find = |file: &str, needle: &str, token: &str| -> (PathBuf, tree_sitter::Point) {
        let (p, _fa, src) = by_path.iter().find(|(p, ..)| p.ends_with(file)).unwrap();
        let (row, line) = src
            .lines()
            .enumerate()
            .find(|(_, l)| l.contains(needle))
            .unwrap_or_else(|| panic!("{file} lost its `{needle}` line"));
        let col = line.find(token).unwrap();
        (p.clone(), tree_sitter::Point { row, column: col })
    };
    let cs_at = |p: &PathBuf, pt: tree_sitter::Point| {
        let fa = &by_path.iter().find(|(q, ..)| q == p).unwrap().1;
        resolve(
            &store,
            fa,
            FileKey::Path(crate::util::canonical_path::CanonicalPath::resolve(p.clone())),
            pt,
            Some(&idx),
            OverrideScope::default(),
        )
    };
    let covers = |locs: &[RefLocation], p: &PathBuf, pt: tree_sitter::Point| {
        locs.iter().any(|l| {
            matches!(&l.key, FileKey::Path(q) if *q == crate::util::canonical_path::CanonicalPath::resolve(p))
                && l.span.start.row == pt.row
                && l.span.start.column <= pt.column
                && pt.column < l.span.end.column
        })
    };

    // Leg 1, from each requires atom.
    for (atom_file, needle) in [("Role.pm", "requires 'fetch'"), ("SubRole.pm", "requires")] {
        let (ap, apt) = find(atom_file, needle, "fetch");
        let atom = cs_at(&ap, apt);
        let impls = atom.implementations();
        assert!(
            !impls.is_empty(),
            "implementations() from {atom_file}'s requires atom is empty — the forward walk lost the edge",
        );
        for l in &impls {
            let FileKey::Path(lp) = &l.key else { continue };
            let back = cs_at(&lp.to_path_buf(), l.span.start).references();
            assert!(
                covers(&back, &ap, apt),
                "leg 1: implementations({atom_file}) reached {}:{}:{} but references from \
                 there does not name the atom — an implementation the inverse forgot",
                lp.display(), l.span.start.row, l.span.start.column,
            );
        }
    }

    // Leg 2 (weakened), from each fulfilling method.
    for m_file in ["Composer.pm", "Deep.pm"] {
        let (mp, mpt) = find(m_file, "sub fetch", "fetch");
        let m = cs_at(&mp, mpt);
        let named_atoms: Vec<(PathBuf, tree_sitter::Point)> = m
            .references()
            .iter()
            .filter_map(|l| match &l.key {
                FileKey::Path(p)
                    if p.ends_with("Role.pm") || p.ends_with("SubRole.pm") =>
                {
                    Some((p.to_path_buf(), l.span.start))
                }
                _ => None,
            })
            .collect();
        assert!(
            !named_atoms.is_empty(),
            "references({m_file}::fetch) names no requires atom at all",
        );
        let some_atom_implements_back = named_atoms.iter().any(|(ap, apt)| {
            let impls = cs_at(ap, *apt).implementations();
            covers(&impls, &mp, mpt)
        });
        assert!(
            some_atom_implements_back,
            "leg 2: no atom named by references({m_file}::fetch) has it in implementations()",
        );
    }
}
