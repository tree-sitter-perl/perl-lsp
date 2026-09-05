//! The flush driver's worklist: propagate a change until the answers stop
//! moving, then publish one generation.
//!
//! `docs/adr/conclusion-layer.md` ("Change propagation: the flush worklist") owns the design. What
//! lives here is the loop and its two hard properties — the cutoff and
//! termination — factored so both are testable without a store, a thread, or a
//! corpus.
//!
//! **The diff artifact is the EVALUATED surface, never the persisted map.** A
//! map is index-free by construction, so when C changes B's map is
//! byte-identical while B's answers have moved; cutting on map equality stops
//! the wave at B and starves B's consumers. That is the whole soundness of the
//! cutoff, it passes every two-file fixture either way, and only a chain can
//! tell the two apart — see
//! `conclusions_tests::a_chain_needs_the_evaluated_surface_not_the_map`.

use crate::index::module_cache;
use crate::model::witnesses::{ConclusionMap, EvaluatedSurface};
use rusqlite::Connection;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Rounds a single flush may take before it is declared non-convergent.
///
/// The all-builds safety net, same role as `MAX_FOLD_ITERATIONS` one tier
/// down: convergence is a property of the lattice, not of this number, and a
/// flush that reaches the cap has found a bug rather than a deep chain. Real
/// dependency chains in a workspace are single digits.
pub const MAX_FLUSH_ROUNDS: usize = 32;

/// What one flush did. Returned rather than logged so a caller — and a test —
/// can assert on the shape of the propagation, not just its result.
#[derive(Debug, Default, PartialEq)]
pub struct FlushOutcome {
    /// The REFRESH set: files whose evaluated surface moved, with the surface
    /// it moved to. Whoever consumes these must re-enrich, re-diagnose or
    /// re-publish — their conclusion maps are untouched, because a map goes
    /// stale only when its own blob changes. A file evaluated and found
    /// unchanged is deliberately absent: the cutoff is the point.
    pub changed: Vec<(PathBuf, EvaluatedSurface)>,
    /// How many worklist rounds ran. `1` means the frontier cut immediately.
    pub rounds: usize,
    /// Files evaluated, including those that cut. The propagation's real cost.
    pub evaluated: usize,
    /// Every file the wave ENQUEUED because a provider's answers moved —
    /// including those that then cut.
    ///
    /// Distinct from `changed` on purpose, and the difference is the whole
    /// point of the re-stamp gate: a consumer whose own conclusion answers did
    /// not move can still DISPATCH differently now that its provider changed,
    /// because a dispatch target is resolved through the index rather than
    /// read off a surface. Marking only what moved would skip exactly those.
    pub enqueued: Vec<PathBuf>,
    /// The generation this flush published its seeds at, if it published.
    ///
    /// `None` means nothing was written: no seeds, a non-convergent wave, or
    /// a failed transaction. Absent is always safe — the store keeps the
    /// previous generation and a consult falls back to a decode.
    pub published: Option<module_cache::Generation>,
    /// The round cap fired: the surfaces never stopped moving. The flush is
    /// abandoned rather than published — a half-propagated generation is worse
    /// than none, because a consult pinned to it would compose answers from a
    /// wave that never finished.
    pub non_convergent: bool,
}

/// Run one flush to quiescence.
///
/// `evaluate` gives a file's surface in the world this flush is building;
/// `baseline` gives it as of the frozen generation; `consumers_of` is the
/// freshness reverse-dep walk.
///
/// The cutoff compares against the surface recorded EARLIER IN THIS FLUSH when
/// there is one, and against the baseline otherwise. That distinction is what
/// makes a cycle terminate: on the second visit A is compared to A-as-just-
/// recorded, so an unchanged re-derivation cuts instead of re-enqueuing B
/// forever. Comparing against the baseline every time would make any cycle run
/// until the cap.
pub fn run_flush(
    dirty: impl IntoIterator<Item = PathBuf>,
    evaluate: &dyn Fn(&Path) -> Option<EvaluatedSurface>,
    baseline: &dyn Fn(&Path) -> Option<EvaluatedSurface>,
    consumers_of: &dyn Fn(&Path) -> Vec<PathBuf>,
) -> FlushOutcome {
    let mut recorded: HashMap<PathBuf, EvaluatedSurface> = HashMap::new();
    let mut frontier: Vec<PathBuf> = dirty.into_iter().collect();
    let mut out = FlushOutcome::default();

    while !frontier.is_empty() {
        out.rounds += 1;
        if out.rounds > MAX_FLUSH_ROUNDS {
            crate::util::ghost_stats::count("flush.non_convergent");
            log::error!(
                "conclusion flush did not converge in {MAX_FLUSH_ROUNDS} rounds; \
                 abandoning rather than publishing a half-propagated generation"
            );
            out.non_convergent = true;
            return out;
        }
        let round = std::mem::take(&mut frontier);
        // Deduped per round: a file reached by three consumers in one round is
        // one re-bake, not three. Without this a wide fan-in multiplies the
        // round's cost by its width for no additional information.
        let mut seen_this_round: HashSet<PathBuf> = HashSet::new();
        for path in round {
            if !seen_this_round.insert(path.clone()) {
                continue;
            }
            out.evaluated += 1;
            let Some(surface) = evaluate(&path) else {
                // Cannot evaluate — a file that vanished, or one whose map is
                // gone. Cutting here is right: we have nothing to say about it
                // and inventing a change would propagate noise.
                crate::util::ghost_stats::count("flush.unevaluable");
                continue;
            };
            let prior = recorded.get(&path).cloned().or_else(|| baseline(&path));
            if prior.as_ref() == Some(&surface) {
                crate::util::ghost_stats::count("flush.cut");
                continue;
            }
            crate::util::ghost_stats::count("flush.moved");
            recorded.insert(path.clone(), surface);
            let consumers = consumers_of(&path);
            out.enqueued.extend(consumers.iter().cloned());
            frontier.extend(consumers);
        }
    }

    out.enqueued.sort();
    out.enqueued.dedup();
    out.changed = recorded.into_iter().collect();
    // Sorted for the same reason the surface itself is: the caller publishes
    // this and compares it, and a `HashMap` drain order would make two equal
    // flushes look different.
    out.changed.sort_by(|a, b| a.0.cmp(&b.0));
    out
}


/// The world one flush evaluates against: a frozen generation underneath, the
/// seeds this flush re-baked on top.
///
/// **A map goes stale only when its OWN file's blob changes.** The bake runs
/// with the index deliberately withheld, so it cannot produce a value that
/// depended on another file — anything cross-file comes out as a `Link` that
/// chases at read time, or as `OpenNone`. A downstream change therefore moves
/// a consumer's ANSWERS without moving its MAP, which is why the propagation
/// re-bakes nothing past the frontier: it decodes one map per reached file and
/// no blobs at all. `module_cache_tests::a_cleared_conclusion_row_is_re_baked_
/// to_the_same_map` is the standing evidence that a re-bake of an unchanged
/// blob reproduces the stored map exactly.
///
/// The overlay is what lets the wave move past its first hop. B's map is
/// index-free, so it reads identically before and after A changes; the only
/// thing that moved is what B's `Link`s chase THROUGH. Evaluating B against
/// the frozen store would reproduce B's frozen surface exactly, cut, and
/// starve B's consumers — the map-equality failure `EvaluatedSurface` exists
/// to avoid, arriving through the resolver instead of through the diff.
///
/// Its map sources are closures rather than a `Connection` for the same reason
/// `follow_link_with` takes a resolver: the overlay is delicate, and a world
/// that can only be built from a store is a world only exercised by whatever a
/// corpus happens to contain.
struct FlushWorld<'a> {
    frozen_src: &'a dyn Fn(&Path) -> Option<ConclusionMap>,
    re_bake: &'a dyn Fn(&Path) -> Option<ConclusionMap>,
    candidates_of: &'a dyn Fn(&str) -> Vec<PathBuf>,
    /// The files whose blobs changed — the only ones re-baked.
    seeds: HashSet<PathBuf>,
    frozen: RefCell<HashMap<PathBuf, Option<Arc<ConclusionMap>>>>,
    fresh: RefCell<HashMap<PathBuf, Option<Arc<ConclusionMap>>>>,
    /// Re-bake-equals-frozen breaks found under `PERL_LSP_FLUSH_EQUIV`.
    breaks: std::cell::Cell<usize>,
}

/// Re-bake every reached file, not just the seeds, and score the invariant the
/// propagation rests on.
///
/// The invariant is load-bearing and its violation is SILENT: a bake that
/// learned to consult an index would make non-seed maps genuinely stale, the
/// wave would evaluate the old ones, and the only symptom would be consumers
/// that quietly stopped being refreshed. Same discipline as
/// `PERL_LSP_CONCL_EQUIV` one tier down — the assumption ships with the switch
/// that checks it.
fn flush_equiv_enabled() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("PERL_LSP_FLUSH_EQUIV").is_ok())
}

impl<'a> FlushWorld<'a> {
    fn new(
        frozen_src: &'a dyn Fn(&Path) -> Option<ConclusionMap>,
        re_bake: &'a dyn Fn(&Path) -> Option<ConclusionMap>,
        candidates_of: &'a dyn Fn(&str) -> Vec<PathBuf>,
        seeds: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        FlushWorld {
            frozen_src,
            re_bake,
            candidates_of,
            seeds: seeds.into_iter().collect(),
            frozen: RefCell::new(HashMap::new()),
            fresh: RefCell::new(HashMap::new()),
            breaks: std::cell::Cell::new(0),
        }
    }

    fn frozen_map(&self, path: &Path) -> Option<Arc<ConclusionMap>> {
        if let Some(hit) = self.frozen.borrow().get(path) {
            return hit.clone();
        }
        let loaded = (self.frozen_src)(path).map(Arc::new);
        self.frozen.borrow_mut().insert(path.to_path_buf(), loaded.clone());
        loaded
    }

    /// A seed's re-bake, memoized.
    ///
    /// Memoized because the bake is a pure function of the file's own blob —
    /// nothing about it depends on the round, so a seed revisited by a cycle
    /// or a fan-in re-EVALUATES (which is the point) but never re-BAKES.
    fn baked(&self, path: &Path) -> Option<Arc<ConclusionMap>> {
        if let Some(hit) = self.fresh.borrow().get(path) {
            return hit.clone();
        }
        let baked = (self.re_bake)(path).map(Arc::new);
        self.fresh.borrow_mut().insert(path.to_path_buf(), baked.clone());
        baked
    }

    /// The map this flush believes a file has: its re-bake if its blob
    /// changed, its stored map otherwise.
    fn current_map(&self, path: &Path) -> Option<Arc<ConclusionMap>> {
        if self.seeds.contains(path) {
            return self.baked(path);
        }
        let frozen = self.frozen_map(path);
        if flush_equiv_enabled() {
            let baked = (self.re_bake)(path);
            if baked.is_some() && baked.as_ref() != frozen.as_deref() {
                self.breaks.set(self.breaks.get() + 1);
                crate::util::ghost_stats::count("flushequiv.break");
                log::warn!(
                    "flush equiv: re-baking '{}' did not reproduce its stored \
                     map — a downstream change moved a map, which the \
                     propagation assumes cannot happen",
                    path.display()
                );
            }
        }
        frozen
    }

    fn resolve(&self, class: &str, overlay: bool) -> Vec<(String, Option<Arc<ConclusionMap>>)> {
        (self.candidates_of)(class)
            .into_iter()
            .map(|p| {
                // Overlay reads only what this flush ALREADY baked — it never
                // bakes on demand. Baking a candidate here would pull the
                // whole reachable graph into a flush seeded by one file, and
                // the pull would be invisible: every candidate of every class
                // any evaluated key mentions, transitively.
                let map = if overlay {
                    self.fresh.borrow().get(p.as_path()).cloned().flatten()
                } else {
                    None
                };
                let map = map.or_else(|| self.frozen_map(&p));
                (p.to_string_lossy().into_owned(), map)
            })
            .collect()
    }

    fn evaluate(&self, path: &Path) -> Option<EvaluatedSurface> {
        let map = self.current_map(path)?;
        Some(map.evaluated_surface(&|class| self.resolve(class, true)))
    }

    /// The file's surface as of the frozen generation — the thing "moved" is
    /// measured against. Evaluated WITHOUT the overlay on purpose: it is the
    /// answer the world gave before this flush started.
    fn baseline(&self, path: &Path) -> Option<EvaluatedSurface> {
        let map = self.frozen_map(path)?;
        Some(map.evaluated_surface(&|class| self.resolve(class, false)))
    }
}

/// Propagate over a world.
///
/// The result is the **refresh set**: the files whose ANSWERS moved, and whose
/// consumers must therefore re-enrich, re-diagnose or re-publish. No map is
/// written — a map goes stale only when its own blob changes, so every file
/// the wave reaches already has the right one in the store.
fn flush_over_world(
    world: &FlushWorld<'_>,
    frontier: Vec<PathBuf>,
    consumers_of: &dyn Fn(&Path) -> Vec<PathBuf>,
) -> FlushOutcome {
    run_flush(
        frontier,
        &|p| world.evaluate(p),
        &|p| world.baseline(p),
        consumers_of,
    )
}

/// Compute the refresh set for a set of just-changed files.
///
/// `fresh` carries each CHANGED file's newly baked map. The caller bakes,
/// because the caller is the one that just built the analysis; making the
/// flush decode a blob to re-derive a map already in RAM would be this
/// layer's own antipattern, and at the seam this is wired to
/// (`didChangeWatchedFiles`) the blob has already been invalidated, so there
/// would be nothing to decode. With `fresh` supplied, the wave reads
/// conclusion maps and nothing else.
///
/// `frontier` is where the wave STARTS, which is not always `fresh`'s keys. A
/// DELETED file has no fresh map and nothing to evaluate, so it enters as its
/// direct consumers instead — they now resolve through a file the store has
/// forgotten, which is a move, and the wave carries it onward from there.
///
/// `consumers_of` is the freshness reverse-dep walk (`dirty_consumers`);
/// `candidates_of` maps a class to the files that declare it, the same
/// relation the live `follow_link` resolves through.
///
/// Publishes the seeds at generation N+1 before returning, which is safe
/// because each conclusions row now carries its OWN `source_fingerprint`: a
/// row whose `modules` row was invalidated a moment ago is not trusted on the
/// strength of that row's absence, it is checked against what the freshness
/// index records for the path and reads absent when they disagree.
///
/// The generation is read ONCE and frozen for the whole wave, so the frozen
/// store the propagation diffs against cannot shift underneath it mid-flush.
/// That is a property of THIS wave's arithmetic; readers need no such freeze,
/// because a row's fingerprint settles its validity on its own.
pub fn flush_refresh_set(
    conn: &Connection,
    fresh: Vec<FreshBake>,
    frontier: Vec<PathBuf>,
    consumers_of: &dyn Fn(&Path) -> Vec<PathBuf>,
    candidates_of: &dyn Fn(&str) -> Vec<PathBuf>,
) -> FlushOutcome {
    let at = module_cache::current_generation(conn);
    let fingerprints: HashMap<PathBuf, u64> = fresh
        .iter()
        .map(|f| (f.path.clone(), f.source_fingerprint))
        .collect();
    let supplied: HashMap<PathBuf, ConclusionMap> =
        fresh.into_iter().map(|f| (f.path, f.map)).collect();
    let seeds: Vec<PathBuf> = supplied.keys().cloned().collect();
    let frozen_src = |path: &Path| {
        module_cache::load_conclusions(conn, &path.to_string_lossy(), at)
    };
    // A seed's map comes from the caller. Anything else is only ever asked for
    // under `PERL_LSP_FLUSH_EQUIV`, which has to pay the blob decode precisely
    // because that is the assumption being checked — and with the bag, for the
    // reason the repair path takes it: a bagless decode bakes a map that
    // concludes nothing while looking like a clean re-bake, and the checker
    // would then report every file as a break.
    let re_bake = |path: &Path| {
        if let Some(m) = supplied.get(path) {
            return Some(m.clone());
        }
        let fa = module_cache::load_one_diag(conn, &path.to_string_lossy(), true).ok()?;
        Some(module_cache::bake_conclusion_map(&fa, &fa.witnesses))
    };
    let world = FlushWorld::new(&frozen_src, &re_bake, candidates_of, seeds);
    let mut out = flush_over_world(&world, frontier, consumers_of);
    out.published = publish_seeds(conn, at, &out, &supplied, &fingerprints);
    out
}

/// Write the seeds' fresh maps as one generation.
///
/// Only the SEEDS. A file the wave merely reached has the right map already —
/// the bake is index-free, so a downstream change moves its answers without
/// moving its map — and re-writing it would spend a transaction to store what
/// is already stored.
///
/// A non-convergent wave publishes nothing: its refresh set is a half-finished
/// propagation, and a consult pinned to that generation would compose answers
/// from a wave that never settled.
///
/// The stamp is the fingerprint the CALLER computed from the analysis it baked
/// from, carried through untouched. Reading it back from the freshness index
/// here would let a concurrent record put a different file's fingerprint on
/// this map, which is the one lie the consult-time compare cannot catch.
fn publish_seeds(
    conn: &Connection,
    at: module_cache::Generation,
    out: &FlushOutcome,
    supplied: &HashMap<PathBuf, ConclusionMap>,
    fingerprints: &HashMap<PathBuf, u64>,
) -> Option<module_cache::Generation> {
    if out.non_convergent || supplied.is_empty() {
        return None;
    }
    let next = module_cache::Generation(at.0 + 1);
    let entries: Vec<(String, ConclusionMap, u64)> = supplied
        .iter()
        .filter_map(|(path, map)| {
            // No fingerprint means no publishable stamp. Skipping is right:
            // an unstamped row would have to be trusted rather than checked.
            let fp = fingerprints.get(path)?;
            Some((path.to_string_lossy().into_owned(), map.clone(), *fp))
        })
        .collect();
    if entries.is_empty() {
        return None;
    }
    if let Err(e) = module_cache::publish_generation(conn, next, &entries) {
        // Never fatal. The previous generation stands, every consult falls
        // back to a decode, and the next flush republishes.
        crate::util::ghost_stats::count("flush.publish_failed");
        log::warn!("conclusion flush: publishing generation {} failed: {e}", next.0);
        return None;
    }
    crate::util::ghost_stats::count_by("flush.published", entries.len() as u64);
    // Reclaim superseded rows immediately. Safe because validity is per-row
    // and content-keyed: a reader that loses an older row either finds the
    // newer one — which passes the same fingerprint compare and therefore
    // carries the same content, the bake being deterministic — or finds
    // nothing and decodes. Neither outcome is a wrong answer, so retention
    // buys speed alone.
    let reclaimed = module_cache::prune_generations_below(conn, next);
    if reclaimed > 0 {
        crate::util::ghost_stats::count_by("flush.pruned", reclaimed as u64);
    }
    Some(next)
}

/// One seed of a flush: a file whose blob just changed, the map the caller
/// baked from its new analysis, and that analysis's surface fingerprint.
///
/// The three travel together because they must describe ONE state — the stamp
/// is what a consult checks the map against, so a stamp gathered separately
/// from the map is a stamp that can describe a different file.
pub struct FreshBake {
    pub path: PathBuf,
    pub map: ConclusionMap,
    pub source_fingerprint: u64,
}

#[cfg(test)]
#[path = "conclusion_flush_tests.rs"]
mod conclusion_flush_tests;

#[cfg(test)]
#[path = "conclusion_flush_store_tests.rs"]
mod conclusion_flush_store_tests;
