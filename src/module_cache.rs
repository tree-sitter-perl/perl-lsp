//! SQLite persistence for the module cache (schema v9).
//!
//! Stores a full `Option<FileAnalysis>` per module, serialized via bincode
//! and compressed with zstd. Validates entries against mtime + file size to
//! detect stale data. Invalidates the entire cache when `@INC` changes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use dashmap::DashMap;
use rusqlite::{params, Connection};

use crate::file_analysis::FileAnalysis;
use crate::module_index::CachedModule;

const SCHEMA_VERSION: &str = "9";

/// Bumped when the builder's analysis output changes shape in a way that
/// invalidates cached blobs. Unlike `SCHEMA_VERSION`, this does not drop
/// the table — stale entries are re-resolved lazily with priority.
pub const EXTRACT_VERSION: i64 = 171;

/// zstd compression level for the `analysis` blob. Lower numbers are faster;
/// 3 is zstd's default and gives a solid space/speed tradeoff.
const ZSTD_LEVEL: i32 = 3;

pub fn cache_base_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("perl-lsp"));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return Some(PathBuf::from(home).join(".cache").join("perl-lsp"));
    }
    None
}

pub fn cache_dir_for_workspace(workspace_root: Option<&str>) -> Option<PathBuf> {
    let base = cache_base_dir()?;
    match workspace_root {
        Some(root) => {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            root.hash(&mut hasher);
            Some(base.join(format!("{:016x}", hasher.finish())))
        }
        None => Some(base),
    }
}

/// Per-language DB filename — Perl keeps `modules.db` (back-compat), every
/// pack language gets its own `modules-{lang}.db` so names never comingle on
/// disk (a Perl `Box` and a C++ class `Box` live in different files). The
/// ONE spelling both openers share.
fn db_path_for(dir: &std::path::Path, lang: &str) -> PathBuf {
    if lang == "perl" {
        dir.join("modules.db")
    } else {
        dir.join(format!("modules-{lang}.db"))
    }
}

#[cfg(not(test))]
pub fn open_cache_db(workspace_root: Option<&str>, lang: &str) -> Option<Connection> {
    let dir = cache_dir_for_workspace(workspace_root)?;
    std::fs::create_dir_all(&dir).ok()?;
    let db_path = db_path_for(&dir, lang);
    log::info!("Module cache: {:?}", db_path);

    match Connection::open(&db_path) {
        Ok(conn) => {
            let _ = conn.execute_batch("PRAGMA journal_mode=WAL;");
            // Two writers share the Perl DB (resolver thread + workspace
            // indexer); a busy writer must wait, not fail its txn — a failed
            // commit after resident copies were stripped is unrecoverable
            // for the session.
            let _ = conn.busy_timeout(std::time::Duration::from_secs(10));
            match init_schema(&conn) {
                Ok(()) => Some(conn),
                Err(e) => {
                    // BUSY/LOCKED is contention (a sibling writer mid-init,
                    // e.g. the one-time idx_modules_path build) — deleting
                    // the live DB under its feet loses every blob the other
                    // writer stripped against. Only recreate on real
                    // corruption/shape failures.
                    if matches!(
                        e.sqlite_error_code(),
                        Some(rusqlite::ErrorCode::DatabaseBusy)
                            | Some(rusqlite::ErrorCode::DatabaseLocked)
                    ) {
                        log::warn!("Cache DB busy during init; running without cache: {}", e);
                        return None;
                    }
                    log::warn!("Cache DB schema init failed: {}. Recreating.", e);
                    drop(conn);
                    let _ = std::fs::remove_file(&db_path);
                    let conn = Connection::open(&db_path).ok()?;
                    let _ = conn.execute_batch("PRAGMA journal_mode=WAL;");
                    let _ = conn.busy_timeout(std::time::Duration::from_secs(10));
                    init_schema(&conn).ok()?;
                    Some(conn)
                }
            }
        }
        Err(e) => {
            log::warn!("Failed to open cache DB: {}", e);
            None
        }
    }
}

#[cfg(test)]
pub fn open_cache_db(_workspace_root: Option<&str>, _lang: &str) -> Option<Connection> {
    None
}

/// Read-only open for query-path consumers (the relational retrieval, bag
/// rehydration): no schema init, no WAL pragma churn — the writer created
/// the schema. Returns `None` when the DB file doesn't exist yet (nothing
/// persisted → no candidates), or in tests.
#[cfg(not(test))]
pub fn open_cache_db_readonly(workspace_root: Option<&str>, lang: &str) -> Option<Connection> {
    let dir = cache_dir_for_workspace(workspace_root)?;
    open_cache_reader_at(&db_path_for(&dir, lang))
}

/// Resilient query-path reader open (`open_reader_retrying`): retries across
/// the transient CANTOPEN window a writer's WAL checkpoint opens instead of
/// returning an empty result.
/// Routing every reader — bag rehydration, the relational-ref reader, warm
/// streaming — through here keeps the writer's WAL-checkpoint window from
/// degrading their results to silent absence. `None` only when the window
/// never clears (DB absent / truly unreadable).
pub fn open_cache_reader_at(db_path: &std::path::Path) -> Option<Connection> {
    open_reader_retrying(db_path).ok()
}

/// Open the cache reader across the transient window a writer's WAL
/// checkpoint/reset opens. During it a fresh open of the WAL-mode DB (SQLite
/// setting up the `-wal`/`-shm` auxiliaries) returns `SQLITE_CANTOPEN` for
/// BOTH read-only and read-write modes — and
/// `busy_timeout` does NOT cover the open itself. Each attempt tries
/// read-only then read-write (a read-write open additionally recovers a WAL
/// a read-only open can't map); bounded backoff (~0.26 s total) waits the
/// window out. `Err` only when the window never clears — a genuinely
/// unreadable DB, a real invariant break the strict tripwire should catch.
pub fn open_reader_retrying(db_path: &std::path::Path) -> Result<Connection, String> {
    let mut delay = std::time::Duration::from_millis(2);
    let mut last = String::new();
    for attempt in 0..10 {
        match open_readonly_at(db_path) {
            Ok(c) => {
                if attempt > 0 {
                    log::warn!(
                        "cache reader open {db_path:?} recovered read-only after \
                         {attempt} retr{} (transient WAL-checkpoint CANTOPEN window)",
                        if attempt == 1 { "y" } else { "ies" }
                    );
                }
                return Ok(c);
            }
            Err(e) => last = e,
        }
        // Reaching here means the read-only open just failed; a read-write
        // open that succeeds recovered the transient window. Log even on
        // attempt 0 so an RW recovery is observable, never silent.
        if let Some(c) = open_rw_shared_at(db_path) {
            log::warn!(
                "cache reader open {db_path:?} recovered read-write on attempt \
                 {attempt} (read-only hit the transient CANTOPEN window)"
            );
            return Ok(c);
        }
        if attempt < 9 {
            std::thread::sleep(delay);
            delay = (delay * 2).min(std::time::Duration::from_millis(50));
        }
    }
    Err(last)
}

/// Read-only open of an explicit DB file. Path-taking so the rehydration
/// logic is unit-testable without the cache-dir plumbing.
pub fn open_readonly_at(db_path: &std::path::Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("readonly open {db_path:?}: {e}"))?;
    let _ = conn.busy_timeout(std::time::Duration::from_secs(10));
    Ok(conn)
}

/// Read-WRITE open (no CREATE) of an explicit, already-persisted DB file —
/// the WAL-checkpoint recovery open. A fresh `SQLITE_OPEN_READ_ONLY` open of a
/// WAL-mode cache DB transiently fails with `SQLITE_CANTOPEN` while a
/// sibling writer is mid-`wal_checkpoint` (the -wal is being truncated and
/// the -shm reset; a read-only conn can't rebuild the wal-index in that
/// window). A read-write open recovers the WAL and, via `busy_timeout`,
/// waits the writer out — so the blob that is on disk the whole time stays
/// reachable. The captured cause of the strict-residency crash.
pub fn open_rw_shared_at(db_path: &std::path::Path) -> Option<Connection> {
    let conn = Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let _ = conn.busy_timeout(std::time::Duration::from_secs(10));
    Some(conn)
}

#[cfg(test)]
pub fn open_cache_db_readonly(_workspace_root: Option<&str>, _lang: &str) -> Option<Connection> {
    None
}

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

/// Bumped when the ROW format of the relational ref index changes shape.
/// Unlike `EXTRACT_VERSION` (which governs the blobs), a mismatch only wipes
/// the derived `refs`/`files`/`strings` tables — the blobs stay valid and the
/// next warm re-shreds rows from the already-decoded analyses for free.
const REF_ROWS_VERSION: &str = "5";

pub fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS modules (
            module_name      TEXT PRIMARY KEY,
            path             TEXT NOT NULL,
            mtime_secs       INTEGER NOT NULL,
            file_size        INTEGER NOT NULL,
            source           TEXT NOT NULL DEFAULT 'import',
            analysis         BLOB,
            extract_version  INTEGER NOT NULL DEFAULT 0,
            deps_stamp       INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS builtins (
            name TEXT PRIMARY KEY,
            doc  TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS files (
            file_id INTEGER PRIMARY KEY,
            path    TEXT NOT NULL UNIQUE,
            source  TEXT NOT NULL DEFAULT 'import'
        );
        CREATE TABLE IF NOT EXISTS strings (
            str_id INTEGER PRIMARY KEY,
            s      TEXT NOT NULL UNIQUE
        );
        CREATE TABLE IF NOT EXISTS refs (
            file_id   INTEGER NOT NULL,
            name_id   INTEGER NOT NULL,
            kind      INTEGER NOT NULL,
            start_row INTEGER NOT NULL,
            start_col INTEGER NOT NULL,
            end_row   INTEGER NOT NULL,
            end_col   INTEGER NOT NULL,
            access    INTEGER NOT NULL,
            flags     INTEGER NOT NULL,
            qual_kind INTEGER NOT NULL,
            qual_id   INTEGER,
            arg_count INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_refs_name ON refs(name_id);
        CREATE INDEX IF NOT EXISTS idx_refs_file ON refs(file_id);
        CREATE TABLE IF NOT EXISTS syms (
            file_id      INTEGER NOT NULL,
            name_id      INTEGER NOT NULL,
            kind         INTEGER NOT NULL,
            start_row    INTEGER NOT NULL,
            start_col    INTEGER NOT NULL,
            end_row      INTEGER NOT NULL,
            end_col      INTEGER NOT NULL,
            container_id INTEGER,
            flags        INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_syms_name ON syms(name_id);
        CREATE INDEX IF NOT EXISTS idx_syms_file ON syms(file_id);
        CREATE TABLE IF NOT EXISTS stubs (
            path TEXT PRIMARY KEY,
            stub BLOB NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_modules_path ON modules(path);",
    )?;
    // Row-format generation for the derived tables (see REF_ROWS_VERSION).
    let rows_version: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'ref_rows_version'",
            [],
            |row| row.get(0),
        )
        .ok();
    // The stamp alone is trusted too far: a DB stamped current by a build
    // whose migration didn't actually reshape the tables would never
    // re-migrate, leaving every shred failing on a missing column while
    // composition quietly masks it (refs stay resident, retrieval dead,
    // diagnostics typeless). Probe the shape the current format requires so
    // a lying stamp still triggers the rebuild.
    let shape_ok = conn
        .prepare("SELECT source FROM files LIMIT 1")
        .map(|_| ())
        .and_then(|_| conn.prepare("SELECT qual_kind FROM refs LIMIT 1").map(|_| ()))
        .and_then(|_| conn.prepare("SELECT flags FROM syms LIMIT 1").map(|_| ()))
        .is_ok();
    if rows_version.as_deref() != Some(REF_ROWS_VERSION) || !shape_ok {
        // DROP, not DELETE: a format change may alter the table SHAPE, and
        // `CREATE TABLE IF NOT EXISTS` above no-ops on the old shape — a
        // row-only wipe would leave every future shred failing on a missing
        // column while composition quietly masks it (refs stay resident,
        // retrieval dead). Recreate from scratch.
        conn.execute_batch(
            "DROP TABLE IF EXISTS refs;
             DROP TABLE IF EXISTS syms;
             DROP TABLE IF EXISTS files;
             DROP TABLE IF EXISTS strings;
             CREATE TABLE files (
                file_id INTEGER PRIMARY KEY,
                path    TEXT NOT NULL UNIQUE,
                source  TEXT NOT NULL DEFAULT 'import'
             );
             CREATE TABLE strings (
                str_id INTEGER PRIMARY KEY,
                s      TEXT NOT NULL UNIQUE
             );
             CREATE TABLE refs (
                file_id   INTEGER NOT NULL,
                name_id   INTEGER NOT NULL,
                kind      INTEGER NOT NULL,
                start_row INTEGER NOT NULL,
                start_col INTEGER NOT NULL,
                end_row   INTEGER NOT NULL,
                end_col   INTEGER NOT NULL,
                access    INTEGER NOT NULL,
                flags     INTEGER NOT NULL,
                qual_kind INTEGER NOT NULL,
                qual_id   INTEGER,
                arg_count INTEGER
             );
             CREATE INDEX idx_refs_name ON refs(name_id);
             CREATE INDEX idx_refs_file ON refs(file_id);
             CREATE TABLE syms (
                file_id      INTEGER NOT NULL,
                name_id      INTEGER NOT NULL,
                kind         INTEGER NOT NULL,
                start_row    INTEGER NOT NULL,
                start_col    INTEGER NOT NULL,
                end_row      INTEGER NOT NULL,
                end_col      INTEGER NOT NULL,
                container_id INTEGER,
                flags        INTEGER NOT NULL
             );
             CREATE INDEX idx_syms_name ON syms(name_id);
             CREATE INDEX idx_syms_file ON syms(file_id);",
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('ref_rows_version', ?1)",
            params![REF_ROWS_VERSION],
        )?;
    }
    // Pre-existing tables (same schema version) predate `deps_stamp`; add it
    // in place rather than bumping SCHEMA_VERSION (a bump drops every row —
    // old rows carry 0, which validates only for empty-closure analyses, so
    // stale pack rows re-analyze while Perl caches survive the upgrade).
    let _ = conn.execute_batch(
        "ALTER TABLE modules ADD COLUMN deps_stamp INTEGER NOT NULL DEFAULT 0;",
    );
    // Stub generation gate — stamped here so every fresh DB is writable by
    // the persist writers (their per-chunk `stub_version_current` check
    // would otherwise fail-closed until the first warm scan stamped it).
    validate_stub_version(conn);

    let version: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .ok();

    match version.as_deref() {
        Some(SCHEMA_VERSION) => Ok(()),
        Some(_) => {
            conn.execute_batch("DROP TABLE IF EXISTS modules;")?;
            clear_derived_rows(conn)?;
            conn.execute_batch(
                "CREATE TABLE modules (
                    module_name      TEXT PRIMARY KEY,
                    path             TEXT NOT NULL,
                    mtime_secs       INTEGER NOT NULL,
                    file_size        INTEGER NOT NULL,
                    source           TEXT NOT NULL DEFAULT 'import',
                    analysis         BLOB,
                    extract_version  INTEGER NOT NULL DEFAULT 0,
                    deps_stamp       INTEGER NOT NULL DEFAULT 0
                );",
            )?;
            conn.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_version', ?1)",
                params![SCHEMA_VERSION],
            )?;
            Ok(())
        }
        None => {
            conn.execute(
                "INSERT INTO meta (key, value) VALUES ('schema_version', ?1)",
                params![SCHEMA_VERSION],
            )?;
            Ok(())
        }
    }
}

/// Wipe the derived relational tables (`refs`/`files`/`strings`). Runs
/// alongside every `DELETE FROM modules` hard-clear: the rows are shredded
/// from the blobs, so a generation that invalidates the blobs invalidates
/// the rows with it. Cheap to rebuild — the next warm re-shreds from the
/// decoded analyses it is loading anyway.
pub fn clear_derived_rows(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "DELETE FROM refs; DELETE FROM syms; DELETE FROM files; DELETE FROM strings; \
         DELETE FROM stubs;",
    )
}

pub fn compute_inc_hash(inc_paths: &[PathBuf]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    for p in inc_paths {
        p.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

pub fn validate_inc_paths(conn: &Connection, inc_paths: &[PathBuf]) -> rusqlite::Result<()> {
    let current_hash = compute_inc_hash(inc_paths);
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'inc_hash'",
            [],
            |row| row.get(0),
        )
        .ok();

    if stored.as_deref() != Some(&current_hash) {
        log::info!(
            "@INC changed (was {:?}, now {}), clearing module cache",
            stored,
            current_hash
        );
        // Import-tier only: workspace blobs bake plugin emissions and their
        // own source, not @INC paths — and the workspace indexer may have
        // written its rows BEFORE this validation runs (two writers, one
        // DB), so anything broader would delete rows mid-write and leave
        // already-evicted resident copies with no retrieval source.
        conn.execute("DELETE FROM modules WHERE source = 'import'", [])?;
        conn.execute(
            "DELETE FROM refs WHERE file_id IN (SELECT file_id FROM files WHERE source = 'import')",
            [],
        )?;
        conn.execute(
            "DELETE FROM syms WHERE file_id IN (SELECT file_id FROM files WHERE source = 'import')",
            [],
        )?;
        conn.execute("DELETE FROM files WHERE source = 'import'", [])?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('inc_hash', ?1)",
            params![current_hash],
        )?;
    }
    Ok(())
}

/// Hydrate the in-memory `builtins` mirror from SQLite, parsing
/// `perlfunc.pod` and writing rows on first use (or when the perl
/// version tag changes since the last run). Returns the populated
/// map. Keyed in `meta` under `builtins_perl_version`: mismatch wipes
/// the table and re-parses, same pattern as `validate_inc_paths` /
/// `validate_plugin_fingerprint`.
pub fn hydrate_builtins(conn: &Connection) -> rusqlite::Result<DashMap<String, String>> {
    let map: DashMap<String, String> = DashMap::new();

    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'builtins_perl_version'",
            [],
            |row| row.get(0),
        )
        .ok();

    let parsed = crate::builtins_pod::parse_perlfunc();

    let need_parse = match (&stored, &parsed) {
        (Some(s), Some(p)) => *s != p.perl_version,
        (None, Some(_)) => true,
        _ => false, // no parse + no cache rows we trust → leave map empty
    };

    if need_parse {
        if let Some(p) = parsed.as_ref() {
            conn.execute("DELETE FROM builtins", [])?;
            let tx = conn.unchecked_transaction()?;
            {
                let mut stmt = tx.prepare("INSERT INTO builtins (name, doc) VALUES (?1, ?2)")?;
                for (name, doc) in &p.entries {
                    stmt.execute(params![name, doc])?;
                }
            }
            tx.commit()?;
            conn.execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES ('builtins_perl_version', ?1)",
                params![p.perl_version],
            )?;
            log::info!("Indexed {} Perl builtins from {}", p.entries.len(), p.perl_version);
        }
    }

    // Read whatever's in the table now (either freshly written, or
    // the same rows from a prior run) into the in-memory mirror.
    let mut stmt = conn.prepare("SELECT name, doc FROM builtins")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for r in rows {
        if let Ok((name, doc)) = r {
            map.insert(name, doc);
        }
    }
    Ok(map)
}

/// Drop the module cache when the plugin set has changed since the last
/// run. `fingerprint` is the value returned by
/// `plugin::rhai_host::plugin_fingerprint()` — a hash over bundled
/// plugin sources plus every `.rhai` in `$PERL_LSP_PLUGIN_DIR`.
///
/// Without this check, a plugin author who edits a `.rhai`, restarts
/// the LSP, and inspects a cross-file query will see the *old*
/// plugin's emissions in the cached `FileAnalysis` blobs — making
/// plugin QA impossible. Mirrors `validate_inc_paths`: same meta-row
/// pattern, same hard-clear on mismatch.
pub fn validate_plugin_fingerprint(conn: &Connection, fingerprint: &str) -> rusqlite::Result<()> {
    // IMMEDIATE: check-and-stamp must be atomic against the other writer
    // (resolver thread vs workspace indexer) — two validators both reading
    // a missing stamp would both hard-clear, the second deleting rows the
    // first writer committed in between.
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = validate_plugin_fingerprint_inner(conn, fingerprint);
    match &result {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(_) => {
            let _ = conn.execute_batch("ROLLBACK");
        }
    }
    result
}

fn validate_plugin_fingerprint_inner(conn: &Connection, fingerprint: &str) -> rusqlite::Result<()> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'plugin_fingerprint'",
            [],
            |row| row.get(0),
        )
        .ok();

    if stored.as_deref() != Some(fingerprint) {
        log::info!(
            "Plugin set changed (was {:?}, now {}), clearing module cache",
            stored,
            fingerprint
        );
        conn.execute("DELETE FROM modules", [])?;
        clear_derived_rows(conn)?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('plugin_fingerprint', ?1)",
            params![fingerprint],
        )?;
    }
    Ok(())
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

/// Stamp over every file in an analysis' include closure — the ANALYSIS-INPUT
/// half of a pack row's validation key. A consumer `.c` row bakes its headers'
/// macro splices and type witnesses; its own (stamp, size) can't see a header
/// edit, so the closure stamp must (M2). Perl analyses have an empty closure
/// → 0, so the Perl path pays nothing. `stat_memo` dedups stats across a warm
/// run (closures overlap heavily — op.c and sv.c share ~90% of perl5's tree).
fn closure_stamp(
    closure: &crate::file_analysis::path_intern::ClosureList,
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

/// The register-from-Surface warm payload: everything bulk registration
/// derives from a WHOLE analysis, precomputed at persist time so warm
/// start never decodes the full blob. `skeleton` is the bag/refs/symbols-
/// stripped analysis — the exact struct the resident copy would be, so
/// all present-view routing and rehydration behave identically to a
/// full-decode-then-strip warm by construction.
pub struct WarmStub {
    pub feed: Vec<(String, bool)>,
    pub specs: Vec<(String, String)>,
    pub surface: crate::surface::Surface,
    pub skeleton: FileAnalysis,
}

/// Bump when the stub's MEANING changes without breaking its bincode
/// decode (a decode break self-heals to the full-blob path). Mismatch
/// wipes the `stubs` table; the next warm backfills from full decodes.
const STUB_VERSION: &str = "3";

/// Gate the `stubs` table on the current stub generation — call once
/// before a stub-consuming warm scan.
pub fn validate_stub_version(conn: &Connection) {
    let cur: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key = 'stub_version'", [], |r| r.get(0))
        .ok();
    if cur.as_deref() == Some(STUB_VERSION) {
        return;
    }
    let _ = conn.execute("DELETE FROM stubs", []);
    let _ = conn.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('stub_version', ?1)",
        params![STUB_VERSION],
    );
}

/// Encode by reference (`&[T]` and `Vec<T>` share a bincode encoding) so
/// the persist path never clones the feed/surface/skeleton it already holds.
pub fn encode_stub(
    feed: &[(String, bool)],
    specs: &[(String, String)],
    surface: &crate::surface::Surface,
    skeleton: &FileAnalysis,
) -> Option<Vec<u8>> {
    let bin = bincode::serialize(&(feed, specs, surface, skeleton)).ok()?;
    zstd::encode_all(bin.as_slice(), ZSTD_LEVEL).ok()
}

pub fn decode_stub(blob: &[u8]) -> Option<WarmStub> {
    let bin = zstd::decode_all(blob).ok()?;
    let (feed, specs, surface, mut skeleton): (
        Vec<(String, bool)>,
        Vec<(String, String)>,
        crate::surface::Surface,
        FileAnalysis,
    ) = bincode::deserialize(&bin).ok()?;
    skeleton.after_deserialize();
    // The eviction flags are `#[serde(skip)]` because a decoded BLOB is
    // whole; the stub's skeleton is the opposite — stripped on all three
    // axes by construction. Re-mark it, or its empty bag/refs/symbols
    // would read as "no facts" instead of "on disk".
    skeleton.evict_witness_bag();
    skeleton.evict_refs();
    skeleton.evict_symbols();
    Some(WarmStub { feed, specs, surface, skeleton })
}

/// The single speller of stub-row removal — every modules-row rewrite or
/// invalidation routes here so a schema change edits one string.
pub fn delete_stub(conn: &Connection, path: &str) {
    let _ = conn.execute("DELETE FROM stubs WHERE path = ?1", params![path]);
}

pub fn save_stub(conn: &Connection, path: &str, blob: &[u8]) {
    let r = conn.execute(
        "INSERT OR REPLACE INTO stubs (path, stub) VALUES (?1, ?2)",
        params![path, blob],
    );
    if let Err(e) = r {
        log::warn!("Failed to save warm stub for '{}': {}", path, e);
    }
}

/// Deferred-backfill guard: insert only while the modules row still carries
/// `stamp`. A concurrent `pack_file_changed` may rewrite the row (deleting
/// its stub) between the warm scan that encoded this stub and this deferred
/// write — re-inserting would revive the pre-edit generation under a
/// fresh-looking row.
pub fn save_stub_if_current(conn: &Connection, path: &str, blob: &[u8], stamp: (i64, i64)) {
    let _ = conn.execute(
        "INSERT OR REPLACE INTO stubs (path, stub)
         SELECT ?1, ?2 FROM modules
         WHERE path = ?1 AND mtime_secs = ?3 AND file_size = ?4",
        params![path, blob, stamp.0, stamp.1],
    );
}

/// Cheap per-chunk re-check for writers: a concurrent process running a
/// DIFFERENT stub generation may wipe + restamp the table mid-session;
/// writing this generation's stubs under the other generation's stamp
/// would serve mixed meanings to the next warm.
pub fn stub_version_current(conn: &Connection) -> bool {
    conn.query_row("SELECT value FROM meta WHERE key = 'stub_version'", [], |r| {
        r.get::<_, String>(0)
    })
    .map(|v| v == STUB_VERSION)
    .unwrap_or(false)
}

/// Serialize FileAnalysis via bincode then compress with zstd.
pub fn encode_analysis(fa: &FileAnalysis) -> Option<Vec<u8>> {
    let bin = bincode::serialize(fa).ok()?;
    zstd::encode_all(bin.as_slice(), ZSTD_LEVEL).ok()
}

/// Decompress + deserialize an analysis blob.
/// Public for the bulk writers' failure recovery: a failed chunk commit
/// un-strips its resident copies by decoding the blobs it still holds.
pub fn decode_analysis(blob: &[u8]) -> Option<FileAnalysis> {
    let bin = zstd::decode_all(blob).ok()?;
    let mut fa: FileAnalysis = bincode::deserialize(&bin).ok()?;
    fa.after_deserialize();
    Some(fa)
}

/// Keyed single-file decode — the Slice-2 rehydration primitive
/// (`docs/adr/memory-slice-2-lru.md`). Loads ONE file's persisted analysis
/// (full witness bag present) by path, without warming the whole table. The
/// resident pack-index copy has its bag evicted after indexing; a type query
/// that reaches into an evicted file rehydrates the exact bag through here.
/// No mtime/closure validation: the caller (`PackBagCache`) invalidates its
/// entry on file change, and the row's shape is EXTRACT_VERSION-pinned.
pub fn load_one(conn: &Connection, path: &str) -> Option<FileAnalysis> {
    load_one_diag(conn, path).ok()
}

/// `load_one` that discriminates the failure (see `RehydrateMiss`) instead
/// of collapsing to `None`, so the rehydration tripwire can name the cause.
pub fn load_one_diag(conn: &Connection, path: &str) -> Result<FileAnalysis, RehydrateMiss> {
    // A dual-homed project-lib file has TWO rows for one path (name-keyed
    // import + path-keyed workspace). Prefer a row whose stamp matches the
    // disk (one tier's persist may have failed or lagged, leaving a stale
    // generation); workspace-first is only the tiebreak. Single-row paths
    // deliberately skip stamp validation — the registered generation may
    // legitimately predate an unsaved edit, and the caller invalidates the
    // LRU on file change.
    let mut stmt = conn
        .prepare(
            "SELECT analysis, mtime_secs, file_size FROM modules WHERE path = ?1 \
             ORDER BY CASE source WHEN 'workspace' THEN 0 ELSE 1 END",
        )
        .map_err(|_| RehydrateMiss::NoRow)?;
    let rows: Vec<(Option<Vec<u8>>, i64, i64)> = stmt
        .query_map(params![path], |row| {
            Ok((
                row.get::<_, Option<Vec<u8>>>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|_| RehydrateMiss::NoRow)?
        .flatten()
        .collect();
    if rows.is_empty() {
        return Err(RehydrateMiss::NoRow);
    }
    let pick = |require_stamp: bool| -> Option<&Vec<u8>> {
        rows.iter().find_map(|(blob, m, sz)| {
            let blob = blob.as_ref().filter(|b| !b.is_empty())?;
            if require_stamp && file_stamp(std::path::Path::new(path)) != Some((*m, *sz)) {
                return None;
            }
            Some(blob)
        })
    };
    let blob = pick(rows.len() > 1)
        .or_else(|| pick(false))
        .ok_or(RehydrateMiss::EmptyBlob)?;
    decode_analysis(blob).ok_or(RehydrateMiss::DecodeFailed)
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
) -> Result<FileAnalysis, RehydrateMiss> {
    let dir = cache_dir_for_workspace(cache_key)
        .ok_or_else(|| RehydrateMiss::OpenerFailed("no cache dir for workspace".into()))?;
    load_with_wal_fallback(&db_path_for(&dir, lang), paths)
}

#[cfg(test)]
pub fn open_and_load_diag(
    _cache_key: Option<&str>,
    _lang: &str,
    _paths: &[String],
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
) -> Result<FileAnalysis, RehydrateMiss> {
    // `open_reader_retrying` waits out the transient CANTOPEN window; the
    // rw_open closure below then handles the (rarer) opened-but-row-invisible
    // case. Both are the WAL-checkpoint recovery.
    rehydrate_from_opens(
        open_reader_retrying(db_path),
        || open_rw_shared_at(db_path),
        paths,
    )
}

/// The fallback POLICY, split from the openers so the read-only-open-failure
/// branch is deterministically testable (the real `SQLITE_CANTOPEN` race
/// can't be forced from static file state). `ro` is the read-only open
/// result (`Err` = the open itself failed — the captured CANTOPEN cause);
/// `rw_open` lazily opens the read-write recovery connection.
fn rehydrate_from_opens(
    ro: Result<Connection, String>,
    rw_open: impl FnOnce() -> Option<Connection>,
    paths: &[String],
) -> Result<FileAnalysis, RehydrateMiss> {
    let ro_err = ro.as_ref().err().cloned();
    let mut last = RehydrateMiss::NoRow;
    if let Ok(conn) = &ro {
        for p in paths {
            match load_one_diag(conn, p) {
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
                if let Ok(fa) = load_one_diag(&rw, p) {
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
    include_closure: &crate::file_analysis::path_intern::ClosureList,
    blob: &[u8],
    source: &str,
    stamp: (i64, i64),
) {
    let (mtime, size) = stamp;
    let deps = closure_stamp(include_closure, &mut std::collections::HashMap::new());
    let r = conn.execute(
        "INSERT OR REPLACE INTO modules (module_name, path, mtime_secs, file_size, source, analysis, extract_version, deps_stamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            module_name,
            path.to_string_lossy(),
            mtime,
            size,
            source,
            Some(blob),
            EXTRACT_VERSION,
            deps
        ],
    );
    if let Err(e) = r {
        log::warn!("Failed to save module blob for '{}': {}", module_name, e);
    }
    // A rewritten modules row orphans any prior stub for the path — a stale
    // skeleton paired with a fresh stamp would be served as valid on the
    // next warm. Writers that have a fresh stub re-insert it right after.
    delete_stub(conn, &path.to_string_lossy());
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
    include_closure: &crate::file_analysis::path_intern::ClosureList,
    stat_memo: &mut std::collections::HashMap<String, (i64, i64)>,
) {
    let deps = closure_stamp(include_closure, stat_memo);
    let _ = conn.execute(
        "UPDATE modules SET deps_stamp = ?1 WHERE path = ?2",
        params![deps, path],
    );
}

/// Replace one file's derived rows — refs AND symbols — in the relational
/// index. One function so both families are the same generation by
/// construction (`files` presence is the single "already shredded" marker;
/// a marker per family would let them drift). Runs inside the caller's
/// transaction when one is open (bulk drains wrap N files per `BEGIN`);
/// standalone callers get per-statement autocommit, which is fine for
/// single-file updates. Upserts the `files` row even for an empty file.
pub fn shred_derived_rows(
    conn: &Connection,
    path: &str,
    source: &str,
    seeds: &[crate::file_analysis::RefRowSeed],
    sym_seeds: &[crate::file_analysis::SymRowSeed],
) -> rusqlite::Result<()> {
    // Sticky workspace tier: project lib/ files are inside the walk AND on
    // @INC (add_project_lib_paths), so the resolver re-shreds them as
    // 'import'. The walk's verdict wins — downgrading would let the @INC
    // hard-clear take an editable file's generation out from under its
    // stripped resident copy.
    conn.execute(
        "INSERT INTO files (path, source) VALUES (?1, ?2)
         ON CONFLICT(path) DO UPDATE SET source =
           CASE WHEN files.source = 'workspace' THEN 'workspace' ELSE excluded.source END",
        params![path, source],
    )?;
    let file_id: i64 = conn.query_row(
        "SELECT file_id FROM files WHERE path = ?1",
        params![path],
        |row| row.get(0),
    )?;
    conn.execute("DELETE FROM refs WHERE file_id = ?1", params![file_id])?;
    conn.execute("DELETE FROM syms WHERE file_id = ?1", params![file_id])?;
    let mut intern = conn.prepare_cached("INSERT OR IGNORE INTO strings (s) VALUES (?1)")?;
    let mut lookup = conn.prepare_cached("SELECT str_id FROM strings WHERE s = ?1")?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO refs (file_id, name_id, kind, start_row, start_col, end_row, end_col,
                           access, flags, qual_kind, qual_id, arg_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )?;
    // Per-call interning memo: files repeat the same handful of names heavily.
    let mut memo: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
    let mut intern_str = |s: &str,
                          memo: &mut std::collections::HashMap<String, i64>|
     -> rusqlite::Result<i64> {
        if let Some(id) = memo.get(s) {
            return Ok(*id);
        }
        intern.execute(params![s])?;
        let id: i64 = lookup.query_row(params![s], |row| row.get(0))?;
        memo.insert(s.to_string(), id);
        Ok(id)
    };
    for seed in seeds {
        let name_id = intern_str(&seed.key, &mut memo)?;
        let qual_id = match seed.qual.as_deref() {
            Some(q) => Some(intern_str(q, &mut memo)?),
            None => None,
        };
        insert.execute(params![
            file_id,
            name_id,
            seed.kind,
            seed.span.start.row as i64,
            seed.span.start.column as i64,
            seed.span.end.row as i64,
            seed.span.end.column as i64,
            seed.access,
            seed.flags,
            seed.qual_kind,
            qual_id,
            seed.arg_count,
        ])?;
    }
    let mut insert_sym = conn.prepare_cached(
        "INSERT INTO syms (file_id, name_id, kind, start_row, start_col, end_row, end_col,
                           container_id, flags)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?;
    for seed in sym_seeds {
        let name_id = intern_str(&seed.name, &mut memo)?;
        let container_id = match seed.container.as_deref() {
            Some(c) => Some(intern_str(c, &mut memo)?),
            None => None,
        };
        insert_sym.execute(params![
            file_id,
            name_id,
            seed.kind,
            seed.span.start.row as i64,
            seed.span.start.column as i64,
            seed.span.end.row as i64,
            seed.span.end.column as i64,
            container_id,
            seed.flags,
        ])?;
    }
    Ok(())
}

/// Drop one file's whole persisted generation — blob row AND derived ref
/// rows, together (the eraser twin of the write invariant "blob + rows
/// describe one generation"). Every invalidation seam calls this; nobody
/// else spells modules-table SQL.
pub fn invalidate_generation(conn: &Connection, path: &str) {
    let _ = conn.execute("DELETE FROM modules WHERE path = ?1", params![path]);
    delete_stub(conn, path);
    delete_ref_rows(conn, path);
}

/// Tier-scoped eraser: drops the generation ONLY when its rows carry
/// `source` — the walk's dead-row GC must not take a dual-homed file's
/// import-tier generation (project-lib files leave the walk when
/// gitignored but stay valid @INC modules).
pub fn invalidate_generation_tier(conn: &Connection, path: &str, source: &str) {
    let _ = conn.execute(
        "DELETE FROM modules WHERE path = ?1 AND source = ?2",
        params![path, source],
    );
    // Stubs are workspace-tier only — an import-tier invalidation must not
    // orphan a dual-homed file's still-valid workspace stub.
    if source == "workspace" {
        delete_stub(conn, path);
    }
    let _ = conn.execute(
        "DELETE FROM refs WHERE file_id IN
           (SELECT file_id FROM files WHERE path = ?1 AND source = ?2)",
        params![path, source],
    );
    let _ = conn.execute(
        "DELETE FROM syms WHERE file_id IN
           (SELECT file_id FROM files WHERE path = ?1 AND source = ?2)",
        params![path, source],
    );
    let _ = conn.execute(
        "DELETE FROM files WHERE path = ?1 AND source = ?2",
        params![path, source],
    );
}

/// Remove a deleted file's rows (the removal half of `shred_derived_rows`).
pub fn delete_ref_rows(conn: &Connection, path: &str) {
    let _ = conn.execute(
        "DELETE FROM refs WHERE file_id IN (SELECT file_id FROM files WHERE path = ?1)",
        params![path],
    );
    let _ = conn.execute(
        "DELETE FROM syms WHERE file_id IN (SELECT file_id FROM files WHERE path = ?1)",
        params![path],
    );
    let _ = conn.execute("DELETE FROM files WHERE path = ?1", params![path]);
}

/// Has `path` been shredded into the relational index? (`files` presence is
/// the marker — `shred_derived_rows` upserts it even for empty files.)
#[cfg(test)]
pub fn has_ref_rows(conn: &Connection, path: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM files WHERE path = ?1",
        params![path],
        |_| Ok(()),
    )
    .is_ok()
}

/// The retrieval half: every indexed file containing at least one ref row
/// whose match key is one of `keys` — the candidate-file set `refs_to`'s
/// SQL arms rehydrate and run the (unchanged) matcher over.
pub fn ref_candidate_files(conn: &Connection, keys: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    // Usage candidacy comes from ref rows; DECLARATION candidacy from sym
    // rows — a file that declares `helper` but never mentions it again has
    // no matching ref row, and without the union the backward walk's
    // matcher (whose declaration half reads symbols) never rehydrates it.
    let Ok(mut stmt) = conn.prepare_cached(
        "SELECT DISTINCT f.path FROM refs r
           JOIN files f ON f.file_id = r.file_id
          WHERE r.name_id = (SELECT str_id FROM strings WHERE s = ?1)
         UNION
         SELECT DISTINCT f.path FROM syms y
           JOIN files f ON f.file_id = y.file_id
          WHERE y.name_id = (SELECT str_id FROM strings WHERE s = ?1)",
    ) else {
        return out;
    };
    let mut seen = std::collections::HashSet::new();
    for key in keys {
        let rows = stmt.query_map(params![key], |row| row.get::<_, String>(0));
        if let Ok(rows) = rows {
            for r in rows {
                match r {
                    Ok(p) => {
                        if seen.insert(p.clone()) {
                            out.push(p);
                        }
                    }
                    // A step-level error (corrupt page, IO) ends the scan —
                    // the candidate list is TRUNCATED, which reads as
                    // "fewer references" with no other witness. Say so.
                    Err(e) => {
                        log::warn!("ref candidate scan aborted mid-iteration: {}", e);
                        break;
                    }
                }
            }
        }
    }
    out
}

/// One workspace/symbol row hit: (path, name, kind code, selection span,
/// container, flags). The caller applies the adapter's kind/flag filters
/// and skips paths a fresher resident copy already answered.
pub struct SymRowHit {
    pub path: String,
    pub name: String,
    pub kind: u8,
    pub start_row: usize,
    pub start_col: usize,
    pub end_row: usize,
    pub end_col: usize,
    pub container: Option<String>,
    pub flags: u8,
}

/// The rows-backed workspace/symbol scan: every WORKSPACE-tier symbol row
/// whose name contains `query`, case-insensitively — the same containment
/// test the resident sweep applies. The `files.source` filter keeps the
/// import tier (@INC deps) out: the resident sweeps never enumerated it,
/// and folding it in would flood project searches with CPAN internals.
pub fn sym_rows_matching(conn: &Connection, query: &str) -> Vec<SymRowHit> {
    let mut out = Vec::new();
    // SQLite LIKE is case-insensitive for ASCII only; the resident sweep
    // lowercases with full Unicode semantics. ASCII queries (the hot path)
    // stay an indexed-ish LIKE; a non-ASCII query walks the name strings
    // with the SAME Rust containment test so an evicted file's `sub Übung`
    // matches exactly like a resident one.
    if !query.is_ascii() {
        let Ok(mut stmt) = conn.prepare_cached(
            "SELECT f.path, n.s, y.kind, y.start_row, y.start_col, y.end_row, y.end_col,
                    c.s, y.flags
               FROM syms y
               JOIN files f ON f.file_id = y.file_id
               JOIN strings n ON n.str_id = y.name_id
               LEFT JOIN strings c ON c.str_id = y.container_id
              WHERE f.source = 'workspace'",
        ) else {
            return out;
        };
        let q = query.to_lowercase();
        let rows = stmt.query_map([], |row| {
            Ok(SymRowHit {
                path: row.get(0)?,
                name: row.get(1)?,
                kind: row.get::<_, i64>(2)? as u8,
                start_row: row.get::<_, i64>(3)? as usize,
                start_col: row.get::<_, i64>(4)? as usize,
                end_row: row.get::<_, i64>(5)? as usize,
                end_col: row.get::<_, i64>(6)? as usize,
                container: row.get(7)?,
                flags: row.get::<_, i64>(8)? as u8,
            })
        });
        if let Ok(rows) = rows {
            for r in rows {
                match r {
                    Ok(hit) => {
                        if hit.name.to_lowercase().contains(&q) {
                            out.push(hit);
                        }
                    }
                    Err(e) => {
                        log::warn!("sym row scan aborted mid-iteration: {}", e);
                        break;
                    }
                }
            }
        }
        return out;
    }
    let Ok(mut stmt) = conn.prepare_cached(
        "SELECT f.path, n.s, y.kind, y.start_row, y.start_col, y.end_row, y.end_col,
                c.s, y.flags
           FROM syms y
           JOIN files f ON f.file_id = y.file_id
           JOIN strings n ON n.str_id = y.name_id
           LEFT JOIN strings c ON c.str_id = y.container_id
          WHERE f.source = 'workspace'
            AND n.s LIKE '%' || ?1 || '%' ESCAPE '\\'",
    ) else {
        return out;
    };
    // LIKE wildcards in the user's query are literals, not patterns.
    let escaped = query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    let rows = stmt.query_map(params![escaped], |row| {
        Ok(SymRowHit {
            path: row.get(0)?,
            name: row.get(1)?,
            kind: row.get::<_, i64>(2)? as u8,
            start_row: row.get::<_, i64>(3)? as usize,
            start_col: row.get::<_, i64>(4)? as usize,
            end_row: row.get::<_, i64>(5)? as usize,
            end_col: row.get::<_, i64>(6)? as usize,
            container: row.get(7)?,
            flags: row.get::<_, i64>(8)? as u8,
        })
    });
    if let Ok(rows) = rows {
        for r in rows {
            match r {
                Ok(hit) => out.push(hit),
                Err(e) => {
                    log::warn!("sym row scan aborted mid-iteration: {}", e);
                    break;
                }
            }
        }
    }
    out
}

/// Row count for one match key — the count-first surface for hot-name
/// capping (`docs/adr/relational-ref-index.md`).
#[cfg(test)]
pub fn ref_count_named(conn: &Connection, key: &str) -> u64 {
    conn.query_row(
        "SELECT COUNT(*) FROM refs
          WHERE name_id = (SELECT str_id FROM strings WHERE s = ?1)",
        params![key],
        |row| row.get::<_, i64>(0),
    )
    .map(|n| n as u64)
    .unwrap_or(0)
}

/// Streaming warm: identical row validation to `warm_cache`, but each valid
/// positive entry is handed to `each` one at a time and nothing is retained
/// here — the caller registers a stripped resident copy and drops the full
/// decode before the next row. This bounds the warm-path transient to ONE
/// file's full analysis instead of the whole table's (the 884 MB abseil
/// warm peak vs its 276 MB cold peak). Negative sentinels are skipped —
/// the pack warm path has no consumer for them. Returns
/// (valid_rows_seen, stale_names) like `warm_cache`.
pub fn warm_cache_streaming(
    conn: &Connection,
    source: &str,
    each: &mut dyn FnMut(String, PathBuf, FileAnalysis),
) -> (usize, Vec<String>) {
    let mut stmt = match conn.prepare(
        "SELECT module_name, path, mtime_secs, file_size, analysis, extract_version, deps_stamp \
         FROM modules WHERE source = ?1",
    ) {
        Ok(s) => s,
        Err(_) => return (0, Vec::new()),
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
        Err(_) => return (0, Vec::new()),
    };

    let mut count = 0usize;
    let mut stale_names = Vec::new();
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
            RowGeneration::Sentinel | RowGeneration::StampStale => continue,
        };
        let Some(blob) = analysis_blob.filter(|b| !b.is_empty()) else {
            continue;
        };
        let Some(fa) = decode_analysis(&blob) else {
            log::warn!("Failed to decode cached analysis for '{}', skipping", module_name);
            continue;
        };
        if closure_stamp(&fa.include_closure, &mut stat_memo) != row_deps_stamp {
            continue;
        }
        count += 1;
        each(module_name, path, fa);
    }
    (count, stale_names)
}

/// One persisted row's generation verdict — the shared first half of every
/// warm scan's validity check (the second half, the closure stamp, runs
/// after decode on whichever struct is in hand). A new validity axis goes
/// HERE, not into one loop.
pub(crate) enum RowGeneration {
    /// Sentinel/negative row — no warm consumer.
    Sentinel,
    /// The file changed or vanished on disk — skip silently.
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
        _ => return RowGeneration::StampStale,
    }
    if row_extract_version < EXTRACT_VERSION {
        return RowGeneration::VersionStale;
    }
    RowGeneration::Current(path)
}

/// Deferred-write chunking: N items per `BEGIN IMMEDIATE`…`COMMIT`, the
/// SQLITE_BUSY_SNAPSHOT-safe shape every post-scan backfill shares (writing
/// inside a streaming SELECT's snapshot turns a concurrent commit into an
/// unretried BUSY_SNAPSHOT abort). A failed txn OPEN abandons the remaining
/// queue (the writer is likely gone); a failed COMMIT rolls back and keeps
/// going — later chunks may land.
pub fn write_in_chunks<T>(
    conn: &Connection,
    items: &[T],
    chunk_size: usize,
    label: &str,
    per_item: impl Fn(&Connection, &T),
) {
    for chunk in items.chunks(chunk_size) {
        if conn.execute_batch("BEGIN IMMEDIATE").is_err() {
            log::error!("{label}: txn open failed; remaining items defer to next warm");
            break;
        }
        for item in chunk {
            per_item(conn, item);
        }
        if let Err(e) = conn.execute_batch("COMMIT") {
            log::error!("{label}: commit failed: {}", e);
            let _ = conn.execute_batch("ROLLBACK");
        }
    }
}

/// Every path that currently has shredded derived rows — the bulk twin of
/// `has_ref_rows` for warm scans (one query instead of one per file).
pub fn paths_with_ref_rows(conn: &Connection) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Ok(mut stmt) = conn.prepare("SELECT path FROM files") else {
        return out;
    };
    if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
        for p in rows.flatten() {
            out.insert(p);
        }
    }
    out
}

/// Every DISTINCT name key present in the `refs` table. The general
/// pre-prune set for `--heatmap`'s per-declaration references projection: a
/// declaration whose name key is ABSENT here has no reference row in any
/// indexed file, so — because rows over-approximate references — the
/// projection is provably empty and the walk can be skipped. Retrieval only;
/// the caller owns the coverage decision (trust "absent ⇒ zero references"
/// only when the store actually covers the files the walk would scan).
pub fn names_with_ref_rows(conn: &Connection) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Ok(mut stmt) =
        conn.prepare("SELECT DISTINCT s.s FROM refs r JOIN strings s ON s.str_id = r.name_id")
    else {
        return out;
    };
    if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
        for n in rows.flatten() {
            out.insert(n);
        }
    }
    out
}

/// One unused-exported symbol row: an `@EXPORT`/`@EXPORT_OK` name with no
/// reference row in any OTHER file. Carries the identity the caller matches a
/// reported symbol against — path + name + the selection-span start.
pub struct DeadExportRow {
    pub path: String,
    pub name: String,
    pub start_row: usize,
    pub start_col: usize,
}

/// The unused-exports view (`docs/adr/relational-ref-index.md`): every
/// WORKSPACE-tier symbol row flagged exported (`SymRowSeed::FLAG_EXPORTED`)
/// whose name key has ZERO ref rows in any OTHER file. Same-file refs are
/// excluded on purpose — a module calling its own exported sub does not make
/// that export live for a *consumer*.
///
/// The result is SOUND IN EXACTLY ONE DIRECTION, and the asymmetry is the
/// point. Ref rows are name-match CANDIDATES — an over-approximation of real
/// references, since the per-`RefKind` matcher still runs per row — so:
///   * zero cross-file candidate rows ⇒ no cross-file reference can exist ⇒
///     the export is TRULY unused by any consumer (a sound "dead export").
///   * one or more candidate rows ⇒ UNKNOWN: a candidate may or may not
///     survive the matcher. Never read this as "used".
/// The right polarity for a dead-export review queue: it never fabricates a
/// dead export; at worst it MISSES one whose sole consumer's candidate row
/// would not have survived the matcher.
pub fn unused_exported_syms(conn: &Connection) -> Vec<DeadExportRow> {
    let mut out = Vec::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT f.path, n.s, y.start_row, y.start_col
           FROM syms y
           JOIN files f ON f.file_id = y.file_id
           JOIN strings n ON n.str_id = y.name_id
          WHERE f.source = 'workspace'
            AND (y.flags & ?1) != 0
            AND NOT EXISTS (
                  SELECT 1 FROM refs r
                   WHERE r.name_id = y.name_id
                     AND r.file_id != y.file_id
                )",
    ) else {
        return out;
    };
    let flag = crate::file_analysis::SymRowSeed::FLAG_EXPORTED as i64;
    let rows = stmt.query_map(params![flag], |row| {
        Ok(DeadExportRow {
            path: row.get(0)?,
            name: row.get(1)?,
            start_row: row.get::<_, i64>(2)? as usize,
            start_col: row.get::<_, i64>(3)? as usize,
        })
    });
    if let Ok(rows) = rows {
        for r in rows {
            match r {
                Ok(hit) => out.push(hit),
                Err(e) => {
                    log::warn!("unused-exports scan aborted mid-iteration: {}", e);
                    break;
                }
            }
        }
    }
    out
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
                if closure_stamp(&stub.skeleton.include_closure, &mut stat_memo)
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
        if closure_stamp(&fa.include_closure, &mut stat_memo) != row_deps_stamp {
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

pub fn warm_cache(
    conn: &Connection,
    cache: &DashMap<String, Option<Arc<CachedModule>>>,
    strip: bool,
) -> (usize, Vec<String>) {
    // Name-keyed warm serves the @INC tier only; 'workspace' rows are
    // path-keyed and stream through `warm_cache_streaming` — loading them
    // here would pollute the module cache with path-string keys.
    let mut stmt = match conn.prepare(
        "SELECT module_name, path, mtime_secs, file_size, analysis, extract_version, deps_stamp FROM modules WHERE source = 'import'",
    ) {
        Ok(s) => s,
        Err(_) => return (0, Vec::new()),
    };

    let rows = match stmt.query_map([], |row| {
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
                match decode_analysis(&blob) {
                    Some(mut fa) => {
                        // A pack file's analysis bakes its headers (splices,
                        // witnesses, closure): the row is valid only while the
                        // whole closure is unchanged, not just the file itself.
                        if closure_stamp(&fa.include_closure, &mut stat_memo) != row_deps_stamp {
                            continue;
                        }
                        // Strip AT INSERT (long-lived processes): the blob
                        // just decoded IS the recoverable generation, and
                        // stripping here (not a post-hoc sweep) can never
                        // touch a copy some OTHER path registered
                        // whole-for-a-reason (writer fallback, watcher).
                        if strip && !fa.degraded {
                            fa.evict_axes(true, false);
                        }
                        cache.insert(
                            module_name,
                            Some(Arc::new(CachedModule::new(path, Arc::new(fa)))),
                        );
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
                &cached.analysis.include_closure,
                &mut std::collections::HashMap::new(),
            );
            (cached.path.to_string_lossy().to_string(), mtime, size, blob, deps)
        }
        None => (String::new(), 0i64, 0i64, None, 0i64),
    };

    let r = conn.execute(
        "INSERT OR REPLACE INTO modules (module_name, path, mtime_secs, file_size, source, analysis, extract_version, deps_stamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![module_name, path_str, mtime, size, source, analysis_blob, EXTRACT_VERSION, deps_stamp],
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
        // Same stale-stub guard as `save_blob_to_db_stamped`.
        delete_stub(conn, &path_str);
    }
    ok
}

/// Drop the modules table when the driver's external analysis inputs (the
/// C++ toolchain: system include roots, predefined macros — or its probe
/// FAILURE) changed since the rows were written. Same meta-row pattern as
/// `validate_inc_paths`: a generation built under degraded/different inputs
/// must not be served under the current ones (H8).
pub fn validate_input_fingerprint(conn: &Connection, fingerprint: u64) -> rusqlite::Result<()> {
    // Same atomic check-and-stamp rationale as `validate_plugin_fingerprint`.
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = validate_input_fingerprint_inner(conn, fingerprint);
    match &result {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(_) => {
            let _ = conn.execute_batch("ROLLBACK");
        }
    }
    result
}

fn validate_input_fingerprint_inner(conn: &Connection, fingerprint: u64) -> rusqlite::Result<()> {
    let fingerprint = format!("{:016x}", fingerprint);
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'input_fingerprint'",
            [],
            |row| row.get(0),
        )
        .ok();

    if stored.as_deref() != Some(&fingerprint) {
        log::info!(
            "Analysis inputs changed (was {:?}, now {}), clearing module cache",
            stored,
            fingerprint
        );
        conn.execute("DELETE FROM modules", [])?;
        clear_derived_rows(conn)?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('input_fingerprint', ?1)",
            params![fingerprint],
        )?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "module_cache_tests.rs"]
mod tests;
