//! The witness vocabulary: attachments, sources, payloads, return
//! expressions, observations, and the framework-fact mirror.

use super::*;

// ---- Core witness types ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Witness {
    pub attachment: WitnessAttachment,
    pub source: WitnessSource,
    pub payload: WitnessPayload,
    /// Source location the witness was emitted at — used by the fold for
    /// narrowing (narrowest containing span wins) and temporal ordering
    /// (witnesses past the query point are skipped). Zero-extent span
    /// means a core-synthesized seed with no single source location.
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WitnessAttachment {
    /// Variable-in-scope facts — what `TypeConstraint` / `CallBinding`
    /// index by.
    Variable { name: String, scope: ScopeId },
    /// Result type of a method call, keyed by its index into
    /// `FileAnalysis::refs`. Only `RefKind::MethodCall` refs get
    /// witnesses here (function calls resolve via `Symbol` edges; the
    /// general rvalue axis is `Expr` below). Chain aggregation
    /// (`X->m()->n()`) folds across this axis.
    Expression(RefIdx),
    /// A symbol property ("this sub is a dispatcher").
    Symbol(SymbolId),
    /// Hash key metadata (writes, mutations, derivations).
    HashKey { owner: HashKeyOwner, name: String },
    /// The value of an expression at this span — the one attachment
    /// shape for every rvalue (literals, variable reads, calls,
    /// ternaries, return-arm bodies, implicit-last statements).
    /// Witnesses here are either a direct `InferredType(t)` (literals,
    /// constructors, builtin returns) or an `Edge` to the resolution
    /// target (`Variable` for `$foo`, `Symbol` for a resolved local sub
    /// call, `Expression` for a method call's resolved type, or another
    /// `Expr` for compound expressions). The per-sub fold reads
    /// `Edge(Expr(span))` arms via `Symbol(sub_id)`.
    Expr(Span),
    /// An entry in a package's symbol table: "what does `name` resolve to
    /// in package `package`?" — the cross-package disambiguation a
    /// name-keyed attachment can't carry. Keyed by a plain package string,
    /// which is why it is not `MethodOnClass`: the key is a namespace, and
    /// plenty of packages that own one are not classes.
    ///
    /// Inheritance composes through `Edge(PackageSymbol(parent, name))`
    /// witnesses the builder emits per `package_parents[C]`, so the
    /// registry's cycle-guarded edge chase walks the MRO with no procedural
    /// ancestor walker. With a `BagContext.module_index`, the materialize
    /// step recurses into the cached module's bag for `package`.
    ///
    /// **Plugin-facing.** Rhai manifests build this variant by NAME through
    /// serde, so the variant and field spellings are an API, not an internal
    /// detail. `MethodOnClass` / `class` stay accepted so a third-party
    /// `.rhai` written against the old names keeps working.
    ///
    /// **`package` is a Rhai reserved keyword — quote it, or write `pkg`:**
    ///
    /// ```text
    /// #{ PackageSymbol: #{ "package": cls, name: m } }   // ok
    /// #{ PackageSymbol: #{ pkg: cls,      name: m } }    // ok
    /// #{ PackageSymbol: #{ package: cls,  name: m } }    // WHOLE SCRIPT DIES
    /// ```
    ///
    /// Unquoted, the script fails to COMPILE, so the plugin loads not at all
    /// — every emission it owns disappears, not just this edge. That is why
    /// `pkg` is aliased: it is the spelling that cannot be got wrong.
    #[serde(alias = "MethodOnClass")]
    PackageSymbol {
        #[serde(alias = "class", alias = "pkg")]
        package: String,
        name: String,
    },
    /// Per-arm return collector for a sub. Each `return EXPR` arm pushes
    /// one `Edge(Expr(body_span))` here; the parent `Symbol(sub_id)`
    /// carries one `Edge(SymbolReturnArm(_))` so consumers querying the
    /// symbol still see arm-fold answers via edge materialization.
    /// Distinct from `Symbol(_)` so `SymbolReturnArmFold` claims by
    /// attachment shape, not source-tag exclusion.
    SymbolReturnArm(SymbolId),
    /// Per-arm collector for a ternary `$c ? A : B`, keyed by the
    /// conditional expression's span. Each arm pushes one
    /// `Edge(Expr(arm_span))` here; the ternary's own `Expr(span)`
    /// carries a single `Edge(BranchArm(span))` so consumers querying
    /// the expression materialize the agreed arm type. Distinct shape
    /// (like `SymbolReturnArm`) so `BranchArmFold` claims by attachment
    /// and the shared `Expr` / `Variable` reducers never see arm
    /// witnesses.
    BranchArm(Span),
    /// Typed-slot collector: "what type does instance slot `key` hold on
    /// class `class`?" Seeded from typed hash-key WRITEs
    /// (`$obj->{key} = <rhs>`) as one `Edge(Expr(rhs_span))` per write;
    /// `SlotTypeFold` agrees the arms via `resolve_return_type` (1+ agree
    /// → that type, disagree → None). Class-keyed so `$self->{h}` and a
    /// differently-typed `$other->{h}` don't cross-contaminate. Nothing
    /// consumes this yet — `$obj->{h}->m()` typing through it is a later
    /// step.
    SlotType { class: String, key: String },
    /// A named type alias — a C `typedef`/C++ `using`. "What does the type
    /// spelling `name` resolve to?" A `typedef unsigned short U16` pushes
    /// `TypeName("U16") → InferredType(ClassName("unsigned short"))`; a
    /// `typedef U16 U16b` pushes `TypeName("U16b") → Edge(TypeName("U16"))`,
    /// so the registry chases the alias graph like any other edge (cycles
    /// broken by the shared visited set — `typedef struct sv SV` is
    /// self-referential and must not loop). A declared type at a USE site
    /// (`U16 x;`, `OP* p;`) edges here instead of committing a
    /// `ClassName` value, so struct/primitive aliases resolve through the
    /// same one graph. An unresolved `TypeName(n)` is terminal: it IS a
    /// type named `n` (the `ClassName(n)` fallback in `query_rec_body`),
    /// which keeps a plain struct tag or unknown class resolving to itself.
    /// Cross-file: the alias name is a Class symbol in its defining file,
    /// so `get_cached(name)` recurses into the header's bag (same shape as
    /// the `PackageSymbol` bridge).
    TypeName(String),
    /// A storage slot — a named field DECLARATION — keyed
    /// **language-generically** by `{owner, name}` (a C struct member, a
    /// Corinna `field`, a Moo `has`; NOT `{struct, name}`). Distinct from
    /// the bag's local subjects (`Variable{name,scope}`, `Expr{span}`):
    /// every access to the slot, in any scope, folds onto the SAME `Field`
    /// subject — the IDENTITY is project-wide (the `field_subject` ancestor
    /// walk converges subtypes on the declaring owner), while the domain
    /// fold GATHERS per querying file (`FileAnalysis::field_domain_for_owner`
    /// reads that file's own `domain_sites`; owner-gated per site).
    /// `DomainCompare` witnesses on this attachment carry the enum a use
    /// compares/assigns the slot against (or `None` counter-evidence);
    /// `DomainCoherenceFold` folds them into the slot's DOMAIN type
    /// (`op_type: uint16_t` storage → `opcode` domain). The domain is a
    /// defeasible refinement for human surfaces; it never changes the
    /// storage type that flows. Kept at the END for bincode variant-index
    /// stability (bump `EXTRACT_VERSION`).
    Field { owner: String, name: String },
}

/// Index into `FileAnalysis::refs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RefIdx(pub u32);

/// `WitnessSource::Builder` tag marking a variable's type as written
/// EXPLICITLY (a declared static-type annotation) rather than inferred.
/// Recognized by `WitnessSource::priority` (an explicit annotation
/// outranks a flow guess) and by the inlay-hint suppression (an annotated
/// declaration needs no synthetic `: T`).
pub const ANNOT_SOURCE: &str = "skeleton-annot";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WitnessSource {
    /// Named builder pass — "signature_extraction", "narrowing", …
    Builder(String),
    /// Plugin id.
    Plugin(String),
    /// Post-build enrichment source.
    Enrichment(String),
    /// Derived from another ref — rename transport chases these as a DAG.
    DerivedFrom(RefIdx),
}

impl WitnessSource {
    /// Priority for "highest-priority source wins" tie-breaking in
    /// reducers. Plugin overrides dominate everything else (the whole
    /// point of an override is "inference reaches the wrong answer
    /// here"). An EXPLICIT type annotation (`ANNOT_SOURCE`) outranks a
    /// same-attachment inferred/flow class assertion: in a statically-
    /// typed pack language the declared type governs member dispatch, so a
    /// declared `RCPV *rcpv = FOO(...)` must resolve members on `RCPV`,
    /// never on a flow guess for the initializer (e.g. the uppercase-call
    /// ctor-convention heuristic that mis-types the macro call). The
    /// remaining weights only need `Plugin > annotation > everything else`.
    pub fn priority(&self) -> u8 {
        match self {
            WitnessSource::Plugin(_) => 100,
            WitnessSource::Builder(tag) if tag == ANNOT_SOURCE => 20,
            WitnessSource::Builder(_)
            | WitnessSource::Enrichment(_)
            | WitnessSource::DerivedFrom(_) => 10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WitnessPayload {
    /// Final-form type belief — what legacy `TypeConstraint` carries.
    InferredType(InferredType),
    /// An **observation** — raw evidence about a value's use, folded by
    /// the framework-aware resolver.
    Observation(TypeObservation),
    /// Edge fact: "the value at my attachment is whatever resolves at
    /// `target`." The registry materializes these at query time — chase
    /// the target via recursive query, replace the edge with a synthetic
    /// `InferredType` witness preserving source + span, then run reducers
    /// against the materialized list. A cycle guard breaks `A → B → A`.
    Edge(WitnessAttachment),
    /// Edge fact for a **method call at a known arity**: "the value at my
    /// attachment is `target`'s return type, dispatched at `arity` args."
    /// Distinct from a plain `Edge` because the call site's arity is
    /// intrinsic to the *call*, not to whatever outer query reached it —
    /// a hint-less `$x` type query that chases here must still pick the
    /// fluent-writer arm of `$obj->setter($v)` (arity ≥ 1), not the
    /// getter arm a hint-less `UnionOnArgs` defaults to. Emitted by
    /// `emit_method_call_return_edges` (`Expression(refidx)` → its
    /// `PackageSymbol{package, method}` at the call's `count_call_args`);
    /// chased like `Edge` but overrides `q.arity_hint` with `arity`.
    CallReturn { target: WitnessAttachment, arity: u32 },
    /// **Explicitly-qualified method dispatch** — the method token carried a
    /// `::` so Perl dispatches from a *named* class, not the invocant's: look
    /// the method up on `method_lookup` but type the result relative to
    /// `receiver_class` (the invocant / enclosing class). Two spellings, one
    /// rule (see `emit_method_call_return_edges`):
    ///   - `$obj->SUPER::m` → `method_lookup` is `PackageSymbol{<enclosing
    ///     package's parent>, m}` (SUPER searches the *writing* package's
    ///     `@ISA`, skipping it);
    ///   - `$obj->Foo::Bar::m` → `method_lookup` is `PackageSymbol{Foo::Bar,
    ///     m}` (fully-qualified: search starts at the named class).
    /// In both, the call still blesses into the CALLER's class, so a ctor
    /// returning `ReturnExpr::ReceiverOr` must substitute the invocant — the
    /// dynamic outer receiver wins when it is a subclass of `receiver_class`.
    /// (A plain `CallReturn` can't express this: its receiver defaults to the
    /// dispatch class, which here is the parent / named class — wrong.)
    QualifiedCallReturn {
        method_lookup: WitnessAttachment,
        receiver_class: String,
        arity: u32,
    },
    /// **Symbol-declarative return type.** A receiver-relative /
    /// arity-relative expression that `ReturnExprReducer` substitutes at
    /// query time using `q.receiver` and `q.arity_hint`. Subsumes both
    /// call-site projection (DBIC `find` emits `Operator(RowOf(Receiver))`
    /// once on the symbol) and arity dispatch (Mojo `has`'s getter/writer
    /// collapse to a single `UnionOnArgs`).
    ///
    /// Attached to `Symbol(_)` (per-sub) and `PackageSymbol{...}`
    /// (class-keyed). Latest wins, so a plugin override re-publishes over
    /// a build-time inference.
    ReturnExpr(ReturnExpr),
    /// Keyed fact. Family + key + value schema is the reducer's
    /// responsibility.
    Fact { family: String, key: String, value: FactValue },
    /// "This witness's subject derives from another ref." Rename
    /// transport walks these.
    Derivation,
    /// Escape hatch for plugin-defined payloads that don't fit above.
    Custom { family: String, json: String },
    /// Edge fact with a projection: "the value at my attachment is the
    /// `step`-projection of whatever resolves at `base`." Emitted for
    /// `expr->{key}` / `expr->[N]` expressions so the drill participates
    /// in the edge graph — the chase materializes `base` at QUERY time
    /// (when cross-file knowledge like an imported literal's
    /// `HashWithKeys` is in hand) and narrows through the step. Kept at
    /// the END for bincode variant-index stability (bump
    /// `EXTRACT_VERSION`).
    Projected {
        base: WitnessAttachment,
        step: ProjectionStep,
    },
    /// **Domain evidence** for a `Field` slot: a single use-site where the
    /// slot interacts with a value operand (a comparison `slot == E`, an
    /// assignment `slot = E`, a `switch(slot){case E}`, a typed-arg
    /// position). `Some(enum)` when the operand resolves to an enumerator
    /// (its enum, resolved cross-file before the witness is pushed);
    /// `None` when it does not — an integer literal, arithmetic, a call —
    /// which is COUNTER-evidence: the site still counts in the coherence
    /// vote's denominator, so a slot that is dominantly a plain integer
    /// with a minority enum idiom refuses a domain instead of reporting
    /// 100% confidence. `DomainCoherenceFold` folds every site:
    /// mostly-agree → that domain, truly-mixed → none. Kept at the END for
    /// bincode variant-index stability (bump `EXTRACT_VERSION`).
    DomainCompare { enum_type: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ProjectionStep {
    HashKey(String),
    ArrayIndex(i32),
    /// A method-call hop off the base value: "dispatch `member` on
    /// whatever class the base resolves to, at the call's own arity."
    /// The registry's spelling of a receiver-EXPRESSION method call —
    /// `$a->b()->c()` has no variable for the outer hop's receiver, so
    /// no `MethodCallBinding` can bridge it; the hop defers the
    /// dispatch to query time, when the base's class (and the index)
    /// are in hand, then chases `PackageSymbol{class, member}` like any
    /// edge. Minted per member-call site by pack extraction; the base
    /// is the receiver's `Variable` (simple receiver) or `Expr` (a
    /// nested call's span, carrying its own hop). Kept at the END for
    /// bincode variant-index stability (bump `EXTRACT_VERSION`).
    MethodHop { member: String, arity: u32 },
    /// The UNIFORM element of a sequence — the foreach/iteration peel
    /// (`foreach ($this->handlers as $handler)` types `$handler` as the
    /// collection's element). Projects a `Sequence` all of whose elements
    /// agree to that one type; a heterogeneous tuple or an untyped
    /// `ArrayRef`/`HashRef` answers `None` (no index is in hand, so no
    /// per-slot answer exists). Kept at the END for bincode variant-index
    /// stability (bump `EXTRACT_VERSION`).
    Element,
    /// The KEY axis of an iterated collection — the pair-form foreach's
    /// first binding (`foreach ($m as $k => $v)`). A `Sequence`'s keys ARE
    /// its positions (`Numeric`); a two-argument parametric instance
    /// (`array<string, V>` docs) projects its first argument. Kept at the
    /// END for bincode variant-index stability (bump `EXTRACT_VERSION`).
    Key,
}

/// A sub's return type as a **deferred computation**, not a value:
/// conceptually `(receiver, arity) -> InferredType`. `ReturnExprReducer`
/// evaluates it against the query's `q.receiver` / `q.arity_hint`.
///
/// This is deliberately distinct from `InferredType` and must NOT be
/// merged into it: `Receiver` is a free variable and `UnionOnArgs` is an
/// arity-indexed dispatch table — neither is a concrete type. Folding
/// these into `InferredType` would force every type *consumer* to handle
/// "what if this is still an unsubstituted hole / a dispatch table?".
/// Keeping the schema (`ReturnExpr`) separate from the value
/// (`InferredType`) confines that concern to `eval_return_expr`.
///
/// See `docs/adr/return-expr.md` and `docs/adr/parametric-types.md` for
/// the sealed-enum rationale (every consumer matches, no `_ => …`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ReturnExpr {
    /// Concrete type — equivalent to a plain `InferredType` payload on a
    /// Symbol attachment.
    Concrete(InferredType),
    /// Receiver placeholder. Evaluates to `q.receiver`; `None` when the
    /// query carries no receiver (build-time lookup, not a call site) —
    /// the reducer returns `None` rather than guessing.
    Receiver,
    /// Receiver-polymorphic constructor return: the call-site invocant's
    /// class, else the carried fallback when there is no receiver. This is
    /// the `bless {}, $class` / `bless {}, ref $self || $self` idiom — an
    /// inherited constructor returns whatever class it was *called on*
    /// (`Child->new` → `Child`), so it must substitute the receiver; the
    /// fallback (the enclosing class) keeps bare `sub_return_type` queries
    /// answering instead of going `None`. Composes through `SUPER::new`.
    ReceiverOr(InferredType),
    /// Apply a parametric operator with `ReturnExpr`-valued sub-positions.
    /// Substitution recurses, evaluates, and re-wraps as `ParametricType`
    /// so the value-side accessors (`class_name`, `hash_key_class`, …)
    /// handle consumption downstream.
    Operator(ParametricOp),
    /// Union over arg-shape. Each branch is `(guard, expr)`; for a
    /// concrete `arity_hint` the first matching guard wins. For a
    /// hint-less query the `Any` branch is preferred, falling back to
    /// `Empty` (so a Mojo `has` getter+writer pair surfaces its primary).
    /// Branch order matters when the hint is concrete — narrow guards
    /// (`Empty`, `Exact`, `AtLeast`) before `Any`.
    UnionOnArgs { branches: Vec<(ArgGuard, ReturnExpr)> },
    /// The call's `n`-th argument type — the positional mirror of
    /// `Receiver`. A parametric identity/projection macro (`#define ID(x)
    /// (x)`, `#define SEL2(a,b) (b)`) declares `Arg(n)` on its Symbol;
    /// `eval_return_expr` substitutes `q.args[n]`, `None` when the query
    /// carries no args (build-time probe / no call site) — same honest
    /// `None` as `Receiver` without a receiver. In-file call sites resolve
    /// the concrete value by chasing the argument's own `Expr` witness
    /// (edges-not-values); this declarative form serves introspection and
    /// arg-threaded queries. See `docs/adr/macro-handling.md`.
    ///
    /// Kept at the END for bincode variant-index stability (bump
    /// `EXTRACT_VERSION`).
    Arg(u32),
}

/// Type-level operators with `ReturnExpr`-valued sub-positions —
/// projections that can't resolve until the receiver is substituted, so
/// they live on the deferred (`ReturnExpr`) side, not on the concrete
/// `InferredType`/`ParametricType` value side. No `_ => …` fall-throughs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ParametricOp {
    /// `RowOf<T>` — projects a `ResultSet { base, row }` to its row class.
    /// `eval_return_expr` evaluates the sub-expression and projects
    /// eagerly: `ResultSet { row, .. }` → `ClassName(row)`, anything else
    /// → `None`.
    RowOf(Box<ReturnExpr>),
    /// `ParamOf<i>` — projects a template `Instance { args }` to its i-th
    /// type argument (`Box<int>::get() → int`). The param-indexed sibling
    /// of `RowOf`: same lazy receiver substitution, a positional axis
    /// instead of the row axis. An operand with no instance args (a bare
    /// `ClassName`, a `ResultSet`) has no i-th parameter → `None`.
    ///
    /// Kept AFTER `RowOf` for bincode variant-index stability (bump
    /// `EXTRACT_VERSION`).
    ParamOf { index: u32, of: Box<ReturnExpr> },
    /// Re-wraps evaluated sub-positions as a template instance —
    /// `vector<T> all()` on `Box<int>` evaluates each arg (`ParamOf`)
    /// and yields `Instance { base: "vector", args: [Numeric] }`, so a
    /// param nested one hop under a template spelling substitutes. Any
    /// arg evaluating to `None` fails the whole instance (an
    /// under-substituted spelling would lie).
    ///
    /// Kept at the END for bincode variant-index stability.
    InstanceOf { base: String, args: Vec<ReturnExpr> },
}

/// Guard for `ReturnExpr::UnionOnArgs` branches, matched against
/// `ReducerQuery.arity_hint`. A `None` hint matches `Any` only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ArgGuard {
    Empty,
    Exact(u32),
    AtLeast(u32),
    /// Arity ≤ N — the `unless @_ > N` band (a low-arity getter guard,
    /// including the compound `unless @_ > N || <non-arity>` where `@_ ≤ N`
    /// is the sound necessary condition).
    AtMost(u32),
    Any,
}

impl ArgGuard {
    /// Match against the call's arity hint. Strict: a guard fires only
    /// when the hint positively matches it; `Any` is the only catch-all,
    /// so a `None` hint never silently fires `Empty`/`Exact`/`AtLeast`.
    ///
    /// Introspection callers pass `None`; sym-introspection entry points
    /// compensate by defaulting the hint from the sym's own `params`
    /// count (a writer sym, params=1, matches `AtLeast(1)`; a getter,
    /// params=0, matches `Empty`).
    pub fn matches(self, arity_hint: Option<u32>) -> bool {
        match (self, arity_hint) {
            (ArgGuard::Empty, Some(0)) => true,
            (ArgGuard::Exact(n), Some(h)) => n == h,
            (ArgGuard::AtLeast(n), Some(h)) => h >= n,
            (ArgGuard::AtMost(n), Some(h)) => h <= n,
            (ArgGuard::Any, _) => true,
            _ => false,
        }
    }
}

/// Raw observations about a value's use, consumed by the
/// framework-aware resolver. These do NOT commit to a concrete type; the
/// resolver projects them to `InferredType` using framework context.
///
/// Hash/Eq are intentionally NOT derived: `InferredType` is `PartialEq`
/// but not `Hash`. Attachment-keyed indexing uses `WitnessAttachment`,
/// not the payload, so Hash on the observation isn't needed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeObservation {
    /// `my $x = Foo->new` or direct `InferredType::ClassName(_)` assertion.
    ClassAssertion(String),
    /// `my $self = shift` / `$_[0]` at the head of a method body.
    FirstParamInMethod { package: String },
    /// `$v->{k}`, `%$v`, `@$v{...}` — hashref-like access.
    HashRefAccess,
    /// `$v->[i]`, `@$v`.
    ArrayRefAccess,
    /// `$v->()`, `&$v`.
    CodeRefInvocation,
    NumericUse,
    StringUse,
    RegexpUse,
    /// `bless [], $c` pins the representation axis to Array.
    BlessTarget(Rep),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Rep {
    Hash,
    Array,
    Scalar,
    Code,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum FactValue {
    Str(String),
    List(Vec<FactValue>),
    Bool(bool),
    Num(f64),
    Map(Vec<(String, FactValue)>),
}

// ---- Framework-mode mirror (builder's FrameworkMode is private to
// builder.rs, so the resolver duplicates a small view) ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[allow(dead_code)]
pub enum FrameworkFact {
    Moo,
    Moose,
    /// Mojo::Base — hashref-backed, fluent-by-default.
    MojoBase,
    /// Perl 5.38 `class` — opaque / inside-out.
    CoreClass,
    /// No framework detected.
    Plain,
}

impl FrameworkFact {
    /// Which representation does this framework's instances back onto?
    /// `None` = rep-agnostic.
    pub fn backing_rep(self) -> Option<Rep> {
        match self {
            FrameworkFact::Moo | FrameworkFact::Moose | FrameworkFact::MojoBase => Some(Rep::Hash),
            FrameworkFact::CoreClass => None, // opaque
            FrameworkFact::Plain => None,
        }
    }
}
