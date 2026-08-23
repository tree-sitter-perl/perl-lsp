//! `ReducerRegistry`: registration order, the recursive edge-chasing
//! query (`query_rec`), and its cycle-guard/memo state.

use super::*;

// ---- Reducer registry ----

/// Cycle guard + result memo key for recursive bag queries, keyed by
/// `(bag_ptr, attachment, receiver_identity, arity_hint)`. Per-bag
/// entries stay separate so a legitimate cross-bag query for the same
/// attachment (the common `MethodOnClass{C, m}` jump into C's own bag)
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
/// `q.receiver` constant within one `MethodOnClass` query) still hashes
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
    memo: std::collections::HashMap<VisitedKey, std::sync::Arc<ReducedValue>>,
}

impl QueryState {
    pub(super) fn new() -> Self {
        QueryState {
            visited: std::collections::HashSet::new(),
            pins: Vec::new(),
            memo: std::collections::HashMap::new(),
        }
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
fn receiver_key(r: &Option<InferredType>) -> Option<String> {
    r.as_ref().map(|t| format!("{t:?}"))
}

/// Receiver to substitute when a chase reaches a *fresh* method dispatch
/// on `MethodOnClass{class}` (an `Edge` or `CallReturn` into a class's
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
        // MethodOnClass primary-fallback after ReturnExprReducer so
        // per-arity declarations win when one matches.
        r.register(Box::new(MethodOnClassReducer));
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
    pub fn query(&self, bag: &WitnessBag, q: &ReducerQuery) -> ReducedValue {
        let mut state = QueryState::new();
        // Is the CONCLUSION LAYER CLOSED for the shape that dominates
        // cross-file traffic? A `MethodOnClass` answer that never reads an
        // `Expr(span)` witness could be served from stored conclusions; one
        // that does needs the raw derivation, so the bag has to come along.
        // Measured at the top-level query only — inner hops are the thing
        // being counted, not separate questions.
        let top_moc = matches!(q.attachment, WitnessAttachment::MethodOnClass { .. });
        if top_moc {
            TOUCHED_EXPR.with(|c| c.set(false));
        }
        // Sole boundary where an owned `ReducedValue` is required; the
        // internal recursion threads `Arc` to avoid deep clones per hop.
        let out = (*self.query_rec(bag, q, &mut state)).clone();
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
        if let Some(cached) = state.memo.get(&key) {
            QUERY_REC_DEPTH.with(|c| c.set(c.get() - 1));
            return std::sync::Arc::clone(cached);
        }
        // `key` has two owners (the visited set, transiently; the memo,
        // for the rest of the query). Clone once for visited, then move
        // the original into the memo store below.
        if !state.visited.insert(key.clone()) {
            QUERY_REC_DEPTH.with(|c| c.set(c.get() - 1));
            return std::sync::Arc::new(ReducedValue::None);
        }
        let result = std::sync::Arc::new(self.query_rec_body(bag, q, state));
        state.visited.remove(&key);
        // Cache the off-path resolution. The query depends only on
        // `(bag, attachment, receiver-class, arity)` (all in `key`) plus
        // the static context, which is fixed for one top-level query.
        state.memo.insert(key, std::sync::Arc::clone(&result));
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

        // Inheritance + bridge fallback for `MethodOnClass{C, m}` queries
        // the local bag couldn't answer. Most cases are covered by
        // build-time edge emission (local writeback emits
        // `MethodOnClass(child, m) → Edge(MethodOnClass(parent, m))`;
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
        //      `MethodOnClass{P, m}` per parent.
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
        if let WitnessAttachment::MethodOnClass { class, name } = q.attachment {
            if let Some(ctx) = q.context {
                // (1) Cross-file primary lookup — every candidate file
                // declaring `class` (a reopened package's method lives in
                // whichever file defines it, not the name-slot winner).
                if let Some(idx) = ctx.module_index {
                    for cached in super::session::visible_def_candidates(idx, class).iter() {
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
                                    point: q.point,
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
                        // (candidate file, attachment, receiver, arity, point,
                        // framework) — the whole key — so one walk derives it
                        // once instead of once per call site.
                        if let Some(hit) =
                            super::session::candidate_answer(idx, &cached.path, q)
                        {
                            if *hit != ReducedValue::None {
                                return (*hit).clone();
                            }
                            continue;
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
                                            idx, &cached.path, q, &v,
                                        );
                                        return v;
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
                                        // Under the equivalence flag, do NOT
                                        // trust it — fall through, run the
                                        // real chase, and let the arm below
                                        // report any answer that absence
                                        // claimed did not exist.
                                        if super::trust_absent_conclusions()
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
                                                q,
                                                &ReducedValue::None,
                                            );
                                            continue;
                                        }
                                    }
                                    // Cross-file hop. Following it needs the
                                    // ladder to re-enter at another file's
                                    // map, which is the next slice; until
                                    // then it degrades to the decode it would
                                    // have done anyway — slower than it will
                                    // be, never wrong.
                                    super::Outcome::Follow { .. } => {
                                        crate::util::ghost_stats::count("consult.baked_follow_unhandled");
                                    }
                                    super::Outcome::Decode => {
                                        crate::util::ghost_stats::count("consult.baked_open");
                                    }
                                }
                            } else {
                                crate::util::ghost_stats::count("consult.not_baked");
                            }
                        }
                        crate::util::ghost_stats::count("moc.provider_fetched");
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
                            if v != ReducedValue::None {
                                v
                            } else {
                            // Fallback-on-miss (R4): the class file's method
                            // return may chain through ITS OWN imports —
                            // invisible to the raw bag, present in the
                            // enriched overlay.
                            crate::util::ghost_stats::count("consult.moc_primary");
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
                            }
                        };
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
                        super::session::remember_candidate_answer(idx, &cached.path, q, &v);
                        if v != ReducedValue::None {
                            return v;
                        }
                    }
                }
                // (2) Inheritance walk via package_parents (local ∪
                // cross-file ∪ synthetic app-surface edge — `parents_of`
                // is the single edge-injection site shared with the
                // FA-side ancestor walks).
                let parents = crate::model::file_analysis::parents_of(
                    class,
                    ctx.package_parents,
                    ctx.module_index,
                    ctx.app_surface_consumers,
                );
                for p in parents {
                    let parent_att = WitnessAttachment::MethodOnClass {
                        class: p,
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
                // declared in OTHER files bridged to `class` aren't
                // reachable via the local bag's edges nor the cross-file
                // primary (`get_cached(class)` returns the canonical class
                // file, not the bridging-plugin file). Ask each matching
                // cached entity for `Symbol(sym.id)` at arity=None —
                // bridged Methods aren't arity-discriminated.
                if let Some(idx) = ctx.module_index {
                    let mut found: Option<InferredType> = None;
                    idx.for_each_entity_bridged_to(class, &mut |_mod, cached, sym| {
                        if found.is_some() {
                            return;
                        }
                        if !matches!(
                            sym.kind,
                            crate::model::file_analysis::SymKind::Sub
                                | crate::model::file_analysis::SymKind::Method
                        ) {
                            return;
                        }
                        if &sym.name != name {
                            return;
                        }
                        // Bridged Method's return lives in the bridging file's
                        // bag — rehydrate it if evicted before querying.
                        crate::util::ghost_stats::count("moc.provider_fetched");
                        let full = idx.bag_present(cached);
                        if let Some(t) = full.symbol_return_type_via_bag(sym.id, None) {
                            found = Some(t);
                            return;
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
                        let enriched = idx.enriched_present(cached);
                        if !std::sync::Arc::ptr_eq(&enriched, &full) {
                            if let Some(t) = enriched.symbol_return_type_via_bag(sym.id, None) {
                                found = Some(t);
                            }
                        }
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
        // `$self->{conn}`). Hops (1) and (2) of the `MethodOnClass`
        // fallback above, same shared visited set; no bridge hop —
        // slot writes are real code, not plugin entities.
        if let WitnessAttachment::SlotType { class, key } = q.attachment {
            if let Some(ctx) = q.context {
                if let Some(idx) = ctx.module_index {
                    for cached in idx.visible_def_candidates(class) {
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
                                    point: q.point,
                                    framework: q.framework,
                                    arity_hint: None,
                                    receiver: q.receiver.clone(),
                                    args: q.args.clone(),
                                    context: Some(&cached_ctx),
                                };
                                (*self.query_rec(&full.witnesses, &sub_q, state)).clone()
                            };
                        crate::util::ghost_stats::count("moc.provider_fetched");
                        let full = idx.bag_present(&cached);
                        if !std::ptr::eq(bag, &full.witnesses) {
                            let v = attempt(&full, state);
                            crate::util::ghost_stats::count(if v == ReducedValue::None {
                                "moc.provider_no_answer"
                            } else {
                                "moc.provider_answered"
                            });
                            if v != ReducedValue::None {
                                return v;
                            }
                            // Fallback-on-miss (R4), symmetric with the
                            // MethodOnClass primary: a slot WRITE typed only in
                            // C's enriched copy resolves here. Today SlotType
                            // seeds are build-gated on a resolvable RHS
                            // (`builder.rs`), so a seed that exists already
                            // answers on the raw bag and this retry is the
                            // forward-looking twin — live the moment slot
                            // seeding emits an unconditional edge. Pin the
                            // enriched Arc: this chase threads the SHARED
                            // QueryState, whose memo keys on bag pointers.
                            crate::util::ghost_stats::count("consult.slot_type");
                            let enriched = idx.enriched_present(&cached);
                            if !std::sync::Arc::ptr_eq(&enriched, &full)
                                && !std::ptr::eq(bag, &enriched.witnesses)
                            {
                                state.pins.push(std::sync::Arc::clone(&enriched));
                                let v = attempt(&enriched, state);
                                if v != ReducedValue::None {
                                    return v;
                                }
                            }
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
        // hop (1) of the `MethodOnClass` fallback, same shared visited set.
        // Failing that, an unresolved alias IS a type of that name: the
        // one-alias-graph terminal (`ClassName(name)`), so a plain struct
        // tag / unknown class / primitive spelling resolves to itself.
        if let WitnessAttachment::TypeName(name) = q.attachment {
            if let Some(ctx) = q.context {
                if let Some(idx) = ctx.module_index {
                    for cached in idx.visible_def_candidates(name) {
                        crate::util::ghost_stats::count("moc.provider_fetched");
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
                                point: q.point,
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
    fn materialize(
        &self,
        bag: &WitnessBag,
        q: &ReducerQuery,
        state: &mut QueryState,
    ) -> Vec<Witness> {
        let raw = bag.for_attachment(q.attachment);
        let mut out: Vec<Witness> = Vec::with_capacity(raw.len());
        for w in raw {
            match &w.payload {
                WitnessPayload::Edge(target) => {
                    let resolved = match (target, q.context) {
                        (
                            WitnessAttachment::Variable { name, scope },
                            Some(ctx),
                        ) => {
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
                            self.query_variable_with_visited(
                                bag, ctx, name, *scope, point,
                                q.receiver.as_ref(), state,
                            )
                        }
                        _ => {
                            // A `MethodOnClass{class,..}` reached through an edge is
                            // a fresh method dispatch: its receiver is that call's
                            // invocant (`class`), so a fluent `ReturnExpr(Receiver)`
                            // substitutes the dispatch class — not whatever the outer
                            // query carried. Mirrors `query_sub_return_type`'s
                            // `effective_receiver`. The exception is an inheritance
                            // hop (`MethodOnClass{child} → Edge(MethodOnClass{parent})`):
                            // there the source is itself a `MethodOnClass`, and the
                            // child's receiver must carry through so an inherited fluent
                            // accessor returns the child, not where `has` was declared.
                            let receiver = match target {
                                WitnessAttachment::MethodOnClass { class, .. }
                                    if !matches!(
                                        q.attachment,
                                        WitnessAttachment::MethodOnClass { .. }
                                    ) =>
                                {
                                    fresh_dispatch_receiver(&q.receiver, class, q.context)
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
                            match &*self.query_rec(bag, &sub_q, state) {
                                ReducedValue::Type(t) => Some(t.clone()),
                                ReducedValue::FactMap(_)
                                | ReducedValue::None => None,
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
                    // a `MethodOnClass`) so a fluent `Receiver` substitutes
                    // it; the arity is the call site's, NOT the outer
                    // query's — that's the whole point of this variant.
                    let receiver = match target {
                        WitnessAttachment::MethodOnClass { class, .. } => {
                            fresh_dispatch_receiver(&q.receiver, class, q.context)
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
                    match &*self.query_rec(bag, &sub_q, state) {
                        ReducedValue::Type(t) => out.push(Witness {
                            attachment: w.attachment.clone(),
                            source: w.source.clone(),
                            payload: WitnessPayload::InferredType(t.clone()),
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
                    let base_t = match (base, q.context) {
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
                    };
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
                                    match &*self.query_rec(bag, &sub_q, state) {
                                        ReducedValue::Type(t) => Some(t.clone()),
                                        ReducedValue::FactMap(_)
                                        | ReducedValue::None => None,
                                    }
                                })
                            }
                            ProjectionStep::ArrayIndex(i) => t.element_at(*i).cloned(),
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
                    match &*self.query_rec(bag, &sub_q, state) {
                        ReducedValue::Type(t) => out.push(Witness {
                            attachment: w.attachment.clone(),
                            source: w.source.clone(),
                            payload: WitnessPayload::InferredType(t.clone()),
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
