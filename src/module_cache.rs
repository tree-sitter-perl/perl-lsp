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
pub const EXTRACT_VERSION: i64 = 131;

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

#[cfg(not(test))]
pub fn open_cache_db(workspace_root: Option<&str>, lang: &str) -> Option<Connection> {
    let dir = cache_dir_for_workspace(workspace_root)?;
    std::fs::create_dir_all(&dir).ok()?;
    // Per-language DB — Perl keeps `modules.db` (back-compat), every pack
    // language gets its own `modules-{lang}.db` so names never comingle on
    // disk (a Perl `Box` and a C++ class `Box` live in different files).
    let db_path = if lang == "perl" {
        dir.join("modules.db")
    } else {
        dir.join(format!("modules-{lang}.db"))
    };
    log::info!("Module cache: {:?}", db_path);

    match Connection::open(&db_path) {
        Ok(conn) => {
            let _ = conn.execute_batch("PRAGMA journal_mode=WAL;");
            match init_schema(&conn) {
                Ok(()) => Some(conn),
                Err(e) => {
                    log::warn!("Cache DB schema init failed: {}. Recreating.", e);
                    drop(conn);
                    let _ = std::fs::remove_file(&db_path);
                    let conn = Connection::open(&db_path).ok()?;
                    let _ = conn.execute_batch("PRAGMA journal_mode=WAL;");
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
        );",
    )?;
    // Pre-existing tables (same schema version) predate `deps_stamp`; add it
    // in place rather than bumping SCHEMA_VERSION (a bump drops every row —
    // old rows carry 0, which validates only for empty-closure analyses, so
    // stale pack rows re-analyze while Perl caches survive the upgrade).
    let _ = conn.execute_batch(
        "ALTER TABLE modules ADD COLUMN deps_stamp INTEGER NOT NULL DEFAULT 0;",
    );

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
        conn.execute("DELETE FROM modules", [])?;
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
fn file_stamp(path: &std::path::Path) -> Option<(i64, i64)> {
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
    closure: &[String],
    stat_memo: &mut std::collections::HashMap<String, (i64, i64)>,
) -> i64 {
    use std::hash::{Hash, Hasher};
    if closure.is_empty() {
        return 0;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for p in closure {
        let stamp = *stat_memo
            .entry(p.clone())
            .or_insert_with(|| file_stamp(std::path::Path::new(p)).unwrap_or((0, -1)));
        p.hash(&mut h);
        stamp.hash(&mut h);
    }
    h.finish() as i64
}

/// Serialize FileAnalysis via bincode then compress with zstd.
fn encode_analysis(fa: &FileAnalysis) -> Option<Vec<u8>> {
    let bin = bincode::serialize(fa).ok()?;
    zstd::encode_all(bin.as_slice(), ZSTD_LEVEL).ok()
}

/// Decompress + deserialize an analysis blob.
fn decode_analysis(blob: &[u8]) -> Option<FileAnalysis> {
    let bin = zstd::decode_all(blob).ok()?;
    let mut fa: FileAnalysis = bincode::deserialize(&bin).ok()?;
    fa.after_deserialize();
    Some(fa)
}

pub fn warm_cache(
    conn: &Connection,
    cache: &DashMap<String, Option<Arc<CachedModule>>>,
) -> (usize, Vec<String>) {
    let mut stmt = match conn.prepare(
        "SELECT module_name, path, mtime_secs, file_size, analysis, extract_version, deps_stamp FROM modules",
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
    for row in rows.flatten() {
        let (module_name, path_str, cached_mtime, cached_size, analysis_blob, row_extract_version, row_deps_stamp) = row;

        // Negative sentinel: empty path + NULL blob.
        if path_str.is_empty() {
            cache.insert(module_name, None);
            count += 1;
            continue;
        }

        let path = PathBuf::from(&path_str);

        // Validate mtime — skip entries where the file changed on disk.
        if let Some((disk_mtime, disk_size)) = file_stamp(&path) {
            if disk_mtime != cached_mtime || disk_size != cached_size {
                continue;
            }
        } else {
            continue; // file deleted
        }

        // Check extract version — stale entries are still loaded but queued for re-resolve.
        if row_extract_version < EXTRACT_VERSION {
            stale_names.push(module_name.clone());
        }

        match analysis_blob {
            Some(blob) if !blob.is_empty() => {
                match decode_analysis(&blob) {
                    Some(fa) => {
                        // A pack file's analysis bakes its headers (splices,
                        // witnesses, closure): the row is valid only while the
                        // whole closure is unchanged, not just the file itself.
                        if closure_stamp(&fa.include_closure, &mut stat_memo) != row_deps_stamp {
                            continue;
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
                // Blob missing / empty — treat as negative sentinel.
                cache.insert(module_name, None);
                count += 1;
            }
        }
    }

    (count, stale_names)
}

pub fn save_to_db(
    conn: &Connection,
    module_name: &str,
    result: &Option<Arc<CachedModule>>,
    source: &str,
) {
    let (path_str, mtime, size, analysis_blob, deps_stamp) = match result {
        Some(cached) => {
            // Degraded analyses (parse/extract failure, skipped gather) must
            // not be persisted: the row would validate on the source file's
            // stamp alone and re-serve the degraded blob every session (H8).
            if cached.analysis.degraded {
                return;
            }
            let (mtime, size) = file_stamp(&cached.path).unwrap_or((0, 0));
            let blob = encode_analysis(&cached.analysis);
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
    if let Err(e) = r {
        log::warn!("Failed to save module cache for '{}': {}", module_name, e);
    }
}

/// Drop the modules table when the driver's external analysis inputs (the
/// C++ toolchain: system include roots, predefined macros — or its probe
/// FAILURE) changed since the rows were written. Same meta-row pattern as
/// `validate_inc_paths`: a generation built under degraded/different inputs
/// must not be served under the current ones (H8).
pub fn validate_input_fingerprint(conn: &Connection, fingerprint: u64) -> rusqlite::Result<()> {
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
