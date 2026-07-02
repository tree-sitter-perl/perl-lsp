//! Unified query surface across FileStore + ModuleIndex.
//!
//! All cross-file LSP queries (references, rename, workspace/symbol) route
//! through this module so that each handler is a one-liner against a single
//! role-masked function instead of reinventing the per-tier walk.
//!
//! `resolve_symbol` is the inverse direction: cursor → target. Every handler
//! that wants "what does this position refer to, cross-file" (LSP references,
//! LSP rename, the CLI mirrors of both) calls it and then hands the
//! `TargetRef` to `refs_to`. Handlers never re-derive the mapping inline —
//! that's how the CLI and LSP used to disagree on hash-key references.

use std::path::PathBuf;

use tower_lsp::lsp_types::Url;

use crate::file_analysis::{AccessKind, CrossFileLookup, FileAnalysis, HandlerOwner, RefKind, Span, SymKind};
use crate::file_store::{FileKey, FileStore};

bitflags::bitflags! {
    /// Which file roles a query should search. Handlers pick the mask that
    /// fits their semantics: rename is EDITABLE (skip deps, they're read-only);
    /// references is VISIBLE (include deps, read-only reads are fine).
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct RoleMask: u8 {
        const OPEN       = 1 << 0;
        const WORKSPACE  = 1 << 1;
        const DEPENDENCY = 1 << 2;
        const BUILTIN    = 1 << 3;

        const EDITABLE = Self::OPEN.bits() | Self::WORKSPACE.bits();
        const VISIBLE  = Self::OPEN.bits() | Self::WORKSPACE.bits() | Self::DEPENDENCY.bits() | Self::BUILTIN.bits();
    }
}

/// How a method that participates in an inheritance hierarchy is scoped for
/// references + rename — `initializationOptions.rename.overrideScope`.
///
/// * `Hierarchy` (default) — the standard IDE refactor: the whole override
///   family (base decl + every override + all dispatching call sites), gathered
///   over proven `@ISA`/`use parent`/role edges (never name matches).
/// * `Dispatch` — precise: only the cursor's own definition + the call sites
///   that dispatch to *that* definition (incl. `SUPER::` calls targeting it),
///   leaving sibling overrides untouched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OverrideScope {
    #[default]
    Hierarchy,
    Dispatch,
}

impl OverrideScope {
    /// Parse the `initializationOptions.rename.overrideScope` string; anything
    /// unrecognized (or absent) is the default `Hierarchy`.
    pub fn from_option(s: &str) -> Self {
        match s {
            "dispatch" => OverrideScope::Dispatch,
            _ => OverrideScope::Hierarchy,
        }
    }
}

/// Identifies what we're collecting references to.
#[derive(Debug, Clone)]
pub struct TargetRef {
    pub name: String,
    pub kind: TargetKind,
    /// Override-fan-out scope for callable (`Sub`/`Method`) targets — read by
    /// `collect_from_analysis` to pick family-membership (Hierarchy) vs
    /// dispatch-chain (Dispatch) matching. Irrelevant for other kinds.
    pub scope: OverrideScope,
    /// For a `Method` target, the inheritance rename-chain
    /// `[cursor_class, ..., defining_class]` computed ONCE from the
    /// originating analysis (the only file that knows the cursor class's
    /// parents). A `sub NAME` declaration in ANY class on this set is a
    /// declaration of the same callable — see `symbol_defines_target`.
    /// Empty for non-Method kinds (their decl match is the strict scope).
    pub method_classes: Vec<String>,
}

impl TargetRef {
    /// Build a `Method` target, precomputing the inheritance rename-chain
    /// from `origin` so declaration matching in any scanned file can admit
    /// `sub NAME` in an ancestor class — not just the cursor's static class.
    ///
    /// The chain can only be derived here: a base file (`BaseWorker.pm`)
    /// scanned later doesn't know its child `MyWorker`, so it can't recompute
    /// the chain that links the call's `MyWorker` invocant to the parent decl.
    pub fn method(
        name: String,
        class: String,
        origin: &FileAnalysis,
        module_index: Option<&dyn CrossFileLookup>,
        scope: OverrideScope,
    ) -> Self {
        let method_classes = method_classes_for(origin, &class, &name, module_index, scope);
        TargetRef {
            name,
            kind: TargetKind::Method { class },
            method_classes,
            scope,
        }
    }

    /// Build a non-Method target (no inheritance fan-out for declarations).
    pub fn new(name: String, kind: TargetKind) -> Self {
        debug_assert!(
            !matches!(kind, TargetKind::Method { .. }),
            "use TargetRef::method so the rename chain is populated"
        );
        TargetRef { name, kind, method_classes: Vec::new(), scope: OverrideScope::default() }
    }

    /// Whether this target renames cross-file through `refs_to` (matched by
    /// owner/scope structure across the workspace) vs. the single-file
    /// `rename_at` fallback. Per-feature policy lives on the target (rule #10),
    /// not inline in a handler. The kinds here key on a workspace-stable owner
    /// (a package, a class, a sub, a package global, a sub-owned hash key, a
    /// handler owner), so name + owner pins them in any file; a lexical or an
    /// owner-less hash key can't be matched by name alone elsewhere and stays
    /// single-file. References ignores this — it walks every kind cross-file.
    pub fn supports_cross_file_rename(&self) -> bool {
        matches!(
            self.kind,
            TargetKind::Sub { .. }
                | TargetKind::Method { .. }
                | TargetKind::Package
                | TargetKind::HashKeyOfSub { .. }
                | TargetKind::Handler { .. }
                | TargetKind::PackageVar { .. }
        )
    }

    /// Map a cursor-resolved `RenameKind` to the cross-file target, sharing
    /// the one mapping across both LSP handlers and both CLI modes so
    /// references and rename can't diverge on target identity (rule #5).
    /// `HashKey`/`Variable` aren't simple cross-file callables — they return
    /// `None` and the caller keeps its owner-expansion / lexical handling.
    pub fn from_rename_kind(
        kind: crate::file_analysis::RenameKind,
        origin: &FileAnalysis,
        module_index: Option<&dyn CrossFileLookup>,
        scope: OverrideScope,
    ) -> Option<Self> {
        use crate::file_analysis::RenameKind;
        Some(match kind {
            RenameKind::Function { name, package } => {
                // A `sub` in a class IS a method (Perl's only sub/method
                // distinction is call shape), so it carries the same override
                // family/chain — a base-`sub` rename reaches overrides + their
                // dispatch sites. A package-less script sub has no class, hence
                // no family.
                let method_classes = match &package {
                    Some(class) => method_classes_for(origin, class, &name, module_index, scope),
                    None => Vec::new(),
                };
                TargetRef { name, kind: TargetKind::Sub { package }, method_classes, scope }
            }
            RenameKind::Method { name, class } => {
                TargetRef::method(name, class, origin, module_index, scope)
            }
            RenameKind::Package(name) => TargetRef::new(name, TargetKind::Package),
            RenameKind::Handler { owner, name } => {
                TargetRef::new(name.clone(), TargetKind::Handler { owner, name })
            }
            RenameKind::HashKey(_) | RenameKind::Variable => return None,
        })
    }
}

/// What the cursor position resolves to, for cross-file queries.
#[derive(Debug, Clone)]
pub enum ResolvedTarget {
    /// A target `refs_to` can walk: callables, packages, handlers, and
    /// hash keys whose owner resolved at build time.
    Target(TargetRef),
    /// A projection group: one source decl spelled several ways (a
    /// Corinna `field $x :param :reader` ↔ its constructor key ↔ its
    /// reader calls). `targets` walk cross-file via `refs_to`;
    /// `local_spans` are the origin-file-only spellings (the field
    /// variable is lexical to the class block). Every span — local and
    /// walked — covers a bare name token, so rename writes one
    /// replacement text everywhere and references list them uniformly.
    Group {
        local_spans: Vec<Span>,
        /// Spellings pinned to a specific file — the class file's
        /// variable/decl spans when the group was minted remotely (the
        /// cursor sat in a consumer; the source decl lives with the
        /// class).
        pinned_spans: Vec<(PathBuf, Span)>,
        members: Vec<GroupMember>,
    },
    /// Inherently file-local: lexical variables, and hash keys with no
    /// resolvable owner. Callers keep their single-file path.
    Local,
}

/// One walkable member of a projection group, carrying its own rename
/// rule: bare spellings take the plain new name; name-mapped accessors
/// (`has_size`) re-derive theirs; members whose names don't embed the
/// attr join references but skip rename (honest).
#[derive(Debug, Clone)]
pub struct GroupMember {
    pub target: TargetRef,
    pub rename: MemberRename,
}

#[derive(Debug, Clone)]
pub enum MemberRename {
    Bare,
    Affixed { prefix: String, suffix: String },
    Skip,
}

impl MemberRename {
    fn text_for(&self, bare_new: &str) -> Option<String> {
        match self {
            MemberRename::Bare => Some(bare_new.to_string()),
            MemberRename::Affixed { prefix, suffix } => {
                Some(format!("{}{}{}", prefix, bare_new, suffix))
            }
            MemberRename::Skip => None,
        }
    }
}

/// Resolve an attribute-group spelling on `start_class` to the group minted from
/// the class that DECLARES the attr — walking up the inheritance chain when
/// `start_class` only inherits it. A subclass use of an inherited attr
/// (`$dog->name`, `Dog->new(name => …)`, `$dog->{name}` where `Dog` inherits
/// `name` from `Animal`) thus reaches the base's full group; the `class_isa`-
/// widened ctor-key/slot members + the override-family accessor then span the
/// whole subtree. `require_reader` gates the method-call entry (only an
/// accessor-bearing group claims a `$x->attr` cursor).
fn attr_group_via_ancestors(
    start_class: &str,
    bare: &str,
    origin: &FileAnalysis,
    idx: &dyn CrossFileLookup,
    require_reader: bool,
    require_internal: bool,
    scope: OverrideScope,
) -> Option<ResolvedTarget> {
    let proj = |c: &str| -> Option<crate::file_analysis::FieldProjections> {
        let ok = |p: &crate::file_analysis::FieldProjections| {
            (!require_reader || p.has_reader) && (!require_internal || p.has_internal)
        };
        origin
            .field_projections_named(bare, c)
            .filter(ok)
            .or_else(|| {
                idx.get_cached(c)
                    .and_then(|cc| cc.analysis.field_projections_named(bare, c))
                    .filter(ok)
            })
    };
    // A `has`/column group is SHARED storage: under Hierarchy mint from the
    // ROOT-most declarer so the `class_isa`-widened ctor-key/slot members span
    // the whole subtree (an overriding subclass and the base resolve to one
    // family group, both directions); under Dispatch, the nearest. But a Corinna
    // `field` is per-class PRIVATE storage — never widen it, mint from the
    // nearest declarer so a subclass's field doesn't capture an ancestor's.
    let mut defining: Option<String> = None;
    let mut nearest_field_backed = false;
    origin.for_each_ancestor_class(start_class, Some(idx), |c| {
        if let Some(p) = proj(c) {
            if defining.is_none() {
                nearest_field_backed = p.field_backed;
            }
            defining = Some(c.to_string());
            if nearest_field_backed || scope == OverrideScope::Dispatch {
                return std::ops::ControlFlow::Break(());
            }
        }
        std::ops::ControlFlow::Continue(())
    });
    let defining = defining?;
    // Mint from the defining class's analysis (origin file if it's local, else
    // the indexed module — its decl/variable spans pin to the class file).
    let ok = |p: &crate::file_analysis::FieldProjections| {
        (!require_reader || p.has_reader) && (!require_internal || p.has_internal)
    };
    if let Some(p) = origin.field_projections_named(bare, &defining) {
        if ok(&p) {
            return Some(group_from_projections(p, origin, None, Some(idx)));
        }
    }
    let cached = idx.get_cached(&defining)?;
    let p = cached.analysis.field_projections_named(bare, &defining)?;
    if !ok(&p) {
        return None;
    }
    Some(group_from_projections(p, &cached.analysis, Some(cached.path.clone()), Some(idx)))
}

/// Cursor → cross-file target. The single entry point for "what does this
/// position refer to" — both LSP handlers (references, rename) and their CLI
/// mirrors route here, so target identity can never diverge between them.
/// `None` = nothing resolvable at the cursor at all.
pub fn resolve_symbol(
    analysis: &FileAnalysis,
    point: tree_sitter::Point,
    module_index: Option<&dyn CrossFileLookup>,
) -> Option<ResolvedTarget> {
    resolve_symbol_scoped(analysis, point, module_index, OverrideScope::default())
}

/// `resolve_symbol` with an explicit override scope — the rename/references
/// handlers pass the configured `rename.overrideScope` so a callable target's
/// `method_classes` is built as the override family (Hierarchy, default) or the
/// dispatch chain (Dispatch). Other callers use `resolve_symbol` (Hierarchy).
pub fn resolve_symbol_scoped(
    analysis: &FileAnalysis,
    point: tree_sitter::Point,
    module_index: Option<&dyn CrossFileLookup>,
    scope: OverrideScope,
) -> Option<ResolvedTarget> {
    use crate::file_analysis::{HashKeyOwner, RenameKind};
    // Field projections claim first: from any spelling of a field group,
    // the answer is the whole group (rename and references stay in
    // lockstep with the in-file `rename_at`/`find_references` union).
    if let Some(p) = analysis.field_projections_at(point) {
        // A cursor on an OVERRIDING subclass's own decl should resolve to the
        // same family group as the base under Hierarchy — bridge to the
        // root-most declaring ancestor (no-op for a non-overridden attr).
        if let Some(idx) = module_index {
            if let Some(g) =
                attr_group_via_ancestors(&p.class, &p.bare, analysis, idx, false, false, scope)
            {
                return Some(g);
            }
        }
        return Some(group_from_projections(p, analysis, None, module_index));
    }
    // Consumer-side: the class lives elsewhere. The owner edge the
    // deferred key (or the accessor call's invocant) already carries
    // reaches the class NAME at query time; one more hop through the
    // index reaches the class's analysis, which holds the group facts
    // the cursor file can't see. Its variable/decl spans pin to the
    // class file.
    if let (Some(idx), Some(r)) = (module_index, analysis.ref_at(point)) {
        use crate::file_analysis::HashKeyOwner;
        match &r.kind {
            RefKind::HashKeyAccess { owner, .. } => {
                // Reach the owning class's group from a consumer-side cursor. The
                // `bool` is `require_internal`: a `$obj->{attr}` deref carries a
                // generic `Class` lookup and is a real reference ONLY to an
                // internal-slot attr (Moo/bless `InternalKey`) — a bridged key
                // (DBIC column) isn't a hash slot, so a deref onto one resolves to
                // nothing. A bridged condition-arg (`search({col})`) resolves to
                // the column group with no slot requirement.
                let target: Option<(String, bool)> = match owner {
                    Some(HashKeyOwner::Bridged { class: c }) => Some((c.clone(), false)),
                    Some(HashKeyOwner::Class(c)) => Some((c.clone(), true)),
                    _ => match analysis.deferred_hash_key_owner(r, module_index) {
                        Some(HashKeyOwner::Sub { package: Some(c), name })
                            if crate::conventions::is_constructor_name(&name) =>
                        {
                            Some((c, false))
                        }
                        Some(HashKeyOwner::Bridged { class: c }) => Some((c, false)),
                        Some(HashKeyOwner::Class(c)) => Some((c, true)),
                        _ => None,
                    },
                };
                if let Some((class, require_internal)) = target {
                    if let Some(g) = attr_group_via_ancestors(
                        &class, &r.target_name, analysis, idx, false, require_internal, scope,
                    ) {
                        return Some(g);
                    }
                }
            }
            RefKind::MethodCall { .. } => {
                let bare = r.unqualified_target_name().to_string();
                if let Some(class) = analysis.method_call_invocant_class(r, module_index) {
                    // Only an accessor-bearing group may claim a method-call
                    // cursor (`require_reader`).
                    if let Some(g) =
                        attr_group_via_ancestors(&class, &bare, analysis, idx, true, false, scope)
                    {
                        return Some(g);
                    }
                }
            }
            _ => {}
        }
    }
    // An `our` package global is a cross-file rename (`$Pkg::var` reaches it
    // from anywhere) — claim it before the lexical `Variable => Local` path.
    if let Some((package, name)) = analysis.package_var_at(point) {
        if package == "main" {
            // `main` is the catch-all namespace every package-less entry script
            // shares, so two *unrelated* scripts' `our $x` both land in
            // `main::x`. Without program-boundary (entrypoint) analysis we can't
            // tell those apart, so a cross-file fan-out would rename one script's
            // global from another's. Stay file-local here (a real package fans
            // out): collect this file's spellings (decl + bare reads + `$main::x`
            // / `$::x`) as a flat group. Lift this once entrypoint analysis can
            // group a program's files (docs/prompt-entrypoint-analysis.md — the
            // same root as multi-app Mojo instance brands).
            let mut locs = Vec::new();
            collect_package_var(&FileKey::Path(PathBuf::new()), analysis, &package, &name, &mut locs);
            let mut spans: Vec<Span> = locs.into_iter().map(|l| l.span).collect();
            // The decl token is both a symbol and a self-ref; dedup so a span
            // isn't rewritten twice.
            spans.sort_by_key(|s| (s.start.row, s.start.column, s.end.row, s.end.column));
            spans.dedup();
            return Some(ResolvedTarget::Group {
                local_spans: spans,
                pinned_spans: Vec::new(),
                members: Vec::new(),
            });
        }
        return Some(ResolvedTarget::Target(TargetRef::new(
            name,
            TargetKind::PackageVar { package },
        )));
    }
    // Pack-language backward lanes: def→uses mirrors of resolutions goto-def
    // already does forward, on the SAME key. All gates are structural facts
    // Perl analyses never exhibit (sigil-less Variable/Field symbols,
    // `macro_defs`), so Perl cursors fall through untouched.
    if let Some(sym) = analysis.symbol_at(point) {
        // A `#define`'s own def site (its symbol's selection span IS a
        // MacroDef). The forward macro lane keys on the bare word
        // (`pack_macro_definition`); the backward target carries the same
        // name-keyed identity — object-like AND function-like.
        if analysis.names_macro_def(&sym.name, Some(sym.selection_span)) {
            return Some(ResolvedTarget::Target(TargetRef::new(
                sym.name.clone(),
                TargetKind::FileScopeValue,
            )));
        }
        // A struct/role member (`op_type` in the `BASEOP` block) and an enum
        // constant (`OP_SCOPE`, carrying its enum as `package`) are BOTH
        // package-tagged sigil-less Variable/Field defs — the DEF can't tell
        // them apart; only the USE shape differs (a member is a member-access
        // `MethodCall`, an enum constant a bare `Variable` value read). Both
        // resolve to the SAME `Method{class}` target their uses resolve to;
        // `collect_from_analysis` matches both shapes. The structural
        // class-content gate keeps a lexical local out — a pack local inside
        // an inline method carries the class as sticky `package` too, so the
        // package tag alone would over-claim.
        if analysis.symbol_is_class_content(sym) {
            let class = sym.package.clone().expect("class content is package-tagged");
            return Some(ResolvedTarget::Target(TargetRef::method(
                sym.name.clone(),
                class,
                analysis,
                module_index,
                scope,
            )));
        }
        // A file-scope global / anonymous-enum constant: bare-name-keyed,
        // like the generic cross-file goto-def tail that resolves its uses.
        if analysis.symbol_is_file_scope_value(sym) {
            return Some(ResolvedTarget::Target(TargetRef::new(
                sym.name.clone(),
                TargetKind::FileScopeValue,
            )));
        }
    }
    // The same lanes from a USE site, so gr from a use equals gr from the
    // def: a bare read / type token that resolves (locally or by name
    // cross-file) to a macro, class content, or file-scope value mints the
    // identical target the def site does.
    if let Some(r) = analysis.ref_at(point) {
        if matches!(r.kind, RefKind::Variable | RefKind::PackageRef) {
            let class_or_value = |a: &FileAnalysis, s: &crate::file_analysis::Symbol| {
                if a.names_macro_def(&s.name, Some(s.selection_span))
                    || a.symbol_is_file_scope_value(s)
                {
                    Some(None)
                } else if a.symbol_is_class_content(s) {
                    Some(s.package.clone())
                } else {
                    None
                }
            };
            let resolved: Option<Option<String>> = match r.resolves_to {
                Some(id) => class_or_value(analysis, analysis.symbol(id)),
                None if analysis.names_macro_def(&r.target_name, None) => Some(None),
                None => module_index
                    .and_then(|idx| idx.get_cached(&r.target_name))
                    .and_then(|cached| {
                        if cached.analysis.names_macro_def(&r.target_name, None) {
                            return Some(None);
                        }
                        cached
                            .analysis
                            .symbols
                            .iter()
                            .filter(|s| s.name == r.target_name)
                            .find_map(|s| class_or_value(&cached.analysis, s))
                    }),
            };
            match resolved {
                Some(Some(class)) => {
                    return Some(ResolvedTarget::Target(TargetRef::method(
                        r.target_name.clone(),
                        class,
                        analysis,
                        module_index,
                        scope,
                    )));
                }
                Some(None) => {
                    return Some(ResolvedTarget::Target(TargetRef::new(
                        r.target_name.clone(),
                        TargetKind::FileScopeValue,
                    )));
                }
                None => {}
            }
        }
    }
    Some(match analysis.rename_kind_at(point, module_index)? {
        RenameKind::Variable => ResolvedTarget::Local,
        RenameKind::HashKey(name) => match analysis.hash_key_owner_at(point) {
            Some(HashKeyOwner::Sub { package, name: sub_name }) => ResolvedTarget::Target(
                TargetRef::new(name, TargetKind::HashKeyOfSub { package, name: sub_name }),
            ),
            // A bridged key (DBIC column condition-arg / accessor): the fallback
            // when no field-group path caught it (e.g. single-file, no index).
            Some(HashKeyOwner::Bridged { class }) => {
                ResolvedTarget::Target(TargetRef::new(name, TargetKind::HashKeyOfBridged(class)))
            }
            // `Class` here is a `$obj->{key}` deref onto a real hash slot. If a
            // field group (Moo/bless `InternalKey`) didn't already claim it, it's
            // a plain deref — single-file. A bridged key is NEVER a `Class` owner,
            // so a deref can't reach one. Variable-owned (lexical `my %h`) and
            // unresolved fall here too; the `Local` path handles all three.
            _ => ResolvedTarget::Local,
        },
        kind => ResolvedTarget::Target(
            TargetRef::from_rename_kind(kind, analysis, module_index, scope)
                .expect("Function/Method/Package/Handler kinds map to a target"),
        ),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetKind {
    /// An `our` (package-global) variable. `package` is the declaring
    /// package; `target.name` is the sigil-bearing name (`$debug`). Unlike a
    /// lexical `my` (which stays `Local`/single-file), a package global is
    /// reachable everywhere as `$Pkg::var`, so rename fans out cross-file:
    /// matches the `our` decl, every qualified `$Pkg::var` access in any file,
    /// and the declaring file's unqualified reads that resolve to it.
    PackageVar { package: String },
    /// A sub defined in a specific package. `None` = the sub has
    /// no package context (top-level script). Matches `Sub`/`Method`
    /// symbols whose `package` field equals this, and `FunctionCall`
    /// refs whose `resolved_package` equals this. Package-scoping
    /// mirrors method's class-scoping — name-only matching
    /// cross-links `Foo::run` and `Bar::run`.
    Sub { package: Option<String> },
    /// A method on a specific class. Matches `Sub`/`Method` symbols
    /// whose `package == class`, and `MethodCall` refs whose invocant
    /// resolves to `class`.
    Method { class: String },
    /// A package/class/module name — matches PackageRef refs.
    Package,
    /// A hash key owned by a specific sub's return value. `package` is the
    /// sub's defining package (or None for top-level / unpackaged subs);
    /// matches `HashKeyOwner::Sub { package, name }` by structural equality.
    HashKeyOfSub { package: Option<String>, name: String },
    /// A hash key owned by a class (Moo `has` slots, DBIC columns on a Result class).
    HashKeyOfBridged(String),
    /// An attr's internal hash slot (`$self->{attr}` — or any
    /// `$obj->{attr}` poke; Perl culture is promiscuous about reaching
    /// into the hashref). STRICT `HashKeyOwner::Class` matching, never
    /// `found_by` — broadening would leak other subs' same-named arg
    /// keys into the projection group this member serves.
    InternalHashKey { class: String },
    /// A `Handler` symbol registered on a class (Mojo events, Dancer
    /// routes, etc.). Both the definition (`Handler` symbol) and call
    /// sites (`DispatchCall` refs) match; stacked registrations all
    /// surface separately so features can enumerate every handler.
    Handler {
        owner: HandlerOwner,
        name: String,
    },
    /// A pack-language file-scope value reachable by BARE NAME from any file
    /// that can see it (C's flat linkage): an object- or function-like
    /// `#define`, a global variable, an anonymous-enum constant. The backward
    /// (def→uses) mirror of the by-name forward resolutions — the macro
    /// goto-def lane and the generic cross-file name tail — so both
    /// directions share one key: the bare name. Matches every `#define` of
    /// the name (config variants included, never pruned), file-scope
    /// `Variable` decls, and name-keyed bare reads / unresolved calls /
    /// type-position uses. Never minted from a Perl cursor (Perl variables
    /// carry sigils; Perl callables aren't `Variable` symbols).
    FileScopeValue,
}

/// A located reference in some file.
#[derive(Debug, Clone)]
pub struct RefLocation {
    pub key: FileKey,
    pub span: Span,
    /// Read/Write/Declaration — used by document_highlight callers that will
    /// migrate to `refs_to` in a follow-up.
    #[allow(dead_code)]
    pub access: AccessKind,
    /// Whether rename may rewrite this span. `false` for a site whose name has
    /// no literal token to replace — a const-folded event name
    /// (`my $e = 'ready'; $obj->on($e)`) whose dispatch span IS the variable.
    /// References lists it (it's a real use); rename skips it (rewriting the
    /// variable would corrupt it). True for every literal occurrence.
    pub rewritable: bool,
}

impl RefLocation {
    pub fn to_url(&self) -> Option<Url> {
        match &self.key {
            FileKey::Url(u) => Some(u.clone()),
            FileKey::Path(p) => Url::from_file_path(p).ok(),
        }
    }
}

/// Group construction shared by the local arm (cursor in the class
/// file: spans are origin-local) and the consumer arm (group minted
/// from the class's cached analysis: spans pin to the class file). The
/// rename chain on the Method target is computed against the CLASS
/// analysis — the only one that knows its parents.
fn group_from_projections(
    p: crate::file_analysis::FieldProjections,
    class_analysis: &FileAnalysis,
    pinned_path: Option<PathBuf>,
    module_index: Option<&dyn CrossFileLookup>,
) -> ResolvedTarget {
    let mut members = Vec::new();
    if p.has_reader {
        // A Corinna `field`'s reader is per-class (private storage), so scope it
        // precisely (Dispatch) — never fan to an ancestor's same-named reader,
        // which would rewrite that class's own private field decl and corrupt
        // it. A `has`/column accessor IS shared down the hierarchy → family.
        let reader_scope = if p.field_backed {
            OverrideScope::Dispatch
        } else {
            OverrideScope::Hierarchy
        };
        members.push(GroupMember {
            target: TargetRef::method(
                p.bare.clone(),
                p.class.clone(),
                class_analysis,
                module_index,
                reader_scope,
            ),
            rename: MemberRename::Bare,
        });
    }
    if p.has_param {
        members.push(GroupMember {
            target: TargetRef::new(
                p.bare.clone(),
                TargetKind::HashKeyOfSub {
                    package: Some(p.class.clone()),
                    name: "new".to_string(),
                },
            ),
            rename: MemberRename::Bare,
        });
    }
    if p.has_internal {
        members.push(GroupMember {
            target: TargetRef::new(
                p.bare.clone(),
                TargetKind::InternalHashKey { class: p.class.clone() },
            ),
            rename: MemberRename::Bare,
        });
    }
    if p.has_class_key {
        // `Bridged`-backed attr (DBIC column): a `HashKeyOfBridged` member catches
        // the column's condition-arg keys (`search`/`find`/`update`), owned by the
        // `Bridged` namespace — NOT a `$row->{col}` deref (a column isn't a slot).
        members.push(GroupMember {
            target: TargetRef::new(
                p.bare.clone(),
                TargetKind::HashKeyOfBridged(p.class.clone()),
            ),
            rename: MemberRename::Bare,
        });
    }
    for m in &p.mapped {
        members.push(GroupMember {
            target: TargetRef::method(
                m.method.clone(),
                p.class.clone(),
                class_analysis,
                module_index,
                OverrideScope::Hierarchy,
            ),
            rename: match &m.affix {
                Some((pre, suf)) => MemberRename::Affixed {
                    prefix: pre.clone(),
                    suffix: suf.clone(),
                },
                None => MemberRename::Skip,
            },
        });
    }
    match pinned_path {
        None => ResolvedTarget::Group {
            local_spans: p.variable_spans,
            pinned_spans: Vec::new(),
            members,
        },
        Some(path) => ResolvedTarget::Group {
            local_spans: Vec::new(),
            pinned_spans: p
                .variable_spans
                .into_iter()
                .map(|s| (path.clone(), s))
                .collect(),
            members,
        },
    }
}

/// Union of `refs_to` over a projection group's targets plus the group's
/// origin-file spans. `mask_override` = `Some(EDITABLE)` for rename;
/// `None` lets each target pick its references mask. Output is sorted +
/// deduped like `refs_to`, and every span covers a bare name token, so a
/// rename caller can write one replacement text at every location.
pub fn group_refs(
    files: &FileStore,
    module_index: Option<&dyn CrossFileLookup>,
    origin: &FileKey,
    local_spans: &[Span],
    pinned_spans: &[(PathBuf, Span)],
    members: &[GroupMember],
    mask_override: Option<RoleMask>,
) -> Vec<RefLocation> {
    let mut out: Vec<RefLocation> = local_spans
        .iter()
        .map(|span| RefLocation {
            key: origin.clone(),
            span: *span,
            access: AccessKind::Read,
            rewritable: true,
        })
        .collect();
    out.extend(pinned_spans.iter().map(|(path, span)| RefLocation {
        key: FileKey::Path(path.clone()),
        span: *span,
        access: AccessKind::Read,
        rewritable: true,
    }));
    for m in members {
        let mask = mask_override
            .unwrap_or_else(|| references_mask_for(files, module_index, &m.target));
        out.extend(refs_to(files, module_index, &m.target, mask));
    }
    out.sort_by(|a, b| {
        key_for_sort(&a.key)
            .cmp(&key_for_sort(&b.key))
            .then_with(|| {
                (a.span.start.row, a.span.start.column)
                    .cmp(&(b.span.start.row, b.span.start.column))
            })
    });
    out.dedup_by(|a, b| file_key_eq(&a.key, &b.key) && a.span == b.span);
    out
}

/// Reject a `newName` that would corrupt rather than rename: empty,
/// whitespace, or just sigils (`$`/`@`/`%`). The LSP client normally validates
/// the new name, but the server must not emit a token-*deleting* edit set when
/// it doesn't — both rename entry points (LSP handler + CLI) gate on this.
/// Keyword/identifier-shape validation stays the client's job; this is the
/// safety floor against silent corruption.
pub fn is_valid_rename_name(new_name: &str) -> bool {
    !new_name.trim().trim_start_matches(['$', '@', '%']).trim().is_empty()
}

/// Rename edit set for a projection group: every span paired with ITS
/// member's replacement text (bare for plain spellings, re-derived for
/// affixed accessors). Bare-member spans win collisions — a synthesized
/// accessor's decl token IS the group decl the bare edit covers.
pub fn group_rename_edits(
    files: &FileStore,
    module_index: Option<&dyn CrossFileLookup>,
    origin: &FileKey,
    local_spans: &[Span],
    pinned_spans: &[(PathBuf, Span)],
    members: &[GroupMember],
    bare_new: &str,
) -> Vec<(RefLocation, String)> {
    let mut out: Vec<(RefLocation, String)> = local_spans
        .iter()
        .map(|span| {
            (
                RefLocation { key: origin.clone(), span: *span, access: AccessKind::Read, rewritable: true },
                bare_new.to_string(),
            )
        })
        .collect();
    out.extend(pinned_spans.iter().map(|(path, span)| {
        (
            RefLocation {
                key: FileKey::Path(path.clone()),
                span: *span,
                access: AccessKind::Read,
                rewritable: true,
            },
            bare_new.to_string(),
        )
    }));
    // Bare members before affixed ones, so a same-span collision keeps the
    // bare edit (dedup below keeps the first).
    let mut ordered: Vec<&GroupMember> = members
        .iter()
        .filter(|m| matches!(m.rename, MemberRename::Bare))
        .collect();
    ordered.extend(
        members
            .iter()
            .filter(|m| !matches!(m.rename, MemberRename::Bare)),
    );
    for m in ordered {
        let Some(text) = m.rename.text_for(bare_new) else { continue };
        for loc in refs_to(files, module_index, &m.target, RoleMask::EDITABLE) {
            out.push((loc, text.clone()));
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|(loc, _)| seen.insert((key_for_sort(&loc.key), loc.span)));
    out
}

/// Collect every reference to `target` across the masked file set.
///
/// - `files`   — open + workspace store
/// - `module_index` — dep cache (consulted only if mask includes Dependency)
pub fn refs_to(
    files: &FileStore,
    module_index: Option<&dyn CrossFileLookup>,
    target: &TargetRef,
    mask: RoleMask,
) -> Vec<RefLocation> {
    let mut out = Vec::new();

    // Names that reach the target through a macro delegation edge — the
    // BACKWARD half of goto-def's see-through (`#define IncRef(sv)
    // Perl_Inc(sv)` means every `IncRef(...)` call site is a reference to
    // `Perl_Inc`). Computed once per query; empty for Perl.
    let aliases = delegation_aliases(files, module_index, target, mask);

    // Open files (canonical — workspace entries for open paths are skipped).
    let mut covered_paths: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    if mask.contains(RoleMask::OPEN) {
        files.for_each_open_mut(|url, doc| {
            let url = url.clone();
            if let Ok(p) = url.to_file_path() {
                covered_paths.insert(p);
            }
            collect_from_analysis(&FileKey::Url(url), &doc.analysis, target, &aliases, module_index, &mut out);
        });
    } else {
        // Even if open isn't in the mask, track the paths so a WORKSPACE walk
        // doesn't duplicate them (an open file's pre-close state isn't meaningful).
        files.for_each_open_mut(|url, _doc| {
            if let Ok(p) = url.to_file_path() {
                covered_paths.insert(p);
            }
        });
    }

    // Workspace files.
    if mask.contains(RoleMask::WORKSPACE) {
        for entry in files.workspace_raw().iter() {
            if covered_paths.contains(entry.key()) {
                continue;
            }
            covered_paths.insert(entry.key().clone());
            collect_from_analysis(&FileKey::Path(entry.key().clone()), entry.value(), target, &aliases, module_index, &mut out);
        }
    }

    // Dependencies (read-only modules from @INC / the pack-language cache).
    // Per-FILE sweep (`for_each_cached_file`): the name-keyed view both
    // repeats files and HIDES a file that lost every name tie. Skip paths an
    // open/workspace copy already covered — those are fresher.
    if mask.contains(RoleMask::DEPENDENCY) {
        if let Some(idx) = module_index {
            idx.for_each_cached_file(&mut |cached| {
                if !covered_paths.insert(cached.path.clone()) {
                    return;
                }
                let key = FileKey::Path(cached.path.clone());
                collect_from_analysis(&key, &cached.analysis, target, &aliases, module_index, &mut out);
            });
        }
    }

    // Sort for stable output, dedupe by (path, span).
    out.sort_by(|a, b| {
        key_for_sort(&a.key)
            .cmp(&key_for_sort(&b.key))
            .then_with(|| {
                (a.span.start.row, a.span.start.column)
                    .cmp(&(b.span.start.row, b.span.start.column))
            })
    });
    out.dedup_by(|a, b| file_key_eq(&a.key, &b.key) && a.span == b.span);
    out
}

/// `textDocument/implementation`: local defs of `name` in every
/// transitive descendant package of the Method target's class. On a
/// role's `requires` marker that's "every composer's def of the
/// contract"; on a class method it's "every subclass override".
/// Goto-def stays on the contract/def itself; call sites stay on
/// references — this is the third verb, not a variant of either.
///
/// A descendant role's own re-`requires` marker is a contract
/// re-declaration, not an implementation — `role_requires` is the
/// recorded fact that identifies (and excludes) it.
pub fn implementations_of(
    origin: &FileAnalysis,
    module_index: Option<&dyn CrossFileLookup>,
    target: &TargetRef,
) -> Vec<RefLocation> {
    let TargetKind::Method { class } = &target.kind else {
        return Vec::new();
    };
    let Some(idx) = module_index else {
        return Vec::new();
    };
    // The composer fan-out is a graph walk: INHERITS_INV from the
    // contract's class — the first strangler-fig consumer ported onto
    // the one walker (docs/prompt-graph-walking.md).
    let mut descendants: Vec<String> = Vec::new();
    let probe = crate::graph::GraphView::new(origin, Some(idx));
    probe.walk(
        crate::graph::Node::Class(class.clone()),
        crate::graph::EdgeKindMask::INHERITS_INV,
        &mut |n| {
            if let crate::graph::Node::Class(c) = n {
                descendants.push(c.clone());
            }
            std::ops::ControlFlow::Continue(())
        },
    );
    let mut out: Vec<RefLocation> = Vec::new();
    for pkg in &descendants {
        // class → home module(s): exact cache key for the common
        // single-package file; the names index covers cross-named and
        // multi-package homes.
        let mut homes: Vec<std::sync::Arc<crate::file_analysis::CachedModule>> = Vec::new();
        if let Some(c) = idx.get_cached(pkg) {
            homes.push(c);
        } else {
            for m in idx.modules_with_symbol(pkg) {
                if let Some(c) = idx.get_cached(&m) {
                    let declares = c.analysis.symbols.iter().any(|s| {
                        matches!(s.kind, SymKind::Package | SymKind::Class) && &s.name == pkg
                    });
                    if declares {
                        homes.push(c);
                    }
                }
            }
        }
        for cached in homes {
            let is_marker = cached
                .analysis
                .role_requires
                .get(pkg.as_str())
                .is_some_and(|reqs| reqs.iter().any(|r| r == &target.name));
            if is_marker {
                continue;
            }
            for s in &cached.analysis.symbols {
                if s.name == target.name
                    && matches!(s.kind, SymKind::Sub | SymKind::Method)
                    && s.package.as_deref() == Some(pkg.as_str())
                {
                    out.push(RefLocation {
                        key: FileKey::Path(cached.path.clone()),
                        span: s.selection_span,
                        access: AccessKind::Declaration,
                        rewritable: true,
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| {
        key_for_sort(&a.key)
            .cmp(&key_for_sort(&b.key))
            .then_with(|| {
                (a.span.start.row, a.span.start.column)
                    .cmp(&(b.span.start.row, b.span.start.column))
            })
    });
    out.dedup_by(|a, b| file_key_eq(&a.key, &b.key) && a.span == b.span);
    out
}

/// The macro names whose call sites dispatch to `target` through delegation
/// edges (`MacroDef::delegate`), transitively (`#define A(x) B(x)`,
/// `#define B(x) F(x)` — both A and B reach F). The backward mirror of the
/// forward see-through offer in `pack_macro_definition`. Only callable-
/// name-keyed kinds have a delegation surface; the DEPENDENCY sweep is
/// gated on the mask so a Perl EDITABLE query never touches the dep cache.
/// Sorted for deterministic output.
fn delegation_aliases(
    files: &FileStore,
    module_index: Option<&dyn CrossFileLookup>,
    target: &TargetRef,
    mask: RoleMask,
) -> Vec<String> {
    if !matches!(
        target.kind,
        TargetKind::Sub { .. } | TargetKind::Method { .. } | TargetKind::FileScopeValue
    ) {
        return Vec::new();
    }
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut add = |a: &FileAnalysis| {
        for m in &a.macro_defs {
            if let Some(d) = &m.delegate {
                pairs.push((m.name.clone(), d.clone()));
            }
        }
    };
    files.for_each_open_mut(|_url, doc| add(&doc.analysis));
    for entry in files.workspace_raw().iter() {
        add(entry.value());
    }
    if mask.contains(RoleMask::DEPENDENCY) {
        if let Some(idx) = module_index {
            idx.for_each_cached_file(&mut |cached| add(&cached.analysis));
        }
    }
    if pairs.is_empty() {
        return Vec::new();
    }
    // Reverse-transitive chase: every name whose delegation chain reaches
    // the target's name.
    let mut out: Vec<String> = Vec::new();
    let mut frontier: Vec<String> = vec![target.name.clone()];
    while let Some(cur) = frontier.pop() {
        for (n, d) in &pairs {
            if *d == cur && *n != target.name && !out.contains(n) {
                out.push(n.clone());
                frontier.push(n.clone());
            }
        }
    }
    out.sort();
    out
}

fn key_for_sort(k: &FileKey) -> PathBuf {
    match k {
        FileKey::Path(p) => p.clone(),
        FileKey::Url(u) => u.to_file_path().unwrap_or_else(|_| PathBuf::from(u.as_str())),
    }
}

fn file_key_eq(a: &FileKey, b: &FileKey) -> bool {
    key_for_sort(a) == key_for_sort(b)
}

/// The classes whose declarations a callable rename should match: the override
/// FAMILY for `Hierarchy` (root + every overrider/inheritor), the dispatch
/// CHAIN for `Dispatch` (cursor class up to the def its dispatch lands on).
fn method_classes_for(
    origin: &FileAnalysis,
    class: &str,
    name: &str,
    module_index: Option<&dyn CrossFileLookup>,
    scope: OverrideScope,
) -> Vec<String> {
    match scope {
        OverrideScope::Hierarchy => origin.method_override_family(class, name, module_index),
        OverrideScope::Dispatch => origin.method_rename_chain(class, name, module_index),
    }
}

/// A dispatch name that is actually spelled by *another* identifier, so the
/// token at `span` is not the literal name and rename must not rewrite it
/// (references still resolve through the fold). A variable fold
/// (`$obj->on($evt)`, `$self->$m()` — a `Variable`/`ContainerAccess` ref covers
/// the span) always counts. A const fold (`$obj->on(EVT)` — a `FunctionCall`
/// ref to the constant covers it) counts only when `include_calls` — for a
/// `Sub`/`Method` target a coinciding `FunctionCall` is the callable's OWN call
/// site (which MUST rename), not a fold; only handlers fold through a call.
/// A literal name (`on('connect')`) sits at its string-content span, uncovered.
fn span_is_folded_name(analysis: &FileAnalysis, span: Span, include_calls: bool) -> bool {
    analysis.refs.iter().any(|r| {
        (matches!(r.kind, RefKind::Variable | RefKind::ContainerAccess)
            || (include_calls && matches!(r.kind, RefKind::FunctionCall { .. })))
            && r.span == span
    })
}

/// True when `sym` is a declaration of `target` (decl-span match).
/// Shared by `collect_from_analysis` (to emit decl locations) and
/// `mask_for_target` (to decide whether the def lives in editable space).
/// `analysis` is the file the symbol lives in — the structural gates
/// (class-content, macro spans) need its scopes/macro table.
fn symbol_defines_target(
    sym: &crate::file_analysis::Symbol,
    target: &TargetRef,
    analysis: &FileAnalysis,
) -> bool {
    use crate::file_analysis::{DeclKind, HashKeyOwner, SymbolDetail};
    if sym.name != target.name {
        return false;
    }
    // Treat a sub and a method in the same package as the same
    // callable — Perl's only distinction between them is call shape.
    // `Sub { package }` matches exactly that scope (None = top-level
    // script sub); `Method { class }` is `Sub { package: Some(class) }`
    // with stricter intent.
    match &target.kind {
        // The `our` decl in the named package (`our $debug` in `Cfg`). The
        // sigil-bearing name is already matched by the `sym.name == target.name`
        // gate above; `collect_package_var` owns the (sigil-narrowed) span.
        TargetKind::PackageVar { package } => {
            matches!(&sym.detail, SymbolDetail::Variable { decl_kind: DeclKind::Our, .. })
                && sym.package.as_deref() == Some(package.as_str())
        }
        TargetKind::Sub { package } => {
            // Exact scope, OR — under Hierarchy — any class in the override
            // family (so a base-`sub` rename also rewrites every override's
            // decl). Dispatch keeps the strict single-scope match.
            let in_scope = sym.package == *package
                || (target.scope == OverrideScope::Hierarchy
                    && target
                        .method_classes
                        .iter()
                        .any(|c| Some(c.as_str()) == sym.package.as_deref()));
            matches!(sym.kind, SymKind::Sub | SymKind::Method) && in_scope
        }
        TargetKind::Method { class } => {
            // A `sub NAME` declaration belongs to this target if it lives in
            // ANY class on the inheritance rename-chain — the parent that
            // actually defines an inherited method, not only the cursor's
            // static class. The chain is precomputed on the target (it can't
            // be re-derived while scanning the base file, which doesn't know
            // its children). Empty chain falls back to the strict class match
            // so a Method built outside `TargetRef::method` still works.
            let on_chain = target
                .method_classes
                .iter()
                .any(|c| Some(c.as_str()) == sym.package.as_deref())
                || sym.package.as_deref() == Some(class.as_str());
            // A data member (cpp `o->field`) or enum constant mints the same
            // by-name uses a method does, so its `Variable`/`Field` decl is
            // the target's declaration too — gated by the structural
            // class-content check, because a pack LOCAL inside an inline
            // method also carries the class as sticky `package` and must
            // never read as a member declaration.
            (matches!(sym.kind, SymKind::Sub | SymKind::Method)
                || analysis.symbol_is_class_content(sym))
                && on_chain
        }
        TargetKind::Package => matches!(
            sym.kind,
            SymKind::Package | SymKind::Class | SymKind::Module
        ),
        TargetKind::HashKeyOfSub { package, name } => matches!(
            &sym.detail,
            SymbolDetail::HashKeyDef {
                owner: HashKeyOwner::Sub { package: op, name: on },
                ..
            } if op == package && on == name
        ),
        TargetKind::HashKeyOfBridged(wanted) => matches!(
            &sym.detail,
            SymbolDetail::HashKeyDef { owner: HashKeyOwner::Bridged { class: n }, .. } if n == wanted
        ),
        // The slot's def is the group decl (the Method/HashKeyDef pair
        // already collect it) — internal-key members contribute access
        // sites only, no decl matching here.
        TargetKind::InternalHashKey { .. } => false,
        TargetKind::Handler { owner, name: hname } => {
            sym.name == *hname
                && matches!(
                    &sym.detail,
                    SymbolDetail::Handler { owner: o, .. } if o == owner
                )
        }
        // Every `#define` of the name is a declaration (config variants in
        // different headers all surface, matching the forward macro lane's
        // never-prune rule — a `#define`'s symbol can be Variable, Sub, or a
        // member-block role's Class), as is a file-scope global's def.
        TargetKind::FileScopeValue => {
            analysis.names_macro_def(&sym.name, Some(sym.selection_span))
                || analysis.symbol_is_file_scope_value(sym)
        }
    }
}

/// Pick the role mask for a *references* query: scope to editable space
/// (OPEN + WORKSPACE) when the target is declared in a file we can edit,
/// else widen to VISIBLE so refs into a dependency-defined symbol still
/// surface. "Find references" on a project symbol must not scan CPAN —
/// see the file-store ADR's RoleMask discipline.
pub fn references_mask_for(
    files: &FileStore,
    module_index: Option<&dyn CrossFileLookup>,
    target: &TargetRef,
) -> RoleMask {
    let mut found_in_editable = false;
    files.for_each_open_mut(|_url, doc| {
        if doc.analysis.symbols.iter().any(|s| symbol_defines_target(s, target, &doc.analysis)) {
            found_in_editable = true;
        }
    });
    if !found_in_editable {
        for entry in files.workspace_raw().iter() {
            if entry.value().symbols.iter().any(|s| symbol_defines_target(s, target, entry.value())) {
                found_in_editable = true;
                break;
            }
        }
    }
    // A class-keyed Method target whose decl we can't see in editable
    // space (cross-file synthesized accessor, parent in @INC) still wins
    // EDITABLE if the *class* is a workspace package — the callers we
    // care about are project files. Fall back to the module index only
    // when nothing project-side claims it.
    if !found_in_editable {
        if let (TargetKind::Method { class }, Some(_idx)) = (&target.kind, module_index) {
            let mut declared_in_workspace = false;
            for entry in files.workspace_raw().iter() {
                if entry.value().symbols.iter().any(|s| {
                    matches!(s.kind, SymKind::Package | SymKind::Class)
                        && s.name == *class
                }) {
                    declared_in_workspace = true;
                    break;
                }
            }
            if declared_in_workspace {
                found_in_editable = true;
            }
        }
    }
    if found_in_editable {
        RoleMask::EDITABLE
    } else {
        RoleMask::VISIBLE
    }
}

/// Collect the rename/reference locations for an `our` package global in one
/// file: the `our` decl, every qualified `$Pkg::var` access (its span is
/// already the bare tail), and the file's unqualified reads that resolve to the
/// decl. Decl + unqualified spans carry the sigil, so they're narrowed past it
/// — the qualifier/sigil survives, only the name token is rewritten.
fn collect_package_var(
    key: &FileKey,
    analysis: &FileAnalysis,
    package: &str,
    name: &str,
    out: &mut Vec<RefLocation>,
) {
    use crate::file_analysis::{DeclKind, RefKind, SymbolDetail};
    // Rewrite only the trailing name token, anchored at the span *end* so the
    // sigil and any `Pkg::` qualifier survive — regardless of whether the ref
    // span is the whole `$Pkg::name` (container/element/slice reads span
    // sigil+qualifier+name) or already the bare tail (scalar reads, which the
    // builder pre-narrows). Byte math: sigils are 1 byte, columns are bytes.
    let sigil_len = name.chars().next().map_or(0, char::len_utf8);
    let base_len = name.len() - sigil_len;
    let tail = |s: Span| Span {
        start: tree_sitter::Point::new(s.end.row, s.end.column.saturating_sub(base_len)),
        end: s.end,
    };
    // `$::x` / `$main::x` / a `main`-package `our $x` all name the same global;
    // `qualified_var_target` yields an empty package for the leading-`::`
    // spelling, so normalize it to the `main` the decl carries.
    fn norm(p: &str) -> &str {
        if p.is_empty() { "main" } else { p }
    }
    let is_our_decl = |id: crate::file_analysis::SymbolId| {
        let s = analysis.symbol(id);
        matches!(&s.detail, SymbolDetail::Variable { decl_kind: DeclKind::Our, .. })
            && s.package.as_deref() == Some(package)
            && s.name == name
    };
    for sym in &analysis.symbols {
        if matches!(&sym.detail, SymbolDetail::Variable { decl_kind: DeclKind::Our, .. })
            && sym.package.as_deref() == Some(package)
            && sym.name == name
        {
            out.push(RefLocation {
                key: key.clone(),
                span: tail(sym.selection_span),
                access: AccessKind::Declaration,
                rewritable: true,
            });
        }
    }
    for r in &analysis.refs {
        if !matches!(r.kind, RefKind::Variable | RefKind::ContainerAccess) {
            continue;
        }
        if let Some((qpkg, qname)) = r.qualified_var_target() {
            // Qualified `$Pkg::var` (the sigil is canonicalized to the declared
            // one, so `@arr` element reads `$Pkg::arr[0]` still match `@arr`).
            if norm(qpkg) == package && qname == name {
                out.push(RefLocation {
                    key: key.clone(),
                    span: tail(r.span),
                    access: r.access,
                    rewritable: true,
                });
            }
        } else if r.target_name == name && r.resolves_to.is_some_and(is_our_decl) {
            // Unqualified — only this package's `our` var (resolved in-file).
            out.push(RefLocation {
                key: key.clone(),
                span: tail(r.span),
                access: r.access,
                rewritable: true,
            });
        }
    }
}

fn collect_from_analysis(
    key: &FileKey,
    analysis: &FileAnalysis,
    target: &TargetRef,
    aliases: &[String],
    module_index: Option<&dyn CrossFileLookup>,
    out: &mut Vec<RefLocation>,
) {
    use crate::file_analysis::HashKeyOwner;

    // Pack languages: name lookups during matching (invocant typing, the
    // typedef chase) must resolve against THIS file's include closure — the
    // same visibility goto-def uses at this file's cursors — or a scanned
    // file's `o->op_type` types against a globally-arbitrary same-named
    // candidate and the site silently drops out. Transparent for Perl
    // (empty closure = the plain index).
    let scoped_storage: Option<crate::file_analysis::ScopedLookup>;
    let module_index: Option<&dyn CrossFileLookup> = match module_index {
        Some(idx) if !analysis.include_closure.is_empty() => {
            let path = key_for_sort(key);
            scoped_storage = Some(crate::file_analysis::ScopedLookup::new(
                idx,
                &analysis.include_closure,
                Some(path.as_path()),
            ));
            Some(scoped_storage.as_ref().unwrap() as &dyn CrossFileLookup)
        }
        other => other,
    };

    // Package globals match by package + (qualified) name, not the callable
    // scope machinery below — and their spans need sigil handling — so collect
    // them on a dedicated path.
    if let TargetKind::PackageVar { package } = &target.kind {
        collect_package_var(key, analysis, package, &target.name, out);
        return;
    }

    // `name` is constant across all refs in this call (it is `target.name`), so
    // the only varying key is the invocant class. Cache chains keyed by class to
    // avoid an O(refs × ancestor_depth) DFS on large files with many same-method
    // calls against the same class.
    let mut rename_chain_cache: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    // A callable/handler name can be FOLDED from another identifier: a variable
    // (`$obj->on($evt)`, `$self->$m()`) or, for handlers, a constant
    // (`on(EVT)`). The folded site is a *reference* to that variable/constant,
    // not a literal name token — rename must skip it (references still list it),
    // or it rewrites the variable/constant and corrupts the dispatch. A
    // `FunctionCall` coincidence is a const-fold for a handler but a Sub's OWN
    // call site otherwise, so only handlers fold through calls. Other kinds
    // (Variable/Package/HashKey) have literal-name spans — always rewritable.
    let (foldable, folds_through_calls) = match target.kind {
        TargetKind::Handler { .. } => (true, true),
        TargetKind::Sub { .. } | TargetKind::Method { .. } => (true, false),
        _ => (false, false),
    };
    let rewritable_at =
        |span: Span| !(foldable && span_is_folded_name(analysis, span, folds_through_calls));

    // Include declaration spans when this file defines the target.
    for sym in &analysis.symbols {
        if symbol_defines_target(sym, target, analysis) {
            out.push(RefLocation {
                key: key.clone(),
                span: sym.selection_span,
                access: AccessKind::Declaration,
                rewritable: rewritable_at(sym.selection_span),
            });
        }
    }

    // Collect usage refs.
    let callable_scope_for_refs: Option<Option<String>> = match &target.kind {
        TargetKind::Sub { package } => Some(package.clone()),
        TargetKind::Method { class } => Some(Some(class.clone())),
        _ => None,
    };
    for r in &analysis.refs {
        // A qualified call (`Foo::baz()` / `$o->Foo::Bar::baz()`) keeps its
        // whole path in `target_name`; match it on the bare callable tail (the
        // dispatch-class checks in the call arms below still pin the right
        // package/class). Every other ref kind matches by exact name.
        let name_matches = if matches!(r.kind, RefKind::FunctionCall { .. } | RefKind::MethodCall { .. }) {
            r.unqualified_target_name() == target.name
        } else {
            r.target_name == target.name
        };
        // A use spelled through a delegating macro (`IncRef(x)` where
        // `#define IncRef(sv) Perl_Inc(sv)`, or the object-like alias
        // `#define op_prune_chain_head Perl_op_prune_chain_head`) IS a use of
        // the target — the backward see-through. Call-shaped when the use was
        // left unexpanded, a bare Variable read when the expansion erased it
        // (the re-minted read carries the alias's name). Bypasses the package
        // gates below (the delegation edge already pinned the identity) and
        // is never rewritable (the token spells the MACRO's name).
        let alias_matched = !name_matches
            && matches!(
                r.kind,
                RefKind::FunctionCall { .. } | RefKind::Variable | RefKind::PackageRef
            )
            && aliases.iter().any(|a| a == r.unqualified_target_name());
        if !name_matches && !alias_matched {
            continue;
        }
        // Sub + Method both match any call into that scope — function
        // or method shape — per the "same callable, two shapes"
        // invariant. Filter is a single scope comparison.
        let matches_kind = alias_matched || match (&target.kind, &r.kind) {
            (TargetKind::Sub { .. } | TargetKind::Method { .. },
             RefKind::FunctionCall { resolved_package }) => {
                let scope = callable_scope_for_refs.as_ref().unwrap();
                // Under Hierarchy a bare call into ANY family class matches (the
                // whole override family); Dispatch keeps the strict single
                // scope. A bare imported call the single-file walk couldn't pin
                // (`use Bank;` auto-imports `@EXPORT`, invisible at build) has
                // `resolved_package: None` — re-derive it here, where the index
                // is in hand.
                let pkg_matches = |pkg: &Option<String>| {
                    pkg == scope
                        || (target.scope == OverrideScope::Hierarchy
                            && target.method_classes.iter().any(|c| Some(c) == pkg.as_ref()))
                };
                match resolved_package {
                    Some(_) => pkg_matches(resolved_package),
                    None => pkg_matches(&analysis.deferred_call_package(r, module_index)),
                }
            }
            (TargetKind::Sub { .. } | TargetKind::Method { .. },
             RefKind::MethodCall { .. }) => {
                // Prefer the build-time-frozen dispatch edge
                // (`resolved_method_target`) so a call that resolved at build
                // time stays matched regardless of query-time inference. An
                // absent edge means build-time lacked cross-file info (SUPER
                // into a cross-file parent; enrichment re-stamps OPEN docs
                // only) — re-resolve lazily here, where the index is in hand,
                // rather than silently excluding the site. Either way the
                // class then fans out over `method_rename_chain` so
                // `$child->m` matches an ancestor-defined target while
                // unrelated same-named methods stay out.
                let scope = callable_scope_for_refs.as_ref().unwrap();
                let method = r.unqualified_target_name();
                {
                    let resolved_class = match r.resolved_method_target.as_ref() {
                        Some(edge) => Some(edge.invocant_class().to_string()),
                        None => analysis.method_call_invocant_class(r, module_index),
                    };
                    match (resolved_class, scope) {
                        (Some(cn), Some(pkg)) => {
                            if target.scope == OverrideScope::Hierarchy {
                                // The override family is precomputed; a call
                                // matches iff its invocant is in it — so
                                // `$child->m` and `$base->m` rename together.
                                // (Every family member is a descendant of the
                                // root, so inheriting-without-override calls are
                                // covered by membership.) The family walk runs
                                // INVERSE edges from the origin file, which can
                                // miss aliases declared elsewhere (perl5's
                                // `typedef struct op OP` lives in perl.h, so
                                // `OP` isn't in `op`'s computed family) — the
                                // UPWARD chain from the invocant's class needs
                                // no inverse index, so admit a class whose
                                // chain reaches the family.
                                target.method_classes.iter().any(|c| c == &cn)
                                    || rename_chain_cache
                                        .entry(cn.clone())
                                        .or_insert_with(|| {
                                            analysis.method_rename_chain(&cn, method, module_index)
                                        })
                                        .iter()
                                        .any(|c| target.method_classes.iter().any(|f| f == c))
                            } else {
                                // Dispatch: the call matches only if it
                                // dispatches to THIS def — `$child->m` reaches an
                                // ancestor target via the per-invocant chain,
                                // unrelated same-named methods stay out.
                                cn == *pkg || rename_chain_cache
                                    .entry(cn.clone())
                                    .or_insert_with(|| {
                                        analysis.method_rename_chain(&cn, method, module_index)
                                    })
                                    .iter()
                                    .any(|c| c == pkg)
                            }
                        }
                        _ => false,
                    }
                }
            }
            (TargetKind::Package, RefKind::PackageRef) => true,
            // A pack-language enum constant / member read by BARE name
            // (`x = OP_SCOPE`, `case OP_SCOPE:`) — a `Variable` ref the
            // generic goto-def resolves to this def by name (the value-read
            // half of the shared Variable/Field DEF). `name_matches` already
            // pinned the name; an UNRESOLVED read is the cross-file case the
            // by-name goto-def owns, and a resolved read counts only when it
            // binds the target's own class content (a genuinely-local
            // variable — even one carrying the class as sticky package —
            // stays out via the structural gate).
            (TargetKind::Method { class }, RefKind::Variable) => match r.resolves_to {
                None => true,
                Some(id) => {
                    let s = analysis.symbol(id);
                    analysis.symbol_is_class_content(s)
                        && (s.package.as_deref() == Some(class.as_str())
                            || target
                                .method_classes
                                .iter()
                                .any(|c| Some(c.as_str()) == s.package.as_deref()))
                }
            },
            // A file-scope value's uses, all bare-name-keyed like its forward
            // resolutions: a value read (object-like macro / global / enum
            // constant), a type-position token (a type-alias `#define` used as
            // a declared type), or an unresolved call (function-like macro —
            // a package-pinned call belongs to that package's sub, not here).
            (TargetKind::FileScopeValue, RefKind::Variable) => match r.resolves_to {
                None => true,
                Some(id) => {
                    let s = analysis.symbol(id);
                    analysis.names_macro_def(&s.name, Some(s.selection_span))
                        || analysis.symbol_is_file_scope_value(s)
                }
            },
            (TargetKind::FileScopeValue, RefKind::PackageRef) => true,
            (TargetKind::FileScopeValue, RefKind::FunctionCall { resolved_package }) => {
                resolved_package.is_none()
            }
            (
                TargetKind::HashKeyOfSub { package, name },
                RefKind::HashKeyAccess { owner, .. },
            ) => {
                // The owning-sub match, widened across inheritance for
                // CONSTRUCTOR keys: a base attr's ctor key
                // (`HashKeyOfSub{Animal, new}`) is also keyed by a SUBCLASS
                // construction (`Dog->new(name => …)`, owner `Sub{Dog, new}`),
                // since `name` is the inherited attr. So renaming a base attr
                // reaches child constructions.
                let sub_matches = |op: &Option<String>, on: &str| -> bool {
                    if on != name.as_str() {
                        return false;
                    }
                    op == package
                        || (crate::conventions::is_constructor_name(on)
                            && match (op.as_deref(), package.as_deref()) {
                                (Some(child), Some(base)) => {
                                    analysis.class_isa(child, base, module_index)
                                }
                                _ => false,
                            })
                };
                match owner {
                    Some(HashKeyOwner::Sub { package: op, name: on }) => sub_matches(op, on),
                    // owner `None` (build gate blind) OR `Variable` (the var is
                    // bound to an imported call enrichment didn't reach in this
                    // unenriched workspace file) — re-derive cross-file, the same
                    // lazy seam method dispatch + deferred owners use above. This
                    // is what makes a producer-origin rename reach the consumer's
                    // `$c->{key}` access without depending on open-doc enrichment.
                    _ => analysis
                        .deferred_hash_key_owner(r, module_index)
                        .is_some_and(|o| {
                            matches!(o, HashKeyOwner::Sub { package: op, name: on } if sub_matches(&op, &on))
                        }),
                }
            },
            (TargetKind::HashKeyOfBridged(wanted), RefKind::HashKeyAccess { owner, .. }) => {
                // A DBIC/Class::Accessor column. Its key uses are the
                // condition args (`$rs->search({ col => … })`), owned by the
                // `Column` namespace — NOT `$row->{col}` derefs, which carry a
                // `Class` lookup and so never match here (a column isn't a hash
                // slot). The owner-`None` case is the cross-file deferred arg key.
                let target_owner = HashKeyOwner::Bridged { class: wanted.clone() };
                match owner {
                    Some(o) => o.found_by(&target_owner),
                    None => analysis
                        .deferred_hash_key_owner(r, module_index)
                        .is_some_and(|o| o.found_by(&target_owner)),
                }
            }
            (TargetKind::InternalHashKey { class },
             RefKind::HashKeyAccess { owner, .. }) => {
                // STRICT Class-owner shape (see the kind's doc), widened
                // only by ancestry: a subclass poking `$self->{attr}` owns
                // the access as ITS class — `Gadget isa Widget` ties it to
                // Widget's attr. Never `found_by` (Sub-owned arg keys stay
                // out).
                matches!(
                    owner,
                    Some(HashKeyOwner::Class(c))
                        if c == class || analysis.class_isa(c, class, module_index)
                )
            }
            (TargetKind::Handler { owner, name: hname },
             RefKind::DispatchCall { owner: ref_owner, .. }) => {
                r.target_name == *hname
                    && matches!(ref_owner, Some(o) if o == owner)
            }
            _ => false,
        };
        if matches_kind {
            // MethodCall r.span covers the whole call expression; callers
            // (rename, highlight) want just the method-name token so they
            // can replace or underline exactly the right characters.
            let span = if let RefKind::MethodCall { method_name_span, .. } = &r.kind {
                *method_name_span
            } else {
                r.span
            };
            out.push(RefLocation {
                key: key.clone(),
                span,
                access: r.access,
                rewritable: !alias_matched && rewritable_at(span),
            });
            // A call folded from a variable (`my $m = 'process'; $self->$m()`)
            // has a non-rewritable name token above; the rewrite belongs on the
            // source string literal the fold came from (rule #9).
            if let Some(src) = r.folded_from {
                out.push(RefLocation {
                    key: key.clone(),
                    span: src,
                    access: r.access,
                    rewritable: rewritable_at(src),
                });
            }
        }
    }

    // Query-time dispatch resolution: gated candidates (which ride the cache
    // ungated, even in non-open workspace/dependency files) resolve their
    // receiver isa-check NOW against the module index. The `Applies` ones are
    // handler call-sites that enrichment-eager promotion would have missed in
    // any file that's never enriched. `applicable_dispatches` skips sites the
    // emit-hook path already materialized above, so no double-count.
    // See `docs/adr/receiver-gated-dispatch.md`.
    if let TargetKind::Handler { owner, name: hname } = &target.kind {
        for applied in analysis.applicable_dispatches(module_index) {
            if &applied.name == hname && &applied.owner == owner {
                out.push(RefLocation {
                    key: key.clone(),
                    span: applied.span,
                    access: AccessKind::Read,
                    rewritable: rewritable_at(applied.span),
                });
            }
        }
    }
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
