//! The relational row store (`files`/`strings`/`refs`/`syms`):
//! shredding, invalidation, chunked deferred writes, and the retrieval
//! views (ref candidates, workspace/symbol rows, dead exports).

use super::*;

/// String-intern cache for the shredder, held for the WRITER's lifetime
/// rather than rebuilt per file.
///
/// Interning was two statements per name per file, and a cold corpus index
/// re-interned the same few thousand names (`$self`, `@_`, `new`) across
/// every file — millions of round trips against the unique index, all
/// redundant. Keyed by `strings_generation` so a `clear_derived_rows` (or a
/// row-format rebuild) that empties `strings` can never leave a cached
/// `str_id` pointing at a row that is gone: a dangling `name_id` writes rows
/// that no retrieval can ever match, which fails silently rather than loudly.
///
/// Thread-local because the bulk indexers shred from Rayon workers; each
/// keeps its own cache and they converge on the same ids.
struct InternMemo {
    generation: i64,
    map: std::collections::HashMap<String, i64>,
}

/// Entry cap. The redundancy this memo removes is concentrated in a few
/// thousand names (`$self`, `@_`, `new`) repeated across every file; the
/// long tail is interned once anyway and caching it buys nothing. Left
/// unbounded, a worker thread would accumulate the corpus's whole unique-name
/// set — 556k strings at 138k files, times a Rayon worker each — so it is
/// capped rather than byte-accounted, per the residency discipline that says
/// a derived cache must be bounded by construction.
const INTERN_MEMO_CAP: usize = 32_768;

impl InternMemo {
    fn reset_if_stale(&mut self, generation: i64) {
        if self.generation != generation {
            self.map.clear();
            self.generation = generation;
        }
    }

    fn remember(&mut self, s: &str, id: i64) {
        // Clear-on-overflow rather than an LRU: the hot set re-warms within a
        // few files, and eviction bookkeeping would cost more than the misses.
        if self.map.len() >= INTERN_MEMO_CAP {
            self.map.clear();
        }
        self.map.insert(s.to_string(), id);
    }
}

thread_local! {
    static INTERN_MEMO: std::cell::RefCell<InternMemo> = std::cell::RefCell::new(InternMemo {
        // No real generation is negative, so the first shred always resets.
        generation: -1,
        map: std::collections::HashMap::new(),
    });
}

/// Replace one file's derived rows — refs AND symbols — in the relational
/// index. One function so both families are the same generation by
/// construction (`files` presence is the single "already shredded" marker;
/// a marker per family would let them drift). Runs inside the caller's
/// transaction when one is open (bulk drains wrap N files per `BEGIN`);
/// standalone callers get per-statement autocommit, which is fine for
/// single-file updates. Upserts the `files` row even for an empty file.
pub fn shred_derived_rows(
    conn: &Connection,
    path: &str,
    source: &str,
    seeds: &[crate::model::file_analysis::RefRowSeed],
    sym_seeds: &[crate::model::file_analysis::SymRowSeed],
) -> rusqlite::Result<()> {
    // Sticky workspace tier: project lib/ files are inside the walk AND on
    // @INC (add_project_lib_paths), so the resolver re-shreds them as
    // 'import'. The walk's verdict wins — downgrading would let the @INC
    // hard-clear take an editable file's generation out from under its
    // stripped resident copy.
    conn.execute(
        "INSERT INTO files (path, source) VALUES (?1, ?2)
         ON CONFLICT(path) DO UPDATE SET source =
           CASE WHEN files.source = 'workspace' THEN 'workspace' ELSE excluded.source END",
        params![path, source],
    )?;
    let file_id: i64 = conn.query_row(
        "SELECT file_id FROM files WHERE path = ?1",
        params![path],
        |row| row.get(0),
    )?;
    conn.execute("DELETE FROM refs WHERE file_id = ?1", params![file_id])?;
    conn.execute("DELETE FROM syms WHERE file_id = ?1", params![file_id])?;

    let generation = crate::index::module_cache::strings_generation(conn);
    INTERN_MEMO.with(|cell| -> rusqlite::Result<()> {
        let mut memo = cell.borrow_mut();
        memo.reset_if_stale(generation);

        let mut intern = conn.prepare_cached("INSERT OR IGNORE INTO strings (s) VALUES (?1)")?;
        let mut lookup = conn.prepare_cached("SELECT str_id FROM strings WHERE s = ?1")?;
        // SELECT first. `INSERT OR IGNORE` for a name already interned is a
        // write statement that does nothing, and at corpus scale almost every
        // name is already there — `$self` was re-interned once per FILE.
        let mut intern_str = |s: &str, memo: &mut InternMemo| -> rusqlite::Result<i64> {
            if let Some(id) = memo.map.get(s) {
                return Ok(*id);
            }
            let id: i64 = match lookup.query_row(params![s], |row| row.get(0)) {
                Ok(id) => id,
                Err(_) => {
                    intern.execute(params![s])?;
                    lookup.query_row(params![s], |row| row.get(0))?
                }
            };
            memo.remember(s, id);
            Ok(id)
        };

        // Only the (name, file) PAIR is stored, so a file's thousands of
        // `$self` refs are ONE row. Deduping HERE rather than leaning on the
        // primary key's conflict handling is the point: it collapses the
        // statement count along with the row count, and the statements were
        // the write pressure.
        let mut insert = conn.prepare_cached(
            // OR IGNORE, not a bare INSERT: the per-file DELETE above should
            // leave every pair fresh, but a duplicate would otherwise abort
            // the whole chunk transaction and take other files' rows with it.
            "INSERT OR IGNORE INTO refs (name_id, file_id) VALUES (?1, ?2)",
        )?;
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for seed in seeds {
            if !seen.insert(seed.key.as_str()) {
                continue;
            }
            let name_id = intern_str(&seed.key, &mut memo)?;
            insert.execute(params![name_id, file_id])?;
        }
        drop(insert);

        // Symbols keep their full row shape — every column has a reader
        // (`sym_rows_matching`, the dead-export view, workspace/symbol).
        let mut insert_sym = conn.prepare_cached(
            "INSERT INTO syms (file_id, name_id, key_id, kind, start_row, start_col, end_row,
                               end_col, container_id, flags)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        for seed in sym_seeds {
            let name_id = intern_str(&seed.name, &mut memo)?;
            // A symbol row carries BOTH names: `name_id` is what it is called
            // (`Mojolicious::Sessions` — what workspace/symbol searches and
            // reports), `key_id` is what a REFERENCE to it is keyed by
            // (`Sessions` — `Ref::match_key` keeps only the last segment).
            // Retrieval probes the key, so a row that stored only the display
            // name was undiscoverable for every qualified symbol: a package's
            // own declaration could not be found through the `syms` union that
            // exists to make declaration-only files candidates.
            let key = crate::model::file_analysis::name_match_key(&seed.name);
            let key_id = if key == seed.name {
                name_id
            } else {
                intern_str(&key, &mut memo)?
            };
            let container_id = match seed.container.as_deref() {
                Some(c) => Some(intern_str(c, &mut memo)?),
                None => None,
            };
            insert_sym.execute(params![
                file_id,
                name_id,
                key_id,
                seed.kind,
                seed.span.start.row as i64,
                seed.span.start.column as i64,
                seed.span.end.row as i64,
                seed.span.end.column as i64,
                container_id,
                seed.flags,
            ])?;
        }
        Ok(())
    })?;
    Ok(())
}

/// Reclaim interned strings nothing references any more.
///
/// `strings` is append-only in normal operation: every deletion path
/// (`delete_ref_rows`, the tier-scoped eraser, a file re-shredding under a
/// changed name set) drops `refs`/`syms` rows and leaves their names behind.
/// Measured over 300 substrate modules, deleting half the files orphans
/// 34.6% of the table and deleting all of them orphans 100% — the table
/// never shrinks, so a long-lived workspace accumulates every name it has
/// ever seen.
///
/// Shaped as a set-difference, not a per-string existence test: `container_id`
/// carries no index, so a `NOT EXISTS` per string would scan `syms` once per
/// string. This builds the live set in one pass over each table instead.
///
/// Returns the number of strings reclaimed. Bumps `strings_generation` when
/// it reclaims any, because the shredder memoizes `str_id`s for the writer's
/// lifetime and a freed id must never be handed out again — that would write
/// refs rows nothing joins to, the silent failure the generation guard
/// exists for.
///
/// Callers must run this at a point where no shred is mid-flight. SQLite
/// serialises writers, so a shred's intern-and-insert cannot interleave with
/// the delete below; what the generation bump protects is the memo a writer
/// built in an EARLIER transaction.
pub fn gc_strings(conn: &Connection) -> usize {
    let deleted = conn.execute(
        "DELETE FROM strings WHERE str_id NOT IN (
             SELECT name_id FROM refs
             UNION SELECT name_id FROM syms
             UNION SELECT key_id FROM syms
             UNION SELECT container_id FROM syms WHERE container_id IS NOT NULL
         )",
        [],
    );
    match deleted {
        Ok(n) if n > 0 => {
            let _ = crate::index::module_cache::bump_strings_generation(conn);
            n
        }
        _ => 0,
    }
}

/// Drop one file's whole persisted generation — blob row AND derived ref
/// rows, together (the eraser twin of the write invariant "blob + rows
/// describe one generation"). Every invalidation seam calls this; nobody
/// else spells modules-table SQL.
pub fn invalidate_generation(conn: &Connection, path: &str) {
    let _ = conn.execute("DELETE FROM modules WHERE path = ?1", params![path]);
    delete_stub(conn, path);
    delete_ref_rows(conn, path);
    forget_orphaned_derivations(conn, path);
}

/// Drop a path's baked map once no blob is left to have derived it.
///
/// The bake is a derivation of the blob, so it must not outlive it. What an
/// orphaned map RISKS is a wrong answer rather than a slow one — the
/// cross-file primary consults the map before it decodes anything, and an
/// `Outcome::Answer` short-circuits the chase.
///
/// Honest about its own reach: no end-to-end path has been demonstrated that
/// reads an orphaned map. Both routes that produce one re-persist the file
/// (blob and map together) or answer from the open-document tier before any
/// consult reaches the stale row. This closes the invariant, not a reproduced
/// bug — and it is worth closing regardless, because "a derivation outlives
/// its source" is exactly the shape whose consequences are invisible until a
/// future caller order makes them visible.
///
/// Conditioned on the modules row rather than unconditional, because the
/// tier-scoped eraser legitimately leaves one behind: a dual-homed file whose
/// import-tier rows the walk GC'd still has a valid workspace blob, and its
/// map still describes it. Dropping maps for every tier eviction would darken
/// the layer across a whole GC sweep and put the survivors on the repair
/// frontier for nothing.
///
/// Covers BOTH derivations of the blob — the baked map and the projected
/// surface. They are written together by one `encode_analysis` and share this
/// condition exactly, so they are erased together; a surviving surface would
/// be the same "a derivation outlived its source" shape one table over, and
/// the warm lane would adopt a projection of a file the store no longer holds.
///
/// The RAM twin is `ModuleIndex::invalidate_conclusions`; both halves are
/// needed and neither is sufficient.
fn forget_orphaned_derivations(conn: &Connection, path: &str) {
    let still_persisted: bool = conn
        .query_row(
            "SELECT 1 FROM modules WHERE path = ?1 LIMIT 1",
            params![path],
            |_| Ok(()),
        )
        .is_ok();
    if !still_persisted {
        super::forget_conclusions(conn, path);
        super::forget_surface(conn, path);
    }
}

/// Tier-scoped eraser: drops the generation ONLY when its rows carry
/// `source` — the walk's dead-row GC must not take a dual-homed file's
/// import-tier generation (project-lib files leave the walk when
/// gitignored but stay valid @INC modules).
pub fn invalidate_generation_tier(conn: &Connection, path: &str, source: &str) {
    let _ = conn.execute(
        "DELETE FROM modules WHERE path = ?1 AND source = ?2",
        params![path, source],
    );
    // Stubs are workspace-tier only — an import-tier invalidation must not
    // orphan a dual-homed file's still-valid workspace stub.
    if source == "workspace" {
        delete_stub(conn, path);
    }
    let _ = conn.execute(
        "DELETE FROM refs WHERE file_id IN
           (SELECT file_id FROM files WHERE path = ?1 AND source = ?2)",
        params![path, source],
    );
    let _ = conn.execute(
        "DELETE FROM syms WHERE file_id IN
           (SELECT file_id FROM files WHERE path = ?1 AND source = ?2)",
        params![path, source],
    );
    let _ = conn.execute(
        "DELETE FROM files WHERE path = ?1 AND source = ?2",
        params![path, source],
    );
    forget_orphaned_derivations(conn, path);
}

/// Remove a deleted file's rows (the removal half of `shred_derived_rows`).
pub fn delete_ref_rows(conn: &Connection, path: &str) {
    let _ = conn.execute(
        "DELETE FROM refs WHERE file_id IN (SELECT file_id FROM files WHERE path = ?1)",
        params![path],
    );
    let _ = conn.execute(
        "DELETE FROM syms WHERE file_id IN (SELECT file_id FROM files WHERE path = ?1)",
        params![path],
    );
    let _ = conn.execute("DELETE FROM files WHERE path = ?1", params![path]);
}

/// Has `path` been shredded into the relational index? (`files` presence is
/// the marker — `shred_derived_rows` upserts it even for empty files.)
#[cfg(test)]
pub fn has_ref_rows(conn: &Connection, path: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM files WHERE path = ?1",
        params![path],
        |_| Ok(()),
    )
    .is_ok()
}

/// The retrieval half: every indexed file containing at least one ref row
/// whose match key is one of `keys` — the candidate-file set `refs_to`'s
/// SQL arms rehydrate and run the (unchanged) matcher over.
pub fn ref_candidate_files(conn: &Connection, keys: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    // Usage candidacy comes from ref rows; DECLARATION candidacy from sym
    // rows — a file that declares `helper` but never mentions it again has
    // no matching ref row, and without the union the backward walk's
    // matcher (whose declaration half reads symbols) never rehydrates it.
    let Ok(mut stmt) = conn.prepare_cached(
        "SELECT DISTINCT f.path FROM refs r
           JOIN files f ON f.file_id = r.file_id
          WHERE r.name_id = (SELECT str_id FROM strings WHERE s = ?1)
         UNION
         SELECT DISTINCT f.path FROM syms y
           JOIN files f ON f.file_id = y.file_id
          WHERE y.key_id = (SELECT str_id FROM strings WHERE s = ?1)",
    ) else {
        return out;
    };
    let mut seen = std::collections::HashSet::new();
    for key in keys {
        let rows = stmt.query_map(params![key], |row| row.get::<_, String>(0));
        if let Ok(rows) = rows {
            for r in rows {
                match r {
                    Ok(p) => {
                        if seen.insert(p.clone()) {
                            out.push(p);
                        }
                    }
                    // A step-level error (corrupt page, IO) ends the scan —
                    // the candidate list is TRUNCATED, which reads as
                    // "fewer references" with no other witness. Say so.
                    Err(e) => {
                        log::warn!("ref candidate scan aborted mid-iteration: {}", e);
                        break;
                    }
                }
            }
        }
    }
    out
}

/// One workspace/symbol row hit: (path, name, kind code, selection span,
/// container, flags). The caller applies the adapter's kind/flag filters
/// and skips paths a fresher resident copy already answered.
pub struct SymRowHit {
    pub path: String,
    pub name: String,
    pub kind: u8,
    pub start_row: usize,
    pub start_col: usize,
    pub end_row: usize,
    pub end_col: usize,
    pub container: Option<String>,
    pub flags: u8,
}

/// The rows-backed workspace/symbol scan: every WORKSPACE-tier symbol row
/// whose name contains `query`, case-insensitively — the same containment
/// test the resident sweep applies. The `files.source` filter keeps the
/// import tier (@INC deps) out: the resident sweeps never enumerated it,
/// and folding it in would flood project searches with CPAN internals.
pub fn sym_rows_matching(conn: &Connection, query: &str) -> Vec<SymRowHit> {
    let mut out = Vec::new();
    // SQLite LIKE is case-insensitive for ASCII only; the resident sweep
    // lowercases with full Unicode semantics. ASCII queries (the hot path)
    // stay an indexed-ish LIKE; a non-ASCII query walks the name strings
    // with the SAME Rust containment test so an evicted file's `sub Übung`
    // matches exactly like a resident one.
    if !query.is_ascii() {
        let Ok(mut stmt) = conn.prepare_cached(
            "SELECT f.path, n.s, y.kind, y.start_row, y.start_col, y.end_row, y.end_col,
                    c.s, y.flags
               FROM syms y
               JOIN files f ON f.file_id = y.file_id
               JOIN strings n ON n.str_id = y.name_id
               LEFT JOIN strings c ON c.str_id = y.container_id
              WHERE f.source = 'workspace'",
        ) else {
            return out;
        };
        let q = query.to_lowercase();
        let rows = stmt.query_map([], |row| {
            Ok(SymRowHit {
                path: row.get(0)?,
                name: row.get(1)?,
                kind: row.get::<_, i64>(2)? as u8,
                start_row: row.get::<_, i64>(3)? as usize,
                start_col: row.get::<_, i64>(4)? as usize,
                end_row: row.get::<_, i64>(5)? as usize,
                end_col: row.get::<_, i64>(6)? as usize,
                container: row.get(7)?,
                flags: row.get::<_, i64>(8)? as u8,
            })
        });
        if let Ok(rows) = rows {
            for r in rows {
                match r {
                    Ok(hit) => {
                        if hit.name.to_lowercase().contains(&q) {
                            out.push(hit);
                        }
                    }
                    Err(e) => {
                        log::warn!("sym row scan aborted mid-iteration: {}", e);
                        break;
                    }
                }
            }
        }
        return out;
    }
    let Ok(mut stmt) = conn.prepare_cached(
        "SELECT f.path, n.s, y.kind, y.start_row, y.start_col, y.end_row, y.end_col,
                c.s, y.flags
           FROM syms y
           JOIN files f ON f.file_id = y.file_id
           JOIN strings n ON n.str_id = y.name_id
           LEFT JOIN strings c ON c.str_id = y.container_id
          WHERE f.source = 'workspace'
            AND n.s LIKE '%' || ?1 || '%' ESCAPE '\\'",
    ) else {
        return out;
    };
    // LIKE wildcards in the user's query are literals, not patterns.
    let escaped = query.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
    let rows = stmt.query_map(params![escaped], |row| {
        Ok(SymRowHit {
            path: row.get(0)?,
            name: row.get(1)?,
            kind: row.get::<_, i64>(2)? as u8,
            start_row: row.get::<_, i64>(3)? as usize,
            start_col: row.get::<_, i64>(4)? as usize,
            end_row: row.get::<_, i64>(5)? as usize,
            end_col: row.get::<_, i64>(6)? as usize,
            container: row.get(7)?,
            flags: row.get::<_, i64>(8)? as u8,
        })
    });
    if let Ok(rows) = rows {
        for r in rows {
            match r {
                Ok(hit) => out.push(hit),
                Err(e) => {
                    log::warn!("sym row scan aborted mid-iteration: {}", e);
                    break;
                }
            }
        }
    }
    out
}

/// Can the row store rule out a member named `name` attributed to container
/// `container` in `path`'s file? Three-valued, and the caller's license to
/// skip a decode hangs on the distinction:
///
/// - `None` — the file was never shredded (`files` presence is the single
///   shredded marker), so the store cannot speak for it. Fail open.
/// - `Some(true)` — a matching sym row exists; the decode is warranted.
/// - `Some(false)` — the file is covered and no row matches: the only
///   verdict that licenses skipping the rehydrate.
///
/// Deliberately kind-blind, and matching `name_id` OR `key_id`: the walk's
/// member test also accepts class-content variables/fields, which rows cannot
/// express — so ANY symbol row under `(name|key, container)` forces the
/// decode. Over-approximation toward the decode is the sound direction: a
/// wasted rehydrate, never a hidden member
/// (`docs/prompt-relational-iteration.md`).
pub fn sym_member_row_exists(
    conn: &Connection,
    path: &str,
    name: &str,
    container: &str,
) -> Option<bool> {
    let file_id: i64 = conn
        .prepare_cached("SELECT file_id FROM files WHERE path = ?1")
        .ok()?
        .query_row(params![path], |row| row.get(0))
        .ok()?;
    let norm = probe_spelling(name);
    // A name or container the strings table never interned yields NULL from
    // the subselect, the comparison is false, and EXISTS answers 0 — which is
    // correct: no row can reference a string that was never stored. The
    // container stays EXACT-match: it is a package name, and its match key
    // strips the qualifier, which would let `Base` claim `My::Base`'s rows.
    conn.prepare_cached(
        "SELECT EXISTS(
            SELECT 1 FROM syms y
             WHERE y.file_id = ?1
               AND y.container_id = (SELECT str_id FROM strings WHERE s = ?4)
               AND (y.name_id IN (SELECT str_id FROM strings WHERE s IN (?2, ?3))
                    OR y.key_id IN (SELECT str_id FROM strings WHERE s IN (?2, ?3))))",
    )
    .ok()?
    .query_row(params![file_id, name, norm, container], |row| row.get(0))
    .ok()
}

/// The store's spelling policy, in ONE place: callers pass the RAW name and
/// every per-file probe also matches its match-key normalization, because
/// refs rows are keyed by `Ref::match_key()` while syms rows carry both the
/// raw symbol name and its key. A caller threading spellings itself is the
/// bug this replaces — a qualified query name (`My::Pkg::helper`) probed raw
/// would miss the `helper`-keyed row and turn fail-open into a wrong skip.
fn probe_spelling(name: &str) -> String {
    crate::model::file_analysis::name_match_key(name)
}

/// The name-only sibling of `sym_member_row_exists`: can the store rule out
/// ANY symbol named `name` (under any container) in `path`'s file? Same
/// three-valued contract — `Some(false)` is the only skip license; `None`
/// (never shredded) must stay distinguishable from it.
///
/// For the bridged-entity walk: a plugin namespace's entities are standard
/// symbols of THEIR file, but their container is the plugin's canonical home
/// package — not the bridged class — so the (name, container) probe cannot
/// serve that walk and a container-blind one can. Over-approximation is
/// still toward the decode.
pub fn sym_name_row_exists(conn: &Connection, path: &str, name: &str) -> Option<bool> {
    let file_id: i64 = conn
        .prepare_cached("SELECT file_id FROM files WHERE path = ?1")
        .ok()?
        .query_row(params![path], |row| row.get(0))
        .ok()?;
    let norm = probe_spelling(name);
    conn.prepare_cached(
        "SELECT EXISTS(
            SELECT 1 FROM syms y
             WHERE y.file_id = ?1
               AND (y.name_id IN (SELECT str_id FROM strings WHERE s IN (?2, ?3))
                    OR y.key_id IN (SELECT str_id FROM strings WHERE s IN (?2, ?3))))",
    )
    .ok()?
    .query_row(params![file_id, name, norm], |row| row.get(0))
    .ok()
}

/// The widest per-file mention probe: can the store rule out ANY row —
/// ref (use-site) or sym (declaration) — named `name` in `path`'s file?
/// Same three-valued contract as its siblings; `Some(false)` is the only
/// skip license, `None` (never shredded) must stay distinguishable.
///
/// For the registry consult pre-filter's un-attributed flavor
/// (`SlotType{.., key}`): a slot-type witness is minted from a hash-key
/// WRITE ref, so a file with no ref row for the key provably carries no
/// such witness. The syms half rides along for over-approximation — a
/// wasted decode is the cheap error.
pub fn name_row_exists(conn: &Connection, path: &str, name: &str) -> Option<bool> {
    let file_id: i64 = conn
        .prepare_cached("SELECT file_id FROM files WHERE path = ?1")
        .ok()?
        .query_row(params![path], |row| row.get(0))
        .ok()?;
    let norm = probe_spelling(name);
    conn.prepare_cached(
        "SELECT EXISTS(
            SELECT 1 FROM refs r
             WHERE r.name_id IN (SELECT str_id FROM strings WHERE s IN (?2, ?3))
               AND r.file_id = ?1)
         OR EXISTS(
            SELECT 1 FROM syms y
             WHERE y.file_id = ?1
               AND (y.name_id IN (SELECT str_id FROM strings WHERE s IN (?2, ?3))
                    OR y.key_id IN (SELECT str_id FROM strings WHERE s IN (?2, ?3))))",
    )
    .ok()?
    .query_row(params![file_id, name, norm], |row| row.get(0))
    .ok()
}

/// How many FILES carry a ref row for one match key.
///
/// Deliberately not called a reference count: rows are `(name_id, file_id)`
/// pairs, so this is a CANDIDATE count — how many files the backward walk
/// would rehydrate — and it is an over-approximation of the answer by
/// construction. That over-approximation is load-bearing (`unused_exported_-
/// syms` is sound only because absence means zero, while presence means
/// "maybe"), so nothing may present this as "how many times X is used".
#[cfg(test)]
pub fn ref_candidate_file_count(conn: &Connection, key: &str) -> u64 {
    conn.query_row(
        "SELECT COUNT(*) FROM refs
          WHERE name_id = (SELECT str_id FROM strings WHERE s = ?1)",
        params![key],
        |row| row.get::<_, i64>(0),
    )
    .map(|n| n as u64)
    .unwrap_or(0)
}

/// Deferred-write chunking: N items per `BEGIN IMMEDIATE`…`COMMIT`, the
/// SQLITE_BUSY_SNAPSHOT-safe shape every post-scan backfill shares (writing
/// inside a streaming SELECT's snapshot turns a concurrent commit into an
/// unretried BUSY_SNAPSHOT abort). A failed txn OPEN abandons the remaining
/// queue (the writer is likely gone); a failed COMMIT rolls back and keeps
/// going — later chunks may land.
pub fn write_in_chunks<T>(
    conn: &Connection,
    items: &[T],
    chunk_size: usize,
    label: &str,
    per_item: impl Fn(&Connection, &T),
) {
    for chunk in items.chunks(chunk_size) {
        if conn.execute_batch("BEGIN IMMEDIATE").is_err() {
            log::error!("{label}: txn open failed; remaining items defer to next warm");
            break;
        }
        for item in chunk {
            per_item(conn, item);
        }
        if let Err(e) = conn.execute_batch("COMMIT") {
            log::error!("{label}: commit failed: {}", e);
            let _ = conn.execute_batch("ROLLBACK");
        }
    }
}

/// Total SOURCE bytes of every persisted analysis (all tiers — a sweep
/// rehydrates @INC providers alongside workspace files). The scale signal
/// `cache_policy` sizes a batch sweep's rehydration cache from: persisted
/// at index time, so a warm start reads it for free.
pub fn persisted_source_bytes(conn: &Connection) -> u64 {
    conn.query_row(
        "SELECT COALESCE(SUM(file_size), 0) FROM modules",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n.max(0) as u64)
    .unwrap_or(0)
}

/// Every path that currently has shredded derived rows — the bulk twin of
/// `has_ref_rows` for warm scans (one query instead of one per file).
pub fn paths_with_ref_rows(conn: &Connection) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Ok(mut stmt) = conn.prepare("SELECT path FROM files") else {
        return out;
    };
    if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
        for p in rows.flatten() {
            out.insert(p);
        }
    }
    out
}

/// Every DISTINCT name key present in the `refs` table. The general
/// pre-prune set for `--heatmap`'s per-declaration references projection: a
/// declaration whose name key is ABSENT here has no reference row in any
/// indexed file, so — because rows over-approximate references — the
/// projection is provably empty and the walk can be skipped. Retrieval only;
/// the caller owns the coverage decision (trust "absent ⇒ zero references"
/// only when the store actually covers the files the walk would scan).
pub fn names_with_ref_rows(conn: &Connection) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Ok(mut stmt) =
        conn.prepare("SELECT DISTINCT s.s FROM refs r JOIN strings s ON s.str_id = r.name_id")
    else {
        return out;
    };
    if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
        for n in rows.flatten() {
            out.insert(n);
        }
    }
    out
}

/// One unused-exported symbol row: an `@EXPORT`/`@EXPORT_OK` name with no
/// reference row in any OTHER file. Carries the identity the caller matches a
/// reported symbol against — path + name + the selection-span start.
pub struct DeadExportRow {
    pub path: String,
    pub name: String,
    pub start_row: usize,
    pub start_col: usize,
}

/// The unused-exports view (`docs/adr/relational-ref-index.md`): every
/// WORKSPACE-tier symbol row flagged exported (`SymRowSeed::FLAG_EXPORTED`)
/// whose name key has ZERO ref rows in any OTHER file. Same-file refs are
/// excluded on purpose — a module calling its own exported sub does not make
/// that export live for a *consumer*.
///
/// The result is SOUND IN EXACTLY ONE DIRECTION, and the asymmetry is the
/// point. Ref rows are name-match CANDIDATES — an over-approximation of real
/// references, since the per-`RefKind` matcher still runs per row — so:
///   * zero cross-file candidate rows ⇒ no cross-file reference can exist ⇒
///     the export is TRULY unused by any consumer (a sound "dead export").
///   * one or more candidate rows ⇒ UNKNOWN: a candidate may or may not
///     survive the matcher. Never read this as "used".
/// The right polarity for a dead-export review queue: it never fabricates a
/// dead export; at worst it MISSES one whose sole consumer's candidate row
/// would not have survived the matcher.
pub fn unused_exported_syms(conn: &Connection) -> Vec<DeadExportRow> {
    let mut out = Vec::new();
    let Ok(mut stmt) = conn.prepare(
        "SELECT f.path, n.s, y.start_row, y.start_col
           FROM syms y
           JOIN files f ON f.file_id = y.file_id
           JOIN strings n ON n.str_id = y.name_id
          WHERE f.source = 'workspace'
            AND (y.flags & ?1) != 0
            AND NOT EXISTS (
                  -- Against the KEY: refs are keyed by `Ref::match_key`, so
                  -- comparing them to a symbol's display name is what let the
                  -- two families drift apart in the first place. A no-op for
                  -- the rows this view gates on (an `@EXPORT` name is bare, so
                  -- key and name are the same string) — it is here so the next
                  -- reader cannot reintroduce the mismatch.
                  SELECT 1 FROM refs r
                   WHERE r.name_id = y.key_id
                     AND r.file_id != y.file_id
                )",
    ) else {
        return out;
    };
    let flag = crate::model::file_analysis::SymRowSeed::FLAG_EXPORTED as i64;
    let rows = stmt.query_map(params![flag], |row| {
        Ok(DeadExportRow {
            path: row.get(0)?,
            name: row.get(1)?,
            start_row: row.get::<_, i64>(2)? as usize,
            start_col: row.get::<_, i64>(3)? as usize,
        })
    });
    if let Ok(rows) = rows {
        for r in rows {
            match r {
                Ok(hit) => out.push(hit),
                Err(e) => {
                    log::warn!("unused-exports scan aborted mid-iteration: {}", e);
                    break;
                }
            }
        }
    }
    out
}
