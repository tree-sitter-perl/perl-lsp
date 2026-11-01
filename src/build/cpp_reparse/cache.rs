//! GatherCache — the one byte-capped, single-flight memo behind the four
//! cpp gather caches — plus byte accounting, on-disk persistence, and the
//! `included_macros` entry point.

use super::*;

// ============================================================================
// GatherCache — the ONE byte-capped, single-flight memo all four cpp gather
// caches (macro table, pre-expanded external, header parse, include closure)
// instantiate. It replaces the bare `OnceLock<Mutex<HashMap>>` those four used
// to be (unbounded growth, check-release-compute-insert races). Two properties,
// coupled by design (`docs/adr/memory-slice-2-lru.md`, the residency discipline
// in CLAUDE.md, hitlist H9-3):
//
//   * SINGLE-FLIGHT population. The first worker to miss a key CLAIMS it and
//     computes; siblings expanding the same header cone (op.c/sv.c share ~90% of
//     their include closure) BLOCK on the claimant's result via a condvar
//     instead of each recomputing the whole expansion. One spelling, four
//     caches — never hand-rolled per cache (rule #10's spirit).
//   * BYTE-ACCOUNTED LRU cap. Retention is bounded by `cap_bytes`; the LRU tail
//     is evicted on insert, never the just-inserted key (a single oversized
//     entry over the whole cap still resolves the query it was loaded for — the
//     `PackBagCache` rule). A cap of 0 means never retain (compute-and-drop).
//
// The two are coupled: a cap makes eviction real, which makes recompute storms
// possible on an evicted shared cone — single-flight collapses each storm to one
// flight. Explicit invalidation (`evict_gather_caches`) removes matching entries
// AND cancels any in-flight compute for those keys (the claimant's now-stale
// result is dropped on publish; a waiter recomputes fresh). No deadlock: the
// state lock is NEVER held across a compute, and invalidation only touches the
// lock.

/// What a single-flight compute produced. `Store` caches the value (byte-
/// accounted, LRU-evicted); `Transient` returns it to the caller WITHOUT caching
/// (a degraded / incomplete result that must re-derive next call). A compute
/// returning `None` (the `try` variant) is a MISS — cache nothing, yield nothing.
pub(super) enum Fill<V> {
    Store(V, usize),
    Transient(V),
}

/// How a `resolve` call settled — lets a caller distinguish a cached answer
/// (hit or freshly stored: authoritative/complete) from a transient one.
pub(super) enum Resolution {
    Cached,
    Transient,
    Missed,
}

struct GatherEntry<S, V> {
    stamp: S,
    value: V,
    bytes: usize,
    last_used: u64,
}

struct GatherState<K, S, V> {
    entries: HashMap<K, GatherEntry<S, V>>,
    /// Keys with a compute currently running (their claimant owns population).
    in_flight: std::collections::HashSet<K>,
    /// In-flight keys an invalidation targeted mid-compute — the claimant drops
    /// its result on publish so a stale table can't land after the invalidate.
    cancelled: std::collections::HashSet<K>,
    total_bytes: usize,
    clock: u64,
}

pub struct GatherCache<K, S, V> {
    state: std::sync::Mutex<GatherState<K, S, V>>,
    ready: std::sync::Condvar,
    cap_bytes: usize,
    /// Report-only ghost-list accounting (`PERL_LSP_GHOST_STATS`). `None`
    /// when the gate is off — no cache decision ever reads this.
    ghost: Option<std::sync::Arc<crate::util::ghost_stats::GhostStats>>,
}

/// Releases an in-flight claim (and clears any cancel marker) even if `compute`
/// panics — otherwise the key would stay `in_flight` forever and every waiter
/// would wedge on the condvar. The success path disarms it after publishing
/// under the lock.
struct FlightGuard<'a, K, S, V>
where
    K: Eq + std::hash::Hash + Clone,
{
    cache: &'a GatherCache<K, S, V>,
    key: &'a K,
    armed: bool,
}

impl<K, S, V> Drop for FlightGuard<'_, K, S, V>
where
    K: Eq + std::hash::Hash + Clone,
{
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(mut st) = self.cache.state.lock() {
            st.in_flight.remove(self.key);
            st.cancelled.remove(self.key);
        }
        self.cache.ready.notify_all();
    }
}

impl<K, S, V> GatherCache<K, S, V>
where
    K: Eq + std::hash::Hash + Clone + std::fmt::Debug,
    S: PartialEq + Clone,
    V: Clone,
{
    /// Production sites use `new_labeled` so ghost-stats reports name their
    /// tier; the unlabeled form serves tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) fn new(cap_bytes: usize) -> Self {
        Self::new_labeled(cap_bytes, "gather")
    }

    /// `new` with a report label naming the tier so ghost-stats output
    /// attributes traffic to the right cache.
    pub(super) fn new_labeled(cap_bytes: usize, label: &str) -> Self {
        GatherCache {
            state: std::sync::Mutex::new(GatherState {
                entries: HashMap::new(),
                in_flight: std::collections::HashSet::new(),
                cancelled: std::collections::HashSet::new(),
                total_bytes: 0,
                clock: 0,
            }),
            ready: std::sync::Condvar::new(),
            cap_bytes,
            ghost: crate::util::ghost_stats::GhostStats::new_if_enabled(format!(
                "{label} cap={}MiB",
                cap_bytes / (1024 * 1024)
            )),
        }
    }

    /// Fetch the stamp-matching cached value or single-flight compute it; the
    /// compute always yields a value (`Store`/`Transient`), so this never misses.
    pub(super) fn get_or_fill<F>(&self, key: K, stamp: S, compute: F) -> V
    where
        F: FnOnce() -> Fill<V>,
    {
        self.resolve(key, stamp, || Some(compute()))
            .0
            .expect("get_or_fill compute always yields a value")
    }

    /// Fetch or single-flight compute. `compute` returns `None` for a MISS
    /// (cache nothing, yield `None` — e.g. the on-open cached-only skip, or a
    /// header that failed to read).
    pub(super) fn get_or_try_fill<F>(&self, key: K, stamp: S, compute: F) -> Option<V>
    where
        F: FnOnce() -> Option<Fill<V>>,
    {
        self.resolve(key, stamp, compute).0
    }

    /// The single-flight + byte-cap core. Returns the value plus how it settled.
    pub(super) fn resolve<F>(&self, key: K, stamp: S, compute: F) -> (Option<V>, Resolution)
    where
        F: FnOnce() -> Option<Fill<V>>,
    {
        // 1. Acquire the key. A stamp-matching entry is a hit; a live in-flight
        //    compute is waited on (the whole point — no duplicate expansion),
        //    EXCEPT from a rayon worker, which duplicates instead of parking;
        //    otherwise claim the key so siblings coalesce onto our compute.
        {
            let mut st = self.state.lock().expect("gather cache poisoned");
            loop {
                let fresh = st
                    .entries
                    .get(&key)
                    .filter(|e| e.stamp == stamp)
                    .map(|e| e.value.clone());
                if let Some(v) = fresh {
                    st.clock += 1;
                    let c = st.clock;
                    if let Some(e) = st.entries.get_mut(&key) {
                        e.last_used = c;
                    }
                    if let Some(g) = &self.ghost {
                        g.on_hit();
                    }
                    return (Some(v), Resolution::Cached);
                }
                if st.in_flight.contains(&key) {
                    // A rayon pool worker must NEVER block on this condvar: the
                    // claimant may be an off-pool thread whose compute needs
                    // this very pool (the gather's level `par_iter`), and a
                    // worker parked here freezes every stack-suspended
                    // continuation beneath it — rayon's work-stealing runs jobs
                    // inline during joins, so the claimant's injected pieces
                    // can be buried under the blocked frame and the pool
                    // wedges whole (the fmt cold-start deadlock: bulk-index
                    // worker waits on the open-doc gather's claim, the gather
                    // waits on the pool). A worker computes a private
                    // duplicate instead — correct value for this caller, no
                    // publish (the claimant owns population, and an
                    // invalidation racing us targets only the claimant's
                    // in-flight entry, so an unclaimed store could land
                    // stale). Coalescing is preserved for every non-pool
                    // waiter, where blocking is safe.
                    if rayon::current_thread_index().is_some() {
                        drop(st);
                        if let Some(g) = &self.ghost {
                            g.on_miss(&format!("{key:?}"));
                        }
                        return match compute() {
                            // Store means the compute succeeded completely —
                            // the value is authoritative even though only the
                            // claimant's copy enters the cache.
                            Some(Fill::Store(v, _)) => (Some(v), Resolution::Cached),
                            Some(Fill::Transient(v)) => (Some(v), Resolution::Transient),
                            None => (None, Resolution::Missed),
                        };
                    }
                    st = self.ready.wait(st).expect("gather cache poisoned");
                    continue;
                }
                st.in_flight.insert(key.clone());
                break;
            }
        }

        // 2. Compute with NO lock held (siblings block on the condvar meanwhile).
        if let Some(g) = &self.ghost {
            g.on_miss(&format!("{key:?}"));
        }
        let mut guard = FlightGuard { cache: self, key: &key, armed: true };
        let outcome = compute();

        // 3. Publish under the lock. An invalidation that landed for this key
        //    mid-compute (recorded in `cancelled`) drops our stale result.
        let mut st = self.state.lock().expect("gather cache poisoned");
        st.in_flight.remove(&key);
        let cancelled = st.cancelled.remove(&key);
        guard.armed = false;
        let out = match outcome {
            Some(Fill::Store(v, bytes)) => {
                if !cancelled && self.cap_bytes > 0 {
                    if let Some(old) = st.entries.remove(&key) {
                        st.total_bytes -= old.bytes;
                    }
                    st.clock += 1;
                    let c = st.clock;
                    st.total_bytes += bytes;
                    st.entries.insert(
                        key.clone(),
                        GatherEntry { stamp, value: v.clone(), bytes, last_used: c },
                    );
                    self.evict_to_cap(&mut st, &key);
                    if let Some(g) = &self.ghost {
                        g.set_usage(st.total_bytes as u64, st.entries.len() as u64);
                    }
                }
                (Some(v), Resolution::Cached)
            }
            Some(Fill::Transient(v)) => (Some(v), Resolution::Transient),
            None => (None, Resolution::Missed),
        };
        drop(st);
        self.ready.notify_all();
        out
    }

    /// Drop LRU-tail entries until resident bytes are within cap. Never evicts
    /// `keep` (the just-inserted key), matching `PackBagCache::evict_to_cap`.
    fn evict_to_cap(&self, st: &mut GatherState<K, S, V>, keep: &K) {
        while st.total_bytes > self.cap_bytes {
            let victim = st
                .entries
                .iter()
                .filter(|&(k, _)| k != keep)
                .min_by_key(|(_, e)| e.last_used)
                .map(|(k, _)| k.clone());
            let Some(victim) = victim else { break };
            if let Some(e) = st.entries.remove(&victim) {
                st.total_bytes -= e.bytes;
                if let Some(g) = &self.ghost {
                    g.on_evict(&format!("{victim:?}"));
                }
            }
        }
    }

    /// Drop every entry whose key satisfies `pred` and cancel any in-flight
    /// compute for such a key (its result is discarded on publish; a waiter
    /// recomputes fresh). Holds only the state lock — never a compute — so it
    /// can't deadlock a worker waiting on the condvar.
    pub(super) fn invalidate<P: Fn(&K) -> bool>(&self, pred: P) {
        let mut st = self.state.lock().expect("gather cache poisoned");
        let victims: Vec<K> = st.entries.keys().filter(|k| pred(k)).cloned().collect();
        for k in victims {
            if let Some(e) = st.entries.remove(&k) {
                st.total_bytes -= e.bytes;
                if let Some(g) = &self.ghost {
                    g.on_invalidate(&format!("{k:?}"));
                }
            }
        }
        let flight: Vec<K> = st.in_flight.iter().filter(|k| pred(k)).cloned().collect();
        for k in flight {
            st.cancelled.insert(k);
        }
        drop(st);
        self.ready.notify_all();
    }

    /// `(entries, resident_bytes)` — the exact accounted footprint (diagnostic).
    pub(super) fn stats(&self) -> (usize, usize) {
        let st = self.state.lock().expect("gather cache poisoned");
        (st.entries.len(), st.total_bytes)
    }
}

/// Per-cache byte cap. Each of the four gather caches gets its own default
/// (justified at its constructor); `PERL_LSP_GATHER_CACHE_MB` overrides ALL of
/// them to one value (0 ⇒ never retain — the most aggressive footprint, for
/// A/B'ing the cap's cost). Mirrors the `maxCacheMb` / `PERL_LSP_*` precedents.
pub(super) fn gather_cap_bytes(default_mb: usize) -> usize {
    let mb = std::env::var("PERL_LSP_GATHER_CACHE_MB")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(default_mb);
    mb.saturating_mul(1024 * 1024)
}

fn macro_heap_bytes(m: &Macro) -> usize {
    m.body.capacity()
        + m.params.as_ref().map_or(0, |p| p.iter().map(|s| s.capacity() + 24).sum())
        + m.guards.iter().map(|t| t.heap_bytes() + 32).sum::<usize>()
        + m.variant_delegates.iter().map(|s| s.capacity() + 24).sum::<usize>()
        + 72
}

pub(super) fn macro_table_heap_bytes(t: &MacroTable) -> usize {
    t.iter().map(|(k, v)| k.capacity() + macro_heap_bytes(v) + 32).sum()
}

pub(super) fn strings_heap_bytes<S: AsRef<str>>(v: &[S]) -> usize {
    v.iter().map(|s| s.as_ref().len() + 24).sum()
}

/// `header_cache` default: 128 MiB. Shared across ALL files (deduped by header
/// PATH, not by consuming file) and NOT dropped by the bulk-index
/// `evict_gather_caches_keep_headers` — it lives for the whole session and is
/// the highest-reuse, lowest-cost tier (~2.6 KB/header measured on re2), so 128
/// MiB holds ~50K distinct headers before the LRU trims the cold tail.
pub(super) const HEADER_CACHE_MB: usize = 128;
/// `macro_table_cache` default: 128 MiB. Per-file raw merged closure table
/// (perl.h ≈ 2000 macros); 128 MiB matches the PackBagCache/enrichment-overlay
/// budget class for the hottest gather tier.
const MACRO_TABLE_CACHE_MB: usize = 128;
/// `pre_expanded_cache` default: 128 MiB. Full+alias mutual pre-expansion ON
/// TOP of the raw table — the biggest per-entry payload; same 128 MiB class.
pub(super) const PRE_EXPANDED_CACHE_MB: usize = 128;
/// `include_closure_cache` default: 64 MiB. Per-file path-string lists only
/// (~37 KB/file on abseil), so 64 MiB holds ~1700 files' closures — the
/// smallest per-entry tier gets the smaller cap.
pub(super) const INCLUDE_CLOSURE_CACHE_MB: usize = 64;

pub(super) fn macro_table_cache() -> &'static GatherCache<std::path::PathBuf, u64, std::sync::Arc<MacroTable>> {
    static C: OnceLock<GatherCache<std::path::PathBuf, u64, std::sync::Arc<MacroTable>>> =
        OnceLock::new();
    C.get_or_init(|| {
        GatherCache::new_labeled(gather_cap_bytes(MACRO_TABLE_CACHE_MB), "gather-macro-table")
    })
}

/// Hash of the file's `#include` directives — the cache key's variable part.
/// Cheap (one line scan); stable across edits that don't touch includes.
pub(super) fn include_set_hash(file_path: &std::path::Path, src: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // What the includes resolve to moves with the compilation database.
    crate::build::cpp_includes::roots_fingerprint(file_path).hash(&mut h);
    for line in src.lines() {
        let t = line.trim_start();
        if t.starts_with('#') && t[1..].trim_start().starts_with("include") {
            t.hash(&mut h);
        }
    }
    h.finish()
}

/// Bump when the persisted macro-table format or the gather's semantics
/// change in a way that invalidates on-disk blobs.
const MACRO_CACHE_VERSION: i64 = 6;

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedMacros {
    include_hash: u64,
    version: i64,
    /// The toolchain the gather resolved system includes against — a probe
    /// failure (or a compiler upgrade) changes which headers the closure
    /// reaches, so a table built under one toolchain must not validate
    /// under another.
    toolchain: u64,
    /// Every transitively-#included header + its content stamp — the table
    /// is valid only while none of them changed (cross-session correctness;
    /// the in-memory cache leans on include_hash alone within a session).
    headers: Vec<(std::path::PathBuf, i64)>,
    table: MacroTable,
}

/// On-disk macro-table cache dir, set once at startup (the CLI / LSP know
/// the workspace root → cache dir). `None` ⇒ persistence off (tests).
fn macro_persist_dir() -> &'static std::sync::OnceLock<Option<std::path::PathBuf>> {
    static D: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    &D
}

/// Point the persisted macro cache at a workspace's cache dir (a `macros/`
/// subdir under it). Idempotent; first call wins.
pub fn set_macro_persist_dir(workspace_cache_dir: Option<std::path::PathBuf>) {
    let resolved = workspace_cache_dir.map(|d| {
        let p = d.join("macros");
        let _ = std::fs::create_dir_all(&p);
        p
    });
    let _ = macro_persist_dir().set(resolved);
}

fn persist_path(file_path: &std::path::Path) -> Option<std::path::PathBuf> {
    use std::hash::{Hash, Hasher};
    let dir = macro_persist_dir().get()?.clone()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    file_path.hash(&mut h);
    Some(dir.join(format!("{:016x}.bin", h.finish())))
}

fn load_persisted(file_path: &std::path::Path, inc_hash: u64) -> Option<MacroTable> {
    let p = persist_path(file_path)?;
    let raw = zstd::decode_all(std::fs::read(&p).ok()?.as_slice()).ok()?;
    let pm: PersistedMacros = bincode::deserialize(&raw).ok()?;
    if pm.include_hash != inc_hash
        || pm.version != MACRO_CACHE_VERSION
        || pm.toolchain != toolchain_fingerprint()
    {
        return None;
    }
    if pm.headers.iter().any(|(hp, st)| file_stamp(hp) != *st) {
        return None; // a header changed on disk
    }
    Some(pm.table)
}

fn save_persisted(
    file_path: &std::path::Path,
    inc_hash: u64,
    headers: Vec<(std::path::PathBuf, i64)>,
    table: &MacroTable,
) {
    let Some(p) = persist_path(file_path) else { return };
    let pm = PersistedMacros {
        include_hash: inc_hash,
        version: MACRO_CACHE_VERSION,
        toolchain: toolchain_fingerprint(),
        headers,
        table: table.clone(),
    };
    if let Ok(raw) = bincode::serialize(&pm) {
        if let Ok(z) = zstd::encode_all(raw.as_slice(), 3) {
            let _ = std::fs::write(&p, z);
        }
    }
}

thread_local! {
    /// When set on the current thread, `included_macros*` skip the cold gather.
    static GATHER_CACHED_ONLY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// When set on the current thread, `included_macros*` return whatever's cached
/// (in-mem or on-disk) but SKIP the cold gather — yielding an empty external
/// set (degraded) instead of blocking. `did_open` sets this so the first
/// analyze of a macro-heavy file is instant; a background task then runs the
/// real gather and re-analyzes (the async-refresh path).
pub fn set_gather_cached_only(v: bool) {
    GATHER_CACHED_ONLY.with(|c| c.set(v));
}
pub(super) fn gather_cached_only() -> bool {
    GATHER_CACHED_ONLY.with(|c| c.get())
}

pub fn included_macros(
    file_path: &std::path::Path,
    src: &str,
    parser: &mut tree_sitter::Parser,
) -> std::sync::Arc<MacroTable> {
    included_macros_inner(file_path, src, parser, true)
        .unwrap_or_else(|| std::sync::Arc::new(MacroTable::new()))
}

/// The three-tier lookup. `allow_cold=false` (the on-open path) stops after the
/// two cache tiers, returning `None` rather than paying the cold gather — the
/// caller degrades to an empty external set and lets a background task warm it.
pub(super) fn included_macros_inner(
    file_path: &std::path::Path,
    src: &str,
    parser: &mut tree_sitter::Parser,
    allow_cold: bool,
) -> Option<std::sync::Arc<MacroTable>> {
    let key = file_path.to_path_buf();
    let inc_hash = include_set_hash(file_path, src);
    // Tier 1 (in-memory, this session) IS the GatherCache hit. On a miss the
    // single-flight claimant runs tiers 2+3; siblings on the same key wait for
    // its result rather than re-paying the cold gather.
    macro_table_cache().get_or_try_fill(key, inc_hash, || {
        // Tier 2: on-disk (across sessions) — kills the cold-start gather.
        if let Some(table) = load_persisted(file_path, inc_hash) {
            let arc = std::sync::Arc::new(table);
            let bytes = macro_table_heap_bytes(&arc);
            return Some(Fill::Store(arc, bytes));
        }
        if !allow_cold {
            return None; // on-open: don't block on the cold gather
        }
        // Tier 3: gather cold, warm disk + this cache.
        let (table, headers) = gather_included_macros(file_path, src, parser);
        save_persisted(file_path, inc_hash, headers, &table);
        let arc = std::sync::Arc::new(table);
        let bytes = macro_table_heap_bytes(&arc);
        Some(Fill::Store(arc, bytes))
    })
}
