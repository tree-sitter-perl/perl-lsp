-- Compare fresh runs against the checked-in KPI baselines.
--   duckdb bench/measurements.duckdb < bench/baseline-check.sql
-- (after bench/load.sql; reads bench/baselines.jsonl directly)
--
-- A move is flagged only when it clears BOTH sides' observed spread — a
-- delta smaller than the noise you measured is the noise. Baselines from a
-- dirty tree are surfaced, not hidden: you can compare against one, but the
-- report says so.

.mode box

WITH base AS (
  -- The NEWEST baseline row per KPI is the promise; older rows are history
  -- (a reseed appends, it never rewrites), and joining all of them would
  -- report every metric once per generation.
  SELECT * FROM read_json_auto('bench/baselines.jsonl')
  QUALIFY row_number() OVER (PARTITION BY corpus, phase, metric ORDER BY date DESC) = 1
),
fresh AS (
  SELECT r.sha, r.ts, m.corpus, m.phase,
         CASE
           WHEN m.kind='timing' AND m.name='check.wall' THEN 'check.wall_ms'
           WHEN m.kind='rss'    AND m.name='peak'       THEN 'check.peak_rss_mb'
           WHEN m.kind='startup'                        THEN 'editor.' || m.name || '_ms'
           WHEN m.kind='verb_ms'                        THEN 'editor.verb.' || m.name || '_ms'
           WHEN m.kind='diag_push_ms'                   THEN 'editor.diag_push_ms'
         END AS metric,
         median(m.value) AS value,
         max(m.value)-min(m.value) AS spread, count(*) AS n
  FROM measurements m JOIN runs r USING (run_id)
  WHERE r.ts = (SELECT max(ts) FROM runs)
  GROUP BY ALL
  HAVING metric IS NOT NULL
)
SELECT f.corpus, f.phase, f.metric,
       b.value AS baseline, round(f.value,1) AS fresh,
       round(f.value - b.value, 1) AS delta,
       round(greatest(f.spread, b.spread), 1) AS noise,
       CASE WHEN abs(f.value-b.value) > greatest(f.spread, b.spread, 1)
            THEN CASE WHEN f.value > b.value THEN 'REGRESSED' ELSE 'improved' END
            ELSE '' END AS verdict,
       b.sha AS base_sha, b.date AS base_date,
       CASE WHEN b.dirty THEN 'DIRTY-BASE' ELSE '' END AS caveat
FROM fresh f JOIN base b USING (corpus, phase, metric)
ORDER BY (verdict='REGRESSED') DESC, abs(f.value-b.value)/nullif(b.value,0) DESC;
