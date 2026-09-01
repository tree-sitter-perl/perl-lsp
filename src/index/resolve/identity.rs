//! Cursor → identity: `resolve_symbol_scoped`'s Target/Group/Local verdict,
//! the inherited-attr group walk, and the pack class-content gate.
use super::*;

/// Resolve an attribute-group spelling on `start_class` to the group minted from
/// the class that DECLARES the attr — walking up the inheritance chain when
/// `start_class` only inherits it. A subclass use of an inherited attr
/// (`$dog->name`, `Dog->new(name => …)`, `$dog->{name}` where `Dog` inherits
/// `name` from `Animal`) thus reaches the base's full group; the `class_isa`-
/// widened ctor-key/slot members + the override-family accessor then span the
/// whole subtree. `require_reader` gates the method-call entry (only an
/// accessor-bearing group claims a `$x->attr` cursor).
pub(super) fn attr_group_via_ancestors(
    start_class: &str,
    bare: &str,
    origin: &FileAnalysis,
    idx: &dyn CrossFileLookup,
    require_reader: bool,
    require_internal: bool,
    scope: OverrideScope,
) -> Option<ResolvedTarget> {
    let proj = |c: &str| -> Option<crate::model::file_analysis::FieldProjections> {
        let ok = |p: &crate::model::file_analysis::FieldProjections| {
            (!require_reader || p.has_reader) && (!require_internal || p.has_internal)
        };
        origin
            .field_projections_named(bare, c)
            .filter(ok)
            .or_else(|| {
                // Whichever candidate file declaring `c` mints the group.
                idx.visible_def_candidates(c)
                    .iter()
                    .find_map(|cc| {
                        idx.whole_present(cc).field_projections_named(bare, c).filter(&ok)
                    })
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
    let ok = |p: &crate::model::file_analysis::FieldProjections| {
        (!require_reader || p.has_reader) && (!require_internal || p.has_internal)
    };
    if let Some(p) = origin.field_projections_named(bare, &defining) {
        if ok(&p) {
            return Some(group_from_projections(p, origin, None, Some(idx)));
        }
    }
    // The defining class's declaring file is whichever candidate mints the
    // projection group, not the name-slot winner.
    idx.visible_def_candidates(&defining).iter().find_map(|cached| {
        let whole = idx.whole_present(cached);
        let p = whole.field_projections_named(bare, &defining).filter(&ok)?;
        Some(group_from_projections(p, &whole, Some(cached.path.clone()), Some(idx)))
    })
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
    use crate::model::file_analysis::{HashKeyOwner, RenameKind};
    // Field projections claim first: from any spelling of a field group,
    // the answer is the whole group — every projection (references, rename,
    // highlights, linked editing) then reads the same group by construction.
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
        use crate::model::file_analysis::HashKeyOwner;
        match &r.kind {
            RefKind::HashKeyAccess { .. } => {
                let owner = r.hash_key_owner();
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
                            if crate::model::conventions::is_constructor_name(&name) =>
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
                // The `our` decl is in the origin file, where goto-def's
                // local path already lands — no group-level decl to add.
                decl_spans: Vec::new(),
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
        // A promoted-ctor-param cursor lands on the `$level` Variable (emitted
        // first); the member identity lives on its Field twin one sigil-column
        // in — resolve THAT, so decl-side references/rename see the accesses.
        let sym = analysis.promoted_field_twin(sym).unwrap_or(sym);
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
                t.member_shape = value_shape_if_overloaded(&t, analysis, module_index);
                t.def_paths = pack_class_def_paths(&t, analysis, module_index);
                t.bare_constant = analysis.class_content_is_bare_constant(sym);
                return Some(promoted_group_or_target(t, analysis, module_index));
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
            let class_or_value = |a: &FileAnalysis, s: &crate::model::file_analysis::Symbol| {
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
            let resolved: Option<Option<(String, bool)>> = match r.resolved_symbol() {
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
                    // Whole view: the class-content / file-scope shape tests
                    // walk symbols, which the resident copy may have evicted.
                    // Any candidate file declaring the name may claim.
                    idx.visible_def_candidates(&r.target_name).iter().find_map(|cached| {
                        let whole = idx.whole_present(cached);
                        whole
                            .symbols()
                            .iter()
                            .filter(|s| s.name == r.target_name)
                            .find_map(|s| class_or_value(&whole, s))
                    })
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
                    t.member_shape = value_shape_if_overloaded(&t, analysis, module_index);
                    t.def_paths = pack_class_def_paths(&t, analysis, module_index);
                    t.bare_constant = bare;
                    return Some(ResolvedTarget::Target(t));
                }
                Some(None) => {
                    let mut t =
                        TargetRef::new(r.target_name.clone(), TargetKind::FileScopeValue);
                    let origin_defines = analysis.names_macro_def(&r.target_name, None)
                        || r.resolved_symbol().is_some();
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
            // A member-token cursor carries its written shape, and a method
            // DECLARATION cursor names a callable; either binds the target
            // only where the class overloads the name across kinds.
            if let TargetKind::Method { class } = &t.kind {
                use crate::model::file_analysis::MemberShape;
                let written = match analysis.ref_at(point).map(|r| &r.kind) {
                    Some(RefKind::MethodCall { shape, .. }) if *shape != MemberShape::Unknown => {
                        Some(*shape)
                    }
                    _ => analysis
                        .symbol_at(point)
                        .filter(|s| matches!(s.kind, SymKind::Sub | SymKind::Method))
                        .map(|_| MemberShape::Callable),
                };
                if let Some(shape) = written {
                    if analysis.member_kinds_overloaded(class, &t.name, module_index) {
                        t.member_shape = shape;
                    }
                }
            }
            // A member-ACCESS cursor (`c->fd`) reaches here as a generic
            // Method kind; when the member is pack class content the target
            // is the same one its DEF site mints, so it carries the same
            // visibility identity. Perl methods are Sub/Method symbols —
            // never class content — and keep empty `def_paths` (no gate).
            if let TargetKind::Method { class } = &t.kind {
                if let Some(bare) = pack_member_of_class(&t.name, class, analysis, module_index) {
                    t.def_paths = pack_class_def_paths(&t, analysis, module_index);
                    t.bare_constant = bare;
                    return Some(promoted_group_or_target(t, analysis, module_index));
                }
            }
            ResolvedTarget::Target(t)
        }
    })
}

/// A class-content DECLARATION names a stored value: `Value` when the class
/// also carries a same-named callable (the only case the shape gates on),
/// `Unknown` otherwise.
fn value_shape_if_overloaded(
    t: &TargetRef,
    analysis: &FileAnalysis,
    module_index: Option<&dyn CrossFileLookup>,
) -> crate::model::file_analysis::MemberShape {
    match &t.kind {
        TargetKind::Method { class }
            if analysis.member_kinds_overloaded(class, &t.name, module_index) =>
        {
            crate::model::file_analysis::MemberShape::Value
        }
        _ => Default::default(),
    }
}

/// Is `name` a pack-language class-content member (struct field, member-block
/// role member, enum constant) of `class` — in the origin file or the class's
/// module as the origin sees it? `Some(bare)` when it is, where `bare` is the
/// enum-constant verdict (`class_content_is_bare_constant`): whether bare
/// unresolved reads of the name count as uses. `None` keeps the pack
/// visibility gate off Perl Method targets minted from the same cursor kinds.
/// Wrap a pack Method-kind member target in its promoted-param group when
/// the declaring class spells the member as a php promoted constructor
/// property (`__construct(public readonly Level $level)`): the one source
/// token declares BOTH the field and the ctor param, so the member's
/// identity must carry the param's body-use spans — a rename that rewrites
/// the decl and the accesses but leaves `$level` body reads behind breaks
/// the code. Origin-declared members fold as `local_spans`; a class living
/// in another file pins them to that file. Not promoted → the plain target.
pub(super) fn promoted_group_or_target(
    t: TargetRef,
    analysis: &FileAnalysis,
    module_index: Option<&dyn CrossFileLookup>,
) -> ResolvedTarget {
    let TargetKind::Method { class } = &t.kind else {
        return ResolvedTarget::Target(t);
    };
    if let Some((decl, spans)) = analysis.promoted_param_use_spans(&t.name, class) {
        return ResolvedTarget::Group {
            local_spans: spans,
            pinned_spans: Vec::new(),
            decl_spans: vec![(None, decl)],
            members: vec![GroupMember { target: t, rename: MemberRename::Bare }],
        };
    }
    if let Some(idx) = module_index {
        for cached in idx.visible_def_candidates(class) {
            let whole = idx.whole_present(&cached);
            if let Some((decl, spans)) = whole.promoted_param_use_spans(&t.name, class) {
                return ResolvedTarget::Group {
                    local_spans: Vec::new(),
                    pinned_spans: spans
                        .into_iter()
                        .map(|s| (cached.path.clone(), s))
                        .collect(),
                    decl_spans: vec![(Some(cached.path.clone()), decl)],
                    members: vec![GroupMember { target: t, rename: MemberRename::Bare }],
                };
            }
        }
    }
    ResolvedTarget::Target(t)
}

pub(super) fn pack_member_of_class(
    name: &str,
    class: &str,
    origin: &FileAnalysis,
    idx: Option<&dyn CrossFileLookup>,
) -> Option<bool> {
    let check = |a: &FileAnalysis| {
        a.symbols()
            .iter()
            .find(|s| {
                s.name == name
                    && s.package.as_deref() == Some(class)
                    && a.symbol_is_class_content(s)
            })
            .map(|s| a.class_content_is_bare_constant(s))
    };
    check(origin).or_else(|| {
        idx.and_then(|i| {
            // Whichever candidate file declaring `class` holds the member.
            i.visible_def_candidates(class)
                .iter()
                .find_map(|c| check(&i.whole_present(c)))
        })
    })
}
