//! Post-walk query-pattern dispatch (`docs/adr/plugin-system.md`).
//!
//! Plugins declare their items of interest as tree-sitter queries
//! (`FrameworkPlugin::patterns`); this driver runs them once per file
//! after the live walk, gates each match by the plugin's triggers at
//! the match site's package, computes the declared projections for
//! actual matches only, and dispatches `on_match`. Emissions flow
//! through the same `apply_emit_action` path as the emit hooks.
//!
//! Runs post-walk (scopes, package ranges, constant folds complete) but
//! BEFORE the deferred `VarType` / named-sub-param flushes, so pattern
//! emissions land in the same downstream machinery as hook emissions.

use std::collections::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock};

use tree_sitter::{
    CaptureQuantifier, Node, Query, QueryCursor, QueryPredicateArg, StreamingIterator,
};

use crate::model::file_analysis::{DispatchCandidate, HandlerOwner, InferredType, ReceiverGated, RefBinding, Span};
use crate::build::plugin::{self, CaptureData, CaptureValue, MatchContext, PatternSpec};

use super::{node_to_span, Builder};

/// A `#receiver-isa?` deferred predicate on a pattern: NOT a match-time
/// filter (receiver isa is a cross-file, query-time question — see
/// `docs/adr/receiver-gated-dispatch.md`). It tags the match so its
/// `DispatchCall` emissions are recorded as `ReceiverGated` candidates
/// instead of applied directly; `FileAnalysis::applicable_dispatches`
/// resolves them against the receiver's actual class at query time,
/// exactly like the `dispatch_verbs()` manifest path.
struct ReceiverGate {
    capture_index: u32,
    target_class: String,
}

/// Extract a pattern's `#receiver-isa? @cap "Class"` predicate, if any.
/// Unknown predicate names land in `general_predicates` unevaluated —
/// the binding's reservation this tier is built on.
fn receiver_gate_for(query: &Query, pattern_index: usize) -> Option<ReceiverGate> {
    for p in query.general_predicates(pattern_index) {
        if &*p.operator != "receiver-isa?" {
            continue;
        }
        let mut cap = None;
        let mut class = None;
        for a in &p.args {
            match a {
                QueryPredicateArg::Capture(ix) => cap = Some(*ix),
                QueryPredicateArg::String(s) => class = Some(s.to_string()),
            }
        }
        match (cap, class) {
            (Some(capture_index), Some(target_class)) => {
                return Some(ReceiverGate {
                    capture_index,
                    target_class,
                })
            }
            _ => {
                log::error!(
                    "#receiver-isa? needs a capture and a class string; got {:?}",
                    p.args
                );
            }
        }
    }
    None
}

/// Compile a pattern query once per unique source text, process-wide.
/// `Query::new` is expensive; patterns are static per plugin load, so
/// the leak is bounded (one per distinct pattern source). Compile
/// errors are cached too — a broken pattern logs once per build, not
/// once per match attempt.
fn cached_pattern_query(source: &str) -> Result<&'static Query, String> {
    static CACHE: OnceLock<Mutex<HashMap<u64, Result<&'static Query, String>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        source.hash(&mut h);
        h.finish()
    };
    if let Some(q) = cache.lock().unwrap().get(&key) {
        return q.clone();
    }
    let language: tree_sitter::Language = ts_parser_perl::LANGUAGE.into();
    let compiled: Result<&'static Query, String> = Query::new(&language, source)
        .map_err(|e| e.to_string())
        .and_then(|q| {
            // A pattern with zero captures is the top-level-predicate
            // trap: `[alts] (#pred …)` does NOT attach the predicate to
            // the alternation — it becomes its own degenerate pattern
            // (matching everywhere, capturing nothing) and the
            // alternation runs UNFILTERED. Hard error so the author
            // fixes the spelling to `([alts] (#pred …))` instead of
            // shipping a dead filter. (This exact trap shipped in
            // `query_cache::cpanfile_requires`.)
            for i in 0..q.pattern_count() {
                let quants = q.capture_quantifiers(i);
                if quants.iter().all(|qt| matches!(qt, CaptureQuantifier::Zero)) {
                    return Err(format!(
                        "pattern #{} captures nothing — a predicate after a bracketed \
                         alternation attaches to NOTHING (the alternation runs \
                         unfiltered). Wrap them in a group: ([…] (#pred …))",
                        i
                    ));
                }
            }
            let leaked: &'static Query = Box::leak(Box::new(q));
            Ok(leaked)
        });
    cache.lock().unwrap().insert(key, compiled.clone());
    compiled
}

/// Compile every Perl pattern query once, up front, at plugin-registry load.
///
/// `cached_pattern_query`'s memo is process-wide but compiles OUTSIDE its
/// lock and is populated lazily on first dispatch. Under the parallel cold
/// workspace index (`par_iter` over `build()`), that lets each Rayon worker
/// recompile the whole ~14-query set on the first file it touches — ~750ms of
/// `Query::new` charged to a handful of files' build phase. Warming
/// the memo here, single-threaded before any parallel build starts, makes
/// every per-file dispatch a pure cache hit and removes the race entirely.
pub(crate) fn warm_pattern_queries<'a>(specs: impl Iterator<Item = &'a PatternSpec>) {
    use rayon::prelude::*;
    let sources: Vec<&str> = specs
        .filter(|s| s.language == "perl")
        .map(|s| s.query.as_str())
        .collect();
    // Distinct sources compile independently, and `cached_pattern_query`
    // compiles outside its lock — parallel warming populates the same memo,
    // it just pays the wall of the slowest single `Query::new` instead of
    // the sum (~520 ms serial for the bundled set; the whole registry warm
    // sits on the first didOpen's critical path when a client opens a file
    // immediately after the handshake).
    sources.par_iter().for_each(|src| {
        let _ = cached_pattern_query(src);
    });
}

/// Verify a pattern's `expect` snippets against the real grammar:
/// parse each snippet, run the query, assert the match count and any
/// declared capture texts. This is the pattern author's guard against
/// the query medium's silent-match-nothing failure mode (field names
/// that print in the CST but don't match in the query engine, anchor
/// subtleties, …). Run by `--plugin-check` and by
/// `bundled_pattern_expects_hold` over every bundled pattern.
pub(crate) fn verify_pattern_expects(spec: &PatternSpec) -> Result<(), String> {
    if spec.language != "perl" {
        return Ok(());
    }
    let query = cached_pattern_query(&spec.query)
        .map_err(|e| format!("pattern `{}`: query compile failed: {}", spec.name, e))?;
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .map_err(|e| e.to_string())?;
    for ex in &spec.expect {
        let tree = parser
            .parse(&ex.src, None)
            .ok_or_else(|| format!("pattern `{}` expect `{}`: parse failed", spec.name, ex.src))?;
        let mut count = 0usize;
        let mut texts: HashMap<String, String> = HashMap::new();
        {
            let mut cursor = QueryCursor::new();
            let mut it = cursor.matches(query, tree.root_node(), ex.src.as_bytes());
            while let Some(m) = it.next() {
                count += 1;
                for c in m.captures {
                    let name = query.capture_names()[c.index as usize];
                    texts.insert(
                        name.to_string(),
                        c.node.utf8_text(ex.src.as_bytes()).unwrap_or("").to_string(),
                    );
                }
            }
        }
        if count != ex.matches {
            return Err(format!(
                "pattern `{}` expect `{}`: {} matches, expected {}",
                spec.name, ex.src, count, ex.matches
            ));
        }
        for (cap, want) in &ex.captures {
            match texts.get(cap) {
                Some(got) if got == want => {}
                other => {
                    return Err(format!(
                        "pattern `{}` expect `{}`: capture @{} = {:?}, expected {:?}",
                        spec.name, ex.src, cap, other, want
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Union of the match's capture spans — the match extent handed to the
/// plugin. A pattern with a root capture (`… ) @call`) gets that node's
/// span, since it encloses every other capture.
fn union_span(caps: &[(u32, Node<'_>)]) -> Span {
    let mut it = caps.iter().map(|(_, n)| node_to_span(*n));
    let first = it.next().expect("non-empty capture list");
    it.fold(first, |acc, s| Span {
        start: acc.start.min(s.start),
        end: acc.end.max(s.end),
    })
}


// ---------------------------------------------------------------------------
// One traversal instead of one per spec.
//
// Each `QueryCursor::matches(query, root, …)` is a full traversal of the
// file's tree, so running the 13 Perl walk-phase patterns as 13 queries walks
// every file 13 times. Tree-sitter's intended shape is one `Query` holding
// many patterns, walked once, with `pattern_index` naming the owner.
//
// Measured on the gold substrate (3,520 files, cold): 17,874 ms of separate
// traversals against 1,814 ms combined, with identical per-spec match sets.
//
// Nothing downstream needs remapping, which is why this is small: capture
// indices are already read through `query.capture_names()` and quantifiers
// through `query.capture_quantifiers(pattern_index)`, so handing
// `build_match_context` the COMBINED query and the COMBINED pattern index is
// the whole translation. `receiver_gate_for` reads
// `general_predicates(pattern_index)` on the same pair.
// ---------------------------------------------------------------------------

/// `PERL_LSP_PD_NO_COMBINE=1` forces the per-spec traversals. The permanent
/// escape hatch: if a future pattern turns out not to compose, this restores
/// the old behaviour without a revert, and it is what the equivalence harness
/// runs as its control arm.
fn combine_disabled() -> bool {
    static OFF: OnceLock<bool> = OnceLock::new();
    *OFF.get_or_init(|| std::env::var("PERL_LSP_PD_NO_COMBINE").as_deref() == Ok("1"))
}

/// Eligible walk-phase specs, in registry order, paired with their compiled
/// query. A spec whose own query fails to compile is DROPPED here and logged —
/// exactly what the per-spec loop did with it, so exclusion changes nothing.
/// The ordinal of a spec in this vec is its bucket index for the round.
fn eligible_walk_specs<'p>(
    plugins: &'p crate::build::plugin::PluginRegistry,
) -> Vec<(&'p dyn plugin::FrameworkPlugin, &'p PatternSpec, &'static Query)> {
    let mut out = Vec::new();
    for p in plugins.all() {
        for spec in p.patterns() {
            if spec.language != "perl" || spec.phase != "walk" {
                continue;
            }
            match cached_pattern_query(&spec.query) {
                Ok(q) => out.push((p, spec, q)),
                Err(e) => log::error!(
                    "plugin `{}` pattern `{}`: query compile failed: {}",
                    p.id(),
                    spec.name,
                    e
                ),
            }
        }
    }
    out
}

/// The combined query for a set of already-valid specs, plus the start
/// pattern index of each spec within it.
struct CombinedWalk<'r> {
    query: &'static Query,
    /// `starts[i]` = combined pattern index at which spec `i` begins. One
    /// source can contribute several patterns, so the owner of combined
    /// pattern `k` is the last `i` with `starts[i] <= k`.
    starts: &'r [usize],
}

/// The owned form the registry stores; `CombinedWalk` is the borrowed view.
struct OwnedCombinedWalk {
    query: &'static Query,
    starts: Vec<usize>,
}

impl CombinedWalk<'_> {
    /// Combine only specs that already compile on their own, so one malformed
    /// `.rhai` can never take dispatch out for the other twelve — it is
    /// dropped by `eligible_walk_specs` before it reaches the concatenation.
    /// `None` means "use the per-spec path", which is always correct.
    fn build(parts: Vec<(String, usize)>) -> Option<OwnedCombinedWalk> {
        if parts.is_empty() {
            return None;
        }
        let joined = parts
            .iter()
            .map(|(src, _)| src.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let query = match cached_pattern_query(&joined) {
            Ok(q) => q,
            Err(e) => {
                log::error!(
                    "combined walk-pattern query failed to compile ({e}); \
                     falling back to per-spec traversal"
                );
                crate::util::ghost_stats::count("pd.combine.compile_failed");
                return None;
            }
        };
        let mut starts = Vec::with_capacity(parts.len());
        let mut acc = 0usize;
        for (_, count) in &parts {
            starts.push(acc);
            acc += count;
        }
        if acc != query.pattern_count() {
            // Concatenation did not preserve pattern count, so routing by
            // offset would mis-attribute matches — silently, to the wrong
            // plugin. Refuse to use it.
            log::error!(
                "combined walk-pattern query has {} patterns, expected {}; \
                 falling back to per-spec traversal",
                query.pattern_count(),
                acc
            );
            crate::util::ghost_stats::count("pd.combine.pattern_count_mismatch");
            return None;
        }
        Some(OwnedCombinedWalk { query, starts })
    }

    fn owner_of(&self, pattern_index: usize) -> usize {
        match self.starts.binary_search(&pattern_index) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
    }
}

thread_local! {
    /// Combine mode pinned for the current build, overriding the env gate.
    /// Mirrors `walk::with_walk_mode` — it is what lets an equivalence check
    /// build the same file both ways inside one process.
    static COMBINE_FORCED: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

pub(crate) fn combine_forced() -> Option<bool> {
    COMBINE_FORCED.with(|c| c.get())
}

/// Run `f` with combine mode pinned. Restores the previous setting after.
pub(crate) fn with_combine<R>(combined: bool, f: impl FnOnce() -> R) -> R {
    let prev = COMBINE_FORCED.with(|c| c.replace(Some(combined)));
    let out = f();
    COMBINE_FORCED.with(|c| c.set(prev));
    out
}

/// `PERL_LSP_PD_EQUIV=1` — check both collection paths agree on every round of
/// every file. Unlike the whole-analysis net this one runs in release builds,
/// because the bar is equivalence over the CORPUS and the corpus is only
/// reachable from the CLI.
pub(crate) fn collection_equiv_enabled() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("PERL_LSP_PD_EQUIV").as_deref() == Ok("1"))
}

/// The registry's combined query, derived once and cached ON the registry.
///
/// Deriving it means joining every spec source and hashing the result to look
/// up the compiled-query memo — a per-PROCESS cost that reads as per-file if
/// it lives at the call site. Measured there first: 1,903 ms across 3,520
/// files, 49% of the whole dispatch phase, eating half the win the combined
/// query had just bought. The registry owns the slot because the answer is a
/// function of the registry's plugins and nothing else.
///
/// `None` = use the per-spec path.
fn combined_walk<'r>(
    registry: &'r crate::build::plugin::PluginRegistry,
    specs: &[(&dyn plugin::FrameworkPlugin, &PatternSpec, &'static Query)],
) -> Option<CombinedWalk<'r>> {
    if !combine_forced().unwrap_or(!combine_disabled()) {
        return None;
    }
    // Owned, so the derivation can run on another thread: the source text
    // plus each spec's pattern count is everything the concatenation needs.
    let parts: Vec<(String, usize)> = specs
        .iter()
        .map(|(_, spec, q)| (spec.query.clone(), q.pattern_count()))
        .collect();
    let ready = registry.combined_walk(move || {
        crate::util::ghost_stats::count("pd.combine.built");
        CombinedWalk::build(parts).map(|c| (c.query, c.starts))
    });
    // How many files ran before the background compile landed. The deferred
    // compile is a startup-ORDERING change, so "how long was the window" is
    // the question a large corpus actually answers; without this you can only
    // infer it from a phase total.
    crate::util::ghost_stats::count(if ready.is_some() {
        "pd.combine.ready"
    } else {
        "pd.combine.pending"
    });
    ready.map(|(query, starts)| CombinedWalk { query, starts })
}

/// Matches for one spec in one round: `(pattern_index, captures)` in the
/// order the cursor produced them, plus the query those indices are relative
/// to (the combined one, or the spec's own under fallback).
type SpecMatches<'t> = (&'static Query, Vec<(usize, Vec<(u32, Node<'t>)>)>);

impl<'a> Builder<'a> {
    /// Innermost package at a point, from the walk's `package_ranges`
    /// (latest-starting containing range wins — same rule as
    /// `FileAnalysis::package_at`), defaulting to the implicit `main`
    /// before any explicit package statement.
    fn package_at_point(&self, point: tree_sitter::Point) -> String {
        let mut best: Option<&crate::model::file_analysis::PackageRange> = None;
        for r in &self.package_ranges {
            if !crate::model::file_analysis::contains_point(&r.span, point) {
                continue;
            }
            let win = match best {
                None => true,
                Some(prev) => {
                    (r.span.start.row, r.span.start.column)
                        > (prev.span.start.row, prev.span.start.column)
                }
            };
            if win {
                best = Some(r);
            }
        }
        best.map(|r| r.package.clone())
            .unwrap_or_else(|| "main".to_string())
    }

    /// Collect this round's matches for every eligible spec.
    ///
    /// One traversal with the combined query when it is available, bucketed
    /// back to each spec by `pattern_index`; otherwise one traversal per spec.
    /// Both arms return the same shape, so the dispatch loop below is written
    /// once and cannot drift between them.
    ///
    /// A cursor that exceeds its match limit DROPS matches instead of
    /// erroring, which would silently under-dispatch. The combined query
    /// carries far more in-flight state than any single pattern, so the check
    /// is wired here permanently and its overrun falls back to the per-spec
    /// traversal for that round rather than trusting a truncated result.
    fn collect_walk_matches(
        &self,
        root: Node<'a>,
        specs: &[(&dyn plugin::FrameworkPlugin, &PatternSpec, &'static Query)],
        combined: Option<&CombinedWalk>,
    ) -> Vec<SpecMatches<'a>> {
        if let Some(c) = combined {
            let mut buckets: Vec<Vec<(usize, Vec<(u32, Node<'a>)>)>> =
                vec![Vec::new(); specs.len()];
            let mut cursor = QueryCursor::new();
            {
                let mut it = cursor.matches(c.query, root, self.source);
                while let Some(m) = it.next() {
                    let caps: Vec<(u32, Node<'a>)> =
                        m.captures.iter().map(|cap| (cap.index, cap.node)).collect();
                    if caps.is_empty() {
                        continue;
                    }
                    buckets[c.owner_of(m.pattern_index)].push((m.pattern_index, caps));
                }
            }
            if !cursor.did_exceed_match_limit() {
                let out: Vec<SpecMatches<'a>> =
                    buckets.into_iter().map(|b| (c.query, b)).collect();
                if collection_equiv_enabled() {
                    self.assert_collections_agree(root, specs, c, &out);
                }
                return out;
            }
            log::error!(
                "combined walk-pattern cursor exceeded its match limit;                  falling back to per-spec traversal for this round"
            );
            crate::util::ghost_stats::count("pd.combine.exceeded_match_limit");
        }
        specs
            .iter()
            .map(|(_, _, q)| {
                let mut out = Vec::new();
                let mut cursor = QueryCursor::new();
                {
                    let mut it = cursor.matches(*q, root, self.source);
                    while let Some(m) = it.next() {
                        let caps: Vec<(u32, Node<'a>)> =
                            m.captures.iter().map(|cap| (cap.index, cap.node)).collect();
                        if !caps.is_empty() {
                            out.push((m.pattern_index, caps));
                        }
                    }
                }
                if cursor.did_exceed_match_limit() {
                    log::error!("walk-pattern cursor exceeded its match limit");
                    crate::util::ghost_stats::count("pd.spec.exceeded_match_limit");
                }
                (*q, out)
            })
            .collect()
    }

    /// `PERL_LSP_PD_EQUIV=1` — assert the combined traversal reproduces the
    /// per-spec traversals exactly, on every round of every file.
    ///
    /// Element-wise rather than set-wise, and this is the whole point: the
    /// dispatch loop below is a pure function of (spec order, per-spec match
    /// list, builder state). If the match lists agree element for element at
    /// the start of every round, the dispatch is identical by construction —
    /// same emissions, same order, same state evolution, inductively across
    /// rounds. A set comparison would agree while the order differed, and
    /// emission order is exactly what this change could break.
    ///
    /// Three things are checked per match, and they are the three that the
    /// combined query renumbers:
    ///   - the pattern index, RELATIVE to the spec's start offset,
    ///   - each capture's NAME (indices are global to the combined query),
    ///   - each capture's quantifier, which decides `Many` vs `One` in
    ///     `build_match_context`.
    fn assert_collections_agree(
        &self,
        root: Node<'a>,
        specs: &[(&dyn plugin::FrameworkPlugin, &PatternSpec, &'static Query)],
        combined: &CombinedWalk,
        got: &[SpecMatches<'a>],
    ) {
        let want = self.collect_walk_matches(root, specs, None);
        let names = combined.query.capture_names();
        for (i, ((_, spec, q), ((_, a), (_, b)))) in
            specs.iter().zip(got.iter().zip(want.iter())).enumerate()
        {
            let local_names = q.capture_names();
            let bad = |what: &str| {
                crate::util::ghost_stats::count("pd.equiv.mismatch");
                panic!(
                    "pattern-combine divergence in `{}`: {what}\n\
                     combined: {:?}\n per-spec: {:?}",
                    spec.name,
                    a.iter().map(|(pi, c)| (*pi, c.len())).collect::<Vec<_>>(),
                    b.iter().map(|(pi, c)| (*pi, c.len())).collect::<Vec<_>>(),
                );
            };
            if a.len() != b.len() {
                bad(&format!("{} matches vs {}", a.len(), b.len()));
            }
            for ((gpi, gcaps), (lpi, lcaps)) in a.iter().zip(b.iter()) {
                if gpi - combined.starts[i] != *lpi {
                    bad(&format!(
                        "pattern index {gpi} - offset {} != {lpi}",
                        combined.starts[i]
                    ));
                }
                if gcaps.len() != lcaps.len() {
                    bad(&format!("{} captures vs {}", gcaps.len(), lcaps.len()));
                }
                let gq = combined.query.capture_quantifiers(*gpi);
                let lq = q.capture_quantifiers(*lpi);
                for ((gix, gnode), (lix, lnode)) in gcaps.iter().zip(lcaps.iter()) {
                    if gnode.id() != lnode.id() {
                        bad("captured a different node");
                    }
                    if names.get(*gix as usize) != local_names.get(*lix as usize) {
                        bad(&format!(
                            "capture name {:?} vs {:?}",
                            names.get(*gix as usize),
                            local_names.get(*lix as usize)
                        ));
                    }
                    if gq.get(*gix as usize) != lq.get(*lix as usize) {
                        bad("capture quantifier differs");
                    }
                }
            }
        }
        crate::util::ghost_stats::count("pd.equiv.file_round_ok");
    }

    /// Run every plugin's declared patterns over the tree and dispatch
    /// matches. Fixed point over trigger gating: emissions can add
    /// package parents / uses that make more gates true, so rounds
    /// repeat until nothing new dispatches. Monotone gate inputs +
    /// per-(plugin, pattern, span) dedup ⇒ termination; the cap is a
    /// debug-only net, mirroring the worklist fold's discipline.
    pub(super) fn dispatch_pattern_plugins(&mut self, root: Node<'a>) {
        if self.plugins.is_empty() {
            return;
        }
        let plugins = self.plugins.clone();
        // Registry order fixes each spec's ordinal, and the ordinal is its
        // bucket. Dispatch still runs spec-by-spec in exactly this order even
        // though the combined traversal yields matches interleaved in tree
        // order — the bucketing is what keeps emission order unchanged.
        let su = crate::util::ghost_stats::ScopedNs::start("pd.setup.eligible");
        let specs = eligible_walk_specs(&plugins);
        drop(su);
        if specs.is_empty() {
            return;
        }
        let sc = crate::util::ghost_stats::ScopedNs::start("pd.setup.combine");
        let combined = combined_walk(&plugins, &specs);
        drop(sc);
        let mut dispatched: HashSet<(String, String, Span)> = HashSet::new();
        let mut rounds_run = 0u64;
        for round in 0..16 {
            debug_assert!(round < 15, "pattern dispatch failed to reach a fixed point");
            rounds_run += 1;
            // Collect matches first: the cursor borrows the tree
            // immutably, the projection pass needs `&mut self`.
            // Text predicates (#eq?, #any-of?, …) are evaluated
            // by the query engine here, since `matches` gets the
            // source text; unknown predicate names pass through
            // unfiltered (the deferred-predicate reservation).
            let rounds_matches = crate::util::ghost_stats::timed("pd.collect", || {
                self.collect_walk_matches(root, &specs, combined.as_ref())
            });
            let mut progressed = false;
            let lr = crate::util::ghost_stats::ScopedNs::start("pd.loop");
            for (ordinal, (p, spec, _)) in specs.iter().enumerate() {
                let (query, collected) = &rounds_matches[ordinal];
                let (p, spec, query) = (*p, *spec, *query);
                // Raw counts recorded on the FIRST round only (later
                // rounds re-run the same query over the same tree);
                // zero-match runs record too so a never-matching
                // pattern shows up at 0 in the stats report.
                if round == 0 {
                    crate::util::timings::record_pattern_matches(
                        p.id(),
                        &spec.name,
                        collected.len(),
                    );
                }
                for (pattern_index, caps) in collected {
                    let pattern_index = *pattern_index;
                    let mspan = union_span(caps);
                    crate::util::ghost_stats::count("pd.match.seen");
                    let key = (p.id().to_string(), spec.name.clone(), mspan);
                    if dispatched.contains(&key) {
                        crate::util::ghost_stats::count("pd.match.dedup_skip");
                        continue;
                    }
                    // The gate runs for EVERY collected match, dispatched or
                    // not, and it is per-match work with per-package inputs:
                    // an O(package_ranges) scan, two owned clones, and an
                    // ancestry walk. Timed as one region because that is the
                    // unit a caller would skip.
                    let g = crate::util::ghost_stats::ScopedNs::start("pd.gate");
                    let pkg = self.package_at_point(mspan.start);
                    let uses = self.package_uses.get(&pkg).cloned().unwrap_or_default();
                    let parents = self.transitive_parents(&pkg);
                    let tq = plugin::TriggerQuery {
                        package_uses: &uses,
                        package_parents: &parents,
                    };
                    let fires = plugin::trigger_fires(p.triggers(), &tq);
                    drop(g);
                    // Trigger didn't fire locally, but a `ClassIsa` gate may
                    // still hold CROSS-FILE (the package has ancestry the
                    // index-free builder can't resolve). Run `on_match` and
                    // DEFER the emission — enrichment re-fires it once the
                    // module index confirms the gate. No parents ⇒ no
                    // cross-file ancestor possible, so nothing to defer.
                    let gate_prefixes = if fires {
                        Vec::new()
                    } else {
                        Self::cross_file_gate_prefixes(p.triggers())
                    };
                    let defer = !fires && !gate_prefixes.is_empty() && !parents.is_empty();
                    if !fires && !defer {
                        crate::util::ghost_stats::count("pd.match.gated_out");
                        continue;
                    }
                    crate::util::ghost_stats::count(if defer {
                        "pd.match.deferred"
                    } else {
                        "pd.match.dispatched"
                    });
                    dispatched.insert(key);
                    progressed = true;
                    crate::util::timings::record_pattern_dispatch(p.id(), &spec.name);
                    // Projections that consult package-relative walk
                    // state (constant folds via the current package,
                    // `__PACKAGE__` receivers) see the match site's
                    // package, exactly as the walk would have.
                    let pkg_for_gate = pkg.clone();
                    let saved =
                        std::mem::replace(&mut self.current_package, Some(pkg.clone()));
                    let c = crate::util::ghost_stats::ScopedNs::start("pd.context");
                    let mctx = self.build_match_context(
                        spec,
                        query,
                        pattern_index,
                        caps,
                        mspan,
                        pkg,
                        uses,
                        parents,
                        None,
                    );
                    drop(c);
                    let actions =
                        crate::util::ghost_stats::timed("pd.on_match", || p.on_match(&spec.name, &mctx));
                    if defer {
                        self.record_gated_pattern_emission(
                            p.id(),
                            gate_prefixes,
                            pkg_for_gate,
                            mspan.start,
                            actions,
                        );
                        self.current_package = saved;
                        continue;
                    }
                    // A #receiver-isa? gate defers DispatchCall
                    // emissions to query time. The build-time
                    // receiver type is a HINT on the candidate
                    // (same role as record_provisional_dispatch's),
                    // never the verdict.
                    let gate = receiver_gate_for(query, pattern_index);
                    let receiver_hint = gate.as_ref().and_then(|g| {
                        let node = caps
                            .iter()
                            .find(|(ix, _)| *ix == g.capture_index)
                            .map(|(_, n)| *n)?;
                        match self.invocant_type_at_node(node) {
                            Some(InferredType::ClassName(c)) => Some(c),
                            _ => None,
                        }
                    });
                    // Emissions attach to the scope AND package open
                    // at the match site — the same context a
                    // walk-time hook emission would have gotten
                    // (apply_emit_action stamps `current_package`
                    // onto symbols, so it must still be the match
                    // site's package here).
                    let e = crate::util::ghost_stats::ScopedNs::start("pd.emit");
                    let match_scope = self.scope_at_point(mspan.start);
                    self.scope_stack.push(match_scope);
                    for a in actions {
                        // A loader's config value must carry an Expr
                        // witness at `config_span` so a cross-file
                        // `expr_type_at_span` (the `$conf` join in
                        // `record_loader_shapes`) resolves its shape.
                        // The captured node lives in `caps`; emit for
                        // it, mirroring the method-form recorder.
                        if let plugin::EmitAction::PluginLoad {
                            config_span: Some(cfg),
                            ..
                        } = &a
                        {
                            if let Some((_, node)) = caps
                                .iter()
                                .find(|(_, n)| node_to_span(*n) == *cfg)
                            {
                                self.emit_expr_witness(*node);
                            }
                        }
                        if let (
                            Some(g),
                            plugin::EmitAction::DispatchCall {
                                name,
                                dispatcher,
                                owner,
                                span,
                                ..
                            },
                        ) = (&gate, &a)
                        {
                            // Receiver-gated dispatch is class-owned by
                            // definition; a Global handler has no receiver
                            // gate, so it takes the ungated emit path below.
                            if let HandlerOwner::Class(owner_class) = owner {
                                self.provisional_dispatches.push(ReceiverGated::new(
                                    g.target_class.clone(),
                                    DispatchCandidate {
                                        name: name.clone(),
                                        span: *span,
                                        dispatcher: dispatcher.clone(),
                                        owner_class: owner_class.clone(),
                                        receiver_class: receiver_hint.clone(),
                                        call_span: mspan,
                                    },
                                ));
                                continue;
                            }
                        }
                        self.apply_emit_action(p.id().to_string(), a);
                    }
                    self.scope_stack.pop();
                    drop(e);
                    self.current_package = saved;
                }
            }
            drop(lr);
            if !progressed {
                break;
            }
        }
        // The fixed-point check re-runs the pattern set over the WHOLE tree —
        // it is a full re-match, not an incremental one. So rounds-per-file is
        // the multiplier on this phase's cost, and the last round is by
        // definition the one that found nothing.
        crate::util::ghost_stats::count_by("build.pattern_rounds", rounds_run);
    }


    /// Fold-phase dispatch: patterns declared `phase: "fold"` run after
    /// the worklist fold's PostFold pass, when chain typing has settled
    /// (route brands, resolved invocants). Differences from the walk
    /// phase, all deliberate:
    ///
    ///   - Matches from ALL fold patterns dispatch in DOCUMENT order,
    ///     because `SetRouteBase` emissions from earlier matches feed
    ///     later matches' `route_defaults` projections.
    ///   - The topic-route base is REPLAYED: the walk recorded group
    ///     scopes (`topic_group_spans`); a base set inside a group
    ///     restores when the replay passes the group's end — the
    ///     group-scoped push/pop semantics of a topic-DSL base.
    ///   - `SetRouteBase` emissions update the replay base instead of
    ///     the (stale) walk stack.
    ///   - Single pass, no gating fixed point: fold emissions don't
    ///     grow trigger inputs today. Revisit if one ever does.
    ///
    /// The deferred `VarType` / named-sub-param flushes ran long before
    /// this phase — fold patterns must not emit those actions.
    pub(super) fn dispatch_pattern_plugins_fold(&mut self, root: Node<'a>) {
        if self.plugins.is_empty() {
            return;
        }
        let plugins = self.plugins.clone();
        type Collected<'p, 'a> = (
            &'p dyn plugin::FrameworkPlugin,
            &'p PatternSpec,
            &'static Query,
            usize,
            Vec<(u32, Node<'a>)>,
            Span,
        );
        let mut collected: Vec<Collected<'_, 'a>> = Vec::new();
        for p in plugins.all() {
            for spec in p.patterns() {
                if spec.language != "perl" || spec.phase != "fold" {
                    continue;
                }
                let query = match cached_pattern_query(&spec.query) {
                    Ok(q) => q,
                    Err(e) => {
                        log::error!(
                            "plugin `{}` pattern `{}`: query compile failed: {}",
                            p.id(),
                            spec.name,
                            e
                        );
                        continue;
                    }
                };
                let mut count = 0usize;
                {
                    let mut cursor = QueryCursor::new();
                    let mut it = cursor.matches(query, root, self.source);
                    while let Some(m) = it.next() {
                        let caps: Vec<(u32, Node<'a>)> =
                            m.captures.iter().map(|c| (c.index, c.node)).collect();
                        if !caps.is_empty() {
                            let span = union_span(&caps);
                            collected.push((p, spec, query, m.pattern_index, caps, span));
                            count += 1;
                        }
                    }
                }
                crate::util::timings::record_pattern_matches(p.id(), &spec.name, count);
            }
        }
        collected.sort_by_key(|(_, _, _, _, _, s)| (s.start.row, s.start.column));

        let groups = self.topic_group_spans.clone();
        let mut gi = 0usize;
        let mut base_stack: Vec<(Span, Option<String>)> = Vec::new();
        let mut current_base: Option<String> = None;
        let mut dispatched: HashSet<(String, String, Span)> = HashSet::new();

        for (p, spec, query, pattern_index, caps, mspan) in collected {
            let point = mspan.start;
            // Leave group frames the replay has passed (inner frames
            // sit on top, so inner-first restore order is automatic).
            while let Some((gspan, _)) = base_stack.last() {
                if (point.row, point.column) > (gspan.end.row, gspan.end.column) {
                    let (_, saved) = base_stack.pop().expect("checked non-empty");
                    current_base = saved;
                } else {
                    break;
                }
            }
            // Enter group frames that contain this match.
            while gi < groups.len() {
                let g = groups[gi];
                if (g.start.row, g.start.column) > (point.row, point.column) {
                    break;
                }
                if crate::model::file_analysis::contains_point(&g, point) {
                    base_stack.push((g, current_base.clone()));
                }
                gi += 1;
            }

            let key = (p.id().to_string(), spec.name.clone(), mspan);
            if dispatched.contains(&key) {
                continue;
            }
            let pkg = self.package_at_point(point);
            let uses = self.package_uses.get(&pkg).cloned().unwrap_or_default();
            let parents = self.transitive_parents(&pkg);
            let tq = plugin::TriggerQuery {
                package_uses: &uses,
                package_parents: &parents,
            };
            let fires = plugin::trigger_fires(p.triggers(), &tq);
            // Cross-file `ClassIsa` deferral, same rule as the walk phase.
            let gate_prefixes = if fires {
                Vec::new()
            } else {
                Self::cross_file_gate_prefixes(p.triggers())
            };
            let defer = !fires && !gate_prefixes.is_empty() && !parents.is_empty();
            if !fires && !defer {
                continue;
            }
            dispatched.insert(key);
            crate::util::timings::record_pattern_dispatch(p.id(), &spec.name);

            let pkg_for_gate = pkg.clone();
            let saved = std::mem::replace(&mut self.current_package, Some(pkg.clone()));
            let mctx = self.build_match_context(
                spec,
                query,
                pattern_index,
                &caps,
                mspan,
                pkg,
                uses,
                parents,
                current_base.as_deref(),
            );
            let actions = p.on_match(&spec.name, &mctx);
            if defer {
                self.record_gated_pattern_emission(
                    p.id(),
                    gate_prefixes,
                    pkg_for_gate,
                    mspan.start,
                    actions,
                );
                self.current_package = saved;
                continue;
            }
            let gate = receiver_gate_for(query, pattern_index);
            let receiver_hint = gate.as_ref().and_then(|g| {
                let node = caps
                    .iter()
                    .find(|(ix, _)| *ix == g.capture_index)
                    .map(|(_, n)| *n)?;
                match self.invocant_type_at_node(node) {
                    Some(InferredType::ClassName(c)) => Some(c),
                    _ => None,
                }
            });
            let match_scope = self.scope_at_point(mspan.start);
            self.scope_stack.push(match_scope);
            for a in actions {
                // Same loader-config witness rule as the walk phase: a
                // PluginLoad's config value must carry an Expr witness at
                // `config_span` or `record_loader_shapes`' cross-file join
                // silently loses the shape — the phases must not diverge.
                if let plugin::EmitAction::PluginLoad {
                    config_span: Some(cfg),
                    ..
                } = &a
                {
                    if let Some((_, node)) =
                        caps.iter().find(|(_, n)| node_to_span(*n) == *cfg)
                    {
                        self.emit_expr_witness(*node);
                    }
                }
                if let plugin::EmitAction::SetRouteBase { controller } = &a {
                    current_base = Some(controller.clone());
                    continue;
                }
                if let (
                    Some(g),
                    plugin::EmitAction::DispatchCall {
                        name,
                        dispatcher,
                        owner,
                        span,
                        ..
                    },
                ) = (&gate, &a)
                {
                    // Global handlers carry no receiver gate — ungated path.
                    if let HandlerOwner::Class(owner_class) = owner {
                        self.provisional_dispatches.push(ReceiverGated::new(
                            g.target_class.clone(),
                            DispatchCandidate {
                                name: name.clone(),
                                span: *span,
                                dispatcher: dispatcher.clone(),
                                owner_class: owner_class.clone(),
                                receiver_class: receiver_hint.clone(),
                                call_span: mspan,
                            },
                        ));
                        continue;
                    }
                }
                self.apply_emit_action(p.id().to_string(), a);
            }
            self.scope_stack.pop();
            self.current_package = saved;
        }
    }

    /// A pattern matched syntactically but its `ClassIsa` trigger did NOT
    /// fire against LOCAL ancestry (rule #1: the builder is index-free, so
    /// `transitive_parents` sees only in-file parents). The match may still
    /// belong to the framework via a CROSS-FILE ancestor — the DBIC result
    /// class reaching `DBIx::Class` through an intermediate base in another
    /// file. Record the already-computed `on_match` output, translated to
    /// file-analysis-native symbols/refs, as a [`GatedEmission`] the
    /// enrichment pass re-fires once the module index can confirm the gate
    /// (`class_isa_prefix`). Trigger semantics are OR, and only `ClassIsa`
    /// triggers can newly-fire cross-file — `gate_prefixes` is exactly that
    /// subset of the plugin's triggers.
    ///
    /// Symbol-emitting actions (`Method`/`HashKeyDef`/`Handler`/`Symbol`) and
    /// the reference actions that link call sites to them
    /// (`DispatchCall`/`HashKeyAccess`) are captured; other kinds under a
    /// deferred gate are logged and skipped (out of scope — none are emitted
    /// by the bundled `ClassIsa` plugins on this path).
    fn record_gated_pattern_emission(
        &mut self,
        plugin_id: &str,
        gate_prefixes: Vec<String>,
        package: String,
        scope_point: tree_sitter::Point,
        actions: Vec<plugin::EmitAction>,
    ) {
        use crate::model::file_analysis::{
            GatedEmission, GatedRef, GatedSymbol, RefKind, SymKind, SymbolDetail,
        };
        use plugin::EmitAction;
        let mut symbols: Vec<GatedSymbol> = Vec::new();
        let mut refs: Vec<GatedRef> = Vec::new();
        for a in actions {
            match a {
                EmitAction::Method {
                    name, span, selection_span, params, is_method, return_type, doc,
                    on_class, display, hide_in_outline, opaque_return, outline_label, ..
                } => {
                    symbols.push(GatedSymbol {
                        name,
                        kind: SymKind::Method,
                        span,
                        selection_span,
                        detail: SymbolDetail::Sub {
                            params: params.into_iter().map(Into::into).collect(),
                            is_method,
                            doc,
                            opaque_return,
                            is_constant: false,
                            lexical: false,
                        },
                        presentation: crate::model::file_analysis::Presentation {
                            hide_in_outline,
                            display,
                            label: outline_label,
                        },
                        on_class,
                        return_type,
                    });
                }
                EmitAction::HashKeyDef { name, owner, span, selection_span } => {
                    symbols.push(GatedSymbol {
                        name,
                        kind: SymKind::HashKeyDef,
                        span,
                        selection_span,
                        detail: SymbolDetail::HashKeyDef { owner, is_dynamic: false },
                        presentation: Default::default(),
                        on_class: None,
                        return_type: None,
                    });
                }
                EmitAction::Handler {
                    name, owner, dispatchers, params, span, selection_span,
                    display, hide_in_outline, outline_label,
                } => {
                    symbols.push(GatedSymbol {
                        name,
                        kind: SymKind::Handler,
                        span,
                        selection_span,
                        detail: SymbolDetail::Handler {
                            owner,
                            dispatchers,
                            params: params.into_iter().map(Into::into).collect(),
                        },
                        presentation: crate::model::file_analysis::Presentation {
                            hide_in_outline,
                            display: Some(display),
                            label: outline_label,
                        },
                        on_class: None,
                        return_type: None,
                    });
                }
                EmitAction::Symbol {
                    name, kind, span, selection_span, detail, return_type,
                    display, hide_in_outline,
                } => {
                    symbols.push(GatedSymbol {
                        name,
                        kind,
                        span,
                        selection_span,
                        detail,
                        presentation: crate::model::file_analysis::Presentation {
                            hide_in_outline,
                            display,
                            label: None,
                        },
                        on_class: None,
                        return_type,
                    });
                }
                EmitAction::DispatchCall { name, dispatcher, owner, span, .. } => {
                    refs.push(GatedRef {
                        kind: RefKind::DispatchCall { dispatcher },
                        span,
                        target_name: name,
                        access: crate::model::file_analysis::AccessKind::Read,
                        binding: Some(RefBinding::Handler { owner, sym: None }),
                    });
                }
                EmitAction::HashKeyAccess { name, owner, var_text, span, access } => {
                    refs.push(GatedRef {
                        kind: RefKind::HashKeyAccess { var_text },
                        span,
                        target_name: name,
                        access,
                        binding: Some(RefBinding::HashKey { owner, sym: None }),
                    });
                }
                other => {
                    log::debug!(
                        "plugin `{}`: deferred cross-file ClassIsa emission drops \
                         unsupported action {:?}",
                        plugin_id,
                        std::mem::discriminant(&other),
                    );
                }
            }
        }
        if symbols.is_empty() && refs.is_empty() {
            return;
        }
        self.gated_emissions.push(GatedEmission {
            gate_prefixes,
            package,
            scope_point,
            plugin_id: plugin_id.to_string(),
            symbols,
            refs,
        });
    }

    /// The `ClassIsa` trigger prefixes of `triggers` — the only trigger
    /// shape that can newly-fire once cross-file ancestry is known (a
    /// `UsesModule` / `Always` verdict is settled locally at build).
    fn cross_file_gate_prefixes(triggers: &[plugin::Trigger]) -> Vec<String> {
        triggers
            .iter()
            .filter_map(|t| match t {
                plugin::Trigger::ClassIsa(prefix) => Some(prefix.clone()),
                _ => None,
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn build_match_context(
        &mut self,
        spec: &PatternSpec,
        query: &Query,
        pattern_index: usize,
        caps: &[(u32, Node<'a>)],
        span: Span,
        package: String,
        package_uses: Vec<String>,
        package_parents: Vec<String>,
        topic_base: Option<&str>,
    ) -> MatchContext {
        let names = query.capture_names();
        let quants = query.capture_quantifiers(pattern_index);
        // Group nodes per capture index, preserving first-seen order.
        let mut order: Vec<u32> = Vec::new();
        let mut grouped: HashMap<u32, Vec<Node<'a>>> = HashMap::new();
        for (idx, node) in caps {
            if !grouped.contains_key(idx) {
                order.push(*idx);
            }
            grouped.entry(*idx).or_default().push(*node);
        }
        let mut captures = HashMap::new();
        for idx in order {
            let nodes = &grouped[&idx];
            let Some(name) = names.get(idx as usize) else {
                continue;
            };
            let projections: &[String] = spec
                .projections
                .get(*name)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let datas: Vec<CaptureData> = nodes
                .iter()
                .map(|n| self.project_capture(*n, projections, topic_base))
                .collect();
            let many = matches!(
                quants.get(idx as usize),
                Some(CaptureQuantifier::ZeroOrMore) | Some(CaptureQuantifier::OneOrMore)
            );
            let value = if many {
                CaptureValue::Many(datas)
            } else {
                // Scalar position: last node wins (there is normally
                // exactly one). An optional capture that didn't match
                // simply isn't present in the map — Rhai reads `()`.
                match datas.into_iter().next_back() {
                    Some(d) => CaptureValue::One(Box::new(d)),
                    None => continue,
                }
            };
            captures.insert((*name).to_string(), value);
        }
        MatchContext {
            pattern: spec.name.clone(),
            span,
            package: Some(package),
            package_parents,
            package_uses,
            captures,
        }
    }

    /// Compute the declared projections for one captured node. `text`
    /// and `span` are free and always present; everything else routes
    /// through the SAME extractors the emit-hook pre-capture uses
    /// (`arg_info_for`, `invocant_type_at_node`) — laziness comes from
    /// only being here for actual matches.
    fn project_capture(
        &mut self,
        node: Node<'a>,
        projections: &[String],
        topic_base: Option<&str>,
    ) -> CaptureData {
        let wants = |k: &str| projections.iter().any(|p| p == k);
        let mut data = CaptureData {
            text: node.utf8_text(self.source).unwrap_or("").to_string(),
            span: node_to_span(node),
            string_value: None,
            string_values: Vec::new(),
            content_span: None,
            inferred_type: None,
            value_shape: None,
            sub_params: Vec::new(),
            callable_return_edge: None,
            list: Vec::new(),
            is_package_receiver: None,
            args: Vec::new(),
            isa: None,
            ref_sub_name: None,
            call_name: None,
            route_defaults: Vec::new(),
        };
        if wants("str")
            || wants("strs")
            || wants("content_span")
            || wants("sub_params")
            || wants("callable_edge")
            || wants("shape")
            || wants("ref_sub_name")
        {
            let ai = self.arg_info_for(node);
            if wants("str") {
                data.string_value = ai.string_value;
            }
            if wants("strs") {
                data.string_values = ai.string_values;
            }
            if wants("content_span") {
                data.content_span = ai.content_span;
            }
            if wants("sub_params") {
                data.sub_params = ai.sub_params;
            }
            if wants("callable_edge") {
                data.callable_return_edge = ai.callable_return_edge;
            }
            if wants("shape") {
                data.value_shape = Some(ai.value_shape);
            }
            if wants("ref_sub_name") {
                data.ref_sub_name = ai.ref_sub_name;
            }
        }
        if wants("ty") {
            data.inferred_type = self.invocant_type_at_node(node);
        }
        if wants("list") {
            data.list = self.extract_arg_name_list(node);
        }
        if wants("args") {
            let flat = self.flat_call_args(vec![node]);
            data.args = flat.iter().map(|n| self.arg_info_for(*n)).collect();
        }
        if wants("isa") {
            data.isa = self.isa_type_in_option_tail(node);
        }
        if wants("call_name") {
            data.call_name = self.invocant_call_name(node);
        }
        if wants("route_defaults") {
            // Same flattening as the legacy CallContext fill: the
            // fold-settled brand's stash + controller, then — for a
            // topic-DSL verb CALL receiver still missing a controller
            // — the replayed topic base (`under(...)->to('ctrl#')`'s
            // SetRouteBase, scoped by group frames).
            let mut defaults: Vec<(String, String)> = Vec::new();
            if let Some(InferredType::BrandedRoute { controller, stash, .. }) =
                self.invocant_type_at_node(node)
            {
                defaults = stash;
                if let Some(c) = controller {
                    defaults.push(("controller".to_string(), c));
                }
            }
            if defaults.iter().all(|(k, _)| k != "controller") {
                if let (Some(dsl), Some(callee)) =
                    (self.active_topic_dsl(), self.invocant_call_name(node))
                {
                    if dsl.verbs.iter().any(|v| *v == callee) {
                        if let Some(c) = topic_base {
                            defaults.push(("controller".to_string(), c.to_string()));
                        }
                    }
                }
            }
            data.route_defaults = defaults;
        }
        if wants("is_package_receiver") {
            // Same rule as the emit-hook path's `is_pkg_call`:
            // `__PACKAGE__` (any spelling conventions certifies) or a
            // bareword naming the match site's own package.
            let is_pkg = crate::model::conventions::is_current_package_token(&data.text)
                || (node.kind() == "package"
                    && Some(data.text.as_str()) == self.current_package.as_deref());
            data.is_package_receiver = Some(is_pkg);
        }
        data
    }
}

