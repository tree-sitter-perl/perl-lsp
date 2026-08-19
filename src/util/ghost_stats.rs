//! Report-only ghost-list accounting for the byte-capped LRUs, gated by
//! `PERL_LSP_GHOST_STATS` (unset ⇒ fully inert: no allocation, no counters).
//!
//! A ghost list holds the KEYS of capacity-evicted entries — never values —
//! so a later lookup that misses the live cache but hits the ghost list is
//! direct evidence the entry was evicted and then wanted again. The per-key
//! refetch histogram separates the two failure modes a plain hit rate
//! conflates: few keys refetched many times each ⇒ a scan is flushing a hot
//! set (fix = admission policy); many keys refetched about once ⇒ genuine
//! capacity shortfall (fix = size).
//!
//! This module observes; it never feeds a cache decision. Gate values:
//! unset/`0` ⇒ off; `1`/`true` ⇒ reports to stderr; any other value ⇒
//! treated as a file path the reports append to (benches that swallow
//! stderr read the file instead).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// Ghost-key capacity per cache. Fixed rather than 2x-live because live
/// entry counts are byte-derived and vary 100x by language (cpp bags ~700 KB
/// ⇒ ~180 entries under 128 MiB; Perl analyses tens of KB ⇒ thousands).
/// 8192 exceeds 2x the largest plausible live count (128 MiB / ~50 KB ≈ 2.6k)
/// while costing well under 1 MiB of path strings.
const GHOST_CAP: usize = 8192;

/// How often a busy cache re-emits its report (every N misses), so a run
/// killed without a clean shutdown still leaves the trail on record.
const EMIT_EVERY_MISSES: u64 = 2000;

/// How much of a cache report to print. The aggregate lines are cheap and
/// always useful; the per-key culprit lists are only worth their bulk once.
#[derive(Clone, Copy)]
pub enum Detail {
    Summary,
    Full,
}

/// How often a long run re-emits the COUNTER block, for the same reason —
/// `emit_all` only runs at a clean shutdown, so a run that is killed used to
/// lose every counter, which is exactly what `EMIT_EVERY_MISSES` exists to
/// prevent on the cache side. A 43-minute corpus run was lost to this.
///
/// Time-based rather than event-based, unlike the cache side: the failure
/// being prevented is "a long run was killed", which is a property of elapsed
/// time. A miss-count trigger emits nothing at all for a workload that never
/// touches a cache — and the counters that say whether a MEASUREMENT was
/// valid (`persist_queue.producer_parked`) come from exactly such a run.
const ATTRIBUTION_REEMIT_MILLISECONDS: u64 = 60_000;

/// `PERL_LSP_GHOST_REEMIT_MILLISECONDS` overrides the interval — a corpus run
/// measured in tens of minutes may want its counters more often than once a
/// minute, and a test wants them far sooner than that.
fn attribution_reemit_interval() -> u64 {
    static I: OnceLock<u64> = OnceLock::new();
    *I.get_or_init(|| {
        std::env::var("PERL_LSP_GHOST_REEMIT_MILLISECONDS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(ATTRIBUTION_REEMIT_MILLISECONDS)
    })
}

/// Is a counter re-emit due? Split out so the interval rule is testable
/// without the process-wide env gate. Saturating: a clock that appears to go
/// backwards must not suppress emission forever.
fn attribution_reemit_due(now_ms: u64, last_ms: u64, interval_ms: u64) -> bool {
    now_ms.saturating_sub(last_ms) >= interval_ms
}

enum Sink {
    Off,
    Stderr,
    File(String),
}

fn sink() -> &'static Sink {
    static S: OnceLock<Sink> = OnceLock::new();
    S.get_or_init(|| match std::env::var("PERL_LSP_GHOST_STATS") {
        Err(_) => Sink::Off,
        Ok(v) if v.is_empty() || v == "0" => Sink::Off,
        Ok(v) if v == "1" || v.eq_ignore_ascii_case("true") => Sink::Stderr,
        Ok(path) => Sink::File(path),
    })
}

pub fn enabled() -> bool {
    !matches!(sink(), Sink::Off)
}

// ---------------------------------------------------------------------------
// Trigger attribution (measurement-only, rides the same gate).
//
// Two complementary views of "who initiates the background work":
// 1. `count(tag)` — cheap named event counters callers drop at candidate
//    trigger sites (refresh callback fired, enrich_open ran, ...).
// 2. Sampled backtraces on cache MISSES (`PERL_LSP_GHOST_TRACE=N` samples
//    every Nth miss; needs debug symbols to be readable). Assumption-free:
//    whatever call path actually drives the decode storm shows up here.
// ---------------------------------------------------------------------------

fn counters() -> &'static Mutex<HashMap<String, u64>> {
    static C: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Bump a named event counter. No-op when the gate is off.
pub fn count(tag: &str) {
    if !enabled() {
        return;
    }
    {
        let mut c = counters().lock().unwrap_or_else(|e| e.into_inner());
        *c.entry(tag.to_string()).or_insert(0) += 1;
    }
    // AFTER the guard drops: the re-emit locks the same map, and this mutex
    // is not reentrant.
    maybe_reemit_attribution();
}

/// Process-start reference for the re-emit clock. An `Instant` cannot live in
/// an atomic, so elapsed-millis-since-this is what the atomic holds.
fn run_started() -> std::time::Instant {
    static S: OnceLock<std::time::Instant> = OnceLock::new();
    *S.get_or_init(std::time::Instant::now)
}

static LAST_ATTRIBUTION_EMIT_MS: AtomicU64 = AtomicU64::new(0);

/// Re-emit the counter block if the interval has elapsed. Called from the
/// counter path rather than a timer thread, so an idle process stays idle;
/// the CAS means exactly one thread emits per interval.
fn maybe_reemit_attribution() {
    let now = run_started().elapsed().as_millis() as u64;
    let last = LAST_ATTRIBUTION_EMIT_MS.load(Ordering::Relaxed);
    if !attribution_reemit_due(now, last, attribution_reemit_interval()) {
        return;
    }
    if LAST_ATTRIBUTION_EMIT_MS
        .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return; // another thread is emitting this interval
    }
    emit_attribution("periodic");
}

/// Add `n` to a named counter in one call. For a per-FILE quantity (fold
/// iterations, POD bytes) a printed line per file is unreadable at corpus
/// scale and unaggregable at any scale; a total plus a sample count gives
/// both the sum and the average, and a single-file run still reads exactly.
pub fn count_by(tag: &str, n: u64) {
    if !enabled() || n == 0 {
        return;
    }
    {
        let mut c = counters().lock().unwrap_or_else(|e| e.into_inner());
        *c.entry(tag.to_string()).or_insert(0) += n;
    }
    maybe_reemit_attribution();
}

/// tag -> (total nanos, call count). A per-call `[PHASE]` line is useless for
/// a region entered once per file across a corpus; what a hot region needs is
/// the SUM and the call count, so an average and a share are derivable.
fn accum() -> &'static Mutex<HashMap<String, (u128, u64)>> {
    static A: OnceLock<Mutex<HashMap<String, (u128, u64)>>> = OnceLock::new();
    A.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Add one timed region's elapsed nanos to `tag`'s running total.
pub fn add_ns(tag: &str, nanos: u128) {
    if !enabled() {
        return;
    }
    let mut a = accum().lock().unwrap_or_else(|e| e.into_inner());
    let e = a.entry(tag.to_string()).or_insert((0, 0));
    e.0 += nanos;
    e.1 += 1;
}

/// Add `n` to `tag`'s running total, for a quantity that is not a duration
/// (bytes, rows, witnesses). Shares the accumulator so the report shows sums
/// and call counts for both without a second table.
pub fn add_n(tag: &str, n: u64) {
    if !enabled() {
        return;
    }
    let mut q = quantities().lock().unwrap_or_else(|e| e.into_inner());
    let e = q.entry(tag.to_string()).or_insert((0, 0));
    e.0 += n as u128;
    e.1 += 1;
}

/// tag -> (total, sample count). Deliberately NOT the duration accumulator:
/// rendering a witness count through a millisecond formatter reads as a
/// timing and gets quoted as one.
fn quantities() -> &'static Mutex<HashMap<String, (u128, u64)>> {
    static Q: OnceLock<Mutex<HashMap<String, (u128, u64)>>> = OnceLock::new();
    Q.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Time `body` into `tag`'s running total. Inert when the gate is off — not
/// even an `Instant::now`, so an instrumented hot path costs nothing unmeasured.
#[inline]
pub fn timed<T>(tag: &str, body: impl FnOnce() -> T) -> T {
    if !enabled() {
        return body();
    }
    let t = std::time::Instant::now();
    let out = body();
    add_ns(tag, t.elapsed().as_nanos());
    out
}

/// Accumulate a region's elapsed time on drop — the `timed` shape for a body
/// that borrows its surroundings mutably and cannot be a closure.
pub struct ScopedNs {
    tag: &'static str,
    started: Option<std::time::Instant>,
}

impl ScopedNs {
    pub fn start(tag: &'static str) -> Self {
        ScopedNs { tag, started: enabled().then(std::time::Instant::now) }
    }
}

impl Drop for ScopedNs {
    fn drop(&mut self) {
        if let Some(t) = self.started {
            add_ns(self.tag, t.elapsed().as_nanos());
        }
    }
}

/// tag -> set of distinct keys seen. Pairs with `count(tag)`: the ratio of
/// total occurrences to distinct keys IS the repeat factor, which is the one
/// number that says whether a memo would pay.
fn distincts() -> &'static Mutex<HashMap<String, std::collections::HashSet<String>>> {
    static D: OnceLock<Mutex<HashMap<String, std::collections::HashSet<String>>>> = OnceLock::new();
    D.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record that `key` was seen under `tag`. No-op when the gate is off.
thread_local! {
    /// Keys looked up during the CURRENT per-file sweep, when one is open.
    /// Repeats are counted per sweep rather than globally on purpose: a memo
    /// scoped to one file's diagnostics can only return an answer it was
    /// already asked for INSIDE that sweep, so a global distinct count would
    /// credit it with cross-file repeats it can never serve.
    static SWEEP: std::cell::RefCell<Option<std::collections::HashSet<String>>> =
        const { std::cell::RefCell::new(None) };
}

/// Open a per-file sweep. Drop closes it and records the totals.
pub struct SweepScope {
    total: u64,
}

impl SweepScope {
    pub fn start() -> Self {
        if enabled() {
            SWEEP.with(|c| *c.borrow_mut() = Some(std::collections::HashSet::new()));
        }
        SweepScope { total: 0 }
    }

    /// Note one lookup of `key` inside the open sweep.
    pub fn note(key: &str) {
        if !enabled() {
            return;
        }
        SWEEP.with(|c| {
            if let Some(set) = c.borrow_mut().as_mut() {
                if !set.contains(key) {
                    set.insert(key.to_string());
                }
            }
        });
        count("sweep.lookup");
    }
}

impl Drop for SweepScope {
    fn drop(&mut self) {
        if !enabled() {
            return;
        }
        let _ = self.total;
        let distinct = SWEEP.with(|c| c.borrow_mut().take().map(|s| s.len()).unwrap_or(0));
        count_by("sweep.distinct", distinct as u64);
        count("sweep.files");
    }
}

pub fn count_distinct(tag: &str, key: &str) {
    if !enabled() {
        return;
    }
    let mut d = distincts().lock().unwrap_or_else(|e| e.into_inner());
    d.entry(tag.to_string()).or_default().insert(key.to_string());
}

thread_local! {
    /// Set while a region wants its downstream cache traffic attributed to it.
    /// A hit rate is only actionable per CALLER: a 99% global rate hides a
    /// caller that misses a third of the time, and that caller is the one to fix.
    static ATTRIB: std::cell::Cell<Option<&'static str>> = const { std::cell::Cell::new(None) };
}

/// Attribute downstream `count_attributed` events to `tag` for this scope.
pub struct Attribute(Option<&'static str>);

impl Attribute {
    pub fn start(tag: &'static str) -> Self {
        Attribute(ATTRIB.with(|a| a.replace(Some(tag))))
    }
}

impl Drop for Attribute {
    fn drop(&mut self) {
        ATTRIB.with(|a| a.set(self.0));
    }
}

/// Bump `<caller>.<event>` when inside an `Attribute` scope, else `<event>`.
pub fn count_attributed(event: &str) {
    if !enabled() {
        return;
    }
    match ATTRIB.with(|a| a.get()) {
        Some(t) => count(&format!("{t}.{event}")),
        None => count(&format!("unattributed.{event}")),
    }
}

fn trace_every() -> u64 {
    static N: OnceLock<u64> = OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("PERL_LSP_GHOST_TRACE")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    })
}

fn trace_buckets() -> &'static Mutex<HashMap<String, u64>> {
    static B: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    B.get_or_init(|| Mutex::new(HashMap::new()))
}

static TRACE_MISS_SEQ: AtomicU64 = AtomicU64::new(0);

/// Every Nth miss (per `PERL_LSP_GHOST_TRACE`), capture + symbolize a
/// backtrace and bucket it by its perl_lsp frame signature.
fn maybe_trace_miss(label: &str) {
    let n = trace_every();
    if n == 0 {
        return;
    }
    let seq = TRACE_MISS_SEQ.fetch_add(1, Ordering::Relaxed);
    if seq % n != 0 {
        return;
    }
    let bt = std::backtrace::Backtrace::force_capture();
    let text = format!("{bt}");
    let mut frames: Vec<String> = Vec::new();
    for line in text.lines() {
        let l = line.trim();
        // Symbol lines look like "N: perl_lsp::path::to::fn"; keep only our
        // own frames and drop the instrumentation/cache plumbing itself.
        let Some(idx) = l.find(": ") else { continue };
        let sym = &l[idx + 2..];
        if !sym.contains("perl_lsp") {
            continue;
        }
        if sym.contains("ghost_stats")
            || sym.contains("pack_bag_cache")
            || sym.contains("rehydrate")
            || sym.contains("bag_for")
        {
            continue;
        }
        // Strip hash suffixes and generic noise for stable bucketing.
        let clean = sym.split_whitespace().next().unwrap_or(sym);
        frames.push(clean.to_string());
        if frames.len() >= 14 {
            break;
        }
    }
    let sig = format!("[{label}] {}", frames.join(" <- "));
    let mut b = trace_buckets().lock().unwrap_or_else(|e| e.into_inner());
    *b.entry(sig).or_insert(0) += 1;
}

fn emit_text(text: &str) {
    match sink() {
        Sink::Off => {}
        Sink::Stderr => eprint!("{text}"),
        Sink::File(path) => {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = f.write_all(text.as_bytes());
            }
        }
    }
}

/// Dump the trigger counters + sampled miss-backtrace buckets.
pub fn emit_attribution(moment: &str) {
    if !enabled() {
        return;
    }
    let mut out = String::new();
    {
        let c = counters().lock().unwrap_or_else(|e| e.into_inner());
        let mut rows: Vec<(&String, &u64)> = c.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        out.push_str(&format!("[ghost-triggers {moment}] event counters:\n"));
        for (k, v) in rows {
            out.push_str(&format!("[ghost-triggers]   {v:>8}  {k}\n"));
        }
    }
    {
        let a = accum().lock().unwrap_or_else(|e| e.into_inner());
        if !a.is_empty() {
            let mut rows: Vec<(&String, &(u128, u64))> = a.iter().collect();
            rows.sort_by(|x, y| y.1 .0.cmp(&x.1 .0).then_with(|| x.0.cmp(y.0)));
            out.push_str(&format!("[ghost-triggers {moment}] accumulated time:\n"));
            for (k, (ns, n)) in rows {
                let ms = *ns as f64 / 1e6;
                let avg_us = if *n > 0 { *ns as f64 / *n as f64 / 1e3 } else { 0.0 };
                out.push_str(&format!(
                    "[ghost-triggers]   {ms:>10.1} ms  n={n:<8} avg={avg_us:>9.1} us  {k}\n"
                ));
            }
        }
    }
    {
        let q = quantities().lock().unwrap_or_else(|e| e.into_inner());
        if !q.is_empty() {
            let mut rows: Vec<(&String, &(u128, u64))> = q.iter().collect();
            rows.sort_by(|x, y| y.1 .0.cmp(&x.1 .0).then_with(|| x.0.cmp(y.0)));
            out.push_str(&format!("[ghost-triggers {moment}] quantities:\n"));
            for (k, (total, n)) in rows {
                let avg = if *n > 0 { *total as f64 / *n as f64 } else { 0.0 };
                out.push_str(&format!(
                    "[ghost-triggers]   total={total:<12} n={n:<8} avg={avg:>10.1}  {k}\n"
                ));
            }
        }
    }
    {
        let d = distincts().lock().unwrap_or_else(|e| e.into_inner());
        if !d.is_empty() {
            let c = counters().lock().unwrap_or_else(|e| e.into_inner());
            let mut rows: Vec<(&String, usize)> =
                d.iter().map(|(k, v)| (k, v.len())).collect();
            rows.sort_by(|x, y| y.1.cmp(&x.1).then_with(|| x.0.cmp(y.0)));
            out.push_str(&format!(
                "[ghost-triggers {moment}] distinct keys (total/distinct = repeat factor):\n"
            ));
            for (k, n) in rows {
                let total = c.get(k).copied().unwrap_or(0);
                let factor = if n > 0 { total as f64 / n as f64 } else { 0.0 };
                out.push_str(&format!(
                    "[ghost-triggers]   distinct={n:<8} total={total:<10} x{factor:<8.2} {k}\n"
                ));
            }
        }
    }
    {
        let b = trace_buckets().lock().unwrap_or_else(|e| e.into_inner());
        let sampled = TRACE_MISS_SEQ.load(Ordering::Relaxed);
        let mut rows: Vec<(&String, &u64)> = b.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        out.push_str(&format!(
            "[ghost-triggers {moment}] miss backtrace buckets (every {}th of {} misses):\n",
            trace_every().max(1),
            sampled
        ));
        for (k, v) in rows.iter().take(40) {
            out.push_str(&format!("[ghost-triggers]   {v:>6}  {k}\n"));
        }
    }
    emit_text(&out);
}

/// Emits `emit_all` when it goes out of scope.
///
/// `main`'s CLI arms each `return` on their own, so one guard at the top of
/// `main` reaches every verb without twenty call sites having to remember.
/// It deliberately does NOT cover `std::process::exit`, which skips `Drop`:
/// the server path emits explicitly before its hard exit, and the CLI's
/// `exit(1)`/`exit(2)` arms are argument and I/O errors with no run to report.
pub struct EmitOnDrop(&'static str);

impl EmitOnDrop {
    pub fn new(moment: &'static str) -> Self {
        Self(moment)
    }
}

impl Drop for EmitOnDrop {
    fn drop(&mut self) {
        emit_all(self.0);
    }
}

fn registry() -> &'static Mutex<Vec<Weak<GhostStats>>> {
    static R: OnceLock<Mutex<Vec<Weak<GhostStats>>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Vec::new()))
}

/// Emit every live cache's report now. Wired to LSP shutdown (explicitly,
/// before the hard exit) and to CLI end-of-run (via `EmitOnDrop` in `main`).
/// No-op when the gate is off.
pub fn emit_all(moment: &str) {
    if !enabled() {
        return;
    }
    let regs = registry().lock().unwrap_or_else(|e| e.into_inner());
    for w in regs.iter() {
        if let Some(g) = w.upgrade() {
            g.emit(moment);
        }
    }
    drop(regs);
    emit_attribution(moment);
}

/// Keys-only eviction ledger with lazy ring deletion: `ring` remembers
/// insertion order, `present` counts a key's live occurrences (removal just
/// zeroes the count; stale ring slots fall off the front).
struct Ghost {
    ring: VecDeque<Arc<str>>,
    present: HashMap<Arc<str>, u32>,
    /// key → times it was looked up again after a capacity eviction.
    refetch: HashMap<Arc<str>, u64>,
    /// Keys dropped by INVALIDATION (freshness), kept separately so a
    /// re-decode after an invalidate is attributed to churn, not capacity.
    inval_ring: VecDeque<Arc<str>>,
    inval_present: HashMap<Arc<str>, u32>,
    /// key → times re-decoded after an invalidation dropped it.
    inval_refetch: HashMap<Arc<str>, u64>,
}

pub struct GhostStats {
    label: String,
    live_hits: AtomicU64,
    misses: AtomicU64,
    ghost_hits: AtomicU64,
    evictions: AtomicU64,
    invalidations: AtomicU64,
    /// Misses whose key an INVALIDATION (not capacity) recently dropped.
    inval_refetches: AtomicU64,
    /// High-water marks reported by the owning cache via `set_usage`.
    peak_bytes: AtomicU64,
    peak_entries: AtomicU64,
    emit_seq: AtomicU64,
    inner: Mutex<Ghost>,
}

impl GhostStats {
    /// `None` when the gate is off — callers store `Option<Arc<GhostStats>>`
    /// and every hook is a single `is_some` check on the default path.
    pub fn new_if_enabled(label: String) -> Option<Arc<GhostStats>> {
        if !enabled() {
            return None;
        }
        let g = Arc::new(GhostStats {
            label,
            live_hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            ghost_hits: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            invalidations: AtomicU64::new(0),
            inval_refetches: AtomicU64::new(0),
            peak_bytes: AtomicU64::new(0),
            peak_entries: AtomicU64::new(0),
            emit_seq: AtomicU64::new(0),
            inner: Mutex::new(Ghost {
                ring: VecDeque::new(),
                present: HashMap::new(),
                refetch: HashMap::new(),
                inval_ring: VecDeque::new(),
                inval_present: HashMap::new(),
                inval_refetch: HashMap::new(),
            }),
        });
        registry()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Arc::downgrade(&g));
        Some(g)
    }

    pub fn on_hit(&self) {
        self.live_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// A lookup that must load. If the key sits on the ghost list this is a
    /// refetch: the cache once held it and capacity pressure pushed it out.
    /// The key leaves the ghost list on refetch so one eviction is counted
    /// once per want-again cycle (re-eviction re-enters it).
    pub fn on_miss(&self, key: &str) {
        maybe_trace_miss(&self.label);
        let n = self.misses.fetch_add(1, Ordering::Relaxed) + 1;
        {
            let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            if g.present.get(key).is_some_and(|c| *c > 0) {
                g.present.remove(key);
                self.ghost_hits.fetch_add(1, Ordering::Relaxed);
                let k: Arc<str> = Arc::from(key);
                *g.refetch.entry(k).or_insert(0) += 1;
            } else if g.inval_present.get(key).is_some_and(|c| *c > 0) {
                g.inval_present.remove(key);
                self.inval_refetches.fetch_add(1, Ordering::Relaxed);
                let k: Arc<str> = Arc::from(key);
                *g.inval_refetch.entry(k).or_insert(0) += 1;
            }
        }
        if n % EMIT_EVERY_MISSES == 0 {
            // Summary only — see `report_with`. This fires on a miss COUNT, so
            // on a busy cache it is the highest-frequency emitter in the
            // process; the culprit list belongs at a terminal moment.
            self.emit_with("periodic", Detail::Summary);
        }
    }

    /// A CAPACITY eviction (LRU tail). Invalidation-driven removals must NOT
    /// come here — a refetch after a legitimate freshness drop is not
    /// evidence of cache misbehavior.
    pub fn on_evict(&self, key: &str) {
        self.evictions.fetch_add(1, Ordering::Relaxed);
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let k: Arc<str> = Arc::from(key);
        g.ring.push_back(k.clone());
        *g.present.entry(k).or_insert(0) += 1;
        while g.ring.len() > GHOST_CAP {
            let Some(old) = g.ring.pop_front() else { break };
            if let Some(c) = g.present.get_mut(&old) {
                *c = c.saturating_sub(1);
                if *c == 0 {
                    g.present.remove(&old);
                }
            }
        }
    }

    pub fn on_invalidate(&self, key: &str) {
        self.invalidations.fetch_add(1, Ordering::Relaxed);
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.present.remove(key);
        let k: Arc<str> = Arc::from(key);
        g.inval_ring.push_back(k.clone());
        *g.inval_present.entry(k).or_insert(0) += 1;
        while g.inval_ring.len() > GHOST_CAP {
            let Some(old) = g.inval_ring.pop_front() else { break };
            if let Some(c) = g.inval_present.get_mut(&old) {
                *c = c.saturating_sub(1);
                if *c == 0 {
                    g.inval_present.remove(&old);
                }
            }
        }
    }

    /// High-water gauge from the owning cache (post-insert byte total and
    /// entry count). Monotone max; report-only.
    pub fn set_usage(&self, bytes: u64, entries: u64) {
        self.peak_bytes.fetch_max(bytes, Ordering::Relaxed);
        self.peak_entries.fetch_max(entries, Ordering::Relaxed);
    }

    /// The full report: hit rate, ghost hits, the refetch histogram, and the
    /// top refetched keys by name (the culprits).
    pub fn report(&self, moment: &str) -> String {
        self.report_with(moment, Detail::Full)
    }

    /// `Summary` drops the per-key culprit lists and keeps the two aggregate
    /// lines. The periodic re-emit uses it because it fires every
    /// `EMIT_EVERY_MISSES` and the culprit list barely changes between fires:
    /// on a 138k run that was ~580 repetitions of the same ~30 lines, 17.5k
    /// of the last 20k stderr lines, for information the histogram beside it
    /// already carries. The full list still prints at every terminal moment
    /// (`emit_all` / drop), and a KILLED run keeps `distinct_refetched_keys`
    /// plus the histogram — which is the "few keys many times vs many keys
    /// once" question this module exists to answer.
    pub fn report_with(&self, moment: &str, detail: Detail) -> String {
        let hits = self.live_hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let ghost_hits = self.ghost_hits.load(Ordering::Relaxed);
        let evictions = self.evictions.load(Ordering::Relaxed);
        let invalidations = self.invalidations.load(Ordering::Relaxed);
        let total = hits + misses;
        let rate = if total > 0 { 100.0 * hits as f64 / total as f64 } else { 0.0 };
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let distinct = g.refetch.len();
        // Histogram buckets over per-key refetch counts.
        let mut buckets = [0u64; 7]; // 1, 2, 3-4, 5-8, 9-16, 17-32, 33+
        for &c in g.refetch.values() {
            let b = match c {
                1 => 0,
                2 => 1,
                3..=4 => 2,
                5..=8 => 3,
                9..=16 => 4,
                17..=32 => 5,
                _ => 6,
            };
            buckets[b] += 1;
        }
        let mut top: Vec<(&Arc<str>, &u64)> = g.refetch.iter().collect();
        top.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        let seq = self.emit_seq.fetch_add(1, Ordering::Relaxed);
        let mut out = String::new();
        out.push_str(&format!(
            "[ghost-stats #{seq} {moment}] {label}: lookups={total} live_hits={hits} \
             (rate={rate:.1}%) misses={misses} ghost_hits={ghost_hits} \
             capacity_evictions={evictions} invalidations={invalidations} \
             inval_refetches={ir} ghost_resident={gr} peak_bytes={pb} \
             peak_entries={pe}\n",
            label = self.label,
            ir = self.inval_refetches.load(Ordering::Relaxed),
            gr = g.present.len(),
            pb = self.peak_bytes.load(Ordering::Relaxed),
            pe = self.peak_entries.load(Ordering::Relaxed),
        ));
        out.push_str(&format!(
            "[ghost-stats #{seq}] {label}: distinct_refetched_keys={distinct} \
             refetch_histogram 1x:{} 2x:{} 3-4x:{} 5-8x:{} 9-16x:{} 17-32x:{} 33+x:{}\n",
            buckets[0], buckets[1], buckets[2], buckets[3], buckets[4], buckets[5], buckets[6],
            label = self.label,
        ));
        if matches!(detail, Detail::Full) {
            for (k, c) in top.iter().take(20) {
                out.push_str(&format!(
                    "[ghost-stats #{seq}] {label}: refetched {c}x  {k}\n",
                    label = self.label
                ));
            }
            let mut itop: Vec<(&Arc<str>, &u64)> = g.inval_refetch.iter().collect();
            itop.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
            for (k, c) in itop.iter().take(10) {
                out.push_str(&format!(
                    "[ghost-stats #{seq}] {label}: inval-refetched {c}x  {k}\n",
                    label = self.label
                ));
            }
        }
        out
    }

    pub fn emit(&self, moment: &str) {
        self.emit_with(moment, Detail::Full)
    }

    pub fn emit_with(&self, moment: &str, detail: Detail) {
        let text = self.report_with(moment, detail);
        match sink() {
            Sink::Off => {}
            Sink::Stderr => eprint!("{text}"),
            Sink::File(path) => {
                use std::io::Write;
                if let Ok(mut f) =
                    std::fs::OpenOptions::new().create(true).append(true).open(path)
                {
                    let _ = f.write_all(text.as_bytes());
                }
            }
        }
    }
}

impl Drop for GhostStats {
    fn drop(&mut self) {
        // Best-effort final flush for caches that do get dropped (the
        // statics and anything alive at process kill rely on the periodic
        // + emit_all paths instead).
        if self.live_hits.load(Ordering::Relaxed) + self.misses.load(Ordering::Relaxed) > 0 {
            self.emit("drop");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bare(label: &str) -> GhostStats {
        GhostStats {
            label: label.to_string(),
            live_hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            ghost_hits: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
            invalidations: AtomicU64::new(0),
            inval_refetches: AtomicU64::new(0),
            peak_bytes: AtomicU64::new(0),
            peak_entries: AtomicU64::new(0),
            emit_seq: AtomicU64::new(0),
            inner: Mutex::new(Ghost {
                ring: VecDeque::new(),
                present: HashMap::new(),
                refetch: HashMap::new(),
                inval_ring: VecDeque::new(),
                inval_present: HashMap::new(),
                inval_refetch: HashMap::new(),
            }),
        }
    }

    /// End-to-end through the env gate: MUST run in isolation
    /// (`cargo test ghost_emit_writes_file`) — any earlier sink() call in the
    /// same process wins the OnceLock and this test's set_var is ignored.
    #[test]
    fn ghost_emit_writes_file() {
        let path = std::env::temp_dir().join(format!("ghost_probe_{}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        std::env::set_var("PERL_LSP_GHOST_STATS", path.display().to_string());
        if !enabled() {
            // Another test initialized the sink first; nothing to assert here.
            return;
        }
        let g = GhostStats::new_if_enabled("probe".into()).expect("gate on");
        g.on_miss("/k");
        g.on_evict("/k");
        g.on_miss("/k");
        emit_all("test");
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(s.contains("probe"), "report names the cache: {s}");
        assert!(s.contains("ghost_hits=1"), "refetch counted: {s}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn refetch_counts_accumulate_per_evict_cycle() {
        let g = bare("t");
        g.on_miss("/a"); // cold: not a ghost hit
        assert_eq!(g.ghost_hits.load(Ordering::Relaxed), 0);
        g.on_evict("/a");
        g.on_miss("/a"); // refetch 1
        g.on_evict("/a");
        g.on_miss("/a"); // refetch 2
        assert_eq!(g.ghost_hits.load(Ordering::Relaxed), 2);
        let inner = g.inner.lock().unwrap();
        assert_eq!(inner.refetch.get("/a").copied(), Some(2));
    }

    #[test]
    fn invalidation_is_not_a_ghost_hit() {
        let g = bare("t");
        g.on_evict("/a");
        g.on_invalidate("/a");
        g.on_miss("/a");
        assert_eq!(g.ghost_hits.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn ring_is_bounded_with_lazy_deletion() {
        let g = bare("t");
        for i in 0..(GHOST_CAP + 100) {
            g.on_evict(&format!("/k{i}"));
        }
        let inner = g.inner.lock().unwrap();
        assert!(inner.ring.len() <= GHOST_CAP);
        assert!(!inner.present.contains_key("/k0"), "oldest keys aged out");
        assert!(inner.present.contains_key(&format!("/k{}", GHOST_CAP + 99)[..]));
    }
}

#[cfg(test)]
mod reemit_tests {
    use super::*;

    /// The counter block used to reach the sink only via `emit_all`, which is
    /// wired to a CLEAN shutdown. A long run that is killed therefore lost
    /// every counter — including the ones that say whether a measurement was
    /// valid at all. The interval rule is what makes the periodic re-emit
    /// fire; the wiring is exercised by the counter path itself.
    #[test]
    fn a_reemit_is_due_only_after_the_interval() {
        let iv = ATTRIBUTION_REEMIT_MILLISECONDS;
        assert!(!attribution_reemit_due(0, 0, iv));
        assert!(!attribution_reemit_due(iv - 1, 0, iv), "just inside the interval");
        assert!(attribution_reemit_due(iv, 0, iv), "exactly at the interval");
        assert!(attribution_reemit_due(iv * 10, iv * 9, iv));
        // A clock that appears to move backwards must not suppress emission
        // forever — saturating, not wrapping.
        assert!(!attribution_reemit_due(5, 10_000, iv));
        assert!(attribution_reemit_due(10_000 + iv, 10_000, iv));
    }
}

thread_local! {
    /// Set while the ancestor walk's candidate loop is fetching. The loop is
    /// in the Model layer and the rehydrate site is in Index, so the marker
    /// lives HERE — `util` is the neutral leaf every layer may import, and
    /// routing it through `index` made Model import Index, which
    /// `layering_tests::imports_flow_down_only` correctly rejected.
    ///
    /// It exists because the loop's own fetch count and the rehydrate site's
    /// miss count have different denominators: most fetches are absorbed by
    /// the sweep memo and never pay a rehydrate. Only the miss site can say
    /// which misses the loop owns.
    static ANCESTOR_WALK: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Run `f` marked as the ancestor walk's candidate fetch.
pub fn in_ancestor_walk<R>(f: impl FnOnce() -> R) -> R {
    ANCESTOR_WALK.with(|c| c.set(c.get() + 1));
    let out = f();
    ANCESTOR_WALK.with(|c| c.set(c.get() - 1));
    out
}

pub fn inside_ancestor_walk() -> bool {
    ANCESTOR_WALK.with(|c| c.get()) > 0
}
