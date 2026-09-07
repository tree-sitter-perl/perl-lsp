//! Byte-capped LRU of rehydrated pack `FileAnalysis`es — the Slice-2
//! rehydration store (`docs/adr/memory-slice-2-lru.md`).
//!
//! After a pack workspace is indexed, every resident pack-index copy of a
//! `FileAnalysis` has its witness bag evicted (the fold already baked its
//! conclusions into pinned fields). The FULL bag stays on disk in the per-lang
//! SQLite blob. When a TYPE query reaches into an evicted file's bag, the pack
//! `ModuleIndex` asks this cache for a bag-present copy: hit → the retained
//! Arc; miss → a keyed single-file decode from SQLite into the LRU.
//!
//! The cap is BYTE-based (`maxCacheMb`), not count-based: cpp bags average
//! ~700 KB/file (10–100× a Perl module), so a count cap would either thrash or
//! blow the footprint. `cap_bytes == 0` disables retention (rehydrate-and-drop)
//! for the most aggressive footprint.
//!
//! Concurrency: a `DashMap` + atomic recency clock. `bag_for` never holds a
//! shard guard across the SQLite decode (it reads/inserts under short per-shard
//! locks) and never across an `.await` (it is fully synchronous), so it adds no
//! guard-across-await hazard (`filestore-guard-discipline`). Two threads racing
//! to rehydrate the same path produce equal Arcs from the same blob; last
//! insert wins, no correctness impact.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use dashmap::DashMap;

use crate::model::file_analysis::FileAnalysis;
use crate::index::module_cache::RehydrateMiss;

type Loader = Box<dyn Fn(&Path, bool) -> Result<FileAnalysis, RehydrateMiss> + Send + Sync>;

pub struct PackBagCache {
    /// Rehydrated, bag-present analyses keyed by canonical path, each paired
    /// with the byte size CHARGED for it. The size travels with the entry so
    /// a removal always credits back exactly what its insert debited — a
    /// re-measured `heap_estimate` (or a lost race) would otherwise leave the
    /// counter drifting upward, and this counter gates eviction.
    entries: DashMap<PathBuf, (Arc<FileAnalysis>, usize)>,
    /// Last-touch stamp per retained path — the LRU recency source.
    recency: DashMap<PathBuf, u64>,
    /// Monotone recency clock; every touch bumps it.
    clock: AtomicU64,
    /// Sum of retained analyses' charged payload. Kept exactly equal to the
    /// sum of `entries`' charges; `resync_bytes` repairs it if a concurrent
    /// interleaving ever breaks that, because an over-count is a one-way
    /// ratchet that collapses the cache to a single entry.
    bytes: AtomicUsize,
    /// How many times `resync_bytes` found the charge total actually wrong
    /// and repaired it. The ghost-stats counter reports the same event but
    /// only under `PERL_LSP_GHOST_STATS`; this one is always live so the
    /// alarm is assertable.
    resyncs: AtomicUsize,
    /// `maxCacheMb * 1 MiB`. `0` ⇒ never retain (rehydrate-and-drop).
    /// Monotone after construction (`raise_cap`): a batch sweep sizes the
    /// cache up to its working set, never down — a cap that shrinks below
    /// one entry is the `evict_to_cap` collapse (`docs/adr/memory-slice-2-lru.md`).
    cap_bytes: AtomicUsize,
    /// Per-path invalidation generation: `invalidate` bumps, a loading
    /// `bag_for` records the value BEFORE its decode and only retains when
    /// it is unchanged after — otherwise a decode racing a writer's
    /// commit+invalidate would insert the PREVIOUS generation just after
    /// the invalidate fired, pinning stale spans until the next edit.
    generation: DashMap<PathBuf, u64>,
    /// Keyed single-file decode (opens the pack SQLite conn on demand).
    loader: Loader,
    /// Report-only ghost-list accounting (`PERL_LSP_GHOST_STATS`). `None`
    /// when the gate is off — no cache decision ever reads this.
    ghost: Option<Arc<crate::util::ghost_stats::GhostStats>>,
}

impl PackBagCache {
    /// Production sites use `new_labeled` so ghost-stats reports name their
    /// tier; the unlabeled form serves tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(
        cap_bytes: usize,
        loader: impl Fn(&Path, bool) -> Result<FileAnalysis, RehydrateMiss> + Send + Sync + 'static,
    ) -> Self {
        Self::new_labeled(cap_bytes, "pack-bag", loader)
    }

    /// `new` with a report label naming the owning tier (hub vs per-lang
    /// sub-index) so the ghost-stats output attributes traffic correctly.
    pub fn new_labeled(
        cap_bytes: usize,
        label: &str,
        loader: impl Fn(&Path, bool) -> Result<FileAnalysis, RehydrateMiss> + Send + Sync + 'static,
    ) -> Self {
        PackBagCache {
            entries: DashMap::new(),
            recency: DashMap::new(),
            clock: AtomicU64::new(0),
            bytes: AtomicUsize::new(0),
            resyncs: AtomicUsize::new(0),
            cap_bytes: AtomicUsize::new(cap_bytes),
            generation: DashMap::new(),
            loader: Box::new(loader),
            ghost: crate::util::ghost_stats::GhostStats::new_if_enabled(format!(
                "{label} cap={}MiB",
                cap_bytes / (1024 * 1024)
            )),
        }
    }

    /// Current cap in bytes.
    pub fn cap_bytes(&self) -> usize {
        self.cap_bytes.load(Ordering::Relaxed)
    }

    /// Raise the cap to `to` — a no-op when the cap is already at least
    /// that (monotone by design, see `cap_bytes`). Returns the cap in
    /// effect afterwards.
    pub fn raise_cap(&self, to: usize) -> usize {
        self.cap_bytes.fetch_max(to, Ordering::Relaxed).max(to)
    }

    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// A bag-present analysis for `path`: retained Arc if resident, else decode
    /// the exact persisted blob from SQLite into the LRU and return it. Returns
    /// `None` only when the loader can't produce the file (no row / decode
    /// failure) — the caller then degrades to the bag-less resident copy.
    pub fn bag_for(&self, path: &Path) -> Option<Arc<FileAnalysis>> {
        self.bag_for_diag(path).ok()
    }

    /// `bag_for` that surfaces the loader's discriminated failure so the
    /// strict-residency tripwire can name WHY rehydration missed instead of
    /// collapsing every cause into "loader returned None".
    pub fn bag_for_diag(&self, path: &Path) -> Result<Arc<FileAnalysis>, RehydrateMiss> {
        self.get_or_load(path, true)
    }

    /// The rows-axes flavor of `bag_for`: refs + symbols GUARANTEED present,
    /// the bag not promised. A miss decodes the same whole blob but strips the
    /// bag BEFORE retaining, so backward-walk traffic (whose matcher reads
    /// refs + symbols only) caches at roughly half the bytes per entry — the
    /// witness bag is ~half a Perl analysis's heap — under the SAME byte cap,
    /// one budget, one recency clock, one invalidation. A whole entry already
    /// resident answers as-is (superset); a stripped entry never satisfies a
    /// later `bag_for` (it re-decodes whole and replaces the entry).
    pub fn rows_for(&self, path: &Path) -> Option<Arc<FileAnalysis>> {
        self.get_or_load(path, false).ok()
    }

    /// `rows_for` with the discriminated miss, for the strict-residency
    /// tripwire — same pairing as `bag_for` / `bag_for_diag`.
    pub fn rows_for_diag(&self, path: &Path) -> Result<Arc<FileAnalysis>, RehydrateMiss> {
        self.get_or_load(path, false)
    }

    fn get_or_load(&self, path: &Path, want_bag: bool) -> Result<Arc<FileAnalysis>, RehydrateMiss> {
        if let Some(hit) = self.entries.get(path) {
            let arc = hit.value().0.clone();
            drop(hit);
            // A stripped entry (retained for a rows-axes reader) cannot serve
            // a bag request — fall through to a whole re-decode that REPLACES
            // it. Serving it would be absence-by-eviction: the type query
            // reads an empty bag as "no type facts", silently.
            if !want_bag || !arc.bag_is_evicted() {
                self.recency.insert(path.to_path_buf(), self.tick());
                if let Some(g) = &self.ghost {
                    g.on_hit();
                }
                crate::util::ghost_stats::count("bagcache.hit");
                crate::util::ghost_stats::count_attributed("bagcache_hit");
                return Ok(arc);
            }
            // Resident, but stripped — a rows-axes reader retained it and a
            // bag request cannot be served from it.
            crate::util::ghost_stats::count("bagcache.miss_stripped_resident");
            crate::util::ghost_stats::count_attributed("bagcache_miss");
        } else {
            crate::util::ghost_stats::count("bagcache.miss_absent");
            crate::util::ghost_stats::count_attributed("bagcache_miss");
        }
        if let Some(g) = &self.ghost {
            g.on_miss(&path.to_string_lossy());
        }
        // The axis of an actual DECODE. Deliberately here and NOT at
        // `rehydrate_axes_or_resident`, which is where the `want_bag`
        // parameter lives and where it is tempting to count: at a 99.2% hit
        // rate that denominator is 1.66M lookups against 14.8k decodes, and
        // it answers 84% rows-axis where the decodes say 69%. Only a decode
        // can waste anything, so only decodes are counted.
        crate::util::ghost_stats::count(if want_bag {
            "decode.want_bag"
        } else {
            "decode.rows_only"
        });
        let gen_before = self.generation.get(path).map(|g| *g).unwrap_or(0);
        // The axis goes DOWN to the loader rather than being applied to what
        // it returns. Decoding the bag and then dropping it was 52.9% of the
        // bincode bytes on a path where 94.9% of decodes never wanted it.
        let mut loaded = crate::util::ghost_stats::timed(
            "bagcache.decode", || (self.loader)(path, want_bag))?;
        // The strip STAYS, even though the loader was asked for the narrow
        // axis. It is not where the saving lives — that is in the bytes SQL
        // never fetched and bincode never walked — and dropping it would make
        // "a rows-lane entry retains without the bag" depend on every loader
        // remembering to honour the flag. A loader that returns a superset is
        // within its contract; the cache still owes callers the marker, since
        // `get_or_load`'s stripped-resident check reads it to decide whether a
        // later bag request may be served from this entry.
        if !want_bag {
            loaded.evict_witness_bag();
        }
        let fa = Arc::new(loaded);
        if self.cap_bytes() == 0 {
            return Ok(fa); // rehydrate-and-drop
        }
        // Retain only if no invalidation landed during the decode — the
        // decoded copy may be the generation the invalidate was erasing.
        // The caller still gets it (best answer available right now); it
        // just must not be pinned.
        let gen_after = self.generation.get(path).map(|g| *g).unwrap_or(0);
        if gen_after != gen_before {
            return Ok(fa);
        }
        let sz = fa.heap_estimate().total();
        // Debit BEFORE the map write, credit the displaced entry after. Two
        // threads racing to rehydrate one path both insert but only one entry
        // survives, so the loser's charge must be refunded — and refunding it
        // before its own debit landed would refund nothing (the counter reads
        // zero) and strand the charge forever.
        self.bytes.fetch_add(sz, Ordering::Relaxed);
        if let Some((_, old_sz)) = self.entries.insert(path.to_path_buf(), (fa.clone(), sz)) {
            self.credit(old_sz);
        }
        self.recency.insert(path.to_path_buf(), self.tick());
        crate::util::ghost_stats::timed("bagcache.evict_to_cap", || self.evict_to_cap(path));
        if let Some(g) = &self.ghost {
            g.set_usage(
                self.bytes.load(Ordering::Relaxed) as u64,
                self.entries.len() as u64,
            );
        }
        Ok(fa)
    }

    /// Refund a charge for an entry that has left the map. Saturating only as
    /// a wrap guard — every credited size was debited before its entry became
    /// visible, so the total can't legitimately go negative.
    fn credit(&self, sz: usize) {
        let _ = self.bytes.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |b| {
            Some(b.saturating_sub(sz))
        });
    }

    /// Drop LRU-tail entries until the retained byte total is within cap. Never
    /// evicts `keep` (the just-inserted path) so a single oversized bag over
    /// the whole cap still resolves the query it was loaded for.
    fn evict_to_cap(&self, keep: &Path) {
        while self.bytes.load(Ordering::Relaxed) > self.cap_bytes() {
            // Lowest recency stamp = least recently used.
            let victim = self
                .recency
                .iter()
                .filter(|e| e.key().as_path() != keep)
                .min_by_key(|e| *e.value())
                .map(|e| e.key().clone());
            let Some(victim) = victim else {
                // Nothing left to evict but the counter still reads over cap:
                // the charge total has drifted above what the map actually
                // holds. Left alone this is a one-way ratchet — every later
                // insert re-enters this loop and drains the cache to `keep`,
                // so the LRU degenerates to one entry and every query pays a
                // full blob decode. Repair from the map and stop.
                self.resync_bytes();
                return;
            };
            self.recency.remove(&victim);
            if let Some((_, (_, sz))) = self.entries.remove(&victim) {
                self.credit(sz);
                if let Some(g) = &self.ghost {
                    g.on_evict(&victim.to_string_lossy());
                }
            }
        }
    }

    /// Recompute the charge total from the map. O(entries), and only reached
    /// when eviction has run out of victims while still reading over cap.
    ///
    /// This is a self-heal for an invariant that should never break, so a
    /// REPAIR is the signal — a drifting counter is what collapsed this LRU to
    /// a single entry and grew the process to 13.9 GB. But running out of
    /// victims does not imply drift: `evict_to_cap` never evicts `keep`, so an
    /// entry bigger than the whole cap lands here every insert with the total
    /// perfectly correct. Counting that would train the reader to ignore the
    /// one number that names the ratchet, so only an actual correction reports.
    fn resync_bytes(&self) {
        let truth: usize = self.entries.iter().map(|e| e.value().1).sum();
        if self.bytes.swap(truth, Ordering::Relaxed) != truth {
            crate::util::ghost_stats::count("pack_bag_cache.resync_bytes_fired");
            self.resyncs.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Drop `path`'s retained bag (a changed/saved file's bag is stale) so the
    /// next type query rehydrates the fresh blob.
    pub fn invalidate(&self, path: &Path) {
        if let Some(g) = &self.ghost {
            g.on_invalidate(&path.to_string_lossy());
        }
        *self.generation.entry(path.to_path_buf()).or_insert(0) += 1;
        self.recency.remove(path);
        if let Some((_, (_, sz))) = self.entries.remove(path) {
            self.credit(sz);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn empty_fa() -> FileAnalysis {
        // A minimal real analysis — its struct shell alone gives a nonzero
        // heap estimate, enough to exercise the byte cap.
        let src = "package M; sub f { 1 }";
        let mut parser = crate::build::builder::create_parser();
        let tree = parser.parse(src, None).unwrap();
        crate::build::builder::build(&tree, src.as_bytes())
    }

    #[test]
    fn cap_zero_never_retains() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let cache = PackBagCache::new(0, move |_p, _want_bag: bool| {
            c.fetch_add(1, Ordering::Relaxed);
            Ok(empty_fa())
        });
        let p = PathBuf::from("/x/a.h");
        assert!(cache.bag_for(&p).is_some());
        assert!(cache.bag_for(&p).is_some());
        // Every access re-decodes; nothing is retained.
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_eq!(cache.entries.len(), 0);
    }

    #[test]
    fn hit_avoids_reload() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let cache = PackBagCache::new(128 * 1024 * 1024, move |_p, _want_bag: bool| {
            c.fetch_add(1, Ordering::Relaxed);
            Ok(empty_fa())
        });
        let p = PathBuf::from("/x/a.h");
        assert!(cache.bag_for(&p).is_some());
        assert!(cache.bag_for(&p).is_some());
        assert_eq!(calls.load(Ordering::Relaxed), 1); // second was a hit
    }

    #[test]
    fn evicts_lru_tail_over_cap() {
        // Each empty FA has a nonzero shell estimate; set a cap that holds
        // only one so inserting a second evicts the first.
        let one = empty_fa().heap_estimate().total();
        assert!(one > 0);
        let cache = PackBagCache::new(one, move |_p, _want_bag: bool| Ok(empty_fa()));
        let a = PathBuf::from("/x/a.h");
        let b = PathBuf::from("/x/b.h");
        cache.bag_for(&a);
        cache.bag_for(&b); // pushes a out (a is LRU, b is the keep)
        assert!(cache.entries.contains_key(&b));
        assert!(!cache.entries.contains_key(&a));
        assert!(cache.bytes.load(Ordering::Relaxed) <= one);
    }

    #[test]
    fn invalidate_during_load_is_not_pinned() {
        // An invalidate landing between the loader's decode and the insert
        // must prevent retention (the decode may be the erased generation).
        // Simulate by invalidating from INSIDE the loader.
        let cache = Arc::new(std::sync::OnceLock::<PackBagCache>::new());
        let c2 = cache.clone();
        let p = PathBuf::from("/x/racy.h");
        let p2 = p.clone();
        let built = PackBagCache::new(128 * 1024 * 1024, move |_p, _want_bag: bool| {
            if let Some(c) = c2.get() {
                c.invalidate(&p2);
            }
            Ok(empty_fa())
        });
        let _ = cache.set(built);
        let c = cache.get().unwrap();
        assert!(c.bag_for(&p).is_some(), "caller still gets the decode");
        assert!(
            !c.entries.contains_key(&p),
            "a decode overlapped by an invalidate must not be retained"
        );
    }

    /// The charge total must equal the sum of what the map actually holds.
    /// When it drifts ABOVE that, `evict_to_cap` can never get back under cap
    /// and drains the LRU to a single entry on every insert — the cache stops
    /// caching, every cross-file bag query pays a full blob decode, and a
    /// long-lived session walks into multi-GB of decode churn.
    #[test]
    fn charge_total_tracks_the_map_and_cannot_ratchet() {
        let one = empty_fa().heap_estimate().total();
        let cache = PackBagCache::new(one * 8, move |_p, _want_bag: bool| Ok(empty_fa()));
        let paths: Vec<PathBuf> =
            (0..24).map(|i| PathBuf::from(format!("/x/{i}.h"))).collect();
        for p in &paths {
            cache.bag_for(p);
        }
        // Poison the counter the way a lost concurrent credit would, then keep
        // using the cache: it must self-repair, not collapse.
        cache.bytes.fetch_add(one * 1000, Ordering::Relaxed);
        for p in &paths {
            cache.bag_for(p);
        }
        let truth: usize = cache.entries.iter().map(|e| e.value().1).sum();
        assert_eq!(
            cache.bytes.load(Ordering::Relaxed),
            truth,
            "charge total drifted from the map"
        );
        assert!(
            cache.entries.len() > 1,
            "cache collapsed to {} entries — the eviction ratchet is back",
            cache.entries.len()
        );
        assert!(cache.bytes.load(Ordering::Relaxed) <= cache.cap_bytes());
    }

    /// Threads racing the same decode both insert; only one entry survives, so
    /// only one charge may. Left unrefunded, the losers' charges are what push
    /// the total past cap for good.
    #[test]
    fn concurrent_decodes_of_one_path_charge_once() {
        let cache = Arc::new(PackBagCache::new(128 * 1024 * 1024, move |_p, _want_bag: bool| {
            // Wide enough for the misses to overlap on the insert.
            std::thread::sleep(std::time::Duration::from_millis(20));
            Ok(empty_fa())
        }));
        let p = PathBuf::from("/x/hot.h");
        let hands: Vec<_> = (0..8)
            .map(|_| {
                let c = Arc::clone(&cache);
                let p = p.clone();
                std::thread::spawn(move || {
                    c.bag_for(&p);
                })
            })
            .collect();
        for h in hands {
            h.join().unwrap();
        }
        let truth: usize = cache.entries.iter().map(|e| e.value().1).sum();
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(
            cache.bytes.load(Ordering::Relaxed),
            truth,
            "racing decodes stacked charges for one entry"
        );
    }

    /// The rows lane retains bag-STRIPPED copies (that's its byte-density
    /// point), and a later bag request must never be served the stripped
    /// entry — it re-decodes whole and replaces it. Serving stripped to a
    /// bag reader is absence-by-eviction on the type axis.
    #[test]
    fn rows_lane_strips_and_bag_request_upgrades() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let cache = PackBagCache::new(128 * 1024 * 1024, move |_p, _want_bag: bool| {
            c.fetch_add(1, Ordering::Relaxed);
            Ok(empty_fa())
        });
        let p = PathBuf::from("/x/a.pm");
        let rows = cache.rows_for(&p).unwrap();
        assert!(rows.bag_is_evicted(), "rows-lane entry retains without the bag");
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        // rows hit — no reload.
        let rows2 = cache.rows_for(&p).unwrap();
        assert!(Arc::ptr_eq(&rows, &rows2));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        // bag request over a stripped entry re-decodes whole.
        let whole = cache.bag_for(&p).unwrap();
        assert!(!whole.bag_is_evicted(), "bag reader never sees the stripped entry");
        assert_eq!(calls.load(Ordering::Relaxed), 2);
        // ...and the whole entry now serves BOTH flavors.
        let rows3 = cache.rows_for(&p).unwrap();
        assert!(Arc::ptr_eq(&whole, &rows3), "whole entry is a superset for rows readers");
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    /// `evict_to_cap` deliberately never evicts `keep`, so an entry larger
    /// than the whole cap leaves the loop unable to get back under it — a
    /// DESIGNED state ("a single oversized bag over the whole cap still
    /// resolves the query it was loaded for"), not drift. It must not trip
    /// the drift alarm: an alarm that fires on a benign, reachable case is
    /// one the next real 13.9 GB ratchet gets to hide behind.
    #[test]
    fn an_oversized_entry_is_not_reported_as_drift() {
        let one = empty_fa().heap_estimate().total();
        assert!(one > 1);
        // Cap below a single entry: the first insert is permanently over.
        let cache = PackBagCache::new(one - 1, move |_p, _want_bag: bool| Ok(empty_fa()));
        let a = PathBuf::from("/x/a.h");
        cache.bag_for(&a);
        assert!(
            cache.bytes.load(Ordering::Relaxed) > cache.cap_bytes(),
            "precondition: the entry alone exceeds the cap"
        );
        let truth: usize = cache.entries.iter().map(|e| e.value().1).sum();
        assert_eq!(
            cache.bytes.load(Ordering::Relaxed),
            truth,
            "precondition: nothing actually drifted"
        );
        assert_eq!(
            cache.resyncs.load(Ordering::Relaxed),
            0,
            "the oversized-keep case is by design, not a byte-accounting drift"
        );
    }

    /// The other half: a genuinely wrong counter MUST still raise the alarm.
    #[test]
    fn a_drifted_counter_is_reported() {
        let one = empty_fa().heap_estimate().total();
        let cache = PackBagCache::new(one * 8, move |_p, _want_bag: bool| Ok(empty_fa()));
        let paths: Vec<PathBuf> =
            (0..4).map(|i| PathBuf::from(format!("/x/{i}.h"))).collect();
        for p in &paths {
            cache.bag_for(p);
        }
        assert_eq!(cache.resyncs.load(Ordering::Relaxed), 0);
        // Poison the counter the way a lost concurrent credit would, then
        // force an INSERT (a hit never reaches `evict_to_cap`).
        cache.bytes.fetch_add(one * 1000, Ordering::Relaxed);
        cache.bag_for(&PathBuf::from("/x/fresh.h"));
        assert!(
            cache.resyncs.load(Ordering::Relaxed) > 0,
            "a real drift must still fire the alarm"
        );
    }

    #[test]
    fn invalidate_drops_entry() {
        let cache = PackBagCache::new(128 * 1024 * 1024, move |_p, _want_bag: bool| Ok(empty_fa()));
        let p = PathBuf::from("/x/a.h");
        cache.bag_for(&p);
        assert!(cache.entries.contains_key(&p));
        cache.invalidate(&p);
        assert!(!cache.entries.contains_key(&p));
        assert_eq!(cache.bytes.load(Ordering::Relaxed), 0);
    }
}
