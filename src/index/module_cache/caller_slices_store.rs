//! Persisted caller slices (`model::witnesses::caller_slice`): one row per
//! (file, call name) as each is first cut, plus the file's frame under the
//! empty key. No row means not cut yet, never "no call". A row holds
//! for the source text it was cut from (`stamp`) under the extractor that
//! cut it; a reader asks for the stamp it holds, so a stale row is never
//! served and needs no deletion to stop being read.

use rusqlite::{params, Connection, OptionalExtension};

use super::EXTRACT_VERSION;

const ZSTD_LEVEL: i32 = 1;

/// The key a file's frame is stored under.
pub const FRAME_KEY: &str = "";

/// Bodies under this many bytes are stored raw: a compressor per small
/// slice cost more time than the bytes it saved (Foswiki, measured).
const RAW_BELOW: usize = 4096;
const RAW: u8 = 0;
const ZSTD: u8 = 1;

pub fn encode_slice_body<T: serde::Serialize>(v: &T) -> Option<Vec<u8>> {
    let bin = bincode::serialize(v).ok()?;
    if bin.len() < RAW_BELOW {
        let mut out = Vec::with_capacity(bin.len() + 1);
        out.push(RAW);
        out.extend_from_slice(&bin);
        return Some(out);
    }
    let mut out = vec![ZSTD];
    out.extend(zstd::encode_all(bin.as_slice(), ZSTD_LEVEL).ok()?);
    Some(out)
}

pub fn decode_slice_body<T: serde::de::DeserializeOwned>(blob: &[u8]) -> Option<T> {
    let (&tag, body) = blob.split_first()?;
    match tag {
        RAW => bincode::deserialize(body).ok(),
        ZSTD => bincode::deserialize(&zstd::decode_all(body).ok()?).ok(),
        _ => None,
    }
}

/// The row for `(path, key)` cut from the text stamped `stamp`.
pub fn load_caller_slice(conn: &Connection, path: &str, key: &str, stamp: u64) -> Option<Vec<u8>> {
    conn.query_row(
        "SELECT body FROM caller_slice WHERE path = ?1 AND key = ?2 AND stamp = ?3 AND extract_version = ?4",
        params![path, key, stamp as i64, EXTRACT_VERSION],
        |row| row.get::<_, Vec<u8>>(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// One row: a file's frame or one call name's slice, cut from the text
/// stamped `stamp`.
pub struct CallerSliceRow {
    pub path: String,
    pub key: String,
    pub stamp: u64,
    pub body: Vec<u8>,
    /// The slice holds a plain function call of `key`: what `calls_among`
    /// asks, answerable without decoding `body`.
    pub plain_calls: bool,
    /// On a frame: every call name of the file has a row at this stamp, so
    /// a name without one is called nowhere in it.
    pub complete: bool,
}

impl CallerSliceRow {
    pub fn frame(path: String, stamp: u64, body: Vec<u8>) -> CallerSliceRow {
        CallerSliceRow { path, key: FRAME_KEY.to_string(), stamp, body, plain_calls: false, complete: false }
    }

    pub fn slice(path: String, key: String, stamp: u64, body: Vec<u8>, slice: &crate::model::witnesses::caller_slice::CallerSlice) -> CallerSliceRow {
        CallerSliceRow { path, key, stamp, body, plain_calls: slice.has_plain_call(), complete: false }
    }
}

/// Whether `path`'s text stamped `stamp` holds a plain function call keyed
/// `key`, from the stored flags alone: the slice row's flag, or `false` when
/// the file's frame says every call name has a row and `key` has none.
/// `None` when the store cannot say.
pub fn caller_slice_plain_calls(conn: &Connection, path: &str, key: &str, stamp: u64) -> Option<bool> {
    let (flag, complete): (Option<bool>, Option<bool>) = conn
        .prepare_cached(
            "SELECT (SELECT plain_calls FROM caller_slice
                      WHERE path = ?1 AND key = ?2 AND stamp = ?3 AND extract_version = ?4),
                    (SELECT complete FROM caller_slice
                      WHERE path = ?1 AND key = '' AND stamp = ?3 AND extract_version = ?4)",
        )
        .ok()?
        .query_row(params![path, key, stamp as i64, EXTRACT_VERSION], |row| Ok((row.get(0)?, row.get(1)?)))
        .ok()?;
    flag.or_else(|| complete.filter(|&c| c).map(|_| false))
}

/// Write `rows` in one transaction.
pub fn save_caller_slices(conn: &Connection, rows: &[CallerSliceRow]) -> rusqlite::Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    match put_caller_slices(conn, rows) {
        Ok(()) => conn.execute_batch("COMMIT"),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Write `rows` inside the caller's transaction. A frame row also drops the
/// file's rows at any other stamp: they can never be read again.
pub fn put_caller_slices(conn: &Connection, rows: &[CallerSliceRow]) -> rusqlite::Result<()> {
    let mut stale = conn.prepare_cached("DELETE FROM caller_slice WHERE path = ?1 AND stamp != ?2")?;
    let mut put = conn.prepare_cached(
        "INSERT OR REPLACE INTO caller_slice (path, key, stamp, extract_version, body, plain_calls, complete) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for r in rows {
        if r.key == FRAME_KEY {
            stale.execute(params![r.path, r.stamp as i64])?;
        }
        put.execute(params![r.path, r.key, r.stamp as i64, EXTRACT_VERSION, r.body, r.plain_calls, r.complete])?;
    }
    Ok(())
}

/// Every row of `fa` — its frame and each call name's slice — cut while the
/// indexer holds the whole analysis, so no caller read decodes the file's
/// bag to cut one. Empty for a gated view (rebuilt per fired set) or an
/// unstamped analysis, which readers cut and never keep.
pub fn caller_slice_rows(path: &str, fa: &crate::model::file_analysis::FileAnalysis) -> Vec<CallerSliceRow> {
    use crate::model::witnesses::caller_slice::{CallerFrame, CallerSlice};
    let stamp = fa.source_stamp;
    if stamp == 0 || !fa.plugin.gated_emissions.is_empty() {
        return Vec::new();
    }
    let frame = CallerFrame::of(fa);
    let mut rows = Vec::new();
    if let Some(body) = encode_slice_body(&frame) {
        rows.push(CallerSliceRow { complete: true, ..CallerSliceRow::frame(path.to_string(), stamp, body) });
    }
    for (key, slice) in CallerSlice::all_of(fa, &frame) {
        if let Some(body) = encode_slice_body(&slice) {
            rows.push(CallerSliceRow::slice(path.to_string(), key, stamp, body, &slice));
        }
    }
    rows
}
