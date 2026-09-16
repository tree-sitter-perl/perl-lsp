//! Pack-language bulk indexing: per-language sub-indexes, the
//! stub-first warm lane, and the deferred persist-writer drain.

use super::*;

/// Index pack-language files (C++/Python/…) into per-language sub-indexes
/// attached to `hub`. GENERIC: registry-driven, so every served pack
/// language gets cross-file from this one walk. Each language keeps its
/// OWN `ModuleIndex` (separate cache — names never comingle across
/// languages), files registered by CLASS name. PERSISTED to a separate
/// `modules-{lang}.db`: warm valid analyses from disk (mtime/size +
/// EXTRACT_VERSION validated), re-analyze only new/changed/stale files,
/// and write the fresh ones back — so a big monorepo doesn't re-analyze
/// every header each launch. `cache_key` is the workspace root the cache
/// dir hashes on (`None` ⇒ no persistence, e.g. tests).
pub fn index_pack_languages(
    root: &std::path::Path,
    cache_key: Option<&str>,
    hub: &crate::index::module_index::ModuleIndex,
    // Per-file progress tick (done, grand_total) across ALL pack languages, so
    // the single pack token's percentage is monotone. Called once per path
    // (warm-skip OR analyzed) — `done` reaches the grand total at the end.
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    // Slice-2 rehydration LRU byte cap (`maxCacheMb * 1 MiB`). The resident
    // pack analyses are bag-stripped after indexing; a type query into an
    // evicted file rehydrates its exact bag from SQLite into this cap. `0`
    // disables retention (rehydrate-and-drop). See `docs/adr/memory-slice-2-lru.md`.
    bag_cache_bytes: usize,
    // Which language families this caller needs. A pack language the scope
    // does not want is not walked at all — on a CPAN-shaped tree the XS/C
    // files beside the `.pm`s are 7.8% of the files and 54% of the startup,
    // and a Perl query cannot consult any of it.
    scope: &crate::build::language_driver::LanguageScope,
) -> usize {
    use ignore::types::TypesBuilder;
    use ignore::WalkBuilder;
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // Persist the transitive macro table across sessions (kills the
    // cold-start gather over perl.h's closure) — pointed at this workspace's
    // cache dir.
    crate::build::cpp_reparse::set_macro_persist_dir(module_cache::cache_dir_for_workspace(cache_key));

    let reg = crate::build::language_driver::LanguageRegistry::with_enabled();

    // Collect every language's paths UP FRONT so the grand total (the progress
    // denominator) is known before any file is analyzed — a single monotone
    // 0→100% stream across all pack languages on the one shared token.
    let mut lang_paths: Vec<(&'static str, Vec<PathBuf>, Vec<PathBuf>)> = Vec::new();
    for driver in reg.pack_drivers() {
        let lang = driver.id();
        if !scope.wants(lang) {
            continue;
        }
        let exts: Vec<&'static str> = driver.extensions().to_vec();
        if exts.is_empty() {
            continue;
        }
        let mut tb = TypesBuilder::new();
        for ext in &exts {
            let _ = tb.add(lang, &format!("*.{ext}"));
        }
        let _ = tb.select(lang);
        let Ok(types) = tb.build() else { continue };
        let mut paths: Vec<PathBuf> = WalkBuilder::new(root)
            .types(types.clone())
            .build()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .filter(|e| e.metadata().map(|m| m.len() < 2_000_000).unwrap_or(false))
            .map(|e| e.into_path())
            .collect();
        // Declared dependency roots (php: composer's vendor packages) sit
        // OUTSIDE the ignore-aware walk on purpose — the project's own
        // .gitignore hides `vendor/`, so these are walked with the git
        // filters off (parents included: the project gitignore must not
        // veto its own dependency tier). Same size cap, same sub-index;
        // dedup covers a committed (non-ignored) vendor tree the main walk
        // already saw.
        let dep_roots = driver.dependency_roots(root);
        for dep_root in &dep_roots {
            paths.extend(
                WalkBuilder::new(dep_root)
                    .types(types.clone())
                    .git_ignore(false)
                    .git_global(false)
                    .git_exclude(false)
                    .parents(false)
                    .build()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                    .filter(|e| e.metadata().map(|m| m.len() < 2_000_000).unwrap_or(false))
                    .map(|e| e.into_path()),
            );
        }
        paths.sort();
        paths.dedup();
        if paths.is_empty() {
            continue;
        }
        lang_paths.push((lang, paths, dep_roots));
    }
    let grand_total: usize = lang_paths.iter().map(|(_, p, _)| p.len()).sum();

    let total = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    for (lang, paths, dep_roots) in lang_paths {
        // Slice-2 bag-rehydration LRU: a loader that opens THIS lang's SQLite
        // conn on demand (rusqlite `Connection` isn't `Sync`, so we open per
        // rehydration miss — rare, and SQLite handles concurrent readers) and
        // decodes the one requested file's full bag.
        let bag_cache = {
            let cache_key_owned = cache_key.map(|s| s.to_string());
            // Retained across rehydrates — same seam as the Perl hub's bag
            // loader; a per-call open on every LRU miss was pure overhead.
            let bag_retained = Arc::new(module_cache::RetainedReader::new());
            let loader = move |path: &std::path::Path, want_bag: bool| {
                // The blob is persisted under the CANONICAL path (both feed
                // paths write `canon`), while the resident copy may be
                // registered under the walk's raw path — canonicalize so the
                // keyed decode matches regardless of which form the caller holds.
                // The discriminated helper survives the readonly-open
                // CANTOPEN/WAL race and names every other miss cause.
                let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                let mut spellings = vec![canon.to_string_lossy().into_owned()];
                let raw = path.to_string_lossy().into_owned();
                if raw != spellings[0] {
                    spellings.push(raw);
                }
                module_cache::open_and_load_diag_retained(
                    &bag_retained,
                    cache_key_owned.as_deref(),
                    lang,
                    &spellings,
                    want_bag,
                )
            };
            Arc::new(crate::index::pack_bag_cache::PackBagCache::new_labeled(
                bag_cache_bytes,
                &format!("pack-{lang}"),
                loader,
            ))
        };
        let pack_index = Arc::new(
            crate::index::module_index::ModuleIndex::new_for_cli().with_bag_cache(bag_cache),
        );
        // Flip this sub-index to pack tier semantics: files under the
        // declared dependency roots are read-only DEPENDENCY, everything
        // else here is the workspace's own (rename-editable). Set even when
        // empty — the flip itself is the fact.
        pack_index.set_dependency_roots(dep_roots);
        // This sub-index's relational-ref-index reader — same per-language DB
        // the drain below writes blobs + rows into.
        {
            let cache_key_owned = cache_key.map(|s| s.to_string());
            pack_index.set_ref_rows_opener(Arc::new(move || {
                module_cache::open_cache_db_readonly(cache_key_owned.as_deref(), lang)
            }));
        }
        let conn = module_cache::open_cache_db(cache_key, lang);
        // A generation built under different analysis inputs (toolchain
        // change — or its probe FAILURE, which empties the system include
        // roots) must not be warmed: hard-clear, same as `validate_inc_paths`.
        if let (Some(ref conn), Some(driver)) = (&conn, reg.for_id(lang)) {
            let _ = module_cache::validate_input_fingerprint(
                conn,
                driver.analysis_input_fingerprint(),
            );
        }

        // WARM: stream valid cached analyses (keyed by file path) one row
        // at a time — register a stripped copy, drop the whole decode before
        // the next row, so at most one full analysis is transiently
        // resident. Version-stale rows re-analyze; rows for files the
        // CURRENT walk no longer includes are dropped, not resurrected.
        let canon_members: std::collections::HashSet<PathBuf> = paths
            .iter()
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
            .collect();
        let mut warmed: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        if let Some(ref conn) = conn {
            module_cache::validate_stub_version(conn);
            let mut dead_rows: Vec<PathBuf> = Vec::new();
            // Deferred past the warm scan — same SQLITE_BUSY_SNAPSHOT
            // rationale as the workspace indexer's backfill.
            let mut pending_backfill: Vec<(
                PathBuf,
                Vec<crate::model::file_analysis::RefRowSeed>,
                Vec<crate::model::file_analysis::SymRowSeed>,
            )> = Vec::new();
            // Stubs whose files warmed through the FULL path this scan —
            // written after it so the next warm takes the stub lane.
            let mut pending_stubs: Vec<(PathBuf, Vec<u8>, (i64, i64))> = Vec::new();
            let rows_present = module_cache::paths_with_ref_rows(conn);
            // A stub's skeleton is stripped by construction; under NO_EVICT
            // the resident copies must stay whole, so stubs are bypassed.
            let use_stubs = eviction_enabled();
            let _n = module_cache::warm_pack_stream_with_stubs(
                conn,
                use_stubs,
                // Dead rows (files the current walk no longer includes) are
                // rejected before any stub/blob bytes are read; stamp-stale
                // dead rows GC too.
                &mut |path| {
                    if canon_members.contains(path) {
                        return true;
                    }
                    dead_rows.push(path.to_path_buf());
                    false
                },
                &mut |path, payload| {
                    use module_cache::{WarmDirective, WarmPayload};
                    let path_str = path.to_string_lossy().into_owned();
                    // Refs strip only when their rows are known present — rows
                    // name candidates for the backward walk; the blob rehydrates.
                    let rows_ok = rows_present.contains(path_str.as_str());
                    let fa = match payload {
                        WarmPayload::Stub(stub) => {
                            if !rows_ok {
                                // Rows missing (REF_ROWS_VERSION wipe): the
                                // re-shred needs the full analysis.
                                return WarmDirective::NeedFull;
                            }
                            // The stub IS a persisted `prepare_pack_parts`
                            // output — rehydrate the token, register through it.
                            let mut parts =
                                crate::index::module_index::PackRegistrationParts::from_warm_stub(stub);
                            parts.record_surface(&pack_index, &path);
                            pack_index.register_symbols_inner(path.clone(), parts);
                            warmed.insert(path);
                            return WarmDirective::Handled;
                        }
                        WarmPayload::Full(_name, fa) => fa,
                    };
                    if !rows_ok {
                        pending_backfill.push((
                            path.clone(),
                            fa.ref_row_seeds(),
                            fa.sym_row_seeds(),
                        ));
                    }
                    // The ladder that used to be `strip_bag && rows_ok`.
                    let level = crate::model::file_analysis::Residency::for_strip(eviction_enabled(), rows_ok);
                    let fully_stripped = level == crate::model::file_analysis::Residency::Skeleton;
                    let mut parts = crate::index::module_index::ModuleIndex::prepare_pack_parts(
                        fa,
                        level,
                    );
                    if fully_stripped {
                        if let Some(blob) = module_cache::encode_stub(
                            parts.feed(),
                            parts.specs(),
                            parts.surface(),
                            parts.arc(),
                        ) {
                            let stamp = module_cache::file_stamp(&path).unwrap_or((0, 0));
                            pending_stubs.push((path.clone(), blob, stamp));
                        }
                    }
                    parts.record_surface(&pack_index, &path);
                    pack_index.register_symbols_inner(path.clone(), parts);
                    warmed.insert(path);
                    WarmDirective::Handled
                },
            );
            module_cache::write_in_chunks(
                conn,
                &pending_stubs,
                256,
                "pack stub backfill",
                |conn, (path, blob, stamp)| {
                    module_cache::save_stub_if_current(
                        conn,
                        &path.to_string_lossy(),
                        blob,
                        *stamp,
                    );
                },
            );
            module_cache::write_in_chunks(
                conn,
                &pending_backfill,
                128,
                "pack row backfill",
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
            for path in dead_rows {
                module_cache::invalidate_generation_tier(
                    conn,
                    &path.to_string_lossy(),
                    "workspace",
                );
            }
        }

        // Analyze only the new/changed/stale files (parallel). Fresh entries
        // stream to a dedicated writer thread over a channel: blobs + rows
        // land in batched txns WHILE workers analyze, so only a bounded
        // window of encoded blobs is in flight and a query racing the bulk
        // index sees each file's rows as soon as its chunk commits.
        // Persistence and eviction are independent: blobs + rows are written
        // whenever a DB exists; only the resident STRIP obeys the eviction
        // switch (the bag/refs are stripped only when recoverable — persisted
        // and non-degraded).
        // Stripped fresh entries defer registration to the writer (post-
        // COMMIT) — same rationale as the workspace indexer's WsFresh. The
        // feed rides along (computed pre-strip); `deferred: false` means the
        // worker registered a whole copy (NO_EVICT) and the writer only
        // persists.
        struct FreshEntry {
            path: PathBuf,
            // For persistence (`include_closure`) — always present. For a
            // deferred entry this is the same arc the token carries.
            arc: Arc<crate::model::file_analysis::FileAnalysis>,
            // `Some` → register the token AFTER the chunk commits (stripped
            // copies). `None` → the worker already registered a whole copy
            // (NO_EVICT/degraded); the writer only persists.
            parts: Option<crate::index::module_index::PackRegistrationParts>,
            blob: crate::index::module_cache::EncodedAnalysis,
            // Warm stub (deferred/stripped entries only) — persisted in the
            // same chunk txn as the blob so the next warm start registers
            // from it without decoding `blob`.
            stub_blob: Option<Vec<u8>>,
            seeds: Vec<crate::model::file_analysis::RefRowSeed>,
            sym_seeds: Vec<crate::model::file_analysis::SymRowSeed>,
            stamp: (i64, i64),
        }
        let (fresh_tx, fresh_rx) = bounded_persist_channel::<FreshEntry>();
        let persist = conn.is_some();
        let strip = persist && eviction_enabled();
        // Every DELIBERATE whole-copy registration under strip increments
        // this; the post-index tripwire flags any fully-resident copy it
        // can't account for (a silent RAM pin no functional test sees).
        let expected_whole = Arc::new(AtomicUsize::new(0));
        let writer_conn = conn;
        let pack_index_writer = Arc::clone(&pack_index);
        let expected_whole_writer = Arc::clone(&expected_whole);
        std::thread::scope(|scope| {
            let writer = scope.spawn(move || {
                // Byte budget for the whole copies a failed chunk retains
                // (see FALLBACK_WHOLE_BYTE_CAP). Per-writer accumulator — the
                // fallback lane is single-threaded (this writer thread).
                let mut fallback_bytes = 0usize;
                run_persist_writer(
                    fresh_rx,
                    writer_conn.as_ref(),
                    "pack persist writer",
                    |conn, batch: &[FreshEntry]| {
                        // Chunk-scoped: a concurrent different-generation
                        // process may wipe/restamp the stubs table mid-run.
                        let stubs_writable = module_cache::stub_version_current(conn);
                        for e in batch {
                            let path_str = e.path.to_string_lossy();
                            module_cache::save_blob_to_db_stamped(
                                conn,
                                &path_str,
                                &e.path,
                                &e.arc.pack.include_closure,
                                &e.blob,
                                "workspace",
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
                            if let Some(sb) = &e.stub_blob {
                                if stubs_writable {
                                    module_cache::save_stub(conn, &path_str, sb);
                                }
                            }
                        }
                    },
                    |e: FreshEntry| {
                        // Stale-pin clear BEFORE the stripped copy is
                        // reachable, so its first rehydration reads the
                        // just-committed blob.
                        pack_index_writer.invalidate_derived_copies(&e.path);
                        if let Some(parts) = e.parts {
                            pack_index_writer.register_symbols_inner(e.path, parts);
                        }
                    },
                    |e: FreshEntry| {
                        pack_index_writer.invalidate_derived_copies(&e.path);
                        if let Some(fa) = e.blob.decode_whole() {
                            let bytes = fa.heap_estimate().total();
                            if fallback_bytes.saturating_add(bytes) <= FALLBACK_WHOLE_BYTE_CAP {
                                fallback_bytes += bytes;
                                // Tripwire-accounted: this whole copy is a
                                // DELIBERATE (failure-bounded) pin.
                                expected_whole_writer.fetch_add(1, Ordering::Relaxed);
                                pack_index_writer.register_symbols(e.path, Arc::new(fa));
                            } else {
                                // Over budget: DROP the resident copy. The
                                // chunk didn't commit, so a stripped copy
                                // would rehydrate to wrong-empty; leaving it
                                // unregistered is honest absence that the next
                                // index/warm re-registers.
                                log::warn!(
                                    "pack persist writer: fallback budget ({} MiB) exceeded — \
                                     dropping resident copy for {:?}; re-indexes next run",
                                    FALLBACK_WHOLE_BYTE_CAP / (1024 * 1024),
                                    e.path,
                                );
                            }
                        }
                    },
                );
            });

            paths.par_iter().for_each(|path| {
                // Tick before any early-out so warm-cache skips also advance the
                // bar — `done` must reach `grand_total`.
                if let Some(cb) = progress {
                    let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                    cb(d, grand_total);
                }
                let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
                if warmed.contains(&canon) {
                    return; // valid cache hit
                }
                let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
                let Some(driver) = reg.for_path(path).filter(|d| d.id() == lang) else { return };
                crate::util::timings::trace_file(&canon);
                crate::util::timings::set_current_file(Some(&canon));
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    analyze_stamped(path, || {
                        let source = std::fs::read_to_string(path).ok()?;
                        Some(driver.analyze_with_path(&source, Some(path)))
                    })
                }));
                crate::util::timings::set_current_file(None);
                if res.is_err() {
                    // eprintln, not log::warn — the CLI runs without a logger,
                    // and a panic that doesn't name its file costs a bisection.
                    eprintln!(
                        "perl-lsp: panic while indexing {}; file skipped",
                        canon.display()
                    );
                }
                if let Ok(Some((analysis, stamp))) = res {
                    // Encode the FULL analysis for the disk write, then strip
                    // the resident copy — one struct, no clone
                    // (`docs/adr/memory-slice-2-lru.md`). Strip only when the
                    // bag/refs are recoverable: persisted and non-degraded
                    // (`save_*` skip degraded rows, so their bag would be lost).
                    let payload = if persist && !analysis.degraded {
                        module_cache::encode_analysis(&analysis).map(|blob| {
                            let seeds: Vec<_> =
                                analysis.ref_row_seeds();
                            let sym_seeds = analysis.sym_row_seeds();
                            (blob, seeds, sym_seeds)
                        })
                    } else {
                        None
                    };
                    if strip && payload.is_some() {
                        // Stripped copy: mint the token pre-strip, hand it to
                        // the writer — it registers after the chunk COMMITS,
                        // so an evicted copy is never reachable before its blob
                        // can rehydrate it.
                        let mut parts = crate::index::module_index::ModuleIndex::prepare_pack_parts(
                            analysis, crate::model::file_analysis::Residency::Skeleton,
                        );
                        let stub_blob = module_cache::encode_stub(
                            parts.feed(),
                            parts.specs(),
                            parts.surface(),
                            parts.arc(),
                        );
                        // Recording before the writer's COMMIT is safe — the
                        // freshness index is session-local.
                        parts.record_surface(&pack_index, &canon);
                        let (blob, seeds, sym_seeds) = payload.unwrap();
                        let arc = Arc::clone(parts.arc());
                        send_to_writer(&fresh_tx, FreshEntry {
                            path: canon.clone(),
                            arc,
                            parts: Some(parts),
                            blob,
                            stub_blob,
                            seeds,
                            sym_seeds,
                            stamp,
                        });
                    } else {
                        // Whole copy: degraded / encode-failed / NO_EVICT.
                        if strip {
                            expected_whole.fetch_add(1, Ordering::Relaxed);
                        }
                        let arc = Arc::new(analysis);
                        pack_index.register_symbols(path.clone(), arc.clone());
                        if let Some((blob, seeds, sym_seeds)) = payload {
                            send_to_writer(&fresh_tx, FreshEntry {
                                path: canon.clone(),
                                arc,
                                parts: None,
                                blob,
                                stub_blob: None,
                                seeds,
                                sym_seeds,
                                stamp,
                            });
                        }
                    }
                    total.fetch_add(1, Ordering::Relaxed);
                    // Residency: this file's merged/expanded macro tables are a
                    // one-shot build input, now dead weight for the rest of the
                    // bulk index (they'd otherwise accumulate to ~1.6 GB of
                    // per-file duplicates on abseil). Drop them the moment the
                    // analysis is built; the shared `header_cache` stays warm so
                    // an on-edit re-gather is a header-BFS, not a cold gather.
                    // Keyed by the same path analyze got, plus its canonical form.
                    let mut drop_set = std::collections::HashSet::with_capacity(2);
                    drop_set.insert(path.clone());
                    drop_set.insert(canon);
                    crate::build::cpp_reparse::evict_gather_caches_keep_headers(&drop_set);
                }
            });

            drop(fresh_tx);
            // The pack twin of the workspace tier's drain window — see
            // `index_perl`'s use of this label for what the number means.
            crate::util::timings::phase("index.writer_drain_after_walk", || {
                let _ = writer.join();
            });
        });
        if strip {
            residency_tripwire(
                &lang.to_string(),
                pack_index.count_fully_resident(),
                expected_whole.load(Ordering::Relaxed),
            );
        }
        hub.attach_pack_index(lang, pack_index);
    }
    if std::env::var_os("PERL_LSP_MEM_REPORT").is_some() {
        eprintln!("[mem-report] {}", crate::build::cpp_reparse::cache_size_report());
    }
    // Heap-composition of the resident pack `FileAnalysis` set — the Slice-2
    // eviction target (`docs/adr/memory-slice-2-lru.md`). Env-gated, inert by
    // default, no query-path cost.
    if std::env::var_os("PERL_LSP_HEAP_DUMP").is_some()
        || std::env::var_os("PERL_LSP_HEAP_JSON").is_some()
        || std::env::var_os("PERL_LSP_HEAP_JSON_DIR").is_some()
    {
        let mut agg = crate::model::file_analysis::HeapBreakdown::default();
        let mut hj = crate::model::file_analysis::HeapJson::new();
        hub.for_each_pack_registered_file(&mut |path, fa| {
            agg.add(&fa.heap_estimate());
            hj.push(path, fa);
        });
        hj.finish();
        eprintln!("[heap-dump] {agg}");
        let (paths, bytes) = crate::model::file_analysis::path_intern::table_stats();
        eprintln!(
            "[heap-dump] path-id table (process-wide, counted once): {} paths, {:.1} MB",
            paths,
            bytes as f64 / (1024.0 * 1024.0)
        );
    }
    total.load(Ordering::Relaxed)
}
