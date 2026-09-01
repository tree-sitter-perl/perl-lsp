//! Single-walk tree-sitter visitor that builds a FileAnalysis.
//!
//! One depth-first walk populates scopes, symbols, refs, type constraints,
//! and fold ranges. Post-passes resolve hash key owners and variable refs.

use std::sync::Arc;

use tree_sitter::{Node, Point, Tree};

use crate::cst::{fq_tail_span, node_to_span};
use crate::model::file_analysis::*;

/// A ready-to-parse tree-sitter Parser for the Perl grammar — the one
/// constructor every parse site (resolver, document, CLI, the s///e
/// snippet re-parse) shares. Lives in the builder layer: the resolver
/// calling down is fine, the builder reaching up was not.
pub fn create_parser() -> tree_sitter::Parser {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .expect("failed to set Perl language");
    parser
}
use crate::build::plugin::{self, PluginRegistry};

pub use crate::build::plugin::default_plugin_registry;

/// Flow-sensitive narrowing: guard recognition + span-scoped emission.
/// A child module so its `impl Builder` methods keep private-field access
/// while the code lives in its own file (rule #1: still driven by `build()`).
mod narrowing;

/// Query-declared plugin capture (`docs/adr/plugin-system.md`):
/// runs plugin-declared tree-sitter patterns post-walk and dispatches
/// `on_match`. Child module for the same private-field reason as
/// `narrowing` — still driven by `build()` (rule #1). `pub(crate)` so
/// `--plugin-check` can run `verify_pattern_expects`.
pub(crate) mod pattern_dispatch;

/// Time one build-pipeline phase into a running TOTAL, not a printed line.
///
/// These regions run once per FILE, and `build()` has ~19 of them: printing
/// each one emits ~4,800 lines/s at corpus scale — 3.2M lines on a 138k walk,
/// which turns the run being measured into a different, much slower run. A
/// per-call line is also the wrong shape for a per-file region; what a hot
/// phase needs is the sum and the call count, so a share and an average are
/// derivable. A single-file debug run gets the same information with n=1.
///
/// Reported by `PERL_LSP_GHOST_STATS` under "accumulated time".
/// `PERL_LSP_PHASE_TIMING` keeps its meaning for regions entered ONCE per run
/// (`cli::*`, `index.*`), where a line per entry is exactly right.
macro_rules! bphase {
    ($label:literal, $body:expr) => {
        $crate::util::ghost_stats::timed(concat!("build::", $label), || $body)
    };
}


pub(crate) mod pipeline;
pub use pipeline::*;
mod arity;
use arity::*;
mod infra;
mod chain;
mod plugin_emit;
pub(crate) mod walk;
use walk::WalkTask;
mod visit_decl;
mod visit_use;
mod emit;
mod visit_calls;
mod frameworks;
mod visit_method;
mod visit_bless;
mod extract;
mod fold;
mod docs;

/// Single CST walk that powers the post-walk `ChainTypingReducer`.
/// Indexes the node sets the reducer needs:
///
/// - `assignment_nodes` — every `assignment_expression`, used to type
///   `my $X = <rhs>` (and bare `$X = …`) via `resolve_invocant_class_tree`.
/// - `return_nodes` — every `return_expression`, indexed by span so
///   the return-arm refresh can match it back to a `ReturnInfo`.
/// - `invocant_nodes` — every `method_call_expression`'s invocant,
///   indexed by span so the post-fold invocant-class refresh can
///   find the right node for a `MethodCall` ref's `invocant_span`.
/// - `method_call_args` — every `method_call_expression`'s args
///   node, indexed by the call-expression span. Lets the post-walk
///   keys-as-HashKeyAccess emission look up the args from a
///   MethodCall ref instead of stashing them on the ref itself or
///   running emit at walk time (where invocant_class is still
///   getting refined).
///
/// Tree-sitter-perl `arguments` field shapes encountered in the
/// wild (notes for parser-side adjustments — `args` is the value
/// of `child_by_field_name("arguments")`):
///
/// | Call shape | `args.kind()`                 | Notes |
/// | --- | --- | --- |
/// | `f()`                | (no `arguments` field)        | empty arglist; field absent |
/// | `f($x)`              | `scalar`                      | single non-literal arg, unwrapped |
/// | `f('x')`             | `string_literal`              | single string, unwrapped |
/// | `f('x', 'y')`        | `list_expression`             | flat positional list |
/// | `f(k => v)`          | `list_expression`             | fat-comma is flat in this list |
/// | `Foo->new(k => v)`   | `parenthesized_expression`    | constructor wraps in parens |
/// | `f({k => v})`        | `anonymous_hash_expression`   | hashref-arg shape — DBIC's `search`/`find`. The hash's children are a `list_expression` of the k=>v pairs |
/// | `qw(a b c)`          | `quoted_word_list`            | DBIC `add_columns(qw(...))` shape |
///
/// Recursive-into-wrapper rule: emitters typically walk one level
/// past `parenthesized_expression` / `anonymous_hash_expression`
/// to the enclosed `list_expression`. The k=>v iteration logic
/// expects a flat positional list; nested wrappers want unwrapping
/// at the call site, not in the arg-walker.
///
/// Built once per `build_with_plugins_inner` call and consumed by both
/// `ChainPassMode::PreFold` and `ChainPassMode::PostFold` invocations
/// of the reducer.
struct ChainTypingIndex<'a> {
    assignment_nodes: Vec<Node<'a>>,
    return_nodes: std::collections::HashMap<(Point, Point), Node<'a>>,
    invocant_nodes: std::collections::HashMap<(Point, Point), Node<'a>>,
    method_call_args: std::collections::HashMap<(Point, Point), Node<'a>>,
    /// Every `method_call_expression` node. `emit_route_brand_witnesses`
    /// reads it post-fold to attach resolved `BrandedRoute` witnesses to
    /// each call's `Expression(refidx)`.
    method_call_nodes: Vec<Node<'a>>,
    /// `hash_element_expression` nodes whose container is itself a
    /// method-call result (`$obj->get_config->{host}`). The container
    /// type — and thus the key's owner class — is only knowable after
    /// the fold resolves the method's return type, so the chained-key
    /// HashKeyAccess emission (`emit_chained_hash_key_refs`) waits for
    /// it. Plain `$var->{key}` keys are emitted during the walk
    /// (`visit_hash_element`) and owner-resolved at phase 3.
    chained_hash_elements: Vec<Node<'a>>,
}

/// Which chain-typing tasks the reducer should apply on this call.
///
/// `PreFold` runs between the two `resolve_return_types` calls —
/// assignments and return arms feed the second fold (assignments via
/// the bag's Variable query for `return $var`; return arms directly
/// through `return_infos`). Invocants are query-time outputs (`Ref.invocant_class`)
/// and don't influence the fold, so they wait until after every sub
/// return type is resolved.
///
/// `PostFold` runs once after the second `resolve_return_types` and
/// types method-call invocants (e.g. the `get_foo` in `get_foo()->bar()`)
/// using the now-final symbol table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ChainPassMode {
    PreFold,
    PostFold,
}


/// Which arity branch a `return_expression` represents. Computed
/// from the shape of the return's parent in the CST.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArityBranch {
    /// `return X unless @_;` / `return X if !@_;` — fires when the
    /// caller passed zero additional args.
    Zero,
    /// `return X if @_ == N;` / `if scalar(@_) == N;` / explicit
    /// `if (@_ == N) { return X }`. Exact-N match.
    Exact(u32),
    /// `return X unless @_ > N;` — fires only at arity ≤ N. Arrives from a
    /// relational arity guard (`@_ > N` / `@_ < N` / …), and — the load-
    /// bearing case — from a COMPOUND guard `unless @_ > N || <non-arity>`
    /// where `@_ ≤ N` is the sound NECESSARY condition (the non-arity
    /// disjunct only narrows it further). An over-approximation of the arm's
    /// true firing domain, but tighter than the fluent `Default`/`Any` arm,
    /// so an honest low-arity type beats the wrong fluent one.
    AtMost(u32),
    /// `return X if @_ > N;` — fires only at arity ≥ N+1. The relational
    /// mirror of `AtMost`; a fluent writer guarded by a magnitude test.
    AtLeast(u32),
    /// Fall-through `return X;` with no condition wrapper — fires
    /// when no earlier arity-gated branch matched.
    Default,
}


/// Structural index of one return-expression in a sub body. Type
/// information lives in the bag: the body expression carries its own
/// `Expr(body_span)` witnesses (literal types, Edges to Variable /
/// Symbol / Expression / nested Expr), and `Symbol(sub_id)` collects
/// per-arm `branch_arm`-source `Edge(Expr(...))` witnesses. What's
/// left in this struct is what consumers still need at a glance:
/// which sub the return belongs to, where it is, the arity bucket it
/// gates on (for `emit_arity_return_witnesses`), and the body span
/// (the `Expr(span)` key both `emit_arity_return_witnesses` and
/// `seed_return_types_from_bag` query inline via `bag_query_expr_span`).
struct ReturnInfo {
    /// The scope (Sub/Method) this return belongs to.
    scope: ScopeId,
    /// Arity-dispatch classification (`unless @_`, `if @_ == N`, …) —
    /// `emit_arity_return_witnesses` reads this to build the
    /// per-scope `ReturnExpr::UnionOnArgs` arm guards. `None` for
    /// returns that aren't arity-gated.
    arity_branch: Option<ArityBranch>,
    /// Span of the inner body expression (the `EXPR` in `return EXPR`).
    /// Doubles as the `WitnessAttachment::Expr(span)` key the per-arm
    /// fold reads. `None` for bare `return;` — those have no value.
    body_span: Option<Span>,
}


struct DeferredVarType {
    variable: String,
    at: Span,
    inferred_type: InferredType,
    /// Emitting plugin id — the flushed TC rides a `Plugin`-priority
    /// witness so the plugin's explicit knowledge (`$c` is a controller)
    /// dominates builder heuristics (`my $c = shift` typing `$c` as the
    /// enclosing class).
    plugin_id: String,
}

/// Plugin-emitted `NamedSubParamType` request, resolved at end-of-build.
/// Keyed by sub name + positional index rather than a span: a `\&name`
/// registration arg carries no callback body span, and the named sub may
/// be a forward reference not yet walked when the plugin fires.
struct DeferredNamedSubParamType {
    sub_name: String,
    /// `None` for a bare `\&name` (current package), `Some(pkg)` for a
    /// qualified `\&Foo::bar`. The resolver matches the sub's enclosing
    /// package against this.
    package: Option<String>,
    param_index: usize,
    inferred_type: InferredType,
    /// Emitting plugin id — see `DeferredVarType::plugin_id`.
    plugin_id: String,
}

struct Builder<'a> {
    source: &'a [u8],

    scopes: Vec<Scope>,
    symbols: Vec<Symbol>,
    refs: Vec<Ref>,
    /// Plugin-emitted `VarType` constraints, resolved to scopes only
    /// after the whole CST has been walked (plugin dispatch runs
    /// before we recurse into call args, so at emit-time the target
    /// scope usually doesn't exist yet).
    deferred_var_types: Vec<DeferredVarType>,
    /// Plugin-emitted `NamedSubParamType` requests (`->helper(_ => \&sub)`),
    /// resolved by sub name + index at end-of-build alongside
    /// `deferred_var_types`.
    deferred_named_sub_param_types: Vec<DeferredNamedSubParamType>,
    fold_ranges: Vec<FoldRange>,
    imports: Vec<Import>,
    /// Return values collected during the walk (explicit `return` + implicit last expr).
    return_infos: Vec<ReturnInfo>,
    /// Pending `push @arr, X, Y` contributions queued at walk time
    /// and re-resolved in the worklist (phase 6) once method-call
    /// return types are filled. Stored as
    /// `(scope, arr_name, contribution_spans)` triples — re-emitted
    /// each iteration as `Variable{arr_name, scope} +
    /// InferredType(Sequence(...))` with the latest known per-arg
    /// types. Clear-and-emit on `source_tag = "array_push"`. Tuple
    /// shape only; cross-scope and conditional branches not handled.
    pending_array_pushes: Vec<(ScopeId, String, Vec<Span>)>,
    /// For each Sub/Method scope, the body span of the last
    /// top-level expression statement. Used as the implicit-return
    /// query key — `seed_return_types_from_bag` reads `Expr(span)` via
    /// `bag_query_expr_span` for scopes without an explicit `return`.
    /// Types ride the bag; this map only carries the structural
    /// pointer to the source span.
    last_expr_span: std::collections::HashMap<ScopeId, Span>,
    /// For each `$obj->{k} = <rhs>` hash-key WRITE, maps the key node's
    /// span (the span the matching `HashKeyAccess` Write ref carries) to
    /// the RHS expression's span. `populate_witness_bag`'s mutation loop
    /// reads it to seed `SlotType{class, key} → Edge(Expr(rhs_span))`
    /// alongside the untyped mutation Fact. Hash-element LHS only; plain
    /// variable assignments never enter it.
    slot_write_rhs_span: std::collections::HashMap<Span, Span>,
    /// Assignments where RHS is a function call — resolved in return-type post-pass.
    call_bindings: Vec<CallBinding>,
    /// Assignments where RHS is a method call — resolved in FileAnalysis post-pass.
    method_call_bindings: Vec<MethodCallBinding>,
    /// Raw POD text blocks collected during the walk (for tail-POD post-pass).
    pod_texts: Vec<String>,
    /// Parent classes for each package (from use parent/base, @ISA, class :isa).
    package_parents: std::collections::HashMap<String, Vec<String>>,
    /// Modules the current package has `use`d, in source order. Used by
    /// `PluginRegistry::applicable` for `Trigger::UsesModule` matching.
    package_uses: std::collections::HashMap<String, Vec<String>>,
    /// `(span, current_package, module, raw_args, imports)` tuples already
    /// processed by `process_use` in this build. Both real `use`
    /// statements (from `visit_use`) and plugin-emitted `SyntheticUse`
    /// actions go through the same gate, so a second `use Moo` — real
    /// or synthetic — is a no-op. Cleared between files (lives on
    /// Builder, not FileAnalysis). Also breaks cycles when a kit
    /// plugin's SyntheticUse re-triggers the same kit plugin's `on_use`
    /// (the re-fired synthetic carries the originating use's span, so the
    /// key still collides and the cycle terminates).
    ///
    /// `span` leads the key because two distinct source statements with
    /// otherwise-identical work identity are genuinely separate work:
    /// `use constant ALPHA => 1; use constant BETA => 2;` share
    /// `(pkg, "constant", [], [])` — the constant name isn't yet folded
    /// into `constant_strings` when `imports` is extracted, so it's empty
    /// for every such line. Without the span the second statement would
    /// short-circuit and its symbol never register.
    ///
    /// `imports` lives in the key because the synthetic shape carries
    /// `args` and `imports` as separate fields, and real `use Foo qw(a)`
    /// vs `use Foo qw(b)` is two distinct pieces of work. Without
    /// `imports` here, `SyntheticUse { args: [], imports: ["a"] }` and
    /// `SyntheticUse { args: [], imports: ["b"] }` would collide on the
    /// args-only key and silently drop the second emission.
    use_dedup: std::collections::HashSet<(Span, Option<String>, String, Vec<String>, Vec<String>)>,
    /// (span_start, span_end, dispatcher, target_name) of DispatchCall refs
    /// already emitted. Two plugins can legitimately both claim a dispatch
    /// site (e.g. the bundled `minion` plugin and a project plugin that adds
    /// the same verb for a Minion subclass); identical refs at one span are
    /// pure noise that would double-count in `refs_to`. First write wins.
    dispatch_dedup: std::collections::HashSet<(Point, Point, String, String)>,
    /// sub_name → delegated sub name, for bodies that are `return other()` or
    /// a bare trailing call. Used to propagate hash-key ownership through
    /// intermediate subs so `sub chain { return get_config() }` doesn't
    /// orphan `$cfg = chain(); $cfg->{host}`.
    sub_return_delegations: std::collections::HashMap<String, String>,
    /// Framework mode per package (Moo, Moose, MojoBase) for accessor synthesis.
    framework_modes: std::collections::HashMap<String, FrameworkMode>,
    /// Functions implicitly imported by OOP frameworks (has, extends, with, etc.)
    framework_imports: std::collections::HashSet<String>,
    /// Known compile-time string values, accumulated during the walk.
    /// Keyed by variable/constant name (e.g. "@COMMON", "BASE_CLASS", "$PREFIX").
    constant_strings: std::collections::HashMap<String, Vec<String>>,
    /// Rename provenance for the const-fold path: for a scalar bound directly
    /// from a single string literal (`my $m = 'process'`), the literal's
    /// content span. A call folded through that scalar (`$self->$m()`) stamps
    /// it as `Ref.folded_from` so rename rewrites the source literal, not the
    /// `$m` variable read (rule #9). Only the direct single-literal binding is
    /// recorded — chained / multi-value folds carry no single source span.
    constant_string_source: std::collections::HashMap<String, Span>,
    /// Names declared via `use constant` (NAME and block forms), per the
    /// enclosing package. A standalone bareword whose text is in this set is
    /// a usage of the constant sub, so it earns a `FunctionCall` ref back to
    /// the def (rule #7). Recognized by set membership, never by name pattern.
    declared_constants: std::collections::HashMap<String, std::collections::HashSet<String>>,
    /// Export-list member token sites: (member name, span, enclosing package).
    /// Each member of `@EXPORT` / `@EXPORT_OK` / `%EXPORT_TAGS` value arrays
    /// that names a local sub earns a `FunctionCall` ref to that sub, emitted
    /// in a post-walk pass (subs are typically declared after the export list,
    /// so the local-sub filter can't run live). Tag-name keys never enter this
    /// list, so they get no ref (rule #7 narrow scope).
    export_member_sites: Vec<(String, Span, Option<String>)>,
    /// Exported function names from @EXPORT assignments.
    export: Vec<String>,
    /// Exported function names from @EXPORT_OK assignments.
    export_ok: Vec<String>,
    /// `%EXPORT_TAGS` membership: tag name (without the `:`/`-` selector
    /// prefix) → member sub names. Feeds the consumer-side `:tag` selector
    /// expansion in `ExportSurface`; `:DEFAULT` is synthesized there from
    /// `export`, so it is not stored here.
    export_tags: std::collections::HashMap<String, Vec<String>>,
    /// Re-export edges minted during the walk: module names whose export
    /// surface this module folds into its own (static `@Other::EXPORT` splice,
    /// loop-push over a resolvable module list, declarative `also => [...]`).
    /// Flushed into `FileAnalysis.reexport_modules`; the surface walks them
    /// transitively at query time. Dedup'd on insert via `record_reexport_edge`.
    reexport_modules: Vec<String>,
    /// `use lib` arguments, as written. Flushed into
    /// `FileAnalysis.lib_roots` — the per-asker half of module visibility.
    lib_roots: Vec<String>,
    /// Plugin-declared namespaces collected during the walk via
    /// `EmitAction::PluginNamespace`. Flushed into the final
    /// `FileAnalysis.plugin.namespaces`.
    plugin_namespaces: Vec<crate::model::file_analysis::PluginNamespace>,
    /// Per-symbol provenance for return types. Populated by plugin
    /// `overrides()` (PluginOverride) and by reducer-driven folds
    /// (ReducerFold). Empty entry == `TypeProvenance::Inferred`.
    /// Flushed into `FileAnalysis.type_provenance` at construction.
    type_provenance: std::collections::HashMap<SymbolId, TypeProvenance>,
    /// The single, unified witness bag — canonical at every phase,
    /// walk-time included. Idiom detectors (branch arms, arity
    /// gating), TC seeding (`push_type_constraint`), and post-walk
    /// passes (hash-key obs, mutations, call-binding propagation) all
    /// push directly here. Moved into `FileAnalysis.witnesses` when
    /// the analysis is constructed — no second seeding pass.
    bag: crate::model::witnesses::WitnessBag,
    /// Nodes that `emit_expr_witness` couldn't resolve at walk time.
    /// Two common shapes:
    ///   * **Forward-defined sub call** — `sub a { b() } sub b {…}`
    ///     is legal Perl, but the walk emits witnesses live and the
    ///     callee isn't in the symbol table yet when `b()` is
    ///     visited.
    ///   * **Walked-before-children parent** — `visit_function_call`
    ///     fires before its arg subtree is walked, so any
    ///     `emit_expr_witness` called inside the parent visitor on
    ///     a method-call arg finds no matching `MethodCall` ref
    ///     yet (refs are pushed during `visit_method_call`,
    ///     which runs later).
    ///
    /// `resolve_forward_expr_witnesses` retries `expr_payload` on
    /// each queued node post-walk — refs are complete + the
    /// symbol table is final by then, so the live and recovery
    /// paths produce byte-identical witnesses (same span, same
    /// `expression` source tag).
    unresolved_expr_nodes: Vec<Node<'a>>,
    /// Per-package framework fact, computed from `framework_modes` once
    /// the walk finishes. Available before `resolve_return_types` so
    /// the bag-aware return-arm fold can ask the framework-aware
    /// reducer with the right context.
    package_framework: std::collections::HashMap<String, crate::model::witnesses::FrameworkFact>,

    /// Packages that explicitly opted out of class machinery (`use Mojo::Base
    /// -strict`). In these, a bare `shift` / `$_[0]` is an ordinary argument,
    /// not the method invocant — see `shift_is_invocant_here`.
    non_oo_packages: std::collections::HashSet<String>,

    // Walk state
    scope_stack: Vec<ScopeId>,
    current_package: Option<String>,
    next_scope_id: u32,
    next_symbol_id: u32,

    /// Flat record of `package`/`class` declarations and the byte
    /// ranges they govern. Independent of the lexical scope tree —
    /// `package Foo;` is not a lexical boundary in Perl. For
    /// statement-form declarations the end is initially seeded with
    /// the file end and gets trimmed when a same-level successor
    /// appears.
    package_ranges: Vec<crate::model::file_analysis::PackageRange>,
    /// Index in `package_ranges` of the currently-open statement-form
    /// declaration (the one a successor `package X;` / `class X;`
    /// would supplant), if any.
    open_statement_package: Option<usize>,

    /// Framework plugin registry. Shared Arc so multiple builders in one
    /// process avoid re-compiling the same Rhai scripts.
    plugins: Arc<PluginRegistry>,

    /// Plugin `dispatch_verbs()` manifest, flattened to verb → spec once at
    /// construction (trigger-independent, like overrides). Drives provisional
    /// dispatch collection in the method-call walk.
    dispatch_manifest: std::collections::HashMap<String, plugin::DispatchVerb>,
    load_manifest: std::collections::HashMap<String, plugin::LoadVerb>,
    /// Constraint-constructor name gate from plugin `type_constraint_names()`
    /// (`InstanceOf`, …), flattened once. A call to one of these is typed as
    /// `TypeConstraintOf` via the plugin's fold rather than its callee return.
    type_constraint_names: std::collections::HashSet<String>,
    /// Plugin `app_surface_consumers()` manifest union, flattened once.
    /// Threaded into `BagContext` so the build-time `PackageSymbol`
    /// inheritance walk injects the synthetic app-surface parent the same
    /// way the query-time walks do (`file_analysis::parents_of`).
    app_surface_consumers: Vec<String>,
    /// Plugin `param_types()` manifest, grouped by method name. At a matching
    /// sub declaration in a role-doer, the named param gets a typed TC.
    param_type_manifest: std::collections::HashMap<String, Vec<plugin::ParamType>>,

    /// Caller-side loader facts (`plugin 'X', {...}`) — flushed into
    /// `FileAnalysis.plugin.loads`.
    plugin_loads: Vec<crate::model::file_analysis::PluginLoadFact>,
    /// Callee-side markers: params whose type arrives from loader
    /// config at enrichment. Flushed into
    /// `FileAnalysis.loader_config_params`.
    loader_config_params: Vec<crate::model::file_analysis::LoaderConfigParam>,
    /// Value-flow edges minted at assignment sites — the provenance tier; each
    /// lowers to the same type witness the builder used to push inline.
    flow_edges: Vec<crate::model::file_analysis::FlowEdge>,
    /// Rules from `param_types()` with `method: None` — applied to every sub
    /// declaration in a matching class, regardless of method name. The
    /// "every action in a controller" case (Catalyst `$c`).
    param_type_wildcards: Vec<plugin::ParamType>,
    /// True when any rule in `param_type_manifest` or `param_type_wildcards`
    /// has `requires_action_attr: true`. Precomputed after manifest load so
    /// `apply_param_type_manifest` can skip `collect_attributes` (a CST walk
    /// + Vec alloc per sub) for projects with no attribute-gated rules.
    any_requires_action_attr: bool,
    /// Dispatch candidates recorded during the walk, promoted to refs in
    /// enrichment once the receiver's cross-file class is known. See
    /// `file_analysis::ProvisionalDispatch`.
    provisional_dispatches: Vec<crate::model::file_analysis::ProvisionalDispatch>,
    /// Plugin pattern emissions deferred because a `ClassIsa` trigger
    /// couldn't be confirmed against LOCAL ancestry at build (rule #1). Each
    /// is re-fired at enrichment when the package's ancestry resolves the
    /// gate prefix cross-file. See `file_analysis::GatedEmission`.
    gated_emissions: Vec<crate::model::file_analysis::GatedEmission>,
    /// `param_types()` role-contract TCs, emitted ungated at the sub walk and
    /// gated on the enclosing package's `isa in_role` (checked cross-file at
    /// query time). See `FileAnalysis::gated_param_types`.
    gated_param_types: Vec<crate::model::file_analysis::ReceiverGated<crate::model::file_analysis::TypeConstraint>>,

    /// Build-time chain-typing cache for MethodCall ref invocants.
    /// Keyed by `refs[idx]`. Walk-time fills it for syntactic cases
    /// (constructor pattern `Foo->new`, `__PACKAGE__->m`); PostFold's
    /// `apply_chain_typing_invocants` fills it for variable invocants
    /// whose TC has crystallized. Build-time consumers
    /// (`emit_method_call_arg_keys`, `emit_method_call_return_edges`,
    /// fixed-point movement counter) read it.
    ///
    /// **Build-only.** Never copied into `FileAnalysis`. The reader-
    /// side counterpart is `FileAnalysis::method_call_invocant_class`,
    /// which queries the bag at read time and so picks up cross-file
    /// enrichment automatically.
    method_call_invocant: std::collections::HashMap<usize, String>,
    /// Plugin-declared projection-group members: accessor methods whose
    /// names derive from an attr (`predicate => has_x`). Flushed into
    /// `FileAnalysis.attr_accessors`.
    attr_projections: Vec<crate::model::file_analysis::AttrProjection>,

    /// Vars whose first escape site is already recorded as an
    /// open-switching `KeyWrite` (walk-local dedup — one escape write
    /// per var is enough: it widens the shape from that point on, and
    /// the mutation-extension pass charges one bag query per record).
    escape_recorded: std::collections::HashSet<String>,

    /// Scalars reassigned after declaration (`$v = …` targeting the
    /// variable itself; element writes go to `key_writes` instead).
    /// Flushed into `FileAnalysis.reassigned_scalars`.
    reassigned_scalars: std::collections::HashSet<String>,

    /// `$var->{key} = …` writes in walk order — input to the
    /// mutation-extension pass (shape extension / open-widening).
    /// Flushed into `FileAnalysis.key_writes`.
    key_writes: Vec<crate::model::file_analysis::KeyWrite>,

    /// Per-role `requires` lists. Flushed into
    /// `FileAnalysis.role_requires`.
    role_requires: std::collections::HashMap<String, Vec<String>>,

    /// SymbolIds of `requires`-synthesized contract markers. Flushed
    /// into `FileAnalysis.contract_symbols`.
    contract_symbols: std::collections::HashSet<crate::model::file_analysis::SymbolId>,

    /// Packages with at least one parent edge we could not fold to a
    /// literal name (runtime-generated roles). Flushed into
    /// `FileAnalysis.dynamic_parent_packages`.
    dynamic_parent_packages: std::collections::HashSet<String>,

    /// Count of dynamic method-dispatch sites (`$obj->$method(...)`) whose
    /// method name is a scalar, not a bareword. Such a call never becomes a
    /// nameable `MethodCall` ref (the dispatched method is unknown at build
    /// time unless const-folding resolves it), so it is invisible to the
    /// static reference graph. Flushed into
    /// `FileAnalysis.dynamic_dispatch_sites`; the heatmap reads it as the
    /// soundness gate that keeps zero-fan-in methods OFF the dead-code list
    /// (Perl may reach them through this invisible edge).
    dynamic_dispatch_sites: u32,

    /// Modules whose `use` makes the consuming package a role — the
    /// union of every plugin's `role_makers()` manifest (the base
    /// engines live in `frameworks/moo.rhai`). Core holds no list;
    /// the set is open by construction.
    role_maker_modules: std::collections::HashSet<String>,

    /// Module → (framework mode, exported keyword surface) for modules
    /// whose `use` grants Moo-family `has` semantics — the union of every
    /// plugin's `framework_mode_makers()` manifest (the bundled set lives
    /// in `frameworks/moo.rhai`), flavor-validated at bake. Core holds no
    /// module list; `visit_use` looks consumers up here. Mojo::Base is
    /// not in this map — its `-base` gate is structural, not a name match.
    framework_mode_modules:
        std::collections::HashMap<String, (FrameworkMode, Vec<String>)>,

    /// Per-file verdict: packages that ARE roles. Flushed into
    /// `FileAnalysis.role_packages`; `is_role_package` reads the baked
    /// set, never re-derives from use lists.
    role_packages: std::collections::HashSet<String>,

    /// DBIC `__PACKAGE__->source_name('X')` override captured during the
    /// walk (see `FileAnalysis::dbic_source_name`).
    dbic_source_name: Option<String>,

    /// Spans of topic-DSL group-scope calls (`group { … }` in lite),
    /// recorded during the walk so the fold-phase pattern dispatch can
    /// replay the topic-route base stack in document order.
    topic_group_spans: Vec<crate::model::file_analysis::Span>,
    /// Plugin-emitted diagnostics (`EmitAction::Diagnostic`), flushed
    /// onto `FileAnalysis.plugin.diagnostics`.
    plugin_diagnostics: Vec<crate::model::file_analysis::PluginDiagnostic>,

    /// Topic-route DSL manifests collected from the plugin registry —
    /// see `plugin::TopicRouteDsl`.
    topic_dsls: Vec<plugin::TopicRouteDsl>,

    /// Per-MethodCall-ref arg count, keyed by ref index. Lets
    /// `emit_method_call_return_edges` pin the call site's arity onto its
    /// `Expression(refidx)` return edge (`CallReturn`), so a fluent
    /// writer `$obj->setter($v)` resolves the writer arm even when the
    /// type query that reaches the edge is hint-less (`my $x = …`).
    /// **Build-only**, like `method_call_invocant`.
    method_call_arity: std::collections::HashMap<usize, u32>,

    /// MethodCall ref indices for which we've published an
    /// `InferredType::Parametric` witness on `Expression(refidx)`
    /// — `recv->resultset('Foo')` and search-family threading
    /// targets. `emit_method_call_return_edges` consults this set
    /// and skips publishing its standard `Edge(PackageSymbol)` for
    /// these refs, so the Parametric isn't masked by the receiver
    /// class's plain return-type entry. **Build-only**, like
    /// `method_call_invocant`.
    parametric_emitted_refs: std::collections::HashSet<usize>,

    /// Dedup for plugin-emitted `MethodCallRef`s, keyed by
    /// `(span, method_name)`. The fold-phase pattern dispatch re-runs
    /// `->to(...)` route patterns after the receiver brand resolves;
    /// this set keeps a walk-phase full-form emission at the same span
    /// from being duplicated by that fold-phase re-run.
    method_call_ref_dedup: std::collections::HashSet<(Point, Point, String)>,

    /// Refs whose `Expression(refidx)` carries a `route_brand`
    /// `BrandedRoute` witness. `emit_method_call_return_edges` skips
    /// these so its `Edge(PackageSymbol{Route, to})` (which folds to a
    /// brandless `ClassName(Route)`) doesn't mask the brand. Same role
    /// as `parametric_emitted_refs`. Cleared+refilled each fold
    /// iteration by `emit_route_brand_witnesses`.
    route_branded_refs: std::collections::HashSet<usize>,

    /// Recorded `defined`/`blessed` guards whose `Optional<T> → T` strip
    /// is re-derived each fold iteration (`emit_defined_narrowing_witnesses`).
    defined_narrowings: Vec<narrowing::DefinedNarrowing>,
    /// Recognized narrowings whose region cutoff resolves post-walk against the
    /// minted FlowEdges (`apply_narrowing_cutoffs`) — the edge-driven rebind
    /// truncation that replaced the `cst::rebinds_scalar` grammar scan.
    pending_narrowings: Vec<narrowing::PendingNarrow>,

    /// Recognized guard conditions, for the redundant/contradictory-guard
    /// diagnostics (D3/D4). Recorded alongside narrowing emission; moved into
    /// `FileAnalysis.guard_sites`.
    guard_sites: Vec<crate::model::file_analysis::GuardSite>,

    /// `$x->[i]` / `$x->()` arrow-deref receivers — the forms with no typed
    /// ref. Moved into `FileAnalysis.arrow_deref_sites` for the deref
    /// diagnostics.
    arrow_deref_sites: Vec<crate::model::file_analysis::ArrowDerefSite>,

    /// Span (of the `anonymous_subroutine_expression` node) →
    /// SymbolId of the synthesized `(anon)` Sub symbol. Populated by
    /// `visit_anonymous_sub`; read by `coderef_return_edge_for` so
    /// the edge for `sub { ... }` literals lands on
    /// `Symbol(sym_id)` instead of `Expr(body_last)` — uniform
    /// attachment shape with named subs, so `ReturnExprReducer`
    /// sees anon-sub arity arms and substitutes Receiver
    /// placeholders without any per-shape dispatch in the chase
    /// site.
    anon_sub_symbol_by_span: std::collections::HashMap<Span, SymbolId>,

    /// Invocant parameter index for the next anonymous sub to be visited.
    /// Set by `visit_function_call` when it detects a Moose/Moo method
    /// modifier (`around`/`before`/`after`): 1 for `around` (first param
    /// is `$orig`), 0 for `before`/`after`. Consumed and cleared by
    /// `visit_anonymous_sub` so it applies only to the immediately
    /// following anon sub, not to nested lambdas.
    modifier_invocant_pos: Option<usize>,

    /// Current nesting of expression-shape typing — see
    /// `MAX_EXPR_TYPE_DEPTH`. Post-order type construction cannot be
    /// queued the way the walk is, so it is bounded instead.
    expr_type_depth: usize,

    /// Pending walk work. The CST descent lives here rather than on the
    /// native stack — see `walk.rs`.
    walk_stack: Vec<WalkTask<'a>>,
    /// Restore the pre-worklist recursive descent (`PERL_LSP_RECURSIVE_WALK=1`).
    /// Read once per build so the walk primitives branch on a bool, not on env.
    /// Test-only: the descent exists for the walk-equivalence net.
    #[cfg(test)]
    recursive_walk: bool,
}

/// Owner-and-gating discriminator for `emit_call_arg_key_accesses`.
/// One emitter, three semantics:
///   * `Strict(owner)` — supplied owner; emit only if a matching
///     HashKeyDef is registered for `(key, owner)`. Prevents
///     `Foo::bar(name=>1)` from latching onto unrelated
///     `Sub{Foo,new}` keys when `name` isn't actually a `bar` arg.
///   * `Open(owner)` — supplied owner; emit unconditionally.
///     The receiver's flavor pinned the owner via
///     `method_arg_owner` — the type IS the gate; cross-file
///     producer's HashKeyDef may not be visible at consumer build.
///   * `Deferred` — owner=None at emit time. Post-walk fixup in
///     `FileAnalysis::fix_chain_receiver_hash_key_owners`
///     fills the owner once the enclosing call's receiver type
///     resolves (in-file via `RefTable::call_at_start` recursion, or
///     cross-file once `module_index` is available).
enum Gate {
    Strict(HashKeyOwner),
    /// Strict, but the owner's class is not defined in this file, so a
    /// local def miss is NOT authoritative (the def may live with the
    /// class, cross-file). Emits `owner: None` on miss; the query-time
    /// class check in `refs_to`'s key arm decides — same discipline as
    /// receiver-gated dispatch (the type is the gate, resolved with the
    /// index in hand).
    StrictOrDefer(HashKeyOwner),
    Open(HashKeyOwner),
    Deferred,
    /// A plugin-declared column-keyed verb (DBIC `search`/`create`/…): walk only
    /// the FIRST hashref arg (the `\%cond`/`\%cols`; the trailing `\%attrs` hash
    /// is not column-keyed). A key that is a `Class` column of the receiver gets
    /// the column owner; otherwise it falls back to the supplied `Sub{class,verb}`
    /// owner (so a generic-named verb like `new` still binds Moo/Corinna ctor
    /// keys). Carries the `Sub{class,verb}` owner.
    ColumnKeyed(HashKeyOwner),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum FrameworkMode {
    Moo,
    Moose,
    MojoBase,
}

impl FrameworkMode {
    /// Parse a manifest flavor string (`FrameworkModeMaker::flavor`).
    /// The flavor vocabulary is core's — each name selects a core
    /// synthesis/isa rule set — so `MojoBase` is deliberately absent:
    /// its gate is structural (`-base`), never a declared module name.
    fn from_flavor(flavor: &str) -> Option<Self> {
        match flavor {
            "Moo" => Some(Self::Moo),
            "Moose" => Some(Self::Moose),
            _ => None,
        }
    }
}

// ---- Tests ----

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "type_inference_invariants_tests.rs"]
mod invariants_tests;
