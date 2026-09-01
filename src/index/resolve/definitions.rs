//! The `definitions()` projection and its ranking helpers (member/overload
//! def locations, preferred-definition laddering).
use super::*;

impl<'a> CandidateSet<'a> {
    /// `parent::method` definition sites: a bounded BFS over the DECLARED
    /// parent edges (never the enclosing class itself), namespace-routed
    /// through the pack's `parent_namespaces` rows so a same-leaf aliased
    /// parent resolves into the RIGHT file, and interface-marked classes
    /// (the "interface" flavor attribute) defer to concrete ones —
    /// `parent::` runs the class chain, and the abstract stub answers only
    /// when nothing concrete defines the method. `None` = no parent
    /// defines it; the caller falls through to the generic lanes.
    fn super_def_locations(
        &self,
        r: &crate::model::file_analysis::Ref,
        method: &str,
        idx: &dyn crate::model::file_analysis::CrossFileLookup,
    ) -> Option<Vec<RefLocation>> {
        let analysis = self.origin;
        let encl = analysis.enclosing_class_for_scope(r.scope)?;
        let parent_ns = |a: &crate::model::file_analysis::FileAnalysis,
                         child: &str,
                         parent: &str|
         -> Option<String> {
            a.pack
                .parent_namespaces
                .iter()
                .find(|(c, p, _)| c == child && p == parent)
                .map(|(_, _, ns)| ns.clone())
        };
        let method_decl_in = |a: &crate::model::file_analysis::FileAnalysis,
                              cls: &str|
         -> Option<Span> {
            a.symbols()
                .iter()
                .find(|s| {
                    matches!(s.kind, SymKind::Sub | SymKind::Method)
                        && s.name == method
                        && s.package.as_deref() == Some(cls)
                })
                .map(|s| s.selection_span)
        };
        // Queue entries: (parent leaf, required namespace when a row pins it).
        let mut queue: std::collections::VecDeque<(String, Option<String>)> = analysis
            .declared_parents(&encl)
            .iter()
            .map(|p| (p.clone(), parent_ns(analysis, &encl, p)))
            .collect();
        let mut fallback: Option<RefLocation> = None;
        let mut seen: std::collections::HashSet<(String, String)> = Default::default();
        let mut budget = 32usize;
        while let Some((parent, want_ns)) = queue.pop_front() {
            if budget == 0 {
                break;
            }
            budget -= 1;
            for cached in idx.visible_def_candidates(&parent) {
                // Origin-exclusion for the same-leaf parent: the child's
                // own file also declares the leaf.
                if crate::index::resolve::file_key_eq(
                    &FileKey::Path(cached.path.clone()),
                    &self.origin_key,
                ) && parent == encl
                {
                    continue;
                }
                if !seen.insert((parent.clone(), cached.path.display().to_string())) {
                    continue;
                }
                let whole = idx.whole_present(&cached);
                let cand_ns = whole
                    .symbols()
                    .iter()
                    .find(|s| matches!(s.kind, SymKind::Class) && s.name == parent)
                    .map(|s| s.package.clone().unwrap_or_default());
                if let (Some(want), Some(cand)) = (&want_ns, &cand_ns) {
                    if want != cand {
                        continue;
                    }
                }
                if Url::from_file_path(&cached.path).is_err() {
                    continue;
                }
                if let Some(span) = method_decl_in(&whole, &parent) {
                    let loc = RefLocation {
                        key: FileKey::Path(cached.path.clone()),
                        span,
                        access: AccessKind::Declaration,
                        rewritable: true,
                        label: None,
                    };
                    if whole.declares_interface(&parent) {
                        fallback.get_or_insert(loc);
                    } else {
                        return Some(vec![loc]);
                    }
                } else {
                    // This parent file doesn't define it — walk ITS parents.
                    queue.extend(
                        whole
                            .declared_parents(&parent)
                            .iter()
                            .map(|p| (p.clone(), parent_ns(&whole, &parent, p))),
                    );
                }
            }
        }
        fallback.map(|l| vec![l])
    }

    /// The def site of `member` on `class` — origin symbols first, then the
    /// class's own cached file. Serves the template-family ranked goto-def
    /// (one location per ladder class that actually defines the member).
    pub(super) fn member_def_location(&self, class: &str, member: &str) -> Option<RefLocation> {
        // The member's def span in `fa` under `class`'s owner set, expanded
        // through inline-namespace transparency so a symbol filed under an
        // `inline namespace head` answers a lookup keyed on its transparent
        // parent `absl`. The set is derived once per scanned fa (the inline
        // attribution rides the file that opened the namespace, so it is
        // recomputed per file, never shared).
        let member_span_in = |fa: &crate::model::file_analysis::FileAnalysis, cls: &str| -> Option<Span> {
            let owners = pack_inline_owner_set(fa, cls);
            fa.symbols()
                .iter()
                .find(|s| s.name == member && pack_member_of(fa, s, &owners))
                .map(|s| s.selection_span)
        };
        if let Some(span) = member_span_in(self.origin, class) {
            return Some(self.origin_decl(span));
        }
        let idx = self.idx()?;
        let loc_of = |cached: &crate::model::file_analysis::CachedModule, span: Span| {
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
        // struct/class/enum that is itself a cache key. Every candidate
        // file declaring the class may hold the member — and an INHERITED
        // member lives on an ancestor (`View::query()` finds Eloquent
        // Model's `query`), so the lookup walks the leaf-keyed parent
        // edges child-first (the instance-receiver path gets this from
        // the invocant ladder's ancestor walk; a bareword-scoped call
        // resolves here and needs its own).
        {
            let mut queue: std::collections::VecDeque<String> =
                std::iter::once(class.to_string()).collect();
            let mut seen: std::collections::HashSet<String> = Default::default();
            let mut budget = 64usize;
            while let Some(cls) = queue.pop_front() {
                if !seen.insert(cls.clone()) || budget == 0 {
                    continue;
                }
                budget -= 1;
                for parent in self.origin.declared_parents(&cls) {
                    queue.push_back(parent.clone());
                }
                // The origin's use-map pins the leaf to ONE namespace
                // (`use Support\Facades\Cache;` — without
                // this, gd on `Cache::store` landed on an unrelated
                // same-leaf class in a never-imported namespace). A
                // candidate declaring the class under a DIFFERENT
                // namespace is not the class this file means; candidates
                // with no namespace claim stay admissible.
                let want_ns = self.origin.leaf_namespace(&cls);
                for cached in idx.visible_def_candidates(&cls) {
                    let a = idx.whole_present(&cached);
                    if let (Some(want), Some(cand)) =
                        (&want_ns, a.declared_class_namespace(&cls))
                    {
                        if want != &cand {
                            continue;
                        }
                    }
                    if let Some(span) = member_span_in(&a, &cls) {
                        return loc_of(&cached, span);
                    }
                    for parent in a.declared_parents(&cls) {
                        queue.push_back(parent.clone());
                    }
                }
            }
        }
        let Some((self_path, visible)) = idx.visibility_scope() else {
            return None;
        };
        let self_str = self_path.to_string_lossy().into_owned();
        let connected = |cached: &crate::model::file_analysis::CachedModule| {
            let p = cached.path.to_string_lossy();
            visible.contains(p.as_ref())
                || cached.analysis.pack.include_closure.contains(&self_str)
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
                if let Some(span) = member_span_in(&idx.whole_present(&cached), class) {
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
            if let Some(span) = member_span_in(&idx.whole_present(cached), class) {
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
    pub(super) fn preferred_definitions(&self, decl: RefLocation, decl_fa: &FileAnalysis) -> Vec<RefLocation> {
        if !self.pack {
            return vec![decl];
        }
        let Some(sym) = decl_fa
            .symbols()
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
        let cand_is_def = |a: &FileAnalysis, s: &crate::model::file_analysis::Symbol| {
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
        for s in decl_fa.symbols().iter().filter(|s| cand_is_def(decl_fa, s)) {
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
                let connected = |cached: &std::sync::Arc<crate::model::file_analysis::CachedModule>| {
                    let p = cached.path.to_string_lossy();
                    cached.path != decl_path
                        && (visible.contains(p.as_ref())
                            || cached.analysis.pack.include_closure.contains(&self_str)
                            || cached.analysis.pack.include_closure.contains(&decl_str))
                };
                let mut cands = idx.def_candidates(&sym.name);
                cands.sort_by(|a, b| a.path.cmp(&b.path));
                for cached in &cands {
                    if !connected(cached) {
                        continue;
                    }
                    let key = FileKey::Path(cached.path.clone());
                    let whole = idx.whole_present(cached);
                    for s in whole.symbols().iter().filter(|s| cand_is_def(&whole, s)) {
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
                        for s in whole.symbols().iter().filter(|s| cand_is_def(&whole, s)) {
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
    pub(super) fn prefer_member_defs(&self, decl: RefLocation) -> Vec<RefLocation> {
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
    pub(super) fn overload_arity_definitions(&self) -> Option<Vec<RefLocation>> {
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
            RefKind::FunctionCall => r.resolved_package().map(str::to_string).or_else(|| {
                analysis.find_definition(self.point, Some(idx)).and_then(|sp| {
                    analysis
                        .symbols()
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
            |a: &FileAnalysis, s: &crate::model::file_analysis::Symbol| a.scopes.iter().any(|sc| sc.span == s.span);
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
        for s in analysis.symbols().iter().filter(|s| {
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
            for cached in idx.visible_def_candidates(p) {
                let key = FileKey::Path(cached.path.clone());
                let whole = idx.whole_present(&cached);
                for s in whole.symbols().iter().filter(|s| {
                    s.name == name
                        && matches!(s.kind, SymKind::Sub | SymKind::Method)
                        && pkg_ok(s.package.as_deref())
                }) {
                    let fit = s.param_arity().map(|a| a.fit(argc)).unwrap_or(0);
                    push(owner_matched(s.package.as_deref()), fit, has_body(&whole, s), false, &key, s.selection_span, &mut cands);
                }
            }
        }
        // Cross-file: the full def-candidates table. Connectivity is
        // closure-based under an include-path scope; a scope-less
        // (Transparent) lookup admits every candidate — a name-keyed pack's
        // same-named definitions are genuine siblings (WordPress's noop.php
        // stubs vs the real implementations), and the ranked, never-pruned
        // family is the honest answer where a single winner was confidently
        // wrong. Arity fit then floats the real signature above a stub.
        {
            let scope = idx
                .visibility_scope()
                .map(|(p, v)| (p.to_string_lossy().into_owned(), v));
            let origin_path = key_for_sort(&self.origin_key);
            let mut cached_files = idx.def_candidates(&name);
            cached_files.sort_by(|a, b| a.path.cmp(&b.path));
            for cached in cached_files {
                if cached.path == origin_path {
                    continue;
                }
                let connected = match &scope {
                    Some((self_str, visible)) => {
                        let p = cached.path.to_string_lossy().into_owned();
                        visible.contains(&p)
                            || cached.analysis.pack.include_closure.contains(self_str)
                    }
                    None => true,
                };
                if !connected {
                    continue;
                }
                let key = FileKey::Path(cached.path.clone());
                let whole = idx.whole_present(&cached);
                for s in whole.symbols().iter().filter(|s| {
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
    pub(super) fn origin_decl(&self, span: Span) -> RefLocation {
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

        // `parent::` gd first (pack languages): it EXCLUDES the origin
        // class's own override by construction — every ranked-family lane
        // below would self-answer when the aliased parent shares the
        // enclosing leaf (`use Support\Collection as BaseCollection;
        // class Collection extends BaseCollection`) — and it routes the
        // same-leaf parent by its recorded namespace row.
        if self.pack {
            if let (Some(r), Some(idx)) = (analysis.ref_at(point), self.idx()) {
                if matches!(r.kind, RefKind::MethodCall { .. }) {
                    if let crate::model::conventions::MethodToken::Super(name) =
                        crate::model::conventions::MethodToken::parse(&r.target_name)
                    {
                        if let Some(locs) = self.super_def_locations(r, name, idx) {
                            return locs;
                        }
                    }
                }
            }
        }

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
                            crate::model::file_analysis::ParametricType::instance_from_spelling(&spelling)
                        {
                            let t = crate::model::file_analysis::InferredType::Parametric(p);
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
            if let RefKind::HashKeyAccess { .. } = r.kind {
                use crate::model::file_analysis::HashKeyOwner;
                let owner = match r.hash_key_owner() {
                    Some(o) => Some(o.clone()),
                    None => analysis.deferred_hash_key_owner(r, Some(idx)),
                };
                let class = match &owner {
                    Some(HashKeyOwner::Sub { package: Some(c), .. }) => Some(c.clone()),
                    Some(HashKeyOwner::Class(c)) => Some(c.clone()),
                    _ => None,
                };
                if let (Some(owner), Some(class)) = (owner, class) {
                    // Whichever candidate file of the class holds the key
                    // def (whole view — HashKeyDefs ride the symbols axis).
                    for cached in idx.visible_def_candidates(&class) {
                        if let Some(def) = idx
                            .whole_present(&cached)
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
            // The call-binding lanes — identity from the set's own
            // `function_binding` accessor (import classification first, then
            // the FQ `Function` package), so hover presents the same binding
            // this projection jumps through.
            match self.function_binding() {
                Some(FunctionBinding::Imported { import, path: module_path, remote: remote_name }) => {
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
                        if crate::model::file_analysis::contains_point(&import.span, point) {
                            return vec![line_loc(module_path, 0)];
                        }
                    }
                    // Fall back to just the use statement.
                    return vec![self.origin_decl(import.span)];
                }

                // Fully-qualified call (`Foo::Bar::baz()`) with no import: the
                // qualifier names the package directly; the defining package
                // lives in another module.
                Some(FunctionBinding::Qualified { package: pkg }) => {
                    let bare = r.unqualified_target_name();
                    // `pkg` may be declared in several files (a Perl package
                    // reopens anywhere; C linkage is flat). The query names a
                    // SYMBOL, and that is the disambiguator: the right file
                    // is the one that defines `bare` — package-attributed
                    // first (a reopened package's sub lives under `pkg`),
                    // then any-package (cross-package installs), smallest
                    // path among several definers so repeat runs are
                    // byte-identical. The candidate SET is scope-narrowed by
                    // `visible_def_candidates` (`CandidateSet::scoped` /
                    // `ScopedLookup` — the ONE visibility seam; Perl's own
                    // ranking tier plugs in there, never in a second filter
                    // here).
                    if let Some(cached) = idx.candidate_defining_sub(pkg, bare) {
                        if Url::from_file_path(&cached.path).is_ok() {
                            if let Some(line) = idx
                                .symbols_present(&cached)
                                .sub_info_view(bare)
                                .map(|s| s.def_line())
                            {
                                return vec![line_loc(cached.path.clone(), line)];
                            }
                        }
                    }
                    // No candidate defines `bare`. Fail safe for a pack
                    // `Scope::member` miss: the owner-anchored member lookup
                    // already ran (and missed), so `pkg::bare` is NOT a
                    // module path — manufacturing a file-top `1:1` location
                    // is a confidently-wrong answer (abseil's every-header
                    // `namespace absl` would land on an arbitrary file).
                    // Perl keeps the file-top fallback — landing on the
                    // `.pm` top is meaningful there.
                    if !self.pack {
                        if let Some(cached) = idx.get_cached(pkg) {
                            if Url::from_file_path(&cached.path).is_ok() {
                                return vec![line_loc(cached.path.clone(), 0)];
                            }
                        }
                    }
                }
                None => {}
            }

            // Fully-qualified variable read (`$Foo::Bar::x`, `@Pkg::arr`):
            // the package lives in another module — resolve the package
            // global through the index, mirroring the FQ-call path. Honest
            // miss (no jump) when the package or its decl is absent.
            if let Some((pkg, name)) = r.qualified_var_target() {
                // Same candidate discipline as the FQ-call arm: the
                // declaring file is whichever of `pkg`'s files defines the
                // global, not the name-slot winner.
                let mut cands = idx.visible_def_candidates(pkg);
                cands.sort_by(|a, b| a.path.cmp(&b.path));
                for cached in &cands {
                    if Url::from_file_path(&cached.path).is_ok() {
                        if let Some(def_line) =
                            idx.symbols_present(cached).package_var_def_line(&name, pkg)
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
                // EVERY file declaring the package is a legitimate landing —
                // a reopened package surfaces as a multi-location picker.
                // Two tiers: files whose symbol lives in TYPE space
                // (Package/Class/Module) arbitrate over value-shape
                // fallbacks — a same-named macro/value file must not join
                // the picker when a real type declaration exists.
                let mut type_hits: Vec<RefLocation> = Vec::new();
                let mut value_hits: Vec<RefLocation> = Vec::new();
                for cached in idx.visible_def_candidates(&r.target_name) {
                    if Url::from_file_path(&cached.path).is_ok() {
                        let whole = idx.whole_present(&cached);
                        let loc = |span| RefLocation {
                            key: FileKey::Path(cached.path.clone()),
                            span,
                            access: AccessKind::Declaration,
                            rewritable: true,
                            label: None,
                        };
                        if let Some(s) = whole.symbols().iter().find(|s| {
                            s.name == r.target_name
                                && matches!(
                                    s.kind,
                                    SymKind::Package | SymKind::Class | SymKind::Module
                                )
                        }) {
                            type_hits.push(loc(s.selection_span));
                        // Type space missed: a pack grammar's TYPE guess in
                        // a type/value-ambiguous slot (template argument)
                        // can name a VALUE the pack index registered under
                        // this same bare name — land on ITS decl, not the
                        // file top. Pack-only structural gates; Perl module
                        // lookups keep the file-top fallback.
                        } else if let Some(s) = whole.symbols().iter().find(|s| {
                            s.name == r.target_name
                                && (whole.symbol_is_class_content(s)
                                    || whole.symbol_is_file_scope_value(s))
                        }) {
                            value_hits.push(loc(s.selection_span));
                        } else {
                            value_hits.push(loc(Span {
                                start: tree_sitter::Point::new(0, 0),
                                end: tree_sitter::Point::new(0, 0),
                            }));
                        }
                    }
                }
                let out = if !type_hits.is_empty() { type_hits } else { value_hits };
                if !out.is_empty() {
                    return out;
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
            if let (RefKind::DispatchCall { .. }, Some(owner)) = (&r.kind, r.handler_owner()) {
                let locs = dispatch_handler_locations(owner, &r.target_name, idx);
                if !locs.is_empty() {
                    return locs;
                }
            }

            // Cross-file method goto-def: inherited methods through the index.
            if matches!(r.kind, RefKind::MethodCall { .. }) {
                use crate::model::file_analysis::MethodResolution;
                // FQ `$o->Foo::Bar::m` dispatches the bare `m` on the named class.
                let method = r.unqualified_target_name();
                if let Some(cn) = analysis.method_call_invocant_class(r, Some(idx)) {
                    // The invocant resolved (e.g. a plugin-bridged route token
                    // → controller class) but the controller lives in THIS
                    // file: jump to the local method symbol. The build-time
                    // freeze normally serves same-file dispatch, but a bridged
                    // invocant is never frozen (its class needs the index), so
                    // re-resolve here.
                    let shape = match &r.kind {
                        RefKind::MethodCall { shape, .. } => *shape,
                        _ => Default::default(),
                    };
                    if let Some(MethodResolution::Local { sym_id, .. }) =
                        analysis.resolve_member_in_ancestors(&cn, method, shape, Some(idx))
                    {
                        if let Some(sym) = analysis.symbols().iter().find(|s| s.id == sym_id) {
                            return vec![self.origin_decl(sym.selection_span)];
                        }
                    }
                    if let Some(MethodResolution::CrossFile { ref class, ref def_module }) =
                        analysis.resolve_member_in_ancestors(&cn, method, shape, Some(idx))
                    {
                        // One path for both: a real inherited method lives in
                        // `class`'s own module; a plugin-bridged helper lives
                        // in `def_module` (the bridging file). Same lookup
                        // either way.
                        let module = def_module.as_deref().unwrap_or(class);
                        // The class's package may span files (Perl reopens
                        // packages anywhere) — pick the candidate that
                        // declares `method` under `class`, then a name-level
                        // match (bridged/materialized copies attribute
                        // synthesized methods loosely), smallest path among
                        // several definers; the name-slot winner only as the
                        // last resort (data-member reads below).
                        let chosen = idx
                            .candidate_defining_sub_in_package(module, class, method)
                            .or_else(|| idx.get_cached(module));
                        if let Some(cached) = chosen {
                            // A cross-file DBIC accessor is a deferred emission
                            // MATERIALIZED into the whole cached copy at index
                            // completion (`materialize_gated_emissions`), so the
                            // whole view carries it — no per-query enrichment.
                            let whole = idx.whole_present(&cached);
                            // A value read lands on the stored member first;
                            // the callable arm below stays its fallback.
                            let field_sym = |whole: &FileAnalysis| {
                                whole.symbols().iter().find(|s| {
                                    matches!(
                                        s.kind,
                                        SymKind::Variable | SymKind::Field | SymKind::Enumerator
                                    ) && s.name == method
                                        && s.package.as_deref() == Some(class.as_str())
                                        && whole.symbol_is_class_content(s)
                                }).map(|s| s.selection_span)
                            };
                            if shape == crate::model::file_analysis::MemberShape::Value {
                                if let Some(span) = field_sym(&whole) {
                                    if Url::from_file_path(&cached.path).is_ok() {
                                        return vec![RefLocation {
                                            key: FileKey::Path(cached.path.clone()),
                                            span,
                                            access: AccessKind::Declaration,
                                            rewritable: true,
                                            label: None,
                                        }];
                                    }
                                }
                            }
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
                                        if let Some(sym) = whole.symbols().iter().find(|s| {
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
                            if let Some(sym) = whole.symbols().iter().find(|s| {
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
                for cached in idx.visible_def_candidates(name) {
                    let whole = idx.whole_present(&cached);
                    if let Some(sym) = whole.symbols().iter().find(|s| {
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

        // Identity backstop: the cursor's resolution is a projection GROUP,
        // so its declaration axis IS the answer. The forward lanes above
        // resolve through the owner the REF carries, which for an inherited
        // attr is the subclass (`$self->{size}` in `Gadget` where `Widget`
        // declares `has size`) — they miss, and `references()` would still
        // name the base's decl, because identity already climbed the
        // ancestry to mint the group. Reading the group here is what keeps
        // the two projections from disagreeing; no second ancestry walk.
        if let Some(ResolvedTarget::Group { decl_spans, .. }) = self.resolution() {
            let out: Vec<RefLocation> = decl_spans
                .iter()
                .filter(|(path, _)| path.as_ref().is_none_or(|p| Url::from_file_path(p).is_ok()))
                .map(|(path, span)| RefLocation {
                    key: path.clone().map_or_else(|| self.origin_key.clone(), FileKey::Path),
                    span: *span,
                    access: AccessKind::Declaration,
                    rewritable: true,
                    label: None,
                })
                .collect();
            if !out.is_empty() {
                return out;
            }
        }

        // The same identity backstop for a CALLABLE target. A method
        // resolves to `Target`, never `Group`, so it never reached the arm
        // above — and the cross-file lane that would have answered it is
        // `resolve_method_in_ancestors`, which climbs UPWARD only. A
        // template method (`$self->step_one()` in a base, declared only in
        // the subclass) lives below, so every forward lane misses while
        // `references()` names the child's decl through the override family
        // identity already computed.
        //
        // This projects that same family rather than walking it again: the
        // declaration axis of `references()` IS the answer, and the whole
        // defect is two projections each growing their own mechanism. It is
        // a LAST RESORT — every forward lane has already missed — so the
        // walk it costs is one nobody paid on the answered path.
        if matches!(self.resolution(), Some(ResolvedTarget::Target(_))) {
            let decls: Vec<RefLocation> = self
                .references()
                .into_iter()
                .filter(|r| r.access == AccessKind::Declaration)
                .collect();
            if !decls.is_empty() {
                return decls;
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
}
