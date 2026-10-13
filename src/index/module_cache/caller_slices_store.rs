//! Persisted caller slices (`model::witnesses::caller_slice`): one row per
//! (file, call name), plus the file's frame under the empty key. A row holds
//! for the source text it was cut from (`stamp`) under the extractor that
//! cut it; a reader asks for the stamp it holds, so a stale row is never
//! served and needs no deletion to stop being read.

use rusqlite::{params, Connection, OptionalExtension};

use super::EXTRACT_VERSION;

const ZSTD_LEVEL: i32 = 1;

/// The key a file's frame is stored under.
pub const FRAME_KEY: &str = "";

pub fn encode_slice_body<T: serde::Serialize>(v: &T) -> Option<Vec<u8>> {
    let bin = bincode::serialize(v).ok()?;
    zstd::encode_all(bin.as_slice(), ZSTD_LEVEL).ok()
}

pub fn decode_slice_body<T: serde::de::DeserializeOwned>(blob: &[u8]) -> Option<T> {
    let bin = zstd::decode_all(blob).ok()?;
    bincode::deserialize(&bin).ok()
}

/// The row for `(path, key)` cut from the text stamped `stamp`.
pub fn load_caller_slice(conn: &Connection, path: &str, key: &str, stamp: u64) -> Option<Vec<u8>> {
    conn.query_row(
        "SELECT body FROM caller_slices WHERE path = ?1 AND key = ?2 AND stamp = ?3 AND extract_version = ?4",
        params![path, key, stamp as i64, EXTRACT_VERSION],
        |row| row.get::<_, Vec<u8>>(0),
    )
    .optional()
    .ok()
    .flatten()
}

/// One file's rows: `(path, stamp, rows)`.
pub type CallerSliceRows = (String, u64, Vec<(String, Vec<u8>)>);

/// Replace each file's rows with `files`' rows, in one transaction. A file
/// is written whole (frame and every name) so a frame at a stamp means
/// every name the file calls has its row at that stamp.
pub fn save_caller_slices(conn: &Connection, files: &[CallerSliceRows]) -> rusqlite::Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        let mut clear = conn.prepare("DELETE FROM caller_slices WHERE path = ?1")?;
        let mut put = conn.prepare(
            "INSERT OR REPLACE INTO caller_slices (path, key, stamp, extract_version, body) VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (path, stamp, rows) in files {
            clear.execute(params![path])?;
            for (key, body) in rows {
                put.execute(params![path, key, *stamp as i64, EXTRACT_VERSION, body])?;
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("COMMIT"),
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Replace one name's row (a widened cut) when the file's rows are still
/// at `stamp`.
pub fn save_widened_caller_slice(conn: &Connection, path: &str, key: &str, stamp: u64, body: &[u8]) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE caller_slices SET body = ?4 WHERE path = ?1 AND key = ?2 AND stamp = ?3",
        params![path, key, stamp as i64, body],
    )?;
    Ok(())
}
