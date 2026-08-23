//! The FileAnalysis blob codec (bincode+zstd), the stamp currencies
//! (file / mtime-nanos / closure), keyed single-file rehydration with
//! WAL-race recovery, and the blob-row writers.

use super::*;

/// zstd compression level for the `analysis` blob. Lower numbers are faster;
/// 3 is zstd's default and gives a solid space/speed tradeoff.
pub(super) const ZSTD_LEVEL: i32 = 3;

/// Discriminated rehydration failure — the honest replacement for the
/// collapsed "loader returned None". Every arm names a distinct on-disk
/// reality so the strict-residency panic points at a mechanism, not a
/// shrug.
#[derive(Debug, Clone)]
pub enum RehydrateMiss {
    /// Couldn't even open the cache DB read-only (SQLite error text).
    OpenerFailed(String),
    /// The `modules` table has no row for any candidate path spelling — not
    /// even through a read-write open (so not the recoverable WAL race).
    NoRow,
    /// Row(s) exist but every candidate blob is NULL/empty.
    EmptyBlob,
    /// Blob present but zstd/bincode decode failed (shape/version skew).
    DecodeFailed,
}

impl std::fmt::Display for RehydrateMiss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RehydrateMiss::OpenerFailed(e) => write!(f, "opener failed: {e}"),
            RehydrateMiss::NoRow => write!(f, "no row for path (read-only and read-write both empty)"),
            RehydrateMiss::EmptyBlob => write!(f, "row present but blob empty/NULL"),
            RehydrateMiss::DecodeFailed => write!(f, "blob decode failed"),
        }
    }
}

/// The row validation stamp: (mtime hashed at NANOSECOND precision, size).
/// Whole seconds miss two same-length writes within one second (generated
/// files, rapid saves) — the M1 staleness window. The `mtime_secs` column
/// name is historical; the value is an opaque equality-checked stamp.
pub fn file_stamp(path: &std::path::Path) -> Option<(i64, i64)> {
    use std::hash::{Hash, Hasher};
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let nanos = mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    nanos.hash(&mut h);
    let size = meta.len() as i64;
    Some((h.finish() as i64, size))
}

/// Raw mtime in nanoseconds since the epoch — an ORDERED source-generation
/// currency, unlike `file_stamp`'s hashed-and-sized equality token. A later
/// save has a strictly greater value (editors write mtime = now, monotone
/// forward even across git operations), so the registration guard can reject
/// a re-analysis built from an EARLIER generation: the `PackInvalidator` swap
/// registers a result only when its event generation is ≥ the one already
/// registered for that path (H9-1 stale-winner race). `None` if unstattable.
pub fn file_mtime_nanos(path: &std::path::Path) -> Option<i64> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let nanos = mtime
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Some(nanos as i64)
}

/// Stamp over every file in an analysis' include closure — the ANALYSIS-INPUT
/// half of a pack row's validation key. A consumer `.c` row bakes its headers'
/// macro splices and type witnesses; its own (stamp, size) can't see a header
/// edit, so the closure stamp must (M2). Perl analyses have an empty closure
/// → 0, so the Perl path pays nothing. `stat_memo` dedups stats across a warm
/// run (closures overlap heavily — op.c and sv.c share ~90% of perl5's tree).
pub(super) fn closure_stamp(
    closure: &crate::model::file_analysis::path_intern::ClosureList,
    stat_memo: &mut std::collections::HashMap<String, (i64, i64)>,
) -> i64 {
    use std::hash::{Hash, Hasher};
    if closure.is_empty() {
        return 0;
    }
    // Commutative fold: the id-list iterates in global mint order, which
    // varies run-to-run (Rayon interning races) — an order-sensitive hash
    // would invalidate every warm row every session, and sorting per file
    // per warm row is n·log n string compares on the path the stamp exists
    // to make cheap. Hash each member independently, fold order-free.
    let mut acc: u64 = 0;
    for p in closure.iter_strs() {
        let stamp = *stat_memo
            .entry(p.as_ref().to_owned())
            .or_insert_with(|| file_stamp(std::path::Path::new(p.as_ref())).unwrap_or((0, -1)));
        let mut h = std::collections::hash_map::DefaultHasher::new();
        p.as_ref().hash(&mut h);
        stamp.hash(&mut h);
        acc = acc.wrapping_add(h.finish());
    }
    acc as i64
}

/// A stored analysis, split into the lane every reader needs and the lane
/// most readers throw away.
///
/// The two halves travel together so a writer cannot persist one without the
/// other: a row whose `analysis` is post-split but whose `bag` never landed
/// would answer every type query with an empty bag — absence read as an
/// answer, which is the failure this whole storage layer is arranged to
/// prevent.
pub struct EncodedAnalysis {
    /// The analysis with its witness bag taken out.
    pub analysis: Vec<u8>,
    /// The baked conclusions for this analysis, zstd+bincode. Rides with the
    /// other halves for the same reason they ride with each other: a writer
    /// that persisted the blob and forgot the map would leave the store
    /// answering ABSENT for the file, and absent means "no answer" rather than
    /// "not baked".
    pub conclusions: Vec<u8>,
    /// The witness bag alone. NEVER empty for a row this code writes — the
    /// `bag IS NULL` test is how a reader tells a pre-split row (bag inline
    /// in `analysis`) from a post-split one, so an empty encoding here would
    /// silently make a new row look old. `zstd` always emits a frame header,
    /// and `encoded_bag_is_never_empty` pins it.
    pub bag: Vec<u8>,
}

impl EncodedAnalysis {
    /// Total stored size, for the writers' byte accounting.
    pub fn len(&self) -> usize {
        self.analysis.len() + self.bag.len() + self.conclusions.len()
    }

    /// Re-decode both halves into the analysis they came from.
    ///
    /// For the bulk writers' failure recovery: a chunk that failed to commit
    /// leaves its resident copies stripped with no committed blob to
    /// rehydrate from, so the in-hand encoding is the only remaining source
    /// and it must come back WHOLE — a bag-less recovery would pin a copy
    /// that answers every type query with silence.
    pub fn decode_whole(&self) -> Option<FileAnalysis> {
        decode_analysis_parts(&self.analysis, Some(&self.bag), true)
    }

    /// The baked map, decoded.
    pub fn conclusion_map(&self) -> Option<crate::model::witnesses::ConclusionMap> {
        let bin = zstd::decode_all(self.conclusions.as_slice()).ok()?;
        bincode::deserialize(&bin).ok()
    }
}

/// Serialize FileAnalysis via bincode then compress with zstd, splitting the
/// witness bag into its own blob.
///
/// The split is why: a refs/symbols reader decodes `analysis` alone, and the
/// bag is 52.9% of the uncompressed bytes bincode would otherwise walk. On
/// the backward-walk workloads that own this path (references, rename,
/// documentHighlight, callHierarchy incoming, heatmap fan-in) 94.9% of
/// decodes want no bag at all.
pub fn encode_analysis(fa: &FileAnalysis) -> Option<EncodedAnalysis> {
    // Split off the bag by value: `bincode` has no field-skipping hook, so
    // the alternative is serializing a clone of the whole analysis. The bag
    // goes back before this returns, so `fa` is observably unchanged.
    let mut owned;
    let (bagless, bag) = {
        owned = fa.clone();
        let bag = std::mem::take(&mut owned.witnesses);
        (&owned, bag)
    };
    let bin = bincode::serialize(bagless).ok()?;
    let analysis = zstd::encode_all(bin.as_slice(), ZSTD_LEVEL).ok()?;
    let bag_bin = bincode::serialize(&bag).ok()?;
    let bag_blob = zstd::encode_all(bag_bin.as_slice(), ZSTD_LEVEL).ok()?;
    debug_assert!(
        !bag_blob.is_empty(),
        "an empty bag blob would read as a pre-split row"
    );
    // Baked here rather than in the writer so the map cannot be forgotten:
    // the same reason the bag and the analysis travel together. Measured at
    // 0.31 ms/file against a 5.4 ms build — 5.8%.
    // Escape hatch and A/B control, same shape as `PERL_LSP_PD_NO_COMBINE`:
    // a new cost on the persist path should be switchable off without a
    // rebuild, so its price can be measured rather than argued about.
    let conclusions = if std::env::var("PERL_LSP_NO_BAKE").is_ok() {
        Vec::new()
    } else {
    crate::util::ghost_stats::timed("persist.bake", || {
        // Every declared sub/method becomes a key, not just those the bag
        // indexed — see `bake_with_symbols`. Without this, a method resolved
        // purely through edges is absent from the map, and absence is read as
        // a proven `None`.
        let syms: Vec<(Option<String>, String, bool)> = fa
            .symbols()
            .iter()
            .map(|s| {
                (
                    s.package.clone(),
                    s.name.clone(),
                    matches!(
                        s.kind,
                        crate::model::file_analysis::SymKind::Sub
                            | crate::model::file_analysis::SymKind::Method
                    ),
                )
            })
            .collect();
        let parents: Vec<(String, Vec<String>)> = fa
            .packages
            .keys()
            .map(|c| (c.clone(), fa.declared_parents(c).to_vec()))
            .collect();
        let map = crate::model::witnesses::bake_full(
            &bag,
            crate::model::witnesses::shared_registry(),
            &fa.packages.keys().cloned().collect(),
            &syms,
            &parents,
        );
        bincode::serialize(&map)
            .ok()
            .and_then(|b| zstd::encode_all(b.as_slice(), ZSTD_LEVEL).ok())
            .unwrap_or_default()
    })
    };
    Some(EncodedAnalysis {
        analysis,
        bag: bag_blob,
        conclusions,
    })
}

/// Decompress + deserialize an analysis blob, installing `bag` when the
/// caller wants the witness lane.
///
/// Row format is NOT inferred here. Pre-split rows carry their bag inside
/// `analysis` and would be indistinguishable from a post-split row whose bag
/// column went missing — both arrive as a `None` bag over an analysis with no
/// witnesses, one benign and one an invariant break. `EXTRACT_VERSION` is
/// bumped for the split and every reader filters on it, so a row reaching
/// this function is always post-split and the ambiguity does not exist.
///
/// A `None` bag therefore means exactly one of two things:
///
///   * `want_bag` false — the caller never fetched the column. Mark the
///     analysis bag-EVICTED so a later type query rehydrates rather than
///     reading an empty bag as "no type facts".
///   * `want_bag` true — the column was NULL on a row that must have one.
///     Same marker, because degrading honestly is what the rest of this
///     layer does with a broken invariant, plus a counter so it is visible
///     instead of silent.
pub fn decode_analysis_parts(
    blob: &[u8],
    bag: Option<&[u8]>,
    want_bag: bool,
) -> Option<FileAnalysis> {
    let mut fa = decode_analysis(blob)?;
    match bag {
        Some(b) => {
            let bin = crate::util::ghost_stats::timed("decode.2_zstd_bag", || zstd::decode_all(b))
                .ok()?;
            fa.witnesses = crate::util::ghost_stats::timed("decode.3_bincode_bag", || {
                bincode::deserialize(&bin)
            })
            .ok()?;
        }
        None => {
            if want_bag {
                crate::util::ghost_stats::count("decode.bag_column_missing");
                log::warn!(
                    "post-split row had no bag column; serving bag-evicted \
                     (types for this file are incomplete until it is rewritten)"
                );
            }
            fa.evict_witness_bag();
        }
    }
    Some(fa)
}

/// Decompress + deserialize an analysis blob.
/// Public for the bulk writers' failure recovery: a failed chunk commit
/// un-strips its resident copies by decoding the blobs it still holds.
pub fn decode_analysis(blob: &[u8]) -> Option<FileAnalysis> {
    // Split because "512 us per rehydrate" is a composite of three stages
    // here plus the SQL fetch outside, and which one dominates decides the
    // fix: a batched fetch only helps stage 1, a codec change only stage 2,
    // and if `after_deserialize` dominates then none of them help — the fix
    // is to stop asking for a fully-indexed analysis per lookup.
    let t = std::time::Instant::now();
    let bin = crate::util::ghost_stats::timed("decode.2_zstd", || zstd::decode_all(blob)).ok()?;
    let mut fa: FileAnalysis = crate::util::ghost_stats::timed("decode.3_bincode", || {
        bincode::deserialize(&bin)
    })
    .ok()?;
    crate::util::ghost_stats::timed("decode.4_after_deser", || fa.after_deserialize());
    // The mean describes two populations: a 512 us average against ~260 ms
    // for the giant blobs the thrash chews on is a 500x spread, and a fix
    // tuned to the mean can miss the tail entirely. Bucket both axes.
    decode_bucket("decode.us", t.elapsed().as_micros() as u64);
    decode_bucket("decode.blob_kb", (blob.len() / 1024) as u64);
    Some(fa)
}

/// Log-ish bucket counter — the distribution behind an average, at the cost
/// of one counter increment.
fn decode_bucket(tag: &str, v: u64) {
    if !crate::util::ghost_stats::enabled() {
        return;
    }
    let label = match v {
        0..=9 => "0-9",
        10..=99 => "10-99",
        100..=999 => "100-999",
        1_000..=9_999 => "1k-10k",
        10_000..=99_999 => "10k-100k",
        100_000..=999_999 => "100k-1M",
        _ => "1M+",
    };
    crate::util::ghost_stats::count(&format!("{tag}.{label}"));
}

/// Keyed single-file decode — the Slice-2 rehydration primitive
/// (`docs/adr/memory-slice-2-lru.md`). Loads ONE file's persisted analysis
/// (full witness bag present) by path, without warming the whole table. The
/// resident pack-index copy has its bag evicted after indexing; a type query
/// that reaches into an evicted file rehydrates the exact bag through here.
/// No mtime/closure validation: the caller (`PackBagCache`) invalidates its
/// entry on file change, and the row's shape is EXTRACT_VERSION-pinned.
pub fn load_one(conn: &Connection, path: &str) -> Option<FileAnalysis> {
    load_one_diag(conn, path, true).ok()
}

/// `load_one` that discriminates the failure (see `RehydrateMiss`) instead
/// of collapsing to `None`, so the rehydration tripwire can name the cause.
///
/// `want_bag` false selects a narrower row: the `bag` column is not named in
/// the SELECT, so SQLite never reads its overflow pages and the decode never
/// walks its bytes. That is the whole point of the split — on backward-walk
/// traffic 94.9% of decodes take this path.
pub fn load_one_diag(
    conn: &Connection,
    path: &str,
    want_bag: bool,
) -> Result<FileAnalysis, RehydrateMiss> {
    // A dual-homed project-lib file has TWO rows for one path (name-keyed
    // import + path-keyed workspace). Prefer a row whose stamp matches the
    // disk (one tier's persist may have failed or lagged, leaving a stale
    // generation); workspace-first is only the tiebreak. Single-row paths
    // deliberately skip stamp validation — the registered generation may
    // legitimately predate an unsaved edit, and the caller invalidates the
    // LRU on file change.
    //
    // The `extract_version` filter is what lets `decode_analysis_parts` skip
    // format inference: a pre-split row carries its bag inside `analysis` and
    // would decode as a post-split row whose bag column vanished. Filtering
    // makes such a row invisible here rather than plausible.
    let mut stmt = conn
        .prepare(if want_bag {
            "SELECT analysis, mtime_secs, file_size, bag FROM modules \
             WHERE path = ?1 AND extract_version = ?2 \
             ORDER BY CASE source WHEN 'workspace' THEN 0 ELSE 1 END"
        } else {
            "SELECT analysis, mtime_secs, file_size, NULL FROM modules \
             WHERE path = ?1 AND extract_version = ?2 \
             ORDER BY CASE source WHEN 'workspace' THEN 0 ELSE 1 END"
        })
        .map_err(|_| RehydrateMiss::NoRow)?;
    // Stage 1 of the rehydrate: the SQL roundtrip that fetches the bytes.
    // Timed separately from the decode because batching the fetch is only
    // worth doing if THIS is the dominant term.
    let rows: Vec<(Option<Vec<u8>>, i64, i64, Option<Vec<u8>>)> =
        crate::util::ghost_stats::timed("decode.1_sql_fetch", || {
            stmt.query_map(params![path, EXTRACT_VERSION], |row| {
                Ok((
                    row.get::<_, Option<Vec<u8>>>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                ))
            })
            .map(|rows| rows.flatten().collect::<Vec<_>>())
        })
        .map_err(|_| RehydrateMiss::NoRow)?;
    if rows.is_empty() {
        return Err(RehydrateMiss::NoRow);
    }
    // The bag rides with the blob it was split from: picking them separately
    // could pair a workspace row's analysis with an import row's bag.
    let pick = |require_stamp: bool| -> Option<(&Vec<u8>, Option<&Vec<u8>>)> {
        rows.iter().find_map(|(blob, m, sz, bag)| {
            let blob = blob.as_ref().filter(|b| !b.is_empty())?;
            if require_stamp && file_stamp(std::path::Path::new(path)) != Some((*m, *sz)) {
                return None;
            }
            Some((blob, bag.as_ref().filter(|b| !b.is_empty())))
        })
    };
    let (blob, bag) = pick(rows.len() > 1)
        .or_else(|| pick(false))
        .ok_or(RehydrateMiss::EmptyBlob)?;
    decode_analysis_parts(blob, bag.map(|b| b.as_slice()), want_bag)
        .ok_or(RehydrateMiss::DecodeFailed)
}

/// The bag-cache rehydration loader body, shared by every per-lang loader
/// closure (Perl hub + pack sub-indexes). Tries each candidate path spelling
/// (canonical vs raw walk path — the blob is written canonical but a
/// resident copy may be keyed raw) and survives the readonly-open CANTOPEN
/// race via `load_with_wal_fallback`'s read-write recovery. Every failure is
/// discriminated for the strict-residency tripwire.
#[cfg(not(test))]
pub fn open_and_load_diag(
    cache_key: Option<&str>,
    lang: &str,
    paths: &[String],
    want_bag: bool,
) -> Result<FileAnalysis, RehydrateMiss> {
    let dir = cache_dir_for_workspace(cache_key)
        .ok_or_else(|| RehydrateMiss::OpenerFailed("no cache dir for workspace".into()))?;
    load_with_wal_fallback(&db_path_for(&dir, lang), paths, want_bag)
}

#[cfg(test)]
pub fn open_and_load_diag(
    _cache_key: Option<&str>,
    _lang: &str,
    _paths: &[String],
    _want_bag: bool,
) -> Result<FileAnalysis, RehydrateMiss> {
    Err(RehydrateMiss::NoRow)
}

/// Rehydrate one file from an explicit cache DB, discriminating every
/// failure and surviving the readonly/WAL-checkpoint race. Path-taking so the
/// whole policy is unit-testable.
///
/// The captured cause: a fresh open of the WAL-mode cache DB transiently
/// returns `SQLITE_CANTOPEN` for BOTH read-only and read-write modes while a
/// sibling writer is mid-`wal_checkpoint`/WAL-reset — SQLite can't set up the
/// `-wal`/`-shm` auxiliaries in that window, and `busy_timeout` doesn't cover
/// the open. The blob is on disk the whole time. `open_reader_retrying` waits
/// the window out with bounded backoff; a recovering read-write read then
/// `wal_checkpoint`s so the next open faces a folded WAL. The strict-residency
/// tripwire still fires only when the window never clears or even a read-write
/// open can't produce the row — a genuinely unreadable/absent blob, a real
/// invariant break.
pub fn load_with_wal_fallback(
    db_path: &std::path::Path,
    paths: &[String],
    want_bag: bool,
) -> Result<FileAnalysis, RehydrateMiss> {
    // `open_reader_retrying` waits out the transient CANTOPEN window; the
    // rw_open closure below then handles the (rarer) opened-but-row-invisible
    // case. Both are the WAL-checkpoint recovery.
    rehydrate_from_opens(
        open_reader_retrying(db_path),
        || open_rw_shared_at(db_path),
        paths,
        want_bag,
    )
}

/// The fallback POLICY, split from the openers so the read-only-open-failure
/// branch is deterministically testable (the real `SQLITE_CANTOPEN` race
/// can't be forced from static file state). `ro` is the read-only open
/// result (`Err` = the open itself failed — the captured CANTOPEN cause);
/// `rw_open` lazily opens the read-write recovery connection.
pub(super) fn rehydrate_from_opens(
    ro: Result<Connection, String>,
    rw_open: impl FnOnce() -> Option<Connection>,
    paths: &[String],
    want_bag: bool,
) -> Result<FileAnalysis, RehydrateMiss> {
    let ro_err = ro.as_ref().err().cloned();
    let mut last = RehydrateMiss::NoRow;
    if let Ok(conn) = &ro {
        for p in paths {
            match load_one_diag(conn, p, want_bag) {
                Ok(fa) => return Ok(fa),
                Err(RehydrateMiss::NoRow) => {}
                Err(other) => last = other,
            }
        }
    }
    // RW fallback covers BOTH a failed readonly open (CANTOPEN race) and a
    // readonly conn that opened but couldn't see the row. Skip it only when
    // readonly gave a definitive non-NoRow verdict (empty/undecodable blob).
    if ro_err.is_some() || matches!(last, RehydrateMiss::NoRow) {
        if let Some(rw) = rw_open() {
            for p in paths {
                if let Ok(fa) = load_one_diag(&rw, p, want_bag) {
                    let _ = rw.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);");
                    return Ok(fa);
                }
            }
        } else if let Some(e) = ro_err {
            // Neither open worked: surface the readonly error text so the
            // tripwire names it (a truly unreadable DB is a real break).
            return Err(RehydrateMiss::OpenerFailed(e));
        }
    }
    Err(last)
}

/// `save_blob_to_db` with a caller-captured `file_stamp` — the bulk drains
/// persist analyses parsed earlier, and stamping at WRITE time would blesses
/// a stale parse with a fresh stamp when the file changed in between (the
/// next warm would then serve the pre-edit analysis as valid). Capture the
/// stamp at parse time; a mid-index edit makes the row invalid by
/// construction.
pub fn save_blob_to_db_stamped(
    conn: &Connection,
    module_name: &str,
    path: &std::path::Path,
    include_closure: &crate::model::file_analysis::path_intern::ClosureList,
    blob: &EncodedAnalysis,
    source: &str,
    stamp: (i64, i64),
) {
    let (mtime, size) = stamp;
    let deps = closure_stamp(include_closure, &mut std::collections::HashMap::new());
    let r = conn.execute(
        "INSERT OR REPLACE INTO modules (module_name, path, mtime_secs, file_size, source, analysis, bag, extract_version, deps_stamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            module_name,
            path.to_string_lossy(),
            mtime,
            size,
            source,
            Some(&blob.analysis),
            Some(&blob.bag),
            EXTRACT_VERSION,
            deps
        ],
    );
    if let Err(e) = r {
        log::warn!("Failed to save module blob for '{}': {}", module_name, e);
    }
    persist_conclusions(conn, &path.to_string_lossy(), blob);
    // A rewritten modules row orphans any prior stub for the path — a stale
    // skeleton paired with a fresh stamp would be served as valid on the
    // next warm. Writers that have a fresh stub re-insert it right after.
    delete_stub(conn, &path.to_string_lossy());
}

/// Write the baked map beside the blob it was derived from.
///
/// At the CURRENT generation, not a new one: persisting a file is that file
/// joining the world as it stands, not a round advancing it. Only a flush
/// advances a generation, and the flush driver is not built yet — until it is,
/// every reader pins the same generation and the retention machinery is
/// correct-but-idle rather than wrong.
///
/// A failure here is logged, never fatal. The blob is already written and
/// remains the derivation of record; a missing map costs a decode, which is
/// the cost we had before this layer existed.
fn persist_conclusions(conn: &Connection, path: &str, enc: &EncodedAnalysis) {
    if enc.conclusions.is_empty() {
        // The bake produced nothing encodable. Leaving no row is right: absent
        // from the STORE means "not baked" (the reader falls back to a decode),
        // which is a different question from a key being absent from a map.
        return;
    }
    let at = current_generation(conn);
    let r = conn.execute(
        "INSERT OR REPLACE INTO conclusions (path, generation, map) VALUES (?1, ?2, ?3)",
        params![path, at.0, enc.conclusions],
    );
    if let Err(e) = r {
        log::warn!("Failed to save conclusions for '{path}': {e}");
    }
}

/// Recompute a persisted row's `deps_stamp` from CURRENT disk state without
/// touching its blob/rows/stub. For consumers of an Unchanged-surface edit:
/// their content is still valid, but a closure member's mtime moved, so the
/// stored stamp would fail the next warm scan and re-trigger the very cold
/// storm the gate prevents in-session. The file's own mtime/size stamp is
/// left alone — a consumer that itself changed on disk stays invalid.
pub fn refresh_deps_stamp(
    conn: &Connection,
    path: &str,
    include_closure: &crate::model::file_analysis::path_intern::ClosureList,
    stat_memo: &mut std::collections::HashMap<String, (i64, i64)>,
) {
    let deps = closure_stamp(include_closure, stat_memo);
    let _ = conn.execute(
        "UPDATE modules SET deps_stamp = ?1 WHERE path = ?2",
        params![deps, path],
    );
}

/// Returns whether the modules row landed — stripping a resident copy is
/// only legal when its blob is actually recoverable.
pub fn save_to_db(
    conn: &Connection,
    module_name: &str,
    result: &Option<Arc<CachedModule>>,
    source: &str,
) -> bool {
    let (path_str, mtime, size, analysis_blob, deps_stamp) = match result {
        Some(cached) => {
            // Degraded analyses (parse/extract failure, skipped gather) must
            // not be persisted: the row would validate on the source file's
            // stamp alone and re-serve the degraded blob every session (H8).
            if cached.analysis.degraded {
                return false;
            }
            let (mtime, size) = file_stamp(&cached.path).unwrap_or((0, 0));
            let blob = encode_analysis(&cached.analysis);
            if blob.is_none() {
                // Encode failure: leave the PREVIOUS row intact — replacing
                // it with a NULL blob would destroy a good generation and
                // warm as a terminal negative sentinel across sessions.
                log::warn!(
                    "Failed to encode analysis for '{}'; keeping prior row",
                    module_name
                );
                return false;
            }
            let deps = closure_stamp(
                &cached.analysis.pack.include_closure,
                &mut std::collections::HashMap::new(),
            );
            (cached.path.to_string_lossy().to_string(), mtime, size, blob, deps)
        }
        None => (String::new(), 0i64, 0i64, None, 0i64),
    };

    let r = conn.execute(
        "INSERT OR REPLACE INTO modules (module_name, path, mtime_secs, file_size, source, analysis, bag, extract_version, deps_stamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            module_name,
            path_str,
            mtime,
            size,
            source,
            analysis_blob.as_ref().map(|b| &b.analysis),
            analysis_blob.as_ref().map(|b| &b.bag),
            EXTRACT_VERSION,
            deps_stamp
        ],
    );
    let ok = match r {
        // A row whose blob failed to ENCODE landed as NULL — not a
        // recoverable generation; stripping against it would lose the bag.
        // (Negative sentinels have no blob by design and nothing to strip.)
        Ok(_) => result.is_none() || analysis_blob.is_some(),
        Err(e) => {
            log::warn!("Failed to save module cache for '{}': {}", module_name, e);
            false
        }
    };
    if !path_str.is_empty() {
        if let Some(enc) = analysis_blob.as_ref() {
            persist_conclusions(conn, &path_str, enc);
        }
        // Same stale-stub guard as `save_blob_to_db_stamped`.
        delete_stub(conn, &path_str);
    }
    ok
}

#[cfg(test)]
pub(super) mod bag_share_probe {
    //! What share of a stored FileAnalysis is the witness bag?
    //!
    //! `rows_for_diag` decodes the whole blob and then strips the bag, so a
    //! rows-axis reader pays zstd + bincode for a lane it discards. That is
    //! only worth acting on if the bag is a large share of the BYTES, and
    //! only in the population whose decodes actually cost — so this reports
    //! the DISTRIBUTION by blob size, never a corpus mean. A mean would
    //! average a few giant analyses into thousands of tiny ones and say the
    //! opposite of what the giants do.
    //!
    //! `cargo test --release bag_share -- --ignored --nocapture`
    use crate::model::file_analysis::FileAnalysis;

    #[test]
    #[ignore]
    fn probe_bag_share_of_stored_bytes() {
        let root = std::path::Path::new("gold-corpus/local/lib/perl5");
        if !root.is_dir() {
            eprintln!("substrate absent — skipping");
            return;
        }
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        collect_pm(root, &mut files, 0);
        files.sort();
        // The single-point grammar accessor — `layering_tests` forbids
        // naming the grammar outside the builder, and it is right to.
        let mut parser = crate::build::builder::create_parser();

        // (zstd bytes whole, zstd bytes bagless, bincode whole, bincode bagless)
        let mut rows: Vec<(usize, usize, usize, usize)> = Vec::new();
        for path in files.iter() {
            let Ok(src) = std::fs::read_to_string(path) else { continue };
            if src.len() > 1_000_000 {
                continue;
            }
            let Some(tree) = parser.parse(&src, None) else { continue };
            let fa = crate::build::builder::build(&tree, src.as_bytes());
            let mut bagless = fa.clone();
            bagless.witnesses = Default::default();
            let (Some(w), Some(b)) = (
                super::encode_analysis(&fa),
                super::encode_analysis(&bagless),
            ) else {
                continue;
            };
            let bw = bincode::serialize(&fa).map(|v| v.len()).unwrap_or(0);
            let bb = bincode::serialize(&bagless).map(|v| v.len()).unwrap_or(0);
            rows.push((w.len(), b.len(), bw, bb));
        }
        assert!(!rows.is_empty(), "no analyses built");

        // Bucketed by STORED size, because the decode cost that matters
        // tracks blob size and the two populations behave differently.
        let buckets: [(&str, usize, usize); 5] = [
            ("   <4 KB", 0, 4 * 1024),
            (" 4-16 KB", 4 * 1024, 16 * 1024),
            ("16-64 KB", 16 * 1024, 64 * 1024),
            ("64-256KB", 64 * 1024, 256 * 1024),
            ("  >256KB", 256 * 1024, usize::MAX),
        ];
        println!("\n{} analyses, sizes are the STORED (zstd) blob\n", rows.len());
        println!("{:<9} {:>6} {:>12} {:>10} {:>10}", "bucket", "n", "zstd bytes", "bag% zstd", "bag% bin");
        let mut tot_w = 0usize;
        let mut tot_b = 0usize;
        for (label, lo, hi) in buckets {
            let sel: Vec<_> = rows.iter().filter(|r| r.0 >= lo && r.0 < hi).collect();
            if sel.is_empty() {
                continue;
            }
            let zw: usize = sel.iter().map(|r| r.0).sum();
            let zb: usize = sel.iter().map(|r| r.1).sum();
            let bw: usize = sel.iter().map(|r| r.2).sum();
            let bb: usize = sel.iter().map(|r| r.3).sum();
            tot_w += zw;
            tot_b += zb;
            println!(
                "{:<9} {:>6} {:>12} {:>9.1}% {:>9.1}%",
                label, sel.len(), zw,
                100.0 * (zw - zb) as f64 / zw as f64,
                100.0 * (bw - bb) as f64 / bw as f64,
            );
        }
        let tot_bw: usize = rows.iter().map(|r| r.2).sum();
        let tot_bb: usize = rows.iter().map(|r| r.3).sum();
        println!(
            "\nwhole corpus: {} zstd bytes, bag is {:.1}% of them",
            tot_w, 100.0 * (tot_w - tot_b) as f64 / tot_w as f64
        );
        // The stage that costs most is bincode deserialize, and it works on
        // the UNCOMPRESSED bytes — so this, not the zstd share, is the bag's
        // share of the expensive half.
        println!(
            "              {} bincode bytes, bag is {:.1}% of them",
            tot_bw, 100.0 * (tot_bw - tot_bb) as f64 / tot_bw as f64
        );
        // Where the BYTES live, which is where the decode seconds live.
        let mut by_size: Vec<_> = rows.clone();
        by_size.sort_by_key(|r| std::cmp::Reverse(r.0));
        let top: usize = (rows.len() / 100).max(1);
        let top_w: usize = by_size[..top].iter().map(|r| r.0).sum();
        let top_b: usize = by_size[..top].iter().map(|r| r.1).sum();
        println!(
            "largest 1% ({} files): {:.1}% of all stored bytes, bag is {:.1}% of them",
            top, 100.0 * top_w as f64 / tot_w as f64,
            100.0 * (top_w - top_b) as f64 / top_w as f64
        );
    }


    /// How big is the CONCLUSION layer next to the derivation it would
    /// replace?
    ///
    /// The conclusion measured here is the POST-FOLD one — the resolved
    /// return per sub, taken through the same registry path a query uses —
    /// not `MethodSurface::ret`, which is projected with no module index and
    /// is therefore the pre-enrichment answer. Two providers with different
    /// enriched returns project byte-identical surfaces, so persisting that
    /// one would store a value the query does not agree with.
    ///
    /// This is a floor on the layer's size, not the whole design: it does not
    /// carry the `ReturnExpr` SHAPES (`Receiver`, `ReceiverPolymorphic`),
    /// where the conclusion is "returns its invocant" rather than a value —
    /// a handful of bytes each, but they must be represented, so the real
    /// layer is somewhat larger than this.
    #[test]
    #[ignore]
    fn probe_conclusion_layer_size() {
        use crate::model::file_analysis::SymKind;
        let root = std::path::Path::new("gold-corpus/local/lib/perl5");
        if !root.is_dir() {
            eprintln!("substrate absent — skipping");
            return;
        }
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        super::bag_share_probe::collect_pm(root, &mut files, 0);
        files.sort();
        let mut parser = crate::build::builder::create_parser();
        let (mut bag_z, mut concl_z, mut bag_b, mut concl_b) = (0usize, 0usize, 0usize, 0usize);
        let (mut subs, mut typed) = (0usize, 0usize);
        for path in files.iter() {
            let Ok(src) = std::fs::read_to_string(path) else { continue };
            if src.len() > 1_000_000 {
                continue;
            }
            let Some(tree) = parser.parse(&src, None) else { continue };
            let fa = crate::build::builder::build(&tree, src.as_bytes());
            let concl: Vec<(String, Option<crate::model::file_analysis::InferredType>)> = fa
                .symbols()
                .iter()
                .filter(|s| matches!(s.kind, SymKind::Sub | SymKind::Method))
                .map(|s| {
                    let t = fa.sub_return_type_at_arity(&s.name, None);
                    (s.name.clone(), t)
                })
                .collect();
            subs += concl.len();
            typed += concl.iter().filter(|(_, t)| t.is_some()).count();
            let (Ok(cb), Ok(bb)) = (
                bincode::serialize(&concl),
                bincode::serialize(&fa.witnesses),
            ) else {
                continue;
            };
            let (Some(cz), Some(bz)) = (
                zstd::encode_all(cb.as_slice(), super::ZSTD_LEVEL).ok(),
                zstd::encode_all(bb.as_slice(), super::ZSTD_LEVEL).ok(),
            ) else {
                continue;
            };
            concl_b += cb.len();
            bag_b += bb.len();
            concl_z += cz.len();
            bag_z += bz.len();
        }
        println!("\nsubs: {subs}, of which the fold gives a return type: {typed} ({:.1}%)",
                 100.0 * typed as f64 / subs.max(1) as f64);
        println!("bag        : {bag_b:>10} bincode  {bag_z:>9} zstd");
        println!("conclusions: {concl_b:>10} bincode  {concl_z:>9} zstd");
        println!("conclusions are {:.1}% of the bag by bincode, {:.1}% by zstd",
                 100.0 * concl_b as f64 / bag_b.max(1) as f64,
                 100.0 * concl_z as f64 / bag_z.max(1) as f64);
    }

    pub(super) fn collect_pm(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>, depth: u32) {
        if depth > 12 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                collect_pm(&p, out, depth + 1);
            } else if p.extension().map(|x| x == "pm").unwrap_or(false) {
                out.push(p);
            }
        }
    }
}

/// How the conclusion bake classifies a real corpus, and how big the map is
/// next to the bag it summarizes.
///
/// `cargo test --release probe_conclusion_bake -- --ignored --nocapture`
#[cfg(test)]
mod bake_probe {
    #[test]
    #[ignore]
    fn probe_conclusion_bake() {
        let root = std::path::Path::new("gold-corpus/local/lib/perl5");
        if !root.is_dir() {
            eprintln!("substrate absent — skipping");
            return;
        }
        let mut files: Vec<std::path::PathBuf> = Vec::new();
        super::bag_share_probe::collect_pm(root, &mut files, 0);
        files.sort();
        let mut parser = crate::build::builder::create_parser();
        let registry = crate::model::witnesses::ReducerRegistry::with_defaults();

        let (mut value, mut return_of, mut open, mut link) = (0usize, 0usize, 0usize, 0usize);
        let (mut demoted, mut no_bare) = (0usize, 0usize);
        let (mut map_bytes, mut bag_bytes, mut n) = (0usize, 0usize, 0usize);
        let mut bake_nanos: u64 = 0;
        let mut build_nanos: u64 = 0;
        for path in files.iter() {
            let Ok(src) = std::fs::read_to_string(path) else { continue };
            if src.len() > 1_000_000 {
                continue;
            }
            let Some(tree) = parser.parse(&src, None) else { continue };
            let tb = std::time::Instant::now();
            let fa = crate::build::builder::build(&tree, src.as_bytes());
            build_nanos += tb.elapsed().as_nanos() as u64;
            let before_demoted = demoted;
            let t0 = std::time::Instant::now();
            let syms: Vec<(Option<String>, String, bool)> = fa
                .symbols()
                .iter()
                .map(|s| {
                    (
                        s.package.clone(),
                        s.name.clone(),
                        matches!(
                            s.kind,
                            crate::model::file_analysis::SymKind::Sub
                                | crate::model::file_analysis::SymKind::Method
                        ),
                    )
                })
                .collect();
            let map = crate::model::witnesses::bake_with_symbols(
                &fa.witnesses,
                &registry,
                &fa.packages.keys().cloned().collect(),
                &syms,
            );
            bake_nanos += t0.elapsed().as_nanos() as u64;
            let _ = before_demoted;
            for c in map.0.values() {
                match c {
                    crate::model::witnesses::Conclusion::Value(_) => value += 1,
                    crate::model::witnesses::Conclusion::ReturnOf(_) => return_of += 1,
                    crate::model::witnesses::Conclusion::Link { .. } => link += 1,
                    crate::model::witnesses::Conclusion::OpenNone => open += 1,
                }
            }
            map_bytes += bincode::serialize(&map).map(|v| v.len()).unwrap_or(0);
            bag_bytes += bincode::serialize(&fa.witnesses).map(|v| v.len()).unwrap_or(0);
            n += 1;
        }
        let _ = (&mut demoted, &mut no_bare);
        let total = value + return_of + open + link;
        println!("\n{n} files, {total} conclusions");
        println!("  Value      {value:>7} ({:.1}%)", pct(value, total));
        println!("  ReturnOf   {return_of:>7} ({:.1}%)", pct(return_of, total));
        println!("  Link       {link:>7} ({:.1}%)", pct(link, total));
        println!("  OpenNone   {open:>7} ({:.1}%)", pct(open, total));
        println!(
            "\nmap {map_bytes} bincode bytes vs bag {bag_bytes} — {:.1}% of the bag",
            pct(map_bytes, bag_bytes)
        );
        // The bake's MARGINAL cost against the build it would ride along with.
        // A bake that costs as much as the analysis it summarizes cannot live
        // in the persist path, whatever it saves later.
        println!(
            "bake {:.0} ms over {n} files ({:.2} ms/file) vs build {:.0} ms — bake is {:.1}% of build",
            bake_nanos as f64 / 1e6,
            bake_nanos as f64 / 1e6 / n.max(1) as f64,
            build_nanos as f64 / 1e6,
            pct(bake_nanos as usize, build_nanos as usize)
        );
    }

    fn pct(a: usize, b: usize) -> f64 {
        if b == 0 { 0.0 } else { 100.0 * a as f64 / b as f64 }
    }
}
