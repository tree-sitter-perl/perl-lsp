//! The reference walks: `refs_to`, `group_refs` + group rename edits,
//! `implementations_of`, specialization families, and delegation aliases.
use super::*;

/// Group construction shared by the local arm (cursor in the class
/// file: spans are origin-local) and the consumer arm (group minted
/// from the class's cached analysis: spans pin to the class file). The
/// rename chain on the Method target is computed against the CLASS
/// analysis — the only one that knows its parents.
pub(super) fn group_from_projections(
    p: crate::model::file_analysis::FieldProjections,
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

/// Which files the backward walk visits. Everything else is shared by
/// construction inside `walk_refs` — the session memo, the delegation
/// aliases, the matcher (`collect_from_analysis`), and the sort+dedup —
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
pub(crate) fn refs_to_in_file(
    files: &FileStore,
    module_index: Option<&dyn CrossFileLookup>,
    target: &TargetRef,
    key: &FileKey,
    analysis: &FileAnalysis,
    mask: RoleMask,
) -> Vec<RefLocation> {
    walk_refs(files, module_index, target, mask, WalkScope::Origin { key, analysis })
}

/// Reject a `newName` that would corrupt rather than rename: empty,
/// whitespace, or just sigils (`$`/`@`/`%`). The LSP client normally validates
/// the new name, but the server must not emit a token-*deleting* edit set when
/// it doesn't — both rename entry points (LSP handler + CLI) gate on this.
/// Keyword/identifier-shape validation stays the client's job; this is the
/// safety floor against silent corruption.
pub fn is_valid_rename_name(new_name: &str) -> bool {
    !crate::model::conventions::strip_variable_sigils(new_name.trim()).trim().is_empty()
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
pub(super) fn retrieval_keys(target: &TargetRef, aliases: &[DelegationAlias]) -> Vec<String> {
    let mut keys = vec![crate::model::file_analysis::name_match_key(&target.name)];
    // A constructor's call sites spell the CLASS (`new Foo(...)`), never the
    // ctor name — without the class key the row filter never hands the
    // matcher the files that hold them (every cross-file `new` dropped, and
    // the heatmap called the ctor dead).
    if let Some(class) = target.ctor_of.as_deref() {
        let k = crate::model::file_analysis::name_match_key(class);
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    for a in aliases {
        let k = crate::model::file_analysis::name_match_key(&a.name);
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
    keys
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
        TargetKind::Sub { .. } | TargetKind::Method { .. } => view.refs().iter().any(|r| {
            matches!(r.kind, RefKind::MethodCall { .. })
                && r.unqualified_target_name() == target.name
                && !r.match_verdict_baked()
        }),
        TargetKind::HashKeyOfSub { .. }
        | TargetKind::HashKeyOfBridged(_)
        | TargetKind::InternalHashKey { .. } => view.refs().iter().any(|r| {
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
    files: &FileStore,
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
    files: &FileStore,
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

    // Names that reach the target through a macro delegation edge — the
    // BACKWARD half of goto-def's see-through (`#define IncRef(sv)
    // Perl_Inc(sv)` means every `IncRef(...)` call site is a reference to
    // `Perl_Inc`). Computed once per query; empty for Perl.
    let aliases = crate::util::timings::phase("refs.aliases", || {
        delegation_aliases(files, module_index, target, mask)
    });

    if let WalkScope::Origin { key, analysis } = scope {
        let file_str = canonical_file_str(key);
        collect_from_analysis(key, analysis, target, &aliases, module_index, &file_str, &mut out);
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
    // holding name-keyed candidate rows, rehydrated (`whole_present`) and run
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
            // Decode the candidate set in parallel before the sequential
            // match: cold, decode was ~60% of the first answer. Workspace
            // FileStore entries answer resident and are skipped; the
            // prefetch's own cap means a candidate set past it decodes its
            // tail serially, as before.
            let to_warm: Vec<std::path::PathBuf> = candidate_paths
                .iter()
                .filter(|p| !covered_paths.contains(*p) && !files.workspace_raw().contains_key(*p))
                .cloned()
                .collect();
            idx.prefetch_refs(&to_warm);
            for path in candidate_paths {
                if covered_paths.contains(&path) {
                    continue;
                }
                // Tier attribution: a FileStore workspace entry rides the
                // WORKSPACE role (Perl project files); everything else the
                // rows name lives in a module-index tier, whose role the
                // INDEX answers per path (`is_dependency_path`): the hub is
                // all-`@INC` (DEPENDENCY), a pack sub-index holds the
                // workspace's own files (WORKSPACE) plus declared dependency
                // roots — composer's vendor (DEPENDENCY). The mask must
                // admit the candidate's OWN tier, or an EDITABLE rename
                // would walk read-only deps (and vice versa).
                let ws_arc = files
                    .workspace_raw()
                    .get(&path)
                    .map(|e| std::sync::Arc::clone(e.value()));
                let cached = match ws_arc {
                    Some(arc) => {
                        if !mask.contains(RoleMask::WORKSPACE) {
                            continue;
                        }
                        std::sync::Arc::new(crate::model::file_analysis::CachedModule::new(
                            path.clone(),
                            arc,
                        ))
                    }
                    None => {
                        let role = if idx.is_dependency_path(&path) {
                            RoleMask::DEPENDENCY
                        } else {
                            RoleMask::WORKSPACE
                        };
                        if !mask.contains(role) {
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
                // (declaration sites) — the rows-axes view, upgraded to
                // whole only when a matching ref needs the bag.
                let full = crate::util::ghost_stats::timed("refs.matcher_view", || matcher_view(idx, &cached, target));
                crate::util::ghost_stats::timed("refs.collect", || {
                    collect_from_analysis(
                        &key, &full, target, &aliases, module_index, &file_str, &mut out,
                    )
                });
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
            // Same rows-axes routing as the sibling sweeps: a workspace
            // copy with rows persisted is refs+symbols-STRIPPED, and the
            // matcher reading it raw silently drops the file's matches.
            let full = match module_index {
                Some(idx) => {
                    let cached = std::sync::Arc::new(
                        crate::model::file_analysis::CachedModule::new(
                            entry.key().clone(),
                            std::sync::Arc::clone(entry.value()),
                        ),
                    );
                    matcher_view(idx, &cached, target)
                }
                None => std::sync::Arc::clone(entry.value()),
            };
            collect_from_analysis(&key, &full, target, &aliases, module_index, &file_str, &mut out);
        }
    }

    // The module-index tiers: `@INC` dependencies AND — in a pack
    // sub-index — the workspace's own files, attributed per path
    // (`is_dependency_path`; declared dependency roots like composer's
    // vendor are the read-only part). Per-FILE sweep
    // (`for_each_cached_file`): the name-keyed view both repeats files and
    // HIDES a file that lost every name tie. Skip paths an open/workspace
    // copy already covered — those are fresher.
    if mask.intersects(RoleMask::DEPENDENCY | RoleMask::WORKSPACE) {
        if let Some(idx) = module_index {
            idx.for_each_cached_file(&mut |cached| {
                let role = if idx.is_dependency_path(&cached.path) {
                    RoleMask::DEPENDENCY
                } else {
                    RoleMask::WORKSPACE
                };
                if !mask.contains(role) {
                    return;
                }
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
                // row-axes-evicted (rows exist, retrieval switched off) —
                // the matcher needs refs + symbols, so take the rows view.
                let full = crate::util::ghost_stats::timed("refs.matcher_view", || matcher_view(idx, cached, target));
                crate::util::ghost_stats::timed("refs.collect", || {
                    collect_from_analysis(&key, &full, target, &aliases, module_index, &file_str, &mut out)
                });
            });
        }
    }

    sorted_deduped(out)
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
/// Does this file declare `leaf` in a namespace OTHER than `contract_ns`,
/// with a recorded inheritance edge back onto `leaf` IN `contract_ns`?
/// That is the same-leaf direct-implementer witness: Laravel's
/// `class Repository implements CacheContract` (the alias resolving to
/// `Contracts\Cache\Repository`) is a SELF-LOOP in leaf space, and only
/// the namespace rows tell the implementer from the contract.
fn declares_self_leaf_implementer(a: &FileAnalysis, leaf: &str, contract_ns: &str) -> bool {
    a.declared_class_namespace(leaf).is_some_and(|ns| ns != contract_ns)
        && a.pack
            .parent_namespaces
            .iter()
            .any(|(c, p, ns)| c == leaf && p == leaf && ns == contract_ns)
}

/// FQ-validate one leaf-keyed family candidate: walk `from`'s parent
/// chain upward and classify how it reaches `target`. Three outcomes per
/// complete walk, and only a PROVEN wrong family prunes:
/// - some chain reaches `target` with the recorded namespace agreeing (or
///   no namespace recorded — Perl, cpp, pre-FQ analyses make no claim) →
///   keep;
/// - every chain that reaches `target` does so through a RECORDED,
///   MISMATCHING namespace (Laravel's three same-leaf `Repository`s) →
///   prune;
/// - no chain reaches `target` at all (a co-ancestor sitting BESIDE the
///   target in a shared descendant's MRO — DBIC's `Ordered`) → keep, the
///   gather put it there for a reason this walk can't see.
fn fq_family_member(
    origin: &FileAnalysis,
    idx: &dyn CrossFileLookup,
    from: &str,
    target: &str,
    target_ns: &str,
) -> bool {
    use std::collections::VecDeque;
    let ns_agrees = |candidate: &str| candidate == target_ns;
    let mut queue: VecDeque<(String, Option<String>)> =
        std::iter::once((from.to_string(), None)).collect();
    let mut seen: std::collections::HashSet<(String, Option<String>)> = Default::default();
    let mut reached_any = false;
    let mut budget = 2048usize;
    while let Some((leaf, want_ns)) = queue.pop_front() {
        if !seen.insert((leaf.clone(), want_ns.clone())) {
            continue;
        }
        if budget == 0 {
            // Truncated walk proves nothing — keep (never prune on a budget).
            return true;
        }
        budget -= 1;
        // Every analysis declaring `leaf`: the origin's own plus the index's
        // candidates (symbols view: the class row + the pinned parents lane).
        let mut visit = |a: &FileAnalysis| -> bool {
            if let Some(w) = &want_ns {
                let cls_ns = a
                    .symbols()
                    .iter()
                    .find(|s| matches!(s.kind, SymKind::Class) && s.name == leaf)
                    .map(|s| s.package.clone().unwrap_or_default());
                if cls_ns.as_deref() != Some(w.as_str()) {
                    return false; // not the namespace this hop meant
                }
            }
            for parent in a.declared_parents(&leaf) {
                let rec = a
                    .pack
                    .parent_namespaces
                    .iter()
                    .find(|(c, p, _)| c == &leaf && p == parent)
                    .map(|(_, _, ns)| ns.clone());
                if parent == target {
                    reached_any = true;
                    match &rec {
                        Some(ns) if ns_agrees(ns) => return true,
                        Some(_) => {} // recorded, wrong family — keep looking
                        None => return true, // no claim → agree (status quo)
                    }
                }
                queue.push_back((parent.clone(), rec));
            }
            false
        };
        if visit(origin) {
            return true;
        }
        for cached in idx.def_candidates(&leaf) {
            let a = idx.symbols_present(&cached);
            if visit(&a) {
                return true;
            }
        }
    }
    !reached_any
}

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
            // FQ family gate: with three same-leaf `Repository`s, INHERITS_INV
            // from the leaf conflates the families — keep only descendants
            // whose chain provably belongs (or makes no claim).
            let mut same_leaf_contract_ns: Option<String> = None;
            if let Some(tns) = origin.leaf_namespace(&target.name) {
                descendants
                    .retain(|d| fq_family_member(origin, idx, d, &target.name, &tns));
                // The self-loop case (`class Repository implements` its
                // same-leaf contract): the walk never re-visits its own seed,
                // so the direct implementer is absent from `descendants`.
                // Re-admit the leaf on namespace-row evidence; emission below
                // serves only the witnessing declarations.
                if idx
                    .def_candidates(&target.name)
                    .iter()
                    .any(|c| {
                        declares_self_leaf_implementer(&idx.symbols_present(c), &target.name, &tns)
                    })
                {
                    descendants.push(target.name.clone());
                    same_leaf_contract_ns = Some(tns);
                }
            }
            for pkg in &descendants {
                let self_leaf_ns = same_leaf_contract_ns.as_ref().filter(|_| pkg == &target.name);
                for cached in idx.def_candidates(pkg) {
                    // Declaration-site scan reads symbols only.
                    let whole = idx.symbols_present(&cached);
                    if let Some(tns) = self_leaf_ns {
                        if !declares_self_leaf_implementer(&whole, pkg, tns) {
                            continue;
                        }
                    }
                    for s in whole.symbols() {
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
    let probe = crate::model::graph::GraphView::new(origin, Some(idx));
    // Descendants PLUS their co-ancestors: an override can live on a SIBLING
    // PARENT of a shared descendant (DBIC's `Ordered` sits alongside `Row` in
    // `Track`'s MRO, not beneath it), which an INHERITS_INV sweep alone never
    // reaches. `dispatch_participants` is that gather, shared with
    // `method_override_family` — while each had its own walk, this verb found
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
    // Same FQ family gate as the Package arm: an implementer that reaches
    // the contract only through a recorded, MISMATCHING namespace belongs
    // to a same-leaf stranger's family.
    let mut same_leaf_contract_ns: Option<String> = None;
    if let Some(tns) = origin.leaf_namespace(class) {
        implementers.retain(|p| fq_family_member(origin, idx, p, class, &tns));
        // The self-loop case: a direct implementer CARRYING the contract's
        // own leaf sits inside `contract_line`, so the exclusion above
        // dropped it. Re-admit the leaf when a foreign-namespace
        // declaration witnesses the edge, and remember the contract's
        // namespace so emission below serves ONLY those declarations
        // (never the contract's own file, never a third same-leaf family).
        if idx
            .def_candidates(class)
            .iter()
            .any(|c| declares_self_leaf_implementer(&idx.symbols_present(c), class, &tns))
        {
            implementers.insert(class.clone());
            same_leaf_contract_ns = Some(tns);
        }
    }

    let mut out: Vec<RefLocation> = Vec::new();
    for pkg in &implementers {
        let self_leaf_ns = same_leaf_contract_ns.as_ref().filter(|_| pkg == class);
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
            if let Some(tns) = self_leaf_ns {
                if !declares_self_leaf_implementer(&idx.symbols_present(&cached), pkg, tns) {
                    continue;
                }
            }
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
pub(super) struct DelegationAlias {
    pub(super) name: String,
    pub(super) def_path: String,
}

/// The macro names whose call sites dispatch to `target` through delegation
/// edges (`MacroDef::delegate`), transitively (`#define A(x) B(x)`,
/// `#define B(x) F(x)` — both A and B reach F), each carrying its own
/// `#define`'s file for the per-scanned-file visibility gate. The backward
/// mirror of the forward see-through offer in `pack_macro_definition`. Only
/// callable-name-keyed kinds have a delegation surface; the DEPENDENCY sweep
/// is gated on the mask so a Perl EDITABLE query never touches the dep cache.
/// Sorted for deterministic output.
pub(super) fn delegation_aliases(
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
        for m in &a.pack.macro_defs {
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
