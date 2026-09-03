//! The `LanguageServer` trait impl — request routing for every LSP verb
//! (one trait impl, kept whole) — plus its perltidy formatting subprocess.

use super::*;

#[tower_lsp::async_trait]
impl LanguageServer for Backend {
    async fn initialize(&self, params: InitializeParams) -> Result<InitializeResult> {
        // Notify resolver thread of workspace root for per-project cache.
        let root = params
            .root_uri
            .as_ref()
            .map(|u| u.as_str())
            .or_else(|| {
                params
                    .workspace_folders
                    .as_ref()
                    .and_then(|f| f.first())
                    .map(|f| f.uri.as_str())
            });
        // Long-lived process: the overlay + rehydration LRU amortize here
        // (one-shot CLI modes leave both off — bisected at 2x warm-harness
        // wall). BEFORE set_workspace_root: the resolver wakes on the root
        // and reads the flag at warm time.
        self.module_index.mark_long_lived();
        self.module_index.set_workspace_root(root);
        // Same root drives repo-local `.perl-lsp/` plugin discovery, so the
        // plugin set and the per-project cache key can't disagree.
        crate::build::plugin::rhai_host::set_workspace_root(root);
        // Re-validate the plugin registry now that the root is known. The
        // heavy compile (~600 ms: rhai plugins + pattern/flow queries)
        // already started at PROCESS start (`main`), before the handshake —
        // the registry cell is keyed by the resolved plugin-source paths, so
        // when this root doesn't change the on-disk set (no repo-local
        // `.perl-lsp/`) this call is a cache hit and the first didOpen build
        // pays nothing. When the root DOES add repo-local plugins, this is
        // the rebuild with the right set — off the handler, and a racing
        // build blocks on the cell rather than duplicating the work. AFTER
        // set_workspace_root, so the repo-local plugin set is the one warmed.
        tokio::task::spawn_blocking(crate::build::plugin::default_plugin_registry);

        // LSP spec: `initialize` carries the client `processId`; "if the parent
        // process is not alive then the server should exit." Poll it on an
        // independent timer — the ROBUST backstop the stdin-EOF path can't be
        // (that's coupled to the read loop, which isn't running precisely when
        // the leak happens: a server wedged mid-analysis isn't reading stdin,
        // and a hard SIGKILL of the editor need not deliver a clean EOF).
        spawn_parent_liveness_monitor(params.process_id);

        // Server-initiated progress is capability-gated (M7): only send
        // `window/workDoneProgress/create` to clients that opted in.
        let wdp = params
            .capabilities
            .window
            .as_ref()
            .and_then(|w| w.work_done_progress)
            .unwrap_or(false);
        self.work_done_progress
            .store(wdp, std::sync::atomic::Ordering::Relaxed);

        // lsp-types 0.94 can't spell a static `typeHierarchyProvider`, so the
        // verb is advertised by dynamic registration — only to clients that
        // opted in (registering at a client that didn't is a spec violation).
        let th_dyn = params
            .capabilities
            .text_document
            .as_ref()
            .and_then(|t| t.type_hierarchy.as_ref())
            .and_then(|t| t.dynamic_registration)
            .unwrap_or(false);
        self.type_hierarchy_dynamic
            .store(th_dyn, std::sync::atomic::Ordering::Relaxed);

        // Every column this server speaks is a tree-sitter BYTE offset —
        // Span/Point math, the CLI coordinates, every emitted Range. LSP's
        // default encoding is UTF-16 code units, so on a line with a
        // non-ASCII character before the cursor the two lanes disagree and
        // every position verb misaligns there. Negotiate honesty where the
        // client permits it (LSP 3.17): offer utf-8 and the client converts
        // for us (nvim 0.10+ does). A client without utf-8 stays on the
        // default — mismatched as before, but as a known gap rather than an
        // unspoken byte==code-unit identity assumption.
        let utf8_positions = params
            .capabilities
            .general
            .as_ref()
            .and_then(|g| g.position_encodings.as_ref())
            .is_some_and(|encs| encs.iter().any(|e| *e == PositionEncodingKind::UTF8));

        // Opt-in diagnostics from `initializationOptions.diagnostics`.
        // The `diagnostics` sub-object deserializes straight into
        // `DiagnosticOptions` (the struct is the schema — camelCase keys,
        // absent ones default to false, e.g. `unresolvedDispatch`). A malformed
        // value leaves the defaults in place rather than failing initialize.
        if let Some(diag) = params
            .initialization_options
            .as_ref()
            .and_then(|o| o.get("diagnostics"))
        {
            if let Ok(parsed) =
                serde_json::from_value::<symbols::DiagnosticOptions>(diag.clone())
            {
                *self.diag_options.lock().unwrap() = parsed;
            }
        }
        // The `rename` sub-object deserializes into `RenameOptions` the same way
        // (`{ "rename": { "overrideScope": "dispatch" } }`); absent / malformed
        // leaves the default whole-hierarchy scope.
        if let Some(rename) = params
            .initialization_options
            .as_ref()
            .and_then(|o| o.get("rename"))
        {
            if let Ok(parsed) =
                serde_json::from_value::<crate::index::resolve::RenameOptions>(rename.clone())
            {
                *self.rename_options.lock().unwrap() = parsed;
            }
        }
        // `coldWaitMs` caps the cold-open pull-verb bounded wait; 0 opts out.
        // Absent / non-integer leaves the default.
        if let Some(ms) = params
            .initialization_options
            .as_ref()
            .and_then(|o| o.get("coldWaitMs"))
            .and_then(|v| v.as_u64())
        {
            self.cold_wait_ms
                .store(ms, std::sync::atomic::Ordering::Relaxed);
        }
        // `maxCacheMb` sizes the Slice-2 bag-rehydration LRU (0 = rehydrate and
        // drop). Absent / non-integer leaves the default.
        if let Some(mb) = params
            .initialization_options
            .as_ref()
            .and_then(|o| o.get("maxCacheMb"))
            .and_then(|v| v.as_u64())
        {
            self.max_cache_mb
                .store(mb, std::sync::atomic::Ordering::Relaxed);
        }

        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "perl-lsp".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                position_encoding: utf8_positions.then_some(PositionEncodingKind::UTF8),
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::FULL),
                        save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                            include_text: Some(true),
                        })),
                        ..Default::default()
                    },
                )),
                document_symbol_provider: Some(OneOf::Left(true)),
                definition_provider: Some(OneOf::Left(true)),
                type_definition_provider: Some(TypeDefinitionProviderCapability::Simple(true)),
                implementation_provider: Some(ImplementationProviderCapability::Simple(true)),
                // NOTE: no `type_hierarchy_provider` field exists in lsp-types
                // 0.94 — the verb is served and advertised via dynamic
                // registration in `initialized` (see `type_hierarchy_dynamic`).
                call_hierarchy_provider: Some(CallHierarchyServerCapability::Simple(true)),
                references_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                rename_provider: Some(OneOf::Right(RenameOptions {
                    prepare_provider: Some(true),
                    work_done_progress_options: Default::default(),
                })),
                document_range_formatting_provider: Some(OneOf::Left(true)),
                // Off: clients with linked edits on by default (Zed) replay
                // keystrokes into every returned range, so a mid-typed `$abel`
                // whose `$a` prefix matches a declared variable live-renames
                // the declaration (#116). Identifier occurrence sets don't fit
                // this verb; re-enable only for an atomic slot like heredoc
                // terminators. The co-edit projection stays CLI-queryable via
                // --linked-editing.
                linked_editing_range_provider: None,
                completion_provider: Some(CompletionOptions {
                    // Union of every served language's trigger chars — Perl
                    // sigils/`->`/`{`, plus a pack language's `.`/`::` etc.
                    // A perl-only build is byte-identical to the old list.
                    trigger_characters: Some(
                        crate::build::language_driver::LanguageRegistry::with_enabled().trigger_chars(),
                    ),
                    ..Default::default()
                }),
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: Some(vec![")".to_string()]),
                    work_done_progress_options: Default::default(),
                }),
                document_highlight_provider: Some(OneOf::Left(true)),
                // Links are fully resolved in the initial pass (registered-
                // only lookups), so no lazy documentLink/resolve round-trip.
                document_link_provider: Some(DocumentLinkOptions {
                    resolve_provider: Some(false),
                    work_done_progress_options: Default::default(),
                }),
                selection_range_provider: Some(SelectionRangeProviderCapability::Simple(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
                inlay_hint_provider: Some(OneOf::Left(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            legend: SemanticTokensLegend {
                                token_types: symbols::semantic_token_types(),
                                token_modifiers: symbols::semantic_token_modifiers(),
                            },
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                            range: None,
                            ..Default::default()
                        },
                    ),
                ),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                workspace: Some(WorkspaceServerCapabilities {
                    workspace_folders: None,
                    file_operations: None,
                }),
                ..ServerCapabilities::default()
            },
            ..Default::default()
        })
    }

    async fn initialized(&self, _: InitializedParams) {
        // Pre-warm each pack language's skeleton-query compilation OFF the
        // message loop, FIRST — before any `.await` that depends on the client
        // answering (register_capability), so the warm-up starts even if the
        // client is slow to respond. `Query::new` is a ~180ms one-time cost
        // baked into the first pack file build; `did_open` runs that build
        // synchronously before its own first `.await`, so without this warm-up
        // it stalls the message loop and the goto-def request queued right
        // behind the open waits the whole ~180ms (measured: first cpp goto-def
        // 196ms, second 25ms, third 1ms). A tiny analyze forces the compile
        // into the driver's `OnceLock`; Perl's query warms on its normal first
        // build. Correctness-inert: it only populates the cache earlier.
        tokio::task::spawn_blocking(|| {
            let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
            // Only pack-served drivers compile a query engine worth warming
            // (the reference driver warms on its normal first build).
            for driver in reg.pack_drivers() {
                // A non-trivial snippet so `parser.parse` yields a tree and
                // the analyze reaches `query_extract::extract` (which is
                // where `Query::new` fires); empty source can parse to
                // `None` and skip it, leaving the cache cold.
                let _ = driver.analyze_with_path("int _perl_lsp_warm;\n", None);
            }
        });

        self.client
            .log_message(MessageType::INFO, "perl-lsp initialized")
            .await;

        // Register file watchers for workspace indexing — every served
        // language's extensions (Perl + pack languages), so out-of-editor
        // changes to a header/pack file reach the invalidation path too.
        let watchers: Vec<FileSystemWatcher> = {
            let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
            reg.languages()
                .into_iter()
                .filter_map(|id| reg.for_id(id))
                .flat_map(|d| d.extensions().iter())
                .map(|ext| FileSystemWatcher {
                    glob_pattern: GlobPattern::String(format!("**/*.{ext}")),
                    kind: None,
                })
                .collect()
        };
        let mut registrations = vec![Registration {
            id: "perl-file-watcher".to_string(),
            method: "workspace/didChangeWatchedFiles".to_string(),
            register_options: Some(serde_json::to_value(DidChangeWatchedFilesRegistrationOptions {
                watchers,
            }).unwrap()),
        }];
        // typeHierarchy: dynamic-only advertisement (lsp-types 0.94 has no
        // static field). Registering `prepare` implies the supertypes/
        // subtypes requests per spec.
        if self
            .type_hierarchy_dynamic
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            registrations.push(Registration {
                id: "perl-type-hierarchy".to_string(),
                method: "textDocument/prepareTypeHierarchy".to_string(),
                register_options: None,
            });
        }
        let _ = self.client.register_capability(registrations).await;

        // Workspace indexing is LAZY + per-language — the first `did_open` of a
        // family triggers `ensure_workspace_indexed`, so a C++ session in a
        // mixed tree never eagerly scans the 4000+ `.pm` files it can't use
        // (that eager perl scan was the multi-minute first-open stall).
    }

    async fn shutdown(&self) -> Result<()> {
        // Report-only cache instrumentation flush; inert unless
        // PERL_LSP_GHOST_STATS is set.
        crate::util::ghost_stats::emit_all("shutdown");
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;
        // Build the document OFF the message loop. `FileStore::open` runs the
        // whole pack pipeline (macro transform + extraction) synchronously — for
        // a 16k-line macro-heavy C file that is ~1.3 s even cached-only, and
        // running it here would head-of-line block every request the client
        // fires on open. cached-only skips the cross-file GATHER (a further
        // ~1.5 s, warmed later by the single-flight gather heal); the per-file
        // build is intrinsic and must simply not block the loop.
        //
        // A per-URI `ReadyGate` marks the build in flight so read verbs
        // bounded-wait for it (`await_open_ready`) instead of racing the empty
        // store. The `set_gather_cached_only` thread-local is set INSIDE the
        // blocking closure so it applies exactly to this build's thread.
        let gate = Arc::new(ReadyGate::default());
        self.opening.insert(uri.clone(), Arc::clone(&gate));
        let files = Arc::clone(&self.files);
        let uri_build = uri.clone();
        let build_started = std::time::Instant::now();
        let opened = tokio::task::spawn_blocking(move || {
            crate::build::cpp_reparse::set_gather_cached_only(true);
            let opened = files.open(uri_build, text);
            crate::build::cpp_reparse::set_gather_cached_only(false);
            opened
        })
        .await
        .unwrap_or(false);
        // Doc is in the store (or the build failed): drop the in-flight marker
        // and wake any verb waiting on it.
        self.opening.remove(&uri);
        gate.open();
        // If the build outran the bounded wait, the verbs the client fired on
        // open (semanticTokens, inlayHint) returned degraded. Their content is
        // now in the store — nudge the client to re-request (LSP server-initiated
        // refresh) so the visible highlighting/hints heal without a keystroke.
        // A fast build (< cap) answered those on the first pull; no nudge needed.
        let cap = self.cold_wait_ms.load(std::sync::atomic::Ordering::Relaxed);
        if opened && cap > 0 && build_started.elapsed().as_millis() as u64 > cap {
            let client = self.client.clone();
            tokio::spawn(async move {
                let _ = client.semantic_tokens_refresh().await;
                let _ = client.inlay_hint_refresh().await;
            });
        }
        let mut needs_gather_refresh = false;
        if opened {
            if let Some(doc) = self.files.get_open(&uri) {
                // Lazily index this file's language family (once) so a C++
                // open doesn't wait on the perl tree.
                self.ensure_workspace_indexed(&doc.language);
                // A gather-dependent file's first analyze was cached-only;
                // warm the gather and re-analyze in the background so full
                // cross-file macros land. The language declares whether a
                // gather exists at all.
                needs_gather_refresh =
                    crate::build::language_driver::LanguageRegistry::caps(doc.language)
                        .context_gather;
                // Enqueue imports for background resolution (non-blocking).
                for imp in &doc.analysis.imports {
                    self.module_index.request_resolve(&imp.module_name);
                }
                // Enqueue parent classes for resolution (inheritance chain).
                for (_pkg, parents) in doc.analysis.package_parent_edges() {
                    for parent in parents {
                        self.module_index.request_resolve(parent);
                    }
                }
            }
        }
        // The open-doc path now owns this file's surface record (buffer
        // shadows disk for every cross-file consumer — `SurfaceWrite`).
        // The record itself + the publish + the dirty-consumer republish all
        // run OFF the message pipeline (`schedule_diag_refresh`): the publish
        // re-enriches against the cross-file index — minutes of synchronous
        // CPU on a large workspace — and tower-lsp polls every handler
        // future inside one task, so awaiting it here head-of-line blocked
        // every other verb (the post-cold-index availability hole).
        if opened {
            if let (Ok(path), Some(doc)) = (uri.to_file_path(), self.files.get_open(&uri)) {
                // Hub-integrated languages record @INC residency here; pack
                // freshness is the invalidator's disk-side gate.
                if crate::build::language_driver::LanguageRegistry::caps(doc.language)
                    .hub_enrichment
                {
                    self.module_index.mark_doc_open(&path);
                    // Record the buffer's surface NOW rather than waiting for
                    // the debounced refresh. Until this lands the file
                    // declares no dependencies, so a watcher-driven wave over
                    // one of its providers finds no consumer edge and marks
                    // nobody — a real window, 150 ms wide, that opens on every
                    // didOpen. `Document::baseline_surface` is the value the
                    // architecture already designates: build-time and
                    // pre-enrichment, which keeps the record
                    // enrichment-invariant exactly as the debounced write is.
                    //
                    // Through `DiagCtx::record_surface`, not a second
                    // spelling: it owns the hub-language gate, the canonical
                    // key and the record→verdict→dirty seam.
                    //
                    // The dirty set is acted on HERE, not left to the
                    // scheduled refresh: this record consumes the transition,
                    // so by the time the refresh runs the surface is already
                    // recorded and its verdict is `Unchanged` with an empty
                    // set. Dropping it strands the open-after-external-change
                    // case it looks like it defers.
                    if let Some(sd) = self.diag_ctx().record_surface(&uri) {
                        self.spawn_republish(sd.dirty);
                    }
                }
            }
        }
        self.schedule_diag_refresh(uri.clone());
        if needs_gather_refresh {
            // The open build was cached-only: mark the degraded window BEFORE
            // spawning the heal, so a cross-file verb racing this open waits
            // for the full-gather analysis instead of the partial closure.
            self.degraded_open
                .entry(uri.clone())
                .or_insert_with(|| Arc::new(ReadyGate::default()));
            // Announce the degraded window (Part 1) and route the initial
            // gather through the single-flight registry (Part 2) — so the
            // first change's heal coalesces into THIS gather instead of
            // spawning a redundant second one.
            let heal_ctx = self.pack_heal_ctx();
            let language = self.files.get_open(&uri).map(|d| d.language);
            if let Some(language) = language {
                heal_ctx.begin_progress(&uri, language).await;
            }
            heal_ctx.request_gather(uri);
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let Some(change) = params.content_changes.into_iter().next() else {
            return;
        };
        let language = match self.files.get_open(&uri) {
            Some(doc) => doc.language,
            None => return,
        };
        // A cheap-build language rebuilds synchronously; the rest
        // (macro-heavy C: ~0.7s/rebuild) update the tree/text immediately so
        // position features stay live, and DEBOUNCE the analysis so a burst
        // of keystrokes pays one rebuild after typing settles, not one each.
        if crate::build::language_driver::LanguageRegistry::caps(language).synchronous_rebuild {
            if let Some(mut doc) = self.files.get_open_mut(&uri) {
                doc.update(change.text);
                for imp in &doc.analysis.imports {
                    self.module_index.request_resolve(&imp.module_name);
                }
                for (_pkg, parents) in doc.analysis.package_parent_edges() {
                    for parent in parents {
                        self.module_index.request_resolve(parent);
                    }
                }
            }
            // Surface record + publish + surface-gated consumer refresh, all
            // debounced OFF the pipeline: the publish re-enriches (heavy),
            // and per-keystroke inline enrichment was the "diagnostics after
            // edit: never" finding at scale.
            self.schedule_diag_refresh(uri);
            return;
        }
        if let Some(mut doc) = self.files.get_open_mut(&uri) {
            doc.update_text_only(change.text);
        }
        self.spawn_debounced_rebuild(uri);
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;
        // A save of an invalidator-owned language forwards to the
        // invalidation seam below; hub languages take the direct path.
        let pack_path = self
            .files
            .get_open(&uri)
            .filter(|doc| {
                crate::build::language_driver::LanguageRegistry::caps(doc.language)
                    .pack_invalidation
            })
            .and_then(|_| uri.to_file_path().ok());
        if let Some(text) = params.text {
            if let Some(mut doc) = self.files.get_open_mut(&uri) {
                doc.update(text);
            }
            self.schedule_diag_refresh(uri.clone());
        }
        // The saved bytes are on disk: re-register this file's indexed copy,
        // evict the caches it participates in, and refresh its open consumers
        // (H1 — a saved dependency must become visible to its consumers
        // without a restart). Runs regardless of includeText.
        //
        // Both language families need this and only the pack half had it.
        // `didChangeWatchedFiles` looks like it should cover the hub — it
        // does not fire when the editor saves a buffer it has open, which is
        // exactly how a dependency gets edited, so a saved `.pm` left every
        // consumer in the session answering from the version before the save.
        match pack_path {
            Some(path) => self.schedule_pack_invalidate(path, false),
            None => {
                // Spawned, not awaited — the same shape `schedule_pack_invalidate`
                // uses, and for the same reason: tower-lsp runs notifications on
                // one task, so awaiting a re-parse + re-register here would hold
                // every following didChange behind it. A save is the most
                // frequent trigger this path has.
                if let Ok(path) = uri.to_file_path() {
                    self.spawn_reindex_saved_perl(path);
                }
            }
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.files.close(&uri);
        // Wake any degraded-window waiter — the doc is gone, there is
        // nothing to wait for.
        if let Some((_, g)) = self.degraded_open.remove(&uri) {
            g.open();
        }
        // Retire any in-flight gather single-flight entry (no leak on close;
        // the running loop's next `finish` sees Vacant and stops) and end the
        // degraded-window progress if one is still live.
        self.gather_reg.forget(&uri);
        // The closed doc's rebuild/diagnostics gates have no further fires to
        // collapse; an in-flight one holds its own Arc and finishes against
        // that (its publish then reads a closed doc → empty diagnostics).
        self.change_debounce.remove(&uri);
        self.diag_debounce.remove(&uri);
        if let Some((_, token)) = self.degraded_progress.remove(&uri) {
            progress_end(&self.client, token).await;
        }
        // Release the surface record to background writers and reconcile:
        // consumers flip back to the indexed DISK copy — if the buffer died
        // with unsaved contract changes, whoever enriched against it is
        // stale and gets republished here.
        if let Ok(path) = uri.to_file_path() {
            if let Some(sd) = self.module_index.mark_doc_closed(&path) {
                // Off-pipeline: each republish re-enriches.
                self.spawn_republish(sd.dirty);
            }
        }
    }

    async fn document_symbol(
        &self,
        params: DocumentSymbolParams,
    ) -> Result<Option<DocumentSymbolResponse>> {
        let uri = &params.text_document.uri;
        self.await_open_ready(uri, WaitPolicy::Complete).await;
        let doc = match self.files.get_open(uri) {
            Some(doc) => doc,
            None => return self.not_ready_or_null(uri),
        };
        let syms = symbols::extract_symbols(&doc.analysis);
        Ok(Some(DocumentSymbolResponse::Nested(syms)))
    }

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> Result<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        // Cold-open bounded waits: first the file's own initial build (may still
        // be in flight — `did_open` runs it off the loop), then its family index
        // — so the query resolves warm instead of returning the one degraded
        // answer the user never re-triggers. Guards dropped before each await;
        // analysis snapshotted AFTER so any heal is picked up.
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        if let Some(language) = self.files.get_open(uri).map(|d| d.language) {
            self.await_index_ready(language, WaitPolicy::Interactive).await;
        }
        // Snapshot + drop the store guard before `resolve()` (reentrant
        // `for_each_open`); see `Document::analysis`.
        let (analysis, text, language) = match self.files.get_open(uri) {
            Some(doc) => (Arc::clone(&doc.analysis), doc.text.clone(), doc.language),
            None => return self.not_ready_or_null(uri),
        };
        let uri = uri.clone();
        self.run_query(move |cx| {
            let self_path = uri.to_file_path().ok();
            // `#include "x.h"` path → the resolved header (`#include` = `use`).
            // A path token, not a name — slot-shaped, so it stays ahead of the
            // set (the ADR's honest boundary). The pack declares whether it has
            // include tokens; asked, never named.
            if crate::build::language_driver::LanguageRegistry::has_include_tokens(&language) {
                if let Some(loc) = symbols::pack_include_definition(
                    &analysis, symbols::position_to_point(pos), self_path.as_deref())
                {
                    return Some(GotoDefinitionResponse::Scalar(loc));
                }
            }
            // cpp/pack functions live in the per-language sub-index; route
            // there so cross-file function goto-def resolves (Perl uses the
            // hub).
            let routed = cx.routed(language);
            let base_idx = routed.as_lookup();
            // Forward projection of the set. The source text unlocks the macro
            // variant lane (ranked, never pruned, see-through delegate) for pack
            // routing; labels ride the candidates and the editor adapter drops
            // them (ordering conveys rank).
            let cs = cx
                .set(
                    base_idx,
                    &analysis,
                    &uri,
                    symbols::position_to_point(pos),
                    crate::index::resolve::OverrideScope::default(),
                )
                .with_source(&text);
            let locs: Vec<Location> = cs
                .definitions()
                .into_iter()
                .filter_map(|l| {
                    let uri = l.to_url()?;
                    Some(Location { uri, range: symbols::span_to_range(l.span) })
                })
                .collect();
            match locs.len() {
                0 => {}
                1 => return Some(GotoDefinitionResponse::Scalar(locs.into_iter().next().unwrap())),
                _ => return Some(GotoDefinitionResponse::Array(locs)),
            }
            // Member access (`obj->field`) flows through the set above: cpp
            // mints a `MethodCall` ref core resolves like any other.
            if crate::build::language_driver::LanguageRegistry::caps(language).cross_file_words {
                // A macro / enum-constant / global usage (`OP_NULL`, `BASEOP`)
                // — the raw word names a local-or-cross-file symbol. The lane
                // sits outside the CandidateSet and still needs this file's
                // closure scope; the set scopes itself at construction.
                // Inside the `cross_file_words` caps gate — a pack-only lane.
                let scoped = crate::model::file_analysis::ScopedLookup::new(
                    base_idx,
                    &analysis.pack.include_closure,
                    self_path.as_deref(),
                    crate::model::file_analysis::VisibilityAxis::IncludeClosure,
                );
                if let Some((target, span, _)) =
                    cx.pack_xfile_word_at(&text, &analysis, pos, &scoped)
                {
                    return Some(GotoDefinitionResponse::Scalar(Location {
                        uri: target.unwrap_or_else(|| uri.clone()),
                        range: symbols::span_to_range(span),
                    }));
                }
            }
            None
        })
        .await
    }

    async fn goto_type_definition(
        &self,
        params: request::GotoTypeDefinitionParams,
    ) -> Result<Option<request::GotoTypeDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        // Cold-open bounded waits, mirroring goto-def: the value's type may
        // resolve through imports, so the family index matters here too.
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        if let Some(language) = self.files.get_open(uri).map(|d| d.language) {
            self.await_index_ready(language, WaitPolicy::Interactive).await;
        }
        // Snapshot + drop the store guard before `resolve()` (reentrant
        // `for_each_open`); see `Document::analysis`.
        let (analysis, language) = match self.files.get_open(uri) {
            Some(doc) => (Arc::clone(&doc.analysis), doc.language),
            None => return self.not_ready_or_null(uri),
        };
        let uri = uri.clone();
        self.run_query(move |cx| {
            // The type axis of the same set goto-def projects: value type →
            // dispatch class → class definition(s). Honest miss when the
            // type doesn't infer — no name-only fallback.
            let routed = cx.routed(language);
            let cs = cx.set(
                routed.as_lookup(),
                &analysis,
                &uri,
                symbols::position_to_point(pos),
                crate::index::resolve::OverrideScope::default(),
            );
            refs_to_locations(cs.type_definitions()).map(GotoDefinitionResponse::Array)
        })
        .await
    }

    async fn prepare_type_hierarchy(
        &self,
        params: TypeHierarchyPrepareParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        self.await_open_ready(uri, WaitPolicy::Complete).await;
        if let Some(language) = self.files.get_open(uri).map(|d| d.language) {
            self.await_index_ready(language, WaitPolicy::Complete).await;
        }
        let (analysis, language) = match self.files.get_open(uri) {
            Some(doc) => (Arc::clone(&doc.analysis), doc.language),
            None => return self.not_ready_or_null(uri),
        };
        let uri = uri.clone();
        self.run_query(move |cx| {
            let routed = cx.routed(language);
            let cs = cx.set(
                routed.as_lookup(),
                &analysis,
                &uri,
                symbols::position_to_point(pos),
                crate::index::resolve::OverrideScope::default(),
            );
            cs.hierarchy_type_item()
                .as_ref()
                .and_then(symbols::to_type_hierarchy_item)
                .map(|i| vec![i])
        })
        .await
    }

    async fn supertypes(
        &self,
        params: TypeHierarchySupertypesParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let item = params.item;
        self.run_query(move |cx| {
            // Re-anchor at the item's own declaration file — the analysis
            // that carries the class's local parent edges.
            let (analysis, key, language) = cx.item_anchor(&item.uri)?;
            let routed = cx.routed(&language);
            let cs = cx.set_at(
                routed.as_lookup(),
                &analysis,
                key,
                symbols::position_to_point(item.selection_range.start),
                crate::index::resolve::OverrideScope::default(),
            );
            let items: Vec<TypeHierarchyItem> = cs
                .supertypes()
                .iter()
                .filter_map(symbols::to_type_hierarchy_item)
                .collect();
            Some(items)
        })
        .await
    }

    async fn subtypes(
        &self,
        params: TypeHierarchySubtypesParams,
    ) -> Result<Option<Vec<TypeHierarchyItem>>> {
        let item = params.item;
        self.run_query(move |cx| {
            let (analysis, key, language) = cx.item_anchor(&item.uri)?;
            let routed = cx.routed(&language);
            let cs = cx.set_at(
                routed.as_lookup(),
                &analysis,
                key,
                symbols::position_to_point(item.selection_range.start),
                crate::index::resolve::OverrideScope::default(),
            );
            let items: Vec<TypeHierarchyItem> = cs
                .subtypes()
                .iter()
                .filter_map(symbols::to_type_hierarchy_item)
                .collect();
            Some(items)
        })
        .await
    }

    async fn prepare_call_hierarchy(
        &self,
        params: CallHierarchyPrepareParams,
    ) -> Result<Option<Vec<CallHierarchyItem>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        self.await_open_ready(uri, WaitPolicy::Complete).await;
        if let Some(language) = self.files.get_open(uri).map(|d| d.language) {
            self.await_index_ready(language, WaitPolicy::Complete).await;
        }
        let (analysis, language) = match self.files.get_open(uri) {
            Some(doc) => (Arc::clone(&doc.analysis), doc.language),
            None => return self.not_ready_or_null(uri),
        };
        let uri = uri.clone();
        self.run_query(move |cx| {
            let routed = cx.routed(language);
            let cs = cx.set(
                routed.as_lookup(),
                &analysis,
                &uri,
                symbols::position_to_point(pos),
                crate::index::resolve::OverrideScope::default(),
            );
            cs.hierarchy_call_item()
                .as_ref()
                .and_then(symbols::to_call_hierarchy_item)
                .map(|i| vec![i])
        })
        .await
    }

    async fn incoming_calls(
        &self,
        params: CallHierarchyIncomingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyIncomingCall>>> {
        let item = params.item;
        let scope = self.override_scope();
        self.run_query(move |cx| {
            let (analysis, key, language) = cx.item_anchor(&item.uri)?;
            let routed = cx.routed(&language);
            // The set minted at the DECLARATION — incoming is its
            // `references()` image (the same projection heatmap fan-in
            // counts), grouped by each site's enclosing callable.
            let cs = cx.set_at(
                routed.as_lookup(),
                &analysis,
                key,
                symbols::position_to_point(item.selection_range.start),
                scope,
            );
            let calls: Vec<CallHierarchyIncomingCall> = cs
                .incoming_calls()
                .iter()
                .filter_map(|e| {
                    Some(CallHierarchyIncomingCall {
                        from: symbols::to_call_hierarchy_item(&e.item)?,
                        from_ranges: e.sites.iter().map(|s| symbols::span_to_range(*s)).collect(),
                    })
                })
                .collect();
            Some(calls)
        })
        .await
    }

    async fn outgoing_calls(
        &self,
        params: CallHierarchyOutgoingCallsParams,
    ) -> Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        let item = params.item;
        self.run_query(move |cx| {
            let (analysis, key, language) = cx.item_anchor(&item.uri)?;
            let routed = cx.routed(&language);
            let cs = cx.set_at(
                routed.as_lookup(),
                &analysis,
                key,
                symbols::position_to_point(item.selection_range.start),
                crate::index::resolve::OverrideScope::default(),
            );
            let calls: Vec<CallHierarchyOutgoingCall> = cs
                .outgoing_calls()
                .iter()
                .filter_map(|e| {
                    Some(CallHierarchyOutgoingCall {
                        to: symbols::to_call_hierarchy_item(&e.item)?,
                        from_ranges: e.sites.iter().map(|s| symbols::span_to_range(*s)).collect(),
                    })
                })
                .collect();
            Some(calls)
        })
        .await
    }

    async fn document_link(
        &self,
        params: DocumentLinkParams,
    ) -> Result<Option<Vec<DocumentLink>>> {
        let uri = &params.text_document.uri;
        // Client-POLLED verb (editors ask on every open/change): no waits,
        // no resolution kicks — one text scan over the stored buffer plus
        // registered-only map lookups. An unknown module yields no link and
        // appears once registration lands.
        let (text, language) = match self.files.get_open(uri) {
            Some(doc) => (doc.text.clone(), doc.language),
            None => return self.not_ready_or_null(uri),
        };
        let self_dir = uri
            .to_file_path()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let root = self
            .module_index
            .workspace_root()
            .and_then(|r| Url::parse(&r).ok())
            .and_then(|u| u.to_file_path().ok());
        self.run_query(move |cx| {
            let routed = cx.routed(language);
            let links: Vec<DocumentLink> = symbols::document_links(
                &text,
                self_dir.as_deref(),
                root.as_deref(),
                Some(routed.as_lookup()),
            )
            .into_iter()
            .filter_map(|l| {
                Some(DocumentLink {
                    range: symbols::span_to_range(l.span),
                    target: Some(l.target.to_url()?),
                    tooltip: None,
                    data: None,
                })
            })
            .collect();
            (!links.is_empty()).then_some(links)
        })
        .await
    }

    async fn goto_implementation(
        &self,
        params: request::GotoImplementationParams,
    ) -> Result<Option<request::GotoImplementationResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        // Cold-open bounded waits (see `await_open_ready` / `await_index_ready`):
        // the file's own initial build, then an in-flight family index.
        self.await_open_ready(uri, WaitPolicy::Complete).await;
        if let Some(language) = self.files.get_open(uri).map(|d| d.language) {
            self.await_index_ready(language, WaitPolicy::Complete).await;
        }
        self.await_open_full(uri, WaitPolicy::Complete).await;
        // Snapshot the open doc (cheap `Arc` clone) and DROP the store guard
        // before `resolve()` — it re-locks the open shards via `for_each_open`,
        // and holding the guard across that reentrant read deadlocks against a
        // concurrently queued writer. See `Document::analysis`.
        let (analysis, language) = match self.files.get_open(uri) {
            Some(doc) => (Arc::clone(&doc.analysis), doc.language),
            None => return self.not_ready_or_null(uri),
        };

        // The family/descendants/domain projection of the same set references
        // and rename resolve from — pack routing is a construction fact, so
        // the resolved target can't diverge across the three verbs.
        let uri = uri.clone();
        self.run_query(move |cx| {
            let routed = cx.routed(language);
            let cs = cx.set(
                routed.as_lookup(),
                &analysis,
                &uri,
                symbols::position_to_point(pos),
                crate::index::resolve::OverrideScope::default(),
            );
            refs_to_locations(cs.implementations()).map(GotoDefinitionResponse::Array)
        })
        .await
    }

    async fn references(&self, params: ReferenceParams) -> Result<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        // Cold-open bounded waits (see `await_open_ready` / `await_index_ready`):
        // the file's own initial build, then an in-flight family index so
        // cross-file references resolve warm (the in-window `op_free` 1 → 118
        // heal) instead of returning def-only.
        self.await_open_ready(uri, WaitPolicy::Complete).await;
        if let Some(language) = self.files.get_open(uri).map(|d| d.language) {
            self.await_index_ready(language, WaitPolicy::Complete).await;
        }
        self.await_open_full(uri, WaitPolicy::Complete).await;
        // Snapshot + drop the store guard before `resolve()` (reentrant
        // `for_each_open`); see `Document::analysis`.
        let (analysis, language) = match self.files.get_open(uri) {
            Some(doc) => (Arc::clone(&doc.analysis), doc.language),
            None => return self.not_ready_or_null(uri),
        };

        let point = symbols::position_to_point(pos);
        let uri = uri.clone();
        let scope = self.override_scope();
        let degraded = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let degraded_flag = Arc::clone(&degraded);
        let out = self.run_query(move |cx| {
            // Pack languages resolve + collect through their sub-index
            // (mirrors goto-def and the CLI) — the hub only knows Perl
            // modules, so a cpp query against it silently misses every
            // cross-file use.
            let routed = cx.routed(language);
            let base_idx = routed.as_lookup();
            let self_path = uri.to_file_path().ok();
            // `#include` reverse — "who includes this header" — owns the path
            // token exclusively (the backward mirror of include goto-def). The
            // pack declares whether it has include tokens; asked, never named.
            if crate::build::language_driver::LanguageRegistry::has_include_tokens(&language) {
                if let Some(incs) = symbols::pack_include_references(
                    &analysis, point, self_path.as_deref(), base_idx)
                {
                    let locs: Vec<Location> = incs
                        .into_iter()
                        .filter_map(|(path, span)| {
                            Some(Location {
                                uri: Url::from_file_path(&path).ok()?,
                                range: symbols::span_to_range(span),
                            })
                        })
                        .collect();
                    return (!locs.is_empty()).then_some(locs);
                }
            }
            // (The reverse domain bridge — enum type → field-slot sites — is a
            // goto-implementation projection, NOT part of plain references.)
            // One construction, one projection — target/group/lexical
            // branching, visibility (incl. the origin's include-closure scope
            // and the pack VISIBLE widening), and the cross-file walk all live
            // inside the set.
            let cs = crate::util::timings::phase("refs.resolve", || {
                cx.set(base_idx, &analysis, &uri, point, scope)
            });
            let locs =
                refs_to_locations(crate::util::timings::phase("refs.project", || cs.references()));
            // The walk's completeness verdict, read on the walk's own thread
            // the moment it closes.
            degraded_flag.store(
                crate::model::witnesses::ResolutionSession::take_last_walk_degraded(),
                std::sync::atomic::Ordering::Relaxed,
            );
            locs
        })
        .await;
        // `references` answers `Location[]`; the protocol gives it no
        // completeness field, so a bounded answer says so the only way it
        // can — to the user, not to a log. Crude, but visible beats
        // correct-in-principle: a short list that looks complete is the
        // failure mode the bound exists to avoid.
        // Once per server session: someone hunting a large workspace runs
        // this verb repeatedly, and a toast per query is how a signal gets
        // trained into noise. The first one has to land; the tenth must not
        // be as loud. Every occurrence still reaches the log.
        static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if degraded.load(std::sync::atomic::Ordering::Relaxed)
            && !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            self.client
                .show_message(
                    MessageType::WARNING,
                    concat!(
                        "perl-lsp: this reference list is INCOMPLETE - the search hit its ",
                        "resolution budget at this workspace size. Raise ",
                        "PERL_LSP_RESOLVE_BUDGET_MILLISECONDS / PERL_LSP_ENRICH_DEPTH for a fuller ",
                        "answer. (Reported once per session; the log records each.)",
                    ),
                )
                .await;
        }
        out
    }

    async fn prepare_rename(
        &self,
        params: TextDocumentPositionParams,
    ) -> Result<Option<PrepareRenameResponse>> {
        self.await_open_ready(&params.text_document.uri, WaitPolicy::Interactive).await;
        // Snapshot + drop the store guard before `resolve()` (reentrant
        // `for_each_open`); see `Document::analysis`.
        let (analysis, language) = match self.files.get_open(&params.text_document.uri) {
            Some(doc) => (Arc::clone(&doc.analysis), doc.language),
            None => return self.not_ready_or_null(&params.text_document.uri),
        };
        let point = symbols::position_to_point(params.position);
        let uri = params.text_document.uri.clone();
        let scope = self.override_scope();
        self.run_query(move |cx| {
            // Same store routing as the rename handler, so this gate probes
            // the target rename would actually act on.
            let routed = cx.routed(language);
            // The rename box's range + placeholder.
            let box_at = analysis
                .symbol_at(point)
                .map(|sym| (sym.selection_span, sym.name.clone()))
                .or_else(|| analysis.ref_at(point).map(|r| (r.span, r.target_name.clone())));
            // Only offer a rename box where `rename` would actually produce
            // edits. Accepting on any `symbol_at`/`ref_at` hit is a UX trap:
            // positions like `@_` or an ownerless constructor key resolve to
            // nothing renameable, so the user gets a box that silently no-ops.
            // `renameable()` mirrors `rename_edits`' arms on the same set
            // (incl. the pack probe: a rename the set would refuse or no-op on
            // offers no box), so this gate tracks new renameable kinds
            // automatically, with no change here.
            let cs = cx.set(routed.as_lookup(), &analysis, &uri, point, scope);
            if !cs.renameable() {
                return None;
            }
            box_at.map(|(span, placeholder)| PrepareRenameResponse::RangeWithPlaceholder {
                range: symbols::span_to_range(span),
                placeholder,
            })
        })
        .await
    }

    async fn rename(&self, params: RenameParams) -> Result<Option<WorkspaceEdit>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        let new_name = &params.new_name;
        if !crate::index::resolve::is_valid_rename_name(new_name) {
            return Err(tower_lsp::jsonrpc::Error::invalid_params(
                "rename: the new name must not be empty or whitespace",
            ));
        }
        self.await_open_ready(uri, WaitPolicy::Complete).await;
        // Cross-file rename edits are act-on-able: a cold-index rename that
        // silently missed files would corrupt the workspace. Wait Complete.
        if let Some(language) = self.files.get_open(uri).map(|d| d.language) {
            self.await_index_ready(language, WaitPolicy::Complete).await;
        }
        self.await_open_full(uri, WaitPolicy::Complete).await;
        // Snapshot + drop the store guard before `resolve()` (reentrant
        // `for_each_open`); see `Document::analysis`.
        let (analysis, language) = match self.files.get_open(uri) {
            Some(doc) => (Arc::clone(&doc.analysis), doc.language),
            None => return self.not_ready_or_null(uri),
        };

        let point = symbols::position_to_point(pos);
        // Rename is the references image + policy, projected from the same
        // set: cross-file walk for workspace-stable targets, per-member texts
        // for groups, the origin file's rename machinery for lexicals. Pack
        // routing is a construction fact on the set: it widens the walk to
        // the per-language cache and REFUSES on alias-spelled sites instead
        // of emitting a partial edit.
        let uri = uri.clone();
        let new_name = new_name.clone();
        let scope = self.override_scope();
        self.run_query(move |cx| {
            let routed = cx.routed(language);
            let cs = cx.set(routed.as_lookup(), &analysis, &uri, point, scope);
            cs.rename_edits(&new_name)
                .map(edit_pairs_to_workspace_edit)
                .map_err(tower_lsp::jsonrpc::Error::invalid_params)
        })
        .await?
    }

    async fn hover(&self, params: HoverParams) -> Result<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        // Cold-open bounded waits (see `await_open_ready` / `await_index_ready`):
        // the file's own initial build, then an in-flight family index so hover
        // resolves warm.
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        if let Some(language) = self.files.get_open(uri).map(|d| d.language) {
            self.await_index_ready(language, WaitPolicy::Interactive).await;
        }
        // Snapshot + drop the store guard before `resolve()` (reentrant
        // `for_each_open`); see `Document::analysis`.
        let (analysis, text, language) = match self.files.get_open(uri) {
            Some(doc) => (Arc::clone(&doc.analysis), doc.text.clone(), doc.language),
            None => return self.not_ready_or_null(uri),
        };
        // Both languages present the set's resolution — constructed exactly
        // like the goto-def handler's set, so the two verbs can't disagree
        // at a position. Each keeps its own presenter: pack renders the
        // hover projection (top-ranked candidate), Perl renders through the
        // model primitives + the set's call-binding accessor.
        let uri = uri.clone();
        self.run_query(move |cx| {
            let routed = cx.routed(language);
            let base_idx = routed.as_lookup();
            let cs = cx
                .set(
                    base_idx,
                    &analysis,
                    &uri,
                    symbols::position_to_point(pos),
                    crate::index::resolve::OverrideScope::default(),
                )
                .with_source(&text);
            let caps = crate::build::language_driver::LanguageRegistry::caps(language);
            if !caps.hover_info {
                if let Some(h) = symbols::pack_hover(&cs, language) {
                    return Some(h);
                }
                // The raw-word fallback outside the set (mirrors goto-def's):
                // a macro / enum-constant / global whose token no ref captures
                // — show its definition line.
                if caps.cross_file_words {
                    let self_path = uri.to_file_path().ok();
                    // Inside the `cross_file_words` caps gate — pack-only.
                    let scoped = crate::model::file_analysis::ScopedLookup::new(
                        base_idx,
                    &analysis.pack.include_closure,
                    self_path.as_deref(),
                    crate::model::file_analysis::VisibilityAxis::IncludeClosure,
                );
                    if let Some((_, _, line)) =
                        cx.pack_xfile_word_at(&text, &analysis, pos, &scoped)
                    {
                        if !line.is_empty() {
                            return Some(Hover {
                                contents: HoverContents::Markup(MarkupContent {
                                    kind: MarkupKind::Markdown,
                                    value: format!("```{}\n{}\n```", language, line),
                                }),
                                range: None,
                            });
                        }
                    }
                }
                return None;
            }
            symbols::perl_hover(&cs, cx.index())
        })
        .await
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let pos = params.text_document_position.position;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        // Snapshot + drop the store guard before completion resolves (both the
        // pack and Perl paths gather cross-file candidates through `resolve()`,
        // which re-locks the open shards via `for_each_open`); see
        // `Document::analysis`. `tree` clones O(1) (tree-sitter refcount).
        let (analysis, text, tree, language, path, package_lines) =
            match self.files.get_open(uri) {
                Some(doc) => (
                    Arc::clone(&doc.analysis),
                    doc.text.clone(),
                    doc.tree.clone(),
                    doc.language,
                    doc.path.clone(),
                    doc.stable_outline.package_lines().to_vec(),
                ),
                None => return self.not_ready_or_null(uri),
            };
        // Both gathers run through `run_query`'s blocking hop: at workspace
        // scale the in-scope tier is tens of thousands of items (multi-MB,
        // ~60 s measured on CPAN-5k), and inline sync work in a handler
        // future stalls tower-lsp's single serve task for every verb.
        if !crate::build::language_driver::LanguageRegistry::caps(language).cursor_context {
            let point = symbols::position_to_point(pos);
            let (items, is_incomplete) = self
                .run_query(move |cx| {
                    pack_completion(
                        cx.files(),
                        &analysis,
                        &text,
                        &tree,
                        point,
                        language,
                        path.as_deref(),
                        cx.index(),
                    )
                })
                .await?;
            if items.is_empty() && !is_incomplete {
                return Ok(None);
            }
            // Prefix-gated cross-file gathering (macros, include-closure
            // symbols) filters server-side, so the client must re-request
            // as the typed prefix changes rather than reuse a cached list.
            return Ok(Some(if is_incomplete {
                CompletionResponse::List(CompletionList { is_incomplete: true, items })
            } else {
                CompletionResponse::Array(items)
            }));
        }
        // Both halves are load-bearing: the gather runs through `run_query`
        // so it cannot stall the shared serve task, and it returns the cap
        // flag that decides `isIncomplete` below.
        let key = FileKey::Url(uri.clone());
        let (items, capped) = self
            .run_query(move |cx| {
                symbols::completion_items(
                    cx.files(),
                    &key,
                    &analysis,
                    &tree,
                    &text,
                    pos,
                    cx.index(),
                    Some(&package_lines),
                )
            })
            .await?;
        // Incomplete when the payload cap fired (re-query narrows
        // server-side as the prefix grows), when any item is a loading
        // placeholder (re-request after the module resolves), or when the
        // list is EMPTY.
        //
        // The empty arm is the load-bearing one. This used to return
        // `Ok(None)` — a null result, which carries no `isIncomplete` field
        // at all and is therefore the most cacheable answer we can give: the
        // client is told there is nothing here and never asks again.
        // Measured over three cold sessions of 156 member positions, 2.8% of
        // member asks answer EMPTY and then answer properly once warm, so a
        // cached null leaves that slot dead for the rest of the session with
        // nothing to tell the user it is wrong. The pack path above already
        // preserves an incomplete-empty response (`items.is_empty() &&
        // !is_incomplete`); this one dropped it unconditionally.
        //
        // The cost lands only where an empty answer is genuinely permanent
        // (5.8% of member asks), and it is close to notional: an empty list
        // has nothing to filter client-side, so `isIncomplete` only asks for
        // a re-query the client already makes when the prefix changes.
        let is_incomplete = capped
            || items.is_empty()
            || items.iter().any(|i| i.insert_text.as_deref() == Some(""));
        {
            if is_incomplete {
                // Trigger resolution for the module being loaded
                for i in &items {
                    if i.insert_text.as_deref() == Some("") {
                        if let Some(ref label) = Some(&i.label) {
                            if let Some(name) = label.strip_prefix("loading ").and_then(|s| s.strip_suffix("...")) {
                                self.module_index.request_resolve(name);
                            }
                        }
                    }
                }
                Ok(Some(CompletionResponse::List(CompletionList {
                    is_incomplete: true,
                    items,
                })))
            } else {
                Ok(Some(CompletionResponse::Array(items)))
            }
        }
    }

    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> Result<Option<SignatureHelp>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        let doc = match self.files.get_open(uri) {
            Some(doc) => doc,
            None => return self.not_ready_or_null(uri),
        };
        let caps = crate::build::language_driver::LanguageRegistry::caps(doc.language);
        if caps.pack_signature_help {
            // A pack document: the call site comes from the pack's own tree
            // and the callee from the member ladder through the routed pack
            // index (the blocking hop, like every cross-file verb).
            let analysis = Arc::clone(&doc.analysis);
            let tree = doc.tree.clone();
            let text = doc.text.clone();
            let language = doc.language;
            drop(doc);
            return self
                .run_query(move |cx| {
                    let routed = cx.routed(language);
                    symbols::pack_signature_help(&analysis, &tree, &text, pos, language, routed.as_lookup())
                })
                .await;
        }
        if !caps.signature_help {
            return Ok(None); // the verb is declared per language
        }
        Ok(symbols::signature_help(&doc.analysis, &doc.tree, &doc.text, pos, &self.module_index))
    }

    async fn document_highlight(
        &self,
        params: DocumentHighlightParams,
    ) -> Result<Option<Vec<DocumentHighlight>>> {
        let uri = &params.text_document_position_params.text_document.uri;
        let pos = params.text_document_position_params.position;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        // Snapshot + drop the store guard before `resolve()` (reentrant
        // `for_each_open`); see `Document::analysis`.
        let (analysis, language) = match self.files.get_open(uri) {
            Some(doc) => (Arc::clone(&doc.analysis), doc.language),
            None => return self.not_ready_or_null(uri),
        };
        // Same construction as references — highlights is its origin-narrowed
        // projection, so the two verbs answer one resolution.
        let uri = uri.clone();
        let scope = self.override_scope();
        self.run_query(move |cx| {
            let routed = cx.routed(language);
            let cs = cx.set(
                routed.as_lookup(),
                &analysis,
                &uri,
                symbols::position_to_point(pos),
                scope,
            );
            let highlights = symbols::document_highlights(&cs);
            (!highlights.is_empty()).then_some(highlights)
        })
        .await
    }

    async fn selection_range(
        &self,
        params: SelectionRangeParams,
    ) -> Result<Option<Vec<SelectionRange>>> {
        let uri = &params.text_document.uri;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        let doc = match self.files.get_open(uri) {
            Some(doc) => doc,
            None => return self.not_ready_or_null(uri),
        };
        if !crate::build::language_driver::LanguageRegistry::caps(doc.language).selection_range {
            return Ok(None); // tree-shape handler, declared per language
        }
        let ranges: Vec<SelectionRange> = params
            .positions
            .iter()
            .map(|pos| symbols::selection_ranges(&doc.tree, *pos))
            .collect();
        Ok(Some(ranges))
    }

    async fn folding_range(
        &self,
        params: FoldingRangeParams,
    ) -> Result<Option<Vec<FoldingRange>>> {
        let uri = &params.text_document.uri;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        let doc = match self.files.get_open(uri) {
            Some(doc) => doc,
            None => return self.not_ready_or_null(uri),
        };
        let ranges = symbols::folding_ranges(&doc.analysis);
        if ranges.is_empty() {
            Ok(None)
        } else {
            Ok(Some(ranges))
        }
    }

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        // Copy the source out and release the DashMap guard before awaiting
        // perltidy: holding a shard read lock across the await deadlocks any
        // concurrent didChange (which needs the write lock) on the same file.
        let source = match self.files.get_open(uri) {
            Some(doc) => doc.text.clone(),
            None => return self.not_ready_or_null(uri),
        };

        // Shell out to perltidy
        let output = match run_perltidy(source.clone()).await {
            Ok(o) => o,
            Err(e) => {
                self.client
                    .log_message(
                        MessageType::ERROR,
                        format!("Failed to run perltidy: {}", e),
                    )
                    .await;
                return Ok(None);
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            self.client
                .log_message(
                    MessageType::WARNING,
                    format!("perltidy exited with error: {}", stderr),
                )
                .await;
            return Ok(None);
        }

        let formatted = String::from_utf8_lossy(&output.stdout).to_string();
        if formatted == source {
            return Ok(None);
        }

        // Replace entire document
        let line_count = source.lines().count();
        Ok(Some(vec![TextEdit {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: line_count as u32,
                    character: 0,
                },
            },
            new_text: formatted,
        }]))
    }

    async fn code_action(&self, params: CodeActionParams) -> Result<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        let doc = match self.files.get_open(uri) {
            Some(doc) => doc,
            None => return self.not_ready_or_null(uri),
        };
        let actions = symbols::code_actions(&params.context.diagnostics, &doc.analysis, uri);
        if actions.is_empty() {
            Ok(None)
        } else {
            Ok(Some(actions))
        }
    }

    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> Result<Option<SemanticTokensResult>> {
        let uri = &params.text_document.uri;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        let doc = match self.files.get_open(uri) {
            Some(doc) => doc,
            None => return self.not_ready_or_null(uri),
        };
        let tokens = symbols::semantic_tokens(&doc.analysis);
        Ok(Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: tokens,
        })))
    }

    async fn inlay_hint(&self, params: InlayHintParams) -> Result<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        self.await_open_ready(uri, WaitPolicy::Interactive).await;
        let doc = match self.files.get_open(uri) {
            Some(doc) => doc,
            None => return self.not_ready_or_null(uri),
        };
        let caps = crate::build::language_driver::LanguageRegistry::caps(doc.language);
        if caps.pack_signature_help {
            // A pack document: the call sites come from its own tree and
            // each callee from the member ladder through the routed pack
            // index — the same hop signature help takes.
            let analysis = Arc::clone(&doc.analysis);
            let tree = doc.tree.clone();
            let text = doc.text.clone();
            let language = doc.language;
            let range = params.range;
            drop(doc);
            return self
                .run_query(move |cx| {
                    let routed = cx.routed(language);
                    let hints = symbols::pack_inlay_hints(&analysis, &tree, &text, range, language, routed.as_lookup());
                    if hints.is_empty() { None } else { Some(hints) }
                })
                .await;
        }
        let hints = symbols::inlay_hints(&doc.analysis, params.range);
        if hints.is_empty() {
            Ok(None)
        } else {
            Ok(Some(hints))
        }
    }

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> Result<Option<Vec<SymbolInformation>>> {
        let query = params.query.to_lowercase();
        // The resident sweeps read whole stores and the rows pass reads
        // SQLite — the whole verb runs behind the blocking hop.
        self.run_query(move |cx| {
            let mut results = Vec::new();
            // Paths a symbols-present resident copy already answered — the
            // rows pass skips these (open docs and un-evicted copies are
            // fresher than their persisted rows; evicted copies are
            // rows-guaranteed).
            let mut covered: std::collections::HashSet<std::path::PathBuf> =
                std::collections::HashSet::new();

            cx.files().for_each_analysis(|key, analysis| {
                let uri = match key {
                    FileKey::Url(u) => u,
                    FileKey::Path(p) => Url::from_file_path(&p).unwrap_or_else(|_| {
                        Url::parse(&format!("file://{}", p.display()))
                            .unwrap()
                    }),
                };
                if !analysis.symbols_are_evicted() {
                    if let Ok(p) = uri.to_file_path() {
                        // Claim the canonical spelling too: rows are keyed
                        // canonical, and an open doc reached through a
                        // symlinked root must shadow its own persisted rows.
                        if let Ok(canon) = std::fs::canonicalize(&p) {
                            covered.insert(canon);
                        }
                        covered.insert(p);
                    }
                }
                for sym in analysis.symbols() {
                    if sym.name.to_lowercase().contains(&query) {
                        if let Some(info) = symbols::symbol_to_workspace_info(sym, uri.clone()) {
                            results.push(info);
                        }
                    }
                }
                // Plugin namespaces — match on both id and kind so users
                // can find "the minion tasks in this workspace" via either
                // "minion" or "tasks".
                for ns in &analysis.plugin.namespaces {
                    let hay = format!("{} {}", ns.id.to_lowercase(), ns.kind.to_lowercase());
                    if hay.contains(&query) {
                        results.push(symbols::plugin_namespace_to_workspace_info(ns, uri.clone()));
                    }
                }
            });

            // Pack-language (C/C++/…) symbols live in per-language
            // sub-indexes, not the FileStore — sweep them so a C typedef/
            // class/free function shows in workspace search alongside Perl
            // packages.
            cx.index().for_each_pack_registered_file(&mut |path, analysis| {
                if !analysis.symbols_are_evicted() {
                    covered.insert(path.to_path_buf());
                }
                let uri = Url::from_file_path(path).unwrap_or_else(|_| {
                    Url::parse(&format!("file://{}", path.display())).unwrap()
                });
                for sym in analysis.symbols() {
                    if sym.name.to_lowercase().contains(&query) {
                        if let Some(info) = symbols::symbol_to_workspace_info(sym, uri.clone()) {
                            results.push(info);
                        }
                    }
                }
            });

            // Rows pass: symbol-evicted copies (Perl workspace + @INC + every
            // pack tier) answer from the relational store — the resident sweep
            // above saw empty vecs for them. Same containment test, same
            // kind/visibility filters as `symbol_to_workspace_info`.
            for hit in cx.sym_rows(&query) {
                let path = std::path::PathBuf::from(&hit.path);
                if covered.contains(&path) {
                    continue;
                }
                if let Some(info) = symbols::sym_row_to_workspace_info(&hit) {
                    results.push(info);
                }
            }

            symbols::dedup_workspace_symbols(&mut results);
            (!results.is_empty()).then_some(results)
        })
        .await
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        // Route by the driver's declared invalidation owner: an
        // invalidator-owned language goes through the invalidation seam
        // (re-register + reverse-closure eviction); the rest take the hub's
        // direct re-index.
        let mut perl_changes: Vec<(PathBuf, FileChangeType)> = Vec::new();
        {
            let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
            for change in params.changes {
                let Ok(path) = change.uri.to_file_path() else { continue };
                match reg.for_path(&path) {
                    Some(d) if d.caps().pack_invalidation => {
                        self.schedule_pack_invalidate(
                            path,
                            change.typ == FileChangeType::DELETED,
                        );
                    }
                    _ => perl_changes.push((path, change.typ)),
                }
            }
        }
        self.reindex_saved_perl(perl_changes).await;
    }

    async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        // Copy the source out and release the DashMap guard before awaiting
        // perltidy — see `formatting` for why holding it across the await
        // deadlocks concurrent didChange on the same file.
        let source = match self.files.get_open(uri) {
            Some(doc) => doc.text.clone(),
            None => return self.not_ready_or_null(uri),
        };

        // Extract lines for the range
        let start_line = params.range.start.line as usize;
        let end_line = params.range.end.line as usize;
        let lines: Vec<&str> = source.lines().collect();
        let end = end_line.saturating_add(1).min(lines.len());
        // A malformed or inverted client range (start after end, or start past
        // EOF) must degrade, not panic on the slice.
        if start_line >= end {
            return Ok(None);
        }
        let range_text: String = lines[start_line..end].join("\n") + "\n";

        // Shell out to perltidy on the range
        let output = match run_perltidy(range_text.clone()).await {
            Ok(o) if o.status.success() => o,
            _ => return Ok(None),
        };

        let formatted = String::from_utf8_lossy(&output.stdout).to_string();
        if formatted == range_text {
            return Ok(None);
        }

        Ok(Some(vec![TextEdit {
            range: Range {
                start: Position { line: start_line as u32, character: 0 },
                end: Position { line: end as u32, character: 0 },
            },
            new_text: formatted,
        }]))
    }

    async fn linked_editing_range(
        &self,
        _params: LinkedEditingRangeParams,
    ) -> Result<Option<LinkedEditingRanges>> {
        // Capability is off (see initialize); null here keeps clients that
        // ignore capabilities from co-editing anyway (#116). The co-edit
        // projection (CandidateSet::linked_editing_spans) stays CLI-queryable
        // via --linked-editing.
        Ok(None)
    }
}

/// Run perltidy over `input`, returning its captured output.
///
/// `kill_on_drop` so a cancelled formatting request (the editor sends
/// `$/cancelRequest`, tower-lsp aborts the handler future) reaps perltidy
/// instead of leaving a `<defunct>` zombie (#80). The stdin write runs in its
/// own task concurrently with `wait_with_output`'s stdout drain so we never
/// block writing stdin while perltidy is blocked writing stdout.
async fn run_perltidy(input: String) -> std::io::Result<std::process::Output> {
    use tokio::io::AsyncWriteExt;

    let mut child = tokio::process::Command::new("perltidy")
        .arg("--standard-output")
        .arg("--standard-error-output")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let stdin = child.stdin.take();
    let writer = tokio::spawn(async move {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(input.as_bytes()).await;
            // drop closes stdin, signalling EOF to perltidy
        }
    });

    let output = child.wait_with_output().await;
    let _ = writer.await;
    output
}

#[cfg(test)]
mod position_encoding_tests {
    use super::*;
    use tower_lsp::LspService;

    async fn init_with(encodings: Option<Vec<PositionEncodingKind>>) -> InitializeResult {
        let (service, _socket) = LspService::new(Backend::new);
        let params = InitializeParams {
            capabilities: ClientCapabilities {
                general: Some(GeneralClientCapabilities {
                    position_encodings: encodings,
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        service.inner().initialize(params).await.expect("initialize")
    }

    /// A client that can speak byte positions is told the truth: every
    /// column this server emits IS a byte offset, so advertising utf-8
    /// makes the client convert instead of misreading them as UTF-16
    /// code units on non-ASCII lines.
    #[tokio::test]
    async fn utf8_offer_is_accepted() {
        let r = init_with(Some(vec![
            PositionEncodingKind::UTF16,
            PositionEncodingKind::UTF8,
        ]))
        .await;
        assert_eq!(r.capabilities.position_encoding, Some(PositionEncodingKind::UTF8));
    }

    /// No utf-8 offer → stay silent (the spec default, utf-16, applies).
    /// The mismatch on non-ASCII lines remains for such clients — a known
    /// gap, not a silently claimed capability.
    #[tokio::test]
    async fn no_offer_stays_on_the_default() {
        let r = init_with(None).await;
        assert_eq!(r.capabilities.position_encoding, None);
        let r = init_with(Some(vec![PositionEncodingKind::UTF16])).await;
        assert_eq!(r.capabilities.position_encoding, None);
    }
}
