//! Warm stubs: the register-from-Surface payload persisted beside each
//! pack blob — codec, version gate, and the guarded write/delete seams.

use super::*;

/// The register-from-Surface warm payload: everything bulk registration
/// derives from a WHOLE analysis, precomputed at persist time so warm
/// start never decodes the full blob. `skeleton` is the bag/refs/symbols-
/// stripped analysis — the exact struct the resident copy would be, so
/// all present-view routing and rehydration behave identically to a
/// full-decode-then-strip warm by construction.
pub struct WarmStub {
    pub feed: Vec<(String, bool)>,
    pub specs: Vec<(String, String)>,
    pub handlers: Vec<(String, crate::model::file_analysis::HandlerOwner)>,
    pub surface: crate::model::surface::Surface,
    pub skeleton: FileAnalysis,
}

/// Bump when the stub's MEANING changes without breaking its bincode
/// decode (a decode break self-heals to the full-blob path). Mismatch
/// wipes the `stubs` table; the next warm backfills from full decodes.
const STUB_VERSION: &str = "10";

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
    handlers: &[(String, crate::model::file_analysis::HandlerOwner)],
    surface: &crate::model::surface::Surface,
    skeleton: &FileAnalysis,
) -> Option<Vec<u8>> {
    let bin = bincode::serialize(&(feed, specs, handlers, surface, skeleton)).ok()?;
    zstd::encode_all(bin.as_slice(), ZSTD_LEVEL).ok()
}

pub fn decode_stub(blob: &[u8]) -> Option<WarmStub> {
    let bin = zstd::decode_all(blob).ok()?;
    let (feed, specs, handlers, surface, mut skeleton): (
        Vec<(String, bool)>,
        Vec<(String, String)>,
        Vec<(String, crate::model::file_analysis::HandlerOwner)>,
        crate::model::surface::Surface,
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
    Some(WarmStub { feed, specs, handlers, surface, skeleton })
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
/// `stamp`. A concurrent pack invalidation may rewrite the row (deleting
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
