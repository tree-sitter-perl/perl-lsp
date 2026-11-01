//! The `definitions()` projection and its ranking helpers (member/overload
//! def locations, preferred-definition laddering).
use super::*;

impl<'a> CandidateSet<'a> {
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
        let member_span_in = |fa: &crate::model::file_analysis::FileAnalysis| -> Option<Span> {
            let owners = pack_inline_owner_set(fa, class);
            fa.symbols()
                .iter()
                .find(|s| s.name == member && pack_member_of(fa, s, &owners))
                .map(|s| s.selection_span)
        };
        if let Some(span) = member_span_in(self.origin) {
            return Some(self.origin_decl(span));
        }
        let idx = self.idx()?;
        let loc_of = |cached: &crate::model::file_analysis::CachedModule, span: Span| {
            Url::from_file_path(&cached.path).ok()?;
            Some(RefLocation::at(FileKey::Path(cached.path.clone()), span, AccessKind::Declaration))
        };
        // Class-keyed cached module — the fast path when `class` names a
        // struct/class/enum that is itself a cache key. Every candidate
        // file declaring the class may hold the member.
        for cached in idx.visible_def_candidates(class) {
            if let Some(span) = member_span_in(&idx.whole_present(&cached)) {
                return loc_of(&cached, span);
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
        hit.map(|(p, span)| RefLocation::at(FileKey::Path(PathBuf::from(p)), span, AccessKind::Declaration))
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
                    && sym.flags.contains(crate::model::file_analysis::SymbolFlags::EXTERN)
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
                            && !s.flags.contains(crate::model::file_analysis::SymbolFlags::EXTERN)
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
            defs.push(RefLocation::at(key.clone(), span, AccessKind::Declaration));
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
                                RefLocation::at(key.clone(), s.selection_span, AccessKind::Declaration),
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
        let name = r.unqualified_target_name(analysis.names()).to_string();
        // Anchor the family's scope on the primary resolution so overload
        // siblings gather in the right class/namespace without re-deriving C++
        // name lookup here.
        let pkg: Option<String> = match &r.kind {
            // An overload set is one class's; a union receiver falls through
            // to the member lane, which offers every arm's definition.
            RefKind::MethodCall { .. } => {
                Some(analysis.receiver_classes(r, Some(idx), ReceiverPolicy::Navigate).single()?.to_string())
            }
            RefKind::FunctionCall | RefKind::ImportedName => r.resolved_package().map(str::to_string).or_else(|| {
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
        // Sort key, best-first: reachable under the asker's config (the
        // candidate's `#if` arm), owner match, arity fit, bodied, local, then a
        // total (path, row, col) order for cache determinism.
        let ranker = ArmRanker::new(analysis, Some(idx));
        type Cand = (u8, bool, u8, bool, bool, PathBuf, usize, usize, RefLocation);
        let mut cands: Vec<Cand> = Vec::new();
        let push = |reach: crate::build::cpp_macro_model::Reachability,
                    owner: bool,
                    fit: u8,
                    bodied: bool,
                    local: bool,
                    key: &FileKey,
                    span: Span,
                    cands: &mut Vec<Cand>| {
            if cands
                .iter()
                .any(|c| file_key_eq(&c.8.key, key) && c.8.span == span)
            {
                return;
            }
            cands.push((
                reach.rank(),
                owner,
                fit,
                bodied,
                local,
                key_for_sort(key),
                span.start.row,
                span.start.column,
                RefLocation::at(key.clone(), span, AccessKind::Declaration).with_label(reach.label()),
            ));
        };
        for s in analysis.symbols().iter().filter(|s| {
            s.name == name
                && matches!(s.kind, SymKind::Sub | SymKind::Method)
                && pkg_ok(s.package.as_deref())
        }) {
            let fit = s.param_arity().map(|a| a.fit(argc)).unwrap_or(0);
            push(ranker.reach(analysis, s), owner_matched(s.package.as_deref()), fit, has_body(analysis, s), true, &self.origin_key, s.selection_span, &mut cands);
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
                    push(ranker.reach(&whole, s), owner_matched(s.package.as_deref()), fit, has_body(&whole, s), false, &key, s.selection_span, &mut cands);
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
                    || cached.analysis.pack.include_closure.contains(&self_str);
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
                    push(ranker.reach(&whole, s), owner_matched(s.package.as_deref()), fit, has_body(&whole, s), false, &key, s.selection_span, &mut cands);
                }
            }
        }
        if cands.len() < 2 {
            return None;
        }
        cands.sort_by(|a, b| {
            a.0.cmp(&b.0) // reachable first
                .then_with(|| b.1.cmp(&a.1)) // owner-matched first
                .then_with(|| b.2.cmp(&a.2)) // arity fit, best first
                .then_with(|| b.3.cmp(&a.3)) // bodied first
                .then_with(|| b.4.cmp(&a.4)) // local first
                .then_with(|| a.5.cmp(&b.5)) // path
                .then_with(|| (a.6, a.7).cmp(&(b.6, b.7))) // row, col
        });
        Some(cands.into_iter().map(|c| c.8).collect())
    }

    /// A declaration site in the origin file.
    pub(super) fn origin_decl(&self, span: Span) -> RefLocation {
        RefLocation::at(self.origin_key.clone(), span, AccessKind::Declaration)
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
                            .map(|(m, key, r)| RefLocation::at(key.clone(), m.selection_span, AccessKind::Declaration).with_label(r.label()))
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
                if let Some(inv) = r.member_site().and_then(|m| m.invocant_span) {
                    if let Some(recv_ty) = analysis.expr_type_at_span(inv, self.idx()) {
                        if recv_ty.as_parametric().is_some() {
                            let member = r.unqualified_target_name(analysis.names());
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
            RefLocation::at(FileKey::Path(path), Span { start: p, end: p }, AccessKind::Declaration)
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
                            return vec![RefLocation::at(FileKey::Path(cached.path.clone()), def.selection_span, AccessKind::Declaration)];
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
                    let bare = r.unqualified_target_name(analysis.names());
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
            if let Some((pkg, name)) = r.qualified_var_target(analysis.names()) {
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
            //
            // The name is the class the token names as its file spells it
            // (`\Lib\Clock` is `Lib\Clock`). A widened answer is a stand-in,
            // not a declaration of it: the labeled widening lane lands those.
            let named_class = matches!(r.kind, RefKind::PackageRef)
                .then(|| analysis.spelled_identity(r))
                .and_then(|name| match idx.visible_def_candidates_widening(&name) {
                    (_, true) => None,
                    (cands, false) => Some((name, cands)),
                });
            if let Some((name, cands)) = named_class {
                // EVERY file declaring the package is a legitimate landing —
                // a reopened package surfaces as a multi-location picker.
                // Two tiers: files whose symbol lives in TYPE space
                // (Package/Class/Module) arbitrate over value-shape
                // fallbacks — a same-named macro/value file must not join
                // the picker when a real type declaration exists.
                let mut type_hits: Vec<RefLocation> = Vec::new();
                let mut value_hits: Vec<RefLocation> = Vec::new();
                for cached in cands {
                    if Url::from_file_path(&cached.path).is_ok() {
                        let whole = idx.whole_present(&cached);
                        let loc = |span| RefLocation::at(FileKey::Path(cached.path.clone()), span, AccessKind::Declaration);
                        if let Some(s) = whole.symbols().iter().find(|s| {
                            s.name == name
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
                            s.name == name
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
                if let Some(path) = idx.module_path_cached(&name) {
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

            // Cross-file member goto-def: inherited methods and fields
            // through the index, walking the family the use's syntax names.
            if let Some(want) = MemberKind::of_ref(&r.kind) {
                use crate::model::file_analysis::MethodResolution;
                let namespace = analysis.names().member_namespace;
                // A union receiver dispatches on every arm: each arm's
                // definition is offered.
                let mut found: Vec<RefLocation> = Vec::new();
                for cn in analysis.receiver_classes(r, Some(idx), ReceiverPolicy::Navigate) {
                    // FQ `$o->Foo::Bar::m` dispatches the bare `m` on the named
                    // class; a construction asks its class for the constructor
                    // by role. The invocant may have resolved (a plugin-bridged
                    // route token → controller class) to a class in THIS file:
                    // a bridged invocant is never frozen (its class needs the
                    // index), so the member is resolved here.
                    let key = r.member_key(analysis.names());
                    let resolution = analysis.resolve_member_key_in_ancestors(&cn, key, Some(idx), want);
                    // A member the language guarantees has no declaration: an
                    // honest miss. A construction's own class token answers
                    // through its `PackageRef`, the cursor's companion.
                    if matches!(resolution, Some(MethodResolution::Root { .. })) {
                        continue;
                    }
                    let Some(called) = (match &resolution {
                        Some(res) => Some(analysis.resolved_member_name(res, key)),
                        None => key.name().map(str::to_string),
                    }) else {
                        continue;
                    };
                    let method: &str = &called;
                    let on_class = |resolution: Option<MethodResolution>| -> Vec<RefLocation> {
                        if let Some(MethodResolution::Local { sym_id, .. }) = resolution {
                            if let Some(sym) = analysis.symbols().iter().find(|s| s.id == sym_id) {
                                return vec![self.origin_decl(sym.selection_span)];
                            }
                        }
                        if let Some(MethodResolution::CrossFile { ref class, ref def_module, .. }) =
                            resolution
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
                                // (`GatedEmission`) the view applies, so the whole
                                // view carries it — no per-query enrichment.
                                let whole = idx.whole_present(&cached);
                                // A stored member of the class: a data field or
                                // an enum constant, not a sub.
                                let stored_member = || {
                                    whole.symbols().iter().find(|s| {
                                        MemberKind::of_sym(s.kind) == MemberKind::Value
                                            && s.name == method
                                            && s.package.as_deref() == Some(class.as_str())
                                            && whole.symbol_is_class_content(s)
                                    })
                                };
                                let stored_loc = |sym: &crate::model::file_analysis::Symbol| {
                                    Url::from_file_path(&cached.path).is_ok().then(|| RefLocation::at(FileKey::Path(cached.path.clone()), sym.selection_span, AccessKind::Declaration))
                                };
                                // A value read lands on the stored member first.
                                if want == MemberKind::Value {
                                    if let Some(loc) = stored_member().and_then(stored_loc) {
                                        return vec![loc];
                                    }
                                }
                                let callable = want.admits_decl(SymKind::Sub, namespace)
                                    .then(|| whole.sub_info_view(method))
                                    .flatten();
                                if let Some(sub_info) = callable {
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
                                                return self.prefer_member_defs(RefLocation::at(FileKey::Path(cached.path.clone()), sym.selection_span, AccessKind::Declaration));
                                            }
                                        }
                                        return vec![line_loc(
                                            cached.path.clone(),
                                            sub_info.def_line(),
                                        )];
                                    }
                                }
                                // A call reaches a stored member where the
                                // language keeps one member namespace.
                                if want.admits_decl(SymKind::Field, namespace) {
                                    if let Some(loc) = stored_member().and_then(stored_loc) {
                                        return vec![loc];
                                    }
                                }
                            }
                        }
                        Vec::new()
                    };
                    for loc in on_class(resolution) {
                        if !found.iter().any(|l| file_key_eq(&l.key, &loc.key) && l.span == loc.span) {
                            found.push(loc);
                        }
                    }
                }
                if !found.is_empty() {
                    return found;
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
            if matches!(r.kind, RefKind::FunctionCall | RefKind::ImportedName | RefKind::Variable) {
                let name = r.unqualified_target_name(analysis.names());
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
                                RefLocation::at(FileKey::Path(cached.path.clone()), sym.selection_span, AccessKind::Declaration),
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
                .map(|(path, span)| RefLocation::at(path.clone().map_or_else(|| self.origin_key.clone(), FileKey::Path), *span, AccessKind::Declaration))
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

        // A class identity nothing indexed declares lands on the same-leaf
        // classes that stand in for it where the scope widens — after every
        // exact lane, so a stranger never competes with the identity's own
        // declaration, and labeled with the class it actually is.
        if let Some(ResolvedTarget::Target(t)) = self.resolution() {
            if matches!(t.kind, TargetKind::Package) {
                let out: Vec<RefLocation> =
                    crate::model::file_analysis::widened_class_stand_ins(idx, &t.name, &t.names)
                        .into_iter()
                        .filter(|w| Url::from_file_path(&w.path).is_ok())
                        .map(|w| RefLocation::at(FileKey::Path(w.path), w.span, AccessKind::Declaration).with_rewrite(SiteRewrite::Skip).with_label(Some(format!("widened: `{}` stands in for `{}`", w.class, t.name))))
                        .collect();
                if !out.is_empty() {
                    return out;
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
}
