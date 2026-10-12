//! Persisted engine answers (`model::witnesses::engine`). Rows are opaque
//! to the store: the key and the answer are the engine's own encodings, and
//! validity is judged by the engine against the index that loads them —
//! the store only drops rows written under another extractor.

use rusqlite::{params, Connection};

use super::EXTRACT_VERSION;

/// Every answer row written under the current extractor, as `(key, answer)`.
pub fn load_answers(conn: &Connection) -> Vec<(Vec<u8>, Vec<u8>)> {
    let Ok(mut stmt) = conn.prepare("SELECT key, answer FROM answers WHERE extract_version = ?1") else {
        return Vec::new();
    };
    stmt.query_map(params![EXTRACT_VERSION], |row| Ok((row.get(0)?, row.get(1)?)))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

/// Upsert `rows` in one transaction.
pub fn save_answers(conn: &Connection, rows: &[(Vec<u8>, Vec<u8>)]) -> rusqlite::Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        let mut stmt = conn.prepare(
            "INSERT OR REPLACE INTO answers (key, extract_version, answer) VALUES (?1, ?2, ?3)",
        )?;
        for (k, a) in rows {
            stmt.execute(params![k, EXTRACT_VERSION, a])?;
        }
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
