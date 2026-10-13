-- Compare fresh runs against the checked-in KPI baselines.
--   duckdb bench/measurements.duckdb < bench/baseline-check.sql
-- (after bench/load.sql; reads bench/baselines.jsonl directly)
--
-- A move is flagged only when it clears BOTH sides' observed spread — a
-- delta smaller than the noise you measured is the noise. Baselines from a
-- dirty tree are surfaced, not hidden: you can compare against one, but the
-- report says so.
--
-- The metric names below MUST spell exactly what seed-baselines.py writes: a
-- name that differs on one side drops out of the join silently, and the
-- report just looks shorter.

.mode box

WITH base_rows AS (
  -- insertion order is preserved on a single-file scan, so seq is the line
  SELECT *, row_number() OVER () AS seq
  FROM read_json_auto('bench/baselines.jsonl')
),
base AS (
  -- The NEWEST baseline row per KPI is the promise; older rows are history
  -- (a reseed appends, it never rewrites), and joining all of them would
  -- report every metric once per generation. A same-day reseed is broken by
  -- line order: the later append wins.
  SELECT * FROM base_rows
  QUALIFY row_number() OVER (PARTITION BY corpus, phase, metric
                             ORDER BY date DESC, seq DESC) = 1
),
latest AS (
  SELECT m.*, r.sha, r.ts
  FROM measurements m JOIN runs r USING (run_id)
  WHERE r.ts = (SELECT max(ts) FROM runs)
),
named AS (
  SELECT corpus, phase, value,
         CASE
           WHEN kind='timing' AND name='check.wall' THEN 'check.wall_ms'
           WHEN kind='rss'    AND name='peak'       THEN 'check.peak_rss_mb'
           WHEN kind='startup'                      THEN 'editor.' || name || '_ms'
           -- keyed by scenario step: the same verb at two points of a
           -- scenario is two promises (seed-baselines.py says why)
           WHEN kind='verb_ms'      THEN 'editor.verb.' || name || '.' || coalesce(tag, name) || '_ms'
           WHEN kind='diag_push_ms' THEN 'editor.diag_push.' || name || '_ms'
           WHEN kind='server_rss' AND name='end'    THEN 'editor.rss_end_mb'
         END AS metric
  FROM latest
),
fresh AS (
  SELECT corpus, phase, metric, median(value) AS value,
         max(value)-min(value) AS spread, count(*) AS n
  FROM named WHERE metric IS NOT NULL
  GROUP BY ALL
  UNION ALL
  -- per-file build stats are taken per rep, then folded across reps like
  -- every other KPI (seed-baselines.py says why); p99 is the seeder's index
  SELECT corpus, phase, metric, median(v), max(v)-min(v), count(*)
  FROM (
    SELECT corpus, phase,
           unnest(['file_build_ms', 'file_build_ms.p99', 'file_build_ms.max']) AS metric,
           unnest([median(value),
                   list_sort(list(value))[least(count(*), floor(count(*) * 0.99)::BIGINT + 1)],
                   max(value)]) AS v
    FROM latest WHERE kind='file_build'
    GROUP BY corpus, phase, rep
  )
  GROUP BY corpus, phase, metric
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
