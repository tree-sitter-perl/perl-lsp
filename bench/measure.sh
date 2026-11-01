#!/usr/bin/env bash
# Measure perl-lsp across the real-project corpora, one JSONL line per fact.
#
#   bench/measure.sh                     # every corpus, 3 reps, cold+warm
#   bench/measure.sh FHEM                # one corpus (still 3 reps; 3 is the floor)
#   bench/measure.sh --out runs/         # where the JSONL lands
#   bench/measure.sh --no-instrumented   # skip the counter pass (KPIs only)
#
# Output is DELIBERATELY tall and raw: {kind,name,value,unit} per row, no
# aggregation, no derived ratios. Reports slice it in DuckDB. A mean computed
# here is a decision the collector has no business making, and a stored ratio
# is how an attempts-vs-completions mixup became a finding once already.
#
# Every repetition is its own row with its own `rep`. There is deliberately no
# way to emit "the number" — a single sample is a sample, and the schema says
# so, because a one-run baseline once produced a phantom +400ms regression
# that survived a day.
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$HERE/target/release/perl-lsp"
BULK="${PERL_CORPORA:-$HOME/perl-corpora}/bulk"
DEPS="${PERL_CORPORA:-$HOME/perl-corpora}/deps"
REPS=3
OUT="$HERE/bench/runs"
# Caps exist so a pathological corpus cannot take the host down with it. FHEM's
# --check peaked at 12 GB once; an unbounded sweep across eight corpora is how
# you lose the box mid-measurement and the run with it.
MAX_RSS_MB=8000
MAX_SECS=1200
INSTRUMENTED=1
ONLY=()

while [ $# -gt 0 ]; do
  case "$1" in
    --reps) REPS="$2"; shift 2;;
    --out)  OUT="$2"; shift 2;;
    --max-rss-mb) MAX_RSS_MB="$2"; shift 2;;
    --max-secs)   MAX_SECS="$2"; shift 2;;
    --no-instrumented) INSTRUMENTED=0; shift;;
    -h|--help) sed -n '2,17p' "$0"; exit 0;;
    *) ONLY+=("$1"); shift;;
  esac
done

# Three is the floor, not a default. A one- or two-run baseline is how a
# phantom +400ms regression survived a day here; the reports mark n<3
# PROVISIONAL, and there is no reason to collect data that arrives pre-marked
# as untrustworthy.
if [ "$REPS" -lt 3 ]; then
  echo "reps raised $REPS -> 3 (a baseline under 3 runs is noise)" >&2
  REPS=3
fi

[ -x "$BIN" ] || { echo "no release binary at $BIN — cargo build --release --features cpp" >&2; exit 1; }
[ -d "$BULK" ] || { echo "no corpora at $BULK — run corpus/bootstrap.sh" >&2; exit 1; }
command -v jq >/dev/null || { echo "needs jq" >&2; exit 1; }

mkdir -p "$OUT"
RUN_ID="$(date -u +%Y%m%dT%H%M%SZ)-$$"
JSONL="$OUT/$RUN_ID.jsonl"
SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT

# --- provenance, once per run -------------------------------------------
# A measurement without its build features is a trap: a non-cpp build
# lang-skips half of gold, and the same class of silence poisons timings.
SHA="$(git -C "$HERE" rev-parse --short=8 HEAD)"
DIRTY=false; git -C "$HERE" diff --quiet || DIRTY=true
FEATURES="$("$BIN" --languages 2>/dev/null | sed 's/.*languages: //')"
jq -cn \
  --arg run_id "$RUN_ID" --arg ts "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg sha "$SHA" --argjson dirty "$DIRTY" --arg features "$FEATURES" \
  --arg host "$(hostname)" --arg kernel "$(uname -r)" \
  --argjson nproc "$(nproc)" \
  --argjson mem_kb "$(awk '/MemTotal/{print $2}' /proc/meminfo)" \
  --argjson load "$(awk '{print $1}' /proc/loadavg)" \
  --argjson reps "$REPS" \
  --argjson max_rss_mb "$MAX_RSS_MB" --argjson max_secs "$MAX_SECS" \
  '{t:"run",run_id:$run_id,ts:$ts,sha:$sha,dirty:$dirty,features:$features,
    host:$host,kernel:$kernel,nproc:$nproc,mem_kb:$mem_kb,loadavg_at_start:$load,
    reps_planned:$reps,max_rss_mb:$max_rss_mb,max_secs:$max_secs}' >> "$JSONL"

emit() { # corpus rep phase kind name value unit
  jq -cn --arg run_id "$RUN_ID" --arg c "$1" --argjson r "$2" --arg p "$3" \
        --arg k "$4" --arg n "$5" --argjson v "$6" --arg u "$7" \
    '{t:"m",run_id:$run_id,corpus:$c,rep:$r,phase:$p,kind:$k,name:$n,value:$v,unit:$u}' \
    >> "$JSONL"
}

# Load average is recorded per measurement, not just per run: a corpus that
# happened to run while the box was busy is not comparable to one that did
# not, and averaging across them silently is the trap.
# Timed reps and counters come from DIFFERENT runs. With PERL_LSP_GHOST_STATS
# on, a witness-heavy build pays for every hop counter it crosses (~25% wall on
# a 46k-line file, `util/ghost_stats.rs::BuildScope`), so a wall, RSS or
# per-file build time taken with it on is not the number a user sees. The timed
# reps run with only the per-file build timer (`PERL_LSP_TIMINGS`, a pair of
# clock reads per file); one extra instrumented pass per corpus carries the
# counters, per-file check lanes and heap composition, and its own wall/RSS
# land under `instrumented_*` kinds that no KPI reads.
measure_one() { # corpus root rep phase cachedir instrumented(0|1)
  local corpus="$1" root="$2" rep="$3" phase="$4" cache="$5" instr="$6"
  local pfx=""; [ "$instr" = 1 ] && pfx="instrumented_"
  local g="$SCRATCH/g.json" t="$SCRATCH/t.json" h="$SCRATCH/h.json"
  rm -f "$g" "$t" "$h"

  local pl=""
  [ -d "$DEPS/$corpus" ] && pl="$DEPS/$corpus/lib/perl5"

  emit "$corpus" "$rep" "$phase" "env" "loadavg" "$(awk '{print $1}' /proc/loadavg)" "load"

  # /usr/bin/time gives peak RSS the process cannot under-report about itself.
  # `ulimit -v` is the hard stop: the process dies with ENOMEM instead of
  # pushing the host into swap, which is the difference between losing one
  # measurement and losing the machine running the sweep.
  local tf="$SCRATCH/time.txt"
  # The RSS cap is the BINARY's (PERL_LSP_MAX_RSS_MB): it reads its own
  # /proc/self/status, which is exact. A shell-side poller has to find the
  # process through subshell -> timeout -> time -> binary and guess with
  # pgrep; it guessed wrong and let a 715 MB corpus sail past a 400 MB cap.
  # The process knows its own size. Ask it.
  #
  # SIGTERM is now handled either way, so an external stop (timeout below, a
  # CI cancellation, a human) also flushes rather than discarding. SIGKILL
  # stays the escalation and never the first move: it is uncatchable, so it
  # takes the instrumentation with it. On Znuny an abrupt 8 GB stop cost 40%
  # of the per-file rows while leaving wall untouched — which is exactly what
  # a silent kill looks like from outside.
  #
  # ulimit and timeout remain HOST backstops set well above the soft cap:
  # only the kernel catches an allocation that outruns a 250ms poll.
  local instr_env=()
  [ "$instr" = 1 ] && instr_env=(PERL_LSP_GHOST_STATS=1 PERL_LSP_GHOST_JSON="$g" PERL_LSP_HEAP_JSON="$h")
  ( ulimit -v $(( (MAX_RSS_MB + 4000) * 1024 )) 2>/dev/null
    env PERL5LIB="$pl" XDG_CACHE_HOME="$cache" \
    PERL_LSP_TIMINGS=1 PERL_LSP_TIMINGS_JSON="$t" "${instr_env[@]}" \
    PERL_LSP_MAX_RSS_MB="$MAX_RSS_MB" PERL_LSP_MAX_SECONDS="$MAX_SECS" \
      timeout -k 30 -s TERM $(( MAX_SECS + 120 )) \
      /usr/bin/time -f '%e %M %P' -o "$tf" \
      "$BIN" --check "$root" >/dev/null 2>&1 )
  local rc=$?

  # A capped run MUST be visible. Absent rows look like a collection bug, and
  # "FHEM has no number" is a finding about FHEM, not about the harness.
  local outcome=ok
  case $rc in
    0)   outcome=ok;;
    1)   outcome=ok_findings;; # --check exit 1 = diagnostics found: lint
                               # semantics, a HEALTHY run. Labeling it error
                               # made 30 of 48 baseline runs read as failures.
    90)  outcome=rss_cap;;    # self-capped: partial data present
    91)  outcome=time_cap;;   # self-capped: partial data present
    92)  outcome=sigterm;;   # harness stopped it; instrumentation flushed
    124|137) outcome=hard_kill;;  # backstop fired — the soft cap did not
    *)   outcome=error;;
  esac
  emit "$corpus" "$rep" "$phase" "${pfx}outcome" "$outcome" "$rc" "exit_code"

  if [ -s "$tf" ]; then
    read -r wall maxrss cpupct < <(tail -1 "$tf")
    emit "$corpus" "$rep" "$phase" "${pfx}timing" "check.wall" "$(awk -v w="$wall" 'BEGIN{print w*1000}')" "ms"
    emit "$corpus" "$rep" "$phase" "${pfx}rss"    "peak"       "$(awk -v m="$maxrss" 'BEGIN{print m/1024}')" "MB"
    emit "$corpus" "$rep" "$phase" "${pfx}cpu"    "utilization" "${cpupct%\%}" "pct"
  fi

  # Every counter, every module. No top-N, no rounding — the distribution is
  # the point, and one pathological file inside a healthy total is exactly
  # what a mean hides.
  [ -s "$g" ] && jq -c --arg run_id "$RUN_ID" --arg c "$corpus" --argjson r "$rep" --arg p "$phase" '
      (.counters   | to_entries[] | {kind:"counter",   name:.key, value:.value,     unit:"n"}),
      (.timings    | to_entries[] | {kind:"accum_ns",  name:.key, value:.value.ns,  unit:"ns"}),
      (.timings    | to_entries[] | {kind:"accum_n",   name:.key, value:.value.n,   unit:"n"}),
      (.quantities | to_entries[] | {kind:"qty_sum",   name:.key, value:.value.sum, unit:"n"}),
      (.quantities | to_entries[] | {kind:"qty_n",     name:.key, value:.value.n,   unit:"n"}),
      ((.file_ns // {}) | to_entries[] | .key as $f | .value | to_entries[] |
        {kind:"check_incl", name:$f, tag:.key, value:(.value.incl_ns/1e6), unit:"ms"},
        {kind:"check_excl", name:$f, tag:.key, value:(.value.excl_ns/1e6), unit:"ms"}),
      ((.file_counts // {}) | to_entries[] | .key as $f | .value | to_entries[] |
        {kind:"file_count", name:$f, tag:.key, value:.value, unit:"n"})
      | {t:"m",run_id:$run_id,corpus:$c,rep:$r,phase:$p} + .' "$g" >> "$JSONL"

  # Per-file build times are KPIs (p50/p99/max): taken from timed reps only.
  [ "$instr" = 0 ] && [ -s "$t" ] && jq -c --arg run_id "$RUN_ID" --arg c "$corpus" --argjson r "$rep" --arg p "$phase" '
      .modules[] | {t:"m",run_id:$run_id,corpus:$c,rep:$r,phase:$p,
                    kind:(if .cached then "file_cached" else "file_build" end),
                    name:.module, value:(.build_ns/1e6), unit:"ms"},
                   {t:"m",run_id:$run_id,corpus:$c,rep:$r,phase:$p,
                    kind:"file_parse", name:.module, value:(.parse_ns/1e6), unit:"ms"}' \
      "$t" >> "$JSONL"

  # Heap composition: one row per (file, bucket) plus the aggregate — the
  # lane that turns "Znuny uses 9GB" into "whose refs". Written post-sweep,
  # pre-evict, so it approximates peak, not exit residue.
  [ -s "$h" ] && jq -c --arg run_id "$RUN_ID" --arg c "$corpus" --argjson r "$rep" --arg p "$phase" '
      (.files[] | .path as $f | to_entries[] | select(.key != "path") |
        {t:"m",run_id:$run_id,corpus:$c,rep:$r,phase:$p,
         kind:"heap_bytes", name:$f, tag:.key, value:.value, unit:"bytes"}),
      (.total | to_entries[] |
        {t:"m",run_id:$run_id,corpus:$c,rep:$r,phase:$p,
         kind:"heap_total_bytes", name:.key, value:.value, unit:"bytes"}),
      {t:"m",run_id:$run_id,corpus:$c,rep:$r,phase:$p,
       kind:"heap_total_bytes", name:"path_intern", value:.path_intern.bytes, unit:"bytes"}' \
      "$h" >> "$JSONL"

  local db; db="$(find "$cache" -name 'modules*.db' -printf '%s\n' 2>/dev/null | awk '{s+=$1} END{print s+0}')"
  emit "$corpus" "$rep" "$phase" "${pfx}store" "modules_db_bytes" "${db:-0}" "bytes"
}

CORPORA=()
if [ ${#ONLY[@]} -gt 0 ]; then CORPORA=("${ONLY[@]}")
else while IFS= read -r d; do CORPORA+=("$(basename "$d")"); done < <(find "$BULK" -mindepth 1 -maxdepth 1 -type d | sort); fi

echo "run $RUN_ID  sha=$SHA dirty=$DIRTY  [$FEATURES]  reps=$REPS"
echo "-> $JSONL"
for corpus in "${CORPORA[@]}"; do
  root="$BULK/$corpus"
  [ -d "$root" ] || { echo "  skip $corpus (missing)"; continue; }
  for rep in $(seq 1 "$REPS"); do
    # Cold means COLD: a private throwaway cache per rep, or the second rep
    # measures the first rep's cache and calls itself cold.
    cache="$SCRATCH/cache-$corpus-$rep"; rm -rf "$cache"; mkdir -p "$cache"
    printf '  %-14s rep %s cold ' "$corpus" "$rep"; measure_one "$corpus" "$root" "$rep" cold "$cache" 0
    printf 'warm\n';                                measure_one "$corpus" "$root" "$rep" warm "$cache" 0
    rm -rf "$cache"
  done
  if [ "$INSTRUMENTED" = 1 ]; then
    # rep 0: the counter pass, never a sample of the timed population.
    cache="$SCRATCH/cache-$corpus-instr"; rm -rf "$cache"; mkdir -p "$cache"
    printf '  %-14s counters cold ' "$corpus"; measure_one "$corpus" "$root" 0 cold "$cache" 1
    printf 'warm\n';                          measure_one "$corpus" "$root" 0 warm "$cache" 1
    rm -rf "$cache"
  fi
done
echo "done: $(wc -l < "$JSONL") rows"
