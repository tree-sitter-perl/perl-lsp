//! Every SQL statement run against a module-cache database, in one registry.
//!
//! A statement that builds a scratch b-tree per execution (an `IN` list, a
//! `UNION`, a `DISTINCT` or `ORDER BY` no index serves) creates and destroys
//! it under SQLite's PROCESS-GLOBAL page-cache mutex, and parallel sweep
//! workers serialize on that mutex: per-file probes shaped that way cost FHEM
//! `--check` 515 s of system time against 3.5 s once reshaped. Nothing at the
//! call site shows the shape, so `registered_statements_plan_without_scratch`
//! EXPLAINs every entry here, and `layering_tests::sql_runs_only_registered_
//! statements` fails any call that runs SQL from anywhere else.
//!
//! `Sql` is only constructible in this module, so a function taking one (the
//! row probes' `run_probe`) can only be handed a registered statement.

/// Registered SQL text. Read it with `registered()` at the rusqlite call.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Sql(&'static str);

impl Sql {
    pub(crate) const fn registered(self) -> &'static str {
        self.0
    }
}

/// How the plan test treats an entry.
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum Plan {
    /// Must compile without any scratch opcode — the default for anything on
    /// a per-request, per-file or per-key path.
    NoScratch,
    /// Builds a scratch structure, allowed because it runs once per run or
    /// per invalidation event. The test asserts it really does, so a statement
    /// that stops needing the exemption has to leave it.
    Scratch(&'static str),
    /// Not EXPLAINed: DDL, a multi-statement batch, or a shape probe that is
    /// prepared and never stepped.
    Unplanned(&'static str),
}

#[cfg(test)]
pub(crate) struct Entry {
    pub name: &'static str,
    pub sql: Sql,
    pub plan: Plan,
}

macro_rules! registry {
    ($( $(#[$attr:meta])* $name:ident => $plan:expr, $sql:expr; )*) => {
        $( $(#[$attr])* pub(crate) const $name: Sql = Sql($sql); )*

        #[cfg(test)]
        pub(crate) const REGISTRY: &[Entry] = &[
            $( Entry {
                name: stringify!($name),
                sql: $name,
                plan: {
                    #[allow(unused_imports)]
                    use Plan::*;
                    $plan
                },
            }, )*
        ];
    };
}

// The row probes resolve every spelling through a scalar subquery with `=`,
// never `IN` (an IN list, even of bound literals, materializes an ephemeral
// b-tree per execution). Each (spelling, column) half is its own `EXISTS`,
// never one `OR` inside a WHERE: an OR plans as a scan of the `file_id`
// prefix, while each half alone is a covering point probe on
// `(file_id, name_id|key_id, container_id)`. Binds: `?1` path, `?2` raw name,
// `?3` its match key (NULL when the same spelling), `?4` container. An
// unshredded file returns NO row (fail open); a never-interned string compares
// NULL, so its halves are false. `row_probes_plan_as_index_point_lookups`
// pins the plan, since the planner chooses it.
macro_rules! sid {
    ($n:literal) => {
        concat!("(SELECT str_id FROM strings WHERE s = ", $n, ")")
    };
}
macro_rules! syms_half {
    ($col:literal, $n:literal) => {
        concat!(
            "EXISTS(SELECT 1 FROM syms y WHERE y.file_id = f.file_id AND y.",
            $col, " = ", sid!($n), ")"
        )
    };
    ($col:literal, $n:literal, container) => {
        concat!(
            "EXISTS(SELECT 1 FROM syms y WHERE y.file_id = f.file_id AND y.",
            $col, " = ", sid!($n), " AND y.container_id = ", sid!("?4"), ")"
        )
    };
}
/// A gated emission's name: present in the views whose gate fires, so any
/// probe that licenses skipping a file fails open on it. Kind- and
/// container-blind, since `held` stores presence only.
macro_rules! held_half {
    ($n:literal) => {
        concat!("EXISTS(SELECT 1 FROM held h WHERE h.name_id = ", sid!($n), " AND h.file_id = f.file_id)")
    };
}
macro_rules! refs_half {
    ($n:literal) => {
        concat!("EXISTS(SELECT 1 FROM refs r WHERE r.name_id = ", sid!($n), " AND r.file_id = f.file_id)")
    };
}

registry! {
    // ---- transactions and connection setup ----
    TXN_BEGIN_IMMEDIATE => NoScratch, "BEGIN IMMEDIATE";
    TXN_COMMIT => NoScratch, "COMMIT";
    TXN_ROLLBACK => NoScratch, "ROLLBACK";
    PRAGMA_JOURNAL_WAL => NoScratch, "PRAGMA journal_mode=WAL";
    PRAGMA_SYNC_OFF => NoScratch, "PRAGMA synchronous=OFF";
    PRAGMA_SYNC_NORMAL => NoScratch, "PRAGMA synchronous=NORMAL";
    PRAGMA_SYNC_FULL => NoScratch, "PRAGMA synchronous=FULL";
    PRAGMA_SYNC_EXTRA => NoScratch, "PRAGMA synchronous=EXTRA";
    WAL_CHECKPOINT_TRUNCATE => NoScratch, "PRAGMA wal_checkpoint(TRUNCATE)";

    // ---- schema and generation gates ----
    SCHEMA_CREATE => Unplanned("DDL batch, once per writer open"),
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
        CREATE TABLE IF NOT EXISTS held (
            name_id INTEGER NOT NULL,
            file_id INTEGER NOT NULL,
            PRIMARY KEY (name_id, file_id)
        ) WITHOUT ROWID;
        CREATE INDEX IF NOT EXISTS idx_held_file ON held(file_id);
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
        CREATE INDEX IF NOT EXISTS idx_syms_file_name ON syms(file_id, name_id, container_id);
        CREATE INDEX IF NOT EXISTS idx_syms_file_key ON syms(file_id, key_id, container_id);
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
        CREATE INDEX IF NOT EXISTS idx_modules_name ON modules(module_name);";
    META_GET => NoScratch, "SELECT value FROM meta WHERE key = ?1";
    META_SET => NoScratch, "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)";
    STRINGS_GENERATION_BUMP => NoScratch,
        "INSERT INTO meta (key, value) VALUES ('strings_generation', '1')
         ON CONFLICT(key) DO UPDATE SET value = CAST(CAST(meta.value AS INTEGER) + 1 AS TEXT)";
    // Shape probes: prepared to test which columns exist, never stepped.
    SHAPE_FILES => NoScratch, "SELECT source FROM files LIMIT 1";
    SHAPE_REFS => NoScratch, "SELECT name_id, file_id FROM refs LIMIT 1";
    SHAPE_SYMS => NoScratch, "SELECT flags, key_id FROM syms LIMIT 1";
    SHAPE_HELD => NoScratch, "SELECT name_id, file_id FROM held LIMIT 1";
    SHAPE_REFS_DROPPED_COLUMN => Unplanned("shape probe that must FAIL to prepare on the current schema"),
        "SELECT qual_kind FROM refs LIMIT 1";
    SHAPE_CONCLUSIONS => NoScratch,
        "SELECT source_fingerprint, flush_generation FROM conclusions LIMIT 1";
    ROWS_REBUILD => Unplanned("DDL batch, once per row-format change"),
        "DROP TABLE IF EXISTS refs;
         DROP TABLE IF EXISTS held;
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
         CREATE TABLE held (
            name_id INTEGER NOT NULL,
            file_id INTEGER NOT NULL,
            PRIMARY KEY (name_id, file_id)
         ) WITHOUT ROWID;
         CREATE INDEX idx_held_file ON held(file_id);
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
         CREATE INDEX idx_syms_file_name ON syms(file_id, name_id, container_id);
         CREATE INDEX idx_syms_file_key ON syms(file_id, key_id, container_id);";
    CONCLUSIONS_REBUILD => Unplanned("DDL batch, once per conclusion-row format change"),
        "DROP TABLE IF EXISTS conclusions;
         CREATE TABLE conclusions (
            path               TEXT NOT NULL,
            generation         INTEGER NOT NULL,
            map                BLOB NOT NULL,
            source_fingerprint INTEGER NOT NULL,
            flush_generation   INTEGER NOT NULL,
            PRIMARY KEY (path, generation)
         ) WITHOUT ROWID;";
    MODULES_ADD_DEPS_STAMP => Unplanned("DDL, in-place migration once per writer open"),
        "ALTER TABLE modules ADD COLUMN deps_stamp INTEGER NOT NULL DEFAULT 0;";
    MODULES_ADD_BAG => Unplanned("DDL, in-place migration once per writer open"),
        "ALTER TABLE modules ADD COLUMN bag BLOB;";
    CONCLUSIONS_CREATE_LEGACY => Unplanned("DDL, in-place migration once per writer open"),
        "CREATE TABLE IF NOT EXISTS conclusions (
            path       TEXT NOT NULL,
            generation INTEGER NOT NULL,
            map        BLOB NOT NULL,
            PRIMARY KEY (path, generation)
        ) WITHOUT ROWID;";
    MODULES_DROP => Unplanned("DDL, once per schema-version change"),
        "DROP TABLE IF EXISTS modules;";
    MODULES_CREATE => Unplanned("DDL batch, once per schema-version change"),
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
        CREATE INDEX IF NOT EXISTS idx_modules_name ON modules(module_name);";
    DERIVED_ROWS_CLEAR => Unplanned("multi-statement hard clear, once per generation wipe"),
        "DELETE FROM refs; DELETE FROM syms; DELETE FROM held; DELETE FROM files; DELETE FROM strings; \
         DELETE FROM stubs; DELETE FROM surfaces;";
    MODULES_CLEAR => NoScratch, "DELETE FROM modules";
    MODULES_CLEAR_IMPORT => NoScratch, "DELETE FROM modules WHERE source = 'import'";
    REFS_CLEAR_IMPORT => Scratch("once per @INC change: the IN set drives idx_refs_file per import file instead of probing `files` once per refs row"),
        "DELETE FROM refs WHERE file_id IN (SELECT file_id FROM files WHERE source = 'import')";
    SYMS_CLEAR_IMPORT => Scratch("once per @INC change: the IN set drives the `file_id` index prefix per import file instead of probing `files` once per syms row"),
        "DELETE FROM syms WHERE file_id IN (SELECT file_id FROM files WHERE source = 'import')";
    HELD_CLEAR_IMPORT => Scratch("once per @INC change: the IN set drives idx_held_file per import file instead of probing `files` once per held row"),
        "DELETE FROM held WHERE file_id IN (SELECT file_id FROM files WHERE source = 'import')";
    FILES_CLEAR_IMPORT => NoScratch, "DELETE FROM files WHERE source = 'import'";
    CONCLUSIONS_CLEAR => NoScratch, "DELETE FROM conclusions";
    BUILTINS_CLEAR => NoScratch, "DELETE FROM builtins";
    BUILTINS_INSERT => NoScratch, "INSERT INTO builtins (name, doc) VALUES (?1, ?2)";
    BUILTINS_ALL => NoScratch, "SELECT name, doc FROM builtins";

    // ---- the modules table: blobs ----
    /// Every candidate row for a path, flagged workspace-or-not; the caller
    /// orders them.
    MODULE_LOAD_WITH_BAG => NoScratch,
        "SELECT analysis, mtime_secs, file_size, bag, source = 'workspace' FROM modules \
         WHERE path = ?1 AND extract_version = ?2";
    /// The bagless twin: `bag` is not named, so SQLite never reads its
    /// overflow pages.
    MODULE_LOAD_NO_BAG => NoScratch,
        "SELECT analysis, mtime_secs, file_size, NULL, source = 'workspace' FROM modules \
         WHERE path = ?1 AND extract_version = ?2";
    MODULE_SAVE => NoScratch,
        "INSERT OR REPLACE INTO modules (module_name, path, mtime_secs, file_size, source, analysis, bag, extract_version, deps_stamp)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
    MODULE_DEPS_STAMP_UPDATE => NoScratch,
        "UPDATE modules SET deps_stamp = ?1 WHERE path = ?2";
    MODULES_DELETE_PATH => NoScratch, "DELETE FROM modules WHERE path = ?1";
    MODULES_DELETE_PATH_TIER => NoScratch,
        "DELETE FROM modules WHERE path = ?1 AND source = ?2";
    MODULES_PATH_EXISTS => NoScratch, "SELECT 1 FROM modules WHERE path = ?1 LIMIT 1";
    MODULES_SOURCE_BYTES => NoScratch, "SELECT COALESCE(SUM(file_size), 0) FROM modules";
    WARM_MODULES_BY_SOURCE => NoScratch,
        "SELECT module_name, path, mtime_secs, file_size, analysis, extract_version, deps_stamp \
         FROM modules WHERE source = ?1";
    /// The name-keyed warm lane: the primary key's order, so a name's rows
    /// arrive together.
    WARM_MODULES_BY_NAME => NoScratch,
        "SELECT module_name, path, mtime_secs, file_size, analysis, extract_version, deps_stamp \
         FROM modules WHERE source = ?1 ORDER BY module_name, path";
    /// One asked name's rows, bag included, in the order they were written:
    /// the map's precedence order.
    WARM_MODULE_ROWS => NoScratch,
        "SELECT path, mtime_secs, file_size, analysis, bag, extract_version, deps_stamp \
         FROM modules WHERE source = ?1 AND module_name = ?2 ORDER BY rowid";
    /// The stub BLOB is column 6, read only after the stamp checks pass;
    /// `length(m.analysis)` reads the record header, not the blob.
    WARM_PACK_WITH_STUBS => NoScratch,
        "SELECT m.module_name, m.path, m.mtime_secs, m.file_size, m.extract_version, \
                m.deps_stamp, s.stub, length(m.analysis) \
         FROM modules m LEFT JOIN stubs s ON s.path = m.path";

    // ---- stubs, surfaces, conclusions ----
    STUBS_CLEAR => NoScratch, "DELETE FROM stubs";
    STUB_DELETE => NoScratch, "DELETE FROM stubs WHERE path = ?1";
    STUB_SAVE => NoScratch, "INSERT OR REPLACE INTO stubs (path, stub) VALUES (?1, ?2)";
    STUB_SAVE_IF_CURRENT => NoScratch,
        "INSERT OR REPLACE INTO stubs (path, stub)
         SELECT ?1, ?2 FROM modules
         WHERE path = ?1 AND mtime_secs = ?3 AND file_size = ?4";
    SURFACE_SAVE => NoScratch,
        "INSERT OR REPLACE INTO surfaces (path, version, surface) VALUES (?1, ?2, ?3)";
    SURFACE_DELETE => NoScratch, "DELETE FROM surfaces WHERE path = ?1";
    SURFACE_LOAD => NoScratch,
        "SELECT surface FROM surfaces WHERE path = ?1 AND version = ?2";
    /// The newest row at or below the pinned generation; the `(path,
    /// generation)` key serves the ORDER BY.
    CONCLUSIONS_LOAD => NoScratch,
        "SELECT map, source_fingerprint, flush_generation FROM conclusions \
         WHERE path = ?1 AND generation <= ?2 \
         ORDER BY generation DESC LIMIT 1";
    CONCLUSIONS_SAVE => NoScratch,
        "INSERT OR REPLACE INTO conclusions \
         (path, generation, map, source_fingerprint, flush_generation) \
         VALUES (?1, ?2, ?3, ?4, ?5)";
    CONCLUSIONS_DELETE_PATH => NoScratch, "DELETE FROM conclusions WHERE path = ?1";
    CONCLUSIONS_PRUNE => Scratch("once per flush round, after the publish: a DELETE whose WHERE reads its own table collects the doomed keys first"),
        "DELETE FROM conclusions WHERE generation < ?1 AND generation < (
             SELECT MAX(c2.generation) FROM conclusions c2
             WHERE c2.path = conclusions.path AND c2.generation <= ?1
         )";
    REPAIR_FRONTIER => NoScratch,
        "SELECT DISTINCT m.path FROM modules m \
         WHERE m.analysis IS NOT NULL AND m.extract_version = ?1 \
           AND (NOT EXISTS ( \
                 SELECT 1 FROM conclusions c \
                 WHERE c.path = m.path AND c.generation <= ?2 \
               ) \
             OR NOT EXISTS ( \
                 SELECT 1 FROM surfaces s \
                 WHERE s.path = m.path AND s.version = ?3 \
               ))";
    /// Same bind list as `REPAIR_FRONTIER`; `?2` (the generation) does not
    /// participate in a surface-only selection.
    REPAIR_FRONTIER_SURFACES => NoScratch,
        "SELECT DISTINCT m.path FROM modules m \
         WHERE m.analysis IS NOT NULL AND m.extract_version = ?1 AND ?2 = ?2 \
           AND NOT EXISTS ( \
                 SELECT 1 FROM surfaces s \
                 WHERE s.path = m.path AND s.version = ?3 \
               )";

    // ---- the relational row store ----
    /// Sticky workspace tier: a project lib/ file re-shredded by the @INC
    /// resolver as 'import' keeps the walk's verdict.
    FILES_UPSERT => NoScratch,
        "INSERT INTO files (path, source) VALUES (?1, ?2)
         ON CONFLICT(path) DO UPDATE SET source =
           CASE WHEN files.source = 'workspace' THEN 'workspace' ELSE excluded.source END";
    FILE_ID_BY_PATH => NoScratch, "SELECT file_id FROM files WHERE path = ?1";
    FILE_ID_BY_PATH_TIER => NoScratch,
        "SELECT file_id FROM files WHERE path = ?1 AND source = ?2";
    FILES_ALL_PATHS => NoScratch, "SELECT path FROM files";
    /// The erasers take a resolved `file_id`: keyed through a subquery
    /// instead (`IN` or `=`), the `refs` delete collects its doomed keys in an
    /// ephemeral b-tree on every invalidation.
    REFS_DELETE_FILE_ID => NoScratch, "DELETE FROM refs WHERE file_id = ?1";
    SYMS_DELETE_FILE_ID => NoScratch, "DELETE FROM syms WHERE file_id = ?1";
    HELD_DELETE_FILE_ID => NoScratch, "DELETE FROM held WHERE file_id = ?1";
    FILES_DELETE_PATH => NoScratch, "DELETE FROM files WHERE path = ?1";
    FILES_DELETE_PATH_TIER => NoScratch,
        "DELETE FROM files WHERE path = ?1 AND source = ?2";
    STRINGS_INTERN => NoScratch, "INSERT OR IGNORE INTO strings (s) VALUES (?1)";
    STRINGS_LOOKUP => NoScratch, "SELECT str_id FROM strings WHERE s = ?1";
    /// OR IGNORE: a duplicate pair would otherwise abort the whole chunk
    /// transaction and take other files' rows with it.
    REFS_INSERT => NoScratch, "INSERT OR IGNORE INTO refs (name_id, file_id) VALUES (?1, ?2)";
    HELD_INSERT => NoScratch, "INSERT OR IGNORE INTO held (name_id, file_id) VALUES (?1, ?2)";
    SYMS_INSERT => NoScratch,
        "INSERT INTO syms (file_id, name_id, key_id, kind, start_row, start_col, end_row,
                           end_col, container_id, flags)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)";
    /// Set-difference rather than a per-string `NOT EXISTS`: `container_id`
    /// carries no index, so a per-string probe would scan `syms` per string.
    STRINGS_GC => Scratch("`--gc-cache` maintenance verb, once per run: the live-id set is built once"),
        "DELETE FROM strings WHERE str_id NOT IN (
             SELECT name_id FROM refs
             UNION SELECT name_id FROM syms
             UNION SELECT key_id FROM syms
             UNION SELECT container_id FROM syms WHERE container_id IS NOT NULL
             UNION SELECT name_id FROM held
         )";
    // The candidate halves are separate statements rather than one UNION,
    // which would dedup through a temp b-tree on every `refs_to` key; the
    // caller dedups. The refs and held halves yield each file once by
    // construction (the primary key is the (name, file) pair).
    REF_CANDIDATES_REFS => NoScratch,
        "SELECT f.path FROM refs r
           JOIN files f ON f.file_id = r.file_id
          WHERE r.name_id = (SELECT str_id FROM strings WHERE s = ?1)";
    REF_CANDIDATES_SYMS => NoScratch,
        "SELECT f.path FROM syms y
           JOIN files f ON f.file_id = y.file_id
          WHERE y.key_id = (SELECT str_id FROM strings WHERE s = ?1)";
    /// A gated emission's names: present only in the views whose gate fires.
    REF_CANDIDATES_HELD => NoScratch,
        "SELECT f.path FROM held h
           JOIN files f ON f.file_id = h.file_id
          WHERE h.name_id = (SELECT str_id FROM strings WHERE s = ?1)";
    #[cfg(test)]
    REF_CANDIDATE_FILE_COUNT => NoScratch,
        "SELECT COUNT(*) FROM refs
          WHERE name_id = (SELECT str_id FROM strings WHERE s = ?1)";
    SYM_MEMBER_PROBE => NoScratch, concat!(
        "SELECT ",
        syms_half!("name_id", "?2", container), " OR ",
        syms_half!("key_id", "?2", container), " OR ",
        syms_half!("name_id", "?3", container), " OR ",
        syms_half!("key_id", "?3", container), " OR ",
        held_half!("?2"), " OR ",
        held_half!("?3"),
        " FROM files f WHERE f.path = ?1"
    );
    SYM_NAME_PROBE => NoScratch, concat!(
        "SELECT ",
        syms_half!("name_id", "?2"), " OR ",
        syms_half!("key_id", "?2"), " OR ",
        syms_half!("name_id", "?3"), " OR ",
        syms_half!("key_id", "?3"), " OR ",
        held_half!("?2"), " OR ",
        held_half!("?3"),
        " FROM files f WHERE f.path = ?1"
    );
    NAME_PROBE => NoScratch, concat!(
        "SELECT ",
        refs_half!("?2"), " OR ",
        refs_half!("?3"), " OR ",
        syms_half!("name_id", "?2"), " OR ",
        syms_half!("key_id", "?2"), " OR ",
        syms_half!("name_id", "?3"), " OR ",
        syms_half!("key_id", "?3"), " OR ",
        held_half!("?2"), " OR ",
        held_half!("?3"),
        " FROM files f WHERE f.path = ?1"
    );
    /// Every workspace-tier symbol row; the non-ASCII workspace/symbol query
    /// filters in Rust with full Unicode case folding.
    SYM_ROWS_WORKSPACE => NoScratch,
        "SELECT f.path, n.s, y.kind, y.start_row, y.start_col, y.end_row, y.end_col,
                c.s, y.flags
           FROM syms y
           JOIN files f ON f.file_id = y.file_id
           JOIN strings n ON n.str_id = y.name_id
           LEFT JOIN strings c ON c.str_id = y.container_id
          WHERE f.source = 'workspace'";
    SYM_ROWS_WORKSPACE_LIKE => NoScratch,
        "SELECT f.path, n.s, y.kind, y.start_row, y.start_col, y.end_row, y.end_col,
                c.s, y.flags
           FROM syms y
           JOIN files f ON f.file_id = y.file_id
           JOIN strings n ON n.str_id = y.name_id
           LEFT JOIN strings c ON c.str_id = y.container_id
          WHERE f.source = 'workspace'
            AND n.s LIKE '%' || ?1 || '%' ESCAPE '\\'";
    REF_NAMES_ALL => NoScratch,
        "SELECT DISTINCT s.s FROM refs r JOIN strings s ON s.str_id = r.name_id";
    /// The heatmap prune's held half: a gated emission's refs reference their
    /// names in the views whose gate fires. The caller unions it in.
    REF_NAMES_HELD => NoScratch,
        "SELECT DISTINCT s.s FROM held h JOIN strings s ON s.str_id = h.name_id";
    /// Against the KEY: refs are keyed by `Ref::match_key`, so comparing them
    /// to a symbol's display name is what let the two families drift apart.
    /// A no-op for the rows this view gates on (an `@EXPORT` name is bare).
    UNUSED_EXPORTS => NoScratch,
        "SELECT f.path, n.s, y.start_row, y.start_col
           FROM syms y
           JOIN files f ON f.file_id = y.file_id
           JOIN strings n ON n.str_id = y.name_id
          WHERE f.source = 'workspace'
            AND (y.flags & ?1) != 0
            AND NOT EXISTS (
                  SELECT 1 FROM refs r
                   WHERE r.name_id = y.key_id
                     AND r.file_id != y.file_id
                )
            AND NOT EXISTS (
                  SELECT 1 FROM held h
                   WHERE h.name_id = y.key_id
                     AND h.file_id != y.file_id
                )";

    // ---- `--gc-cache` report ----
    GC_COUNT_FILES => NoScratch, "SELECT COUNT(*) FROM files";
    GC_COUNT_REFS => NoScratch, "SELECT COUNT(*) FROM refs";
    GC_COUNT_SYMS => NoScratch, "SELECT COUNT(*) FROM syms";
    GC_COUNT_HELD => NoScratch, "SELECT COUNT(*) FROM held";
    GC_COUNT_STRINGS => NoScratch, "SELECT COUNT(*) FROM strings";
    GC_COUNT_ORPHANED_STRINGS => NoScratch,
        "SELECT COUNT(*) FROM strings s WHERE NOT EXISTS(SELECT 1 FROM refs r WHERE r.name_id=s.str_id) \
             AND NOT EXISTS(SELECT 1 FROM syms y WHERE y.name_id=s.str_id OR y.key_id=s.str_id \
                            OR y.container_id=s.str_id) \
             AND NOT EXISTS(SELECT 1 FROM held h WHERE h.name_id=s.str_id)";
    GC_COUNT_WORKSPACE_FILES => NoScratch,
        "SELECT COUNT(*) FROM files f WHERE f.source='workspace'";
}
