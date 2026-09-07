use std::path::PathBuf;
use std::sync::Arc;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::lsp_types::{notification, request};
use tower_lsp::{Client, LanguageServer};

use crate::lsp::cursor_slot::identifier_prefix;
use crate::index::file_store::{FileKey, FileStore};
use crate::index::module_index::ModuleIndex;
use crate::lsp::symbols;

mod completion;
pub use completion::*;
mod gates;
use gates::*;
mod indexing;
use indexing::*;
mod lifecycle;
use lifecycle::*;
mod query;
mod server;

/// Default bounded-wait cap for the cold-open pull-verb heal (ms). A gd/hover/
/// references issued while the family index is in-flight blocks up to this long
/// awaiting completion, then resolves warm; 0 opts out. Overridable via
/// `initializationOptions.coldWaitMs`.
const DEFAULT_COLD_WAIT_MS: u64 = 400;

/// Slice-2 bag-rehydration LRU cap in MiB, from `initializationOptions.
/// maxCacheMb`. ~180 abseil bags at ~700 KB each; `0` disables retention
/// (rehydrate-and-drop). See `docs/adr/memory-slice-2-lru.md`.
pub const DEFAULT_MAX_CACHE_MB: u64 = 128;

/// Startup default for the rehydration cap: `PERL_LSP_MAX_CACHE_MB` overrides
/// `DEFAULT_MAX_CACHE_MB` when set (a QA/measurement knob — `0` forces every
/// cross-file type query to re-decode, the completeness-under-forced-rehydration
/// mode). `initializationOptions.maxCacheMb` still wins over this at `initialize`.
pub fn max_cache_mb_default() -> u64 {
    std::env::var("PERL_LSP_MAX_CACHE_MB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_MAX_CACHE_MB)
}

/// Per-language-family completion signal for the cold-open bounded wait. The
/// KICKOFF latch (`perl_indexed`/`pack_indexed`) flips synchronously on the
/// first `did_open`; these gates open on COMPLETION — the workspace/pack index
/// has attached and `heal_open_docs` ran. A pull verb arriving in the
/// in-flight window (latch set, gate closed) arms the family's `ReadyGate`
/// and waits bounded. Touched only via the gate's atomics + `Notify` — never
/// behind a FileStore guard — so the wait is deadlock-safe by construction.
#[derive(Default)]
struct IndexReady {
    perl: ReadyGate,
    pack: ReadyGate,
}

/// Fires the family's completion signal on EVERY exit path of the indexing
/// task (including the no-root early-out and a panic), so a bounded waiter is
/// never left blocking for an index that will never announce.
struct IndexDoneGuard {
    ready: Arc<IndexReady>,
    want_perl: bool,
}

impl Drop for IndexDoneGuard {
    fn drop(&mut self) {
        if self.want_perl {
            self.ready.perl.open();
        } else {
            self.ready.pack.open();
        }
    }
}

pub struct Backend {
    client: Client,
    files: Arc<FileStore>,
    module_index: Arc<ModuleIndex>,
    /// Per-document rebuild debounce (`DebouncedLatest`): each `did_change`
    /// fires it, and only the fire that survives the settle window rebuilds —
    /// so a burst of keystrokes triggers ONE analysis (~0.7s on a big
    /// macro-heavy C file) after typing settles, not one per keystroke.
    /// Pack languages only; Perl rebuilds synchronously (cheap).
    change_debounce: Arc<dashmap::DashMap<Url, Arc<ChangeGate>>>,
    /// Per-document DIAGNOSTICS debounce (`schedule_diag_refresh`): the
    /// enrich+collect+republish sequence runs off the message pipeline,
    /// debounced and serialized per URI. Separate from `change_debounce` —
    /// sharing one `DebouncedLatest` would let a diagnostics fire supersede
    /// a pending pack rebuild (and vice versa).
    diag_debounce: Arc<dashmap::DashMap<Url, Arc<ChangeGate>>>,
    /// Workspace indexing is LAZY + per-language: a family's index runs on the
    /// first `did_open` of a file in it, not eagerly at `initialized`. So a C++
    /// session in a mixed tree (e.g. perl5) never pays to index the 4000+ `.pm`
    /// files it can't use — that eager perl scan was the multi-minute first-open
    /// stall. One-shot latches, swap-guarded.
    perl_indexed: Arc<std::sync::atomic::AtomicBool>,
    pack_indexed: Arc<std::sync::atomic::AtomicBool>,
    /// Did the client advertise `window.workDoneProgress`? Server-initiated
    /// progress (`window/workDoneProgress/create`) is only legal — and only
    /// useful — when it did; sending it anyway wedges indexing behind a
    /// request minimal clients never answer.
    work_done_progress: Arc<std::sync::atomic::AtomicBool>,
    /// Did the client advertise `textDocument.typeHierarchy.dynamicRegistration`?
    /// lsp-types 0.94 (pinned by tower-lsp 0.20) has no
    /// `type_hierarchy_provider` field on `ServerCapabilities`, so the verb —
    /// fully served (`prepare_type_hierarchy`/`supertypes`/`subtypes`) — is
    /// advertised the only spec-legal way left: dynamic registration in
    /// `initialized`, gated on this flag.
    type_hierarchy_dynamic: Arc<std::sync::atomic::AtomicBool>,
    /// The pack-file invalidation owner (`index::pack_invalidator`): the
    /// serialization lock, the H9-2 bulk-index coordinator, and the H9-1
    /// generation discipline live THERE. Backend only forwards events
    /// (`file_changed` / the bulk-index begin/finish marks) and publishes
    /// the returned open-doc refresh set.
    pack_invalidator: Arc<crate::index::pack_invalidator::PackInvalidator>,
    /// Opt-in diagnostic toggles, set from `initializationOptions.diagnostics`.
    /// Shared with the resolver refresh callback (which also publishes
    /// diagnostics), hence the `Arc<Mutex<_>>`. `DiagnosticOptions` is `Copy`,
    /// so readers lock only to copy it out — never across an await. All
    /// default off; the always-on hints ignore these.
    diag_options: Arc<std::sync::Mutex<symbols::DiagnosticOptions>>,
    /// `initializationOptions.rename` options (the serde `RenameOptions` schema,
    /// same pattern as `diag_options`). `overrideScope = "dispatch"` picks the
    /// precise method-override scope; default is the whole-hierarchy family.
    rename_options: Arc<std::sync::Mutex<crate::index::resolve::RenameOptions>>,
    /// Cold-open bounded-wait completion signals per language family.
    index_ready: Arc<IndexReady>,
    /// Bounded-wait cap (ms) for the cold-open pull-verb heal; 0 disables it.
    /// Set from `initializationOptions.coldWaitMs`, default `DEFAULT_COLD_WAIT_MS`.
    cold_wait_ms: Arc<std::sync::atomic::AtomicU64>,
    /// Slice-2 rehydration LRU cap in MiB, from `initializationOptions.
    /// maxCacheMb` (default `DEFAULT_MAX_CACHE_MB`, `0` disables retention).
    max_cache_mb: Arc<std::sync::atomic::AtomicU64>,
    /// URIs whose initial `did_open` build is in flight (running off the message
    /// loop). A read verb that finds the doc still absent bounded-waits on the
    /// per-URI `ReadyGate` instead of racing an empty store — the same heal
    /// shape as `await_index_ready`, but for the file's own first build (a big
    /// macro-heavy C file is ~1.3 s and must not run synchronously on `did_open`).
    opening: Arc<dashmap::DashMap<Url, Arc<ReadyGate>>>,
    /// URIs whose OPEN analysis is DEGRADED — built with the cached-only
    /// cross-file gather (a fresh server's gather cache is empty even when
    /// modules.db is warm), pending the background full-gather heal
    /// (`PackHealCtx::run_gather_once`). Cross-file act-on-able verbs
    /// (references/rename/implementations) bounded-wait on the entry's
    /// `ReadyGate` (`await_open_full`) instead of answering from the partial
    /// closure — the answer LOOKS complete and isn't (curl: 4 sites vs 155
    /// inside the window). Per-file verbs (outline, hover) don't wait: their
    /// answers don't read the cross-file closure.
    degraded_open: Arc<dashmap::DashMap<Url, Arc<ReadyGate>>>,
    /// Live work-done progress token per degraded URI — the LSP-visible
    /// announcement that the cross-file gather is still warming and the
    /// published diagnostics are provisional. Reserved once per degraded
    /// window (subsequent keystrokes reuse it, no spam), ended when the heal
    /// lands and full-quality diagnostics publish. Absent when the client
    /// never advertised `window/workDoneProgress`. See docs/forks-resolved.md
    /// (Part 1 of the first-change-diagnostics follow-ups).
    degraded_progress: Arc<dashmap::DashMap<Url, NumberOrString>>,
    /// Per-URI single-flight coordinator for the cross-file gather heal
    /// (`docs/forks-resolved.md` Part 2). A gather already running for a URI
    /// is not re-spawned by a fresh heal request; the request coalesces into
    /// it, and the running loop re-runs at most ONCE more if the buffer moved
    /// while it gathered — N keystrokes collapse to one re-gather, not N
    /// abandoned gathers. Holds only bookkeeping counters, never analyses.
    gather_reg: Arc<GatherRegistry>,
}

/// Single-flight bookkeeping for one in-flight gather (see `GatherRegistry`).
/// `running` is the request generation the active gather is servicing;
/// `wanted` is the highest generation requested. `wanted > running` at
/// completion means a request landed mid-gather → re-run once against the
/// latest buffer, coalescing every intervening request into that one re-run.
#[derive(Clone, Copy)]
struct GatherState {
    running: u64,
    wanted: u64,
}

/// Per-URI single-flight gather coordinator — pure bookkeeping, no I/O and no
/// analyses (residency-safe). Entry present ⇒ a gather loop owns this URI.
#[derive(Default)]
struct GatherRegistry {
    inner: dashmap::DashMap<Url, GatherState>,
}

impl GatherRegistry {
    /// Register a gather request. Returns `true` when the caller must SPAWN a
    /// gather loop (the URI was idle); `false` when a loop is already running
    /// and this request coalesced into it (its `wanted` generation bumped).
    fn request(&self, uri: &Url) -> bool {
        use dashmap::mapref::entry::Entry;
        match self.inner.entry(uri.clone()) {
            Entry::Occupied(mut e) => {
                e.get_mut().wanted += 1;
                false
            }
            Entry::Vacant(v) => {
                v.insert(GatherState { running: 1, wanted: 1 });
                true
            }
        }
    }

    /// A gather iteration finished. Returns `true` when the loop must RE-RUN
    /// (a request arrived while it gathered — advance `running` to the latest
    /// `wanted`, so any number of intervening requests collapse into one
    /// re-run); `false` when the entry retired (removed — bounded, no leak).
    fn finish(&self, uri: &Url) -> bool {
        use dashmap::mapref::entry::Entry;
        match self.inner.entry(uri.clone()) {
            Entry::Occupied(mut e) => {
                let s = e.get_mut();
                if s.wanted > s.running {
                    s.running = s.wanted;
                    true
                } else {
                    e.remove();
                    false
                }
            }
            // `forget` (didClose) already retired us — stop, don't re-run.
            Entry::Vacant(_) => false,
        }
    }

    /// Drop the URI's entry (didClose). The running loop's next `finish` sees
    /// `Vacant` and stops without re-running — no leak on close.
    fn forget(&self, uri: &Url) {
        self.inner.remove(uri);
    }

    #[cfg(test)]
    fn is_inflight(&self, uri: &Url) -> bool {
        self.inner.contains_key(uri)
    }
}

/// Shared clones a background pack-gather heal needs. Built from `&self` on the
/// message loop, then moved into the spawned heal task — so the heal owns its
/// own handles and never touches `Backend`. Holds Arcs/counters only.
#[derive(Clone)]
struct PackHealCtx {
    files: Arc<FileStore>,
    module_index: Arc<ModuleIndex>,
    client: Client,
    options: symbols::DiagnosticOptions,
    degraded_open: Arc<dashmap::DashMap<Url, Arc<ReadyGate>>>,
    degraded_progress: Arc<dashmap::DashMap<Url, NumberOrString>>,
    gather_reg: Arc<GatherRegistry>,
    work_done: Arc<std::sync::atomic::AtomicBool>,
    index_ready: Arc<IndexReady>,
}

/// How long a verb is willing to wait for in-flight state
/// (`docs/open-forks.md` "Answer honesty under index/enrichment
/// windows"). The policy is DATA at each call site: a verb whose answer
/// is act-on-able (rename edits, a references sweep) declares
/// `Complete`; latency-critical interactive verbs stay `Interactive`.
/// Redirecting a verb later is a one-word change at its call site.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WaitPolicy {
/// Bounded by `cold_wait_ms` (~400 ms default): serve best-effort
/// fast, heal via refresh channels where they exist.
Interactive,
/// Wait for the in-flight build/index to actually land (generous
/// ceiling so a wedged task can't hang the verb forever). Answers
/// must not be silently partial.
Complete,
}

#[cfg(test)]
mod first_change_tests {
    //! Part 1 (degraded-window progress) + Part 2 (single-flight gather)
    //! bookkeeping — the pure coordinators, exercised without a live LSP
    //! Client. The full progress-notification + heal path is covered by the
    //! e2e/acceptance harness; here we pin the invariants the ruling names.
    use super::*;

    fn uri(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    // ---- Part 2: single-flight gather registry ----

    #[test]
    fn concurrent_requests_spawn_exactly_one_gather() {
        // Many heal requests for one URI while a gather is in flight → exactly
        // one caller is told to SPAWN; the rest coalesce.
        let reg = GatherRegistry::default();
        let u = uri("file:///a.c");
        assert!(reg.request(&u), "first request must spawn");
        for _ in 0..50 {
            assert!(!reg.request(&u), "in-flight requests must coalesce, not spawn");
        }
        assert!(reg.is_inflight(&u));
    }

    #[test]
    fn stale_generation_completion_reruns_exactly_once() {
        // N keystrokes during a running gather bump `wanted`; the loop must
        // re-run ONCE (coalescing all N), then retire — never N re-runs.
        let reg = GatherRegistry::default();
        let u = uri("file:///a.c");
        assert!(reg.request(&u)); // spawn: running=1, wanted=1
        // 5 keystrokes land while the first gather runs.
        for _ in 0..5 {
            assert!(!reg.request(&u)); // wanted climbs to 6
        }
        // First gather completes: wanted(6) > running(1) → re-run once.
        assert!(reg.finish(&u), "stale generation must re-run");
        // No requests during the re-run: it completes and retires.
        assert!(!reg.finish(&u), "up-to-date generation must retire, not re-run");
        assert!(!reg.is_inflight(&u), "entry retired — no leak");
    }

    #[test]
    fn quiescent_completion_retires_entry() {
        let reg = GatherRegistry::default();
        let u = uri("file:///a.c");
        assert!(reg.request(&u));
        assert!(!reg.finish(&u), "no intervening request → retire");
        assert!(!reg.is_inflight(&u));
        // A later request after retirement spawns a fresh loop.
        assert!(reg.request(&u), "post-retirement request spawns anew");
    }

    #[test]
    fn forget_stops_the_loop_and_cleans_the_entry() {
        // didClose: forget removes the entry; the running loop's next finish
        // sees Vacant and stops (returns false), no re-run, no leak.
        let reg = GatherRegistry::default();
        let u = uri("file:///a.c");
        assert!(reg.request(&u));
        assert!(!reg.request(&u)); // a keystroke bumped wanted — would normally re-run
        reg.forget(&u);
        assert!(!reg.is_inflight(&u), "close cleaned the entry");
        assert!(
            !reg.finish(&u),
            "closed URI must not re-run even with a pending wanted bump"
        );
    }

    #[test]
    fn registries_are_independent_per_uri() {
        let reg = GatherRegistry::default();
        let a = uri("file:///a.c");
        let b = uri("file:///b.c");
        assert!(reg.request(&a));
        assert!(reg.request(&b), "a second URI spawns its own gather");
        assert!(!reg.finish(&a), "a retires with no intervening request");
        assert!(!reg.is_inflight(&a));
        assert!(reg.is_inflight(&b), "retiring a must not touch b");
    }

    #[test]
    fn many_threads_race_to_one_spawn() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let reg = Arc::new(GatherRegistry::default());
        let u = uri("file:///race.c");
        let spawns = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..32 {
            let reg = Arc::clone(&reg);
            let u = u.clone();
            let spawns = Arc::clone(&spawns);
            handles.push(std::thread::spawn(move || {
                if reg.request(&u) {
                    spawns.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(
            spawns.load(Ordering::Relaxed),
            1,
            "exactly one thread wins the spawn under contention"
        );
    }

    // ---- Part 1: degraded-window progress token reservation ----

    #[test]
    fn one_begin_per_window_reused_across_keystrokes() {
        // The progress token is reserved once per degraded window; subsequent
        // didChanges inside the same window reuse it (no per-keystroke Begin).
        let map: dashmap::DashMap<Url, NumberOrString> = dashmap::DashMap::new();
        let u = uri("file:///a.c");
        let t0 = NumberOrString::String("perl-lsp/degraded-0".into());
        assert!(
            reserve_degraded_token(&map, &u, t0.clone()),
            "first reservation mints the token"
        );
        for i in 1..10 {
            let t = NumberOrString::String(format!("perl-lsp/degraded-{i}"));
            assert!(
                !reserve_degraded_token(&map, &u, t),
                "reservations within the same window reuse the open token"
            );
        }
        // The stored token is still the first one (later mints were discarded).
        assert_eq!(map.get(&u).map(|v| v.clone()), Some(t0));
    }

    #[test]
    fn releasing_the_window_allows_a_fresh_begin() {
        let map: dashmap::DashMap<Url, NumberOrString> = dashmap::DashMap::new();
        let u = uri("file:///a.c");
        assert!(reserve_degraded_token(
            &map,
            &u,
            NumberOrString::String("t0".into())
        ));
        // clear_degraded / close removes the entry (window over).
        assert!(map.remove(&u).is_some());
        // Next degraded window mints a fresh token.
        assert!(
            reserve_degraded_token(&map, &u, NumberOrString::String("t1".into())),
            "a new window announces itself with a new token"
        );
    }
}
