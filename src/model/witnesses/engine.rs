//! The answer engine: a verifying memo for long-distance answers — the ones
//! whose value depends on files the asker never named (a parameter's type
//! joins the call sites of its sub across the whole index).
//!
//! Every key is an `EngineKey`, and the key alone says how its value is
//! computed (`registry::compute_engine_key`): a computation is a function of
//! its key and the index, with nothing captured from the asker.
//!
//! An entry records what its computation READ, as dependencies:
//!
//! - `File { path, version }` — an analysis read through the index (any of
//!   the residency views). `version` is the index's registration generation
//!   for the path, so any re-registration of the file moves it.
//! - `Lookup { lookup, hash }` — a name relation the index derives from
//!   many files (`tracked::Lookup`: who declares a name, who exports it,
//!   which files mention it, ...), by the hash of the value read. A body
//!   edit that adds or removes a call site moves the mentions relation
//!   without touching any surface.
//! - the world generation, carried on the entry: what every answer assumes
//!   without reading it (the `@INC` and workspace roots). Nothing a file
//!   edit or a registration changes is in it; those reach an answer only
//!   through the two dependencies above, so an edit stales only the answers
//!   that read what it moved.
//!
//! Lookup is two-tier. When nothing in the index moved since the entry was
//! verified (`resolution_epoch` equal) it is a hit outright. Otherwise the
//! entry is re-verified: same world, every file at its recorded version,
//! every relation hashing the same — then the answer stands and is re-stamped
//! at the current epoch without recomputing. An edit to a file the answer
//! never read costs one verification, not a recomputation.
//!
//! Dependencies are a graph, as in salsa: a computation's frame records the
//! reads it makes itself, plus an edge to each engine answer it consults,
//! stamped with that answer's `changed` serial. Verifying an entry verifies
//! its children first; a child recomputed to the same value keeps its serial,
//! so its parents stand (early cutoff). A caller join is the fold of one
//! child per caller file (`Contribution`), so an edit recomputes the edited
//! file's part and refolds the rest from the store.
//!
//! Keys that depend on each other (parameters passed around in a circle)
//! settle by fixpoint at the outermost key of the cycle (`answer`). Values
//! computed on the way read an assumption: they are PROVISIONAL, held by the
//! thread computing the cycle and reused only within the round that produced
//! them. They reach the shared store, whole, once the cycle settles — another
//! thread could not use them, and a shared slot would let it overwrite one
//! mid-cycle.
//!
//! Whole entries outlive the process as a graph (`PersistedNode`): each
//! node's own reads in terms a later process can check (a file by its
//! `source_stamp`, a relation by its hash, the world by `world_digest`),
//! and each child by the hash of the value it read. A later process adopts a
//! node when its reads hold and every child — adopted or recomputed — still
//! hashes the same, so an edit one process saw stops at the first answer
//! whose value it did not move, in the next process too.

use super::*;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::model::file_analysis::{CachedModule, CrossFileLookup, FileAnalysis};

use super::tracked::{Lookup, Tracked};

/// One thing an answer read.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Dep {
    File { path: PathBuf, version: u64 },
    Lookup { lookup: Lookup, hash: u64 },
}

/// Which files a mentions set holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum MentionKind {
    /// Any ref keyed by the name.
    Any,
    /// A plain function call keyed by the name. A sub's caller join reads
    /// only these: `Run` is mentioned by every file that calls a `Run`
    /// method, and each such file would be read whole to find no caller.
    Calls,
}

/// Which of a call's arguments a caller join reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum JoinWant {
    /// The one argument at this `@_` position.
    At(u32),
    /// Every argument from this `@_` position on, as key/value pairs.
    PairsFrom(u32),
}

/// A parameter of `package::name` joined over its callers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct JoinKey {
    pub package: String,
    pub name: String,
    pub method: bool,
    pub want: JoinWant,
}

/// What the engine answers. Closed: each key names its computation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum EngineKey {
    /// A parameter's type over every file calling its sub.
    Join(JoinKey),
    /// One caller file's part of a join.
    Contribution { join: JoinKey, file: PathBuf },
    /// A callback parameter, asked of the files declaring its member's class.
    CallbackProviders { package: String, name: String, param: u32, index: u32 },
    /// One candidate file's answer to a cross-file query.
    Consult(ConsultKey),
}

/// What a chase asks of one candidate file: the query, point-free (a
/// consumer's point means nothing against the candidate's spans), asked of
/// that file's bag. As a key of its own, one candidate's verdict is
/// computed once for every computation that asks it, and an edit to the
/// candidate recomputes only its own verdicts.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct ConsultKey {
    pub file: PathBuf,
    pub attachment: WitnessAttachment,
    pub framework: FrameworkFact,
    pub arity: Option<u32>,
    pub receiver: Option<InferredType>,
    pub args: Vec<InferredType>,
}

/// An engine answer.
#[derive(Debug, Clone, PartialEq)]
pub enum EngineValue {
    Value(ReducedValue),
    /// A caller file whose own call sites disagree: the join it feeds has
    /// no answer, whatever the other files say.
    Disagree,
}

impl EngineValue {
    pub const NOTHING: EngineValue = EngineValue::Value(ReducedValue::None);

    /// The value as a reduced answer; a disagreement answers nothing.
    pub fn reduced(self) -> ReducedValue {
        match self {
            EngineValue::Value(v) => v,
            EngineValue::Disagree => ReducedValue::None,
        }
    }

    /// The value in the form a later process can read. `None` for a fact
    /// map, which no engine key answers today and nothing persists.
    fn persisted(&self) -> Option<PersistedValue> {
        match self {
            EngineValue::Value(ReducedValue::Type(t)) => Some(PersistedValue::Type(t.clone())),
            EngineValue::Value(ReducedValue::None) => Some(PersistedValue::Nothing),
            EngineValue::Value(ReducedValue::FactMap(_)) => None,
            EngineValue::Disagree => Some(PersistedValue::Disagree),
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
enum PersistedValue {
    Nothing,
    Type(InferredType),
    Disagree,
}

impl PersistedValue {
    fn into_value(self) -> EngineValue {
        match self {
            PersistedValue::Nothing => EngineValue::NOTHING,
            PersistedValue::Type(t) => EngineValue::Value(ReducedValue::Type(t)),
            PersistedValue::Disagree => EngineValue::Disagree,
        }
    }
}

/// The hash a parent records for a child: stable across processes, so a
/// later process can tell whether the child it computes is the one read.
fn value_hash(v: &EngineValue) -> Option<u64> {
    use std::hash::Hasher;
    let bytes = bincode::serialize(&v.persisted()?).ok()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write(&bytes);
    Some(h.finish())
}

pub fn encode_key(key: &EngineKey) -> Option<Vec<u8>> {
    bincode::serialize(key).ok()
}

#[derive(Debug, Clone)]
struct Entry {
    value: Arc<EngineValue>,
    /// `value_hash` of `value`; `None` for a value nothing persists.
    hash: Option<u64>,
    /// `resolution_epoch` when last verified.
    epoch: u64,
    world: u64,
    deps: Arc<[Dep]>,
    /// The engine answers it consulted, each with the `changed` serial it read.
    children: Arc<[(EngineKey, u64)]>,
    /// Moves only when a recomputation produced a different value.
    changed: u64,
    /// `world_digest` of the world `world` names; `None` when the index
    /// does not persist.
    digest: Option<u64>,
}

static CHANGED_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn next_changed() -> u64 {
    CHANGED_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// One read of a persisted node, checkable by another process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistedRead {
    File { path: PathBuf, stamp: u64 },
    Lookup { lookup: Lookup, hash: u64 },
}

/// `PersistedRead::to_row` kinds.
pub const READ_FILE: i64 = 0;
pub const READ_LOOKUP: i64 = 1;

impl PersistedRead {
    /// `(kind, subject, stamp)`: the read as a row. A file's subject is its
    /// path's bytes, so "what read this file" is one indexed lookup.
    pub fn to_row(&self) -> Option<(i64, Vec<u8>, i64)> {
        Some(match self {
            PersistedRead::File { path, stamp } => {
                (READ_FILE, path.as_os_str().as_encoded_bytes().to_vec(), *stamp as i64)
            }
            PersistedRead::Lookup { lookup, hash } => (READ_LOOKUP, bincode::serialize(lookup).ok()?, *hash as i64),
        })
    }

    pub fn from_row(kind: i64, subject: Vec<u8>, stamp: i64) -> Option<PersistedRead> {
        Some(match kind {
            READ_FILE => PersistedRead::File { path: PathBuf::from(String::from_utf8(subject).ok()?), stamp: stamp as u64 },
            READ_LOOKUP => PersistedRead::Lookup { lookup: bincode::deserialize(&subject).ok()?, hash: stamp as u64 },
            _ => return None,
        })
    }
}

/// An answer as it is persisted: its value, its own reads, and the children
/// it consulted with the hash of the value each one answered.
#[derive(Debug, Clone)]
pub struct PersistedNode {
    /// The encoded value (opaque to the store).
    pub value: Vec<u8>,
    pub value_hash: u64,
    pub world: u64,
    pub reads: Vec<PersistedRead>,
    /// `(encoded child key, child value hash)`.
    pub kids: Vec<(Vec<u8>, u64)>,
}

/// Byte caps. Past its cap a shard drops everything and starts over: a full
/// reset is the one eviction that can never serve a stale entry, and a
/// parent whose child was dropped verifies as stale. The pending buffer
/// instead stops taking rows until it is flushed; a row it refuses is an
/// answer the next process recomputes.
const ENTRIES_BYTE_CAP: usize = 32 << 20;
const MENTIONS_BYTE_CAP: usize = 16 << 20;
const PENDING_BYTE_CAP: usize = 16 << 20;

/// Entry shards: lookups run on every worker thread, and one lock over the
/// whole map serialized them.
const SHARDS: usize = 16;

/// A map with the estimated heap bytes of what it holds.
#[derive(Default)]
struct Budgeted<M> {
    map: M,
    bytes: usize,
}

fn path_bytes(p: &std::path::Path) -> usize {
    std::mem::size_of::<PathBuf>() + p.as_os_str().len()
}

fn key_bytes(key: &EngineKey) -> usize {
    std::mem::size_of::<EngineKey>() + bincode::serialized_size(key).unwrap_or(0) as usize
}

fn entry_bytes(key: &EngineKey, e: &Entry) -> usize {
    std::mem::size_of::<Entry>()
        + key_bytes(key)
        + match &*e.value {
            EngineValue::Value(ReducedValue::Type(t)) => bincode::serialized_size(t).unwrap_or(0) as usize,
            // A fact map is the size of its debug rendering, near enough.
            EngineValue::Value(ReducedValue::FactMap(m)) => format!("{m:?}").len(),
            EngineValue::Value(ReducedValue::None) | EngineValue::Disagree => 0,
        }
        + e.deps
            .iter()
            .map(|d| {
                std::mem::size_of::<Dep>()
                    + match d {
                        Dep::File { path, .. } => path.as_os_str().len(),
                        Dep::Lookup { lookup, .. } => bincode::serialized_size(lookup).unwrap_or(0) as usize,
                    }
            })
            .sum::<usize>()
        + e.children.iter().map(|(k, _)| key_bytes(k) + std::mem::size_of::<u64>()).sum::<usize>()
}

type Shard = Mutex<Budgeted<HashMap<EngineKey, Entry>>>;

/// The index-owned answer store. Lives as long as the index; validity is
/// re-established per lookup, never by a caller remembering to invalidate.
#[derive(Default)]
pub struct AnswerStore {
    shards: [Shard; SHARDS],
    /// Nodes not yet persisted, with their encoded keys.
    pending: Mutex<Budgeted<Vec<(Vec<u8>, PersistedNode)>>>,
    /// `mentions_of` per key, valid while `resolution_epoch` stands.
    mentions: Mutex<Budgeted<HashMap<(MentionKind, String), (u64, Arc<Vec<PathBuf>>)>>>,
    /// `lookup_holds` per relation: `(resolution_epoch, hash)`.
    lookups: Mutex<Budgeted<HashMap<Lookup, (u64, u64)>>>,
    /// `uncovered_files` at a `resolution_epoch`.
    uncovered: Mutex<Option<(u64, Arc<Vec<Arc<CachedModule>>>)>>,
    /// Whether an indexed file holds a plain function call keyed by a
    /// name, at the file's registration version. An edit moves the epoch
    /// and drops every `Calls` set; the verdicts let the rebuild re-read
    /// only the files that changed, not every file mentioning the name.
    call_verdicts: Mutex<Budgeted<HashMap<(String, PathBuf), (u64, bool)>>>,
}

impl AnswerStore {
    pub fn new() -> AnswerStore {
        AnswerStore::default()
    }

    fn shard(&self, key: &EngineKey) -> &Shard {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.hash(&mut h);
        &self.shards[h.finish() as usize % SHARDS]
    }

    fn get(&self, key: &EngineKey) -> Option<Entry> {
        self.shard(key).lock().ok().and_then(|m| m.map.get(key).cloned())
    }

    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.lock().map(|m| m.map.len()).unwrap_or(0)).sum()
    }

    /// Estimated heap bytes held: entries, pending rows, mentions memo.
    pub fn heap_bytes(&self) -> usize {
        self.shards.iter().map(|s| s.lock().map(|m| m.bytes).unwrap_or(0)).sum::<usize>()
            + self.pending.lock().map(|m| m.bytes).unwrap_or(0)
            + self.mentions.lock().map(|m| m.bytes).unwrap_or(0)
            + self.lookups.lock().map(|m| m.bytes).unwrap_or(0)
            + self.call_verdicts.lock().map(|m| m.bytes).unwrap_or(0)
    }

    /// Nodes written since the last take, for the caller to persist.
    pub fn take_pending(&self) -> Vec<(Vec<u8>, PersistedNode)> {
        self.pending.lock().map(|mut p| std::mem::take(&mut *p).map).unwrap_or_default()
    }
}

/// `e` as a persisted node, encoded when it is computed so its stamps are
/// the ones it read. `None` for an entry that cannot outlive the process:
/// unpersisting index, a value outside the persisted shape, a file read with
/// no source stamp, or a child the store no longer holds.
fn encode_persisted(
    idx: &dyn CrossFileLookup,
    store: &AnswerStore,
    key: &EngineKey,
    e: &Entry,
) -> Option<(Vec<u8>, PersistedNode)> {
    let world = e.digest?;
    let value_hash = e.hash?;
    let value = bincode::serialize(&e.value.persisted()?).ok()?;
    let mut reads = Vec::with_capacity(e.deps.len());
    for d in e.deps.iter() {
        reads.push(match d {
            Dep::File { path, .. } => PersistedRead::File { path: path.clone(), stamp: idx.source_stamp(path)? },
            Dep::Lookup { lookup, hash } => PersistedRead::Lookup { lookup: lookup.clone(), hash: *hash },
        });
    }
    let mut kids = Vec::with_capacity(e.children.len());
    for (k, _) in e.children.iter() {
        kids.push((encode_key(k)?, store.get(k)?.hash?));
    }
    Some((encode_key(key)?, PersistedNode { value, value_hash, world, reads, kids }))
}

/// Adopt the persisted node for `key` when every read it records still
/// holds and every child, answered now, hashes to the value it read. The
/// children are asked under a frame of `key`'s own, so their edges land on
/// the adopted entry; a child that reads back into `key` or into an open
/// cycle refuses the node, which is then computed.
fn adopt_persisted(idx: &dyn CrossFileLookup, key: &EngineKey, epoch: u64) -> Option<Entry> {
    let node = idx.persisted_node(&encode_key(key)?)?;
    let digest = idx.world_digest()?;
    if node.world != digest {
        crate::util::ghost_stats::count("engine.persisted_world_moved");
        return None;
    }
    let mut deps = Vec::with_capacity(node.reads.len());
    for read in &node.reads {
        match read {
            PersistedRead::File { path, stamp } => {
                if idx.source_stamp(path) != Some(*stamp) {
                    crate::util::ghost_stats::count("engine.persisted_file_moved");
                    return None;
                }
                deps.push(Dep::File { path: path.clone(), version: idx.file_version(path)? });
            }
            PersistedRead::Lookup { lookup, hash } => {
                if lookup_holds(idx, lookup) != *hash {
                    crate::util::ghost_stats::count("engine.persisted_lookup_moved");
                    return None;
                }
                deps.push(Dep::Lookup { lookup: lookup.clone(), hash: *hash });
            }
        }
    }
    let value: PersistedValue = bincode::deserialize(&node.value).ok()?;
    let members_mark = MEMBERS.with(|m| m.borrow().len());
    FRAMES.with(|f| f.borrow_mut().push(Frame::open(key.clone(), members_mark, false)));
    let mut kids_hold = true;
    for (kid, hash) in &node.kids {
        let Ok(kid) = bincode::deserialize::<EngineKey>(kid) else {
            kids_hold = false;
            break;
        };
        if value_hash(&answer(idx, &kid)) != Some(*hash) {
            crate::util::ghost_stats::count("engine.persisted_kid_moved");
            kids_hold = false;
            break;
        }
    }
    let frame = FRAMES.with(|f| f.borrow_mut().pop())?;
    if !kids_hold || frame.is_head || frame.tainted || frame.reads_open.is_some() {
        // Whatever the children computed against this frame's assumption
        // is retired with it.
        next_round(members_mark);
        return None;
    }
    Some(Entry {
        value: Arc::new(value.into_value()),
        hash: Some(node.value_hash),
        epoch,
        world: idx.world_generation(),
        deps: deps.into(),
        children: frame.children.into_iter().collect::<Vec<_>>().into(),
        changed: next_changed(),
        digest: Some(digest),
    })
}

/// One open computation.
struct Frame {
    key: EngineKey,
    /// A set: a chase reads the same file at every hop that reaches it, and
    /// a list of every read grows with the chase, not with what it read.
    deps: std::collections::HashSet<Dep>,
    /// The engine answers consulted directly beneath this frame.
    children: HashMap<EngineKey, u64>,
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

impl Frame {
    fn open(key: EngineKey, members_mark: usize, unsettled_before: bool) -> Frame {
        Frame {
            key,
            deps: std::collections::HashSet::new(),
            children: HashMap::new(),
            reads_open: None,
            tainted: false,
            is_head: false,
            changed: false,
            members_mark,
            unsettled_before,
        }
    }
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
    static ASSUMED: RefCell<HashMap<EngineKey, EngineValue>> = RefCell::new(HashMap::new());
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
    /// Open `capture` calls, outermost first.
    static CAPTURES: RefCell<Vec<Capture>> = const { RefCell::new(Vec::new()) };
    /// Stored answers being checked (`verify`), outermost first.
    static VERIFYING: RefCell<Vec<EngineKey>> = const { RefCell::new(Vec::new()) };
    /// Depth of `untracked` scopes: reads inside one belong to a relation
    /// whose own hash is the recorded dependency, not to the open frame.
    static UNTRACKED: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
thread_local! {
    static COMPUTED: RefCell<Vec<EngineKey>> = const { RefCell::new(Vec::new()) };
}

/// The keys this thread computed (rather than served from the store)
/// since the last call.
#[cfg(test)]
pub fn take_computed() -> Vec<EngineKey> {
    COMPUTED.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// Run `f` without recording its reads into the open frames. For a derived
/// relation (the mentions sets) whose result is recorded as one hashed
/// dependency: the per-file reads that built it would only over-depend,
/// and a memoized result could not repeat them anyway.
pub(crate) fn untracked<R>(f: impl FnOnce() -> R) -> R {
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            UNTRACKED.with(|u| u.set(u.get() - 1));
        }
    }
    UNTRACKED.with(|u| u.set(u.get() + 1));
    let _leave = Leave;
    f()
}

struct Member {
    key: EngineKey,
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

/// Record a read into the innermost open frame; the frames around it reach
/// it through their child edge. Called by the index's read seams; a no-op
/// when no engine computation is running.
pub fn note_dep(dep: Dep) {
    if UNTRACKED.with(|u| u.get()) > 0 {
        return;
    }
    let depth = FRAMES.with(|f| {
        let mut frames = f.borrow_mut();
        let depth = frames.len();
        if let Some(frame) = frames.last_mut() {
            frame.deps.insert(dep.clone());
        }
        depth
    });
    CAPTURES.with(|c| {
        for cap in c.borrow_mut().iter_mut().filter(|cap| cap.depth == depth) {
            cap.deps.insert(dep.clone());
        }
    });
}

/// Record that the innermost open frame consulted engine answer `key`.
fn note_child(key: &EngineKey, changed: u64) {
    let depth = FRAMES.with(|f| {
        let mut frames = f.borrow_mut();
        let depth = frames.len();
        if let Some(frame) = frames.last_mut() {
            frame.children.insert(key.clone(), changed);
        }
        depth
    });
    CAPTURES.with(|c| {
        for cap in c.borrow_mut().iter_mut().filter(|cap| cap.depth == depth) {
            cap.children.push((key.clone(), changed));
        }
    });
}

/// What a computation recorded, kept by a memo beside its value so a later
/// hit records it again (`replay`). A memo that served a value without its
/// reads would hide them from the engine answer that hit it.
#[derive(Debug, Default)]
pub struct Captured {
    deps: std::collections::BTreeSet<Dep>,
    children: Vec<(EngineKey, u64)>,
}

struct Capture {
    /// The frame depth the capture opened at: reads made there are its
    /// own; deeper frames reach it through their child edge.
    depth: usize,
    deps: std::collections::BTreeSet<Dep>,
    children: Vec<(EngineKey, u64)>,
}

/// Captures what the open engine computation records from here until the
/// guard drops, for a memo to keep beside the value it is about to compute.
/// Opened on a memo miss; scoped to the miss so captures nest in call order.
pub(crate) struct CaptureGuard {
    /// Index into `CAPTURES`; `None` when nothing would be recorded.
    slot: Option<usize>,
}

impl CaptureGuard {
    pub(crate) fn open() -> CaptureGuard {
        if !any_frame_open() {
            return CaptureGuard { slot: None };
        }
        let depth = FRAMES.with(|fr| fr.borrow().len());
        let slot = CAPTURES.with(|c| {
            let mut caps = c.borrow_mut();
            caps.push(Capture { depth, deps: Default::default(), children: Vec::new() });
            caps.len() - 1
        });
        CaptureGuard { slot: Some(slot) }
    }

    /// What was recorded so far. `None` when no computation was open: a
    /// memo storing that value must not replay it as having read nothing,
    /// so absent means "unknown" and a hit inside a computation recomputes.
    pub(crate) fn captured(&self) -> Option<Arc<Captured>> {
        let slot = self.slot?;
        CAPTURES.with(|c| {
            let caps = c.borrow();
            let cap = &caps[slot];
            Some(Arc::new(Captured { deps: cap.deps.clone(), children: cap.children.clone() }))
        })
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        if let Some(slot) = self.slot {
            CAPTURES.with(|c| {
                let mut caps = c.borrow_mut();
                debug_assert_eq!(caps.len(), slot + 1, "captures close in the order they opened");
                caps.truncate(slot);
            });
        }
    }
}

/// Record a memo hit's captured reads into the open computation. `false`
/// when one is open and the memo has no capture: the caller recomputes.
pub(crate) fn replay(captured: Option<&Captured>) -> bool {
    if !any_frame_open() {
        return true;
    }
    let Some(c) = captured else { return false };
    for d in &c.deps {
        note_dep(d.clone());
    }
    for (k, changed) in &c.children {
        note_child(k, *changed);
    }
    true
}

/// Is a join of the same sub's arguments, at an earlier position, already
/// open on this thread? A sub that shifts and then forwards `@_` to itself
/// (`my $this = shift; my $node = shift; $node->evaluate(@_)`) passes its
/// own position `i + 1` as the callee's position `i`, so each hop of the
/// recursion asks a NEW key: the chase never meets itself as a cycle and
/// runs to the depth cap. Cutting the deeper position drops only the
/// self-forwarding caller's contribution; every other caller still joins.
pub fn deeper_position_of_open_join(key: &JoinKey) -> bool {
    let position = |w: &JoinWant| match w {
        JoinWant::At(i) | JoinWant::PairsFrom(i) => *i,
    };
    FRAMES.with(|f| {
        f.borrow().iter().any(|frame| {
            let open = match &frame.key {
                EngineKey::Join(j) | EngineKey::Contribution { join: j, .. } => j,
                EngineKey::CallbackProviders { .. } | EngineKey::Consult(_) => return false,
            };
            open.package == key.package
                && open.name == key.name
                && open.method == key.method
                && position(&open.want) < position(&key.want)
        })
    })
}

/// Would a read now be recorded?
pub(crate) fn frame_open() -> bool {
    any_frame_open()
}

fn any_frame_open() -> bool {
    UNTRACKED.with(|u| u.get()) == 0 && FRAMES.with(|f| !f.borrow().is_empty())
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

/// The current hash of `lookup`, recomputed at most once per
/// `resolution_epoch`: every answer that read it verifies against one
/// recomputation.
fn lookup_holds(idx: &dyn CrossFileLookup, lookup: &Lookup) -> u64 {
    let epoch = idx.resolution_epoch();
    let store = idx.answer_store();
    if let Some(store) = store {
        if let Some(&(at, hash)) = store.lookups.lock().ok().as_ref().and_then(|m| m.map.get(lookup)) {
            if at == epoch {
                crate::util::ghost_stats::count("engine.lookup_memo_hit");
                return hash;
            }
        }
    }
    crate::util::ghost_stats::count("engine.lookup_recompute");
    let hash = untracked(|| super::tracked::lookup_hash(idx, lookup));
    if let Some(store) = store {
        if let Ok(mut m) = store.lookups.lock() {
            let bytes = std::mem::size_of::<(Lookup, (u64, u64))>()
                + bincode::serialized_size(lookup).unwrap_or(0) as usize;
            if m.bytes + bytes > MENTIONS_BYTE_CAP {
                *m = Budgeted::default();
            }
            if m.map.insert(lookup.clone(), (epoch, hash)).is_none() {
                m.bytes += bytes;
            }
        }
    }
    hash
}

/// The files the index says mention `key` the way `kind` asks
/// (`mentions_among`, `calls_among`), memoized per `resolution_epoch`: one
/// verification checks every mentions set its answer's chase read.
pub fn mentions_of(idx: &dyn CrossFileLookup, kind: MentionKind, key: &str) -> Tracked<Arc<Vec<PathBuf>>> {
    Tracked::derive(Lookup::Mentions(kind, key.to_string()), || mentions_of_untracked(idx, kind, key))
}

fn mentions_of_untracked(idx: &dyn CrossFileLookup, kind: MentionKind, key: &str) -> Arc<Vec<PathBuf>> {
    let Some(store) = idx.answer_store() else {
        let any = mentions_among(idx, key, &uncovered_files(idx));
        return Arc::new(match kind {
            MentionKind::Any => any,
            MentionKind::Calls => calls_among(idx, None, key, &any),
        });
    };
    let epoch = idx.resolution_epoch();
    let slot = (kind, key.to_string());
    if let Some((at, paths)) = store.mentions.lock().ok().and_then(|m| m.map.get(&slot).cloned()) {
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
    let paths = Arc::new(match kind {
        MentionKind::Any => mentions_among(idx, key, &uncovered),
        MentionKind::Calls => calls_among(idx, Some(store), key, &mentions_of(idx, MentionKind::Any, key).read()),
    });
    if let Ok(mut m) = store.mentions.lock() {
        let bytes = std::mem::size_of::<((MentionKind, String), (u64, Arc<Vec<PathBuf>>))>()
            + key.len()
            + paths.iter().map(|p| path_bytes(p)).sum::<usize>();
        if m.bytes + bytes > MENTIONS_BYTE_CAP {
            crate::util::ghost_stats::count("engine.mentions_reset");
            *m = Budgeted::default();
        }
        // A replaced entry's bytes stay counted until the next reset: an
        // overcount, never an undercount.
        m.map.insert(slot, (epoch, Arc::clone(&paths)));
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
    }).read();
    files
}

/// The mentions relation for `key`: its ref rows plus every uncovered file
/// whose refs carry it, with each open document answering for its own path
/// from its current build. Sorted, so its hash is a stable `Mentions`
/// dependency.
fn mentions_among(idx: &dyn CrossFileLookup, key: &str, uncovered: &[Arc<CachedModule>]) -> Vec<PathBuf> {
    let open = idx.open_callers().read();
    let is_open = |p: &PathBuf| open.iter().any(|o| &o.path == p);
    let mut paths: Vec<PathBuf> = idx.ref_candidate_paths(&[key.to_string()]);
    for cached in uncovered {
        // An evicted copy can't say, so it stays a candidate.
        let fa = idx.facts_present(cached);
        if fa.refs_are_evicted() || !fa.ref_indices_keyed(key).is_empty() {
            paths.push(cached.path.clone());
        }
    }
    paths.retain(|p| !is_open(p));
    for o in &open {
        if !idx.facts_present(o).ref_indices_keyed(key).is_empty() {
            paths.push(o.path.clone());
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

/// The files among `mentions` holding a plain function call keyed `key`,
/// each read through its refs view (open documents through their build).
/// An indexed file's verdict is memoized at its registration version.
fn calls_among(idx: &dyn CrossFileLookup, store: Option<&AnswerStore>, key: &str, mentions: &[PathBuf]) -> Vec<PathBuf> {
    use crate::model::file_analysis::RefKind;
    let calls = |analysis: &FileAnalysis| {
        analysis
            .ref_indices_keyed(key)
            .iter()
            .any(|&ri| matches!(analysis.refs()[ri].kind, RefKind::FunctionCall { .. }))
    };
    let open = idx.open_callers().read();
    mentions
        .iter()
        .filter(|path| {
            if let Some(o) = open.iter().find(|o| &o.path == *path) {
                return calls(&idx.facts_present(o));
            }
            // Unregistered: nothing to read, so nothing calls it.
            let Some(cached) = idx.cached_by_path(path).read() else { return false };
            let version = idx.file_version(path);
            let slot = (key.to_string(), (*path).clone());
            if let (Some(v), Some(store)) = (version, store) {
                let hit = store.call_verdicts.lock().ok().and_then(|m| m.map.get(&slot).copied());
                if let Some((_, verdict)) = hit.filter(|(at, _)| *at == v) {
                    crate::util::ghost_stats::count("engine.call_verdict_hit");
                    return verdict;
                }
            }
            let verdict = calls(&idx.refs_present(&cached));
            if let (Some(v), Some(store)) = (version, store) {
                if let Ok(mut m) = store.call_verdicts.lock() {
                    let bytes = std::mem::size_of::<((String, PathBuf), (u64, bool))>() + key.len() + path_bytes(path);
                    if m.bytes + bytes > MENTIONS_BYTE_CAP {
                        *m = Budgeted::default();
                    }
                    m.map.insert(slot, (v, verdict));
                    m.bytes += bytes;
                }
            }
            verdict
        })
        .cloned()
        .collect()
}

/// Every file that mentions `class` or one of its descendants, each name's
/// mentions recorded as a dependency; `None` when the family is too large
/// to enumerate.
pub fn class_family_mentions(idx: &dyn CrossFileLookup, class: &str) -> Option<std::collections::HashSet<PathBuf>> {
    const MAX_FAMILY: usize = 256;
    let mut family = vec![class.to_string()];
    let mut i = 0;
    while i < family.len() && family.len() < MAX_FAMILY {
        for (child, _) in idx.direct_children_of(&family[i]).read() {
            if !family.contains(&child) {
                family.push(child);
            }
        }
        i += 1;
    }
    if i < family.len() {
        crate::util::ghost_stats::count("engine.family_cap");
        return None;
    }
    let mut keys: Vec<String> = family
        .iter()
        .map(|c| crate::model::conventions::name_match_key(c, &crate::model::conventions::PERL_SPELLINGS))
        .collect();
    keys.sort();
    keys.dedup();
    let mut out = std::collections::HashSet::new();
    for key in keys {
        out.extend(mentions_of(idx, MentionKind::Any, &key).read().iter().cloned());
    }
    Some(out)
}

/// The analysis a caller join reads for `path`: the open document's build
/// when one is open, else the indexed copy, whole.
pub fn caller_view(idx: &dyn CrossFileLookup, path: &std::path::Path) -> Option<Arc<FileAnalysis>> {
    if let Some(o) = idx.open_callers().read().into_iter().find(|o| o.path == path) {
        return Some(idx.facts_present(&o));
    }
    let cached = idx.cached_by_path(path).read()?;
    Some(idx.whole_present(&cached))
}

/// Is `e` still the answer: its own reads unchanged, and every child it
/// consulted still holding the value it read (verified first, and restamped
/// at `epoch` when it holds).
///
/// A check never re-enters itself: a child already being checked further up
/// (stored answers that consult each other in a cycle) cannot vouch for this
/// entry, and a stale child is recomputed without being checked again. Each
/// check of a stale child once restarted a full-depth check of it, so a
/// stored cycle recursed without bound (a Foswiki warm start ran to 13.9 GB).
fn verify(idx: &dyn CrossFileLookup, store: &AnswerStore, key: &EngineKey, e: &Entry, epoch: u64, depth: usize) -> bool {
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            VERIFYING.with(|v| v.borrow_mut().pop());
        }
    }
    VERIFYING.with(|v| v.borrow_mut().push(key.clone()));
    let _leave = Leave;
    super::registry::with_stack_headroom(|| verify_on_stack(idx, store, e, epoch, depth))
}

fn being_verified(key: &EngineKey) -> bool {
    VERIFYING.with(|v| v.borrow().iter().any(|k| k == key))
}

fn verify_on_stack(idx: &dyn CrossFileLookup, store: &AnswerStore, e: &Entry, epoch: u64, depth: usize) -> bool {
    const MAX_VERIFY_DEPTH: usize = 64;
    if depth > MAX_VERIFY_DEPTH || e.world != idx.world_generation() {
        crate::util::ghost_stats::count(if depth > MAX_VERIFY_DEPTH { "engine.stale_by.depth" } else { "engine.stale_by.world" });
        return false;
    }
    let reads_hold = e.deps.iter().all(|d| match d {
        Dep::File { path, version } => {
            let holds = idx.file_version(path) == Some(*version);
            if !holds {
                crate::util::ghost_stats::count("engine.stale_by.file");
            }
            holds
        }
        Dep::Lookup { lookup, hash } => {
            let holds = lookup_holds(idx, lookup) == *hash;
            if !holds {
                crate::util::ghost_stats::count("engine.stale_by.lookup");
            }
            holds
        }
    });
    reads_hold
        && e.children.iter().all(|(k, changed)| {
            let Some(child) = store.get(k) else {
                crate::util::ghost_stats::count("engine.stale_by.child_evicted");
                return false;
            };
            if child.epoch == epoch {
                if child.changed != *changed {
                    crate::util::ghost_stats::count("engine.stale_by.child_changed");
                    return false;
                }
                return true;
            }
            // A stale child is brought current first — recomputed if it must
            // be — so a recomputation that lands on the same value keeps the
            // serial this entry read, and the entry stands.
            if being_verified(k) {
                crate::util::ghost_stats::count("engine.stale_by.verify_cycle");
                return false;
            }
            if !verify(idx, store, k, &child, epoch, depth + 1) {
                let _ = super::registry::with_stack_headroom(|| answer_on_stack(idx, k, Check::KnownStale));
            } else {
                restamp(store, k, epoch);
            }
            match store.get(k) {
                Some(now) if now.changed == *changed => true,
                _ => {
                    crate::util::ghost_stats::count("engine.stale_by.child_changed");
                    false
                }
            }
        })
}

fn restamp(store: &AnswerStore, key: &EngineKey, epoch: u64) {
    if let Ok(mut m) = store.shard(key).lock() {
        if let Some(slot) = m.map.get_mut(key) {
            slot.epoch = epoch;
        }
    }
}

/// Answer `key` from the store, or compute it and store it. Without a store
/// this is the computation.
///
/// The computation is `registry::compute_engine_key`, a function of the key
/// and the index on a fresh `QueryState`: a memo carried in from the asking
/// query would answer sub-queries without the reads that record their
/// dependencies, and its cycle cuts would depend on the asker. Cycles across
/// engine keys settle here instead: a key re-entered while open answers its
/// current assumption (nothing, at first), and when its computation finishes
/// it is recomputed from what it produced until the two agree. The values
/// computed on the way read an assumption, so they are provisional; the
/// settled head is whole unless it read a head further out.
pub fn answer(idx: &dyn CrossFileLookup, key: &EngineKey) -> EngineValue {
    // An answer's computation asks other answers, through chases that are
    // themselves recursive: the engine's own recursion needs the headroom
    // the chases have (a Foswiki edit overflowed a 2 MiB worker without it).
    super::registry::with_stack_headroom(|| answer_on_stack(idx, key, Check::Verify))
}

/// Whether a stored answer from an earlier epoch is checked before use.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Check {
    Verify,
    /// The caller just failed to verify it.
    KnownStale,
}

fn answer_on_stack(idx: &dyn CrossFileLookup, key: &EngineKey, check: Check) -> EngineValue {
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
        return ASSUMED.with(|a| a.borrow().get(key).cloned()).unwrap_or(EngineValue::NOTHING);
    }
    let member = MEMBERS.with(|m| {
        m.borrow().iter().rev().find(|mb| &mb.key == key).map(|mb| (mb.reads_open, Arc::clone(&mb.entry.value), mb.entry.changed))
    });
    if let Some((read, value, changed)) = member {
        // A member of the fixpoint in progress reads the assumptions its own
        // computation did.
        crate::util::ghost_stats::count("engine.member_hit");
        note_child(key, changed);
        match read {
            Some(d) => mark_reads_open(d),
            None => taint_open_frames(),
        }
        return (*value).clone();
    }
    let store = idx.answer_store();
    let epoch = idx.resolution_epoch();
    if let Some(store) = store {
        let mut found = store.get(key);
        if found.is_none() {
            if let Some(e) = crate::util::ghost_stats::timed("engine.adopt_time", || adopt_persisted(idx, key, epoch)) {
                crate::util::ghost_stats::count("engine.persisted_adopt");
                insert_entry(store, key, e.clone());
                found = Some(e);
            }
        }
        if let Some(e) = found {
            let fresh = e.epoch == epoch
                || (check == Check::Verify && !being_verified(key) && {
                    crate::util::ghost_stats::count("engine.verify");
                    verify(idx, store, key, &e, epoch, 0)
                });
            if fresh {
                crate::util::ghost_stats::count("engine.hit");
                note_child(key, e.changed);
                if e.epoch != epoch {
                    restamp(store, key, epoch);
                }
                return (*e.value).clone();
            }
            crate::util::ghost_stats::count("engine.stale");
        }
    }
    crate::util::ghost_stats::count("engine.compute");
    #[cfg(test)]
    COMPUTED.with(|c| c.borrow_mut().push(key.clone()));
    let world = idx.world_generation();
    let digest = idx.world_digest();
    let members_mark = MEMBERS.with(|m| m.borrow().len());
    let unsettled_before = UNSETTLED.with(|u| u.replace(false));
    let depth = FRAMES.with(|f| {
        let mut frames = f.borrow_mut();
        frames.push(Frame::open(key.clone(), members_mark, unsettled_before));
        frames.len() - 1
    });
    let mut rounds = 0;
    let (value, moved) = loop {
        let v = crate::util::ghost_stats::timed("engine.compute_time", || super::registry::compute_engine_key(idx, key));
        let (inner, moved) = FRAMES.with(|f| {
            let mut frames = f.borrow_mut();
            let fr = &mut frames[depth];
            let own_moved = fr.is_head
                && ASSUMED.with(|a| a.borrow_mut().insert(key.clone(), v.clone()))
                    .map_or(v != EngineValue::NOTHING, |prev| prev != v);
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
            break (EngineValue::NOTHING, false);
        }
    };
    let Some(Frame { deps, children, reads_open, tainted, .. }) = FRAMES.with(|f| f.borrow_mut().pop()) else {
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
        let mut deps: Vec<Dep> = deps.into_iter().collect();
        deps.sort();
        // Early cutoff: a recomputation that lands on the value already held
        // keeps its serial, so the answers that read it stay valid.
        let changed = store
            .get(key)
            .filter(|old| *old.value == value)
            .map(|old| old.changed)
            .unwrap_or_else(next_changed);
        let entry = Entry {
            hash: value_hash(&value),
            value: Arc::new(value.clone()),
            epoch,
            world,
            deps: deps.into(),
            children: children.into_iter().collect::<Vec<_>>().into(),
            changed,
            digest,
        };
        note_child(key, changed);
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

fn store_entry(idx: &dyn CrossFileLookup, store: &AnswerStore, key: &EngineKey, entry: Entry) {
    // Stored first: the node's children are read back from the store.
    insert_entry(store, key, entry.clone());
    if let Some(row) = encode_persisted(idx, store, key, &entry) {
        push_pending(store, row);
    }
}

fn insert_entry(store: &AnswerStore, key: &EngineKey, entry: Entry) {
    let bytes = entry_bytes(key, &entry);
    if let Ok(mut m) = store.shard(key).lock() {
        if m.bytes + bytes > ENTRIES_BYTE_CAP / SHARDS {
            crate::util::ghost_stats::count("engine.reset");
            *m = Budgeted::default();
        }
        if let Some(old) = m.map.insert(key.clone(), entry) {
            m.bytes = m.bytes.saturating_sub(entry_bytes(key, &old));
        }
        m.bytes += bytes;
    }
}

fn push_pending(store: &AnswerStore, row: (Vec<u8>, PersistedNode)) {
    let node = &row.1;
    let bytes = std::mem::size_of::<(Vec<u8>, PersistedNode)>()
        + row.0.len()
        + node.value.len()
        + node.reads.iter().map(|r| std::mem::size_of::<PersistedRead>() + r.to_row().map_or(0, |row| row.1.len())).sum::<usize>()
        + node.kids.iter().map(|(k, _)| std::mem::size_of::<(Vec<u8>, u64)>() + k.len()).sum::<usize>();
    if let Ok(mut p) = store.pending.lock() {
        if p.bytes + bytes <= PENDING_BYTE_CAP {
            p.map.push(row);
            p.bytes += bytes;
        } else {
            crate::util::ghost_stats::count("engine.pending_full");
        }
    }
}

/// `node` with its value replaced and its hash kept: lets a test tell an
/// adopted answer from a recomputed one.
#[cfg(test)]
pub(crate) fn with_persisted_value(node: &PersistedNode, value: Option<InferredType>) -> PersistedNode {
    let v = match value {
        Some(t) => PersistedValue::Type(t),
        None => PersistedValue::Nothing,
    };
    PersistedNode { value: bincode::serialize(&v).expect("encode"), ..node.clone() }
}

/// The key a persisted node was written under, for tests that pick nodes.
#[cfg(test)]
pub(crate) fn decode_key(bytes: &[u8]) -> Option<EngineKey> {
    bincode::deserialize(bytes).ok()
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    fn key(name: &str) -> EngineKey {
        EngineKey::Join(JoinKey {
            package: "P".into(),
            name: name.into(),
            method: false,
            want: JoinWant::At(0),
        })
    }

    fn file_dep(p: &str) -> Dep {
        Dep::File { path: PathBuf::from(p), version: 1 }
    }

    fn open(name: &str) {
        FRAMES.with(|f| f.borrow_mut().push(Frame::open(key(name), 0, false)));
    }

    fn close() -> Frame {
        FRAMES.with(|f| f.borrow_mut().pop()).expect("an open frame")
    }

    /// A memo hit in a later computation records what the miss read: the
    /// provider file and the engine answer consulted beneath it.
    #[test]
    fn a_memo_hit_records_what_its_miss_read() {
        open("first");
        let captured = {
            let guard = CaptureGuard::open();
            note_dep(file_dep("/provider.pm"));
            note_child(&key("child"), 7);
            guard.captured()
        };
        note_dep(file_dep("/after.pm"));
        let first = close();
        assert!(first.deps.contains(&file_dep("/provider.pm")), "the capture does not swallow the frame's read");
        let captured = captured.expect("a computation was open");
        assert!(!captured.deps.contains(&file_dep("/after.pm")), "a read after the guard closed is not the memo's");

        open("second");
        assert!(replay(Some(&captured)));
        let second = close();
        assert!(second.deps.contains(&file_dep("/provider.pm")));
        assert_eq!(second.children.get(&key("child")), Some(&7));
    }

    /// A value memoized outside any computation read things nobody
    /// recorded: inside one, the hit must recompute, not replay nothing.
    #[test]
    fn a_capture_from_outside_a_computation_cannot_be_replayed_inside_one() {
        let guard = CaptureGuard::open();
        assert!(guard.captured().is_none());
        drop(guard);
        assert!(replay(None), "outside a computation there is nothing to record");
        open("inside");
        assert!(!replay(None));
        close();
    }

    /// A read inside a nested engine frame belongs to that frame; the
    /// capture reaches it through the child edge the nested answer leaves.
    #[test]
    fn a_nested_frame_read_belongs_to_the_nested_frame() {
        open("outer");
        let guard = CaptureGuard::open();
        open("inner");
        note_dep(file_dep("/deep.pm"));
        close();
        note_child(&key("inner"), 3);
        let captured = guard.captured().expect("open");
        drop(guard);
        close();
        assert!(!captured.deps.contains(&file_dep("/deep.pm")));
        assert!(captured.children.contains(&(key("inner"), 3)));
    }
}
