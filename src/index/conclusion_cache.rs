//! A byte-bounded resident cache of baked conclusion maps.
//!
//! The consult path reads a map per candidate per query, so it cannot go to
//! SQLite each time — that would trade a decode for a round trip and win
//! nothing. It also cannot be unbounded: the residency discipline
//! (`CLAUDE.md`) is that every derived-copy cache is byte-accounted, because
//! an unbounded one is invisible until a corpus is large enough to hurt, and
//! then it is a memory regression no functional test can see.
//!
//! Maps are small — 27.1% of the bag's bincode bytes over the substrate, about
//! 15 MB for 2,693 files — so the default cap holds a realistic workspace
//! outright and the eviction path is the exception rather than the rule.
//!
//! **A miss is "not cached", never "no conclusions".** The loader returning
//! `None` means the store has nothing for this path at this generation, which
//! sends the caller to a decode. That is a different statement from a KEY
//! being absent within a map, which means the bag deterministically answers
//! `None`. Collapsing the two would turn every unbaked file into a file that
//! concludes nothing.

use crate::model::witnesses::ConclusionMap;
use dashmap::DashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// What a lookup found, kept distinct so a caller cannot accidentally read
/// "nothing stored" as "nothing concluded".
pub enum Cached {
    /// The store has a map for this path.
    Map(Arc<ConclusionMap>),
    /// The store has no map for this path — it was never baked, or its bake
    /// was cleared. Decode.
    NotBaked,
}

type Loader = Box<dyn Fn(&Path) -> Option<ConclusionMap> + Send + Sync>;

pub struct ConclusionCache {
    entries: DashMap<PathBuf, (Arc<ConclusionMap>, usize)>,
    /// Paths the loader has already reported absent. Without this, every
    /// consult against an unbaked file re-queries SQLite forever — the
    /// negative answer is the one a hot loop hits most while a workspace is
    /// still being baked.
    absent: DashMap<PathBuf, ()>,
    bytes: AtomicUsize,
    cap_bytes: usize,
    loader: Loader,
}

impl ConclusionCache {
    pub fn new(
        cap_bytes: usize,
        loader: impl Fn(&Path) -> Option<ConclusionMap> + Send + Sync + 'static,
    ) -> Self {
        Self {
            entries: DashMap::new(),
            absent: DashMap::new(),
            bytes: AtomicUsize::new(0),
            cap_bytes,
            loader: Box::new(loader),
        }
    }

    pub fn get(&self, path: &Path) -> Cached {
        if let Some(hit) = self.entries.get(path) {
            crate::util::ghost_stats::count("conclcache.hit");
            return Cached::Map(hit.value().0.clone());
        }
        if self.absent.contains_key(path) {
            crate::util::ghost_stats::count("conclcache.known_absent");
            return Cached::NotBaked;
        }
        crate::util::ghost_stats::count("conclcache.miss");
        let Some(map) = crate::util::ghost_stats::timed("conclcache.load", || {
            (self.loader)(path)
        }) else {
            self.absent.insert(path.to_path_buf(), ());
            return Cached::NotBaked;
        };
        let sz = estimate_bytes(&map);
        let arc = Arc::new(map);
        if self.cap_bytes > 0 {
            self.bytes.fetch_add(sz, Ordering::Relaxed);
            if let Some((_, old)) = self.entries.insert(path.to_path_buf(), (arc.clone(), sz)) {
                self.bytes.fetch_sub(old.min(self.bytes.load(Ordering::Relaxed)), Ordering::Relaxed);
            }
            self.evict_to_cap(path);
        }
        Cached::Map(arc)
    }

    /// Forget one path — its file changed, so its bake is void.
    pub fn invalidate(&self, path: &Path) {
        if let Some((_, (_, sz))) = self.entries.remove(path) {
            self.bytes
                .fetch_sub(sz.min(self.bytes.load(Ordering::Relaxed)), Ordering::Relaxed);
        }
        // The negative memo must go too, or a file baked AFTER being probed
        // stays permanently "not baked" and never serves a conclusion.
        self.absent.remove(path);
    }

    /// Forget everything — the derivation changed, so every map is void.
    pub fn clear(&self) {
        self.entries.clear();
        self.absent.clear();
        self.bytes.store(0, Ordering::Relaxed);
    }

    pub fn resident_bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Drop entries until within cap, never evicting `keep`.
    ///
    /// Eviction order is unspecified rather than LRU. Maps are near-uniform in
    /// size and the access pattern over a consult storm is close to uniform
    /// too, so a recency structure would cost more to maintain than it saves —
    /// and pretending to an LRU that is really insertion-order is worse than
    /// saying it is arbitrary.
    fn evict_to_cap(&self, keep: &Path) {
        while self.bytes.load(Ordering::Relaxed) > self.cap_bytes {
            let victim = self
                .entries
                .iter()
                .map(|e| e.key().clone())
                .find(|p| p != keep);
            let Some(victim) = victim else { break };
            if let Some((_, (_, sz))) = self.entries.remove(&victim) {
                self.bytes
                    .fetch_sub(sz.min(self.bytes.load(Ordering::Relaxed)), Ordering::Relaxed);
                crate::util::ghost_stats::count("conclcache.evicted");
            } else {
                break;
            }
        }
    }
}

/// Rough resident size of a map.
///
/// Deliberately an estimate: an exact figure would need a deep walk of every
/// `InferredType`, which costs more than the accounting is worth. It only has
/// to be proportional and never zero — a cap enforced against a zero estimate
/// is not a cap.
fn estimate_bytes(map: &ConclusionMap) -> usize {
    // Key + enum discriminant + a typical small payload, plus the hash slot.
    const PER_ENTRY: usize = 160;
    64 + map.len() * PER_ENTRY
}

#[cfg(test)]
#[path = "conclusion_cache_tests.rs"]
mod conclusion_cache_tests;
