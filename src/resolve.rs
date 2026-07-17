//! Unified query surface across FileStore + ModuleIndex.
//!
//! `resolve(cursor) → CandidateSet` is the one resolution entry point:
//! identity (`resolve_symbol_scoped`'s Target/Group/Local verdict),
//! visibility (RoleMask), edges, and per-site policy are owned by the set,
//! and every navigation verb — goto-def, references, rename, prepareRename,
//! implementations — is a projection of it. Handlers and CLI mirrors are
//! one-liners over a projection; none re-derives identity or the per-tier
//! walk inline (that's how the CLI and LSP used to disagree on hash-key
//! references, and how visibility axes used to reach one feature and miss
//! its siblings). See `docs/adr/resolution-candidate-set.md`.
//!
//! `refs_to` / `group_refs` / `references_mask_for` are the set's internals
//! (still exercised directly by tests); new axes go into CandidateSet
//! construction, never into a handler.

use std::path::PathBuf;

use tower_lsp::lsp_types::Url;

use crate::file_analysis::{
    AccessKind, CompletionCandidate, CrossFileLookup, FileAnalysis, HandlerOwner, RefKind, Span,
    SymKind,
};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OverrideScope {
    #[default]
    Hierarchy,
    Dispatch,
}

impl OverrideScope {
    /// Parse the override-scope string for the CLI (`--rename … <scope>`);
    /// anything unrecognized (or absent) is the default `Hierarchy`. The LSP
    /// path deserializes `RenameOptions` straight from JSON instead.
    pub fn from_option(s: &str) -> Self {
        match s {
            "dispatch" => OverrideScope::Dispatch,
            _ => OverrideScope::Hierarchy,
        }
    }
}

/// `initializationOptions.rename` — the rename sub-object as a serde schema (the
/// struct IS the schema: camelCase keys, absent ones default). Mirrors
/// `symbols::DiagnosticOptions`; a malformed value leaves the defaults in place.
#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RenameOptions {
    pub override_scope: OverrideScope,
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
    /// Whether the target's defining symbol is an enum-constant shape — a
    /// name C hoists into the enclosing scope, so BARE unresolved reads
    /// (`case OP_SCOPE:`) are legitimate uses. False for receiver-reached
    /// members (fields/methods) and every Perl target: bare same-named
    /// tokens elsewhere are noise, not references (the `formatter::format`
    /// 1621-hit sweep). Minted from the defining symbol
    /// (`class_content_is_bare_constant`); the matcher may also re-derive it
    /// per scanned file when the index is in hand.
    pub bare_constant: bool,
    /// Pack-language visibility identity: the canonical paths of the files
    /// that define this target AS THE ORIGIN FILE SEES IT (the origin itself,
    /// candidates in its include closure, and candidates whose closure reaches
    /// back to the origin — the header-decl ↔ TU-def link). The backward match
    /// side (`collect_from_analysis`) accepts a name match in a scanned file
    /// only when that file can see one of these — the same include-closure
    /// visibility forward resolution (`ScopedLookup`) applies, so gd and gr
    /// stay mirrored on the SAME key: name + visibility. Empty = no gate
    /// (every Perl target: Perl identity is package/sigil, not closure).
    pub def_paths: Vec<String>,
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
            def_paths: Vec::new(),
            bare_constant: false,
        }
    }

    /// Build a `Method` target for a class-OWNED synthesized accessor (a Moo
    /// `has` reader, a DBIC column/relationship accessor). Its override family
    /// is `owned_accessor_family` — the owning class and its descendants only,
    /// NEVER a framework ancestor that happens to define a real `sub` of the
    /// same name (`DBIx::Class::PK::id`). Renaming a synthesized `id` column
    /// must not reach that generic accessor nor every unrelated sibling Result
    /// class under it. Fixed `Hierarchy` scope: an owned accessor is
    /// shared down the hierarchy by construction; the family already encodes
    /// exactly the classes that inherit it.
    pub fn owned_accessor(
        name: String,
        class: String,
        origin: &FileAnalysis,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Self {
        let method_classes = origin.owned_accessor_family(&class, module_index);
        TargetRef {
            name,
            kind: TargetKind::Method { class },
            method_classes,
            scope: OverrideScope::Hierarchy,
            def_paths: Vec::new(),
            bare_constant: false,
        }
    }

    /// Build a non-Method target (no inheritance fan-out for declarations).
    pub fn new(name: String, kind: TargetKind) -> Self {
        debug_assert!(
            !matches!(kind, TargetKind::Method { .. }),
            "use TargetRef::method so the rename chain is populated"
        );
        TargetRef {
            name,
            kind,
            method_classes: Vec::new(),
            scope: OverrideScope::default(),
            def_paths: Vec::new(),
            bare_constant: false,
        }
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
                | TargetKind::FileScopeValue
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
                // Function targets keep empty def_paths HERE: a Sub cursor
                // is language-neutral (Perl subs mint the same RenameKind)
                // and Perl visibility is package-keyed, never closure-gated.
                // The pack instance of the gate is minted at the set level
                // (`CandidateSet::resolution`), on the caller-declared pack
                // routing fact. Macro-named cursors never reach this arm
                // (the canonical FileScopeValue lanes claim them first,
                // WITH def_paths).
                TargetRef {
                    name,
                    kind: TargetKind::Sub { package },
                    method_classes,
                    scope,
                    def_paths: Vec::new(),
                    bare_constant: false,
                }
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
                    .and_then(|cc| idx.whole_present(&cc).field_projections_named(bare, c))
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
    let whole = idx.whole_present(&cached);
    let p = whole.field_projections_named(bare, &defining)?;
    if !ok(&p) {
        return None;
    }
    Some(group_from_projections(p, &whole, Some(cached.path.clone()), Some(idx)))
}

/// Cursor → cross-file target with the default override scope. Production
/// callers go through `resolve()`/`CandidateSet` (which forces identity via
/// `resolve_symbol_scoped`); this wrapper serves tests probing identity
/// directly.
#[cfg_attr(not(test), allow(dead_code))]
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
            let mut t = TargetRef::new(sym.name.clone(), TargetKind::FileScopeValue);
            t.def_paths = pack_def_paths(&sym.name, true, module_index);
            return Some(ResolvedTarget::Target(t));
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
            // The class tag normally rides class-content symbols by construction;
            // a malformed/adversarial FileAnalysis without it has no target to
            // mint here, so fall through to the remaining lanes rather than assert.
            if let Some(class) = sym.package.clone() {
                let mut t = TargetRef::method(
                    sym.name.clone(),
                    class,
                    analysis,
                    module_index,
                    scope,
                );
                t.def_paths = pack_class_def_paths(&t, analysis, module_index);
                t.bare_constant = analysis.class_content_is_bare_constant(sym);
                return Some(ResolvedTarget::Target(t));
            }
        }
        // A file-scope global / anonymous-enum constant: bare-name-keyed,
        // like the generic cross-file goto-def tail that resolves its uses.
        if analysis.symbol_is_file_scope_value(sym) {
            let mut t = TargetRef::new(sym.name.clone(), TargetKind::FileScopeValue);
            t.def_paths = pack_def_paths(&sym.name, true, module_index);
            return Some(ResolvedTarget::Target(t));
        }
        // An unexpanded function-like macro use in DECLARATION position
        // parses as a Sub/Method decl (`int x ABSL_GUARDED_BY(mu);` — the
        // attribute macro reads as a function declarator) or a Variable decl
        // (`string_view s ABSL_ATTRIBUTE_LIFETIME_BOUND` — a phantom second
        // parameter). The token IS the macro: mint the same canonical
        // `FileScopeValue` identity the `#define` site and the erased-use
        // re-mints carry, so gr agrees from any spelling (the two-lane split
        // was the gr-undercount root). Class content and file-scope values
        // claimed above, so a Variable reaching here is the artifact shape.
        if matches!(sym.kind, SymKind::Sub | SymKind::Method | SymKind::Variable)
            && names_visible_macro(&sym.name, analysis, module_index)
        {
            let mut t = TargetRef::new(sym.name.clone(), TargetKind::FileScopeValue);
            t.def_paths = pack_def_paths(
                &sym.name,
                analysis.names_macro_def(&sym.name, None),
                module_index,
            );
            return Some(ResolvedTarget::Target(t));
        }
    }
    // The same lanes from a USE site, so gr from a use equals gr from the
    // def: a bare read / type token that resolves (locally or by name
    // cross-file) to a macro, class content, or file-scope value mints the
    // identical target the def site does.
    if let Some(r) = analysis.ref_at(point) {
        // A left-unexpanded function-like macro CALL (`ABSL_PREDICT_TRUE(x)`)
        // is call-shaped, never a per-package Sub — same canonical macro
        // identity as the def site.
        if matches!(r.kind, RefKind::FunctionCall { .. }) {
            let name = r.unqualified_target_name();
            if names_visible_macro(name, analysis, module_index) {
                let mut t = TargetRef::new(name.to_string(), TargetKind::FileScopeValue);
                t.def_paths =
                    pack_def_paths(name, analysis.names_macro_def(name, None), module_index);
                return Some(ResolvedTarget::Target(t));
            }
        }
        if matches!(r.kind, RefKind::Variable | RefKind::PackageRef) {
            let class_or_value = |a: &FileAnalysis, s: &crate::file_analysis::Symbol| {
                if a.names_macro_def(&s.name, Some(s.selection_span))
                    || a.symbol_is_file_scope_value(s)
                {
                    Some(None)
                } else if a.symbol_is_class_content(s) {
                    s.package
                        .clone()
                        .map(|p| Some((p, a.class_content_is_bare_constant(s))))
                } else {
                    None
                }
            };
            let resolved: Option<Option<(String, bool)>> = match r.resolves_to {
                // A read that resolved to an unexpanded-use artifact (the
                // phantom Variable a decl-position macro mints) still keys
                // the macro identity — the class-content/file-scope shapes
                // claim first.
                Some(id) => class_or_value(analysis, analysis.symbol(id)).or_else(|| {
                    names_visible_macro(&r.target_name, analysis, module_index).then_some(None)
                }),
                // Any-candidate macro check (not the one-winner `get_cached`
                // view): a macro whose name loses the name tie to a same-named
                // function's file still keys the macro identity.
                None if names_visible_macro(&r.target_name, analysis, module_index) => {
                    Some(None)
                }
                None => module_index.and_then(|idx| {
                    let cached = idx.get_cached(&r.target_name)?;
                    // Whole view: the class-content / file-scope shape tests
                    // walk symbols, which the resident copy may have evicted.
                    let whole = idx.whole_present(&cached);
                    whole
                        .symbols
                        .iter()
                        .filter(|s| s.name == r.target_name)
                        .find_map(|s| class_or_value(&whole, s))
                }),
            };
            match resolved {
                Some(Some((class, bare))) => {
                    let mut t = TargetRef::method(
                        r.target_name.clone(),
                        class,
                        analysis,
                        module_index,
                        scope,
                    );
                    t.def_paths = pack_class_def_paths(&t, analysis, module_index);
                    t.bare_constant = bare;
                    return Some(ResolvedTarget::Target(t));
                }
                Some(None) => {
                    let mut t =
                        TargetRef::new(r.target_name.clone(), TargetKind::FileScopeValue);
                    let origin_defines = analysis.names_macro_def(&r.target_name, None)
                        || r.resolves_to.is_some();
                    t.def_paths =
                        pack_def_paths(&r.target_name, origin_defines, module_index);
                    return Some(ResolvedTarget::Target(t));
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
        kind => {
            // A kind that doesn't map to a target (malformed input reaching a
            // rename kind the constructor doesn't cover) has no resolution here.
            let Some(mut t) = TargetRef::from_rename_kind(kind, analysis, module_index, scope)
            else {
                return None;
            };
            // A member-ACCESS cursor (`c->fd`) reaches here as a generic
            // Method kind; when the member is pack class content the target
            // is the same one its DEF site mints, so it carries the same
            // visibility identity. Perl methods are Sub/Method symbols —
            // never class content — and keep empty `def_paths` (no gate).
            if let TargetKind::Method { class } = &t.kind {
                if let Some(bare) = pack_member_of_class(&t.name, class, analysis, module_index) {
                    t.def_paths = pack_class_def_paths(&t, analysis, module_index);
                    t.bare_constant = bare;
                }
            }
            ResolvedTarget::Target(t)
        }
    })
}

/// Is `name` a pack-language class-content member (struct field, member-block
/// role member, enum constant) of `class` — in the origin file or the class's
/// module as the origin sees it? `Some(bare)` when it is, where `bare` is the
/// enum-constant verdict (`class_content_is_bare_constant`): whether bare
/// unresolved reads of the name count as uses. `None` keeps the pack
/// visibility gate off Perl Method targets minted from the same cursor kinds.
fn pack_member_of_class(
    name: &str,
    class: &str,
    origin: &FileAnalysis,
    idx: Option<&dyn CrossFileLookup>,
) -> Option<bool> {
    let check = |a: &FileAnalysis| {
        a.symbols
            .iter()
            .find(|s| {
                s.name == name
                    && s.package.as_deref() == Some(class)
                    && a.symbol_is_class_content(s)
            })
            .map(|s| a.class_content_is_bare_constant(s))
    };
    check(origin).or_else(|| {
        idx.and_then(|i| i.get_cached(class))
            .and_then(|c| check(&c.analysis))
    })
}

/// The canonical answer to "what does this name mean, from here" — and the
/// one object every navigation feature projects from. Identity (what the
/// cursor resolves to), visibility (which file roles a walk may see), edges
/// (override families / groups / descendants), and per-site policy
/// (`rewritable`, per-member rename texts) are all owned here; goto-def,
/// references, rename, and implementations are projections of the same set,
/// so an axis added to construction is inherited by every feature at once.
/// See `docs/adr/resolution-candidate-set.md`.
///
/// Borrow discipline: the set only ever READS the stores (projections walk
/// via `FileStore::for_each_open`), so an LSP handler may hold its open-doc
/// read guard for the set's whole lifetime.
pub struct CandidateSet<'a> {
    files: &'a FileStore,
    origin: &'a FileAnalysis,
    origin_key: FileKey,
    point: tree_sitter::Point,
    /// The routed base index (the Perl hub, or a per-language pack
    /// sub-index). Backward walks (`refs_to`, group walks) take THIS —
    /// `collect_from_analysis` re-scopes per scanned file.
    module_index: Option<&'a dyn CrossFileLookup>,
    /// The origin's include-closure scope over `module_index`, built once at
    /// construction — the per-origin visibility rule every forward
    /// resolution (identity minting, goto-def, implementations) reads, so no
    /// entry point re-applies the `ScopedLookup` decorator (arc-review C1's
    /// root shape). Transparent for Perl (empty closure).
    scoped: Option<crate::file_analysis::ScopedLookup<'a>>,
    /// Routed through a per-language pack sub-index (the caller's routing
    /// fact). Two policy consequences, applied at the set level so every
    /// projection agrees: visibility widens to VISIBLE (pack workspace
    /// files ride the DEPENDENCY role — a storage artifact of the
    /// per-language cache, which registers only workspace-walk files), and
    /// rename REFUSES on alias-spelled sites instead of silently skipping.
    pack: bool,
    /// The origin document's raw text, when the caller has it. Feeds the
    /// raw-word candidate lanes (macro variants): a macro use can vanish
    /// from the reparsed analysis (expand-and-reparse), so the byte-level
    /// word is the reliable key. `None` = those lanes stay silent.
    source: Option<&'a str>,
    scope: OverrideScope,
    /// Identity, minted once via `resolve_symbol_scoped` — lazily, so a
    /// projection that never consults it (goto-def's forward path) doesn't
    /// pay the override-family walk. `None` = nothing cross-file-resolvable
    /// at the cursor; local projections still answer from `origin`.
    resolution: std::sync::OnceLock<Option<ResolvedTarget>>,
    /// Visibility for a `Target` resolution, memoized — computed by
    /// `references_mask_for` on first use (group members keep their
    /// per-member masks inside the group projections).
    visibility: std::sync::OnceLock<RoleMask>,
    /// Construction-time visibility override: when set, EVERY projection
    /// (references, rename, group walks) scopes to it — the seam future
    /// axes (closure visibility, language boundaries) plug into.
    visibility_override: Option<RoleMask>,
}

/// Cursor → CandidateSet: the single resolution entry point. Handlers and
/// CLI mirrors construct the set once and project; none of them re-derive
/// identity, visibility, or per-site policy on their own.
pub fn resolve<'a>(
    files: &'a FileStore,
    origin: &'a FileAnalysis,
    origin_key: FileKey,
    point: tree_sitter::Point,
    module_index: Option<&'a dyn CrossFileLookup>,
    scope: OverrideScope,
) -> CandidateSet<'a> {
    // The per-origin closure scope is a construction fact: forward
    // resolutions see the names THIS file's preprocessor would (C's flat
    // linkage), and Perl origins pass through untouched (empty closure).
    let self_path = match &origin_key {
        FileKey::Path(p) => Some(p.clone()),
        FileKey::Url(u) => u.to_file_path().ok(),
    };
    let scoped = module_index.map(|idx| {
        crate::file_analysis::ScopedLookup::new(
            idx,
            &origin.include_closure,
            self_path.as_deref(),
        )
    });
    CandidateSet {
        files,
        origin,
        origin_key,
        point,
        module_index,
        scoped,
        pack: false,
        source: None,
        scope,
        resolution: std::sync::OnceLock::new(),
        visibility: std::sync::OnceLock::new(),
        visibility_override: None,
    }
}

impl<'a> CandidateSet<'a> {
    /// Constrain every projection to `mask`. The one knob demonstrating the
    /// symmetry invariant: narrowing visibility here narrows references AND
    /// rename AND group walks together — no per-feature re-application. The
    /// seam future construction axes (closure visibility, language
    /// boundaries) ride; exercised by the invariant test and by
    /// `--heatmap`'s `--include-deps` scope knob.
    pub fn with_visibility(mut self, mask: RoleMask) -> Self {
        self.visibility_override = Some(mask);
        self.visibility = std::sync::OnceLock::new();
        self
    }

    /// Declare the caller routed this origin through a per-language pack
    /// sub-index. A routing fact, like which store — the policy consequences
    /// (VISIBLE-wide walks, rename's full-or-refuse) live on the set.
    pub fn pack_routed(mut self) -> Self {
        self.pack = true;
        self
    }

    /// Supply the origin document's raw text — unlocks the raw-word
    /// candidate lanes (macro variants in `definitions()`).
    pub fn with_source(mut self, source: &'a str) -> Self {
        self.source = Some(source);
        self
    }

    /// Per-language name semantics on the set's identity keying: normalize
    /// a typed NEW NAME to the bare identity token edits write. Perl names
    /// carry sigils (`conventions.rs` owns the rule); pack languages
    /// canonicalize spellings at extraction (the LangPack `shape_name`
    /// hook — cpp's `canonical_template_spelling` is that seam's cpp
    /// instance), so their typed names pass through bare. New per-language
    /// spelling rules plug in HERE, never inline in a projection.
    fn bare_new_name<'n>(&self, typed: &'n str) -> &'n str {
        if self.pack {
            typed
        } else {
            crate::conventions::strip_variable_sigils(typed)
        }
    }

    /// The origin-scoped index — every forward resolution (identity,
    /// goto-def, implementations) reads through the closure scope built at
    /// construction. Backward walks take `self.module_index` (the base):
    /// `collect_from_analysis` re-scopes per scanned file.
    fn idx(&self) -> Option<&dyn CrossFileLookup> {
        self.scoped
            .as_ref()
            .map(|s| s as &dyn CrossFileLookup)
    }

    /// What the cursor resolved to. Exposed for callers that need
    /// target-level policy questions (e.g. diagnostics asking a target's
    /// kind); projections below cover the feature verbs.
    pub fn resolution(&self) -> Option<&ResolvedTarget> {
        self.resolution
            .get_or_init(|| {
                let mut r =
                    resolve_symbol_scoped(self.origin, self.point, self.idx(), self.scope);
                // Pack routing: a plain function (Sub) target's visibility
                // identity is closure-keyed like every other pack target —
                // its def_paths are minted HERE, on the routing fact the
                // caller declared, because the Sub cursor shape itself is
                // language-neutral (a Perl `sub` mints the same RenameKind)
                // and Perl visibility is package-keyed, never closure-gated.
                if self.pack {
                    if let Some(ResolvedTarget::Target(t)) = &mut r {
                        if matches!(t.kind, TargetKind::Sub { .. }) && t.def_paths.is_empty() {
                            let origin_defines = self.origin.symbols.iter().any(|s| {
                                s.name == t.name
                                    && matches!(s.kind, SymKind::Sub | SymKind::Method)
                            });
                            t.def_paths = pack_def_paths(&t.name, origin_defines, self.idx());
                        }
                    }
                }
                r
            })
            .as_ref()
    }

    /// The set-level visibility for a `Target` resolution: the override when
    /// present; VISIBLE for pack routing (pack workspace files ride the
    /// DEPENDENCY role); else `references_mask_for`'s editable-vs-visible
    /// verdict.
    fn target_visibility(&self, target: &TargetRef) -> RoleMask {
        *self.visibility.get_or_init(|| {
            self.visibility_override.unwrap_or_else(|| {
                if self.pack {
                    RoleMask::VISIBLE
                } else {
                    references_mask_for(self.files, self.module_index, target)
                }
            })
        })
    }

    /// The backward image of the set: every reference (declarations + use
    /// sites) across the visible universe. Lexical/unowned cursors answer
    /// from the origin file's in-file union.
    pub fn references(&self) -> Vec<RefLocation> {
        match self.resolution() {
            Some(ResolvedTarget::Target(t)) => {
                let mask = self.target_visibility(t);
                refs_to(self.files, self.module_index, t, mask)
            }
            Some(ResolvedTarget::Group { local_spans, pinned_spans, members }) => group_refs(
                self.files,
                self.module_index,
                &self.origin_key,
                local_spans,
                pinned_spans,
                members,
                self.visibility_override,
            ),
            Some(ResolvedTarget::Local) | None => self
                .origin
                .find_references(self.point, self.idx())
                .into_iter()
                .map(|span| RefLocation {
                    key: self.origin_key.clone(),
                    span,
                    access: AccessKind::Read,
                    rewritable: true,
                    label: None
                })
                .collect(),
        }
    }

    /// Whether rename at this cursor would produce edits — the prepareRename
    /// gate. Mirrors `rename_edits`' arms so the box is offered exactly where
    /// edits exist. Pack targets probe the real edit set: a set rename would
    /// refuse (alias-spelled sites) or no-op on must not offer a box.
    pub fn renameable(&self) -> bool {
        match self.resolution() {
            Some(ResolvedTarget::Target(t)) if t.supports_cross_file_rename() => {
                if self.pack {
                    self.rename_edits("x").is_ok_and(|e| !e.is_empty())
                } else {
                    true
                }
            }
            Some(ResolvedTarget::Group { .. }) => true,
            Some(_) => self
                .origin
                .rename_at(self.point, "x")
                .is_some_and(|e| !e.is_empty()),
            None => false,
        }
    }

    /// Rename = the references image + rewritability policy, with each span
    /// paired to ITS replacement text (bare vs re-derived affixed accessor
    /// names for groups). Policy lives on the set/locations, not in handlers:
    /// non-rewritable sites (const-folded names) are references but never
    /// edits, and the walk stops at editable space (for pack routing,
    /// "editable" includes the per-language cache — see `pack_routed`).
    /// `Ok(empty)` = nothing renameable here; `Err` = a rename that would
    /// SILENTLY BREAK code — a pack set containing an alias-spelled site (a
    /// use through a delegating `#define`, `rewritable: false`) refuses: the
    /// macro's body isn't a collected span, so renaming the target would
    /// leave the delegation chain pointing at the old name. Perl's
    /// non-rewritable sites (variable-folded dispatch) keep their
    /// long-standing skip.
    pub fn rename_edits(&self, new_name: &str) -> Result<Vec<(RefLocation, String)>, String> {
        let editable = if self.pack {
            RoleMask::VISIBLE
        } else {
            self.visibility_override
                .map(|m| m & RoleMask::EDITABLE)
                .unwrap_or(RoleMask::EDITABLE)
        };
        Ok(match self.resolution() {
            Some(ResolvedTarget::Target(t)) if t.supports_cross_file_rename() => {
                let locations = refs_to(self.files, self.module_index, t, editable);
                if self.pack && locations.iter().any(|l| !l.rewritable) {
                    return Err(format!(
                        "rename of `{}` would leave sites spelled through a delegating macro \
                         unchanged (the macro body is not rewritten) — refusing rather than \
                         emitting a partial edit",
                        t.name
                    ));
                }
                locations
                    .into_iter()
                    .filter(|loc| loc.rewritable)
                    .map(|loc| (loc, new_name.to_string()))
                    .collect()
            }
            Some(ResolvedTarget::Group { local_spans, pinned_spans, members }) => {
                // Group spellings are bare name tokens; a sigil on the typed
                // name applies only to variable-shaped members' own rules.
                let bare_new = self.bare_new_name(new_name);
                group_rename_edits(
                    self.files,
                    self.module_index,
                    &self.origin_key,
                    local_spans,
                    pinned_spans,
                    members,
                    bare_new,
                    editable,
                )
            }
            // Lexical variables, unowned hash keys, non-cross-file targets:
            // the origin file's rename machinery owns the edit set.
            Some(_) => self
                .origin
                .rename_at(self.point, new_name)
                .unwrap_or_default()
                .into_iter()
                .map(|(span, text)| {
                    (
                        RefLocation {
                            key: self.origin_key.clone(),
                            span,
                            access: AccessKind::Read,
                            rewritable: true,
                            label: None
                        },
                        text,
                    )
                })
                .collect(),
            None => Vec::new(),
        })
    }

    /// The family/descendants walk over the set: every override/composer
    /// definition of a Method target, the specialization family of a
    /// template primary (Package targets), and — from an enum TYPE's own
    /// def — the reverse domain bridge: the field-slot sites whose recovered
    /// domain is that enum. The bridge is an implementations-style
    /// projection of the domain edge, deliberately NOT part of plain
    /// references (from an enumerator it fanned ~56 real references out to
    /// the field's ~950 sites).
    pub fn implementations(&self) -> Vec<RefLocation> {
        // Domain slot sites come off the cursor's own Class def, before
        // identity minting — the enum def resolves to a Package target whose
        // family walk is a different edge set.
        let mut out: Vec<RefLocation> = Vec::new();
        if let Some(idx) = self.module_index {
            if let Some(sym) = self.origin.symbol_at(self.point) {
                // Enums are `SymKind::Class` in cpp (no distinct kind), so gate
                // the enum→field-slot bridge on the Class actually HAVING
                // enumerators — otherwise a plain class fires it, and any field
                // member whose owning class shares the class's name resolves as
                // a bogus "enumerator of this enum" (leveldb `Iterator` matched
                // SkipList::Iterator's `node_` field). An empty/real class has
                // no enumerators → no domain sites key to it anyway, so this
                // never suppresses a genuine enum result.
                if matches!(sym.kind, SymKind::Class)
                    && !self.origin.enum_members(&sym.name, Some(idx)).is_empty()
                {
                    let enum_name = sym.name.clone();
                    idx.for_each_cached_file(&mut |cached| {
                        // `resolve_enumerator_enum`'s local arm reads the
                        // copy's own symbols — take the whole view.
                        for span in idx
                            .whole_present(cached)
                            .field_sites_for_enum(&enum_name, Some(idx))
                        {
                            out.push(RefLocation {
                                key: FileKey::Path(cached.path.clone()),
                                span,
                                access: AccessKind::Read,
                                rewritable: false,
                                label: None
                            });
                        }
                    });
                }
            }
        }
        if let Some(ResolvedTarget::Target(t)) = self.resolution() {
            out.extend(implementations_of(self.origin, self.idx(), t));
        }
        // Domain sites first (the bridge is the headline answer on an enum
        // def), then the family walk; first occurrence wins the dedup.
        let mut seen = std::collections::HashSet::new();
        out.retain(|l| seen.insert((key_for_sort(&l.key), l.span)));
        out
    }

    /// Hover projection: the top-ranked candidate of the forward walk — the
    /// SAME identity, visibility, and ranking `definitions()` computes, so
    /// hover and goto-def answer one resolution and can't disagree on what
    /// the cursor means (no hover dark where gd works, no
    /// bare-name hijack where gd is right). Presentation — markdown, kind
    /// labels, member drill-downs — is the adapter's
    /// (`symbols::pack_hover_markdown`); this returns WHAT to present.
    pub fn hover_candidate(&self) -> Option<RefLocation> {
        self.definitions().into_iter().next()
    }

    /// Read access for adapters presenting a projection (the hover renderer
    /// works from the same origin/point/scoped-index the set resolved with,
    /// so presentation lookups can't drift from resolution).
    pub fn origin_analysis(&self) -> &'a FileAnalysis {
        self.origin
    }
    pub fn origin_file_key(&self) -> &FileKey {
        &self.origin_key
    }
    pub fn cursor(&self) -> tree_sitter::Point {
        self.point
    }
    pub fn origin_source(&self) -> Option<&'a str> {
        self.source
    }
    /// The origin-scoped index — the closure-scoped view every forward
    /// resolution reads (`idx`), exposed so adapters query member types /
    /// config-variant leaves through the same visibility the set used.
    pub fn scoped_index(&self) -> Option<&dyn CrossFileLookup> {
        self.idx()
    }

    /// The def site of `member` on `class` — origin symbols first, then the
    /// class's own cached file. Serves the template-family ranked goto-def
    /// (one location per ladder class that actually defines the member).
    fn member_def_location(&self, class: &str, member: &str) -> Option<RefLocation> {
        // The member's def span in `fa` under `class`'s owner set, expanded
        // through inline-namespace transparency so a symbol filed under an
        // `inline namespace head` answers a lookup keyed on its transparent
        // parent `absl`. The set is derived once per scanned fa (the inline
        // attribution rides the file that opened the namespace, so it is
        // recomputed per file, never shared).
        let member_span_in = |fa: &crate::file_analysis::FileAnalysis| -> Option<Span> {
            let owners = pack_inline_owner_set(fa, class);
            fa.symbols
                .iter()
                .find(|s| s.name == member && pack_member_of(fa, s, &owners))
                .map(|s| s.selection_span)
        };
        if let Some(span) = member_span_in(self.origin) {
            return Some(self.origin_decl(span));
        }
        let idx = self.idx()?;
        let loc_of = |cached: &crate::file_analysis::CachedModule, span: Span| {
            Url::from_file_path(&cached.path).ok()?;
            Some(RefLocation {
                key: FileKey::Path(cached.path.clone()),
                span,
                access: AccessKind::Declaration,
                rewritable: true,
                label: None,
            })
        };
        // Class-keyed cached module — the fast path when `class` names a
        // struct/class/enum that is itself a cache key.
        if let Some(cached) = idx.get_cached(class) {
            if let Some(span) = member_span_in(&idx.whole_present(&cached)) {
                return loc_of(&cached, span);
            }
        }
        let Some((self_path, visible)) = idx.visibility_scope() else {
            return None;
        };
        let self_str = self_path.to_string_lossy().into_owned();
        let connected = |cached: &crate::file_analysis::CachedModule| {
            let p = cached.path.to_string_lossy();
            visible.contains(p.as_ref())
                || cached.analysis.include_closure.contains(&self_str)
        };
        // Name-indexed def candidates first (a File-scope member the index
        // registered) — the cheap subset, path-sorted for determinism.
        {
            let mut cands = idx.def_candidates(member);
            cands.sort_by(|a, b| a.path.cmp(&b.path));
            for cached in cands {
                if !connected(&cached) {
                    continue;
                }
                if let Some(span) = member_span_in(&idx.whole_present(&cached)) {
                    return loc_of(&cached, span);
                }
            }
        }
        // Include-closure scan: a namespace-scoped unscoped-enum enumerator
        // (`spdlog::level::info`) is NOT linkage-visible, so the name index
        // never registered it and the class key (`level`, a namespace) is no
        // cache winner. The origin's own include closure still carries the
        // defining header — walk it directly, applying the same owner test.
        // Only reached when both keyed lookups miss (a namespace-qualified
        // member), so the broad scan stays off the hot path.
        let mut hit: Option<(String, Span)> = None;
        idx.for_each_cached_file(&mut |cached| {
            if !connected(cached) || Url::from_file_path(&cached.path).is_err() {
                return;
            }
            // Broad scan, cold tail only (both keyed lookups missed) — the
            // rehydration LRU bounds the per-file cost for evicted copies.
            if let Some(span) = member_span_in(&idx.whole_present(cached)) {
                let p = cached.path.to_string_lossy().into_owned();
                if hit.as_ref().is_none_or(|(hp, _)| p < *hp) {
                    hit = Some((p, span));
                }
            }
        });
        hit.map(|(p, span)| RefLocation {
            key: FileKey::Path(PathBuf::from(p)),
            span,
            access: AccessKind::Declaration,
            rewritable: true,
            label: None,
        })
    }

    /// Decl→def preference (pack routing): when a resolved location is a
    /// bodiless DECLARATION — a function prototype, an `extern` variable
    /// decl — rank the bodied definition(s) of the same identity first,
    /// declaration kept (never pruned). Identity spans files on the same
    /// facts the backward walk's visibility gate uses: the def-candidates
    /// table plus closure connectivity (forward: the origin reaches the
    /// defining file; reverse: the defining TU includes the origin header —
    /// `server.c` defines the `server` its own `server.h` declares extern).
    /// "Definition-ness" is asked of the symbol, not a header-vs-TU shape
    /// branch: a callable's body mints a scope spanning it; a variable
    /// carries its `extern` storage class as an attribute. Anything that
    /// isn't a bodiless decl of those shapes passes through untouched.
    fn preferred_definitions(&self, decl: RefLocation, decl_fa: &FileAnalysis) -> Vec<RefLocation> {
        if !self.pack {
            return vec![decl];
        }
        let Some(sym) = decl_fa
            .symbols
            .iter()
            .find(|s| s.selection_span.start == decl.span.start)
        else {
            return vec![decl];
        };
        let hunt = match sym.kind {
            SymKind::Sub | SymKind::Method => {
                !decl_fa.scopes.iter().any(|sc| sc.span == sym.span)
            }
            SymKind::Variable => {
                decl_fa.symbol_is_file_scope_value(sym)
                    && sym.attributes.iter().any(|a| a == "extern")
            }
            _ => false,
        };
        if !hunt {
            return vec![decl];
        }
        // Identity is owner-exact here, NOT recall-biased: the decl→def hunt
        // decides "is this the SAME symbol's body," and a same-named FREE
        // function (package `None`) is a different identity than a class member
        // (package `Some`) — the `pkg_agrees` recall bias that admits an
        // unattributed side would let color.h's free `format` masquerade as the
        // body of `native_formatter::format`. Keep `None == None` (a free-fn
        // prototype and its TU body, an `extern` and its definition) and
        // tail-equal `Some`/`Some` (a member declared in-class, defined
        // out-of-line); reject the vacuous `Some`/`None` cross.
        let owner_agrees_forward = |a: Option<&str>, b: Option<&str>| match (a, b) {
            (None, None) => true,
            (Some(x), Some(y)) => pkg_agrees(true, Some(x), Some(y)),
            _ => false,
        };
        let cand_is_def = |a: &FileAnalysis, s: &crate::file_analysis::Symbol| {
            s.name == sym.name
                && owner_agrees_forward(sym.package.as_deref(), s.package.as_deref())
                && match sym.kind {
                    SymKind::Sub | SymKind::Method => {
                        matches!(s.kind, SymKind::Sub | SymKind::Method)
                            && a.scopes.iter().any(|sc| sc.span == s.span)
                    }
                    _ => {
                        matches!(s.kind, SymKind::Variable)
                            && a.symbol_is_file_scope_value(s)
                            && !s.attributes.iter().any(|at| at == "extern")
                    }
                }
        };
        let mut defs: Vec<RefLocation> = Vec::new();
        let push = |defs: &mut Vec<RefLocation>, key: &FileKey, span: Span| {
            if span.start == decl.span.start && file_key_eq(key, &decl.key) {
                return;
            }
            if defs.iter().any(|l| file_key_eq(&l.key, key) && l.span == span) {
                return;
            }
            defs.push(RefLocation {
                key: key.clone(),
                span,
                access: AccessKind::Declaration,
                rewritable: true,
                label: None,
            });
        };
        // The decl's own file first (a static's forward decl and body).
        for s in decl_fa.symbols.iter().filter(|s| cand_is_def(decl_fa, s)) {
            push(&mut defs, &decl.key, s.selection_span);
        }
        // Cross-file: the full def-candidates table, closure-connected to the
        // origin, path-sorted so the ranking is deterministic across the
        // cache's randomized iteration order.
        if let Some(idx) = self.idx() {
            if let Some((self_path, visible)) = idx.visibility_scope() {
                let self_str = self_path.to_string_lossy().into_owned();
                let decl_path = key_for_sort(&decl.key);
                let decl_str = decl_path.to_string_lossy().into_owned();
                // Connected when the origin sees the def, when the def's TU
                // includes the origin (the reverse `server.c` ⊇ `server.h`
                // link), OR when the def's TU includes the DECL's file. The
                // last is the general C separate-compilation link: a body's
                // TU includes the header that declares the same identity —
                // and the decl is the proven-same-symbol waypoint the origin
                // already resolved to. A THIRD TU calling through a shared
                // header (`t_string.c` → `server.h` proto, body in `db.c`)
                // reaches the body via this clause though it never sees
                // `db.c` textually.
                let connected = |cached: &std::sync::Arc<crate::file_analysis::CachedModule>| {
                    let p = cached.path.to_string_lossy();
                    cached.path != decl_path
                        && (visible.contains(p.as_ref())
                            || cached.analysis.include_closure.contains(&self_str)
                            || cached.analysis.include_closure.contains(&decl_str))
                };
                let mut cands = idx.def_candidates(&sym.name);
                cands.sort_by(|a, b| a.path.cmp(&b.path));
                for cached in &cands {
                    if !connected(cached) {
                        continue;
                    }
                    let key = FileKey::Path(cached.path.clone());
                    let whole = idx.whole_present(cached);
                    for s in whole.symbols.iter().filter(|s| cand_is_def(&whole, s)) {
                        push(&mut defs, &key, s.selection_span);
                    }
                }
                // Member fallback: a class member's out-of-line body is NOT
                // linkage-visible, so the name-keyed `def_candidates` table
                // never pulls its TU in (the same gap `member_def_location`'s
                // broad scan and `overload_arity_definitions`' `get_cached`
                // seed cover). When the identity is a class-owned callable and
                // no bodied def surfaced above, sweep the connected cached
                // files directly. Gated on the empty result so free-function /
                // static decl→def (already covered) never pays the broad scan.
                let is_member_callable = matches!(sym.kind, SymKind::Sub | SymKind::Method)
                    && sym.package.is_some();
                if defs.is_empty() && is_member_callable {
                    let mut hits: Vec<(String, RefLocation)> = Vec::new();
                    idx.for_each_cached_file(&mut |cached| {
                        if !connected(cached) {
                            return;
                        }
                        let key = FileKey::Path(cached.path.clone());
                        let whole = idx.whole_present(cached);
                        for s in whole.symbols.iter().filter(|s| cand_is_def(&whole, s)) {
                            if s.selection_span.start == decl.span.start
                                && file_key_eq(&key, &decl.key)
                            {
                                continue;
                            }
                            hits.push((
                                cached.path.to_string_lossy().into_owned(),
                                RefLocation {
                                    key: key.clone(),
                                    span: s.selection_span,
                                    access: AccessKind::Declaration,
                                    rewritable: true,
                                    label: None,
                                },
                            ));
                        }
                    });
                    hits.sort_by(|a, b| a.0.cmp(&b.0));
                    for (_, loc) in hits {
                        push(&mut defs, &loc.key, loc.span);
                    }
                }
            }
        }
        if defs.is_empty() {
            return vec![decl];
        }
        defs.push(decl);
        defs
    }

    /// Route a member/method goto-def location through the decl→def sibling
    /// axis. `member_def_location` and the cross-file inherited-method path both
    /// land on the class's DECLARATION (the header's in-class prototype); a
    /// `ClassName::method(){}` body lives out-of-line in another TU. The
    /// free-function by-name tail already hops decl→def through
    /// `preferred_definitions`; members went straight to the decl because their
    /// resolution enters through the owner-anchored / inherited-method paths
    /// instead. Feed the SAME axis by resolving the decl's own FileAnalysis
    /// (origin, or the cross-file cached copy) so the bodied def ranks first,
    /// decl kept. A decl whose analysis is unreachable degrades to the decl.
    fn prefer_member_defs(&self, decl: RefLocation) -> Vec<RefLocation> {
        if !self.pack {
            return vec![decl];
        }
        if file_key_eq(&decl.key, &self.origin_key) {
            return self.preferred_definitions(decl, self.origin);
        }
        if let (Some(idx), FileKey::Path(p)) = (self.idx(), decl.key.clone()) {
            if let Some(cached) = idx.cached_by_path(&p) {
                let whole = idx.whole_present(&cached);
                return self.preferred_definitions(decl, &whole);
            }
        }
        vec![decl]
    }

    /// Overload arity ranking (pack): a call to a name with MULTIPLE callable
    /// definitions ranks the family by how each signature's declared arity
    /// fits the call's written arg count — an exact `params == args` overload
    /// first, then defaults/variadic-compatible ones, then mismatches. Ranked,
    /// NEVER pruned (the whole overload set stays visible, like macro
    /// variants); ties break bodied-then-local-then-position so the pick is
    /// deterministic. `None` (fall through to single-def resolution) unless the
    /// call carries an arg count AND ≥2 same-name callables are in scope — so
    /// non-overloaded resolution is untouched. The arity FUEL is structural
    /// (`Symbol::param_arity` / `Ref::arg_count`); the fit rule lives on
    /// `ParamArity::fit`.
    fn overload_arity_definitions(&self) -> Option<Vec<RefLocation>> {
        if !self.pack {
            return None;
        }
        let analysis = self.origin;
        let idx = self.idx()?;
        let r = analysis.ref_at(self.point)?;
        let argc = r.arg_count?;
        let name = r.unqualified_target_name().to_string();
        // Anchor the family's scope on the primary resolution so overload
        // siblings gather in the right class/namespace without re-deriving C++
        // name lookup here.
        let pkg: Option<String> = match &r.kind {
            RefKind::MethodCall { .. } => Some(analysis.method_call_invocant_class(r, Some(idx))?),
            RefKind::FunctionCall { resolved_package } => resolved_package.clone().or_else(|| {
                analysis.find_definition(self.point, Some(idx)).and_then(|sp| {
                    analysis
                        .symbols
                        .iter()
                        .find(|s| {
                            s.selection_span.start == sp.start
                                && matches!(s.kind, SymKind::Sub | SymKind::Method)
                        })
                        .and_then(|s| s.package.clone())
                })
            }),
            _ => return None,
        };
        let pkg_ok = |sp: Option<&str>| match &pkg {
            None => sp.is_none(),
            Some(p) => pkg_agrees(true, Some(p), sp),
        };
        let has_body =
            |a: &FileAnalysis, s: &crate::file_analysis::Symbol| a.scopes.iter().any(|sc| sc.span == s.span);
        // Owner match: the candidate GENUINELY belongs to the anchored owner
        // (both sides carry a package and the tails agree) — NOT via the
        // `pkg_ok` recall bias that admits an unattributed side. A member call
        // (`logger.info(x)`) anchors on the receiver class, so a real member
        // ranks above a same-named FREE function (package `None`); the free
        // function stays in the set, just below. `None` anchor (a free-fn call)
        // leaves the axis flat, so non-member overloading is untouched.
        let owner_matched = |sp: Option<&str>| match (pkg.as_deref(), sp) {
            (Some(p), Some(q)) => pkg_agrees(true, Some(p), Some(q)),
            _ => false,
        };
        // Sort key, best-first: owner match, then arity fit, then bodied, then
        // local, then a total (path, row, col) order for cache determinism.
        let mut cands: Vec<(bool, u8, bool, bool, PathBuf, usize, usize, RefLocation)> = Vec::new();
        let push = |owner: bool,
                    fit: u8,
                    bodied: bool,
                    local: bool,
                        key: &FileKey,
                        span: Span,
                        cands: &mut Vec<(bool, u8, bool, bool, PathBuf, usize, usize, RefLocation)>| {
            if cands
                .iter()
                .any(|c| file_key_eq(&c.7.key, key) && c.7.span == span)
            {
                return;
            }
            cands.push((
                owner,
                fit,
                bodied,
                local,
                key_for_sort(key),
                span.start.row,
                span.start.column,
                RefLocation {
                    key: key.clone(),
                    span,
                    access: AccessKind::Declaration,
                    rewritable: true,
                    label: None,
                },
            ));
        };
        for s in analysis.symbols.iter().filter(|s| {
            s.name == name
                && matches!(s.kind, SymKind::Sub | SymKind::Method)
                && pkg_ok(s.package.as_deref())
        }) {
            let fit = s.param_arity().map(|a| a.fit(argc)).unwrap_or(0);
            push(owner_matched(s.package.as_deref()), fit, has_body(analysis, s), true, &self.origin_key, s.selection_span, &mut cands);
        }
        // The receiver class's OWN cached file. A member method is not
        // linkage-visible, so the def-candidates table below (keyed on the
        // bare name) never pulls the member's header in when the same-named
        // FREE function that collides lives in a DIFFERENT file (spdlog's
        // `logger::info` in logger.h vs `spdlog::info` in spdlog.h). Seed the
        // owner-matched member directly from `get_cached(class)` so the
        // ranking can float it above the frees.
        if let Some(p) = &pkg {
            if let Some(cached) = idx.get_cached(p) {
                let key = FileKey::Path(cached.path.clone());
                let whole = idx.whole_present(&cached);
                for s in whole.symbols.iter().filter(|s| {
                    s.name == name
                        && matches!(s.kind, SymKind::Sub | SymKind::Method)
                        && pkg_ok(s.package.as_deref())
                }) {
                    let fit = s.param_arity().map(|a| a.fit(argc)).unwrap_or(0);
                    push(owner_matched(s.package.as_deref()), fit, has_body(&whole, s), false, &key, s.selection_span, &mut cands);
                }
            }
        }
        // Cross-file: the full def-candidates table, closure-connected to the
        // origin (same connectivity gate as the decl→def ranking).
        if let Some((self_path, visible)) = idx.visibility_scope() {
            let self_str = self_path.to_string_lossy().into_owned();
            let origin_path = key_for_sort(&self.origin_key);
            let mut cached_files = idx.def_candidates(&name);
            cached_files.sort_by(|a, b| a.path.cmp(&b.path));
            for cached in cached_files {
                if cached.path == origin_path {
                    continue;
                }
                let p = cached.path.to_string_lossy().into_owned();
                let connected = visible.contains(&p)
                    || cached.analysis.include_closure.contains(&self_str);
                if !connected {
                    continue;
                }
                let key = FileKey::Path(cached.path.clone());
                let whole = idx.whole_present(&cached);
                for s in whole.symbols.iter().filter(|s| {
                    s.name == name
                        && matches!(s.kind, SymKind::Sub | SymKind::Method)
                        && pkg_ok(s.package.as_deref())
                }) {
                    let fit = s.param_arity().map(|a| a.fit(argc)).unwrap_or(0);
                    push(owner_matched(s.package.as_deref()), fit, has_body(&whole, s), false, &key, s.selection_span, &mut cands);
                }
            }
        }
        if cands.len() < 2 {
            return None;
        }
        cands.sort_by(|a, b| {
            b.0.cmp(&a.0) // owner-matched first
                .then_with(|| b.1.cmp(&a.1)) // arity fit, best first
                .then_with(|| b.2.cmp(&a.2)) // bodied first
                .then_with(|| b.3.cmp(&a.3)) // local first
                .then_with(|| a.4.cmp(&b.4)) // path
                .then_with(|| (a.5, a.6).cmp(&(b.5, b.6))) // row, col
        });
        Some(cands.into_iter().map(|c| c.7).collect())
    }

    /// A declaration site in the origin file.
    fn origin_decl(&self, span: Span) -> RefLocation {
        RefLocation {
            key: self.origin_key.clone(),
            span,
            access: AccessKind::Declaration,
            rewritable: true,
            label: None
        }
    }

    /// Forward projection: goto-definition. Returns the winning path's
    /// location(s) — multi-location only for stacked handler registrations;
    /// the never-pruned ranked multi-def is the documented residual the
    /// spike's ranking axis fills in (see the ADR's merge plan).
    pub fn definitions(&self) -> Vec<RefLocation> {
        let analysis = self.origin;
        let point = self.point;

        // Owner-anchored forward resolution: a `::`-qualified value read
        // (`dynamic::STRING`, `absl::StatusCode::kNotFound`) names its OWNER
        // explicitly — the qualifier segment touching the token. Resolve the
        // member on that owner BEFORE any bare-name fallback (the macro lane
        // below, the by-name file-scope tail), which would otherwise hijack the
        // name via a same-named `#define` or free symbol in an unrelated file.
        // A macro has no namespace, so a qualified word is never a macro use;
        // anchoring on the qualifier the cursor already wrote is unconditional.
        // Falls through untouched when the qualifier resolves nothing (a
        // namespace middle-segment `ns::Code` handled by the type/last-resort
        // paths).
        if self.pack {
            if let Some(source) = self.source {
                if let Some(owner) = qualifier_at_point(source, point) {
                    if let Some(name) = word_at_point(source, point) {
                        if let Some(loc) = self.member_def_location(owner, name) {
                            // The member lookup lands on the class DECLARATION;
                            // hop to the out-of-line body (decl→def axis).
                            return self.prefer_member_defs(loc);
                        }
                    }
                }
            }
        }

        // Macro-aware goto-def OWNS a macro-named word (pack routing): the
        // `#define` wins over a use's self-span, EVERY def site comes back
        // (config variants across files never pruned), reachability-RANKED
        // config-active first — the total order `definitions()` returns is
        // the ranking axis, per candidate — plus any direct-delegation
        // see-through target, labeled. `docs/adr/macro-handling.md`.
        if self.pack {
            if let (Some(source), Some(idx)) = (self.source, self.idx()) {
                if let Some(word) = word_at_point(source, point) {
                    // Shape gate: a function-like `#define F(x)` expands only at
                    // call shape `F(`. At a parenless site it can't claim the
                    // token — drop those variants so a parenless type token
                    // (`OP** p`) falls through to the typedef/class lane instead
                    // of landing on a same-named regex-internal `#define OP(p)`.
                    let call_shaped = token_is_call_shaped(source, point);
                    let ranked: Vec<_> = ranked_macro_variants(analysis, word, &self.origin_key, idx)
                        .into_iter()
                        .filter(|(m, _, _)| call_shaped || m.params.is_none())
                        .collect();
                    if !ranked.is_empty() {
                        let mut out: Vec<RefLocation> = ranked
                            .iter()
                            .map(|(m, key, r)| RefLocation {
                                key: key.clone(),
                                span: m.selection_span,
                                access: AccessKind::Declaration,
                                rewritable: true,
                                label: r.label(),
                            })
                            .collect();
                        // See-through: a direct-delegation wrapper
                        // (`#define F(x) G(x)`) also offers the delegate `G`,
                        // resolved from the top-ranked delegating variant so
                        // the offer follows the config-active body. A
                        // self-delegation (`#define S S`) resolves to the
                        // definition itself — already offered above.
                        if let Some((m, _, _)) = ranked
                            .iter()
                            .find(|(m, _, _)| m.delegate.as_deref().is_some_and(|d| d != m.name))
                        {
                            if let Some(delegate) = &m.delegate {
                                if let Some(mut loc) =
                                    pack_symbol_def_location(analysis, &self.origin_key, delegate, idx)
                                {
                                    loc.label = Some(format!("delegates to {delegate}"));
                                    out.push(loc);
                                }
                            }
                        }
                        return out;
                    }
                }
            }
        }

        // Query-time dispatch goto-def: a `$minion->enqueue('task')` whose
        // receiver isa-resolves (possibly cross-file) jumps to the handler,
        // even when no `DispatchCall` ref was materialized for this site. The
        // gate is applied in `dispatch_at`; we just map the resolved handler
        // to its definition. Runs first because the cursor is on the
        // name-string arg, which the paths below would otherwise treat as a
        // plain string literal. See `docs/adr/receiver-gated-dispatch.md`.
        if let Some(idx) = self.idx() {
            if let Some(applied) = analysis.dispatch_at(point, Some(idx)) {
                let locs = dispatch_handler_locations(&applied.owner, &applied.name, idx);
                if !locs.is_empty() {
                    return locs;
                }
            }
        }

        // Plain goto-def on a field returns the field DECLARATION only — its
        // inferred domain type (`op_type` → `enum opcode`) is a TYPE, not a
        // declaration, so it does not fold into goto-def. The domain bridge
        // (`field_domain`) still powers hover and a future goto-type-definition
        // (both read it directly); a domain-typed field resolves through the
        // same general member/cross-file paths below as any other field.
        // (hitlist-6 Family B, user decision.)

        // Template-family ranked member goto-def: a member use on a template
        // INSTANCE resolves down the specificity ladder (exact spec >
        // partial-pattern spec > primary) — the dispatch winner FIRST, and
        // every other family class defining the same member kept (ranked,
        // never pruned), so a spec's override and the primary's generic def
        // both offer.
        if self.pack {
            if let Some(r) = analysis.ref_at(point) {
                if let RefKind::MethodCall { invocant_span: Some(inv), .. } = &r.kind {
                    if let Some(recv_ty) = analysis.expr_type_at_span(*inv, self.idx()) {
                        if recv_ty.as_parametric().is_some() {
                            let member = r.unqualified_target_name();
                            let mut out: Vec<RefLocation> = Vec::new();
                            for (class, _) in
                                analysis.dispatch_ladder_of(&recv_ty, self.idx())
                            {
                                let Some(loc) = self.member_def_location(&class, member)
                                else {
                                    continue;
                                };
                                if !out.iter().any(|l| {
                                    file_key_eq(&l.key, &loc.key) && l.span == loc.span
                                }) {
                                    out.push(loc);
                                }
                            }
                            if !out.is_empty() {
                                return out;
                            }
                        }
                    }
                }
            }
        }

        // Type-REFERENCE gd consults the specificity ladder: a template-
        // instance spelling in type position (a base clause `: formatter<
        // std::tm, Char>`, a declared type) resolves canonical-spelling →
        // per-spec class FIRST, partial patterns next, the primary ranked
        // behind — never pruned. Plain (non-template) type refs fall through
        // untouched (`template_instance_spelling` answers `None`).
        if self.pack {
            if let (Some(source), Some(r)) = (self.source, analysis.ref_at(point)) {
                if matches!(r.kind, RefKind::PackageRef) {
                    if let Some(spelling) = template_instance_spelling(source, r.span) {
                        if let Some(p) =
                            crate::file_analysis::ParametricType::instance_from_spelling(&spelling)
                        {
                            let t = crate::file_analysis::InferredType::Parametric(p);
                            let mut out: Vec<RefLocation> = Vec::new();
                            for (class, _) in analysis.dispatch_ladder_of(&t, self.idx()) {
                                let Some(idx) = self.idx() else { break };
                                let Some(loc) = self.type_def_location(&class, idx) else {
                                    continue;
                                };
                                if !out.iter().any(|l| {
                                    file_key_eq(&l.key, &loc.key) && l.span == loc.span
                                }) {
                                    out.push(loc);
                                }
                            }
                            if !out.is_empty() {
                                return out;
                            }
                        }
                    }
                }
            }
        }

        // Overload arity ranking: a call to an overloaded name ranks the
        // family by how each signature fits the call's arg count (never
        // pruned). Supersedes the single-winner `find_definition` below, which
        // is arity-blind. `None` for non-overloaded calls, leaving the path
        // below untouched.
        if let Some(ranked) = self.overload_arity_definitions() {
            return ranked;
        }

        // Local definition first — through the decl→def ranking, so a cursor
        // on (or a call resolving to) a bodiless prototype / extern decl
        // offers the bodied definition first, decl kept.
        if let Some(span) = analysis.find_definition(point, self.idx()) {
            return self.preferred_definitions(self.origin_decl(span), analysis);
        }

        let Some(idx) = self.idx() else {
            return Vec::new();
        };
        let line_loc = |path: PathBuf, line: u32| -> RefLocation {
            let p = tree_sitter::Point::new(line as usize, 0);
            RefLocation {
                key: FileKey::Path(path),
                span: Span { start: p, end: p },
                access: AccessKind::Declaration,
                rewritable: true,
                label: None
            }
        };

        // Cross-file hash-key defs. Two shapes share the lookup:
        //   * deferred ctor key (`owner: None`) — the build-time gate
        //     couldn't see the class; derive the owner now (enclosing call's
        //     invocant class, index in hand);
        //   * resolved Class owner (`$row->{name}` upgraded post-fold to
        //     `Class(NestedRow)`) whose class — and therefore its
        //     `add_columns` / `has` / `:param` HashKeyDef — lives elsewhere.
        // Either way: the class's cached analysis carries the def.
        if let Some(r) = analysis.ref_at(point) {
            if let RefKind::HashKeyAccess { ref owner, .. } = r.kind {
                use crate::file_analysis::HashKeyOwner;
                let owner = match owner {
                    Some(o) => Some(o.clone()),
                    None => analysis.deferred_hash_key_owner(r, Some(idx)),
                };
                let class = match &owner {
                    Some(HashKeyOwner::Sub { package: Some(c), .. }) => Some(c.clone()),
                    Some(HashKeyOwner::Class(c)) => Some(c.clone()),
                    _ => None,
                };
                if let (Some(owner), Some(class)) = (owner, class) {
                    if let Some(cached) = idx.get_cached(&class) {
                        if let Some(def) = cached
                            .analysis
                            .hash_key_defs_for_owner(&owner)
                            .into_iter()
                            .find(|d| d.name == r.target_name)
                        {
                            return vec![RefLocation {
                                key: FileKey::Path(cached.path.clone()),
                                span: def.selection_span,
                                access: AccessKind::Declaration,
                                rewritable: true,
                                label: None
                            }];
                        }
                    }
                }
            }
        }

        if let Some(r) = analysis.ref_at(point) {
            // Function call matching an imported symbol.
            if matches!(r.kind, RefKind::FunctionCall { .. }) {
                if let Some((import, module_path, remote_name)) =
                    resolve_imported_function(analysis, &r.target_name, idx)
                {
                    // Cross-file sub_info lookup uses the REMOTE name —
                    // distinct from target_name for renaming imports.
                    // Re-export aware: the def may live in a module
                    // `import.module_name` re-exports (Test::Most →
                    // Test::More's `ok`). Chase the edges to the defining
                    // module; fall back to the directly-`use`d path.
                    let defining =
                        idx.defining_module_cached(&import.module_name, &remote_name);
                    let module_path = defining
                        .as_ref()
                        .map(|m| m.path.clone())
                        .unwrap_or(module_path);
                    if Url::from_file_path(&module_path).is_ok() {
                        // The defining sub's line in the .pm — `Some` only when
                        // the module (or one it re-exports) defines the remote
                        // name. One hop to it whenever known: landing on the
                        // consumer's `use` line was never the goal.
                        let def_line = defining.and_then(|cached| {
                            idx.whole_present(&cached)
                                .sub_info_view(&remote_name)
                                .map(|s| s.def_line())
                        });
                        if let Some(line) = def_line {
                            return vec![line_loc(module_path, line)];
                        }
                        // Cursor on the import name with an unresolved def:
                        // jump to the top of the .pm (better than the
                        // consumer's use line).
                        if crate::file_analysis::contains_point(&import.span, point) {
                            return vec![line_loc(module_path, 0)];
                        }
                    }
                    // Fall back to just the use statement.
                    return vec![self.origin_decl(import.span)];
                }

                // Fully-qualified call (`Foo::Bar::baz()`) with no import: the
                // qualifier names the package directly; the defining package
                // lives in another module. Resolve via `resolved_package` (the
                // qualifier) and the bare sub name.
                if let RefKind::FunctionCall { resolved_package: Some(pkg) } = &r.kind {
                    let bare = r.unqualified_target_name();
                    if let Some(cached) = idx.get_cached(pkg) {
                        if Url::from_file_path(&cached.path).is_ok() {
                            match idx
                                .whole_present(&cached)
                                .sub_info_view(bare)
                                .map(|s| s.def_line())
                            {
                                Some(line) => return vec![line_loc(cached.path.clone(), line)],
                                // Fail safe for a pack `Scope::member` miss: the
                                // owner-anchored member lookup already ran (and
                                // missed), and `pkg` names no sub `bare` in the
                                // resolved module — so `pkg::bare` is NOT a
                                // module path. Manufacturing a file-top `1:1`
                                // location is a confidently-wrong answer (worse
                                // than none for goto-def: abseil's every-header
                                // `namespace absl` makes `get_cached("absl")`
                                // land on an arbitrary file). Perl keeps the
                                // file-top fallback — landing on the `.pm` top
                                // is meaningful there.
                                None if self.pack => {}
                                None => return vec![line_loc(cached.path.clone(), 0)],
                            }
                        }
                    }
                }
            }

            // Fully-qualified variable read (`$Foo::Bar::x`, `@Pkg::arr`):
            // the package lives in another module — resolve the package
            // global through the index, mirroring the FQ-call path. Honest
            // miss (no jump) when the package or its decl is absent.
            if let Some((pkg, name)) = r.qualified_var_target() {
                if let Some(cached) = idx.get_cached(pkg) {
                    if Url::from_file_path(&cached.path).is_ok() {
                        if let Some(def_line) =
                            idx.whole_present(&cached).package_var_def_line(&name, pkg)
                        {
                            return vec![line_loc(cached.path.clone(), def_line)];
                        }
                    }
                }
            }

            // Cross-file package/type goto-def: resolve the name via the
            // index. Land on the declaring symbol when the cached analysis
            // knows it (a Perl `package Foo;` line, a cpp `struct op` /
            // typedef name); fall back to the top of the file. Resolve the
            // CachedModule ONCE and take path AND range from it — pairing
            // `module_path_cached`'s file with a separately-scoped
            // `get_cached`'s range splices two candidates when the name is
            // defined in more than one file.
            if matches!(r.kind, RefKind::PackageRef) {
                if let Some(cached) = idx.get_cached(&r.target_name) {
                    if Url::from_file_path(&cached.path).is_ok() {
                        let whole = idx.whole_present(&cached);
                        let span = whole
                            .symbols
                            .iter()
                            .find(|s| {
                                s.name == r.target_name
                                    && matches!(
                                        s.kind,
                                        SymKind::Package | SymKind::Class | SymKind::Module
                                    )
                            })
                            // Type space missed: a pack grammar's TYPE guess in
                            // a type/value-ambiguous slot (template argument)
                            // can name a VALUE the pack index registered under
                            // this same bare name — land on ITS decl, not the
                            // file top. Pack-only structural gates; Perl module
                            // lookups keep the file-top fallback.
                            .or_else(|| {
                                whole.symbols.iter().find(|s| {
                                    s.name == r.target_name
                                        && (whole.symbol_is_class_content(s)
                                            || whole.symbol_is_file_scope_value(s))
                                })
                            })
                            .map(|s| s.selection_span)
                            .unwrap_or(Span {
                                start: tree_sitter::Point::new(0, 0),
                                end: tree_sitter::Point::new(0, 0),
                            });
                        return vec![RefLocation {
                            key: FileKey::Path(cached.path.clone()),
                            span,
                            access: AccessKind::Declaration,
                            rewritable: true,
                            label: None
                        }];
                    }
                }
                // No analysis cached: the path map alone still beats no
                // answer — land at the top of the file.
                if let Some(path) = idx.module_path_cached(&r.target_name) {
                    if Url::from_file_path(&path).is_ok() {
                        return vec![line_loc(path, 0)];
                    }
                }
            }

            // Cross-file DispatchCall goto-def: `$consumer->emit('ready')` in
            // one file jumps to `$producer->on('ready', sub)` in another.
            // Stacked registrations all surface (multi-location picker).
            if let RefKind::DispatchCall { owner: Some(owner), .. } = &r.kind {
                let locs = dispatch_handler_locations(owner, &r.target_name, idx);
                if !locs.is_empty() {
                    return locs;
                }
            }

            // Cross-file method goto-def: inherited methods through the index.
            if matches!(r.kind, RefKind::MethodCall { .. }) {
                use crate::file_analysis::MethodResolution;
                // FQ `$o->Foo::Bar::m` dispatches the bare `m` on the named class.
                let method = r.unqualified_target_name();
                if let Some(cn) = analysis.method_call_invocant_class(r, Some(idx)) {
                    // The invocant resolved (e.g. a plugin-bridged route token
                    // → controller class) but the controller lives in THIS
                    // file: jump to the local method symbol. The build-time
                    // freeze normally serves same-file dispatch, but a bridged
                    // invocant is never frozen (its class needs the index), so
                    // re-resolve here.
                    if let Some(MethodResolution::Local { sym_id, .. }) =
                        analysis.resolve_method_in_ancestors(&cn, method, Some(idx))
                    {
                        if let Some(sym) = analysis.symbols.iter().find(|s| s.id == sym_id) {
                            return vec![self.origin_decl(sym.selection_span)];
                        }
                    }
                    if let Some(MethodResolution::CrossFile { ref class, ref def_module }) =
                        analysis.resolve_method_in_ancestors(&cn, method, Some(idx))
                    {
                        // One path for both: a real inherited method lives in
                        // `class`'s own module; a plugin-bridged helper lives
                        // in `def_module` (the bridging file). Same lookup
                        // either way.
                        let module = def_module.as_deref().unwrap_or(class);
                        if let Some(cached) = idx.get_cached(module) {
                            // A cross-file DBIC accessor is a deferred emission
                            // MATERIALIZED into the whole cached copy at index
                            // completion (`materialize_gated_emissions`), so the
                            // whole view carries it — no per-query enrichment.
                            let whole = idx.whole_present(&cached);
                            if let Some(sub_info) = whole.sub_info_view(method) {
                                if Url::from_file_path(&cached.path).is_ok() {
                                    // A pack member call lands on the class
                                    // module's DECLARATION; hop to the
                                    // out-of-line body (decl→def axis) like the
                                    // free-function tail. The decl's own symbol
                                    // span (not just `def_line`) is what the
                                    // axis matches on. Perl subs keep the
                                    // `def_line` jump (`prefer_member_defs` is a
                                    // no-op off-pack anyway).
                                    if self.pack {
                                        if let Some(sym) = whole.symbols.iter().find(|s| {
                                            s.name == method
                                                && matches!(s.kind, SymKind::Sub | SymKind::Method)
                                                && pkg_agrees(true, s.package.as_deref(), Some(class))
                                        }) {
                                            return self.prefer_member_defs(RefLocation {
                                                key: FileKey::Path(cached.path.clone()),
                                                span: sym.selection_span,
                                                access: AccessKind::Declaration,
                                                rewritable: true,
                                                label: None,
                                            });
                                        }
                                    }
                                    return vec![line_loc(
                                        cached.path.clone(),
                                        sub_info.def_line(),
                                    )];
                                }
                            }
                            // cpp data field (or enum constant): a
                            // Variable/Field/Enumerator member, not a sub.
                            if let Some(sym) = whole.symbols.iter().find(|s| {
                                matches!(
                                    s.kind,
                                    SymKind::Variable | SymKind::Field | SymKind::Enumerator
                                ) && s.name == method
                                    && s.package.as_deref() == Some(class.as_str())
                                    && whole.symbol_is_class_content(s)
                            }) {
                                if Url::from_file_path(&cached.path).is_ok() {
                                    return vec![RefLocation {
                                        key: FileKey::Path(cached.path.clone()),
                                        span: sym.selection_span,
                                        access: AccessKind::Declaration,
                                        rewritable: true,
                                        label: None
                                    }];
                                }
                            }
                        }
                    }
                }
            }
        }

        // Generic cross-file goto-def for a call OR a bare value read that
        // didn't resolve locally or via Perl imports. Pack languages register
        // free functions + file-scope vars/macros/enum-constants by name, so
        // look the name up in the cross-file index → the file that
        // declares/defines it → that symbol. A `Variable` ref reaches here
        // only when it had no local `resolves_to` (the local path above
        // already returned for resolved ones), so this is the cross-file
        // tail: `OP_SCOPE` used in op.c resolving to its enumerator def in
        // opnames.h. (Perl's cache is keyed by MODULE name, so a bare-name
        // lookup no-ops.) A `::`-qualified value read
        // (`absl::StatusCode::kNotFound`) is already handled at the top by the
        // owner-anchored step (`qualifier_at_point` + `member_def_location`),
        // which fires unconditionally for pack routing before this tail.
        if let Some(r) = analysis.ref_at(point) {
            if matches!(r.kind, RefKind::FunctionCall { .. } | RefKind::Variable) {
                let name = r.unqualified_target_name();
                if let Some(cached) = idx.get_cached(name) {
                    let whole = idx.whole_present(&cached);
                    if let Some(sym) = whole.symbols.iter().find(|s| {
                        s.name == name
                            && matches!(s.kind, SymKind::Sub | SymKind::Variable | SymKind::Enumerator)
                    }) {
                        if Url::from_file_path(&cached.path).is_ok() {
                            // A call resolving to a header PROTOTYPE offers
                            // the bodied definition first (decl→def ranking).
                            return self.preferred_definitions(
                                RefLocation {
                                    key: FileKey::Path(cached.path.clone()),
                                    span: sym.selection_span,
                                    access: AccessKind::Declaration,
                                    rewritable: true,
                                    label: None,
                                },
                                &whole,
                            );
                        }
                    }
                }
            }
        }

        // Last resort (pack): a token no query captures — a namespace middle
        // segment (`StatusCode` in `absl::StatusCode::kNotFound` is a
        // namespace_identifier, ref-less) — resolves by word to a named
        // type/namespace def.
        if self.pack {
            if let Some(source) = self.source {
                if let Some(word) = word_at_point(source, point) {
                    if let Some(loc) = self.type_def_location(word, idx) {
                        return vec![loc];
                    }
                }
            }
        }
        Vec::new()
    }

    /// The def location of a named type (a Class symbol — enum/struct/
    /// typedef — or a namespace/module), local first, then cross-file by
    /// name. Used by the spec-ladder type gd and the bare-word fallback.
    fn type_def_location(&self, type_name: &str, idx: &dyn CrossFileLookup) -> Option<RefLocation> {
        let wanted =
            |k: &SymKind| matches!(k, SymKind::Class | SymKind::Package | SymKind::Module);
        if let Some(sym) = self
            .origin
            .symbols
            .iter()
            .find(|s| s.name == type_name && wanted(&s.kind))
        {
            return Some(self.origin_decl(sym.selection_span));
        }
        let cached = idx.get_cached(type_name)?;
        let whole = idx.whole_present(&cached);
        let sym = whole
            .symbols
            .iter()
            .find(|s| s.name == type_name && wanted(&s.kind))?;
        Some(RefLocation {
            key: FileKey::Path(cached.path.clone()),
            span: sym.selection_span,
            access: AccessKind::Declaration,
            rewritable: true,
            label: None
        })
    }

    /// Completion visibility: unlike the navigation projections there is no
    /// resolved target to run `references_mask_for` on (the cursor sits on a
    /// prefix, not a name), so the default is the full VISIBLE universe; the
    /// construction-time override still narrows it — the same one knob that
    /// narrows references/rename.
    fn completion_visibility(&self) -> RoleMask {
        self.visibility_override.unwrap_or(RoleMask::VISIBLE)
    }

    /// Completion candidate gathering: the prefix-enumeration of the same
    /// visible universe the navigation projections resolve against. This is
    /// the SOURCE of identifier candidates only — cursor-context gating
    /// (which slot the cursor is in) and item presentation stay in the LSP
    /// adapter. Sources by tier:
    ///
    /// - OPEN — the origin file's in-scope names (variables, subs,
    ///   packages: the origin is the document being edited, i.e. the open
    ///   tier by definition of the completion verb) and the names its `use`
    ///   statements explicitly import (origin-file facts; the dep cache only
    ///   enriches their detail).
    /// - DEPENDENCY — names supplied by other modules' export surfaces:
    ///   the rest of an imported module's `@EXPORT`/`@EXPORT_OK`, and every
    ///   cached exporter's surface as auto-import candidates.
    ///
    /// `import_slot` is the slot's import affordance: whether accepting an
    /// import-sourced name here has somewhere to land its `use` edit.
    /// `false` means the slot offers no import-sourced names at all (today:
    /// every slot except the general identifier slot) — an import candidate
    /// without a place for its edit would complete to broken code. The
    /// candidates carry the importable-from FACT (`ImportFact`); the
    /// adapter composes fact + affordance into the edit.
    ///
    /// The general slot passes `""` (clients filter by prefix); a non-empty
    /// prefix narrows server-side for callers that want it.
    pub fn complete(
        &self,
        prefix: &str,
        import_slot: bool,
    ) -> Vec<CompletionCandidate> {
        let mask = self.completion_visibility();
        // Pack routing: the identifier universe is the origin's #include
        // closure — C's import surface ("C = Perl, everything exported": the
        // closure IS the import list, so enum constants, free functions,
        // typedefs and globals from included headers are candidates exactly
        // like imported subs are for Perl). Same projection, same mask knob;
        // the sources differ per routing because the languages' name-supply
        // models differ, not the seam.
        if self.pack {
            let mut out = Vec::new();
            if mask.contains(RoleMask::DEPENDENCY) && !self.origin.include_closure.is_empty() {
                if let Some(idx) = self.module_index {
                    let visible: std::collections::HashSet<String> =
                        self.origin.include_closure.iter_strs().map(|a| a.as_ref().to_owned()).collect();
                    // Many candidate names come from the same header —
                    // resolve each FILE's whole view once per request, not
                    // once per name (the LRU absorbs misses, but even hits
                    // pay a map probe + recency write).
                    let mut whole_memo: std::collections::HashMap<
                        PathBuf,
                        std::sync::Arc<crate::file_analysis::FileAnalysis>,
                    > = std::collections::HashMap::new();
                    for (name, cached) in idx.visible_defs_with_prefix(prefix, &visible) {
                        // Only linkage-visible defs (a TU-static never
                        // completes elsewhere). Symbol detail (kind, parent
                        // enum) reads the whole view — the resident copy may
                        // be symbol-evicted.
                        let whole = whole_memo
                            .entry(cached.path.clone())
                            .or_insert_with(|| idx.whole_present(&cached))
                            .clone();
                        let Some(sym) = whole
                            .symbols_named(&name)
                            .iter()
                            .map(|id| whole.symbol(*id))
                            .find(|s| whole.is_linkage_visible(s))
                        else {
                            continue;
                        };
                        let header = cached
                            .path
                            .file_name()
                            .map(|f| f.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        // An enum constant carries its parent enum as
                        // `package` — "opcode — opnames.h" reads the domain
                        // at a glance.
                        let detail = match sym.package.as_deref() {
                            Some(p) if !p.is_empty() => format!("{} — {}", p, header),
                            _ => header,
                        };
                        out.push(CompletionCandidate {
                            label: name.clone(),
                            kind: sym.kind.clone(),
                            detail: Some(detail),
                            insert_text: None,
                            sort_priority: crate::file_analysis::PRIORITY_CLOSURE,
                            additional_edits: vec![],
                            import_fact: None,
                            display_override: None,
                        });
                    }
                }
            }
            return out;
        }
        let mut out = Vec::new();
        if mask.contains(RoleMask::OPEN) {
            out.extend(self.origin.complete_general(self.point));
        }
        if let (true, Some(idx)) = (import_slot, self.module_index) {
            import_candidates(self.origin, idx, mask, &mut out);
            if mask.contains(RoleMask::DEPENDENCY) {
                unimported_export_candidates(self.origin, idx, &mut out);
            }
        }
        if !prefix.is_empty() {
            out.retain(|c| c.label.starts_with(prefix));
        }
        out
    }

    /// The loadable-module half of the completion universe: names a `use`
    /// statement (or a `Foo::` path drill) can reach, as
    /// (name, is_resolved). Dependency-tier by construction — both the
    /// resolved module cache and the @INC availability scan live behind the
    /// index. Workspace-package names are a documented gap: the store holds
    /// their analyses but no gathering source enumerates them yet, here or
    /// pre-seam (see the ADR's honest-boundary list). In-file package names
    /// ride `complete()`'s OPEN tier instead.
    pub fn complete_modules(&self, prefix: &str) -> Vec<(String, bool)> {
        let mask = self.completion_visibility();
        let mut out = Vec::new();
        if mask.contains(RoleMask::DEPENDENCY) {
            if let Some(idx) = self.module_index {
                out.extend(idx.complete_module_names(prefix));
            }
        }
        out
    }

    /// `complete_modules` shaped into candidates: indexed modules rank
    /// above merely-available ones. Presentation (the MODULE kind, the
    /// availability detail) rides the candidate so the one adapter
    /// projection reproduces the `use`-line / path-drill module half.
    pub fn complete_module_candidates(&self, prefix: &str) -> Vec<CompletionCandidate> {
        self.complete_modules(prefix)
            .into_iter()
            .map(|(name, is_resolved)| {
                let (detail, sort_priority) = if is_resolved {
                    (Some("indexed".to_string()), 10u8)
                } else {
                    (Some("available".to_string()), 50u8)
                };
                CompletionCandidate {
                    label: name,
                    kind: SymKind::Module,
                    detail,
                    insert_text: None,
                    sort_priority,
                    additional_edits: vec![],
                    import_fact: None,
                    display_override: None,
                }
            })
            .collect()
    }

    /// Candidates for a `Package::<cursor>` drill: the subs declared in (or
    /// inherited by) `package` — bare-name inserts so the typed prefix stays
    /// put (tier 10) — plus the sub-packages nested under it, both the
    /// loadable modules the set's module universe knows and the in-file
    /// `package Package::Other` names its OPEN tier holds (tier 20, labelled
    /// by the suffix so the client's `Package::<typed>` filter matches).
    pub fn complete_qualified_path(
        &self,
        module_index: &dyn CrossFileLookup,
        package: &str,
    ) -> Vec<CompletionCandidate> {
        // Pack routing: the qualifier names a namespace/class owner; the
        // candidates are its members, gathered through the SAME
        // owner-membership predicate owner-anchored goto-def resolves with.
        // Same projection, per-routing sources — like `complete()`.
        if self.pack {
            return self.complete_pack_qualified(module_index, package);
        }
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<CompletionCandidate> = Vec::new();

        for c in self.origin.complete_methods_for_class(package, Some(module_index)) {
            if !seen.insert(c.label.clone()) {
                continue;
            }
            out.push(CompletionCandidate {
                label: c.label.clone(),
                kind: SymKind::Sub,
                detail: c.detail.or_else(|| Some(format!("from {}", package))),
                insert_text: Some(c.label),
                sort_priority: 10,
                additional_edits: vec![],
                import_fact: None,
                display_override: None,
            });
        }

        let prefix = format!("{}::", package);
        let mut subpaths: Vec<(String, &'static str)> = Vec::new();
        for (name, is_resolved) in self.complete_modules(&prefix) {
            subpaths.push((name, if is_resolved { "indexed" } else { "available" }));
        }
        for c in self.complete(&prefix, false) {
            if !matches!(c.kind, SymKind::Package | SymKind::Class) {
                continue;
            }
            subpaths.push((c.label, "in-file"));
        }
        for (name, hint) in subpaths {
            let suffix = match name.strip_prefix(&prefix) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => continue,
            };
            if !seen.insert(suffix.clone()) {
                continue;
            }
            out.push(CompletionCandidate {
                label: suffix.clone(),
                kind: SymKind::Module,
                detail: Some(hint.to_string()),
                insert_text: Some(suffix),
                sort_priority: 20,
                additional_edits: vec![],
                import_fact: None,
                display_override: None,
            });
        }
        out
    }

    /// Pack half of the qualified-path drill (`fmtx::<cursor>`): the members
    /// of the owner the qualifier names — never the global pool. Per file,
    /// membership is `pack_member_of` over the inline-expanded owner set
    /// (inline namespaces are transparent), plus the nested containers
    /// (sub-namespaces, types) filed directly under the owner. Sources by
    /// tier: OPEN = the origin's own symbols; DEPENDENCY = every cached file
    /// closure-connected to the origin — the same connectivity the
    /// owner-anchored goto-def scan walks, so completion offers exactly what
    /// gd can resolve. Empty when the qualifier resolves nothing (e.g. a
    /// macro-guarded namespace open left members unattributed) — the caller
    /// falls through to the bare-identifier universe, mirroring gd.
    fn complete_pack_qualified(
        &self,
        module_index: &dyn CrossFileLookup,
        owner: &str,
    ) -> Vec<CompletionCandidate> {
        let mask = self.completion_visibility();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<CompletionCandidate> = Vec::new();
        let gather = |fa: &FileAnalysis,
                      header: Option<&str>,
                      seen: &mut std::collections::HashSet<String>,
                      out: &mut Vec<CompletionCandidate>| {
            let owners = pack_inline_owner_set(fa, owner);
            for s in &fa.symbols {
                let nested_container = matches!(s.kind, SymKind::Package | SymKind::Class)
                    && s.package.as_deref().is_some_and(|p| owners.iter().any(|o| o == p));
                if !nested_container && !pack_member_of(fa, s, &owners) {
                    continue;
                }
                // a default-named symbol is structure, not an addressable name
                if s.attributes.iter().any(|a| a == "anonymous") {
                    continue;
                }
                if !seen.insert(s.name.clone()) {
                    continue;
                }
                let detail = match (s.package.as_deref(), header) {
                    (Some(p), Some(h)) if !p.is_empty() => Some(format!("{} — {}", p, h)),
                    (_, Some(h)) => Some(h.to_string()),
                    (Some(p), None) if !p.is_empty() => Some(p.to_string()),
                    _ => None,
                };
                out.push(CompletionCandidate {
                    label: s.name.clone(),
                    kind: s.kind.clone(),
                    detail,
                    insert_text: None,
                    sort_priority: if nested_container { 20 } else { 10 },
                    additional_edits: vec![],
                    import_fact: None,
                    display_override: None,
                });
            }
        };
        if mask.contains(RoleMask::OPEN) {
            gather(self.origin, None, &mut seen, &mut out);
        }
        if mask.contains(RoleMask::DEPENDENCY) {
            if let Some((self_path, visible)) = module_index.visibility_scope() {
                let self_str = self_path.to_string_lossy().into_owned();
                module_index.for_each_cached_file(&mut |cached| {
                    let p = cached.path.to_string_lossy();
                    let connected = visible.contains(p.as_ref())
                        || cached.analysis.include_closure.contains(&self_str);
                    if !connected {
                        return;
                    }
                    let header =
                        cached.path.file_name().map(|f| f.to_string_lossy().into_owned());
                    // The gather reads symbols — closure-connected copies may
                    // be symbol-evicted; the LRU bounds the rehydration.
                    let whole = module_index.whole_present(cached);
                    gather(&whole, header.as_deref(), &mut seen, &mut out);
                });
            }
        }
        out
    }
}

/// Candidates for names a `use` statement makes (or could make) available:
/// explicitly imported symbols, then the imported modules' remaining
/// `@EXPORT`/`@EXPORT_OK` surfaces as auto-add-to-qw candidates. The `seen`
/// set is marked unconditionally so a tier-masked explicit import can never
/// be re-offered by the export walk under the wrong affordance.
fn import_candidates(
    origin: &FileAnalysis,
    idx: &dyn CrossFileLookup,
    mask: RoleMask,
    out: &mut Vec<CompletionCandidate>,
) {
    use crate::file_analysis::{
        format_inferred_type, SymKind as FaSymKind, PRIORITY_AUTO_ADD_QW, PRIORITY_BARE_IMPORT,
        PRIORITY_EXPLICIT_IMPORT,
    };
    let mut seen = std::collections::HashSet::new();

    for import in &origin.imports {
        let cached = idx.get_cached(&import.module_name);

        // Explicitly imported symbols (from the qw list): origin-file names.
        // Dedup/dispatch by LOCAL name (what the user types); resolve detail
        // against REMOTE name (what exists in the source module) so renaming
        // imports like `del` → `delete` show the real doc.
        for is in &import.imported_symbols {
            let local = &is.local_name;
            if !seen.insert(local.clone()) {
                continue;
            }
            if !origin.symbols_named(local).is_empty() {
                continue;
            }
            if !mask.contains(RoleMask::OPEN) {
                continue;
            }
            let whole = cached.as_ref().map(|c| idx.bag_present(c));
            let detail =
                completion_detail_for_import(is.remote(), whole.as_deref(), &import.module_name);
            out.push(CompletionCandidate {
                label: local.clone(),
                kind: FaSymKind::Sub,
                detail: Some(detail),
                insert_text: None,
                sort_priority: PRIORITY_EXPLICIT_IMPORT,
                additional_edits: vec![],
                import_fact: None,
                display_override: None,
            });
        }

        // The module's remaining export surface: dependency-file names.
        if !mask.contains(RoleMask::DEPENDENCY) {
            continue;
        }
        if let Some(ref cached) = cached {
            let fa = &cached.analysis;
            let all_exported: Vec<&String> = if import.imported_symbols.is_empty() {
                // Bare `use Foo;` — offer @EXPORT
                fa.export.iter().collect()
            } else {
                // `use Foo qw(bar)` — offer remaining @EXPORT + @EXPORT_OK
                let mut all = Vec::new();
                all.extend(fa.export.iter());
                all.extend(fa.export_ok.iter());
                all
            };

            for name in all_exported {
                // Skip already-offered (explicitly imported) and locally defined
                if !seen.insert(name.clone()) {
                    continue;
                }
                if !origin.symbols_named(name).is_empty() {
                    continue;
                }

                let rt_prefix = idx
                    .whole_present(cached)
                    .sub_info_view(name)
                    .and_then(|s| s.return_type(None))
                    .map(|rt| format!("→ {} ", format_inferred_type(&rt)))
                    .unwrap_or_default();

                // The FACT: this name can join the existing qw() list at
                // its close paren. The adapter composes the edit; a bare
                // `use Foo;` has no list to join (no fact, no edit).
                let (detail, priority, import_fact) =
                    if let Some(close_pos) = import.qw_close_paren {
                        (
                            format!("{}{} (auto-import)", rt_prefix, import.module_name),
                            PRIORITY_AUTO_ADD_QW,
                            Some(crate::file_analysis::ImportFact::AddToQw {
                                name: name.clone(),
                                qw_close: close_pos,
                            }),
                        )
                    } else {
                        (
                            format!("{}imported from {}", rt_prefix, import.module_name),
                            PRIORITY_BARE_IMPORT,
                            None,
                        )
                    };

                out.push(CompletionCandidate {
                    label: name.clone(),
                    kind: FaSymKind::Sub,
                    detail: Some(detail),
                    insert_text: None,
                    sort_priority: priority,
                    additional_edits: vec![],
                    import_fact,
                    display_override: None,
                });
            }
        }
    }
}

/// Auto-import candidates: every cached exporter's `@EXPORT`/`@EXPORT_OK`
/// surface, each carrying the importable-from FACT (`ImportFact::NewUse`);
/// the adapter composes the `use Module qw(func);` edit at the slot's
/// affordance.
fn unimported_export_candidates(
    origin: &FileAnalysis,
    idx: &dyn CrossFileLookup,
    out: &mut Vec<CompletionCandidate>,
) {
    use crate::file_analysis::{SymKind as FaSymKind, PRIORITY_UNIMPORTED};
    let mut candidates = Vec::new();

    // Already-imported modules are the import walk's job, not this one's.
    let imported_modules: std::collections::HashSet<&str> = origin
        .imports
        .iter()
        .map(|i| i.module_name.as_str())
        .collect();

    idx.for_each_cached(&mut |module_name, cached| {
        if imported_modules.contains(module_name) {
            return;
        }

        let fa = &cached.analysis;
        let all_exported = fa.export.iter().chain(fa.export_ok.iter());
        for name in all_exported {
            // Skip functions already defined locally
            if !origin.symbols_named(name).is_empty() {
                continue;
            }
            candidates.push(CompletionCandidate {
                label: name.clone(),
                kind: FaSymKind::Sub,
                detail: Some(format!("{} (auto-import)", module_name)),
                insert_text: None,
                sort_priority: PRIORITY_UNIMPORTED,
                additional_edits: vec![],
                import_fact: Some(crate::file_analysis::ImportFact::NewUse {
                    module: module_name.to_string(),
                    name: name.clone(),
                }),
                display_override: None,
            });
        }
    });

    // Sort for deterministic order
    candidates.sort_by(|a, b| a.label.cmp(&b.label).then(a.detail.cmp(&b.detail)));
    out.extend(candidates);
}

fn completion_detail_for_import(
    name: &str,
    // The bag-present analysis (`idx.bag_present`) — return types read the
    // bag, and the resident index copy may be evicted.
    whole: Option<&crate::file_analysis::FileAnalysis>,
    module_name: &str,
) -> String {
    use crate::file_analysis::format_inferred_type;
    if let Some(whole) = whole {
        if let Some(sub_info) = whole.sub_info_view(name) {
            if let Some(rt) = sub_info.return_type(None) {
                return format!("→ {} ({})", format_inferred_type(&rt), module_name);
            }
        }
    }
    format!("imported from {}", module_name)
}

/// All `Handler` definitions matching `(owner, name)` across cached modules.
/// A dispatch (`$emitter->emit('ready')`) can target stacked registrations
/// in different files; every hit surfaces so the editor can show a picker.
/// Shared by the materialized-ref path and the query-time `dispatch_at` path
/// so both resolve handlers identically.
fn dispatch_handler_locations(
    owner: &HandlerOwner,
    name: &str,
    module_index: &dyn CrossFileLookup,
) -> Vec<RefLocation> {
    use crate::file_analysis::SymbolDetail;
    let mut locs: Vec<RefLocation> = Vec::new();
    for module_name in module_index.modules_with_symbol(name) {
        let Some(cached) = module_index.get_cached(&module_name) else { continue };
        let whole = module_index.whole_present(&cached);
        for sym in &whole.symbols {
            if sym.name != name {
                continue;
            }
            if let SymbolDetail::Handler { owner: o, .. } = &sym.detail {
                if o == owner {
                    locs.push(RefLocation {
                        key: FileKey::Path(cached.path.clone()),
                        span: sym.selection_span,
                        access: AccessKind::Declaration,
                        rewritable: true,
                        label: None
                    });
                }
            }
        }
    }
    locs
}

/// How a function name relates to an importing `use` statement. Both
/// goto-def and the unresolved-function diagnostic read this one verdict so
/// they can never disagree on whether a name is resolvable as imported
/// (NAV § (c): the divergent-export-surface root cause).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImportResolution {
    /// The name is brought into the caller's namespace: named in `qw(...)`,
    /// pulled in by a `:tag` selector against the producer surface, or
    /// auto-imported by a bare `use Foo;`. Goto-def jumps; the diagnostic
    /// stays silent (the name is genuinely available here).
    Brought,
    /// The name is exported by the imported module but this `use` didn't
    /// bring it in (e.g. a named `qw(other)` that omits it). Goto-def can
    /// still jump to the def; the diagnostic offers the "exported but not
    /// imported" hint.
    ExportedNotBrought,
}

/// Classify a name against a single import. Routes through the consumer
/// evaluator (`imported_names`) so the verdict is exactly "is this name in the
/// bound set this `use` produces" — the single notion of import binding that
/// diagnostics, goto-def, and references all read (NAV § (c)). Returns the
/// resolved verdict plus the REMOTE (origin) name for the matched local name.
///
/// `cached` is the producer's `FileAnalysis` when known; its `export_surface`
/// expands `:tag` selectors and supplies the `@EXPORT` defaults for a bare
/// `use`. When absent (module not yet cached), the evaluator still binds
/// explicitly-named `qw()` imports — those don't need the surface — so an
/// explicit named import is never spuriously flagged while the resolver warms.
fn classify_import(
    import: &crate::file_analysis::Import,
    func_name: &str,
    cached: Option<&crate::file_analysis::CachedModule>,
    module_index: &dyn CrossFileLookup,
) -> Option<(ImportResolution, String)> {
    if let Some(cached) = cached {
        let surface = cached.analysis.export_surface_with_index(module_index);
        let bound = crate::file_analysis::imported_names(import, &surface);
        if let Some((_local, remote)) = bound.iter().find(|(local, _)| local == func_name) {
            return Some((ImportResolution::Brought, remote.clone()));
        }
        // Not bound by this `use`, but on the producer surface → the actionable
        // "exported but not imported" hint (a named `qw(other)` omitting it, or
        // an `@EXPORT_OK` name reached only by a bare `use` — GATE-5).
        if surface.exports(func_name) {
            return Some((ImportResolution::ExportedNotBrought, func_name.to_string()));
        }
        return None;
    }
    // Module not cached yet: only an explicitly-named import can be judged
    // `Brought` without the producer surface (tags / bare-use defaults need it).
    // This keeps a `qw(foo)` import from being flagged while the resolver warms,
    // and never resolves a bare/tagged name it can't actually verify.
    if let Some(sym) = import.imported_symbols.iter().find(|s| s.local_name == *func_name) {
        return Some((ImportResolution::Brought, sym.remote().to_string()));
    }
    None
}

/// Best resolution of `func_name` across all imports: the matched import, its
/// remote name, the resolvability verdict, and — when known — the module path
/// for navigation. `Brought` wins over `ExportedNotBrought` when several
/// imports relate. The single resolvability query goto-def, the diagnostic, and
/// references all read, so they can never disagree on the bound set.
pub(crate) fn resolve_imported_function_classified<'b>(
    analysis: &'b FileAnalysis,
    func_name: &str,
    module_index: &dyn CrossFileLookup,
) -> Option<(&'b crate::file_analysis::Import, Option<PathBuf>, String, ImportResolution)> {
    let mut best: Option<(
        &'b crate::file_analysis::Import,
        Option<PathBuf>,
        String,
        ImportResolution,
    )> = None;
    for import in &analysis.imports {
        let cached = module_index.get_cached(&import.module_name);
        let Some((res, remote)) = classify_import(import, func_name, cached.as_deref(), module_index) else { continue };
        let path = cached.as_ref().map(|c| c.path.clone());
        // `Brought` is the strongest verdict; once found, keep it.
        if matches!(best, Some((_, _, _, ImportResolution::Brought))) {
            continue;
        }
        best = Some((import, path, remote, res));
    }
    best
}

/// Find which import provides a given function name, with a concrete module
/// path to jump to. Returns the matched Import, the module's path, and the
/// REMOTE name (the sub's actual name in the source module — differs from the
/// caller's `func_name` only for renaming imports like `del` → `delete`).
/// Callers use the remote name for whole-view `sub_info_view(...)` lookups so
/// hover/gd/sig-help reach the real sub.
pub(crate) fn resolve_imported_function<'b>(
    analysis: &'b FileAnalysis,
    func_name: &str,
    module_index: &dyn CrossFileLookup,
) -> Option<(&'b crate::file_analysis::Import, PathBuf, String)> {
    // Goto-def needs a concrete module path to jump to.
    resolve_imported_function_classified(analysis, func_name, module_index)
        .and_then(|(import, path, remote, _)| path.map(|p| (import, p, remote)))
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
    /// A per-candidate fact worth surfacing beside the location — a macro
    /// variant's reachability verdict, a delegation see-through note. LSP
    /// `Location` has no label slot so the editor adapter drops it (ordering
    /// conveys rank); the CLI renders it and the gold harness asserts on it.
    pub label: Option<String>,
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
        // it. A `has`/column accessor IS shared down the hierarchy, but its
        // identity is the OWNING class: `owned_accessor` roots the family at
        // `p.class` and its descendants, never upward at a framework ancestor
        // that defines a real same-named `sub` (e.g. an `id` column colliding
        // with `DBIx::Class::PK::id`).
        let target = if p.field_backed {
            TargetRef::method(
                p.bare.clone(),
                p.class.clone(),
                class_analysis,
                module_index,
                OverrideScope::Dispatch,
            )
        } else {
            TargetRef::owned_accessor(
                p.bare.clone(),
                p.class.clone(),
                class_analysis,
                module_index,
            )
        };
        members.push(GroupMember {
            target,
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
        // Name-mapped accessors (`has_size` for attr `size`) are class-owned
        // too — same owner-rooted family as the reader (never a framework
        // ancestor's same-named `sub`).
        members.push(GroupMember {
            target: TargetRef::owned_accessor(
                m.method.clone(),
                p.class.clone(),
                class_analysis,
                module_index,
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
            label: None
        })
        .collect();
    out.extend(pinned_spans.iter().map(|(path, span)| RefLocation {
        key: FileKey::Path(path.clone()),
        span: *span,
        access: AccessKind::Read,
        rewritable: true,
        label: None
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
    !crate::conventions::strip_variable_sigils(new_name.trim()).trim().is_empty()
}

/// Rename edit set for a projection group: every span paired with ITS
/// member's replacement text (bare for plain spellings, re-derived for
/// affixed accessors). Bare-member spans win collisions — a synthesized
/// accessor's decl token IS the group decl the bare edit covers.
#[allow(clippy::too_many_arguments)]
pub fn group_rename_edits(
    files: &FileStore,
    module_index: Option<&dyn CrossFileLookup>,
    origin: &FileKey,
    local_spans: &[Span],
    pinned_spans: &[(PathBuf, Span)],
    members: &[GroupMember],
    bare_new: &str,
    mask: RoleMask,
) -> Vec<(RefLocation, String)> {
    let mut out: Vec<(RefLocation, String)> = local_spans
        .iter()
        .map(|span| {
            (
                RefLocation { key: origin.clone(), span: *span, access: AccessKind::Read, rewritable: true, label: None},
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
                label: None
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
        for loc in refs_to(files, module_index, &m.target, mask) {
            out.push((loc, text.clone()));
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|(loc, _)| seen.insert((key_for_sort(&loc.key), loc.span)));
    out
}

/// Per-process override for the relational retrieval switch — the parity
/// harness toggles this between two projections of one set (an env write
/// there would race other threads reading the env). 0 = defer to the env.
static REF_ROWS_OVERRIDE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn set_ref_rows_override(on: Option<bool>) {
    REF_ROWS_OVERRIDE.store(
        match on {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        },
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// The relational retrieval switch (`docs/adr/relational-ref-index.md`).
/// ON by default — resident index copies are refs-evicted after persist, so
/// the SQL retrieval IS the reference path for them. `PERL_LSP_REF_ROWS=0`
/// forces the resident-only walk (pair it with PERL_LSP_NO_EVICT=1, or
/// evicted-file sites vanish — the parity harness runs exactly that pairing).
fn ref_rows_enabled() -> bool {
    match REF_ROWS_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => return true,
        2 => return false,
        _ => {}
    }
    match std::env::var("PERL_LSP_REF_ROWS") {
        Ok(v) => v != "0",
        Err(_) => true,
    }
}

/// The name keys the relational retrieval probes for `target`: the target
/// name's match key plus every delegation alias's — the same
/// `name_match_key` spelling rows are written under, so retrieval is exactly
/// as generous as the matcher's name checks.
fn retrieval_keys(target: &TargetRef, aliases: &[DelegationAlias]) -> Vec<String> {
    let mut keys = vec![crate::file_analysis::name_match_key(&target.name)];
    for a in aliases {
        let k = crate::file_analysis::name_match_key(&a.name);
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys
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

    // Textual-inclusion extension of the closure gate: a file whose own
    // closure reaches no def path still sees the target when a DIRECT seer
    // includes it (`ae.c: #include "ae_epoll.c"` — the fragment compiles
    // inside the includer's TU with the includer's preamble, so its
    // `zmalloc(...)` calls are real references). One sweep collects the
    // union of the direct seers' closures; membership is the reverse edge.
    // Empty def_paths (no gate — every Perl target) skips the sweep.
    let mut seen_by_inclusion: std::collections::HashSet<String> = Default::default();
    let target_def_ids = def_path_ids(target);
    if !target.def_paths.is_empty() {
        if let Some(idx) = module_index {
            idx.for_each_cached_file(&mut |cached| {
                let own = cached.path.to_string_lossy();
                if file_sees_target_ids(target, &target_def_ids, &cached.analysis, &own) {
                    seen_by_inclusion.extend(cached.analysis.include_closure.iter_strs().map(|a| a.as_ref().to_owned()));
                }
            });
        }
    }
    let gate = |analysis: &FileAnalysis, file_str: &str| {
        file_sees_target_ids(target, &target_def_ids, analysis, file_str)
            || seen_by_inclusion.contains(file_str)
    };

    // Row-narrowing gate: when the relational store is live for a masked
    // dep/workspace tier, its `files` set is the complete "which files hold
    // rows" marker. A file WITH rows but ABSENT from the candidate set has
    // no matching ref/sym row, so — rows over-approximate references — it
    // provably matches nothing; the resident sweeps below skip rehydrating
    // it, leaving only rows-ABSENT files (persistence off, mid-index lag) to
    // the whole-view fallback. Empty set (`PERL_LSP_REF_ROWS=0`, no opener,
    // degraded) ⇒ every file is swept, exactly as before. This is what makes
    // the pack references path cost track candidate count, not tree size.
    // Sweep-narrowing kill-switch (`PERL_LSP_REFS_NARROW=0`), the A/B lever
    // for the row-narrowed backward walk. Answer-preservation verified:
    // abseil narrowed vs swept byte-identical; curl identical either way
    // (its server-warm under-answer PREDATES narrowing — the open-doc
    // cached-only target-minting divergence, ledgered separately in
    // docs/open-forks.md "Answer honesty under index/enrichment windows").
    let narrow_enabled = std::env::var_os("PERL_LSP_REFS_NARROW")
        .map(|v| v != "0")
        .unwrap_or(true);
    let rows_active =
        ref_rows_enabled() && mask.intersects(RoleMask::WORKSPACE | RoleMask::DEPENDENCY);
    // Armed by the relational block below. The sweep-skip is sound ONLY
    // for files that hold rows AND are NOT candidates (provably matchless).
    // A CANDIDATE must never be skipped by the sweeps even though it holds
    // rows: the relational block can fail to RESOLVE it (`cached_by_path`
    // path-spelling gaps under warm-stub registration — observed on curl:
    // server-warm references 4 sites vs the sweep's 155) and an unresolved
    // candidate falls through to the whole-view sweeps for coverage.
    // Empty candidate retrieval leaves narrowing off entirely.
    let mut rows_indexed: std::collections::HashSet<PathBuf> = Default::default();
    let mut candidate_set: std::collections::HashSet<PathBuf> = Default::default();

    // Open files (canonical — workspace entries for open paths are skipped).
    let mut covered_paths: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    if mask.contains(RoleMask::OPEN) {
        files.for_each_open(|url, doc| {
            let url = url.clone();
            if let Ok(p) = url.to_file_path() {
                // Claim the canonical spelling too: candidate rows are keyed
                // canonical, and an open doc reached through a symlinked
                // root must shadow its own persisted generation.
                if let Ok(canon) = std::fs::canonicalize(&p) {
                    covered_paths.insert(canon);
                }
                covered_paths.insert(p);
            }
            // The walk applies visibility: role mask picked the tier, the
            // closure gate decides per file (`file_sees_target_ids`); the
            // matcher below only matches.
            let key = FileKey::Url(url);
            let file_str = canonical_file_str(&key);
            if !gate(&doc.analysis, &file_str) {
                return;
            }
            collect_from_analysis(&key, &doc.analysis, target, &aliases, module_index, &file_str, &mut out);
        });
    } else {
        // Even if open isn't in the mask, track the paths so a WORKSPACE walk
        // doesn't duplicate them (an open file's pre-close state isn't meaningful).
        files.for_each_open(|url, _doc| {
            if let Ok(p) = url.to_file_path() {
                if let Ok(canon) = std::fs::canonicalize(&p) {
                    covered_paths.insert(canon);
                }
                covered_paths.insert(p);
            }
        });
    }

    // Relational retrieval (`docs/adr/relational-ref-index.md`): the files
    // holding name-keyed candidate rows, rehydrated (`refs_present`) and run
    // through the SAME matcher as every resident copy. Runs BEFORE the
    // resident sweep and claims `covered_paths`, so each file is collected
    // from its best copy exactly once; the sweep behind it still contributes
    // declaration-only files and files without rows (degraded, persistence
    // off, mid-index lag) — composition stays at-least-as-complete whether
    // or not resident refs were evicted.
    if rows_active {
        if let Some(idx) = module_index {
            let keys = retrieval_keys(target, &aliases);
            let candidate_paths = idx.ref_candidate_paths(&keys);
            if std::env::var_os("PERL_LSP_REFS_DEBUG").is_some() {
                eprintln!(
                    "[refs-debug] keys={:?} candidates={} narrow={}",
                    keys,
                    candidate_paths.len(),
                    narrow_enabled
                );
            }
            if narrow_enabled && !candidate_paths.is_empty() {
                rows_indexed = idx.ref_indexed_paths();
                candidate_set = candidate_paths.iter().cloned().collect();
            }
            for path in candidate_paths {
                if covered_paths.contains(&path) {
                    continue;
                }
                // Tier attribution: a FileStore workspace entry rides the
                // WORKSPACE role (Perl project files); everything else the
                // rows name lives in a module-index tier (DEPENDENCY —
                // @INC and the pack caches). The mask must admit the
                // candidate's OWN tier, or an EDITABLE rename would walk
                // read-only deps (and vice versa).
                let ws_arc = files
                    .workspace_raw()
                    .get(&path)
                    .map(|e| std::sync::Arc::clone(e.value()));
                let cached = match ws_arc {
                    Some(arc) => {
                        if !mask.contains(RoleMask::WORKSPACE) {
                            continue;
                        }
                        std::sync::Arc::new(crate::file_analysis::CachedModule::new(
                            path.clone(),
                            arc,
                        ))
                    }
                    None => {
                        if !mask.contains(RoleMask::DEPENDENCY) {
                            continue;
                        }
                        match idx.cached_by_path(&path) {
                            Some(cm) => cm,
                            None => continue,
                        }
                    }
                };
                covered_paths.insert(path);
                let key = FileKey::Path(cached.path.clone());
                let file_str = canonical_file_str(&key);
                if !gate(&cached.analysis, &file_str) {
                    continue;
                }
                // The matcher reads refs (usage sites) AND symbols
                // (declaration sites) — take the whole view.
                let full = idx.whole_present(&cached);
                collect_from_analysis(
                    &key, &full, target, &aliases, module_index, &file_str, &mut out,
                );
            }
        }
    }

    // Workspace files.
    if mask.contains(RoleMask::WORKSPACE) {
        for entry in files.workspace_raw().iter() {
            if covered_paths.contains(entry.key()) {
                continue;
            }
            // Shredded AND not a candidate → holds no matching row; skip
            // the whole-view rehydration. Candidates always fall through
            // (the relational block may have failed to resolve them).
            if rows_indexed.contains(entry.key()) && !candidate_set.contains(entry.key()) {
                continue;
            }
            covered_paths.insert(entry.key().clone());
            let key = FileKey::Path(entry.key().clone());
            let file_str = canonical_file_str(&key);
            if !gate(entry.value(), &file_str) {
                continue;
            }
            // Same whole-view routing as the sibling sweeps: a workspace
            // copy with rows persisted is refs+symbols-STRIPPED, and the
            // matcher reading it raw silently drops the file's matches.
            let full = match module_index {
                Some(idx) => {
                    let cached = std::sync::Arc::new(
                        crate::file_analysis::CachedModule::new(
                            entry.key().clone(),
                            std::sync::Arc::clone(entry.value()),
                        ),
                    );
                    idx.whole_present(&cached)
                }
                None => std::sync::Arc::clone(entry.value()),
            };
            collect_from_analysis(&key, &full, target, &aliases, module_index, &file_str, &mut out);
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
                // Same row-narrowing skip as the workspace sweep: shredded
                // but not a candidate ⇒ provably matchless; candidates
                // always fall through.
                if rows_indexed.contains(&cached.path) && !candidate_set.contains(&cached.path) {
                    return;
                }
                let key = FileKey::Path(cached.path.clone());
                let file_str = canonical_file_str(&key);
                if !gate(&cached.analysis, &file_str) {
                    return;
                }
                // Rows-off fallback sweep: copies here may still be
                // symbol-evicted (rows exist, retrieval switched off) —
                // the matcher needs symbols, so take the whole view.
                let full = idx.whole_present(cached);
                collect_from_analysis(&key, &full, target, &aliases, module_index, &file_str, &mut out);
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

/// `textDocument/implementation`: defs of `name` on every class that
/// participates in the target method's dispatch for some concrete
/// descendant — the transitive descendants of the Method target's class
/// PLUS their co-ancestors (sibling parents contributed by multi-parent
/// composition: `load_components`, Moo/Moose `with`, multi-base `use base`).
/// On a role's `requires` marker that's "every composer's def of the
/// contract"; on a class method it's "every override that can win dispatch".
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
    // On a class/package name: the specialization FAMILY view — every spec
    // of the primary template (`formatter` → all `formatter<...>` defs).
    // gr on the primary stays "uses of the primary"; the family is this
    // verb's answer (fork 4, docs/adr/cpp-templates.md).
    if matches!(target.kind, TargetKind::Package) {
        let mut out = specialization_family(origin, module_index, &target.name);
        // A plain base class (not a template primary): its "implementations"
        // are the concrete subclasses — the INHERITS_INV descendants' class
        // def sites. The edge graph gates this: an unrelated same-named nested
        // class (SkipList::Iterator) has no INHERITS edge to the target, so it
        // never appears in the descendant set even though the by-name index
        // holds a Class of the same spelling.
        if let Some(idx) = module_index {
            let probe = crate::graph::GraphView::new(origin, Some(idx));
            let mut descendants: Vec<String> = Vec::new();
            probe.walk(
                crate::graph::Node::Class(target.name.clone()),
                crate::graph::EdgeKindMask::INHERITS_INV,
                &mut |n| {
                    if let crate::graph::Node::Class(c) = n {
                        descendants.push(c.clone());
                    }
                    std::ops::ControlFlow::Continue(())
                },
            );
            for pkg in &descendants {
                for cached in idx.def_candidates(pkg) {
                    let whole = idx.whole_present(&cached);
                    for s in &whole.symbols {
                        if &s.name == pkg && matches!(s.kind, SymKind::Class) {
                            out.push(RefLocation {
                                key: FileKey::Path(cached.path.clone()),
                                span: s.selection_span,
                                access: AccessKind::Declaration,
                                rewritable: false,
                                label: None,
                            });
                        }
                    }
                }
            }
        }
        out.sort_by(|a, b| {
            key_for_sort(&a.key).cmp(&key_for_sort(&b.key)).then_with(|| {
                (a.span.start.row, a.span.start.column)
                    .cmp(&(b.span.start.row, b.span.start.column))
            })
        });
        out.dedup_by(|a, b| file_key_eq(&a.key, &b.key) && a.span == b.span);
        return out;
    }
    // Both class-bearing target kinds seed the dispatch fan-out: a
    // `Method{class}` (call-site cursor) and a `Sub{package: Some}` (cursor
    // ON a `sub NAME` decl inside a package). Perl has no sub/method
    // distinction — any sub in a package is dispatchable as a method — so the
    // decl of `sub update` in `DBIx::Class::Row` is as much an implementation
    // root as an `$obj->update` call whose invocant types to that class.
    let class = match &target.kind {
        TargetKind::Method { class } => class,
        TargetKind::Sub { package: Some(pkg) } => pkg,
        _ => return Vec::new(),
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

    // Mixin/sibling overrides. A concrete class assembles its dispatch table
    // from MULTIPLE parents (Perl multi-parent composition: `load_components`,
    // Moo/Moose `with` roles, `use base` with several bases). An override of
    // the target's method can therefore live on a SIBLING PARENT of a shared
    // descendant — a class that is an ancestor of some concrete descendant of
    // the target yet is NOT itself a descendant of the target, so the
    // INHERITS_INV sweep above never reaches it (DBIC's `Ordered` sits
    // alongside `Row` in `Track`'s MRO, not beneath it). Surface these by
    // walking UP each descendant's full MRO and collecting every co-ancestor:
    // a class that shares a concrete descendant with the target participates
    // in that descendant's dispatch for the method.
    let mut implementers: std::collections::BTreeSet<String> =
        descendants.iter().cloned().collect();
    for d in &descendants {
        probe.walk(
            crate::graph::Node::Class(d.clone()),
            crate::graph::EdgeKindMask::INHERITS,
            &mut |n| {
                if let crate::graph::Node::Class(c) = n {
                    implementers.insert(c.clone());
                }
                std::ops::ControlFlow::Continue(())
            },
        );
    }
    // The target and its own ancestry are the CONTRACT side, not an
    // implementation: goto-def lands on the target itself, and a superclass
    // method sits BEHIND the target in every descendant's MRO (shadowed by the
    // target's own def — it never wins). Exclude both so the verb reports only
    // the classes that override at or ahead of the contract.
    let mut contract_line: std::collections::HashSet<String> =
        std::iter::once(class.clone()).collect();
    probe.walk(
        crate::graph::Node::Class(class.clone()),
        crate::graph::EdgeKindMask::INHERITS,
        &mut |n| {
            if let crate::graph::Node::Class(c) = n {
                contract_line.insert(c.clone());
            }
            std::ops::ControlFlow::Continue(())
        },
    );
    implementers.retain(|p| !contract_line.contains(p));

    let mut out: Vec<RefLocation> = Vec::new();
    for pkg in &implementers {
        // class → home module(s): exact cache key for the common
        // single-package file; the names index covers cross-named and
        // multi-package homes.
        let mut homes: Vec<std::sync::Arc<crate::file_analysis::CachedModule>> = Vec::new();
        if let Some(c) = idx.get_cached(pkg) {
            homes.push(c);
        } else {
            for m in idx.modules_with_symbol(pkg) {
                if let Some(c) = idx.get_cached(&m) {
                    let declares = idx.whole_present(&c).symbols.iter().any(|s| {
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
            let whole = idx.whole_present(&cached);
            for s in &whole.symbols {
                if s.name == target.name
                    && matches!(s.kind, SymKind::Sub | SymKind::Method)
                    && s.package.as_deref() == Some(pkg.as_str())
                {
                    out.push(RefLocation {
                        key: FileKey::Path(cached.path.clone()),
                        span: s.selection_span,
                        access: AccessKind::Declaration,
                        rewritable: true,
                        label: None
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

/// The specialization family of primary template `name`: every spec class's
/// def site, cross-file. Spec NAMES come off the graph's `Specializes` edges
/// (local `FileAnalysis.specializes` + the index's spec map); def sites
/// resolve through the by-name index (spec Class symbols are indexed under
/// their canonical spelling). `rewritable: false` — a spec's selection span
/// is the whole `X<args>` spelling; renaming the primary rewrites the base
/// TOKEN inside it via its PackageRef, never this span wholesale.
fn specialization_family(
    origin: &FileAnalysis,
    module_index: Option<&dyn CrossFileLookup>,
    primary: &str,
) -> Vec<RefLocation> {
    let mut specs: Vec<String> = Vec::new();
    let probe = crate::graph::GraphView::new(origin, module_index);
    probe.walk(
        crate::graph::Node::Class(primary.to_string()),
        crate::graph::EdgeKindMask::SPECIALIZES,
        &mut |n| {
            if let crate::graph::Node::Class(c) = n {
                specs.push(c.clone());
            }
            std::ops::ControlFlow::Continue(())
        },
    );
    let mut out: Vec<RefLocation> = Vec::new();
    for spec in &specs {
        // Def sites resolve through the index alone (the origin file is
        // itself indexed, so its own specs surface with a real path key).
        // `def_candidates` is the by-name candidate table the pack index
        // keys everything on — every file defining this spec spelling.
        let Some(idx) = module_index else { continue };
        for cached in idx.def_candidates(spec) {
            let whole = idx.whole_present(&cached);
            for s in &whole.symbols {
                if &s.name == spec && matches!(s.kind, SymKind::Class) {
                    out.push(RefLocation {
                        key: FileKey::Path(cached.path.clone()),
                        span: s.selection_span,
                        access: AccessKind::Declaration,
                        rewritable: false,
                        label: None
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

/// A macro name whose call sites dispatch to the target through a delegation
/// edge, plus the canonical path of the `#define` that mints the edge. The
/// path is the alias's VISIBILITY key: an unexpanded `IncRef(x)` in file F
/// means `Perl_Inc` only when F's preprocessor would expand it — the `#define`
/// must sit in F's include closure (or F itself). Matching without that gate
/// let every Perl `croak(...)` in a mixed workspace count as a reference to
/// perl5's C `Perl_croak_nocontext` via embed.h's alias.
struct DelegationAlias {
    name: String,
    def_path: String,
}

/// The macro names whose call sites dispatch to `target` through delegation
/// edges (`MacroDef::delegate`), transitively (`#define A(x) B(x)`,
/// `#define B(x) F(x)` — both A and B reach F), each carrying its own
/// `#define`'s file for the per-scanned-file visibility gate. The backward
/// mirror of the forward see-through offer in `pack_macro_definition`. Only
/// callable-name-keyed kinds have a delegation surface; the DEPENDENCY sweep
/// is gated on the mask so a Perl EDITABLE query never touches the dep cache.
/// Sorted for deterministic output.
fn delegation_aliases(
    files: &FileStore,
    module_index: Option<&dyn CrossFileLookup>,
    target: &TargetRef,
    mask: RoleMask,
) -> Vec<DelegationAlias> {
    if !matches!(
        target.kind,
        TargetKind::Sub { .. } | TargetKind::Method { .. } | TargetKind::FileScopeValue
    ) {
        return Vec::new();
    }
    // (alias name, delegate, canonical path of the #define)
    let mut pairs: Vec<(String, String, String)> = Vec::new();
    let mut add = |a: &FileAnalysis, path: &str| {
        for m in &a.macro_defs {
            if let Some(d) = &m.delegate {
                pairs.push((m.name.clone(), d.clone(), path.to_string()));
            }
        }
    };
    // Read-only walk: handlers hold their open-doc READ guard across
    // projections (the set's borrow discipline), so a write lock here
    // deadlocks the moment a diagnostics refresh queues behind it.
    files.for_each_open(|url, doc| {
        let path = url
            .to_file_path()
            .map(|p| {
                std::fs::canonicalize(&p)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|_| url.to_string());
        add(&doc.analysis, &path);
    });
    for entry in files.workspace_raw().iter() {
        add(entry.value(), &entry.key().to_string_lossy());
    }
    if mask.contains(RoleMask::DEPENDENCY) {
        if let Some(idx) = module_index {
            idx.for_each_cached_file(&mut |cached| {
                add(&cached.analysis, &cached.path.to_string_lossy());
            });
        }
    }
    if pairs.is_empty() {
        return Vec::new();
    }
    // Reverse-transitive chase: every name whose delegation chain reaches
    // the target's name. Each alias keeps ALL its def sites (config variants
    // of the same alias live in different headers).
    let mut out: Vec<DelegationAlias> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut frontier: Vec<String> = vec![target.name.clone()];
    while let Some(cur) = frontier.pop() {
        for (n, d, p) in &pairs {
            if *d == cur && *n != target.name {
                if !out.iter().any(|a| a.name == *n && a.def_path == *p) {
                    out.push(DelegationAlias { name: n.clone(), def_path: p.clone() });
                }
                if seen.insert(n.clone()) {
                    frontier.push(n.clone());
                }
            }
        }
    }
    out.sort_by(|a, b| (&a.name, &a.def_path).cmp(&(&b.name, &b.def_path)));
    out
}

/// The identifier under `point` in `source`, or `None` if the cursor is not
/// on a `[A-Za-z0-9_]` word. Byte-scan (macros vanish from the analysis under
/// the expand-and-reparse policy, so the raw word is the reliable key).
pub fn word_at_point(source: &str, point: tree_sitter::Point) -> Option<&str> {
    let cursor = crate::cursor_sentinel::point_to_byte(source, point);
    let b = source.as_bytes();
    let is_id = |c: u8| c == b'_' || c.is_ascii_alphanumeric();
    if cursor > b.len() {
        return None;
    }
    let mut start = cursor;
    while start > 0 && is_id(b[start - 1]) {
        start -= 1;
    }
    let mut end = cursor;
    while end < b.len() && is_id(b[end]) {
        end += 1;
    }
    (start < end).then(|| &source[start..end])
}

/// Is the identifier at `point` CALL-SHAPED — its token immediately followed
/// (skipping whitespace) by `(`? The C preprocessor expands a function-like
/// macro ONLY at this shape, so a PARENLESS token (`OP** p`, the `OP` in a
/// `typedef`) is never a function-like macro's use and a `#define OP(p)` must
/// not claim it. Object-like macros claim regardless (they expand at any
/// occurrence). This is the SITE half of the shape gate; the def's
/// `params.is_some()` is the candidate half.
pub(crate) fn token_is_call_shaped(source: &str, point: tree_sitter::Point) -> bool {
    let cursor = crate::cursor_sentinel::point_to_byte(source, point);
    let b = source.as_bytes();
    let is_id = |c: u8| c == b'_' || c.is_ascii_alphanumeric();
    if cursor > b.len() {
        return false;
    }
    let mut end = cursor;
    while end < b.len() && is_id(b[end]) {
        end += 1;
    }
    while end < b.len() && b[end].is_ascii_whitespace() {
        end += 1;
    }
    end < b.len() && b[end] == b'('
}

/// The full `Base<...>` spelling at a type ref: `span` covers the base token;
/// when `<` follows immediately, extend to the balanced `>` and canonicalize
/// (`canonical_template_spelling` — the identity key specs are filed under).
/// `None` for plain type refs, unbalanced brackets, or a statement boundary
/// before the close (a stray comparison, not template args).
fn template_instance_spelling(source: &str, span: Span) -> Option<String> {
    let start = crate::cursor_sentinel::point_to_byte(source, span.start);
    let mut i = crate::cursor_sentinel::point_to_byte(source, span.end);
    let b = source.as_bytes();
    if i >= b.len() || b[i] != b'<' {
        return None;
    }
    let mut depth = 0usize;
    while i < b.len() {
        match b[i] {
            b'<' => depth += 1,
            b'>' => {
                depth -= 1;
                if depth == 0 {
                    return Some(crate::file_analysis::canonical_template_spelling(
                        &source[start..=i],
                    ));
                }
            }
            b';' | b'{' | b'}' => return None,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Is `s` addressable as `owner::<its name>` — the owner-membership predicate
/// shared by owner-anchored goto-def (`member_def_location`) and the
/// qualified-completion gather (`complete_pack_qualified`), so "resolvable"
/// and "offered" never drift apart. Methods/subs key by package; a data
/// member (or enum constant) must be the owner's OWN content, not a sub-body
/// local carrying the owner as sticky package.
/// Membership of `s` in an owner set already expanded through
/// inline-namespace transparency (`pack_inline_owner_set`). The set is passed
/// in — never a single raw package — so goto-def's owner lookup
/// (`member_def_location`) and completion agree with the references gate that
/// a symbol filed under an `inline namespace head` satisfies a query keyed on
/// its transparent parent `absl`.
fn pack_member_of(
    fa: &crate::file_analysis::FileAnalysis,
    s: &crate::file_analysis::Symbol,
    owners: &[String],
) -> bool {
    let in_owners = |p: Option<&str>| p.is_some_and(|p| owners.iter().any(|o| o == p));
    match s.kind {
        SymKind::Method | SymKind::Sub => in_owners(s.package.as_deref()),
        SymKind::Variable | SymKind::Field | SymKind::Enumerator => {
            if !fa.symbol_is_class_content(s) {
                return false;
            }
            if in_owners(s.package.as_deref()) {
                return true;
            }
            // Unscoped-enum leak: `dynamic::STRING` / `level::info`
            // where the enumerator's enum is nested in a class OR
            // namespace `owner`. C++ makes an unscoped enum's
            // enumerators members of EVERY enclosing named scope,
            // addressable by that scope's name — but extraction files
            // the enumerator under its tightest container (the enum),
            // so the direct package match above misses the outer
            // scope. Bridge it structurally: the enumerator's span
            // lives inside a container symbol named `owner`
            // (span-contained, and not the enumerator itself). Works
            // whether the scope is a struct (`dynamic`) or a namespace
            // (`level`), without depending on how either tags package.
            matches!(s.kind, SymKind::Enumerator)
                && fa.symbols.iter().any(|c| {
                    owners.iter().any(|o| o == &c.name)
                        && c.span != s.span
                        && (c.span.start.row, c.span.start.column)
                            <= (s.span.start.row, s.span.start.column)
                        && (s.span.end.row, s.span.end.column)
                            <= (c.span.end.row, c.span.end.column)
                })
        }
        _ => false,
    }
}

/// `owner` plus every inline namespace nested under it (transitively), per
/// C++'s inline-namespace transparency: `namespace fmt { inline namespace
/// v11 { ... } }` makes `v11`'s members addressable as `fmt::` members.
/// Extraction tags inline namespaces with the "inline" attribute; a plain
/// nested namespace never joins the set (its members need their own
/// qualifier).
fn pack_inline_owner_set(fa: &crate::file_analysis::FileAnalysis, owner: &str) -> Vec<String> {
    let mut owners = vec![owner.to_string()];
    loop {
        let mut grew = false;
        for s in &fa.symbols {
            if s.kind == SymKind::Package
                && s.attributes.iter().any(|a| a == "inline")
                && s.package.as_deref().is_some_and(|p| owners.iter().any(|o| o == p))
                && !owners.contains(&s.name)
            {
                owners.push(s.name.clone());
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    owners
}

/// The `::`-qualifier owning the identifier under `point` — `dynamic` for the
/// cursor anywhere in `STRING` of `dynamic::STRING`. Walks back to the token
/// start (like `word_at_point`), then scans a leading `::` scope.
/// `None` when the token has no leading `::` scope.
pub(crate) fn qualifier_at_point(source: &str, point: tree_sitter::Point) -> Option<&str> {
    let cursor = crate::cursor_sentinel::point_to_byte(source, point);
    let b = source.as_bytes();
    let is_id = |c: u8| c == b'_' || c.is_ascii_alphanumeric();
    if cursor > b.len() {
        return None;
    }
    let mut start = cursor;
    while start > 0 && is_id(b[start - 1]) {
        start -= 1;
    }
    if start < 2 || !source.is_char_boundary(start) || &source[start - 2..start] != "::" {
        return None;
    }
    let e = start - 2;
    let mut s = e;
    while s > 0 && is_id(b[s - 1]) {
        s -= 1;
    }
    (s < e).then(|| &source[s..e])
}

/// Every `#define` of `word` across the origin file + the cached modules,
/// ranked config-active first by the SAME total order goto-def and hover both
/// consume (`docs/adr/macro-handling.md`): reachability rank, then
/// (path, row, col) so the winner is deterministic across processes (the
/// cache iterates in randomized DashMap order). Empty when `word` names no
/// macro. This is the one place the variant set is gathered +
/// reachability-classified — `definitions()` returns all of them (never
/// pruned), hover walks the top one's alias chain to its leaf.
pub(crate) fn ranked_macro_variants(
    analysis: &FileAnalysis,
    word: &str,
    origin_key: &FileKey,
    module_index: &dyn CrossFileLookup,
) -> Vec<(crate::file_analysis::MacroDef, FileKey, crate::cpp_macro_model::Reachability)> {
    use crate::cpp_macro_model::classify;
    use crate::file_analysis::MacroDef;
    use std::collections::HashSet;

    // One pass over every cached module + this file: collect the def sites for
    // `word` (config variants live in different headers — win32.h vs perl.h; we
    // keep them ALL, never the last-writer only) AND the reachability config
    // (the whole macro universe). Enumerating the cache directly is robust to a
    // cold reverse index — `modules_with_symbol` can be empty before it warms.
    let mut sites: Vec<(MacroDef, FileKey)> = Vec::new();
    let mut seen: HashSet<(PathBuf, usize, usize)> = HashSet::new();
    let mut defined: HashSet<String> = HashSet::new();
    let mut universe: HashSet<String> = HashSet::new();
    let mut push = |m: &MacroDef, k: &FileKey, sites: &mut Vec<(MacroDef, FileKey)>| {
        let key = (key_for_sort(k), m.selection_span.start.row, m.selection_span.start.column);
        if seen.insert(key) {
            sites.push((m.clone(), k.clone()));
        }
    };
    let note = |m: &MacroDef, defined: &mut HashSet<String>, universe: &mut HashSet<String>| {
        universe.insert(m.name.clone());
        if m.guards.is_empty() {
            defined.insert(m.name.clone());
        }
    };
    for m in &analysis.macro_defs {
        note(m, &mut defined, &mut universe);
        if m.name == word {
            push(m, origin_key, &mut sites);
        }
    }
    // Per-FILE sweep: the name-keyed cache view both repeats files and hides
    // a file that lost every name tie.
    module_index.for_each_cached_file(&mut |cached| {
        let file_key = FileKey::Path(cached.path.clone());
        for m in &cached.analysis.macro_defs {
            note(m, &mut defined, &mut universe);
            if m.name == word {
                push(m, &file_key, &mut sites);
            }
        }
    });

    if sites.is_empty() {
        return Vec::new();
    }

    // The include-guard idiom `#ifndef X … #define X … #endif` guards a macro's
    // definition on its OWN not-yet-defined-ness. At that guard X is not yet
    // defined, so X's own name must not count as `defined` when ranking X's
    // variants — else every arm reads as unreachable. General over the pattern,
    // not a per-name rule.
    defined.remove(word);
    // Toolchain predefined macros (`__GNUC__`, …) are ON here exactly as they
    // are in build-side variant selection — navigation and minting share the
    // one seeding point so they can't disagree on which arm is Active.
    let cfg = crate::cpp_reparse::known_config_with_toolchain(defined, universe);

    // Rank, active-first. Never prune — a lower-ranked (e.g. win32) def stays,
    // labeled. The secondary (path, line, col) key is a TOTAL order so the
    // result is deterministic across processes.
    let mut ranked: Vec<(MacroDef, FileKey, _)> = sites
        .into_iter()
        .map(|(m, k)| {
            let r = classify(&m.guards, &cfg);
            (m, k, r)
        })
        .collect();
    ranked.sort_by(|(ma, ka, ra), (mb, kb, rb)| {
        ra.rank()
            .cmp(&rb.rank())
            .then_with(|| key_for_sort(ka).cmp(&key_for_sort(kb)))
            .then_with(|| ma.selection_span.start.row.cmp(&mb.selection_span.start.row))
            .then_with(|| ma.selection_span.start.column.cmp(&mb.selection_span.start.column))
    });
    ranked
}

/// Resolve a pack-language symbol NAME (a delegate callee, a free function) to
/// its def location — local symbols and the cross-file index, preferring a
/// DEFINITION over a prototype: a definition's body mints a scope spanning
/// the symbol (the universal `(function_definition) @scope`), a declaration
/// doesn't, so `fix_optchain` see-through lands in op.c, not proto.h. Ties
/// break local-first then (path, position) so the pick is deterministic
/// across the cache's randomized iteration order.
fn pack_symbol_def_location(
    analysis: &FileAnalysis,
    origin_key: &FileKey,
    name: &str,
    module_index: &dyn CrossFileLookup,
) -> Option<RefLocation> {
    let wanted = |k: &SymKind| matches!(k, SymKind::Sub | SymKind::Variable | SymKind::Class);
    let has_body = |a: &FileAnalysis, s: &crate::file_analysis::Symbol| {
        a.scopes.iter().any(|sc| sc.span == s.span)
    };
    // (bodied, local, path, row, col) — the bodied/local flags are inverted
    // in the sort below so `true` ranks first.
    let mut candidates: Vec<(bool, bool, PathBuf, usize, usize, RefLocation)> = Vec::new();
    for sym in analysis.symbols.iter().filter(|s| s.name == name && wanted(&s.kind)) {
        candidates.push((
            has_body(analysis, sym),
            true,
            key_for_sort(origin_key),
            sym.selection_span.start.row,
            sym.selection_span.start.column,
            RefLocation {
                key: origin_key.clone(),
                span: sym.selection_span,
                access: AccessKind::Declaration,
                rewritable: true,
                label: None,
            },
        ));
    }
    // The FULL candidate table for `name` — a definition legitimately lives
    // in a file the one-winner `get_cached` view (or the include closure)
    // never serves (`Perl_fix_optchain`'s body is in peep.c; proto.h wins
    // the scoped lookup).
    let mut seen_paths: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for cached in module_index.def_candidates(name) {
        if !seen_paths.insert(cached.path.clone()) {
            continue;
        }
        let whole = module_index.whole_present(&cached);
        for sym in whole.symbols.iter().filter(|s| s.name == name && wanted(&s.kind)) {
            candidates.push((
                has_body(&whole, sym),
                false,
                cached.path.clone(),
                sym.selection_span.start.row,
                sym.selection_span.start.column,
                RefLocation {
                    key: FileKey::Path(cached.path.clone()),
                    span: sym.selection_span,
                    access: AccessKind::Declaration,
                    rewritable: true,
                    label: None,
                },
            ));
        }
    }
    candidates.sort_by(|a, b| {
        b.0.cmp(&a.0) // bodied first
            .then_with(|| b.1.cmp(&a.1)) // then local
            .then_with(|| a.2.cmp(&b.2))
            .then_with(|| (a.3, a.4).cmp(&(b.3, b.4)))
    });
    candidates.into_iter().next().map(|c| c.5)
}

pub(crate) fn key_for_sort(k: &FileKey) -> PathBuf {
    match k {
        FileKey::Path(p) => p.clone(),
        FileKey::Url(u) => u.to_file_path().unwrap_or_else(|_| PathBuf::from(u.as_str())),
    }
}

pub(crate) fn file_key_eq(a: &FileKey, b: &FileKey) -> bool {
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

/// Does `name` name a `#define` anywhere the origin can reach — its own macro
/// table or any cached def candidate? The macro-identity discriminator for the
/// canonical `FileScopeValue` lane: a function-like macro's occurrences appear
/// as Sub-shaped decls/calls (left unexpanded) AND re-minted Variable reads
/// (expanded-and-erased), and every spelling must mint the SAME target or gr
/// sweeps only its own lane. Perl analyses carry no `macro_defs`, and the Perl
/// hub's name-keyed cache holds no macro tables, so Perl cursors never enter.
fn names_visible_macro(
    name: &str,
    origin: &FileAnalysis,
    idx: Option<&dyn CrossFileLookup>,
) -> bool {
    origin.names_macro_def(name, None)
        || idx.is_some_and(|i| {
            i.def_candidates(name)
                .iter()
                .any(|c| c.analysis.names_macro_def(name, None))
        })
}

/// Namespace-aware package agreement. Exact equality is the total rule (Perl
/// packages are absolute). `relative` — true only for closure-carrying (pack)
/// analyses — adds C++'s relative-lookup semantics: a call's qualifier and a
/// def's namespace both carry only their innermost segment, so tails compare;
/// and a side with NO attribution (the macro-guarded-namespace-open gap, or a
/// plain unqualified call) matches rather than silently dropping the site —
/// references bias to recall under partial attribution, and the `def_paths`
/// closure gate has already pinned file connectivity.
fn pkg_agrees(relative: bool, a: Option<&str>, b: Option<&str>) -> bool {
    if a == b {
        return true;
    }
    if !relative {
        return false;
    }
    match (a, b) {
        (Some(x), Some(y)) => {
            x.rsplit("::").next().unwrap_or(x) == y.rsplit("::").next().unwrap_or(y)
        }
        _ => true,
    }
}

/// The visibility identity (`TargetRef::def_paths`) of a pack-language target
/// keyed on `name` (a class for member/enum-constant targets, the bare value
/// name for `FileScopeValue`): every def candidate closure-connected to the
/// ORIGIN — the origin file itself when it defines the name, candidates the
/// origin's closure reaches (forward: the included header), and candidates
/// whose own closure reaches back to the origin (reverse: the `.c` TU defining
/// the function whose extern decl the origin header carries — a definition
/// legitimately lives outside every consumer's closure). Empty when the lookup
/// carries no scope (Perl, or an unscoped caller): no gate, exactly the
/// pre-existing behavior.
fn pack_def_paths(
    name: &str,
    origin_defines: bool,
    idx: Option<&dyn CrossFileLookup>,
) -> Vec<String> {
    let Some(idx) = idx else { return Vec::new() };
    let Some((self_path, visible)) = idx.visibility_scope() else {
        return Vec::new();
    };
    let self_str = self_path.to_string_lossy().into_owned();
    let mut out: Vec<String> = Vec::new();
    if origin_defines {
        out.push(self_str.clone());
    }
    for c in idx.def_candidates(name) {
        let p = c.path.to_string_lossy().into_owned();
        // A `#define` of the name anywhere joins unconditionally — config
        // variants of one conceptual macro live in disjoint headers (win32.h
        // vs the unix header) and the forward lane's never-prune rule keeps
        // them one identity. Non-macro values (globals, statics) stay
        // closure-strict: two unrelated `static int counter` TUs are two
        // targets.
        if c.analysis.names_macro_def(name, None)
            || visible.contains(&p)
            || c.analysis.include_closure.contains(&self_str)
        {
            out.push(p);
        }
    }
    out.sort();
    out.dedup();
    out
}

/// `pack_def_paths` unioned over a Method target's class set (`class` +
/// `method_classes` — typedef aliases like perl5's `OP` ↔ `struct op` each
/// have their own defining header). A member's visibility IS its class's:
/// a file sees `op_type` iff it sees `struct op`'s definition.
fn pack_class_def_paths(
    target: &TargetRef,
    origin: &FileAnalysis,
    idx: Option<&dyn CrossFileLookup>,
) -> Vec<String> {
    let TargetKind::Method { class } = &target.kind else {
        return Vec::new();
    };
    let mut classes: Vec<&str> = vec![class.as_str()];
    classes.extend(target.method_classes.iter().map(|c| c.as_str()));
    classes.sort();
    classes.dedup();
    let origin_declares = |c: &str| {
        origin
            .symbols
            .iter()
            .any(|s| matches!(s.kind, SymKind::Class) && s.name == c)
    };
    let mut out: Vec<String> = Vec::new();
    for c in classes {
        out.extend(pack_def_paths(c, origin_declares(c), idx));
    }
    out.sort();
    out.dedup();
    out
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
///
/// The covering ref must spell a DIFFERENT identifier (`$m`, `EVT`) than the
/// target: a bare enum/global read is itself a `Variable` ref whose token IS
/// the literal name — that's the collected use, not a fold, and it must stay
/// rewritable. (Perl variable names carry their sigil, so they can never
/// coincide with a callable name.)
fn span_is_folded_name(
    analysis: &FileAnalysis,
    span: Span,
    include_calls: bool,
    literal_name: &str,
) -> bool {
    analysis.refs.iter().any(|r| {
        (matches!(r.kind, RefKind::Variable | RefKind::ContainerAccess)
            || (include_calls && matches!(r.kind, RefKind::FunctionCall { .. })))
            && r.span == span
            && r.target_name != literal_name
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
            // decl). Dispatch keeps the strict single-scope match. Pack
            // analyses compare namespace-aware (`pkg_agrees`), recovering an
            // unattributed def's namespace positionally so a `detail::` def
            // still declares its `detail`-scoped target.
            let relative = !analysis.include_closure.is_empty();
            let recovered = match (sym.package.as_deref(), relative) {
                (None, true) => analysis.enclosing_package_of(&sym.span),
                _ => None,
            };
            let sym_pkg = sym.package.as_deref().or(recovered.as_deref());
            let in_scope = pkg_agrees(relative, sym_pkg, package.as_deref())
                || (target.scope == OverrideScope::Hierarchy
                    && target
                        .method_classes
                        .iter()
                        .any(|c| Some(c.as_str()) == sym_pkg));
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
        // member-block role's Class), as is a file-scope global's def. A
        // Sub/Method symbol elsewhere is an unexpanded function-like macro
        // USE parsed as a declaration (`int x ABSL_GUARDED_BY(mu);`) — the
        // preprocessor would expand that token, so it joins the same
        // identity; the `def_paths` gate already pinned this file as one
        // that sees the macro.
        TargetKind::FileScopeValue => {
            analysis.names_macro_def(&sym.name, Some(sym.selection_span))
                || analysis.symbol_is_file_scope_value(sym)
                || ((!analysis.include_closure.is_empty() || !analysis.macro_defs.is_empty())
                    && matches!(sym.kind, SymKind::Sub | SymKind::Method))
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
    files.for_each_open(|_url, doc| {
        if doc.analysis.symbols.iter().any(|s| symbol_defines_target(s, target, &doc.analysis)) {
            found_in_editable = true;
        }
    });
    // Workspace copies may be symbol-evicted (an empty vec here is "on
    // disk", not "declares nothing") — the resident scan covers whole
    // copies; evicted ones are checked via the row-store candidate filter
    // below (a couple of rehydrations, never a whole-tree decode).
    if !found_in_editable {
        for entry in files.workspace_raw().iter() {
            if entry.value().symbols_are_evicted() {
                continue;
            }
            if entry.value().symbols.iter().any(|s| symbol_defines_target(s, target, entry.value())) {
                found_in_editable = true;
                break;
            }
        }
    }
    if !found_in_editable {
        if let Some(idx) = module_index {
            let keys = retrieval_keys(target, &[]);
            for path in idx.ref_candidate_paths(&keys) {
                let Some(arc) = files
                    .workspace_raw()
                    .get(&path)
                    .map(|e| std::sync::Arc::clone(e.value()))
                else {
                    continue;
                };
                if !arc.symbols_are_evicted() {
                    continue; // the resident scan already judged it
                }
                let cm = std::sync::Arc::new(crate::file_analysis::CachedModule::new(
                    path.clone(),
                    arc,
                ));
                let whole = idx.whole_present(&cm);
                if whole.symbols.iter().any(|s| symbol_defines_target(s, target, &whole)) {
                    found_in_editable = true;
                    break;
                }
            }
        }
    }
    // A class-keyed Method target whose decl we can't see in editable
    // space (cross-file synthesized accessor, parent in @INC) still wins
    // EDITABLE if the *class* is a workspace package — the callers we
    // care about are project files. Fall back to the module index only
    // when nothing project-side claims it.
    if !found_in_editable {
        if let (TargetKind::Method { class }, Some(idx)) = (&target.kind, module_index) {
            let declares_class = |fa: &FileAnalysis| {
                fa.symbols.iter().any(|s| {
                    matches!(s.kind, SymKind::Package | SymKind::Class) && s.name == *class
                })
            };
            let mut declared_in_workspace = false;
            for entry in files.workspace_raw().iter() {
                if !entry.value().symbols_are_evicted() && declares_class(entry.value()) {
                    declared_in_workspace = true;
                    break;
                }
            }
            if !declared_in_workspace {
                let keys = vec![crate::file_analysis::name_match_key(class)];
                for path in idx.ref_candidate_paths(&keys) {
                    let Some(arc) = files
                        .workspace_raw()
                        .get(&path)
                        .map(|e| std::sync::Arc::clone(e.value()))
                    else {
                        continue;
                    };
                    if !arc.symbols_are_evicted() {
                        continue;
                    }
                    let cm = std::sync::Arc::new(crate::file_analysis::CachedModule::new(
                        path.clone(),
                        arc,
                    ));
                    if declares_class(&idx.whole_present(&cm)) {
                        declared_in_workspace = true;
                        break;
                    }
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
                label: None
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
                    label: None
                });
            }
        } else if r.target_name == name && r.resolves_to.is_some_and(is_our_decl) {
            // Unqualified — only this package's `our` var (resolved in-file).
            out.push(RefLocation {
                key: key.clone(),
                span: tail(r.span),
                access: r.access,
                rewritable: true,
                label: None
            });
        }
    }
}

/// The per-query half of the visibility gate: each def_path's global path
/// id, resolved ONCE — the per-candidate test is then lock-free binary
/// search (`contains_id`). `None` = that def_path is in no closure at all.
fn def_path_ids(target: &TargetRef) -> Vec<Option<u32>> {
    target
        .def_paths
        .iter()
        .map(|d| crate::file_analysis::path_intern::lookup_id(d))
        .collect()
}

fn file_sees_target_ids(
    target: &TargetRef,
    ids: &[Option<u32>],
    analysis: &FileAnalysis,
    file_str: &str,
) -> bool {
    target.def_paths.is_empty()
        || target.def_paths.iter().zip(ids).any(|(d, id)| {
            d == file_str || id.is_some_and(|id| analysis.include_closure.contains_id(id))
        })
}

/// A `FileKey`'s canonical path string — the spelling the visibility facts
/// (`def_paths`, alias def sites, include closures) are keyed in.
fn canonical_file_str(key: &FileKey) -> String {
    let file_path = key_for_sort(key);
    std::fs::canonicalize(&file_path)
        .unwrap_or(file_path)
        .to_string_lossy()
        .into_owned()
}

fn collect_from_analysis(
    key: &FileKey,
    analysis: &FileAnalysis,
    target: &TargetRef,
    aliases: &[DelegationAlias],
    module_index: Option<&dyn CrossFileLookup>,
    file_str: &str,
    out: &mut Vec<RefLocation>,
) {
    use crate::file_analysis::HashKeyOwner;

    // An alias applies in THIS file only if its `#define` is visible here
    // (macro expansion requires inclusion). Files of another language have
    // no pack closure, so they can never match an alias — the cross-language
    // pollution gate.
    let visible_aliases: Vec<&DelegationAlias> = aliases
        .iter()
        .filter(|a| {
            a.def_path == file_str
                || analysis.include_closure.contains(&a.def_path)
        })
        .collect();

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
            // SAFETY: scoped_storage was just set to Some(..) on the line above,
            // in this same match arm — a lifetime-extension idiom, not a fallible read.
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

    // Pack (closure-carrying) files speak C++'s relative name lookup and may
    // carry only partial namespace attribution — `pkg_agrees` reads this.
    let relative_ns = !analysis.include_closure.is_empty();
    // Bare unresolved reads count as uses of a Method target only when the
    // member is an enum-constant shape (its name hoists into the enclosing
    // scope). Receiver-reached members (struct fields, methods) are matched
    // through their call sites — a bare same-named token elsewhere is noise
    // (the `formatter::format` 1621-hit sweep). Resolved once per scanned
    // file, under this file's own closure scope.
    let bare_constant_member = match &target.kind {
        TargetKind::Method { class } => {
            pack_member_of_class(&target.name, class, analysis, module_index).unwrap_or(false)
        }
        _ => false,
    };
    // A FileScopeValue whose name is a macro THIS file can see: any resolved
    // same-named read here is a macro use (the preprocessor would have
    // expanded the token), even when it bound to an unexpanded-use artifact
    // symbol. A GLOBAL-flavored target keeps the strict resolved-symbol
    // check, so a shadowing local named like the global stays out.
    let macro_visible_here = matches!(target.kind, TargetKind::FileScopeValue)
        && names_visible_macro(&target.name, analysis, module_index);

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
    let rewritable_at = |span: Span| {
        !(foldable && span_is_folded_name(analysis, span, folds_through_calls, &target.name))
    };

    // Include declaration spans when this file defines the target.
    for sym in &analysis.symbols {
        if symbol_defines_target(sym, target, analysis) {
            out.push(RefLocation {
                key: key.clone(),
                span: sym.selection_span,
                access: AccessKind::Declaration,
                rewritable: rewritable_at(sym.selection_span),
                label: None
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
            && visible_aliases
                .iter()
                .any(|a| a.name == r.unqualified_target_name());
        if !name_matches && !alias_matched {
            continue;
        }
        // Sub + Method both match any call into that scope — function
        // or method shape — per the "same callable, two shapes"
        // invariant. Filter is a single scope comparison.
        let matches_kind = alias_matched || match (&target.kind, &r.kind) {
            (TargetKind::Sub { .. } | TargetKind::Method { .. },
             RefKind::FunctionCall { resolved_package }) => {
                // callable_scope_for_refs is derived from the same target.kind
                // match above; a mismatch means malformed input rather than a
                // real match, so skip this ref instead of asserting the invariant.
                let Some(scope) = callable_scope_for_refs.as_ref() else {
                    continue;
                };
                // Under Hierarchy a bare call into ANY family class matches (the
                // whole override family); Dispatch keeps the strict single
                // scope. A bare imported call the single-file walk couldn't pin
                // (`use Bank;` auto-imports `@EXPORT`, invisible at build) has
                // `resolved_package: None` — re-derive it here, where the index
                // is in hand.
                // Relative-namespace semantics apply to namespace-scoped Subs
                // only: a Method target's scope is a CLASS, which an
                // unqualified call can't name-look-up into from outside — the
                // tolerance would re-open the bare-name sweep on members.
                let ns_relative = relative_ns && matches!(target.kind, TargetKind::Sub { .. });
                let pkg_matches = |pkg: &Option<String>| {
                    pkg_agrees(ns_relative, pkg.as_deref(), scope.as_deref())
                        // Inline-namespace transparency, BOTH directions. A
                        // qualified `mylib::is_thing` / `absl::X` keys on the
                        // transparent parent while the def sits under an inline
                        // child (`v1`, `head`); an UNQUALIFIED in-namespace use
                        // is the mirror — its enclosing owner is the inline
                        // CHILD (`v1`) while the def is attributed to the parent
                        // (`mylib`) whenever the child namespace was opened by a
                        // macro the sticky context never recorded. Expanding
                        // only one side matches the first but drops the second
                        // (the def-anchored gr asymmetry). Expand BOTH and test
                        // for a shared owner: a parent's set contains its inline
                        // children, so parent↔child agrees whichever side names
                        // the parent. Unrelated namespaces share nothing.
                        || match (pkg.as_deref(), scope.as_deref()) {
                            (Some(named), Some(actual)) => {
                                let a = pack_inline_owner_set(analysis, named);
                                let b = pack_inline_owner_set(analysis, actual);
                                a.iter().any(|o| b.contains(o))
                            }
                            _ => false,
                        }
                        || (target.scope == OverrideScope::Hierarchy
                            && target.method_classes.iter().any(|c| Some(c) == pkg.as_ref()))
                };
                match resolved_package {
                    Some(_) => pkg_matches(resolved_package),
                    None => {
                        // Unqualified + unresolved: derive the caller's own
                        // enclosing namespace positionally (pack) — a plain
                        // `vformat_to(...)` inside `namespace fmt` looks up
                        // fmt's, not detail's — before falling to the
                        // no-package comparison.
                        let derived = analysis.deferred_call_package(r, module_index).or_else(
                            || {
                                relative_ns
                                    .then(|| analysis.enclosing_package_of(&r.span))
                                    .flatten()
                            },
                        );
                        pkg_matches(&derived)
                    }
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
                // Same derived-from-the-same-match invariant as the FunctionCall
                // arm above; skip rather than assert if it ever doesn't hold.
                let Some(scope) = callable_scope_for_refs.as_ref() else {
                    continue;
                };
                let method = r.unqualified_target_name();
                {
                    let resolved_class = match r.resolved_method_target.as_ref() {
                        // The frozen edge can carry an UNRESOLVED DBIC source
                        // moniker (`Artist`) when it was stamped at build with
                        // no index (a closed call-site file — enrichment
                        // re-stamps OPEN docs only). Map it to the FQ result
                        // class here, index in hand, so `$row->cds` sites match
                        // the same target goto-def reaches. No-op for a class
                        // that already resolves.
                        Some(edge) => Some(analysis.resolve_dbic_source_moniker(
                            edge.invocant_class().to_string(),
                            None,
                            module_index,
                        )),
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
            // A pack-language enum constant read by BARE name (`x = OP_SCOPE`,
            // `case OP_SCOPE:`) — a `Variable` ref the generic goto-def
            // resolves to this def by name (the value-read half of the shared
            // Variable/Field DEF). An UNRESOLVED read counts only when the
            // member's name actually hoists into the enclosing scope
            // (`bare_constant_member`) — receiver-reached members (fields,
            // methods) never match bare tokens, or every stray `format` in
            // the workspace joins the set. A resolved read counts only when
            // it binds the target's own class content (a genuinely-local
            // variable — even one carrying the class as sticky package —
            // stays out via the structural gate).
            (TargetKind::Method { class }, RefKind::Variable) => match r.resolves_to {
                None => target.bare_constant || bare_constant_member,
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
            // The same bare-constant gate for a TYPE-guessed token: a pack
            // grammar parses a value in a type/value-ambiguous slot (a
            // template argument `MakeError<StatusCode::kNotFound>`) as a type,
            // minting a PackageRef — for an enum-constant member that token is
            // a use (the value hoists, exactly like the unresolved bare read
            // above). Receiver-reached members stay out on the same gate.
            (TargetKind::Method { .. }, RefKind::PackageRef) => {
                target.bare_constant || bare_constant_member
            }
            // A file-scope value's uses, all bare-name-keyed like its forward
            // resolutions: a value read (object-like macro / global / enum
            // constant), a type-position token (a type-alias `#define` used as
            // a declared type), or an unresolved call (function-like macro —
            // a package-pinned call belongs to that package's sub, not here).
            (TargetKind::FileScopeValue, RefKind::Variable) => match r.resolves_to {
                None => true,
                Some(id) => {
                    let s = analysis.symbol(id);
                    macro_visible_here
                        || analysis.names_macro_def(&s.name, Some(s.selection_span))
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
                label: None
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
                    label: None
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
                    label: None
                });
            }
        }
    }
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
