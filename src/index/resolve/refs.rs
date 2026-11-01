//! The reference walks: `refs_to`, `group_refs` + group rename edits,
//! `implementations_of`, and specialization families.
use crate::util::canonical_path::CanonicalPath;
use super::*;

/// Group construction shared by the local arm (cursor in the class
/// file: spans are origin-local) and the consumer arm (group minted
/// from the class's cached analysis: spans pin to the class file). The
/// rename chain on the Method target is computed against the CLASS
/// analysis — the only one that knows its parents.
pub(super) fn group_from_projections(
    p: crate::model::file_analysis::FieldProjections,
    class_analysis: &FileAnalysis,
    pinned_path: Option<CanonicalPath>,
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
        let mut target = if p.private {
            TargetRef::method(
                p.bare.clone(),
                p.class.clone(),
                Some(p.reader_kind),
                class_analysis,
                module_index,
                OverrideScope::Dispatch,
            )
        } else {
            TargetRef::owned_accessor(
                p.bare.clone(),
                p.class.clone(),
                p.reader_kind,
                class_analysis,
                module_index,
            )
        };
        target.def_paths = pack_class_def_paths(&target, class_analysis, module_index);
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
                class_analysis,
            ),
            rename: MemberRename::Bare,
        });
    }
    if p.has_internal {
        members.push(GroupMember {
            target: TargetRef::new(
                p.bare.clone(),
                TargetKind::InternalHashKey { class: p.class.clone() },
                class_analysis,
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
                class_analysis,
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
                MemberKind::Callable,
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
            decl_spans: p.decl_spans.into_iter().map(|s| (None, s)).collect(),
            members,
        },
        Some(path) => ResolvedTarget::Group {
            local_spans: Vec::new(),
            pinned_spans: p
                .variable_spans
                .into_iter()
                .map(|s| (path.clone(), s))
                .collect(),
            decl_spans: p
                .decl_spans
                .into_iter()
                .map(|s| (Some(path.clone()), s))
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
    files: &OpenDocs,
    module_index: Option<&dyn CrossFileLookup>,
    origin: &FileKey,
    local_spans: &[Span],
    pinned_spans: &[(CanonicalPath, Span)],
    members: &[GroupMember],
    mask_override: Option<RoleMask>,
) -> Vec<RefLocation> {
    let mut out: Vec<RefLocation> = local_spans
        .iter()
        .map(|span| RefLocation::at(origin.clone(), *span, AccessKind::Read))
        .collect();
    out.extend(pinned_spans.iter().map(|(path, span)| RefLocation::at(FileKey::Path(path.clone()), *span, AccessKind::Read)));
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

/// Which files the backward walk visits. Everything else is shared by
/// construction inside `walk_refs` — the session memo, the matcher
/// (`collect_from_analysis`), and the sort+dedup —
/// so "highlights is the origin-file slice of references" is ONE code
/// path with a smaller enumeration, not a sibling implementation kept in
/// agreement by a test. A new cross-cutting axis (a session concern, an
/// alias kind, a mask behavior) lands in `walk_refs` above the scope
/// split and both projections inherit it; it cannot be added to
/// references and forgotten in highlights.
pub(super) enum WalkScope<'a> {
    /// Every file the mask + closure gate admit: open docs, relational
    /// row candidates, the workspace sweep, the dependency sweep.
    Workspace,
    /// The origin document only — the highlights / linked-editing
    /// enumeration. Two deliberate asymmetries, stated here once: no
    /// closure gate (the origin minted the target at its own cursor, so
    /// it sees it by definition — and a fragment origin, seen only by
    /// textual inclusion, must still answer its own highlights); and the
    /// analysis is the origin's own copy handed to `resolve()` — already
    /// whole and enriched, so it takes no `matcher_view` routing.
    Origin { key: &'a FileKey, analysis: &'a FileAnalysis },
}

/// `refs_to` narrowed to ONE file — the origin scope of the same driver,
/// so the highlights image is the in-file slice of `references()` without
/// paying the workspace walk per cursor move.
pub(super) fn refs_to_in_file(
    files: &OpenDocs,
    module_index: Option<&dyn CrossFileLookup>,
    target: &TargetRef,
    key: &FileKey,
    analysis: &FileAnalysis,
    mask: RoleMask,
) -> Vec<RefLocation> {
    walk_refs(files, module_index, target, mask, WalkScope::Origin { key, analysis })
}

/// Reject a `newName` that would corrupt rather than rename: empty,
/// whitespace, or just the language's sigils. The LSP client normally validates
/// the new name, but the server must not emit a token-*deleting* edit set when
/// it doesn't — both rename entry points (LSP handler + CLI) gate on this.
/// Keyword/identifier-shape validation stays the client's job; this is the
/// safety floor against silent corruption.
pub fn is_valid_rename_name(new_name: &str, names: &crate::model::file_analysis::NameSpellings) -> bool {
    !names.strip_sigils(new_name.trim()).trim().is_empty()
}

/// Rename edit set for a projection group: every span paired with ITS
/// member's replacement text (bare for plain spellings, re-derived for
/// affixed accessors). Bare-member spans win collisions — a synthesized
/// accessor's decl token IS the group decl the bare edit covers.
#[allow(clippy::too_many_arguments)]
pub fn group_rename_edits(
    files: &OpenDocs,
    module_index: Option<&dyn CrossFileLookup>,
    origin: &FileKey,
    local_spans: &[Span],
    pinned_spans: &[(CanonicalPath, Span)],
    members: &[GroupMember],
    bare_new: &str,
    mask: RoleMask,
) -> Result<Vec<(RefLocation, String)>, String> {
    let mut out: Vec<(RefLocation, String)> = local_spans
        .iter()
        .map(|span| {
            (
                RefLocation::at(origin.clone(), *span, AccessKind::Read),
                bare_new.to_string(),
            )
        })
        .collect();
    out.extend(pinned_spans.iter().map(|(path, span)| {
        (
            RefLocation::at(FileKey::Path(path.clone()), *span, AccessKind::Read),
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
    // A union site appears in the image of each member it dispatches to; it
    // is safe only when every class it names is one of them, so what no
    // member's image covers is what its `also_on` lists share. A covered
    // site rewrites; an uncovered one keeps the classes left over.
    let mut uncovered: std::collections::HashMap<(PathBuf, Span), Vec<String>> =
        std::collections::HashMap::new();
    for (loc, _) in &out {
        let at = (key_for_sort(&loc.key), loc.span);
        match uncovered.get_mut(&at) {
            Some(left) => left.retain(|c| loc.rewrite.also_on().contains(c)),
            None => {
                uncovered.insert(at, loc.rewrite.also_on().to_vec());
            }
        }
    }
    for (loc, _) in &mut out {
        if let SiteRewrite::Shared { also_on } = &mut loc.rewrite {
            *also_on = uncovered[&(key_for_sort(&loc.key), loc.span)].clone();
            if also_on.is_empty() {
                loc.rewrite = SiteRewrite::Yes;
            }
        }
    }
    let name = members.first().map_or(bare_new, |m| m.target.name.as_str());
    let mut out = settle_rename_sites(name, out, |(loc, _)| loc)?;
    let mut seen = std::collections::HashSet::new();
    out.retain(|(loc, _)| seen.insert((key_for_sort(&loc.key), loc.span)));
    Ok(out)
}

/// The rename policy every rename path applies to its image: any site whose
/// `SiteRewrite` refuses refuses the whole rename (a partial edit would
/// silently break code there); otherwise the edits are the sites that
/// rewrite.
pub(crate) fn settle_rename_sites<T>(
    name: &str,
    sites: Vec<T>,
    loc: impl Fn(&T) -> &RefLocation,
) -> Result<Vec<T>, String> {
    if let Some(why) = sites.iter().find_map(|s| site_refusal(name, loc(s))) {
        return Err(why);
    }
    Ok(sites.into_iter().filter(|s| loc(s).rewrite.rewrites()).collect())
}

/// Why renaming `name` refuses at `loc`, when it does.
fn site_refusal(name: &str, loc: &RefLocation) -> Option<String> {
    match &loc.rewrite {
        SiteRewrite::Yes | SiteRewrite::Skip => None,
        SiteRewrite::ThroughMacro => Some(format!(
            "rename of `{name}` would leave sites spelled through a delegating macro \
             unchanged (the macro body is not rewritten) — refusing rather than \
             emitting a partial edit"
        )),
        SiteRewrite::Construction => Some(format!(
            "rename of `{name}` would leave its constructions unchanged (they spell the \
             class, not the constructor) — refusing rather than emitting a partial edit"
        )),
        SiteRewrite::Shared { also_on } => {
            let at = match &loc.key {
                FileKey::Path(p) => p.display().to_string(),
                FileKey::Url(u, _) => u.to_string(),
            };
            Some(format!(
                "`{}` at {}:{} may also be {}'s; rename from there to rename them together",
                name,
                at,
                loc.span.start.row + 1,
                also_on.join(" or "),
            ))
        }
    }
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

/// Sweep-wide memo over the relational retrieval — opened by a batch verb
/// that mints one backward walk per declaration (`--heatmap`) and whose
/// index is frozen for the run: every declaration of `new` probes the same
/// key, and every walk re-fetched the whole shredded-path set (one
/// `SELECT path FROM files` per declaration, ~3 s of a 113 s BMO run) under
/// the single retained-reader mutex, which a parallel gather would
/// serialize on. Entries are keyed by index identity + `resolution_epoch`,
/// so a shape mutation invalidates them like every other epoch memo.
/// Closed (the default) ⇒ every walk queries the store, exactly as before.
struct RetrievalMemo {
    candidates: dashmap::DashMap<(usize, u64, Vec<String>), std::sync::Arc<Vec<CanonicalPath>>>,
    indexed: dashmap::DashMap<(usize, u64), std::sync::Arc<std::collections::HashSet<CanonicalPath>>>,
    /// The dependency sweep's file partition, per index: every walk visited
    /// every cached file only to skip the shredded non-candidates (networkx
    /// heatmap: 693 files × 8,287 walks, 20.6 s of a 51 s gather).
    dep_files: dashmap::DashMap<(usize, u64), std::sync::Arc<DepFiles>>,
}

/// One cached-file snapshot split by the rows narrowing: files the rows
/// don't cover (always swept) and every file by path (candidates are looked
/// up, never scanned for).
struct DepFiles {
    unindexed: Vec<std::sync::Arc<crate::model::file_analysis::CachedModule>>,
    by_path: std::collections::HashMap<CanonicalPath, std::sync::Arc<crate::model::file_analysis::CachedModule>>,
}

static RETRIEVAL_MEMO: std::sync::RwLock<Option<RetrievalMemo>> = std::sync::RwLock::new(None);

/// Opens the retrieval memo for one batch sweep; closes (and drops) it on
/// drop. `PERL_LSP_NO_RETRIEVAL_MEMO=1` leaves it closed — the A/B control.
pub struct RetrievalMemoGuard(());

impl RetrievalMemoGuard {
    pub fn open() -> Self {
        let off = std::env::var("PERL_LSP_NO_RETRIEVAL_MEMO").as_deref() == Ok("1");
        if !off {
            if let Ok(mut slot) = RETRIEVAL_MEMO.write() {
                *slot = Some(RetrievalMemo {
                    candidates: dashmap::DashMap::new(),
                    indexed: dashmap::DashMap::new(),
                    dep_files: dashmap::DashMap::new(),
                });
            }
        }
        RetrievalMemoGuard(())
    }
}

impl Drop for RetrievalMemoGuard {
    fn drop(&mut self) {
        if let Ok(mut slot) = RETRIEVAL_MEMO.write() {
            *slot = None;
        }
    }
}

fn index_identity(idx: &dyn CrossFileLookup) -> (usize, u64) {
    (
        idx as *const dyn CrossFileLookup as *const () as usize,
        idx.resolution_epoch(),
    )
}

/// `idx.ref_candidate_paths(keys)`, memoized while a sweep memo is open.
fn retrieve_candidates(idx: &dyn CrossFileLookup, keys: &[String]) -> std::sync::Arc<Vec<CanonicalPath>> {
    if let Ok(slot) = RETRIEVAL_MEMO.read() {
        if let Some(memo) = slot.as_ref() {
            let (id, epoch) = index_identity(idx);
            let k = (id, epoch, keys.to_vec());
            if let Some(hit) = memo.candidates.get(&k) {
                crate::util::ghost_stats::count("refs.retrieval.memo_hit");
                return std::sync::Arc::clone(hit.value());
            }
            let fresh = std::sync::Arc::new(idx.ref_candidate_paths(keys));
            memo.candidates.insert(k, std::sync::Arc::clone(&fresh));
            return fresh;
        }
    }
    std::sync::Arc::new(idx.ref_candidate_paths(keys))
}

/// `idx.ref_indexed_paths()`, memoized while a sweep memo is open.
fn retrieve_indexed(idx: &dyn CrossFileLookup) -> std::sync::Arc<std::collections::HashSet<CanonicalPath>> {
    if let Ok(slot) = RETRIEVAL_MEMO.read() {
        if let Some(memo) = slot.as_ref() {
            let k = index_identity(idx);
            if let Some(hit) = memo.indexed.get(&k) {
                return std::sync::Arc::clone(hit.value());
            }
            let fresh = std::sync::Arc::new(idx.ref_indexed_paths());
            memo.indexed.insert(k, std::sync::Arc::clone(&fresh));
            return fresh;
        }
    }
    std::sync::Arc::new(idx.ref_indexed_paths())
}

/// The dependency sweep's partition against `rows_indexed`, while a sweep
/// memo is open. Keyed like `retrieve_indexed`, which `rows_indexed` came
/// from, so the two always describe the same index state.
fn retrieve_dep_files(
    idx: &dyn CrossFileLookup,
    rows_indexed: &std::collections::HashSet<CanonicalPath>,
) -> Option<std::sync::Arc<DepFiles>> {
    let slot = RETRIEVAL_MEMO.read().ok()?;
    let memo = slot.as_ref()?;
    let k = index_identity(idx);
    if let Some(hit) = memo.dep_files.get(&k) {
        return Some(std::sync::Arc::clone(hit.value()));
    }
    let mut df = DepFiles { unindexed: Vec::new(), by_path: Default::default() };
    idx.for_each_cached_file(&mut |cached| {
        if !rows_indexed.contains(&cached.path) {
            df.unindexed.push(std::sync::Arc::clone(cached));
        }
        df.by_path
            .entry(cached.path.clone())
            .or_insert_with(|| std::sync::Arc::clone(cached));
    });
    let df = std::sync::Arc::new(df);
    memo.dep_files.insert(k, std::sync::Arc::clone(&df));
    Some(df)
}

/// The name keys the relational retrieval probes for `target`, spelled the
/// way rows are written (`name_match_key`), so retrieval is exactly as
/// generous as the matcher's name checks. A use spelled through a delegating
/// macro carries a row under the delegate's key too (`RefTable::row_seeds`),
/// so it needs no widening here.
pub(super) fn retrieval_keys(target: &TargetRef) -> Vec<String> {
    target.retrieval_keys(&target.names)
}

/// The analysis view the backward-walk matcher runs on for one closed file:
/// the rows-axes view (`refs_present` — refs + symbols, bag not promised),
/// upgraded to `whole_present` only when some name-matching ref's verdict
/// isn't baked (`Ref::match_verdict_baked`) — those are exactly the matcher
/// arms that re-derive through the file's witness bag at query time
/// (unstamped method-call invocants, unowned hash keys), and a bag-stripped
/// view would silently drop their sites. Handler targets additionally
/// resolve receiver-gated dispatch candidates through the bag, so any file
/// carrying provisional dispatches takes the whole view for them.
///
/// The pre-scan runs on the rows view's OWN refs, so the upgrade decision
/// can never miss a row the matcher would see. Cost model: verdict-baked
/// files (the vast majority) cache bag-stripped at ~half the bytes, so the
/// walk's working set fits the LRU instead of cycling it.
pub(super) fn matcher_view(
    idx: &dyn CrossFileLookup,
    cached: &std::sync::Arc<crate::model::file_analysis::CachedModule>,
    target: &TargetRef,
) -> std::sync::Arc<FileAnalysis> {
    let view = idx.refs_present(cached);
    let needs_whole = match &target.kind {
        TargetKind::Handler { .. } => !view.provisional_dispatches.is_empty(),
        TargetKind::Sub { .. } | TargetKind::Method { .. } => target
            .retrieval_keys(view.names())
            .iter()
            .flat_map(|k| view.ref_indices_keyed(k).iter().map(|&i| &view.refs()[i]))
            .any(|r| {
                r.member_site().is_some()
                    && (r.unqualified_target_name(view.names()) == target.name
                        || (r.constructs() && !target.constructed_classes.is_empty()))
                    && !r.match_verdict_baked()
            }),
        TargetKind::HashKeyOfSub { .. }
        | TargetKind::HashKeyOfBridged(_)
        | TargetKind::InternalHashKey { .. } => refs_keyed(&view, &target.name).any(|r| {
            matches!(r.kind, RefKind::HashKeyAccess { .. })
                && r.target_name == target.name
                && !r.match_verdict_baked()
        }),
        _ => false,
    };
    if needs_whole {
        crate::util::ghost_stats::count("refs.matcher_upgrade");
        // Split by CAUSE: the arm that forced the upgrade is the target kind,
        // so a whole-copy decode is attributable to unstamped method calls,
        // unbaked hash keys, or live dispatches — a bare upgrade count cannot
        // say which, and they have different fixes.
        crate::util::ghost_stats::count(match &target.kind {
            TargetKind::Handler { .. } => "refs.upgrade_by.handler",
            TargetKind::Sub { .. } | TargetKind::Method { .. } => "refs.upgrade_by.methodcall",
            TargetKind::HashKeyOfSub { .. }
            | TargetKind::HashKeyOfBridged(_)
            | TargetKind::InternalHashKey { .. } => "refs.upgrade_by.hashkey",
            _ => "refs.upgrade_by.other",
        });
        return idx.whole_present(cached);
    }
    crate::util::ghost_stats::count("refs.matcher_rows_view");
    crate::util::ghost_stats::count(match &target.kind {
        TargetKind::Handler { .. } => "refs.rowsview_by.handler",
        TargetKind::Sub { .. } | TargetKind::Method { .. } => "refs.rowsview_by.methodcall",
        TargetKind::HashKeyOfSub { .. }
        | TargetKind::HashKeyOfBridged(_)
        | TargetKind::InternalHashKey { .. } => "refs.rowsview_by.hashkey",
        _ => "refs.rowsview_by.other",
    });
    view
}

/// Collect every reference to `target` across the masked file set.
///
/// - `files`   — open + workspace store
/// - `module_index` — dep cache (consulted only if mask includes Dependency)
pub fn refs_to(
    files: &OpenDocs,
    module_index: Option<&dyn CrossFileLookup>,
    target: &TargetRef,
    mask: RoleMask,
) -> Vec<RefLocation> {
    walk_refs(files, module_index, target, mask, WalkScope::Workspace)
}

/// THE backward walk. Both reference-shaped projections are this one
/// driver — `references()` at `WalkScope::Workspace`, highlights /
/// linked-editing at `WalkScope::Origin` — differing ONLY in which files
/// the scope enumerates (see `WalkScope` for the origin scope's two
/// stated asymmetries).
fn walk_refs(
    files: &OpenDocs,
    module_index: Option<&dyn CrossFileLookup>,
    target: &TargetRef,
    mask: RoleMask,
    scope: WalkScope<'_>,
) -> Vec<RefLocation> {
    // One backward walk issues a top-level type query per candidate call
    // site, and each re-derives the same cross-file `PackageSymbol`
    // lattice. The session is the memo that spans them (plus the consult
    // budget that bounds the walk when even the memo isn't enough) —
    // `docs/adr/resolution-session.md`. Entered for BOTH scopes: a
    // cursor-move storm of highlight queries re-derives the same lattice
    // a workspace walk would.
    let _session = crate::model::witnesses::ResolutionSession::enter(module_index);
    let mut out = Vec::new();
    let memo = WalkMemo::default();

    if let WalkScope::Origin { key, analysis } = scope {
        let file = match key {
            FileKey::Path(p) => p.clone(),
            FileKey::Url(_, p) => p.clone(),
        };
        collect_from_analysis(key, analysis, target, module_index, &file, &memo, &mut out);
        return sorted_deduped(out);
    }

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
                    seen_by_inclusion.extend(cached.analysis.pack.include_closure.iter_strs().map(|a| a.as_ref().to_owned()));
                }
            });
        }
    }
    let gate = |analysis: &FileAnalysis, file: &CanonicalPath| {
        let file_str = file.to_string_lossy();
        file_sees_target_ids(target, &target_def_ids, analysis, &file_str)
            || seen_by_inclusion.contains(file_str.as_ref())
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
    let mut rows_indexed: std::sync::Arc<std::collections::HashSet<CanonicalPath>> = Default::default();
    let mut candidate_set: std::collections::HashSet<CanonicalPath> = Default::default();

    // Open docs (canonical — their index entries are skipped).
    let mut covered_paths: std::collections::HashSet<CanonicalPath> = std::collections::HashSet::new();
    // An open doc reached through a symlinked root must shadow its own
    // persisted generation, so it claims the spelling candidate rows carry.
    let mut cover = |url: &Url, doc: &crate::index::document::Document| {
        if url.to_file_path().is_ok() {
            covered_paths.insert(doc.canonical_path.clone());
        }
    };
    if mask.contains(RoleMask::OPEN) {
        files.for_each_open(|url, doc| {
            cover(url, doc);
            // The walk applies visibility: role mask picked the tier, the
            // closure gate decides per file (`file_sees_target_ids`); the
            // matcher below only matches.
            if !gate(&doc.analysis, &doc.canonical_path) {
                return;
            }
            let key = FileKey::Url(url.clone(), doc.canonical_path.clone());
            collect_from_analysis(&key, &doc.analysis, target, module_index, &doc.canonical_path, &memo, &mut out);
        });
    } else {
        // Even if open isn't in the mask, track the paths so a WORKSPACE walk
        // doesn't duplicate them (an open file's pre-close state isn't meaningful).
        files.for_each_open(|url, doc| cover(url, doc));
    }

    // Relational retrieval (`docs/adr/relational-ref-index.md`): the files
    // holding name-keyed candidate rows, rehydrated (`whole_present`) and run
    // through the SAME matcher as every resident copy. Runs BEFORE the
    // resident sweep and claims `covered_paths`, so each file is collected
    // from its best copy exactly once; the sweep behind it still contributes
    // declaration-only files and files without rows (degraded, persistence
    // off, mid-index lag) — composition stays at-least-as-complete whether
    // or not resident refs were evicted.
    if rows_active {
        if let Some(idx) = module_index {
            let keys = retrieval_keys(target);
            let candidate_paths = crate::util::ghost_stats::timed("refs.retrieval.candidates", || retrieve_candidates(idx, &keys));
            crate::util::ghost_stats::count("refs.walks");
            crate::util::ghost_stats::add_n("refs.candidates", candidate_paths.len() as u64);
            if std::env::var_os("PERL_LSP_REFS_DEBUG").is_some() {
                eprintln!(
                    "[refs-debug] keys={:?} candidates={} narrow={}",
                    keys,
                    candidate_paths.len(),
                    narrow_enabled
                );
            }
            if narrow_enabled && !candidate_paths.is_empty() {
                rows_indexed = crate::util::ghost_stats::timed("refs.retrieval.indexed_paths", || retrieve_indexed(idx));
                candidate_set = candidate_paths.iter().cloned().collect();
            }
            for path in candidate_paths.iter() {
                if covered_paths.contains(path) {
                    continue;
                }
                // Tier attribution: a front-door registration rides
                // WORKSPACE, the rest (@INC) DEPENDENCY. The mask must admit
                // the candidate's OWN tier, or an EDITABLE rename would walk
                // read-only deps (and vice versa).
                if !mask.contains(index_role(idx, path)) {
                    continue;
                }
                let Some(cached) = idx.cached_by_path(path) else { continue };
                covered_paths.insert(path.clone());
                if !gate(&cached.analysis, &cached.path) {
                    continue;
                }
                let key = FileKey::Path(cached.path.clone());
                // The matcher reads refs (usage sites) AND symbols
                // (declaration sites) — the rows-axes view, upgraded to
                // whole only when a matching ref needs the bag.
                let full = crate::util::ghost_stats::timed("refs.cand.view", || matcher_view(idx, &cached, target));
                crate::util::ghost_stats::timed("refs.cand.collect", || collect_from_analysis(
                    &key, &full, target, module_index, &cached.path, &memo, &mut out,
                ));
            }
        }
    }

    // The index's files, each admitted by its OWN tier (`index_role`): the
    // project's front-door registrations and the @INC dependencies. Per-FILE
    // sweep (`for_each_cached_file`): the name-keyed view both repeats files
    // and HIDES a file that lost every name tie. Skip paths an open doc
    // already covered — it is fresher.
    if mask.intersects(RoleMask::WORKSPACE | RoleMask::DEPENDENCY) {
        let _t = crate::util::ghost_stats::ScopedNs::start("refs.sweep.deps");
        if let Some(idx) = module_index {
            let mut visit = |cached: &std::sync::Arc<crate::model::file_analysis::CachedModule>| {
                if !mask.contains(index_role(idx, &cached.path)) {
                    return;
                }
                // Shredded but not a candidate ⇒ provably matchless; skip the
                // rehydration. Candidates always fall through (the relational
                // block may have failed to resolve them).
                if rows_indexed.contains(&cached.path) && !candidate_set.contains(&cached.path) {
                    return;
                }
                if !covered_paths.insert(cached.path.clone()) {
                    return;
                }
                if !gate(&cached.analysis, &cached.path) {
                    return;
                }
                let key = FileKey::Path(cached.path.clone());
                // Rows-off fallback sweep: copies here may still be
                // row-axes-evicted (rows exist, retrieval switched off) —
                // the matcher needs refs + symbols, so take the rows view.
                let full = matcher_view(idx, cached, target);
                collect_from_analysis(&key, &full, target, module_index, &cached.path, &memo, &mut out);
            };
            // Narrowed, a sweep visits exactly the unshredded files plus the
            // candidates — so a sweep memo enumerates those instead of
            // every cached file.
            let partition = if rows_indexed.is_empty() {
                None
            } else {
                retrieve_dep_files(idx, &rows_indexed)
            };
            match partition {
                Some(df) => {
                    for cached in &df.unindexed {
                        visit(cached);
                    }
                    for path in &candidate_set {
                        if let Some(cached) = df.by_path.get(path) {
                            visit(cached);
                        }
                    }
                }
                None => idx.for_each_cached_file(&mut |cached| visit(cached)),
            }
        }
    }

    sorted_deduped(out)
}

/// The tier an index-held file rides: WORKSPACE for a front-door
/// registration, DEPENDENCY otherwise.
pub(super) fn index_role(idx: &dyn CrossFileLookup, path: &CanonicalPath) -> RoleMask {
    if idx.is_dependency_path(path) {
        RoleMask::DEPENDENCY
    } else {
        RoleMask::WORKSPACE
    }
}

/// Stable output order + (path, span) dedup — the one spelling both walk
/// scopes exit through. (The origin scope's single file makes the key
/// component a constant; identical result, one implementation.)
fn sorted_deduped(mut out: Vec<RefLocation>) -> Vec<RefLocation> {
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
            let probe = crate::model::graph::GraphView::new(origin, Some(idx));
            let mut descendants: Vec<String> = Vec::new();
            probe.walk(
                crate::model::graph::Node::Class(target.name.clone()),
                crate::model::graph::EdgeKindMask::INHERITS_INV,
                &mut |n| {
                    if let crate::model::graph::Node::Class(c) = n {
                        descendants.push(c.clone());
                    }
                    crate::model::graph::WalkControl::Continue
                },
            );
            for pkg in &descendants {
                for cached in idx.def_candidates(pkg) {
                    // Declaration-site scan reads symbols only.
                    let whole = idx.symbols_present(&cached);
                    for s in whole.symbols() {
                        if &s.name == pkg && matches!(s.kind, SymKind::Class) {
                            out.push(RefLocation::at(FileKey::Path(cached.path.clone()), s.selection_span, AccessKind::Declaration).with_rewrite(SiteRewrite::Skip));
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
    let probe = crate::model::graph::GraphView::new(origin, Some(idx));
    // Descendants PLUS their co-ancestors: an override can live on a SIBLING
    // PARENT of a shared descendant (DBIC's `Ordered` sits alongside `Row` in
    // `Track`'s MRO, not beneath it), which an INHERITS_INV sweep alone never
    // reaches. `dispatch_participants` is that gather, shared with
    // `member_override_family` — while each had its own walk, this verb found
    // the sibling and `references` did not, from the same cursor.
    let mut implementers = origin.dispatch_participants(class, Some(idx));
    // The target and its own ancestry are the CONTRACT side, not an
    // implementation: goto-def lands on the target itself, and a superclass
    // method sits BEHIND the target in every descendant's MRO (shadowed by the
    // target's own def — it never wins). Exclude both so the verb reports only
    // the classes that override at or ahead of the contract.
    let mut contract_line: std::collections::HashSet<String> =
        std::iter::once(class.clone()).collect();
    probe.walk(
        crate::model::graph::Node::Class(class.clone()),
        crate::model::graph::EdgeKindMask::INHERITS
            | crate::model::graph::EdgeKindMask::APP_SURFACE,
        &mut |n| {
            if let crate::model::graph::Node::Class(c) = n {
                contract_line.insert(c.clone());
            }
            crate::model::graph::WalkControl::Continue
        },
    );
    implementers.retain(|p| !contract_line.contains(p));

    let mut out: Vec<RefLocation> = Vec::new();
    for pkg in &implementers {
        // class → home module(s): exact cache key for the common
        // single-package file; the names index covers cross-named and
        // multi-package homes.
        // EVERY file declaring `pkg` is a home (the package relation is
        // name → set of files); the names index covers cross-named and
        // multi-package homes when the direct registration is empty.
        let mut homes: Vec<std::sync::Arc<crate::model::file_analysis::CachedModule>> =
            idx.visible_def_candidates(pkg);
        if homes.is_empty() {
            for m in idx.modules_with_symbol(pkg) {
                for c in idx.visible_def_candidates(&m) {
                    let declares = idx.symbols_present(&c).symbols().iter().any(|s| {
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
                .role_requires(pkg.as_str())
                .iter()
                .any(|r| r == &target.name);
            if is_marker {
                continue;
            }
            // Override-def scan reads symbols only (role markers read the
            // pinned `packages` lane on the resident copy above).
            let whole = idx.symbols_present(&cached);
            for s in whole.symbols() {
                if s.name == target.name
                    && matches!(s.kind, SymKind::Sub | SymKind::Method)
                    && s.package.as_deref() == Some(pkg.as_str())
                {
                    out.push(RefLocation::at(FileKey::Path(cached.path.clone()), s.selection_span, AccessKind::Declaration));
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
/// (local `FileAnalysis.pack.specializes` + the index's spec map); def sites
/// resolve through the by-name index (spec Class symbols are indexed under
/// their canonical spelling). `rewritable: false` — a spec's selection span
/// is the whole `X<args>` spelling; renaming the primary rewrites the base
/// TOKEN inside it via its PackageRef, never this span wholesale.
pub(super) fn specialization_family(
    origin: &FileAnalysis,
    module_index: Option<&dyn CrossFileLookup>,
    primary: &str,
) -> Vec<RefLocation> {
    let mut specs: Vec<String> = Vec::new();
    let probe = crate::model::graph::GraphView::new(origin, module_index);
    probe.walk(
        crate::model::graph::Node::Class(primary.to_string()),
        crate::model::graph::EdgeKindMask::SPECIALIZES,
        &mut |n| {
            if let crate::model::graph::Node::Class(c) = n {
                specs.push(c.clone());
            }
            crate::model::graph::WalkControl::Continue
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
            // Spec-class def-site scan reads symbols only.
            let whole = idx.symbols_present(&cached);
            for s in whole.symbols() {
                if &s.name == spec && matches!(s.kind, SymKind::Class) {
                    out.push(RefLocation::at(FileKey::Path(cached.path.clone()), s.selection_span, AccessKind::Declaration).with_rewrite(SiteRewrite::Skip));
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
