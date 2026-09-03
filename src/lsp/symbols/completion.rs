//! Completion items: candidate conversion, in-scope + native paths, auto-import edits.

use super::*;

pub(crate) fn fa_completion_kind(kind: &FaSymKind) -> CompletionItemKind {
    match kind {
        FaSymKind::Sub => CompletionItemKind::FUNCTION,
        FaSymKind::Method => CompletionItemKind::METHOD,
        FaSymKind::Variable => CompletionItemKind::VARIABLE,
        FaSymKind::Field => CompletionItemKind::FIELD,
        FaSymKind::Enumerator => CompletionItemKind::ENUM_MEMBER,
        FaSymKind::Package => CompletionItemKind::CLASS,
        FaSymKind::Class => CompletionItemKind::CLASS,
        FaSymKind::Module => CompletionItemKind::MODULE,
        FaSymKind::HashKeyDef => CompletionItemKind::PROPERTY,
        FaSymKind::Handler => CompletionItemKind::EVENT,
        FaSymKind::Namespace => CompletionItemKind::MODULE,
    }
}

/// Rank scope-variable candidates whose inferred type matches `expected`
/// first, keeping every other candidate in place (never prunes). A matching
/// variable keeps its `PRIORITY_LOCAL` slot while the non-matching locals it
/// leads are nudged one tier down, so the client's sort_text agrees with the
/// stable reorder the CLI/gold sees. Exact `InferredType` equality, or same
/// class name (a `ClassName` matches by class).
fn rank_candidates_by_expected_type(
    candidates: &mut Vec<CompletionCandidate>,
    expected: &InferredType,
    analysis: &FileAnalysis,
    point: Point,
) {
    let is_match = |c: &CompletionCandidate| -> bool {
        matches!(c.kind, FaSymKind::Variable)
            && analysis
                .inferred_type_via_bag(&c.label, point)
                .is_some_and(|t| inferred_type_matches(expected, &t))
    };
    let mut tagged: Vec<(bool, CompletionCandidate)> =
        candidates.drain(..).map(|c| (is_match(&c), c)).collect();
    // Demote every non-match by one from ITS OWN tier, rather than only
    // variables sitting exactly on `PRIORITY_LOCAL`. The old guard never
    // fired on the path this function actually serves: an in-scope `my` in
    // `complete_general` carries `PRIORITY_FILE_WIDE`, so `$total` and
    // `$label` both shipped as `010…` and the type-match ranking existed
    // ONLY as the position this reorder gave them — invisible while the
    // under-cap list went out unsorted, and lost the moment it is sorted
    // (`010$label` sorts before `010$total`).
    //
    // One step is enough and cannot cross a tier: the priority constants are
    // at least two apart, so a demoted candidate stays between its own tier
    // and the next.
    for (m, c) in tagged.iter_mut() {
        if !*m {
            c.sort_priority = c.sort_priority.saturating_add(1);
        }
    }
    tagged.sort_by_key(|(m, _)| !*m); // stable: matches (key false) lead
    *candidates = tagged.into_iter().map(|(_, c)| c).collect();
}

/// Does `actual` satisfy the `expected` slot type — exact enum equality, or
/// (for object types) the same class name.
fn inferred_type_matches(expected: &InferredType, actual: &InferredType) -> bool {
    expected == actual
        || matches!(
            (expected.class_name(), actual.class_name()),
            (Some(a), Some(b)) if a == b
        )
}

/// Payload ceiling for one completion response. The unbounded tiers (the
/// auto-import export firehose, the module-name universe, a pack include
/// closure) scale with the WORKSPACE, not the cursor — measured 7.8 MB /
/// ~50k items per keystroke at the 138k-file corpus. An editor renders
/// ~a dozen rows; 200 leaves client-side fuzzy filtering ample headroom
/// between keystrokes while keeping the worst-case payload ~30 KB
/// (~160 B/item measured), and it sits ABOVE the bounded tiers' typical
/// sizes (in-scope + imports + builtins ≈ low hundreds) so ordinary files
/// still get their complete list with `isIncomplete: false`.
pub const MAX_COMPLETION_ITEMS: usize = 200;

/// Rank-then-cut to `MAX_COMPLETION_ITEMS`. Returns `true` when the list
/// was reduced — the LSP `isIncomplete` flag, the honest "there are more,
/// re-query as you type" signal (a silently truncated list that claims
/// completeness makes the client cache it and never ask again).
///
/// Order of operations is the design: narrow by the TYPED prefix first
/// (each keystroke re-queries under `isIncomplete`, so the server-side
/// filter converges to the complete answer), then rank by the same key the
/// client sorts on (`sort_text`, label fallback) so the cut keeps the
/// useful half — local/in-scope/imported tiers carry lower sort_text than
/// the workspace firehose by construction. Under the cap nothing changes:
/// the full list returns as a complete (client-cacheable) response.
pub(crate) fn cap_completion_items(items: &mut Vec<CompletionItem>, typed_prefix: &str) -> bool {
    // Sorted whether or not anything is cut. The tiers assemble from
    // hash-backed stores, so an unsorted list ships in iteration order and
    // differs between two runs of the same binary on the same file — 173 of
    // 1,458 positions over four cold runs (`bench/sweep`), every one of them
    // under the cap. It also means the priority tiers and the type-match
    // ranking were computed and then DISCARDED below 200 items, which is
    // where most lists live.
    //
    // This does not claim users saw scrambled lists: a conforming client
    // re-sorts by `sort_text`. It makes our own output reproducible, and it
    // is what puts the ranking work into effect at all.
    if items.len() <= MAX_COMPLETION_ITEMS {
        sort_completion_items(items);
        return false;
    }
    if !typed_prefix.is_empty() {
        items.retain(|i| {
            i.filter_text
                .as_deref()
                .unwrap_or(&i.label)
                .starts_with(typed_prefix)
        });
    }
    if items.len() > MAX_COMPLETION_ITEMS {
        sort_completion_items(items);
        items.truncate(MAX_COMPLETION_ITEMS);
    }
    true
}

/// The ONE finishing step of every completion assembly, Perl and pack
/// (`docs/adr/sibling-forks.md` — the two-bug fork's shared skeleton):
/// the slot's typed prefix narrows the over-cap cut, the cap
/// ranks-then-cuts, and `isIncomplete` composes as "the cut happened OR a
/// prefix-gated source is live". A list complete for THIS prefix but not
/// for the next keystroke must say so whichever lane assembled it — the
/// flag half of exactly the divergence where one lane answered null while
/// its sibling branch preserved an incomplete-empty response.
/// `sigil_active` is the Perl lane's one asymmetry, stated here once: a
/// sigil-triggered list's labels carry the sigil the typed prefix lacks,
/// so prefix-narrowing would empty the list; the pack lane has no sigil
/// trigger and passes false.
pub(crate) fn finish_completion(
    mut items: Vec<CompletionItem>,
    slot: &crate::lsp::cursor_slot::Slot,
    sigil_active: bool,
    live_prefix_gated_sources: bool,
) -> (Vec<CompletionItem>, bool) {
    let typed_prefix = match slot {
        crate::lsp::cursor_slot::Slot::Identifier { prefix } if !sigil_active => prefix.as_str(),
        _ => "",
    };
    let capped = cap_completion_items(&mut items, typed_prefix);
    (items, capped || live_prefix_gated_sources)
}

/// The order the client sorts on (`sort_text`, label fallback), with label
/// then kind as tie-breaks so the comparator is TOTAL. `sort_by` is stable,
/// so two candidates agreeing on every key would otherwise keep the order
/// the producing map iterated in — the nondeterminism this exists to remove.
fn sort_completion_items(items: &mut [CompletionItem]) {
    items.sort_by(|a, b| {
        let ka = a.sort_text.as_deref().unwrap_or(&a.label);
        let kb = b.sort_text.as_deref().unwrap_or(&b.label);
        ka.cmp(kb)
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| format!("{:?}", a.kind).cmp(&format!("{:?}", b.kind)))
    });
}

pub(crate) fn candidate_to_completion_item(c: CompletionCandidate) -> CompletionItem {
    let additional_text_edits = if c.additional_edits.is_empty() {
        None
    } else {
        Some(
            c.additional_edits
                .iter()
                .map(|(span, text)| TextEdit {
                    range: span_to_range(*span),
                    new_text: text.clone(),
                })
                .collect(),
        )
    };
    // `filter_text` is what LSP clients match the typed prefix against
    // when narrowing the completion list client-side. By default it's
    // the label. But when `insert_text` differs (e.g. dispatch-target
    // candidates insert `'connect'` while the label is just `connect`),
    // some clients fall back to `insert_text` for filtering — then
    // typing `c` after `(` stops matching because insert_text starts
    // with `'`. Set filter_text explicitly to the bare label so
    // client-side filtering keys on the name regardless.
    let filter_text = Some(c.label.clone());

    // Sort text places dispatch handlers ABOVE anything
    // complete_general can produce. Both default to sort_priority 0;
    // tied at "000" they interleave alphabetically (connect, fire,
    // message, wire) which makes handlers look like they're mixed
    // into noise. Prefixing with a space character ensures the
    // handler group sorts first as a block — space (0x20) < digit
    // (0x30) lexicographically.
    //
    // The label is the intra-priority tie-break in every case (module /
    // import-list / qualified-path candidates carry it explicitly, and
    // it's what a client falls back to for equal sortText anyway — so
    // spelling it here is ranking-neutral for the identifier/member/key
    // arms and lets this one projection reproduce those arms byte-for-byte).
    let sort_text = if matches!(c.kind, FaSymKind::Handler) {
        Some(format!(" {:03}{}", c.sort_priority, c.label))
    } else {
        Some(format!("{:03}{}", c.sort_priority, c.label))
    };
    let kind = if let Some(ref d) = c.display_override {
        handler_display_to_completion_kind(d)
    } else {
        fa_completion_kind(&c.kind)
    };
    CompletionItem {
        label: c.label,
        kind: Some(kind),
        detail: c.detail,
        insert_text: c.insert_text,
        filter_text,
        sort_text,
        additional_text_edits,
        ..Default::default()
    }
}

/// Language-agnostic in-scope completion: every symbol visible from
/// `point` — top-level definitions (functions / classes / packages,
/// globally addressable) plus locals / params / methods / fields whose
/// declaring scope encloses the cursor — as plain CompletionItems. The
/// client filters by the typed prefix (sigils and all). This is the
/// pack-language completion path (half 1): no cursor context, no member
/// resolution — the `.`/`->` receiver seam is a separate design.
pub fn in_scope_completion(analysis: &FileAnalysis, point: Point) -> Vec<CompletionItem> {
    use std::collections::HashSet;
    let chain: HashSet<_> = analysis
        .scope_at(point)
        .map(|s| analysis.scope_chain(s).into_iter().collect())
        .unwrap_or_default();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut items = Vec::new();
    for sym in analysis.symbols() {
        // Top-level defs are addressable anywhere; everything else
        // (params, locals, a class's methods/fields) only where the
        // declaring scope is on the cursor's scope chain.
        let top_level = matches!(
            sym.kind,
            FaSymKind::Sub | FaSymKind::Class | FaSymKind::Package
        );
        if !top_level && !chain.contains(&sym.scope) {
            continue;
        }
        if sym.name.is_empty() || !seen.insert(sym.name.as_str()) {
            continue;
        }
        items.push(CompletionItem {
            label: sym.name.clone(),
            kind: Some(fa_completion_kind(&sym.kind)),
            ..Default::default()
        });
    }
    items
}

pub fn completion_items(
    files: &crate::index::file_store::FileStore,
    origin_key: &crate::index::file_store::FileKey,
    analysis: &FileAnalysis,
    tree: &Tree,
    source: &str,
    pos: Position,
    module_index: &ModuleIndex,
    stable_packages: Option<&[(String, usize)]>,
) -> (Vec<CompletionItem>, bool) {
    let point = position_to_point(pos);

    // Plugin query hook — runs BEFORE the native path. A plugin can
    // contribute items and optionally claim exclusivity for the slot
    // (e.g. Minion's arg-0 task-name completion: pure tasks, no
    // Minion instance-method firehose).
    if let Some(qctx) = cursor_context::build_plugin_query_context(analysis, tree, source.as_bytes(), point) {
        let registry = crate::build::plugin::default_plugin_registry();
        let (uses, parents) = analysis.trigger_view_at(point);
        let query = crate::build::plugin::TriggerQuery {
            package_uses: &uses,
            package_parents: &parents,
        };
        let mut plugin_items: Vec<CompletionItem> = Vec::new();
        let mut exclusive = false;
        for p in registry.applicable(&query) {
            if let Some(answer) = p.on_completion(&qctx) {
                if answer.exclusive { exclusive = true; }
                for c in answer.items {
                    plugin_items.push(plugin_completion_to_item(c));
                }
                // Plugin-delegated dispatch-target completion: walk
                // Handler symbols whose owner matches and contribute
                // their names as items. Saves each plugin from
                // reimplementing the symbol-table scan.
                if let Some(req) = answer.dispatch_targets_for {
                    plugin_items.extend(dispatch_target_items_for(
                        analysis, module_index, &req.owner_class, &req.dispatcher_names,
                    ));
                }
            }
        }
        if exclusive {
            return (plugin_items, false);
        }
        if !plugin_items.is_empty() {
            let (native, is_incomplete) = completion_items_native(files, origin_key, analysis, tree, source, pos, module_index, stable_packages);
            let mut out = plugin_items;
            out.extend(native);
            return (out, is_incomplete);
        }
    }

    completion_items_native(files, origin_key, analysis, tree, source, pos, module_index, stable_packages)
}

/// Test-only convenience: completion against a bare analysis with an empty
/// store (gathering still routes through the CandidateSet; visibility
/// defaults to the full VISIBLE universe).
#[cfg(test)]
pub fn completion_items_for_test(
    analysis: &FileAnalysis,
    tree: &Tree,
    source: &str,
    pos: Position,
    module_index: &ModuleIndex,
    stable_packages: Option<&[(String, usize)]>,
) -> Vec<CompletionItem> {
    let files = crate::index::file_store::FileStore::new();
    let key = crate::index::file_store::FileKey::Path(std::path::PathBuf::from("/test/origin.pl"));
    completion_items(&files, &key, analysis, tree, source, pos, module_index, stable_packages).0
}

/// The native completion path — the plugin-aware `completion_items`
/// wrapper above falls through to it.
#[allow(clippy::too_many_arguments)]
fn completion_items_native(
    files: &crate::index::file_store::FileStore,
    origin_key: &crate::index::file_store::FileKey,
    analysis: &FileAnalysis,
    tree: &Tree,
    source: &str,
    pos: Position,
    module_index: &ModuleIndex,
    stable_packages: Option<&[(String, usize)]>,
) -> (Vec<CompletionItem>, bool) {
    let point = position_to_point(pos);
    // Candidate GATHERING routes through the resolution CandidateSet — the
    // same visible universe references/rename/goto-def project from
    // (docs/adr/resolution-candidate-set.md). The cursor-context matching
    // below decides which slot the cursor is in; the set decides where the
    // identifier names come from.
    let cs = crate::index::resolve::resolve(
        files,
        analysis,
        origin_key.clone(),
        point,
        Some(module_index),
        crate::index::resolve::OverrideScope::default(),
    );

    // The slot verdict (`docs/adr/cursor-slots.md`) — Perl's detector
    // wraps `cursor_context`'s tree-then-text chain unchanged.
    let crate::lsp::cursor_slot::DetectedSlot { slot, arm: slot_arm } = crate::lsp::cursor_slot::detect_slot(
        analysis, tree, source, point, "perl", Some(module_index));
    // Bare-sigil trigger (`$|`/`@|`/`%|`) decoded once so the match below
    // doesn't need a second borrow of `slot` inside its own arm.
    let sigil_trigger = slot.sigil();

    // Mid-string completion for plugin-emitted MethodCallRefs. When the
    // cursor sits inside the span of a MethodCallRef emitted by a plugin
    // (e.g. `->to('Users#lis|')` in mojo-routes), offer methods on the
    // target class — prefix-filtered by whatever's been typed since the
    // `#` (or the whole prefix if none). This generalizes: any plugin
    // that drops a MethodCallRef at a string span gets scoped method
    // completion for free. Runs first so it preempts the generic paths.
    if let Some(refs) = refs_at_point_matching(analysis, point, |r|
        matches!(r.kind, RefKind::MethodCall { .. })
    ) {
        for r in &refs {
            if let RefKind::MethodCall { invocant, .. } = &r.kind {
                let early = mid_string_methodref_completions(
                    analysis, module_index, invocant.text(), source, point, r.span,
                );
                if !early.is_empty() {
                    return (early, false);
                }
            }
        }
    }

    // Dispatch-target completions are orthogonal to the context match:
    // inside `$obj->emit(^)` the cursor is both after a `->` (tree
    // detects `Method`) and inside call args. Pull the call context out
    // once, prepend handler completions at arg-0, and SUPPRESS the global
    // sub/module firehose at arg-N>0 so comma-triggered completion in a
    // dispatch call doesn't dump hundreds of unrelated symbols (sig help
    // is the right affordance past arg-0).
    //
    // Dispatch items go in a separate vec so we can retarget their
    // textEdit range to the string-content span mid-string, without
    // having to filter the shared `candidates` buffer by kind later.
    let mut dispatch_items: Vec<CompletionItem> = Vec::new();
    let mut candidates: Vec<CompletionCandidate> = Vec::new();
    let mut suppress_firehose = false;
    if let Some(call_ctx) = cursor_context::find_call_context(tree, source.as_bytes(), point) {
        if call_ctx.is_method {
            let dispatch_class = analysis.invocant_text_to_class(call_ctx.invocant.as_deref(), point);
            let has_any_handlers = dispatch_class.as_ref().is_some_and(|c|
                class_has_dispatch_handlers(analysis, module_index, c, &call_ctx.name)
            );
            // Debug line for dispatch completion — one-shot diagnoses
            // every "starting to type kills completion" / "no routes
            // offered" report. Includes the four values that together
            // determine whether dispatch fires and which handlers pass
            // the ancestor-walk filter: call name, invocant text,
            // resolved class (None = inferred_type miss), active_param
            // (>0 short-circuits to vars-only), and has_any_handlers
            // (false = bridges empty or filter mismatch).
            log::debug!(
                "completion dispatch: method={:?} invocant={:?} class={:?} active_param={} has_handlers={}",
                call_ctx.name, call_ctx.invocant, dispatch_class,
                call_ctx.active_param, has_any_handlers,
            );

            if call_ctx.active_param == 0 && has_any_handlers {
                // arg-0 of a known dispatcher: handlers at the top,
                // suppress the global sub/module firehose that would
                // otherwise drown them.
                let dispatch_cands = dispatch_target_completions(
                    analysis,
                    module_index,
                    call_ctx.invocant.as_deref(),
                    &call_ctx.name,
                    point,
                    tree,
                );
                dispatch_items.extend(
                    dispatch_cands.into_iter().map(candidate_to_completion_item),
                );
                // When the cursor is inside the string arg
                // (`url_for('/us|ers/profile')`) pin each item's
                // textEdit to the string-content span. The client's
                // default word-at-cursor (nvim's `iskeyword` default
                // excludes `/`, `#`, `:`) can't see across those
                // chars, so filter_text alone is dropped for labels
                // like `/users/profile` or `Users#list`. textEdit.range
                // tells the client "filter by the whole in-range
                // text" — works regardless of keyword class.
                if let Some(span) = string_content_span_at(tree, point) {
                    retarget_items_to_span(&mut dispatch_items, span);
                }
                suppress_firehose = true;
            } else if call_ctx.active_param > 0 && has_any_handlers
                && !matches!(slot, Slot::Key { .. })
            {
                // Past arg-0 in a known dispatcher: the only sensible
                // completion is variables-in-scope (candidates for
                // passing as the next arg). Sig help handles shape
                // guidance. Short-circuit the context match entirely.
                //
                // EXCEPT when the cursor is sitting inside a nested
                // hash literal — that's a HashKey context and the
                // callee (or a plugin) has real keys to offer for it
                // (Minion's `enqueue(..., [...], { | })` options).
                // Skipping the short-circuit there lets the HashKey
                // match run and populate `priority`/`queue`/etc.
                let vars_only: Vec<CompletionCandidate> = cs.complete("", false)
                    .into_iter()
                    .filter(|c| matches!(c.kind, FaSymKind::Variable | FaSymKind::Field))
                    .collect();
                candidates.extend(vars_only);
                return (candidates.drain(..).map(candidate_to_completion_item).collect(), false);
            }
        }
    }

    candidates.extend::<Vec<CompletionCandidate>>(match slot {
        Slot::Member { ref receiver, .. } => {
            // In-scope lexical methods ride every `->` completion with their
            // mandatory `&` prefix — they're excluded from the class-keyed
            // walks below, so this is their one entry (empty for pack
            // languages, which mint no lexical subs).
            let mut member_cands = analysis.complete_lexical_methods_at(point);
            member_cands.extend(if let Some(ref ty) = receiver.receiver_type {
                // `class_name_lenient` peels `Optional<Foo>` to `Foo` so an
                // unguarded optional receiver still offers its methods — the
                // same lenient receiver projection goto/hover/refs now use.
                if let Some(cn) = ty.class_name_lenient() {
                    analysis.complete_methods_for_class(cn, Some(module_index))
                } else {
                    // Ref types get deref snippet completions (handled below)
                    Vec::new()
                }
            } else {
                let invocant_text = receiver.receiver_text.as_deref().unwrap_or("");
                analysis.complete_methods(invocant_text, point, Some(module_index))
            });
            member_cands
        }
        Slot::Key { ref owner } => {
            // Keys already written in the enclosing hash literal —
            // they shouldn't re-appear in the suggestions. Scoped to
            // the hash_expression directly so unrelated nearby calls
            // don't interfere. Works for both class-typed hashes and
            // sub-owned ones.
            let used = cursor_context::used_keys_in_enclosing_hash(tree, source.as_bytes(), point);
            let class_name = owner.owner_type.as_ref().and_then(|t| t.class_name());
            let candidates = if let Some(cn) = class_name {
                analysis.complete_hash_keys_for_class(cn, point, Some(module_index))
            } else if let Some(ref sub_name) = owner.source_sub {
                // Routes to HashKeyOwner::Sub { name } — catches both
                // plugin-emitted HashKeyDefs (minion enqueue options)
                // AND body-derived keys from `$opts->{...}` accesses
                // in a final-hashref param. Previously this branch
                // was skipped when owner_type was None, so real hash
                // literals at a call-arg position returned nothing.
                analysis.complete_hash_keys_for_sub(sub_name, point, Some(module_index))
            } else {
                analysis.complete_hash_keys(&owner.var_text, point, Some(module_index))
            };
            candidates.into_iter().filter(|c| !used.contains(&c.label)).collect()
        }
        Slot::Import { ref module } => {
            if let Some(ref name) = module {
                // The export surface is entity content on `CachedModule`;
                // the "still indexing" placeholder is a slot affordance
                // (no entity to gather yet), so it stays adapter-side.
                // Union across every candidate file of the module — a split
                // exporter's surface spans the set (dedup by label, first
                // candidate's detail wins).
                let cands = module_index.visible_def_candidates(name);
                if cands.is_empty() {
                    return (vec![import_list_loading_placeholder(name)], false);
                }
                let mut seen = std::collections::HashSet::new();
                return (
                    cands
                        .iter()
                        .flat_map(|cached| {
                            module_index.whole_present(cached).import_list_candidates()
                        })
                        .filter(|c| seen.insert(c.label.clone()))
                        .map(candidate_to_completion_item)
                        .collect(),
                    false,
                );
            }
            Vec::new()
        }
        Slot::ModulePath { ref prefix } => {
            // `use Foo::<cursor>` → the loadable-module half; `Foo::<cursor>`
            // mid-expression → the qualified-path drill (subs + sub-packages).
            // Both are candidate-level on the set; this branch is the answer,
            // so it returns directly (the global firehose is suppressed). The
            // arm (not a local field) tells the two renders apart.
            let candidates = if slot_arm == crate::lsp::cursor_slot::DetectorArm::UseModule {
                cs.complete_module_candidates(prefix)
            } else {
                cs.complete_qualified_path(module_index, prefix)
            };
            let items: Vec<CompletionItem> =
                candidates.into_iter().map(candidate_to_completion_item).collect();
            // The candidate sources already narrowed by the qualifier
            // prefix, so the cap's own prefix pass has nothing to add.
            return finish_completion(items, &slot, false, false);
        }
        Slot::Identifier { .. } if sigil_trigger.is_some() => {
            analysis.complete_variables(point, sigil_trigger.expect("checked by guard"))
        }
        Slot::Identifier { .. } => {
            let mut items = Vec::new();
            // Keyval arg completions if inside a call at key position.
            // (Dispatch-target completions are handled above the match
            // regardless of context, so they apply whether the slot
            // resolves to Member, Identifier, or anything else.)
            if let Some(call_ctx) =
                cursor_context::find_call_context(tree, source.as_bytes(), point)
            {
                if call_ctx.at_key_position {
                    items.extend(analysis.complete_keyval_args(
                        &call_ctx.name,
                        call_ctx.is_method,
                        call_ctx.invocant.as_deref(),
                        point,
                        &call_ctx.used_keys,
                        Some(module_index),
                    ));
                }
            }
            // Identifier universe from the CandidateSet: in-scope names,
            // plus the import-sourced firehose when the slot has an
            // import affordance. The firehose is useful at top-level
            // positions, harmful when we just offered dispatch handlers
            // at arg-0 (they'd drown in it) — `suppress_firehose` is set
            // above when the cursor is at arg-0 of a known dispatcher
            // call, and withholds the affordance. The candidates carry the
            // importable-from FACT; the edit is composed HERE, fact + slot
            // affordance (`auto_import_span` needs the LSP-side stable
            // outline) — placement is the adapter's, not the model's.
            let mut import_sourced = cs.complete("", !suppress_firehose);
            if !suppress_firehose {
                let insert_at = auto_import_span(analysis, point, stable_packages);
                for c in &mut import_sourced {
                    match &c.import_fact {
                        Some(crate::model::file_analysis::ImportFact::AddToQw { name, qw_close }) => {
                            let at = crate::model::file_analysis::Span { start: *qw_close, end: *qw_close };
                            c.additional_edits.push((at, format!(" {}", name)));
                        }
                        Some(crate::model::file_analysis::ImportFact::NewUse { module, name }) => {
                            c.additional_edits
                                .push((insert_at, format!("use {} qw({});\n", module, name)));
                        }
                        None => {}
                    }
                }
            }
            items.extend(import_sourced);

            items
        }
        // Perl's slot detector never produces these — ArgPosition is
        // `detect_call_slot`'s question (sig-help's), TypePosition has no
        // Perl detector at all.
        Slot::TypePosition { .. } | Slot::ArgPosition { .. } | Slot::RailName { .. } => Vec::new(),
    });

    // Type-constrained ranking: when the cursor sits at a call arg whose
    // callee has a typed param, scope variables whose inferred type matches
    // rank first (`Slot::expected_type` — the seam's Perl consumer). Purely
    // a reorder + priority boost on the gathered candidates; nothing is
    // pruned (a mid-refactor mismatch stays visible).
    if let Some(expected) = crate::lsp::cursor_slot::detect_call_slot(tree, source.as_bytes(), point)
        .and_then(|s| s.slot.expected_type(analysis, point, Some(module_index)))
    {
        rank_candidates_by_expected_type(&mut candidates, &expected, analysis, point);
    }

    let mut items: Vec<CompletionItem> = candidates
        .drain(..)
        .map(candidate_to_completion_item)
        .collect();
    // Dispatch items stay at the top — their sort_text already leads
    // with a space so they group above the priority-numbered rest,
    // but the authoritative ordering is "dispatch first" so they're
    // prepended explicitly.
    if !dispatch_items.is_empty() {
        let mut with_dispatch = dispatch_items;
        with_dispatch.extend(items);
        items = with_dispatch;
    }

    // Ref-type deref snippets when completing after ->
    if let Slot::Member { ref receiver, .. } = slot {
        if let Some(ref ty) = receiver.receiver_type {
            if !ty.is_object() {
                items.extend(ref_type_snippet_completions(ty));
            }
        }
    }

    // Payload cap over the assembled universe (the identifier slot's
    // auto-import firehose is the workspace-scaled tier); the shared
    // finisher owns the prefix rule and the flag composition.
    finish_completion(items, &slot, sigil_trigger.is_some(), false)
}

/// The `use Module qw(|)` "still indexing" affordance — shown while the
/// named module's export surface (the entity) isn't cached yet. Not an
/// entity candidate, so it's built here rather than via the projection.
fn import_list_loading_placeholder(module_name: &str) -> CompletionItem {
    CompletionItem {
        label: format!("loading {}...", module_name),
        kind: Some(CompletionItemKind::TEXT),
        detail: Some("Module is being indexed".to_string()),
        insert_text: Some(String::new()),
        sort_text: Some("999".to_string()),
        ..Default::default()
    }
}

/// Returns snippet completions for ref-type dereference after `->`.
fn ref_type_snippet_completions(ty: &InferredType) -> Vec<CompletionItem> {
    match ty {
        InferredType::ArrayRef => vec![CompletionItem {
            label: "[index]".to_string(),
            kind: Some(CompletionItemKind::SNIPPET),
            detail: Some("array dereference".to_string()),
            insert_text: Some("[$0]".to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            sort_text: Some("000".to_string()),
            ..Default::default()
        }],
        InferredType::CodeRef { .. } => vec![CompletionItem {
            label: "(args)".to_string(),
            kind: Some(CompletionItemKind::SNIPPET),
            detail: Some("code dereference".to_string()),
            insert_text: Some("($0)".to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            sort_text: Some("000".to_string()),
            ..Default::default()
        }],
        InferredType::HashRef => vec![CompletionItem {
            label: "{key}".to_string(),
            kind: Some(CompletionItemKind::SNIPPET),
            detail: Some("hash dereference".to_string()),
            insert_text: Some("{$0}".to_string()),
            insert_text_format: Some(InsertTextFormat::SNIPPET),
            sort_text: Some("000".to_string()),
            ..Default::default()
        }],
        _ => Vec::new(),
    }
}

/// Language-agnostic hover for pack languages: the symbol's declaration
/// line in a language-appropriate code fence + its kind. Resolves a
/// cursor on a def directly, or on a call/ref to the local def it names.
/// The Perl `hover_info` renderer is Perl-specific (```perl fences,
/// method-resolution prose); pack languages get this instead.
/// Member completion for a pack language: the members of `class` (the
/// type of the `.`/`->` receiver, resolved by the sentinel) as items. The
/// tree work (sentinel reparse → receiver → type, incl. chains) happens in
/// the backend; this is the tree-free class → members → items half.
/// `op_fix = Some((operator_span, correct_operator))` attaches an
/// `additionalTextEdit` to every item that swaps the typed `.`/`->` for the
/// one the receiver's pointer depth requires (Mode A — accepting `width` on
/// `p.` yields `p->width`). `None` leaves the items untouched (operator
/// already correct, or DEEP receiver shown-only).
pub fn member_completion_for_class(
    analysis: &FileAnalysis,
    class: &str,
    module_index: &dyn crate::model::file_analysis::CrossFileLookup,
    op_fix: Option<(crate::model::file_analysis::Span, String)>,
    point: Point,
    scoped: bool,
) -> Option<Vec<CompletionItem>> {
    // The access-specifier gate needs to know whether the
    // CURSOR itself is lexically inside `class`'s own body — self-access
    // sees non-public members, an external receiver doesn't.
    let requesting_class = analysis
        .scope_at(point)
        .and_then(|sc| analysis.enclosing_class_for_scope(sc));
    let mut candidates = analysis.complete_members_for_class(
        class, Some(module_index), requesting_class.as_deref(),
    );
    // `Foo::` completes the class's constants and static members;
    // `$o->` the instance ones — an enumerator/constant is never reached
    // through an instance, a static member is (php allows it) but not what
    // the operator asks for.
    let sigil = analysis.pack.static_property_sigil.clone();
    candidates.retain(|c| {
        let constant = matches!(c.kind, FaSymKind::Enumerator);
        let static_field = c.is_static && matches!(c.kind, FaSymKind::Variable | FaSymKind::Field);
        if scoped {
            constant || c.is_static
        } else {
            // a static property is not reachable through an instance
            !constant && !static_field
        }
    });
    if scoped && !sigil.is_empty() {
        for c in candidates.iter_mut() {
            if c.is_static && matches!(c.kind, FaSymKind::Variable | FaSymKind::Field) {
                c.label = format!("{sigil}{}", c.label);
                c.insert_text = None;
            }
        }
    }
    // The class-name literal (`Foo::class`) is a member of every class the
    // pack declares it for — a convention on the pack, not a symbol.
    let literal = &analysis.pack.class_literal_member;
    if scoped && !literal.is_empty() && !candidates.iter().any(|c| &c.label == literal) {
        candidates.push(crate::model::file_analysis::CompletionCandidate {
            label: literal.clone(),
            kind: FaSymKind::Enumerator,
            is_static: false,
            detail: Some(format!("{class}::{literal}")),
            insert_text: None,
            sort_priority: crate::model::file_analysis::PRIORITY_LESS_RELEVANT,
            additional_edits: vec![],
            import_fact: None,
            display_override: None,
        });
    }
    if candidates.is_empty() {
        return None;
    }
    Some(
        candidates
            .into_iter()
            .map(|mut c| {
                if let Some((span, text)) = &op_fix {
                    c.additional_edits.push((*span, text.clone()));
                }
                candidate_to_completion_item(c)
            })
            .collect(),
    )
}

// ---- Import resolution helpers ----

/// Where a completion-accepted auto-import `use` edit lands: the standard
/// insertion position for the package under `point`, clamped to fall at or
/// above the cursor — an edit below the cursor would import after the call
/// being completed.
fn auto_import_span(
    analysis: &FileAnalysis,
    point: Point,
    stable_packages: Option<&[(String, usize)]>,
) -> crate::model::file_analysis::Span {
    let mut insert_pos = find_use_insertion_position(analysis, point, stable_packages);

    // If the computed position is after the cursor, fall back to inserting
    // after the nearest import or package statement ABOVE the cursor.
    if insert_pos.line as usize > point.row {
        // Find the last import above the cursor
        let last_import_above = analysis.imports.iter().rev()
            .find(|imp| imp.span.start.row < point.row);
        if let Some(imp) = last_import_above {
            insert_pos = Position { line: imp.span.end.row as u32 + 1, character: 0 };
        } else {
            // Find the last package statement above the cursor
            let last_pkg_above = analysis.symbols().iter().rev()
                .find(|s| matches!(s.kind, FaSymKind::Package | FaSymKind::Class) && s.selection_span.start.row < point.row);
            if let Some(pkg) = last_pkg_above {
                insert_pos = Position { line: pkg.selection_span.start.row as u32 + 1, character: 0 };
            }
            // else: keep original position (top of file)
        }
    }

    let p = tree_sitter::Point {
        row: insert_pos.line as usize,
        column: insert_pos.character as usize,
    };
    crate::model::file_analysis::Span { start: p, end: p }
}

pub(super) fn format_imported_signature(name: &str, sub_info: &SubInfo<'_>) -> String {
    let params_str = sub_info
        .params()
        .iter()
        .map(|p| p.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let mut sig = format!("sub {}({})", name, params_str);
    if let Some(rt) = sub_info.return_type(None) {
        sig.push_str(&format!(" → {}", format_inferred_type(&rt)));
    }
    sig
}

#[cfg(test)]
mod cap_tests {
    use super::*;

    fn item(label: &str, priority: u8) -> CompletionItem {
        CompletionItem {
            label: label.to_string(),
            sort_text: Some(format!("{:03}{}", priority, label)),
            filter_text: Some(label.to_string()),
            ..Default::default()
        }
    }

    /// The finisher's flag composition — the #143 class: an EMPTY list
    /// with a live prefix-gated source is an incomplete-empty response
    /// (`isIncomplete: true`), never a complete null the client caches.
    /// Both lanes exit through this one speller now; the divergence where
    /// one lane dropped the flag its sibling branch preserved is
    /// unrepresentable.
    #[test]
    fn empty_with_live_prefix_gated_source_is_incomplete() {
        let slot = crate::lsp::cursor_slot::Slot::Identifier { prefix: "xy".into() };
        let (items, incomplete) = finish_completion(Vec::new(), &slot, false, true);
        assert!(items.is_empty());
        assert!(incomplete, "a live prefix-gated source keeps the client re-asking");
        let (_, complete) = finish_completion(Vec::new(), &slot, false, false);
        assert!(!complete, "no cut, no live source: the empty list is genuinely complete");
    }

    /// The Perl lane's one finisher asymmetry: a sigil-triggered list's
    /// labels carry the sigil the typed prefix lacks, so prefix-narrowing
    /// must not run (it would empty the list at the cut).
    #[test]
    fn sigil_active_suppresses_prefix_narrowing() {
        let slot = crate::lsp::cursor_slot::Slot::Identifier { prefix: "se".into() };
        let over: Vec<CompletionItem> =
            (0..250).map(|i| item(&format!("$var{i:03}"), 0)).collect();
        let (kept, incomplete) = finish_completion(over.clone(), &slot, true, false);
        assert!(incomplete);
        assert_eq!(kept.len(), MAX_COMPLETION_ITEMS, "cut, but never prefix-emptied");
        let (narrowed, _) = finish_completion(over, &slot, false, false);
        assert!(narrowed.is_empty(), "without the sigil gate the prefix filter applies");
    }

    /// Under the cap the list is untouched and reported complete — ordinary
    /// files keep the exact behavior they had before the cap existed.
    #[test]
    fn under_cap_is_complete_and_untouched() {
        let mut items: Vec<CompletionItem> =
            (0..50).map(|i| item(&format!("name{i:03}"), 25)).collect();
        let before: Vec<String> = items.iter().map(|i| i.label.clone()).collect();
        assert!(!cap_completion_items(&mut items, ""));
        assert_eq!(before, items.iter().map(|i| i.label.clone()).collect::<Vec<_>>());
    }

    /// Over the cap: rank by sort_text THEN cut, so the low-priority tiers
    /// (local/in-scope/imported) survive and the workspace firehose is what
    /// gets truncated. The flag is the LSP isIncomplete signal.
    #[test]
    fn over_cap_keeps_the_ranked_head() {
        let mut items: Vec<CompletionItem> = Vec::new();
        // Firehose tier first in gathering order — ranking must still save
        // the locals appended after it.
        for i in 0..MAX_COMPLETION_ITEMS + 50 {
            items.push(item(&format!("export{i:05}"), 25));
        }
        for i in 0..10 {
            items.push(item(&format!("local{i}"), 0));
        }
        assert!(cap_completion_items(&mut items, ""));
        assert_eq!(items.len(), MAX_COMPLETION_ITEMS);
        for i in 0..10 {
            let l = format!("local{i}");
            assert!(items.iter().any(|it| it.label == l), "local tier survived the cut: {l}");
        }
    }

    /// A typed prefix narrows server-side before the rank+cut — each
    /// keystroke's re-query (isIncomplete) converges on the complete answer.
    #[test]
    fn typed_prefix_narrows_before_the_cut() {
        let mut items: Vec<CompletionItem> = Vec::new();
        for i in 0..MAX_COMPLETION_ITEMS + 50 {
            items.push(item(&format!("noise{i:05}"), 25));
        }
        items.push(item("get_config", 25));
        assert!(cap_completion_items(&mut items, "get_"));
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "get_config");
    }
}
