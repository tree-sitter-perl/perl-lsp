//! Schema and generation gates: table DDL, the version stamps
//! (schema / extract / ref-rows), and the meta-keyed hard-clear
//! validators (@INC, plugin set, analysis inputs) plus builtins
//! hydration (same meta-row pattern).

use super::*;

const SCHEMA_VERSION: &str = "10";

/// Bumped when the builder's analysis output changes shape in a way that
/// invalidates cached blobs. Unlike `SCHEMA_VERSION`, this does not drop
/// the table — stale entries are re-resolved lazily with priority.
pub const EXTRACT_VERSION: i64 = 193;

/// Bumped when the ROW format of the relational ref index changes shape.
/// Unlike `EXTRACT_VERSION` (which governs the blobs), a mismatch only wipes
/// the derived `refs`/`files`/`strings` tables — the blobs stay valid and the
/// next warm re-shreds rows from the already-decoded analyses for free.
pub(super) const REF_ROWS_VERSION: &str = "6";

/// Row format of the `conclusions` lane. Bump on any change to the row's
/// SHAPE or to what its stamp means.
///
/// Separate from `REF_ROWS_VERSION` because the lanes fail differently: a
/// stale ref row answers a retrieval wrongly, a stale conclusion row answers
/// a TYPE wrongly. Sharing one version would make either change wipe both.
pub(super) const CONCLUSION_ROWS_VERSION: &str = "1";

/// Fingerprint over everything that can change what a derivation CONCLUDES,
/// computed by `build.rs` at compile time.
///
/// Deliberately not a hand-maintained integer like its neighbours above. Those
/// guard SHAPE: a stale one is caught the moment a decode fails loudly. This
/// guards MEANING — a reducer edit leaves bytes that decode perfectly and
/// answer wrongly — and there is nothing downstream to notice. A version
/// someone has to remember to bump is the wrong instrument for a failure
/// nothing else can see (`docs/adr/conclusion-layer.md`).
const CONCLUSION_SOURCE_FINGERPRINT: &str = env!("PERL_LSP_CONCLUSION_FINGERPRINT");

/// The source fingerprint, plus the ENV that steers what a bake produces.
///
/// Source alone is not the derivation. `PERL_LSP_MINT_LINKS` and
/// `PERL_LSP_NO_BAKE` change what gets STORED, and their output outlives the
/// process that wrote it — so one measurement run under a flag leaves every
/// later run reading maps it would never have baked, with nothing to notice.
/// Caught the honest way: a `--check` under `MINT_LINKS=1` took a gold row from
/// PASS to FAIL for every subsequent run until the cache was wiped by hand, and
/// the failure looked exactly like a code regression.
///
/// Consult-side flags (`PERL_LSP_CONCL_EQUIV`, `PERL_LSP_NO_TRUST_ABSENT`)
/// deliberately stay OUT: they change how a stored map is READ, not what it
/// contains, so folding them in would discard a good cache on every
/// measurement.
pub fn conclusion_fingerprint() -> &'static str {
    static FP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    FP.get_or_init(|| {
        let set: Vec<bool> = BAKE_STEERING_FLAGS
            .iter()
            .map(|f| std::env::var(f).is_ok())
            .collect();
        fingerprint_with(CONCLUSION_SOURCE_FINGERPRINT, &set)
    })
}

/// The projection version a persisted `Surface` is stamped with.
///
/// `Surface::project` reads the witness bag, so a warm lane that re-projects
/// from a bag-EVICTED copy records a different surface for the same unchanged
/// file than the cold lane did — measured at 76.7% of conclusions rows
/// rejected against rows that were in fact correct, and a warm-start freshness
/// verdict computed over a degraded projection
/// (`docs/adr/storage-engine.md`). Persisting the cold projection
/// is what makes the two lanes agree.
///
/// Its own gate, independent of `SCHEMA_VERSION`: a change to the projection
/// must invalidate persisted surfaces without dropping blobs, and a version
/// somebody has to remember to bump is the wrong instrument for a failure
/// nothing downstream can see — a stale surface deserializes cleanly and
/// simply describes a file that no longer exists in that shape.
pub fn surface_version() -> &'static str {
    env!("PERL_LSP_SURFACE_FINGERPRINT")
}

/// The env vars that change what a bake STORES. Consult-side flags do not
/// belong here — see `conclusion_fingerprint`.
const BAKE_STEERING_FLAGS: [&str; 2] = ["PERL_LSP_MINT_LINKS", "PERL_LSP_NO_BAKE"];

/// Split from the env read so the property — a set flag yields a DIFFERENT
/// fingerprint — is testable. `conclusion_fingerprint` memoizes into a
/// `OnceLock` on first use, so a test that set the variable could never observe
/// it.
fn fingerprint_with(source: &str, set: &[bool]) -> String {
    let mut fp = source.to_string();
    for (flag, on) in BAKE_STEERING_FLAGS.iter().zip(set) {
        if *on {
            fp.push('+');
            fp.push_str(flag);
        }
    }
    fp
}

#[cfg(test)]
mod fingerprint_tests {
    use super::*;

    /// A bake-steering flag must move the fingerprint, because its output
    /// OUTLIVES the process that wrote it.
    ///
    /// Base-verify by returning `source.to_string()` unconditionally: this
    /// fails, and the failure it stands in for does not — one `--check` under
    /// `PERL_LSP_MINT_LINKS=1` left a gold row failing for every later run
    /// until the cache was wiped by hand, looking exactly like a code
    /// regression.
    #[test]
    fn a_bake_steering_flag_changes_the_fingerprint() {
        let plain = fingerprint_with("abc", &[false, false]);
        assert_eq!(plain, "abc", "no flag set must leave the source alone");
        for i in 0..BAKE_STEERING_FLAGS.len() {
            let mut set = vec![false; BAKE_STEERING_FLAGS.len()];
            set[i] = true;
            assert_ne!(
                fingerprint_with("abc", &set),
                plain,
                "{} does not move the fingerprint, so a run under it silently \
                 leaves its maps for every later run to read",
                BAKE_STEERING_FLAGS[i]
            );
        }
        // And they must not collide with each other.
        assert_ne!(
            fingerprint_with("abc", &[true, false]),
            fingerprint_with("abc", &[false, true]),
        );
    }
}

pub fn init_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS modules (
            module_name      TEXT NOT NULL,
            path             TEXT NOT NULL,
            mtime_secs       INTEGER NOT NULL,
            file_size        INTEGER NOT NULL,
            source           TEXT NOT NULL DEFAULT 'import',
            analysis         BLOB,
            bag              BLOB,
            extract_version  INTEGER NOT NULL DEFAULT 0,
            deps_stamp       INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (module_name, path)
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
            name_id INTEGER NOT NULL,
            file_id INTEGER NOT NULL,
            PRIMARY KEY (name_id, file_id)
        ) WITHOUT ROWID;
        CREATE INDEX IF NOT EXISTS idx_refs_file ON refs(file_id);
        CREATE TABLE IF NOT EXISTS syms (
            file_id      INTEGER NOT NULL,
            name_id      INTEGER NOT NULL,
            key_id       INTEGER NOT NULL,
            kind         INTEGER NOT NULL,
            start_row    INTEGER NOT NULL,
            start_col    INTEGER NOT NULL,
            end_row      INTEGER NOT NULL,
            end_col      INTEGER NOT NULL,
            container_id INTEGER,
            flags        INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_syms_name ON syms(name_id);
        CREATE INDEX IF NOT EXISTS idx_syms_key ON syms(key_id);
        CREATE INDEX IF NOT EXISTS idx_syms_file ON syms(file_id);
        CREATE TABLE IF NOT EXISTS conclusions (
            path       TEXT NOT NULL,
            generation INTEGER NOT NULL,
            map        BLOB NOT NULL,
            PRIMARY KEY (path, generation)
        ) WITHOUT ROWID;
        CREATE TABLE IF NOT EXISTS stubs (
            path TEXT PRIMARY KEY,
            stub BLOB NOT NULL
        );
        CREATE TABLE IF NOT EXISTS surfaces (
            path    TEXT PRIMARY KEY,
            version TEXT NOT NULL,
            surface BLOB NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_modules_path ON modules(path);
        CREATE INDEX IF NOT EXISTS idx_modules_name ON modules(module_name);",
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
        .and_then(|_| conn.prepare("SELECT name_id, file_id FROM refs LIMIT 1").map(|_| ()))
        .and_then(|_| conn.prepare("SELECT flags, key_id FROM syms LIMIT 1").map(|_| ()))
        .is_ok()
        // The columns the current format DROPPED must be gone. Probing only
        // for what the new shape HAS would pass on the old twelve-column
        // table (it has those two as well), so a lying stamp would keep a
        // pre-dedup table alive and every shred would fail on the columns
        // it no longer writes.
        && conn.prepare("SELECT qual_kind FROM refs LIMIT 1").is_err();
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
                name_id INTEGER NOT NULL,
                file_id INTEGER NOT NULL,
                PRIMARY KEY (name_id, file_id)
             ) WITHOUT ROWID;
             CREATE INDEX idx_refs_file ON refs(file_id);
             CREATE TABLE syms (
                file_id      INTEGER NOT NULL,
                name_id      INTEGER NOT NULL,
                key_id       INTEGER NOT NULL,
                kind         INTEGER NOT NULL,
                start_row    INTEGER NOT NULL,
                start_col    INTEGER NOT NULL,
                end_row      INTEGER NOT NULL,
                end_col      INTEGER NOT NULL,
                container_id INTEGER,
                flags        INTEGER NOT NULL
             );
             CREATE INDEX idx_syms_name ON syms(name_id);
             CREATE INDEX idx_syms_key ON syms(key_id);
             CREATE INDEX idx_syms_file ON syms(file_id);",
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('ref_rows_version', ?1)",
            params![REF_ROWS_VERSION],
        )?;
        // The rebuild above DROPPED `strings`; every id a live writer memoized
        // for the previous generation is now dangling.
        bump_strings_generation(conn)?;
    }

    // The conclusions lane's own version + shape probe, same policy and same
    // reason: a DB stamped current by a build whose migration did not reshape
    // the table would keep serving rows whose stamp columns do not exist, and
    // every validity compare would fail open to "no stamp" — which reads as a
    // usable row rather than an unusable one.
    //
    // Wipe and re-bake, never a blob drop: the blobs are the derivation of
    // record and the repair frontier re-bakes from them.
    let concl_version: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'conclusion_rows_version'",
            [],
            |row| row.get(0),
        )
        .ok();
    let concl_shape_ok = conn
        .prepare("SELECT source_fingerprint, flush_generation FROM conclusions LIMIT 1")
        .is_ok();
    if concl_version.as_deref() != Some(CONCLUSION_ROWS_VERSION) || !concl_shape_ok {
        conn.execute_batch(
            "DROP TABLE IF EXISTS conclusions;
             CREATE TABLE conclusions (
                path               TEXT NOT NULL,
                generation         INTEGER NOT NULL,
                map                BLOB NOT NULL,
                source_fingerprint INTEGER NOT NULL,
                flush_generation   INTEGER NOT NULL,
                PRIMARY KEY (path, generation)
             ) WITHOUT ROWID;",
        )?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('conclusion_rows_version', ?1)",
            params![CONCLUSION_ROWS_VERSION],
        )?;
    }
    // Pre-existing tables (same schema version) predate `deps_stamp`; add it
    // in place rather than bumping SCHEMA_VERSION (a bump drops every row —
    // old rows carry 0, which validates only for empty-closure analyses, so
    // stale pack rows re-analyze while Perl caches survive the upgrade).
    let _ = conn.execute_batch(
        "ALTER TABLE modules ADD COLUMN deps_stamp INTEGER NOT NULL DEFAULT 0;",
    );
    // Same in-place treatment for the split-out witness bag. Pre-existing
    // rows keep their bag inside `analysis` and get a NULL here; they are
    // never READ as post-split rows, because the split bumped
    // `EXTRACT_VERSION` and every reader filters on it. So they age out by
    // re-resolution rather than needing a migration or a format probe.
    let _ = conn.execute_batch("ALTER TABLE modules ADD COLUMN bag BLOB;");
    // Pre-existing DBs predate the conclusion store; create it rather than
    // bumping SCHEMA_VERSION, which drops every blob row. An empty store is
    // correct on arrival — absent means "not baked", never "no answer".
    let _ = conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS conclusions (
            path       TEXT NOT NULL,
            generation INTEGER NOT NULL,
            map        BLOB NOT NULL,
            PRIMARY KEY (path, generation)
        ) WITHOUT ROWID;",
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
                    module_name      TEXT NOT NULL,
                    path             TEXT NOT NULL,
                    mtime_secs       INTEGER NOT NULL,
                    file_size        INTEGER NOT NULL,
                    source           TEXT NOT NULL DEFAULT 'import',
                    analysis         BLOB,
                    bag              BLOB,
                    extract_version  INTEGER NOT NULL DEFAULT 0,
                    deps_stamp       INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (module_name, path)
                );
                CREATE INDEX IF NOT EXISTS idx_modules_path ON modules(path);
                CREATE INDEX IF NOT EXISTS idx_modules_name ON modules(module_name);",
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

/// Wipe the derived tables (`refs`/`syms`/`files`/`strings`/`stubs`/
/// `surfaces`). Runs
/// alongside every `DELETE FROM modules` hard-clear: the rows are shredded
/// from the blobs, so a generation that invalidates the blobs invalidates
/// the rows with it. Cheap to rebuild — the next warm re-shreds from the
/// decoded analyses it is loading anyway.
pub fn clear_derived_rows(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "DELETE FROM refs; DELETE FROM syms; DELETE FROM files; DELETE FROM strings; \
         DELETE FROM stubs; DELETE FROM surfaces;",
    )?;
    bump_strings_generation(conn)
}

/// Invalidate every cached `str_id`. The shredder memoizes interned strings
/// for the writer's lifetime — without this, a wipe would leave those ids
/// pointing at rows that no longer exist and the refs rows written after it
/// would carry dangling `name_id`s: retrieval silently dead, nothing failing
/// loudly. Every path that empties `strings` bumps this.
pub fn bump_strings_generation(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES ('strings_generation', '1')
         ON CONFLICT(key) DO UPDATE SET value = CAST(CAST(meta.value AS INTEGER) + 1 AS TEXT)",
        [],
    )
    .map(|_| ())
}

/// The current `strings` generation. `0` when unstamped (a fresh DB) — the
/// memo keys on it, so an unstamped and a stamped-1 database never share
/// cached ids.
pub fn strings_generation(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT CAST(value AS INTEGER) FROM meta WHERE key = 'strings_generation'",
        [],
        |row| row.get(0),
    )
    .unwrap_or(0)
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

    let parsed = crate::index::builtins_pod::parse_perlfunc();

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

/// Drop the conclusion store when the derivation that produced it changed.
///
/// Deliberately NOT a `modules` wipe. A source change invalidates what the
/// conclusions MEAN while leaving every blob perfectly valid, so the right
/// cost is one re-bake per file — a decode of a blob we already have, which
/// is precisely the decode stage 1 made cheaper. Dropping blobs here would
/// turn a re-bake into a re-parse of the whole corpus for no reason.
///
/// Same meta-row shape as `validate_plugin_fingerprint`, including the
/// IMMEDIATE transaction: two validators racing on a missing stamp would both
/// clear, and the second would delete rows the first had just written.
pub fn validate_conclusion_fingerprint(
    conn: &Connection,
    fingerprint: &str,
) -> rusqlite::Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = validate_conclusion_fingerprint_inner(conn, fingerprint);
    match &result {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(_) => {
            let _ = conn.execute_batch("ROLLBACK");
        }
    }
    result
}

fn validate_conclusion_fingerprint_inner(
    conn: &Connection,
    fingerprint: &str,
) -> rusqlite::Result<()> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = 'conclusion_fingerprint'",
            [],
            |row| row.get(0),
        )
        .ok();
    if stored.as_deref() != Some(fingerprint) {
        log::info!(
            "Conclusion derivation changed (was {:?}, now {}), clearing conclusions \
             (blobs kept — each file re-bakes from the blob it already has)",
            stored,
            fingerprint
        );
        conn.execute("DELETE FROM conclusions", [])?;
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('conclusion_fingerprint', ?1)",
            params![fingerprint],
        )?;
    }
    Ok(())
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
