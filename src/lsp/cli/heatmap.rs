//! `--heatmap` (fan-in/fan-out/dead-code report, JSON/CSV/HTML) and the
//! `--refs-parity` A/B net over the same eligible-symbol walk.

use super::*;

/// Which symbols a usage heatmap lists: nameable callables and packages.
/// A listing policy, not an identity decision — identity is minted by the
/// CandidateSet at the symbol's declaration. Anonymous subs (`(anon)`) and
/// other non-identifier names have no nameable reference graph (their name
/// would cross-link every other anon); lexical variables, hash-key/field
/// slots, and handlers have no meaningful cross-file usage count.
fn heatmap_symbol_eligible(sym: &file_analysis::Symbol) -> bool {
    use file_analysis::SymKind;
    sym.name.starts_with(|c: char| c.is_alphabetic() || c == '_')
        && matches!(
            sym.kind,
            SymKind::Sub
                | SymKind::Method
                | SymKind::Package
                | SymKind::Class
                | SymKind::Module
        )
}

/// One heatmap row for one symbol — the shared body every gather loop
/// calls, so fan-in counts come from the SAME `references()` projection by
/// construction (no second ref walk). Tier-specific behavior arrives as
/// data, never a family flag: `visibility` is the mask override to apply
/// (`None` when the set's construction-derived routing already widens to
/// VISIBLE — pack workspace files ride the DEPENDENCY role, a storage
/// artifact of the per-language cache), and the entry-point guard reads the
/// analysis language's declared `entrypoint_symbols`.
/// Returns `(row, is_callable, dead, dead_export)`.
///
/// `forced_fan_in` is the relational pre-prune verdict: `Some(0)` means the
/// row store proved this declaration's references projection empty (no ref
/// row for its name), so the `references()` walk is skipped and fan-in is 0.
/// The pre-prune may only ever assert PROVABLY-EMPTY, never a substituted
/// count — `None` runs the full projection, and every computed fan-in still
/// comes from `references()`. `dead_export_override` is the row-backed
/// unused-exports verdict, passed ONLY alongside a skipped walk (where it is
/// provably equal to what the projection would derive); whenever the
/// projection runs, `None` lets it decide (exported with zero cross-file
/// references) — strictly more accurate than candidate rows, which
/// over-approximate real references.
#[allow(clippy::too_many_arguments)]
fn heatmap_symbol_row(
    ws: &file_store::FileStore,
    routing_idx: &dyn file_analysis::CrossFileLookup,
    path: &std::path::Path,
    analysis: &file_analysis::FileAnalysis,
    sym: &file_analysis::Symbol,
    visibility: Option<resolve::RoleMask>,
    scope: resolve::OverrideScope,
    has_dynamic_dispatch: bool,
    forced_fan_in: Option<usize>,
    dead_export_override: Option<bool>,
    sources: &mut SourceCache,
) -> (serde_json::Value, bool, bool, bool) {
    use file_analysis::{AccessKind, Namespace, RefKind, SymKind};
    use std::collections::HashSet;

    let within = |outer: &file_analysis::Span, inner: &file_analysis::Span| {
        let s = |p: &tree_sitter::Point| (p.row, p.column);
        s(&inner.start) >= s(&outer.start) && s(&inner.end) <= s(&outer.end)
    };
    let path_str = path.display().to_string();

    // fan_in = the references image minus the symbol's declaration site(s);
    // cross_file_fan_in additionally drops every same-file reference (the
    // dead-export test: an export used only by its own module is dead to
    // consumers). Both project from the ONE `references()` set minted at the
    // declaration — identity is never re-derived heatmap-side. Pack routing
    // is a construction fact (which sub-index, VISIBLE-wide walk), declared
    // here exactly as the references/goto-def CLI mirrors declare it.
    //
    // The relational pre-prune (`forced_fan_in`) may skip this walk only when
    // the row store proved it empty; a computed count always comes from here.
    let (fan_in, cross_file_fan_in) = match forced_fan_in {
        Some(n) => (n, 0usize),
        None => {
            let mut cs = crate::util::ghost_stats::timed("heatmap.mint", || resolve::resolve(
                ws,
                analysis,
                file_store::FileKey::Path(path.to_path_buf()),
                sym.selection_span.start,
                Some(routing_idx),
                scope,
            ));
            if let Some(mask) = visibility {
                cs = cs.with_visibility(mask);
            }
            let locs = crate::util::ghost_stats::timed("heatmap.references", || cs.references());
            let fan_in = locs
                .iter()
                .filter(|l| l.access != AccessKind::Declaration)
                .filter(|l| {
                    !(l.span == sym.selection_span
                        && matches!(&l.key, file_store::FileKey::Path(p) if p == path))
                })
                .count();
            let cross_file = locs
                .iter()
                .filter(|l| l.access != AccessKind::Declaration)
                .filter(|l| !matches!(&l.key, file_store::FileKey::Path(p) if p == path))
                .count();
            (fan_in, cross_file)
        }
    };

    // fan_out = distinct callee names referenced inside this body (subs /
    // methods only). Packages have no body to scan.
    let is_callable = matches!(sym.kind, SymKind::Sub | SymKind::Method);
    let fan_out: Option<usize> = if is_callable {
        let _t = crate::util::ghost_stats::ScopedNs::start("heatmap.fanout");
        let mut callees: HashSet<&str> = HashSet::new();
        for r in analysis.refs() {
            if matches!(
                r.kind,
                RefKind::FunctionCall { .. }
                    | RefKind::MethodCall { .. }
                    | RefKind::DispatchCall { .. }
            ) && within(&sym.span, &r.span)
            {
                callees.insert(r.unqualified_target_name());
            }
        }
        callees.remove(sym.name.as_str());
        Some(callees.len())
    } else {
        None
    };

    let exported = analysis.exports_name(&sym.name);
    let native = matches!(sym.namespace, Namespace::Language);

    // Reachability guard — why a zero-fan-in symbol is NOT flagged dead.
    // Ordered most-specific-first. Address-taken / used-as-value functions
    // need no guard: a non-call reference (`&fn`, function-pointer decay) is
    // still a reference, so it lands in `fan_in` and never reaches here.
    let guard: Option<&'static str> = if fan_in > 0 {
        None
    } else if exported {
        Some("exported")
    } else if conventions::is_constructor_name(&sym.name) {
        Some("constructor")
    } else if !native {
        Some("framework-synthesized")
    } else if is_callable
        && crate::build::language_driver::LanguageRegistry::caps(&analysis.language)
            .entrypoint_symbols
            .contains(&sym.name.as_str())
    {
        // Runtime entry (C/C++ `main`): entered over the ABI, never a source
        // call site the static graph can see. The language declares which
        // names are entry points; nothing here compares names or families.
        Some("entry-point")
    } else if matches!(sym.kind, SymKind::Package | SymKind::Class | SymKind::Module) {
        Some("package-implicit-use")
    } else if has_dynamic_dispatch
        && matches!(sym.kind, SymKind::Sub | SymKind::Method)
        && sym.package.as_deref().is_some_and(|p| p != "main")
    {
        Some("dynamic-dispatch")
    } else {
        None
    };

    let dead = fan_in == 0 && guard.is_none();
    // A dead export is an EXPORTED callable with no cross-file reference —
    // orthogonal to `dead_code_candidate` (which the `exported` guard shields).
    // Row-backed when the pre-prune supplied a verdict; otherwise the
    // projection's cross-file count answers it.
    let dead_export = match dead_export_override {
        Some(v) => v,
        None => is_callable && exported && cross_file_fan_in == 0,
    };
    let (line, col) = sources.display(
        &path_str,
        sym.selection_span.start.row,
        sym.selection_span.start.column,
    );
    let kind = format!("{:?}", sym.kind);

    let row = serde_json::json!({
        "name": sym.name,
        "kind": kind,
        "package": sym.package,
        "file": path_str,
        "line": line,
        "col": col,
        "fan_in": fan_in,
        "fan_out": fan_out,
        "exported": exported,
        "dead_code_candidate": dead,
        "dead_export": dead_export,
        "reachable_guard": guard,
    });
    (row, is_callable, dead, dead_export)
}

/// --refs-parity <root> — the relational-ref-index migration net
/// (`docs/adr/relational-ref-index.md`). Mints the CandidateSet at every
/// heatmap-eligible symbol declaration (Perl workspace + pack files) and
/// projects `references()` twice — resident scan (`PERL_LSP_REF_ROWS=0`) vs
/// SQL retrieval (`=1`) — asserting identical (file, span, access,
/// rewritable) sets. Exit 1 on any divergence. A dev/CI net, not a user
/// verb: run it against a real corpus after touching `refs_to`, the shred,
/// or the eviction seams.
pub(crate) fn cli_refs_parity(root: &str, sample: Option<usize>) {
    // The A/B needs the resident side complete: keep refs + bags resident
    // (rows are still written — eviction and persistence are independent).
    std::env::set_var("PERL_LSP_NO_EVICT", "1");
    let (ws, idx) = cli_full_startup(root, crate::build::language_driver::LanguageScope::All);
    let scope = override_scope_from_env();

    let mut pack_entries: Vec<(
        std::path::PathBuf,
        std::sync::Arc<file_analysis::FileAnalysis>,
        std::sync::Arc<module_index::ModuleIndex>,
    )> = Vec::new();
    idx.for_each_pack_index(|_lang, pack| {
        pack.for_each_registered_file(&mut |cached| {
            // Index copies are refs-evicted; fan-out scans + set minting read
            // refs, so take the refs-present view (resident when not evicted,
            // rehydrated otherwise). Batch-CLI-sized cost, not a query path.
            pack_entries.push((
                cached.path.clone(),
                file_analysis::CrossFileLookup::whole_present(pack.as_ref(), cached),
                std::sync::Arc::clone(pack),
            ));
        });
    });
    pack_entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut entries: Vec<(std::path::PathBuf, std::sync::Arc<file_analysis::FileAnalysis>)> = ws
        .workspace_raw()
        .iter()
        .map(|e| {
            // Workspace copies may be refs-evicted; fan-out scans + set
            // minting read refs, so take the refs-present view.
            let cm = std::sync::Arc::new(file_analysis::CachedModule::new(
                e.key().clone(),
                std::sync::Arc::clone(e.value()),
            ));
            (
                e.key().clone(),
                file_analysis::CrossFileLookup::whole_present(&idx, &cm),
            )
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let normalize = |locs: &[resolve::RefLocation]| -> Vec<String> {
        let mut v: Vec<String> = locs
            .iter()
            .map(|l| {
                format!(
                    "{:?}:{}:{}-{}:{}:{:?}:{}",
                    l.key,
                    l.span.start.row,
                    l.span.start.column,
                    l.span.end.row,
                    l.span.end.column,
                    l.access,
                    l.rewritable
                )
            })
            .collect();
        v.sort();
        v
    };

    // `--sample=N` strides the symbol universe down to ~N checks — the
    // per-phase quick net (~a minute). The full sweep (no flag) is the
    // pre-merge gate: it re-runs the OLD O(symbols × tree) resident walk
    // per symbol, so it is heatmap×2-shaped by construction.
    let mut seen_symbols = 0usize;
    let total_symbols: usize = entries.iter().map(|(_, a)| a.symbols().len()).sum::<usize>()
        + pack_entries.iter().map(|(_, a, _)| a.symbols().len()).sum::<usize>();
    let stride = sample
        .map(|n| (total_symbols / n.max(1)).max(1))
        .unwrap_or(1);
    let mut checked = 0usize;
    let mut mismatched = 0usize;
    let mut check = |ws: &file_store::FileStore,
                     routing: &dyn file_analysis::CrossFileLookup,
                     path: &std::path::Path,
                     analysis: &file_analysis::FileAnalysis,
                     visibility: Option<resolve::RoleMask>,
                     checked: &mut usize,
                     mismatched: &mut usize| {
        for sym in analysis.symbols() {
            seen_symbols += 1;
            if seen_symbols % stride != 0 {
                continue;
            }
            if sym.hidden_in_outline() || !heatmap_symbol_eligible(sym) {
                continue;
            }
            if *checked % 200 == 0 && *checked > 0 {
                eprintln!("refs-parity: {} checked...", *checked);
            }
            let mut cs = resolve::resolve(
                ws,
                analysis,
                file_store::FileKey::Path(path.to_path_buf()),
                sym.selection_span.start,
                Some(routing),
                scope,
            );
            if let Some(mask) = visibility {
                cs = cs.with_visibility(mask);
            }
            resolve::set_ref_rows_override(Some(false));
            let resident = normalize(&cs.references());
            resolve::set_ref_rows_override(Some(true));
            let rows = normalize(&cs.references());
            resolve::set_ref_rows_override(None);
            *checked += 1;
            if resident != rows {
                *mismatched += 1;
                let only_resident: Vec<_> =
                    resident.iter().filter(|x| !rows.contains(x)).take(3).collect();
                let only_rows: Vec<_> =
                    rows.iter().filter(|x| !resident.contains(x)).take(3).collect();
                eprintln!(
                    "PARITY MISMATCH {}::{} @ {:?} — resident {} vs rows {}\n  only-resident: {:?}\n  only-rows: {:?}",
                    sym.package.as_deref().unwrap_or(""),
                    sym.name,
                    path,
                    resident.len(),
                    rows.len(),
                    only_resident,
                    only_rows
                );
            }
        }
    };

    for (path, analysis) in &entries {
        check(&ws, &idx, path, analysis, Some(resolve::RoleMask::VISIBLE), &mut checked, &mut mismatched);
    }
    for (path, analysis, pack) in &pack_entries {
        // Pack routing widens to VISIBLE at set construction — no override.
        check(&ws, pack.as_ref(), path, analysis, None, &mut checked, &mut mismatched);
    }

    println!(
        "refs-parity: {} symbols checked, {} mismatched",
        checked, mismatched
    );
    if mismatched > 0 {
        super::exit_with(1, "exit");
    }
}

/// The relational pre-prune a tier hands its items: the DISTINCT ref-name
/// key set (a name absent here has a provably-empty references projection)
/// and the unused-export keys, `(path, name, row, col)`.
type PruneIndex = (
    std::collections::HashSet<String>,
    std::collections::HashSet<(String, String, usize, usize)>,
);

/// One heatmap work item: an eligible declaration plus the tier facts its
/// row needs — the routing index, the pre-prune (row-store-backed tiers
/// only), the visibility override. Tier-specific behavior arrives as data
/// here, never as a family flag in the row builder.
struct HeatmapItem<'a> {
    routing: &'a (dyn file_analysis::CrossFileLookup + Sync),
    path: &'a std::path::Path,
    analysis: &'a file_analysis::FileAnalysis,
    sym: &'a file_analysis::Symbol,
    prune: Option<&'a PruneIndex>,
    visibility: Option<resolve::RoleMask>,
}

impl HeatmapItem<'_> {
    /// The row store's verdict for this declaration, when its tier has one:
    /// `(forced_fan_in, dead_export_override)`. A name with no reference
    /// row anywhere has a provably-empty projection, so the walk is skipped
    /// and fan-in forced to 0; a `None` tier always takes the full
    /// projection. The dead-export verdict substitutes ONLY alongside a
    /// skipped walk, where it is provably equal to what the projection
    /// would derive (no ref rows at all ⇒ no cross-file references). When
    /// the walk runs, the projection decides: a candidate row is an
    /// over-approximation, so the rows can say "maybe used" for an export
    /// whose every candidate the matcher rejects — a real dead export the
    /// row verdict would mask.
    fn prune_verdict(&self) -> (Option<usize>, Option<bool>) {
        let Some((referenced_names, dead_keys)) = self.prune else {
            return (None, None);
        };
        let sym = self.sym;
        let key = file_analysis::name_match_key(&sym.name);
        let forced = if referenced_names.contains(&key) {
            None // has reference rows — the projection must run
        } else {
            Some(0usize) // no reference row anywhere → provably empty
        };
        let dead_export = forced.map(|_| {
            let is_callable = matches!(
                sym.kind,
                file_analysis::SymKind::Sub | file_analysis::SymKind::Method
            );
            let sel = sym.selection_span.start;
            is_callable
                && dead_keys.contains(&(
                    self.path.to_string_lossy().to_string(),
                    sym.name.clone(),
                    sel.row,
                    sel.column,
                ))
        });
        (forced, dead_export)
    }
}

/// --heatmap <root> [--csv|--html] [--include-deps] [--all] — Code-usage heatmap.
///
/// Emits per-symbol USAGE metrics as a projection of the resolution
/// CandidateSet (`docs/adr/resolution-candidate-set.md`): fan-in is the
/// `references()` image of the set minted at each symbol's declaration —
/// the SAME set the references/rename verbs project from, so heatmap counts
/// cannot diverge from what `textDocument/references` answers, and every
/// construction axis (visibility masks, group/attr field splats, override
/// families, future closure/delegation gating) is inherited for free. It is
/// a reporting view, not a new analysis tier:
///
///   * fan_in  — how many reference sites a symbol has across the workspace
///               (call sites; the symbol's own declaration is excluded).
///   * fan_out — how many DISTINCT callees a sub/method references in its body
///               (cheap intra-file span containment; `null` for packages).
///   * dead_code_candidate — fan_in == 0 AND no reachability guard fired.
///   * dead_export — an EXPORTED sub with zero CROSS-FILE references (the
///               unused-exports view, `docs/adr/relational-ref-index.md`);
///               orthogonal to dead_code_candidate, which the `exported`
///               guard shields. Sound in one direction (row candidates
///               over-approximate references). When the relational store
///               covers the workspace it also PRE-PRUNES the fan-in walk for
///               provably-unreferenced names; the answer is unchanged, only
///               the work is skipped, and it degrades to the full projection
///               when the store is absent (`PERL_LSP_REF_ROWS=0`, cold cache,
///               `--include-deps`).
///
/// HONEST LABEL: a "dead-code candidate" here is an UNREFERENCED SYMBOL — a
/// reachability heuristic, NOT MISRA C:2012 Rule 2.2 dead code (undecidable).
/// We OVER-APPROXIMATE reachability (sound for "is it live?", may under-report
/// dead): a symbol is treated as reachable (never flagged) when it is exported,
/// is a constructor, or — for methods, when ANY file in the workspace dispatches
/// dynamically (`$obj->$method`) — could be reached through an edge the static
/// graph can't see. Failure modes: symbolic code refs (`\&name`, `&{$n}`),
/// `can`/`->$method` with an unresolved name, `AUTOLOAD`, and string `eval` are
/// invisible; function candidates assume none of these reach them.
pub(crate) fn cli_heatmap(root: &str, opts: &[String]) {
    let csv = opts.iter().any(|a| a == "--csv");
    let html = opts.iter().any(|a| a == "--html");
    let include_deps = opts.iter().any(|a| a == "--include-deps");
    // By default only candidate-eligible kinds (subs/methods/packages with a
    // body) are listed; `--all` keeps every counted symbol in `symbols`.
    let emit_all = opts.iter().any(|a| a == "--all");

    let (ws, idx) = cli_full_startup(root, crate::build::language_driver::LanguageScope::All);

    // Pack-language (C/C++/…) files live in per-language sub-indexes, not the
    // Perl `FileStore` — `workspace/symbol` and Mode-B diagnostics sweep these
    // separately, and the heatmap gathers them the same way. Each entry keeps
    // its sub-index so fan-in routes through it (identity minting + the
    // backward reference walk both need the pack cache, not the Perl hub).
    // Snapshot to a Vec for a stable order and to sum dynamic dispatch below.
    let mut pack_entries: Vec<(
        std::path::PathBuf,
        std::sync::Arc<file_analysis::FileAnalysis>,
        std::sync::Arc<module_index::ModuleIndex>,
    )> = Vec::new();
    idx.for_each_pack_index(|_lang, pack| {
        pack.for_each_registered_file(&mut |cached| {
            // Index copies are refs-evicted; fan-out scans + set minting read
            // refs, so take the refs-present view (resident when not evicted,
            // rehydrated otherwise). Batch-CLI-sized cost, not a query path.
            pack_entries.push((
                cached.path.clone(),
                file_analysis::CrossFileLookup::whole_present(pack.as_ref(), cached),
                std::sync::Arc::clone(pack),
            ));
        });
    });
    pack_entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Workspace-level soundness gate. Any dynamic method dispatch makes the
    // static call graph an under-approximation of method reachability, so a
    // zero-fan-in METHOD can't be proven dead. Pack files contribute too
    // (virtual / function-pointer dispatch counts the same).
    let mut dynamic_dispatch_sites: u64 = 0;
    for entry in ws.workspace_raw().iter() {
        dynamic_dispatch_sites += entry.value().dynamic_dispatch_sites as u64;
    }
    for (_p, analysis, _pack) in &pack_entries {
        dynamic_dispatch_sites += analysis.dynamic_dispatch_sites as u64;
    }
    let has_dynamic_dispatch = dynamic_dispatch_sites > 0;

    // References across open + workspace files; `--include-deps` also walks
    // cached @INC modules so a library symbol used only from a dependency
    // shows nonzero fan-in. Applied as the CandidateSet's construction-time
    // visibility so every projection inherits it. The default matches the
    // set's own verdict — every heatmap symbol is workspace-declared, so
    // `references_mask_for` answers EDITABLE by construction — while skipping
    // that verdict's per-symbol whole-store scan.
    let mask = if include_deps {
        resolve::RoleMask::VISIBLE
    } else {
        resolve::RoleMask::EDITABLE
    };
    let scope = override_scope_from_env();

    // Heatmap output keeps its established 1-based/char coordinates
    // (`SourceCache::new(CoordFmt::EditorOneBasedChar)` per gather worker).
    let mut symbol_rows: Vec<serde_json::Value> = Vec::new();
    let mut dead_rows: Vec<serde_json::Value> = Vec::new();
    let mut dead_export_rows: Vec<serde_json::Value> = Vec::new();

    // Stable file order so output is deterministic across runs.
    let mut entries: Vec<(std::path::PathBuf, std::sync::Arc<file_analysis::FileAnalysis>)> = ws
        .workspace_raw()
        .iter()
        .map(|e| {
            // Workspace copies may be refs-evicted; fan-out scans + set
            // minting read refs, so take the refs-present view.
            let cm = std::sync::Arc::new(file_analysis::CachedModule::new(
                e.key().clone(),
                std::sync::Arc::clone(e.value()),
            ));
            (
                e.key().clone(),
                file_analysis::CrossFileLookup::whole_present(&idx, &cm),
            )
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Relational pre-prune (`docs/adr/relational-ref-index.md`, phase 4). The
    // row store answers two things the per-declaration `references()` walk
    // would otherwise rediscover file-by-file: which names have ANY reference
    // row (a name absent here has a provably-empty projection → fan-in 0, walk
    // skipped) and which exported syms have no cross-file reference (the
    // unused-exports view → the dead-export verdict, no walk). Both are SOUND
    // ONLY when the store covers every file the walk would scan, so this is
    // gated: rows enabled (`PERL_LSP_REF_ROWS != 0`), the store available and
    // covering every workspace entry, and EDITABLE scope — `--include-deps`
    // widens the walk to @INC files whose ref rows this Perl store does not
    // witness. Any gate unmet ⇒ `None` ⇒ every declaration takes the full
    // projection and the dead-export verdict is derived from it (unchanged
    // behavior; pure fallback). Pack symbols always take the projection —
    // their per-language store is a separate coverage question left to the
    // sound fallback.
    let rows_env_on = std::env::var("PERL_LSP_REF_ROWS")
        .map(|v| v != "0")
        .unwrap_or(true);
    let perl_prune: Option<PruneIndex> = if rows_env_on && !include_deps {
        match (idx.ref_prune_index(), idx.unused_exported_syms()) {
            (Some((referenced_names, shredded)), Some(dead)) => {
                let covered = entries
                    .iter()
                    .all(|(p, _)| shredded.contains(p.to_string_lossy().as_ref()));
                if covered {
                    let dead_keys = dead
                        .into_iter()
                        .map(|d| (d.path, d.name, d.start_row, d.start_col))
                        .collect();
                    Some((referenced_names, dead_keys))
                } else {
                    None
                }
            }
            _ => None,
        }
    } else {
        None
    };

    // One walk per declaration over a frozen index: memoize the relational
    // retrieval for the sweep (same-named declarations share their
    // candidate set; the shredded-path set is fetched once, not per walk),
    // and size the rehydration LRU to the corpus — the sweep's working set
    // by construction (`cache_policy::sweep_bag_cache_cap`).
    let _retrieval = resolve::RetrievalMemoGuard::open();
    // Every index the sweep rehydrates through: the Perl hub AND each
    // pack-language sub-index (its own LRU over its own DB).
    if let Some(cap) = idx.size_bag_cache_for_sweep() {
        eprintln!("Rehydration cache: {} MiB for the sweep", cap / (1024 * 1024));
    }
    idx.for_each_pack_index(|lang, pack| {
        if let Some(cap) = pack.size_bag_cache_for_sweep() {
            eprintln!("Rehydration cache ({lang}): {} MiB for the sweep", cap / (1024 * 1024));
        }
    });

    // One work item per eligible declaration, through `heatmap_symbol_row`
    // — the one place fan-in/fan-out/dead are computed, so Perl and pack
    // share the exact `references()` projection. `hidden_in_outline` folds
    // arity-variant accessor twins / DSL-import infrastructure into their
    // listed primary (same contract the outline honors);
    // `heatmap_symbol_eligible` keeps it to nameable callables/packages.
    let mut items: Vec<HeatmapItem<'_>> = Vec::new();
    for (path, analysis) in &entries {
        for sym in analysis.symbols() {
            if sym.hidden_in_outline() || !heatmap_symbol_eligible(sym) {
                continue;
            }
            items.push(HeatmapItem { routing: &idx, path, analysis, sym, prune: perl_prune.as_ref(), visibility: Some(mask) });
        }
    }
    // Pack languages route through their own sub-index (VISIBLE-wide —
    // pack workspace files ride the DEPENDENCY role); the set derives that
    // from the origin's stamped language, so no visibility override — and
    // no pre-prune: their refs aren't in the hub's row store.
    for (path, analysis, pack) in &pack_entries {
        for sym in analysis.symbols() {
            if sym.hidden_in_outline() || !heatmap_symbol_eligible(sym) {
                continue;
            }
            items.push(HeatmapItem { routing: pack.as_ref(), path, analysis, sym, prune: None, visibility: None });
        }
    }

    // Each item is one independent `references()` walk, fanned out over the
    // Rayon pool PER DECLARATION (`RAYON_NUM_THREADS` bounds it — the knob
    // that trades wall for peak RSS, since each worker holds its own
    // in-flight rehydrated candidates). Per-file granularity left the wall
    // at the longest file's gather (a 300-sub module) whatever the worker
    // count. Results are collected IN ITEM ORDER and folded below: the
    // report's sorts are stable and `dead_code_candidates` is emitted
    // unsorted, so gather order reaches the output and must equal the
    // serial order (two runs byte-identical is the acceptance). No channel
    // — unlike `--check` nothing streams; the report is sorted whole.
    // `SourceCache` is per-worker (line/column rendering only).
    let _g_gather = crate::util::timings::PhaseGuard::start("cli::heatmap_gather");
    let results: Vec<(serde_json::Value, bool, bool, bool)> = {
        use rayon::prelude::*;
        items
            .par_iter()
            .map_init(
                || SourceCache::new(CoordFmt::EditorOneBasedChar),
                |sources, item: &HeatmapItem<'_>| {
                    let (forced_fan_in, dead_export_override) = item.prune_verdict();
                    heatmap_symbol_row(
                        &ws,
                        item.routing,
                        item.path,
                        item.analysis,
                        item.sym,
                        item.visibility,
                        scope,
                        has_dynamic_dispatch,
                        forced_fan_in,
                        dead_export_override,
                        sources,
                    )
                },
            )
            .collect()
    };
    drop(_g_gather);
    for (row, is_callable, dead, dead_export) in results {
        if dead {
            dead_rows.push(row.clone());
        }
        if dead_export {
            dead_export_rows.push(row.clone());
        }
        if emit_all || is_callable || dead {
            symbol_rows.push(row);
        }
    }

    // Heaviest fan-in first — the hotspots a reader wants up top.
    symbol_rows.sort_by(|a, b| {
        b["fan_in"].as_u64().cmp(&a["fan_in"].as_u64())
            .then_with(|| a["file"].as_str().cmp(&b["file"].as_str()))
            .then_with(|| a["line"].as_u64().cmp(&b["line"].as_u64()))
    });
    // Dead exports read best alphabetically — this is a to-triage list, not a
    // hotspot ranking.
    dead_export_rows.sort_by(|a, b| {
        a["name"].as_str().cmp(&b["name"].as_str())
            .then_with(|| a["file"].as_str().cmp(&b["file"].as_str()))
            .then_with(|| a["line"].as_u64().cmp(&b["line"].as_u64()))
    });

    if csv {
        println!("name,kind,package,file,line,col,fan_in,fan_out,exported,dead_code_candidate,dead_export,reachable_guard");
        let cell = |v: &serde_json::Value| -> String {
            match v {
                serde_json::Value::Null => String::new(),
                serde_json::Value::String(s) => csv_escape(s),
                other => other.to_string(),
            }
        };
        for r in &symbol_rows {
            println!(
                "{},{},{},{},{},{},{},{},{},{},{},{}",
                cell(&r["name"]), cell(&r["kind"]), cell(&r["package"]), cell(&r["file"]),
                cell(&r["line"]), cell(&r["col"]), cell(&r["fan_in"]), cell(&r["fan_out"]),
                cell(&r["exported"]), cell(&r["dead_code_candidate"]), cell(&r["dead_export"]),
                cell(&r["reachable_guard"]),
            );
        }
        return;
    }

    let out = serde_json::json!({
        "schema": "perl-lsp.heatmap.v1",
        "kind": "usage-heatmap",
        "label": "dead_code_candidate: a symbol with no references found — a review queue, not a delete list. Confirm it's unused before removing.",
        "dead_export_label": "dead_export: an EXPORTED sub with no reference from any OTHER file — its export earns nothing, though the module may use it internally. Sound-in-one-direction (rows over-approximate references, so zero cross-file candidates means truly unused by consumers; a nonzero count is never read as 'used'). A review queue for shrinking export surface, not a delete list.",
        "soundness": "Flagging errs toward reachable, so it never flags exported symbols, constructors, framework-synthesized members, packages, or (when the workspace uses dynamic dispatch) any method. C/C++ dead-code is more over-approximate: `main` and address-taken functions are shielded, but a zero-fan-in symbol may still be exported/`extern \"C\"` ABI surface, a callback wired through a function pointer the graph can't follow, or a template instantiated in an unscanned translation unit — treat the list as a review queue.",
        "root": root,
        "files_indexed": entries.len() + pack_entries.len(),
        "dynamic_dispatch_sites": dynamic_dispatch_sites,
        "include_deps": include_deps,
        "summary": {
            "symbols_reported": symbol_rows.len(),
            "dead_code_candidates": dead_rows.len(),
            "dead_exports": dead_export_rows.len(),
        },
        "symbols": symbol_rows,
        "dead_code_candidates": dead_rows,
        "dead_exports": dead_export_rows,
    });

    // `--html` wraps the SAME report in a self-contained, offline viewer
    // (treemap heat + fan-in/fan-out butterfly). No external assets: the
    // report JSON is embedded so the file opens straight off disk.
    if html {
        println!("{}", heatmap_html(&out));
        return;
    }

    println!("{}", serde_json::to_string_pretty(&out).unwrap());
}

/// Render a `--heatmap` report as a single self-contained HTML document.
///
/// The whole report is embedded as a `<script type="application/json">`
/// blob and drawn client-side with dependency-free SVG — no CDN, no build
/// step, opens with a `file://` URL. Two views over the same `symbols[]`:
/// a squarified treemap (tile area = fan_in+1, color = fan_in heat,
/// dead-code candidates outlined) and a back-to-back fan-in/fan-out
/// butterfly of the hottest symbols.
fn heatmap_html(report: &serde_json::Value) -> String {
    // The report carries file paths (attacker-adjacent text), so escape every
    // `<` to its JSON unicode form: that makes a stray `</script>` impossible
    // regardless of content, and `JSON.parse` restores the `<` client-side.
    let data = serde_json::to_string(report)
        .unwrap_or_else(|_| "{}".to_string())
        .replace('<', "\\u003c");
    HEATMAP_HTML_TEMPLATE.replace("__HEATMAP_DATA__", &data)
}

/// Self-contained viewer template; `__HEATMAP_DATA__` is replaced with the
/// embedded report JSON. Kept as one literal so the asset travels with the
/// binary (no runtime file lookup, no build-time bundling).
const HEATMAP_HTML_TEMPLATE: &str = include_str!("../../heatmap.html");

/// Minimal RFC-4180 CSV field escaping: quote when the value contains a
/// comma, quote, or newline; double embedded quotes.
fn csv_escape(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}
