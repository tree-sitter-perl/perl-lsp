//! Plugin namespaces, bridges, receiver-gated dispatch, call bindings
//! and deref/guard site records.

use super::*;

// ---- Handler owner ----

/// Plugin-chosen LSP display kind for a `Handler`. Handlers all share
/// the same internal mechanism (string-dispatched, stacked, cross-file),
/// but they aren't all the same thing *semantically* — routes are
/// method-ish, events are event-ish, config keys are field-ish. The
/// plugin decides what icon the editor shows.
///
/// Expand this enum when a plugin has a concept that doesn't fit any
/// existing variant; every variant maps to a corresponding LSP
/// `SymbolKind` / `CompletionItemKind` via thin translation in
/// `symbols.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HandlerDisplay {
    Event,
    Method,
    Function,
    Field,
    Property,
    Constant,
    /// Plugin-synthesized callable on the framework's app instance.
    /// Renders with LSP kind FUNCTION; outline detail prints "helper".
    Helper,
    /// Framework-declared URL pattern. Same LSP kind as Helper/Task;
    /// outline detail prints "route" so the client can disambiguate.
    Route,
    /// A controller action referenced from a routing declaration —
    /// e.g. the `Users#list` target of Mojolicious' `->to('Users#list')`
    /// (Mojo's own docs call this an "action") or Catalyst's
    /// `->forward('/users/list')`. Distinct from `Route` because no
    /// request-handling body lives at this source site: it's a
    /// cross-reference to a method that lives elsewhere. Outline word
    /// "action" so `<route> GET /users` and `<action> Users#list` read
    /// as two different kinds of line items.
    Action,
    /// Job-queue / worker task (Minion etc.). Helper-kin for the LSP
    /// kind, distinct "task" word in outline detail.
    Task,
}

impl Default for HandlerDisplay {
    fn default() -> Self { Self::Event }
}

impl HandlerDisplay {
    /// Short human-readable word the outline puts in `detail` so LSP
    /// clients can show `[Function] name — helper` even though the
    /// LSP kind enum doesn't have a Helper variant. Returns `None`
    /// for display kinds that don't carry a distinguishing word
    /// beyond the LSP kind itself.
    pub fn outline_word(&self) -> Option<&'static str> {
        match self {
            HandlerDisplay::Event => Some("event"),
            HandlerDisplay::Helper => Some("helper"),
            HandlerDisplay::Route => Some("route"),
            HandlerDisplay::Action => Some("action"),
            HandlerDisplay::Task => Some("task"),
            _ => None,
        }
    }
}

/// Owner of a `Handler` symbol. Distinct from `HashKeyOwner` because
/// hash keys and dispatch handlers are different concepts even though
/// both happen to be keyed by a name under a class. Keeping them split
/// prevents overload creep — each stays free to evolve on its own axis.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HandlerOwner {
    /// Handler is registered on a specific class (typical for Mojo
    /// events, Moose roles, DBIC relationships, etc.).
    Class(String),
}

// ---- Plugin namespace ----

/// A plugin-controlled scope: the plugin says "I own a namespace — here's
/// its bridges (how Perl-space expressions find it) and its entities",
/// rather than masquerading entities as Methods on a hijacked Perl class.
///
/// Why this exists:
///   * Helpers aren't methods on `Mojolicious::Controller`. They're
///     callables on the app instance, reached THROUGH a controller.
///   * Two apps in one workspace become two `PluginNamespace`s with
///     the same `Class("Mojolicious::Controller")` bridge. Their
///     entities don't collide at the class level — they're owned by
///     the namespace, not the class.
///   * Cross-file lookup is one primitive
///     (`ModuleIndex::for_each_entity_bridged_to(class, ...)`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginNamespace {
    /// Plugin-generated unique identifier. E.g.
    /// `"mojo-app:/abs/path/to/MyApp.pm"` or
    /// `"minion:$minion@MyApp.pm:5"`. Plugin decides how to
    /// disambiguate multiple instances in a single workspace.
    pub id: String,
    /// Which plugin registered this namespace — the Namespace::Framework
    /// `id` that gets stamped on emitted entities.
    pub plugin_id: String,
    /// Plugin-defined kind tag. `"app"`, `"minion"`, `"emitter"`, …
    /// Used by display/completion to tell users what sort of thing
    /// a namespace member is.
    pub kind: String,
    /// Symbols that belong to this namespace. Cross-references into
    /// the same FileAnalysis's `symbols` table — plugins still emit
    /// Methods / Handlers normally; the namespace indexes them.
    pub entities: Vec<SymbolId>,
    /// How Perl-space expressions reach this namespace's entities.
    pub bridges: Vec<Bridge>,
    /// Span where the plugin declared the namespace (typically the
    /// registration call — `$app->plugin('Minion', ...)` etc.).
    pub decl_span: Span,
}

/// A connection from a Perl-space type/shape into a plugin namespace.
/// When a lookup asks "what's reachable from class X?", the core
/// unions Perl-native methods with entities from every namespace
/// whose bridges match X.
///
/// Currently only `Class` is wired — `Bareword` / `Variable` would
/// require lookup machinery (`bareword_index`, per-variable bridge
/// table) that doesn't exist yet. Re-add them when a concrete plugin
/// needs the shape; speculative variants just rot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Bridge {
    /// Any expression typed as this class (or a subclass reached via
    /// inheritance walk) can see this namespace's entities. The
    /// canonical bridge for framework helpers on controllers.
    Class(String),
}

/// Fictional "app surface" class — the synthetic ancestor that the
/// Mojolicious app / controller / command classes (the manifest-declared
/// consumer set, see `FrameworkPlugin::app_surface_consumers`) all
/// inherit, so a single bridge target reaches every receiver that can see
/// helpers (see `FrameworkPlugin::app_surface_consumers`, `docs/adr/plugin-system.md`).
/// Helpers bridge to THIS one class;
/// the consumer classes get it as a synthetic parent injected in the MRO
/// walk (`parents_of`). The existing ancestor walk + bridge resolution
/// then finds helpers with no per-receiver bridge list. Not a real Perl
/// package — never resolves to a file, has no parents, so it's inert in
/// the walk beyond contributing its bridge.
pub const APP_SURFACE_CLASS: &str = "Mojolicious::_AppSurface";

/// The lexical scope chain `[start, parent, …, file]` over a bare
/// `&[Scope]` slice — the single source of the parent-climb. A free
/// function (not a `FileAnalysis` method) so the witness-bag query path,
/// which holds `BagContext.scopes: &[Scope]` and never a
/// `&FileAnalysis`, shares it. `FileAnalysis::scope_chain` is the thin
/// wrapper. A scope has one parent and no cycles, so this is a linked-
/// list climb, not a graph walk — the graph deliberately does not model
/// it (`docs/adr/graph-walking.md`).
pub fn scope_chain_of(scopes: &[Scope], start: ScopeId) -> Vec<ScopeId> {
    let mut chain = Vec::new();
    let mut current = Some(start);
    while let Some(id) = current {
        chain.push(id);
        current = scopes[id.0 as usize].parent;
    }
    chain
}

/// Three-way outcome of resolving a [`ReceiverGated`] value against a
/// concrete receiver class. Splitting "doesn't apply" from "can't tell"
/// is load-bearing: `DoesNotApply` is a settled negative (the receiver
/// typed, it just isn't a descendant of the gate), while `ReceiverUntyped`
/// is a *typing gap* — the receiver couldn't be pinned to any class, so
/// applicability is unknown. The opt-in `unresolved-dispatch` diagnostic
/// fires only on the latter; treating the two alike would either bury real
/// gaps or spew noise on every unrelated receiver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateResult<U> {
    /// Receiver `isa` the gate class — here is the inner value.
    Applies(U),
    /// Receiver typed to a concrete class that is NOT a descendant of the
    /// gate. Settled negative; never diagnosed.
    DoesNotApply,
    /// Receiver class is unknown (`None` / unresolved). A genuine typing
    /// gap — the only state the diagnostic surfaces.
    ReceiverUntyped,
}

/// A value whose inner payload can be read ONLY through a cross-file isa
/// check against a receiver class. The enforcement is structural, not a
/// convention: `inner` is private with no `pub` field, no `Deref`, no
/// `into_inner` — the sole reader is [`resolve_for`](Self::resolve_for),
/// which gates on the receiver. A consumer therefore *cannot* observe
/// gated content without first asking "does this receiver qualify?", so a
/// future caller that forgets the isa filter is a compile error, not a
/// silent drift (rule #10: the type carries the rule, the consumer can't
/// re-decide it).
///
/// `gate` is a single `ClassName` today; widening it to a set later is a
/// change to `resolve_for`'s internals, not to call sites — they already
/// only ever see `Applies`/`DoesNotApply`/`ReceiverUntyped`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiverGated<T> {
    /// The receiver must `isa` this class for `inner` to be readable.
    gate: String,
    /// Gated payload. PRIVATE by design — see the type's doc.
    inner: T,
}

impl<T> ReceiverGated<T> {
    /// Mint a gated value. The only constructor — pairs the payload with
    /// the class the receiver must descend from to read it.
    pub fn new(gate: impl Into<String>, inner: T) -> Self {
        Self { gate: gate.into(), inner }
    }

    /// The gate class, exposed for diagnostics/observability. Reading the
    /// gate is harmless — it's the *inner payload* that's protected.
    pub fn gate(&self) -> &str {
        &self.gate
    }

    /// The one reader. `receiver_class` is the concrete class of the
    /// dispatch receiver as the bag resolved it (cross-file aware); `None`
    /// or an unresolved name yields `ReceiverUntyped`. Otherwise the inner
    /// value is handed back iff the receiver `isa` the gate, walking the
    /// single `class_isa` seam (local `PackageFacts::parents` ∪ cross-file
    /// `parents_cached`).
    pub fn resolve_for(
        &self,
        receiver_class: Option<&str>,
        local: &dyn LocalParents,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> GateResult<&T> {
        match receiver_class {
            None => GateResult::ReceiverUntyped,
            Some(recv) if recv.is_empty() => GateResult::ReceiverUntyped,
            Some(recv) => {
                if class_isa(recv, &self.gate, local, module_index) {
                    GateResult::Applies(&self.inner)
                } else {
                    GateResult::DoesNotApply
                }
            }
        }
    }
}

// ---- Hash key owner (for scope graph) ----

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HashKeyOwner {
    Class(String),
    /// A plugin-owned COLUMN namespace bridged to a class (DBIC /
    /// `Class::Accessor`): a named field addressable in query-condition args
    /// (`$rs->search({ col => … })`) and via its accessor (`$row->col`), but
    /// NOT a literal hash slot of the object — `$row->{col}` is undef in DBIC
    /// (columns live behind `_column_data`). Deliberately distinct from
    /// `Class` (real instance hash keys: Moo `InternalKey` slots, bless keys),
    /// so a generic `$obj->{key}` deref (a `Class` lookup) never reaches a
    /// column. Rule #8: plugin-synthesized content lives in its own namespace,
    /// not masquerading as the class's own keys.
    Bridged { class: String },
    Variable { name: String, def_scope: ScopeId },
    /// Hash keys from a sub's return value: `sub get_config { return { host => 1 } }`.
    /// `package` is the enclosing Perl package at the sub's declaration site
    /// (or `None` for top-level script subs where no `package` statement is
    /// in scope). Without this, two different packages each defining
    /// `sub get_config { ... host ... }` would collide at query time.
    Sub { package: Option<String>, name: String },
}

impl HashKeyOwner {
    /// Directional match: would a lookup with `lookup` owner reach a
    /// def with `self` owner? Strict equality, plus the broadening
    /// rule that a `Class(C)` lookup picks up `Sub{Some(C), _}` defs.
    ///
    /// Why: bless-inside-a-sub registers HashKeyDefs as `Sub{C,
    /// sub_name}` (the constructor sub). `has` does the same. But
    /// `$obj->{key}` deref refs and `complete_hash_keys_for_class`
    /// callers carry `Class(C)` — that's the "any key for objects of
    /// C" lookup. The asymmetry keeps strict `Sub{C, M}` lookups
    /// from accidentally finding keys registered to a *different*
    /// method on the same class.
    pub fn found_by(&self, lookup: &HashKeyOwner) -> bool {
        if self == lookup { return true; }
        match (self, lookup) {
            (HashKeyOwner::Sub { package: Some(c1), .. }, HashKeyOwner::Class(c2)) => c1 == c2,
            _ => false,
        }
    }
}


// ---- Call binding ----

/// A variable assigned from a function call: `my $cfg = get_config()`.
/// Stored in FileAnalysis so query-time resolution can follow the chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallBinding {
    pub variable: String,
    pub func_name: String,
    pub scope: ScopeId,
    pub span: Span,
}

/// One `$var->{key} = …` write observed at walk time. The mutation-
/// extension pass (`witnesses::emit_mutation_extension_witnesses`)
/// folds these into the variable's structural shape: an unconditional
/// static-key write extends a closed `HashWithKeys` (the key joins the
/// shape, its value typed from `rhs_span`); a dynamic key or a
/// conditionally-executed write switches the shape open
/// (docs/adr/structural-shapes.md). Persisted so cross-file
/// enrichment can re-run the pass once imported shapes land.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyWrite {
    pub var_text: String,
    pub key: WriteKey,
    pub scope: ScopeId,
    /// Key-node span — temporal anchor and per-var ordering.
    pub span: Span,
    /// RHS expression span — types the extended key's value.
    pub rhs_span: Option<Span>,
    /// Syntactically conditional within its sub (if/postfix/ternary/
    /// loop/short-circuit). Scope-crossing writes (nested block or
    /// closure relative to the decl scope) are detected in the pass.
    pub conditional: bool,
}

/// What a `KeyWrite` lands on.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WriteKey {
    /// Static hash key — extends/retypes the named entry on a
    /// `HashWithKeys` shape.
    Hash(String),
    /// Static array index (direct arrow write, `$v->[N] = …`) —
    /// retypes the slot / appends at `len` on a `Sequence` tuple.
    Index(i32),
    /// Dynamic key, slice, or escape — membership unknowable;
    /// switches a `HashWithKeys` shape open.
    Unknown,
}

/// A method call binding: `$var = $invocant->method()`.
/// Recorded during build, resolved in post-pass via `find_method_return_type`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodCallBinding {
    pub variable: String,
    pub invocant_var: String,
    pub method_name: String,
    pub scope: ScopeId,
    pub span: Span,
}

/// The readable half of a dispatch candidate — everything needed to
/// synthesize a `DispatchCall` ref / handler link ONCE the receiver passes
/// the gate. Carried as the inner payload of a [`ReceiverGated`], so the
/// only way to reach these fields is `resolve_for(receiver_class, …)`:
/// no consumer can mistake an unfiltered candidate for a confirmed
/// dispatch. The gate class (`target_class`) lives on the wrapper.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchCandidate {
    /// The handler name (task / event) — the dispatch's first meaningful arg.
    pub name: String,
    /// Span of the name argument (the future `DispatchCall` ref span).
    pub span: Span,
    /// The verb (`enqueue`), kept for the `DispatchCall.dispatcher`.
    pub dispatcher: String,
    /// Handler owner the synthesized ref pairs against (e.g. `Minion`).
    pub owner_class: String,
    /// Receiver's class as resolved at build time, if any. `None` when the
    /// receiver type wasn't known locally (e.g. a helper-returned value);
    /// query-time resolution then re-resolves it cross-file via `call_span`'s
    /// MethodCall ref + the module index.
    #[serde(default)]
    pub receiver_class: Option<String>,
    /// Whole-call span of the dispatch call (`node_to_span` of the
    /// `method_call_expression`). Matches the native MethodCall ref's `span`,
    /// so the resolver can find that ref and resolve the receiver class with
    /// the index when `receiver_class` is `None`.
    pub call_span: Span,
}

/// A build-time dispatch candidate gated on its receiver type: a call to a
/// plugin-declared dispatch verb (`$x->enqueue('T')`), recorded before we
/// know whether the receiver actually `isa` the verb's target class. The
/// gate (`target_class`) lives on the `ReceiverGated` wrapper; resolution
/// is cross-file and happens at QUERY time in `resolve.rs`
/// (`refs_to`) and dispatch goto-def — never eagerly materialized, so
/// candidates in non-open workspace/dependency files surface the same as
/// open ones. See `docs/adr/receiver-gated-dispatch.md`.
pub type ProvisionalDispatch = ReceiverGated<DispatchCandidate>;

impl ProvisionalDispatch {
    /// Receiver-locator accessors. These three fields are gate *input*, not
    /// gated *content*: the host needs the call site to resolve the receiver
    /// class that the gate then checks (chicken-and-egg otherwise). Only the
    /// handler-link payload (`name`, `owner_class`) stays behind
    /// `resolve_for`. Defined ON the type so the type author — not an outside
    /// consumer — draws the input/content line.
    pub(super) fn receiver_hint(&self) -> Option<&String> {
        self.inner.receiver_class.as_ref()
    }
    pub(super) fn call_span(&self) -> Span {
        self.inner.call_span
    }
    pub(super) fn dispatcher(&self) -> &str {
        &self.inner.dispatcher
    }
}

/// A plugin PATTERN emission (`on_match` output) deferred at build because
/// its `ClassIsa` trigger couldn't be confirmed against the file's
/// LOCAL-only ancestry (rule #1: the builder is index-free, so
/// `transitive_parents` sees only in-file parents). The idiomatic case is a
/// DBIC result class whose `isa DBIx::Class` route runs through an
/// intermediate base in another file (`Artist → BaseResult → DBIx::Class::
/// Core`), so the syntactically-matched `has_many`/`add_columns` synthesis
/// never fires.
///
/// The build records the already-computed emission (tree-free,
/// file-analysis-native — the `on_match` result translated in
/// `pattern_dispatch`, which speaks `EmitAction`) plus the gate prefixes.
/// `enrich_imported_types_with_keys` re-fires it when the package's ancestry
/// resolves ANY gate prefix CROSS-FILE (`class_isa_prefix`, the same MRO
/// seam as every other ancestry walk). Idempotent by construction: the
/// re-fired symbols/refs land ABOVE the symbol/ref tables' baselines
/// and are truncated + re-derived every enrichment cycle, so a file whose
/// ancestry resolves late converges to the same analysis as one built with
/// it known — the same discipline as `ReceiverGated` dispatch, but for
/// symbol emission (which feeds every symbol-table consumer, so it can't be
/// gated at one query seam — it must materialize into the analysis).
/// See `docs/adr/receiver-gated-dispatch.md` (Phase 2) and
/// `docs/prompt-enrichment-inheritance-residual.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatedEmission {
    /// Any of these `ClassIsa` prefixes holding cross-file re-fires the
    /// emission — trigger semantics are OR across a plugin's triggers, and
    /// only `ClassIsa` triggers can newly-fire cross-file (`UsesModule` /
    /// `Always` are settled locally at build).
    pub gate_prefixes: Vec<String>,
    /// The package whose cross-file ancestry is checked against the gate.
    pub package: String,
    /// Match-site point; the re-fired symbols/refs attach to the scope here.
    #[serde(with = "PointDef")]
    pub scope_point: Point,
    /// The plugin id — namespace-tags the re-fired symbols (`Framework{id}`).
    pub plugin_id: String,
    /// Symbols the pattern's `on_match` produced (columns, relationship
    /// accessors, event handlers, …).
    pub symbols: Vec<GatedSymbol>,
    /// Refs the pattern produced (dispatch-call / method-call / hash-key
    /// access sites) so cross-file references reach the re-fired symbols.
    pub refs: Vec<GatedRef>,
}

/// One symbol inside a [`GatedEmission`] — the file-analysis-native
/// projection of a symbol-emitting `EmitAction` (`Method` / `HashKeyDef` /
/// `Handler` / `Symbol`). The `SymbolId` is minted at apply time (it must
/// equal the symbol's positional index — `FileAnalysis::symbol` indexes by
/// it), so it is deliberately absent here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatedSymbol {
    pub name: String,
    pub kind: SymKind,
    pub span: Span,
    pub selection_span: Span,
    pub detail: SymbolDetail,
    /// Presentation policy carried from the emitting action, stamped
    /// onto the minted `Symbol` at apply time.
    #[serde(default)]
    pub presentation: Presentation,
    /// Explicit owning class (`EmitAction::Method.on_class`); when `None` the
    /// symbol is keyed under the emission's match-site package.
    pub on_class: Option<String>,
    /// Return type → a `Symbol(sid) → InferredType` Plugin-priority bag
    /// witness pushed at apply (plus a `PackageSymbol{package,name}` mirror for
    /// class-scoped methods, so cross-file return-type queries reach it).
    pub return_type: Option<InferredType>,
}

/// One ref inside a [`GatedEmission`]. Scope is resolved at apply from the
/// emission's `scope_point`; `binding` carries the plugin-declared owner —
/// its linked symbol is left for the enrichment re-index to fill
/// (HashKeyAccess → HashKeyDef) the same way build does.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatedRef {
    pub kind: RefKind,
    pub span: Span,
    pub target_name: String,
    pub access: AccessKind,
    #[serde(default)]
    pub binding: Option<RefBinding>,
}

/// A confirmed dispatch — a gated candidate whose receiver isa-resolved at
/// query time. The projection `refs_to` / goto-def consume to match a
/// `Handler` target `(owner, name)` at `span`.
#[derive(Debug, Clone)]
pub struct AppliedDispatch {
    pub name: String,
    pub span: Span,
    pub owner: HandlerOwner,
}

/// A dispatch candidate whose receiver couldn't be typed — a typing gap the
/// opt-in diagnostic surfaces. Never `DoesNotApply` (that's a settled
/// negative).
#[derive(Debug, Clone)]
pub struct UntypedDispatch {
    pub call_span: Span,
    pub dispatcher: String,
    pub gate: String,
}

/// The dereference form at a `DerefSite` — what the receiver is being used
/// as, which decides both the diagnostic wording and (for D6) the rep the
/// access demands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DerefForm {
    /// `$x->method(...)` — carries the method name.
    Method(String),
    /// `$x->{key}` hash dereference.
    HashKey,
    /// `$x->[i]` array dereference.
    ArrayIndex,
    /// `$x->(...)` code-ref call.
    Call,
}

impl DerefForm {
    /// The container rep this form requires of its receiver, if any. A method
    /// call requires nothing structural (any object/value can receive one).
    pub fn demands_rep(&self) -> Option<RepKind> {
        match self {
            DerefForm::HashKey => Some(RepKind::Hash),
            DerefForm::ArrayIndex => Some(RepKind::Array),
            DerefForm::Call => Some(RepKind::Code),
            DerefForm::Method(_) => None,
        }
    }

    /// Human phrase for diagnostics ("a hash deref", …).
    pub fn access_phrase(&self) -> &'static str {
        match self {
            DerefForm::HashKey => "a `->{...}` hash deref",
            DerefForm::ArrayIndex => "a `->[...]` array deref",
            DerefForm::Call => "a `->(...)` call",
            DerefForm::Method(_) => "a method call",
        }
    }
}

/// The container rep of a value — what `ref()` reports. The D6 axis: a deref
/// form demands one rep; a guard may prove the receiver is another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepKind {
    Hash,
    Array,
    Code,
}

impl RepKind {
    /// The rep of a type, if it has a concrete container rep. `ClassName`
    /// (a blessed object) answers `None` — it can legitimately overload any
    /// deref, so it is never a mismatch.
    pub fn of(ty: &InferredType) -> Option<RepKind> {
        if ty.is_hash_shaped() {
            Some(RepKind::Hash)
        } else if ty.is_array_shaped() {
            Some(RepKind::Array)
        } else if matches!(ty, InferredType::CodeRef { .. }) {
            Some(RepKind::Code)
        } else {
            None
        }
    }

    pub fn noun(self) -> &'static str {
        match self {
            RepKind::Hash => "a hash ref",
            RepKind::Array => "an array ref",
            RepKind::Code => "a code ref",
        }
    }
}

/// A scalar-receiver dereference paired with the receiver's narrowed type
/// at the use point — see `FileAnalysis::deref_receiver_sites`.
#[derive(Debug, Clone)]
pub struct DerefSite {
    /// The diagnostic range (the dereferencing ref's span).
    pub span: Span,
    /// Receiver spelling (`$x`).
    pub receiver: String,
    /// Receiver type at the use point, with narrowing applied.
    pub receiver_ty: InferredType,
    pub form: DerefForm,
}

/// A builder-recorded arrow-deref receiver for the forms that carry no
/// typed ref of their own (`$x->[i]`, `$x->()`) — the array/code analog of
/// the method-call and hash-deref refs `deref_receiver_sites` reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArrowDerefSite {
    pub receiver: String,
    pub span: Span,
    pub form: DerefForm,
}

/// What a recorded guard tests about its subject — the build-time
/// recognition (`builder/narrowing.rs`) projected to a query-time fact so
/// the redundant/contradictory-guard diagnostics (D3/D4) can compare it
/// against the subject's prior type without re-walking the tree.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum GuardPredicate {
    /// `isa`/`DOES`/`ref…eq` proving a concrete type.
    IsType(InferredType),
    /// `defined`/`blessed` — asserts the subject is not undef.
    Defined,
}

/// A guard condition recorded at build time for the redundancy diagnostics
/// (D3/D4). The narrowing engine already recognizes these; this captures
/// the subject + predicate + the point to read the subject's PRIOR type
/// (in the guard, before any narrowed region).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardSite {
    /// Subject's source spelling (`$x`, or a place key).
    pub subject: String,
    pub scope: ScopeId,
    pub predicate: GuardPredicate,
    /// Whether the predicate holds where the guard EXPRESSION is true (`!`
    /// flips it) — lets D3/D4 resolve always-true vs always-false.
    pub asserts_when_true: bool,
    /// Diagnostic range (the guard condition).
    pub span: Span,
    /// Where to read the subject's prior (un-narrowed) type.
    #[serde(with = "PointDef")]
    pub before_point: Point,
}

/// A D3/D4 verdict on a recorded guard, ready for `symbols.rs` to render.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardVerdict {
    /// The condition is always true given the subject's prior type (D3).
    AlwaysTrue,
    /// The condition is always false (D4).
    AlwaysFalse,
}

/// A resolved redundant/contradictory-guard finding — the **neutral** facts
/// the render layer turns into a message, never the English itself. The model
/// is the driver-neutral IR (`language_driver.rs`): every language's D3/D4
/// diagnostics read the same verdict, and the user-facing phrasing ("this
/// guard is redundant" vs a language's own wording) is a render-time concern
/// that belongs at the edge, not baked into the analysis.
#[derive(Debug, Clone)]
pub struct GuardRedundancy {
    pub span: Span,
    pub verdict: GuardVerdict,
    /// Subject's source spelling (`$x`), for the rendered message.
    pub subject: String,
    /// The predicate the guard tested — the renderer names the concrete type
    /// (`IsType`) or definedness (`Defined`) as the message needs.
    pub predicate: GuardPredicate,
}

/// A read of a hash key that the base's CLOSED structural shape does not
/// define — the unknown-hash-key finding, in neutral facts for the render
/// layer. Produced by `FileAnalysis::closed_shape_key_typos` (variable base)
/// and `projected_key_typos` (expression base).
#[derive(Debug, Clone)]
pub struct KeyTypoSite {
    /// Diagnostic range (the keyed read).
    pub span: Span,
    /// The key as spelled at the read site.
    pub key: String,
    /// Every key the closed shape defines, untruncated — eliding long lists
    /// is the renderer's call.
    pub known_keys: Vec<String>,
    /// The base's source spelling (`$config`, `%config`) when the base is a
    /// variable; `None` for the expression-base spelling (`cfg()->{k}`).
    pub spelling: Option<String>,
}

