//! The hierarchy projections: goto-type-definition, type hierarchy
//! (prepare/supertypes/subtypes), and call hierarchy (prepare/incoming/
//! outgoing). Each is a projection of machinery that already exists —
//! the witness bag's value types (`type_definitions`), the `GraphView`
//! inheritance edges (`supertypes`/`subtypes`), and the `references()`
//! image plus the in-body call refs (`incoming_calls`/`outgoing_calls`)
//! — never a new analysis walk. Incoming-call counts are the SAME
//! `references()` projection `--heatmap` fans in from, so the two can't
//! disagree (docs/adr/resolution-candidate-set.md).

use super::*;
use std::sync::Arc;

/// One node of a type/call hierarchy answer: a named declaration the
/// adapter can render as an LSP `TypeHierarchyItem`/`CallHierarchyItem`
/// (name + kind + declaring package + declaration location).
#[derive(Debug, Clone)]
pub struct HierarchyItem {
    pub name: String,
    pub kind: SymKind,
    /// Declaring package, when the declaration carries one.
    pub detail: Option<String>,
    pub location: RefLocation,
}

/// One edge of a call-hierarchy answer: the other endpoint (`item` — the
/// caller for incoming, the callee for outgoing) plus the call-site spans.
/// Incoming sites live in the CALLER's file (`item.location.key`);
/// outgoing sites live in the queried sub's own file.
#[derive(Debug, Clone)]
pub struct CallEdge {
    pub item: HierarchyItem,
    pub sites: Vec<Span>,
}

/// The analysis a `FileKey` denotes, across the same tiers the reference
/// walk sweeps: open docs, workspace entries, then the index's cached
/// registration (whole view — the enclosing-caller lookup reads symbols,
/// which index copies evict). Hierarchy adapters use this to re-anchor at
/// an item's own file (LSP hierarchy requests hand back an item, not a
/// cursor in an open document).
pub fn analysis_for_key(
    files: &FileStore,
    module_index: Option<&dyn CrossFileLookup>,
    key: &FileKey,
) -> Option<Arc<FileAnalysis>> {
    let path = match key {
        FileKey::Url(u) => {
            let mut hit = None;
            files.for_each_open(|url, doc| {
                if url == u {
                    hit = Some(Arc::clone(&doc.analysis));
                }
            });
            if hit.is_some() {
                return hit;
            }
            u.to_file_path().ok()?
        }
        FileKey::Path(p) => p.clone(),
    };
    // Workspace copies are symbols-evicted after persist; the enclosing-
    // caller lookup reads symbols, so an evicted copy must rehydrate through
    // the index (`whole_present`) rather than answer absence-as-answer.
    let resident = files.workspace_raw().get(&path).map(|e| Arc::clone(e.value()));
    if let Some(a) = &resident {
        if !a.symbols_are_evicted() {
            return resident;
        }
    }
    module_index
        .and_then(|idx| idx.cached_by_path(&path).map(|cm| idx.whole_present(&cm)))
        .or(resident)
}

impl<'a> CandidateSet<'a> {
    /// goto-type-definition: the definition of the class the VALUE at the
    /// cursor carries — `$obj` jumps to the package its inferred type
    /// dispatches against. Strictly type-driven: when nothing infers, the
    /// answer is empty (a name-only fallback would re-introduce the
    /// constructor-flood class of wrong answers goto-def already retired).
    pub fn type_definitions(&self) -> Vec<RefLocation> {
        let Some(t) = self.cursor_value_type() else {
            return Vec::new();
        };
        let Some(class) = self.origin.dispatch_class_of(&t, self.idx()) else {
            return Vec::new();
        };
        self.type_defs_of(&class)
    }

    /// The type the expression under the cursor produces, via the two
    /// canonical query entries (variables through the bag's scope walk,
    /// everything else through the span-attached `Expr` witnesses).
    fn cursor_value_type(&self) -> Option<crate::model::file_analysis::InferredType> {
        if let Some(r) = self.origin.ref_at(self.point) {
            return match &r.kind {
                RefKind::Variable => {
                    self.origin
                        .inferred_type_via_bag_ctx(&r.target_name, self.point, self.idx())
                }
                // A member token: the receiver's type, then the member's
                // value on it — a method's return first, a field's declared
                // type as the fallback (the ladder hover reads); the bare
                // span's own witnesses when the receiver does not type.
                RefKind::MethodCall { invocant_span: Some(inv), .. } => self
                    .origin
                    .expr_type_at_span(*inv, self.idx())
                    .and_then(|t| {
                        self.origin.member_value_type(&t, r.unqualified_target_name(), self.idx(), r.arg_count)
                    })
                    .or_else(|| self.origin.expr_type_at_span(r.span, self.idx())),
                _ => self.origin.expr_type_at_span(r.span, self.idx()),
            };
        }
        // A variable DECLARATION site (`my $obj = ...` with the cursor on
        // `$obj`): probe at the declaration's end so the initializing
        // assignment's witness is temporally visible.
        let sym = self.origin.symbol_at(self.point)?;
        if matches!(sym.kind, SymKind::Variable) {
            return self
                .origin
                .inferred_type_via_bag_ctx(&sym.name, sym.span.end, self.idx());
        }
        None
    }

    /// Every definition site of a named class/package — the origin file's
    /// own declaration plus every visible candidate file's (a package is a
    /// SET of files; the reopened-package case keeps all of them). Falls
    /// back to the resolved module file's top exactly like goto-def on a
    /// `use` statement, never to a name-only guess.
    pub(super) fn type_defs_of(&self, class: &str) -> Vec<RefLocation> {
        let wanted =
            |k: &SymKind| matches!(k, SymKind::Class | SymKind::Package | SymKind::Module);
        let mut out: Vec<RefLocation> = Vec::new();
        for s in self
            .origin
            .symbols()
            .iter()
            .filter(|s| s.name == class && wanted(&s.kind))
        {
            out.push(self.origin_decl(s.selection_span));
        }
        if let Some(idx) = self.idx() {
            for cached in idx.visible_def_candidates(class) {
                let whole = idx.whole_present(&cached);
                for s in whole
                    .symbols()
                    .iter()
                    .filter(|s| s.name == class && wanted(&s.kind))
                {
                    out.push(RefLocation {
                        key: FileKey::Path(cached.path.clone()),
                        span: s.selection_span,
                        access: AccessKind::Declaration,
                        rewritable: true,
                        label: None,
                    });
                }
            }
            if out.is_empty() {
                // The module resolves to a file but its package symbol
                // isn't scanned (e.g. a name-keyed @INC module whose file
                // the candidates table doesn't carry): the file top is the
                // honest goto-def parity answer.
                if let Some(path) = idx.module_path_cached(class) {
                    out.push(RefLocation {
                        key: FileKey::Path(path),
                        span: Span {
                            start: tree_sitter::Point::new(0, 0),
                            end: tree_sitter::Point::new(0, 0),
                        },
                        access: AccessKind::Declaration,
                        rewritable: false,
                        label: None,
                    });
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        out.retain(|l| seen.insert((key_for_sort(&l.key), l.span)));
        out
    }

    /// The class the cursor denotes for hierarchy purposes: an explicit
    /// class/package token first (declaration or reference), else the
    /// class of the value at the cursor (so preparing on `$obj` opens the
    /// hierarchy of its inferred class).
    fn cursor_class(&self) -> Option<String> {
        if let Some(s) = self.origin.symbol_at(self.point) {
            if matches!(s.kind, SymKind::Class | SymKind::Package | SymKind::Module) {
                return Some(s.name.clone());
            }
        }
        if let Some(r) = self.origin.ref_at(self.point) {
            if matches!(r.kind, RefKind::PackageRef) {
                return Some(r.target_name.clone());
            }
        }
        let t = self.cursor_value_type()?;
        self.origin.dispatch_class_of(&t, self.idx())
    }

    /// typeHierarchy/prepare: the class item at the cursor, or nothing —
    /// there is no name-only fallback and no item for an unlocatable class.
    pub fn hierarchy_type_item(&self) -> Option<HierarchyItem> {
        let class = self.cursor_class()?;
        self.class_item(&class)
    }

    /// typeHierarchy/supertypes: the DIRECT parents of the class at the
    /// cursor — one `GraphView` level over the same `INHERITS | APP_SURFACE`
    /// mask every full-MRO walk passes (the client recurses per level).
    /// Parents whose definition can't be located (the synthetic app-surface
    /// class, an uninstalled module) mint no item: honest miss, not a guess.
    pub fn supertypes(&self) -> Vec<HierarchyItem> {
        let Some(class) = self.cursor_class() else {
            return Vec::new();
        };
        self.direct_edges(&class, crate::model::graph::EdgeKindMask::INHERITS
            | crate::model::graph::EdgeKindMask::APP_SURFACE)
    }

    /// typeHierarchy/subtypes: the DIRECT children — one level of the same
    /// `INHERITS_INV` edge the implementations fan-out walks transitively.
    pub fn subtypes(&self) -> Vec<HierarchyItem> {
        let Some(class) = self.cursor_class() else {
            return Vec::new();
        };
        self.direct_edges(&class, crate::model::graph::EdgeKindMask::INHERITS_INV)
    }

    /// One `GraphView` level from `class` over `mask`: visit each reached
    /// node, prune its expansion (the hierarchy protocol is per-level — the
    /// client re-queries on expand), and mint items for locatable classes.
    fn direct_edges(
        &self,
        class: &str,
        mask: crate::model::graph::EdgeKindMask,
    ) -> Vec<HierarchyItem> {
        let graph = crate::model::graph::GraphView::new(self.origin, self.idx());
        let mut names: Vec<String> = Vec::new();
        graph.walk(
            crate::model::graph::Node::Class(class.to_string()),
            mask,
            &mut |n| {
                if let crate::model::graph::Node::Class(c) = n {
                    names.push(c.clone());
                }
                crate::model::graph::WalkControl::PruneChildren
            },
        );
        names.iter().filter_map(|c| self.class_item(c)).collect()
    }

    /// Mint the hierarchy item for a named class: its first locatable
    /// definition site (kind read from the defining symbol when the site
    /// carries one).
    fn class_item(&self, class: &str) -> Option<HierarchyItem> {
        let location = self.type_defs_of(class).into_iter().next()?;
        let kind = match &location.key {
            k if file_key_eq(k, &self.origin_key) => self
                .origin
                .symbols()
                .iter()
                .find(|s| s.name == class && contains_point_of(s, location.span.start))
                .map(|s| s.kind),
            FileKey::Path(p) => self.idx().and_then(|idx| {
                let cm = idx.cached_by_path(p)?;
                let whole = idx.whole_present(&cm);
                whole
                    .symbols()
                    .iter()
                    .find(|s| s.name == class && contains_point_of(s, location.span.start))
                    .map(|s| s.kind)
            }),
            _ => None,
        }
        .unwrap_or(SymKind::Package);
        Some(HierarchyItem {
            name: class.to_string(),
            kind,
            detail: None,
            location,
        })
    }

    /// callHierarchy/prepare: the callable at the cursor — its own
    /// declaration when the cursor sits on one, else the declaration the
    /// call under the cursor resolves to (the same forward resolution
    /// goto-def projects).
    pub fn hierarchy_call_item(&self) -> Option<HierarchyItem> {
        if let Some(s) = self.origin.symbol_at(self.point) {
            if matches!(s.kind, SymKind::Sub | SymKind::Method) {
                return Some(HierarchyItem {
                    name: s.name.clone(),
                    kind: s.kind,
                    detail: s.package.clone(),
                    location: self.origin_decl(s.selection_span),
                });
            }
        }
        let target = match self.resolution()? {
            ResolvedTarget::Target(t) => t,
            _ => return None,
        };
        let (kind, detail) = match &target.kind {
            TargetKind::Method { class } => (SymKind::Method, Some(class.clone())),
            TargetKind::Sub { package } => (SymKind::Sub, package.clone()),
            _ => return None,
        };
        let location = self.definitions().into_iter().next()?;
        // Snap to the defining symbol's SELECTION span: goto-def may land on
        // the decl's full span (the `sub` keyword), but the item's location
        // is the anchor incoming/outgoing re-resolve at, and those need the
        // name token (`symbol_at` keys on selection spans).
        if let Some(a) = analysis_for_key(self.files, self.module_index, &location.key) {
            if let Some(s) = a.enclosing_callable_at(location.span.start) {
                return Some(HierarchyItem {
                    name: s.name.clone(),
                    kind: s.kind,
                    detail: s.package.clone(),
                    location: RefLocation {
                        key: location.key,
                        span: s.selection_span,
                        access: AccessKind::Declaration,
                        rewritable: false,
                        label: None,
                    },
                });
            }
        }
        Some(HierarchyItem {
            name: target.name.clone(),
            kind,
            detail,
            location,
        })
    }

    /// callHierarchy/incomingCalls, for a set minted AT THE DECLARATION
    /// (the prepare item's location): the `references()` image — the same
    /// projection `--heatmap` derives fan-in from, so the two counts agree
    /// by construction — grouped by each site's enclosing callable.
    /// Top-level sites (import-time calls, scripts) have no enclosing
    /// callable and are dropped; the count a client displays is therefore
    /// a lower bound of the heatmap's fan-in, never a different walk.
    pub fn incoming_calls(&self) -> Vec<CallEdge> {
        let mut edges: Vec<CallEdge> = Vec::new();
        // (file sort key, caller selection span) → edge index.
        let mut by_caller: std::collections::HashMap<(std::path::PathBuf, Span), usize> =
            std::collections::HashMap::new();
        let mut analyses: std::collections::HashMap<std::path::PathBuf, Option<Arc<FileAnalysis>>> =
            std::collections::HashMap::new();
        for loc in self.references() {
            if loc.access == AccessKind::Declaration {
                continue;
            }
            let file_key = key_for_sort(&loc.key);
            let analysis = analyses
                .entry(file_key.clone())
                .or_insert_with(|| {
                    analysis_for_key(self.files, self.module_index, &loc.key)
                })
                .clone();
            let Some(analysis) = analysis else { continue };
            let Some(caller) = analysis.enclosing_callable_at(loc.span.start) else {
                continue;
            };
            let entry = by_caller.entry((file_key, caller.selection_span));
            match entry {
                std::collections::hash_map::Entry::Occupied(e) => {
                    edges[*e.get()].sites.push(loc.span);
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(edges.len());
                    edges.push(CallEdge {
                        item: HierarchyItem {
                            name: caller.name.clone(),
                            kind: caller.kind,
                            detail: caller.package.clone(),
                            location: RefLocation {
                                key: loc.key.clone(),
                                span: caller.selection_span,
                                access: AccessKind::Declaration,
                                rewritable: false,
                                label: None,
                            },
                        },
                        sites: vec![loc.span],
                    });
                }
            }
        }
        edges.sort_by(|a, b| {
            key_for_sort(&a.item.location.key)
                .cmp(&key_for_sort(&b.item.location.key))
                .then_with(|| {
                    (a.item.location.span.start.row, a.item.location.span.start.column)
                        .cmp(&(b.item.location.span.start.row, b.item.location.span.start.column))
                })
        });
        edges
    }

    /// callHierarchy/outgoingCalls, for a set minted AT THE DECLARATION:
    /// the call refs inside the sub's body span (the same in-body lane
    /// `--heatmap`'s fan-out counts), each callee resolved through the
    /// forward `definitions()` projection at its own call site. Callees
    /// that resolve nowhere (builtins, unresolved dynamic calls) mint no
    /// edge — honest miss, matching the diagnostics story.
    pub fn outgoing_calls(&self) -> Vec<CallEdge> {
        let Some(sym) = self
            .origin
            .symbol_at(self.point)
            .filter(|s| matches!(s.kind, SymKind::Sub | SymKind::Method))
        else {
            return Vec::new();
        };
        let body = sym.span;
        let within = |outer: &Span, inner: &Span| {
            let s = |p: &tree_sitter::Point| (p.row, p.column);
            s(&inner.start) >= s(&outer.start) && s(&inner.end) <= s(&outer.end)
        };
        let mut edges: Vec<CallEdge> = Vec::new();
        let mut by_callee: std::collections::HashMap<(std::path::PathBuf, Span), usize> =
            std::collections::HashMap::new();
        for r in self.origin.refs() {
            // `site` is the span a client highlights (the name token, not the
            // whole chain expression); `token` is where the forward
            // resolution anchors.
            let (token, site) = match &r.kind {
                RefKind::FunctionCall => (r.span.start, r.span),
                RefKind::MethodCall { method_name_span, .. } => {
                    (method_name_span.start, *method_name_span)
                }
                RefKind::DispatchCall { .. } => (r.span.start, r.span),
                _ => continue,
            };
            if !within(&body, &r.span) {
                continue;
            }
            // The callee's declaration, via the same forward resolution
            // goto-def runs at this call site.
            let cs = super::resolve(
                self.files,
                self.origin,
                self.origin_key.clone(),
                token,
                self.module_index,
                self.scope,
            );
            let Some(def) = cs.definitions().into_iter().next() else {
                continue;
            };
            // A "callee" whose definition is the queried sub's own decl is
            // recursion — keep it, VS Code renders it; but a def that is
            // just this call site again (self-span echo) is noise.
            if file_key_eq(&def.key, &self.origin_key) && def.span == r.span {
                continue;
            }
            let key = (key_for_sort(&def.key), def.span);
            match by_callee.entry(key) {
                std::collections::hash_map::Entry::Occupied(e) => {
                    edges[*e.get()].sites.push(site);
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(edges.len());
                    edges.push(CallEdge {
                        item: HierarchyItem {
                            name: r.unqualified_target_name().to_string(),
                            kind: SymKind::Sub,
                            detail: None,
                            location: def,
                        },
                        sites: vec![site],
                    });
                }
            }
        }
        edges
    }
}

/// Whether `sym`'s selection span starts at `point` (the def-site match
/// `class_item` uses to read back the defining symbol's kind).
fn contains_point_of(sym: &crate::model::file_analysis::Symbol, point: tree_sitter::Point) -> bool {
    sym.selection_span.start == point
}
