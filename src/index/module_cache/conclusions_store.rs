//! Persistence for the conclusion layer.
//!
//! The store is **generation-stamped**, which is not decoration. The driver
//! this feeds (`docs/adr/conclusion-layer.md`, "Change propagation: the
//! flush worklist") processes a dirty frontier as one round against a
//! FROZEN generation, and publishes the
//! next generation atomically when the round's diffs go empty. Two properties
//! follow, and both are the store's job rather than the driver's:
//!
//! * A reader pins a generation. A consult that begins during a flush must see
//!   gen N throughout — never a half-built N+1 — or it can compose an answer
//!   out of two different worlds and be wrong in a way that reproduces on
//!   neither.
//! * A round's writes land together. `publish_generation` is one transaction,
//!   so a crash mid-flush leaves gen N intact rather than a mixture.
//!
//! Invalidation is NOT the blob stamp alone. A conclusion also goes stale when
//! the derivation that produced it changes, which leaves the bytes valid and
//! the meaning wrong — `validate_conclusion_fingerprint` owns that, and it
//! clears conclusions while keeping blobs, because the repair is a re-bake and
//! a re-bake wants the blob.

use super::*;
use crate::model::witnesses::ConclusionMap;

/// The generation a reader is pinned to.
///
/// A newtype rather than a bare `i64` so a caller cannot pass the file's
/// mtime, a row id, or "the current one" by accident — every one of which
/// would read as a plausible number and silently select the wrong world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Generation(pub i64);

impl Generation {
    /// The generation a store has before anything is published into it.
    pub const INITIAL: Generation = Generation(0);
}

/// The store's current generation — what a fresh reader should pin.
pub fn current_generation(conn: &Connection) -> Generation {
    // `prepare_cached` for the same reason as `load_conclusions_stamped`:
    // read per consult-path load on a retained connection.
    conn.prepare_cached(sql::META_GET.registered())
        .ok()
        .and_then(|mut s| {
            s.query_row(params!["conclusion_generation"], |row| row.get::<_, String>(0)).ok()
        })
        .and_then(|v| v.parse::<i64>().ok())
        .map(Generation)
        .unwrap_or(Generation::INITIAL)
}

/// Load one file's conclusions AS OF a pinned generation.
///
/// The newest row at or below `at`. Two halves, and both are load-bearing:
///
/// Rows ABOVE `at` are invisible, because mid-flush the writer may already
/// have published part of gen N+1 and a reader pinned to N must not see it.
///
/// Rows BELOW `at` survive, because the key is `(path, generation)` rather
/// than `path`. Keying on path alone would make publishing N+1 REPLACE the
/// gen-N row, and a reader still pinned to N would then find nothing — which
/// the evaluator reads as a definite `None`, the one meaning absence must
/// never carry. The pin would have silently become a way to get wrong
/// answers instead of a way to avoid them.
pub fn load_conclusions(
    conn: &Connection,
    path: &str,
    at: Generation,
) -> Option<ConclusionMap> {
    load_conclusions_stamped(conn, path, at).map(|(m, _)| m)
}

/// `load_conclusions` plus the row's own stamps — what a validity check needs.
///
/// The map is returned WITHOUT judging it. Validity is the caller's compare,
/// because only the caller knows the fingerprint the index currently records
/// for this path, and re-deriving it here would mean hashing a surface on the
/// consult path — which is the hot path this whole layer exists to shorten.
pub fn load_conclusions_stamped(
    conn: &Connection,
    path: &str,
    at: Generation,
) -> Option<(ConclusionMap, RowStamp)> {
    // `prepare_cached`: the consult path runs this once per unique file on a
    // RETAINED connection, so the parsed statement amortizes; a plain
    // `query_row` re-parses the SQL on every load.
    let (blob, fp, gen): (Vec<u8>, i64, i64) = conn
        .prepare_cached(sql::CONCLUSIONS_LOAD.registered())
        .ok()?
        .query_row(params![path, at.0], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .ok()?;
    let map = decode_blob(&blob)?;
    Some((map, RowStamp { source_fingerprint: fp as u64, flush_generation: Generation(gen) }))
}

/// What a persisted conclusion row asserts about itself.
///
/// `source_fingerprint` is the surface fingerprint of the analysis the map was
/// baked from — the same value `FreshnessIndex` records for the path. A row
/// whose fingerprint differs from what the index currently records describes a
/// file that has moved, and reads as absent.
///
/// That one compare subsumes three failure modes that used to need three
/// different erasers to remember them: an ORPHANED row whose `modules` row was
/// erased, a STALE row for an edited file, and a row from an INTERRUPTED write.
/// None of them can match, so correctness stops depending on any caller
/// remembering to delete anything — the erasers remain for space, not for
/// truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowStamp {
    pub source_fingerprint: u64,
    pub flush_generation: Generation,
}

/// Write one file's conclusions at a generation.
///
/// Caller-supplied generation, not "current + 1": a round writes MANY files at
/// one generation, and letting each write pick its own would scatter a single
/// logical round across several, which is exactly the half-built state the
/// pinning is designed to prevent.
///
/// The row carries its own `source_fingerprint`, so it does not depend on a
/// `modules` row existing to be judged: a consult compares the stamp against
/// what the freshness index records for the path, and a row describing any
/// other state reads absent.
pub fn save_conclusions(
    conn: &Connection,
    path: &str,
    at: Generation,
    map: &ConclusionMap,
    stamp: RowStamp,
) -> bool {
    let Some(blob) = encode_blob(map) else {
        return false;
    };
    conn.execute(
        sql::CONCLUSIONS_SAVE.registered(),
        params![path, at.0, blob, stamp.source_fingerprint as i64, stamp.flush_generation.0],
    )
    .is_ok()
}

/// Publish a completed round: every entry lands, then the generation advances,
/// in ONE transaction.
///
/// The ordering inside matters and is the reverse of what reads natural. Rows
/// are written first and the generation stamp last, so a reader that catches
/// the transaction mid-flight still sees the OLD stamp and therefore ignores
/// the new rows. Stamping first would publish a generation whose rows do not
/// all exist yet, and `load_conclusions` would serve absence — which the
/// evaluator reads as a definite `None`, the one thing absence must never
/// mean.
pub fn publish_generation(
    conn: &Connection,
    at: Generation,
    entries: &[(String, ConclusionMap, u64)],
) -> rusqlite::Result<()> {
    super::in_immediate_txn(conn, || {
        for (path, map, fingerprint) in entries {
            let stamp = RowStamp {
                source_fingerprint: *fingerprint,
                flush_generation: at,
            };
            if !save_conclusions(conn, path, at, map, stamp) {
                // An entry that will not encode cannot be published, and
                // publishing the rest under this generation would present a
                // partial round as complete. Fail the round instead.
                return Err(rusqlite::Error::InvalidParameterName(format!(
                    "conclusion encode failed for {path}"
                )));
            }
        }
        conn.execute(
            sql::META_SET.registered(),
            params!["conclusion_generation", at.0.to_string()],
        )?;
        Ok(())
    })
}

/// Drop one path's conclusions — the file changed, so its bake is void.
pub fn forget_conclusions(conn: &Connection, path: &str) {
    let _ = conn.execute(sql::CONCLUSIONS_DELETE_PATH.registered(), params![path]);
}

/// Discard superseded rows once no reader can still be pinned below `keep`.
///
/// Retaining old generations is what makes a pin safe, so the table grows one
/// row per file per round until something reclaims them — and the caller is
/// the only one who knows when the last reader let go. Deliberately not
/// automatic: a sweep that guessed would delete the generation a live consult
/// is reading, which is the same absence-as-answer failure the retention
/// exists to prevent, arriving by a different door.
pub fn prune_generations_below(conn: &Connection, keep: Generation) -> usize {
    // Keep the newest row at or below `keep` for each path — that is the one a
    // reader pinned at `keep` still resolves to. Only rows OLDER than that are
    // genuinely unreachable.
    conn.execute(sql::CONCLUSIONS_PRUNE.registered(), params![keep.0]).unwrap_or(0)
}

/// Paths holding a valid blob but no conclusion row a reader could use, OR no
/// persisted surface at the current projection version.
///
/// Both lanes share one frontier because they share the expensive step: a
/// with-bag decode. Splitting them would decode each file twice to fix two
/// products of one projection.
///
/// The repair frontier. `validate_conclusion_fingerprint` clears conclusions
/// and deliberately keeps the blobs, "because the repair is a re-bake" — but
/// nothing drove that re-bake, so after any source edit the layer stayed dark
/// until someone ran a full `--clear-cache` by hand
/// (`conclcache.known_absent` read 156,746 in that state). Answer-neutral —
/// absent from the STORE means decode, not "no answer" — and purely a cost,
/// but a permanent one, and it silently made every measurement taken after a
/// rebuild a measurement of an empty layer.
///
/// Enumerated by QUERY rather than by remembering what the clear touched, so
/// it also covers a file whose bake failed to encode, one written by a
/// `PERL_LSP_NO_BAKE` run, and any future path that persists a blob without a
/// map. "Which rows are missing" is a question the store can always answer;
/// "which rows did we drop" is one only the dropper knows.
///
/// Version-filtered: a row below `EXTRACT_VERSION` is going to be re-parsed
/// anyway, and baking it would spend the work twice.
///
/// The two disjuncts repair different PRODUCTS, and only one obeys the bake
/// control: `include_missing_maps=false` (a `PERL_LSP_NO_BAKE` arm) selects
/// surface-missing paths only, because the map half of the repair is the
/// second conclusions producer — while the surface half is the freshness
/// machinery's product, which the control does not claim to ablate. Gating
/// the WHOLE frontier on the bake flag left a NO_BAKE arm running against
/// un-repaired surfaces: a control that ablated more than the thing it
/// controls, the `the_bake_gate_has_one_reader` failure shape one level up.
pub fn paths_needing_repair(
    conn: &Connection,
    at: Generation,
    include_missing_maps: bool,
) -> Vec<String> {
    let frontier = if include_missing_maps {
        sql::REPAIR_FRONTIER
    } else {
        sql::REPAIR_FRONTIER_SURFACES
    };
    let mut stmt = match conn.prepare(frontier.registered()) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("conclusion repair: could not enumerate frontier: {e}");
            return Vec::new();
        }
    };
    let rows = stmt.query_map(params![EXTRACT_VERSION, at.0, super::surface_version()], |r| {
        r.get::<_, String>(0)
    });
    match rows {
        Ok(it) => it.filter_map(|r| r.ok()).collect(),
        Err(e) => {
            log::warn!("conclusion repair: frontier query failed: {e}");
            Vec::new()
        }
    }
}

/// How many paths one repair slice takes.
///
/// Small on purpose. The slice runs where the resolver thread would otherwise
/// be blocked on its condvar, so its length is the worst-case delay a real
/// resolve request waits behind — this is a latency bound, not a throughput
/// knob. Bigger chunks would amortize the transaction better and make the
/// server less responsive, which is the wrong trade for work that has no
/// deadline.
pub const REPAIR_SLICE: usize = 32;

/// Re-bake one slice of the frontier and store it. Returns how many landed.
///
/// The decode and the bake happen OUTSIDE the transaction, and only the writes
/// go inside it: a slice is tens of milliseconds of CPU, and holding SQLite's
/// write lock across that would stall every other writer for the duration of
/// work that is not writing.
pub fn repair_conclusions_slice(
    conn: &Connection,
    paths: &[String],
    at: Generation,
) -> usize {
    // The map half obeys the bake control (the repair lane is the SECOND
    // conclusions producer — letting it bake under PERL_LSP_NO_BAKE would
    // hand the control's own arm a full layer from its second warm run);
    // the surface half runs regardless — the same split as the frontier
    // query, or the control ablates a lane it does not claim.
    let bake_on = !crate::model::witnesses::bake_disabled();
    let baked: Vec<(&String, Vec<u8>, u64, Vec<u8>)> = paths
        .iter()
        .filter_map(|path| {
            // WITH the bag: the bake reads witnesses, and a bagless decode
            // would produce a map that concludes nothing while looking like a
            // successful repair — the same silent-empty shape this whole
            // repair exists to undo.
            let fa = super::load_one_diag(conn, path, true).ok()?;
            let blob = if bake_on {
                super::bake_conclusions_blob(&fa, &fa.witnesses)
            } else {
                Vec::new()
            };
            // Stamped from the analysis the repair actually decoded, not from
            // the index: a repair races the writers, and a fingerprint read
            // elsewhere could describe a newer state than these bytes.
            let projected = crate::model::surface::Surface::project(&fa);
            let fingerprint = crate::model::surface::surface_fingerprint(&projected);
            // The repair is the ONLY lane that decodes with the bag after a
            // projection-version change, so it is where a surface written by
            // an older `Surface::project` gets replaced. Free here: the
            // fingerprint above already paid for the projection.
            let surface = super::encode_surface(&projected).unwrap_or_default();
            if blob.is_empty() && surface.is_empty() {
                // Nothing encodable in EITHER product. Leaving both rows
                // absent is right: the reader falls back to a decode, which
                // is where it already was. (An unencodable map alone must
                // not drop the surface write — that was the executor's own
                // copy of the half-gate.)
                crate::util::ghost_stats::count("repair.nothing_to_bake");
                return None;
            }
            Some((path, blob, fingerprint, surface))
        })
        .collect();
    if baked.is_empty() {
        return 0;
    }
    let mut landed = 0usize;
    if conn.execute_batch(sql::TXN_BEGIN_IMMEDIATE.registered()).is_err() {
        log::warn!("conclusion repair: txn open failed; the slice defers to the next pass");
        return 0;
    }
    for (path, blob, fingerprint, surface) in &baked {
        if !surface.is_empty() {
            let r = conn.execute(
                sql::SURFACE_SAVE.registered(),
                params![path, super::surface_version(), surface],
            );
            if let Err(e) = r {
                log::warn!("conclusion repair: surface store failed for '{path}': {e}");
            }
        }
        if blob.is_empty() {
            // Surface-only repair (bake gated, or the map would not encode):
            // the surface write above is the whole product.
            landed += 1;
            continue;
        }
        let r = conn.execute(
            sql::CONCLUSIONS_SAVE.registered(),
            params![path, at.0, blob, *fingerprint as i64, at.0],
        );
        match r {
            Ok(_) => landed += 1,
            Err(e) => log::warn!("conclusion repair: store failed for '{path}': {e}"),
        }
    }
    if let Err(e) = conn.execute_batch(sql::TXN_COMMIT.registered()) {
        log::warn!("conclusion repair: commit failed: {e}");
        let _ = conn.execute_batch(sql::TXN_ROLLBACK.registered());
        return 0;
    }
    crate::util::ghost_stats::count_by("repair.baked", landed as u64);
    landed
}
