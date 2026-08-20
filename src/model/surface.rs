//! The span-free cross-file Surface (`docs/adr/storage-engine.md`).
//!
//! A position-independent projection of one file's cross-file-VISIBLE facts.
//! Equality of two Surfaces means "no cross-file-visible change": a body
//! edit, a reformat, a comment, a private-local rename must yield an EQUAL
//! Surface — that equality is the early-cutoff firewall the freshness engine
//! gates on (rebuild → Surface equal? → stop; else re-enrich exactly the
//! dirty consumers). One smuggled span collapses the firewall silently, so:
//!
//! - **No spans, no `Point`s, no byte offsets, no `ScopeId`/`SymbolId`/
//!   `RefIdx`, anywhere.** Every one of those shifts on unrelated edits.
//!   The equality tests are the regression net; a Surface field addition
//!   without an equality test is a review reject (`docs/adr/storage-engine.md`).
//!   The FileAnalysis→Surface direction is compiler-enforced:
//!   `FileAnalysis::surface_feed` destructures every field with no `..`,
//!   so a new field cannot compile until classified as projected or
//!   reasoned-not-visible.
//! - **Typed fields, not display strings** — `Option<InferredType>`, never
//!   `"returns Foo"` (rule #10's lossy-string form). File-internal
//!   attachment identities inside a type (a `CodeRef` body edge) are
//!   sanitized by `despan` below.
//! - **Canonical ordering.** Everything is sorted so builder iteration
//!   order can never masquerade as a semantic change.
//!
//! The Surface is NOT the outline: `documentSymbol` is span-bearing and
//! type-blind — riding it would both under-invalidate (return-type/body
//! `@ISA` edits keep the symbol list identical) and over-invalidate (every
//! sub moves on reformat). The Surface is the lower, position-independent
//! layer; the outline stays a span-bearing sibling.

use serde::{Deserialize, Serialize};

use crate::model::file_analysis::{
    FileAnalysis, HashKeyOwner, InferredType, ParametricType, SymKind,
};

/// One package/class/namespace's cross-file-visible facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PackageSurface {
    pub name: String,
    /// Resolved isa/roles/loaded components, post-fold (`PackageFacts::parents`).
    pub parents: Vec<String>,
    /// Is this package a role (plugin-declared role-maker verdict)?
    pub is_role: bool,
    /// Cross-file-callable members, sorted by (name, kind).
    pub methods: Vec<MethodSurface>,
    /// Cross-file-visible non-callables owned by this package (class
    /// fields, named-enum constants, `our` globals), sorted by (name, kind).
    pub values: Vec<ValueSurface>,
}

/// One callable's cross-file-visible contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MethodSurface {
    pub name: String,
    /// `SymKind` discriminant via `sym_kind_code` — a method vs sub vs
    /// handler distinction IS cross-file-visible (dispatch differs).
    pub kind: u8,
    /// Declared arity (total, required, variadic) when the language mints
    /// it — the overload-ranking axis.
    pub arity: Option<(usize, usize, bool)>,
    /// The bag-resolved return type, `despan`ned. Local conclusion only
    /// (no module index at projection time): a cross-file-dependent return
    /// that resolves to `None` here is honest — the consumer's enrichment
    /// re-asks with an index.
    pub ret: Option<InferredType>,
    /// Hash keys owned by this sub (`HashKeyOwner::Sub`) — the
    /// imported-hash-key completion surface.
    pub hash_keys: Vec<String>,
}

/// One cross-file-visible non-callable: a C global / enum constant /
/// struct field, a Perl `our` package global. Adding, removing, or
/// re-kinding one IS a cross-file change (member access, bare-value
/// reads, goto-def all see it) — without these the cpp firewall reads a
/// new global as Unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValueSurface {
    pub name: String,
    /// `SymKind` discriminant via `sym_kind_code`.
    pub kind: u8,
}

/// One `#define`'s cross-file-visible contract. Under textual inclusion
/// the BODY is semantics for every consumer (an expansion change with an
/// unchanged name would silently under-invalidate without it), and the
/// guard trail decides which definition a config sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MacroSurface {
    pub name: String,
    pub params: Option<Vec<String>>,
    pub body: String,
    pub guards: Vec<String>,
}

/// The whole file's span-free cross-file surface. `Default` is the empty
/// surface (a file exporting nothing).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Surface {
    pub packages: Vec<PackageSurface>,
    /// Cross-file-visible values with no owning package (C file-scope
    /// globals / anonymous-enum constants), sorted.
    pub free_values: Vec<ValueSurface>,
    /// Package-less callables — C's dominant export shape (free functions;
    /// also file-scope Perl subs in packageless scripts). Sorted like
    /// package methods.
    pub free_methods: Vec<MethodSurface>,
    /// `#define`s, sorted by (name, guards) — config variants of one name
    /// each surface.
    pub macros: Vec<MacroSurface>,
    /// Raw `#include` specs (pack languages), sorted. A header adding or
    /// dropping an include changes every consumer's transitive closure —
    /// cross-file-visible even though nothing else moved.
    pub includes: Vec<String>,
    /// Modules this file loads (`use`/`require`/plugin loads) — the
    /// DEPENDENCY half of the freshness edge: this file's enrichment
    /// depends on the Surface of each import ∪ parent ∪ bridge.
    pub imports: Vec<String>,
    pub exports: Vec<String>,
    pub exports_ok: Vec<String>,
    /// `%EXPORT_TAGS` membership, tag → sorted members. A member moving
    /// between tags (or a tag rename) changes what a consumer's
    /// `use Foo qw(:tag)` binds even when the flat `exports_ok` set is
    /// unchanged — the grouping itself is cross-file semantics.
    pub export_tags: Vec<(String, Vec<String>)>,
    pub reexports: Vec<String>,
    /// Classes plugin namespaces in THIS file bridge content onto.
    pub plugin_bridges: Vec<String>,
    /// Manifest-declared app-surface consumer classes.
    pub app_surface_consumers: Vec<String>,
    /// DBIC `source_name` override — the registered source moniker when it
    /// differs from the class basename. Consumers' `resultset('X')`
    /// resolve through it, so an edit is cross-file-visible with no other
    /// projected change.
    pub dbic_source_name: Option<String>,
}

impl Surface {
    /// Project `fa`'s surface. Runs right after `finalize_post_walk()` —
    /// the bag is present (return types resolve) and enrichment has NOT
    /// run (the surface is the file's OWN facts, never its imports').
    ///
    /// All field access routes through `surface_feed()` — the exhaustive
    /// classification gate that makes a new `FileAnalysis` field a compile
    /// error until its cross-file visibility is decided.
    pub fn project(fa: &FileAnalysis) -> Surface {
        let feed: crate::model::file_analysis::SurfaceFeed = fa.surface_feed();
        let mut by_pkg: std::collections::BTreeMap<String, PackageSurface> =
            std::collections::BTreeMap::new();
        let mut free_values: Vec<ValueSurface> = Vec::new();
        let mut free_methods: Vec<MethodSurface> = Vec::new();
        // One pass over the (few) HashKeyDef symbols instead of an
        // O(symbols) scan per sub — projection runs per registration.
        let hash_key_defs: Vec<&crate::model::file_analysis::Symbol> = feed
            .symbols
            .iter()
            .filter(|s| {
                matches!(s.detail, crate::model::file_analysis::SymbolDetail::HashKeyDef { .. })
            })
            .collect();
        // Every package this file records facts about exists on the
        // surface even if it declares no callable members. One entry read
        // per package covers both projected lanes; the exhaustive
        // destructure is the per-package half of R1's classification gate
        // (`surface_feed.rs`) — a new `PackageFacts` field fails to
        // compile here until its cross-file visibility is decided.
        for (pkg, facts) in feed.packages {
            let crate::model::file_analysis::PackageFacts {
                // ---- Cross-file-visible: projected below.
                parents,
                is_role,
                // ---- File-internal: shape THIS file's own answers, not
                // what another file observes of it.
                uses: _uses, // per-package plugin-trigger view; the `use` edges themselves ride `imports`
                framework: _framework, // framework return folds → `ret`; synthesized accessors are already `symbols`
                requires: _requires, // required names synthesize contract-marker Method symbols that already project
                dynamic_parents: _dynamic_parents, // honest-silence gate for this file's own diagnostics; resolvable edges ride `parents`
            } = facts;
            let entry = by_pkg.entry(pkg.clone()).or_insert_with(|| PackageSurface {
                name: pkg.clone(),
                ..Default::default()
            });
            let mut parents = parents.clone();
            parents.sort_unstable();
            parents.dedup();
            entry.parents = parents;
            entry.is_role = *is_role;
        }
        for sym in feed.symbols {
            match sym.kind {
                SymKind::Package | SymKind::Class | SymKind::Module => {
                    by_pkg.entry(sym.name.clone()).or_insert_with(|| PackageSurface {
                        name: sym.name.clone(),
                        ..Default::default()
                    });
                }
                SymKind::Sub | SymKind::Method | SymKind::Handler => {
                    // Cross-file-visible only: lexical subs aren't
                    // addressable outside their block.
                    if matches!(
                        &sym.detail,
                        crate::model::file_analysis::SymbolDetail::Sub { lexical: true, .. }
                    ) {
                        continue;
                    }
                    let owner = HashKeyOwner::Sub {
                        package: sym.package.clone(),
                        name: sym.name.clone(),
                    };
                    let hash_keys: Vec<String> = {
                        let mut ks: Vec<String> = hash_key_defs
                            .iter()
                            .filter(|s| {
                                if let crate::model::file_analysis::SymbolDetail::HashKeyDef {
                                    owner: ref o,
                                    ..
                                } = s.detail
                                {
                                    o.found_by(&owner)
                                } else {
                                    false
                                }
                            })
                            .map(|s| s.name.clone())
                            .collect();
                        ks.sort_unstable();
                        ks.dedup();
                        ks
                    };
                    let m = MethodSurface {
                        name: sym.name.clone(),
                        kind: crate::model::file_analysis::sym_kind_code(&sym.kind),
                        arity: sym
                            .param_arity()
                            .map(|a| (a.total, a.required, a.variadic)),
                        ret: feed
                            .analysis
                            .symbol_return_type_via_bag(sym.id, None)
                            .map(|t| despan(&t)),
                        hash_keys,
                    };
                    match sym.package.clone() {
                        // Package-less callables (C free functions, subs in
                        // packageless scripts) are cross-file-visible too —
                        // for C they're MOST of the surface.
                        None => free_methods.push(m),
                        Some(pkg) => {
                            by_pkg
                                .entry(pkg.clone())
                                .or_insert_with(|| PackageSurface {
                                    name: pkg,
                                    ..Default::default()
                                })
                                .methods
                                .push(m);
                        }
                    }
                }
                SymKind::Variable | SymKind::Field | SymKind::Enumerator => {
                    // Cross-file-visible values: the C-linkage file-scope
                    // gate OR class content (fields / named-enum constants
                    // reachable via member access).
                    if !(feed.analysis.is_linkage_visible(sym)
                        || feed.analysis.symbol_is_class_content(sym))
                    {
                        continue;
                    }
                    let v = ValueSurface {
                        name: sym.name.clone(),
                        kind: crate::model::file_analysis::sym_kind_code(&sym.kind),
                    };
                    match sym.package.clone() {
                        Some(pkg) => {
                            by_pkg
                                .entry(pkg.clone())
                                .or_insert_with(|| PackageSurface {
                                    name: pkg,
                                    ..Default::default()
                                })
                                .values
                                .push(v);
                        }
                        None => free_values.push(v),
                    }
                }
                _ => {}
            }
        }
        let mut packages: Vec<PackageSurface> = by_pkg.into_values().collect();
        let sort_values = |vs: &mut Vec<ValueSurface>| {
            vs.sort_by(|a, b| (&a.name, a.kind).cmp(&(&b.name, b.kind)));
            vs.dedup_by(|a, b| a.name == b.name && a.kind == b.kind);
        };
        // Duplicate names collapse only when FULLY equal (rw accessor
        // pairs). Name-only dedup would hide a contract edit to the
        // shadowed duplicate — Perl dispatches the LAST definition, so a
        // change to either must flip equality. Stable sort keeps builder
        // order among name-equal entries: deterministic per source.
        let sort_methods = |ms: &mut Vec<MethodSurface>| {
            ms.sort_by(|a, b| (&a.name, a.kind).cmp(&(&b.name, b.kind)));
            ms.dedup();
        };
        for p in &mut packages {
            sort_methods(&mut p.methods);
            sort_values(&mut p.values);
        }
        sort_values(&mut free_values);
        sort_methods(&mut free_methods);
        let mut imports: Vec<String> = feed
            .imports
            .iter()
            .map(|i| i.module_name.clone())
            .chain(feed.plugin_loads.iter().map(|f| f.name.clone()))
            .collect();
        imports.sort_unstable();
        imports.dedup();
        let sorted = |v: &[String]| {
            let mut v = v.to_vec();
            v.sort_unstable();
            v.dedup();
            v
        };
        let mut plugin_bridges: Vec<String> = feed
            .plugin_namespaces
            .iter()
            .flat_map(|ns| {
                ns.bridges.iter().map(|b| {
                    let crate::model::file_analysis::Bridge::Class(c) = b;
                    c.clone()
                })
            })
            .collect();
        plugin_bridges.sort_unstable();
        plugin_bridges.dedup();
        let mut macros: Vec<MacroSurface> = feed
            .macro_defs
            .iter()
            .map(|m| MacroSurface {
                name: m.name.clone(),
                params: m.params.clone(),
                body: m.body.clone(),
                guards: m.guards.clone(),
            })
            .collect();
        macros.sort_by(|a, b| (&a.name, &a.guards).cmp(&(&b.name, &b.guards)));
        macros.dedup();
        let mut includes: Vec<String> =
            feed.include_directives.iter().map(|(_, raw)| raw.clone()).collect();
        includes.sort_unstable();
        includes.dedup();
        let mut export_tags: Vec<(String, Vec<String>)> = feed
            .export_tags
            .iter()
            .map(|(tag, members)| (tag.clone(), sorted(members)))
            .collect();
        export_tags.sort_by(|a, b| a.0.cmp(&b.0));
        Surface {
            packages,
            free_values,
            free_methods,
            macros,
            includes,
            imports,
            exports: sorted(feed.export),
            exports_ok: sorted(feed.export_ok),
            export_tags,
            reexports: sorted(feed.reexport_modules),
            plugin_bridges,
            app_surface_consumers: sorted(feed.app_surface_consumers),
            dbic_source_name: feed.dbic_source_name.clone(),
        }
    }
}

/// Strip file-internal identities out of an `InferredType` so the surface
/// value is position-independent. The one offender is `CodeRef`'s
/// `return_edge`: an `Expr(span)` (or any other file-internal attachment)
/// shifts on unrelated edits AND is meaningless to another file — only the
/// `PackageSymbol` edge is both stable and cross-file-resolvable. Container
/// variants recurse.
fn despan(t: &InferredType) -> InferredType {
    use crate::model::witnesses::WitnessAttachment;
    match t {
        InferredType::CodeRef { return_edge } => InferredType::CodeRef {
            return_edge: match return_edge {
                Some(WitnessAttachment::PackageSymbol { .. }) => return_edge.clone(),
                _ => None,
            },
        },
        InferredType::Sequence(items) => {
            InferredType::Sequence(items.iter().map(despan).collect())
        }
        InferredType::TypeConstraintOf(inner) => {
            InferredType::TypeConstraintOf(Box::new(despan(inner)))
        }
        InferredType::Optional(inner) => InferredType::Optional(Box::new(despan(inner))),
        InferredType::HashWithKeys { keys, open } => InferredType::HashWithKeys {
            keys: keys
                .iter()
                .map(|(k, v)| (k.clone(), v.as_ref().map(|t| Box::new(despan(t)))))
                .collect(),
            open: *open,
        },
        InferredType::Parametric(p) => InferredType::Parametric(match p {
            ParametricType::ResultSet { .. } => p.clone(),
            ParametricType::Instance { base, args } => ParametricType::Instance {
                base: base.clone(),
                args: args.iter().map(despan).collect(),
            },
        }),
        other => other.clone(),
    }
}

/// The verdict `FreshnessIndex::record` hands back — what a rebuild of one
/// file means for everyone else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceVerdict {
    /// First sighting — no prior surface to compare (startup registration).
    FirstSeen,
    /// Surface equal: a body edit / reformat / comment. NOTHING cross-file
    /// changed — consumers stay fresh, the walk stops here.
    Unchanged,
    /// Cross-file-visible change — `dirty_consumers` names who must
    /// re-enrich.
    Changed,
}

/// What the index retains per file: enough to answer "did the surface
/// change?" (the fingerprint) and "who consumes what this file provides?"
/// (the provided names) — NOT the surface itself. Full surfaces resident
/// for every indexed file would rebuild the payload the eviction axes
/// stripped (cpp macro BODIES ride the surface); persistence of full
/// surfaces belongs to the warm-start blob, not this index.
struct SurfaceRecord {
    fingerprint: u64,
    /// Names the file provides (its declared packages).
    provided: Vec<String>,
    /// Names the LAST re-record dropped (renamed/deleted packages).
    /// Consumers of a departed name are exactly the ones its removal
    /// breaks, so the dirty walk seeds from `provided ∪ stale_provided`
    /// until the next re-record replaces the set.
    stale_provided: Vec<String>,
}

/// Process-local surface identity. In-memory only (SipHash keys are
/// per-process) — the persisted form carries its own stable encoding.
/// Streams the serialization straight into the hasher: record() runs per
/// keystroke on open docs, so no intermediate buffer.
fn surface_fingerprint(s: &Surface) -> u64 {
    use std::hash::Hasher;
    struct HashWriter(std::collections::hash_map::DefaultHasher);
    impl std::io::Write for HashWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.write(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut w = HashWriter(std::collections::hash_map::DefaultHasher::new());
    let _ = bincode::serialize_into(&mut w, s);
    w.0.finish()
}

/// The freshness engine (`docs/adr/storage-engine.md`): per-file
/// surface records + a name-keyed reverse-dependency index. The
/// dependency edge is DECLARED
/// by the consumer's own surface — file F depends on every name in its
/// imports ∪ parents ∪ bridges — and the dirty walk is provider-name →
/// consumers, transitive with a seen-set (C's change dirties B extends C,
/// which dirties A importing B, because A's enrichment reads through B).
#[derive(Default)]
pub struct FreshnessIndex {
    surfaces: dashmap::DashMap<std::path::PathBuf, SurfaceRecord>,
    /// provider NAME (package/module) → consumer paths.
    consumers: dashmap::DashMap<String, std::collections::HashSet<std::path::PathBuf>>,
    /// consumer path → the provider names it last declared edges to
    /// (the removal half — edges must not accumulate across re-records).
    deps_of: dashmap::DashMap<std::path::PathBuf, Vec<String>>,
    /// Monotone count of MUTATING writes (Changed/FirstSeen records and
    /// removes; an Unchanged record touches nothing and doesn't count).
    /// One leg of the enrichment-key memo's validity epoch: any freshness
    /// mutation funnels through `record`/`remove` by construction, so the
    /// bump cannot be forgotten at a call site.
    writes: std::sync::atomic::AtomicU64,
}

impl FreshnessIndex {
    /// Names `s` DEPENDS on: its imports, every package's parents, and the
    /// classes its plugins bridge onto.
    fn dep_names(s: &Surface) -> Vec<String> {
        let mut names: Vec<String> = s.imports.clone();
        for p in &s.packages {
            names.extend(p.parents.iter().cloned());
        }
        names.extend(s.plugin_bridges.iter().cloned());
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Names `s` PROVIDES: its declared packages (the keys consumers'
    /// edges point at — Perl imports/extends by package name).
    fn provided_names(s: &Surface) -> impl Iterator<Item = &str> {
        s.packages.iter().map(|p| p.name.as_str())
    }

    /// Record `path`'s freshly-built surface; maintain its outgoing edges;
    /// return what changed. Call with the WHOLE analysis's projection at
    /// registration/rebuild time.
    pub fn record(&self, path: &std::path::Path, surface: Surface) -> SurfaceVerdict {
        let fingerprint = surface_fingerprint(&surface);
        let verdict = match self.surfaces.get(path) {
            None => SurfaceVerdict::FirstSeen,
            Some(old) if old.fingerprint == fingerprint => SurfaceVerdict::Unchanged,
            Some(_) => SurfaceVerdict::Changed,
        };
        if verdict != SurfaceVerdict::Unchanged {
            self.writes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let new_deps = Self::dep_names(&surface);
            let old_deps = self
                .deps_of
                .insert(path.to_path_buf(), new_deps.clone())
                .unwrap_or_default();
            for gone in old_deps.iter().filter(|d| !new_deps.contains(d)) {
                if let Some(mut set) = self.consumers.get_mut(gone) {
                    set.remove(path);
                }
            }
            for dep in &new_deps {
                self.consumers
                    .entry(dep.clone())
                    .or_default()
                    .insert(path.to_path_buf());
            }
            let provided: Vec<String> =
                Self::provided_names(&surface).map(str::to_owned).collect();
            let stale_provided: Vec<String> = match self.surfaces.get(path) {
                Some(old) => old
                    .provided
                    .iter()
                    .filter(|n| !provided.contains(n))
                    .cloned()
                    .collect(),
                None => Vec::new(),
            };
            self.surfaces.insert(
                path.to_path_buf(),
                SurfaceRecord { fingerprint, provided, stale_provided },
            );
        }
        verdict
    }

    /// The last-recorded fingerprint for `path` — the enrichment overlay's
    /// validity key half (a consumer's enriched copy is valid only while
    /// its own AND every dep's fingerprint stand).
    pub fn fingerprint_of(&self, path: &std::path::Path) -> Option<u64> {
        self.surfaces.get(path).map(|r| r.fingerprint)
    }

    /// The provider names `path` last declared edges to (sorted, deduped)
    /// — the other half of the overlay key: enrichment reads exactly
    /// these providers' surfaces.
    pub fn deps_of_names(&self, path: &std::path::Path) -> Vec<String> {
        self.deps_of.get(path).map(|v| v.clone()).unwrap_or_default()
    }

    /// The mutating-write count — see the `writes` field.
    pub fn write_count(&self) -> u64 {
        self.writes.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Drop a deleted file's record and edges.
    pub fn remove(&self, path: &std::path::Path) {
        self.writes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.surfaces.remove(path);
        if let Some((_, deps)) = self.deps_of.remove(path) {
            for d in deps {
                if let Some(mut set) = self.consumers.get_mut(&d) {
                    set.remove(path);
                }
            }
        }
    }

    /// The transitive dirty closure after `changed_path`'s surface changed:
    /// every file whose enrichment can observe it, walked provider-name →
    /// consumers with a seen-set (bounded, cycle-safe). The changed file
    /// itself is NOT in the set (its own rebuild triggered this).
    pub fn dirty_consumers(
        &self,
        changed_path: &std::path::Path,
    ) -> std::collections::HashSet<std::path::PathBuf> {
        let mut dirty: std::collections::HashSet<std::path::PathBuf> = Default::default();
        let mut frontier: Vec<String> = match self.surfaces.get(changed_path) {
            // stale_provided too: a RENAMED/DELETED package's consumers are
            // exactly the ones its departure broke.
            Some(r) => {
                r.provided.iter().chain(r.stale_provided.iter()).cloned().collect()
            }
            None => return dirty,
        };
        let mut seen_names: std::collections::HashSet<String> = frontier.iter().cloned().collect();
        while let Some(name) = frontier.pop() {
            let Some(consumers) = self.consumers.get(&name) else { continue };
            for c in consumers.iter() {
                if c == changed_path || !dirty.insert(c.clone()) {
                    continue;
                }
                // A dirty consumer's OWN providers propagate: its enriched
                // result feeds files that depend on IT.
                if let Some(r) = self.surfaces.get(c.as_path()) {
                    for p in &r.provided {
                        if seen_names.insert(p.clone()) {
                            frontier.push(p.clone());
                        }
                    }
                }
            }
        }
        dirty
    }
}

#[cfg(test)]
#[path = "surface_tests.rs"]
mod tests;
