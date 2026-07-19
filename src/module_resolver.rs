//! Module resolver: background thread that resolves Perl modules from `@INC`.
//!
//! Discovers `@INC` paths, locates `.pm` files, parses them in-process with
//! tree-sitter-perl, and extracts export metadata for the module index.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
use tower_lsp::lsp_types::*;
use tower_lsp::lsp_types::{notification, request};
use tower_lsp::Client;
use tree_sitter::Parser;

use crate::cpanfile;
use crate::module_cache;
use crate::module_index::{CachedModule, ModuleEdgeIndexes, ResolveNotify, ResolveQueue, WorkspaceRootChannel};

/// Callback invoked after each module is resolved. Used to trigger diagnostic refresh.
pub type OnResolved = Box<dyn Fn() + Send + Sync>;

/// Spawn the resolver thread. Returns immediately; the thread runs in the background.
///
/// The `on_resolved` callback fires after each module is inserted into the cache,
/// allowing the backend to re-publish diagnostics.
pub fn spawn_resolver(
    cache: Arc<DashMap<String, Option<Arc<CachedModule>>>>,
    edges: Arc<ModuleEdgeIndexes>,
    stale_modules: Arc<DashMap<String, ()>>,
    available_modules: Arc<DashMap<String, PathBuf>>,
    builtins: Arc<DashMap<String, String>>,
    queue: Arc<ResolveQueue>,
    resolved: Arc<ResolveNotify>,
    workspace_root: Arc<WorkspaceRootChannel>,
    client: Client,
    on_resolved: OnResolved,
    long_lived: Arc<std::sync::atomic::AtomicBool>,
    bag_cache: Arc<std::sync::RwLock<Option<Arc<crate::pack_bag_cache::PackBagCache>>>>,
    // The enrichment-key generation maps, threaded the same way as
    // `long_lived`/`bag_cache`: the resolver thread mints a generation for
    // every @INC provider it warms or (re-)resolves so `enrichment_key` reads
    // a real, ABA-proof token instead of an Arc pointer.
    registration_gen: Arc<DashMap<PathBuf, u64>>,
    gen_counter: Arc<std::sync::atomic::AtomicU64>,
) {
    let handle = tokio::runtime::Handle::current();

    std::thread::Builder::new()
        .name("module-resolver".into())
        .spawn(move || {
            let mut inc_paths = discover_inc_paths();

            // Wait for workspace root from initialize() for per-project cache path.
            let ws_root = wait_for_workspace_root(&workspace_root);

            // Auto-discover project-local lib paths (lib/, local/lib/perl5/).
            if let Some(ref root_uri) = ws_root {
                if let Some(root_path) = uri_to_path(root_uri) {
                    add_project_lib_paths(&mut inc_paths, &root_path);
                }
            }

            // Scan @INC for available module names (fast, no parsing — just readdir)
            scan_inc_module_names(&inc_paths, &available_modules);
            log::info!("@INC scan: {} modules available", available_modules.len());

            // Warm the in-memory cache from SQLite.
            let db = module_cache::open_cache_db(ws_root.as_deref(), "perl");
            if let Some(ref conn) = db {
                let _ = module_cache::validate_inc_paths(conn, &inc_paths);
                let _ = module_cache::validate_plugin_fingerprint(
                    conn,
                    &crate::plugin::rhai_host::plugin_fingerprint(),
                );
                // Hydrate Perl builtin hover docs (cached in SQLite,
                // re-parsed from perlfunc.pod only when the perl
                // version tag changes).
                match module_cache::hydrate_builtins(conn) {
                    Ok(map) => {
                        for entry in map.iter() {
                            builtins.insert(entry.key().clone(), entry.value().clone());
                        }
                    }
                    Err(e) => log::warn!("Builtins hydrate failed: {}", e),
                }
                let strip_warm = long_lived.load(std::sync::atomic::Ordering::Relaxed)
                    && eviction_enabled();
                let (n, stale_names) = module_cache::warm_cache(conn, &cache, strip_warm);
                log::info!("Warmed module cache: {} entries loaded from disk, {} stale", n, stale_names.len());
                // Stamp generations for the warm-loaded @INC providers (they
                // landed in the cache without a registration front door).
                crate::module_index::stamp_missing_import_gens(
                    &cache, &registration_gen, &gen_counter,
                );
                // Queue stale modules for priority re-resolution.
                for name in &stale_names {
                    stale_modules.insert(name.clone(), ());
                }
                if !stale_names.is_empty() {
                    let mut pq = queue.priority.lock().unwrap();
                    pq.extend(stale_names);
                    queue.condvar.notify_one();
                }
                // Build reverse index from warmed cache.
                rebuild_reverse_index(&cache, &edges);
            }

            // Track which extract version each module was resolved at.
            let mut seen: HashMap<String, i64> = HashMap::new();

            // One parser + one parent-fallback memo for the whole sweep.
            // Without the memo, every child whose own exports are empty re-parses
            // its parent (e.g. ~50× Exporter, ~30× URI on a cold cpanfile run).
            let mut parser = create_parser();
            let mut parse_memo: ParseMemo = HashMap::new();

            // Queue cpanfile dependencies (non-blocking — lets priority items go first).
            // Track total for progress reporting in the main loop.
            let mut cpanfile_total = 0usize;
            let mut cpanfile_done = 0usize;
            if let Some(ref root_uri) = ws_root {
                if let Some(root_path) = uri_to_path(root_uri) {
                    let cpanfile_modules = cpanfile::parse_cpanfile(&root_path);
                    let to_resolve: Vec<String> = cpanfile_modules
                        .into_iter()
                        .filter(|m| !cache.contains_key(m.as_str()))
                        .collect();

                    if !to_resolve.is_empty() {
                        cpanfile_total = to_resolve.len();
                        log::info!("cpanfile: {} modules queued for indexing", cpanfile_total);

                        // Start progress bar.
                        let token = NumberOrString::String("perl-lsp/indexing".to_string());
                        let _ = handle.block_on(client.send_request::<request::WorkDoneProgressCreate>(
                            WorkDoneProgressCreateParams { token: token.clone() },
                        ));
                        handle.block_on(client.send_notification::<notification::Progress>(
                            ProgressParams {
                                token,
                                value: ProgressParamsValue::WorkDone(WorkDoneProgress::Begin(
                                    WorkDoneProgressBegin {
                                        title: "Indexing Perl modules".into(),
                                        cancellable: Some(false),
                                        message: None,
                                        percentage: Some(0),
                                    },
                                )),
                            },
                        ));

                        let mut pending = queue.pending.lock().unwrap();
                        pending.extend(to_resolve);
                        queue.condvar.notify_one();
                    }
                }
            }

            // Main resolve loop — drain priority first, then pending.
            loop {
                let batch = drain_next_batch(&queue);

                for module_name in batch {
                    // Allow re-resolution when extract version is outdated.
                    if let Some(&ver) = seen.get(&module_name) {
                        if ver >= module_cache::EXTRACT_VERSION {
                            continue;
                        }
                    }
                    seen.insert(module_name.clone(), module_cache::EXTRACT_VERSION);

                    let is_re_resolve = stale_modules.contains_key(&module_name);
                    if is_re_resolve {
                        log::info!("Re-resolving stale module '{}'", module_name);
                        // Stale entry must not be served from the run-local memo.
                        parse_memo.remove(&module_name);
                    } else {
                        log::info!("Resolving module '{}'", module_name);
                    }

                    let result = parse_module(&inc_paths, &module_name, &mut parser, &mut parse_memo);
                    match &result {
                        Some(m) => log::info!(
                            "Resolved '{}': {} export, {} export_ok",
                            module_name,
                            m.analysis.export.len(),
                            m.analysis.export_ok.len()
                        ),
                        None => log::info!("No exports found for '{}'", module_name),
                    }
                    let persisted = db
                        .as_ref()
                        .map(|conn| save_module_generation(conn, &module_name, &result))
                        .unwrap_or(false);
                    let stored =
                        strip_import_copy(&result, persisted, eviction_enabled());
                    // The memo would otherwise pin the WHOLE closure for the
                    // thread's lifetime — a second copy of the tier. Failed
                    // resolves are NOT memoized (pre-existing semantics: the
                    // parent-fallback re-probes, catching mid-session
                    // installs).
                    match &stored {
                        Some(_) => {
                            parse_memo.insert(module_name.clone(), stored.clone());
                        }
                        None => {
                            parse_memo.remove(&module_name);
                        }
                    }
                    // Stale-pin clear BEFORE the new copy is reachable — a
                    // re-resolve replaced the blob; a query racing this
                    // insert must not rehydrate the prior generation.
                    if let Some(ref m) = stored {
                        if let Some(bc) = bag_cache.read().ok().and_then(|g| g.clone()) {
                            bc.invalidate(&m.path);
                        }
                        // Mint a fresh generation: a re-resolve (content
                        // changed) moves every consumer's enrichment key.
                        crate::module_index::mint_registration_gen(
                            &registration_gen, &gen_counter, &m.path,
                        );
                    }
                    insert_into_cache(&cache, &edges, &module_name, stored);

                    // Descend into the module's own dependencies so the
                    // chain keeps resolving beyond the open doc's direct
                    // imports. Without this the cache stops at depth 1 —
                    // e.g. opening a Mojolicious::Lite script resolves
                    // Mojolicious.pm, but Mojolicious.pm's
                    // `has routes => sub { Mojolicious::Routes->new }`
                    // never triggers a resolve on Mojolicious::Routes,
                    // and `$r->get` on line 71 of the demo chain-dies
                    // because the intermediate class is a cache miss.
                    //
                    // The `seen` guard above makes this cycle-safe: a
                    // transitively-enqueued name that was already
                    // resolved at the current EXTRACT_VERSION gets
                    // skipped on its next turn.
                    if let Some(ref m) = result {
                        let mut pending = queue.pending.lock().unwrap();
                        let enqueue = |pending: &mut Vec<String>, name: String| {
                            if name.is_empty() { return; }
                            if cache.contains_key(&name) { return; }
                            if seen.contains_key(&name) { return; }
                            if !pending.iter().any(|p| p == &name) {
                                pending.push(name);
                            }
                        };
                        // Explicit imports — the module's own `use` statements.
                        for imp in &m.analysis.imports {
                            enqueue(&mut pending, imp.module_name.clone());
                        }
                        // Re-export edges — a re-exporting module (Test::Most →
                        // Test::More) pulls its producers' surfaces transitively,
                        // so those producers must be resolved even when no file
                        // `use`s them directly.
                        for re in &m.analysis.reexport_modules {
                            enqueue(&mut pending, re.clone());
                        }
                        // Parent classes — inheritance chain.
                        for parents in m.analysis.package_parents.values() {
                            for parent in parents {
                                enqueue(&mut pending, parent.clone());
                            }
                        }
                        // ClassName return types — `has foo => sub { Bar->new }`,
                        // plugin-emitted typed Subs, method return annotations.
                        // These are the chain-invisible-but-reachable classes
                        // the user's chain walks through at query time.
                        for sym in &m.analysis.symbols {
                            use crate::file_analysis::{InferredType, SymKind, SymbolDetail};
                            if !matches!(sym.kind, SymKind::Sub | SymKind::Method) { continue; }
                            if !matches!(sym.detail, SymbolDetail::Sub { .. }) { continue; }
                            if let Some(InferredType::ClassName(c)) =
                                m.analysis.symbol_return_type_via_bag(sym.id, None)
                            {
                                enqueue(&mut pending, c);
                            }
                        }
                        if !pending.is_empty() {
                            queue.condvar.notify_one();
                        }
                    }

                    // Remove from stale set after successful re-resolution.
                    if is_re_resolve {
                        stale_modules.remove(&module_name);
                    }

                    // Report cpanfile progress.
                    if cpanfile_total > 0 && cpanfile_done < cpanfile_total {
                        cpanfile_done += 1;
                        let pct = (cpanfile_done * 100 / cpanfile_total) as u32;
                        let token = NumberOrString::String("perl-lsp/indexing".to_string());
                        if cpanfile_done < cpanfile_total {
                            handle.block_on(client.send_notification::<notification::Progress>(
                                ProgressParams {
                                    token,
                                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::Report(
                                        WorkDoneProgressReport {
                                            cancellable: Some(false),
                                            message: Some(format!("{} ({}/{})", module_name, cpanfile_done, cpanfile_total)),
                                            percentage: Some(pct),
                                        },
                                    )),
                                },
                            ));
                        } else {
                            handle.block_on(client.send_notification::<notification::Progress>(
                                ProgressParams {
                                    token,
                                    value: ProgressParamsValue::WorkDone(WorkDoneProgress::End(
                                        WorkDoneProgressEnd {
                                            message: Some(format!("Indexed {} modules", cpanfile_total)),
                                        },
                                    )),
                                },
                            ));
                        }
                    }

                    // Signal waiters and trigger diagnostic refresh.
                    {
                        let _g = resolved.mu.lock().unwrap();
                        resolved.cv.notify_all();
                    }
                    on_resolved();
                }
            }
        })
        .expect("failed to spawn module-resolver thread");
}

/// Drain the next batch from the queue, checking priority first.
fn drain_next_batch(queue: &ResolveQueue) -> Vec<String> {
    // Check priority first
    {
        let mut priority = queue.priority.lock().unwrap();
        if !priority.is_empty() {
            return std::mem::take(&mut *priority);
        }
    }
    // Wait for pending
    let mut pending = queue.pending.lock().unwrap();
    loop {
        if !pending.is_empty() {
            // Before draining pending, re-check priority
            let mut priority = queue.priority.lock().unwrap();
            if !priority.is_empty() {
                return std::mem::take(&mut *priority);
            }
            return std::mem::take(&mut *pending);
        }
        pending = queue.condvar.wait(pending).unwrap();
    }
}

/// Headless resolver — no Client, no LSP progress. Same @INC scan,
/// project-local lib discovery, SQLite warm/persist, and index feeds
/// as the full resolver. Serves tests AND one-shot CLI sessions
/// (`ModuleIndex::new_for_cli`), which previously had NO resolver at
/// all and could only read what editor sessions had cached.
#[doc(hidden)]
pub fn spawn_test_resolver(
    cache: Arc<DashMap<String, Option<Arc<CachedModule>>>>,
    edges: Arc<ModuleEdgeIndexes>,
    stale_modules: Arc<DashMap<String, ()>>,
    available_modules: Arc<DashMap<String, PathBuf>>,
    queue: Arc<ResolveQueue>,
    resolved: Arc<ResolveNotify>,
    workspace_root: Arc<WorkspaceRootChannel>,
    registration_gen: Arc<DashMap<PathBuf, u64>>,
    gen_counter: Arc<std::sync::atomic::AtomicU64>,
) {
    std::thread::Builder::new()
        .name("module-resolver-test".into())
        .spawn(move || {
            let mut inc_paths = discover_inc_paths();
            let ws_root = wait_for_workspace_root(&workspace_root);

            if let Some(ref root_uri) = ws_root {
                if let Some(root_path) = uri_to_path(root_uri) {
                    add_project_lib_paths(&mut inc_paths, &root_path);
                }
            }

            scan_inc_module_names(&inc_paths, &available_modules);

            let db = module_cache::open_cache_db(ws_root.as_deref(), "perl");
            if let Some(ref conn) = db {
                let _ = module_cache::validate_inc_paths(conn, &inc_paths);
                let _ = module_cache::validate_plugin_fingerprint(
                    conn,
                    &crate::plugin::rhai_host::plugin_fingerprint(),
                );
                let (_, stale_names) = module_cache::warm_cache(conn, &cache, false);
                crate::module_index::stamp_missing_import_gens(
                    &cache, &registration_gen, &gen_counter,
                );
                for name in stale_names {
                    stale_modules.insert(name, ());
                }
                rebuild_reverse_index(&cache, &edges);
            }

            let mut seen: HashMap<String, i64> = HashMap::new();
            let mut parser = create_parser();
            let mut parse_memo: ParseMemo = HashMap::new();
            loop {
                let batch = drain_next_batch(&queue);
                for module_name in batch {
                    if let Some(&ver) = seen.get(&module_name) {
                        if ver >= module_cache::EXTRACT_VERSION {
                            continue;
                        }
                    }
                    seen.insert(module_name.clone(), module_cache::EXTRACT_VERSION);
                    if stale_modules.contains_key(&module_name) {
                        parse_memo.remove(&module_name);
                    }

                    let result = parse_module(&inc_paths, &module_name, &mut parser, &mut parse_memo);
                    let persisted = db
                        .as_ref()
                        .map(|conn| save_module_generation(conn, &module_name, &result))
                        .unwrap_or(false);
                    let stored =
                        strip_import_copy(&result, persisted, eviction_enabled());
                    parse_memo.insert(module_name.clone(), stored.clone());
                    if let Some(ref m) = stored {
                        crate::module_index::mint_registration_gen(
                            &registration_gen, &gen_counter, &m.path,
                        );
                    }
                    insert_into_cache(&cache, &edges, &module_name, stored);
                    stale_modules.remove(&module_name);
                    let _g = resolved.mu.lock().unwrap();
                    resolved.cv.notify_all();
                }
            }
        })
        .expect("failed to spawn test module-resolver thread");
}

// ---- Internal helpers ----

fn wait_for_workspace_root(ws_root_channel: &WorkspaceRootChannel) -> Option<String> {
    let mut guard = ws_root_channel.root.lock().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while guard.is_none() {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            log::warn!("Timed out waiting for workspace root; using global cache");
            break;
        }
        let (g, _) = ws_root_channel
            .condvar
            .wait_timeout(guard, remaining)
            .unwrap();
        guard = g;
    }
    guard.clone().flatten()
}

/// Insert a resolved module into the cache and update the edge indexes.
/// The @INC tier's registration-owned strip: once the blob is persisted,
/// the resident copy drops its witness bag (the dominant share of a CPAN
/// module's payload; `bag_present` rehydrates through the hub's LRU).
/// Symbols and refs stay resident this slice — their reader routing for
/// the import tier is the follow-up in
/// `docs/prompt-storage-residuals.md`. Degraded
/// analyses keep the bag (their rows never persist).
fn strip_import_copy(
    result: &Option<Arc<CachedModule>>,
    persisted: bool,
    strip: bool,
) -> Option<Arc<CachedModule>> {
    match result {
        Some(m) if persisted && strip && !m.analysis.degraded => {
            let mut fa = (*m.analysis).clone();
            fa.evict_axes(true, false);
            Some(Arc::new(CachedModule::new(m.path.clone(), Arc::new(fa))))
        }
        _ => result.clone(),
    }
}

fn insert_into_cache(
    cache: &DashMap<String, Option<Arc<CachedModule>>>,
    edges: &ModuleEdgeIndexes,
    module_name: &str,
    result: Option<Arc<CachedModule>>,
) {
    if let Some(ref cached) = result {
        edges.feed(module_name, &cached.analysis);
    } else if matches!(cache.get(module_name).as_deref(), Some(Some(_))) {
        // On-demand @INC resolution missed this module (`None`), but the
        // workspace indexer already built it (e.g. a project module under a
        // relative `use lib` the resolver's @INC doesn't cover). Don't let
        // the miss clobber the indexed copy — and don't leave the reverse
        // index pointing at a module the cache no longer holds (the orphan
        // that broke cross-file Handler / dispatch lookup). Keep the Some.
        return;
    }
    cache.insert(module_name.to_string(), result);
}

/// Rebuild edge indexes from existing cache (e.g. after warming from
/// SQLite). The warm path writes blobs straight into the cache without
/// touching the indexes, so skipping this leaves every reverse lookup
/// blind on warm starts (cold/warm attribution, the B6 class).
fn rebuild_reverse_index(
    cache: &DashMap<String, Option<Arc<CachedModule>>>,
    edges: &ModuleEdgeIndexes,
) {
    edges.clear();
    for entry in cache.iter() {
        if let Some(ref cached) = *entry.value() {
            edges.feed(entry.key(), &cached.analysis);
        }
    }
}

// ---- Module parsing ----

/// Run-local memo for `resolve_and_parse_with_memo`. Persists across many
/// top-level calls within a single resolver sweep so that parent-fallback
/// recursion (e.g. 50 children all inheriting from `Exporter`) parses each
/// parent exactly once.
pub type ParseMemo = HashMap<String, Option<Arc<CachedModule>>>;

/// Parse a module file directly in-process.
/// tree-sitter-perl is stable — no subprocess isolation needed.
fn parse_module(
    inc_paths: &[PathBuf],
    module_name: &str,
    parser: &mut Parser,
    memo: &mut ParseMemo,
) -> Option<Arc<CachedModule>> {
    resolve_and_parse_with_memo(inc_paths, module_name, parser, memo)
}

pub use crate::builder::create_parser;

// ---- Resolution ----

pub fn resolve_module_path(inc_paths: &[PathBuf], module_name: &str) -> Option<PathBuf> {
    let rel_path = module_name.replace("::", "/") + ".pm";
    for inc in inc_paths {
        let full = inc.join(&rel_path);
        if full.is_file() {
            return Some(full);
        }
    }
    None
}

#[allow(dead_code)]
pub fn resolve_and_parse(
    inc_paths: &[PathBuf],
    module_name: &str,
    parser: &mut Parser,
) -> Option<Arc<CachedModule>> {
    let mut memo: ParseMemo = HashMap::new();
    resolve_and_parse_with_memo(inc_paths, module_name, parser, &mut memo)
}

/// Parse a module while sharing a memo across calls. Callers that resolve
/// many modules in a loop (the resolver thread, CLI startup) should hoist
/// one `ParseMemo` and reuse it so parent-fallback recursion doesn't
/// re-parse the same ancestor for each child.
pub fn resolve_and_parse_with_memo(
    inc_paths: &[PathBuf],
    module_name: &str,
    parser: &mut Parser,
    memo: &mut ParseMemo,
) -> Option<Arc<CachedModule>> {
    let mut visiting: std::collections::HashSet<String> = std::collections::HashSet::new();
    resolve_and_parse_inner(inc_paths, module_name, parser, &mut visiting, memo)
}

fn resolve_and_parse_inner(
    inc_paths: &[PathBuf],
    module_name: &str,
    parser: &mut Parser,
    visiting: &mut std::collections::HashSet<String>,
    memo: &mut ParseMemo,
) -> Option<Arc<CachedModule>> {
    if let Some(cached) = memo.get(module_name) {
        return cached.clone();
    }
    if !visiting.insert(module_name.to_string()) {
        // Cycle in `@ISA` parent fallback — bail rather than blow the stack.
        return None;
    }

    let bench = std::env::var_os("PERL_LSP_BENCH").is_some();
    let bench_start = if bench { Some(std::time::Instant::now()) } else { None };

    let path = resolve_module_path(inc_paths, module_name)?;
    let metadata = std::fs::metadata(&path).ok()?;
    if metadata.len() > 1_000_000 {
        if let Some(start) = bench_start {
            eprintln!("bench\t{}\t{}\toversize\t{}", module_name, start.elapsed().as_micros(), metadata.len());
        }
        return None;
    }
    let bytes = metadata.len();
    let source = std::fs::read_to_string(&path).ok()?;

    let timing = crate::timings::is_enabled();
    let t_parse = if timing { Some(std::time::Instant::now()) } else { None };
    let tree = parser.parse(&source, None)?;
    let parse_dur = t_parse.map(|s| s.elapsed()).unwrap_or_default();

    let t_build = if timing { Some(std::time::Instant::now()) } else { None };
    let mut analysis = crate::builder::build(&tree, source.as_bytes());
    let build_dur = t_build.map(|s| s.elapsed()).unwrap_or_default();
    crate::timings::record_built(module_name, parse_dur, build_dur);

    // If this module has no exports but inherits via @ISA (e.g. DDP → Data::Printer),
    // fall back to the first parent's exports. This only patches `export`/`export_ok`;
    // the parent's own cached analysis is still the source of truth for its symbols.
    if analysis.export.is_empty() && analysis.export_ok.is_empty() {
        let parents = crate::module_index::primary_package_parents(&analysis, module_name);
        for parent in &parents {
            if let Some(parent_cached) =
                resolve_and_parse_inner(inc_paths, parent, parser, visiting, memo)
            {
                if !parent_cached.analysis.export.is_empty()
                    || !parent_cached.analysis.export_ok.is_empty()
                {
                    analysis.export = parent_cached.analysis.export.clone();
                    analysis.export_ok = parent_cached.analysis.export_ok.clone();
                    break;
                }
            }
        }
    }

    let symbols = analysis.symbols.len();
    let result = Arc::new(CachedModule::new(path, Arc::new(analysis)));
    if let Some(start) = bench_start {
        eprintln!("bench\t{}\t{}\t{}\t{}", module_name, start.elapsed().as_micros(), symbols, bytes);
    }
    memo.insert(module_name.to_string(), Some(result.clone()));
    Some(result)
}

// ---- @INC discovery ----

pub fn discover_inc_paths() -> Vec<PathBuf> {
    let output = std::process::Command::new("perl")
        .args(["-e", r#"print join "\n", @INC"#])
        .stdin(std::process::Stdio::null())
        .output();

    match output {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .collect(),
        _ => vec![],
    }
}

/// Add project-local lib paths (lib/, local/lib/perl5/) to the front of @INC.
/// Called by the resolver thread, test resolver, and CLI tools.
pub fn add_project_lib_paths(inc_paths: &mut Vec<PathBuf>, workspace_root: &std::path::Path) {
    for local_lib in &["lib", "local/lib/perl5"] {
        let p = workspace_root.join(local_lib);
        if p.is_dir() {
            log::info!("Auto-discovered project lib: {:?}", p);
            inc_paths.insert(0, p);
        }
    }
}


/// Variant that also registers each indexed file in the given
/// `ModuleIndex` under its primary package name. Used so workspace
/// modules participate in cross-file lookups (method resolution,
/// Handler walks, etc.) without waiting for an on-demand `use`
/// resolve. Without this, `->to('Users#list')` couldn't find
/// `test_files/lib/Users.pm` because nothing ever triggers a
/// module_index populate for workspace files.
/// Does this extensionless file start with a Perl shebang
/// (`#!...perl`)? The entrypoint-script test — `jobs`, `login`,
/// Mojo::Lite apps. Peeks 64 bytes; never called on extensioned files.
fn has_perl_shebang(path: &std::path::Path) -> bool {
    if path.extension().is_some() {
        return false;
    }
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(path) else { return false };
    let mut buf = [0u8; 64];
    let Ok(n) = f.read(&mut buf) else { return false };
    let head = String::from_utf8_lossy(&buf[..n]);
    let first = head.lines().next().unwrap_or("");
    first.starts_with("#!") && first.contains("perl")
}

/// Extensionless Perl entrypoint scripts, found by a SHALLOW (depth-1)
/// shebang scan over the conventional dirs — repo root, `bin/`,
/// `script/` — plus any `extra` dirs (relative to `root`). Shallow +
/// dir-scoped on purpose: entrypoints are direct files in known
/// places, so this never walks a source tree.
///
/// `extra` is the SEAM for a future workspace-config `entrypoint_dirs`
/// knob: today every caller passes `&[]`; wiring config is one line at
/// the call site, no change here. (The config-file reader itself is
/// deliberately deferred until there's a real config story to design.)
fn scan_entrypoint_scripts(root: &std::path::Path, extra: &[String]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        vec![root.to_path_buf(), root.join("bin"), root.join("script")];
    dirs.extend(extra.iter().map(|d| root.join(d)));
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            if std::fs::metadata(&p).map(|m| m.len() < 1_000_000).unwrap_or(false)
                && has_perl_shebang(&p)
            {
                out.push(p);
            }
        }
    }
    out
}

pub fn index_workspace_with_index(
    root: &std::path::Path,
    files: &crate::file_store::FileStore,
    module_index: Option<&crate::module_index::ModuleIndex>,
    // Per-file progress tick (done, total), called from the Rayon workers as
    // files complete. LSP-agnostic: the caller owns any notification / throttle
    // policy. Invoked once per path processed (success OR skip), so `done`
    // reaches `total` at the end.
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
) -> usize {
    use ignore::types::TypesBuilder;
    use ignore::WalkBuilder;
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // Extensioned Perl (`*.pm/*.pl/*.t`) — type-pruned at the walk
    // level (cheap; never descends into a JS tree's files).
    let mut types_builder = TypesBuilder::new();
    types_builder.add("perl", "*.pm").unwrap();
    types_builder.add("perl", "*.pl").unwrap();
    types_builder.add("perl", "*.t").unwrap();
    types_builder.select("perl");
    let types = types_builder.build().unwrap();

    let mut paths: Vec<PathBuf> = WalkBuilder::new(root)
        .types(types)
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter(|e| e.metadata().map(|m| m.len() < 1_000_000).unwrap_or(false))
        .map(|e| e.into_path())
        .collect();

    // Extensionless entrypoint SCRIPTS (`#!/usr/bin/env perl` — crm's
    // `jobs`/`login`/… Mojo::Lite apps) carry no glob, so a SHALLOW
    // shebang scan over the conventional entrypoint dirs catches them
    // without enumerating the whole tree. These scripts are exactly
    // where `plugin 'X'` loads live; skipping them blinded the
    // entrypoint-scan lint and goto-def into entrypoint-defined symbols.
    // `&[]` today; the seam for a future workspace-config
    // `entrypoint_dirs` (additive to the built-in root/bin/script).
    paths.extend(scan_entrypoint_scripts(root, &[]));

    let count = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    let total = paths.len();

    // Perl workspace persistence (`docs/adr/relational-ref-index.md`,
    // phase 3): blobs + ref rows land in `modules.db` under
    // `source='workspace'` (path-keyed, like the pack tier), warm starts
    // skip re-parsing unchanged files, and — once persisted — the resident
    // copies are refs/bag-stripped like every other index tier. The cache
    // key is the hub's workspace-root spelling, the SAME one the resolver
    // thread and the hub's readers hash, so all three address one DB.
    let cache_key = module_index.and_then(|i| i.workspace_root());
    let conn = module_cache::open_cache_db(cache_key.as_deref(), "perl");
    // Validate-and-stamp the plugin fingerprint BEFORE writing: the resolver
    // thread runs the same (atomic) check concurrently on this DB, and an
    // unstamped fresh DB reads as a mismatch there — it would hard-clear the
    // rows this indexer is about to write.
    if let Some(ref conn) = conn {
        let _ = module_cache::validate_plugin_fingerprint(
            conn,
            &crate::plugin::rhai_host::plugin_fingerprint(),
        );
    }
    // Persistence and eviction are independent: blobs + rows are written
    // whenever a DB exists (the parity harness runs under PERL_LSP_NO_EVICT
    // and still needs the relational side populated); only the resident
    // STRIP obeys the eviction switch.
    let persist = conn.is_some();
    let strip = persist && eviction_enabled();

    // The walk's canonical membership set: warm rows are admitted only for
    // files the CURRENT walk still includes — a path newly gitignored (or
    // newly over the size cap) must not resurrect from its cached row, and
    // its stale generation is dropped.
    let canon_members: std::collections::HashSet<PathBuf> = paths
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect();

    // WARM: stream valid 'workspace' rows — record projections from the
    // full decode, strip, register, drop. Stale/changed rows fall through
    // to the parallel re-parse below.
    let mut warmed: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    if let Some(ref conn) = conn {
        let mut dead_rows: Vec<PathBuf> = Vec::new();
        // Backfill shreds are DEFERRED past the warm scan: writing inside
        // the streaming SELECT's transaction pins a read snapshot that a
        // concurrent resolver-thread commit turns into SQLITE_BUSY_SNAPSHOT
        // (not retried by the busy handler) — silently voiding the whole
        // backfill. Row-less files stay WHOLE this session (refs resident);
        // their rows land below for the next one.
        let mut pending_backfill: Vec<(
            PathBuf,
            Vec<crate::file_analysis::RefRowSeed>,
            Vec<crate::file_analysis::SymRowSeed>,
        )> = Vec::new();
        let rows_present = module_cache::paths_with_ref_rows(conn);
        let (_n, _stale) =
            module_cache::warm_cache_streaming(conn, "workspace", &mut |_name, path, mut fa| {
                if !canon_members.contains(&path) {
                    dead_rows.push(path);
                    return;
                }
                let path_str = path.to_string_lossy();
                // Refs strip ONLY when their rows are known present — an
                // evicted copy without rows is invisible to the backward
                // walk (rows name candidates; the blob rehydrates).
                let rows_ok = rows_present.contains(path_str.as_ref());
                if !rows_ok {
                    pending_backfill.push((
                        path.clone(),
                        fa.refs.iter().map(|r| r.row_seed()).collect(),
                        fa.sym_row_seeds(),
                    ));
                }
                if let Some(idx) = module_index {
                    idx.record_workspace_projections(&path, &fa);
                }
                // Registration-owned strip: the name/edge feeds read the
                // WHOLE analysis, then the requested axes evict, then the
                // stripped arc is stored (feeds must never see an emptied
                // `symbols`).
                let strip_bag = eviction_enabled();
                let strip_rows = strip_bag && rows_ok;
                let arc = match module_index {
                    Some(idx) => idx.register_workspace_stripping(
                        path.clone(),
                        fa,
                        strip_bag,
                        strip_rows,
                    ),
                    None => {
                        // No index (CLI-less warm): no feeds to extract —
                        // strip and store.
                        fa.evict_axes(strip_bag, strip_rows);
                        std::sync::Arc::new(fa)
                    }
                };
                files.insert_workspace_arc(path.clone(), arc);
                count.fetch_add(1, Ordering::Relaxed);
                warmed.insert(path);
            });
        module_cache::write_in_chunks(
            conn,
            &pending_backfill,
            128,
            "workspace row backfill",
            |conn, (path, seeds, sym_seeds)| {
                if let Err(e) = module_cache::shred_derived_rows(
                    conn,
                    &path.to_string_lossy(),
                    "workspace",
                    seeds,
                    sym_seeds,
                ) {
                    log::warn!("Failed to backfill derived rows for {:?}: {}", path, e);
                }
            },
        );
        for path in dead_rows {
            module_cache::invalidate_generation_tier(
                conn,
                &path.to_string_lossy(),
                "workspace",
            );
        }
    }

    // Fresh entries stream to a dedicated writer over a channel: the writer
    // persists (batched txns) WHILE workers parse, so only a small window of
    // blobs+seeds is ever in flight (never the whole tree's), and a query
    // racing the bulk index sees each file's rows as soon as its chunk
    // commits. Parse-time stamps ride along so a mid-index edit invalidates
    // the row by construction.
    // Entries whose resident copy was STRIPPED defer their residency
    // registration to the writer (post-COMMIT): an evicted copy registered
    // before its blob exists rehydrates to nothing and serves wrong-empty.
    struct WsFresh {
        path: PathBuf,
        /// The copy to mirror into the FileStore on `deferred` entries
        /// (stripped; the feed half already ran on the whole analysis).
        arc: std::sync::Arc<crate::file_analysis::FileAnalysis>,
        /// `Some` → register the residency token AFTER the chunk commits
        /// (only when an index exists; `None` covers packageless / no-index
        /// deferred entries and every persist-only whole copy).
        parts: Option<crate::module_index::WorkspaceRegistrationParts>,
        /// Register + mirror in the writer after COMMIT. `false` = the
        /// worker already registered a WHOLE copy (NO_EVICT); persist only.
        deferred: bool,
        blob: Vec<u8>,
        seeds: Vec<crate::file_analysis::RefRowSeed>,
        sym_seeds: Vec<crate::file_analysis::SymRowSeed>,
        closure: crate::file_analysis::path_intern::ClosureList,
        stamp: (i64, i64),
    }
    let (fresh_tx, fresh_rx) = std::sync::mpsc::channel::<WsFresh>();
    let timing = crate::timings::is_enabled();

    // Deliberate whole-copy accounting for the workspace-tier residency
    // tripwire — the Perl twin of the pack indexer's counter.
    let expected_whole = Arc::new(AtomicUsize::new(0));
    let expected_whole_writer = Arc::clone(&expected_whole);

    // The Connection moves INTO the writer thread (rusqlite connections are
    // Send, not Sync); nothing after the scope needs it.
    let writer_conn = conn;
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || {
            // Same failure-bounded whole-copy budget as the pack writer.
            let mut fallback_bytes = 0usize;
            run_persist_writer(
                fresh_rx,
                writer_conn.as_ref(),
                "workspace persist writer",
                |conn, batch: &[WsFresh]| {
                    for e in batch {
                        let path_str = e.path.to_string_lossy();
                        module_cache::save_blob_to_db_stamped(
                            conn, &path_str, &e.path, &e.closure, &e.blob, "workspace",
                            e.stamp,
                        );
                        if let Err(err) = module_cache::shred_derived_rows(
                            conn, &path_str, "workspace", &e.seeds, &e.sym_seeds,
                        ) {
                            log::warn!(
                                "Failed to shred derived rows for {:?}: {}",
                                e.path,
                                err
                            );
                        }
                    }
                },
                |e: WsFresh| {
                    if let Some(idx) = module_index {
                        // Clear any stale LRU pin BEFORE the stripped copy
                        // becomes reachable, so its first rehydration reads
                        // the just-committed blob.
                        idx.invalidate_bag_cache(&e.path);
                    }
                    if e.deferred {
                        files.insert_workspace_arc(e.path.clone(), e.arc.clone());
                        if let (Some(idx), Some(parts)) = (module_index, e.parts) {
                            idx.register_workspace_residency(e.path, parts);
                        }
                    }
                },
                |e: WsFresh| {
                    // The chunk never landed but the copies were stripped
                    // for it. The blob in hand IS the whole analysis —
                    // register full copies instead, so nothing is lost
                    // beyond the persistence itself (disk full / lock storm
                    // stays loud AND self-heals) — up to the budget.
                    if let Some(idx) = module_index {
                        idx.invalidate_bag_cache(&e.path);
                    }
                    if let Some(fa) = module_cache::decode_analysis(&e.blob) {
                        let bytes = fa.heap_estimate().total();
                        if fallback_bytes.saturating_add(bytes) > FALLBACK_WHOLE_BYTE_CAP {
                            // Over budget: DROP (the chunk didn't commit, so a
                            // stripped copy has no blob to rehydrate from —
                            // honest absence, re-indexed next run).
                            log::warn!(
                                "workspace persist writer: fallback budget ({} MiB) exceeded — \
                                 dropping resident copy for {:?}; re-indexes next run",
                                FALLBACK_WHOLE_BYTE_CAP / (1024 * 1024),
                                e.path,
                            );
                            return;
                        }
                        fallback_bytes += bytes;
                        let arc = std::sync::Arc::new(fa);
                        files.insert_workspace_arc(e.path.clone(), arc.clone());
                        if let Some(idx) = module_index {
                            let _ = idx.register_workspace_resident(e.path.clone(), arc);
                            // A deliberate whole pin — account it so the
                            // tripwire flags only UNEXPLAINED residents.
                            expected_whole_writer.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                },
            );
        });

        // Force the plugin registry — plugin load AND pattern/flow query
        // compilation — to initialize once, single-threaded, before
        // the parallel build below. Otherwise the first `build()` to trigger
        // the registry's OnceLock stalls every other Rayon worker on it,
        // charging ~1s of one-time compile to whichever files happen to block.
        let _ = crate::plugin::default_plugin_registry();

        paths.par_iter().for_each(|path| {
            // Blobs are keyed canonical (matches the warm rows + the CLI's
            // canonicalized origin staging); register under the same spelling
            // so cold and warm runs key the stores identically.
            let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
            if warmed.contains(&canon) {
                if let Some(cb) = progress {
                    let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                    cb(d, total);
                }
                return;
            }
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                analyze_stamped(path, || {
                    let source = std::fs::read_to_string(path).ok()?;
                    let mut parser = create_parser();
                    let t_parse = if timing { Some(std::time::Instant::now()) } else { None };
                    let tree = parser.parse(&source, None)?;
                    let parse_dur = t_parse.map(|s| s.elapsed()).unwrap_or_default();
                    let t_build = if timing { Some(std::time::Instant::now()) } else { None };
                    let analysis = crate::builder::build(&tree, source.as_bytes());
                    let build_dur = t_build.map(|s| s.elapsed()).unwrap_or_default();
                    if timing {
                        crate::timings::record_built(
                            path.strip_prefix(root).unwrap_or(path).display().to_string(),
                            parse_dur,
                            build_dur,
                        );
                    }
                    Some(analysis)
                })
            }));

            match result {
                // `analyze_stamped` returning None covers BOTH a failed
                // read/parse and a changed-under-us stamp — the watcher (or
                // next warm) owns the fresher truth either way.
                Ok(Some((mut analysis, stamp))) => {
                    // Projections that read the bag run on the whole
                    // analysis; the persisted generation is encoded whole;
                    // only then is the resident copy stripped.
                    if let Some(idx) = module_index {
                        idx.record_workspace_projections(&canon, &analysis);
                    }
                    let payload = if persist && !analysis.degraded {
                        module_cache::encode_analysis(&analysis).map(|blob| {
                            let seeds: Vec<_> =
                                analysis.refs.iter().map(|r| r.row_seed()).collect();
                            let sym_seeds = analysis.sym_row_seeds();
                            let closure = analysis.include_closure.clone();
                            (blob, seeds, sym_seeds, closure)
                        })
                    } else {
                        None
                    };
                    if strip && payload.is_some() {
                        // Stripped copy: feed half now (whole analysis),
                        // residency + FileStore mirror in the writer AFTER
                        // its chunk commits. Until then the file reads as
                        // "not yet indexed" — never wrong-empty.
                        let (arc, parts) = match module_index {
                            Some(idx) => {
                                let parts =
                                    idx.prepare_workspace_parts(analysis, true, true);
                                parts.record_surface(idx, &canon);
                                (std::sync::Arc::clone(parts.arc()), Some(parts))
                            }
                            None => {
                                analysis.evict_axes(true, true);
                                (std::sync::Arc::new(analysis), None)
                            }
                        };
                        let (blob, seeds, sym_seeds, closure) = payload.unwrap();
                        let _ = fresh_tx.send(WsFresh {
                            path: canon.clone(),
                            arc,
                            parts,
                            deferred: true,
                            blob,
                            seeds,
                            sym_seeds,
                            closure,
                            stamp,
                        });
                    } else {
                        // Whole copy (no persistence, degraded, or NO_EVICT):
                        // register immediately (no strip — the whole-copy door);
                        // still persist when a blob exists. `false, false` mints
                        // the token without evicting or recording surface, so
                        // this path's freshness behavior is unchanged.
                        let arc = match module_index {
                            Some(idx) => {
                                let parts = idx.prepare_workspace_parts(analysis, false, false);
                                let arc = std::sync::Arc::clone(parts.arc());
                                idx.register_workspace_residency(canon.clone(), parts);
                                // Deliberate whole pin (unpersistable /
                                // degraded / NO_EVICT) — accounted for the
                                // tripwire.
                                expected_whole.fetch_add(1, Ordering::Relaxed);
                                arc
                            }
                            None => std::sync::Arc::new(analysis),
                        };
                        if let Some((blob, seeds, sym_seeds, closure)) = payload {
                            let _ = fresh_tx.send(WsFresh {
                                path: canon.clone(),
                                arc: arc.clone(),
                                parts: None,
                                deferred: false,
                                blob,
                                seeds,
                                sym_seeds,
                                closure,
                                stamp,
                            });
                        }
                        files.insert_workspace_arc(canon.clone(), arc);
                    }
                    count.fetch_add(1, Ordering::Relaxed);
                }
                Ok(None) => { /* parse failed, skip */ }
                Err(_) => {
                    log::warn!("Panic while indexing {:?}, skipping", path);
                }
            }
            if let Some(cb) = progress {
                let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                cb(d, total);
            }
        });

        drop(fresh_tx);
        let _ = writer.join();
    });

    // Workspace-tier residency tripwire, mirroring the pack indexer's:
    // gated off under NO_EVICT (everything is deliberately whole there).
    if let Some(idx) = module_index {
        if eviction_enabled() {
            residency_tripwire(
                "workspace",
                idx.count_fully_resident(),
                expected_whole.load(Ordering::Relaxed),
            );
        }
    }

    count.load(Ordering::Relaxed)
}

/// Index pack-language files (C++/Python/…) into per-language sub-indexes
/// attached to `hub`. GENERIC: registry-driven, so every served pack
/// language gets cross-file from this one walk. Each language keeps its
/// OWN `ModuleIndex` (separate cache — names never comingle across
/// languages), files registered by CLASS name. PERSISTED to a separate
/// `modules-{lang}.db`: warm valid analyses from disk (mtime/size +
/// EXTRACT_VERSION validated), re-analyze only new/changed/stale files,
/// and write the fresh ones back — so a big monorepo doesn't re-analyze
/// every header each launch. `cache_key` is the workspace root the cache
/// dir hashes on (`None` ⇒ no persistence, e.g. tests).
/// Slice-2 eviction off-switch: `PERL_LSP_NO_EVICT` keeps every resident pack
/// bag in memory (the pre-Slice-2 footprint) — an emergency knob and the A/B
/// lever for isolating an eviction-caused regression.
pub(crate) fn eviction_enabled() -> bool {
    std::env::var_os("PERL_LSP_NO_EVICT").is_none()
}

/// `PERL_LSP_STRICT_RESIDENCY=1`: residency invariant breaks (an evicted
/// copy that can't rehydrate, a tripwire overrun) PANIC instead of
/// degrading. The gold harness sets it so a session serving
/// absence-as-answer dies as a CRASH row (hard fail) rather than scoring
/// wrong answers — the cold-flake net. Off by default: a live server
/// prefers degraded-but-useful.
pub(crate) fn strict_residency() -> bool {
    std::env::var_os("PERL_LSP_STRICT_RESIDENCY").is_some_and(|v| v != "0")
}

/// The post-bulk-index residency check, one speller for the pack tier and
/// the Perl workspace tier: fully-resident registered copies beyond the
/// deliberately-accounted ones (writer fallbacks, degraded/unpersisted
/// analyses) mean a registration path is silently pinning whole analyses —
/// the RAM regression no functional test can see. `debug_assert` catches it
/// in `cargo test`; strict mode makes it fatal in release (the gold net).
fn residency_tripwire(tier: &str, whole: usize, expected: usize) {
    if whole <= expected {
        return;
    }
    log::error!(
        "residency tripwire ({tier}): {whole} fully-resident copies, only \
         {expected} accounted (writer fallbacks / degraded) — a registration \
         path is pinning whole analyses"
    );
    debug_assert!(
        false,
        "residency tripwire ({tier}): {whole} fully-resident > {expected} accounted"
    );
    if strict_residency() {
        panic!(
            "PERL_LSP_STRICT_RESIDENCY: residency tripwire ({tier}): {whole} \
             fully-resident copies, only {expected} accounted"
        );
    }
}

/// Persist one module's generation: blob + its relational ref rows, always
/// together (`docs/adr/relational-ref-index.md` — rows and blob describe the
/// same analysis or neither exists). `save_to_db` skips degraded analyses;
/// mirror that here so no rows exist for an unpersisted blob.
/// Returns whether the blob row landed (the strip-legality signal).
fn save_module_generation(
    conn: &rusqlite::Connection,
    module_name: &str,
    result: &Option<Arc<CachedModule>>,
) -> bool {
    if let Some(m) = result {
        // A bag-evicted copy IS the already-persisted generation —
        // re-encoding it would overwrite the good blob with a bagless one.
        if m.analysis.bag_is_evicted() {
            return true;
        }
    }
    let persisted = module_cache::save_to_db(conn, module_name, result, "import");
    if !persisted {
        // Blob didn't land (busy/encode failure): shredding rows now would
        // pair a NEW generation's rows with an OLD (or absent) blob —
        // "blob + rows describe one generation" is the write invariant.
        return false;
    }
    if let Some(m) = result {
        if !m.analysis.degraded {
            let seeds: Vec<_> = m.analysis.refs.iter().map(|r| r.row_seed()).collect();
            let sym_seeds = m.analysis.sym_row_seeds();
            if let Err(e) = module_cache::shred_derived_rows(
                conn,
                &m.path.to_string_lossy(),
                "import",
                &seeds,
                &sym_seeds,
            ) {
                log::warn!("Failed to shred derived rows for '{}': {}", module_name, e);
            }
        }
    }
    persisted
}

pub fn index_pack_languages(
    root: &std::path::Path,
    cache_key: Option<&str>,
    hub: &crate::module_index::ModuleIndex,
    // Per-file progress tick (done, grand_total) across ALL pack languages, so
    // the single pack token's percentage is monotone. Called once per path
    // (warm-skip OR analyzed) — `done` reaches the grand total at the end.
    progress: Option<&(dyn Fn(usize, usize) + Sync)>,
    // Slice-2 rehydration LRU byte cap (`maxCacheMb * 1 MiB`). The resident
    // pack analyses are bag-stripped after indexing; a type query into an
    // evicted file rehydrates its exact bag from SQLite into this cap. `0`
    // disables retention (rehydrate-and-drop). See `docs/adr/memory-slice-2-lru.md`.
    bag_cache_bytes: usize,
) -> usize {
    use ignore::types::TypesBuilder;
    use ignore::WalkBuilder;
    use rayon::prelude::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // Persist the transitive macro table across sessions (kills the
    // cold-start gather over perl.h's closure) — pointed at this workspace's
    // cache dir.
    crate::cpp_reparse::set_macro_persist_dir(module_cache::cache_dir_for_workspace(cache_key));

    let reg = crate::language_driver::LanguageRegistry::with_enabled();

    // Collect every language's paths UP FRONT so the grand total (the progress
    // denominator) is known before any file is analyzed — a single monotone
    // 0→100% stream across all pack languages on the one shared token.
    let mut lang_paths: Vec<(&'static str, Vec<PathBuf>)> = Vec::new();
    for lang in reg.languages() {
        if lang == "perl" {
            continue;
        }
        let exts: Vec<&'static str> = reg
            .for_id(lang)
            .map(|d| d.extensions().to_vec())
            .unwrap_or_default();
        if exts.is_empty() {
            continue;
        }
        let mut tb = TypesBuilder::new();
        for ext in &exts {
            let _ = tb.add(lang, &format!("*.{ext}"));
        }
        let _ = tb.select(lang);
        let Ok(types) = tb.build() else { continue };
        let paths: Vec<PathBuf> = WalkBuilder::new(root)
            .types(types)
            .build()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
            .filter(|e| e.metadata().map(|m| m.len() < 2_000_000).unwrap_or(false))
            .map(|e| e.into_path())
            .collect();
        if paths.is_empty() {
            continue;
        }
        lang_paths.push((lang, paths));
    }
    let grand_total: usize = lang_paths.iter().map(|(_, p)| p.len()).sum();

    let total = AtomicUsize::new(0);
    let done = AtomicUsize::new(0);
    for (lang, paths) in lang_paths {
        // Slice-2 bag-rehydration LRU: a loader that opens THIS lang's SQLite
        // conn on demand (rusqlite `Connection` isn't `Sync`, so we open per
        // rehydration miss — rare, and SQLite handles concurrent readers) and
        // decodes the one requested file's full bag.
        let bag_cache = {
            let cache_key_owned = cache_key.map(|s| s.to_string());
            let loader = move |path: &std::path::Path| {
                // The blob is persisted under the CANONICAL path (both feed
                // paths write `canon`), while the resident copy may be
                // registered under the walk's raw path — canonicalize so the
                // keyed decode matches regardless of which form the caller holds.
                // The discriminated helper survives the readonly-open
                // CANTOPEN/WAL race and names every other miss cause.
                let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                let mut spellings = vec![canon.to_string_lossy().into_owned()];
                let raw = path.to_string_lossy().into_owned();
                if raw != spellings[0] {
                    spellings.push(raw);
                }
                module_cache::open_and_load_diag(cache_key_owned.as_deref(), lang, &spellings)
            };
            Arc::new(crate::pack_bag_cache::PackBagCache::new(bag_cache_bytes, loader))
        };
        let pack_index = Arc::new(
            crate::module_index::ModuleIndex::new_for_cli().with_bag_cache(bag_cache),
        );
        // This sub-index's relational-ref-index reader — same per-language DB
        // the drain below writes blobs + rows into.
        {
            let cache_key_owned = cache_key.map(|s| s.to_string());
            pack_index.set_ref_rows_opener(Arc::new(move || {
                module_cache::open_cache_db_readonly(cache_key_owned.as_deref(), lang)
            }));
        }
        let conn = module_cache::open_cache_db(cache_key, lang);
        // A generation built under different analysis inputs (toolchain
        // change — or its probe FAILURE, which empties the system include
        // roots) must not be warmed: hard-clear, same as `validate_inc_paths`.
        if let (Some(ref conn), Some(driver)) = (&conn, reg.for_id(lang)) {
            let _ = module_cache::validate_input_fingerprint(
                conn,
                driver.analysis_input_fingerprint(),
            );
        }

        // WARM: stream valid cached analyses (keyed by file path) one row
        // at a time — register a stripped copy, drop the whole decode before
        // the next row, so at most one full analysis is transiently
        // resident. Version-stale rows re-analyze; rows for files the
        // CURRENT walk no longer includes are dropped, not resurrected.
        let canon_members: std::collections::HashSet<PathBuf> = paths
            .iter()
            .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
            .collect();
        let mut warmed: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        if let Some(ref conn) = conn {
            module_cache::validate_stub_version(conn);
            let mut dead_rows: Vec<PathBuf> = Vec::new();
            // Deferred past the warm scan — same SQLITE_BUSY_SNAPSHOT
            // rationale as the workspace indexer's backfill.
            let mut pending_backfill: Vec<(
                PathBuf,
                Vec<crate::file_analysis::RefRowSeed>,
                Vec<crate::file_analysis::SymRowSeed>,
            )> = Vec::new();
            // Stubs whose files warmed through the FULL path this scan —
            // written after it so the next warm takes the stub lane.
            let mut pending_stubs: Vec<(PathBuf, Vec<u8>, (i64, i64))> = Vec::new();
            let rows_present = module_cache::paths_with_ref_rows(conn);
            // A stub's skeleton is stripped by construction; under NO_EVICT
            // the resident copies must stay whole, so stubs are bypassed.
            let use_stubs = eviction_enabled();
            let _n = module_cache::warm_pack_stream_with_stubs(
                conn,
                use_stubs,
                // Dead rows (files the current walk no longer includes) are
                // rejected before any stub/blob bytes are read; stamp-stale
                // dead rows GC too.
                &mut |path| {
                    if canon_members.contains(path) {
                        return true;
                    }
                    dead_rows.push(path.to_path_buf());
                    false
                },
                &mut |path, payload| {
                    use module_cache::{WarmDirective, WarmPayload};
                    let path_str = path.to_string_lossy().into_owned();
                    // Refs strip only when their rows are known present — rows
                    // name candidates for the backward walk; the blob rehydrates.
                    let rows_ok = rows_present.contains(path_str.as_str());
                    let fa = match payload {
                        WarmPayload::Stub(stub) => {
                            if !rows_ok {
                                // Rows missing (REF_ROWS_VERSION wipe): the
                                // re-shred needs the full analysis.
                                return WarmDirective::NeedFull;
                            }
                            // The stub IS a persisted `prepare_pack_parts`
                            // output — rehydrate the token, register through it.
                            let parts =
                                crate::module_index::PackRegistrationParts::from_warm_stub(stub);
                            parts.record_surface(&pack_index, &path);
                            pack_index.register_symbols_inner(path.clone(), parts);
                            warmed.insert(path);
                            return WarmDirective::Handled;
                        }
                        WarmPayload::Full(_name, fa) => fa,
                    };
                    if !rows_ok {
                        pending_backfill.push((
                            path.clone(),
                            fa.refs.iter().map(|r| r.row_seed()).collect(),
                            fa.sym_row_seeds(),
                        ));
                    }
                    let strip_bag = eviction_enabled();
                    let fully_stripped = strip_bag && rows_ok;
                    let parts = crate::module_index::ModuleIndex::prepare_pack_parts(
                        fa,
                        strip_bag,
                        fully_stripped,
                    );
                    if fully_stripped {
                        if let Some(blob) = module_cache::encode_stub(
                            parts.feed(),
                            parts.specs(),
                            parts.surface(),
                            parts.arc(),
                        ) {
                            let stamp = module_cache::file_stamp(&path).unwrap_or((0, 0));
                            pending_stubs.push((path.clone(), blob, stamp));
                        }
                    }
                    parts.record_surface(&pack_index, &path);
                    pack_index.register_symbols_inner(path.clone(), parts);
                    warmed.insert(path);
                    WarmDirective::Handled
                },
            );
            module_cache::write_in_chunks(
                conn,
                &pending_stubs,
                256,
                "pack stub backfill",
                |conn, (path, blob, stamp)| {
                    module_cache::save_stub_if_current(
                        conn,
                        &path.to_string_lossy(),
                        blob,
                        *stamp,
                    );
                },
            );
            module_cache::write_in_chunks(
                conn,
                &pending_backfill,
                128,
                "pack row backfill",
                |conn, (path, seeds, sym_seeds)| {
                    if let Err(e) = module_cache::shred_derived_rows(
                        conn,
                        &path.to_string_lossy(),
                        "workspace",
                        seeds,
                        sym_seeds,
                    ) {
                        log::warn!("Failed to backfill derived rows for {:?}: {}", path, e);
                    }
                },
            );
            for path in dead_rows {
                module_cache::invalidate_generation_tier(
                    conn,
                    &path.to_string_lossy(),
                    "workspace",
                );
            }
        }

        // Analyze only the new/changed/stale files (parallel). Fresh entries
        // stream to a dedicated writer thread over a channel: blobs + rows
        // land in batched txns WHILE workers analyze, so only a bounded
        // window of encoded blobs is in flight and a query racing the bulk
        // index sees each file's rows as soon as its chunk commits.
        // Persistence and eviction are independent: blobs + rows are written
        // whenever a DB exists; only the resident STRIP obeys the eviction
        // switch (the bag/refs are stripped only when recoverable — persisted
        // and non-degraded).
        // Stripped fresh entries defer registration to the writer (post-
        // COMMIT) — same rationale as the workspace indexer's WsFresh. The
        // feed rides along (computed pre-strip); `deferred: false` means the
        // worker registered a whole copy (NO_EVICT) and the writer only
        // persists.
        struct FreshEntry {
            path: PathBuf,
            // For persistence (`include_closure`) — always present. For a
            // deferred entry this is the same arc the token carries.
            arc: Arc<crate::file_analysis::FileAnalysis>,
            // `Some` → register the token AFTER the chunk commits (stripped
            // copies). `None` → the worker already registered a whole copy
            // (NO_EVICT/degraded); the writer only persists.
            parts: Option<crate::module_index::PackRegistrationParts>,
            blob: Vec<u8>,
            // Warm stub (deferred/stripped entries only) — persisted in the
            // same chunk txn as the blob so the next warm start registers
            // from it without decoding `blob`.
            stub_blob: Option<Vec<u8>>,
            seeds: Vec<crate::file_analysis::RefRowSeed>,
            sym_seeds: Vec<crate::file_analysis::SymRowSeed>,
            stamp: (i64, i64),
        }
        let (fresh_tx, fresh_rx) = std::sync::mpsc::channel::<FreshEntry>();
        let persist = conn.is_some();
        let strip = persist && eviction_enabled();
        // Every DELIBERATE whole-copy registration under strip increments
        // this; the post-index tripwire flags any fully-resident copy it
        // can't account for (a silent RAM pin no functional test sees).
        let expected_whole = Arc::new(AtomicUsize::new(0));
        let writer_conn = conn;
        let pack_index_writer = Arc::clone(&pack_index);
        let expected_whole_writer = Arc::clone(&expected_whole);
        std::thread::scope(|scope| {
            let writer = scope.spawn(move || {
                // Byte budget for the whole copies a failed chunk retains
                // (see FALLBACK_WHOLE_BYTE_CAP). Per-writer accumulator — the
                // fallback lane is single-threaded (this writer thread).
                let mut fallback_bytes = 0usize;
                run_persist_writer(
                    fresh_rx,
                    writer_conn.as_ref(),
                    "pack persist writer",
                    |conn, batch: &[FreshEntry]| {
                        // Chunk-scoped: a concurrent different-generation
                        // process may wipe/restamp the stubs table mid-run.
                        let stubs_writable = module_cache::stub_version_current(conn);
                        for e in batch {
                            let path_str = e.path.to_string_lossy();
                            module_cache::save_blob_to_db_stamped(
                                conn,
                                &path_str,
                                &e.path,
                                &e.arc.include_closure,
                                &e.blob,
                                "workspace",
                                e.stamp,
                            );
                            if let Err(err) = module_cache::shred_derived_rows(
                                conn, &path_str, "workspace", &e.seeds, &e.sym_seeds,
                            ) {
                                log::warn!(
                                    "Failed to shred derived rows for {:?}: {}",
                                    e.path,
                                    err
                                );
                            }
                            if let Some(sb) = &e.stub_blob {
                                if stubs_writable {
                                    module_cache::save_stub(conn, &path_str, sb);
                                }
                            }
                        }
                    },
                    |e: FreshEntry| {
                        // Stale-pin clear BEFORE the stripped copy is
                        // reachable, so its first rehydration reads the
                        // just-committed blob.
                        pack_index_writer.invalidate_bag_cache(&e.path);
                        if let Some(parts) = e.parts {
                            pack_index_writer.register_symbols_inner(e.path, parts);
                        }
                    },
                    |e: FreshEntry| {
                        pack_index_writer.invalidate_bag_cache(&e.path);
                        if let Some(fa) = module_cache::decode_analysis(&e.blob) {
                            let bytes = fa.heap_estimate().total();
                            if fallback_bytes.saturating_add(bytes) <= FALLBACK_WHOLE_BYTE_CAP {
                                fallback_bytes += bytes;
                                // Tripwire-accounted: this whole copy is a
                                // DELIBERATE (failure-bounded) pin.
                                expected_whole_writer.fetch_add(1, Ordering::Relaxed);
                                pack_index_writer.register_symbols(e.path, Arc::new(fa));
                            } else {
                                // Over budget: DROP the resident copy. The
                                // chunk didn't commit, so a stripped copy
                                // would rehydrate to wrong-empty; leaving it
                                // unregistered is honest absence that the next
                                // index/warm re-registers.
                                log::warn!(
                                    "pack persist writer: fallback budget ({} MiB) exceeded — \
                                     dropping resident copy for {:?}; re-indexes next run",
                                    FALLBACK_WHOLE_BYTE_CAP / (1024 * 1024),
                                    e.path,
                                );
                            }
                        }
                    },
                );
            });

            paths.par_iter().for_each(|path| {
                // Tick before any early-out so warm-cache skips also advance the
                // bar — `done` must reach `grand_total`.
                if let Some(cb) = progress {
                    let d = done.fetch_add(1, Ordering::Relaxed) + 1;
                    cb(d, grand_total);
                }
                let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
                if warmed.contains(&canon) {
                    return; // valid cache hit
                }
                let reg = crate::language_driver::LanguageRegistry::with_enabled();
                let Some(driver) = reg.for_path(path).filter(|d| d.id() == lang) else { return };
                let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    analyze_stamped(path, || {
                        let source = std::fs::read_to_string(path).ok()?;
                        Some(driver.analyze_with_path(&source, Some(path)))
                    })
                }));
                if let Ok(Some((analysis, stamp))) = res {
                    // Encode the FULL analysis for the disk write, then strip
                    // the resident copy — one struct, no clone
                    // (`docs/adr/memory-slice-2-lru.md`). Strip only when the
                    // bag/refs are recoverable: persisted and non-degraded
                    // (`save_*` skip degraded rows, so their bag would be lost).
                    let payload = if persist && !analysis.degraded {
                        module_cache::encode_analysis(&analysis).map(|blob| {
                            let seeds: Vec<_> =
                                analysis.refs.iter().map(|r| r.row_seed()).collect();
                            let sym_seeds = analysis.sym_row_seeds();
                            (blob, seeds, sym_seeds)
                        })
                    } else {
                        None
                    };
                    if strip && payload.is_some() {
                        // Stripped copy: mint the token pre-strip, hand it to
                        // the writer — it registers after the chunk COMMITS,
                        // so an evicted copy is never reachable before its blob
                        // can rehydrate it.
                        let parts = crate::module_index::ModuleIndex::prepare_pack_parts(
                            analysis, true, true,
                        );
                        let stub_blob = module_cache::encode_stub(
                            parts.feed(),
                            parts.specs(),
                            parts.surface(),
                            parts.arc(),
                        );
                        // Recording before the writer's COMMIT is safe — the
                        // freshness index is session-local.
                        parts.record_surface(&pack_index, &canon);
                        let (blob, seeds, sym_seeds) = payload.unwrap();
                        let arc = Arc::clone(parts.arc());
                        let _ = fresh_tx.send(FreshEntry {
                            path: canon.clone(),
                            arc,
                            parts: Some(parts),
                            blob,
                            stub_blob,
                            seeds,
                            sym_seeds,
                            stamp,
                        });
                    } else {
                        // Whole copy: degraded / encode-failed / NO_EVICT.
                        if strip {
                            expected_whole.fetch_add(1, Ordering::Relaxed);
                        }
                        let arc = Arc::new(analysis);
                        pack_index.register_symbols(path.clone(), arc.clone());
                        if let Some((blob, seeds, sym_seeds)) = payload {
                            let _ = fresh_tx.send(FreshEntry {
                                path: canon.clone(),
                                arc,
                                parts: None,
                                blob,
                                stub_blob: None,
                                seeds,
                                sym_seeds,
                                stamp,
                            });
                        }
                    }
                    total.fetch_add(1, Ordering::Relaxed);
                    // Residency: this file's merged/expanded macro tables are a
                    // one-shot build input, now dead weight for the rest of the
                    // bulk index (they'd otherwise accumulate to ~1.6 GB of
                    // per-file duplicates on abseil). Drop them the moment the
                    // analysis is built; the shared `header_cache` stays warm so
                    // an on-edit re-gather is a header-BFS, not a cold gather.
                    // Keyed by the same path analyze got, plus its canonical form.
                    let mut drop_set = std::collections::HashSet::with_capacity(2);
                    drop_set.insert(path.clone());
                    drop_set.insert(canon);
                    crate::cpp_reparse::evict_gather_caches_keep_headers(&drop_set);
                }
            });

            drop(fresh_tx);
            let _ = writer.join();
        });
        if strip {
            residency_tripwire(
                &lang.to_string(),
                pack_index.count_fully_resident(),
                expected_whole.load(Ordering::Relaxed),
            );
        }
        hub.attach_pack_index(lang, pack_index);
    }
    if std::env::var_os("PERL_LSP_MEM_REPORT").is_some() {
        eprintln!("[mem-report] {}", crate::cpp_reparse::cache_size_report());
    }
    // Heap-composition of the resident pack `FileAnalysis` set — the Slice-2
    // eviction target (`docs/adr/memory-slice-2-lru.md`). Env-gated, inert by
    // default, no query-path cost.
    if std::env::var_os("PERL_LSP_HEAP_DUMP").is_some() {
        let mut agg = crate::file_analysis::HeapBreakdown::default();
        hub.for_each_pack_registered_file(&mut |_path, fa| agg.add(&fa.heap_estimate()));
        eprintln!("[heap-dump] {agg}");
        let (paths, bytes) = crate::file_analysis::path_intern::table_stats();
        eprintln!(
            "[heap-dump] path-id table (process-wide, counted once): {} paths, {:.1} MB",
            paths,
            bytes as f64 / (1024.0 * 1024.0)
        );
    }
    total.load(Ordering::Relaxed)
}

/// Stamp-before-read + re-stat-after-parse: capture the disk stamp, run the
/// read+analyze, and return None when the file changed underneath — a
/// write-time stamp would bless a stale parse as the current generation and
/// every future warm would serve it as valid. Both fresh workers route
/// their changed-under-us protocol through here.
fn analyze_stamped<T>(
    path: &std::path::Path,
    f: impl FnOnce() -> Option<T>,
) -> Option<(T, (i64, i64))> {
    let stamp = module_cache::file_stamp(path).unwrap_or((0, 0));
    let out = f()?;
    if module_cache::file_stamp(path) != Some(stamp) {
        return None;
    }
    Some((out, stamp))
}

/// Byte budget for whole `FileAnalysis` copies the persist writer retains
/// when a chunk fails to commit (disk full) or panics. The strip is licensed
/// only by a landed blob, so a fallback keeps copies WHOLE — and a
/// persistently failing writer would otherwise pin the ENTIRE tree resident
/// (the docs/forks-resolved.md "writer fallback budget" entry). Past the cap
/// we DROP the resident copy rather than register a stripped one: the chunk
/// didn't commit, so a stripped copy's blob isn't on disk and could only
/// rehydrate to wrong-empty. Dropping is honest absence — the file reads as
/// "not indexed this session" and the next index/warm re-registers it; it
/// never serves wrong data and never leaves an evicted copy unrehydratable
/// (nothing is evicted — nothing is registered). Byte-accounted like the
/// enrichment overlay (`ENRICHED_BYTE_CAP`); 128 MiB per writer thread — a
/// transient failure degrades gracefully, a permanent one can't OOM.
const FALLBACK_WHOLE_BYTE_CAP: usize = 128 * 1024 * 1024;

/// The persist-writer harness both bulk indexers share: batches entries off
/// the channel (≤128 per txn), owns BEGIN IMMEDIATE / COMMIT / ROLLBACK
/// (IMMEDIATE — a deferred txn that reads before writing can hit an
/// unretryable SQLITE_BUSY_SNAPSHOT against a concurrent writer), and hands
/// every entry to exactly one of `on_committed` (deferred registration) or
/// `on_fallback` (commit failure OR chunk panic — the whole-copy self-heal;
/// a panic must not kill the writer, workers keep stripping copies whose
/// sends would silently fail). With no Connection the channel drains
/// unregistered. Registration runs inside the panic guard, mirroring the
/// txn: entries a mid-batch registration panic leaves behind take the
/// fallback lane instead of vanishing.
fn run_persist_writer<E>(
    rx: std::sync::mpsc::Receiver<E>,
    conn: Option<&rusqlite::Connection>,
    label: &str,
    write_batch: impl Fn(&rusqlite::Connection, &[E]),
    mut on_committed: impl FnMut(E),
    mut on_fallback: impl FnMut(E),
) {
    let Some(conn) = conn else {
        while rx.recv().is_ok() {}
        return;
    };
    let mut batch: Vec<E> = Vec::new();
    let mut process = |batch: &mut Vec<E>| {
        if batch.is_empty() {
            return;
        }
        let n = batch.len();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let txn_open = conn.execute_batch("BEGIN IMMEDIATE").is_ok();
            write_batch(conn, batch);
            let committed = txn_open
                && match conn.execute_batch("COMMIT") {
                    Ok(()) => true,
                    Err(err) => {
                        let _ = conn.execute_batch("ROLLBACK");
                        log::error!(
                            "{label}: commit failed ({n} files, registering whole copies): {err}"
                        );
                        false
                    }
                };
            if committed {
                for e in batch.drain(..) {
                    on_committed(e);
                }
            } else {
                for e in batch.drain(..) {
                    on_fallback(e);
                }
            }
        }));
        if r.is_err() {
            // A panic can leave the txn open; roll back defensively so the
            // NEXT chunk's BEGIN isn't poisoned.
            let _ = conn.execute_batch("ROLLBACK");
            log::error!("{label}: chunk panicked ({n} files) — registering whole copies");
            for e in batch.drain(..) {
                on_fallback(e);
            }
        }
    };
    while let Ok(entry) = rx.recv() {
        batch.push(entry);
        while batch.len() < 128 {
            match rx.try_recv() {
                Ok(e) => batch.push(e),
                Err(_) => break,
            }
        }
        process(&mut batch);
    }
    process(&mut batch);
}

/// Coordinates watcher invalidations against the INITIAL pack bulk index
/// (`index_pack_languages`) — H9-2. While that index is in flight the pack
/// sub-indexes aren't attached to the hub yet, so a `pack_file_changed` would
/// find no `pack_index` and silently drop the save (and even once attached,
/// racing the bulk cone re-analyzes it twice, uncoordinated). Instead a save
/// arriving during the index is DEFERRED into a bounded set (one entry per
/// distinct path changed during the index) and reconciled ONCE at completion:
/// the caller re-runs `pack_file_changed` per deferred path against current
/// disk, and the H9-1 source-generation guard makes that safe — the reconcile
/// reads the freshest bytes (highest generation) and outranks whatever the
/// bulk pass registered.
#[derive(Default)]
pub struct PackChangeCoordinator {
    in_flight: std::sync::atomic::AtomicBool,
    // path -> deleted. A HashMap so repeated saves of one path collapse to a
    // single reconcile (the reconcile reads current disk regardless).
    deferred: std::sync::Mutex<std::collections::HashMap<PathBuf, bool>>,
}

impl PackChangeCoordinator {
    /// Mark the initial pack index in flight. Call synchronously before the
    /// index is scheduled so a save racing the scheduling is still deferred.
    pub fn begin_index(&self) {
        self.in_flight
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Record a watched-file change. Returns `true` when the caller should
    /// DEFER (the index is in flight → the change is queued for reconcile);
    /// `false` when it should run `pack_file_changed` now. The flag check and
    /// the queue insert are one critical section with `finish_index`'s clear +
    /// drain, so a save can never be both dropped from the queue AND skipped by
    /// the normal path.
    pub fn note_change(&self, canon: &std::path::Path, deleted: bool) -> bool {
        let mut q = self.deferred.lock().unwrap_or_else(|e| e.into_inner());
        if self.in_flight.load(std::sync::atomic::Ordering::Relaxed) {
            q.insert(canon.to_path_buf(), deleted);
            true
        } else {
            false
        }
    }

    /// Clear the in-flight flag and drain the deferred set, atomically w.r.t.
    /// `note_change`. The returned pairs are the paths to reconcile once.
    pub fn finish_index(&self) -> Vec<(PathBuf, bool)> {
        let mut q = self.deferred.lock().unwrap_or_else(|e| e.into_inner());
        self.in_flight
            .store(false, std::sync::atomic::Ordering::Relaxed);
        q.drain().collect()
    }

    #[cfg(test)]
    pub fn is_in_flight(&self) -> bool {
        self.in_flight.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// In-session invalidation for a changed (saved/watched) or deleted pack
/// file — the H1 seam. The include closure is the cross-file visibility
/// key, so it is also the REVERSE-dependency key: a consumer is any
/// registered file whose `include_closure` contains the changed path.
/// Order matters: evict the per-file analysis caches FIRST (macro tables,
/// pre-expanded variants, closures) so the re-analyses here — and the
/// open documents' background refresh after — re-gather instead of
/// serving the frozen tables. Blocking (Rayon inside); callers run it
/// off the message loop.
pub fn pack_file_changed(
    root_uri: Option<&str>,
    hub: &crate::module_index::ModuleIndex,
    path: &std::path::Path,
    deleted: bool,
) {
    use rayon::prelude::*;
    use std::sync::Arc;
    let reg = crate::language_driver::LanguageRegistry::with_enabled();
    let Some(driver) = reg.for_path(path) else { return };
    if driver.id() == "perl" {
        return;
    }
    let lang = driver.id();
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let canon_str = canon.to_string_lossy().into_owned();
    let pack = hub.pack_index(lang);

    // The source generation this invalidation registers under (H9-1): the
    // changed file's mtime, captured at call time. Every result (the changed
    // file AND its consumers) is claimed at this generation, so a later save's
    // invalidation — a strictly greater mtime — outranks it and a straggling
    // stale re-analysis (a smaller mtime) is rejected at the swap. A delete has
    // no mtime; use wall-clock now, which is monotone-forward past any prior
    // save and lets the deletion win.
    let event_gen = module_cache::file_mtime_nanos(&canon).unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(i64::MAX)
    });

    let mut consumers: Vec<PathBuf> = Vec::new();
    // Closures ride along for the Unchanged case: the consumers' persisted
    // deps_stamps must be recomputed (the edited header's mtime moved) or
    // the next warm scan rejects every consumer row and the cold storm the
    // gate prevents in-session comes back at restart.
    let mut consumer_closures: Vec<(PathBuf, crate::file_analysis::path_intern::ClosureList)> =
        Vec::new();
    if let Some(ref pack) = pack {
        pack.for_each_registered_file(&mut |cm| {
            if cm.analysis.include_closure.contains(&canon_str) {
                consumers.push(cm.path.clone());
                consumer_closures.push((cm.path.clone(), cm.analysis.include_closure.clone()));
            }
        });
    }

    if deleted {
        // The departed file's own header/macro/closure caches go too — a
        // consumer re-gather resolving the deleted header from its
        // still-warm entry would make the deletion invisible.
        crate::cpp_reparse::evict_analysis_caches(&std::iter::once(canon.clone()).collect());
        if let Some(ref pack) = pack {
            pack.unregister_file(&canon);
            pack.remove_surface(&canon);
            pack.forget_source_gen(&canon);
        }
    }

    // The surface gate (the freshness firewall, pack flavor): re-analyze
    // the CHANGED file first, alone. If its span-free surface is unchanged
    // — a body edit, a comment, a reformat in a header — every consumer's
    // analysis is still semantically valid (macro bodies and include
    // directives are ON the surface, so textual-inclusion effects are
    // covered) and the whole consumer re-analysis storm is skipped. A
    // deep-header comment edit re-parses ONE file, not hundreds of TUs.
    let mut changed_verdict = crate::surface::SurfaceVerdict::Changed;
    let mut changed_fa: Option<Arc<crate::file_analysis::FileAnalysis>> = None;
    if !deleted {
        crate::cpp_reparse::evict_analysis_caches(&std::iter::once(canon.clone()).collect());
        if let (Some(ref pack), Ok(source)) = (&pack, std::fs::read_to_string(&canon)) {
            let probe = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                driver.analyze_with_path(&source, Some(&canon))
            }));
            if let Ok(fa) = probe {
                changed_verdict = pack.record_surface(&canon, &fa);
                changed_fa = Some(Arc::new(fa));
            }
        }
    }
    let skip_consumers = matches!(changed_verdict, crate::surface::SurfaceVerdict::Unchanged);
    if !skip_consumers && !consumers.is_empty() {
        // The changed file's own caches were evicted before the probe and are
        // fresh — evict only the consumers' so they re-gather.
        crate::cpp_reparse::evict_analysis_caches(&consumers.iter().cloned().collect());
    }

    // Re-analyze the changed file (unless deleted) + every consumer
    // (parallel), then swap registrations. Unregister-then-register so names
    // the new version no longer defines don't linger in `all_defs` / the
    // cache winner slot. Consumers re-analyze on delete too — their splices
    // and closures baked the departed header.
    let mut targets: Vec<PathBuf> = Vec::with_capacity(consumers.len() + 1);
    if !deleted && changed_fa.is_none() {
        targets.push(canon.clone());
    }
    if !skip_consumers {
        targets.extend(consumers);
    }
    targets.sort();
    targets.dedup();
    targets.retain(|p| changed_fa.is_none() || *p != canon);
    let mut results: Vec<(PathBuf, Arc<crate::file_analysis::FileAnalysis>)> = targets
        .par_iter()
        .filter_map(|p| {
            let reg = crate::language_driver::LanguageRegistry::with_enabled();
            let driver = reg.for_path(p).filter(|d| d.id() == lang)?;
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let source = std::fs::read_to_string(p).ok()?;
                Some(driver.analyze_with_path(&source, Some(p)))
            }));
            match res {
                Ok(Some(analysis)) => Some((p.clone(), Arc::new(analysis))),
                _ => None,
            }
        })
        .collect();
    if let Some(fa) = changed_fa {
        results.push((canon.clone(), fa));
    }
    // Persist the FULL analyses (bag present) FIRST so the on-disk blob can
    // rehydrate, then register bag-STRIPPED resident copies and drop each
    // file's now-stale entry from the rehydration LRU (change #6). `results`
    // holds the full arcs; `save_to_db` encodes them whole. Strip only when we
    // actually persisted — else the bag would be unrecoverable, so keep it.
    let persisted = if let Some(conn) = module_cache::open_cache_db(root_uri, lang) {
        if deleted {
            module_cache::delete_ref_rows(&conn, &canon_str);
        }
        let tx = conn.unchecked_transaction().ok();
        for (p, arc) in &results {
            let p_str = p.to_string_lossy();
            let cached = Arc::new(CachedModule::new(p.clone(), arc.clone()));
            module_cache::save_to_db(&conn, &p_str, &Some(cached), "workspace");
            if !arc.degraded {
                let seeds: Vec<_> = arc.refs.iter().map(|r| r.row_seed()).collect();
                let sym_seeds = arc.sym_row_seeds();
                if let Err(e) = module_cache::shred_derived_rows(
                    &conn, &p_str, "workspace", &seeds, &sym_seeds,
                ) {
                    log::warn!("Failed to shred derived rows for {:?}: {}", p, e);
                }
            }
        }
        if skip_consumers {
            // Unchanged gate: the consumers' rows/blobs/stubs are still
            // valid, but the edited header's mtime moved every consumer's
            // closure stamp — refresh them or the next warm rejects every
            // consumer row (the restart cold storm).
            let mut memo = std::collections::HashMap::new();
            for (p, closure) in &consumer_closures {
                module_cache::refresh_deps_stamp(
                    &conn,
                    &p.to_string_lossy(),
                    closure,
                    &mut memo,
                );
            }
        }
        if let Some(tx) = tx {
            let _ = tx.commit();
        }
        true
    } else {
        false
    };
    if let Some(ref pack) = pack {
        for (p, arc) in &results {
            // H9-1 generation guard: claim BEFORE unregistering, so a rejected
            // (strictly-older) result leaves the fresher registration intact
            // rather than tearing it down. A stale re-analysis that read
            // pre-save bytes loses to nothing — it simply isn't registered, and
            // the writer that read post-save bytes (or a later save's event)
            // wins. This also closes hazard 3: an under-invalidated consumer the
            // bulk pass registered from pre-save bytes carries a lower generation
            // than the reconcile that reads current disk, so the reconcile wins
            // and no pre-save bytes are silently served.
            if !pack.claim_source_gen(p, event_gen) {
                log::debug!(
                    "pack swap: skip stale re-register of {:?} (event gen {} < registered)",
                    p,
                    event_gen
                );
                continue;
            }
            pack.unregister_file(p);
            // Drop the stale LRU pin BEFORE the new stripped copy becomes
            // reachable — a query racing this re-register must not
            // rehydrate the pre-edit generation against the new
            // registration (the blob+rows committed above).
            pack.invalidate_bag_cache(p);
            if persisted && !arc.degraded && eviction_enabled() {
                // Registration-owned strip (feeds read the whole copy).
                let _ = pack.register_symbols_stripping((*p).clone(), (**arc).clone(), true, true);
            } else {
                pack.register_symbols(p.clone(), arc.clone());
            }
        }
    }
}

/// Scan @INC directories for .pm files, populating the available_modules map.
/// Fast — no file reads, just directory traversal + path→module name conversion.
fn scan_inc_module_names(inc_paths: &[PathBuf], available: &DashMap<String, PathBuf>) {
    for inc in inc_paths {
        if inc.is_dir() {
            scan_dir_recursive(inc, inc, available, 0);
        }
    }
}

fn scan_dir_recursive(base: &std::path::Path, dir: &std::path::Path, available: &DashMap<String, PathBuf>, depth: u32) {
    if depth > 15 { return; } // prevent symlink loops
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.is_dir() {
            scan_dir_recursive(base, &path, available, depth + 1);
        } else if path.extension().map(|e| e == "pm").unwrap_or(false) {
            if let Ok(rel) = path.strip_prefix(base) {
                let module_name = rel.to_string_lossy()
                    .trim_end_matches(".pm")
                    .replace(std::path::MAIN_SEPARATOR, "::");
                available.insert(module_name, path.clone());
            }
        }
    }
}

fn uri_to_path(uri: &str) -> Option<PathBuf> {
    uri.strip_prefix("file://").map(PathBuf::from)
}

#[cfg(test)]
#[path = "module_resolver_tests.rs"]
mod tests;
