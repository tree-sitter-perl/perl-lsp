//! Persistence for the conclusion layer.
//!
//! The store is **generation-stamped**, which is not decoration. The driver
//! this feeds (`docs/prompt-enrichment-alternatives.md` §3c′) processes a
//! dirty frontier as one round against a FROZEN generation, and publishes the
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
    conn.query_row(
        "SELECT value FROM meta WHERE key = 'conclusion_generation'",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
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
    let blob: Vec<u8> = conn
        .query_row(
            "SELECT map FROM conclusions WHERE path = ?1 AND generation <= ?2 \
             ORDER BY generation DESC LIMIT 1",
            params![path, at.0],
            |row| row.get(0),
        )
        .ok()?;
    let bin = zstd::decode_all(blob.as_slice()).ok()?;
    bincode::deserialize(&bin).ok()
}

/// Write one file's conclusions at a generation.
///
/// Caller-supplied generation, not "current + 1": a round writes MANY files at
/// one generation, and letting each write pick its own would scatter a single
/// logical round across several, which is exactly the half-built state the
/// pinning is designed to prevent.
pub fn save_conclusions(
    conn: &Connection,
    path: &str,
    at: Generation,
    map: &ConclusionMap,
) -> bool {
    let Ok(bin) = bincode::serialize(map) else {
        return false;
    };
    let Ok(blob) = zstd::encode_all(bin.as_slice(), ZSTD_LEVEL) else {
        return false;
    };
    conn.execute(
        "INSERT OR REPLACE INTO conclusions (path, generation, map) VALUES (?1, ?2, ?3)",
        params![path, at.0, blob],
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
    entries: &[(String, ConclusionMap)],
) -> rusqlite::Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| -> rusqlite::Result<()> {
        for (path, map) in entries {
            if !save_conclusions(conn, path, at, map) {
                // An entry that will not encode cannot be published, and
                // publishing the rest under this generation would present a
                // partial round as complete. Fail the round instead.
                return Err(rusqlite::Error::InvalidParameterName(format!(
                    "conclusion encode failed for {path}"
                )));
            }
        }
        conn.execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES ('conclusion_generation', ?1)",
            params![at.0.to_string()],
        )?;
        Ok(())
    })();
    match &result {
        Ok(()) => conn.execute_batch("COMMIT")?,
        Err(_) => {
            let _ = conn.execute_batch("ROLLBACK");
        }
    }
    result
}

/// Drop one path's conclusions — the file changed, so its bake is void.
pub fn forget_conclusions(conn: &Connection, path: &str) {
    let _ = conn.execute("DELETE FROM conclusions WHERE path = ?1", params![path]);
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
    conn.execute(
        "DELETE FROM conclusions WHERE generation < ?1 AND generation < (
             SELECT MAX(c2.generation) FROM conclusions c2
             WHERE c2.path = conclusions.path AND c2.generation <= ?1
         )",
        params![keep.0],
    )
    .unwrap_or(0)
}
