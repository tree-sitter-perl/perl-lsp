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
///
/// A `Variable` is temporal — its reducer answers AT `q.point` — so its key
/// carries the point: `$x ||= …` reads `$x` before the write the outer query
/// is resolving, which is a different question, not a cycle. Every other
/// attachment ignores the point and keeps it out of the key, so the memo
/// still collapses their diamonds.
use super::engine::{EngineKey, EngineValue, JoinKey, JoinWant};

/// The hash the arguments from `from` on spell as key/value pairs: a literal
/// word in key position names a key, the value after it types it. Anything
/// the pairs can't name (a computed key, a spread, a dangling key) leaves
/// the hash open.
fn pairs_type(
    args: &[CallArg],
    from: usize,
    type_of: &mut dyn FnMut(&WitnessAttachment) -> Option<InferredType>,
) -> InferredType {
    let mut keys: Vec<(String, Option<Box<InferredType>>)> = Vec::new();
    let mut open = false;
    let mut set = |keys: &mut Vec<(String, Option<Box<InferredType>>)>, word: &str, t: Option<Box<InferredType>>| {
        match keys.iter_mut().find(|(k, _)| k == word) {
            Some(slot) => slot.1 = t,
            None => keys.push((word.to_string(), t)),
        }
    };
    let rest = args.get(from..).unwrap_or_default();
    let mut i = 0;
    while i < rest.len() {
        // A spread hash (`%args`, a forwarded `@_`) lays its known keys in
        // place, later keys winning as Perl's list assignment does.
        let spread = match &rest[i] {
            CallArg::Spread(att) => type_of(att),
            CallArg::Forward { package, name, from, method } => type_of(&WitnessAttachment::ParamPairs {
                package: package.clone(),
                name: name.clone(),
                from: *from,
                method: *method,
            }),
            _ => None,
        };
        if matches!(rest[i], CallArg::Spread(_) | CallArg::Forward { .. }) {
            match spread {
                Some(InferredType::HashWithKeys { keys: spread_keys, open: o }) => {
                    for (k, t) in spread_keys.iter() {
                        set(&mut keys, k, t.clone());
                    }
                    open |= o;
                    i += 1;
                    continue;
                }
                _ => {
                    open = true;
                    break;
                }
            }
        }
        match (&rest[i], rest.get(i + 1)) {
            (CallArg::Word { word, .. }, Some(CallArg::One(v) | CallArg::Word { at: v, .. })) => {
                let t = type_of(v).map(Box::new);
                set(&mut keys, word, t);
                i += 2;
            }
            _ => {
                open = true;
                break;
            }
        }
    }
    InferredType::HashWithKeys { keys: crate::model::file_analysis::SharedKeys::new(keys), open }
}

/// Class order for a caller join: the caller file's own parents, then the
/// index's.
struct CallerOrder<'a> {
    local: &'a dyn crate::model::file_analysis::LocalParents,
    idx: &'a dyn crate::model::file_analysis::CrossFileLookup,
}

impl crate::model::file_analysis::ClassOrder for CallerOrder<'_> {
    fn common_class(&self, a: &str, b: &str) -> Option<String> {
        crate::model::file_analysis::least_common_class(a, b, self.local, Some(self.idx))
    }
}

/// The attachment an argument list holds at `i`, when nothing before it
/// spreads (a `Spread` makes every later position unknown; a `Forward` hands
/// the position to the forwarding sub's own parameter).
fn positional_arg(args: Option<&[CallArg]>, i: u32) -> Option<WitnessAttachment> {
    let args = args?;
    for (k, a) in args.iter().enumerate() {
        match a {
            CallArg::Spread(_) => return None,
            CallArg::Forward { package, name, from, method } => {
                return Some(WitnessAttachment::ParamIn {
                    package: package.clone(),
                    name: name.clone(),
                    index: from + (i - k as u32),
                    method: *method,
                })
            }
            CallArg::One(t) | CallArg::Word { at: t, .. } if k as u32 == i => return Some(t.clone()),
            CallArg::One(_) | CallArg::Word { .. } => {}
        }
    }
    None
}

type VisitedKey = (usize, WitnessAttachment, Option<String>, Option<u32>, Option<Point>);
type VisitedSet = std::collections::HashSet<VisitedKey>;

/// Whether a consult candidate is the file whose bag is being chased. The
/// query's origin answers by path; only an origin-less query (a root read
/// with no file identity) pays a bag fetch to compare identities.
fn consults_self(
    bag: &WitnessBag,
    q: &ReducerQuery,
    idx: &dyn crate::model::file_analysis::CrossFileLookup,
    cached: &std::sync::Arc<crate::model::file_analysis::CachedModule>,
) -> bool {
    match q.context.and_then(|c| c.origin) {
        Some(origin) => origin == cached.path.as_path(),
        None => {
            crate::util::ghost_stats::count("consult.self_fetch");
            let full = super::engine::untracked(|| idx.bag_present(cached));
            std::ptr::eq(bag, &full.witnesses)
        }
    }
}

fn visited_key(bag: &WitnessBag, q: &ReducerQuery) -> VisitedKey {
    let point = match q.attachment {
        WitnessAttachment::Variable { .. } => q.point,
        _ => None,
    };
    (
        bag as *const _ as usize,
        q.attachment.clone(),
        receiver_key(&q.receiver),
        q.arity_hint,
        point,
    )
}

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
/// `#[inline(never)]` keeps `query_rec`'s frame small: it recurses once per MRO
/// hop, so a temporary built inline is paid once per live hop. Stack growth
/// keeps a big frame from overflowing, but every extra KiB per hop is another
/// segment a deep chase has to map. Temporaries belong in a callee's frame,
/// not in one that is live hundreds deep.
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

/// `(value, recorded_exit, cut)` — see `QueryState::memo`.
type MemoEntry = (std::sync::Arc<ReducedValue>, bool, bool);

/// What a fold-memo answer depends on beyond the per-query key and the
/// generations the memo holds: the framework and arguments the per-query memo
/// holds fixed for one query, and which scope table the context reads.
type FoldMemoKey = (VisitedKey, FrameworkFact, Option<String>, usize);

/// The answers for one generation of the bag and of the parent graph.
/// Generations never repeat, so when either moves on every entry is dead;
/// holding only the current one keeps the memo bounded by the queries between
/// two pushes, not by the whole build.
#[derive(Default)]
struct FoldMemo {
    generation: u64,
    parents_generation: u64,
    answers: std::collections::HashMap<FoldMemoKey, (std::sync::Arc<ReducedValue>, bool)>,
}

thread_local! {
    /// The build's memo, shared by every top-level query while a
    /// `FoldMemoScope` is open. `None` outside a build.
    static FOLD_MEMO: std::cell::RefCell<Option<FoldMemo>> = const { std::cell::RefCell::new(None) };
}

/// Shares resolved sub-answers across the top-level queries of one build.
///
/// The fold asks the same questions many times: every pass, every iteration,
/// and every call in a chain re-types the chain's prefix. Within a build the
/// answers can only change when the bag or the parent graph does, and
/// build-time queries run with no module index, so nothing outside the file
/// can move them. Any write to either moves its generation and empties the
/// memo, and only subtrees that were never cut short are stored.
pub(crate) struct FoldMemoScope {
    outer: Option<FoldMemo>,
}

impl FoldMemoScope {
    /// `PERL_LSP_NO_FOLD_MEMO=1` leaves the scope closed: the A/B control,
    /// and the check that the memo changes no answer.
    pub(crate) fn enter() -> Self {
        let fresh = (!build_memos_disabled()).then(FoldMemo::default);
        let outer = FOLD_MEMO.with(|m| std::mem::replace(&mut *m.borrow_mut(), fresh));
        FoldMemoScope { outer }
    }
}

/// `PERL_LSP_NO_FOLD_MEMO=1` (or `without_fold_memo` in tests) turns off
/// every memo that shares answers across a build's queries, as one switch.
pub(crate) fn build_memos_disabled() -> bool {
    static DISABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let disabled = *DISABLED.get_or_init(|| std::env::var_os("PERL_LSP_NO_FOLD_MEMO").is_some());
    #[cfg(test)]
    let disabled = disabled || FOLD_MEMO_FORCED_OFF.with(|c| c.get());
    disabled
}

#[cfg(test)]
thread_local! {
    static FOLD_MEMO_FORCED_OFF: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with the fold memo off — the control arm of the equivalence net.
#[cfg(test)]
pub(crate) fn without_fold_memo<R>(f: impl FnOnce() -> R) -> R {
    let prev = FOLD_MEMO_FORCED_OFF.with(|c| c.replace(true));
    let out = f();
    FOLD_MEMO_FORCED_OFF.with(|c| c.set(prev));
    out
}

/// `PERL_LSP_FOLD_MEMO_EQUIV=1 cargo test` rebuilds every file the suite
/// touches with the memo off and asserts the analyses agree.
#[cfg(test)]
pub(crate) fn fold_memo_equiv_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("PERL_LSP_FOLD_MEMO_EQUIV").as_deref() == Ok("1"))
}

impl Drop for FoldMemoScope {
    fn drop(&mut self) {
        let outer = self.outer.take();
        FOLD_MEMO.with(|m| *m.borrow_mut() = outer);
    }
}

/// True while a `FoldMemoScope` is open with the memo on. Build-time memos
/// that live beside it ride the same scope, kill switch and equivalence net.
pub(crate) fn fold_memo_open() -> bool {
    FOLD_MEMO.with(|m| m.borrow().is_some())
}

/// Answer `q` from the fold memo, or hand back the generations and key to
/// store under once it is computed (`None` when no scope is open, or when the answer may depend
/// on more than this file: a query that can reach the module index is never
/// shared).
///
/// Out of line and boxed for the same reason as `note_moc_exit`: `query_rec`
/// is live hundreds of frames deep, and the key's temporaries would grow every
/// one of them.
#[inline(never)]
fn fold_memo_probe(
    bag: &WitnessBag,
    q: &ReducerQuery,
    key: &VisitedKey,
    state: &mut QueryState,
) -> Result<std::sync::Arc<ReducedValue>, Option<Box<(FoldMemoStamp, FoldMemoKey)>>> {
    if !FOLD_MEMO.with(|m| m.borrow().is_some()) {
        return Err(None);
    }
    let scopes = match q.context {
        Some(ctx) if ctx.module_index.is_some() => return Err(None),
        Some(ctx) => ctx.scopes.as_ptr() as usize,
        None => 0,
    };
    let args = (!q.args.is_empty()).then(|| format!("{:?}", q.args));
    let stamp = FoldMemoStamp {
        generation: bag.generation(),
        parents_generation: q.context.map(|ctx| ctx.package_parents.generation()),
    };
    let fk = (key.clone(), q.framework, args, scopes);
    let hit = FOLD_MEMO.with(|m| {
        let mut m = m.borrow_mut();
        let memo = m.as_mut()?;
        if !stamp.matches(memo) {
            memo.answers.clear();
            memo.generation = stamp.generation;
            if let Some(parents_generation) = stamp.parents_generation {
                memo.parents_generation = parents_generation;
            }
            return None;
        }
        memo.answers.get(&fk).cloned()
    });
    let Some((cached, recorded_exit)) = hit else {
        return Err(Some(Box::new((stamp, fk))));
    };
    crate::util::ghost_stats::count("fold_memo.hit");
    // The same laundering guard as the per-query memo.
    if recorded_exit && state.opaque_frames > 0 {
        crate::util::ghost_stats::count("residual.memo_under_opaque");
        state.poisoned = true;
    }
    state.memo.insert(key.clone(), (std::sync::Arc::clone(&cached), recorded_exit, false));
    Ok(cached)
}

/// The generations a probe saw. A query without a context reads no parent
/// graph, so it leaves the memo's parent generation alone.
struct FoldMemoStamp {
    generation: u64,
    parents_generation: Option<u64>,
}

impl FoldMemoStamp {
    fn matches(&self, memo: &FoldMemo) -> bool {
        memo.generation == self.generation
            && self.parents_generation.map_or(true, |p| p == memo.parents_generation)
    }
}

#[inline(never)]
fn fold_memo_store(fk: Box<(FoldMemoStamp, FoldMemoKey)>, result: &std::sync::Arc<ReducedValue>, recorded_exit: bool) {
    let (stamp, fk) = *fk;
    FOLD_MEMO.with(|m| {
        if let Some(memo) = m.borrow_mut().as_mut().filter(|memo| stamp.matches(memo)) {
            crate::util::ghost_stats::count("fold_memo.store");
            memo.answers.insert(fk, (std::sync::Arc::clone(result), recorded_exit));
        }
    });
}

pub(super) struct QueryState {
    visited: VisitedSet,
    /// Other files' copies consulted during this query — pinned so memo
    /// entries keyed on their bag ADDRESSES stay valid even if the LRU
    /// drops its own reference mid-query.
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
    ///
    /// The second flag is "this subtree was cut short": a cycle guard returned
    /// for an on-path key, or the depth cap fired, somewhere beneath it. Such a
    /// value depends on the path it was reached from, so it may be reused
    /// inside this query (the memo always has) but never by another one.
    memo: std::collections::HashMap<VisitedKey, MemoEntry>,
    /// Cuts seen so far in this query; read as a difference across a subtree.
    cuts: u64,
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
    /// The calls the chase is inside, innermost last: a parameter of the
    /// callee reads the argument its own call site passes before it reads
    /// the join over every caller.
    call_frames: Vec<CallFrame>,
}

/// One call the chase descended through: the callee's name and what that
/// call site passes.
struct CallFrame {
    name: String,
    /// Whether the call dispatched as a method: its `@_` then heads with
    /// the receiver, so positions only line up with a method's view.
    method: bool,
    receiver: Option<InferredType>,
    args: Vec<InferredType>,
}

impl QueryState {
    pub(super) fn new() -> Self {
        QueryState {
            visited: std::collections::HashSet::new(),
            pins: Vec::new(),
            memo: std::collections::HashMap::new(),
            cuts: 0,
            residual: Vec::new(),
            poisoned: false,
            opaque_frames: 0,
            call_frames: Vec::new(),
        }
    }

    /// Run `f` inside the call `frame`. Answers computed in it depend on
    /// what the call passes, so they get a memo of their own and count as
    /// cut: no store outside this query keeps them.
    fn in_call_frame<T>(&mut self, frame: CallFrame, f: impl FnOnce(&mut Self) -> T) -> T {
        self.call_frames.push(frame);
        let outer = std::mem::take(&mut self.memo);
        let out = f(self);
        self.memo = outer;
        self.call_frames.pop();
        out
    }

    /// The argument the innermost call passes at `@_` position `index`, when
    /// that call is to `name` and the argument is typed.
    fn frame_argument(&mut self, name: &str, method: bool, index: u32) -> Option<InferredType> {
        let frame = self.call_frames.last().filter(|f| f.name == name && f.method == method)?;
        let t = match (method, index) {
            (true, 0) => frame.receiver.clone(),
            (true, i) => frame.args.get(i as usize - 1).cloned(),
            (false, i) => frame.args.get(i as usize).cloned(),
        }
        .filter(InferredType::is_known)?;
        self.cuts += 1;
        Some(t)
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
    let fa = idx.facts_present(cached);
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

/// Lay the attachment's `BrandOverlay` marks over its reduced value. After
/// reduction rather than inside a reducer, because marks ride whatever
/// answered: an `Invoke`, an edge, a plain type.
#[inline(never)]
fn apply_brand_overlays(bag: &WitnessBag, q: &ReducerQuery, v: ReducedValue) -> ReducedValue {
    let ReducedValue::Type(mut t) = v else { return v };
    for w in bag.iter_attachment(q.attachment) {
        if let WitnessPayload::BrandOverlay { ns, on_class, set, drop } = &w.payload {
            let applies = t.class_name().is_some_and(|c| {
                c == on_class || q.context.is_some_and(|ctx| is_subclass_of(c, on_class, ctx))
            });
            if applies {
                t = t.overlay(ns, set, drop);
            }
        }
    }
    ReducedValue::Type(t)
}

fn receiver_key(r: &Option<InferredType>) -> Option<String> {
    r.as_ref().map(|t| format!("{t:?}"))
}

/// Receiver to substitute when a chase reaches a *fresh* method dispatch
/// on `PackageSymbol{package}` (an `Edge` into a class's
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
pub(super) fn fresh_dispatch_receiver(
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
/// the primary cycle guard; this cap bounds the WORK a new, unaccounted-for
/// recursion shape can do (the stack itself grows, see below). On hit, warn once
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
/// **A budget, not a stack guard.** `query_rec` grows its stack through
/// [`with_stack_headroom`], so how deep a chase can go does not depend on the
/// thread's stack size or the build profile — one value serves both.
const QUERY_REC_DEPTH_CAP: u32 = 512;

/// Headroom a recursive resolver keeps before it switches to a fresh stack
/// segment. One `query_rec` hop costs about 5 KiB in a debug build and under
/// 1 KiB in release, so this is many hops of margin.
const STACK_RED_ZONE: usize = 64 * 1024;
/// Size of each segment mapped once the red zone is reached. It is unmapped
/// when the recursion unwinds past it; a larger segment means a deep chase
/// crosses into a new one less often.
const STACK_SEGMENT: usize = 1024 * 1024;

/// Runs `f`, first moving onto a fresh stack segment if this thread is within
/// `STACK_RED_ZONE` of the end of its stack. Every recursive resolver calls
/// this at its single re-entry point, so its depth cap is a work budget and
/// not a function of the thread's stack size or the build profile. `f` runs on
/// the same thread, so thread-locals are unaffected.
pub(crate) fn with_stack_headroom<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(STACK_RED_ZONE, STACK_SEGMENT, f)
}

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

/// How many times `QUERY_REC_DEPTH_CAP` has fired on this thread. Read as a
/// difference across a region: a rise means an answer computed inside it
/// depended on how deep the region was entered.
pub(crate) fn query_rec_truncations() -> u64 {
    QUERY_REC_TRUNCATIONS.with(|c| c.get())
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
        r.register(Box::new(InstanceShapeFold));
        r.register(Box::new(SlotElementsFold));
        r.register(Box::new(ParamWritesFold));
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
        // The two `Field{..}` reducers claim disjoint PAYLOADS on one
        // attachment: FieldValueReducer the slot's materialized value edge
        // (what a `ValueHop` reads), DomainCoherenceFold the `DomainCompare`
        // vote. Value first is load-bearing — the domain is a human-surface
        // refinement and must never be the type that flows.
        r.register(Box::new(FieldValueReducer));
        r.register(Box::new(DomainCoherenceFold));
        // `Param{..}` is a disjoint attachment shape; order isn't load-bearing.
        r.register(Box::new(ParamBindingReducer));
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
        with_stack_headroom(|| self.query_rec_on_stack(bag, q, state))
    }

    fn query_rec_on_stack(
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
        if crate::util::ghost_stats::enabled() {
            for w in bag.iter_attachment(&q.attachment) {
                crate::util::ghost_stats::count(match &w.payload {
                    WitnessPayload::Observation(_) => "hop.OBSERVATION",
                    WitnessPayload::InferredType(_) => "hop.inferred_type",
                    WitnessPayload::Edge(_) => "hop.edge",
                    WitnessPayload::Invoke { .. } => "hop.invoke",
                    WitnessPayload::ReturnExpr(_) => "hop.return_expr",
                    WitnessPayload::Fact { .. } => "hop.fact",
                    WitnessPayload::Derivation => "hop.derivation",
                    WitnessPayload::Custom { .. } => "hop.custom",
                    WitnessPayload::Projected { .. } => "hop.projected",
                    _ => "hop.other",
                });
            }
        }
        if matches!(q.attachment, WitnessAttachment::Expr(_)) {
            TOUCHED_EXPR.with(|c| c.set(true));
            // WHY the chase needs the raw derivation here. If these land in a
            // few recurring payload shapes, each is a candidate for a
            // PARAMETERISED conclusion (`ReturnExpr::Receiver` already is
            // one — "returns its invocant", a function of the query rather
            // than a value). If they are spread across everything, the
            // derivation is genuinely open and no conclusion layer closes it.
            if crate::util::ghost_stats::enabled() {
                for w in bag.iter_attachment(&q.attachment) {
                    crate::util::ghost_stats::count(match &w.payload {
                        WitnessPayload::InferredType(_) => "expr_hop.inferred_type",
                        WitnessPayload::Observation(_) => "expr_hop.observation",
                        WitnessPayload::Edge(_) => "expr_hop.edge",
                        WitnessPayload::Invoke { .. } => "expr_hop.invoke",
                        WitnessPayload::ReturnExpr(_) => "expr_hop.return_expr",
                        WitnessPayload::Fact { .. } => "expr_hop.fact",
                        WitnessPayload::Derivation => "expr_hop.derivation",
                        WitnessPayload::Custom { .. } => "expr_hop.custom",
                        WitnessPayload::Projected { .. } => "expr_hop.projected",
                        _ => "expr_hop.other",
                    });
                }
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
        let key = visited_key(bag, q);
        // Memo hit: this key was fully resolved earlier in THIS query and
        // isn't on the current path (cycle guard handles on-path keys).
        if let Some((cached, recorded_exit, cut)) = state.memo.get(&key) {
            let cached = std::sync::Arc::clone(cached);
            if *cut {
                state.cuts += 1;
            }
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
        let fold_key = match fold_memo_probe(bag, q, &key, state) {
            Ok(cached) => {
                QUERY_REC_DEPTH.with(|c| c.set(c.get() - 1));
                return cached;
            }
            Err(fold_key) => fold_key,
        };
        // `key` has two owners (the visited set, transiently; the memo,
        // for the rest of the query). Clone once for visited, then move
        // the original into the memo store below.
        if !state.visited.insert(key.clone()) {
            state.cuts += 1;
            QUERY_REC_DEPTH.with(|c| c.set(c.get() - 1));
            return std::sync::Arc::new(ReducedValue::None);
        }
        let exits_before = state.residual.len();
        let poison_before = state.poisoned;
        let cuts_before = state.cuts;
        let truncations_before = QUERY_REC_TRUNCATIONS.with(|c| c.get());
        let value = match apply_brand_overlays(bag, q, self.query_rec_body(bag, q, state)) {
            ReducedValue::Type(t) if q.attachment.fed_by_callers() => ReducedValue::Type(t.opened()),
            v @ (ReducedValue::Type(_) | ReducedValue::FactMap(_) | ReducedValue::None) => v,
        };
        let result = std::sync::Arc::new(value);
        let recorded_exit =
            state.residual.len() > exits_before || (state.poisoned && !poison_before);
        let cut = state.cuts > cuts_before
            || QUERY_REC_TRUNCATIONS.with(|c| c.get()) > truncations_before;
        state.visited.remove(&key);
        if let (Some(fk), false) = (fold_key, cut) {
            fold_memo_store(fk, &result, recorded_exit);
        }
        // Cache the off-path resolution. The query depends only on
        // `(bag, attachment, receiver-class, arity)` (all in `key`) plus
        // the static context, which is fixed for one top-level query.
        state.memo.insert(key, (std::sync::Arc::clone(&result), recorded_exit, cut));
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
                        // bag, and may chain through that file's OWN imports —
                        // so the hop carries the index and shares this query's
                        // state: mutual bridges cut on the visited set instead
                        // of spawning a fresh guard per hop.
                        crate::util::ghost_stats::count("moc.provider_fetched");
                        crate::util::ghost_stats::count("mocsite.bridged");
                        let full = idx.bag_present(cached);
                        match self.query_symbol_in(idx, &full, sym.id, q, state) {
                            ReducedValue::Type(t) => {
                                found = Some(t);
                                return ControlFlow::Break(());
                            }
                            ReducedValue::FactMap(_) | ReducedValue::None => {}
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
            if let Some(t) = self.slot_from_instance_shape(bag, q, state, class, key) {
                return ReducedValue::Type(t);
            }
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
                        if let Some((hit, captured)) =
                            super::session::sweep_answer(idx, &cached.path, &verdict_key)
                        {
                            super::session::remember_candidate_answer(
                                idx, &cached.path, &memo_q, &hit, captured,
                            );
                            if *hit != ReducedValue::None {
                                return (*hit).clone();
                            }
                            continue;
                        }
                        // Both memo tiers missed: what this candidate's
                        // verdict reads is captured for them to replay.
                        let capture = super::engine::CaptureGuard::open();
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
                                    &ReducedValue::None, capture.captured(),
                                );
                                idx.remember_sweep_consult(
                                    &cached.path,
                                    &verdict_key,
                                    &ReducedValue::None, capture.captured(),
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
                                    origin: Some(&cached.path),
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
                        // Untracked: the copy only tells whether the candidate
                        // is this very bag; its answer is read through the
                        // consult key, which records the file itself.
                        if !consults_self(bag, q, idx, &cached) {
                            let v = self
                                .consult_by_key(idx, cached, q, None)
                                .unwrap_or_else(|| {
                                    let full = super::engine::untracked(|| idx.bag_present(&cached));
                                    attempt(&full, state)
                                });
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
                                    idx, &cached.path, &memo_q, &v, capture.captured(),
                                );
                                idx.remember_sweep_consult(&cached.path, &verdict_key, &v, capture.captured());
                                return v;
                            }
                            super::session::remember_candidate_answer(
                                idx,
                                &cached.path,
                                &memo_q,
                                &ReducedValue::None, capture.captured(),
                            );
                            idx.remember_sweep_consult(
                                &cached.path, &verdict_key, &ReducedValue::None, capture.captured(),
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
                    for cached in idx.visible_def_candidates(name).read() {
                        crate::util::ghost_stats::count("moc.provider_fetched");
                        crate::util::ghost_stats::count("mocsite.type_name");
                        if !consults_self(bag, q, idx, &cached) {
                            let full = idx.bag_present(&cached);
                            let cached_ctx = BagContext {
                                scopes: &full.scopes,
                                package_framework: &full.packages,
                                module_index: Some(idx),
                                package_parents: &full.packages,
                                app_surface_consumers: &full.plugin.app_surface_consumers,
                                origin: Some(&cached.path),
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

        // `Field{owner, name}` the local bag couldn't answer: the slot's
        // value edge lives in the file declaring `owner` (cross-file
        // primary), or the slot is a PARENT's (ancestry) — hops (1) and (2)
        // of the `PackageSymbol` ladder, same shared visited set. No bridge
        // hop: a plugin entity is a callable, never a stored value.
        if let WitnessAttachment::Field { owner, name } = q.attachment {
            if let Some(v) = self.owner_keyed_fallback(bag, q, state, owner, "field", None, &|p| {
                WitnessAttachment::Field { owner: p, name: name.clone() }
            }) {
                return v;
            }
        }

        // `Param{package, name, index}`: the callee's bag is the authority.
        // Its own witness present but untyped → the position IS bound, so
        // `Unknown` (a value flowed, nothing named it) rather than silence —
        // silence would read as by-value. Absent locally → the callee lives
        // elsewhere (cross-file primary) or the position is a parent's
        // (ancestry): the `Field` ladder, same shared visited set.
        if let WitnessAttachment::Param { package, name, index } = q.attachment {
            if !bag.for_attachment(q.attachment).is_empty() {
                return ReducedValue::Type(InferredType::Unknown);
            }
            if let Some(v) = self.owner_keyed_fallback(bag, q, state, package, "param", None, &|p| {
                WitnessAttachment::Param { package: p, name: name.clone(), index: *index }
            }) {
                return v;
            }
        }

        if let WitnessAttachment::SlotElements { class, key } = q.attachment {
            let provider = EngineKey::Providers(q.attachment.clone());
            if let Some(v) = self.owner_keyed_fallback(bag, q, state, class, "slot_elements", Some(&provider), &|p| {
                WitnessAttachment::SlotElements { class: p, key: key.clone() }
            }) {
                return v;
            }
        }

        if let WitnessAttachment::ParamWrites {
            package,
            name,
            index,
            method,
        } = q.attachment
        {
            let provider = EngineKey::Providers(q.attachment.clone());
            if let Some(v) = self.owner_keyed_fallback(
                bag,
                q,
                state,
                package,
                "param_writes",
                Some(&provider),
                &|p| WitnessAttachment::ParamWrites {
                    package: p,
                    name: name.clone(),
                    index: *index,
                    method: *method,
                },
            ) {
                return v;
            }
        }

        if let WitnessAttachment::CallbackIn { .. } = q.attachment {
            if let Some(v) = self.callback_in_fallback(bag, q, state) {
                return v;
            }
        }

        if let WitnessAttachment::ImportedSub { name } = q.attachment {
            if let Some(v) = self.imported_sub_fallback(q, state, name) {
                return v;
            }
        }

        if let WitnessAttachment::ParamIn { package, name, index, method } = q.attachment {
            if let Some(v) = self.param_in_fallback(q, state, package, name, *method, JoinWant::At(*index)) {
                return v;
            }
        }

        if let WitnessAttachment::ParamPairs { package, name, from, method } = q.attachment {
            if let Some(v) = self.param_in_fallback(q, state, package, name, *method, JoinWant::PairsFrom(*from)) {
                return v;
            }
        }

        ReducedValue::None
    }

    /// `ParamIn{package, name, index}` the callee's bag can't answer: the
    /// join of the argument over the sub's callers (`param_in_join`), an
    /// engine answer; a parameter whose callers pass it back to itself cuts
    /// on the engine's open-key path.
    fn param_in_fallback(
        &self,
        q: &ReducerQuery,
        state: &mut QueryState,
        package: &str,
        name: &str,
        method: bool,
        want: JoinWant,
    ) -> Option<ReducedValue> {
        if let JoinWant::At(index) = want {
            if let Some(t) = state.frame_argument(name, method, index) {
                crate::util::ghost_stats::count("param_in.from_call_site");
                return Some(ReducedValue::Type(t));
            }
        }
        let ctx = q.context?;
        let Some(idx) = ctx.module_index else {
            super::note_bake_exit("param_in", true);
            return None;
        };
        let join = JoinKey { package: package.to_string(), name: name.to_string(), method, want };
        if super::engine::deeper_position_of_open_join(&join) {
            crate::util::ghost_stats::count("param_in.forward_cut");
            return Some(ReducedValue::None);
        }
        Some(super::engine::answer(idx, &EngineKey::Join(join)).reduced())
    }

    /// A slot no write types may still arrive with the instance: the hash
    /// its constructor spreads into the blessed literal (`InstanceShape`).
    /// Out of line so the recursive body's frame doesn't carry it.
    #[inline(never)]
    fn slot_from_instance_shape(
        &self,
        bag: &WitnessBag,
        q: &ReducerQuery,
        state: &mut QueryState,
        class: &str,
        key: &str,
    ) -> Option<InferredType> {
        let shape = WitnessAttachment::InstanceShape { class: class.to_string() };
        if bag.for_attachment(&shape).is_empty() {
            return None;
        }
        let sub_q = ReducerQuery {
            attachment: &shape,
            point: None,
            framework: q.framework,
            arity_hint: None,
            receiver: None,
            args: Vec::new(),
            context: q.context,
        };
        state.in_opaque_frame(|state| match &*self.query_rec(bag, &sub_q, state) {
            ReducedValue::Type(t) => t.key_value_type(key).flatten().cloned(),
            ReducedValue::FactMap(_) | ReducedValue::None => None,
        })
    }

    /// The join of one parameter over the call sites of its sub: one engine
    /// answer per caller file (`param_contribution`), folded. The mentions
    /// relation is the ref rows (`ref_candidate_paths`), plus every resident
    /// file the rows don't cover. Agreeing answers fold to one; a file whose
    /// call sites disagree, or two files that do, answer nothing.
    pub(super) fn param_in_join(&self, idx: &dyn crate::model::file_analysis::CrossFileLookup, join: &JoinKey) -> EngineValue {
        let key = crate::model::conventions::name_match_key(&join.name, &crate::model::conventions::PERL_SPELLINGS);
        // A sub's callers call it as a function; a method's spell any ref.
        let kind = if join.method { super::engine::MentionKind::Any } else { super::engine::MentionKind::Calls };
        let mut paths = (*super::engine::mentions_of(idx, kind, &key).read()).clone();
        // A method's caller names the callee's family somewhere in its file:
        // the receiver it spells or the package it declares. Reading only
        // those files keeps a name every file mentions (`new`, `Get`) from
        // sweeping the index once per class. A sub's callers are already
        // keyed by the package their call resolves to; a qualified call
        // (`Pkg::f()`) names no family member, so subs are not narrowed.
        // TODO(caller-narrowing): a method caller whose receiver is untyped
        // in a file that never names the family is left out of the join.
        // A family too large to enumerate reads every mention instead.
        if join.method {
            if let Some(family) = super::engine::class_family_mentions(idx, &join.package) {
                paths.retain(|p| family.contains(p));
            }
        }
        crate::util::ghost_stats::count("param_in.join");
        crate::util::ghost_stats::count_by("param_in.files", paths.len() as u64);
        // Across files the class order is the index's alone: no one caller's
        // own parents decide between two others' answers.
        let no_local: std::collections::HashMap<String, crate::model::file_analysis::PackageFacts> =
            std::collections::HashMap::new();
        let order = CallerOrder { local: &no_local, idx };
        let mut joined: Option<InferredType> = None;
        for path in paths {
            let part = super::engine::answer(idx, &EngineKey::Contribution { join: join.clone(), file: path.clone() });
            if crate::util::ghost_stats::probe("join_parts") {
                eprintln!("[join_part] {}::{} {:?} <- {} = {part:?}", join.package, join.name, join.want, path.display());
            }
            let t = match part {
                EngineValue::Value(ReducedValue::Type(t)) => t,
                EngineValue::Value(ReducedValue::FactMap(_) | ReducedValue::None) => continue,
                EngineValue::Disagree => return EngineValue::Disagree,
            };
            joined = match joined {
                None => Some(t),
                Some(j) => match crate::model::file_analysis::join(&j, &t, &order) {
                    Some(u) => Some(u),
                    None => {
                        crate::util::ghost_stats::count("param_in.disagree");
                        return EngineValue::Disagree;
                    }
                },
            };
        }
        EngineValue::Value(joined.map_or(ReducedValue::None, ReducedValue::Type))
    }

    /// A candidate's verdict on `q` through its consult key; `None` when
    /// the index keeps no answers (a scoped view), where the caller chases
    /// it inline.
    fn consult_by_key(
        &self,
        idx: &dyn crate::model::file_analysis::CrossFileLookup,
        cached: &std::sync::Arc<crate::model::file_analysis::CachedModule>,
        q: &ReducerQuery,
        arity: Option<u32>,
    ) -> Option<ReducedValue> {
        idx.answer_store()?;
        let key = EngineKey::Consult(super::engine::ConsultKey {
            file: cached.path.clone(),
            attachment: q.attachment.clone(),
            framework: q.framework,
            arity,
            receiver: q.receiver.clone(),
            args: q.args.clone(),
        });
        Some(super::engine::answer(idx, &key).reduced())
    }

    /// `EngineKey::Consult`: the candidate's bag asked the query on its own
    /// context, with a cycle guard of its own: what it answers cannot
    /// depend on the path that asked it.
    pub(super) fn consult(
        &self,
        idx: &dyn crate::model::file_analysis::CrossFileLookup,
        c: &super::engine::ConsultKey,
    ) -> ReducedValue {
        let Some(cached) = idx.cached_by_path(&c.file).read() else {
            return ReducedValue::None;
        };
        let full = idx.bag_present(&cached);
        let ctx = BagContext {
            scopes: &full.scopes,
            package_framework: &full.packages,
            module_index: Some(idx),
            package_parents: &full.packages,
            app_surface_consumers: &full.plugin.app_surface_consumers,
            origin: Some(&c.file),
        };
        let q = ReducerQuery {
            attachment: &c.attachment,
            point: None,
            framework: c.framework,
            arity_hint: c.arity,
            receiver: c.receiver.clone(),
            args: c.args.clone(),
            context: Some(&ctx),
        };
        // The asking call's arguments ride the key, so the callee's own
        // parameters read them here exactly as a local call's frame would.
        let callee = match &c.attachment {
            WitnessAttachment::PackageSymbol { name, .. } => Some(name.clone()),
            WitnessAttachment::Symbol(sid) => {
                full.symbols().get(sid.0 as usize).map(|s| s.name.clone())
            }
            _ => None,
        };
        let mut state = QueryState::new();
        match callee {
            Some(name) => {
                let method = c.receiver.is_some();
                let frame =
                    CallFrame { name, method, receiver: c.receiver.clone(), args: c.args.clone() };
                state.in_call_frame(frame, |state| {
                    (*self.query_rec(&full.witnesses, &q, state)).clone()
                })
            }
            None => (*self.query_rec(&full.witnesses, &q, &mut state)).clone(),
        }
    }

    /// One caller file's part of a join: the argument `join.want` names at
    /// each call site of the sub in `file`, folded. A method call counts when
    /// its receiver is the package or a subclass, or untyped (a may-caller);
    /// a function call when it names the package or calls the bare name.
    pub(super) fn param_contribution(
        &self,
        idx: &dyn crate::model::file_analysis::CrossFileLookup,
        join: &JoinKey,
        file: &std::path::Path,
    ) -> EngineValue {
        let Some(cached) = crate::util::ghost_stats::timed("contrib.caller_view", || super::engine::caller_view(idx, file)) else { return EngineValue::NOTHING };
        let key = crate::model::conventions::name_match_key(&join.name, &crate::model::conventions::PERL_SPELLINGS);
        let mut read = crate::util::ghost_stats::timed("contrib.slice_read", || idx.caller_slice(&cached, &key));
        // A read the cut left out widens the cut and asks again; each round
        // adds what the last one minted, so a few rounds reach every read.
        let mut rounds = 0;
        let value = loop {
            let value = crate::util::ghost_stats::timed("contrib.query", || self.contribution_in(idx, join, &read, file));
            let misses = super::take_cut_misses(read.slice.witnesses.generation());
            if misses.is_empty() {
                break value;
            }
            rounds += 1;
            if rounds > 8 {
                crate::util::ghost_stats::count("caller_slice.widen_gave_up");
                let whole = super::engine::caller_view_whole(idx, file, &key);
                break whole.map_or(value, |w| self.contribution_in(idx, join, &w, file));
            }
            read = crate::util::ghost_stats::timed("contrib.widen", || idx.widen_caller_slice(&cached, &key, &read, misses));
        };
        if slice_equiv() {
            let whole = super::engine::untracked(|| super::engine::caller_view_whole(idx, file, &key));
            if let Some(whole) = whole {
                let control = self.contribution_in(idx, join, &whole, file);
                if control != value {
                    crate::util::ghost_stats::count("caller_slice.equiv_mismatch");
                    eprintln!(
                        "[slice-equiv] {}::{} {:?} in {}: slice {value:?}, whole {control:?}",
                        join.package,
                        join.name,
                        join.want,
                        file.display()
                    );
                } else {
                    crate::util::ghost_stats::count("caller_slice.equiv_match");
                }
            }
        }
        value
    }

    fn contribution_in(
        &self,
        idx: &dyn crate::model::file_analysis::CrossFileLookup,
        join: &JoinKey,
        read: &super::caller_slice::CallerRead,
        origin: &std::path::Path,
    ) -> EngineValue {
        let (package, name) = (join.package.as_str(), join.name.as_str());
        let (frame, full) = (&*read.frame, &*read.slice);
        // The state's memo keys on this copy's bag address, so the copy is
        // pinned exactly as long as the state lives.
        let state = &mut QueryState::new();
        let caller_ctx = BagContext {
            scopes: &frame.scopes,
            package_framework: &frame.packages,
            module_index: Some(idx),
            package_parents: &frame.packages,
            app_surface_consumers: &frame.app_surface_consumers,
            origin: Some(origin),
        };
        let mut joined: Option<InferredType> = None;
        for &(ri, method) in &full.calls {
            let att = WitnessAttachment::Expression(super::RefIdx(ri));
            let Some((receiver, args)) = full.witnesses.for_attachment(&att).iter().find_map(|w| match &w.payload {
                WitnessPayload::Invoke { callee: Callee::Method { name: n, .. }, receiver, args } if method && n == name => {
                    Some((receiver.clone(), args.clone()))
                }
                WitnessPayload::Invoke { callee: Callee::Function { package: p, name: n, .. }, args, .. }
                    if !method && n == name && p.as_deref().map_or(true, |p| p == package) =>
                {
                    Some((None, args.clone()))
                }
                _ => None,
            }) else {
                continue;
            };
            let query_in = |this: &Self, att: &WitnessAttachment, state: &mut QueryState| {
                let q = ReducerQuery {
                    attachment: att,
                    point: None,
                    framework: FrameworkFact::Plain,
                    arity_hint: None,
                    receiver: None,
                    args: Vec::new(),
                    context: Some(&caller_ctx),
                };
                (*this.query_rec(&full.witnesses, &q, state)).clone()
            };
            if let Some(recv) = &receiver {
                let recv_class = match query_in(self, recv, state) {
                    ReducedValue::Type(t) => t.class_name().map(str::to_string),
                    ReducedValue::FactMap(_) | ReducedValue::None => None,
                };
                if let Some(c) = recv_class {
                    if c != package && !crate::model::file_analysis::class_isa(&c, package, &frame.packages, Some(idx)) {
                        continue;
                    }
                }
            }
            let skip = |i: u32| if method { i.checked_sub(1) } else { Some(i) };
            let value = match join.want {
                JoinWant::At(index) => {
                    let value_att = match (method, index) {
                        (true, 0) => receiver,
                        (_, i) => skip(i).and_then(|i| positional_arg(args.as_deref(), i)),
                    };
                    let Some(value_att) = value_att else { continue };
                    match query_in(self, &value_att, state) {
                        ReducedValue::Type(t) => Some(t),
                        ReducedValue::FactMap(_) | ReducedValue::None => None,
                    }
                }
                JoinWant::PairsFrom(from) => {
                    let (Some(from), Some(args)) = (skip(from), args.as_deref()) else { continue };
                    Some(pairs_type(args, from as usize, &mut |att| match query_in(self, att, state) {
                        ReducedValue::Type(t) => Some(t),
                        ReducedValue::FactMap(_) | ReducedValue::None => None,
                    }))
                }
            };
            let Some(t) = value else { continue };
            joined = match joined {
                None => Some(t),
                Some(j) => match crate::model::file_analysis::join(&j, &t, &CallerOrder { local: &frame.packages, idx }) {
                    Some(u) => Some(u),
                    None => {
                        crate::util::ghost_stats::count("param_in.disagree");
                        return EngineValue::Disagree;
                    }
                },
            };
        }
        EngineValue::Value(joined.map_or(ReducedValue::None, ReducedValue::Type))
    }

    /// Query `Symbol(sid)` inside another file's bag, as a hop of THIS query:
    /// the hop carries the index (its answer may chain through that file's own
    /// imports) and shares `state`, so a cross-file cycle cuts on the visited
    /// set. The copy is pinned because the memo keys on bag addresses.
    fn query_symbol_in(
        &self,
        idx: &dyn crate::model::file_analysis::CrossFileLookup,
        full: &std::sync::Arc<crate::model::file_analysis::FileAnalysis>,
        sid: crate::model::file_analysis::SymbolId,
        q: &ReducerQuery,
        state: &mut QueryState,
    ) -> ReducedValue {
        state.pins.push(std::sync::Arc::clone(full));
        let cached_ctx = BagContext {
            scopes: &full.scopes,
            package_framework: &full.packages,
            module_index: Some(idx),
            package_parents: &full.packages,
            app_surface_consumers: &full.plugin.app_surface_consumers,
            origin: None,
        };
        let att = WitnessAttachment::Symbol(sid);
        let sub_q = ReducerQuery {
            attachment: &att,
            point: None,
            framework: FrameworkFact::Plain,
            arity_hint: q.arity_hint,
            receiver: q.receiver.clone(),
            args: q.args.clone(),
            context: Some(&cached_ctx),
        };
        (*self.query_rec(&full.witnesses, &sub_q, state)).clone()
    }

    /// `ImportedSub{name}`: the sub a bare call names but its file does not
    /// declare. Every exporter of `name`, every file registered under that
    /// exporter (a split exporter's sub lives in whichever file defines it),
    /// asked for the defining symbol's return under the caller's arity and
    /// argument types, one visited set throughout — so mutually importing
    /// modules terminate on the cycle guard rather than a depth cap.
    #[inline(never)]
    fn imported_sub_fallback(
        &self,
        q: &ReducerQuery,
        state: &mut QueryState,
        name: &str,
    ) -> Option<ReducedValue> {
        let ctx = q.context?;
        let Some(idx) = ctx.module_index else {
            super::note_bake_exit("imported_sub", true);
            return None;
        };
        let memo_q = ReducerQuery {
            attachment: q.attachment,
            point: None,
            framework: FrameworkFact::Plain,
            arity_hint: q.arity_hint,
            receiver: None,
            args: q.args.clone(),
            context: None,
        };
        for module_name in idx.find_exporters(name).read() {
            for cached in super::session::visible_def_candidates(idx, &module_name).iter() {
                if let Some(hit) = super::session::candidate_answer(idx, &cached.path, &memo_q) {
                    if *hit != ReducedValue::None {
                        return Some((*hit).clone());
                    }
                    continue;
                }
                // The memo missed: what this candidate's verdict reads is
                // captured for it to replay.
                let capture = super::engine::CaptureGuard::open();
                crate::util::ghost_stats::count("mocsite.imported_sub");
                let full = idx.bag_present(cached);
                let Some(sym) = full.symbols().iter().find(|s| {
                    s.name == name
                        && matches!(
                            s.kind,
                            crate::model::file_analysis::SymKind::Sub
                                | crate::model::file_analysis::SymKind::Method
                        )
                }) else {
                    super::session::remember_candidate_answer(
                        idx,
                        &cached.path,
                        &memo_q,
                        &ReducedValue::None, capture.captured(),
                    );
                    continue;
                };
                let cuts_before = state.cuts;
                let v = self.query_symbol_in(idx, &full, sym.id, q, state);
                // A cut answer depends on the path it was reached from; only a
                // whole one may outlive this query.
                if state.cuts == cuts_before {
                    super::session::remember_candidate_answer(idx, &cached.path, &memo_q, &v, capture.captured());
                }
                if v != ReducedValue::None {
                    return Some(v);
                }
            }
        }
        None
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



    /// The owner-keyed ladder shared by the `Field` and `Param` fallbacks:
    /// every candidate file declaring `owner` (cross-file primary), then the
    /// same attachment re-keyed to each parent (`rekey`), one visited set
    /// throughout. No bridge hop — neither a slot nor a parameter is a
    /// plugin-synthesized callable.
    ///
    /// Out of line for the same reason as `moc_cross_file_primary`: this
    /// block's locals would otherwise live in `query_rec_body`'s frame,
    /// which is live once per inheritance hop.
    #[inline(never)]
    fn owner_keyed_fallback(
        &self,
        bag: &WitnessBag,
        q: &ReducerQuery,
        state: &mut QueryState,
        owner: &str,
        site: &'static str,
        provider: Option<&EngineKey>,
        rekey: &dyn Fn(String) -> WitnessAttachment,
    ) -> Option<ReducedValue> {
        let ctx = q.context?;
        if ctx.module_index.is_none() {
            // A value hop runs under an opaque frame (the projection
            // combines it), so no conclusion key names this exit.
            super::note_bake_exit(site, false);
        }
        if let Some(idx) = ctx.module_index {
            // An engine-keyed scan is shared by every asker: asked from the
            // owner's own file it re-reads that file at the normalized point,
            // and a re-entry lands on the open frame.
            let v = match provider {
                Some(key) => super::engine::answer(idx, key).reduced(),
                None => self.owner_provider_scan(
                    idx,
                    Some(bag),
                    q.attachment,
                    q.framework,
                    q.receiver.clone(),
                    q.args.clone(),
                    state,
                    owner,
                    site,
                ),
            };
            if v != ReducedValue::None {
                return Some(v);
            }
        }
        let parents = crate::model::file_analysis::parents_of(
            owner,
            ctx.package_parents,
            ctx.module_index,
            ctx.app_surface_consumers,
        );
        for p in parents {
            let parent_att = rekey(p);
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
                return Some((*v).clone());
            }
        }
        None
    }

    /// `CallbackIn` the asking bag can't answer: the files declaring the
    /// member's class, as one engine answer shared by every sub literal
    /// handed to the member, then the class's parents. Out of line: the key
    /// it builds would otherwise sit in `query_rec_body`'s frame on every hop.
    #[inline(never)]
    fn callback_in_fallback(&self, bag: &WitnessBag, q: &ReducerQuery, state: &mut QueryState) -> Option<ReducedValue> {
        let WitnessAttachment::CallbackIn { package, name, param, index } = q.attachment else {
            return None;
        };
        let provider = EngineKey::Providers(q.attachment.clone());
        self.owner_keyed_fallback(bag, q, state, package, "callback_in", Some(&provider), &|p| {
            WitnessAttachment::CallbackIn { package: p, name: name.clone(), param: *param, index: *index }
        })
    }

    /// The cross-file primary hop of an owner-keyed attachment: every file
    /// declaring `owner`, asked at the normalized point, first answer wins.
    /// `skip` is the asking bag, which already answered for itself.
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn owner_provider_scan(
        &self,
        idx: &dyn crate::model::file_analysis::CrossFileLookup,
        skip: Option<&WitnessBag>,
        attachment: &WitnessAttachment,
        framework: FrameworkFact,
        receiver: Option<InferredType>,
        args: Vec<InferredType>,
        state: &mut QueryState,
        owner: &str,
        site: &'static str,
    ) -> ReducedValue {
        for cached in idx.visible_def_candidates(owner).read() {
            crate::util::ghost_stats::count("moc.provider_fetched");
            crate::util::ghost_stats::count(if site == "field" { "mocsite.field" } else { "mocsite.param" });
            let full = idx.bag_present(&cached);
            if skip.is_some_and(|b| std::ptr::eq(b, &full.witnesses)) {
                continue;
            }
            let cached_ctx = BagContext {
                scopes: &full.scopes,
                package_framework: &full.packages,
                module_index: Some(idx),
                package_parents: &full.packages,
                app_surface_consumers: &full.plugin.app_surface_consumers,
                origin: Some(&cached.path),
            };
            let sub_q = ReducerQuery {
                attachment,
                // Cross-file: point normalized (see the slot arm).
                point: None,
                framework,
                arity_hint: None,
                receiver: receiver.clone(),
                args: args.clone(),
                context: Some(&cached_ctx),
            };
            let v = self.query_rec(&full.witnesses, &sub_q, state);
            if *v != ReducedValue::None {
                return (*v).clone();
            }
        }
        ReducedValue::None
    }

    /// The cross-file primary hop of the `PackageSymbol` ladder: every file
    /// declaring `package`, asked in ladder order, first answer wins,
    /// with the consult pre-filter (`sweep_candidate_may_answer`) skipping
    /// candidates the rows prove silent.
    ///
    /// Out of line, and `#[inline(never)]`, because of the STACK. This block's
    /// locals — two capture-heavy `attempt` closures, the conclusion-outcome
    /// bookkeeping, the equivalence probes — otherwise live in the caller's
    /// frame, and the caller is `query_rec_body`, which is live once per MRO
    /// hop. An index-less chase
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
                        origin: Some(&cached.path),
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
            if let Some((hit, captured)) = super::session::sweep_answer(idx, &cached.path, &verdict_key) {
                super::session::remember_candidate_answer(idx, &cached.path, &memo_q, &hit, captured);
                if *hit != ReducedValue::None {
                    return Some((*hit).clone());
                }
                continue;
            }
            // Both memo tiers missed: what this candidate's verdict reads is
            // captured for them to replay.
            let capture = super::engine::CaptureGuard::open();
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
                        &ReducedValue::None, capture.captured(),
                    );
                    idx.remember_sweep_consult(
                        &cached.path,
                        &verdict_key,
                        &ReducedValue::None, capture.captured(),
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
                                idx, &cached.path, &memo_q, &v, capture.captured(),
                            );
                            idx.remember_sweep_consult(&cached.path, &verdict_key, &v, capture.captured());
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
                                && idx.class_is_bridged_to(package).read()
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
                                    &ReducedValue::None, capture.captured(),
                                );
                                idx.remember_sweep_consult(
                                    &cached.path, &verdict_key, &ReducedValue::None, capture.captured(),
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
                        } if !idx.class_is_bridged_to(package).read() => {
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
                                            idx, &cached.path, &memo_q, &v, capture.captured(),
                                        );
                                        idx.remember_sweep_consult(
                                            &cached.path, &verdict_key, &v, capture.captured(),
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
                                    &idx.facts_present(cached),
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
                        origin: None,
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
                    &ReducedValue::None, capture.captured(),
                );
                idx.remember_sweep_consult(&cached.path, &verdict_key, &ReducedValue::None, capture.captured());
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
            if consults_self(bag, q, idx, cached) {
                // Self: the reducers above already tried this bag.
                // Not an answer about the candidate, so nothing to
                // remember either.
                continue;
            }
            let v = {
                let v = crate::util::ghost_stats::timed(
                    "consult.attempt", || {
                        self.consult_by_key(idx, cached, q, q.arity_hint)
                            .unwrap_or_else(|| {
                                let full = crate::util::ghost_stats::timed("consult.bag_present", || {
                                    // Untracked: the consult key records the file.
                                    super::engine::untracked(|| idx.bag_present(cached))
                                });
                                attempt(&full, state)
                            })
                    });
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
                v
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
                    let full = super::engine::untracked(|| idx.bag_present(cached));
                    let candidate_key = visited_key(&full.witnesses, q);
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
            super::session::remember_candidate_answer(idx, &cached.path, &memo_q, &v, capture.captured());
            idx.remember_sweep_consult(&cached.path, &verdict_key, &v, capture.captured());
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
        let window = super::reducers::LiveWindow::at(&raw, q);
        let mut out: Vec<Witness> = Vec::with_capacity(raw.len());
        for &w in raw.iter() {
            if window.edge_is_dead(&raw, w) {
                continue;
            }
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
                    match resolved {
                        Some(t) => out.push(Witness {
                            attachment: w.attachment.clone(),
                            source: w.source.clone(),
                            payload: WitnessPayload::InferredType(t),
                            span: w.span,
                        }),
                        // An edge that didn't resolve drops out — same as a
                        // witness no reducer claims. The write it lowered
                        // still HAPPENED: its `Reset` marker records that.
                        None => {}
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
                            // The read's own point, as the Edge arm above.
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
                                    crate::model::file_analysis::join_all(elems, super::reducers::class_order(q))
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
                            ProjectionStep::SlotElements { .. } | ProjectionStep::CallbackOf { .. } => {
                                t.class_name().map(str::to_string).and_then(|class| {
                                    let att = match step {
                                        ProjectionStep::CallbackOf { member, param, index } => {
                                            WitnessAttachment::CallbackIn {
                                                package: class,
                                                name: member.clone(),
                                                param: *param,
                                                index: *index,
                                            }
                                        }
                                        ProjectionStep::SlotElements { key } => {
                                            WitnessAttachment::SlotElements { class, key: key.clone() }
                                        }
                                        _ => return None,
                                    };
                                    let sub_q = ReducerQuery {
                                        attachment: &att,
                                        point: None,
                                        framework: q.framework,
                                        arity_hint: None,
                                        receiver: None,
                                        args: Vec::new(),
                                        context: q.context,
                                    };
                                    state.in_opaque_frame(|state| match &*self.query_rec(bag, &sub_q, state) {
                                        ReducedValue::Type(t) => Some(t.clone()),
                                        ReducedValue::FactMap(_) | ReducedValue::None => None,
                                    })
                                })
                            }
                            ProjectionStep::ParamOf { member, index } => {
                                // The dispatch class picks the callee; the
                                // callee's bag says whether the position
                                // aliases (`Param` fallback below).
                                t.class_name().map(str::to_string).and_then(|class| {
                                    let att = WitnessAttachment::Param {
                                        package: class,
                                        name: member.clone(),
                                        index: *index,
                                    };
                                    let sub_q = ReducerQuery {
                                        attachment: &att,
                                        point: q.point,
                                        framework: q.framework,
                                        arity_hint: None,
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
                            ProjectionStep::ValueHop { member } => {
                                // A value read chases `Field{class, member}`,
                                // the slot's own subject, so a same-named
                                // callable's return never answers it (a call
                                // is an `Invoke`). The base type IS the
                                // receiver, so a slot typed by the class's
                                // template parameters substitutes the base's
                                // arguments.
                                t.class_name().map(str::to_string).and_then(|class| {
                                    let att = WitnessAttachment::Field { owner: class, name: member.clone() };
                                    let sub_q = ReducerQuery {
                                        attachment: &att,
                                        point: q.point,
                                        framework: q.framework,
                                        arity_hint: None,
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
                            ProjectionStep::AsKey { key } => Some(InferredType::HashWithKeys {
                                keys: crate::model::file_analysis::SharedKeys::new(vec![(
                                    key.clone(),
                                    Some(Box::new(t.clone())),
                                )]),
                                open: false,
                            }),
                            ProjectionStep::WrittenBy {
                                package,
                                name,
                                index,
                                method,
                            } => {
                                let att = WitnessAttachment::ParamWrites {
                                    package: package.clone(),
                                    name: name.clone(),
                                    index: *index,
                                    method: *method,
                                };
                                let sub_q = ReducerQuery {
                                    attachment: &att,
                                    point: None,
                                    framework: q.framework,
                                    arity_hint: None,
                                    receiver: None,
                                    args: Vec::new(),
                                    context: q.context,
                                };
                                let written = state.in_opaque_frame(|state| {
                                    match &*self.query_rec(bag, &sub_q, state) {
                                        ReducedValue::Type(t) => Some(t.clone()),
                                        ReducedValue::FactMap(_) | ReducedValue::None => None,
                                    }
                                });
                                written.and_then(|w| merge_written_keys(&t, &w))
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
                WitnessPayload::Tuple(elems) => {
                    // Every slot must answer: a partial tuple would put the
                    // wrong type at an index. Opaque frames throughout — the
                    // value is assembled here, no sub-chase names it.
                    let mut types = Vec::with_capacity(elems.len());
                    for el in elems {
                        let sub_q = ReducerQuery {
                            attachment: el,
                            point: q.point,
                            framework: q.framework,
                            arity_hint: None,
                            receiver: q.receiver.clone(),
                            args: q.args.clone(),
                            context: q.context,
                        };
                        let t = state.in_opaque_frame(|state| {
                            match &*self.query_rec(bag, &sub_q, state) {
                                ReducedValue::Type(t) => Some(t.clone()),
                                ReducedValue::FactMap(_) | ReducedValue::None => None,
                            }
                        });
                        match t {
                            Some(t) => types.push(t),
                            None => {
                                types.clear();
                                break;
                            }
                        }
                    }
                    if !types.is_empty() {
                        out.push(Witness {
                            attachment: w.attachment.clone(),
                            source: w.source.clone(),
                            payload: WitnessPayload::InferredType(
                                crate::model::file_analysis::InferredType::Sequence(types),
                            ),
                            span: w.span,
                        });
                    }
                }
                WitnessPayload::ReturnExpr(re) if re.reads_attachments() => {
                    // Opaque: each leaf is read under this query's receiver and
                    // args, and the answer is the expression around it.
                    let resolved = re.map_of(&mut |att| {
                        let sub_q = ReducerQuery { attachment: att, ..q.clone() };
                        state.in_opaque_frame(|state| match &*self.query_rec(bag, &sub_q, state) {
                            ReducedValue::Type(t) => Some(t.clone()),
                            ReducedValue::FactMap(_) | ReducedValue::None => None,
                        })
                    });
                    out.push(Witness {
                        attachment: w.attachment.clone(),
                        source: w.source.clone(),
                        payload: WitnessPayload::ReturnExpr(resolved),
                        span: w.span,
                    });
                }
                WitnessPayload::Invoke { callee, receiver, args } => {
                    if let Some(t) = self.invoke(bag, q, state, callee, receiver.as_ref(), args.as_deref()) {
                        out.push(Witness {
                            attachment: w.attachment.clone(),
                            source: w.source.clone(),
                            payload: WitnessPayload::InferredType(t),
                            span: w.span,
                        });
                    }
                }
                _ => out.push(w.clone()),
            }
        }
        out
    }

    /// Materialize `att` under `q`'s point and context, in an opaque frame:
    /// an `Invoke` operand is an input to this frame's answer, never the
    /// answer itself.
    fn operand_type(
        &self,
        bag: &WitnessBag,
        q: &ReducerQuery,
        state: &mut QueryState,
        att: &WitnessAttachment,
    ) -> Option<InferredType> {
        let sub_q = ReducerQuery {
            attachment: att,
            point: q.point,
            framework: q.framework,
            arity_hint: None,
            // The enclosing call's receiver still binds `$self` inside the
            // operand (`return $self->m` typed at a `Child->outer` site).
            receiver: q.receiver.clone(),
            args: Vec::new(),
            context: q.context,
        };
        state.in_opaque_frame(|state| match &*self.query_rec(bag, &sub_q, state) {
            ReducedValue::Type(t) => Some(t.clone()),
            ReducedValue::FactMap(_) | ReducedValue::None => None,
        })
    }

    /// The value of an `Invoke`: materialize the receiver and arguments,
    /// pick the lookup attachment, and query it with the receiver's full
    /// type and the argument types substituted.
    fn invoke(
        &self,
        bag: &WitnessBag,
        q: &ReducerQuery,
        state: &mut QueryState,
        callee: &Callee,
        receiver: Option<&WitnessAttachment>,
        args: Option<&[CallArg]>,
    ) -> Option<InferredType> {
        let recv = receiver.and_then(|r| self.operand_type(bag, q, state, r));
        // Positional argument types up to the first spread; an operand that
        // types nothing holds its position as `Unknown`.
        // TODO(prototypes): a callee prototype like `(\@)` takes an aggregate
        // as ONE value, so whether an argument spreads is the callee's call;
        // the operand only says it can flatten. Needs prototype parsing.
        let mut arg_types: Vec<InferredType> = Vec::new();
        let mut spread = false;
        for a in args.unwrap_or_default() {
            match a {
                CallArg::One(att) | CallArg::Word { at: att, .. } => arg_types.push(
                    self.operand_type(bag, q, state, att).unwrap_or(InferredType::Unknown),
                ),
                CallArg::Spread(_) | CallArg::Forward { .. } => {
                    spread = true;
                    break;
                }
            }
        }
        let exact = args.is_some() && !spread;
        let name = match callee {
            Callee::Function { name, .. } => Some((name.clone(), false)),
            Callee::Method { name, .. } => Some((name.clone(), true)),
            Callee::Value(_) => None,
        };
        match name {
            Some((name, method)) => {
                let frame =
                    CallFrame { name, method, receiver: recv.clone(), args: arg_types.clone() };
                state.in_call_frame(frame, |state| {
                    self.invoke_with(bag, q, state, callee, recv, arg_types, exact)
                })
            }
            None => self.invoke_with(bag, q, state, callee, recv, arg_types, exact),
        }
    }

    /// `invoke` once the receiver and arguments are typed.
    #[allow(clippy::too_many_arguments)]
    fn invoke_with(
        &self,
        bag: &WitnessBag,
        q: &ReducerQuery,
        state: &mut QueryState,
        callee: &Callee,
        mut recv: Option<InferredType>,
        mut arg_types: Vec<InferredType>,
        exact: bool,
    ) -> Option<InferredType> {
        // A bare call in a body that elides the receiver: the class's member
        // (its own or inherited, in any file) hides every free function of
        // the name, with the class itself as the receiver.
        if let Callee::Function { name, member_of: Some(class), .. } = callee {
            let att = WitnessAttachment::PackageSymbol { package: class.clone(), name: name.clone() };
            let sub_q = ReducerQuery {
                attachment: &att,
                point: q.point,
                framework: q.framework,
                arity_hint: exact.then_some(arg_types.len() as u32),
                receiver: Some(InferredType::ClassName(class.clone())),
                args: arg_types.clone(),
                context: q.context,
            };
            let member = state.in_opaque_frame(|state| match &*self.query_rec(bag, &sub_q, state) {
                ReducedValue::Type(t) => Some(t.clone()),
                ReducedValue::FactMap(_) | ReducedValue::None => None,
            });
            if member.is_some() {
                return member;
            }
        }
        let lookups: Vec<WitnessAttachment> = match callee {
            Callee::Method { name, lookup, .. } => {
                let classes: Vec<String> = match lookup {
                    Lookup::Receiver => {
                        recv.as_ref().and_then(|t| t.class_name()).map(str::to_string).into_iter().collect()
                    }
                    Lookup::Super { writer } => match q.context {
                        Some(ctx) => crate::model::file_analysis::parents_of(
                            writer,
                            ctx.package_parents,
                            ctx.module_index,
                            ctx.app_surface_consumers,
                        ),
                        None => Vec::new(),
                    },
                    Lookup::Named(class) => vec![class.clone()],
                };
                // A call with no typed receiver still blesses into the class
                // it dispatched on (`Foo->new`, a plugin-declared class).
                if recv.is_none() {
                    if let Lookup::Named(class) | Lookup::Super { writer: class } = lookup {
                        recv = Some(InferredType::ClassName(class.clone()));
                    }
                }
                classes
                    .into_iter()
                    .map(|package| WitnessAttachment::PackageSymbol { package, name: name.clone() })
                    .collect()
            }
            Callee::Function { name, local: None, package: Some(package), .. } => {
                vec![WitnessAttachment::PackageSymbol { package: package.clone(), name: name.clone() }]
            }
            Callee::Function { name, local: None, package: None, .. } => {
                let Some(ctx) = q.context.filter(|c| c.module_index.is_some()) else {
                    // The definition is in another file, and no conclusion
                    // key names a free function, so a bake can't link it.
                    super::note_bake_exit("free_function", false);
                    state.note_exit(None);
                    return None;
                };
                let arity = exact.then_some(arg_types.len() as u32);
                return self.free_function_cross_file(ctx, name, arity, recv, &arg_types, state);
            }
            Callee::Function { local: Some(sid), .. } => vec![WitnessAttachment::Symbol(*sid)],
            Callee::Value(att) => {
                let target = self.operand_type(bag, q, state, att)?.callable_return_edge()?.clone();
                // Invoking a method through a coderef passes its invocant as
                // the first argument (`$cb->($obj, …)` for `\&Class::m`).
                if recv.is_none() && matches!(target, WitnessAttachment::PackageSymbol { .. }) {
                    if !arg_types.is_empty() {
                        recv = Some(arg_types.remove(0));
                    }
                }
                vec![target]
            }
        };
        let arity = exact.then_some(arg_types.len() as u32);
        // SUPER walks the writer's parents in MRO order: the first that
        // answers is the method Perl would run.
        let found = lookups.into_iter().find_map(|att| {
            let sub_q = ReducerQuery {
                attachment: &att,
                point: q.point,
                framework: q.framework,
                arity_hint: arity,
                receiver: recv.clone(),
                args: arg_types.clone(),
                context: q.context,
            };
            state.in_opaque_frame(|state| match &*self.query_rec(bag, &sub_q, state) {
                ReducedValue::Type(t) => Some(t.clone()),
                ReducedValue::FactMap(_) | ReducedValue::None => None,
            })
        });
        let constructs = matches!(callee, Callee::Method { constructs: true, .. });
        found.or_else(|| {
            constructs
                .then(|| recv?.class_name().map(|c| InferredType::ClassName(c.to_string())))
                .flatten()
        })
    }

    /// The free function `name` as the files `ctx` can see it: a module
    /// exporting it, then a pack's linkage-visible definitions. One ladder
    /// for both entries — `query_sub_return_type`'s cross-file half and an
    /// unresolved `Callee::Function` — and the chase shares `state`, so a
    /// call that crosses back into a file already on the path stops at the
    /// cycle guard instead of recursing through a fresh query.
    pub(super) fn free_function_cross_file(
        &self,
        ctx: &BagContext,
        name: &str,
        arity_hint: Option<u32>,
        receiver: Option<InferredType>,
        args: &[InferredType],
        state: &mut QueryState,
    ) -> Option<InferredType> {
        use crate::model::file_analysis::{FileAnalysis, SymKind};
        use std::sync::Arc;
        let idx = ctx.module_index?;
        if !super::session::budget_available(idx) {
            return None;
        }
        let is_callable = |s: &crate::model::file_analysis::Symbol| {
            s.name == name && matches!(s.kind, SymKind::Sub | SymKind::Method)
        };
        // The callable's answer in its own file, under the caller's arity,
        // receiver and arguments. The copy is pinned: the memo keys on its
        // bag's address for the rest of this query.
        let ask = |full: &Arc<FileAnalysis>, origin: &std::path::Path, sym: SymbolId, state: &mut QueryState| {
            state.pins.push(Arc::clone(full));
            let cached_ctx = BagContext {
                scopes: &full.scopes,
                package_framework: &full.packages,
                module_index: Some(idx),
                package_parents: &full.packages,
                app_surface_consumers: &full.plugin.app_surface_consumers,
                origin: Some(origin),
            };
            let att = WitnessAttachment::Symbol(sym);
            let q = ReducerQuery {
                attachment: &att,
                point: None,
                framework: FrameworkFact::Plain,
                arity_hint,
                receiver: receiver.clone(),
                args: args.to_vec(),
                context: Some(&cached_ctx),
            };
            match &*self.query_rec(&full.witnesses, &q, state) {
                ReducedValue::Type(t) => Some(t.clone()),
                ReducedValue::FactMap(_) | ReducedValue::None => None,
            }
        };
        // A module exporting it: the `ImportedSub` key, so the exporter walk
        // and its per-candidate memo are the ones a bare call's bake reads.
        {
            let att = WitnessAttachment::ImportedSub { name: name.to_string() };
            let q = ReducerQuery {
                attachment: &att,
                point: None,
                framework: FrameworkFact::Plain,
                arity_hint,
                receiver: receiver.clone(),
                args: args.to_vec(),
                context: Some(ctx),
            };
            let empty = WitnessBag::default();
            match &*self.query_rec(&empty, &q, state) {
                ReducedValue::Type(t) => return Some(t.clone()),
                ReducedValue::FactMap(_) | ReducedValue::None => {}
            }
        }
        // A pack callee defined in an INCLUDED file carries no export edge (C
        // free functions never populate `@EXPORT`). It crosses the boundary
        // by the include-closure visibility goto-def uses (`pack_def_paths`):
        // a candidate is reachable iff the querying file sees its path or the
        // candidate includes the querying file — never a bare name match, so
        // a same-named function in an unrelated TU can't contaminate. A Flat
        // scope (a name-keyed pack) has no closure to gate by: every
        // candidate is reachable by rule. Disagreeing candidates are a
        // genuine ambiguity and answer nothing; a prototype and its
        // definition agree. Transparent hosts (no rule known) skip the arm.
        let scope = idx.visibility_scope().map(|(p, v)| (p.to_string_lossy().into_owned(), v));
        if scope.is_none() && !idx.flat_scope() {
            return None;
        }
        let mut answer: Option<InferredType> = None;
        for cached in idx.def_candidates(name).read() {
            let p = cached.path.to_string_lossy();
            // Reachability reads pinned fields (path + include_closure), so
            // gate BEFORE rehydrating: an unreachable candidate never pays a
            // bag decode.
            let reachable = match &scope {
                Some((self_str, visible)) => {
                    visible.contains(p.as_ref())
                        || idx.facts_present(&cached).pack.include_closure.contains(self_str)
                }
                None => true,
            };
            if !reachable {
                continue;
            }
            let full = idx.bag_present(&cached);
            let Some(sym) = full
                .symbols()
                .iter()
                .find(|s| is_callable(s) && full.is_linkage_visible(s))
                .map(|s| s.id)
            else {
                continue;
            };
            match (ask(&full, &cached.path, sym, state), &answer) {
                (Some(t), Some(existing)) if *existing != t => return None,
                (Some(t), None) => answer = Some(t),
                _ => {}
            }
        }
        answer
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
    binds_variable_where(bag, var, scope, |w| w.span.start <= point)
}

fn binds_variable_where(
    bag: &WitnessBag,
    var: &str,
    scope: ScopeId,
    at: impl Fn(&Witness) -> bool,
) -> bool {
    let att = WitnessAttachment::Variable {
        name: var.to_string(),
        scope,
    };
    bag.for_attachment(&att).iter().any(|w| {
        at(w)
            && (w.payload.binds_value()
                || !matches!(&w.payload, WitnessPayload::Observation(_) | WitnessPayload::Reset))
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

/// The computation behind each engine key: the key and the index are all it
/// reads, each on a fresh query state.
/// `PERL_LSP_SLICE_EQUIV=1`: every caller contribution is also computed
/// over the whole bag and compared.
fn slice_equiv() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("PERL_LSP_SLICE_EQUIV").is_ok_and(|v| v == "1"))
}

/// The least value above both `prev` and `next` on the engine's lattice:
/// nothing below every type, a disagreement above them, two types at their
/// cross-file join (a disagreement when they have none).
pub(super) fn widen(
    idx: &dyn crate::model::file_analysis::CrossFileLookup,
    prev: &EngineValue,
    next: &EngineValue,
) -> EngineValue {
    match (prev, next) {
        (EngineValue::Disagree, _) | (_, EngineValue::Disagree) => EngineValue::Disagree,
        (EngineValue::Value(ReducedValue::None), v) | (v, EngineValue::Value(ReducedValue::None)) => v.clone(),
        (EngineValue::Value(ReducedValue::Type(a)), EngineValue::Value(ReducedValue::Type(b))) => {
            let no_local: std::collections::HashMap<String, crate::model::file_analysis::PackageFacts> =
                std::collections::HashMap::new();
            match crate::model::file_analysis::join(a, b, &CallerOrder { local: &no_local, idx }) {
                Some(t) => EngineValue::Value(ReducedValue::Type(t)),
                None => EngineValue::Disagree,
            }
        }
        // A fact map has no order; the latest round stands.
        (_, v) => v.clone(),
    }
}

pub(super) fn compute_engine_key(
    idx: &dyn crate::model::file_analysis::CrossFileLookup,
    key: &EngineKey,
) -> EngineValue {
    let reg = super::conclusions::shared_registry();
    match key {
        EngineKey::Join(join) => reg.param_in_join(idx, join),
        EngineKey::Contribution { join, file } => reg.param_contribution(idx, join, file),
        EngineKey::Consult(c) => EngineValue::Value(reg.consult(idx, c)),
        EngineKey::Providers(att) => {
            let Some((owner, site)) = att.provider_owner() else {
                return EngineValue::Value(ReducedValue::None);
            };
            EngineValue::Value(reg.owner_provider_scan(
                idx,
                None,
                att,
                FrameworkFact::Plain,
                None,
                Vec::new(),
                &mut QueryState::new(),
                owner,
                site,
            ))
        }
    }
}

/// `base` with the keys a callee wrote through it merged in: a written key
/// takes the callee's value type, an unwritten one keeps the caller's. Only
/// a hash shape takes keys; anything else is left as it was.
fn merge_written_keys(base: &InferredType, written: &InferredType) -> Option<InferredType> {
    let (
        InferredType::HashWithKeys { keys, open },
        InferredType::HashWithKeys {
            keys: wkeys,
            open: wopen,
        },
    ) = (base, written)
    else {
        return None;
    };
    let mut keys = keys.clone();
    let merged = keys.to_mut();
    for (k, v) in wkeys.iter() {
        match merged.iter_mut().find(|(name, _)| name == k) {
            Some(entry) => {
                if v.is_some() {
                    entry.1 = v.clone();
                }
            }
            None => merged.push((k.clone(), v.clone())),
        }
    }
    Some(InferredType::HashWithKeys {
        keys,
        open: *open || *wopen,
    })
}
