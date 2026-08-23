//! Perl workspace bulk indexing: the gitignore-aware walk plus the
//! shallow entrypoint-script scan, the streaming warm lane, and the
//! deferred persist of fresh analyses.

use super::*;

/// Variant that also registers each indexed file in the given
/// `ModuleIndex` under its primary package name. Used so workspace
/// modules participate in cross-file lookups (method resolution,
/// Handler walks, etc.) without waiting for an on-demand `use`
/// resolve. Without this, `->to('Users#list')` couldn't find
/// `test_files/lib/Users.pm` because nothing ever triggers a
/// module_index populate for workspace files.
/// Does this extensionless file start with a Perl shebang
/// (`#!...perl`)? The entrypoint-script test — `jobs`, `login`,
/// Mojo::Lite apps. Peeks 64 bytes; never called on extensioned files.
fn has_perl_shebang(path: &std::path::Path) -> bool {
    if path.extension().is_some() {
        return false;
    }
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut buf = [0u8; 64];
    let Ok(n) = f.read(&mut buf) else { return false };
    let head = String::from_utf8_lossy(&buf[..n]);
    let first = head.lines().next().unwrap_or("");
    first.starts_with("#!") && first.contains("perl")
}

/// Extensionless Perl entrypoint scripts, found by a SHALLOW (depth-1)
/// shebang scan over the conventional dirs — repo root, `bin/`,
/// `script/` — plus any `extra` dirs (relative to `root`). Shallow +
/// dir-scoped on purpose: entrypoints are direct files in known
/// places, so this never walks a source tree.
///
/// `extra` is the SEAM for a future workspace-config `entrypoint_dirs`
/// knob: today every caller passes `&[]`; wiring config is one line at
/// the call site, no change here. (The config-file reader itself is
/// deliberately deferred until there's a real config story to design.)
pub(super) fn scan_entrypoint_scripts(root: &std::path::Path, extra: &[String]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        vec![root.to_path_buf(), root.join("bin"), root.join("script")];
    dirs.extend(extra.iter().map(|d| root.join(d)));
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            if std::fs::metadata(&p).map(|m| m.len() < 1_000_000).unwrap_or(false)
                && has_perl_shebang(&p)
            {
                out.push(p);
            }
        }
    }
    out
}

pub fn index_workspace_with_index(
    root: &std::path::Path,
    files: &crate::index::file_store::FileStore,
    module_index: Option<&crate::index::module_index::ModuleIndex>,
    // Per-file progress tick (done, total), called from the Rayon workers as
    // files complete. LSP-agnostic: the caller owns any notification / throttle
    // policy. Invoked once per path processed (success OR skip), so `done`
    // reaches `total` at the end.
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    // Fired once when the parallel WALK has processed every file but the
    // persist writer is still draining its backlog (at 100k+ files the drain
    // outlives the walk by minutes). Lets the caller announce the phase
    // honestly instead of sitting at 100% looking hung. The ready gate still
    // opens only on RETURN — registration of stripped fresh copies is
    // deferred to post-commit, so the index is not fully attached until the
    // drain completes.
    walk_done: Option<&(dyn Fn() + Sync)>,
) -> usize {
    use ignore::types::TypesBuilder;
    use ignore::WalkBuilder;
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Extensioned Perl (`*.pm/*.pl/*.t`) — type-pruned at the walk
    // level (cheap; never descends into a JS tree's files).
    let mut types_builder = TypesBuilder::new();
    types_builder.add("perl", "*.pm").unwrap();
    types_builder.add("perl", "*.pl").unwrap();
    types_builder.add("perl", "*.t").unwrap();
    types_builder.select("perl");
    let types = types_builder.build().unwrap();

    let mut paths: Vec<PathBuf> = WalkBuilder::new(root)
        .types(types)
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter(|e| e.metadata().map(|m| m.len() < 1_000_000).unwrap_or(false))
        .map(|e| e.into_path())
        .collect();

    // Extensionless entrypoint SCRIPTS (`#!/usr/bin/env perl` — crm's
    // `jobs`/`login`/… Mojo::Lite apps) carry no glob, so a SHALLOW
    // shebang scan over the conventional entrypoint dirs catches them
    // without enumerating the whole tree. These scripts are exactly
    // where `plugin 'X'` loads live; skipping them blinded the
    // entrypoint-scan lint and goto-def into entrypoint-defined symbols.
    // `&[]` today; the seam for a future workspace-config
    // `entrypoint_dirs` (additive to the built-in root/bin/script).
    paths.extend(scan_entrypoint_scripts(root, &[]));

    let count = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let total = paths.len();

    // Perl workspace persistence (`docs/adr/relational-ref-index.md`,
    // phase 3): blobs + ref rows land in `modules.db` under
    // `source='workspace'` (path-keyed, like the pack tier), warm starts
    // skip re-parsing unchanged files, and — once persisted — the resident
    // copies are refs/bag-stripped like every other index tier. The cache
    // key is the hub's workspace-root spelling, the SAME one the resolver
    // thread and the hub's readers hash, so all three address one DB.
    let cache_key = module_index.and_then(|i| i.workspace_root());
    let conn = module_cache::open_cache_db(cache_key.as_deref(), "perl");
    // Validate-and-stamp the plugin fingerprint BEFORE writing: the resolver
    // thread runs the same (atomic) check concurrently on this DB, and an
    // unstamped fresh DB reads as a mismatch there — it would hard-clear the
    // rows this indexer is about to write.
    if let Some(ref conn) = conn {
        let _ = module_cache::validate_plugin_fingerprint(
            conn,
            &crate::build::plugin::rhai_host::plugin_fingerprint(),
        );
        // Beside the plugin gate, and for the same reason: a cached artifact
        // that describes a derivation we no longer run. This one clears
        // conclusions only — the blobs stay, because the repair is a re-bake.
        let _ = module_cache::validate_conclusion_fingerprint(
            conn,
            module_cache::CONCLUSION_FINGERPRINT,
        );
    }
    // Persistence and eviction are independent: blobs + rows are written
    // whenever a DB exists (the parity harness runs under PERL_LSP_NO_EVICT
    // and still needs the relational side populated); only the resident
    // STRIP obeys the eviction switch.
    let persist = conn.is_some();
    let strip = persist && eviction_enabled();

    // The walk's canonical membership set: warm rows are admitted only for
    // files the CURRENT walk still includes — a path newly gitignored (or
    // newly over the size cap) must not resurrect from its cached row, and
    // its stale generation is dropped.
    let canon_members: std::collections::HashSet<PathBuf> = paths
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect();

    // WARM: stream valid 'workspace' rows — record projections from the
    // full decode, strip, register, drop. Stale/changed rows fall through
    // to the parallel re-parse below.
    let mut warmed: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    if let Some(ref conn) = conn {
        let mut dead_rows: Vec<PathBuf> = Vec::new();
        // Backfill shreds are DEFERRED past the warm scan: writing inside
        // the streaming SELECT's transaction pins a read snapshot that a
        // concurrent resolver-thread commit turns into SQLITE_BUSY_SNAPSHOT
        // (not retried by the busy handler) — silently voiding the whole
        // backfill. Row-less files stay WHOLE this session (refs resident);
        // their rows land below for the next one.
        let mut pending_backfill: Vec<(
            PathBuf,
            Vec<crate::model::file_analysis::RefRowSeed>,
            Vec<crate::model::file_analysis::SymRowSeed>,
        )> = Vec::new();
        let rows_present = module_cache::paths_with_ref_rows(conn);
        let (_n, _stale, gone) =
            module_cache::warm_cache_streaming(conn, "workspace", &mut |_name, path, mut fa| {
                if !canon_members.contains(&path) {
                    dead_rows.push(path);
                    return;
                }
                let path_str = path.to_string_lossy();
                // Refs strip ONLY when their rows are known present — an
                // evicted copy without rows is invisible to the backward
                // walk (rows name candidates; the blob rehydrates).
                let rows_ok = rows_present.contains(path_str.as_ref());
                if !rows_ok {
                    pending_backfill.push((
                        path.clone(),
                        fa.ref_row_seeds(),
                        fa.sym_row_seeds(),
                    ));
                }
                if let Some(idx) = module_index {
                    // The loader-shape half of these projections reads the
                    // witness bag (`expr_type_at_span` over the config
                    // literal's `Expr` witnesses), and the warm scan hands
                    // out bag-evicted copies. Reading one anyway does not
                    // fail — it records NO shape, so the closed
                    // `HashWithKeys` a `plugin 'X', {...}` load contributes
                    // simply never exists on a warm start and every
                    // diagnostic downstream of it goes quiet. Rehydrate for
                    // the files that actually carry loads: entrypoints and
                    // little else, so this is a handful of point fetches,
                    // not a re-decode of the workspace.
                    if !fa.plugin.loads.is_empty() && fa.bag_is_evicted() {
                        match module_cache::load_one(conn, &path.to_string_lossy()) {
                            Some(whole) => idx.record_workspace_projections(&path, &whole),
                            None => idx.record_workspace_projections(&path, &fa),
                        }
                    } else {
                        idx.record_workspace_projections(&path, &fa);
                    }
                }
                // Registration-owned strip: the name/edge feeds read the
                // WHOLE analysis, then the requested axes evict, then the
                // stripped arc is stored (feeds must never see an emptied
                // `symbols`).
                // The ladder that used to be `strip_rows = strip_bag && rows_ok`.
                let level = crate::model::file_analysis::Residency::for_strip(
                    eviction_enabled(),
                    rows_ok,
                );
                let arc = match module_index {
                    Some(idx) => idx.register_workspace_stripping(
                        path.clone(),
                        fa,
                        level,
                    ),
                    None => {
                        // No index (CLI-less warm): no feeds to extract —
                        // strip and store.
                        fa.evict_to(level);
                        std::sync::Arc::new(fa)
                    }
                };
                files.insert_workspace_arc(path.clone(), arc);
                count.fetch_add(1, Ordering::Relaxed);
                warmed.insert(path);
            });
        module_cache::write_in_chunks(
            conn,
            &pending_backfill,
            128,
            "workspace row backfill",
            |conn, (path, seeds, sym_seeds)| {
                if let Err(e) = module_cache::shred_derived_rows(
                    conn,
                    &path.to_string_lossy(),
                    "workspace",
                    seeds,
                    sym_seeds,
                ) {
                    log::warn!("Failed to backfill derived rows for {:?}: {}", path, e);
                }
            },
        );
        // Rows whose FILE is gone. The membership check above never sees
        // them — the scan skips a row it cannot stamp — so without this they
        // are never collectable at all and the store keeps a dead generation
        // per deleted file forever.
        dead_rows.extend(gone);
        for path in dead_rows {
            module_cache::invalidate_generation_tier(
                conn,
                &path.to_string_lossy(),
                "workspace",
            );
        }
    }

    // Fresh entries stream to a dedicated writer over a channel: the writer
    // persists (batched txns) WHILE workers parse, so only a small window of
    // blobs+seeds is ever in flight (never the whole tree's), and a query
    // racing the bulk index sees each file's rows as soon as its chunk
    // commits. Parse-time stamps ride along so a mid-index edit invalidates
    // the row by construction.
    // Entries whose resident copy was STRIPPED defer their residency
    // registration to the writer (post-COMMIT): an evicted copy registered
    // before its blob exists rehydrates to nothing and serves wrong-empty.
    struct WsFresh {
        path: PathBuf,
        /// The copy to mirror into the FileStore on `deferred` entries
        /// (stripped; the feed half already ran on the whole analysis).
        arc: std::sync::Arc<crate::model::file_analysis::FileAnalysis>,
        /// `Some` → register the residency token AFTER the chunk commits
        /// (only when an index exists; `None` covers packageless / no-index
        /// deferred entries and every persist-only whole copy).
        parts: Option<crate::index::module_index::WorkspaceRegistrationParts>,
        /// Register + mirror in the writer after COMMIT. `false` = the
        /// worker already registered a WHOLE copy (NO_EVICT); persist only.
        deferred: bool,
        blob: crate::index::module_cache::EncodedAnalysis,
        seeds: Vec<crate::model::file_analysis::RefRowSeed>,
        sym_seeds: Vec<crate::model::file_analysis::SymRowSeed>,
        closure: crate::model::file_analysis::path_intern::ClosureList,
        stamp: (i64, i64),
    }
    let (fresh_tx, fresh_rx) = bounded_persist_channel::<WsFresh>();
    let timing = crate::util::timings::is_enabled();

    // Deliberate whole-copy accounting for the workspace-tier residency
    // tripwire — the Perl twin of the pack indexer's counter.
    let expected_whole = Arc::new(AtomicUsize::new(0));
    let expected_whole_writer = Arc::clone(&expected_whole);

    // The Connection moves INTO the writer thread (rusqlite connections are
    // Send, not Sync); nothing after the scope needs it.
    let writer_conn = conn;
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            // Same failure-bounded whole-copy budget as the pack writer.
            let mut fallback_bytes = 0usize;
            run_persist_writer(
                fresh_rx,
                writer_conn.as_ref(),
                "workspace persist writer",
                |conn, batch: &[WsFresh]| {
                    for e in batch {
                        let path_str = e.path.to_string_lossy();
                        module_cache::save_blob_to_db_stamped(
                            conn, &path_str, &e.path, &e.closure, &e.blob, "workspace",
                            e.stamp,
                        );
                        if let Err(err) = module_cache::shred_derived_rows(
                            conn, &path_str, "workspace", &e.seeds, &e.sym_seeds,
                        ) {
                            log::warn!(
                                "Failed to shred derived rows for {:?}: {}",
                                e.path,
                                err
                            );
                        }
                    }
                },
                |e: WsFresh| {
                    if let Some(idx) = module_index {
                        // Clear any stale LRU pin BEFORE the stripped copy
                        // becomes reachable, so its first rehydration reads
                        // the just-committed blob.
                        idx.invalidate_bag_cache(&e.path);
                    }
                    if e.deferred {
                        files.insert_workspace_arc(e.path.clone(), e.arc.clone());
                        if let (Some(idx), Some(parts)) = (module_index, e.parts) {
                            idx.register_workspace_residency(e.path, parts);
                        }
                    }
                },
                |e: WsFresh| {
                    // The chunk never landed but the copies were stripped
                    // for it. The blob in hand IS the whole analysis —
                    // register full copies instead, so nothing is lost
                    // beyond the persistence itself (disk full / lock storm
                    // stays loud AND self-heals) — up to the budget.
                    if let Some(idx) = module_index {
                        idx.invalidate_bag_cache(&e.path);
                    }
                    if let Some(fa) = e.blob.decode_whole() {
                        let bytes = fa.heap_estimate().total();
                        if fallback_bytes.saturating_add(bytes) > FALLBACK_WHOLE_BYTE_CAP {
                            // Over budget: DROP (the chunk didn't commit, so a
                            // stripped copy has no blob to rehydrate from —
                            // honest absence, re-indexed next run).
                            log::warn!(
                                "workspace persist writer: fallback budget ({} MiB) exceeded — \
                                 dropping resident copy for {:?}; re-indexes next run",
                                FALLBACK_WHOLE_BYTE_CAP / (1024 * 1024),
                                e.path,
                            );
                            return;
                        }
                        fallback_bytes += bytes;
                        let arc = std::sync::Arc::new(fa);
                        files.insert_workspace_arc(e.path.clone(), arc.clone());
                        if let Some(idx) = module_index {
                            let _ = idx.register_workspace_resident(e.path.clone(), arc);
                            // A deliberate whole pin — account it so the
                            // tripwire flags only UNEXPLAINED residents.
                            expected_whole_writer.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                },
            );
        });

        // Force the plugin registry — plugin load AND pattern/flow query
        // compilation — to initialize once, single-threaded, before
        // the parallel build below. Otherwise the first `build()` to trigger
        // the registry's OnceLock stalls every other Rayon worker on it,
        // charging ~1s of one-time compile to whichever files happen to block.
        let _ = crate::build::plugin::default_plugin_registry();

        paths.par_iter().for_each(|path| {
            // Blobs are keyed canonical (matches the warm rows + the CLI's
            // canonicalized origin staging); register under the same spelling
            // so cold and warm runs key the stores identically.
            let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
            if warmed.contains(&canon) {
                if let Some(cb) = progress {
                    let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                    cb(d, total);
                }
                return;
            }
            crate::util::timings::trace_file(&canon);
            crate::util::timings::set_current_file(Some(&canon));
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                analyze_stamped(path, || {
                    // Per-file segments accumulate rather than print: a
                    // `[PHASE]` line per region per file is ~4,800 lines/s at
                    // corpus scale and turns the run it is measuring into a
                    // different run. Shares come from the totals.
                    let source = crate::util::ghost_stats::timed("walk.read", || std::fs::read_to_string(path)).ok()?;
                    let mut parser = create_parser();
                    let t_parse = if timing { Some(std::time::Instant::now()) } else { None };
                    let tree = crate::util::ghost_stats::timed("walk.parse", || parser.parse(&source, None))?;
                    let parse_dur = t_parse.map(|s| s.elapsed()).unwrap_or_default();
                    let t_build = if timing { Some(std::time::Instant::now()) } else { None };
                    let analysis = crate::util::ghost_stats::timed("walk.build", || {
                        crate::build::builder::build(&tree, source.as_bytes())
                    });
                    let build_dur = t_build.map(|s| s.elapsed()).unwrap_or_default();
                    if timing {
                        crate::util::timings::record_built(
                            path.strip_prefix(root).unwrap_or(path).display().to_string(),
                            parse_dur,
                            build_dur,
                        );
                    }
                    Some(analysis)
                })
            }));

            match result {
                // `analyze_stamped` returning None covers BOTH a failed
                // read/parse and a changed-under-us stamp — the watcher (or
                // next warm) owns the fresher truth either way.
                Ok(Some((mut analysis, stamp))) => {
                    // Projections that read the bag run on the whole
                    // analysis; the persisted generation is encoded whole;
                    // only then is the resident copy stripped.
                    if let Some(idx) = module_index {
                        crate::util::ghost_stats::timed("walk.projections", || {
                            idx.record_workspace_projections(&canon, &analysis)
                        });
                    }
                    let payload = if persist && !analysis.degraded {
                        crate::util::ghost_stats::timed("walk.encode", || module_cache::encode_analysis(&analysis)).map(|blob| {
                            // One tag over both seed shreds so the sample
                            // count stays one-per-file and the average reads
                            // as "seed cost per file".
                            let (seeds, sym_seeds) = crate::util::ghost_stats::timed(
                                "walk.row_seeds",
                                || (analysis.ref_row_seeds(), analysis.sym_row_seeds()),
                            );
                            // MEASUREMENT-ONLY (PERL_LSP_QUAL_DUMP=<path>): the
                            // shred-time (key, kind, qual) distribution per file,
                            // for costing a class axis on the ref rows. Formatted
                            // HERE because `RefRowSeed` is a Model type and the
                            // util tier may not see it. Remove once that question
                            // is answered.
                            if crate::util::qual_dump::enabled() {
                                let p = canon.to_string_lossy();
                                let mut buf = String::with_capacity(seeds.len() * 48);
                                for sd in &seeds {
                                    buf.push_str(&format!(
                                        "{p}\t{}\t{}\t{}\t{}\n",
                                        sd.kind,
                                        sd.qual_kind,
                                        sd.qual.as_deref().unwrap_or("?"),
                                        sd.key
                                    ));
                                }
                                crate::util::qual_dump::append(&buf);
                            }
                            let closure = analysis.pack.include_closure.clone();
                            (blob, seeds, sym_seeds, closure)
                        })
                    } else {
                        None
                    };
                    if strip && payload.is_some() {
                        // Stripped copy: feed half now (whole analysis),
                        // residency + FileStore mirror in the writer AFTER
                        // its chunk commits. Until then the file reads as
                        // "not yet indexed" — never wrong-empty.
                        let (arc, parts) = match module_index {
                            Some(idx) => {
                                let mut parts = crate::util::ghost_stats::timed("walk.prepare_parts", || {
                                    idx.prepare_workspace_parts(&canon, analysis, crate::model::file_analysis::Residency::Skeleton)
                                });
                                // Takes the surface out rather than cloning it:
                                // the writer's registration half discards it, so
                                // it would otherwise ride the queue only to be
                                // dropped.
                                crate::util::ghost_stats::timed("walk.record_surface", || parts.record_surface(idx, &canon));
                                (std::sync::Arc::clone(parts.arc()), Some(parts))
                            }
                            None => {
                                analysis.evict_to(crate::model::file_analysis::Residency::Skeleton);
                                (std::sync::Arc::new(analysis), None)
                            }
                        };
                        let (blob, seeds, sym_seeds, closure) = payload.unwrap();
                        send_to_writer(&fresh_tx, WsFresh {
                            path: canon.clone(),
                            arc,
                            parts,
                            deferred: true,
                            blob,
                            seeds,
                            sym_seeds,
                            closure,
                            stamp,
                        });
                    } else {
                        // Whole copy (no persistence, degraded, or NO_EVICT):
                        // register immediately (no strip — the whole-copy door);
                        // still persist when a blob exists. `false, false` mints
                        // the token without evicting or recording surface, so
                        // this path's freshness behavior is unchanged.
                        let arc = match module_index {
                            Some(idx) => {
                                let parts =
                                    idx.prepare_workspace_parts(&canon, analysis, crate::model::file_analysis::Residency::Whole);
                                let arc = std::sync::Arc::clone(parts.arc());
                                idx.register_workspace_residency(canon.clone(), parts);
                                // Deliberate whole pin (unpersistable /
                                // degraded / NO_EVICT) — accounted for the
                                // tripwire.
                                expected_whole.fetch_add(1, Ordering::Relaxed);
                                arc
                            }
                            None => std::sync::Arc::new(analysis),
                        };
                        if let Some((blob, seeds, sym_seeds, closure)) = payload {
                            send_to_writer(&fresh_tx, WsFresh {
                                path: canon.clone(),
                                arc: arc.clone(),
                                parts: None,
                                deferred: false,
                                blob,
                                seeds,
                                sym_seeds,
                                closure,
                                stamp,
                            });
                        }
                        files.insert_workspace_arc(canon.clone(), arc);
                    }
                    count.fetch_add(1, Ordering::Relaxed);
                }
                Ok(None) => { /* parse failed, skip */ }
                Err(_) => {
                    // eprintln, not log::warn — the CLI runs without a logger,
                    // and a panic that doesn't name its file costs a bisection.
                    eprintln!(
                        "perl-lsp: panic while indexing {}; file skipped",
                        canon.display()
                    );
                }
            }
            crate::util::timings::set_current_file(None);
            if let Some(cb) = progress {
                let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                cb(d, total);
            }
        });

        drop(fresh_tx);
        if let Some(cb) = walk_done {
            cb();
        }
        // The window the readiness gate is held open by AFTER every file has
        // been analyzed — the "100% walked, still saving" phase. Bounding the
        // persist queue makes this a function of the queue depth rather than
        // of the corpus: the walk cannot outrun the writer by more than the
        // depth, so only that much is left to drain here. Timed because that
        // is the claim, and it is the number to check on a real tree.
        crate::util::timings::phase("index.writer_drain_after_walk", || {
            let _ = writer.join();
        });
    });

    // Workspace-tier residency tripwire, mirroring the pack indexer's:
    // gated off under NO_EVICT (everything is deliberately whole there).
    // Timed alongside the drain: it sweeps every registered file, so it is a
    // second O(corpus) term between the walk and the readiness gate, and the
    // window is only the drain's if the other terms are measured too.
    if let Some(idx) = module_index {
        if eviction_enabled() {
            crate::util::timings::phase("index.residency_tripwire", || {
                residency_tripwire(
                    "workspace",
                    idx.count_fully_resident(),
                    expected_whole.load(Ordering::Relaxed),
                );
            });
        }
    }

    count.load(Ordering::Relaxed)
}
