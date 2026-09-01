//! `ReducerRegistry`: registration order, the recursive edge-chasing
//! query (`query_rec`), and its cycle-guard/memo state.

use super::*;

// ---- Reducer registry ----

/// Cycle guard + result memo key for recursive bag queries, keyed by
/// `(bag_ptr, attachment, receiver_identity, arity_hint)`. Per-bag
/// entries stay separate so a legitimate cross-bag query for the same
/// attachment (the common `PackageSymbol{C, m}` jump into C's own bag)
/// isn't misread as a cycle. The receiver **identity** + arity hint
/// widen the key so two queries differing only in `receiver` /
/// `arity_hint` aren't treated as duplicates — `UnionOnArgs` and
/// `Receiver` substitution can produce different answers.
///
/// The receiver slot is the receiver's FULL structural identity, not a
/// variant tag. `ReturnExpr::Receiver` substitutes the whole receiver,
/// so `ClassName("Foo")` and `ClassName("Bar")` reaching one attachment
/// resolve to different classes; a variant-only discriminant collapses
/// them to one memo key and the memo hands Foo's answer to Bar (silent
/// wrong type). A same-receiver diamond (the inheritance walk holds
/// `q.receiver` constant within one `PackageSymbol` query) still hashes
/// to one key, so memoization still kills the exponential re-chase.
type VisitedKey = (usize, WitnessAttachment, Option<String>, Option<u32>);
type VisitedSet = std::collections::HashSet<VisitedKey>;

/// Per-top-level-`query` traversal state: the cycle guard plus a result
/// memo. The bag forms a DAG of edges; without memoization a diamond
/// (two paths reaching one shared sub-attachment) re-chases the shared
/// subtree on every path, which is exponential on dense files
/// (SQL::Abstract's method graph took minutes). The memo caches each
/// attachment's resolved value *for the duration of one top-level query*
/// so a re-reached node returns in O(1).
///
/// Soundness vs the cycle guard: `query_rec` only consults/stores the
/// memo for a key that is NOT currently on the path (the visited-guard
/// has already returned for on-path keys). A cached value is therefore
/// the node's resolution computed with that node off the path — exactly
/// what any other off-path reentry would compute. The memo is dropped
/// when the top-level query returns, so it never leaks state across
/// queries whose context (scopes / module_index / framework) differs.
/// Record a would-be consult, OUT OF LINE.
///
/// `#[inline(never)]` is load-bearing rather than a hint. `query_rec` recurses
/// once per MRO hop and the depth cap is tuned against a 2 MiB stack at 600
/// hops; building a `ConclusionKey` inline grew that frame enough to overflow
/// before the cap could fire. Temporaries belong in a callee's frame, not in
/// one that is live 600 deep.
#[inline(never)]
fn note_moc_exit(state: &mut QueryState, class: &str, name: &str) {
    state.note_exit(Some(super::ConclusionKey::MethodOnClass {
        class: class.to_string(),
        name: name.to_string(),
    }));
}

#[inline(never)]
fn note_type_name_exit(state: &mut QueryState, name: &str) {
    state.note_exit(Some(super::ConclusionKey::TypeName(name.to_string())));
}

#[inline(never)]
fn note_slot_exit(state: &mut QueryState, class: &str, key: &str) {
    state.note_exit(Some(super::ConclusionKey::SlotType {
        class: class.to_string(),
        key: key.to_string(),
    }));
}

#[inline(never)]
fn note_parent_rungs(state: &mut QueryState, parents: &[String], name: &str) {
    for p in parents {
        state.note_exit(Some(super::ConclusionKey::MethodOnClass {
            class: p.clone(),
            name: name.to_string(),
        }));
    }
}

thread_local! {
    /// Residuals + poison from the most recent top-level `query`.
    ///
    /// A thread-local rather than a return value because `query` returns a
    /// `ReducedValue` to a hundred call sites, and only the bake asks this
    /// question. Published unconditionally and read only by the bake, which is
    /// single-threaded per file.
    static LAST_RESIDUAL: std::cell::RefCell<(bool, Vec<super::ConclusionKey>)> =
        const { std::cell::RefCell::new((false, Vec::new())) };
}

pub(super) struct QueryState {
    visited: VisitedSet,
    /// Enriched copies consulted during this query — pinned so memo
    /// entries keyed on their bag ADDRESSES stay valid even if the
    /// overlay's eviction drops its own reference mid-query.
    pins: Vec<std::sync::Arc<crate::model::file_analysis::FileAnalysis>>,
    // `Arc` so a memo store/hit clones one heap pointer, not the
    // (String-bearing) `ReducedValue`. `HashMap::new()` pre-allocates
    // no buckets, so a shallow query that never re-reaches a node (the
    // common hover/completion 1–2-hop case) pays nothing for the memo —
    // the table is lazily allocated on the first insert.
    /// The flag beside each value is "computing this subtree recorded an exit".
    /// A memo hit skips the subtree, and with it the `note_exit` calls that
    /// would have poisoned a re-entry from inside a combining frame — so the
    /// fact has to be carried on the entry instead of re-derived.
    memo: std::collections::HashMap<VisitedKey, (std::sync::Arc<ReducedValue>, bool)>,
    /// Where a BAKE's chase would have consulted the index, in the order the
    /// ladder reached them.
    ///
    /// Order is the semantics: Perl's DFS-MRO is an ordered ladder, so the
    /// residuals of an un-poisoned `None` are first-answer-wins candidates and
    /// become a `Link`'s `targets` verbatim.
    pub(super) residual: Vec<super::ConclusionKey>,
    /// The chase reached a point it cannot represent as a portable key, so no
    /// `Link` may be minted from it.
    ///
    /// Poison is one-way and never cleared. A chase that combined frames — an
    /// arm fold, a `materialize` splice into a populated witness list — has an
    /// answer that is not "whichever ladder rung answers first", and a `Link`
    /// can only express the latter.
    pub(super) poisoned: bool,
    /// How many COMBINING frames the chase is currently inside.
    ///
    /// A `Link` says "the answer is the first of these keys that answers". That
    /// is only true of the top-level query if every frame between it and the
    /// exit is transparent — the exit's answer is returned unchanged. A frame
    /// that folds the sub-chase's answer together with sibling witnesses, drills
    /// through it, or re-dispatches it under a different receiver or arity
    /// returns something the exit key alone does not name, so a residual
    /// recorded beneath one is not a rung and poisons instead of being kept.
    ///
    /// A counter rather than a flag because the frames nest, and the recording
    /// site is the innermost one — it has to know whether ANY ancestor is
    /// opaque, not whether its immediate parent is.
    opaque_frames: u32,
}

impl QueryState {
    pub(super) fn new() -> Self {
        QueryState {
            visited: std::collections::HashSet::new(),
            pins: Vec::new(),
            memo: std::collections::HashMap::new(),
            residual: Vec::new(),
            poisoned: false,
            opaque_frames: 0,
        }
    }

    /// Record where the chase would have consulted the index.
    ///
    /// `None` means the exit cannot be named portably — a per-file `SymbolId`,
    /// an `Expr(span)` — so the whole chase is poisoned. Naming the would-be
    /// key at every consult site is what keeps a future site from silently
    /// bypassing residualization: an unrecorded exit plus trusted absence is a
    /// silent wrong `None`.
    pub(super) fn note_exit(&mut self, would_ask: Option<super::ConclusionKey>) {
        match would_ask {
            // Nameable, but reached through a frame that will transform it.
            // Dropping the residual silently would be worse than poisoning:
            // the surviving rungs would then describe a ladder the answer never
            // actually took, and a `Link` minted from them answers where the
            // chase does not.
            Some(_) if self.opaque_frames > 0 => {
                crate::util::ghost_stats::count("residual.under_opaque");
                self.poisoned = true;
            }
            Some(k) => self.residual.push(k),
            None => self.poisoned = true,
        }
    }

    /// The chase combined frames rather than taking the first answering rung,
    /// so its result is not expressible as an ordered `Link`.
    #[allow(dead_code)]
    pub(super) fn poison(&mut self) {
        self.poisoned = true;
    }

    /// Run `f` with the chase marked as being inside a combining frame.
    ///
    /// Paired via a closure rather than enter/leave calls because every early
    /// return inside `materialize`'s arms would otherwise have to remember to
    /// decrement, and a missed decrement does not fail — it silently poisons
    /// the rest of the chase, which reads as "the layer just does not mint
    /// Links here" and is the hardest kind of bug to see.
    ///
    pub(super) fn in_opaque_frame<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> T {
        self.opaque_frames += 1;
        let out = f(self);
        self.opaque_frames -= 1;
        out
    }
}

/// Hashable full-identity projection of `q.receiver` for the cycle/memo
/// key. `None` stays `None`; otherwise the receiver's complete structural
/// identity (Debug projection) so two distinct receivers — including two
/// `ClassName(_)` with different class names — never share a key. This is
/// the soundness-load-bearing slot: `ReturnExpr::Receiver` substitutes the
/// whole receiver, so the memo must keep different receivers apart. Debug
/// is structurally faithful for every `InferredType` variant (each field
/// is itself `Debug`), so equality of the string implies equality of the
/// receiver for keying purposes.
fn consult_prefilter_equiv() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("PERL_LSP_CONSULT_PREFILTER_EQUIV").is_ok())
}

/// May candidate `cached` contribute ANY answer to a class-keyed consult
/// (`PackageSymbol{class, name}` when `attributed`, `SlotType{class, key}`
/// otherwise)? The registry-sweep face of the rows-backed pre-filter
/// (`docs/prompt-relational-iteration.md`) for the many-provider shape a
/// package-`main` monoculture mints — where a no-answer sweep walks every
/// declaring file to learn nothing.
///
/// The rows can only speak for the candidate's BAG; the chase has three
/// candidate-LOCAL routes that answer without any bag witness for the
/// name, each gated here from the never-evicted lanes:
///   * the attempt walks the candidate's own `declared_parents` (and its
///     dynamic-parents marker — undecidable, so it always fails open);
///   * the synthetic app-surface edge, keyed off the candidate's
///     `app_surface_consumers`;
///   * declarative attachment names with no backing row — parametric
///     method declarations, plugin `Method` overrides, bridged entities
///     under a foreign container — carried per file as
///     `unrowed_attachment_names`, derived generically from the final bag
///     so no push site can bypass it.
/// Everything else an attempt can relay (the re-entrant sweep, the
/// idx-wide `parents_cached` union, the global bridge enumeration) is
/// candidate-INDEPENDENT: the consumer's own chase arms reach it whether
/// or not this candidate is skipped.
///
/// Fail-open everywhere any input cannot speak; ships with
/// `PERL_LSP_NO_CONSULT_PREFILTER` (disable, in the rows probe) and
/// `PERL_LSP_CONSULT_PREFILTER_EQUIV` (run the skipped attempt anyway and
/// scream on divergence).
pub(super) fn sweep_candidate_may_answer(
    idx: &dyn crate::model::file_analysis::CrossFileLookup,
    cached: &std::sync::Arc<crate::model::file_analysis::CachedModule>,
    class: &str,
    name: &str,
    attributed: bool,
) -> bool {
    let fa = &cached.analysis;
    if !fa.declared_parents(class).is_empty() {
        return true;
    }
    if fa.has_dynamic_parents(class) {
        return true;
    }
    if fa.plugin.app_surface_consumers.iter().any(|c| c == class) {
        return true;
    }
    if fa
        .unrowed_attachment_names
        .binary_search_by(|n| n.as_str().cmp(name))
        .is_ok()
    {
        return true;
    }
    idx.candidate_bag_may_answer(cached, name, class, attributed)
}

fn receiver_key(r: &Option<InferredType>) -> Option<String> {
    r.as_ref().map(|t| format!("{t:?}"))
}

/// Receiver to substitute when a chase reaches a *fresh* method dispatch
/// on `PackageSymbol{package}` (an `Edge` or `CallReturn` into a class's
/// method): the receiver is that call's invocant, i.e. `class`. A fluent
/// `ReturnExpr(Receiver)` substitutes the dispatch class.
///
/// But when the outer query already carries the invocant's *resolved
/// value* and that value's class identity IS `class`, prefer the richer
/// value — it carries parametric structure (`Parametric(ResultSet{base,
/// row})`) that a bare `ClassName(class)` drops, which is exactly what
/// `Operator(RowOf(Receiver))` (DBIC `find`) needs to project the row
/// class. Same class, strictly more information; the value answers the
/// projection (rule #10), the chase never inspects the shape.
fn fresh_dispatch_receiver(
    incoming: &Option<InferredType>,
    class: &str,
    ctx: Option<&BagContext>,
) -> Option<InferredType> {
    if let Some(t) = incoming {
        if let Some(cn) = t.class_name() {
            // Preserve a receiver that IS the dispatch class — or a SUBCLASS of
            // it (SUPER:: dispatch, inherited methods): more specific, still valid.
            if cn == class || ctx.is_some_and(|c| is_subclass_of(cn, class, c)) {
                return Some(t.clone());
            }
        }
    }
    Some(InferredType::ClassName(class.to_string()))
}

/// Is `child` a (transitive) subclass of `ancestor`? Bounded BFS over `parents_of`.
fn is_subclass_of(child: &str, ancestor: &str, ctx: &BagContext) -> bool {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut queue: std::collections::VecDeque<String> =
        std::collections::VecDeque::from([child.to_string()]);
    let mut steps = 0;
    while let Some(c) = queue.pop_front() {
        steps += 1;
        if steps > 64 {
            break;
        }
        if !seen.insert(c.clone()) {
            continue;
        }
        for p in crate::model::file_analysis::parents_of(
            &c,
            ctx.package_parents,
            ctx.module_index,
            ctx.app_surface_consumers,
        ) {
            if p == ancestor {
                return true;
            }
            if !seen.contains(&p) {
                queue.push_back(p);
            }
        }
    }
    false
}

/// Depth backstop for `query_rec`. The `(bag, attachment)` visited set is
/// the primary cycle guard; this cap is belt-and-braces against a new,
/// unaccounted-for recursion shape blowing the stack. On hit, warn once
/// per process and return `None` (give up cleanly rather than abort).
///
/// **It fires in production** (Tier 2 of the scale hitlist, seen again in
/// the row-#3 probe), so treat it as a live degradation path, not a
/// should-never-happen. Every hit is counted as `query_rec.depth_cap`
/// under `PERL_LSP_GHOST_STATS` — the one-shot warning says it happened,
/// the counter says how often, and only the second one can tell a rare
/// pathological file from a systematic truncation.
///
/// Known interaction, unfixed and now MEASURED rather than guessed at: a
/// subtree truncated here still gets MEMOIZED by the caller. `VisitedKey` is
/// `(bag, attachment, receiver, arity)` — depth is not in it — so a node
/// first reached near the cap caches its truncated answer and a later,
/// shallower consult reads that instead of re-deriving the full one. Which
/// nodes lose depends on traversal order.
///
/// Two facts bound how much this matters, both worth knowing before anyone
/// "fixes" it:
///
/// 1. **It cannot outlive one top-level query.** `QueryState` — memo included
///    — is minted in `query` and dropped when it returns. So this is not a
///    cache that poisons a session; the window is a single query's traversal.
/// 2. **Guarding it is expensive and, so far, buys nothing observable.** A
///    prototype tagged each entry with the depth that produced it and refused
///    to serve a truncated entry to a shallower consult. On a synthetic
///    diamond (a 400-hop branch and a 2-hop branch meeting at a node whose
///    own tail crosses the cap) it rejected 80,200 entries and re-derived
///    them — 5.6x the wall time, 7s to 39s — and the top-level answer was
///    IDENTICAL with and without it. The mechanism fires constantly; a shape
///    where it changes what a user sees was not found.
///
/// So the cost is confirmed real and the benefit is still unevidenced. A fix
/// wants a corpus case where the served answer actually differs — not a
/// reproduction of the mechanism, which is easy and proves nothing.
///
/// **Profile-aware, because the stack ceiling is.** Measured on a 2 MiB stack
/// (the tokio blocking-pool and rayon worker size) with an `@ISA` chain of N
/// packages, one `query_rec` level per hop:
///
/// | build | deepest chain that answers | at 512 |
/// |---|---|---|
/// | release | ≥2,000 (cap fires first) | cap fires, answer degrades to `None` |
/// | debug | 400 | **stack overflow — the process aborts** |
///
/// So a single value cannot serve both: 512 is under the release ceiling and
/// over the debug one, and a debug abort is a `cargo test` that dies rather
/// than fails. Release keeps 512 — this changes no shipped answer — and debug
/// drops to a value with margin under its own measured ceiling.
#[cfg(not(debug_assertions))]
const QUERY_REC_DEPTH_CAP: u32 = 512;
#[cfg(debug_assertions)]
const QUERY_REC_DEPTH_CAP: u32 = 256;

thread_local! {
    /// Set when an edge chase reads an `Expr(span)` attachment — the marker
    /// for "this answer needed the raw derivation, not just a conclusion".
    static TOUCHED_EXPR: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

thread_local! {
    static QUERY_REC_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// One-shot so we don't flood stderr while a deep walk unwinds.
    static QUERY_REC_DEPTH_WARNED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// How many times the cap has fired on this thread. Read as a DIFFERENCE
    /// across a region — an absolute count would be sticky for the rest of the
    /// process once any chase anywhere truncated.
    static QUERY_REC_TRUNCATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[derive(Default)]
pub struct ReducerRegistry {
    reducers: Vec<Box<dyn WitnessReducer>>,
}

impl ReducerRegistry {
    pub fn new() -> Self {
        Self { reducers: Vec::new() }
    }

    pub fn with_defaults() -> Self {
        let mut r = Self::new();
        // Order is load-bearing — earlier reducers claim first.
        // Plugin overrides short-circuit before any inferred fold.
        r.register(Box::new(PluginOverrideReducer));
        // ReturnExpr is symbol-declarative — before every value-side
        // reducer so a sub's declared shape (Mojo `has`'s UnionOnArgs,
        // DBIC `find`'s Operator(RowOf, Receiver)) wins over per-arity
        // observations or primary-sym writeback.
        r.register(Box::new(ReturnExprReducer));
        // SymbolReturnArmFold claims the dedicated `SymbolReturnArm(_)`
        // shape; single-arm answers surface here, where BranchArmFold's
        // ≥2-arm rule would reject them.
        r.register(Box::new(SymbolReturnArmFold));
        // SlotTypeFold claims the dedicated `SlotType{..}` shape. Nothing
        // consumes it yet (typed `$obj->{k}` resolution is a later step),
        // so placement here is non-load-bearing — grouped with the other
        // arm-agreement folds for legibility.
        r.register(Box::new(SlotTypeFold));
        // BranchArmFold claims the dedicated `BranchArm(_)` shape — no
        // overlap with the Variable/Expr folds below, so order here is
        // not load-bearing.
        r.register(Box::new(BranchArmFold));
        r.register(Box::new(FrameworkAwareTypeFold));
        r.register(Box::new(ExprReturn));
        // PackageSymbol primary-fallback after ReturnExprReducer so
        // per-arity declarations win when one matches.
        r.register(Box::new(PackageSymbolReducer));
        // TypeName is a disjoint attachment shape (typedef/using aliases),
        // so order isn't load-bearing — grouped with the other class-keyed
        // fallbacks. The `ClassName(name)` terminal lives in query_rec_body.
        r.register(Box::new(TypeNameReducer));
        // DomainCoherenceFold claims the disjoint `Field{..}` shape (the
        // int-used-as-enum domain vote) — no overlap with any flow-axis
        // reducer, so order isn't load-bearing.
        r.register(Box::new(DomainCoherenceFold));
        // Last — fallback for "this Symbol's stored return type".
        r.register(Box::new(SubReturnReducer));
        r
    }

    pub fn register(&mut self, r: Box<dyn WitnessReducer>) {
        self.reducers.push(r);
    }

    /// Query the registry for the first reducer returning a non-`None`
    /// value. Edge materialization runs first: `Edge(target)` witnesses
    /// on the queried attachment are chased via recursive query and
    /// replaced by synthetic `InferredType` witnesses (preserving source
    /// + span) before reducers see the list, so edges compose with
    /// existing reducers without reducer-side awareness.
    ///
    /// The cycle guard is threaded across both edge chases (within one
    /// bag) and the inheritance fallback (which crosses bags), closing
    /// mutual-inheritance loops that span files.
    /// The residuals of the most recent `query`, if that chase can be
    /// expressed as an ordered `Link`.
    ///
    /// `None` when the chase was poisoned, when it recorded nothing, or when
    /// it is not a bake — a live query's exits are not residualization
    /// candidates, and counting them would mint `Link`s from ordinary degraded
    /// lookups.
    pub fn residuals_of_last_query(&self) -> Option<Vec<super::ConclusionKey>> {
        LAST_RESIDUAL.with(|r| {
            let (poisoned, keys) = &*r.borrow();
            if *poisoned || keys.is_empty() {
                return None;
            }
            // Order preserved, duplicates dropped: the same parent can be
            // reached by two candidate files, and a repeated rung would make
            // the walk redo work rather than change its answer.
            let mut seen = std::collections::HashSet::new();
            let out: Vec<_> = keys
                .iter()
                .filter(|k| seen.insert((*k).clone()))
                .cloned()
                .collect();
            Some(out)
        })
    }

    pub fn query(&self, bag: &WitnessBag, q: &ReducerQuery) -> ReducedValue {
        let mut state = QueryState::new();
        // Is the CONCLUSION LAYER CLOSED for the shape that dominates
        // cross-file traffic? A `PackageSymbol` answer that never reads an
        // `Expr(span)` witness could be served from stored conclusions; one
        // that does needs the raw derivation, so the bag has to come along.
        // Measured at the top-level query only — inner hops are the thing
        // being counted, not separate questions.
        let top_moc = matches!(q.attachment, WitnessAttachment::PackageSymbol { .. });
        if top_moc {
            // Attribution: what share of top-level moc queries are main-keyed?
            // Two buckets, not per-package keys — a script-heavy corpus mints
            // these from every plain call, and the question a fix must answer
            // is main's SHARE, not a cardinality-unbounded census.
            if crate::util::ghost_stats::enabled() {
                if let WitnessAttachment::PackageSymbol { package, .. } = &q.attachment {
                    crate::util::ghost_stats::count(if package == "main" {
                        "mocpkg.main"
                    } else {
                        "mocpkg.other"
                    });
                }
            }
            TOUCHED_EXPR.with(|c| c.set(false));
        }
        // Sole boundary where an owned `ReducedValue` is required; the
        // internal recursion threads `Arc` to avoid deep clones per hop.
        let out = (*self.query_rec(bag, q, &mut state)).clone();
        // Publish for the bake. Cleared-and-set on every query, so a later
        // query can never be minted from an earlier chase's exits.
        LAST_RESIDUAL.with(|r| {
            *r.borrow_mut() = (state.poisoned, std::mem::take(&mut state.residual));
        });
        if top_moc {
            crate::util::ghost_stats::count(if TOUCHED_EXPR.with(|c| c.get()) {
                "moc.touched_expr"
            } else {
                "moc.conclusions_only"
            });
        }
        out
    }

    /// Returns an `Arc` so the memo, the cycle-guard early-outs, and the
    /// edge-chase recursion all share one heap allocation per resolved
    /// node instead of deep-cloning a (String-bearing) `ReducedValue` on
    /// every store, hit, and return.
    pub(super) fn query_rec(
        &self,
        bag: &WitnessBag,
        q: &ReducerQuery,
        state: &mut QueryState,
    ) -> std::sync::Arc<ReducedValue> {
        // The chase has landed on a raw-derivation attachment. Whatever the
        // top-level question was, its answer now depends on the bag's
        // observations rather than on any conclusion we could have stored.
        // Closure test proper: what does the chase read at EVERY attachment
        // it enters, not just the `Expr` ones. An `Edge` is only expressible
        // as a conclusion if what it points at is too, transitively — so an
        // `Observation` anywhere in the walk is what would make the layer
        // genuinely open.
        for w in bag.for_attachment(&q.attachment) {
            crate::util::ghost_stats::count(match &w.payload {
                WitnessPayload::Observation(_) => "hop.OBSERVATION",
                WitnessPayload::InferredType(_) => "hop.inferred_type",
                WitnessPayload::Edge(_) => "hop.edge",
                WitnessPayload::CallReturn { .. } => "hop.call_return",
                WitnessPayload::QualifiedCallReturn { .. } => "hop.qualified_call",
                WitnessPayload::ReturnExpr(_) => "hop.return_expr",
                WitnessPayload::Fact { .. } => "hop.fact",
                WitnessPayload::Derivation => "hop.derivation",
                WitnessPayload::Custom { .. } => "hop.custom",
                WitnessPayload::Projected { .. } => "hop.projected",
                _ => "hop.other",
            });
        }
        if matches!(q.attachment, WitnessAttachment::Expr(_)) {
            TOUCHED_EXPR.with(|c| c.set(true));
            // WHY the chase needs the raw derivation here. If these land in a
            // few recurring payload shapes, each is a candidate for a
            // PARAMETERISED conclusion (`ReturnExpr::Receiver` already is
            // one — "returns its invocant", a function of the query rather
            // than a value). If they are spread across everything, the
            // derivation is genuinely open and no conclusion layer closes it.
            for w in bag.for_attachment(&q.attachment) {
                crate::util::ghost_stats::count(match &w.payload {
                    WitnessPayload::InferredType(_) => "expr_hop.inferred_type",
                    WitnessPayload::Observation(_) => "expr_hop.observation",
                    WitnessPayload::Edge(_) => "expr_hop.edge",
                    WitnessPayload::CallReturn { .. } => "expr_hop.call_return",
                    WitnessPayload::QualifiedCallReturn { .. } => "expr_hop.qualified_call",
                    WitnessPayload::ReturnExpr(_) => "expr_hop.return_expr",
                    WitnessPayload::Fact { .. } => "expr_hop.fact",
                    WitnessPayload::Derivation => "expr_hop.derivation",
                    WitnessPayload::Custom { .. } => "expr_hop.custom",
                    WitnessPayload::Projected { .. } => "expr_hop.projected",
                    _ => "expr_hop.other",
                });
            }
        }
        let depth = QUERY_REC_DEPTH.with(|c| {
            let d = c.get();
            c.set(d + 1);
            d
        });
        if depth >= QUERY_REC_DEPTH_CAP {
            // Counted on EVERY hit: the one-shot warning below says the cap
            // fired, but only a count distinguishes one pathological file
            // from a systematic truncation across the corpus.
            crate::util::ghost_stats::count("query_rec.depth_cap");
            // A truncated chase answers `None` for a reason no key names. Any
            // `Link` minted around it would claim the ladder ended where the
            // budget ran out, not where the answer was. `truncated` is the
            // separate, non-poison record of the same event: a LIVE chase that
            // truncates is not wrong, it is incomplete, so comparing a
            // conclusion against it proves nothing either way.
            state.poisoned = true;
            // The LIVE counterpart of the poison above, and it has to be
            // thread-local rather than a `QueryState` field: the cap counts
            // frames across the whole THREAD, so it fires inside nested
            // top-level queries (`symbol_return_type_via_bag`, the enrichment
            // overlay) that carry a `QueryState` of their own. A per-state flag
            // reads false for exactly the chases truncated deepest.
            QUERY_REC_TRUNCATIONS.with(|c| c.set(c.get() + 1));
            QUERY_REC_DEPTH_WARNED.with(|w| {
                if !w.get() {
                    w.set(true);
                    log::warn!(
                        "query_rec depth cap ({}) hit on attachment {:?} — returning \
                         None, so this answer is silently incomplete. Further hits are \
                         counted as `query_rec.depth_cap` (PERL_LSP_GHOST_STATS).",
                        QUERY_REC_DEPTH_CAP,
                        q.attachment,
                    );
                }
            });
            QUERY_REC_DEPTH.with(|c| c.set(c.get() - 1));
            return std::sync::Arc::new(ReducedValue::None);
        }
        let key: VisitedKey = (
            bag as *const _ as usize,
            q.attachment.clone(),
            receiver_key(&q.receiver),
            q.arity_hint,
        );
        // Memo hit: this key was fully resolved earlier in THIS query and
        // isn't on the current path (cycle guard handles on-path keys).
        if let Some((cached, recorded_exit)) = state.memo.get(&key) {
            let cached = std::sync::Arc::clone(cached);
            // Re-reaching an exiting subtree from inside a combining frame is
            // the same claim as reaching it there the first time; the memo must
            // not launder it into a rung just because the first reach happened
            // to be transparent.
            if *recorded_exit && state.opaque_frames > 0 {
                crate::util::ghost_stats::count("residual.memo_under_opaque");
                state.poisoned = true;
            }
            QUERY_REC_DEPTH.with(|c| c.set(c.get() - 1));
            return cached;
        }
        // `key` has two owners (the visited set, transiently; the memo,
        // for the rest of the query). Clone once for visited, then move
        // the original into the memo store below.
        if !state.visited.insert(key.clone()) {
            QUERY_REC_DEPTH.with(|c| c.set(c.get() - 1));
            return std::sync::Arc::new(ReducedValue::None);
        }
        let exits_before = state.residual.len();
        let poison_before = state.poisoned;
        let result = std::sync::Arc::new(self.query_rec_body(bag, q, state));
        let recorded_exit =
            state.residual.len() > exits_before || (state.poisoned && !poison_before);
        state.visited.remove(&key);
        // Cache the off-path resolution. The query depends only on
        // `(bag, attachment, receiver-class, arity)` (all in `key`) plus
        // the static context, which is fixed for one top-level query.
        state.memo.insert(key, (std::sync::Arc::clone(&result), recorded_exit));
        QUERY_REC_DEPTH.with(|c| c.set(c.get() - 1));
        result
    }

    fn query_rec_body(
        &self,
        bag: &WitnessBag,
        q: &ReducerQuery,
        state: &mut QueryState,
    ) -> ReducedValue {
        let materialized = self.materialize(bag, q, state);

        for r in &self.reducers {
            let claimed: Vec<&Witness> =
                materialized.iter().filter(|w| r.claims(w)).collect();
            if claimed.is_empty() {
                continue;
            }
            let v = r.reduce(&claimed, q);
            if v != ReducedValue::None {
                return v;
            }
        }

        // Inheritance + bridge fallback for `PackageSymbol{C, m}` queries
        // the local bag couldn't answer. Most cases are covered by
        // build-time edge emission (local writeback emits
        // `PackageSymbol(child, m) → Edge(PackageSymbol(parent, m))`;
        // enrichment projects the same for cross-file parents), resolved
        // by the generic edge-chase. This fallback covers the residual:
        // hand-crafted FAs / isolated tests, and cross-file
        // plugin-namespace bridges declared in other files. Three
        // structural facts compose:
        //
        //   1. `module_index.get_cached(C)` — when `C` lives in another
        //      file, recurse into its cached bag for C's direct facts.
        //   2. `package_parents[C]` (local) ∪ `parents_cached(C)`
        //      (cross-file) — the Perl DFS-MRO chain; recurse on
        //      `PackageSymbol{P, m}` per parent.
        //   3. `for_each_entity_bridged_to(class, ...)` — entities in
        //      other files' plugin namespaces bridged to `class`; query
        //      each cached bag by `Symbol(sym.id)` (per-FA SymbolIds
        //      can't be portably edge-encoded).
        //
        // The shared visited set breaks local and cross-file cycles.
        //
        // Budget gate for EVERY cross-file hop below (primary, ancestry,
        // bridges, slot writes). The local reducers above have already run,
        // so a spent walk still answers from what this bag knows and only
        // stops CHASING. Gating the hops individually let the cheap ones
        // through and the walk kept running; one gate at the boundary is
        // the honest placement.
        if let Some(idx) = q.context.and_then(|c| c.module_index) {
            if !super::session::budget_available(idx) {
                return ReducedValue::None;
            }
        }
        if let WitnessAttachment::PackageSymbol { package, name } = q.attachment {
            if let Some(ctx) = q.context {
                // (1) Cross-file primary lookup — every candidate file
                // declaring `package` (a reopened package's method lives in
                // whichever file defines it, not the name-slot winner).
                if ctx.module_index.is_none() {
                    super::note_bake_exit("moc_primary", true);
                    note_moc_exit(state, package, name);
                }
                if let Some(idx) = ctx.module_index {
                    if let Some(v) =
                        self.moc_cross_file_primary(bag, q, state, ctx, idx, package)
                    {
                        return v;
                    }
                }
                // (2) Inheritance walk via package_parents (local ∪
                // cross-file ∪ synthetic app-surface edge — `parents_of`
                // is the single edge-injection site shared with the
                // FA-side ancestor walks).
                if ctx.module_index.is_none() {
                    // The parent NAME is local (`PackageFacts.parents`), so a
                    // parent hop is nameable even though the parent's FILE is
                    // not reachable from here. Each parent is a ladder rung and
                    // is recorded in MRO order below.
                    super::note_bake_exit("parent_walk", true);
                }
                let parents = crate::model::file_analysis::parents_of(
                    package,
                    ctx.package_parents,
                    ctx.module_index,
                    ctx.app_surface_consumers,
                );
                // Each parent is a ladder rung. Recorded in MRO order, because
                // order IS the semantics of a `Link`'s fan-out: first answer
                // wins, exactly as this loop behaves.
                if ctx.module_index.is_none() {
                    note_parent_rungs(state, &parents, name);
                }
                for p in parents {
                    let parent_att = WitnessAttachment::PackageSymbol {
                        package: p,
                        name: name.clone(),
                    };
                    let sub_q = ReducerQuery {
                        attachment: &parent_att,
                        point: q.point,
                        framework: q.framework,
                        arity_hint: q.arity_hint,
                        receiver: q.receiver.clone(),
                        args: q.args.clone(),
                        context: q.context,
                    };
                    let v = self.query_rec(bag, &sub_q, state);
                    if *v != ReducedValue::None {
                        return (*v).clone();
                    }
                }
                // (3) Cross-file plugin-namespace bridges. Plugin entities
                // declared in OTHER files bridged to `package` aren't
                // reachable via the local bag's edges nor the cross-file
                // primary (`get_cached(package)` returns the canonical package
                // file, not the bridging-plugin file). Ask each matching
                // cached entity for `Symbol(sym.id)` at arity=None —
                // bridged Methods aren't arity-discriminated.
                if ctx.module_index.is_none() {
                    // NOT a poison and NOT a residual. A bridge target is a
                    // per-file `SymbolId` that no portable key can name, so it
                    // would poison every chase — 99.96% of them, measured. The
                    // consult-side guard (`class_is_bridged_to`) now covers
                    // exactly the forms this arm could contradict, so the bake
                    // no longer has to reason about it at all.
                    super::note_bake_exit("bridge", false);
                }
                if let Some(idx) = ctx.module_index {
                    // LIVE-mode denominator for the bake's `residual.site.bridge`
                    // count: how often would that consult have yielded anything
                    // at all? A would-be consult that returns nothing is not a
                    // dependence, and counting it as one makes the poison rate
                    // look total when it may be negligible.
                    let mut bridge_seen = false;
                    idx.for_each_entity_bridged_to(package, &mut |_m, _c, _s| {
                        bridge_seen = true;
                        std::ops::ControlFlow::Break(())
                    });
                    crate::util::ghost_stats::count(if bridge_seen {
                        "bridge.live_yields"
                    } else {
                        "bridge.live_empty"
                    });
                    // Per-CLASS, because the guard is per-class while the
                    // 98.3%-vacuous figure is per-call. If the real yields
                    // concentrate in a few hot classes, those stay guarded-off
                    // permanently and the decode cost lands exactly where
                    // bridges are real — which is what sizes the follow-on.
                    if bridge_seen && crate::util::ghost_stats::enabled() {
                        // `enabled()` first: `format!` allocates before `count`
                        // can decline, so the naive spelling pays for a string
                        // per yield even with stats off.
                        crate::util::ghost_stats::count(&format!("bridgecls.{package}"));
                    }
                    let mut found: Option<InferredType> = None;
                    idx.for_each_entity_bridged_to_named(package, name, &mut |_mod, cached, sym| {
                        use std::ops::ControlFlow;
                        if !matches!(
                            sym.kind,
                            crate::model::file_analysis::SymKind::Sub
                                | crate::model::file_analysis::SymKind::Method
                        ) {
                            return ControlFlow::Continue(());
                        }
                        if &sym.name != name {
                            return ControlFlow::Continue(());
                        }
                        // Bridged Method's return lives in the bridging file's
                        // bag — rehydrate it if evicted before querying.
                        crate::util::ghost_stats::count("moc.provider_fetched");
                        crate::util::ghost_stats::count("mocsite.bridged");
                        let full = idx.bag_present(cached);
                        if let Some(t) = full.symbol_return_type_via_bag(sym.id, None) {
                            found = Some(t);
                            return ControlFlow::Break(());
                        }
                        // Fallback-on-miss (R4): the bridged Method's return may
                        // chain through the bridging file's OWN imports — baked
                        // only into the enriched overlay. `symbol_return_type_via_bag`
                        // owns its answer (private registry + QueryState), so no
                        // `state.pins` push is needed. Kept index-less by design:
                        // a ctx-ful leaf query would spawn a fresh cycle guard per
                        // bridged hop, so mutual bridges recurse unbounded; the
                        // ENRICHING-guarded bake is the safe route to the same
                        // transitive answer.
                        crate::util::ghost_stats::count("consult.bridged");
                        if idx.serves_enriched() {
                            let enriched = idx.enriched_present(cached);
                            if !std::sync::Arc::ptr_eq(&enriched, &full) {
                                if let Some(t) =
                                    enriched.symbol_return_type_via_bag(sym.id, None)
                                {
                                    found = Some(t);
                                    return ControlFlow::Break(());
                                }
                            }
                        }
                        ControlFlow::Continue(())
                    });
                    if let Some(t) = found {
                        return ReducedValue::Type(t);
                    }
                }
            }
        }

        // `SlotType{C, k}` the local bag couldn't answer: the typed
        // slot WRITE may live in C's own file (cross-file primary) or
        // anywhere up C's ancestry (a base class's BUILD populating
        // `$self->{conn}`). Hops (1) and (2) of the `PackageSymbol`
        // fallback above, same shared visited set; no bridge hop —
        // slot writes are real code, not plugin entities.
        if let WitnessAttachment::SlotType { class, key } = q.attachment {
            if let Some(ctx) = q.context {
                if ctx.module_index.is_none() {
                    super::note_bake_exit("slot_type", true);
                    note_slot_exit(state, class, key);
                }
                if let Some(idx) = ctx.module_index {
                    // The point-free memo spelling, same as the primary's:
                    // answers below are computed point-free, so one (class,
                    // key, candidate) verdict serves every call site in the
                    // sweep. This arm carried 99.99% of FHEM's 12.3M provider
                    // ATTEMPTS — the sweep memo made each attempt's fetch an
                    // Arc bump, but the CHASE per attempt (a full query_rec
                    // into the provider bag) is what the answer memo removes.
                    let memo_q = ReducerQuery {
                        attachment: q.attachment,
                        point: None,
                        framework: q.framework,
                        arity_hint: None,
                        receiver: q.receiver.clone(),
                        args: q.args.clone(),
                        context: None,
                    };
                    let verdict_key = super::ConsultVerdictKey::of(&memo_q);
                    for cached in super::session::visible_def_candidates(idx, class).iter() {
                        if let Some(hit) =
                            super::session::candidate_answer(idx, &cached.path, &memo_q)
                        {
                            if *hit != ReducedValue::None {
                                return (*hit).clone();
                            }
                            continue;
                        }
                        // The sweep tier, behind the session memo — a verdict
                        // another file's build already derived this sweep.
                        if let Some(hit) =
                            idx.sweep_consult_answer(&cached.path, &verdict_key)
                        {
                            super::session::remember_candidate_answer(
                                idx, &cached.path, &memo_q, &hit,
                            );
                            if *hit != ReducedValue::None {
                                return (*hit).clone();
                            }
                            continue;
                        }
                        // The rows-backed pre-filter — same placement and
                        // memoized-skip discipline as the primary's (see
                        // `moc_cross_file_primary`); the un-attributed
                        // flavor, because a slot key is a hash-key ref,
                        // not an attributed symbol.
                        let prefilter_denied =
                            !sweep_candidate_may_answer(idx, cached, class, key, false);
                        if prefilter_denied {
                            crate::util::ghost_stats::count("consult.prefilter_skip");
                            if !consult_prefilter_equiv() {
                                super::session::remember_candidate_answer(
                                    idx,
                                    &cached.path,
                                    &memo_q,
                                    &ReducedValue::None,
                                );
                                idx.remember_sweep_consult(
                                    &cached.path,
                                    &verdict_key,
                                    &ReducedValue::None,
                                );
                                continue;
                            }
                        }
                        let attempt =
                            |full: &std::sync::Arc<crate::model::file_analysis::FileAnalysis>,
                             state: &mut _| {
                                let cached_ctx = BagContext {
                                    scopes: &full.scopes,
                                    package_framework: &full.packages,
                                    module_index: Some(idx),
                                    package_parents: &full.packages,
                                    app_surface_consumers: &full.plugin.app_surface_consumers,
                                };
                                let sub_q = ReducerQuery {
                                    attachment: q.attachment,
                                    // Cross-file: the point is CONSUMER-file
                                    // coordinates, meaningless against the
                                    // provider's spans (the imported-sub
                                    // recursion in query.rs already passes
                                    // None). Point-free is also what lets a
                                    // memo key collapse across call sites.
                                    point: None,
                                    framework: q.framework,
                                    arity_hint: None,
                                    receiver: q.receiver.clone(),
                                    args: q.args.clone(),
                                    context: Some(&cached_ctx),
                                };
                                (*self.query_rec(&full.witnesses, &sub_q, state)).clone()
                            };
                        crate::util::ghost_stats::count("moc.provider_fetched");
                        crate::util::ghost_stats::count("mocsite.slot_type");
                        let full = idx.bag_present(&cached);
                        if !std::ptr::eq(bag, &full.witnesses) {
                            let v = attempt(&full, state);
                            crate::util::ghost_stats::count(if v == ReducedValue::None {
                                "moc.provider_no_answer"
                            } else {
                                "moc.provider_answered"
                            });
                            if v != ReducedValue::None {
                                if prefilter_denied {
                                    crate::util::ghost_stats::count(
                                        "consult.prefilter_break",
                                    );
                                    log::error!(
                                        "consult pre-filter break: rows proved {:?} \
                                         silent in {:?} but the chase answered {v:?}",
                                        q.attachment,
                                        cached.path
                                    );
                                    debug_assert!(
                                        false,
                                        "consult pre-filter hid an answer; see log"
                                    );
                                }
                                super::session::remember_candidate_answer(
                                    idx, &cached.path, &memo_q, &v,
                                );
                                idx.remember_sweep_consult(&cached.path, &verdict_key, &v);
                                return v;
                            }
                            // Fallback-on-miss (R4), symmetric with the
                            // PackageSymbol primary: a slot WRITE typed only in
                            // C's enriched copy resolves here. Today SlotType
                            // seeds are build-gated on a resolvable RHS
                            // (`builder.rs`), so a seed that exists already
                            // answers on the raw bag and this retry is the
                            // forward-looking twin — live the moment slot
                            // seeding emits an unconditional edge. Pin the
                            // enriched Arc: this chase threads the SHARED
                            // QueryState, whose memo keys on bag pointers.
                            crate::util::ghost_stats::count("consult.slot_type");
                            if idx.serves_enriched() {
                                let enriched = idx.enriched_present(&cached);
                                if !std::sync::Arc::ptr_eq(&enriched, &full)
                                    && !std::ptr::eq(bag, &enriched.witnesses)
                                {
                                    state.pins.push(std::sync::Arc::clone(&enriched));
                                    let v = attempt(&enriched, state);
                                    if v != ReducedValue::None {
                                        if prefilter_denied {
                                            crate::util::ghost_stats::count(
                                                "consult.prefilter_break",
                                            );
                                            log::error!(
                                                "consult pre-filter break (enriched): \
                                                 rows proved {:?} silent in {:?} but \
                                                 the chase answered {v:?}",
                                                q.attachment,
                                                cached.path
                                            );
                                            debug_assert!(
                                                false,
                                                "consult pre-filter hid an answer; see log"
                                            );
                                        }
                                        super::session::remember_candidate_answer(
                                            idx, &cached.path, &memo_q, &v,
                                        );
                                        idx.remember_sweep_consult(
                                            &cached.path, &verdict_key, &v,
                                        );
                                        return v;
                                    }
                                }
                            }
                            super::session::remember_candidate_answer(
                                idx,
                                &cached.path,
                                &memo_q,
                                &ReducedValue::None,
                            );
                            idx.remember_sweep_consult(
                                &cached.path, &verdict_key, &ReducedValue::None,
                            );
                        }
                    }
                }
                let parents = crate::model::file_analysis::parents_of(
                    class,
                    ctx.package_parents,
                    ctx.module_index,
                    ctx.app_surface_consumers,
                );
                for p in parents {
                    let parent_att = WitnessAttachment::SlotType {
                        class: p,
                        key: key.clone(),
                    };
                    let sub_q = ReducerQuery {
                        attachment: &parent_att,
                        point: q.point,
                        framework: q.framework,
                        arity_hint: None,
                        receiver: q.receiver.clone(),
                        args: q.args.clone(),
                        context: q.context,
                    };
                    let v = self.query_rec(bag, &sub_q, state);
                    if *v != ReducedValue::None {
                        return (*v).clone();
                    }
                }
            }
        }

        // `TypeName(name)` the local bag couldn't answer: the typedef may
        // live in another file (a header the alias name is a Class symbol
        // in). `get_cached(name)` finds that file; recurse into its bag —
        // hop (1) of the `PackageSymbol` fallback, same shared visited set.
        // Failing that, an unresolved alias IS a type of that name: the
        // one-alias-graph terminal (`ClassName(name)`), so a plain struct
        // tag / unknown class / primitive spelling resolves to itself.
        if let WitnessAttachment::TypeName(name) = q.attachment {
            if let Some(ctx) = q.context {
                if ctx.module_index.is_none() {
                    super::note_bake_exit("type_name", true);
                    note_type_name_exit(state, name);
                }
                if let Some(idx) = ctx.module_index {
                    for cached in idx.visible_def_candidates(name) {
                        crate::util::ghost_stats::count("moc.provider_fetched");
                        crate::util::ghost_stats::count("mocsite.type_name");
                        let full = idx.bag_present(&cached);
                        if !std::ptr::eq(bag, &full.witnesses) {
                            let cached_ctx = BagContext {
                                scopes: &full.scopes,
                                package_framework: &full.packages,
                                module_index: Some(idx),
                                package_parents: &full.packages,
                                app_surface_consumers: &full.plugin.app_surface_consumers,
                            };
                            let sub_q = ReducerQuery {
                                attachment: q.attachment,
                                // Cross-file: point normalized (see the slot arm).
                                point: None,
                                framework: q.framework,
                                arity_hint: None,
                                receiver: q.receiver.clone(),
                                args: q.args.clone(),
                                context: Some(&cached_ctx),
                            };
                            let v = self.query_rec(&full.witnesses, &sub_q, state);
                            if *v != ReducedValue::None {
                                return (*v).clone();
                            }
                        }
                    }
                }
            }
            // A template-shaped terminal (`TypeName("Box<Widget>")` — an
            // alias chain that bottomed out on a template spelling) peels
            // into the Instance flavor so dispatch keys the base, same as
            // an annot-site spelling.
            return ReducedValue::Type(
                crate::model::file_analysis::ParametricType::instance_from_spelling(name)
                    .map(InferredType::Parametric)
                    .unwrap_or_else(|| InferredType::ClassName(name.clone())),
            );
        }

        ReducedValue::None
    }

    /// Resolve every Edge witness on `q.attachment` to an `InferredType`
    /// witness via recursive query; non-edge witnesses pass through. The
    /// returned list is fresh-owned so reducers can borrow into it.
    ///
    /// `Edge(Variable{...})` targets are special-cased — variable
    /// resolution needs a scope-chain walk + the scope's framework. With
    /// a `BagContext`, this delegates to `query_variable_with_visited` so
    /// the recursion shares the caller's cycle guard (calling the public
    /// `query_variable_type` would reset visited and reopen mutual
    /// `Edge(Variable)` loops).


    /// The cross-file primary hop of the `PackageSymbol` ladder: every file
    /// declaring `package`, asked in ladder order, first answer wins,
    /// with the consult pre-filter (`sweep_candidate_may_answer`) skipping
    /// candidates the rows prove silent.
    ///
    /// Out of line, and `#[inline(never)]`, because of the STACK. This block's
    /// locals — two capture-heavy `attempt` closures, the conclusion-outcome
    /// bookkeeping, the equivalence probes — otherwise live in the caller's
    /// frame, and the caller is `query_rec_body`, which is live once per MRO
    /// hop against the 2 MiB stack the depth-cap test pins. An index-less chase
    /// (a bake, a hand-built FA) never enters here at all, so keeping it inline
    /// charged every one of those hops for a block it does not run.
    #[inline(never)]
    fn moc_cross_file_primary(
        &self,
        bag: &WitnessBag,
        q: &ReducerQuery,
        state: &mut QueryState,
        ctx: &BagContext,
        idx: &dyn crate::model::file_analysis::CrossFileLookup,
        package: &str,
    ) -> Option<ReducedValue> {
        // The memo spelling of this query: point-free, because the answers
        // below are computed point-free (cross-file sub-queries normalize the
        // consumer's point out) and a point-carrying key made every call site
        // a fresh memo miss.
        let memo_q = ReducerQuery {
            attachment: q.attachment,
            point: None,
            framework: q.framework,
            arity_hint: q.arity_hint,
            receiver: q.receiver.clone(),
            args: q.args.clone(),
            context: None,
        };
        // The sweep-tier spelling of the same key — shared across files and
        // workers where a batch sweep opened the store (SweepAnswerGuard).
        let verdict_key = super::ConsultVerdictKey::of(&memo_q);
        let att_name = match q.attachment {
            WitnessAttachment::PackageSymbol { name, .. } => name.as_str(),
            _ => return None,
        };
        for cached in super::session::visible_def_candidates(idx, package).iter() {
            // Rehydrate the target file's bag if its resident copy
            // was Slice-2-evicted; the cross-file chase reads its
            // witnesses (`docs/adr/memory-slice-2-lru.md`).
            let attempt =
                |full: &std::sync::Arc<crate::model::file_analysis::FileAnalysis>,
                 state: &mut _| {
                    let cached_ctx = BagContext {
                        scopes: &full.scopes,
                        package_framework: &full.packages,
                        module_index: Some(idx),
                        package_parents: &full.packages,
                        app_surface_consumers: &full.plugin.app_surface_consumers,
                    };
                    let sub_q = ReducerQuery {
                        attachment: q.attachment,
                        // Cross-file: the point is CONSUMER-file coordinates,
                        // meaningless against the provider's spans (the
                        // imported-sub recursion in query.rs already passes
                        // None). Point-free is also what lets the memo key
                        // below collapse across call sites — keyed WITH the
                        // point, a hash key read at 500 sites was 500 memo
                        // misses per candidate.
                        point: None,
                        framework: q.framework,
                        arity_hint: q.arity_hint,
                        receiver: q.receiver.clone(),
                        args: q.args.clone(),
                        context: Some(&cached_ctx),
                    };
                    (*self.query_rec(&full.witnesses, &sub_q, state)).clone()
                };
            // This candidate's contribution, remembered ACROSS
            // top-level queries. `attempt` is a pure function of
            // (candidate file, attachment, receiver, arity, framework) —
            // the whole key now that the point is normalized out — so one
            // walk derives it once instead of once per call site.
            if let Some(hit) =
                super::session::candidate_answer(idx, &cached.path, &memo_q)
            {
                if *hit != ReducedValue::None {
                    return Some((*hit).clone());
                }
                continue;
            }
            // The sweep tier, behind the (cheaper) session memo: a verdict
            // another file's build already derived this sweep.
            if let Some(hit) = idx.sweep_consult_answer(&cached.path, &verdict_key) {
                super::session::remember_candidate_answer(idx, &cached.path, &memo_q, &hit);
                if *hit != ReducedValue::None {
                    return Some((*hit).clone());
                }
                continue;
            }
            // The rows-backed pre-filter, behind both memo tiers (their hit
            // is cheaper) and ahead of the budget spend (a skip is not a
            // consult). A skip is remembered as the `None` verdict it
            // claims, so each (candidate, key) pair is probed once and
            // memo-hits thereafter — the same first-encounter floor as the
            // chase it replaces, at a row probe instead of a decode.
            let prefilter_denied =
                !sweep_candidate_may_answer(idx, cached, package, att_name, true);
            if prefilter_denied {
                crate::util::ghost_stats::count("consult.prefilter_skip");
                if !consult_prefilter_equiv() {
                    super::session::remember_candidate_answer(
                        idx,
                        &cached.path,
                        &memo_q,
                        &ReducedValue::None,
                    );
                    idx.remember_sweep_consult(
                        &cached.path,
                        &verdict_key,
                        &ReducedValue::None,
                    );
                    continue;
                }
            }
            if !super::session::spend_consult(idx) {
                break;
            }
            // THE CONCLUSION LOOKUP, ahead of the decode.
            //
            // This is the whole point of the layer: 78% of a
            // consult is the chase, not the fetch, and a baked
            // answer skips both. Placed after the session memo
            // (a hit there is cheaper still) and before
            // `bag_present` (which decodes).
            //
            // The three outcomes are NOT interchangeable:
            //   Answer  — serve it, no decode.
            //   None    — the map PROVES no answer; fall through
            //             to the next candidate exactly as a
            //             decoded miss would, still no decode.
            //   Decode  — `OpenNone`: unbakeable here, so pay the
            //             full price for this key alone.
            // A `Follow` is not yet honoured — see below.
            let mut baked_said_absent = false;
            // The map proved this candidate has no LOCAL answer. Weaker than
            // `baked_said_absent` (which can end the whole resolution) and
            // stronger than a decode (which would discover the same thing).
            let mut not_local = false;
            // WHY this candidate is about to be decoded, so the decode's
            // OUTCOME can be attributed back to its cause. That is the number
            // that decides whether a per-class conclusion form is worth
            // building: a decode whose chase then answers nothing locally was
            // spent walking to a parent the map could have named.
            let mut decode_cause: Option<super::OpenReason> = None;
            // Set when a certificate said this candidate's silence is real and
            // `PERL_LSP_CLOSED_EQUIV` asked for the decode anyway, so the arm
            // below can report an answer the trust claimed could not exist.
            let mut closed_absence = false;
            // Under the equivalence flag a followed answer is held
            // rather than returned, so the chase below runs and can
            // contradict it.
            let mut followed_answer: Option<InferredType> = None;
            if let Some(key) =
                super::ConclusionKey::from_attachment(q.attachment)
            {
                if let Some(map) = idx.conclusions_for(&cached.path) {
                    match map.evaluate(
                        &key,
                        q.receiver.as_ref(),
                        q.arity_hint,
                        &q.args,
                    ) {
                        super::Outcome::Answer(t) => {
                            crate::util::ghost_stats::count("consult.baked_answer");
                            let v = ReducedValue::Type(t);
                            // The memo still gets the answer. A
                            // baked hit is cheap but not free, and
                            // the memo is the tier above it.
                            super::session::remember_candidate_answer(
                                idx, &cached.path, &memo_q, &v,
                            );
                            idx.remember_sweep_consult(&cached.path, &verdict_key, &v);
                            return Some(v);
                        }
                        // ABSENT. The spec lets this mean a proven
                        // `None` and skip the candidate outright —
                        // "the sharpest knife in the design" — but
                        // that is sound only if the bake enumerated
                        // every key the bag could answer, and today
                        // it does not: it walks the bag's
                        // attachment index, while the live chase
                        // also answers keys that carry no witnesses
                        // (inheritance edges, reducer synthesis).
                        //
                        // A wrongly-absent key makes the ladder
                        // skip a candidate that would have
                        // answered; the answer is then found
                        // further up the parent walk, so the OUTPUT
                        // agrees and only the cost betrays it.
                        // Measured on `--dump-package Catalyst`:
                        // trusting absence took 892 decodes to
                        // 2,721 and 2.76s to 4.20s, byte-identical
                        // output throughout. A silent 3x, invisible
                        // to every correctness check we have.
                        //
                        // So absence falls through to the decode
                        // until the enumeration is provably
                        // complete. `PERL_LSP_TRUST_ABSENT` turns
                        // the knife back on for measuring that work
                        // as it lands.
                        super::Outcome::None => {
                            crate::util::ghost_stats::count("consult.baked_none");
                            baked_said_absent = true;
                            // Absence is conclusive only for a
                            // class with NO ancestors at all. The
                            // per-file bake cannot establish that:
                            // Perl packages are open, and a file
                            // that REOPENS a package without
                            // repeating its `@ISA` sees a
                            // parentless class. `PPI::XSAccessor`
                            // does exactly that to `PPI::Token`,
                            // and it accounted for 75 of the
                            // equivalence breaks left after the
                            // per-file check.
                            //
                            // So the question is asked where the
                            // cross-file union lives, through the
                            // same `parents_of` every other
                            // ancestor walk uses.
                            let has_ancestors =
                                !crate::model::file_analysis::parents_of(
                                    package,
                                    ctx.package_parents,
                                    ctx.module_index,
                                    ctx.app_surface_consumers,
                                )
                                .is_empty();
                            if has_ancestors {
                                crate::util::ghost_stats::count(
                                    "consult.absent_but_inherits",
                                );
                                baked_said_absent = false;
                            }
                            // The bridge guard, and it closes a
                            // hole that PREDATES the conclusion
                            // layer: trusting absence asks only
                            // "no ancestors", while the live
                            // ladder's bridge arm runs regardless
                            // of ancestry. A PARENTLESS BRIDGED
                            // class therefore has its absence
                            // trusted while the chase answers
                            // through the bridge. The substrate
                            // happens to contain no such class —
                            // corpus luck, not soundness.
                            if baked_said_absent
                                && idx.class_is_bridged_to(package)
                            {
                                crate::util::ghost_stats::count(
                                    "consult.absent_but_bridged",
                                );
                                baked_said_absent = false;
                            }
                            // Under the equivalence flag, do NOT
                            // trust it — fall through, run the
                            // real chase, and let the arm below
                            // report any answer that absence
                            // claimed did not exist.
                            if baked_said_absent
                                && super::trust_absent_conclusions()
                                && !super::verify_absent_conclusions()
                            {
                                // Remember the None BEFORE
                                // continuing. Skipping the memo
                                // was the actual cost of trusting
                                // absence: this candidate is asked
                                // hundreds of times per run, and
                                // each repeat re-walked its
                                // ancestors instead of hitting the
                                // tier that exists to stop exactly
                                // that.
                                super::session::remember_candidate_answer(
                                    idx,
                                    &cached.path,
                                    &memo_q,
                                    &ReducedValue::None,
                                );
                                idx.remember_sweep_consult(
                                    &cached.path, &verdict_key, &ReducedValue::None,
                                );
                                continue;
                            }
                        }
                        // PROVEN NOT-LOCAL. The file declares the
                        // class and enumerated its own members;
                        // the key is not among them, so its chase
                        // has nothing local to find. Skip the
                        // candidate and let the ladder continue —
                        // more candidates, then the parent walk,
                        // then bridges — which is precisely what
                        // the decode was going to discover.
                        //
                        // Measured before it was built: of the
                        // decodes this replaces, 43,465 answered
                        // nothing against 558 that answered.
                        //
                        // NOT a `Follow` at the parents. That would
                        // skip candidates 2..n, and a reopened
                        // package's method lives in a later
                        // candidate. Continuing the loop keeps the
                        // ladder's order correct by construction.
                        super::Outcome::NotLocal => {
                            crate::util::ghost_stats::count("consult.not_local");
                            not_local = !super::not_local_disabled();
                        }
                        // Cross-file hop: re-enter the ladder at
                        // the target file's map, no decode.
                        //
                        // This is the first conclusion form whose
                        // failure is UNSOUND rather than merely
                        // slow — a wrong absence costs a decode, a
                        // wrong `Link` serves a wrong answer — so
                        // it is scored by `PERL_LSP_CONCL_EQUIV`
                        // against the chase it replaces, and it
                        // degrades to that chase whenever the walk
                        // cannot complete.
                        super::Outcome::Follow {
                            targets,
                            arity,
                            receiver,
                        } if !idx.class_is_bridged_to(package) => {
                            match follow_link(idx, &targets, &receiver, arity, &q.args) {
                                Some(t) => {
                                    crate::util::ghost_stats::count(
                                        "consult.baked_follow",
                                    );
                                    if super::verify_absent_conclusions() {
                                        followed_answer = Some(t);
                                    } else {
                                        let v = ReducedValue::Type(t);
                                        super::session::remember_candidate_answer(
                                            idx, &cached.path, &memo_q, &v,
                                        );
                                        idx.remember_sweep_consult(
                                            &cached.path, &verdict_key, &v,
                                        );
                                        return Some(v);
                                    }
                                }
                                None => {
                                    crate::util::ghost_stats::count(
                                        "consult.baked_follow_incomplete",
                                    );
                                }
                            }
                        }
                        // A `Link` says "everything before the
                        // bridge arm answered None", which a
                        // bridged class can contradict. Decode.
                        super::Outcome::Follow { .. } => {
                            crate::util::ghost_stats::count(
                                "consult.follow_but_bridged",
                            );
                        }
                        // The cause rides the outcome, so the tally is taken
                        // where the cost lands. A bake-side count would count
                        // KEYS; this counts decodes, weighted by how often
                        // each key is actually asked.
                        super::Outcome::Decode(reason) => {
                            crate::util::ghost_stats::count("consult.baked_open");
                            crate::util::ghost_stats::count(reason.tag());
                            // World-level closedness (§6j). `AbsentNotClosed`
                            // means "this file never declared the class, so
                            // its silence says nothing" — but if the class's
                            // whole ancestry is enumerable AND every provider
                            // still matches what a certificate recorded, then
                            // silence across that closure is a real None and
                            // the decode buys nothing. Measured at 96.8%
                            // wasted in this population.
                            //
                            // The certificate is consulted HERE, at the
                            // consumption site, because this is where the
                            // index is in hand — the verdict itself is
                            // produced index-free inside the bake, which is
                            // what keeps maps index-free and both EQUIV
                            // disciplines intact.
                            if reason == super::OpenReason::AbsentNotClosed
                                && super::closedness::class_is_closed(
                                    idx,
                                    &cached.analysis,
                                    package,
                                )
                            {
                                crate::util::ghost_stats::count(
                                    "closed.trusted_absence",
                                );
                                if super::verify_closedness() {
                                    // Score the read: decode anyway and let
                                    // the arm below report any answer this
                                    // trusted absence claimed could not exist.
                                    closed_absence = true;
                                    decode_cause = Some(reason);
                                } else {
                                    continue;
                                }
                            } else {
                                decode_cause = Some(reason);
                            }
                        }
                    }
                } else {
                    crate::util::ghost_stats::count("consult.not_baked");
                }
            }
            // Skip the decode the verdict just made pointless. Under the
            // equivalence flag, do NOT skip: run the chase and let the arm
            // below report any answer the verdict claimed this file could not
            // give.
            if not_local {
                // THE CHECK, and it has to ask the right question. The failure
                // this verdict can cause is serving a PARENT's answer where the
                // candidate defines the method itself and the bake's
                // enumeration missed it. So the comparison is against a
                // LOCAL-ONLY chase — the same index-less context the bake ran
                // under — not against the candidate's full chase, which walks
                // its own parents and legitimately answers for methods the
                // verdict correctly calls not-local.
                //
                // My first version compared against the full chase and read
                // 555 breaks, every one an inherited accessor the verdict was
                // right about. A check that fires on correct behaviour is
                // worse than none: it would have sent me hunting an
                // enumeration gap that isn't there.
                if super::verify_absent_conclusions() {
                    let full = idx.bag_present(cached);
                    let local_ctx = BagContext {
                        scopes: &full.scopes,
                        package_framework: &full.packages,
                        module_index: None,
                        package_parents: &full.packages,
                        app_surface_consumers: &full.plugin.app_surface_consumers,
                    };
                    let sub_q = ReducerQuery {
                        attachment: q.attachment,
                        point: q.point,
                        framework: q.framework,
                        arity_hint: q.arity_hint,
                        receiver: q.receiver.clone(),
                        args: q.args.clone(),
                        context: Some(&local_ctx),
                    };
                    // A fresh state: this is a separate question, and threading
                    // the live one would let its cycle guard answer `None` for
                    // a key the local chase can actually resolve.
                    let mut probe_state = QueryState::new();
                    let local = (*self.query_rec(&full.witnesses, &sub_q, &mut probe_state))
                        .clone();
                    if local != ReducedValue::None {
                        crate::util::ghost_stats::count("concl.not_local_break");
                        log::error!(
                            "conclusion not-local break: the map proved {:?} has no LOCAL \
                             answer in {:?}, but an index-less chase of that file answered \
                             {local:?} — the bake's local enumeration is incomplete, and \
                             skipping the candidate would serve a parent's answer over its \
                             own",
                            q.attachment,
                            cached.path
                        );
                        debug_assert!(
                            false,
                            "a not-local verdict hid a local answer; see log"
                        );
                    } else {
                        crate::util::ghost_stats::count("concl.not_local_ok");
                    }
                }
                // Remember it, for the reason trusting absence had to learn
                // the hard way: a candidate is asked hundreds of times per
                // run, and skipping the memo costs more than the decode saved.
                super::session::remember_candidate_answer(
                    idx,
                    &cached.path,
                    &memo_q,
                    &ReducedValue::None,
                );
                idx.remember_sweep_consult(&cached.path, &verdict_key, &ReducedValue::None);
                continue;
            }
            crate::util::ghost_stats::count("moc.provider_fetched");
                        crate::util::ghost_stats::count("mocsite.primary");
            // Attribution twin of `mocpkg.*`: which class family drives the
            // FETCHES (each one a decode on LRU miss), not just the queries.
            crate::util::ghost_stats::count(if package == "main" {
                "mocfetch.main"
            } else {
                "mocfetch.other"
            });
            // The three costs of one cross-file consult, split
            // because a conclusion layer would remove the first
            // two and CANNOT remove the third (enrichment is bag
            // surgery). Sizing stage 2 means knowing which is
            // which, not the total.
            //
            // These NEST over the `decode.*` stage split rather
            // than restating it: a miss here descends through
            // `bagcache.decode` into `decode.2_zstd`/`3_bincode`.
            // Summing a `consult.*` against a `decode.*` term
            // double-counts the same microseconds.
            // Snapshotted before the chase runs, so the classifier
            // below asks "did the cap fire during THIS chase"
            // rather than "has it ever fired on this thread".
            let truncations_before = QUERY_REC_TRUNCATIONS.with(|c| c.get());
            let full = crate::util::ghost_stats::timed(
                "consult.bag_present", || idx.bag_present(cached));
            if std::ptr::eq(bag, &full.witnesses) {
                // Self: the reducers above already tried this bag.
                // Not an answer about the candidate, so nothing to
                // remember either.
                continue;
            }
            let v = {
                let v = crate::util::ghost_stats::timed(
                    "consult.attempt", || attempt(&full, state));
                crate::util::ghost_stats::count(if v == ReducedValue::None {
                    "moc.provider_no_answer"
                } else {
                    "moc.provider_answered"
                });
                if let Some(cause) = decode_cause {
                    // `enabled()` first: `format!` allocates before `count`
                    // can decline.
                    if crate::util::ghost_stats::enabled() {
                        crate::util::ghost_stats::count(&format!(
                            "{}.{}",
                            cause.tag(),
                            if v == ReducedValue::None { "wasted" } else { "paid" }
                        ));
                    }
                }
                if closed_absence && v != ReducedValue::None {
                    // The certificate validated and the class's whole
                    // ancestry was enumerable, yet the chase found an answer
                    // there. Trusting that silence would have served a
                    // confident `None` — the one failure in this arc whose
                    // cost is a wrong answer rather than a decode.
                    crate::util::ghost_stats::count("closedequiv.break");
                    log::warn!(
                        "closed equiv: trusted absence for '{package}' but the \
                         chase answered — the certificate's closure is not the \
                         whole world for this class"
                    );
                }
                if v != ReducedValue::None {
                    v
                } else {
                // Fallback-on-miss (R4): the package file's method
                // return may chain through ITS OWN imports —
                // invisible to the raw bag, present in the
                // enriched overlay.
                crate::util::ghost_stats::count("consult.moc_primary");
                // `serves_enriched` first: when the overlay is off (one-shot
                // CLI) the fetch below returns the bag it already has, so the
                // retry is a guaranteed no-op paid per escalation.
                if idx.serves_enriched() {
                    let enriched = crate::util::ghost_stats::timed(
                        "consult.enriched", || idx.enriched_present(cached));
                    if !std::sync::Arc::ptr_eq(&enriched, &full)
                        && !std::ptr::eq(bag, &enriched.witnesses)
                    {
                        state.pins.push(std::sync::Arc::clone(&enriched));
                        attempt(&enriched, state)
                    } else {
                        ReducedValue::None
                    }
                } else {
                    ReducedValue::None
                }
                }
            };
            if let Some(followed) = &followed_answer {
                // Scored against the chase, not against the output.
                // A wrong `Link` is the layer's only failure mode
                // that serves a wrong ANSWER rather than costing a
                // decode, so it is the one that must be compared
                // where the claim is made.
                let agrees = matches!(&v, ReducedValue::Type(t) if t == followed);
                if !agrees {
                    // WHY the chase said `None`, because two of
                    // the reasons are not disagreement at all:
                    //
                    //  * TRUNCATED — the depth cap fired, so the
                    //    `None` means "ran out of frames".
                    //  * GUARDED — the shared cycle guard had this
                    //    very candidate key on the path, so the
                    //    chase returned without walking a rung. The
                    //    outer frame still standing on it walks
                    //    them.
                    //
                    // Over the substrate every remaining break is
                    // the second. The probe this replaced asked
                    // whether a LINK TARGET was on the path, which
                    // was doubly wrong: the guard cuts on the
                    // candidate key, and before self-rungs were
                    // filtered the targets included the key being
                    // chased, so it matched every time. A
                    // classifier that cannot fail is not one.
                    let candidate_key: VisitedKey = (
                        &full.witnesses as *const _ as usize,
                        q.attachment.clone(),
                        receiver_key(&q.receiver),
                        q.arity_hint,
                    );
                    let excused = if truncations_before
                        != QUERY_REC_TRUNCATIONS.with(|c| c.get())
                    {
                        Some("concl.follow_break_truncated")
                    } else if state.visited.contains(&candidate_key) {
                        Some("concl.follow_break_guarded")
                    } else {
                        None
                    };
                    crate::util::ghost_stats::count(
                        excused.unwrap_or("concl.follow_break"),
                    );
                    log::error!(
                        "conclusion follow break: a Link for {:?} in {:?} \
                         resolved to {followed:?} but the chase answered {v:?} \
                         — a baked cross-file hop disagrees with the hop it \
                         replaces",
                        q.attachment,
                        cached.path
                    );
                    debug_assert!(
                        excused.is_some(),
                        "Link disagreed with a chase that actually walked; see log"
                    );
                } else {
                    crate::util::ghost_stats::count("concl.follow_ok");
                }
            }
            if super::verify_absent_conclusions()
                && baked_said_absent
                && v != ReducedValue::None
            {
                crate::util::ghost_stats::count("concl.equiv_break");
                log::error!(
                    "conclusion equivalence break: the map reported {:?} ABSENT for \
                     {:?} (which is read as a proven None) but the chase answered \
                     {v:?} — the bake's key enumeration is incomplete",
                    q.attachment,
                    cached.path
                );
                debug_assert!(
                    false,
                    "conclusion absence disagreed with the chase; see log"
                );
            }
            if prefilter_denied && v != ReducedValue::None {
                // Equiv mode ran the attempt the pre-filter would have
                // skipped, and it answered: some route the gates and rows
                // cannot see exists. This is the "silently missing method"
                // failure the fail-open ladder exists to prevent — scream.
                crate::util::ghost_stats::count("consult.prefilter_break");
                log::error!(
                    "consult pre-filter break: rows proved {:?} silent in {:?} \
                     but the chase answered {v:?} — a bag answer route is \
                     missing from the gates or the unrowed-names derivation",
                    q.attachment,
                    cached.path
                );
                debug_assert!(false, "consult pre-filter hid an answer; see log");
            }
            super::session::remember_candidate_answer(idx, &cached.path, &memo_q, &v);
            idx.remember_sweep_consult(&cached.path, &verdict_key, &v);
            if v != ReducedValue::None {
                return Some(v);
            }
        }

        None
    }

    fn materialize(
        &self,
        bag: &WitnessBag,
        q: &ReducerQuery,
        state: &mut QueryState,
    ) -> Vec<Witness> {
        let raw = bag.for_attachment(q.attachment);
        // Is this attachment's value a pass-through of ONE sub-chase, or a fold
        // over several? With siblings present, whatever a sub-chase answers is
        // combined with them before this frame returns, so no single exit key
        // names this frame's answer.
        let sole_witness = raw.len() == 1;
        let mut out: Vec<Witness> = Vec::with_capacity(raw.len());
        for w in raw {
            match &w.payload {
                WitnessPayload::Edge(target) => {
                    let resolved = match (target, q.context) {
                        (
                            WitnessAttachment::Variable { name, scope },
                            Some(ctx),
                        ) => state.in_opaque_frame(|state| {
                            // Narrowing point: an edge reached FROM a positioned
                            // expression (a variable read recorded at `Expr(span)`)
                            // resolves the slot at the read's own location, so a
                            // flow-sensitive guard refines it only inside the
                            // guard's region (docs/adr/flow-narrowing.md). Other
                            // edge sources have no read position; the scope end is
                            // the standing temporal approximation.
                            let point = match q.attachment {
                                WitnessAttachment::Expr(span) => span.start,
                                _ => scope_point(ctx.scopes, *scope),
                            };
                            // Opaque: the scope walk defers a rep-only answer
                            // and lets an outer class identity beat it, so the
                            // value is chosen ACROSS scopes rather than taken
                            // from the first that answers.
                            self.query_variable_with_visited(
                                bag, ctx, name, *scope, point,
                                q.receiver.as_ref(), state,
                            )
                        }),
                        _ => {
                            // A `PackageSymbol{package,..}` reached through an edge is
                            // a fresh method dispatch: its receiver is that call's
                            // invocant (`class`), so a fluent `ReturnExpr(Receiver)`
                            // substitutes the dispatch class — not whatever the outer
                            // query carried. Mirrors `query_sub_return_type`'s
                            // `effective_receiver`. The exception is an inheritance
                            // hop (`PackageSymbol{child} → Edge(PackageSymbol{parent})`):
                            // there the source is itself a `PackageSymbol`, and the
                            // child's receiver must carry through so an inherited fluent
                            // accessor returns the child, not where `has` was declared.
                            let redispatched = matches!(
                                target,
                                WitnessAttachment::PackageSymbol { .. }
                            ) && !matches!(
                                q.attachment,
                                WitnessAttachment::PackageSymbol { .. }
                            );
                            let receiver = match target {
                                WitnessAttachment::PackageSymbol { package, .. }
                                    if redispatched =>
                                {
                                    fresh_dispatch_receiver(&q.receiver, package, q.context)
                                }
                                _ => q.receiver.clone(),
                            };
                            let sub_q = ReducerQuery {
                                attachment: target,
                                point: q.point,
                                framework: q.framework,
                                arity_hint: q.arity_hint,
                                receiver,
                                args: q.args.clone(),
                                context: q.context,
                            };
                            // The ONE transparent frame in `materialize`: a lone
                            // edge, chased under the query it was reached with,
                            // whose answer this frame returns unchanged. A
                            // re-dispatch substitutes a different receiver, and
                            // a `ReturnExpr::Receiver` at the far end then
                            // answers about that receiver rather than the one a
                            // `Link` follow would thread.
                            let transparent = sole_witness && !redispatched;
                            let chase = |state: &mut QueryState| {
                                match &*self.query_rec(bag, &sub_q, state) {
                                    ReducedValue::Type(t) => Some(t.clone()),
                                    ReducedValue::FactMap(_)
                                    | ReducedValue::None => None,
                                }
                            };
                            if transparent {
                                chase(state)
                            } else {
                                state.in_opaque_frame(chase)
                            }
                        }
                    };
                    if let Some(t) = resolved {
                        out.push(Witness {
                            attachment: w.attachment.clone(),
                            source: w.source.clone(),
                            payload: WitnessPayload::InferredType(t),
                            span: w.span,
                        });
                    }
                    // An edge that didn't resolve drops out — same as a
                    // witness no reducer claims.
                }
                WitnessPayload::CallReturn { target, arity } => {
                    // A fresh method dispatch at the call's own arity. The
                    // receiver is the dispatch class (`target`'s class, for
                    // a `PackageSymbol`) so a fluent `Receiver` substitutes
                    // it; the arity is the call site's, NOT the outer
                    // query's — that's the whole point of this variant.
                    let receiver = match target {
                        WitnessAttachment::PackageSymbol { package, .. } => {
                            fresh_dispatch_receiver(&q.receiver, package, q.context)
                        }
                        _ => q.receiver.clone(),
                    };
                    let sub_q = ReducerQuery {
                        attachment: target,
                        point: q.point,
                        framework: q.framework,
                        arity_hint: Some(*arity),
                        receiver,
                        args: q.args.clone(),
                        context: q.context,
                    };
                    // Opaque: the call site's arity and dispatch receiver both
                    // replace the outer query's, so the exit key is asked a
                    // different question than a `Link` follow would ask.
                    let v = state.in_opaque_frame(|state| {
                        (*self.query_rec(bag, &sub_q, state)).clone()
                    });
                    match v {
                        ReducedValue::Type(t) => out.push(Witness {
                            attachment: w.attachment.clone(),
                            source: w.source.clone(),
                            payload: WitnessPayload::InferredType(t),
                            span: w.span,
                        }),
                        ReducedValue::FactMap(_) | ReducedValue::None => {}
                    }
                }
                WitnessPayload::Projected { base, step } => {
                    // Materialize the base, then narrow through the step —
                    // the value-side mirror of the build-time
                    // `invocant_type_at_node` drill, run where the index is
                    // in hand so imported structural types project too.
                    // A Variable base scope-walks like the Edge arm above
                    // (`$h{k}` projects off `%h`, whose witnesses live on
                    // the decl scope, not the access scope).
                    // Opaque throughout: this frame returns a value drilled OUT
                    // of the sub-chase's answer, never the answer itself, so no
                    // exit key beneath it names what this frame produces.
                    let base_t = state.in_opaque_frame(|state| match (base, q.context) {
                        (WitnessAttachment::Variable { name, scope }, Some(ctx)) => {
                            let point = scope_point(ctx.scopes, *scope);
                            self.query_variable_with_visited(
                                bag, ctx, name, *scope, point,
                                q.receiver.as_ref(), state,
                            )
                        }
                        _ => {
                            let sub_q = ReducerQuery {
                                attachment: base,
                                point: q.point,
                                framework: q.framework,
                                arity_hint: None,
                                receiver: q.receiver.clone(),
                                args: q.args.clone(),
                                context: q.context,
                            };
                            match &*self.query_rec(bag, &sub_q, state) {
                                ReducedValue::Type(t) => Some(t.clone()),
                                ReducedValue::FactMap(_)
                                | ReducedValue::None => None,
                            }
                        }
                    });
                    if let Some(t) = base_t {
                        let projected = match step {
                            ProjectionStep::HashKey(k) => {
                                t.key_value_type(k).flatten().cloned().or_else(|| {
                                    // Class-typed base: the structural
                                    // literal can't answer, but a typed
                                    // slot WRITE can — `SlotType{class,
                                    // key}`, local or (via the arm in
                                    // query_rec_body) cross-file and up
                                    // the ancestry. The read drills
                                    // through the registry, never a
                                    // baked value.
                                    let class = t.class_name()?.to_string();
                                    let att = WitnessAttachment::SlotType {
                                        class,
                                        key: k.clone(),
                                    };
                                    let sub_q = ReducerQuery {
                                        attachment: &att,
                                        point: q.point,
                                        framework: q.framework,
                                        arity_hint: None,
                                        receiver: q.receiver.clone(),
                                        args: q.args.clone(),
                                        context: q.context,
                                    };
                                    state.in_opaque_frame(|state| {
                                        match &*self.query_rec(bag, &sub_q, state) {
                                            ReducedValue::Type(t) => Some(t.clone()),
                                            ReducedValue::FactMap(_)
                                            | ReducedValue::None => None,
                                        }
                                    })
                                })
                            }
                            ProjectionStep::ArrayIndex(i) => t.element_at(*i).cloned(),
                            ProjectionStep::Element => match &t {
                                crate::model::file_analysis::InferredType::Sequence(elems) => {
                                    let mut it = elems.iter();
                                    it.next().filter(|first| it.all(|e| e == *first)).cloned()
                                }
                                // A parametric container's TRAILING argument is
                                // its element by the same positional convention
                                // `ParamOf` projects (`array<K, V>` → V,
                                // `vector<T>` → T) — no base-name branch.
                                crate::model::file_analysis::InferredType::Parametric(
                                    crate::model::file_analysis::ParametricType::Instance {
                                        args, ..
                                    },
                                ) => args.last().cloned(),
                                _ => None,
                            },
                            ProjectionStep::Key => match &t {
                                // A sequence's keys ARE its positions.
                                crate::model::file_analysis::InferredType::Sequence(_) => {
                                    Some(crate::model::file_analysis::InferredType::Numeric)
                                }
                                // A two-argument instance keys by its first
                                // argument (`array<string, V>` → string).
                                crate::model::file_analysis::InferredType::Parametric(
                                    crate::model::file_analysis::ParametricType::Instance {
                                        args, ..
                                    },
                                ) if args.len() == 2 => args.first().cloned(),
                                _ => None,
                            },
                            ProjectionStep::MethodHop { member, arity } => {
                                // Fresh dispatch on the base's class at the
                                // call site's own arity; the base type IS the
                                // dynamic receiver, so a fluent `Receiver`
                                // return substitutes it (`$q->where()->get()`).
                                t.class_name().map(str::to_string).and_then(|class| {
                                    let att = WitnessAttachment::PackageSymbol {
                                        package: class,
                                        name: member.clone(),
                                    };
                                    let sub_q = ReducerQuery {
                                        attachment: &att,
                                        point: q.point,
                                        framework: q.framework,
                                        arity_hint: Some(*arity),
                                        receiver: Some(t.clone()),
                                        args: q.args.clone(),
                                        context: q.context,
                                    };
                                    state.in_opaque_frame(|state| {
                                        match &*self.query_rec(bag, &sub_q, state) {
                                            ReducedValue::Type(t) => Some(t.clone()),
                                            ReducedValue::FactMap(_)
                                            | ReducedValue::None => None,
                                        }
                                    })
                                })
                            }
                        };
                        if let Some(t) = projected {
                            out.push(Witness {
                                attachment: w.attachment.clone(),
                                source: w.source.clone(),
                                payload: WitnessPayload::InferredType(t),
                                span: w.span,
                            });
                        }
                    }
                }
                WitnessPayload::QualifiedCallReturn { method_lookup, receiver_class, arity } => {
                    // Look the method up on the named/parent class, but the
                    // receiver is the INVOCANT (enclosing) class — prefer a
                    // dynamic outer receiver only when it's a subclass of it
                    // (same rule as a fresh dispatch onto `receiver_class`).
                    let receiver =
                        fresh_dispatch_receiver(&q.receiver, receiver_class, q.context);
                    let sub_q = ReducerQuery {
                        attachment: method_lookup,
                        point: q.point,
                        framework: q.framework,
                        arity_hint: Some(*arity),
                        receiver,
                        args: q.args.clone(),
                        context: q.context,
                    };
                    // Opaque for the same reason as `CallReturn`, plus the
                    // lookup class and the receiver class deliberately differ.
                    let v = state.in_opaque_frame(|state| {
                        (*self.query_rec(bag, &sub_q, state)).clone()
                    });
                    match v {
                        ReducedValue::Type(t) => out.push(Witness {
                            attachment: w.attachment.clone(),
                            source: w.source.clone(),
                            payload: WitnessPayload::InferredType(t),
                            span: w.span,
                        }),
                        ReducedValue::FactMap(_) | ReducedValue::None => {}
                    }
                }
                _ => out.push(w.clone()),
            }
        }
        out
    }

    /// Scope-chain variable lookup with an explicit visited set.
    /// `query_variable_type` is the public entry; this is the inner loop,
    /// factored out so callers already inside a `query_rec` recursion
    /// (currently `materialize` for `Edge(Variable)`) can thread their
    /// cycle guard through, closing mutual `$a → $b → $a` edge cycles.
    pub(super) fn query_variable_with_visited(
        &self,
        bag: &WitnessBag,
        ctx: &BagContext,
        var: &str,
        scope: ScopeId,
        point: Point,
        receiver: Option<&InferredType>,
        state: &mut QueryState,
    ) -> Option<InferredType> {
        let chain = crate::model::file_analysis::scope_chain_of(ctx.scopes, scope);
        let framework = chain
            .iter()
            .find_map(|sid| ctx.scopes[sid.0 as usize].package.as_ref())
            .and_then(|pkg| ctx.package_framework.framework_of(pkg))
            .unwrap_or(FrameworkFact::Plain);
        // A scope that only OBSERVES rep use of the variable (`$self->{k}`
        // inside a nested block → HashRefAccess) yields a bare `HashRef`,
        // but the variable's identity — an invocant's ClassName seeded on
        // the sub scope — lives further out the chain. Class identity
        // anywhere dominates such a rep-only projection: the same
        // identity-over-rep rule `FrameworkAwareTypeFold` applies within a
        // scope, lifted across the scope walk. A scope that actually BINDS
        // the variable (explicit type / edge / class-or-bless observation)
        // is authoritative and returned immediately, so genuine shadowing
        // (`my $x = {}`) still wins. Defer the weak answer until the chain
        // is exhausted.
        let mut weak: Option<InferredType> = None;
        for sid in chain {
            let att = WitnessAttachment::Variable {
                name: var.to_string(),
                scope: sid,
            };
            let q = ReducerQuery {
                attachment: &att,
                point: Some(point),
                framework,
                arity_hint: None,
                // Threaded from the chasing query so a deferred
                // `ReturnExpr::ReceiverOr` on the variable (a statement-
                // position `bless $obj, $class`) substitutes the CALL
                // SITE's class — the hop that makes an inherited
                // `$class->new; ...; return $object` ctor type to the
                // subclass it was called on.
                receiver: receiver.cloned(),
                args: Vec::new(),
                context: Some(ctx),
            };
            match &*self.query_rec(bag, &q, state) {
                ReducedValue::Type(t) => {
                    let t = t.clone();
                    if t.class_name().is_some() || scope_binds_variable(bag, var, sid, point) {
                        return Some(t);
                    }
                    if weak.is_none() {
                        weak = Some(t);
                    }
                }
                ReducedValue::FactMap(_) | ReducedValue::None => {}
            }
        }
        weak
    }
}

/// Does this scope *bind* the variable — establish its value/identity via
/// an explicit type, an assignment edge, or a class/bless observation — as
/// opposed to merely OBSERVING rep use (`$v->{k}` → `HashRefAccess`)? A
/// binding scope's reduced type is authoritative; a rep-only scope's is a
/// weak projection an outer class identity dominates (see the caller). A
/// binding after the query point doesn't count. New value-carrying payload
/// variants count as bindings by default — only the bare rep/scalar
/// observations are the weak case.
fn scope_binds_variable(bag: &WitnessBag, var: &str, scope: ScopeId, point: Point) -> bool {
    let att = WitnessAttachment::Variable {
        name: var.to_string(),
        scope,
    };
    bag.for_attachment(&att).iter().any(|w| {
        w.span.start <= point
            && !matches!(
                &w.payload,
                WitnessPayload::Observation(
                    TypeObservation::HashRefAccess
                        | TypeObservation::ArrayRefAccess
                        | TypeObservation::CodeRefInvocation
                        | TypeObservation::NumericUse
                        | TypeObservation::StringUse
                        | TypeObservation::RegexpUse
                )
            )
    })
}

/// Pick the "where am I asking from?" `Point` for a scope-chained
/// Variable query. The scope's end position works for temporal
/// narrowing; materialize doesn't have the chasing witness's span, so
/// this is a safe approximation.
fn scope_point(scopes: &[Scope], scope: ScopeId) -> tree_sitter::Point {
    scopes
        .get(scope.0 as usize)
        .map(|s| s.span.end)
        .unwrap_or(tree_sitter::Point { row: 0, column: 0 })
}

/// Walk a `Link` chain through the conclusion maps, without decoding a bag.
///
/// `None` means the walk could not complete — a cycle, the hop cap, a file with
/// no map, or a key that resolves to `OpenNone`. Every one of those degrades to
/// the decode the caller would have done anyway, so an incomplete walk is slow
/// rather than wrong. Only a completed walk returns an answer.
///
/// The visited set is `(path, key, receiver, arity)` — the same identity
/// `VisitedKey` uses for the live chase, because a `Link` chain can revisit a
/// file under a DIFFERENT receiver and that is a distinct question, not a cycle.
fn follow_link(
    idx: &dyn crate::model::file_analysis::CrossFileLookup,
    targets: &[super::ConclusionKey],
    receiver: &Option<InferredType>,
    arity: Option<u32>,
    args: &[InferredType],
) -> Option<InferredType> {
    follow_link_with(
        &|class: &str| {
            super::session::visible_def_candidates(idx, class)
                .iter()
                .map(|c| {
                    (
                        c.path.to_string_lossy().into_owned(),
                        idx.conclusions_for(&c.path),
                    )
                })
                .collect()
        },
        targets,
        receiver,
        arity,
        args,
    )
}

/// The walk itself, over a resolver rather than the index.
///
/// Split so the traversal — visited set, ladder semantics, hop cap — can be
/// tested against hand-built maps. Testing it through `CrossFileLookup` would
/// mean standing up a whole index, which is how a walk this delicate ends up
/// exercised only by whatever the corpus happens to contain.
pub(super) fn follow_link_with(
    resolve: &dyn Fn(&str) -> Vec<(String, Option<std::sync::Arc<super::ConclusionMap>>)>,
    targets: &[super::ConclusionKey],
    receiver: &Option<InferredType>,
    arity: Option<u32>,
    args: &[InferredType],
) -> Option<InferredType> {
    // First-answer-wins over the rungs, mirroring the ladder that produced
    // them. A rung that proves `None` moves to the next; anything that cannot
    // be resolved without a bag abandons the whole walk.
    for t in targets {
        if let Some(v) = follow_one(resolve, t, receiver, arity, args) {
            return Some(v);
        }
    }
    None
}

fn follow_one(
    resolve: &dyn Fn(&str) -> Vec<(String, Option<std::sync::Arc<super::ConclusionMap>>)>,
    target: &super::ConclusionKey,
    receiver: &Option<InferredType>,
    arity: Option<u32>,
    args: &[InferredType],
) -> Option<InferredType> {
    let mut key = target.clone();
    let mut recv = receiver.clone();
    let mut ar = arity;
    let mut seen: std::collections::HashSet<(String, super::ConclusionKey, String, Option<u32>)> =
        std::collections::HashSet::new();

    for _ in 0..super::MAX_FOLLOW_HOPS {
        let super::ConclusionKey::MethodOnClass { class, .. } = &key else {
            // Only class-keyed hops can be resolved to a candidate FILE; the
            // other key shapes have no such relation to walk.
            return None;
        };
        let candidates = resolve(class);
        let mut next: Option<(super::ConclusionKey, Option<InferredType>, Option<u32>)> = None;
        for (path, map) in candidates.into_iter() {
            let ident = (path, key.clone(), format!("{recv:?}"), ar);
            if !seen.insert(ident) {
                continue;
            }
            let map = map?;
            match map.evaluate(&key, recv.as_ref(), ar, args) {
                super::Outcome::Answer(t) => return Some(t),
                // This candidate proves nothing; the ladder moves on, exactly
                // as the live chase's candidate loop does.
                super::Outcome::None => continue,
                // Unbakeable here, so the walk cannot answer without the bag.
                // Proves nothing LOCAL, which for a follow is the same licence
                // as `None`: this candidate cannot answer, the ladder moves on.
                super::Outcome::NotLocal => continue,
                super::Outcome::Decode(_) => return None,
                super::Outcome::Follow {
                    targets,
                    arity,
                    receiver,
                } => {
                    // Follow the first rung of a nested fan-out; the remaining
                    // rungs of THAT link are explored by the recursion above
                    // only if this one dead-ends, which the outer loop handles.
                    next = targets.into_iter().next().map(|t| (t, receiver, arity));
                    break;
                }
            }
        }
        let (t, r, a) = next?;
        key = t;
        recv = r;
        ar = a;
    }
    // Hop cap. Continuing costs more than the decode this is replacing.
    crate::util::ghost_stats::count("follow.hop_cap");
    None
}

#[cfg(test)]
#[path = "consult_prefilter_tests.rs"]
mod consult_prefilter_tests;
