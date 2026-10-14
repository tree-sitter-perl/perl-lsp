//! Include-closure macro gathering: pre-expanded external tables, header
//! parse/include caches, toolchain include resolution, and cache eviction.

use super::*;

/// One pre-expanded variant of the external table + the identifiers its bodies
/// name. `table` is `pre_expand_bodies`d once; `body_idents` (every identifier
/// in a `table` body) drives the clean-split test — a file-local name in this
/// set means an external expansion would depend on it, so the split can't bake
/// it and the analyze falls to the slow single-tier path.
#[derive(Default)]
pub(super) struct ExpandedVariant {
    pub(super) table: MacroTable,
    pub(super) body_idents: std::collections::HashSet<String>,
}

impl ExpandedVariant {
    fn of(macros: &MacroTable) -> Self {
        let table = pre_expand_bodies(macros);
        let body_idents = body_identifiers(&table);
        ExpandedVariant { table, body_idents }
    }
}

/// The EXTERNAL macro table (from the `#include` closure), mutually pre-expanded
/// ONCE per include-set and cached. External-referencing-external object refs
/// are baked into the variants, so the per-analyze transform never re-fixpoints
/// the huge external set (perl.h ≈ 2000 macros) — it fixpoints only the
/// file-LOCAL macros and resolves external names by lookup here. `raw` is
/// retained for the byte-identical slow fallback, whose single-tier merge +
/// fixpoint needs the un-pre-expanded external bodies.
#[derive(Default)]
pub struct PreExpandedExternal {
    pub(super) raw: std::sync::Arc<MacroTable>,
    /// Full mutual pre-expansion (the `preprocess_with` path).
    full: ExpandedVariant,
    /// Identifier-alias subset only (the parse-damage `alias_only` fallback):
    /// `is_identifier_alias`-retained BEFORE expansion, matching the old
    /// merge-then-retain-then-fixpoint order.
    alias: ExpandedVariant,
    /// The gather was SKIPPED (cached-only miss on open), not run: this
    /// empty table is a stand-in, not the truth. Analyses built from it
    /// are marked degraded so the persist tier never freezes them.
    pub degraded: bool,
}

impl PreExpandedExternal {
    pub fn empty() -> Self {
        Self::default()
    }

    /// The cached-only miss: empty AND flagged so downstream consumers know
    /// the external table is a placeholder, not a real (possibly empty) gather.
    fn degraded_empty() -> Self {
        PreExpandedExternal { degraded: true, ..Self::default() }
    }

    pub(super) fn from_raw(raw: std::sync::Arc<MacroTable>) -> Self {
        // This mutual pre-expansion is the O(external) work the two-tier split
        // hoists out of every analyze — paid ONCE per include-set here, then
        // reused warm. Labelled so `PERL_LSP_PHASE_TIMING` shows the per-analyze
        // cost it eliminates.
        let (full, alias) = crate::util::timings::phase("cpp.external_preexpand", || {
            let full = ExpandedVariant::of(&raw);
            let mut alias_src = (*raw).clone();
            alias_src.retain(|_, m| is_identifier_alias(m));
            (full, ExpandedVariant::of(&alias_src))
        });
        PreExpandedExternal { raw, full, alias, degraded: false }
    }

    pub(super) fn variant(&self, alias_only: bool) -> &ExpandedVariant {
        if alias_only {
            &self.alias
        } else {
            &self.full
        }
    }

    /// Object-like gathered macros as `(name, body)` — the raw (un-pre-expanded)
    /// bodies, so a `#define X Y` stays an alias EDGE (`X → TypeName(Y)`) the
    /// bag chases, rather than a flattened leaf. The type-alias emission uses
    /// this to carry an include-closure's type macros (`U16TYPE` from a
    /// gitignored generated `config.h`) into every consuming file's bag, where
    /// the cross-file `TypeName` chase can never index the header directly.
    pub fn object_like_macros(&self) -> impl Iterator<Item = (&str, &str)> {
        self.raw
            .iter()
            .filter(|(_, m)| m.params.is_none())
            .map(|(k, m)| (k.as_str(), m.body.as_str()))
    }

    /// Every gathered macro NAME (object- and function-like) — the include
    /// closure's macro universe. The nested-macro-body ref lane unions this
    /// with the file's own `#define`s so a body token naming a header-defined
    /// macro (`SvFLAGS` used inside an `hv.h` macro) still mints a reference.
    pub fn macro_names(&self) -> impl Iterator<Item = &str> {
        self.raw.keys().map(|k| k.as_str())
    }

    /// Where gathered macro `name` is defined in the gathering file, as
    /// `(from, until, delegate)` per `Macro::spans` entry, over
    /// `include_lines` — any per-line projection of `include_lines(src)`
    /// (byte offsets, rows). `None` when the closure never defines it.
    pub fn gathered_spans(&self, name: &str, include_lines: &[usize]) -> Option<Vec<(usize, Option<usize>, Option<String>)>> {
        let m = self.raw.get(name)?;
        let line = |o: u32| include_lines.get(o as usize).copied();
        Some(m.spans.iter().map(|s| (line(s.from).unwrap_or(0), s.until.and_then(line), s.delegate.clone())).collect())
    }

    /// Whether the include closure `#define`s `name` anywhere, whether or
    /// not that definition reaches the gathering file.
    pub fn defines(&self, name: &str) -> bool {
        self.raw.contains_key(name)
    }

    /// The include lines (as in `gathered_spans`) whose headers `#undef`
    /// gathered macro `name`.
    pub fn undefined_lines(&self, name: &str, include_lines: &[usize]) -> Vec<usize> {
        let Some(m) = self.raw.get(name) else { return Vec::new() };
        m.undefined_at.iter().filter_map(|&o| include_lines.get(o as usize).copied()).collect()
    }
}

/// The line shape `include_set_hash` keys on: `#` then `include`.
pub(super) fn is_include_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with('#') && t[1..].trim_start().starts_with("include")
}

/// Every `#include` line as `(byte offset, row)` of its start, in source
/// order — the ordinal space `Macro::spans` indexes. It is the line
/// set `include_set_hash` hashes, so an ordinal stays valid exactly as long
/// as the gathered table cached under that hash does.
pub fn include_lines(src: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut byte = 0;
    for (row, line) in src.split_inclusive('\n').enumerate() {
        if is_include_line(line) {
            out.push((byte, row));
        }
        byte += line.len();
    }
    out
}

/// Every identifier token appearing in any macro body — the reference
/// candidates. Used to detect an external body that (transitively, since
/// `expanded` bodies are already baked) names a file-local macro.
fn body_identifiers(macros: &MacroTable) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for m in macros.values() {
        let bytes = m.body.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if is_ident_byte(bytes[i]) && (i == 0 || !is_ident_byte(bytes[i - 1])) {
                let s = i;
                while i < bytes.len() && is_ident_byte(bytes[i]) {
                    i += 1;
                }
                out.insert(m.body[s..i].to_string());
            } else {
                i += 1;
            }
        }
    }
    out
}

fn pre_expanded_cache() -> &'static GatherCache<std::path::PathBuf, u64, std::sync::Arc<PreExpandedExternal>>
{
    static C: OnceLock<
        GatherCache<std::path::PathBuf, u64, std::sync::Arc<PreExpandedExternal>>,
    > = OnceLock::new();
    C.get_or_init(|| {
        GatherCache::new_labeled(gather_cap_bytes(PRE_EXPANDED_CACHE_MB), "gather-pre-expanded")
    })
}

/// The full+alias expanded-variant payload ADDED on top of the raw table (the
/// raw `Arc` is shared with `macro_table_cache`, so it is NOT counted here).
fn pre_expanded_heap_bytes(pe: &PreExpandedExternal) -> usize {
    macro_table_heap_bytes(&pe.full.table)
        + pe.full.body_idents.iter().map(|s| s.len() + 24).sum::<usize>()
        + macro_table_heap_bytes(&pe.alias.table)
        + pe.alias.body_idents.iter().map(|s| s.len() + 24).sum::<usize>()
}

/// `included_macros` plus the one-time mutual pre-expansion of the external
/// table, cached by the same (file, include-set) key. Warm analyzes reuse the
/// pre-expanded table for free — the transform then only fixpoints file-local
/// macros. This is the driver's `gather_macros` hook.
pub fn included_macros_pre_expanded(
    file_path: &std::path::Path,
    src: &str,
    parser: &mut tree_sitter::Parser,
) -> std::sync::Arc<PreExpandedExternal> {
    let key = file_path.to_path_buf();
    let inc_hash = include_set_hash(src);
    pre_expanded_cache().get_or_fill(key, inc_hash, || {
        // In cached-only mode (on-open), a raw-table miss yields an EMPTY
        // external set that is deliberately NOT cached (`Transient`) — so the
        // background gather's real table lands cleanly once it warms and this
        // file is re-analyzed.
        match included_macros_inner(file_path, src, parser, !gather_cached_only()) {
            Some(raw) => {
                let pe = std::sync::Arc::new(PreExpandedExternal::from_raw(raw));
                let bytes = pre_expanded_heap_bytes(&pe);
                Fill::Store(pe, bytes)
            }
            None => Fill::Transient(std::sync::Arc::new(PreExpandedExternal::degraded_empty())),
        }
    })
}

thread_local! {
    /// Each Rayon worker keeps its own `Parser` — tree-sitter parsers aren't
    /// `Sync`, so the parallel frontier can't share one. Created once per thread.
    static POOL_PARSER: std::cell::RefCell<Option<tree_sitter::Parser>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `f` with this thread's pooled parser for `lang`.
fn with_pooled_parser<T>(
    lang: &tree_sitter::Language,
    f: impl FnOnce(&mut tree_sitter::Parser) -> T,
) -> T {
    POOL_PARSER.with(|slot| {
        let mut b = slot.borrow_mut();
        if b.is_none() {
            let mut p = tree_sitter::Parser::new();
            p.set_language(lang).expect("cpp grammar for pooled parser");
            *b = Some(p);
        }
        f(b.as_mut().expect("pooled parser present"))
    })
}

/// Walk the `#include` closure and collect every reachable header's macros.
///
/// Parallel + memoized: each BFS LEVEL's headers are parsed concurrently (Rayon,
/// one pooled `Parser` per worker); `header_info` memoizes by `(path, mtime)` so
/// a header shared across the closure — or across FILES (op.c and sv.c share
/// ~90% of perl5's tree) — is parsed exactly once. There is no header cap: the
/// `seen` set alone bounds the walk (cycles + re-visits), and the memoize bounds
/// the cost, so op.c's full closure is collected instead of truncated.
///
/// BREADTH-first, first-wins: the file's DIRECT includes are merged before
/// theirs, so the closest (most relevant) header's definition of a name wins —
/// the abseil `mutex.h`-vs-`thread_annotations.h` invariant. Determinism under
/// parallelism: a level is canonicalized + deduped SERIALLY in queue order, and
/// the parsed results are merged (and their children enqueued) in that same
/// order, so the macro table is deterministic regardless of parallelism.
pub(super) fn gather_included_macros(
    file_path: &std::path::Path,
    src: &str,
    parser: &mut tree_sitter::Parser,
) -> (BTreeMap<String, Macro>, Vec<(std::path::PathBuf, i64)>) {
    use rayon::prelude::*;
    let mut macros = BTreeMap::new();
    let mut headers: Vec<(std::path::PathBuf, i64)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    if let Ok(p) = file_path.canonicalize() {
        seen.insert(p);
    }
    let Some(lang) = parser.language().map(|l| (*l).clone()) else {
        return (macros, headers);
    };
    // Each header's index in `parsed`, and which header each of its
    // includes resolved to — what the preprocessor-order walk replays once
    // every header is parsed.
    let mut index_of: std::collections::HashMap<std::path::PathBuf, usize> = std::collections::HashMap::new();
    let mut parsed: Vec<std::sync::Arc<CachedHeader>> = Vec::new();
    let mut included: Vec<Vec<Option<usize>>> = Vec::new();
    let mut roots: Vec<(u32, usize)> = Vec::new();
    let line_starts: Vec<usize> = include_lines(src).into_iter().map(|(b, _)| b).collect();
    let mut frontier: Vec<(std::path::PathBuf, Parent)> = include_sites(src, parser)
        .into_iter()
        .filter_map(|(inc, at)| {
            let ordinal = line_starts.partition_point(|&b| b <= at).checked_sub(1)?;
            Some((resolve_include(file_path, &inc)?, Parent::Line(ordinal as u32)))
        })
        .collect();
    while !frontier.is_empty() {
        // Canonicalize + dedup this level in queue order (cheap stat) so the
        // parallel parse below can't perturb the first-wins merge order.
        let mut level: Vec<(std::path::PathBuf, Parent)> = Vec::with_capacity(frontier.len());
        let mut edges: Vec<(Parent, std::path::PathBuf)> = Vec::new();
        for (path, parent) in frontier.drain(..) {
            let Ok(canon) = path.canonicalize() else { continue };
            if seen.insert(canon.clone()) {
                level.push((canon, parent));
            } else {
                edges.push((parent, canon));
            }
        }
        // header_info is pure per header → parse the level concurrently.
        let infos: Vec<Option<std::sync::Arc<CachedHeader>>> = level
            .par_iter()
            .map(|(canon, _)| with_pooled_parser(&lang, |p| header_info(canon, p)))
            .collect();
        let mut next: Vec<(std::path::PathBuf, Parent)> = Vec::new();
        for ((canon, parent), info) in level.into_iter().zip(infos) {
            let Some(info) = info else { continue };
            headers.push((canon.clone(), file_stamp(&canon)));
            for (k, v) in &info.macros {
                macros.entry(k.clone()).or_insert_with(|| v.clone());
            }
            for (i, inc) in info.includes.iter().enumerate() {
                if let Some(nx) = resolve_include(&canon, inc) {
                    next.push((nx, Parent::Header(parsed.len(), i)));
                }
            }
            index_of.insert(canon.clone(), parsed.len());
            included.push(vec![None; info.includes.len()]);
            parsed.push(info);
            edges.push((parent, canon));
        }
        for (parent, canon) in edges {
            let Some(&child) = index_of.get(&canon) else { continue };
            match parent {
                Parent::Line(o) => roots.push((o, child)),
                Parent::Header(p, i) => included[p][i] = Some(child),
            }
        }
        frontier = next;
    }
    let order = replay_in_preprocessor_order(&parsed, &included, roots);
    for (name, spans) in order.spans {
        if let Some(m) = macros.get_mut(&name) {
            m.spans = spans;
        }
    }
    // TODO(undef-only-header): a name the closure only undefines has no
    // table entry to carry it, so it ends no `#define` above the include.
    for (name, ordinals) in order.undefined_at {
        if let Some(m) = macros.get_mut(&name) {
            m.undefined_at = ordinals;
        }
    }
    (macros, headers)
}

/// What replaying the include closure in preprocessor order says about
/// each name: where it is defined, and where it is undefined.
#[derive(Default)]
struct PreprocessorOrder {
    spans: std::collections::HashMap<String, Vec<GatheredSpan>>,
    undefined_at: std::collections::HashMap<String, Vec<u32>>,
}

/// Replay every header's directives the way the preprocessor reads them:
/// the gathering file's includes in source order, each header's
/// `#define`/`#undef`/`#include` lines in its own order, descending into an
/// include where it stands. A header is replayed once, at the first include
/// reaching it, because an include guard makes the later ones no-ops. Every
/// directive lands at the ordinal of the gathering file's include line it
/// is replayed under (`roots`: `(ordinal, header)`).
fn replay_in_preprocessor_order(
    parsed: &[std::sync::Arc<CachedHeader>],
    included: &[Vec<Option<usize>>],
    mut roots: Vec<(u32, usize)>,
) -> PreprocessorOrder {
    let mut out = PreprocessorOrder::default();
    // Definitions not yet undefined: `(delegate, from)` per name.
    let mut open: std::collections::HashMap<&str, Vec<(Option<&str>, u32)>> = std::collections::HashMap::new();
    let mut visited = vec![false; parsed.len()];
    roots.sort_by_key(|&(o, _)| o);
    for (o, root) in roots {
        if std::mem::replace(&mut visited[root], true) {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
        while let Some((h, i)) = stack.pop() {
            let Some(directive) = parsed[h].directives.get(i) else { continue };
            stack.push((h, i + 1));
            match directive {
                Directive::Define { name, delegate } => {
                    let defs = open.entry(name).or_default();
                    if !defs.iter().any(|(d, _)| *d == delegate.as_deref()) {
                        defs.push((delegate.as_deref(), o));
                    }
                }
                Directive::Undef(name) => {
                    let at = out.undefined_at.entry(name.clone()).or_default();
                    if at.last() != Some(&o) {
                        at.push(o);
                    }
                    // A definition from this same include line was made and
                    // undone inside it: it never reaches the gathering file.
                    for (d, from) in open.remove(name.as_str()).unwrap_or_default() {
                        if from < o {
                            push_span(&mut out.spans, name, GatheredSpan { from, until: Some(o), delegate: d.map(str::to_string) });
                        }
                    }
                }
                Directive::Include(k) => {
                    if let Some(c) = included[h][*k] {
                        if !std::mem::replace(&mut visited[c], true) {
                            stack.push((c, 0));
                        }
                    }
                }
            }
        }
    }
    for (name, defs) in open {
        for (d, from) in defs {
            push_span(&mut out.spans, name, GatheredSpan { from, until: None, delegate: d.map(str::to_string) });
        }
    }
    out
}

fn push_span(spans: &mut std::collections::HashMap<String, Vec<GatheredSpan>>, name: &str, span: GatheredSpan) {
    let v = spans.entry(name.to_string()).or_default();
    if !v.contains(&span) {
        v.push(span);
    }
}

/// Who included a frontier header: one of the gathering file's own
/// `#include` lines (its ordinal), or an already-walked header's `i`th
/// include.
#[derive(Clone, Copy)]
enum Parent {
    Line(u32),
    Header(usize, usize),
}

/// The persisted macro table's per-header validation stamp: a hash of
/// (mtime nanos, size). Whole-second mtimes miss two same-length writes
/// within one second (generated headers, rapid saves) — nanosecond
/// precision plus size closes that window. 0 if unreadable.
pub(super) fn file_stamp(path: &std::path::Path) -> i64 {
    use std::hash::{Hash, Hasher};
    let Ok(meta) = std::fs::metadata(path) else { return 0 };
    let Ok(mtime) = meta.modified() else { return 0 };
    let nanos = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    nanos.hash(&mut h);
    meta.len().hash(&mut h);
    h.finish() as i64
}

/// A header's cached (macros + include edges), by (path, mtime). The cache
/// makes the per-edit re-gather cheap (warm hits skip read+parse).
fn header_info(canon: &std::path::Path, parser: &mut tree_sitter::Parser) -> Option<std::sync::Arc<CachedHeader>> {
    let mut build = |canon: &std::path::Path| -> Option<std::sync::Arc<CachedHeader>> {
        let src = std::fs::read_to_string(canon).ok()?;
        let tree = parser.parse(&src, None)?;
        let sites = include_sites_tree(&tree, &src);
        Some(std::sync::Arc::new(CachedHeader {
            macros: collect_macros(&tree, src.as_bytes()),
            directives: header_directives(&tree, &src, &sites),
            includes: sites.into_iter().map(|(p, _)| p).collect(),
        }))
    };
    // No mtime (metadata failed) ⇒ no stamp: compute uncached, as before.
    let Some(mtime) = std::fs::metadata(canon).and_then(|m| m.modified()).ok() else {
        return build(canon);
    };
    // Single-flight by (path, mtime): sibling TUs including the same header
    // (op.c/sv.c share most of theirs) wait for ONE read+parse, not N.
    header_cache().get_or_try_fill(canon.to_path_buf(), mtime, || {
        build(canon).map(|info| {
            let bytes = header_heap_bytes(&info);
            Fill::Store(info, bytes)
        })
    })
}

fn header_heap_bytes(h: &CachedHeader) -> usize {
    macro_table_heap_bytes(&h.macros)
        + strings_heap_bytes(&h.includes)
        + h.directives
            .iter()
            .map(|d| match d {
                Directive::Define { name, delegate } => name.len() + delegate.as_ref().map_or(0, String::len) + 56,
                Directive::Undef(name) => name.len() + 56,
                Directive::Include(_) => 56,
            })
            .sum::<usize>()
}

/// A header's own #defines + its include edges — cached by (path, mtime)
/// so the per-edit re-gather doesn't re-read + re-parse the same dozens of
/// transitive headers every keystroke (the server is long-lived; headers
/// rarely change mid-edit, and mtime invalidates when they do).
struct CachedHeader {
    macros: BTreeMap<String, Macro>,
    includes: Vec<String>,
    /// Its `#define`, unconditional `#undef` and `#include` lines in source
    /// order — what `replay_in_preprocessor_order` reads.
    directives: Vec<Directive>,
}

enum Directive {
    Define { name: String, delegate: Option<String> },
    Undef(String),
    /// The `i`th entry of `CachedHeader::includes`.
    Include(usize),
}

/// A header's directives in source order; `sites` are its includes as
/// `include_sites_tree` lists them.
fn header_directives(tree: &Tree, src: &str, sites: &[(String, usize)]) -> Vec<Directive> {
    let mut row_start = vec![0];
    row_start.extend(src.match_indices('\n').map(|(i, _)| i + 1));
    let mut at: Vec<(usize, Directive)> = Vec::new();
    walk_macro_defs(tree, src.as_bytes(), |name, m, (start, _)| {
        let byte = row_start.get(start.row).map_or(0, |b| b + start.column);
        at.push((byte, Directive::Define { delegate: m.delegate(), name }));
    });
    walk_macro_undefs(tree, src.as_bytes(), |name, node| at.push((node.start_byte(), Directive::Undef(name))));
    at.extend(sites.iter().enumerate().map(|(i, (_, byte))| (*byte, Directive::Include(i))));
    at.sort_by_key(|(byte, _)| *byte);
    at.into_iter().map(|(_, d)| d).collect()
}

fn header_cache(
) -> &'static GatherCache<std::path::PathBuf, std::time::SystemTime, std::sync::Arc<CachedHeader>> {
    static C: OnceLock<
        GatherCache<std::path::PathBuf, std::time::SystemTime, std::sync::Arc<CachedHeader>>,
    > = OnceLock::new();
    C.get_or_init(|| GatherCache::new_labeled(gather_cap_bytes(HEADER_CACHE_MB), "gather-header"))
}

/// The default C/C++ toolchain's discovered surface (system include roots +
/// predefined macros), probed once via the compiler and cached process-globally
/// (`OnceLock`). `None` when no compiler is on PATH — include resolution then
/// degrades to workspace-only (today's behavior). Probed as C++ so `include_dirs`
/// is the SUPERSET that also resolves the C system headers (`<sys/mman.h>`);
/// `predefined_macros` rides along for the `#if`-eval consumer.
pub fn toolchain_info() -> Option<&'static crate::build::cpp_toolchain::ToolchainInfo> {
    static INFO: std::sync::OnceLock<Option<crate::build::cpp_toolchain::ToolchainInfo>> =
        std::sync::OnceLock::new();
    INFO.get_or_init(|| {
        crate::build::cpp_toolchain::default_compiler(crate::build::cpp_toolchain::Lang::Cpp)
            .and_then(|c| crate::build::cpp_toolchain::probe(&c, None))
    })
    .as_ref()
}

/// Identity of the analysis-input toolchain: compiler version + system
/// include roots + predefined macros, or a distinct sentinel when the probe
/// failed. Rides every persist-tier validation key (macro tables, the
/// pack modules DB) so a degraded generation — probe failure silently
/// emptying the system include roots — can never freeze into the cache
/// and be re-served after the toolchain comes back.
pub fn toolchain_fingerprint() -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match toolchain_info() {
        Some(t) => {
            t.compiler_version.hash(&mut h);
            t.include_dirs.hash(&mut h);
            t.predefined_macros.hash(&mut h);
        }
        None => "no-toolchain".hash(&mut h),
    }
    h.finish()
}

/// System/stdlib include roots, in compiler search order — the `<...>` fallback
/// for headers no workspace ancestor holds (`<sys/mman.h>`). Empty when no
/// compiler was found.
fn system_include_dirs() -> &'static [std::path::PathBuf] {
    static DIRS: std::sync::OnceLock<Vec<std::path::PathBuf>> = std::sync::OnceLock::new();
    DIRS.get_or_init(|| toolchain_info().map(|t| t.include_dirs.clone()).unwrap_or_default())
}

/// Resolve an include like `spdlog/common.h` or `<sys/mman.h>` to a real path.
/// Workspace-first: walk up from the file's dir, first ancestor `R` where
/// `R/<inc>` exists wins (project/relative headers, quoted or angle-bracket).
/// Only when no ancestor has it do the toolchain's system roots answer — so a
/// system `<sys/mman.h>` resolves (its subtree was silently lost before), while
/// a project header still shadows a same-named system one.
fn resolve_include(file_path: &std::path::Path, inc: &str) -> Option<std::path::PathBuf> {
    if let Some(mut dir) = file_path.parent() {
        loop {
            let cand = dir.join(inc);
            if cand.is_file() {
                return Some(cand);
            }
            // The conventional `-Iinclude` layout: a test/src file spelling
            // `#include "fmt/format.h"` reaches `<root>/include/fmt/format.h`.
            // Without this the whole test/src tree gets an empty project
            // closure and the visibility gate cuts it off from every target.
            let cand = dir.join("include").join(inc);
            if cand.is_file() {
                return Some(cand);
            }
            match dir.parent() {
                Some(p) => dir = p,
                None => break,
            }
        }
    }
    for root in system_include_dirs() {
        let cand = root.join(inc);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// Resolve one `#include` path token (quoted or angle-bracket) to a real file,
/// workspace-first then toolchain roots. The public seam for goto-def on an
/// `#include` path token (`FileAnalysis::include_directives`).
pub fn resolve_include_path(file_path: &std::path::Path, inc: &str) -> Option<std::path::PathBuf> {
    resolve_include(file_path, inc)
}

/// Every `#include` directive's raw path text, by a cheap per-line scan (no
/// parse) — the header BFS only needs the paths, so this stays far lighter than
/// `header_info`'s full tree parse. Quoted `"x.h"` and angle-bracket `<x.h>`
/// alike (the walk-up resolver finds project headers written either way).
fn scan_include_directives(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in src.lines() {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix('#') else { continue };
        let rest = rest.trim_start();
        let Some(rest) = rest.strip_prefix("include") else { continue };
        let rest = rest.trim_start();
        let path = match rest.as_bytes().first() {
            Some(b'"') => rest[1..].split('"').next(),
            Some(b'<') => rest[1..].split('>').next(),
            _ => None,
        };
        if let Some(p) = path {
            if !p.is_empty() {
                out.push(p.to_string());
            }
        }
    }
    out
}

fn include_closure_cache(
) -> &'static GatherCache<std::path::PathBuf, u64, std::sync::Arc<Vec<String>>> {
    static C: OnceLock<GatherCache<std::path::PathBuf, u64, std::sync::Arc<Vec<String>>>> =
        OnceLock::new();
    C.get_or_init(|| {
        GatherCache::new_labeled(gather_cap_bytes(INCLUDE_CLOSURE_CACHE_MB), "gather-include-closure")
    })
}

/// The transitive `#include` closure of `file_path`, as canonical path strings
/// (sorted, unique) — the cross-file VISIBILITY key (`docs/adr/macro-handling.md`,
/// "the include-closure lie"): a name resolves preferentially to a definition in
/// a file this set reaches. BFS over the include graph via a cheap line scan;
/// memoized by (path, include-set hash) so the per-edit re-analyze is warm.
///
/// Respects the on-open cached-only gate: on open an unwarmed file returns an
/// empty closure — like cross-file macros, the background re-analyze fills it —
/// so the first open never blocks on the cold header walk. An empty closure is
/// safe: the visibility ranking degrades to the global winner (today's behavior).
///
/// The bool is COMPLETENESS: `false` when the closure is a placeholder (the
/// cached-only skip) or was truncated by a header that RESOLVED and exists yet
/// failed to read (non-UTF-8, transient I/O). A truncated closure is the one
/// blind spot of the `deps_stamp` persist key — the stamp is recomputed over
/// the STORED list at load time, so it self-validates whatever subset was
/// frozen and never re-derives (`module_cache::closure_stamp`). The driver
/// folds `!complete` into `degraded` so `save_to_db` refuses the row; a
/// complete gather next session re-derives it. An UNRESOLVED include (a system
/// header off the search path) is NOT incompleteness — it's a legitimate
/// closure boundary, deterministic across runs.
pub fn include_closure(file_path: &std::path::Path, src: &str) -> (Vec<String>, bool) {
    let key = file_path.to_path_buf();
    let inc_hash = include_set_hash(src);
    // The walk runs single-flight on a miss. A hit or a freshly-stored (COMPLETE)
    // closure resolves `Cached`; the cached-only placeholder and a truncated
    // closure resolve `Transient` (returned, never cached) → `complete = false`.
    let (arc, res) = include_closure_cache().resolve(key, inc_hash, || {
        if gather_cached_only() {
            // on-open placeholder: fill on background re-analyze
            return Some(Fill::Transient(std::sync::Arc::new(Vec::new())));
        }
        let mut seen = std::collections::HashSet::new();
        if let Ok(p) = file_path.canonicalize() {
            seen.insert(p);
        }
        let mut out: Vec<String> = Vec::new();
        let mut complete = true;
        let mut frontier: Vec<std::path::PathBuf> = scan_include_directives(src)
            .iter()
            .filter_map(|inc| resolve_include(file_path, inc))
            .collect();
        while !frontier.is_empty() {
            let mut next: Vec<std::path::PathBuf> = Vec::new();
            for path in frontier.drain(..) {
                let Ok(canon) = path.canonicalize() else { continue };
                if !seen.insert(canon.clone()) {
                    continue;
                }
                out.push(canon.to_string_lossy().into_owned());
                match std::fs::read_to_string(&canon) {
                    Ok(hsrc) => {
                        for inc in scan_include_directives(&hsrc) {
                            if let Some(nx) = resolve_include(&canon, &inc) {
                                next.push(nx);
                            }
                        }
                    }
                    // The header canonicalized (exists) but couldn't be read: its
                    // transitive includes are silently dropped, truncating the
                    // closure. Mark incomplete so the analysis isn't frozen.
                    Err(_) => complete = false,
                }
            }
            frontier = next;
        }
        out.sort();
        out.dedup();
        let arc = std::sync::Arc::new(out);
        // Only memoize a COMPLETE closure: a transient truncation must re-gather
        // next call, not stick in the in-session cache.
        if complete {
            let bytes = strings_heap_bytes(&arc);
            Some(Fill::Store(arc, bytes))
        } else {
            Some(Fill::Transient(arc))
        }
    });
    let complete = matches!(res, Resolution::Cached);
    (arc.map(|a| (*a).clone()).unwrap_or_default(), complete)
}

/// Drop every per-file analysis cache entry for the given files (CANONICAL
/// paths): the tier-1 macro table, its pre-expanded variants, the include
/// closure, and the header parse cache. The in-session invalidation seam —
/// a saved/changed pack file evicts itself + every consumer whose closure
/// contains it, so the next analyze re-gathers instead of serving the
/// frozen table (cache keys are whatever path `analyze_with_path` got, so
/// membership is checked on the canonicalized key).
pub fn evict_analysis_caches(files: &std::collections::HashSet<std::path::PathBuf>) {
    evict_gather_caches(files, true);
}

/// Residency-only eviction for the bulk workspace index: drop the per-file
/// merged/expanded macro tables (`macro_table_cache`, `pre_expanded_cache`) +
/// the closure memo for files whose `FileAnalysis` is already built and
/// persisted, but keep `header_cache` warm. The per-file tables are a private
/// memo of each source file's include-closure merge — never read by any other
/// file's gather (that only consults `header_cache`), disk-backed, and cheaply
/// re-derived from the warm shared header table on a later on-edit re-gather.
/// See `docs/adr/memory-slice-2-lru.md`. Content-edit invalidation
/// must NOT use this — a changed header's own `header_cache` entry has to go,
/// so that path calls `evict_analysis_caches` (drops headers too).
pub fn evict_gather_caches_keep_headers(files: &std::collections::HashSet<std::path::PathBuf>) {
    evict_gather_caches(files, false);
}

fn evict_gather_caches(files: &std::collections::HashSet<std::path::PathBuf>, drop_headers: bool) {
    let hit = |key: &std::path::PathBuf| {
        files.contains(key)
            || key
                .canonicalize()
                .map(|c| files.contains(&c))
                .unwrap_or(false)
    };
    // `invalidate` drops matching entries AND cancels any in-flight compute for
    // them (a claimant's stale result is discarded on publish; a waiter
    // recomputes) — no deadlock, it only touches the state lock.
    macro_table_cache().invalidate(&hit);
    pre_expanded_cache().invalidate(&hit);
    include_closure_cache().invalidate(&hit);
    if drop_headers {
        header_cache().invalidate(&hit);
    }
}

/// Measurement aid (gated by callers behind `PERL_LSP_MEM_REPORT`): a rough
/// resident-byte estimate of the four process-global gather caches. Counts the
/// heap payload of each `String`/`Vec` (capacity), not `size_of` overhead, so
/// the numbers track the actual macro-table blow-up. NOT wired into any query
/// path — a diagnostic only.
pub fn cache_size_report() -> String {
    // The caches now byte-account at insert time, so the resident footprint is
    // read straight off each `GatherCache` (`macro_table_heap_bytes` etc. are
    // the same estimators these totals were summed with).
    let (mt_n, mt_b) = macro_table_cache().stats();
    let (hc_n, hc_b) = header_cache().stats();
    // pre_expanded's `raw` Arc is SHARED with macro_table_cache (same
    // allocation) — its total counts only the ADDED full+alias variants.
    let (pe_n, pe_b) = pre_expanded_cache().stats();
    let (ic_n, ic_b) = include_closure_cache().stats();
    let mb = |b: usize| b as f64 / 1_048_576.0;
    format!(
        "cpp gather caches (heap payload est.):\n  header_cache:       {hc_n:>6} headers, {:>8.1} MB (shared across files)\n  macro_table_cache:  {mt_n:>6} files,   {:>8.1} MB (raw merged table, Arc-shared w/ pre_expanded)\n  pre_expanded_cache: {pe_n:>6} files,   {:>8.1} MB (full+alias expanded variants, ON TOP of raw)\n  include_closure:    {ic_n:>6} files,   {:>8.1} MB\n  TOTAL: {:>8.1} MB",
        mb(hc_b), mb(mt_b), mb(pe_b), mb(ic_b), mb(hc_b + mt_b + pe_b + ic_b)
    )
}

fn include_sites(src: &str, parser: &mut tree_sitter::Parser) -> Vec<(String, usize)> {
    match parser.parse(src, None) {
        Some(tree) => include_sites_tree(&tree, src),
        None => Vec::new(),
    }
}

/// Every include's path, quoted (`"x/y.h"`) and angle-bracket
/// (`<lib/y.h>`) alike, with its path token's byte offset — library
/// headers write project includes with `<>`, and the walk-up resolver finds
/// both (true system headers like `<vector>` simply don't resolve in the
/// workspace, and are skipped).
fn include_sites_tree(tree: &Tree, src: &str) -> Vec<(String, usize)> {
    let q = cached_query(&INCLUDE_Q, &tree.language(), INCLUDE_QUERY);
    let names = q.capture_names().to_vec();
    let mut out = Vec::new();
    let mut cursor = QueryCursor::new();
    let mut it = cursor.matches(q, tree.root_node(), src.as_bytes());
    while let Some(m) = it.next() {
        for c in m.captures {
            let Ok(t) = c.node.utf8_text(src.as_bytes()) else { continue };
            match names[c.index as usize] {
                "p" => out.push((t.to_string(), c.node.start_byte())),
                "s" => out.push((t.trim_start_matches('<').trim_end_matches('>').to_string(), c.node.start_byte())),
                _ => {}
            }
        }
    }
    out
}

/// An object-like macro whose body is a single bare identifier — a pure
/// rename (`op_prune_chain_head → Perl_op_prune_chain_head`). Expanding it
/// is provably parse-safe (an identifier replaces an identifier; the token
/// structure is unchanged), so it can be kept even when the full
/// expansion's validate gate rejects the file.
pub(super) fn is_identifier_alias(m: &Macro) -> bool {
    m.params.is_none()
        && !m.body.is_empty()
        && m.body.bytes().all(is_ident_byte)
}
