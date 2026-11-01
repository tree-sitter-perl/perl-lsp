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
    let mut stmt = match conn.prepare(sql::WARM_MODULES_BY_SOURCE.registered()) {
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
        let Some(fa) = decode_analysis_parts(&blob, None, false) else {
            log::warn!("Failed to decode cached analysis for '{}', skipping", module_name);
            continue;
        };
        if closure_stamp(&fa.pack.include_closure, &mut stat_memo) != row_deps_stamp {
            continue;
        }
        count += 1;
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
    /// Blob shape is not THIS binary's `EXTRACT_VERSION` — older OR newer.
    /// A newer stamp is a downgrade (an editor rolled its plugin back
    /// against a warm cache); its blobs decode to nothing this binary can
    /// read, and admitting them registered stubs nothing could rehydrate:
    /// 739 files indexed, 0 symbols, 0 diagnostics, silently. The caller
    /// decides between skip (workspace tiers re-analyze from the walk) and
    /// queue-for-re-resolve (the name-keyed @INC tier).
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
    if row_extract_version != EXTRACT_VERSION {
        return RowGeneration::VersionStale;
    }
    RowGeneration::Current(path)
}

#[cfg(test)]
mod row_generation_tests {
    use super::*;

    /// A row stamped by ANY other binary version is foreign — newer as much
    /// as older. The `<` this replaced let a downgrade warm-load blobs it
    /// could not decode and answer empty.
    #[test]
    fn foreign_extract_version_is_stale_in_both_directions() {
        let dir = std::env::temp_dir().join(format!("rowgen_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("x.pm");
        std::fs::write(&file, "package X; 1;\n").unwrap();
        let path = file.to_string_lossy().into_owned();
        let (mtime, size) = crate::index::module_cache::file_stamp(&file).unwrap();
        let verdict = |v: i64| classify_row_generation(&path, mtime, size, v);
        assert!(matches!(verdict(EXTRACT_VERSION), RowGeneration::Current(_)));
        assert!(matches!(verdict(EXTRACT_VERSION - 1), RowGeneration::VersionStale));
        assert!(matches!(verdict(EXTRACT_VERSION + 1), RowGeneration::VersionStale));
        let _ = std::fs::remove_dir_all(&dir);
    }
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
/// So does `inputs_current`, the driver's word on what the analysis read
/// from its path (the module it was filed under); a row it rejects is
/// skipped like a stamp-stale one.
pub fn warm_pack_stream_with_stubs(
    conn: &Connection,
    use_stubs: bool,
    admit: &mut dyn FnMut(&std::path::Path) -> bool,
    inputs_current: &mut dyn FnMut(&std::path::Path, &FileAnalysis) -> bool,
    each: &mut dyn FnMut(PathBuf, WarmPayload) -> WarmDirective,
) -> usize {
    // The stub BLOB is only accessed AFTER the stamp/version checks pass
    // (lazy row access — rejected rows never pull the blob's pages). A stub
    // must never register when its full blob can't rehydrate it (NULL/empty
    // ⇒ wrong-empty answers instead of a fresh re-analysis).
    let mut stmt = match conn.prepare(sql::WARM_PACK_WITH_STUBS.registered()) {
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
                    || !inputs_current(&path, &stub.skeleton)
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
        if closure_stamp(&fa.pack.include_closure, &mut stat_memo) != row_deps_stamp
            || !inputs_current(&path, &fa)
        {
            continue;
        }
        count += 1;
        let _ = each(path, WarmPayload::Full(module_name, fa));
    }
    count
}

/// The name-keyed (import-tier) warm lane, for every language's store:
/// each persisted landing, one NAME at a time, handed to `land` with the
/// files that answered it — or with none, for a remembered miss. The table
/// holds one row per FILE, so a name with two providers warms as both. The
/// map's precedence order is not stored, so they arrive in path order (the
/// order-independent tie-break the workspace tier uses), which repeats
/// byte-identically across warms. `land` returning false stops the scan:
/// the map moved under it, and what it would land is asked again. Returns
/// the rows warmed and the names whose rows predate this extractor (landed
/// all the same, and asked again).
pub fn warm_cache(
    conn: &Connection,
    land: &mut dyn FnMut(&str, Vec<Arc<CachedModule>>) -> bool,
) -> (usize, Vec<String>) {
    // 'workspace' rows are path-keyed and stream through
    // `warm_cache_streaming`. The tag is the keying scheme, never the
    // writer: every name-keyed producer shares `NAME_KEYED_SOURCE` so a new
    // writer cannot strand its rows unread. The order is the primary key's,
    // so the scan walks its index and a name's rows arrive together.
    let mut stmt = match conn.prepare(sql::WARM_MODULES_BY_NAME.registered()) {
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

    /// One name's rows: the files that warmed, and whether a remembered
    /// miss was among them.
    struct Group {
        name: String,
        providers: Vec<Arc<CachedModule>>,
        miss: bool,
    }
    fn flush(group: Option<Group>, land: &mut dyn FnMut(&str, Vec<Arc<CachedModule>>) -> bool) -> bool {
        match group {
            // A file outranks a remembered miss of the same name.
            Some(g) if !g.providers.is_empty() || g.miss => land(&g.name, g.providers),
            _ => true,
        }
    }

    let mut count = 0usize;
    let mut stale_names = Vec::new();
    let mut group: Option<Group> = None;
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
        if group.as_ref().is_none_or(|g| g.name != module_name) {
            if !flush(group.take(), land) {
                return (count, stale_names);
            }
            group = Some(Group { name: module_name.clone(), providers: Vec::new(), miss: false });
        }
        let g = group.as_mut().expect("opened above");

        let path = match classify_row_generation(
            &path_str,
            cached_mtime,
            cached_size,
            row_extract_version,
        ) {
            // Negative sentinel: empty path + NULL blob — a remembered miss.
            RowGeneration::Sentinel => {
                g.miss = true;
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
            // Stale entries still load, queued for priority re-resolve.
            RowGeneration::VersionStale => {
                stale_names.push(module_name.clone());
                PathBuf::from(&path_str)
            }
            RowGeneration::Current(p) => p,
        };

        match analysis_blob {
            Some(blob) if !blob.is_empty() => match decode_analysis_parts(&blob, None, false) {
                Some(fa) => {
                    // A pack file's analysis bakes its headers (splices,
                    // witnesses, closure): the row is valid only while the
                    // whole closure is unchanged, not just the file itself.
                    if closure_stamp(&fa.pack.include_closure, &mut stat_memo) != row_deps_stamp {
                        continue;
                    }
                    g.providers.push(Arc::new(CachedModule::new(path, Arc::new(fa))));
                    count += 1;
                }
                None => {
                    log::warn!("Failed to decode cached analysis for '{}', skipping", module_name);
                }
            },
            _ => {
                // Blob missing / empty on a NON-sentinel row (a legacy
                // NULL-blob write): re-resolve rather than remember a miss —
                // a sentinel here would be a terminal cross-session false
                // negative for a real module.
                if path_str.is_empty() {
                    g.miss = true;
                } else {
                    stale_names.push(module_name);
                }
                count += 1;
            }
        }
    }
    flush(group.take(), land);

    (count, stale_names)
}
