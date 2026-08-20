//! FileAnalysis: single-pass scope graph for Perl source files.
//!
//! Built once per parse/reparse via `builder::build()`. Every LSP query
//! becomes a lookup against these tables instead of a tree walk.
//!
//! Designed to compose into a project index: `HashMap<PathBuf, FileAnalysis>`.

use std::collections::{HashMap, HashSet};
use serde::{Deserialize, Serialize};
use tree_sitter::Point;


mod cross_file;
pub use cross_file::*;
mod core_types;
pub use core_types::*;
mod ref_table;
pub use ref_table::*;
mod symbol_table;
pub use symbol_table::*;
mod pack_facts;
pub use pack_facts::*;
mod plugin_facts;
pub use plugin_facts::*;
mod types;
pub use types::*;
mod dispatch;
pub use dispatch::*;
mod imports;
pub use imports::*;
mod outline;
pub use outline::*;
mod lifecycle;
pub use lifecycle::*;
mod surface_feed;
pub use surface_feed::*;
mod ancestry;
pub use ancestry::*;
mod queries;
mod enrichment;
mod class_queries;
mod cursor_queries;
mod invocants;
mod hover;
mod sym_index;
mod completion;
pub use completion::*;

// ---- FileAnalysis ----

// `Clone` supports `Arc::make_mut` copy-on-write on `Document::analysis`:
// diagnostics enrichment mutates through the Arc, cloning only when a request
// handler is concurrently reading a snapshot (the deadlock-avoidance path).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileAnalysis {
    // Core tables
    pub scopes: Vec<Scope>,
    /// The symbol axis: every declaration, the name/scope indices over
    /// them, their eviction flag and their enrichment baseline. Read
    /// through `symbols()` and the delegating query methods.
    symbols: SymbolTable,
    /// The reference axis: every ref, the indices over them, their
    /// eviction flag and their enrichment baseline. Read through
    /// `refs()` and the delegating query methods.
    refs: RefTable,
    pub fold_ranges: Vec<FoldRange>,
    pub imports: Vec<Import>,
    pub call_bindings: Vec<CallBinding>,
    pub method_call_bindings: Vec<MethodCallBinding>,

    /// Flat list of `package`/`class` declarations and the byte ranges
    /// they govern. Replaces `Scope::package` walks for `package_at`.
    /// `#[serde(default)]` so older cache blobs deserialize as empty
    /// (`package_at` falls back to the legacy scope walk in that case).
    #[serde(default)]
    pub package_ranges: Vec<PackageRange>,

    /// Everything a pack driver recorded that Perl has no analog for —
    /// macros, the include graph, template parameters, move tracking.
    /// Empty for a Perl analysis.
    #[serde(default)]
    pub pack: PackFacts,

    /// Everything the plugin registry contributed — declared namespaces,
    /// loader facts, emitted diagnostics, deferred emissions, app-surface
    /// consumers.
    #[serde(default)]
    pub plugin: PluginFacts,

    /// Everything this file records about each of its packages, one entry
    /// per package name. Ancestry, the plugin trigger view, the framework
    /// fold and the role lanes all read the same entry, so a per-package
    /// join is a single lookup.
    #[serde(default)]
    pub packages: HashMap<String, PackageFacts>,

    /// Functions implicitly imported by OOP frameworks (e.g. `has`, `extends`, `with`).
    /// Used to suppress "not defined" diagnostics for these known framework keywords.
    pub framework_imports: HashSet<String>,

    /// Exported function names from `@EXPORT = ...` assignments.
    pub export: Vec<String>,
    /// Exported function names from `@EXPORT_OK = ...` assignments.
    pub export_ok: Vec<String>,
    /// `%EXPORT_TAGS` membership — tag name (no `:`/`-` prefix) → member subs.
    /// Feeds the consumer-side `:tag` selector expansion (`ExportSurface`).
    /// `:DEFAULT` is synthesized from `export` at query time, not stored.
    #[serde(default)]
    pub export_tags: HashMap<String, Vec<String>>,

    /// Re-export edges: other modules whose export surface this module folds
    /// into its own. Three statically-recognized idioms mint these (see
    /// `docs/adr/reexport-surface.md`): a static `@Other::EXPORT` element in an
    /// `@EXPORT` assignment, a loop-push over a statically-resolvable module
    /// list (`push @EXPORT, @{"${m}::EXPORT"}`), and a declarative `also => [...]`
    /// in `setup_import_methods`. `ExportSurface` walks these transitively at
    /// query time (cross-file, seen-set bounded) — the closure is NOT baked here
    /// (depth stays a query-time edge property, mirroring the inheritance
    /// edge-walk). Runtime `import`-delegation is deliberately unmodeled.
    #[serde(default)]
    pub reexport_modules: Vec<String>,

    /// Search-path roots this file prepends to `@INC` — the literal
    /// arguments of its `use lib` lines, AS WRITTEN. THE per-asker half of
    /// module visibility: `t/lib` for a test, `lib` for the app, a vendored
    /// `local/lib/perl5`. Stored unresolved because the analysis must stay
    /// location-independent to ride the cache; relative entries resolve
    /// against the workspace root and the file's own directory at query
    /// time, and an entry that names no directory (an interpolated
    /// `"$FindBin::Bin/../lib"`) drops out THERE. Nothing is filtered here:
    /// a text test for "looks interpolated" would be a guess, while
    /// "resolves to a real directory" is the question that actually matters.
    #[serde(default)]
    pub lib_roots: Vec<String>,

    /// Per-symbol provenance for return types. Populated for plugin
    /// `overrides()` and for reducer-driven folds over the witness bag.
    /// Missing entry == `TypeProvenance::Inferred`.
    /// Read-only debugging aid: features like hover/completion don't
    /// branch on it; it exists so `--dump-package` and a future
    /// inspector can answer "why does the LSP think this returns X?"
    /// without re-running the build.
    #[serde(default)]
    pub type_provenance: HashMap<SymbolId, TypeProvenance>,

    /// The witness bag. Canonical store for type facts:
    /// every Variable type, Symbol/PackageSymbol return type, branch
    /// arm Edge, hash-key observation. `inferred_type_via_bag` reads
    /// here. The builder pushes directly via `push_type_constraint`
    /// (Variable witnesses with TC shape) and per-attachment emit
    /// helpers; cache blobs round-trip the bag in full.
    #[serde(default)]
    pub witnesses: crate::model::witnesses::WitnessBag,

    /// Slice-2 residency flag: the resident pack-index copy of a workspace
    /// file has its witness bag evicted after the fold bakes its conclusions
    /// into pinned fields (`docs/adr/memory-slice-2-lru.md`). Set by
    /// `evict_witness_bag`, `#[serde(skip)]` so the on-disk blob (which keeps
    /// the FULL bag) always deserializes with the flag `false` — a rehydrated
    /// analysis is bag-present and indistinguishable from a never-evicted one.
    /// Consumers reading a foreign bag rehydrate through `CrossFileLookup::
    /// bag_present` when this is set; the empty bag is "evicted", not "no facts".
    #[serde(skip, default)]
    bag_evicted: bool,

    /// Witness-bag baseline — `enrich_imported_types_with_keys`
    /// truncates back to this length before re-deriving so repeat
    /// calls stay idempotent.
    #[serde(default)]
    base_witness_count: usize,

    /// Build-time dispatch candidates, each gated on its receiver's class.
    /// The builder records one per call matching a plugin `DispatchVerb`,
    /// ungated and per-file; the gate (`isa target_class`) is checked at
    /// QUERY time by `applicable_dispatches` (cross-file receiver isa), so
    /// candidates in non-open files surface the same as open ones. The
    /// `ReceiverGated` wrapper makes the inner handler payload unreadable
    /// without that check. See `docs/adr/receiver-gated-dispatch.md`.
    #[serde(default)]
    pub provisional_dispatches: Vec<ProvisionalDispatch>,

    /// Guard conditions recognized by the narrowing engine, recorded for the
    /// redundant/contradictory-guard diagnostics (D3/D4). Open-doc only in
    /// practice (we don't diagnose deps), but rides the cache blob like every
    /// other field.
    #[serde(default)]
    pub guard_sites: Vec<GuardSite>,

    /// Arrow-deref receivers for `$x->[i]` / `$x->()` — the forms with no
    /// typed ref. `deref_receiver_sites` merges these with the method-call /
    /// hash-deref refs so the deref diagnostics cover every arrow form.
    #[serde(default)]
    pub arrow_deref_sites: Vec<ArrowDerefSite>,

    /// Plugin `param_types()` role-contract TCs, each gated on the enclosing
    /// package's `isa` the rule's `in_role` class. The builder emits one per
    /// matching sub declaration UNCONDITIONALLY (no local-ancestry
    /// precondition — it's index-free per rule #1), so a controller whose
    /// `in_role` ancestor is reachable only CROSS-FILE still carries the
    /// candidate. The gate (`isa in_role`) is checked at QUERY time in
    /// `inferred_type_via_bag_ctx`, where the module index resolves the
    /// enclosing package's ancestry cross-file. The `ReceiverGated` wrapper
    /// keeps the typed TC unreadable without that check (rule #10). See
    /// `docs/adr/receiver-gated-dispatch.md` (Phase 2).
    #[serde(default)]
    pub gated_param_types: Vec<ReceiverGated<TypeConstraint>>,
    #[serde(default)]
    pub attr_projections: Vec<AttrProjection>,

    /// Scalars reassigned after declaration (`$v = …` with the
    /// variable itself as assignment target — element writes are NOT
    /// reassignment, they're modeled as shape mutations). A closed
    /// shape on a reassigned scalar isn't trustworthy: the other
    /// assignment may carry a different (unknown) value. The
    /// conditional-reassignment lattice disagreement is the modeled
    /// fix; this set is its trust-gate stand-in.
    #[serde(default)]
    pub reassigned_scalars: HashSet<String>,

    /// Hash-key writes, in walk order. Input to the mutation-extension
    /// pass — see [`KeyWrite`].
    #[serde(default)]
    pub key_writes: Vec<KeyWrite>,

    /// SymbolIds of `requires`-synthesized contract markers. A marker
    /// resolves like a Method (in-role `$self->name` dispatch, hover,
    /// goto-def land on the contract) but is NOT a provision — the
    /// composer-mismatch check excludes these by id, so a role that
    /// both requires AND defines a name (the default-implementation
    /// pattern) still counts the real def.
    #[serde(default)]
    pub contract_symbols: HashSet<SymbolId>,

    /// DBIC `__PACKAGE__->source_name('X')` override for this file's result
    /// class — the registered SOURCE moniker when it differs from the class
    /// basename. `None` = the moniker is the basename (the common case).
    /// Consulted by `resolve_dbic_source_moniker` so a `resultset('X')`
    /// whose `X` is a source_name (not a basename) still finds its class.
    /// Per-file: the DBIC one-result-class-per-file convention makes this
    /// unambiguous in practice.
    #[serde(default)]
    pub dbic_source_name: Option<String>,

    /// Method verbs whose first hashref arg is keyed by the receiver class's
    /// columns (DBIC `search`/`create`/…). The plugin-declared verb set, baked
    /// here so query-time owner resolution can mint the column owner cross-file.
    #[serde(default)]
    pub column_keyed_verbs: HashSet<String>,
    /// Number of dynamic method-dispatch sites (`$obj->$method(...)`) in
    /// this file — calls whose method name is a scalar, not a bareword.
    /// They produce no nameable `MethodCall` ref (unless const-folding
    /// resolves the name), so they are invisible to the static reference
    /// graph. The `--heatmap` dead-code pass reads this as a per-workspace
    /// soundness gate: when any file dispatches dynamically, a zero-fan-in
    /// method can't be proven dead (Perl may reach it through this
    /// invisible edge), so it is NOT flagged.
    #[serde(default)]
    pub dynamic_dispatch_sites: u32,

    /// Callee-side markers: params whose type arrives from loader
    /// config (the `from_loader_config` ParamType flavor).
    #[serde(default)]
    pub loader_config_params: Vec<LoaderConfigParam>,
    /// Value-flow edges: every assignment/binding's `source → target` +
    /// extraction. The general provenance tier above the type witness bag.
    #[serde(default)]
    pub flow_edges: Vec<FlowEdge>,

    /// This analysis was produced from degraded inputs — a parse/extract
    /// failure, or a skipped cross-file macro gather (the on-open
    /// cached-only path). Degraded analyses may be SERVED (best effort
    /// beats nothing) but must never be PERSISTED: a cache row is
    /// validated only by its source file's stamp, so a frozen degraded
    /// blob would be re-served every future session. serde(skip): warm
    /// blobs are non-degraded by construction, no on-disk representation.
    #[serde(skip, default)]
    pub degraded: bool,

    /// The id of the language driver that built this analysis — the origin
    /// identity `resolve()` derives pack routing from at CandidateSet
    /// construction (`is_pack_language`), so no verb handler carries the
    /// routing decision. Stamped by `PackDriver::analyze_with_path`;
    /// `"perl"` for the native builder and for cache blobs predating the
    /// field (serde default).
    #[serde(default = "default_language")]
    pub language: String,

    // Indices (built in post-pass — skipped by serde; call rebuild_all_indices() after deserialize)
    #[serde(skip, default)]
    scope_starts: Vec<(Point, ScopeId)>, // sorted by start point
    /// Union of `export` + `export_ok` for O(1) membership tests.
    /// Rebuilt by `build_indices` (called from `new` and `after_deserialize`),
    /// so it is valid for freshly-built and SQLite-cached modules alike.
    #[serde(skip, default)]
    export_lookup: HashSet<String>,
}

/// serde default for `FileAnalysis::language` — the native builder's id,
/// also what pre-field cache blobs deserialize as.
fn default_language() -> String {
    "perl".to_string()
}

/// What one package in this file declares about itself. Keyed by package
/// name in `FileAnalysis::packages`, so every per-package consumer
/// (ancestry, the plugin trigger view, the framework fold, the role
/// diagnostics, `Surface::project`) resolves its facts in one lookup.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PackageFacts {
    /// Parent classes, from `use parent`/`use base`, `@ISA`, `class :isa`,
    /// `with`, and `__PACKAGE__->load_components`.
    #[serde(default)]
    pub parents: Vec<String>,

    /// Modules `use`-d inside the package. Powers trigger-matching for
    /// plugin query hooks (emit-path builder state isn't visible at
    /// cursor time).
    #[serde(default)]
    pub uses: Vec<String>,

    /// Detected framework mode (for the type resolver), set when
    /// `use Moo` / `use Mojo::Base` / `use Moose` etc. is observed.
    #[serde(default)]
    pub framework: Option<crate::model::witnesses::FrameworkFact>,

    /// A role's `requires` list: the method contracts a composing class
    /// must fulfill. The synthesized Method symbols carry the in-role
    /// resolution; this record feeds the composer-mismatch diagnostic
    /// (docs/adr/role-contracts.md).
    #[serde(default)]
    pub requires: Vec<String>,

    /// This package IS a role — the baked verdict behind
    /// `is_role_package`, fed by the builder's open role-maker set.
    #[serde(default)]
    pub is_role: bool,

    /// `parents` is INCOMPLETE — at least one `with`/`extends` argument
    /// didn't fold to a literal name (runtime-generated roles:
    /// `with ReportProxy(type => ...)`). `class_has_unresolved_ancestor`
    /// treats this as an unresolved edge so inheritance-dependent
    /// diagnostics stay honest-silent.
    #[serde(default)]
    pub dynamic_parents: bool,
}

impl PackageFacts {
    /// Fold a walk-time collector's per-lane maps into the stored table.
    /// The one seam between accumulating each lane independently (the
    /// builder, the pack skeleton) and the one-entry-per-package shape
    /// every consumer reads.
    pub fn fold(
        parents: HashMap<String, Vec<String>>,
        uses: HashMap<String, Vec<String>>,
        framework: HashMap<String, crate::model::witnesses::FrameworkFact>,
        requires: HashMap<String, Vec<String>>,
        roles: HashSet<String>,
        dynamic_parents: HashSet<String>,
    ) -> HashMap<String, PackageFacts> {
        let mut out: HashMap<String, PackageFacts> = HashMap::new();
        for (pkg, v) in parents {
            out.entry(pkg).or_default().parents = v;
        }
        for (pkg, v) in uses {
            out.entry(pkg).or_default().uses = v;
        }
        for (pkg, v) in framework {
            out.entry(pkg).or_default().framework = Some(v);
        }
        for (pkg, v) in requires {
            out.entry(pkg).or_default().requires = v;
        }
        for pkg in roles {
            out.entry(pkg).or_default().is_role = true;
        }
        for pkg in dynamic_parents {
            out.entry(pkg).or_default().dynamic_parents = true;
        }
        out
    }
}

impl FileAnalysis {
    /// `pkg`'s locally declared parents (empty when it declares none) —
    /// the LOCAL half of the ancestry seam; `parents_of` adds the
    /// cross-file and app-surface edges.
    pub fn declared_parents(&self, pkg: &str) -> &[String] {
        self.packages.declared_parents(pkg)
    }

    /// Every package this file records, with its locally declared parents
    /// — the inheritance-edge enumeration for reverse-index builds and
    /// dependency sweeps.
    pub fn package_parent_edges(&self) -> impl Iterator<Item = (&String, &[String])> {
        self.packages.iter().map(|(pkg, f)| (pkg, f.parents.as_slice()))
    }

    /// Modules `use`-d inside `pkg`.
    pub fn package_uses(&self, pkg: &str) -> &[String] {
        self.packages.get(pkg).map_or(&[], |f| f.uses.as_slice())
    }

    /// `pkg`'s detected framework mode, if any.
    pub fn package_framework(&self, pkg: &str) -> Option<crate::model::witnesses::FrameworkFact> {
        self.packages.get(pkg).and_then(|f| f.framework)
    }

    /// The method contracts `pkg` requires of its composers (empty unless
    /// `pkg` is a role that declared `requires`).
    pub fn role_requires(&self, pkg: &str) -> &[String] {
        self.packages.get(pkg).map_or(&[], |f| f.requires.as_slice())
    }

    /// Is `pkg`'s recorded parent list incomplete (a `with`/`extends`
    /// argument that didn't fold to a literal name)?
    pub fn has_dynamic_parents(&self, pkg: &str) -> bool {
        self.packages.get(pkg).is_some_and(|f| f.dynamic_parents)
    }
}

/// Read a package's declared parents from any store that keys them by
/// package name — the builder's walk-time map (facts still accumulating)
/// or a finished `FileAnalysis`'s table. `parents_of` and the isa walkers
/// take this so the union/injection policy has one body regardless of
/// which side holds the facts.
pub trait LocalParents {
    fn declared_parents(&self, package: &str) -> &[String];
}

impl LocalParents for HashMap<String, Vec<String>> {
    fn declared_parents(&self, package: &str) -> &[String] {
        self.get(package).map_or(&[], |v| v.as_slice())
    }
}

impl LocalParents for HashMap<String, PackageFacts> {
    fn declared_parents(&self, package: &str) -> &[String] {
        self.get(package).map_or(&[], |f| f.parents.as_slice())
    }
}

/// The framework twin of [`LocalParents`]: read a package's detected
/// framework from either side's store.
pub trait PackageFrameworks {
    fn framework_of(&self, package: &str) -> Option<crate::model::witnesses::FrameworkFact>;
}

impl PackageFrameworks for HashMap<String, crate::model::witnesses::FrameworkFact> {
    fn framework_of(&self, package: &str) -> Option<crate::model::witnesses::FrameworkFact> {
        self.get(package).copied()
    }
}

impl PackageFrameworks for HashMap<String, PackageFacts> {
    fn framework_of(&self, package: &str) -> Option<crate::model::witnesses::FrameworkFact> {
        self.get(package).and_then(|f| f.framework)
    }
}

/// Everything the builder hands over to construct a `FileAnalysis`.
/// Field-named so a swapped pair of same-typed tables can't compile
/// silently the way positional args could, and hand-crafted test FAs
/// spell only the tables they use (`..Default::default()`).
///
/// Lanes that own their fields on the analysis (`PackFacts`,
/// `PluginFacts`, the per-package `PackageFacts` table) cross this
/// boundary as themselves — `new` moves them in whole. The symbol and ref
/// axes stay flat vecs: the builder appends to them as it walks, and
/// `SymbolTable`/`RefTable` adopt the finished vec.
#[derive(Default)]
pub struct FileAnalysisParts {
    pub scopes: Vec<Scope>,
    pub symbols: Vec<Symbol>,
    pub refs: Vec<Ref>,
    pub fold_ranges: Vec<FoldRange>,
    pub imports: Vec<Import>,
    pub call_bindings: Vec<CallBinding>,
    pub packages: HashMap<String, PackageFacts>,
    pub pack: PackFacts,
    pub plugin: PluginFacts,
    pub method_call_bindings: Vec<MethodCallBinding>,
    pub framework_imports: HashSet<String>,
    pub export: Vec<String>,
    pub export_ok: Vec<String>,
    pub export_tags: HashMap<String, Vec<String>>,
    pub reexport_modules: Vec<String>,
    pub lib_roots: Vec<String>,
    pub type_provenance: HashMap<SymbolId, TypeProvenance>,
    pub package_ranges: Vec<PackageRange>,
    pub witnesses: crate::model::witnesses::WitnessBag,
    pub provisional_dispatches: Vec<ProvisionalDispatch>,
    pub guard_sites: Vec<GuardSite>,
    pub arrow_deref_sites: Vec<ArrowDerefSite>,
    pub gated_param_types: Vec<ReceiverGated<TypeConstraint>>,
    pub attr_projections: Vec<AttrProjection>,
    pub reassigned_scalars: HashSet<String>,
    pub key_writes: Vec<KeyWrite>,
    pub contract_symbols: HashSet<SymbolId>,
    pub dbic_source_name: Option<String>,
    pub column_keyed_verbs: HashSet<String>,
    pub dynamic_dispatch_sites: u32,
    pub loader_config_params: Vec<LoaderConfigParam>,
    pub flow_edges: Vec<FlowEdge>,
}

/// One domain-typing use-site: the `slot` field was compared/assigned
/// against `value` at `slot_span`. `value`'s enum resolves cross-file at
/// query time — an enumerator carries its `enum` (the enum-container
/// work). Language-generic evidence: a Perl field source mints
/// the same rows off its own accesses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DomainSite {
    pub slot: String,
    pub value: String,
    pub slot_span: Span,
}

/// A slot's resolved domain: the enum it is *used as*. The domain is a
/// defeasible refinement for human surfaces; the storage type underneath is
/// what flows (the hover site composes its own storage-leaf display).
/// `confidence` is the dominant enum's share of the coherence vote.
#[derive(Debug, Clone, PartialEq)]
pub struct NominalDomain {
    pub domain: String,
    pub confidence: f32,
}

/// "This file loads plugin `name`, passing the config value at
/// `config_span`" — the caller half of loader-config param typing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginLoadFact {
    pub name: String,
    pub config_span: Option<Span>,
}

/// "This param's type arrives from whoever loads me" — the callee
/// half. `in_role` re-gates at enrichment (the package must still
/// isa the declaring role/class).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoaderConfigParam {
    pub variable: String,
    pub scope: ScopeId,
    pub in_role: String,
}

/// One projection of a field/attr decl — the entity that encodes group
/// membership directly. A decl is ONE name spelled several ways; each
/// spelling is minted AT BUILD by the synthesis that knows the
/// semantics: Moo/Moose/Mojo::Base `has` mints `CtorKey` + `Accessor` +
/// `InternalKey` (hash-backed repr — the repr gate IS whether
/// `InternalKey` was minted), Corinna fields mint `CtorKey`/`Accessor`
/// only (fields are not hash entries), plugins enroll name-mapped
/// accessors via `EmitAction::Method.attr`. Group features (rename /
/// references union) walk the stored projections — no scattered
/// query-time re-derivation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AttrProjection {
    pub class: String,
    pub attr: String,
    pub kind: AttrProjectionKind,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum AttrProjectionKind {
    /// `Class->new(attr => …)` constructor key.
    CtorKey,
    /// `$self->{attr}` internal hash slot. Minted ONLY by hash-backed
    /// synthesis; membership matching is STRICT `HashKeyOwner::Class`
    /// equality (never `found_by` — that broadening would leak other
    /// subs' same-named arg keys into the group).
    InternalKey,
    /// An accessor method; `affix = (prefix, suffix)` when the name
    /// embeds the attr (rename re-derives it), `None` = references-only.
    Accessor {
        method: String,
        affix: Option<(String, String)>,
    },
}

impl AttrProjection {
    /// Derive the accessor affix by locating the attr inside the method
    /// name (`_has_token` = `_has` + `_token`). Done once, at minting.
    pub fn accessor(class: String, attr: String, method: String) -> Self {
        let affix = method.find(attr.as_str()).map(|i| {
            (
                method[..i].to_string(),
                method[i + attr.len()..].to_string(),
            )
        });
        AttrProjection {
            class,
            attr,
            kind: AttrProjectionKind::Accessor { method, affix },
        }
    }
}

/// Cross-file-facing facts of a field group — see
/// `FileAnalysis::field_projections_at`.
pub struct FieldProjections {
    pub class: String,
    pub bare: String,
    pub has_param: bool,
    pub has_reader: bool,
    /// An `InternalKey` projection was minted (hash-backed repr) —
    /// `$obj->{attr}` slot pokes join the group, cross-file included.
    pub has_internal: bool,
    /// A `Class(class)`-owned `HashKeyDef` backs this attr (DBIC columns,
    /// `Class::Accessor`): the key is reached `found_by`-style, so every
    /// access named `attr` — direct deref `$row->{attr}`, search/find/update
    /// arg keys (`Sub{class, verb}`-owned) — joins the group. Distinct from
    /// `has_internal` (STRICT `Class` match, Moo/bless internal slots).
    pub has_class_key: bool,
    /// Backed by a Corinna `field $x` lexical (vs a `has`/column pair). Field
    /// storage is per-class PRIVATE — not inherited — so the inheritance bridge
    /// must NOT widen a field-backed group to an ancestor's same-named field.
    pub field_backed: bool,
    /// Origin-file variable spellings (decl + body uses), bare-adjusted.
    pub variable_spans: Vec<Span>,
    /// The DECLARATION subset of `variable_spans` — the `has`/column token,
    /// or the `field $x` decl. Goto-def is the projection that wants just
    /// this; references/rename take every spelling.
    pub decl_spans: Vec<Span>,
    /// Plugin-declared, name-mapped members (`predicate => has_size`).
    /// `affix` = `(prefix, suffix)` when the method name embeds the attr —
    /// rename re-derives the name; `None` = references-only member.
    pub mapped: Vec<MappedMember>,
}

#[derive(Debug, Clone)]
pub struct MappedMember {
    pub method: String,
    pub affix: Option<(String, String)>,
}

/// One field/attr-group entity: the facts the projection union needs.
/// Corinna fields carry the field symbol (variable spellings live on it);
/// Moo `has` attrs have no variable side — their decl token is the
/// synthesized pair's selection span.
struct FieldGroup {
    field_sym: Option<SymbolId>,
    decl_span: Option<Span>,
    class: String,
    bare: String,
    has_param: bool,
    has_reader: bool,
}

/// Occurs-check + memo node for the `expr_type_at_span` ⇄
/// `method_call_return_type_via_bag` mutual recursion. The pair hops
/// across FileAnalysis instances (a chained cross-file return-type query
/// re-enters the receiver's file) and mints a fresh `ReducerRegistry`
/// query per hop, so the registry's own bag-keyed visited set never sees
/// the repeat — only this outer guard can. Keyed by (FileAnalysis
/// identity, span/ref) just as the registry keys on the bag pointer.
#[derive(PartialEq, Eq, Hash, Clone, Copy)]
enum ResolveNode {
    /// `expr_type_at_span(span)` on a given FileAnalysis instance.
    Expr(usize, Span),
    /// `method_call_return_type_via_bag(ref_idx)` on a given instance.
    MethodCall(usize, usize),
}

thread_local! {
    /// Per-thread active-resolution stack (the occurs check). Rayon build
    /// workers each own one; never a shared field (these are `&self`
    /// methods).
    static RESOLVE_STACK: std::cell::RefCell<Vec<ResolveNode>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Per-thread memo, alive only for the duration of one outermost
    /// resolution (cleared when the stack drains). Collapses the
    /// exponential re-computation of a node reached through many parents
    /// in a dense cross-file return-type graph — the cycle guard bounds
    /// *depth*, this bounds *work*. Within one outermost resolution the
    /// FileAnalyses are immutable, so a node's answer is stable and safe
    /// to reuse; on-path (cycle-blocked) answers are never memoized.
    static RESOLVE_MEMO: std::cell::RefCell<HashMap<ResolveNode, Option<InferredType>>> =
        std::cell::RefCell::new(HashMap::new());
}

/// Depth backstop for a genuinely (non-cyclic) deep chain — the occurs
/// check is the primary termination guarantee; this only guards the
/// stack against a pathological linear descent.
const RESOLVE_DEPTH_CAP: usize = 256;

/// Entry cap on the stack-scoped memo. A dense workspace's reachable
/// return-type graph is bounded by its ref count; this only fires on a
/// pathological blow-up, degrading to recompute (still terminating via
/// the occurs check) rather than growing unbounded.
const RESOLVE_MEMO_CAP: usize = 50_000;

/// RAII stack frame. `enter` returns `None` when `node` is already on
/// the active stack (a return-type cycle → answer `None` instead of
/// re-entering) or the depth cap is hit; otherwise pushes and pops on
/// unwind. When the stack drains back to empty the memo is cleared, so
/// it never outlives the outermost resolution.
struct ResolveGuard;

impl ResolveGuard {
    fn enter(node: ResolveNode) -> Option<Self> {
        RESOLVE_STACK.with(|s| {
            let mut st = s.borrow_mut();
            if st.len() >= RESOLVE_DEPTH_CAP || st.contains(&node) {
                None
            } else {
                st.push(node);
                Some(ResolveGuard)
            }
        })
    }

    /// Memo lookup for `node` — hits only survive within one outermost
    /// resolution (the guard clears the map on drain).
    fn memo_get(node: &ResolveNode) -> Option<Option<InferredType>> {
        RESOLVE_MEMO.with(|m| m.borrow().get(node).cloned())
    }

    /// Record `node`'s resolved answer for reuse by later parents in this
    /// same outermost resolution.
    fn memo_put(node: ResolveNode, ty: Option<InferredType>) {
        RESOLVE_MEMO.with(|m| {
            let mut mm = m.borrow_mut();
            if mm.len() < RESOLVE_MEMO_CAP {
                mm.insert(node, ty);
            }
        });
    }
}

impl Drop for ResolveGuard {
    fn drop(&mut self) {
        RESOLVE_STACK.with(|s| {
            let mut st = s.borrow_mut();
            st.pop();
            if st.is_empty() {
                RESOLVE_MEMO.with(|m| m.borrow_mut().clear());
            }
        });
    }
}


#[cfg(test)]
#[path = "file_analysis_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "call_ref_index_tests.rs"]
mod call_ref_index_tests;

#[cfg(test)]
#[path = "parametric_resultset_tests.rs"]
mod parametric_resultset_tests;

#[cfg(test)]
#[path = "return_expr_tests.rs"]
mod return_expr_tests;
