//! Schema and generation gates: table DDL, the version stamps
//! (schema / extract / ref-rows), and the meta-keyed hard-clear
//! validators (@INC, plugin set, analysis inputs) plus builtins
//! hydration (same meta-row pattern).

use super::*;

const SCHEMA_VERSION: &str = "10";

/// Bumped when the builder's analysis output changes shape in a way that
/// invalidates cached blobs. Unlike `SCHEMA_VERSION`, this does not drop
/// the table — stale entries are re-resolved lazily with priority.
pub const EXTRACT_VERSION: i64 = 317;

/// Bumped when the ROW format of the relational ref index changes shape.
/// Unlike `EXTRACT_VERSION` (which governs the blobs), a mismatch only wipes
/// the derived `refs`/`files`/`strings` tables — the blobs stay valid and the
/// next warm re-shreds rows from the already-decoded analyses for free.
pub(super) const REF_ROWS_VERSION: &str = "9";

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
    conn.execute_batch(sql::SCHEMA_CREATE.registered())?;
    // Row-format generation for the derived tables (see REF_ROWS_VERSION).
    let rows_version: Option<String> = conn
        .query_row(sql::META_GET.registered(), params!["ref_rows_version"], |row| row.get(0))
        .ok();
    // The stamp alone is trusted too far: a DB stamped current by a build
    // whose migration didn't actually reshape the tables would never
    // re-migrate, leaving every shred failing on a missing column while
    // composition quietly masks it (refs stay resident, retrieval dead,
    // diagnostics typeless). Probe the shape the current format requires so
    // a lying stamp still triggers the rebuild.
    let shape_ok = conn
        .prepare(sql::SHAPE_FILES.registered())
        .map(|_| ())
        .and_then(|_| conn.prepare(sql::SHAPE_REFS.registered()).map(|_| ()))
        .and_then(|_| conn.prepare(sql::SHAPE_SYMS.registered()).map(|_| ()))
        .and_then(|_| conn.prepare(sql::SHAPE_HELD.registered()).map(|_| ()))
        .is_ok()
        // The columns the current format DROPPED must be gone. Probing only
        // for what the new shape HAS would pass on the old twelve-column
        // table (it has those two as well), so a lying stamp would keep a
        // pre-dedup table alive and every shred would fail on the columns
        // it no longer writes.
        && conn.prepare(sql::SHAPE_REFS_DROPPED_COLUMN.registered()).is_err();
    if rows_version.as_deref() != Some(REF_ROWS_VERSION) || !shape_ok {
        // DROP, not DELETE: a format change may alter the table SHAPE, and
        // `CREATE TABLE IF NOT EXISTS` above no-ops on the old shape — a
        // row-only wipe would leave every future shred failing on a missing
        // column while composition quietly masks it (refs stay resident,
        // retrieval dead). Recreate from scratch.
        conn.execute_batch(sql::ROWS_REBUILD.registered())?;
        conn.execute(sql::META_SET.registered(), params!["ref_rows_version", REF_ROWS_VERSION])?;
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
        .query_row(sql::META_GET.registered(), params!["conclusion_rows_version"], |row| row.get(0))
        .ok();
    let concl_shape_ok = conn
        .prepare(sql::SHAPE_CONCLUSIONS.registered())
        .is_ok();
    if concl_version.as_deref() != Some(CONCLUSION_ROWS_VERSION) || !concl_shape_ok {
        conn.execute_batch(sql::CONCLUSIONS_REBUILD.registered())?;
        conn.execute(
            sql::META_SET.registered(),
            params!["conclusion_rows_version", CONCLUSION_ROWS_VERSION],
        )?;
    }
    // Pre-existing tables (same schema version) predate `deps_stamp`; add it
    // in place rather than bumping SCHEMA_VERSION (a bump drops every row —
    // old rows carry 0, which validates only for empty-closure analyses, so
    // stale pack rows re-analyze while Perl caches survive the upgrade).
    let _ = conn.execute_batch(sql::MODULES_ADD_DEPS_STAMP.registered());
    // Same in-place treatment for the split-out witness bag. Pre-existing
    // rows keep their bag inside `analysis` and get a NULL here; they are
    // never READ as post-split rows, because the split bumped
    // `EXTRACT_VERSION` and every reader filters on it. So they age out by
    // re-resolution rather than needing a migration or a format probe.
    let _ = conn.execute_batch(sql::MODULES_ADD_BAG.registered());
    // Pre-existing DBs predate the conclusion store; create it rather than
    // bumping SCHEMA_VERSION, which drops every blob row. An empty store is
    // correct on arrival — absent means "not baked", never "no answer".
    let _ = conn.execute_batch(sql::CONCLUSIONS_CREATE_LEGACY.registered());
    // Stub generation gate — stamped here so every fresh DB is writable by
    // the persist writers (their per-chunk `stub_version_current` check
    // would otherwise fail-closed until the first warm scan stamped it).
    validate_stub_version(conn);

    let version: Option<String> = conn
        .query_row(sql::META_GET.registered(), params!["schema_version"], |row| row.get(0))
        .ok();

    match version.as_deref() {
        Some(SCHEMA_VERSION) => Ok(()),
        Some(_) => {
            conn.execute_batch(sql::MODULES_DROP.registered())?;
            clear_derived_rows(conn)?;
            conn.execute_batch(sql::MODULES_CREATE.registered())?;
            conn.execute(sql::META_SET.registered(), params!["schema_version", SCHEMA_VERSION])?;
            Ok(())
        }
        None => {
            conn.execute(sql::META_SET.registered(), params!["schema_version", SCHEMA_VERSION])?;
            Ok(())
        }
    }
}

/// Wipe the derived tables (`refs`/`syms`/`held`/`files`/`strings`/`stubs`/
/// `surfaces`). Runs
/// alongside every `DELETE FROM modules` hard-clear: the rows are shredded
/// from the blobs, so a generation that invalidates the blobs invalidates
/// the rows with it. Cheap to rebuild — the next warm re-shreds from the
/// decoded analyses it is loading anyway.
pub fn clear_derived_rows(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(sql::DERIVED_ROWS_CLEAR.registered())?;
    bump_strings_generation(conn)
}

/// Invalidate every cached `str_id`. The shredder memoizes interned strings
/// for the writer's lifetime — without this, a wipe would leave those ids
/// pointing at rows that no longer exist and the refs rows written after it
/// would carry dangling `name_id`s: retrieval silently dead, nothing failing
/// loudly. Every path that empties `strings` bumps this.
pub fn bump_strings_generation(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(sql::STRINGS_GENERATION_BUMP.registered(), [])
        .map(|_| ())
}

/// The current `strings` generation. `0` when unstamped (a fresh DB) — the
/// memo keys on it, so an unstamped and a stamped-1 database never share
/// cached ids.
pub fn strings_generation(conn: &Connection) -> i64 {
    conn.query_row(sql::META_GET.registered(), params!["strings_generation"], |row| {
        row.get::<_, String>(0)
    })
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(0)
}

/// Drop the import tier when the dependency map it was landed under moved
/// (`MapSnapshot::fingerprint`: search path, tier roots, and the manifests'
/// stamps). The same move retires every landing in a running process, so
/// one rule serves the warm start and the in-session move alike.
///
/// Import-tier only: workspace blobs bake plugin emissions and their own
/// source, not the map — and the workspace indexer may have written its
/// rows BEFORE this validation runs (two writers, one DB), so anything
/// broader would delete rows mid-write and leave already-evicted resident
/// copies with no retrieval source.
pub fn validate_dependency_fingerprint(conn: &Connection, fingerprint: u64) -> rusqlite::Result<()> {
    let current = format!("{:016x}", fingerprint);
    let stored: Option<String> = conn
        .query_row(sql::META_GET.registered(), params!["dependency_fingerprint"], |row| row.get(0))
        .ok();

    if stored.as_deref() != Some(&current) {
        log::info!(
            "Dependency map changed (was {:?}, now {}), clearing the import tier",
            stored,
            current
        );
        conn.execute(sql::MODULES_CLEAR_IMPORT.registered(), [])?;
        conn.execute(sql::REFS_CLEAR_IMPORT.registered(), [])?;
        conn.execute(sql::SYMS_CLEAR_IMPORT.registered(), [])?;
        conn.execute(sql::HELD_CLEAR_IMPORT.registered(), [])?;
        conn.execute(sql::FILES_CLEAR_IMPORT.registered(), [])?;
        conn.execute(sql::META_SET.registered(), params!["dependency_fingerprint", current])?;
    }
    Ok(())
}

/// The store gates every opener of a language's DB runs before reading it:
/// the driver's analysis inputs (a hard clear) and the conclusion
/// derivation (conclusions only). Idempotent, so the resolver and the bulk
/// indexer each run it on the one DB they share.
pub fn validate_store(
    conn: &Connection,
    driver: &dyn crate::build::language_driver::LanguageDriver,
    root: Option<&std::path::Path>,
) -> rusqlite::Result<()> {
    validate_input_fingerprint(conn, driver.analysis_input_fingerprint(root))?;
    validate_conclusion_fingerprint(conn, crate::index::module_cache::conclusion_fingerprint())
}

/// Hydrate the in-memory `builtins` mirror from SQLite, parsing
/// `perlfunc.pod` and writing rows on first use (or when the perl
/// version tag changes since the last run). Returns the populated
/// map. Keyed in `meta` under `builtins_perl_version`: mismatch wipes
/// the table and re-parses, same pattern as `validate_input_fingerprint`.
pub fn hydrate_builtins(conn: &Connection) -> rusqlite::Result<DashMap<String, String>> {
    let map: DashMap<String, String> = DashMap::new();

    let stored: Option<String> = conn
        .query_row(sql::META_GET.registered(), params!["builtins_perl_version"], |row| row.get(0))
        .ok();

    let parsed = crate::index::builtins_pod::parse_perlfunc();

    let need_parse = match (&stored, &parsed) {
        (Some(s), Some(p)) => *s != p.perl_version,
        (None, Some(_)) => true,
        _ => false, // no parse + no cache rows we trust → leave map empty
    };

    if need_parse {
        if let Some(p) = parsed.as_ref() {
            conn.execute(sql::BUILTINS_CLEAR.registered(), [])?;
            in_immediate_txn(conn, || {
                let mut stmt = conn.prepare(sql::BUILTINS_INSERT.registered())?;
                for (name, doc) in &p.entries {
                    stmt.execute(params![name, doc])?;
                }
                Ok(())
            })?;
            conn.execute(
                sql::META_SET.registered(),
                params!["builtins_perl_version", p.perl_version],
            )?;
            log::info!("Indexed {} Perl builtins from {}", p.entries.len(), p.perl_version);
        }
    }

    // Read whatever's in the table now (either freshly written, or
    // the same rows from a prior run) into the in-memory mirror.
    let mut stmt = conn.prepare(sql::BUILTINS_ALL.registered())?;
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
/// Same meta-row shape as `validate_input_fingerprint`, including the
/// IMMEDIATE transaction: two validators racing on a missing stamp would both
/// clear, and the second would delete rows the first had just written.
pub fn validate_conclusion_fingerprint(
    conn: &Connection,
    fingerprint: &str,
) -> rusqlite::Result<()> {
    in_immediate_txn(conn, || validate_conclusion_fingerprint_inner(conn, fingerprint))
}

fn validate_conclusion_fingerprint_inner(
    conn: &Connection,
    fingerprint: &str,
) -> rusqlite::Result<()> {
    let stored: Option<String> = conn
        .query_row(sql::META_GET.registered(), params!["conclusion_fingerprint"], |row| row.get(0))
        .ok();
    if stored.as_deref() != Some(fingerprint) {
        log::info!(
            "Conclusion derivation changed (was {:?}, now {}), clearing conclusions \
             (blobs kept — each file re-bakes from the blob it already has)",
            stored,
            fingerprint
        );
        conn.execute(sql::CONCLUSIONS_CLEAR.registered(), [])?;
        conn.execute(sql::META_SET.registered(), params!["conclusion_fingerprint", fingerprint])?;
    }
    Ok(())
}

/// Drop the modules table when the driver's external analysis inputs (the
/// C++ toolchain: system include roots, predefined macros — or its probe
/// FAILURE; Perl's plugin set) changed since the rows were written: a
/// generation built under degraded/different inputs must not be served
/// under the current ones (H8). Without it a plugin author who edits a
/// `.rhai` and restarts would read the old plugin's emissions from cached
/// blobs.
pub fn validate_input_fingerprint(conn: &Connection, fingerprint: u64) -> rusqlite::Result<()> {
    // IMMEDIATE: check-and-stamp must be atomic against the other writer
    // (resolver thread vs bulk indexer) — two validators both reading a
    // missing stamp would both hard-clear, the second deleting rows the
    // first writer committed in between.
    in_immediate_txn(conn, || validate_input_fingerprint_inner(conn, fingerprint))
}

fn validate_input_fingerprint_inner(conn: &Connection, fingerprint: u64) -> rusqlite::Result<()> {
    let fingerprint = format!("{:016x}", fingerprint);
    let stored: Option<String> = conn
        .query_row(sql::META_GET.registered(), params!["input_fingerprint"], |row| row.get(0))
        .ok();

    if stored.as_deref() != Some(&fingerprint) {
        log::info!(
            "Analysis inputs changed (was {:?}, now {}), clearing module cache",
            stored,
            fingerprint
        );
        conn.execute(sql::MODULES_CLEAR.registered(), [])?;
        clear_derived_rows(conn)?;
        conn.execute(sql::META_SET.registered(), params!["input_fingerprint", fingerprint])?;
    }
    Ok(())
}

/// Run `body` inside `BEGIN IMMEDIATE`, committing on `Ok` and rolling back
/// on `Err`. IMMEDIATE takes the write lock up front, so a check-and-stamp
/// validator's read cannot race the other writer's clear.
pub(crate) fn in_immediate_txn<T>(
    conn: &Connection,
    body: impl FnOnce() -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    conn.execute_batch(sql::TXN_BEGIN_IMMEDIATE.registered())?;
    let result = body();
    match &result {
        Ok(_) => conn.execute_batch(sql::TXN_COMMIT.registered())?,
        Err(_) => {
            let _ = conn.execute_batch(sql::TXN_ROLLBACK.registered());
        }
    }
    result
}
