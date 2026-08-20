//! The resolution session: a memo for cross-file consult ANSWERS that
//! survives across top-level registry queries.
//!
//! `QueryState`'s memo dedups within ONE `ReducerRegistry::query`. A
//! backward reference walk issues one such query per candidate call site,
//! and each re-derives the same `PackageSymbol{package, method}` lattice
//! from scratch — at 138k files (5–12 files declaring a common package
//! name, mutual imports throughout) that re-derivation is combinatorial
//! and the verb never returns. The session is the outer memo: opened for
//! the duration of one walk, keyed on the CANDIDATE FILE's path rather
//! than a bag address, so it needs no pins and cannot ABA.
//!
//! Validity rides `CrossFileLookup::resolution_epoch()` — the same
//! additive counter the enrichment-key memo validates against. Any index
//! mutation moves it and the session drops everything; over-invalidation
//! is the safe direction.
//!
//! Soundness gates, all load-bearing:
//! - **One visibility scope.** Entries are used only for queries running
//!   under the SAME `&dyn CrossFileLookup` the session was opened with. A
//!   pack file's `ScopedLookup` is a different object, so its
//!   closure-narrowed candidate view never reads a memo minted under the
//!   unscoped index (nor writes one).
//! - **Complete values only.** A value computed while the cycle guard cut
//!   a key that lies ABOVE the candidate's own subtree depends on the path
//!   that produced it; reusing it elsewhere would serve a truncated
//!   answer. `QueryState` tracks the shallowest blocked depth, and the
//!   memo declines to store when it is above the evaluation's own root.
//! - **Full query identity in the key.** Attachment, receiver identity,
//!   arity hint, point and framework all ride the key for the same reason
//!   they ride `QueryState`'s: two queries differing in any of them can
//!   resolve differently.

use super::*;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::model::file_analysis::{CachedModule, CrossFileLookup};

/// Which view of a candidate file the answer came from is NOT part of the
/// key: `candidate_answer` memoizes the candidate's WHOLE contribution
/// (raw bag, then the enriched-overlay retry), which is what the caller
/// consumes as one unit.
#[derive(PartialEq, Eq, Hash, Clone)]
struct CandidateKey {
    /// Interned candidate path — a `u32` so the millions of lookups hash
    /// four bytes instead of a `PathBuf`.
    path: u32,
    attachment: WitnessAttachment,
    receiver: Option<String>,
    arity: Option<u32>,
    point: Option<Point>,
    framework: FrameworkFact,
}

struct SessionState {
    /// Data address of the lookup this session was opened on. Entries are
    /// only valid under it (see the visibility-scope gate above).
    index_id: usize,
    /// `resolution_epoch()` when the memo was last known good.
    epoch: u64,
    paths: HashMap<PathBuf, u32>,
    memo: HashMap<CandidateKey, Arc<ReducedValue>>,
    /// `visible_def_candidates` is a clone + sort of the whole candidate
    /// vec per call; one walk asks for the same class millions of times.
    candidates: HashMap<String, Arc<Vec<Arc<CachedModule>>>>,
    /// Consult budget for the whole walk. Even memoized, some query at
    /// some scale exceeds any bound; a capped, marked-incomplete answer
    /// beats one that never returns.
    ///
    /// TWO units, because neither alone is sizable. A COUNT is
    /// deterministic — the same query degrades at the same place on every
    /// run — but cannot be sized: a consult costs microseconds on a warm
    /// small project and ~2.5 ms at 138k files (~9 blob rehydrations
    /// each), so any count generous enough for a healthy workspace walk
    /// (Koha: ~5k) is already tens of seconds at scale. A DEADLINE is
    /// scale-free and gives the verb a real latency contract, at the cost
    /// of being load-dependent. The count is set far above anything a
    /// real query reaches, so it is the deterministic backstop and the
    /// clock is what actually fires.
    fuel: u64,
    deadline: Option<std::time::Instant>,
    /// Set when `fuel` ran out — the walk's answer is an
    /// under-approximation from that point on.
    exhausted: bool,
    stats: SessionStats,
}

/// What the session did — the memo's whole point, in two numbers.
/// Always accounted (the session only exists inside a walk, so this is two
/// increments per consult, not a gated instrument).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SessionStats {
    /// Candidate evaluations actually performed.
    pub consults: u64,
    /// Candidate evaluations answered from the memo.
    pub hits: u64,
}

thread_local! {
    static SESSION: RefCell<Option<SessionState>> = const { RefCell::new(None) };
    /// Degradation verdict of the walk that just CLOSED on this thread.
    /// `references` returns `Location[]` — the protocol has no
    /// `isIncomplete` for it (that lives on `CompletionList`) — so the
    /// verdict has to leave the walk out of band. It is published here on
    /// the owning guard's drop and read by the handler immediately after,
    /// on the same thread, before it can be overwritten.
    static LAST_WALK_DEGRADED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Consult budget for one walk. Sized from measurement: a workspace-scale
/// `references` needs low tens of thousands of cross-file consults, so
/// this is ~two orders of margin over a healthy query and still bounds
/// the pathological one. `PERL_LSP_RESOLVE_FUEL=0` disables the budget.
fn default_fuel() -> u64 {
    std::env::var("PERL_LSP_RESOLVE_FUEL")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(|v| if v == 0 { u64::MAX } else { v })
        .unwrap_or(5_000_000)
}

/// Wall-clock half of the budget. `0` = unbounded.
fn default_budget_ms() -> u64 {
    std::env::var("PERL_LSP_RESOLVE_BUDGET_MILLISECONDS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(30_000)
}

/// RAII handle. Only the OUTERMOST guard owns the session — a nested
/// `enter` (a verb that composes two walks) rides the open one rather
/// than resetting the memo mid-walk.
pub struct ResolutionSession {
    owns: bool,
}

impl ResolutionSession {
    /// Open a session for `idx`. `None` (no cross-file index) opens
    /// nothing: every memoizable consult goes through a lookup.
    pub fn enter(idx: Option<&dyn CrossFileLookup>) -> ResolutionSession {
        Self::enter_with_budget(idx, default_fuel())
    }

    /// `enter` with an explicit consult budget — the seam a caller with its
    /// own latency contract (or a test) sets the bound from.
    pub fn enter_with_budget(
        idx: Option<&dyn CrossFileLookup>,
        fuel: u64,
    ) -> ResolutionSession {
        let Some(idx) = idx else {
            return ResolutionSession { owns: false };
        };
        let id = idx as *const dyn CrossFileLookup as *const () as usize;
        let epoch = idx.resolution_epoch();
        SESSION.with(|s| {
            let mut slot = s.borrow_mut();
            if slot.is_some() {
                return ResolutionSession { owns: false };
            }
            *slot = Some(SessionState {
                index_id: id,
                epoch,
                paths: HashMap::new(),
                memo: HashMap::new(),
                candidates: HashMap::new(),
                fuel,
                deadline: match default_budget_ms() {
                    0 => None,
                    ms => Some(std::time::Instant::now() + std::time::Duration::from_millis(ms)),
                },
                exhausted: false,
                stats: SessionStats::default(),
            });
            ResolutionSession { owns: true }
        })
    }

    /// Did this walk run out of consult budget? An exhausted walk answered
    /// from what it had reached — callers that report completeness read
    /// this before the guard drops.
    pub fn degraded() -> bool {
        SESSION.with(|s| s.borrow().as_ref().is_some_and(|st| st.exhausted))
    }

    /// Did the walk that just closed on this thread under-answer? Read it
    /// IMMEDIATELY after the projection returns, on the projection's own
    /// thread; the read clears it, so a later walk cannot inherit a stale
    /// verdict. A caller with a user-visible channel (the LSP handler's
    /// `window/showMessage`) owes them the warning — a degradation that
    /// only reaches a server log is a silently short answer.
    pub fn take_last_walk_degraded() -> bool {
        LAST_WALK_DEGRADED.with(|c| c.replace(false))
    }

    /// Consults performed vs answered from the memo for the open session.
    /// `None` when no session is open.
    pub fn stats() -> Option<SessionStats> {
        SESSION.with(|s| s.borrow().as_ref().map(|st| st.stats))
    }

    /// Declare that this walk answered with less than it could have. Any
    /// degradation a consumer cannot otherwise see routes here — the
    /// budget, and the enrichment depth cap, whose declines silently serve
    /// a raw bag where an enriched one was due. A degradation nobody can
    /// observe is the failure mode this codebase keeps finding: not a
    /// crash, a quietly smaller answer.
    pub fn mark_degraded(reason: &str) {
        SESSION.with(|s| {
            if let Some(st) = s.borrow_mut().as_mut() {
                if !st.exhausted {
                    st.exhausted = true;
                    eprintln!(
                        "perl-lsp: resolution degraded ({reason}) — this answer is an \
                         UNDER-APPROXIMATION, not a complete one."
                    );
                }
            }
        });
    }
}

impl Drop for ResolutionSession {
    fn drop(&mut self) {
        if self.owns {
            let degraded = SESSION.with(|s| {
                let st = s.borrow_mut().take();
                st.is_some_and(|st| st.exhausted)
            });
            LAST_WALK_DEGRADED.with(|c| c.set(degraded));
        }
    }
}

/// Run `f` on the session iff one is open, is bound to `idx`, and its
/// epoch still holds. A moved epoch clears the memo first (the index
/// changed under us; every remembered answer is suspect).
fn with_session<R>(
    idx: &dyn CrossFileLookup,
    f: impl FnOnce(&mut SessionState) -> R,
) -> Option<R> {
    let id = idx as *const dyn CrossFileLookup as *const () as usize;
    SESSION.with(|s| {
        let mut slot = s.borrow_mut();
        let Some(st) = slot.as_mut() else {
            // No walk open on this thread — the consult belongs to a
            // background cascade (enrichment, the open-doc heal), not to a
            // verb. Counted because "the memo did not apply" is the first
            // thing to check when a walk stays slow.
            crate::util::ghost_stats::count("session.absent");
            return None;
        };
        if st.index_id != id {
            crate::util::ghost_stats::count("session.foreign_index");
            return None;
        }
        let now = idx.resolution_epoch();
        if now != st.epoch {
            crate::util::ghost_stats::count("session.epoch_clear");
            st.epoch = now;
            st.memo.clear();
            st.candidates.clear();
            st.paths.clear();
        }
        Some(f(st))
    })
}

impl SessionState {
    fn intern(&mut self, path: &Path) -> u32 {
        if let Some(id) = self.paths.get(path) {
            return *id;
        }
        let id = self.paths.len() as u32;
        self.paths.insert(path.to_path_buf(), id);
        id
    }
}

/// `visible_def_candidates` behind the session. The index spelling clones
/// and re-sorts the candidate vec per call; one walk asks the same
/// question millions of times, so the sorted vec is shared by `Arc`.
pub(super) fn visible_def_candidates(
    idx: &dyn CrossFileLookup,
    class: &str,
) -> Arc<Vec<Arc<CachedModule>>> {
    if let Some(hit) = with_session(idx, |st| {
        st.candidates.get(class).map(Arc::clone).inspect(|_| {
            crate::util::ghost_stats::count("session.candidates_hit");
        })
    })
    .flatten()
    {
        return hit;
    }
    let v = Arc::new(idx.visible_def_candidates(class));
    with_session(idx, |st| {
        st.candidates.insert(class.to_string(), Arc::clone(&v));
    });
    v
}

/// The remembered answer for "what does candidate file `path` contribute
/// to this query", or `None` when the session can't answer.
pub(super) fn candidate_answer(
    idx: &dyn CrossFileLookup,
    path: &Path,
    q: &ReducerQuery,
) -> Option<Arc<ReducedValue>> {
    with_session(idx, |st| {
        let id = *st.paths.get(path)?;
        let key = candidate_key(id, q);
        let hit = st.memo.get(&key).map(Arc::clone);
        if hit.is_some() {
            st.stats.hits += 1;
            crate::util::ghost_stats::count("session.memo_hit");
        }
        hit
    })
    .flatten()
}

/// Remember a candidate's contribution.
///
/// A value the cycle guard truncated is remembered like any other. That is
/// the SAME acceptance `QueryState`'s own memo already makes — it stores
/// every off-path resolution regardless of whether an ancestor cut fed it,
/// and reuses it elsewhere in the query where that ancestor is not on the
/// path. The session widens the window from one query to one walk; it does
/// not introduce a new class of answer. Refusing truncated values was
/// measured instead: mutual imports make cuts near-universal at CPAN scale
/// (508,319 refusals against 5,870 stores in one walk), so the strict memo
/// remembered nothing and the walk still did not return.
pub(super) fn remember_candidate_answer(
    idx: &dyn CrossFileLookup,
    path: &Path,
    q: &ReducerQuery,
    value: &ReducedValue,
) {
    with_session(idx, |st| {
        let id = st.intern(path);
        let key = candidate_key(id, q);
        st.memo.insert(key, Arc::new(value.clone()));
        crate::util::ghost_stats::count("session.memo_store");
    });
}

fn candidate_key(path: u32, q: &ReducerQuery) -> CandidateKey {
    CandidateKey {
        path,
        attachment: q.attachment.clone(),
        receiver: q.receiver.as_ref().map(|t| format!("{t:?}")),
        arity: q.arity_hint,
        point: q.point,
        framework: q.framework,
    }
}

/// Is there budget left to chase cross-file at all? Read-only — the
/// per-candidate spend still happens in the loop. Marks the walk degraded
/// the first time it answers `false`.
pub(super) fn budget_available(idx: &dyn CrossFileLookup) -> bool {
    with_session(idx, |st| {
        let out_of_time = st.deadline.is_some_and(|d| std::time::Instant::now() >= d);
        if st.fuel > 0 && !out_of_time {
            return true;
        }
        note_exhausted(st, out_of_time);
        false
    })
    .unwrap_or(true)
}

fn note_exhausted(st: &mut SessionState, out_of_time: bool) {
    if !st.exhausted {
        st.exhausted = true;
        eprintln!(
            "perl-lsp: resolution budget exhausted ({}) — this answer is an \
             UNDER-APPROXIMATION, not a complete one. \
             PERL_LSP_RESOLVE_BUDGET_MILLISECONDS / PERL_LSP_RESOLVE_FUEL raise it \
             (0 = unbounded).",
            if out_of_time { "time" } else { "consults" },
        );
    }
    crate::util::ghost_stats::count("session.budget_exhausted");
}

/// Spend one unit of the walk's consult budget. `false` once the budget
/// is gone — the caller answers from what it already has and the walk is
/// marked degraded. No session (a plain hover, a test) ⇒ no budget.
pub(super) fn spend_consult(idx: &dyn CrossFileLookup) -> bool {
    with_session(idx, |st| {
        let out_of_time = st.deadline.is_some_and(|d| std::time::Instant::now() >= d);
        if st.fuel == 0 || out_of_time {
            note_exhausted(st, out_of_time);
            return false;
        }
        st.fuel -= 1;
        st.stats.consults += 1;
        true
    })
    .unwrap_or(true)
}
