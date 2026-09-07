//! A byte-bounded resident store of closedness certificates.
//!
//! Certificates are derived state, so the residency discipline applies: every
//! derived-copy cache is byte-accounted, because an unbounded one is
//! invisible until a corpus is large enough to hurt and then it is a memory
//! regression no functional test can see.
//!
//! They are small — a class name plus its ancestor closure's provider paths —
//! and a miss costs an ancestry walk the consulting site was about to pay
//! anyway, so the cap can be modest and the eviction policy coarse.
//!
//! **Nothing here decides correctness.** A certificate handed back from this
//! store is still validated against the live index before it is trusted, so a
//! stale entry costs a validation that fails and a decode — exactly where the
//! caller already was. That is why this needs no invalidation hooks and takes
//! no part in the eraser discipline.

use crate::model::witnesses::ClosednessCertificate;
use dashmap::DashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Default cap. Certificates run tens to low hundreds of bytes each, so this
/// holds a large workspace's certified classes outright; it exists to bound
/// the pathological case, not to shape the common one.
pub const DEFAULT_CAP_BYTES: usize = 8 * 1024 * 1024;

pub struct ClosednessStore {
    entries: DashMap<String, (Arc<ClosednessCertificate>, usize)>,
    bytes: AtomicUsize,
    cap_bytes: usize,
}

impl Default for ClosednessStore {
    fn default() -> Self {
        Self::new(DEFAULT_CAP_BYTES)
    }
}

impl ClosednessStore {
    pub fn new(cap_bytes: usize) -> Self {
        ClosednessStore {
            entries: DashMap::new(),
            bytes: AtomicUsize::new(0),
            cap_bytes,
        }
    }

    pub fn get(&self, class: &str) -> Option<Arc<ClosednessCertificate>> {
        self.entries.get(class).map(|e| Arc::clone(&e.0))
    }

    /// Remember a certificate, evicting coarsely if that puts the store over
    /// its cap.
    ///
    /// The eviction is a bulk clear rather than an LRU: a re-mint is one
    /// ancestry walk on a path already bound for a decode, so the cost of
    /// being wrong about WHICH entry to drop is far below the cost of the
    /// bookkeeping that would get it right.
    pub fn put(&self, class: &str, cert: Arc<ClosednessCertificate>) {
        let size = cert.heap_bytes() + class.len();
        if let Some(old) = self.entries.insert(class.to_string(), (cert, size)) {
            self.bytes.fetch_sub(old.1, Ordering::Relaxed);
        }
        let now = self.bytes.fetch_add(size, Ordering::Relaxed) + size;
        if now > self.cap_bytes {
            crate::util::ghost_stats::count("closed.store_cleared");
            self.entries.clear();
            self.bytes.store(0, Ordering::Relaxed);
        }
    }

    #[cfg(test)]
    pub fn resident_bytes(&self) -> usize {
        self.bytes.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
#[path = "closedness_store_tests.rs"]
mod tests;
