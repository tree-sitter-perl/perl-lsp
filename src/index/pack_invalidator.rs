//! The pack-file invalidation subsystem, in ONE home: file change → which
//! pack analyses are stale → re-analyze + swap registrations → which OPEN
//! documents the caller must refresh.
//!
//! `PackInvalidator` owns the three coordination facts that keep concurrent
//! invalidations sound:
//!
//! - the **serialization lock** (did_save + watcher events can race on the
//!   same header; unregister/register swaps must not interleave);
//! - the **bulk-index coordinator** (H9-2): changes arriving while the
//!   initial pack index is still attaching are deferred and reconciled once
//!   at `finish_bulk_index`;
//! - the **source-generation discipline** (H9-1): every swap claims the
//!   changed file's mtime generation, so a stale re-analysis (pre-save
//!   bytes) can never revert a fresher registration.
//!
//! The only mutation entry points are `file_changed` and
//! `finish_bulk_index` (plus the `begin_bulk_index` mark) — the worker that
//! evicts/re-analyzes/swaps is private, so a new invalidation path cannot
//! compile around the lock, the coordinator, or the generation guard.
//!
//! **The consumer rule is spelled once** (`consumes`): a consumer is any
//! analysis whose include closure contains the changed path, or — when the
//! change created or deleted a module marker (`LanguageDriver::module_marker`,
//! python's `__init__.py`) — any file under the marker's directory, whose
//! module name just changed. Both the registered-file re-analysis storm and
//! the open-document refresh set apply it. Closure consumers sit behind the
//! surface gate — an Unchanged verdict (body/comment edit in a header)
//! re-analyzes the changed file alone and returns an empty consumer set,
//! open documents included: their analyses are still semantically valid
//! (macro bodies and include directives are ON the surface), so
//! re-gathering them would be pure storm. Marker consumers do not: their
//! name moved whatever the marker file says.
//!
//! Lock order: `change_lock` is the subsystem's OUTERMOST lock — acquired
//! only from the entry points with no other lock held. Everything the worker
//! touches (pack index maps, the module-cache connection, the reparse
//! caches) nests strictly beneath it, and the open-document sweep over
//! `OpenDocs` runs AFTER the guard drops, so `change_lock` never meets a
//! store shard guard.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dashmap::DashMap;
use tower_lsp::lsp_types::Url;

use crate::index::open_docs::OpenDocs;
use crate::index::module_cache;
use crate::index::module_index::{CachedModule, ModuleIndex};
use crate::model::file_analysis::FileAnalysis;
use crate::util::canonical_path::CanonicalPath;

/// One change, as the consumer rule reads it.
struct Change<'a> {
    canon_str: &'a str,
    /// The directory whose marked state the change flipped.
    refiled: Option<&'a Path>,
}

/// Why an analysis is stale after a change.
#[derive(PartialEq, Eq)]
enum Consumes {
    /// Its include closure reaches the changed file (surface-gated).
    Closure,
    /// It lies under a directory whose marker came or went: the module it
    /// is filed under changed.
    Refile,
}

/// THE consumer rule: the include closure is the cross-file visibility key,
/// so it is also the REVERSE-dependency key; a flipped marker re-files
/// everything under its directory. Every consumer set — registered files
/// and open documents alike — is computed through this one predicate.
/// `path` is only asked (canonicalized) when a marker flipped.
fn consumes(path: impl FnOnce() -> Option<PathBuf>, analysis: &FileAnalysis, change: &Change<'_>) -> Option<Consumes> {
    if let Some(dir) = change.refiled {
        if path().is_some_and(|p| p.starts_with(dir) && p.as_os_str() != change.canon_str) {
            return Some(Consumes::Refile);
        }
    }
    analysis.pack.include_closure.contains(change.canon_str).then_some(Consumes::Closure)
}

/// What a `file_changed` call decided, for the caller to act on. The
/// invalidator mutates the index side itself; the caller (the LSP backend)
/// only publishes: it maps `refresh_open` onto its gather/diagnostics
/// machinery.
pub struct InvalidationOutcome {
    /// The change arrived during the initial pack bulk index and was
    /// deferred (H9-2). Nothing to publish now; `finish_bulk_index`
    /// reconciles it and the end-of-index open-doc heal covers the refresh.
    pub deferred: bool,
    /// OPEN documents to re-gather: the changed file itself (when open, not
    /// deleted, and its language gathers context) plus every open consumer
    /// — closure consumers gated by the surface verdict exactly like the
    /// registered-consumer storm.
    pub refresh_open: Vec<Url>,
}

/// What one invalidation sweep decided, for the open-document half.
#[derive(Default)]
struct Swept {
    /// The surface gate skipped the closure consumers.
    skip_consumers: bool,
    /// The directory a created/deleted marker re-filed.
    refiled: Option<PathBuf>,
}

impl InvalidationOutcome {
    fn deferred() -> Self {
        InvalidationOutcome { deferred: true, refresh_open: Vec::new() }
    }
}

/// Coordinates watcher invalidations against the INITIAL pack bulk index
/// (`index_pack_language`) — H9-2. While that index is in flight, an
/// invalidation would race the bulk cone and re-analyze it twice,
/// uncoordinated (and its registration could be overwritten by the bulk
/// pass's older read). Instead a save
/// arriving during the index is DEFERRED into a bounded set (one entry per
/// distinct path changed during the index) and reconciled ONCE at
/// completion: `finish_bulk_index` re-runs the worker per deferred path
/// against current disk, and the H9-1 source-generation guard makes that
/// safe — the reconcile reads the freshest bytes (highest generation) and
/// outranks whatever the bulk pass registered.
#[derive(Default)]
pub(crate) struct PackChangeCoordinator {
    in_flight: std::sync::atomic::AtomicBool,
    // path -> deleted. A HashMap so repeated saves of one path collapse to a
    // single reconcile (the reconcile reads current disk regardless).
    deferred: std::sync::Mutex<std::collections::HashMap<PathBuf, bool>>,
}

impl PackChangeCoordinator {
    /// Mark the initial pack index in flight. Call synchronously before the
    /// index is scheduled so a save racing the scheduling is still deferred.
    pub(crate) fn begin_index(&self) {
        self.in_flight
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Record a watched-file change. Returns `true` when the caller should
    /// DEFER (the index is in flight → the change is queued for reconcile);
    /// `false` when it should run the invalidation now. The flag check and
    /// the queue insert are one critical section with `finish_index`'s clear +
    /// drain, so a save can never be both dropped from the queue AND skipped by
    /// the normal path.
    pub(crate) fn note_change(&self, canon: &Path, deleted: bool) -> bool {
        let mut q = self.deferred.lock().unwrap_or_else(|e| e.into_inner());
        if self.in_flight.load(std::sync::atomic::Ordering::Relaxed) {
            q.insert(canon.to_path_buf(), deleted);
            true
        } else {
            false
        }
    }

    /// Clear the in-flight flag and drain the deferred set, atomically w.r.t.
    /// `note_change`. The returned pairs are the paths to reconcile once.
    pub(crate) fn finish_index(&self) -> Vec<(PathBuf, bool)> {
        let mut q = self.deferred.lock().unwrap_or_else(|e| e.into_inner());
        self.in_flight
            .store(false, std::sync::atomic::Ordering::Relaxed);
        q.drain().collect()
    }

    #[cfg(test)]
    pub(crate) fn is_in_flight(&self) -> bool {
        self.in_flight.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// The owner. One per server/CLI session, shared by the save/watcher event
/// forwarders and the bulk-index lifecycle. See the module doc for what it
/// owns and why.
pub struct PackInvalidator {
    /// The language whose files this invalidator re-analyzes — the
    /// process's served language in production.
    driver: &'static dyn crate::build::language_driver::LanguageDriver,
    change_lock: std::sync::Mutex<()>,
    coord: PackChangeCoordinator,
    /// The SOURCE generation (`module_cache::file_mtime_nanos`) the currently
    /// registered pack analysis for a path was built from — the H9-1
    /// stale-winner guard. The swap claims a path at its event generation and
    /// registers only when the claim succeeds (`incoming >= registered`), so
    /// a re-analysis that read pre-save bytes (a lower generation) can never
    /// revert a fresher registration, and a deferred-invalidation reconcile
    /// (H9-2) safely overrides only paths whose registered generation is
    /// older than the save it reconciles. Empty for a path the swap never
    /// touched (bulk/warm register ungated — they are the baseline every real
    /// edit outranks, and the reconcile's unregister+register replaces their
    /// entry outright).
    registered_source_gen: DashMap<PathBuf, i64>,
}

impl Default for PackInvalidator {
    fn default() -> Self {
        Self::serving(crate::build::language_driver::LanguageRegistry::served())
    }
}

impl PackInvalidator {
    /// An invalidator for `driver`'s files.
    pub fn serving(driver: &'static dyn crate::build::language_driver::LanguageDriver) -> Self {
        PackInvalidator {
            driver,
            change_lock: Default::default(),
            coord: Default::default(),
            registered_source_gen: Default::default(),
        }
    }

    /// Mark the initial pack bulk index in flight (H9-2). Call synchronously
    /// before the index task is scheduled so a save racing the scheduling
    /// still defers into the reconcile set.
    pub fn begin_bulk_index(&self) {
        self.coord.begin_index();
    }

    /// A pack file's bytes changed on disk (save or watcher event) — or the
    /// file was deleted. Runs the whole invalidation (blocking: Rayon inside;
    /// call off the message loop) and returns the outcome the caller
    /// publishes. `open_docs` is the caller's open-document view; the
    /// consumer rule maps it to the URIs whose gather must re-run.
    pub fn file_changed(
        &self,
        root_uri: Option<&str>,
        index: &ModuleIndex,
        open_docs: &OpenDocs,
        path: &Path,
        deleted: bool,
    ) -> InvalidationOutcome {
        let canon = CanonicalPath::resolve(path).into_path_buf();
        if self.coord.note_change(&canon, deleted) {
            return InvalidationOutcome::deferred();
        }
        let swept = {
            let _g = self.change_lock.lock().unwrap_or_else(|e| e.into_inner());
            self.pack_file_changed(root_uri, index, &canon, deleted)
        };
        // Open consumers re-analyze AFTER the eviction so their gather runs
        // cold against the new bytes. Off the serialization lock — the sweep
        // only reads closures, and holding `change_lock` across OpenDocs
        // shard guards would mint a lock ordering for no protection.
        let refresh_open = self.open_docs_to_refresh(open_docs, &canon, deleted, &swept);
        InvalidationOutcome { deferred: false, refresh_open }
    }

    /// The pack bulk index finished; reconcile every save deferred
    /// during the bulk index (H9-2) exactly once, off the same serialization
    /// lock steady-state invalidations use. The H9-1 generation guard makes
    /// this safe: each reconcile reads current disk (the freshest
    /// generation) and outranks whatever the bulk pass registered from
    /// earlier bytes. Open-doc refresh is NOT computed here — the
    /// end-of-index heal re-publishes every open pack doc anyway.
    pub fn finish_bulk_index(&self, root_uri: Option<&str>, index: &ModuleIndex) {
        let deferred = self.coord.finish_index();
        if deferred.is_empty() {
            return;
        }
        log::debug!(
            "pack index complete: reconciling {} deferred change(s)",
            deferred.len()
        );
        let _g = self.change_lock.lock().unwrap_or_else(|e| e.into_inner());
        for (path, deleted) in deferred {
            self.pack_file_changed(root_uri, index, &path, deleted);
        }
    }

    /// The open half of the consumer answer: the changed file's own open doc
    /// (its bytes changed regardless of verdict) plus open consumers by the
    /// one rule, surface-gated like the registered storm.
    fn open_docs_to_refresh(
        &self,
        open_docs: &OpenDocs,
        canon: &Path,
        deleted: bool,
        swept: &Swept,
    ) -> Vec<Url> {
        let canon_str = canon.to_string_lossy().into_owned();
        let change = Change { canon_str: &canon_str, refiled: swept.refiled.as_deref() };
        // A language that gathers no context has nothing to re-gather when
        // its own bytes change: the open buffer is its source.
        let gathers = self.driver.caps().context_gather;
        let mut out: Vec<Url> = Vec::new();
        open_docs.for_each_open(|u, doc| {
            let doc_path = || u.to_file_path().ok().map(|_| doc.canonical_path.to_path_buf());
            let is_self = gathers && !deleted && doc_path().is_some_and(|p| p == canon);
            let stale = match consumes(doc_path, &doc.analysis, &change) {
                Some(Consumes::Refile) => true,
                Some(Consumes::Closure) => !swept.skip_consumers,
                None => false,
            };
            if is_self || stale {
                out.push(u.clone());
            }
        });
        out
    }

    /// Claim `path` at source generation `gen` (H9-1). Succeeds — recording
    /// `gen` — iff `gen >= the generation already registered` (empty ⇒ the
    /// baseline `i64::MIN`, so a first claim always wins). A tie succeeds so a
    /// serialized fresh re-registration (the deferred-invalidation reconcile
    /// running after the bulk index) still lands; only a STRICTLY older
    /// generation — a re-analysis that read pre-save bytes — is rejected. The
    /// check-and-update is atomic under the DashMap entry lock, so two racing
    /// swaps can't both read-then-clobber. Callers that get `false` must NOT
    /// register: they would revert a fresher copy.
    pub(crate) fn claim_source_gen(&self, path: &Path, gen: i64) -> bool {
        use dashmap::mapref::entry::Entry;
        let canon = CanonicalPath::resolve(path).into_path_buf();
        match self.registered_source_gen.entry(canon) {
            Entry::Occupied(mut e) => {
                if gen >= *e.get() {
                    *e.get_mut() = gen;
                    true
                } else {
                    false
                }
            }
            Entry::Vacant(e) => {
                e.insert(gen);
                true
            }
        }
    }

    /// Forget `path`'s source generation (H9-1) — a genuine delete, so a later
    /// recreation claims from the baseline again.
    fn forget_source_gen(&self, path: &Path) {
        let canon = CanonicalPath::resolve(path).into_path_buf();
        self.registered_source_gen.remove(&canon);
    }

    /// In-session invalidation worker for a changed (saved/watched) or
    /// deleted pack file — the H1 seam. Order matters: evict the per-file
    /// analysis caches FIRST (macro tables, pre-expanded variants, closures)
    /// so the re-analyses here — and the open documents' background refresh
    /// after — re-gather instead of serving the frozen tables; and note a
    /// created/deleted module marker in the walk snapshot BEFORE anything
    /// re-analyzes, so every re-analysis names its module from the new
    /// state. Returns what the sweep decided for the open-doc half.
    /// Private: callers go through `file_changed` / `finish_bulk_index`,
    /// which hold `change_lock` and consult the coordinator.
    fn pack_file_changed(
        &self,
        root_uri: Option<&str>,
        index: &ModuleIndex,
        path: &Path,
        deleted: bool,
    ) -> Swept {
        use rayon::prelude::*;
        let driver = self.driver;
        // Only invalidator-owned languages route here; Perl's changes are the direct re-index path's business.
        if !driver.caps().pack_invalidation {
            return Swept::default();
        }
        let lang = driver.id();
        let canon = CanonicalPath::resolve(path);
        let canon_str = canon.to_string_lossy().into_owned();
        let pack = index;
        let refiled = driver
            .module_marker()
            .and_then(|m| crate::build::walk_markers::note_marker(m, &canon, !deleted));
        let change = Change { canon_str: &canon_str, refiled: refiled.as_deref() };

        // The source generation this invalidation registers under (H9-1): the
        // changed file's mtime, captured at call time. Every result (the changed
        // file AND its consumers) is claimed at this generation, so a later save's
        // invalidation — a strictly greater mtime — outranks it and a straggling
        // stale re-analysis (a smaller mtime) is rejected at the swap. A delete has
        // no mtime; use wall-clock now, which is monotone-forward past any prior
        // save and lets the deletion win.
        // A marker flip is an event NOW, whatever the marker's mtime says
        // (`mv` keeps an old one): the re-filed modules must outrank the
        // generations their last saves registered.
        let now = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(i64::MAX)
        };
        let event_gen = match refiled {
            Some(_) => now(),
            None => module_cache::file_mtime_nanos(&canon).unwrap_or_else(now),
        };

        let mut consumers: Vec<CanonicalPath> = Vec::new();
        let mut refile: Vec<CanonicalPath> = Vec::new();
        // Closures ride along for the Unchanged case: the consumers' persisted
        // deps_stamps must be recomputed (the edited header's mtime moved) or
        // the next warm scan rejects every consumer row and the cold storm the
        // gate prevents in-session comes back at restart.
        let mut consumer_closures: Vec<(PathBuf, crate::model::file_analysis::path_intern::ClosureList)> =
            Vec::new();
        pack.for_each_registered_file(&mut |cm| {
            match consumes(|| Some(cm.path.to_path_buf()), &cm.analysis, &change) {
                Some(Consumes::Closure) => {
                    consumers.push(cm.path.clone());
                    consumer_closures.push((cm.path.to_path_buf(), cm.analysis.pack.include_closure.clone()));
                }
                Some(Consumes::Refile) => refile.push(cm.path.clone()),
                None => {}
            }
        });

        if deleted {
            // The departed file's own header/macro/closure caches go too — a
            // consumer re-gather resolving the deleted header from its
            // still-warm entry would make the deletion invisible.
            crate::build::cpp_reparse::evict_analysis_caches(&std::iter::once(canon.to_path_buf()).collect());
            pack.unregister_file(&canon);
            pack.remove_surface(&canon);
            self.forget_source_gen(&canon);
        }

        // The surface gate (the freshness firewall, pack flavor): re-analyze
        // the CHANGED file first, alone. If its span-free surface is unchanged
        // — a body edit, a comment, a reformat in a header — every consumer's
        // analysis is still semantically valid (macro bodies and include
        // directives are ON the surface, so textual-inclusion effects are
        // covered) and the whole consumer re-analysis storm is skipped. A
        // deep-header comment edit re-parses ONE file, not hundreds of TUs.
        let mut changed_verdict = crate::model::surface::SurfaceVerdict::Changed;
        let mut changed_fa: Option<Arc<FileAnalysis>> = None;
        if !deleted {
            crate::build::cpp_reparse::evict_analysis_caches(&std::iter::once(canon.to_path_buf()).collect());
            if let Ok(source) = std::fs::read_to_string(&canon) {
                let probe = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    driver.analyze_with_path(&source, Some(&canon))
                }));
                if let Ok(fa) = probe {
                    changed_verdict = pack.record_surface(&canon, &fa);
                    changed_fa = Some(Arc::new(fa));
                }
            }
        }
        let skip_consumers = matches!(changed_verdict, crate::model::surface::SurfaceVerdict::Unchanged);
        if !skip_consumers {
            consumers.append(&mut refile);
        } else {
            // Re-filed modules re-analyze whatever the marker's own surface
            // says; the closure consumers keep their (still valid) rows.
            std::mem::swap(&mut consumers, &mut refile);
        }
        if !consumers.is_empty() {
            // The changed file's own caches were evicted before the probe and are
            // fresh — evict only the consumers' so they re-gather.
            crate::build::cpp_reparse::evict_analysis_caches(&consumers.iter().map(|p| p.to_path_buf()).collect());
        }

        // Re-analyze the changed file (unless deleted) + every consumer
        // (parallel), then swap registrations. Unregister-then-register so names
        // the new version no longer defines don't linger in `all_defs` / the
        // cache winner slot. Consumers re-analyze on delete too — their splices
        // and closures baked the departed header.
        let mut targets: Vec<CanonicalPath> = Vec::with_capacity(consumers.len() + 1);
        if !deleted && changed_fa.is_none() {
            targets.push(canon.clone());
        }
        targets.extend(consumers);
        targets.sort();
        targets.dedup();
        targets.retain(|p| changed_fa.is_none() || *p != canon);
        let mut results: Vec<(CanonicalPath, Arc<FileAnalysis>)> = targets
            .par_iter()
            .filter_map(|p| {
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let source = std::fs::read_to_string(p).ok()?;
                    Some(driver.analyze_with_path(&source, Some(p)))
                }));
                match res {
                    Ok(Some(analysis)) => Some((p.clone(), Arc::new(analysis))),
                    _ => None,
                }
            })
            .collect();
        if let Some(fa) = changed_fa {
            results.push((canon.clone(), fa));
        }
        // Persist FIRST so a stripped copy always has a blob to rehydrate
        // from, then register. The fork is `run_persist_writer`'s: it owns
        // BEGIN IMMEDIATE / COMMIT / ROLLBACK and routes every entry to the
        // committed or the fallback lane. The strip licence is a LANDED blob
        // for that path — `save_to_db`'s verdict, under a transaction that
        // committed — and exactly one thing gets to decide it.
        let conn = module_cache::open_cache_db(root_uri, lang);
        if let Some(ref conn) = conn {
            if deleted {
                module_cache::delete_ref_rows(conn, &canon_str);
            }
            if skip_consumers {
                // Unchanged gate: the consumers' rows/blobs/stubs are still
                // valid, but the edited header's mtime moved every consumer's
                // closure stamp — refresh them or the next warm rejects every
                // consumer row (the restart cold storm). Outside the writer's
                // transaction: these rows are already valid, so a refresh that
                // lands while the changed file's blob does not costs nothing.
                let mut memo = std::collections::HashMap::new();
                for (p, closure) in &consumer_closures {
                    module_cache::refresh_deps_stamp(
                        conn,
                        &p.to_string_lossy(),
                        closure,
                        &mut memo,
                    );
                }
            }
        }
        match conn {
            Some(ref conn) => {
                // Paths whose blob row actually landed. Read by the committed
                // lane only — the fallback lane's transaction is gone, so
                // nothing in it landed whatever the per-file verdict said.
                let landed: std::cell::RefCell<std::collections::HashSet<CanonicalPath>> =
                    Default::default();
                let (tx, rx) = std::sync::mpsc::channel();
                for (p, arc) in &results {
                    let _ = tx.send((p.clone(), Arc::clone(arc)));
                }
                drop(tx);
                crate::index::module_resolver::run_persist_writer(
                    rx,
                    Some(conn),
                    "pack swap persist",
                    |conn, batch: &[(CanonicalPath, Arc<FileAnalysis>)]| {
                        for (p, arc) in batch {
                            let p_str = p.to_string_lossy();
                            let cached = Arc::new(CachedModule::new(p.clone(), Arc::clone(arc)));
                            if !module_cache::save_to_db(conn, &p_str, &Some(cached), "workspace") {
                                // Shredding now would pair a new generation's
                                // rows with a blob that isn't there — the same
                                // write invariant `save_module_generation` holds.
                                continue;
                            }
                            if !arc.degraded {
                                let seeds = arc.row_seeds();
                                if let Err(e) = module_cache::shred_derived_rows(
                                    conn, &p_str, "workspace", &seeds,
                                ) {
                                    log::warn!("Failed to shred derived rows for {:?}: {}", p, e);
                                }
                            }
                            landed.borrow_mut().insert(p.clone());
                        }
                    },
                    |(p, arc): (CanonicalPath, Arc<FileAnalysis>)| {
                        let ok = landed.borrow().contains(&p);
                        self.register_after_swap(pack, &p, &arc, ok, event_gen);
                    },
                    |(p, arc): (CanonicalPath, Arc<FileAnalysis>)| {
                        self.register_after_swap(pack, &p, &arc, false, event_gen);
                    },
                );
            }
            // No cache DB at all: nothing can land, so nothing may be
            // stripped. (`run_persist_writer`'s own no-connection arm drains
            // its channel unregistered, which would drop these files.)
            None => {
                for (p, arc) in &results {
                    self.register_after_swap(pack, p, arc, false, event_gen);
                }
            }
        }
        Swept { skip_consumers, refiled }
    }

    /// The registration half of a swap, for one file. `landed` is the strip
    /// licence — a blob committed for THIS path — because a stripped copy
    /// without one does not read as absent: `load_one_diag` skips stamp
    /// validation on a single-row path, so it rehydrates the PREVIOUS
    /// generation and the file answers pre-edit for the rest of the session,
    /// with no rehydration miss counted and no strict-residency panic.
    fn register_after_swap(
        &self,
        pack: &ModuleIndex,
        path: &Path,
        arc: &Arc<FileAnalysis>,
        landed: bool,
        event_gen: i64,
    ) {
        // H9-1 generation guard: claim BEFORE unregistering, so a rejected
        // (strictly-older) result leaves the fresher registration intact rather
        // than tearing it down. A stale re-analysis that read pre-save bytes
        // loses to nothing — it simply isn't registered, and the writer that
        // read post-save bytes (or a later save's event) wins. This also closes
        // hazard 3: an under-invalidated consumer the bulk pass registered from
        // pre-save bytes carries a lower generation than the reconcile that
        // reads current disk, so the reconcile wins and no pre-save bytes are
        // silently served.
        if !self.claim_source_gen(path, event_gen) {
            log::debug!(
                "pack swap: skip stale re-register of {:?} (event gen {} < registered)",
                path,
                event_gen
            );
            return;
        }
        pack.unregister_file(path);
        // Drop the stale LRU pin BEFORE the new copy becomes reachable — a
        // query racing this re-register must not rehydrate the pre-edit
        // generation against the new registration.
        pack.invalidate_derived_copies(path);
        if landed && !arc.degraded && crate::index::module_resolver::eviction_enabled() {
            // Registration-owned strip (feeds read the whole copy).
            let _ = pack.register_symbols_stripping(path.to_path_buf(), (**arc).clone(), crate::model::file_analysis::Residency::Skeleton);
        } else {
            pack.register_symbols(path.to_path_buf(), Arc::clone(arc));
        }
    }

}

#[cfg(test)]
#[path = "pack_invalidator_tests.rs"]
mod pack_invalidator_tests;
