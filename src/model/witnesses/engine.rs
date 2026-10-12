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
//!
//! Whole entries outlive the process. A persisted entry restates its
//! dependencies in terms a later process can check: each file by its
//! `source_stamp`, each mentions set by its hash, the world by
//! `world_digest`. A later process adopts the entry on its first miss for
//! the key when all three still hold, and from then on it is an ordinary
//! entry.

use super::*;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::model::file_analysis::{CachedModule, CrossFileLookup};

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
    /// `world_digest` of the world `world` names; `None` when the index
    /// does not persist.
    digest: Option<u64>,
}

/// An entry as it is persisted: every dependency in a form another process
/// can check against its own index.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct PersistedAnswer {
    value: Option<InferredType>,
    world: u64,
    files: Vec<(PathBuf, u64)>,
    mentions: Vec<(String, u64)>,
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
    /// Encoded rows not yet persisted; bounded like `entries`.
    pending: Mutex<Vec<(Vec<u8>, Vec<u8>)>>,
    /// `mentions_of` per key, valid while `resolution_epoch` stands.
    mentions: Mutex<HashMap<String, (u64, Arc<Vec<PathBuf>>)>>,
    /// `uncovered_files` at a `resolution_epoch`.
    uncovered: Mutex<Option<(u64, Arc<Vec<Arc<CachedModule>>>)>>,
}

impl AnswerStore {
    pub fn new() -> AnswerStore {
        AnswerStore::default()
    }

    pub fn len(&self) -> usize {
        self.entries.lock().map(|m| m.len()).unwrap_or(0)
    }

    /// Rows written since the last take, for the caller to persist.
    pub fn take_pending(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.pending.lock().map(|mut p| std::mem::take(&mut *p)).unwrap_or_default()
    }
}

/// `e` as a persisted row, encoded when it is computed so its stamps are the
/// ones it read. `None` for an entry that cannot outlive the process: cut,
/// unpersisting index, a payload outside the persisted shape, or a file read
/// with no source stamp.
fn encode_persisted(idx: &dyn CrossFileLookup, key: &WitnessAttachment, e: &Entry) -> Option<(Vec<u8>, Vec<u8>)> {
    if e.provisional {
        return None;
    }
    let world = e.digest?;
    let value = match &*e.value {
        ReducedValue::Type(t) => Some(t.clone()),
        ReducedValue::None => None,
        ReducedValue::FactMap(_) => return None,
    };
    let mut files = Vec::new();
    let mut mentions = Vec::new();
    for d in e.deps.iter() {
        match d {
            Dep::File { path, .. } => files.push((path.clone(), idx.source_stamp(path)?)),
            Dep::Mentions { key, hash } => mentions.push((key.clone(), *hash)),
        }
    }
    let p = PersistedAnswer { value, world, files, mentions };
    Some((bincode::serialize(key).ok()?, bincode::serialize(&p).ok()?))
}

/// Adopt the persisted answer for `key`, when every dependency it records
/// still holds in `idx`, as an entry stamped at `epoch`.
fn adopt_persisted(idx: &dyn CrossFileLookup, key: &WitnessAttachment, epoch: u64) -> Option<Entry> {
    let raw = idx.persisted_answer(&bincode::serialize(key).ok()?)?;
    let p: PersistedAnswer = bincode::deserialize(&raw).ok()?;
    let digest = idx.world_digest()?;
    if p.world != digest {
        crate::util::ghost_stats::count("engine.persisted_world_moved");
        return None;
    }
    let mut deps = Vec::with_capacity(p.files.len() + p.mentions.len());
    for (path, stamp) in p.files {
        if idx.source_stamp(&path) != Some(stamp) {
            crate::util::ghost_stats::count("engine.persisted_file_moved");
            return None;
        }
        let version = idx.file_version(&path)?;
        deps.push(Dep::File { path, version });
    }
    for (key, hash) in p.mentions {
        if mentions_hash(&mentions_of(idx, &key)) != hash {
            crate::util::ghost_stats::count("engine.persisted_mentions_moved");
            return None;
        }
        deps.push(Dep::Mentions { key, hash });
    }
    let value = match p.value {
        Some(t) => ReducedValue::Type(t),
        None => ReducedValue::None,
    };
    Some(Entry {
        value: Arc::new(value),
        epoch,
        world: idx.world_generation(),
        deps: deps.into(),
        provisional: false,
        digest: Some(digest),
    })
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

/// The files the index says mention `key` (`mentions_among`), memoized per
/// `resolution_epoch`: one verification checks every mentions set its
/// answer's chase read.
pub fn mentions_of(idx: &dyn CrossFileLookup, key: &str) -> Arc<Vec<PathBuf>> {
    let Some(store) = idx.answer_store() else {
        return Arc::new(mentions_among(idx, key, &uncovered_files(idx)));
    };
    let epoch = idx.resolution_epoch();
    if let Some((at, paths)) = store.mentions.lock().ok().and_then(|m| m.get(key).cloned()) {
        if at == epoch {
            return paths;
        }
    }
    let uncovered = match store.uncovered.lock().ok().and_then(|u| u.clone()) {
        Some((at, files)) if at == epoch => files,
        _ => {
            let files = Arc::new(uncovered_files(idx));
            if let Ok(mut u) = store.uncovered.lock() {
                *u = Some((epoch, Arc::clone(&files)));
            }
            files
        }
    };
    let paths = Arc::new(mentions_among(idx, key, &uncovered));
    if let Ok(mut m) = store.mentions.lock() {
        if m.len() >= ANSWER_STORE_CAP {
            m.clear();
        }
        m.insert(key.to_string(), (epoch, Arc::clone(&paths)));
    }
    paths
}

/// Held files the ref rows don't cover (never shredded): their resident
/// copies are the only record of what they mention.
fn uncovered_files(idx: &dyn CrossFileLookup) -> Vec<Arc<CachedModule>> {
    let shredded = idx.ref_indexed_paths();
    let mut files = Vec::new();
    idx.for_each_cached_file(&mut |cached| {
        if !shredded.contains(&cached.path) {
            files.push(Arc::clone(cached));
        }
    });
    files
}

/// The mentions relation for `key`: its ref rows plus every uncovered file
/// whose refs carry it. Sorted, so its hash is a stable `Mentions`
/// dependency.
fn mentions_among(idx: &dyn CrossFileLookup, key: &str, uncovered: &[Arc<CachedModule>]) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = idx.ref_candidate_paths(&[key.to_string()]);
    for cached in uncovered {
        // An evicted copy can't say, so it stays a candidate.
        if cached.analysis.refs_are_evicted() || !cached.analysis.ref_indices_keyed(key).is_empty() {
            paths.push(cached.path.clone());
        }
    }
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
    let mut found = store.entries.lock().ok().and_then(|m| m.get(key).cloned());
    if found.is_none() {
        if let Some(e) = crate::util::ghost_stats::timed("engine.adopt_time", || adopt_persisted(idx, key, epoch)) {
            crate::util::ghost_stats::count("engine.persisted_adopt");
            if let Ok(mut m) = store.entries.lock() {
                m.insert(key.clone(), e.clone());
            }
            found = Some(e);
        }
    }
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
            // A path-dependent value makes everything computed from it
            // path-dependent too.
            if e.provisional {
                FRAMES.with(|f| {
                    for fr in f.borrow_mut().iter_mut() {
                        fr.cut = true;
                    }
                });
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
    let digest = idx.world_digest();
    FRAMES.with(|f| f.borrow_mut().push(Frame { key: key.clone(), deps: Vec::new(), cut: false }));
    let (value, chase_cut) = crate::util::ghost_stats::timed("engine.compute_time", compute);
    let Some(Frame { mut deps, cut, .. }) = FRAMES.with(|f| f.borrow_mut().pop()) else {
        return value;
    };
    let provisional = cut || chase_cut;
    if provisional {
        crate::util::ghost_stats::count("engine.provisional");
    }
    deps.sort();
    deps.dedup();
    let entry = Entry { value: Arc::new(value.clone()), epoch, world, deps: deps.into(), provisional, digest };
    if let Some(row) = encode_persisted(idx, key, &entry) {
        if let Ok(mut p) = store.pending.lock() {
            if p.len() < ANSWER_STORE_CAP {
                p.push(row);
            }
        }
    }
    if let Ok(mut m) = store.entries.lock() {
        if m.len() >= ANSWER_STORE_CAP {
            crate::util::ghost_stats::count("engine.reset");
            m.clear();
        }
        m.insert(key.clone(), entry);
    }
    value
}

/// `raw` with its value replaced, every recorded dependency kept: lets a
/// test tell an adopted answer from a recomputed one.
#[cfg(test)]
pub(crate) fn with_persisted_value(raw: &[u8], value: Option<InferredType>) -> Vec<u8> {
    let mut p: PersistedAnswer = bincode::deserialize(raw).expect("a persisted row");
    p.value = value;
    bincode::serialize(&p).expect("re-encode")
}
