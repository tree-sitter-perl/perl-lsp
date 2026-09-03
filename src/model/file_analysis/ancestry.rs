//! Ancestry walking: the parent-enumeration seam (`parents_of`), the
//! bounded isa walkers, the include-self MRO walk with its method
//! resolution, and the family/descendant walks.

use super::*;

/// `PERL_LSP_MEMBER_PREFILTER_EQUIV`: run every candidate scan the member
/// pre-filter would skip and score the agreement. Costs strictly more than
/// not filtering, by design — a measurement mode, not a safety net; the same
/// discipline as `PERL_LSP_RESTAMP_EQUIV`.
fn member_prefilter_equiv() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("PERL_LSP_MEMBER_PREFILTER_EQUIV").is_ok())
}

/// The REAL parent edges: the file's own `PackageFacts::parents` ∪
/// cross-file `parents_cached`. One of `parents_of`'s two component
/// spellers — the graph's `EdgeKind::Inherits` derivation reads this
/// directly so the app-surface edge stays maskable.
pub fn real_parents_of(
    class: &str,
    local: &dyn LocalParents,
    module_index: Option<&dyn CrossFileLookup>,
) -> Vec<String> {
    let mut parents: Vec<String> = local.declared_parents(class).to_vec();
    if let Some(idx) = module_index {
        for p in idx.parents_cached(class) {
            if !parents.contains(&p) {
                parents.push(p);
            }
        }
    }
    parents
}

/// The synthetic app-surface ancestor (`APP_SURFACE_CLASS`) when `class`
/// is one of the declared `consumers`. The ONE speller of the edge
/// condition — `parents_of` composes it and the graph's
/// `EdgeKind::AppSurface` derivation reads it directly.
pub fn app_surface_parent(class: &str, consumers: &[String]) -> Option<String> {
    if class != APP_SURFACE_CLASS && consumers.iter().any(|c| c == class) {
        Some(APP_SURFACE_CLASS.to_string())
    } else {
        None
    }
}

/// The full parent enumeration: real ancestors ∪ the synthetic
/// app-surface edge. Every direct parent-enumeration site
/// (`collect_ancestor_methods`, the `PackageSymbol` inheritance walk in
/// `witnesses/`) routes through here so they can't drift; graph walks
/// compose the same two spellers per edge kind. Real ancestors come
/// first; the surface is appended last so same-name overrides on a real
/// parent win. The surface has no parents of its own, so the walk's
/// seen-set + depth cap bound it like any edge.
pub fn parents_of(
    class: &str,
    local: &dyn LocalParents,
    module_index: Option<&dyn CrossFileLookup>,
    consumers: &[String],
) -> Vec<String> {
    let mut parents = real_parents_of(class, local, module_index);
    if let Some(s) = app_surface_parent(class, consumers) {
        if !parents.contains(&s) {
            parents.push(s);
        }
    }
    parents
}

/// Per-node classification returned by an ancestry-walk predicate.
pub(super) enum WalkVerdict {
    /// This class satisfies the query — short-circuit the walk to `true`.
    Hit,
    /// Not a match; keep walking its parents.
    Miss,
    /// Disqualifier — short-circuit the traversal (the walk returns `false`;
    /// a predicate that needs to distinguish reject-from-exhaust reads its
    /// own captured state, as `class_is_dbic_result` does).
    Reject,
}

/// The isa family's face of THE bounded DFS (`graph::bounded_dfs` — one
/// engine under this and `GraphView::walk`, `docs/adr/sibling-forks.md`).
/// `parents_of` supplies the per-node parent seam (local
/// `PackageFacts::parents` ∪ cross-file `parents_cached`, or
/// cross-file-only for the DBIC gate); `predicate` classifies each
/// visited class; `bound` is the call site's declared guarantee
/// (`WalkBound::ISA` — 200 visited classes, depth unbounded — unless the
/// site narrows it). Returns `true` iff a `Hit` verdict terminated the
/// walk; a `Reject` or exhaustion returns `false`. Ancestors are visited
/// in MRO order (left parent's line first), which existence-style
/// predicates cannot observe; the DBIC gate's Hit/Reject flags are
/// order-independent by construction (`rejected` forces false whichever
/// side is seen first).
pub(super) fn walk_ancestry(
    origin: &str,
    bound: crate::model::graph::WalkBound,
    mut parents_of: impl FnMut(&str) -> Vec<String>,
    mut predicate: impl FnMut(&str) -> WalkVerdict,
) -> bool {
    use crate::model::graph::WalkControl;
    // The isa questions include self (`C0 isa C0`); the engine never
    // visits the origin, so it is classified here.
    match predicate(origin) {
        WalkVerdict::Hit => return true,
        WalkVerdict::Reject => return false,
        WalkVerdict::Miss => {}
    }
    let mut hit = false;
    crate::model::graph::bounded_dfs(
        origin.to_string(),
        bound,
        |cur, out| out.extend(parents_of(cur)),
        &mut |cur| match predicate(cur) {
            WalkVerdict::Hit => {
                hit = true;
                WalkControl::Stop
            }
            WalkVerdict::Reject => WalkControl::Stop,
            WalkVerdict::Miss => WalkControl::Continue,
        },
    );
    hit
}

/// The local+cross-file parent seam for the isa walkers: the file's own
/// declared parents first (preserving push order under a budget
/// truncation), then the cross-file graph via `module_index.parents_cached`
/// (keyed by module name, which coincides with the class name here).
fn isa_parents(
    cur: &str,
    local: &dyn LocalParents,
    module_index: Option<&dyn CrossFileLookup>,
) -> Vec<String> {
    let mut v = local.declared_parents(cur).to_vec();
    if let Some(idx) = module_index {
        v.extend(idx.parents_cached(cur));
    }
    v
}

/// Does `class` equal `target` or descend from it? The single isa-walk seam
/// — both the `ReceiverGated` gate and `FileAnalysis::class_isa` route
/// through the shared [`walk_ancestry`] over the local+cross-file parent
/// graph, so the MRO is enumerated in exactly one place.
pub fn class_isa(
    class: &str,
    target: &str,
    local: &dyn LocalParents,
    module_index: Option<&dyn CrossFileLookup>,
) -> bool {
    walk_ancestry(
        class,
        crate::model::graph::WalkBound::ISA,
        |cur| isa_parents(cur, local, module_index),
        |c| {
            if c == target {
                WalkVerdict::Hit
            } else {
                WalkVerdict::Miss
            }
        },
    )
}

/// Does `class`, or any of its transitive ancestors (cross-file), satisfy
/// a plugin `ClassIsa(prefix)` trigger? The trigger's PREFIX semantics —
/// exact match OR a `prefix::`-namespaced descendant — mirror
/// `plugin::trigger_fires`, so this is the cross-file-aware analog of the
/// build-time local-only `transitive_parents` gate. Shares [`walk_ancestry`]
/// (the same local+cross-file seam as `class_isa`), so the graph is walked
/// in one place; the only difference is the per-node predicate is a prefix
/// test, not exact equality. Deliberately NOT `parents_of`: the synthetic
/// `APP_SURFACE_CLASS` edge is a method-dispatch bridge (Mojo helpers), not
/// an `isa` relation, so a plugin `ClassIsa` gate must not treat an
/// app-surface consumer as a descendant of the surface. Both isa-walk seams
/// exclude it by construction.
pub fn class_isa_prefix(
    class: &str,
    prefix: &str,
    local: &dyn LocalParents,
    module_index: Option<&dyn CrossFileLookup>,
) -> bool {
    let ns = format!("{prefix}::");
    walk_ancestry(
        class,
        crate::model::graph::WalkBound::ISA,
        |cur| isa_parents(cur, local, module_index),
        |c| {
            if c == prefix || c.starts_with(&ns) {
                WalkVerdict::Hit
            } else {
                WalkVerdict::Miss
            }
        },
    )
}

impl FileAnalysis {
    /// Single-source-of-truth DFS ancestor walk for every per-class
    /// lookup on this file: walks `class_name` and every ancestor
    /// (local `PackageFacts::parents` ∪ cross-file `parents_cached`),
    /// cycle-safe via a `seen` set, depth-capped at 20 (Perl's default
    /// MRO bound). Visitor decides when to stop via `ControlFlow::Break`.
    ///
    /// Both `resolve_method_in_ancestors` (find-first) and
    /// `for_each_dispatch_handler_on_class` (collect-all) route
    /// through here so the two code paths can never drift on MRO
    /// rules. New ancestor-aware queries should reuse it too rather
    /// than reroll the walk.
    /// The include-self MRO walk: visit `class_name` itself, then every
    /// proper ancestor in Perl's left-to-right DFS order. The one place
    /// self-handling lives for the "method on this class or up the
    /// chain" consumers — it runs their own closure on self, which is
    /// consumer-specific (so it can't live in `walk`, which has no
    /// closure for the origin). Proper-ancestor traversal is
    /// `walk(class, INHERITS)`. `SUPER::` — parents only, never self —
    /// is the bare `walk` (see `resolve_super_method`).
    pub fn for_each_ancestor_class(
        &self,
        class_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
        visit: impl FnMut(&str) -> std::ops::ControlFlow<()>,
    ) {
        let _ = self.for_each_ancestor_class_reporting_truncation(
            class_name,
            module_index,
            visit,
        );
    }

    /// [`Self::for_each_ancestor_class`], plus whether the walk was cut off
    /// by a graph bound.
    ///
    /// A truncated walk is indistinguishable from a small hierarchy: the
    /// visitor simply stops seeing ancestors. That is fine for a best-effort
    /// consumer and wrong for one whose answer means "this is the whole
    /// ancestry" — it would claim a closure it never enumerated.
    pub fn for_each_ancestor_class_reporting_truncation(
        &self,
        class_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
        mut visit: impl FnMut(&str) -> std::ops::ControlFlow<()>,
    ) -> bool {
        if visit(class_name).is_break() {
            return false;
        }
        let graph = crate::model::graph::GraphView::new(self, module_index);
        graph.walk(
            crate::model::graph::Node::Class(class_name.to_string()),
            crate::model::graph::EdgeKindMask::INHERITS
                | crate::model::graph::EdgeKindMask::APP_SURFACE,
            &mut |n| match n {
                crate::model::graph::Node::Class(c) => match visit(c) {
                    std::ops::ControlFlow::Break(()) => crate::model::graph::WalkControl::Stop,
                    std::ops::ControlFlow::Continue(()) => {
                        crate::model::graph::WalkControl::Continue
                    }
                },
                _ => crate::model::graph::WalkControl::Continue,
            },
        )
    }

    /// Test-only access to the include-self ancestor walk.
    #[cfg(test)]
    pub fn for_each_ancestor_class_test(
        &self,
        class_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
        visit: impl FnMut(&str) -> std::ops::ControlFlow<()>,
    ) {
        self.for_each_ancestor_class(class_name, module_index, visit)
    }

    /// `child isa ancestor`? — the MRO walk (local ∪ cross-file parents)
    /// as a predicate. `true` when `child == ancestor` too.
    pub fn class_isa(
        &self,
        child: &str,
        ancestor: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> bool {
        // `walk` yields reached nodes only, so the reflexive `X isa X`
        // is a direct check; the rest is an INHERITS traversal.
        if child == ancestor {
            return true;
        }
        let graph = crate::model::graph::GraphView::new(self, module_index);
        let mut found = false;
        graph.walk(
            crate::model::graph::Node::Class(child.to_string()),
            crate::model::graph::EdgeKindMask::INHERITS
                | crate::model::graph::EdgeKindMask::APP_SURFACE,
            &mut |n| {
                if matches!(n, crate::model::graph::Node::Class(c) if c == ancestor) {
                    found = true;
                    crate::model::graph::WalkControl::Stop
                } else {
                    crate::model::graph::WalkControl::Continue
                }
            },
        );
        found
    }

    /// Inheritance chain for a method rename: `[class, ..., defining_class]`.
    ///
    /// Cross-class method rename has to touch two distinct things:
    ///   * the `sub M` definition in whichever ancestor actually
    ///     defines the method, and
    ///   * every `$obj->M(...)` call site whose static `invocant_class`
    ///     is the rename target *or* an intermediate ancestor that
    ///     inherited (didn't override) the method.
    ///
    /// `rename_method_in_class` is per-class — so callers iterate this
    /// chain. Stops at the first ancestor that defines the method
    /// (inclusive); intermediate ancestors that *override* are
    /// skipped because they're a different method from the
    /// inheritance perspective.
    /// The full **override family** of `(class, method)` — the contract root
    /// (topmost ancestor defining the method) plus every class that inherits or
    /// overrides it. The membership set for `OverrideScope::Hierarchy` rename:
    /// renaming any member rewrites them all, so an override never silently
    /// desyncs from its base (the standard IDE refactor). Gathered over PROVEN
    /// inheritance
    /// edges only (`@ISA`/`use parent`/Moo via `GraphView`), NEVER name matches
    /// — two unrelated classes both defining `sub render {}` with no edge
    /// between them are not a family.
    pub fn method_override_family(
        &self,
        class_name: &str,
        method_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<String> {
        let defines = |cls: &str| {
            matches!(
                self.resolve_method_in_ancestors(cls, method_name, module_index),
                Some(MethodResolution::Local { class: ref c, .. })
                    | Some(MethodResolution::CrossFile { class: ref c, .. })
                    if c == cls
            )
        };
        // Contract root: the topmost ancestor (incl. the cursor class) that
        // defines the method — an override roots at the base it overrides.
        let mut root = class_name.to_string();
        self.for_each_ancestor_class(class_name, module_index, |cls| {
            if defines(cls) {
                root = cls.to_string();
            }
            std::ops::ControlFlow::Continue(())
        });
        // Root + every class participating in its dispatch. Descendants
        // alone would miss a sibling that composes into a shared consumer,
        // which is where a role's caller lives — and `collect`'s membership
        // test is what turns that miss into an empty references answer.
        let mut family = self.descendant_family(root.clone(), module_index);
        for p in self.dispatch_participants(&root, module_index) {
            if !family.iter().any(|f| f == &p) {
                family.push(p);
            }
        }
        family
    }

    /// Every class that participates in `class`'s dispatch for a method:
    /// the transitive `INHERITS_INV` descendants, PLUS the co-ancestors
    /// those descendants reach walking back UP their own MRO.
    ///
    /// The co-ancestor half is what a descendants-only walk cannot see. A
    /// concrete class assembles its dispatch table from SEVERAL parents
    /// (Moo/Moose `with`, `load_components`, multi-base `use base`), so a
    /// role that only CALLS `$self->m` sits alongside the role that defines
    /// it — sibling parents of one composer, neither below the other. The
    /// path between them is down to the shared composer and back up, which
    /// is why `INHERITS_INV` alone returns nothing and the caller sees an
    /// empty answer rather than a wrong one.
    ///
    /// One gather, two readers: `implementations_of` (which then subtracts
    /// its own contract line) and `method_override_family`. They disagreed
    /// while each had its own walk — `--implementations` found the sibling
    /// and `references` did not, from the same cursor.
    pub fn dispatch_participants(
        &self,
        class: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> std::collections::BTreeSet<String> {
        use crate::model::graph::{EdgeKindMask, GraphView, Node, WalkControl};
        let probe = GraphView::new(self, module_index);
        let mut descendants: Vec<String> = Vec::new();
        probe.walk(Node::Class(class.to_string()), EdgeKindMask::INHERITS_INV, &mut |n| {
            if let Node::Class(c) = n {
                descendants.push(c.clone());
            }
            WalkControl::Continue
        });
        let mut out: std::collections::BTreeSet<String> =
            descendants.iter().cloned().collect();
        for d in &descendants {
            probe.walk(
                Node::Class(d.clone()),
                EdgeKindMask::INHERITS | EdgeKindMask::APP_SURFACE,
                &mut |n| {
                    if let Node::Class(c) = n {
                        out.insert(c.clone());
                    }
                    WalkControl::Continue
                },
            );
        }
        out
    }

    /// A root class plus every transitive descendant that inherits from it,
    /// over PROVEN inheritance edges only (`GraphView`'s `INHERITS_INV`
    /// walk, which excludes the origin — re-added as the family head).
    /// The shared descendant-walk tail of `method_override_family` and
    /// `owned_accessor_family`; the two differ only in how they choose the
    /// root (contract-root search UP vs the owning class itself).
    fn descendant_family(
        &self,
        root: String,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<String> {
        let mut family = vec![root.clone()];
        let graph = crate::model::graph::GraphView::new(self, module_index);
        graph.walk(
            crate::model::graph::Node::Class(root),
            crate::model::graph::EdgeKindMask::INHERITS_INV,
            &mut |n| {
                if let crate::model::graph::Node::Class(c) = n {
                    if !family.iter().any(|f| f == c) {
                        family.push(c.clone());
                    }
                }
                crate::model::graph::WalkControl::Continue
            },
        );
        family
    }

    /// The rename family for a class-OWNED synthesized accessor (a Moo `has`
    /// reader, a DBIC column/relationship accessor): the owning class plus
    /// every transitive descendant that inherits it. Unlike
    /// `method_override_family`, it never searches UPWARD for a contract
    /// root — a synthesized accessor is owned by its declaring class, and a
    /// same-named method in a framework ancestor (`DBIx::Class::PK::id` vs a
    /// synthesized `id` column) is a name collision, not the same symbol.
    /// Rooting at that ancestor would fan the rename across every unrelated
    /// sibling subclass of it (rule #10: gate on the owner axis, never the
    /// bare name). The declaring class is already resolved by
    /// `attr_group_via_ancestors` before the group is minted, so `class_name`
    /// is the true owner even when the cursor sat on an inheriting subclass.
    pub fn owned_accessor_family(
        &self,
        class_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<String> {
        self.descendant_family(class_name.to_string(), module_index)
    }

    pub fn method_rename_chain(
        &self,
        class_name: &str,
        method_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Vec<String> {
        let defining = match self.resolve_method_in_ancestors(class_name, method_name, module_index) {
            Some(MethodResolution::Local { class, .. })
            | Some(MethodResolution::CrossFile { class, .. }) => class,
            None => return vec![class_name.to_string()],
        };
        let mut chain = Vec::new();
        self.for_each_ancestor_class(class_name, module_index, |cls| {
            chain.push(cls.to_string());
            if cls == defining {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        });
        if chain.is_empty() { chain.push(class_name.to_string()); }
        chain
    }

    /// Does `cls` ITSELF define/bridge `method_name`? The per-class check —
    /// no ancestor walk — factored out of the MRO loop so normal dispatch and
    /// `SUPER::` share one definition of "this class has the method". (a) local
    /// symbols packaged under `cls`, (b) local plugin-namespace entities
    /// bridged to `cls`, (c) cross-file: `cls`'s own module, cross-package
    /// typeglob installs, and plugin bridges from other files.
    fn method_resolution_on_class(
        &self,
        cls: &str,
        method_name: &str,
        shape: MemberShape,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<MethodResolution> {
        // A written shape asks for the agreeing kind FIRST (`$this->recorded`
        // reads the property, `$this->recorded()` calls the method); the
        // other kind stays the fallback so a class that does not overload
        // the name answers exactly as a shape-less lookup would.
        let agrees = |kind: SymKind| match shape {
            MemberShape::Unknown => true,
            MemberShape::Callable => matches!(kind, SymKind::Sub | SymKind::Method),
            MemberShape::Value => !matches!(kind, SymKind::Sub | SymKind::Method),
        };
        if shape != MemberShape::Unknown {
            if let Some(r) = self.member_resolution_on_class_pass(cls, method_name, module_index, &agrees) {
                return Some(r);
            }
            // php: the syntax decided the kind; a value read of a name only
            // a method carries is an undeclared property, not that method.
            if self.pack.member_shapes_are_strict {
                return None;
            }
        }
        self.member_resolution_on_class_pass(cls, method_name, module_index, &|_| true)
    }

    fn member_resolution_on_class_pass(
        &self,
        cls: &str,
        method_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
        agrees: &dyn Fn(SymKind) -> bool,
    ) -> Option<MethodResolution> {
        // (a) Local symbols in this file packaged under `cls`. Methods AND
        // data members: cpp `obj->field` mints the same `MethodCall` ref as a
        // method call, and a `Variable`/`Field` member is its def. (Perl
        // members are always Sub/Method, so this is a no-op there.) A
        // Variable/Field must be the class's OWN content — a lexical local
        // inside an inline method carries the class as sticky `package` too
        // (`T* data = this->data();` in a member body is NOT member `data`).
        for &sid in self.symbols_named(method_name) {
            let sym = self.symbol(sid);
            let member_kind = match sym.kind {
                SymKind::Sub | SymKind::Method => true,
                SymKind::Variable | SymKind::Field | SymKind::Enumerator => {
                    self.symbol_is_class_content(sym)
                }
                _ => false,
            };
            // A re-export (`using Base::m;`) is API surface, not a def —
            // fall through so the walk reaches the origin ancestor.
            if member_kind && agrees(sym.kind) && !sym.is_reexport() && self.symbol_in_class(sid, cls) {
                return Some(MethodResolution::Local { class: cls.to_string(), sym_id: sid });
            }
        }
        // (b) Local plugin-namespace entities bridged to `cls`.
        for ns in &self.plugin.namespaces {
            if !ns.bridges.iter().any(|b| matches!(b, Bridge::Class(c) if c == cls)) { continue; }
            for sym_id in &ns.entities {
                let Some(sym) = self.symbols.get(sym_id.0 as usize) else { continue };
                if !matches!(sym.kind, SymKind::Sub | SymKind::Method) { continue; }
                if sym.name == method_name {
                    return Some(MethodResolution::Local { class: cls.to_string(), sym_id: *sym_id });
                }
            }
        }
        // (c) Cross-file: the cached module for `cls` itself (real
        // CPAN/user-defined methods) plus plugin-emitted methods registered via
        // bridges from other workspace files (rule #8 — `for_each_entity_
        // bridged_to`).
        if let Some(idx) = module_index {
            // EVERY file declaring `cls` that the QUERY can see is a
            // candidate (a Perl package reopens anywhere; the name-slot
            // winner alone would hide a method the losing file defines).
            // `visible_def_candidates` applies the scope: a closure-scoped
            // (pack) origin narrows to connected files — an unrelated TU's
            // same-named class must not hijack the walk — while Perl gets
            // the whole relation. The goto-def consumer re-picks the
            // defining candidate with the same test.
            crate::util::ghost_stats::mroc_begin();
            for cached in idx.visible_def_candidates(cls) {
                // Rows-backed pre-filter (`docs/prompt-relational-iteration.md`):
                // skip the rehydrate when the row store PROVES this candidate
                // declares nothing named `method_name` under `cls`. Fail-open
                // everywhere the store cannot speak; the equiv switch runs the
                // skipped scan anyway and screams on divergence.
                let may_declare = idx.candidate_may_declare(&cached, method_name, cls);
                if !may_declare {
                    crate::util::ghost_stats::count("mroc.candidate_prefiltered");
                    if !member_prefilter_equiv() {
                        continue;
                    }
                }
                // Class-scoped, not file-scoped: a pack file holds MANY
                // classes, so "some sub of this name exists in cls's file"
                // would let an unrelated same-named member hijack
                // the walk at `cls` and stop it before the true ancestor.
                // Re-exports fall through, same as the local arm.
                // Symbols-axis read only (existence, kind, package, class-
                // content — never the bag/refs), so the import tier answers
                // from its resident copy instead of decoding the whole blob.
                // Fetched-vs-matched for this walk. The comment above says the
                // symbols-axis read answers from the resident copy; that holds
                // only while the copy still HAS symbols, and the workspace tier
                // strips them, so at scale this is a decode per candidate.
                crate::util::ghost_stats::count("mroc.candidate_fetched");
                crate::util::ghost_stats::mroc_note(&cached.path);
                let whole = crate::util::ghost_stats::in_ancestor_walk(|| {
                    idx.symbols_present(&cached)
                });
                let has_member = whole.symbols.iter().any(|s| {
                    s.name == method_name
                        && s.package.as_deref() == Some(cls)
                        && !s.is_reexport()
                        && agrees(s.kind)
                        && (matches!(s.kind, SymKind::Sub | SymKind::Method)
                            || (matches!(
                                s.kind,
                                SymKind::Variable | SymKind::Field | SymKind::Enumerator
                                // On the WHOLE view: the class-content test
                                // resolves the owning container through
                                // `symbols_named`, which the evicted copy
                                // answers empty.
                            ) && whole.symbol_is_class_content(s)))
                });
                crate::util::ghost_stats::count(if has_member {
                    "mroc.candidate_matched"
                } else {
                    "mroc.candidate_wasted"
                });
                if !may_declare {
                    // PERL_LSP_MEMBER_PREFILTER_EQUIV: the skipped scan ran —
                    // score the verdict the gate would have acted on.
                    crate::util::ghost_stats::count(if has_member {
                        "memberprefilter.break"
                    } else {
                        "memberprefilter.agreed"
                    });
                    if has_member {
                        log::warn!(
                            "member prefilter equiv: rows said {cls}::{method_name} \
                             absent in {:?} but the decode found it — the shred \
                             missed a symbol-minting path",
                            cached.path
                        );
                    }
                }
                if has_member {
                    return Some(MethodResolution::CrossFile { class: cls.to_string(), def_module: None });
                }
                // A cross-file DBIC result class's column/relationship accessors
                // are DEFERRED plugin emissions (`gated_emissions`) that the raw
                // cached copy doesn't carry. They are MATERIALIZED into the whole
                // cached copy at index completion
                // (`ModuleIndex::materialize_gated_emissions`), so the
                // `has_member` check above already sees them — no per-query
                // enrichment hop here (that nested a full enrichment per hop and
                // overflowed the stack on deep dep graphs). See `GatedEmission`.
            }
            // Cross-package typeglob install: the method is attributed to `cls`
            // but lives in a differently-named module file (`*{'DateTime::'.
            // $sub} = …` inside `package DateTime::PP`). Record the home module.
            if let Some(home) = idx.module_declaring_method_in_package(method_name, cls) {
                return Some(MethodResolution::CrossFile { class: cls.to_string(), def_module: Some(home) });
            }
            // Plugin-bridged method (a Mojo helper synthesized in another file,
            // bridged to `cls`). Record the registration module so the def
            // lookup hits the right file, not `cls`'s own module.
            let mut bridged_module: Option<String> = None;
            idx.for_each_entity_bridged_to_named(cls, method_name, &mut |mod_name, _cached, sym| {
                use std::ops::ControlFlow;
                if !matches!(sym.kind, SymKind::Sub | SymKind::Method) {
                    return ControlFlow::Continue(());
                }
                if sym.name == method_name {
                    bridged_module = Some(mod_name.to_string());
                    return ControlFlow::Break(());
                }
                ControlFlow::Continue(())
            });
            if bridged_module.is_some() {
                return Some(MethodResolution::CrossFile { class: cls.to_string(), def_module: bridged_module });
            }
        }
        None
    }

    /// Walk the inheritance chain to find a method (DFS, matches Perl's default MRO).
    pub fn resolve_method_in_ancestors(
        &self,
        class_name: &str,
        method_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<MethodResolution> {
        self.resolve_member_in_ancestors(class_name, method_name, MemberShape::Unknown, module_index)
    }

    /// `resolve_method_in_ancestors` with the cursor token's written shape:
    /// a value read prefers the class's property, a call its method, on
    /// every class of the walk (the other kind stays the fallback).
    pub fn resolve_member_in_ancestors(
        &self,
        class_name: &str,
        method_name: &str,
        shape: MemberShape,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<MethodResolution> {
        let _t = crate::util::ghost_stats::ScopedNs::start("mroc.total");
        let mut result: Option<MethodResolution> = None;
        let mut iface_fallback: Option<MethodResolution> = None;
        self.for_each_ancestor_class(class_name, module_index, |cls| {
            match self.method_resolution_on_class(cls, method_name, shape, module_index) {
                // An INTERFACE hit is held as fallback, never the answer
                // while a concrete definer exists: php's MRO interleaves
                // `implements` (header) ahead of `use Trait` (body), so the
                // abstract stub otherwise shadows the trait method every
                // consumer actually runs (laravel `Collection->eachSpread`).
                Some(r) => {
                    if self.hit_class_is_interface(cls, &r, module_index) {
                        iface_fallback.get_or_insert(r);
                        std::ops::ControlFlow::Continue(())
                    } else {
                        result = Some(r);
                        std::ops::ControlFlow::Break(())
                    }
                }
                None => std::ops::ControlFlow::Continue(()),
            }
        });
        result.or(iface_fallback)
    }

    /// Is the class that answered a method resolution an INTERFACE (php:
    /// a Class symbol carrying the "interface" flavor attribute)? Cost-
    /// shaped by the hit kind: a Local hit consults only the local symbol
    /// (for Perl that scan never matches — no symbol carries the flavor —
    /// and no cross-file work happens); a CrossFile hit also sweeps the
    /// class's candidate files, whose copies the resolution itself just
    /// rehydrated (LRU-warm).
    fn hit_class_is_interface(
        &self,
        cls: &str,
        r: &MethodResolution,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> bool {
        match r {
            MethodResolution::Local { .. } => self.declares_interface(cls),
            MethodResolution::CrossFile { .. } => {
                self.declares_interface(cls)
                    || module_index.is_some_and(|i| {
                        i.visible_def_candidates(cls)
                            .iter()
                            .any(|c| i.whole_present(c).declares_interface(cls))
                    })
            }
        }
    }

    /// Does THIS file declare `class` as an interface? php's interfaces are
    /// `SymKind::Class` symbols carrying the "interface" flavor attribute
    /// (stamped from the `@classattr.interface` capture); Perl never marks
    /// one. The one speller every interface-deferral walk asks.
    pub fn declares_interface(&self, class: &str) -> bool {
        self.symbols().iter().any(|s| {
            matches!(s.kind, SymKind::Class)
                && s.name == class
                && s.attributes.iter().any(|x| x == "interface")
        })
    }

    /// `$self->SUPER::m` dispatch: resolve `method_name` over `enclosing`'s
    /// parents' MRO, skipping `enclosing` itself (Perl's SUPER searches the
    /// current package's parents). Walks the FULL DFS MRO — every parent in
    /// `@ISA`, not just the first — so a method defined on a later parent (or a
    /// grandparent reached only through it) resolves exactly as Perl would.
    pub fn resolve_super_method(
        &self,
        enclosing: &str,
        method_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<MethodResolution> {
        // A SAME-LEAF parent (php aliased import — `use Support\Collection
        // as BaseCollection; class Collection extends BaseCollection`)
        // collapses into the ORIGIN node of the leaf-keyed walk below and
        // is skipped, so `parent::` fell through to a DEEPER ancestor
        // (typically an interface's abstract stub). Resolve it explicitly
        // first: the pack's parent-namespace row names the parent's
        // namespace, and the candidate file whose Class symbol carries
        // that namespace is the real parent.
        if let Some(idx) = module_index {
            for (child, parent, ns) in &self.pack.parent_namespaces {
                if child != enclosing || parent != enclosing {
                    continue;
                }
                for cached in idx.visible_def_candidates(parent) {
                    let whole = idx.whole_present(&cached);
                    let cand_ns = whole
                        .symbols()
                        .iter()
                        .find(|s| matches!(s.kind, SymKind::Class) && &s.name == parent)
                        .map(|s| s.package.clone().unwrap_or_default());
                    if cand_ns.as_deref() == Some(ns.as_str())
                        && whole
                            .method_resolution_on_class(parent, method_name, MemberShape::Unknown, module_index)
                            .is_some()
                    {
                        return Some(MethodResolution::CrossFile {
                            class: parent.clone(),
                            def_module: None,
                        });
                    }
                }
            }
        }
        // SUPER:: searches the PARENTS, never the enclosing class — so
        // it is the bare `walk`, origin-excluded by construction. A hit on
        // an INTERFACE-marked class (php: the same SymKind::Class, told
        // apart by the "interface" flavor attribute) is kept only as a
        // fallback: `parent::` runs the concrete class chain, and the
        // abstract stub is the answer only when nothing concrete defines
        // the method.
        let mut result: Option<MethodResolution> = None;
        let mut iface_fallback: Option<MethodResolution> = None;
        let graph = crate::model::graph::GraphView::new(self, module_index);
        graph.walk(
            crate::model::graph::Node::Class(enclosing.to_string()),
            crate::model::graph::EdgeKindMask::INHERITS
                | crate::model::graph::EdgeKindMask::APP_SURFACE,
            &mut |n| {
                let crate::model::graph::Node::Class(cls) = n else {
                    return crate::model::graph::WalkControl::Continue;
                };
                match self.method_resolution_on_class(cls, method_name, MemberShape::Unknown, module_index) {
                    Some(r) => {
                        if self.hit_class_is_interface(cls, &r, module_index) {
                            iface_fallback.get_or_insert(r);
                            crate::model::graph::WalkControl::Continue
                        } else {
                            result = Some(r);
                            crate::model::graph::WalkControl::Stop
                        }
                    }
                    None => crate::model::graph::WalkControl::Continue,
                }
            },
        );
        result.or(iface_fallback)
    }

    /// Does `class` (or any ancestor we CAN reach) name a parent that
    /// resolves to nothing — not a package/class defined in this file,
    /// not a cached module in `module_index`/@INC, and not the synthetic
    /// app-surface edge? If so the ISA chain is incomplete: a method we
    /// can't find locally might be inherited from the unresolvable parent,
    /// so consumers (the `unresolved-method` diagnostic) must stay honest-
    /// silent rather than emit a confident false positive.
    ///
    /// The SINGLE source of the "is the inheritance chain incomplete"
    /// property, so every invocant-typing path (direct `Pkg->m`, `$self`/
    /// FirstParam, variable-typed) asks the same question and can't drift
    /// (rule #10). Walks via `for_each_ancestor_class` so the MRO + seen-set
    /// + depth cap match `resolve_method_in_ancestors` exactly.
    pub fn class_has_unresolved_ancestor(
        &self,
        class_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> bool {
        let class_is_known = |name: &str| -> bool {
            if name == APP_SURFACE_CLASS {
                return true;
            }
            let local = self.symbols.iter().any(|s| {
                matches!(s.kind, SymKind::Class | SymKind::Package | SymKind::Module)
                    && s.name == name
            });
            if local {
                return true;
            }
            module_index
                .map(|idx| idx.get_cached(name).is_some())
                .unwrap_or(false)
        };

        let mut incomplete = false;
        self.for_each_ancestor_class(class_name, module_index, |cls| {
            // A parent edge that never folded to a name (runtime-
            // generated role: `with ReportProxy(type => ...)`) is as
            // unresolved as a named parent we can't find — the
            // recorded list isn't the whole ancestry.
            let dynamic_here = self.has_dynamic_parents(cls)
                || module_index.is_some_and(|idx| {
                    // ANY file declaring `cls` may hold the runtime-generated
                    // parent edge (packages lane — never evicted).
                    idx.visible_def_candidates(cls)
                        .iter()
                        .any(|c| c.analysis.has_dynamic_parents(cls))
                });
            if dynamic_here {
                incomplete = true;
                return std::ops::ControlFlow::Break(());
            }
            let parents = parents_of(
                cls,
                &self.packages,
                module_index,
                &self.plugin.app_surface_consumers,
            );
            for p in &parents {
                if !class_is_known(p) {
                    incomplete = true;
                    return std::ops::ControlFlow::Break(());
                }
            }
            std::ops::ControlFlow::Continue(())
        });
        incomplete
    }

    /// Recursively collect methods from a class and its ancestors, deduping by name.
    pub(super) fn collect_ancestor_methods(
        &self,
        original_class: &str,
        class_name: &str,
        module_index: Option<&dyn CrossFileLookup>,
        candidates: &mut Vec<CompletionCandidate>,
        seen_names: &mut HashSet<String>,
        depth: usize,
        requesting_class: Option<&str>,
    ) {
        if depth > 20 {
            return;
        }
        // Access-specifier gate: visible from outside
        // `class_name`'s own body only when NOT tagged non-public.
        // Callability gate on the same closure so every enumeration loop in
        // this walk (local, plugin-namespace, cross-file) shares it: an
        // anonymous sub (`*__HM_DEDUP = sub () {0}`) is a symbol in the
        // class but not a name a method call can ever spell.
        let visible = |sym: &Symbol| {
            crate::model::conventions::is_callable_sub_name(&sym.name)
                // A lexical sub/method (`my sub` / `my method`) is scoped to
                // its block, not the class: it never dispatches by name on an
                // MRO and is invisible cross-file. The point-aware `&name`
                // lane (`complete_lexical_methods_at`) owns offering it.
                && !matches!(&sym.detail, SymbolDetail::Sub { lexical: true, .. })
                && (requesting_class == Some(class_name)
                    || !sym.attributes.iter().any(|a| a == "non_public"))
        };

        // Local methods in this class
        for sym in &self.symbols {
            if matches!(sym.kind, SymKind::Sub | SymKind::Method) {
                if self.symbol_in_class(sym.id, class_name)
                    && !seen_names.contains(&sym.name)
                    && visible(sym)
                {
                    seen_names.insert(sym.name.clone());
                    let defining = if class_name != original_class { Some(class_name) } else { None };
                    let display_override = sym.presentation.display;
                    candidates.push(CompletionCandidate {
                        label: sym.name.clone(),
                        kind: sym.kind,
                        is_static: sym.attributes.iter().any(|a| a == "static"),
                        detail: Some(self.method_detail(original_class, &sym.name, defining, module_index)),
                        insert_text: None,
                        sort_priority: PRIORITY_LOCAL,
                        additional_edits: vec![],
                        import_fact: None,
                        display_override,
                    });
                }
            }
        }

        // Local plugin-namespace entities bridged to this class. The
        // same-file equivalent of `for_each_entity_bridged_to` — plugin
        // namespaces in THIS FileAnalysis whose bridges include
        // `class_name`. Namespace membership is the sole filter (per
        // `for_each_entity_bridged_to` docs); entity packages can be
        // different from `class_name` (e.g. a helper Method whose
        // package is `Mojolicious::Controller` surfacing from a
        // `Mojolicious` query when the namespace bridges both).
        for ns in &self.plugin.namespaces {
            let bridges_class = ns.bridges.iter().any(|b|
                matches!(b, Bridge::Class(c) if c == class_name));
            if !bridges_class { continue; }
            for sym_id in &ns.entities {
                let Some(sym) = self.symbols.get(sym_id.0 as usize) else { continue };
                if !matches!(sym.kind, SymKind::Sub | SymKind::Method) { continue; }
                if seen_names.contains(&sym.name) { continue; }
                if !visible(sym) { continue; }
                seen_names.insert(sym.name.clone());
                let defining = if class_name != original_class { Some(class_name) } else { None };
                let display_override = sym.presentation.display;
                candidates.push(CompletionCandidate {
                    label: sym.name.clone(),
                    kind: sym.kind,
                    is_static: sym.attributes.iter().any(|a| a == "static"),
                    detail: Some(self.method_detail(original_class, &sym.name, defining, module_index)),
                    insert_text: None,
                    sort_priority: PRIORITY_LOCAL,
                    additional_edits: vec![],
                    import_fact: None,
                    display_override,
                });
            }
        }

        // Cross-file entity + own-class method collection. Parent
        // recursion (local ∪ cross-file ∪ synthetic app-surface edge)
        // is the single `parents_of` walk at the end of the fn.
        if let Some(idx) = module_index {
            // Two sources of candidates:
            //   (1) Plugin entities reached through bridges (helpers,
            //       routes, tasks, etc. — explicit `Bridge::Class(X)`
            //       declarations from PluginNamespaces across the
            //       workspace).
            //   (2) The cached module whose primary package IS
            //       class_name (real CPAN/user-defined methods on the
            //       class itself).
            // Collect into a temporary list to avoid borrow-checker
            // issues with the closure capturing &mut seen_names/candidates.
            let mut bridged: Vec<(String, SymKind, Option<SymbolDetail>, Option<HandlerDisplay>, bool)> = Vec::new();
            idx.for_each_entity_bridged_to(class_name, &mut |_mod, _cached, sym| {
                use std::ops::ControlFlow;
                if !matches!(sym.kind, SymKind::Sub | SymKind::Method) {
                    return ControlFlow::Continue(());
                }
                if !visible(sym) {
                    return ControlFlow::Continue(());
                }
                bridged.push((
                    sym.name.clone(),
                    sym.kind,
                    Some(sym.detail.clone()),
                    sym.presentation.display,
                    sym.attributes.iter().any(|a| a == "static"),
                ));
                ControlFlow::Continue(())
            });
            for (name, kind, detail, display_override, is_static) in bridged {
                if seen_names.contains(&name) { continue; }
                seen_names.insert(name.clone());
                let is_method = kind == SymKind::Method
                    || matches!(detail, Some(SymbolDetail::Sub { is_method: true, .. }));
                let kind = if is_method { SymKind::Method } else { SymKind::Sub };
                let defining = if class_name != original_class { Some(class_name) } else { None };
                let method_detail_str = self.method_detail(original_class, &name, defining, module_index);
                candidates.push(CompletionCandidate {
                    label: name,
                    kind,
                    is_static,
                    detail: Some(method_detail_str),
                    insert_text: None,
                    sort_priority: PRIORITY_LOCAL,
                    additional_edits: vec![],
                    import_fact: None,
                    display_override,
                });
            }
            // (2) Real methods on class_name's own cached module — EVERY
            // file declaring the class (a reopened package's methods live
            // across the set; `seen_names` keeps child-shadows-parent).
            for cached in idx.visible_def_candidates(class_name) {
                let whole = idx.whole_present(&cached);
                for sym in &whole.symbols {
                    if !matches!(sym.kind, SymKind::Sub | SymKind::Method) { continue; }
                    if sym.package.as_deref() != Some(class_name) { continue; }
                    if seen_names.contains(&sym.name) { continue; }
                    if !visible(sym) { continue; }
                    seen_names.insert(sym.name.clone());
                    let is_method = sym.kind == SymKind::Method
                        || matches!(sym.detail, SymbolDetail::Sub { is_method: true, .. });
                    let kind = if is_method { SymKind::Method } else { SymKind::Sub };
                    let defining = if class_name != original_class { Some(class_name) } else { None };
                    let detail = self.method_detail(original_class, &sym.name, defining, module_index);
                    let display_override = sym.presentation.display;
                    candidates.push(CompletionCandidate {
                        label: sym.name.clone(),
                        kind,
                        is_static: sym.attributes.iter().any(|a| a == "static"),
                        detail: Some(detail),
                        insert_text: None,
                        sort_priority: PRIORITY_LOCAL,
                        additional_edits: vec![],
                        import_fact: None,
                        display_override,
                    });
                }
            }

        }

        // Walk parents: local ∪ cross-file ∪ synthetic app-surface edge,
        // unioned + deduped by `parents_of` (the single edge-injection
        // site). Name dedup across the recursion is the `seen_names` set.
        for parent in parents_of(
            class_name,
            &self.packages,
            module_index,
            &self.plugin.app_surface_consumers,
        ) {
            self.collect_ancestor_methods(
                original_class, &parent, module_index, candidates, seen_names, depth + 1,
                requesting_class,
            );
        }
    }

}
