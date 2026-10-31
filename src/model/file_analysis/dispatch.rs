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
    /// A flat, receiver-less namespace — one rail per kind of string-named
    /// framework entity (WordPress hooks, Laravel route names, view names,
    /// config keys …), declared by the overlay capture's suffix
    /// (`@def.handler.named.route` / `@ref.dispatch.named.route`). No
    /// receiver types the dispatch — name + owner equality is the entire
    /// match, so receiver-gated machinery (dispatch-verb manifests,
    /// invocant matching) skips these; the rail keeps a route name and a
    /// same-spelled view name apart. Every flat namespace is NAMED by its
    /// overlay: an unnamed "global" one would claim the whole program for
    /// what is one framework's hook space. What the rail's names ARE
    /// (`RailNames`) is the rail document's declaration, asked through
    /// `names_are`, never a second variant.
    Rail(String),
}

/// What a rail's names denote — the rail document's `names_are`
/// declaration, carried as `PackFacts::class_named_rails` and read through
/// `HandlerOwner::names_are`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailNames {
    /// The name is a string the framework matches verbatim (a route name,
    /// a view, a config key, a hook): renameable, the edit rewriting
    /// inside the quotes at every site.
    Strings,
    /// The name is a CLASS identity (Laravel's event bus: an emission
    /// `event(new X)` and a listener's `handle(X $e)` meet on `X`). Spans
    /// sit on the emission and handler TOKENS, never on the name itself,
    /// so the rail is navigable but never renameable — the class rename
    /// owns the name — and its use ref is a companion of the class
    /// token's own ref at the same span.
    Classes,
}

impl HandlerOwner {
    /// What this owner's names denote, under the rail declarations `pack`
    /// carries. A class-owned handler's name is an event string; a rail
    /// nobody declared denotes strings — the common and conservative case.
    pub fn names_are(&self, pack: &PackFacts) -> RailNames {
        match self {
            HandlerOwner::Class(_) => RailNames::Strings,
            HandlerOwner::Rail(rail) => {
                if pack.class_named_rails.iter().any(|r| r == rail) {
                    RailNames::Classes
                } else {
                    RailNames::Strings
                }
            }
        }
    }

    /// The rail's names are class identities — see `RailNames::Classes`.
    pub fn names_are_classes(&self, pack: &PackFacts) -> bool {
        self.names_are(pack) == RailNames::Classes
    }
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

/// Every class this file's entities are reachable from: each namespace
/// bridge, and the owner of each class-owned Handler. Owning a handler IS
/// bridging it to the owner — one stored fact, read as both.
pub(crate) fn bridge_classes<'a>(
    symbols: &'a [Symbol],
    namespaces: &'a [PluginNamespace],
) -> impl Iterator<Item = &'a str> {
    let bridged = namespaces.iter().flat_map(|ns| ns.bridges.iter().map(|Bridge::Class(c)| c.as_str()));
    let owners = symbols.iter().filter_map(|s| match &s.detail {
        SymbolDetail::Handler { owner: HandlerOwner::Class(c), .. } => Some(c.as_str()),
        _ => None,
    });
    bridged.chain(owners)
}

/// The entities this file makes visible from `class`, each once: the
/// entities of namespaces bridged to it, and the Handlers it owns.
pub(crate) fn for_each_entity_bridged_to<'a>(
    symbols: &'a [Symbol],
    namespaces: &'a [PluginNamespace],
    class: &str,
    mut visit: impl FnMut(&'a Symbol) -> std::ops::ControlFlow<()>,
) -> std::ops::ControlFlow<()> {
    let mut seen: HashSet<SymbolId> = HashSet::new();
    for ns in namespaces {
        if !ns.bridges.iter().any(|Bridge::Class(c)| c == class) { continue; }
        for id in &ns.entities {
            let Some(sym) = symbols.get(id.0 as usize) else { continue };
            if seen.insert(*id) { visit(sym)?; }
        }
    }
    for sym in symbols {
        if matches!(&sym.detail, SymbolDetail::Handler { owner: HandlerOwner::Class(c), .. } if c == class)
            && seen.insert(sym.id)
        {
            visit(sym)?;
        }
    }
    std::ops::ControlFlow::Continue(())
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

/// The containment chain `[start, parent, …, file]` over a bare
/// `&[Scope]` slice: what a scope is INSIDE (its class, its callable, its
/// package), never what it can see. It is plain ids and lists no names;
/// a name lookup takes `lookup_chain`, which states the scoping rules.
/// Every caller outside this file is allowlisted with its reason
/// (`layering_tests::raw_scope_climbs_are_containment_only`). A scope has
/// one parent and no cycles, so this is a linked-list climb, not a graph
/// walk — the graph deliberately does not model it
/// (`docs/adr/graph-walking.md`).
pub fn scope_chain_of(scopes: &[Scope], start: ScopeId) -> Vec<ScopeId> {
    let mut chain = Vec::new();
    let mut current = Some(start);
    while let Some(id) = current {
        chain.push(id);
        current = scopes[id.0 as usize].parent;
    }
    chain
}

/// Where a name used in a scope is looked up, innermost first: the scope
/// chain minus the scopes whose own rule says a use written there cannot
/// see their names. THE value every name lookup walks — a variable read,
/// a bare class spelling (`UseMap`'s scope step), in-scope completion,
/// Perl's binders — so a language's scoping rule is stated once, as an
/// arm of `lookup_chain`, and inherited by all of them. It is the only
/// way to list the names a scope binds for lookup (`bindings`);
/// `scope_chain_of` is the containment climb and lists none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupChain(Vec<ScopeId>);

impl LookupChain {
    /// The scopes a name is looked up in, innermost first.
    pub fn scopes(&self) -> impl Iterator<Item = ScopeId> + '_ {
        self.0.iter().copied()
    }

    /// Whether a declaration made in `scope` is in view.
    pub fn contains(&self, scope: ScopeId) -> bool {
        self.0.contains(&scope)
    }

    /// Every symbol a scope on the chain binds, innermost scope first and
    /// in declaration order within one. A key — of a hash, or an
    /// instance's slot (`self.x = …`) — is reached through its container
    /// and binds no name, and a mention declares nothing.
    pub fn bindings(self, symbols: &SymbolTable) -> impl Iterator<Item = &Symbol> {
        self.0
            .into_iter()
            .flat_map(move |scope| symbols.declared_directly_in(scope).iter().map(move |sid| &symbols[sid.0 as usize]))
            .filter(|s| s.kind != SymKind::HashKeyDef && !s.is_mention())
    }
}

/// `from` and the scopes around it, out to and including the nearest
/// callable: where code written in `from` can stand without leaving the
/// callable it runs in. THE callable boundary. Ownership reads it (a
/// declaration joins the class whose own scope is on it — a method's
/// declarations are its locals) and so does `lookup_chain`'s direct arm.
pub fn callable_reach(scopes: &[Scope], from: ScopeId) -> impl Iterator<Item = ScopeId> + '_ {
    let mut next = Some(from);
    std::iter::from_fn(move || {
        let id = next?;
        let scope = &scopes[id.0 as usize];
        next = match scope.kind {
            ScopeKind::Sub { .. } | ScopeKind::Method { .. } => None,
            _ => scope.parent,
        };
        Some(id)
    })
}

/// The `LookupChain` from `from`. Every block stays on it: a Perl or C++
/// block binds its own locals.
///
/// Arms:
/// - `Scope::direct` (`@scope.direct`): in force only for code written
///   directly in the scope, so only the innermost direct scope within
///   `from`'s `callable_reach` is on the chain. From a method of a Python
///   class, or a class nested in it, the class body is not.
pub fn lookup_chain(scopes: &[Scope], from: ScopeId) -> LookupChain {
    let direct = callable_reach(scopes, from).find(|&id| scopes[id.0 as usize].direct);
    let mut chain = scope_chain_of(scopes, from);
    chain.retain(|&id| !scopes[id.0 as usize].direct || Some(id) == direct);
    LookupChain(chain)
}

/// What a Perl variable use names. THE binding rule: the builder's binder
/// mints from it and `FileAnalysis::variable_binding` answers from it, so a
/// by-name ask and a bound ref agree on identity by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariableUse {
    /// A declaration the lookup chain reaches.
    Declared(SymbolId),
    /// The package global the name reaches: a qualified spelling names its
    /// package, a special variable lives in `main`, any other undeclared
    /// name in the package the use is written in.
    Global { package: String, name: String },
    /// An `our` of another package is in lexical view: the name is
    /// declared, so it is no implicit global, but the use can't reach it.
    GatedOur,
}

/// Resolve the variable use `name` written at `at`. `decls_in(scope)`
/// lists the variables `scope` declares under `name` as `(id, start,
/// gate)`, the gate being the package an `our` is visible from.
pub fn variable_use<'a, I>(
    name: &str,
    at: Point,
    chain: LookupChain,
    use_package: Option<&str>,
    names: &NameSpellings,
    decls_in: impl Fn(ScopeId) -> I,
) -> VariableUse
where
    I: IntoIterator<Item = (SymbolId, Point, Option<&'a str>)>,
{
    // The qualifier names the package directly, so the lexical chain is
    // irrelevant and a `my` of the same name is never the answer.
    if let Some((package, name)) = crate::model::conventions::split_qualified_var(name, names) {
        return VariableUse::Global { package: package.to_string(), name };
    }
    let mut gated_our = false;
    for scope in chain.scopes() {
        let mut found = None;
        for (id, start, gate) in decls_in(scope) {
            if start > at {
                continue;
            }
            match gate {
                Some(pkg) if pkg != use_package.unwrap_or("main") => gated_our = true,
                _ => found = Some(id),
            }
        }
        if let Some(id) = found {
            return VariableUse::Declared(id);
        }
    }
    if gated_our {
        return VariableUse::GatedOur;
    }
    // Perl forces the special variables into `main` from every package.
    let package = if crate::model::conventions::is_special_variable(name) {
        "main"
    } else {
        use_package.unwrap_or("main")
    };
    VariableUse::Global { package: package.to_string(), name: name.to_string() }
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

#[derive(Debug, Clone, PartialEq, PartialOrd, Ord, Eq, Hash, Serialize, Deserialize)]
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
    /// The hops below `key` a nested write lands through, outward:
    /// `$v->{a}{b}{c} = …` is `key: a`, `path: [b, c]`.
    #[serde(default)]
    pub path: Vec<WriteKey>,
    pub scope: ScopeId,
    /// Key-node span — temporal anchor and per-var ordering.
    pub span: Span,
    /// RHS expression span — types the extended key's value.
    pub rhs_span: Option<Span>,
    /// A list-assignment slot (`($v->{k}, $x) = f()`): the value is element
    /// N of the RHS, not the RHS itself.
    #[serde(default)]
    pub rhs_slot: Option<i32>,
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

/// A method call binding: `$var = $invocant->method()`. The variable's
/// VALUE is its flow edge; this is the hash-key ownership relation — `$var`'s
/// keys are the ones `call`'s method returns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodCallBinding {
    pub variable: String,
    /// The bound `MethodCall` ref: its method name, and its invocant read
    /// (which precedes the write, so `$n = $n->parent` keys on the `$n`
    /// being replaced).
    pub call: crate::model::witnesses::RefIdx,
    pub scope: ScopeId,
    /// Where the write lands (`cst::assignment_write_point`) to the end
    /// of the assignment; the binding is anchored at its start.
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
/// The gate is a [`Question`] (`GatedEmission::question`), decided in
/// exactly two places, both through `FileAnalysis::gate_verdicts`: an open
/// document's enrichment, and the index's view seam
/// (`ModuleIndex::gated_view`), which hands every reader of a closed file
/// the copy with its firing emissions applied. The applied content sits
/// above the symbol/ref baselines, so a file whose ancestry resolves late
/// converges to one built with it known. The producer feeds index every
/// emission ungated, as candidates (`FileAnalysis::gated_candidates`):
/// the reverse index and Surface by name, the row store in its own `held`
/// presence table, never in the base `refs`/`syms` rows every view shows.
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

impl GatedEmission {
    /// What the emission waits on.
    pub fn question(&self) -> Question {
        Question::ClassIsaPrefix { package: self.package.clone(), prefixes: self.gate_prefixes.clone() }
    }
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

/// What a set of [`GatedEmission`]s adds to a file, minted file-native
/// (`FileAnalysis::gated_candidates`). Not stored: a view applies it, a
/// producer feed indexes it.
#[derive(Debug, Default)]
pub struct GatedMint {
    pub symbols: Vec<Symbol>,
    pub refs: Vec<Ref>,
    pub witnesses: Vec<crate::model::witnesses::Witness>,
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

    /// Whether the form dies on an undef receiver whatever the lexical
    /// hints say. Every receiver here is a variable or a part of one, and
    /// an element access through one autovivifies it (`my $x; $x->{a}`
    /// leaves `$x = {}`) unless `no autovivification` says otherwise; a
    /// call has nothing to call.
    pub fn dies_on_undef(&self) -> bool {
        match self {
            DerefForm::HashKey | DerefForm::ArrayIndex => false,
            DerefForm::Call | DerefForm::Method(_) => true,
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
    pub subject: Subject,
    /// Receiver type at the use point, with narrowing applied.
    pub receiver_ty: InferredType,
    pub form: DerefForm,
    /// An undef receiver dies here (D1/D2 fire) rather than vivifying.
    pub dies_on_undef: bool,
}

/// A builder-recorded deref receiver for the forms that carry no typed
/// ref of their own (`$x->[i]`, `$x->()`, `$x->{a}{k}`) — the analog of
/// the method-call and hash-deref refs `deref_receiver_sites` reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArrowDerefSite {
    /// The receiver as written: a variable's canonical name, or a part of
    /// one's spelling for the rendered message.
    pub receiver: String,
    /// The receiver's place, when it is a part of a variable.
    pub place: Option<Box<Place>>,
    pub span: Span,
    pub form: DerefForm,
    /// An undef receiver dies here rather than vivifying, decided where
    /// the hints in force are known.
    #[serde(default)]
    pub dies_on_undef: bool,
}

impl ArrowDerefSite {
    pub fn subject(&self) -> Subject {
        match &self.place {
            Some(place) => Subject::Place((**place).clone()),
            None => Subject::Variable(self.receiver.clone()),
        }
    }
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
    pub subject: Subject,
    /// The subject as written, for the rendered message.
    pub spelling: String,
    pub scope: ScopeId,
    pub predicate: GuardPredicate,
    /// Whether the predicate holds where the guard EXPRESSION is true (`!`
    /// flips it) — lets D3/D4 resolve always-true vs always-false.
    pub asserts_when_true: bool,
    /// Diagnostic range: the guard literal (the predicate plus the `!`s
    /// applied directly to it), not the whole condition around it.
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
    /// Whether the subject's prior type satisfies `predicate`. Under a
    /// negation (`!defined $x`) this is the opposite of the verdict, so the
    /// message states the subject's fact from this and the guard's from
    /// `verdict`.
    pub satisfied: bool,
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


impl FileAnalysis {
    /// Every class this file's entities are reachable from, deduped
    /// (see [`bridge_classes`]), the owners its gated emissions' handlers
    /// would have included as candidates.
    pub fn bridge_classes(&self) -> Vec<String> {
        let gated = self.gated_candidates();
        let mut out: Vec<String> = bridge_classes(self.symbols.as_slice(), &self.plugin.namespaces)
            .chain(bridge_classes(&gated.symbols, &[]))
            .map(str::to_string)
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The entities this file makes visible from `class`, each once
    /// (see [`for_each_entity_bridged_to`]).
    pub fn for_each_entity_bridged_to<'a>(
        &'a self,
        class: &str,
        visit: impl FnMut(&'a Symbol) -> std::ops::ControlFlow<()>,
    ) -> std::ops::ControlFlow<()> {
        for_each_entity_bridged_to(self.symbols.as_slice(), &self.plugin.namespaces, class, visit)
    }

    /// Does this file bridge a Sub/Method named `name` onto `class`?
    pub fn bridges_callable(&self, class: &str, name: &str) -> bool {
        self.for_each_entity_bridged_to(class, |s| {
            if matches!(s.kind, SymKind::Sub | SymKind::Method) && s.name == name {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        })
        .is_break()
    }
}
