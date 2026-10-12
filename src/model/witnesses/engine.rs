//! The answer engine: a verifying memo for long-distance answers — the ones
//! whose value depends on files the asker never named (`ParamIn` joins the
//! call sites of a sub across the whole index).
//!
//! An entry records what its computation READ, as dependencies:
//!
//! - `File { path, version }` — an analysis read through the index (any of
//!   the residency views). `version` is the index's registration generation
//!   for the path, so any re-registration of the file moves it.
//! - `Mentions { key, hash }` — the set of files the index says mention a
//!   name. A body edit that adds or removes a call site changes it without
//!   touching any surface.
//! - the world generation, carried on the entry: every surface-changing
//!   write and every structure change (a file added, removed, a provider
//!   swapped) moves it. Name lookups (exporters, parents, declarers, bridges)
//!   read only surfaces, so this one number covers them.
//!
//! Lookup is two-tier. When nothing in the index moved since the entry was
//! verified (`resolution_epoch` equal) it is a hit outright. Otherwise the
//! entry is re-verified: same world, every file at its recorded version,
//! every mentions set unchanged — then the answer stands and is re-stamped
//! at the current epoch without recomputing. An edit to a file the answer
//! never read costs one verification, not a recomputation.
//!
//! Dependencies are flattened: a computation's frame collects every read
//! made beneath it, including the reads of nested engine answers (a hit
//! replays its entry's dependencies into the open frames). A value computed
//! while a cycle guard cut something is path-dependent: it is kept as
//! PROVISIONAL, served only while the index has not moved at all, and never
//! re-verified or persisted. Recomputing it on every ask is what a call
//! graph full of parameters passed around in circles cannot afford.

use super::*;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::model::file_analysis::CrossFileLookup;

/// One thing an answer read.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dep {
    File { path: PathBuf, version: u64 },
    Mentions { key: String, hash: u64 },
}

#[derive(Debug, Clone)]
struct Entry {
    value: Arc<ReducedValue>,
    /// `resolution_epoch` when last verified.
    epoch: u64,
    world: u64,
    deps: Arc<[Dep]>,
    /// Computed under a cut: path-dependent, so it is served only while
    /// nothing in the index has moved (the epoch fast path), never
    /// re-verified, never persisted.
    provisional: bool,
}

/// Above this many entries the store drops everything and starts over: an
/// entry is a few hundred bytes, so the cap bounds the store near a few MB,
/// and a full reset is the one eviction that can never serve a stale entry.
const ANSWER_STORE_CAP: usize = 16_384;

/// The index-owned answer store. Lives as long as the index; validity is
/// re-established per lookup, never by a caller remembering to invalidate.
#[derive(Default)]
pub struct AnswerStore {
    entries: Mutex<HashMap<WitnessAttachment, Entry>>,
}

impl AnswerStore {
    pub fn new() -> AnswerStore {
        AnswerStore::default()
    }

    pub fn len(&self) -> usize {
        self.entries.lock().map(|m| m.len()).unwrap_or(0)
    }
}

/// One open computation.
struct Frame {
    key: WitnessAttachment,
    deps: Vec<Dep>,
    /// A key on the open path was re-entered beneath this frame, so its
    /// value depends on the path that reached it.
    cut: bool,
}

thread_local! {
    /// Open computation frames, innermost last; each collects the reads made
    /// while it is open.
    static FRAMES: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
}

/// Record a read into every open frame. Called by the index's read seams; a
/// no-op when no engine computation is running.
pub fn note_dep(dep: Dep) {
    FRAMES.with(|f| {
        let mut frames = f.borrow_mut();
        for frame in frames.iter_mut() {
            frame.deps.push(dep.clone());
        }
    });
}

fn any_frame_open() -> bool {
    FRAMES.with(|f| !f.borrow().is_empty())
}

/// Record that `path` was read, at its current version. Cheap enough to
/// call at every read seam: nothing happens unless a frame is open.
pub fn note_file(idx: &dyn CrossFileLookup, path: &std::path::Path) {
    if !any_frame_open() {
        return;
    }
    if let Some(version) = idx.file_version(path) {
        note_dep(Dep::File { path: path.to_path_buf(), version });
    }
}

pub fn mentions_hash(paths: &[PathBuf]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    paths.hash(&mut h);
    h.finish()
}

/// The files the index says mention `key`: its ref rows, plus every
/// resident file the rows don't cover whose own refs carry the key (never
/// shredded, so its resident copy still holds its refs). Sorted, so its hash
/// is a stable `Mentions` dependency.
pub fn mentions_of(idx: &dyn CrossFileLookup, key: &str) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = idx.ref_candidate_paths(&[key.to_string()]);
    let shredded = idx.ref_indexed_paths();
    idx.for_each_cached_file(&mut |cached| {
        // An evicted copy can't say, so it stays a candidate.
        let mentions = cached.analysis.refs_are_evicted()
            || !cached.analysis.ref_indices_keyed(key).is_empty();
        if !shredded.contains(&cached.path) && mentions {
            paths.push(cached.path.clone());
        }
    });
    paths.sort();
    paths.dedup();
    paths
}

fn verify(idx: &dyn CrossFileLookup, e: &Entry) -> bool {
    if e.world != idx.world_generation() {
        return false;
    }
    e.deps.iter().all(|d| match d {
        Dep::File { path, version } => idx.file_version(path) == Some(*version),
        Dep::Mentions { key, hash } => mentions_hash(&mentions_of(idx, key)) == *hash,
    })
}

/// Answer `key` from the store, or compute it with `compute` and store it
/// when the computation was whole (`compute` returns the value and whether
/// its own chase was cut). Without a store this is `compute`.
///
/// `compute` must run on a fresh `QueryState`: a memo carried in from the
/// asking query would answer sub-queries without the reads that record
/// their dependencies. Cycles across engine keys are cut here instead — a
/// key re-entered while open answers nothing, and every open frame is
/// marked cut, so none of the path-dependent values is stored.
pub fn answer(
    idx: &dyn CrossFileLookup,
    key: &WitnessAttachment,
    compute: impl FnOnce() -> (ReducedValue, bool),
) -> ReducedValue {
    let reentered = FRAMES.with(|f| {
        let mut frames = f.borrow_mut();
        let open = frames.iter().any(|fr| &fr.key == key);
        if open {
            for fr in frames.iter_mut() {
                fr.cut = true;
            }
        }
        open
    });
    if reentered {
        crate::util::ghost_stats::count("engine.cycle");
        return ReducedValue::None;
    }
    let Some(store) = idx.answer_store() else {
        FRAMES.with(|f| f.borrow_mut().push(Frame { key: key.clone(), deps: Vec::new(), cut: false }));
        let (v, _) = compute();
        FRAMES.with(|f| f.borrow_mut().pop());
        return v;
    };
    let epoch = idx.resolution_epoch();
    let found = store.entries.lock().ok().and_then(|m| m.get(key).cloned());
    if let Some(e) = found {
        let fresh = e.epoch == epoch
            || (!e.provisional && {
                crate::util::ghost_stats::count("engine.verify");
                verify(idx, &e)
            });
        if fresh {
            crate::util::ghost_stats::count("engine.hit");
            for d in e.deps.iter() {
                note_dep(d.clone());
            }
            if e.epoch != epoch {
                if let Ok(mut m) = store.entries.lock() {
                    if let Some(slot) = m.get_mut(key) {
                        slot.epoch = epoch;
                    }
                }
            }
            return (*e.value).clone();
        }
        crate::util::ghost_stats::count("engine.stale");
    }
    crate::util::ghost_stats::count("engine.compute");
    let world = idx.world_generation();
    FRAMES.with(|f| f.borrow_mut().push(Frame { key: key.clone(), deps: Vec::new(), cut: false }));
    let (value, chase_cut) = compute();
    let Some(Frame { mut deps, cut, .. }) = FRAMES.with(|f| f.borrow_mut().pop()) else {
        return value;
    };
    let provisional = cut || chase_cut;
    if provisional {
        crate::util::ghost_stats::count("engine.provisional");
    }
    deps.sort();
    deps.dedup();
    if let Ok(mut m) = store.entries.lock() {
        if m.len() >= ANSWER_STORE_CAP {
            crate::util::ghost_stats::count("engine.reset");
            m.clear();
        }
        m.insert(
            key.clone(),
            Entry { value: Arc::new(value.clone()), epoch, world, deps: deps.into(), provisional },
        );
    }
    value
}
