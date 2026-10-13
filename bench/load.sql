-- Load every JSONL run into bench/measurements.duckdb.
--   duckdb bench/measurements.duckdb < bench/load.sql
--
-- Idempotent: re-running re-reads every file and rebuilds both tables, which
-- is cheap at this scale and means a re-collected run replaces itself rather
-- than double-counting. The JSONL files are the source of truth; the database
-- is a derived artifact and safe to delete.

CREATE OR REPLACE TABLE runs AS
SELECT run_id, ts::TIMESTAMP AS ts, sha, dirty, features, host, kernel,
       nproc, mem_kb, loadavg_at_start, reps_planned
FROM read_json_auto('bench/runs/*.jsonl', union_by_name := true, ignore_errors := true)
WHERE t = 'run';

CREATE OR REPLACE TABLE measurements AS
SELECT run_id, corpus, rep, phase, kind, name, tag, value, unit
FROM (
  -- `tag` keys per-file lanes and editor scenario steps; the empty BY NAME
  -- arm keeps the column when no loaded run carries one
  SELECT * FROM read_json_auto('bench/runs/*.jsonl', union_by_name := true, ignore_errors := true)
  UNION ALL BY NAME SELECT NULL::VARCHAR AS tag WHERE false
)
WHERE t = 'm';

-- A run whose binary was built from a dirty tree, or on a loaded box, is not
-- comparable to one that was not. Surfaced as a view rather than filtered
-- away: the harness records what happened, reports decide what to trust.
CREATE OR REPLACE VIEW trustworthy_runs AS
SELECT * FROM runs WHERE NOT dirty AND loadavg_at_start < 2.0;
