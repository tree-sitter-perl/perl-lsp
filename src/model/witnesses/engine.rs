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
//! replays its entry's dependencies into the open frames).
//!
//! Keys that depend on each other (parameters passed around in a circle)
//! settle by fixpoint at the outermost key of the cycle (`answer`). Values
//! computed on the way read an assumption: they are PROVISIONAL, held by the
//! thread computing the cycle and reused only within the round that produced
//! them. They reach the shared store, whole, once the cycle settles — another
//! thread could not use them, and a shared slot would let it overwrite one
//! mid-cycle.
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

use crate::model::file_analysis::{CachedModule, CrossFileLookup, FileAnalysis};

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

/// Byte caps. Past its cap a map drops everything and starts over: a full
/// reset is the one eviction that can never serve a stale entry. The pending
/// buffer instead stops taking rows until it is flushed; a row it refuses
/// is an answer the next process recomputes.
const ENTRIES_BYTE_CAP: usize = 32 << 20;
const MENTIONS_BYTE_CAP: usize = 16 << 20;
const PENDING_BYTE_CAP: usize = 16 << 20;

/// A map with the estimated heap bytes of what it holds.
#[derive(Default)]
struct Budgeted<M> {
    map: M,
    bytes: usize,
}

fn path_bytes(p: &std::path::Path) -> usize {
    std::mem::size_of::<PathBuf>() + p.as_os_str().len()
}

fn entry_bytes(key: &WitnessAttachment, e: &Entry) -> usize {
    std::mem::size_of::<(WitnessAttachment, Entry)>()
        + bincode::serialized_size(key).unwrap_or(0) as usize
        + match &*e.value {
            ReducedValue::Type(t) => bincode::serialized_size(t).unwrap_or(0) as usize,
            // A fact map is the size of its debug rendering, near enough.
            ReducedValue::FactMap(m) => format!("{m:?}").len(),
            ReducedValue::None => 0,
        }
        + e.deps
            .iter()
            .map(|d| {
                std::mem::size_of::<Dep>()
                    + match d {
                        Dep::File { path, .. } => path.as_os_str().len(),
                        Dep::Mentions { key, .. } => key.len(),
                    }
            })
            .sum::<usize>()
}

/// The index-owned answer store. Lives as long as the index; validity is
/// re-established per lookup, never by a caller remembering to invalidate.
#[derive(Default)]
pub struct AnswerStore {
    entries: Mutex<Budgeted<HashMap<WitnessAttachment, Entry>>>,
    /// Encoded rows not yet persisted.
    pending: Mutex<Budgeted<Vec<(Vec<u8>, Vec<u8>)>>>,
    /// `mentions_of` per key, valid while `resolution_epoch` stands.
    mentions: Mutex<Budgeted<HashMap<String, (u64, Arc<Vec<PathBuf>>)>>>,
    /// `uncovered_files` at a `resolution_epoch`.
    uncovered: Mutex<Option<(u64, Arc<Vec<Arc<CachedModule>>>)>>,
}

impl AnswerStore {
    pub fn new() -> AnswerStore {
        AnswerStore::default()
    }

    pub fn len(&self) -> usize {
        self.entries.lock().map(|m| m.map.len()).unwrap_or(0)
    }

    /// Estimated heap bytes held: entries, pending rows, mentions memo.
    pub fn heap_bytes(&self) -> usize {
        self.entries.lock().map(|m| m.bytes).unwrap_or(0)
            + self.pending.lock().map(|m| m.bytes).unwrap_or(0)
            + self.mentions.lock().map(|m| m.bytes).unwrap_or(0)
    }

    /// Rows written since the last take, for the caller to persist.
    pub fn take_pending(&self) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.pending.lock().map(|mut p| std::mem::take(&mut *p).map).unwrap_or_default()
    }
}

/// `e` as a persisted row, encoded when it is computed so its stamps are the
/// ones it read. `None` for an entry that cannot outlive the process:
/// unpersisting index, a payload outside the persisted shape, or a file read
/// with no source stamp.
fn encode_persisted(idx: &dyn CrossFileLookup, key: &WitnessAttachment, e: &Entry) -> Option<(Vec<u8>, Vec<u8>)> {
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
        digest: Some(digest),
    })
}

/// One open computation.
struct Frame {
    key: WitnessAttachment,
    deps: Vec<Dep>,
    /// Depth of the outermost open key this computation read while that
    /// key was still being computed (a cycle back to an ancestor frame):
    /// the value holds only for that ancestor's assumption.
    reads_open: Option<usize>,
    /// Read a provisional value that read no open frame's assumption: it
    /// holds only for assumptions this computation never saw.
    tainted: bool,
    /// This key was re-entered while open in the current round.
    is_head: bool,
    /// An assumption inside this computation's cycle moved this round, so
    /// the cycle has not settled.
    changed: bool,
    /// `MEMBERS` length when this frame opened: the members past it were
    /// computed under this frame.
    members_mark: usize,
    /// `UNSETTLED` when this frame opened, restored when it closes.
    unsettled_before: bool,
}

/// Rounds a cycle may take before its fixpoint is declared not to exist
/// and its outermost key answers nothing. The joins only move up a finite
/// class lattice, so real cycles settle in two or three.
const MAX_FIXPOINT_ROUNDS: usize = 8;

thread_local! {
    /// Open computation frames, innermost last; each collects the reads made
    /// while it is open.
    static FRAMES: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
    /// What a re-entered key answers: its value from the previous round of
    /// the cycle it is in (nothing, at first). Lives until the outermost
    /// computation returns, so a key inside a cycle keeps its assumption
    /// across the outer rounds that recompute it.
    static ASSUMED: RefCell<HashMap<WitnessAttachment, ReducedValue>> = RefCell::new(HashMap::new());
    /// Provisional values computed under the open frames, outermost first;
    /// the only place they live, and what a lookup serves them from:
    /// when a frame closes outside every open cycle with the cycles under it
    /// settled, the assumptions its members read were the settled values, so
    /// they are whole. Each carries the depth of the open frame whose
    /// assumption it read (nothing, when it read a value from outside the
    /// fixpoint).
    static MEMBERS: RefCell<Vec<Member>> = const { RefCell::new(Vec::new()) };
    /// A cycle under the innermost settling frame gave up unsettled: its
    /// round's values read assumptions that never held.
    static UNSETTLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

struct Member {
    key: WitnessAttachment,
    entry: Entry,
    /// Depth of the open frame whose assumption it read; `None` when it read
    /// a value from outside the fixpoint.
    reads_open: Option<usize>,
}

/// Retire the current round of the frame that opened at `mark`: the
/// members computed under it read an assumption that just moved.
fn next_round(mark: usize) {
    MEMBERS.with(|m| m.borrow_mut().truncate(mark));
}

fn taint_open_frames() {
    FRAMES.with(|f| {
        for fr in f.borrow_mut().iter_mut() {
            fr.tainted = true;
        }
    });
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
    if let Some((at, paths)) = store.mentions.lock().ok().and_then(|m| m.map.get(key).cloned()) {
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
        let bytes = std::mem::size_of::<(String, (u64, Arc<Vec<PathBuf>>))>()
            + key.len()
            + paths.iter().map(|p| path_bytes(p)).sum::<usize>();
        if m.bytes + bytes > MENTIONS_BYTE_CAP {
            crate::util::ghost_stats::count("engine.mentions_reset");
            *m = Budgeted::default();
        }
        // A replaced entry's bytes stay counted until the next reset: an
        // overcount, never an undercount.
        m.map.insert(key.to_string(), (epoch, Arc::clone(&paths)));
        m.bytes += bytes;
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
/// whose refs carry it, with each open document answering for its own path
/// from its current build. Sorted, so its hash is a stable `Mentions`
/// dependency.
fn mentions_among(idx: &dyn CrossFileLookup, key: &str, uncovered: &[Arc<CachedModule>]) -> Vec<PathBuf> {
    let open = idx.open_callers();
    let is_open = |p: &PathBuf| open.iter().any(|o| &o.path == p);
    let mut paths: Vec<PathBuf> = idx.ref_candidate_paths(&[key.to_string()]);
    for cached in uncovered {
        // An evicted copy can't say, so it stays a candidate.
        if cached.analysis.refs_are_evicted() || !cached.analysis.ref_indices_keyed(key).is_empty() {
            paths.push(cached.path.clone());
        }
    }
    paths.retain(|p| !is_open(p));
    for o in &open {
        if !o.analysis.ref_indices_keyed(key).is_empty() {
            paths.push(o.path.clone());
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

/// Every file that mentions `class` or one of its descendants, each name's
/// mentions recorded as a dependency.
pub fn class_family_mentions(idx: &dyn CrossFileLookup, class: &str) -> std::collections::HashSet<PathBuf> {
    const MAX_FAMILY: usize = 256;
    let mut family = vec![class.to_string()];
    let mut i = 0;
    while i < family.len() && family.len() < MAX_FAMILY {
        for (child, _) in idx.direct_children_of(&family[i]) {
            if !family.contains(&child) {
                family.push(child);
            }
        }
        i += 1;
    }
    let mut keys: Vec<String> = family
        .iter()
        .map(|c| crate::model::conventions::name_match_key(c, &crate::model::conventions::PERL_SPELLINGS))
        .collect();
    keys.sort();
    keys.dedup();
    let mut out = std::collections::HashSet::new();
    for key in keys {
        let paths = mentions_of(idx, &key);
        note_dep(Dep::Mentions { key, hash: mentions_hash(&paths) });
        out.extend(paths.iter().cloned());
    }
    out
}

/// The analysis a caller join reads for `path`: the open document's build
/// when one is open, else the indexed copy, whole.
pub fn caller_view(idx: &dyn CrossFileLookup, path: &std::path::Path) -> Option<Arc<FileAnalysis>> {
    if let Some(o) = idx.open_callers().into_iter().find(|o| o.path == path) {
        note_file(idx, path);
        return Some(Arc::clone(&o.analysis));
    }
    let cached = idx.cached_by_path(path)?;
    Some(idx.whole_present(&cached))
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

/// Answer `key` from the store, or compute it with `compute` and store it.
/// Without a store this is `compute`.
///
/// `compute` must run on a fresh `QueryState`: a memo carried in from the
/// asking query would answer sub-queries without the reads that record
/// their dependencies, and its cycle cuts would depend on the asker. On a
/// fresh state the chase is a function of the key and the index alone. Cycles across engine keys settle here instead: a key
/// re-entered while open answers its current assumption (nothing, at
/// first), and when its computation finishes it is recomputed from what it
/// produced until the two agree. The values computed on the way read an
/// assumption, so they are provisional; the settled head is whole unless it
/// read a head further out.
pub fn answer(
    idx: &dyn CrossFileLookup,
    key: &WitnessAttachment,
    mut compute: impl FnMut() -> ReducedValue,
) -> ReducedValue {
    let reentered = FRAMES.with(|f| {
        let mut frames = f.borrow_mut();
        let pos = frames.iter().position(|fr| &fr.key == key)?;
        frames[pos].is_head = true;
        for fr in frames[pos + 1..].iter_mut() {
            fr.reads_open = Some(fr.reads_open.map_or(pos, |d| d.min(pos)));
        }
        Some(())
    });
    if reentered.is_some() {
        crate::util::ghost_stats::count("engine.cycle");
        return ASSUMED.with(|a| a.borrow().get(key).cloned()).unwrap_or(ReducedValue::None);
    }
    let member = MEMBERS.with(|m| {
        m.borrow().iter().rev().find(|mb| &mb.key == key).map(|mb| (mb.reads_open, Arc::clone(&mb.entry.value), Arc::clone(&mb.entry.deps)))
    });
    if let Some((read, value, deps)) = member {
        // A member of the fixpoint in progress reads the assumptions its own
        // computation did.
        crate::util::ghost_stats::count("engine.member_hit");
        for d in deps.iter() {
            note_dep(d.clone());
        }
        match read {
            Some(d) => mark_reads_open(d),
            None => taint_open_frames(),
        }
        return (*value).clone();
    }
    let store = idx.answer_store();
    let epoch = idx.resolution_epoch();
    if let Some(store) = store {
        let mut found = store.entries.lock().ok().and_then(|m| m.map.get(key).cloned());
        if found.is_none() {
            if let Some(e) = crate::util::ghost_stats::timed("engine.adopt_time", || adopt_persisted(idx, key, epoch)) {
                crate::util::ghost_stats::count("engine.persisted_adopt");
                insert_entry(store, key, e.clone());
                found = Some(e);
            }
        }
        if let Some(e) = found {
            let fresh = e.epoch == epoch || {
                crate::util::ghost_stats::count("engine.verify");
                verify(idx, &e)
            };
            if fresh {
                crate::util::ghost_stats::count("engine.hit");
                for d in e.deps.iter() {
                    note_dep(d.clone());
                }
                if e.epoch != epoch {
                    if let Ok(mut m) = store.entries.lock() {
                        if let Some(slot) = m.map.get_mut(key) {
                            slot.epoch = epoch;
                        }
                    }
                }
                return (*e.value).clone();
            }
            crate::util::ghost_stats::count("engine.stale");
        }
    }
    crate::util::ghost_stats::count("engine.compute");
    let world = idx.world_generation();
    let digest = idx.world_digest();
    let members_mark = MEMBERS.with(|m| m.borrow().len());
    let unsettled_before = UNSETTLED.with(|u| u.replace(false));
    let depth = FRAMES.with(|f| {
        let mut frames = f.borrow_mut();
        frames.push(Frame {
            key: key.clone(),
            deps: Vec::new(),
            reads_open: None,
            tainted: false,
            is_head: false,
            changed: false,
            members_mark,
            unsettled_before,
        });
        frames.len() - 1
    });
    let mut rounds = 0;
    let (value, moved) = loop {
        let v = crate::util::ghost_stats::timed("engine.compute_time", &mut compute);
        let (inner, moved) = FRAMES.with(|f| {
            let mut frames = f.borrow_mut();
            let fr = &mut frames[depth];
            let own_moved = fr.is_head
                && ASSUMED.with(|a| a.borrow_mut().insert(key.clone(), v.clone()))
                    .map_or(v != ReducedValue::None, |prev| prev != v);
            let moved = own_moved || fr.changed;
            fr.is_head = false;
            fr.changed = false;
            (fr.reads_open.is_some(), moved)
        });
        // A key inside a larger cycle takes one pass: the outermost key's
        // rounds recompute it, with its assumption carried over.
        if inner || !moved {
            break (v, moved);
        }
        next_round(members_mark);
        rounds += 1;
        crate::util::ghost_stats::count("engine.fixpoint_round");
        if rounds >= MAX_FIXPOINT_ROUNDS {
            crate::util::ghost_stats::count("engine.fixpoint_unsettled");
            UNSETTLED.with(|u| u.set(true));
            break (ReducedValue::None, false);
        }
    };
    let Some(Frame { mut deps, reads_open, tainted, .. }) = FRAMES.with(|f| f.borrow_mut().pop()) else {
        return value;
    };
    if let Some(d) = reads_open {
        if moved {
            FRAMES.with(|f| {
                if let Some(outer) = f.borrow_mut().get_mut(d) {
                    outer.changed = true;
                }
            });
        }
    }
    let path_dependent = tainted;
    if path_dependent {
        crate::util::ghost_stats::count("engine.tainted");
        taint_open_frames();
    }
    let provisional = path_dependent || reads_open.is_some();
    if provisional {
        crate::util::ghost_stats::count("engine.provisional");
    }
    if let Some(store) = store {
        deps.sort();
        deps.dedup();
        let entry = Entry {
            value: Arc::new(value.clone()),
            epoch,
            world,
            deps: deps.into(),
            digest,
        };
        if provisional {
            MEMBERS.with(|m| m.borrow_mut().push(Member { key: key.clone(), entry, reads_open }));
        } else {
            store_entry(idx, store, key, entry);
        }
    }
    let unsettled = UNSETTLED.with(|u| u.get());
    if reads_open.is_none() {
        // No open frame's assumption reaches this value, so every cycle under
        // it is closed: its members read only assumptions that held, unless a
        // cycle gave up or the chase read a value from outside the fixpoint.
        let members = MEMBERS.with(|m| m.borrow_mut().split_off(members_mark));
        if unsettled {
            // Nothing the round computed is served again.
            crate::util::ghost_stats::count("engine.retired_unsettled");
        } else if path_dependent {
            crate::util::ghost_stats::count("engine.settled_tainted");
        } else if let Some(store) = store {
            for m in &members {
                crate::util::ghost_stats::count("engine.promoted");
                store_entry(idx, store, &m.key, m.entry.clone());
            }
        }
        ASSUMED.with(|a| {
            let mut a = a.borrow_mut();
            a.remove(key);
            for m in &members {
                a.remove(&m.key);
            }
        });
        UNSETTLED.with(|u| u.set(unsettled_before));
    } else {
        UNSETTLED.with(|u| u.set(unsettled_before || unsettled));
    }
    if depth == 0 {
        ASSUMED.with(|a| a.borrow_mut().clear());
    }
    value
}

/// Mark every open frame above `depth` as reading an open computation.
fn mark_reads_open(depth: usize) {
    FRAMES.with(|f| {
        let mut frames = f.borrow_mut();
        for fr in frames.iter_mut().skip(depth + 1) {
            fr.reads_open = Some(fr.reads_open.map_or(depth, |d| d.min(depth)));
        }
    });
}

fn store_entry(idx: &dyn CrossFileLookup, store: &AnswerStore, key: &WitnessAttachment, entry: Entry) {
    if let Some(row) = encode_persisted(idx, key, &entry) {
        push_pending(store, row);
    }
    insert_entry(store, key, entry);
}

fn insert_entry(store: &AnswerStore, key: &WitnessAttachment, entry: Entry) {
    let bytes = entry_bytes(key, &entry);
    if let Ok(mut m) = store.entries.lock() {
        if m.bytes + bytes > ENTRIES_BYTE_CAP {
            crate::util::ghost_stats::count("engine.reset");
            *m = Budgeted::default();
        }
        if let Some(old) = m.map.insert(key.clone(), entry) {
            m.bytes = m.bytes.saturating_sub(entry_bytes(key, &old));
        }
        m.bytes += bytes;
    }
}

fn push_pending(store: &AnswerStore, row: (Vec<u8>, Vec<u8>)) {
    let bytes = 2 * std::mem::size_of::<Vec<u8>>() + row.0.len() + row.1.len();
    if let Ok(mut p) = store.pending.lock() {
        if p.bytes + bytes <= PENDING_BYTE_CAP {
            p.map.push(row);
            p.bytes += bytes;
        } else {
            crate::util::ghost_stats::count("engine.pending_full");
        }
    }
}

/// `raw` with its value replaced, every recorded dependency kept: lets a
/// test tell an adopted answer from a recomputed one.
#[cfg(test)]
pub(crate) fn with_persisted_value(raw: &[u8], value: Option<InferredType>) -> Vec<u8> {
    let mut p: PersistedAnswer = bincode::deserialize(raw).expect("a persisted row");
    p.value = value;
    bincode::serialize(&p).expect("re-encode")
}
