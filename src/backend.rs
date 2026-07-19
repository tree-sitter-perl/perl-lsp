use std::path::PathBuf;
use std::sync::Arc;

use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;
use tower_lsp::lsp_types::{notification, request};
use tower_lsp::{Client, LanguageServer};

use crate::cursor_slot::identifier_prefix;
use crate::file_store::{FileKey, FileStore};
use crate::module_index::ModuleIndex;
use crate::symbols;

/// Pack-language completion: member access (sentinel reparse → receiver
/// span → type → members) with an in-scope-symbol fallback. Shared by the
/// LSP completion handler and the CLI/--batch mirror so the editor and
/// gold agree. Perl completion stays in `cursor_context`.
///
/// The returned flag is LSP `is_incomplete`: true when the bare-identifier
/// half consulted prefix-gated cross-file sources (macros / include-closure
/// symbols) — those are filtered server-side by the typed prefix, so the
/// client must re-request as the prefix changes instead of trusting its
/// cached list. Member completion and closure-less languages return a
/// complete list (false).
pub fn pack_completion(
    files: &crate::file_store::FileStore,
    analysis: &crate::file_analysis::FileAnalysis,
    source: &str,
    tree: &tree_sitter::Tree,
    point: tree_sitter::Point,
    language: &str,
    path: Option<&std::path::Path>,
    module_index: &ModuleIndex,
) -> (Vec<CompletionItem>, bool) {
    // Cross-file resolves against THIS language's sub-index (its own
    // cache — no cross-language overlap), falling back to the hub when
    // none is attached.
    let pack = module_index.pack_index(language);
    let base_idx: &dyn crate::file_analysis::CrossFileLookup =
        pack.as_deref().map_or(module_index, |i| i);
    // Scope member/type resolution to the file's include closure.
    let scoped = crate::file_analysis::ScopedLookup::new(
        base_idx, &analysis.include_closure, path);
    let xidx: &dyn crate::file_analysis::CrossFileLookup = &scoped;
    // The slot verdict — Member (sentinel reparse → receiver span →
    // type) or the bare-identifier fallback (no registered driver / no
    // LangPack / no dangling member access) — comes from the one
    // cursor-tier entry (`docs/adr/cursor-slots.md`); this adapter only
    // projects it onto LSP items.
    let crate::cursor_slot::DetectedSlot { slot, .. } =
        crate::cursor_slot::detect_slot(analysis, tree, source, point, language, Some(xidx));
    if let crate::cursor_slot::Slot::Member { receiver, .. } = &slot {
        if let Some(class) =
            receiver.receiver_type.as_ref().and_then(|ty| ty.class_name().map(|s| s.to_string()))
        {
            // Mode A: the member items carry the operator-swap edit
            // (`p.` → `p->`) when the receiver's pointer depth wants
            // a different operator than was typed. The diagnostic
            // path (Mode B) is the universal fallback.
            if let Some(items) = symbols::member_completion_for_class(
                analysis, &class, xidx, receiver.op_fix.clone(), point,
            ) {
                return (items, false);
            }
        }
    }
    // `fmtx::|` — a qualified path completes to the OWNER's members
    // (workspace + dependency roles), never the global pool: the qualifier
    // is a hard filter by meaning. The gather is the CandidateSet's
    // qualified-path projection (pack lane), anchored on the same qualifier
    // detection goto-def uses. Falls through to the bare-identifier universe
    // when the owner resolves nothing (e.g. a macro-guarded namespace open
    // left members unattributed), mirroring gd's owner-anchored
    // fall-through.
    if let crate::cursor_slot::Slot::ModulePath { ref prefix, .. } = slot {
        let cs = crate::resolve::resolve(
            files,
            analysis,
            crate::file_store::FileKey::Path(
                path.map(|p| p.to_path_buf()).unwrap_or_default(),
            ),
            point,
            Some(base_idx),
            crate::resolve::OverrideScope::default(),
        )
        .pack_routed();
        let candidates = cs.complete_qualified_path(xidx, prefix);
        if !candidates.is_empty() {
            return (
                candidates.into_iter().map(symbols::candidate_to_completion_item).collect(),
                false,
            );
        }
    }
    // `o->op_type == |` — the equality's field operand types the slot to
    // an enum DOMAIN. Rank that enum's members first (never prune the
    // bare-identifier universe): the type-constrained-completion payoff of
    // the `Slot::expected_type` seam (`docs/adr/cursor-slots.md`).
    if let crate::cursor_slot::Slot::ArgPosition { .. } = &slot {
        if let Some(crate::file_analysis::InferredType::ClassName(enum_name)) =
            slot.expected_type(analysis, point, Some(xidx))
        {
            let members = analysis.enum_members(&enum_name, Some(xidx));
            if !members.is_empty() {
                let mut items = symbols::in_scope_completion(analysis, point);
                let macros_live = macro_completion(source, point, language, path, &mut items);
                let closure_live = closure_symbol_completion(
                    files, analysis, source, point, language, path, module_index, &mut items);
                rank_domain_members(&mut items, &members, &enum_name);
                return (items, macros_live || closure_live);
            }
        }
    }
    let mut items = symbols::in_scope_completion(analysis, point);
    let macros_live = macro_completion(source, point, language, path, &mut items);
    let closure_live = closure_symbol_completion(
        files, analysis, source, point, language, path, module_index, &mut items);
    (items, macros_live || closure_live)
}

/// Move a domain's enum members to the front of the completion list with a
/// leading sort_text so the client ranks them first, without pruning the
/// bare-identifier universe already gathered. Members keep declaration order
/// (their numeric enum order) via a fixed-width index, and any copy already
/// present in the gathered list (an in-scope enumerator) is de-duplicated so
/// the ranked entry is the only one.
fn rank_domain_members(items: &mut Vec<CompletionItem>, members: &[String], enum_name: &str) {
    let member_set: std::collections::HashSet<&str> =
        members.iter().map(String::as_str).collect();
    items.retain(|i| !member_set.contains(i.label.as_str()));
    // "000" leads: '0' (0x30) sorts before every identifier first char.
    let mut ranked: Vec<CompletionItem> = members
        .iter()
        .enumerate()
        .map(|(i, m)| CompletionItem {
            label: m.clone(),
            kind: Some(CompletionItemKind::ENUM_MEMBER),
            detail: Some(enum_name.to_string()),
            sort_text: Some(format!("000{:04}{}", i, m)),
            ..Default::default()
        })
        .collect();
    ranked.append(items);
    *items = ranked;
}

/// Bare-identifier cross-file completion: the file-scope symbols of every
/// header in the file's `#include` closure — C's import surface ("C = Perl,
/// everything exported": the closure is the import list, so enum constants,
/// free functions, typedefs and globals from included headers are candidates
/// exactly like imported subs are for Perl). Enumeration is gated to
/// closure-member files (`visible_defs_with_prefix` — a file that doesn't
/// include a header never sees its names) and prefix-gated like macros (no
/// bare-cursor dump of a large closure). Own-file symbols win dedup; closure
/// items sort after them (`~` sorts past every identifier char). Cross-file
/// `#define`s arrive via `macro_completion`, which also reaches headers the
/// workspace index never parsed; the dedup order makes its richer
/// `#define`-body detail win for names both sources know.
///
/// Returns whether this source is live for the file (a non-empty closure) —
/// the `is_incomplete` signal, independent of whether the current prefix
/// matched anything.
fn closure_symbol_completion(
    files: &crate::file_store::FileStore,
    analysis: &crate::file_analysis::FileAnalysis,
    source: &str,
    point: tree_sitter::Point,
    language: &str,
    path: Option<&std::path::Path>,
    module_index: &ModuleIndex,
    items: &mut Vec<CompletionItem>,
) -> bool {
    if analysis.include_closure.is_empty() {
        return false;
    }
    let cursor = crate::cursor_sentinel::point_to_byte(source, point);
    let prefix = identifier_prefix(source, cursor);
    if prefix.is_empty() {
        return true; // live source, waiting for a prefix
    }
    let pack = module_index.pack_index(language);
    let base_idx: &dyn crate::file_analysis::CrossFileLookup =
        pack.as_deref().map_or(module_index, |i| i);
    let seen: std::collections::HashSet<String> =
        items.iter().map(|i| i.label.clone()).collect();
    // The closure-gated identifier universe is the set's completion
    // projection (the cpp instance of `complete(prefix)`); this adapter
    // owns slot detection (the typed prefix), dedup against in-scope
    // items, and presentation (the past-`z` sort tier).
    let cs = crate::resolve::resolve(
        files,
        analysis,
        crate::file_store::FileKey::Path(path.map(|p| p.to_path_buf()).unwrap_or_default()),
        point,
        Some(base_idx),
        crate::resolve::OverrideScope::default(),
    )
    .pack_routed();
    let candidates =
        crate::timings::phase("completion.closure_symbols", || cs.complete(prefix, false));
    for c in candidates {
        if seen.contains(&c.label) {
            continue;
        }
        items.push(CompletionItem {
            label: c.label.clone(),
            kind: Some(symbols::fa_completion_kind(&c.kind)),
            detail: c.detail,
            sort_text: Some(format!("~{}", c.label)),
            ..Default::default()
        });
    }
    true
}

/// Identifier-context macro completion (C preprocessor): the `#define`s
/// reachable through `#include`s — the API surface (perl5: `Newx`/`SvPV`).
/// Does this language's pack declare `#include`-style path tokens (the
/// header-is-the-module reference goto-def resolves and references reverses)?
/// Asked of the pack via the single `for_id` lookup — Perl's driver has no
/// `LangPack` so it answers false without a language-name branch (rule #10).
fn language_has_include_tokens(language: &str) -> bool {
    crate::language_driver::LanguageRegistry::with_enabled()
        .for_id(language)
        .and_then(|d| d.lang_pack())
        .is_some_and(|p| p.include_path_tokens)
}

/// The file's OWN `#define`s are already symbols (in `items`); this adds the
/// cross-file ones. Prefix-filtered server-side (a macro-heavy include
/// closure reaches thousands — perl.h alone is ~2000), and the header cache
/// is warm from analyze, so the re-gather is cheap.
///
/// Returns whether this source is live for the file (C preprocessor + a
/// path to gather from) — the `is_incomplete` signal, independent of
/// whether the current prefix matched anything.
fn macro_completion(
    source: &str,
    point: tree_sitter::Point,
    language: &str,
    path: Option<&std::path::Path>,
    items: &mut Vec<CompletionItem>,
) -> bool {
    if language != "cpp" {
        return false; // only C/C++ have a preprocessor
    }
    let Some(p) = path else { return false };
    let reg = crate::language_driver::LanguageRegistry::with_enabled();
    let Some(driver) = reg.for_id(language) else { return false };
    let cursor = crate::cursor_sentinel::point_to_byte(source, point);
    let prefix = identifier_prefix(source, cursor);
    if prefix.is_empty() {
        return true; // no bare-cursor dump of the whole macro table
    }
    let mut parser = driver.make_parser();
    let macros = crate::cpp_reparse::included_macros(p, source, &mut parser);
    let seen: std::collections::HashSet<String> =
        items.iter().map(|i| i.label.clone()).collect();
    for (name, m) in macros.iter() {
        if !name.starts_with(prefix) || seen.contains(name) {
            continue;
        }
        let (kind, detail) = match &m.params {
            Some(params) => (
                CompletionItemKind::FUNCTION,
                format!("#define {}({})", name, params.join(", ")),
            ),
            None => (
                CompletionItemKind::CONSTANT,
                format!("#define {} {}", name, m.body.trim()),
            ),
        };
        items.push(CompletionItem {
            label: name.clone(),
            kind: Some(kind),
            detail: Some(detail),
            // Cross-file candidates rank after own-file symbols (which
            // carry no sort_text, so clients sort them by bare label).
            sort_text: Some(format!("~{}", name)),
            ..Default::default()
        });
    }
    true
}

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
/// first `did_open`; these fire on COMPLETION — the workspace/pack index has
/// attached and `heal_open_docs` ran. A pull verb arriving in the in-flight
/// window (latch set, `done` clear) registers on the matching `Notify` and
/// waits bounded. Touched only via atomics + `Notify::notify_waiters` — never
/// behind a FileStore guard — so the wait is deadlock-safe by construction.
#[derive(Default)]
struct IndexReady {
    perl_done: std::sync::atomic::AtomicBool,
    pack_done: std::sync::atomic::AtomicBool,
    perl_notify: tokio::sync::Notify,
    pack_notify: tokio::sync::Notify,
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
        use std::sync::atomic::Ordering;
        if self.want_perl {
            self.ready.perl_done.store(true, Ordering::Relaxed);
            self.ready.perl_notify.notify_waiters();
        } else {
            self.ready.pack_done.store(true, Ordering::Relaxed);
            self.ready.pack_notify.notify_waiters();
        }
    }
}

pub struct Backend {
    client: Client,
    files: Arc<FileStore>,
    module_index: Arc<ModuleIndex>,
    /// Per-document edit generation. Each `did_change` bumps it; a debounced
    /// rebuild task only proceeds if its captured generation is still the
    /// latest — so a burst of keystrokes triggers ONE analysis (~0.7s on a
    /// big macro-heavy C file) after typing settles, not one per keystroke.
    /// Pack languages only; Perl rebuilds synchronously (cheap).
    change_gen: Arc<dashmap::DashMap<Url, u64>>,
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
    /// Serializes pack-file invalidation runs (did_save + watcher events can
    /// race on the same header; unregister/register swaps must not interleave).
    pack_change_lock: Arc<std::sync::Mutex<()>>,
    /// Opt-in diagnostic toggles, set from `initializationOptions.diagnostics`.
    /// Shared with the resolver refresh callback (which also publishes
    /// diagnostics), hence the `Arc<Mutex<_>>`. `DiagnosticOptions` is `Copy`,
    /// so readers lock only to copy it out — never across an await. All
    /// default off; the always-on hints ignore these.
    diag_options: Arc<std::sync::Mutex<symbols::DiagnosticOptions>>,
    /// `initializationOptions.rename` options (the serde `RenameOptions` schema,
    /// same pattern as `diag_options`). `overrideScope = "dispatch"` picks the
    /// precise method-override scope; default is the whole-hierarchy family.
    rename_options: Arc<std::sync::Mutex<crate::resolve::RenameOptions>>,
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
    /// per-URI `Notify` instead of racing an empty store — the same heal shape
    /// as `await_index_ready`, but for the file's own first build (a big
    /// macro-heavy C file is ~1.3 s and must not run synchronously on `did_open`).
    opening: Arc<dashmap::DashMap<Url, Arc<tokio::sync::Notify>>>,
    /// URIs whose OPEN analysis is DEGRADED — built with the cached-only
    /// cross-file gather (a fresh server's gather cache is empty even when
    /// modules.db is warm), pending the background full-gather heal
    /// (`PackHealCtx::run_gather_once`). Cross-file act-on-able verbs
    /// (references/rename/implementations) bounded-wait on the entry's
    /// `Notify` (`await_open_full`) instead of answering from the partial
    /// closure — the answer LOOKS complete and isn't (curl: 4 sites vs 155
    /// inside the window). Per-file verbs (outline, hover) don't wait: their
    /// answers don't read the cross-file closure.
    degraded_open: Arc<dashmap::DashMap<Url, Arc<tokio::sync::Notify>>>,
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
    degraded_open: Arc<dashmap::DashMap<Url, Arc<tokio::sync::Notify>>>,
    degraded_progress: Arc<dashmap::DashMap<Url, NumberOrString>>,
    gather_reg: Arc<GatherRegistry>,
    work_done: Arc<std::sync::atomic::AtomicBool>,
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

impl Backend {
    fn diagnostic_options(&self) -> symbols::DiagnosticOptions {
        *self.diag_options.lock().unwrap()
    }

    /// The configured method-override fan-out scope for references + rename.
    fn override_scope(&self) -> crate::resolve::OverrideScope {
        self.rename_options.lock().unwrap().override_scope
    }

    /// Index the opened file's language FAMILY's workspace, once, in the
    /// background. `perl` → the `.pm/.pl/.t` scan; any pack language (C++/
    /// Python/…) → the pack-language scan. Latched per family so a C++-only
    /// session never touches the perl tree, and vice versa.
    fn ensure_workspace_indexed(&self, language: &str) {
        use std::sync::atomic::Ordering;
        let want_perl = language == "perl";
        let latch = if want_perl { &self.perl_indexed } else { &self.pack_indexed };
        if latch.swap(true, Ordering::Relaxed) {
            return; // already indexed (or in flight)
        }
        let files = Arc::clone(&self.files);
        let client = self.client.clone();
        let module_index = Arc::clone(&self.module_index);
        let root = self.module_index.workspace_root();
        let options = self.diagnostic_options();
        // Server-initiated progress requires the client capability; a client
        // that never advertised it may also never ANSWER the create request —
        // and indexing must proceed regardless (LSP spec).
        let progress = self
            .work_done_progress
            .load(std::sync::atomic::Ordering::Relaxed);
        let index_ready = Arc::clone(&self.index_ready);
        let heal_ctx = self.pack_heal_ctx();
        let bag_cache_bytes =
            self.max_cache_mb.load(std::sync::atomic::Ordering::Relaxed) as usize * 1024 * 1024;
        tokio::task::spawn_blocking(move || {
            // Announces completion (or the no-root early-out) to bounded waiters
            // on Drop — every exit path of this closure, panic included.
            let _done = IndexDoneGuard { ready: index_ready, want_perl };
            let Some(root_uri) = root else { return };
            let Some(root_path) = root_uri.strip_prefix("file://") else { return };
            let root_path = PathBuf::from(root_path);
            let rt = tokio::runtime::Handle::current();
            let token = NumberOrString::String(format!(
                "perl-lsp/workspace-index-{}",
                if want_perl { "perl" } else { "pack" }
            ));
            if progress {
                // tower-lsp holds the server→client request's oneshot SENDER in
                // its pending map until the reply lands, and panics ("receiver
                // already dropped") if that reply arrives after we dropped the
                // RECEIVER. A bare `timeout(.., send_request)` drops the receiver
                // on timeout, so a slow client's late `create` reply would take
                // the whole server down (#36). Spawn the request onto a DETACHED
                // task instead: dropping its `JoinHandle` on timeout leaves the
                // task — and its receiver — alive, so a late reply routes to a
                // live receiver (a harmless `Ok`) rather than panicking. The 2s
                // cap only bounds how long we wait; indexing must proceed even if
                // a capable-but-slow client never answers.
                let create = rt.spawn({
                    let client = client.clone();
                    let token = token.clone();
                    async move {
                        let _ = client
                            .send_request::<request::WorkDoneProgressCreate>(
                                WorkDoneProgressCreateParams { token },
                            )
                            .await;
                    }
                });
                let _ = rt.block_on(tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    create,
                ));
                rt.block_on(client.send_notification::<notification::Progress>(ProgressParams {
                    token: token.clone(),
                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(
                        WorkDoneProgressBegin {
                            title: "Indexing workspace".into(),
                            cancellable: Some(false),
                            message: Some("Scanning files...".into()),
                            percentage: Some(0),
                        },
                    )),
                }));
            }
            // Throttled percentage progress. The Rayon index workers call `cb`
            // per file (cheap: an atomic `fetch_max` guard); only a ≥2% advance
            // (or the final tick) crosses the channel, where a tokio task owns
            // the actual `Report` notification. This keeps `send_notification`
            // OFF the Rayon worker threads — no `block_on` from the pool — and
            // bounds emissions to ~50 per index regardless of file count.
            let emitter = progress.then(|| {
                let (tx, mut rx) =
                    tokio::sync::mpsc::unbounded_channel::<(u32, usize, usize)>();
                let client_e = client.clone();
                let token_e = token.clone();
                let handle = rt.spawn(async move {
                    while let Some((pct, done, total)) = rx.recv().await {
                        client_e
                            .send_notification::<notification::Progress>(ProgressParams {
                                token: token_e.clone(),
                                value: ProgressParamsValue::WorkDone(
                                    WorkDoneProgress::Report(WorkDoneProgressReport {
                                        cancellable: Some(false),
                                        message: Some(format!("{done}/{total} files")),
                                        percentage: Some(pct),
                                    }),
                                ),
                            })
                            .await;
                    }
                });
                (tx, handle)
            });
            let last_pct = std::sync::atomic::AtomicU8::new(0);
            let cb = emitter.as_ref().map(|(tx, _)| {
                let tx = tx.clone();
                move |done: usize, total: usize| {
                    let pct = if total == 0 {
                        100u8
                    } else {
                        ((done * 100 / total).min(100)) as u8
                    };
                    let prev = last_pct.fetch_max(pct, std::sync::atomic::Ordering::Relaxed);
                    if pct >= prev.saturating_add(2) || done >= total {
                        let _ = tx.send((pct as u32, done, total));
                    }
                }
            });
            let cb_ref: Option<&(dyn Fn(usize, usize) + Sync)> =
                cb.as_ref().map(|c| c as &(dyn Fn(usize, usize) + Sync));
            let count = if want_perl {
                crate::module_resolver::index_workspace_with_index(
                    &root_path,
                    &files,
                    Some(&module_index),
                    cb_ref,
                )
            } else {
                crate::module_resolver::index_pack_languages(
                    &root_path,
                    Some(root_uri.as_str()),
                    &module_index,
                    cb_ref,
                    bag_cache_bytes,
                )
            };
            // Drop the sender(s) so the emitter's channel closes, then drain it
            // — guarantees the final Report lands before End.
            drop(cb);
            if let Some((tx, handle)) = emitter {
                drop(tx);
                let _ = rt.block_on(handle);
            }
            if progress {
                rt.block_on(client.send_notification::<notification::Progress>(ProgressParams {
                    token,
                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(
                        WorkDoneProgressEnd {
                            message: Some(if want_perl {
                                format!("Indexed {} Perl files", count)
                            } else {
                                let reg = crate::language_driver::LanguageRegistry::with_enabled();
                                let langs: Vec<&str> = reg
                                    .languages()
                                    .into_iter()
                                    .filter(|id| *id != "perl")
                                    .map(crate::language_driver::LanguageRegistry::display_name)
                                    .collect();
                                format!("Indexed {} {} files", count, langs.join("/"))
                            }),
                        },
                    )),
                }));
            }
            // Heal the cold-open degraded window: the index this file's family
            // needs has now ATTACHED (the latch marked KICKOFF; this is the
            // completion signal). Re-analyze + re-publish every open doc in the
            // family so pull-verb answers baked in the cached-only open window
            // (truncated cross-file closure, `None` gd/hover) self-heal without
            // the user re-triggering.
            Self::heal_open_docs(&heal_ctx, want_perl);
        });
    }

    /// Re-derive + re-publish every OPEN document in a language family after its
    /// workspace index / macro gather lands — the pull-verb heal for the
    /// cold-open degraded window. Pack docs get a full OFF-lock re-analysis
    /// (their `did_open` gather was cached-only + the cross-file index is now
    /// warm); perl docs get an enrich + diagnostics re-publish.
    ///
    /// FileStore guard discipline: pack URIs are collected under a read guard
    /// that is DROPPED before any re-analysis, and each re-analysis snapshots
    /// text off the lock (`PackHealCtx::run_gather_once`). The perl branch enriches
    /// under the write guard but touches only `module_index` (never re-locks the
    /// store) and publishes after the guard drops — the same shape the resolver
    /// `on_refresh` callback already uses safely.
    fn heal_open_docs(ctx: &PackHealCtx, want_perl: bool) {
        log::debug!(
            "cold-window heal: index landed for {} family",
            if want_perl { "perl" } else { "pack" }
        );
        if want_perl {
            let mut pending: Vec<(Url, Vec<Diagnostic>)> = Vec::new();
            ctx.files.for_each_open_mut(|uri, doc| {
                if doc.language != "perl" {
                    return;
                }
                std::sync::Arc::make_mut(&mut doc.analysis)
                    .enrich_imported_types_with_keys(Some(ctx.module_index.as_ref()));
                let diags =
                    symbols::collect_diagnostics(&doc.analysis, &ctx.module_index, ctx.options);
                pending.push((uri.clone(), diags));
            });
            if pending.is_empty() {
                return;
            }
            let client = ctx.client.clone();
            tokio::spawn(async move {
                for (uri, diags) in pending {
                    client.publish_diagnostics(uri, diags, None).await;
                }
            });
        } else {
            let mut uris: Vec<Url> = Vec::new();
            ctx.files.for_each_open(|uri, doc| {
                if doc.language != "perl" {
                    uris.push(uri.clone());
                }
            });
            // Route each through the single-flight registry: a doc already
            // mid-gather coalesces instead of double-gathering its cone.
            for uri in uris {
                ctx.request_gather(uri);
            }
        }
    }

    /// Bounded wait for the opened file's language-family workspace/pack index
    /// to finish, when — and ONLY when — it is actually in-flight: KICKED OFF
    /// (`ensure_workspace_indexed` flipped the latch at `did_open`) but not yet
    /// DONE. This closes the residual cold-open window for PULL verbs
    /// (goto-def / hover / references): unlike completion (`isIncomplete`) and
    /// diagnostics (server re-push), a one-shot gd/hover the user fired in the
    /// window got its degraded answer and is gone. Blocking the handler briefly
    /// for the imminent index lets it resolve against the warm cross-file index
    /// instead (e.g. references `op_free` 1 → 118).
    ///
    /// Zero added latency in the common cases: the warm session (index already
    /// `done` → returns before awaiting) and the no-index case (latch never set).
    /// Bounded by `cold_wait_ms` (0 opts out) so it can never wedge, and on
    /// timeout the handler resolves degraded exactly as before.
    ///
    /// GUARD DISCIPLINE: holds NO FileStore guard across the await — it touches
    /// only the family's `done` atomic + `Notify`. Callers peek `language` under
    /// a `get_open` guard that DROPS before this await, and snapshot `analysis`
    /// fresh AFTER it, picking up any heal (see the hazard note on
    /// `FileStore::for_each_open`).
    /// `WaitPolicy` → millisecond cap. `cold_wait_ms == 0` is the global
    /// "never block" opt-out and wins over any policy.
    fn wait_cap(&self, policy: WaitPolicy) -> u64 {
        let interactive = self
            .cold_wait_ms
            .load(std::sync::atomic::Ordering::Relaxed);
        match policy {
            _ if interactive == 0 => 0,
            WaitPolicy::Interactive => interactive,
            // Generous ceiling: bounded (a wedged index can't hang the verb
            // forever) but far beyond any real build/index time.
            WaitPolicy::Complete => 120_000,
        }
    }

    async fn await_index_ready(&self, language: &str, policy: WaitPolicy) {
        use std::sync::atomic::Ordering;
        let want_perl = language == "perl";
        let latch = if want_perl { &self.perl_indexed } else { &self.pack_indexed };
        let (done, notify) = if want_perl {
            (&self.index_ready.perl_done, &self.index_ready.perl_notify)
        } else {
            (&self.index_ready.pack_done, &self.index_ready.pack_notify)
        };
        // Only wait when an index is actually coming: kicked off but not done.
        if !latch.load(Ordering::Relaxed) || done.load(Ordering::Relaxed) {
            return;
        }
        let cap = self.wait_cap(policy);
        if cap == 0 {
            return; // opt-out
        }
        // Register interest BEFORE the final `done` re-check to close the
        // notify lost-wakeup race (a completion between the first check and the
        // await would otherwise be missed), then wait bounded.
        let waited = notify.notified();
        if done.load(Ordering::Relaxed) {
            return;
        }
        self.bounded_wait_with_progress(cap, waited, "Waiting for workspace index")
            .await;
    }

    /// Bounded wait for a freshly-opened document's INITIAL build, when it is
    /// still in flight (`did_open` runs the build off the message loop so the
    /// loop stays responsive during the ~1.3 s cold build of a big C file). A
    /// read verb calls this before `get_open`: a small/medium file (build <
    /// cap) resolves warm on the first pull exactly as before; a pathological
    /// file degrades after the cap and heals once the build lands + republishes.
    ///
    /// GUARD DISCIPLINE: holds NO FileStore / DashMap guard across the await —
    /// it snapshots the `Notify` Arc out of the `opening` map and drops that
    /// guard before awaiting. Callers snapshot `analysis` fresh AFTER it.
    async fn await_open_ready(&self, uri: &Url, policy: WaitPolicy) {
        if self.files.get_open(uri).is_some() {
            return; // already built
        }
        let Some(notify) = self.opening.get(uri).map(|n| Arc::clone(n.value())) else {
            return; // not an in-flight open (unknown/closed file)
        };
        let cap = self.wait_cap(policy);
        if cap == 0 {
            return; // opt-out
        }
        // Register interest BEFORE the final presence re-check to close the
        // notify lost-wakeup race, then wait bounded.
        let waited = notify.notified();
        if self.files.get_open(uri).is_some() {
            return;
        }
        self.bounded_wait_with_progress(cap, waited, "Waiting for file analysis")
            .await;
    }

    /// Bounded wait for the open document's FULL-quality analysis — past the
    /// degraded cached-only-gather window (`degraded_open`). Only cross-file
    /// act-on-able verbs (references / rename / implementations) call this,
    /// AFTER `await_open_ready`: their answers read the cross-file closure,
    /// and inside the window they return a subset that looks complete (curl:
    /// 4 reference sites vs 155). Per-file verbs (outline, hover, completion)
    /// deliberately don't — their answers don't need the gather, and waiting
    /// would regress open→outline latency for nothing. `Interactive` policy
    /// returns immediately: fast-best-effort verbs keep today's behavior.
    async fn await_open_full(&self, uri: &Url, policy: WaitPolicy) {
        if !matches!(policy, WaitPolicy::Complete) {
            return;
        }
        let Some(notify) = self.degraded_open.get(uri).map(|n| Arc::clone(n.value())) else {
            return; // not degraded (perl doc, heal already landed, or never opened)
        };
        let cap = self.wait_cap(policy);
        if cap == 0 {
            return; // opt-out
        }
        // Register interest BEFORE the re-check (lost-wakeup discipline).
        let waited = notify.notified();
        if !self.degraded_open.contains_key(uri) {
            return;
        }
        self.bounded_wait_with_progress(cap, waited, "Waiting for cross-file analysis")
            .await;
    }

    /// Bounded wait that surfaces as client progress once it actually
    /// BLOCKS. Silent for the first 500 ms — warm paths and every
    /// `Interactive` wait (cap ≤ ~400 ms) resolve inside it, so no UI
    /// noise; only a `Complete` wait that outlives the quiet window mints
    /// a work-done token, keeping the honest-answer block visible instead
    /// of reading as a hung request.
    async fn bounded_wait_with_progress<F>(&self, cap_ms: u64, wait: F, title: &str)
    where
        F: std::future::Future<Output = ()>,
    {
        use std::time::Duration;
        const QUIET_MS: u64 = 500;
        tokio::pin!(wait);
        let quiet = cap_ms.min(QUIET_MS);
        if tokio::time::timeout(Duration::from_millis(quiet), &mut wait)
            .await
            .is_ok()
        {
            return;
        }
        let remaining = cap_ms.saturating_sub(quiet);
        if remaining == 0 {
            return;
        }
        // Server-initiated progress requires the client capability; a client
        // that never advertised it may also never answer the create request.
        if !self
            .work_done_progress
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            let _ = tokio::time::timeout(Duration::from_millis(remaining), &mut wait).await;
            return;
        }
        static WAIT_TOKEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let token = NumberOrString::String(format!(
            "perl-lsp/wait-{}",
            WAIT_TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        progress_create_and_begin(&self.client, &token, title).await;
        let _ = tokio::time::timeout(Duration::from_millis(remaining), &mut wait).await;
        progress_end(&self.client, token).await;
    }
}

/// The one spelling of "create + begin a work-done progress" — reused by the
/// blocking-wait announcement (`bounded_wait_with_progress`) and the degraded
/// diagnostics announcement (`PackHealCtx::begin_progress`). The detached
/// create-request task keeps the oneshot receiver alive past the 2 s timeout,
/// so a late reply can't panic tower-lsp's pending map (#36). Capability
/// gating is the caller's responsibility — a token minted here presumes the
/// client advertised `window/workDoneProgress`.
async fn progress_create_and_begin(client: &Client, token: &NumberOrString, title: &str) {
    let create = tokio::spawn({
        let client = client.clone();
        let token = token.clone();
        async move {
            let _ = client
                .send_request::<request::WorkDoneProgressCreate>(WorkDoneProgressCreateParams {
                    token,
                })
                .await;
        }
    });
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), create).await;
    client
        .send_notification::<notification::Progress>(ProgressParams {
            token: token.clone(),
            value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(WorkDoneProgressBegin {
                title: title.into(),
                cancellable: Some(false),
                message: None,
                percentage: None,
            })),
        })
        .await;
}

/// Reserve the per-window progress slot for `uri` atomically. Returns `true`
/// exactly once per window — the first caller mints the token; every later
/// caller reuses it (returns `false`), so a keystroke burst inside one degraded
/// window announces itself with a single Begin, not one per change. Releasing
/// the slot (`clear_degraded`/close removes the entry) lets the next window
/// reserve again. The DashMap entry guard is dropped before return — no lock is
/// held across the caller's subsequent `.await`.
fn reserve_degraded_token(
    map: &dashmap::DashMap<Url, NumberOrString>,
    uri: &Url,
    token: NumberOrString,
) -> bool {
    use dashmap::mapref::entry::Entry;
    match map.entry(uri.clone()) {
        Entry::Occupied(_) => false,
        Entry::Vacant(v) => {
            v.insert(token);
            true
        }
    }
}

/// The one spelling of "end a work-done progress".
async fn progress_end(client: &Client, token: NumberOrString) {
    client
        .send_notification::<notification::Progress>(ProgressParams {
            token,
            value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(WorkDoneProgressEnd {
                message: None,
            })),
        })
        .await;
}

impl PackHealCtx {
    /// Single-flight gather request. If a gather loop is already running for
    /// `uri`, coalesces into it (no new task); otherwise registers the URI and
    /// spawns the loop. Never awaits a gather — the change path stays
    /// cached-only + fire-and-forget.
    fn request_gather(&self, uri: Url) {
        if !self.gather_reg.request(&uri) {
            return; // a loop already owns this URI; the request coalesced in
        }
        let ctx = self.clone();
        tokio::spawn(async move {
            ctx.run_gather_loop(uri).await;
        });
    }

    /// One gather owner per URI: gather → (maybe) re-run once if the buffer
    /// moved mid-gather → retire. When the loop retires it clears the degraded
    /// window and ends the provisional-diagnostics progress — i.e. progress
    /// ends exactly when full-quality diagnostics have published.
    async fn run_gather_loop(self, uri: Url) {
        loop {
            self.run_gather_once(&uri).await;
            if !self.gather_reg.finish(&uri) {
                break;
            }
        }
        self.clear_degraded(&uri).await;
    }

    /// Announce the degraded window: begin a work-done progress that says the
    /// gather is warming and diagnostics are provisional. Idempotent per
    /// window — the token is reserved once and reused across keystrokes (no
    /// spam), and released by `clear_degraded`/close. Capability-gated: a no-op
    /// when the client never advertised `window/workDoneProgress`.
    async fn begin_progress(&self, uri: &Url, language: &str) {
        if !self.work_done.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        static DEGRADED_TOKEN: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let token = NumberOrString::String(format!(
            "perl-lsp/degraded-{}",
            DEGRADED_TOKEN.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        // Reserve the slot atomically so two concurrent begins for the same
        // URI mint exactly one token.
        if !reserve_degraded_token(&self.degraded_progress, uri, token.clone()) {
            return; // this window already announced itself; reuse the token
        }
        let title = format!("{language} index warming — diagnostics are provisional");
        progress_create_and_begin(&self.client, &token, &title).await;
    }

    /// End the degraded window's progress if one is live (removes the token —
    /// bounded, one End per Begin).
    async fn end_progress(&self, uri: &Url) {
        if let Some((_, token)) = self.degraded_progress.remove(uri) {
            progress_end(&self.client, token).await;
        }
    }

    /// Clear the degraded-open mark, wake `await_open_full` waiters, and end
    /// the provisional-diagnostics progress. The window is over.
    async fn clear_degraded(&self, uri: &Url) {
        if let Some((_, n)) = self.degraded_open.remove(uri) {
            n.notify_waiters();
        }
        self.end_progress(uri).await;
    }

    /// One cross-file gather + full-quality re-analyze + re-publish for an open
    /// pack document. Cold gather allowed (this task has cached-only OFF).
    /// Does NOT clear the degraded window or spawn a successor — the enclosing
    /// `run_gather_loop` owns retirement. A stale-text result is dropped
    /// (no clobber); the loop's `finish` decides whether to re-run.
    async fn run_gather_once(&self, uri: &Url) {
        let Some((text, path, language)) = self
            .files
            .get_open(uri)
            .filter(|d| d.language != "perl")
            .map(|d| (d.text.clone(), d.path.clone(), d.language))
        else {
            return;
        };
        let snapshot = text.clone();
        // Full analyze on a blocking thread so the ~1.5 s gather never stalls
        // the executor.
        let analysis = tokio::task::spawn_blocking(move || {
            crate::language_driver::LanguageRegistry::with_enabled()
                .for_id(language)
                .map(|d| d.analyze_with_path(&text, path.as_deref()))
        })
        .await
        .ok()
        .flatten();
        let Some(analysis) = analysis else {
            return;
        };
        // A keystroke may have landed while we gathered; the debounced rebuild
        // owns the newer text, so don't clobber it with this stale build (the
        // loop re-runs against the latest text — the gather cache stays warm
        // for unchanged included files, so the re-run is cheap).
        if self
            .files
            .get_open(uri)
            .map(|d| d.text != snapshot)
            .unwrap_or(true)
        {
            return;
        }
        for imp in &analysis.imports {
            self.module_index.request_resolve(&imp.module_name);
        }
        for parents in analysis.package_parents.values() {
            for parent in parents {
                self.module_index.request_resolve(parent);
            }
        }
        if let Some(mut doc) = self.files.get_open_mut(uri) {
            doc.apply_rebuilt(analysis);
        }
        let diags = self
            .files
            .get_open(uri)
            .map(|doc| symbols::pack_diagnostics(&doc.analysis, self.options));
        if let Some(diags) = diags {
            self.client
                .publish_diagnostics(uri.clone(), diags, None)
                .await;
        }
    }
}

impl Backend {
    /// Build the shared context a background pack-gather heal runs with.
    fn pack_heal_ctx(&self) -> PackHealCtx {
        PackHealCtx {
            files: Arc::clone(&self.files),
            module_index: Arc::clone(&self.module_index),
            client: self.client.clone(),
            options: self.diagnostic_options(),
            degraded_open: Arc::clone(&self.degraded_open),
            degraded_progress: Arc::clone(&self.degraded_progress),
            gather_reg: Arc::clone(&self.gather_reg),
            work_done: Arc::clone(&self.work_done_progress),
        }
    }

    pub fn new(client: Client) -> Self {
        let files: Arc<FileStore> = Arc::new(FileStore::new());

        // We need Arc<ModuleIndex> so the refresh callback can access it.
        // Two-phase init: create ModuleIndex whose refresh callback references
        // a later-set Arc<ModuleIndex>, then wire up the Arc.
        let diag_options = Arc::new(std::sync::Mutex::new(symbols::DiagnosticOptions::default()));

        let refresh_client = client.clone();
        let refresh_files = Arc::clone(&files);
        let refresh_diag_options = Arc::clone(&diag_options);

        let module_index_holder: Arc<std::sync::OnceLock<Arc<ModuleIndex>>> =
            Arc::new(std::sync::OnceLock::new());
        let holder_clone = Arc::clone(&module_index_holder);

        // Coalesce generation for the per-module refresh storm: each resolved
        // module fires `on_refresh` (~33 in ~400ms opening a Perl file with a
        // dozen `use`s), each otherwise a full `for_each_open_mut` + publish —
        // CPU + stdout pressure that WIDENS the cold-open degraded window. Every
        // fire bumps this generation and debounces; only the latest surviving
        // fire republishes, so the burst collapses to ~one refresh. Lives only
        // in the closure — nothing outside bumps it.
        let refresh_gen_cb = Arc::new(std::sync::atomic::AtomicU64::new(0));

        // Capture the tokio handle so the callback can spawn async work
        // from the resolver thread (which has no tokio context).
        let tokio_handle = tokio::runtime::Handle::current();
        let on_refresh = move || {
            use std::sync::atomic::Ordering;
            let client = refresh_client.clone();
            let files = Arc::clone(&refresh_files);
            let holder = Arc::clone(&holder_clone);
            let diag_options = Arc::clone(&refresh_diag_options);
            let refresh_gen = Arc::clone(&refresh_gen_cb);
            // Debounce: bump the generation, then only the LATEST fire that
            // survives the settle window does the work. A tight resolver burst
            // (~45 modules in ~400ms) thus republishes once, not 45×.
            let my_gen = refresh_gen.fetch_add(1, Ordering::Relaxed) + 1;
            log::debug!("diag-refresh fired (gen {})", my_gen);
            tokio_handle.spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(120)).await;
                if refresh_gen.load(Ordering::Relaxed) != my_gen {
                    return; // a newer fire superseded this one
                }
                let module_index = match holder.get() {
                    Some(idx) => idx,
                    None => return,
                };
                log::debug!("diag-refresh executing (gen {})", my_gen);
                // Collect (uri, diagnostics) first without holding the store lock
                // across the await — publishing is async and could deadlock.
                let mut pending: Vec<(Url, Vec<Diagnostic>)> = Vec::new();
                let options = *diag_options.lock().unwrap();
                files.for_each_open_mut(|uri, doc| {
                    let diagnostics = if doc.language == "perl" {
                        std::sync::Arc::make_mut(&mut doc.analysis)
                            .enrich_imported_types_with_keys(Some(module_index.as_ref()));
                        symbols::collect_diagnostics(&doc.analysis, module_index, options)
                    } else {
                        symbols::pack_diagnostics(&doc.analysis, options)
                    };
                    pending.push((uri.clone(), diagnostics));
                });
                for (uri, diags) in pending {
                    client.publish_diagnostics(uri, diags, None).await;
                }
            });
        };

        let module_index = Arc::new(ModuleIndex::new(client.clone(), on_refresh));
        let _ = module_index_holder.set(Arc::clone(&module_index));

        Backend {
            module_index,
            client,
            files,
            change_gen: Arc::new(dashmap::DashMap::new()),
            perl_indexed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pack_indexed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            work_done_progress: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pack_change_lock: Arc::new(std::sync::Mutex::new(())),
            diag_options,
            rename_options: Arc::new(std::sync::Mutex::new(crate::resolve::RenameOptions::default())),
            index_ready: Arc::new(IndexReady::default()),
            cold_wait_ms: Arc::new(std::sync::atomic::AtomicU64::new(DEFAULT_COLD_WAIT_MS)),
            max_cache_mb: Arc::new(std::sync::atomic::AtomicU64::new(max_cache_mb_default())),
            opening: Arc::new(dashmap::DashMap::new()),
            degraded_open: Arc::new(dashmap::DashMap::new()),
            degraded_progress: Arc::new(dashmap::DashMap::new()),
            gather_reg: Arc::new(GatherRegistry::default()),
        }
    }

    /// After a debounce, rebuild the pack analysis for `uri` OFF the document
    /// lock (snapshot text → `spawn_blocking` build → write back) + publish
    /// diagnostics — but only while `generation` is still the latest edit, so
    /// a burst of keystrokes collapses to ONE rebuild after typing settles.
    fn spawn_debounced_rebuild(&self, uri: Url, generation: u64) {
        let files = Arc::clone(&self.files);
        let module_index = Arc::clone(&self.module_index);
        let client = self.client.clone();
        let change_gen = Arc::clone(&self.change_gen);
        let options = self.diagnostic_options();
        let degraded_open = Arc::clone(&self.degraded_open);
        let heal_ctx = self.pack_heal_ctx();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            let is_latest = || change_gen.get(&uri).map(|v| *v) == Some(generation);
            if !is_latest() {
                return;
            }
            // Snapshot the latest text off the lock; build on a blocking
            // thread so the ~0.7s analysis never stalls completion/hover.
            let Some((text, path, language)) = files
                .get_open(&uri)
                .map(|d| (d.text.clone(), d.path.clone(), d.language))
            else {
                return;
            };
            // A pack file's cross-file GATHER is cold on the first change after
            // a cold open (did_open's gather bails once the text changes, so it
            // can't warm us). Paying the ~24 s cold gather HERE would make the
            // first keystroke's diagnostics land 24 s late. Run CACHED-ONLY for
            // fast, degraded diagnostics — same as did_open — then heal via a
            // background gather refresh below. The flag is a thread-local no-op
            // for perl. See docs/open-forks.md.
            let analysis = tokio::task::spawn_blocking(move || {
                crate::cpp_reparse::set_gather_cached_only(true);
                let a = crate::language_driver::LanguageRegistry::with_enabled()
                    .for_id(language)
                    .map(|d| d.analyze_with_path(&text, path.as_deref()));
                crate::cpp_reparse::set_gather_cached_only(false);
                a
            })
            .await
            .ok()
            .flatten();
            let Some(analysis) = analysis else {
                return;
            };
            if !is_latest() {
                return; // a newer keystroke superseded this build
            }
            for imp in &analysis.imports {
                module_index.request_resolve(&imp.module_name);
            }
            for parents in analysis.package_parents.values() {
                for parent in parents {
                    module_index.request_resolve(parent);
                }
            }
            if let Some(mut doc) = files.get_open_mut(&uri) {
                doc.apply_rebuilt(analysis);
            }
            let diags = files
                .get_open(&uri)
                .map(|doc| symbols::pack_diagnostics(&doc.analysis, options));
            if let Some(diags) = diags {
                client.publish_diagnostics(uri.clone(), diags, None).await;
            }
            // Heal: warm the cross-file gather off this task and re-publish
            // full-quality diagnostics when it lands. The cached-only rebuild
            // just re-opened the degraded window for cross-file verbs; mark it
            // (so `await_open_full` holds Complete verbs until the heal lands),
            // announce it via progress (Part 1), then route the heal through
            // the single-flight registry (Part 2) so a typing burst coalesces
            // into ONE gather instead of abandoning one per keystroke. Perl has
            // no gather and is skipped.
            if language != "perl" {
                degraded_open
                    .entry(uri.clone())
                    .or_insert_with(|| Arc::new(tokio::sync::Notify::new()));
                heal_ctx.begin_progress(&uri, language).await;
                heal_ctx.request_gather(uri);
            }
        });
    }

    /// A pack file's bytes changed on disk (save or watcher event) — run the
    /// invalidation off the message loop: evict its per-file caches +
    /// every consumer's (reverse-closure), re-register the pack index, then
    /// refresh every OPEN pack document whose include closure contains the
    /// changed file (or that IS it), so in-session edits become visible
    /// without a restart.
    fn schedule_pack_invalidate(&self, path: PathBuf, deleted: bool) {
        let files = Arc::clone(&self.files);
        let module_index = Arc::clone(&self.module_index);
        let lock = Arc::clone(&self.pack_change_lock);
        let root = self.module_index.workspace_root();
        let heal_ctx = self.pack_heal_ctx();
        tokio::spawn(async move {
            let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
            {
                let module_index = Arc::clone(&module_index);
                let canon = canon.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let _g = lock.lock().unwrap_or_else(|e| e.into_inner());
                    crate::module_resolver::pack_file_changed(
                        root.as_deref(),
                        &module_index,
                        &canon,
                        deleted,
                    );
                })
                .await;
            }
            // Open consumers re-analyze AFTER the eviction so their gather
            // runs cold against the new header bytes.
            let canon_str = canon.to_string_lossy().into_owned();
            let mut to_refresh: Vec<Url> = Vec::new();
            files.for_each_analysis(|key, analysis| {
                if let FileKey::Url(u) = key {
                    let is_self = !deleted
                        && u.to_file_path()
                            .ok()
                            .map(|p| p.canonicalize().unwrap_or(p) == canon)
                            .unwrap_or(false);
                    if is_self || analysis.include_closure.contains(&canon_str) {
                        to_refresh.push(u);
                    }
                }
            });
            // Route through the single-flight registry (Part 2): a consumer
            // already mid-gather coalesces (re-runs once against the freshly
            // evicted caches) instead of double-gathering the same cone.
            for uri in to_refresh {
                heal_ctx.request_gather(uri);
            }
        });
    }

    /// A bare identifier at the cursor that names a CROSS-FILE top-level
    /// symbol — a macro (`OP_NULL`, `BASEOP`), enum constant, global, or
    /// type. Resolves off the RAW word, so it works even when the macro
    /// expanded AWAY in the analysis (the token isn't a captured ref).
    /// Returns (target uri, def span, the def's source line for hover).
    fn pack_xfile_word_at(
        &self,
        text: &str,
        doc_analysis: &crate::file_analysis::FileAnalysis,
        pos: Position,
        idx: &dyn crate::file_analysis::CrossFileLookup,
    ) -> Option<(Option<Url>, crate::file_analysis::Span, String)> {
        let word = symbols::word_at_point(text, symbols::position_to_point(pos))?;
        // Pick the best DEFINITION among same-named symbols (a `#define X` plus
        // its raw usages): prefer the `#define` line, else the earliest.
        let pick = |analysis: &crate::file_analysis::FileAnalysis, src: &str| {
            let lines: Vec<&str> = src.lines().collect();
            let line_of =
                |s: &crate::file_analysis::Symbol| lines.get(s.selection_span.start.row).copied();
            let cands: Vec<&crate::file_analysis::Symbol> =
                analysis.symbols.iter().filter(|s| s.name == word).collect();
            let sym = cands
                .iter()
                .find(|s| line_of(s).is_some_and(|l| l.trim_start().starts_with("#define")))
                .or_else(|| cands.iter().min_by_key(|s| s.selection_span.start.row))
                .copied()?;
            Some((sym.selection_span, line_of(sym).map(|l| l.trim().to_string()).unwrap_or_default()))
        };
        // A macro defined in THIS file (`BASEOP` in op.h) — the usage isn't a
        // captured ref, so find_definition missed it, but the def symbol is
        // local. Fall back to the cross-file index for usages from elsewhere.
        if let Some((span, line)) = pick(doc_analysis, text) {
            return Some((None, span, line));
        }
        let cached = idx.get_cached(word)?;
        let text = std::fs::read_to_string(&cached.path).ok()?;
        let (span, line) = pick(&idx.whole_present(&cached), &text)?;
        Some((Url::from_file_path(&cached.path).ok(), span, line))
    }

    /// The freshness engine's consumption half for OPEN docs: after an
    /// edit to `uri` rebuilt its analysis, record the new surface. An
    /// `Unchanged` verdict is the early-cutoff — a body edit refreshes
    /// nobody. `Changed` re-enriches + republishes exactly the OPEN docs
    /// in the transitive dirty closure (closed workspace consumers stay
    /// correct through the query-time walks; their always-enriched
    /// materialization is the next phase).
    /// Record the open doc's surface right after a rebuild — BEFORE
    /// `publish_diagnostics` enriches the analysis in place.
    /// `Surface::project`'s contract is the file's OWN facts: an enriched
    /// projection would fingerprint imported types into the record and
    /// flip-flop verdicts against the workspace indexer's pre-enrichment
    /// records (spurious Changed storms on body edits).
    /// Records through `record_and_dirty` — the shared record→verdict→dirty
    /// seam — so the open-doc editor path can't record a surface without the
    /// dirty consumer set. The caller acts on the returned set (republish).
    fn record_open_doc_surface(&self, uri: &Url) -> Option<crate::module_index::SurfaceDirty> {
        let path = uri.to_file_path().ok()?;
        let canon = std::fs::canonicalize(&path).unwrap_or(path);
        let doc = self.files.get_open(uri)?;
        if doc.language != "perl" {
            return None;
        }
        Some(self.module_index.record_and_dirty(
            &canon,
            &doc.analysis,
            crate::module_index::SurfaceWrite::OpenDoc,
        ))
    }

    /// Re-enrich + republish every OPEN doc in a dirty closure — the one
    /// speller of the membership rule (canonical-path match), shared by
    /// the in-editor verdict path and the watcher's aggregated closure.
    async fn republish_open_docs_in(
        &self,
        dirty: &std::collections::HashSet<std::path::PathBuf>,
    ) {
        if dirty.is_empty() {
            return;
        }
        let mut to_refresh: Vec<Url> = Vec::new();
        self.files.for_each_open(|u, _doc| {
            if let Ok(p) = u.to_file_path() {
                let c = std::fs::canonicalize(&p).unwrap_or(p);
                if dirty.contains(&c) {
                    to_refresh.push(u.clone());
                }
            }
        });
        for u in to_refresh {
            self.publish_diagnostics(&u).await;
        }
    }

    fn enrich_analysis(&self, uri: &Url) {
        if let Some(mut doc) = self.files.get_open_mut(uri) {
            // enrichment is Perl-flavored (imported-type/hash-key keys);
            // pack languages skip it.
            if doc.language == "perl" {
                std::sync::Arc::make_mut(&mut doc.analysis)
                    .enrich_imported_types_with_keys(Some(&*self.module_index));
            }
        }
    }

    async fn publish_diagnostics(&self, uri: &Url) {
        self.enrich_analysis(uri);
        let options = self.diagnostic_options();
        let diagnostics = match self.files.get_open(uri) {
            Some(doc) if doc.language == "perl" => {
                symbols::collect_diagnostics(&doc.analysis, &self.module_index, options)
            }
            // Pack languages stay honest-silent EXCEPT the always-on
            // member-access operator mismatch and the opt-in use-after-move
            // (gated by `DiagnosticOptions.use_after_move`).
            Some(doc) => symbols::pack_diagnostics(&doc.analysis, options),
            None => vec![],
        };
        self.client
            .publish_diagnostics(uri.clone(), diagnostics, None)
            .await;
    }
}

/// `(RefLocation, text)` pairs → one `WorkspaceEdit` (per-member texts).
fn edit_pairs_to_workspace_edit(
    edits: Vec<(crate::resolve::RefLocation, String)>,
) -> Option<WorkspaceEdit> {
    if edits.is_empty() {
        return None;
    }
    let mut all_changes: std::collections::HashMap<Url, Vec<TextEdit>> =
        std::collections::HashMap::new();
    for (loc, text) in edits {
        if let Some(uri) = loc.to_url() {
            all_changes.entry(uri).or_default().push(TextEdit {
                range: symbols::span_to_range(loc.span),
                new_text: text,
            });
        }
    }
    if all_changes.is_empty() {
        None
    } else {
        Some(WorkspaceEdit { changes: Some(all_changes), ..Default::default() })
    }
}


fn refs_to_locations(results: Vec<crate::resolve::RefLocation>) -> Option<Vec<Location>> {
    let mut locations: Vec<Location> = results
        .into_iter()
        .filter_map(|r| {
            let uri = r.to_url()?;
            Some(Location {
                uri,
                range: symbols::span_to_range(r.span),
            })
        })
        .collect();
    if locations.is_empty() {
        return None;
    }
    locations.sort_by(|a, b| {
        a.uri.as_str().cmp(b.uri.as_str())
            .then_with(|| a.range.start.line.cmp(&b.range.start.line))
            .then_with(|| a.range.start.character.cmp(&b.range.start.character))
    });
    locations.dedup_by(|a, b| a.uri == b.uri && a.range == b.range);
    Some(locations)
}

/// How often the parent-liveness monitor polls the client `processId`. ~10s is
/// the cadence vscode-languageserver-node / lsp4j / jdt.ls use — cheap enough to
/// run unconditionally, tight enough that a leaked server dies within a poll.
const PARENT_LIVENESS_POLL: std::time::Duration = std::time::Duration::from_secs(10);

/// Spawn a detached timer that self-exits when the LSP client (parent) process
/// dies. This is INDEPENDENT of the stdin read loop by design: the leak cases
/// are exactly when the read loop isn't running (server wedged mid-analysis, or
/// a hard SIGKILL of the editor that delivered no clean EOF). `None` disables
/// the check — per spec, a null `processId` means the client didn't fork us.
fn spawn_parent_liveness_monitor(process_id: Option<u32>) {
    let Some(pid) = process_id else { return };
    if pid == 0 {
        return;
    }
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(PARENT_LIVENESS_POLL).await;
            if !parent_process_alive(pid) {
                // Client gone; nothing to flush after the connection drops.
                // Exit hard so background `spawn_blocking` indexing (which parks
                // on `send_request` once the client vanishes) can't keep the
                // runtime — and a multi-GB workspace index — alive.
                std::process::exit(0);
            }
        }
    });
}

/// Linux liveness probe: `/proc/<pid>` vanishes once the process is reaped. No
/// new dependency, no signal side effects (unlike `kill(pid, 0)`).
#[cfg(target_os = "linux")]
fn parent_process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

/// Off Linux there's no cheap dependency-free probe, so assume alive — never
/// false-positive into an exit. The stdin-EOF path still covers clean shutdown.
#[cfg(not(target_os = "linux"))]
fn parent_process_alive(_pid: u32) -> bool {
    true
}

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        // Notify resolver thread of workspace root for per-project cache.
        let root = params
            .root_uri
            .as_ref()
            .map(|u| u.as_str())
            .or_else(|| {
                params
                    .workspace_folders
                    .as_ref()
                    .and_then(|f| f.first())
                    .map(|f| f.uri.as_str())
            });
        // Long-lived process: the overlay + rehydration LRU amortize here
        // (one-shot CLI modes leave both off — bisected at 2x warm-harness
        // wall). BEFORE set_workspace_root: the resolver wakes on the root
        // and reads the flag at warm time.
        self.module_index.mark_long_lived();
        self.module_index.set_workspace_root(root);
        // Same root drives repo-local `.perl-lsp/` plugin discovery, so the
        // plugin set and the per-project cache key can't disagree.
        crate::plugin::rhai_host::set_workspace_root(root);

        // LSP spec: `initialize` carries the client `processId`; "if the parent
        // process is not alive then the server should exit." Poll it on an
        // independent timer — the ROBUST backstop the stdin-EOF path can't be
        // (that's coupled to the read loop, which isn't running precisely when
        // the leak happens: a server wedged mid-analysis isn't reading stdin,
        // and a hard SIGKILL of the editor need not deliver a clean EOF).
        spawn_parent_liveness_monitor(params.process_id);

        // Server-initiated progress is capability-gated (M7): only send
        // `window/workDoneProgress/create` to clients that opted in.
        let wdp = params
            .capabilities
            .window
            .as_ref()
            .and_then(|w| w.work_done_progress)
            .unwrap_or(false);
        self.work_done_progress
            .store(wdp, std::sync::atomic::Ordering::Relaxed);

        // Opt-in diagnostics from `initializationOptions.diagnostics`.
        // The `diagnostics` sub-object deserializes straight into
        // `DiagnosticOptions` (the struct is the schema — camelCase keys,
        // absent ones default to false, e.g. `unresolvedDispatch`). A malformed
        // value leaves the defaults in place rather than failing initialize.
        if let Some(diag) = params
            .initialization_options
            .as_ref()
            .and_then(|o| o.get("diagnostics"))
        {
            if let Ok(parsed) =
                serde_json::from_value::<symbols::DiagnosticOptions>(diag.clone())
            {
                *self.diag_options.lock().unwrap() = parsed;
            }
        }
        // The `rename` sub-object deserializes into `RenameOptions` the same way
        // (`{ "rename": { "overrideScope": "dispatch" } }`); absent / malformed
        // leaves the default whole-hierarchy scope.
        if let Some(rename) = params
            .initialization_options
            .as_ref()
            .and_then(|o| o.get("rename"))
        {
            if let Ok(parsed) =
                serde_json::from_value::<crate::resolve::RenameOptions>(rename.clone())
            {
                *self.rename_options.lock().unwrap() = parsed;
            }
        }
        // `coldWaitMs` caps the cold-open pull-verb bounded wait; 0 opts out.
        // Absent / non-integer leaves the default.
        if let Some(ms) = params
            .initialization_options
            .as_ref()
            .and_then(|o| o.get("coldWaitMs"))
            .and_then(|v| v.as_u64())
        {
            self.cold_wait_ms
                .store(ms, std::sync::atomic::Ordering::Relaxed);
        }
        // `maxCacheMb` sizes the Slice-2 bag-rehydration LRU (0 = rehydrate and
        // drop). Absent / non-integer leaves the default.
        if let Some(mb) = params
            .initialization_options
            .as_ref()
            .and_then(|o| o.get("maxCacheMb"))
            .and_then(|v| v.as_u64())
        {
            self.max_cache_mb
                .store(mb, std::sync::atomic::Ordering::Relaxed);
        }

        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "perl-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                            include_text: Some(true),
                        })),
                        ..Default::default()
                    },
                )),
                document_symbol_provider: Some(OneOf::Left(true)),
                definition_provider: Some(OneOf::Left(true)),
                implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
                references_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
                document_range_formatting_provider: Some(OneOf::Left(true)),
                linked_editing_range_provider: Some(LinkedEditingRangeServerCapabilities::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    // Union of every served language's trigger chars — Perl
                    // sigils/`->`/`{`, plus a pack language's `.`/`::` etc.
                    // A perl-only build is byte-identical to the old list.
                    trigger_characters: Some(
                        crate::language_driver::LanguageRegistry::with_enabled().trigger_chars(),
                    ),
                    ..Default::default()
                }),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: Some(vec![")".to_string()]),
                    work_done_progress_options: Default::default(),
                }),
                document_highlight_provider: Some(OneOf::Left(true)),
                selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                inlay_hint_provider: Some(OneOf::Left(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: SemanticTokensLegend {
                                token_types: symbols::semantic_token_types(),
                                token_modifiers: symbols::semantic_token_modifiers(),
                            },
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                            ..Default::default()
                        },
                    ),
                ),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                workspace: Some(WorkspaceServerCapabilities {
                    workspace_folders: None,
                    file_operations: None,
                }),
                ..ServerCapabilities::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        // Pre-warm each pack language's skeleton-query compilation OFF the
        // message loop, FIRST — before any `.await` that depends on the client
        // answering (register_capability), so the warm-up starts even if the
        // client is slow to respond. `Query::new` is a ~180ms one-time cost
        // baked into the first pack file build; `did_open` runs that build
        // synchronously before its own first `.await`, so without this warm-up
        // it stalls the message loop and the goto-def request queued right
        // behind the open waits the whole ~180ms (measured: first cpp goto-def
        // 196ms, second 25ms, third 1ms). A tiny analyze forces the compile
        // into the driver's `OnceLock`; Perl's query warms on its normal first
        // build. Correctness-inert: it only populates the cache earlier.
        tokio::task::spawn_blocking(|| {
            let reg = crate::language_driver::LanguageRegistry::with_enabled();
            for id in reg.languages() {
                if id == "perl" {
                    continue;
                }
                if let Some(driver) = reg.for_id(id) {
                    // A non-trivial snippet so `parser.parse` yields a tree and
                    // the analyze reaches `query_extract::extract` (which is
                    // where `Query::new` fires); empty source can parse to
                    // `None` and skip it, leaving the cache cold.
                    let _ = driver.analyze_with_path("int _perl_lsp_warm;\n", None);
                }
            }
        });

        self.client
            .log_message(MessageType::INFO, "perl-lsp initialized")
            .await;

        // Register file watchers for workspace indexing — every served
        // language's extensions (Perl + pack languages), so out-of-editor
        // changes to a header/pack file reach the invalidation path too.
        let watchers: Vec<FileSystemWatcher> = {
            let reg = crate::language_driver::LanguageRegistry::with_enabled();
            reg.languages()
                .into_iter()
                .filter_map(|id| reg.for_id(id))
                .flat_map(|d| d.extensions().iter())
                .map(|ext| FileSystemWatcher {
                    glob_pattern: GlobPattern::String(format!("**/*.{ext}")),
                    kind: None,
                })
                .collect()
        };
        let registrations = vec![Registration {
            id: "perl-file-watcher".to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
            register_options: Some(serde_json::to_value(DidChangeWatchedFilesRegistrationOptions {
                watchers,
            }).unwrap()),
        }];
        let _ = self.client.register_capability(registrations).await;

        // Workspace indexing is LAZY + per-language — the first `did_open` of a
        // family triggers `ensure_workspace_indexed`, so a C++ session in a
        // mixed tree never eagerly scans the 4000+ `.pm` files it can't use
        // (that eager perl scan was the multi-minute first-open stall).
    }

    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        // Build the document OFF the message loop. `FileStore::open` runs the
        // whole pack pipeline (macro transform + extraction) synchronously — for
        // a 16k-line macro-heavy C file that is ~1.3 s even cached-only, and
        // running it here would head-of-line block every request the client
        // fires on open. cached-only skips the cross-file GATHER (a further
        // ~1.5 s, warmed later by the single-flight gather heal); the per-file
        // build is intrinsic and must simply not block the loop.
        //
        // A per-URI `Notify` marks the build in flight so read verbs bounded-wait
        // for it (`await_open_ready`) instead of racing the empty store. The
        // `set_gather_cached_only` thread-local is set INSIDE the blocking closure
        // so it applies exactly to this build's thread.
        let notify = Arc::new(tokio::sync::Notify::new());
        self.opening.insert(uri.clone(), Arc::clone(&notify));
        let files = Arc::clone(&self.files);
        let uri_build = uri.clone();
        let build_started = std::time::Instant::now();
        let opened = tokio::task::spawn_blocking(move || {
            crate::cpp_reparse::set_gather_cached_only(true);
            let opened = files.open(uri_build, text);
            crate::cpp_reparse::set_gather_cached_only(false);
            opened
        })
        .await
        .unwrap_or(false);
        // Doc is in the store (or the build failed): drop the in-flight marker
        // and wake any verb waiting on it.
        self.opening.remove(&uri);
        notify.notify_waiters();
        // If the build outran the bounded wait, the verbs the client fired on
        // open (semanticTokens, inlayHint) returned degraded. Their content is
        // now in the store — nudge the client to re-request (LSP server-initiated
        // refresh) so the visible highlighting/hints heal without a keystroke.
        // A fast build (< cap) answered those on the first pull; no nudge needed.
        let cap = self.cold_wait_ms.load(std::sync::atomic::Ordering::Relaxed);
        if opened && cap > 0 && build_started.elapsed().as_millis() as u64 > cap {
            let client = self.client.clone();
            tokio::spawn(async move {
                let _ = client.semantic_tokens_refresh().await;
                let _ = client.inlay_hint_refresh().await;
            });
        }
        let mut needs_gather_refresh = false;
        if opened {
            if let Some(doc) = self.files.get_open(&uri) {
                // Lazily index this file's language family (once) so a C++
                // open doesn't wait on the perl tree.
                self.ensure_workspace_indexed(&doc.language);
                // A pack file's first analyze was cached-only; warm the gather
                // and re-analyze in the background so full cross-file macros land.
                needs_gather_refresh = doc.language != "perl";
                // Enqueue imports for background resolution (non-blocking).
                for imp in &doc.analysis.imports {
                    self.module_index.request_resolve(&imp.module_name);
                }
                // Enqueue parent classes for resolution (inheritance chain).
                for parents in doc.analysis.package_parents.values() {
                    for parent in parents {
                        self.module_index.request_resolve(parent);
                    }
                }
            }
        }
        // The open-doc path now owns this file's surface record (buffer
        // shadows disk for every cross-file consumer — `SurfaceWrite`).
        // Recording here also catches an open-after-external-change: the
        // buffer's surface vs the indexer's record → Changed → refresh.
        let mut opened_dirty = None;
        if opened {
            if let (Ok(path), Some(doc)) = (uri.to_file_path(), self.files.get_open(&uri)) {
                if doc.language == "perl" {
                    self.module_index.mark_doc_open(&path);
                    opened_dirty = self.record_open_doc_surface(&uri);
                }
            }
        }
        self.publish_diagnostics(&uri).await;
        if let Some(sd) = opened_dirty {
            self.republish_open_docs_in(&sd.dirty).await;
        }
        if needs_gather_refresh {
            // The open build was cached-only: mark the degraded window BEFORE
            // spawning the heal, so a cross-file verb racing this open waits
            // for the full-gather analysis instead of the partial closure.
            self.degraded_open
                .entry(uri.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Notify::new()));
            // Announce the degraded window (Part 1) and route the initial
            // gather through the single-flight registry (Part 2) — so the
            // first change's heal coalesces into THIS gather instead of
            // spawning a redundant second one.
            let heal_ctx = self.pack_heal_ctx();
            let language = self.files.get_open(&uri).map(|d| d.language);
            if let Some(language) = language {
                heal_ctx.begin_progress(&uri, language).await;
            }
            heal_ctx.request_gather(uri);
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let Some(change) = params.content_changes.into_iter().next() else {
            return;
        };
        let language = match self.files.get_open(&uri) {
            Some(doc) => doc.language,
            None => return,
        };
        // Perl rebuilds synchronously — its build is cheap. Pack languages
        // (macro-heavy C: ~0.7s/rebuild) update the tree/text immediately so
        // position features stay live, and DEBOUNCE the analysis so a burst
        // of keystrokes pays one rebuild after typing settles, not one each.
        if language == "perl" {
            if let Some(mut doc) = self.files.get_open_mut(&uri) {
                doc.update(change.text);
                for imp in &doc.analysis.imports {
                    self.module_index.request_resolve(&imp.module_name);
                }
                for parents in doc.analysis.package_parents.values() {
                    for parent in parents {
                        self.module_index.request_resolve(parent);
                    }
                }
            }
            // Pre-enrichment record — publish_diagnostics enriches in place.
            let recorded = self.record_open_doc_surface(&uri);
            self.publish_diagnostics(&uri).await;
            // Surface-gated consumer refresh: a body edit stops here
            // (Unchanged → empty dirty set); a contract change republishes
            // the open docs that can see it.
            if let Some(sd) = recorded {
                self.republish_open_docs_in(&sd.dirty).await;
            }
            return;
        }
        if let Some(mut doc) = self.files.get_open_mut(&uri) {
            doc.update_text_only(change.text);
        }
        let generation = {
            let mut e = self.change_gen.entry(uri.clone()).or_insert(0);
            *e += 1;
            *e
        };
        self.spawn_debounced_rebuild(uri, generation);
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        let pack_path = self
            .files
            .get_open(&uri)
            .filter(|doc| doc.language != "perl")
            .and_then(|_| uri.to_file_path().ok());
        if let Some(text) = params.text {
            if let Some(mut doc) = self.files.get_open_mut(&uri) {
                doc.update(text);
            }
            let recorded = self.record_open_doc_surface(&uri);
            self.publish_diagnostics(&uri).await;
            if let Some(sd) = recorded {
                self.republish_open_docs_in(&sd.dirty).await;
            }
        }
        // The saved bytes are on disk: re-register this file's indexed copy,
        // evict the macro/closure caches it participates in, and refresh its
        // open consumers (H1 — a saved header must become visible to its
        // includers without a restart). Runs regardless of includeText.
        if let Some(path) = pack_path {
            self.schedule_pack_invalidate(path, false);
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.files.close(&uri);
        // Wake any degraded-window waiter — the doc is gone, there is
        // nothing to wait for.
        if let Some((_, n)) = self.degraded_open.remove(&uri) {
            n.notify_waiters();
        }
        // Retire any in-flight gather single-flight entry (no leak on close;
        // the running loop's next `finish` sees Vacant and stops) and end the
        // degraded-window progress if one is still live.
        self.gather_reg.forget(&uri);
        if let Some((_, token)) = self.degraded_progress.remove(&uri) {
            progress_end(&self.client, token).await;
        }
        // Release the surface record to background writers and reconcile:
        // consumers flip back to the indexed DISK copy — if the buffer died
        // with unsaved contract changes, whoever enriched against it is
        // stale and gets republished here.
        if let Ok(path) = uri.to_file_path() {
            if let Some(sd) = self.module_index.mark_doc_closed(&path) {
                self.republish_open_docs_in(&sd.dirty).await;
            }
        }
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        self.await_open_ready(uri, WaitPolicy::Complete).await;
        let doc = match self.files.get_open(uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        let syms = symbols::extract_symbols(&doc.analysis);
        Ok(Some(DocumentSymbolResponse::Nested(syms)))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        // Cold-open bounded waits: first the file's own initial build (may still
        // be in flight — `did_open` runs it off the loop), then its family index
        // — so the query resolves warm instead of returning the one degraded
        // answer the user never re-triggers. Guards dropped before each await;
        // analysis snapshotted AFTER so any heal is picked up.
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        if let Some(language) = self.files.get_open(uri).map(|d| d.language) {
            self.await_index_ready(language, WaitPolicy::Interactive).await;
        }
        // Snapshot + drop the store guard before `resolve()` (reentrant
        // `for_each_open`); see `Document::analysis`.
        let (analysis, text, language) = match self.files.get_open(uri) {
            Some(doc) => (Arc::clone(&doc.analysis), doc.text.clone(), doc.language),
            None => return Ok(None),
        };
        // cpp/pack functions live in the per-language sub-index; route there
        // so cross-file function goto-def resolves (Perl uses the hub).
        let pack = (language != "perl")
            .then(|| self.module_index.pack_index(language))
            .flatten();
        let base_idx: &dyn crate::file_analysis::CrossFileLookup = match pack.as_deref() {
            Some(i) => i,
            None => &*self.module_index,
        };
        // The raw-word lanes below (macro variants, cross-file word fallback)
        // sit outside the CandidateSet and still need this file's closure
        // scope; the set scopes itself at construction.
        let self_path = uri.to_file_path().ok();
        let scoped = crate::file_analysis::ScopedLookup::new(
            base_idx, &analysis.include_closure, self_path.as_deref());
        let idx: &dyn crate::file_analysis::CrossFileLookup = &scoped;
        // `#include "x.h"` path → the resolved header (`#include` = `use`).
        // A path token, not a name — slot-shaped, so it stays ahead of the
        // set (the ADR's honest boundary). The pack declares whether it has
        // include tokens; asked, never named.
        if language_has_include_tokens(&language) {
            if let Some(loc) = symbols::pack_include_definition(
                &analysis, symbols::position_to_point(pos), self_path.as_deref())
            {
                return Ok(Some(GotoDefinitionResponse::Scalar(loc)));
            }
        }
        // Forward projection of the set. The source text unlocks the macro
        // variant lane (ranked, never pruned, see-through delegate) for pack
        // routing; labels ride the candidates and the editor adapter drops
        // them (ordering conveys rank).
        let mut cs = crate::resolve::resolve(
            &self.files,
            &analysis,
            FileKey::Url(uri.clone()),
            symbols::position_to_point(pos),
            Some(base_idx),
            crate::resolve::OverrideScope::default(),
        )
        .with_source(&text);
        if pack.is_some() {
            cs = cs.pack_routed();
        }
        let locs: Vec<Location> = cs
            .definitions()
            .into_iter()
            .filter_map(|l| {
                let uri = l.to_url()?;
                Some(Location { uri, range: symbols::span_to_range(l.span) })
            })
            .collect();
        match locs.len() {
            0 => {}
            1 => return Ok(Some(GotoDefinitionResponse::Scalar(locs.into_iter().next().unwrap()))),
            _ => return Ok(Some(GotoDefinitionResponse::Array(locs))),
        }
        // Member access (`obj->field`) now flows through `find_definition`
        // above: cpp mints a `MethodCall` ref core resolves like any other.
        if language != "perl" {
            // A macro / enum-constant / global usage (`OP_NULL`, `BASEOP`) —
            // the raw word names a local-or-cross-file symbol.
            if let Some((target, span, _)) = self.pack_xfile_word_at(&text, &analysis, pos, idx) {
                return Ok(Some(GotoDefinitionResponse::Scalar(Location {
                    uri: target.unwrap_or_else(|| uri.clone()),
                    range: symbols::span_to_range(span),
                })));
            }
        }
        Ok(None)
    }

    async fn goto_implementation(
        &self,
        params: request::GotoImplementationParams,
    ) -> Result<Option<request::GotoImplementationResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        // Cold-open bounded waits (see `await_open_ready` / `await_index_ready`):
        // the file's own initial build, then an in-flight family index.
        self.await_open_ready(uri, WaitPolicy::Complete).await;
        if let Some(language) = self.files.get_open(uri).map(|d| d.language) {
            self.await_index_ready(language, WaitPolicy::Complete).await;
        }
        self.await_open_full(uri, WaitPolicy::Complete).await;
        // Snapshot the open doc (cheap `Arc` clone) and DROP the store guard
        // before `resolve()` — it re-locks the open shards via `for_each_open`,
        // and holding the guard across that reentrant read deadlocks against a
        // concurrent `for_each_open_mut` writer. See `Document::analysis`.
        let (analysis, language) = match self.files.get_open(uri) {
            Some(doc) => (Arc::clone(&doc.analysis), doc.language),
            None => return Ok(None),
        };

        // The family/descendants/domain projection of the same set references
        // and rename resolve from — pack routing declared at construction so
        // the resolved target can't diverge across the three verbs.
        let pack = (language != "perl")
            .then(|| self.module_index.pack_index(language))
            .flatten();
        let base_idx: &dyn crate::file_analysis::CrossFileLookup = match pack.as_deref() {
            Some(i) => i,
            None => &*self.module_index,
        };
        let mut cs = crate::resolve::resolve(
            &self.files,
            &analysis,
            FileKey::Url(uri.clone()),
            symbols::position_to_point(pos),
            Some(base_idx),
            crate::resolve::OverrideScope::default(),
        );
        if pack.is_some() {
            cs = cs.pack_routed();
        }
        Ok(refs_to_locations(cs.implementations()).map(GotoDefinitionResponse::Array))
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        // Cold-open bounded waits (see `await_open_ready` / `await_index_ready`):
        // the file's own initial build, then an in-flight family index so
        // cross-file references resolve warm (the in-window `op_free` 1 → 118
        // heal) instead of returning def-only.
        self.await_open_ready(uri, WaitPolicy::Complete).await;
        if let Some(language) = self.files.get_open(uri).map(|d| d.language) {
            self.await_index_ready(language, WaitPolicy::Complete).await;
        }
        self.await_open_full(uri, WaitPolicy::Complete).await;
        // Snapshot + drop the store guard before `resolve()` (reentrant
        // `for_each_open`); see `Document::analysis`.
        let (analysis, language) = match self.files.get_open(uri) {
            Some(doc) => (Arc::clone(&doc.analysis), doc.language),
            None => return Ok(None),
        };

        let point = symbols::position_to_point(pos);
        // Pack languages resolve + collect through their sub-index (mirrors
        // goto-def and the CLI) — the hub only knows Perl modules, so a cpp
        // query against it silently misses every cross-file use.
        let pack = (language != "perl")
            .then(|| self.module_index.pack_index(language))
            .flatten();
        let base_idx: &dyn crate::file_analysis::CrossFileLookup = match pack.as_deref() {
            Some(i) => i,
            None => &*self.module_index,
        };
        let self_path = uri.to_file_path().ok();
        // `#include` reverse — "who includes this header" — owns the path
        // token exclusively (the backward mirror of include goto-def). The
        // pack declares whether it has include tokens; asked, never named.
        if language_has_include_tokens(&language) {
            if let Some(incs) = symbols::pack_include_references(
                &analysis, point, self_path.as_deref(), base_idx)
            {
                let locs: Vec<Location> = incs
                    .into_iter()
                    .filter_map(|(path, span)| {
                        Some(Location {
                            uri: Url::from_file_path(&path).ok()?,
                            range: symbols::span_to_range(span),
                        })
                    })
                    .collect();
                return Ok((!locs.is_empty()).then_some(locs));
            }
        }
        // (The reverse domain bridge — enum type → field-slot sites — is a
        // goto-implementation projection, NOT part of plain references.)
        // One construction, one projection — target/group/lexical branching,
        // visibility (incl. the origin's include-closure scope and the pack
        // VISIBLE widening), and the cross-file walk all live inside the set.
        // The backward walk does real I/O now (relational retrieval +
        // candidate-blob rehydration) — run construction + projection on the
        // blocking pool, never the reactor. Everything moved is Arc'd.
        let files = Arc::clone(&self.files);
        let module_index = Arc::clone(&self.module_index);
        let uri = uri.clone();
        let scope = self.override_scope();
        let locs = tokio::task::spawn_blocking(move || {
            let base_idx: &dyn crate::file_analysis::CrossFileLookup = match pack.as_deref() {
                Some(i) => i,
                None => &*module_index,
            };
            let mut cs = crate::resolve::resolve(
                &files,
                &analysis,
                FileKey::Url(uri),
                point,
                Some(base_idx),
                scope,
            );
            if pack.is_some() {
                cs = cs.pack_routed();
            }
            refs_to_locations(cs.references())
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?;
        Ok(locs)
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        self.await_open_ready(&params.text_document.uri, WaitPolicy::Interactive).await;
        // Snapshot + drop the store guard before `resolve()` (reentrant
        // `for_each_open`); see `Document::analysis`.
        let (analysis, language) = match self.files.get_open(&params.text_document.uri) {
            Some(doc) => (Arc::clone(&doc.analysis), doc.language),
            None => return Ok(None),
        };
        let point = symbols::position_to_point(params.position);
        // Same pack routing as the rename handler, so this gate probes the
        // target rename would actually act on.
        let pack = (language != "perl")
            .then(|| self.module_index.pack_index(language))
            .flatten();
        let base_idx: &dyn crate::file_analysis::CrossFileLookup = match pack.as_deref() {
            Some(i) => i,
            None => &*self.module_index,
        };
        // The rename box's range + placeholder.
        let box_at = analysis
            .symbol_at(point)
            .map(|sym| (sym.selection_span, sym.name.clone()))
            .or_else(|| analysis.ref_at(point).map(|r| (r.span, r.target_name.clone())));
        // Only offer a rename box where `rename` would actually produce edits.
        // Accepting on any `symbol_at`/`ref_at` hit is a UX trap: positions like
        // `@_` or an ownerless constructor key resolve to nothing renameable, so
        // the user gets a box that silently no-ops. `renameable()` mirrors
        // `rename_edits`' arms on the same set (incl. the pack probe: a rename
        // the set would refuse or no-op on offers no box), so this gate tracks
        // new renameable kinds automatically, with no change here.
        let mut cs = crate::resolve::resolve(
            &self.files,
            &analysis,
            FileKey::Url(params.text_document.uri.clone()),
            point,
            Some(base_idx),
            self.override_scope(),
        );
        if pack.is_some() {
            cs = cs.pack_routed();
        }
        let renameable = cs.renameable();
        if !renameable {
            return Ok(None);
        }
        Ok(box_at.map(|(span, placeholder)| PrepareRenameResponse::RangeWithPlaceholder {
            range: symbols::span_to_range(span),
            placeholder,
        }))
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let new_name = &params.new_name;
        if !crate::resolve::is_valid_rename_name(new_name) {
            return Err(tower_lsp::jsonrpc::Error::invalid_params(
                "rename: the new name must not be empty or whitespace",
            ));
        }
        self.await_open_ready(uri, WaitPolicy::Complete).await;
        // Cross-file rename edits are act-on-able: a cold-index rename that
        // silently missed files would corrupt the workspace. Wait Complete.
        if let Some(language) = self.files.get_open(uri).map(|d| d.language) {
            self.await_index_ready(language, WaitPolicy::Complete).await;
        }
        self.await_open_full(uri, WaitPolicy::Complete).await;
        // Snapshot + drop the store guard before `resolve()` (reentrant
        // `for_each_open`); see `Document::analysis`.
        let (analysis, language) = match self.files.get_open(uri) {
            Some(doc) => (Arc::clone(&doc.analysis), doc.language),
            None => return Ok(None),
        };

        let point = symbols::position_to_point(pos);
        // Rename is the references image + policy, projected from the same
        // set: cross-file walk for workspace-stable targets, per-member texts
        // for groups, the origin file's rename machinery for lexicals. The
        // pack routing fact is declared at construction; the set widens the
        // walk to the per-language cache and REFUSES on alias-spelled sites
        // instead of emitting a partial edit.
        let pack = (language != "perl")
            .then(|| self.module_index.pack_index(language))
            .flatten();
        let _base_idx: &dyn crate::file_analysis::CrossFileLookup = match pack.as_deref() {
            Some(i) => i,
            None => &*self.module_index,
        };
        // Same blocking-pool routing as `references`: rename projects the
        // references image, which now reads SQLite + rehydrates blobs.
        let files = Arc::clone(&self.files);
        let module_index = Arc::clone(&self.module_index);
        let uri = uri.clone();
        let new_name = new_name.clone();
        let scope = self.override_scope();
        tokio::task::spawn_blocking(move || {
            let base_idx: &dyn crate::file_analysis::CrossFileLookup = match pack.as_deref() {
                Some(i) => i,
                None => &*module_index,
            };
            let mut cs = crate::resolve::resolve(
                &files,
                &analysis,
                FileKey::Url(uri),
                point,
                Some(base_idx),
                scope,
            );
            if pack.is_some() {
                cs = cs.pack_routed();
            }
            cs.rename_edits(&new_name)
                .map(edit_pairs_to_workspace_edit)
                .map_err(tower_lsp::jsonrpc::Error::invalid_params)
        })
        .await
        .map_err(|_| tower_lsp::jsonrpc::Error::internal_error())?
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        // Cold-open bounded waits (see `await_open_ready` / `await_index_ready`):
        // the file's own initial build, then an in-flight family index so hover
        // resolves warm.
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        if let Some(language) = self.files.get_open(uri).map(|d| d.language) {
            self.await_index_ready(language, WaitPolicy::Interactive).await;
        }
        // Snapshot + drop the store guard before `resolve()` (reentrant
        // `for_each_open`); see `Document::analysis`.
        let (analysis, text, language) = match self.files.get_open(uri) {
            Some(doc) => (Arc::clone(&doc.analysis), doc.text.clone(), doc.language),
            None => return Ok(None),
        };
        // Perl's hover renderer is Perl-specific; pack languages present the
        // CandidateSet's hover projection (the top-ranked candidate goto-def
        // would jump to) — constructed exactly like the goto-def handler's
        // set, so the two verbs can't disagree at a position.
        if language != "perl" {
            let pack = self.module_index.pack_index(language);
            let base_idx: &dyn crate::file_analysis::CrossFileLookup =
                pack.as_deref().map_or(&*self.module_index, |i| i);
            let mut cs = crate::resolve::resolve(
                &self.files,
                &analysis,
                FileKey::Url(uri.clone()),
                symbols::position_to_point(pos),
                Some(base_idx),
                crate::resolve::OverrideScope::default(),
            )
            .with_source(&text);
            if pack.is_some() {
                cs = cs.pack_routed();
            }
            if let Some(h) = symbols::pack_hover(&cs, language) {
                return Ok(Some(h));
            }
            // The raw-word fallback outside the set (mirrors goto-def's): a
            // macro / enum-constant / global whose token no ref captures —
            // show its definition line.
            let self_path = uri.to_file_path().ok();
            let scoped = crate::file_analysis::ScopedLookup::new(
                base_idx, &analysis.include_closure, self_path.as_deref());
            let xidx: &dyn crate::file_analysis::CrossFileLookup = &scoped;
            if let Some((_, _, line)) = self.pack_xfile_word_at(&text, &analysis, pos, xidx) {
                if !line.is_empty() {
                    return Ok(Some(Hover {
                        contents: HoverContents::Markup(MarkupContent {
                            kind: MarkupKind::Markdown,
                            value: format!("```{}\n{}\n```", language, line),
                        }),
                        range: None,
                    }));
                }
            }
            return Ok(None);
        }
        Ok(symbols::hover_info(
            &analysis,
            &text,
            pos,
            &self.module_index,
        ))
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        // Snapshot + drop the store guard before completion resolves (both the
        // pack and Perl paths gather cross-file candidates through `resolve()`,
        // which re-locks the open shards via `for_each_open`); see
        // `Document::analysis`. `tree` clones O(1) (tree-sitter refcount).
        let (analysis, text, tree, language, path, package_lines) =
            match self.files.get_open(uri) {
                Some(doc) => (
                    Arc::clone(&doc.analysis),
                    doc.text.clone(),
                    doc.tree.clone(),
                    doc.language,
                    doc.path.clone(),
                    doc.stable_outline.package_lines().to_vec(),
                ),
                None => return Ok(None),
            };
        if language != "perl" {
            let (items, is_incomplete) = pack_completion(
                &self.files,
                &analysis,
                &text,
                &tree,
                symbols::position_to_point(pos),
                language,
                path.as_deref(),
                &self.module_index,
            );
            if items.is_empty() && !is_incomplete {
                return Ok(None);
            }
            // Prefix-gated cross-file gathering (macros, include-closure
            // symbols) filters server-side, so the client must re-request
            // as the typed prefix changes rather than reuse a cached list.
            return Ok(Some(if is_incomplete {
                CompletionResponse::List(CompletionList { is_incomplete: true, items })
            } else {
                CompletionResponse::Array(items)
            }));
        }
        let items = symbols::completion_items(
            &self.files,
            &FileKey::Url(uri.clone()),
            &analysis,
            &tree,
            &text,
            pos,
            &self.module_index,
            Some(&package_lines),
        );
        if items.is_empty() {
            Ok(None)
        } else {
            // If any item is a loading placeholder (empty insert_text), mark as incomplete
            // so the editor re-requests on next keystroke after the module resolves.
            let is_incomplete = items.iter().any(|i| i.insert_text.as_deref() == Some(""));
            if is_incomplete {
                // Trigger resolution for the module being loaded
                for i in &items {
                    if i.insert_text.as_deref() == Some("") {
                        if let Some(ref label) = Some(&i.label) {
                            if let Some(name) = label.strip_prefix("loading ").and_then(|s| s.strip_suffix("...")) {
                                self.module_index.request_resolve(name);
                            }
                        }
                    }
                }
                Ok(Some(CompletionResponse::List(CompletionList {
                    is_incomplete: true,
                    items,
                })))
            } else {
                Ok(Some(CompletionResponse::Array(items)))
            }
        }
    }

    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> Result<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        let doc = match self.files.get_open(uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        if doc.language != "perl" {
            return Ok(None); // Perl cursor-context handler
        }
        Ok(symbols::signature_help(&doc.analysis, &doc.tree, &doc.text, pos, &self.module_index))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        let doc = match self.files.get_open(uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        let highlights = symbols::document_highlights(&doc.analysis, pos, Some(&*self.module_index));
        if highlights.is_empty() {
            Ok(None)
        } else {
            Ok(Some(highlights))
        }
    }

    async fn selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> Result<Option<Vec<SelectionRange>>> {
        let uri = &params.text_document.uri;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        let doc = match self.files.get_open(uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        if doc.language != "perl" {
            return Ok(None); // tree-shape handler, Perl-tuned for v1
        }
        let ranges: Vec<SelectionRange> = params
            .positions
            .iter()
            .map(|pos| symbols::selection_ranges(&doc.tree, *pos))
            .collect();
        Ok(Some(ranges))
    }

    async fn folding_range(
        &self,
        params: FoldingRangeParams,
    ) -> Result<Option<Vec<FoldingRange>>> {
        let uri = &params.text_document.uri;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        let doc = match self.files.get_open(uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        let ranges = symbols::folding_ranges(&doc.analysis);
        if ranges.is_empty() {
            Ok(None)
        } else {
            Ok(Some(ranges))
        }
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        // Copy the source out and release the DashMap guard before awaiting
        // perltidy: holding a shard read lock across the await deadlocks any
        // concurrent didChange (which needs the write lock) on the same file.
        let source = match self.files.get_open(uri) {
            Some(doc) => doc.text.clone(),
            None => return Ok(None),
        };

        // Shell out to perltidy
        let output = match run_perltidy(source.clone()).await {
            Ok(o) => o,
            Err(e) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("Failed to run perltidy: {}", e),
                    )
                    .await;
                return Ok(None);
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!("perltidy exited with error: {}", stderr),
                )
                .await;
            return Ok(None);
        }

        let formatted = String::from_utf8_lossy(&output.stdout).to_string();
        if formatted == source {
            return Ok(None);
        }

        // Replace entire document
        let line_count = source.lines().count();
        Ok(Some(vec![TextEdit {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: line_count as u32,
                    character: 0,
                },
            },
            new_text: formatted,
        }]))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        let doc = match self.files.get_open(uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        let actions = symbols::code_actions(&params.context.diagnostics, &doc.analysis, uri);
        if actions.is_empty() {
            Ok(None)
        } else {
            Ok(Some(actions))
        }
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = &params.text_document.uri;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        let doc = match self.files.get_open(uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        let tokens = symbols::semantic_tokens(&doc.analysis);
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: tokens,
        })))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        let doc = match self.files.get_open(uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };
        let hints = symbols::inlay_hints(&doc.analysis, params.range);
        if hints.is_empty() {
            Ok(None)
        } else {
            Ok(Some(hints))
        }
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let query = params.query.to_lowercase();
        let mut results = Vec::new();
        // Paths a symbols-present resident copy already answered — the rows
        // pass skips these (open docs and un-evicted copies are fresher than
        // their persisted rows; evicted copies are rows-guaranteed).
        let mut covered: std::collections::HashSet<std::path::PathBuf> =
            std::collections::HashSet::new();

        self.files.for_each_analysis(|key, analysis| {
            let uri = match key {
                FileKey::Url(u) => u,
                FileKey::Path(p) => Url::from_file_path(&p).unwrap_or_else(|_| {
                    Url::parse(&format!("file://{}", p.display()))
                        .unwrap()
                }),
            };
            if !analysis.symbols_are_evicted() {
                if let Ok(p) = uri.to_file_path() {
                    // Claim the canonical spelling too: rows are keyed
                    // canonical, and an open doc reached through a symlinked
                    // root must shadow its own persisted rows.
                    if let Ok(canon) = std::fs::canonicalize(&p) {
                        covered.insert(canon);
                    }
                    covered.insert(p);
                }
            }
            for sym in &analysis.symbols {
                if sym.name.to_lowercase().contains(&query) {
                    if let Some(info) = symbols::symbol_to_workspace_info(sym, uri.clone()) {
                        results.push(info);
                    }
                }
            }
            // Plugin namespaces — match on both id and kind so users
            // can find "the minion tasks in this workspace" via either
            // "minion" or "tasks".
            for ns in &analysis.plugin_namespaces {
                let hay = format!("{} {}", ns.id.to_lowercase(), ns.kind.to_lowercase());
                if hay.contains(&query) {
                    results.push(symbols::plugin_namespace_to_workspace_info(ns, uri.clone()));
                }
            }
        });

        // Pack-language (C/C++/…) symbols live in per-language sub-indexes, not
        // the FileStore — sweep them so a C typedef/class/free function shows in
        // workspace search alongside Perl packages.
        self.module_index.for_each_pack_registered_file(&mut |path, analysis| {
            if !analysis.symbols_are_evicted() {
                covered.insert(path.to_path_buf());
            }
            let uri = Url::from_file_path(path).unwrap_or_else(|_| {
                Url::parse(&format!("file://{}", path.display())).unwrap()
            });
            for sym in &analysis.symbols {
                if sym.name.to_lowercase().contains(&query) {
                    if let Some(info) = symbols::symbol_to_workspace_info(sym, uri.clone()) {
                        results.push(info);
                    }
                }
            }
        });

        // Rows pass: symbol-evicted copies (Perl workspace + @INC + every
        // pack tier) answer from the relational store — the resident sweep
        // above saw empty vecs for them. Same containment test, same
        // kind/visibility filters as `symbol_to_workspace_info`.
        for hit in symbols::sym_row_search(&self.module_index, &query) {
            let path = std::path::PathBuf::from(&hit.path);
            if covered.contains(&path) {
                continue;
            }
            if let Some(info) = symbols::sym_row_to_workspace_info(&hit) {
                results.push(info);
            }
        }

        symbols::dedup_workspace_symbols(&mut results);
        if results.is_empty() {
            Ok(None)
        } else {
            Ok(Some(results))
        }
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        // Route by language: pack files go through the invalidation seam
        // (re-register + reverse-closure eviction — the old path parsed
        // them with the Perl parser); Perl keeps the direct re-index.
        let mut perl_changes: Vec<(PathBuf, FileChangeType)> = Vec::new();
        {
            let reg = crate::language_driver::LanguageRegistry::with_enabled();
            for change in params.changes {
                let Ok(path) = change.uri.to_file_path() else { continue };
                match reg.for_path(&path).map(|d| d.id()) {
                    Some(id) if id != "perl" => {
                        self.schedule_pack_invalidate(
                            path,
                            change.typ == FileChangeType::DELETED,
                        );
                    }
                    _ => perl_changes.push((path, change.typ)),
                }
            }
        }
        if perl_changes.is_empty() {
            return;
        }
        let files = Arc::clone(&self.files);
        let module_index = Arc::clone(&self.module_index);
        let dirty = tokio::task::spawn_blocking(move || {
            // Externally changed deps break their consumers' enrichment too
            // — collect the dirty closure while the records are in hand and
            // hand it back for the open-doc republish below.
            let mut dirty_all: std::collections::HashSet<PathBuf> = Default::default();
            // The persisted generation (blob + ref rows) is now stale for
            // these paths; drop it so warm starts re-parse and the
            // relational retrieval can't serve outdated spans. The fresh
            // in-RAM copy registered below is FULL (never stripped), so the
            // resident sweep covers it until the next bulk index persists a
            // new generation.
            let ws_key = module_index.workspace_root();
            let conn = crate::module_cache::open_cache_db(ws_key.as_deref(), "perl");
            for (path, typ) in perl_changes {
                // A DELETED file can't canonicalize (it's gone) — resolve the
                // parent instead so the spelling still matches the canonical
                // keys everything was registered/persisted under.
                let canon = path.canonicalize().unwrap_or_else(|_| {
                    match (path.parent(), path.file_name()) {
                        (Some(dir), Some(name)) => std::fs::canonicalize(dir)
                            .map(|d| d.join(name))
                            .unwrap_or_else(|_| path.clone()),
                        _ => path.clone(),
                    }
                });
                if let Some(ref conn) = conn {
                    crate::module_cache::invalidate_generation(conn, &canon.to_string_lossy());
                    if canon != path {
                        crate::module_cache::invalidate_generation(
                            conn,
                            &path.to_string_lossy(),
                        );
                    }
                }
                module_index.invalidate_bag_cache(&canon);
                match typ {
                    FileChangeType::DELETED => {
                        files.remove_workspace(&path);
                        files.remove_workspace(&canon);
                        // Consumers of the departed file's packages, BEFORE
                        // the record (and its provided names) are removed.
                        dirty_all.extend(module_index.dirty_consumers(&canon));
                        // The hub's path/name registrations must go too, or
                        // the dead file stays a retrieval candidate and a
                        // phantom module in name lookups.
                        module_index.unregister_workspace_path(&canon);
                    }
                    _ => {
                        // Re-index the file (created or changed). The fresh
                        // copy registers WHOLE (refs + bag) in both stores:
                        // its persisted generation was just invalidated, so
                        // the resident copy is the only source until the
                        // next bulk index re-persists.
                        if let Ok(source) = std::fs::read_to_string(&path) {
                            let mut parser = crate::module_resolver::create_parser();
                            if let Some(tree) = parser.parse(&source, None) {
                                let analysis = crate::builder::build(&tree, source.as_bytes());
                                let arc = Arc::new(analysis);
                                files.insert_workspace_arc(canon.clone(), arc.clone());
                                module_index.record_workspace_projections(&canon, &arc);
                                // register_workspace_resident routes through
                                // record_and_dirty: the dirty set is bound to
                                // the record, so a re-register can't drop it.
                                let sd = module_index
                                    .register_workspace_resident(canon.clone(), arc);
                                dirty_all.extend(sd.dirty);
                            }
                        }
                    }
                }
            }
            dirty_all
        })
        .await
        .unwrap_or_default();
        self.republish_open_docs_in(&dirty).await;
    }

    async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        // Copy the source out and release the DashMap guard before awaiting
        // perltidy — see `formatting` for why holding it across the await
        // deadlocks concurrent didChange on the same file.
        let source = match self.files.get_open(uri) {
            Some(doc) => doc.text.clone(),
            None => return Ok(None),
        };

        // Extract lines for the range
        let start_line = params.range.start.line as usize;
        let end_line = params.range.end.line as usize;
        let lines: Vec<&str> = source.lines().collect();
        let end = end_line.saturating_add(1).min(lines.len());
        // A malformed or inverted client range (start after end, or start past
        // EOF) must degrade, not panic on the slice.
        if start_line >= end {
            return Ok(None);
        }
        let range_text: String = lines[start_line..end].join("\n") + "\n";

        // Shell out to perltidy on the range
        let output = match run_perltidy(range_text.clone()).await {
            Ok(o) if o.status.success() => o,
            _ => return Ok(None),
        };

        let formatted = String::from_utf8_lossy(&output.stdout).to_string();
        if formatted == range_text {
            return Ok(None);
        }

        Ok(Some(vec![TextEdit {
            range: Range {
                start: Position { line: start_line as u32, character: 0 },
                end: Position { line: end as u32, character: 0 },
            },
            new_text: formatted,
        }]))
    }

    async fn linked_editing_range(
        &self,
        params: LinkedEditingRangeParams,
    ) -> Result<Option<LinkedEditingRanges>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        let doc = match self.files.get_open(uri) {
            Some(doc) => doc,
            None => return Ok(None),
        };

        match symbols::linked_editing_ranges(&doc.analysis, pos, Some(&*self.module_index)) {
            Some(ranges) => Ok(Some(LinkedEditingRanges {
                ranges,
                word_pattern: None,
            })),
            None => Ok(None),
        }
    }
}

/// Run perltidy over `input`, returning its captured output.
///
/// `kill_on_drop` so a cancelled formatting request (the editor sends
/// `$/cancelRequest`, tower-lsp aborts the handler future) reaps perltidy
/// instead of leaving a `<defunct>` zombie (#80). The stdin write runs in its
/// own task concurrently with `wait_with_output`'s stdout drain so we never
/// block writing stdin while perltidy is blocked writing stdout.
async fn run_perltidy(input: String) -> std::io::Result<std::process::Output> {
    use tokio::io::AsyncWriteExt;

    let mut child = tokio::process::Command::new("perltidy")
        .arg("--standard-output")
        .arg("--standard-error-output")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let stdin = child.stdin.take();
    let writer = tokio::spawn(async move {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(input.as_bytes()).await;
            // drop closes stdin, signalling EOF to perltidy
        }
    });

    let output = child.wait_with_output().await;
    let _ = writer.await;
    output
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
