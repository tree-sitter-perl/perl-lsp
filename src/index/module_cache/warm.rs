//! Warm scans: row-generation classification and the three warm lanes
//! (name-keyed @INC, streaming workspace, stub-first pack).

use super::*;

/// Streaming warm: identical row validation to `warm_cache`, but each valid
/// positive entry is handed to `each` one at a time and nothing is retained
/// here — the caller registers a stripped resident copy and drops the full
/// decode before the next row. This bounds the warm-path transient to ONE
/// file's full analysis instead of the whole table's (the 884 MB abseil
/// warm peak vs its 276 MB cold peak). Negative sentinels are skipped —
/// the pack warm path has no consumer for them. Returns
/// (valid_rows_seen, stale_names) like `warm_cache`.
/// Returns `(valid_rows_seen, stale_names, missing_paths)`. `missing_paths`
/// are rows whose FILE is gone from disk: only this scan can see them (the
/// caller's membership check never runs for a row the scan skips), and
/// nothing else will ever collect them, so the caller must.
pub fn warm_cache_streaming(
    conn: &Connection,
    source: &str,
    each: &mut dyn FnMut(String, PathBuf, FileAnalysis),
) -> (usize, Vec<String>, Vec<PathBuf>) {
    let mut stmt = match conn.prepare(
        "SELECT module_name, path, mtime_secs, file_size, analysis, extract_version, deps_stamp \
         FROM modules WHERE source = ?1",
    ) {
        Ok(s) => s,
        Err(_) => return (0, Vec::new(), Vec::new()),
    };
    let map_row = |row: &rusqlite::Row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<Vec<u8>>>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
        ))
    };
    let rows = match stmt.query_map(params![source], map_row) {
        Ok(r) => r,
        Err(_) => return (0, Vec::new(), Vec::new()),
    };

    let mut count = 0usize;
    let mut stale_names = Vec::new();
    let mut missing: Vec<PathBuf> = Vec::new();
    let mut stat_memo: std::collections::HashMap<String, (i64, i64)> =
        std::collections::HashMap::new();
    for row in rows.map(|r| {
        if let Err(ref e) = r {
            log::warn!("cache warm scan error (row skipped / scan may truncate): {}", e);
        }
        r
    }).flatten() {
        let (module_name, path_str, cached_mtime, cached_size, analysis_blob, row_extract_version, row_deps_stamp) = row;
        let path = match classify_row_generation(
            &path_str,
            cached_mtime,
            cached_size,
            row_extract_version,
        ) {
            RowGeneration::Current(p) => p,
            RowGeneration::VersionStale => {
                stale_names.push(module_name);
                continue; // stale rows re-analyze; don't register the old shape
            }
            RowGeneration::Missing => {
                missing.push(PathBuf::from(&path_str));
                continue;
            }
            RowGeneration::Sentinel | RowGeneration::StampStale => continue,
        };
        let Some(blob) = analysis_blob.filter(|b| !b.is_empty()) else {
            continue;
        };
        // `decode_analysis_parts(.., None, false)` and not the raw
        // `decode_analysis`: the scan deliberately does not fetch the bag
        // column, and only the former marks the result bag-EVICTED. Without
        // the marker a bagless copy is indistinguishable from one that
        // genuinely has no type facts, so every downstream reader takes the
        // empty bag at face value instead of rehydrating — silently, with no
        // error and no empty-vs-evicted question it could even ask.
        let _g_dec = crate::util::ghost_stats::ScopedNs::start("warm.decode");
        let Some(fa) = decode_analysis_parts(&blob, None, false) else {
            log::warn!("Failed to decode cached analysis for '{}', skipping", module_name);
            continue;
        };
        if closure_stamp(&fa.pack.include_closure, &mut stat_memo) != row_deps_stamp {
            continue;
        }
        drop(_g_dec);
        count += 1;
        let _g_reg = crate::util::ghost_stats::ScopedNs::start("warm.register");
        each(module_name, path, fa);
    }
    (count, stale_names, missing)
}

/// One persisted row's generation verdict — the shared first half of every
/// warm scan's validity check (the second half, the closure stamp, runs
/// after decode on whichever struct is in hand). A new validity axis goes
/// HERE, not into one loop.
pub(crate) enum RowGeneration {
    /// Sentinel/negative row — no warm consumer.
    Sentinel,
    /// The file is GONE from disk. Distinct from `StampStale` on purpose:
    /// a changed file re-analyses and re-shreds, while a deleted one never
    /// comes back on its own, so its rows are only collectable HERE. They
    /// used to fall into `StampStale` and be skipped before the walk's
    /// membership check could see them, which made them immortal — the
    /// store grew a dead generation per deleted file forever, and the
    /// dead-export view counted a deleted file as a live user.
    Missing,
    /// The file changed on disk — skip silently.
    StampStale,
    /// Blob shape predates EXTRACT_VERSION — the caller decides between
    /// skip (workspace tiers re-analyze from the walk) and queue-for-
    /// re-resolve (the name-keyed @INC tier).
    VersionStale,
    Current(PathBuf),
}

pub(crate) fn classify_row_generation(
    path_str: &str,
    cached_mtime: i64,
    cached_size: i64,
    row_extract_version: i64,
) -> RowGeneration {
    if path_str.is_empty() {
        return RowGeneration::Sentinel;
    }
    let path = PathBuf::from(path_str);
    match file_stamp(&path) {
        Some((m, sz)) if m == cached_mtime && sz == cached_size => {}
        Some(_) => return RowGeneration::StampStale,
        None => return RowGeneration::Missing,
    }
    if row_extract_version < EXTRACT_VERSION {
        return RowGeneration::VersionStale;
    }
    RowGeneration::Current(path)
}

/// One admitted warm row, in the lane the store could serve it from.
pub enum WarmPayload {
    /// The compact stub decoded and validated — register from it.
    Stub(WarmStub),
    /// The full analysis (stub absent/declined/disabled) — the
    /// one-transient-decode lane. Carries the row's module_name.
    Full(String, FileAnalysis),
}

/// The consumer's answer for a `WarmPayload::Stub`: `NeedFull` re-serves
/// the same row through the blob lane (e.g. derived rows are missing and
/// re-shredding needs the whole analysis). Ignored for `Full`.
#[derive(PartialEq, Eq)]
pub enum WarmDirective {
    Handled,
    NeedFull,
}

/// The register-from-Surface warm scan: stream row METADATA (never the
/// `analysis` column — its overflow pages are what the 9-minute wall is
/// made of), validate stamps/version, and serve each valid file through
/// ONE consumer callback, stub lane first. `admit` runs BEFORE any blob
/// or stub bytes are touched — rejected paths (dead rows) cost only the
/// metadata read, and the caller records them inside the predicate.
/// Closure-stamp validation runs on whichever struct is in hand (stub
/// skeleton or full analysis); both carry the pinned `include_closure`.
pub fn warm_pack_stream_with_stubs(
    conn: &Connection,
    use_stubs: bool,
    admit: &mut dyn FnMut(&std::path::Path) -> bool,
    each: &mut dyn FnMut(PathBuf, WarmPayload) -> WarmDirective,
) -> usize {
    // Cheap integer/text columns first; the stub BLOB is column 6 and is
    // only accessed AFTER the stamp/version checks pass (lazy row access —
    // rejected rows never pull the blob's pages). `length(m.analysis)`
    // reads the record header, not the blob: a stub must never register
    // when its full blob can't rehydrate it (NULL/empty ⇒ wrong-empty
    // answers instead of a fresh re-analysis).
    let mut stmt = match conn.prepare(
        "SELECT m.module_name, m.path, m.mtime_secs, m.file_size, m.extract_version, \
                m.deps_stamp, s.stub, length(m.analysis) \
         FROM modules m LEFT JOIN stubs s ON s.path = m.path",
    ) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let mut rows = match stmt.query([]) {
        Ok(r) => r,
        Err(_) => return 0,
    };
    let mut count = 0usize;
    let mut stat_memo: std::collections::HashMap<String, (i64, i64)> =
        std::collections::HashMap::new();
    loop {
        let row = match rows.next() {
            Ok(Some(r)) => r,
            Ok(None) => break,
            Err(e) => {
                log::warn!("pack warm scan error (scan may truncate): {}", e);
                break;
            }
        };
        let Ok(path_str) = row.get::<_, String>(1) else { continue };
        if !path_str.is_empty() && !admit(std::path::Path::new(&path_str)) {
            continue;
        }
        let (Ok(cached_mtime), Ok(cached_size)) =
            (row.get::<_, i64>(2), row.get::<_, i64>(3))
        else {
            continue;
        };
        let Ok(row_extract_version) = row.get::<_, i64>(4) else { continue };
        let path = match classify_row_generation(
            &path_str,
            cached_mtime,
            cached_size,
            row_extract_version,
        ) {
            RowGeneration::Current(p) => p,
            // Workspace tier: stale rows re-analyze from the walk.
            _ => continue,
        };
        let Ok(row_deps_stamp) = row.get::<_, i64>(5) else { continue };
        let blob_len = row.get::<_, Option<i64>>(7).ok().flatten().unwrap_or(0);
        if use_stubs && blob_len > 0 {
            let stub_blob = row.get::<_, Option<Vec<u8>>>(6).ok().flatten();
            if let Some(stub) = stub_blob.as_deref().and_then(decode_stub) {
                if closure_stamp(&stub.skeleton.pack.include_closure, &mut stat_memo)
                    != row_deps_stamp
                {
                    continue;
                }
                if each(path.clone(), WarmPayload::Stub(stub)) == WarmDirective::Handled {
                    count += 1;
                    continue;
                }
                // NeedFull (e.g. derived rows missing — the full analysis
                // is needed to re-shred): fall through to the blob.
            }
        }
        let module_name = row.get::<_, String>(0).unwrap_or_default();
        let Some(fa) = load_one(conn, &path_str) else { continue };
        if closure_stamp(&fa.pack.include_closure, &mut stat_memo) != row_deps_stamp {
            continue;
        }
        count += 1;
        let _ = each(path, WarmPayload::Full(module_name, fa));
    }
    count
}

/// None-over-Some, atomically: the workspace registrar may register this
/// name concurrently, and a sentinel clobber answers "no such module" all
/// session. The entry API holds the shard lock across check+insert.
fn insert_sentinel_guarded(
    cache: &DashMap<String, Option<Arc<CachedModule>>>,
    module_name: String,
) {
    match cache.entry(module_name) {
        dashmap::mapref::entry::Entry::Occupied(mut o) => {
            if o.get().is_none() {
                o.insert(None);
            }
        }
        dashmap::mapref::entry::Entry::Vacant(v) => {
            v.insert(None);
        }
    }
}

/// The name-keyed (@INC) warm lane. `cache` takes the name-slot winner and
/// `all_defs` the whole provider relation — the table holds one row per
/// FILE, so a name with two providers warms as two rows and the winner is
/// DERIVED (@INC order is unavailable here, so it is the smallest path —
/// the same order-independent tie-break the workspace tier uses; the
/// resolver's own insert corrects it to true @INC order once it runs).
pub fn warm_cache(
    conn: &Connection,
    cache: &DashMap<String, Option<Arc<CachedModule>>>,
    all_defs: &DashMap<String, Vec<Arc<CachedModule>>>,
    strip: bool,
) -> (usize, Vec<String>) {
    // Name-keyed warm serves the @INC tier only; 'workspace' rows are
    // path-keyed and stream through `warm_cache_streaming` — loading them
    // here would pollute the module cache with path-string keys. The tag is
    // the keying scheme, never the writer: every name-keyed producer shares
    // `NAME_KEYED_SOURCE` so a new writer cannot strand its rows unread.
    let mut stmt = match conn.prepare(
        "SELECT module_name, path, mtime_secs, file_size, analysis, extract_version, deps_stamp FROM modules WHERE source = ?1",
    ) {
        Ok(s) => s,
        Err(_) => return (0, Vec::new()),
    };

    let rows = match stmt.query_map(params![NAME_KEYED_SOURCE], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, Option<Vec<u8>>>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
        ))
    }) {
        Ok(r) => r,
        Err(_) => return (0, Vec::new()),
    };

    let mut count = 0usize;
    let mut stale_names = Vec::new();
    // Closure members overlap heavily across rows; stat each once per warm.
    let mut stat_memo: std::collections::HashMap<String, (i64, i64)> =
        std::collections::HashMap::new();
    for row in rows.map(|r| {
        if let Err(ref e) = r {
            log::warn!("cache warm scan error (row skipped / scan may truncate): {}", e);
        }
        r
    }).flatten() {
        let (module_name, path_str, cached_mtime, cached_size, analysis_blob, row_extract_version, row_deps_stamp) = row;

        let path = match classify_row_generation(
            &path_str,
            cached_mtime,
            cached_size,
            row_extract_version,
        ) {
            // Negative sentinel: empty path + NULL blob — a remembered miss.
            // Never over a Some: the workspace indexer may have registered
            // this name concurrently (the insert_into_cache guard's warm
            // twin — a clobber here answers "no such module" all session).
            RowGeneration::Sentinel => {
                insert_sentinel_guarded(cache, module_name);
                count += 1;
                continue;
            }
            RowGeneration::StampStale => continue,
            // The provider file is gone. Its rows can only be seen here, so
            // drop the generation rather than carry it forever.
            RowGeneration::Missing => {
                let _ = crate::index::module_cache::invalidate_generation(conn, &path_str);
                continue;
            }
            // @INC tier policy: stale entries still load, queued for
            // priority re-resolve.
            RowGeneration::VersionStale => {
                stale_names.push(module_name.clone());
                PathBuf::from(&path_str)
            }
            RowGeneration::Current(p) => p,
        };

        match analysis_blob {
            Some(blob) if !blob.is_empty() => {
                match decode_analysis_parts(&blob, None, false) {
                    Some(mut fa) => {
                        // A pack file's analysis bakes its headers (splices,
                        // witnesses, closure): the row is valid only while the
                        // whole closure is unchanged, not just the file itself.
                        if closure_stamp(&fa.pack.include_closure, &mut stat_memo) != row_deps_stamp {
                            continue;
                        }
                        // Strip AT INSERT (long-lived processes): the blob
                        // just decoded IS the recoverable generation, and
                        // stripping here (not a post-hoc sweep) can never
                        // touch a copy some OTHER path registered
                        // whole-for-a-reason (writer fallback, watcher).
                        if strip && !fa.degraded {
                            fa.evict_to(crate::model::file_analysis::Residency::RowsOnly);
                        }
                        let cand = Arc::new(CachedModule::new(path, Arc::new(fa)));
                        adopt_warm_provider(cache, all_defs, &module_name, cand);
                        count += 1;
                    }
                    None => {
                        log::warn!("Failed to decode cached analysis for '{}', skipping", module_name);
                    }
                }
            }
            _ => {
                // Blob missing / empty on a NON-sentinel row (a legacy
                // NULL-blob write): re-resolve rather than remember a miss —
                // a sentinel here would be a terminal cross-session false
                // negative for a real module.
                if path_str.is_empty() {
                    insert_sentinel_guarded(cache, module_name);
                } else {
                    stale_names.push(module_name);
                }
                count += 1;
            }
        }
    }

    (count, stale_names)
}

/// Adopt one warmed row as a provider of `module_name`: replace-by-path in
/// the relation (a re-warm must not stack), then re-derive the name-slot
/// winner from the SET. Row order out of SQLite is not @INC order and is
/// not stable, so the winner cannot be "whichever row arrived last" —
/// smallest path makes repeat warms byte-identical.
fn adopt_warm_provider(
    cache: &DashMap<String, Option<Arc<CachedModule>>>,
    all_defs: &DashMap<String, Vec<Arc<CachedModule>>>,
    module_name: &str,
    cand: Arc<CachedModule>,
) {
    let mut v = all_defs.entry(module_name.to_string()).or_default();
    match v.iter().position(|c| c.path == cand.path) {
        Some(i) => v[i] = cand.clone(),
        None => v.push(cand.clone()),
    }
    let winner = v.iter().min_by(|a, b| a.path.cmp(&b.path)).cloned();
    drop(v);
    if let Some(w) = winner {
        cache.insert(module_name.to_string(), Some(w));
    }
}
