//! Persisted engine answers (`model::witnesses::engine`) as a graph: one
//! `answer_nodes` row per answer, its reads in `answer_reads`, the answers it
//! consulted in `answer_kids`. Keys and values are the engine's own
//! encodings; validity is judged by the engine against the index that loads
//! them — the store only drops rows written under another extractor.
//!
//! The reads are rows, not a blob, so "which answers read this file" is one
//! indexed query (`answers_reading`).

use rusqlite::{params, Connection};

use crate::model::witnesses::engine::{PersistedNode, PersistedRead};

use super::EXTRACT_VERSION;

/// Every node written under the current extractor, with its encoded key.
pub fn load_answers(conn: &Connection) -> Vec<(Vec<u8>, PersistedNode)> {
    let mut nodes: std::collections::HashMap<Vec<u8>, PersistedNode> = std::collections::HashMap::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT key, value, value_hash, world FROM answer_nodes WHERE extract_version = ?1",
    ) else {
        return Vec::new();
    };
    let rows = stmt.query_map(params![EXTRACT_VERSION], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    });
    for (key, value, hash, world) in rows.into_iter().flatten().flatten() {
        let node = PersistedNode { value, value_hash: hash as u64, world: world as u64, reads: Vec::new(), kids: Vec::new() };
        nodes.insert(key, node);
    }
    if let Ok(mut stmt) = conn.prepare("SELECT key, kind, subject, stamp FROM answer_reads") {
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, i64>(1)?, row.get::<_, Vec<u8>>(2)?, row.get::<_, i64>(3)?))
        });
        for (key, kind, subject, stamp) in rows.into_iter().flatten().flatten() {
            let (Some(node), Some(read)) = (nodes.get_mut(&key), PersistedRead::from_row(kind, subject, stamp)) else {
                continue;
            };
            node.reads.push(read);
        }
    }
    if let Ok(mut stmt) = conn.prepare("SELECT key, child, child_hash FROM answer_kids") {
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, Vec<u8>>(1)?, row.get::<_, i64>(2)?))
        });
        for (key, child, hash) in rows.into_iter().flatten().flatten() {
            if let Some(node) = nodes.get_mut(&key) {
                node.kids.push((child, hash as u64));
            }
        }
    }
    nodes.into_iter().collect()
}

/// Upsert `nodes` in one transaction, replacing each key's reads and kids.
pub fn save_answers(conn: &Connection, nodes: &[(Vec<u8>, PersistedNode)]) -> rusqlite::Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE")?;
    let result = (|| {
        let mut node_stmt = conn.prepare(
            "INSERT OR REPLACE INTO answer_nodes (key, extract_version, value, value_hash, world) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        let mut clear_reads = conn.prepare("DELETE FROM answer_reads WHERE key = ?1")?;
        let mut clear_kids = conn.prepare("DELETE FROM answer_kids WHERE key = ?1")?;
        let mut read_stmt =
            conn.prepare("INSERT INTO answer_reads (key, kind, subject, stamp) VALUES (?1, ?2, ?3, ?4)")?;
        let mut kid_stmt = conn.prepare("INSERT INTO answer_kids (key, child, child_hash) VALUES (?1, ?2, ?3)")?;
        for (k, n) in nodes {
            node_stmt.execute(params![k, EXTRACT_VERSION, n.value, n.value_hash as i64, n.world as i64])?;
            clear_reads.execute(params![k])?;
            clear_kids.execute(params![k])?;
            for r in &n.reads {
                let Some((kind, subject, stamp)) = r.to_row() else { continue };
                read_stmt.execute(params![k, kind, subject, stamp])?;
            }
            for (child, hash) in &n.kids {
                kid_stmt.execute(params![k, child, *hash as i64])?;
            }
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

/// The encoded keys of every persisted answer that read `path`.
pub fn answers_reading(conn: &Connection, path: &std::path::Path) -> Vec<Vec<u8>> {
    let Ok(mut stmt) = conn.prepare("SELECT key FROM answer_reads WHERE kind = ?1 AND subject = ?2") else {
        return Vec::new();
    };
    let subject = path.as_os_str().as_encoded_bytes();
    stmt.query_map(params![crate::model::witnesses::engine::READ_FILE, subject], |row| row.get(0))
        .map(|rows| rows.filter_map(Result::ok).collect())
        .unwrap_or_default()
}
