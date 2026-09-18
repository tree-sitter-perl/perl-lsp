//! The liveness family: which bindings a callable reads, and which it does
//! not. The unbound-read lane and the never-read lane ask opposite halves of
//! one question, so they share the walk that answers it — a read is visited
//! once and lands in both tables.

use super::*;

/// Every read in the file, keyed by name, as the chain of callables it sits
/// inside (innermost first). A read counts for the callable it is in, every
/// enclosing callable (a closure's capture reads the outer binding through
/// its own copy) and every callable nested inside it (a by-reference capture
/// is written inside the closure and read by the scope around it).
type ReadChains = HashMap<String, Vec<Vec<u32>>>;

impl FileAnalysis {
    /// The callable a scope belongs to — the lanes' unit of liveness.
    fn callable_of(&self, scope: ScopeId) -> Option<ScopeId> {
        self.scope_chain(scope)
            .into_iter()
            .find(|&sc| matches!(self.scope(sc).kind, ScopeKind::Sub { .. } | ScopeKind::Method { .. }))
    }

    /// The callables enclosing `scope`, innermost first.
    fn callables_up(&self, scope: ScopeId) -> Vec<u32> {
        self.scope_chain(scope)
            .into_iter()
            .filter(|&sc| matches!(self.scope(sc).kind, ScopeKind::Sub { .. } | ScopeKind::Method { .. }))
            .map(|sc| sc.0)
            .collect()
    }

    /// Does the callable owning `scope` materialize variables no declaration
    /// names (php `extract`, `eval`)? The extractor stamped that on the
    /// callable, so both lanes read the flag instead of matching call spans
    /// against the body they sit in.
    fn dynamic_vars(&self, scope: ScopeId) -> bool {
        self.scope(scope)
            .owner
            .is_some_and(|sid| self.symbol(sid).flags.contains(SymbolFlags::DYNAMIC_VARS))
    }

    /// The two liveness lanes over ONE walk of the reads: a read with no
    /// binding is the unbound-read finding, and the same read is what keeps
    /// a declaration off the never-read list.
    ///
    /// Both run on facts: a read the runtime binds carries a binding
    /// (`@ref.var.implicit`), and a language whose document binds nothing
    /// produces no unbound reads to report.
    pub fn liveness_findings(&self, facts: &LaneFacts<'_>) -> Vec<Finding> {
        let mut out = Vec::new();
        let mut read_chains: ReadChains = HashMap::new();
        // The unbound candidates, collected on the same pass and judged
        // after it: the checks below are the expensive half, and a read that
        // never reaches them still has to count for the never-read lane.
        let mut unbound: Vec<&Ref> = Vec::new();
        for r in self.refs() {
            if !matches!(r.kind, RefKind::Variable) {
                continue;
            }
            if !matches!(r.access, AccessKind::Write) {
                read_chains
                    .entry(r.target_name.clone())
                    .or_default()
                    .push(self.callables_up(r.scope));
            }
            // a WRITE binds (a language that declares a variable by
            // assigning it), and so does a binding the runtime supplies
            if r.binding.is_some() || matches!(r.access, AccessKind::Write) {
                continue;
            }
            unbound.push(r);
        }

        // A bare variable written as a call argument is bound by the call
        // when the callee declares that position by reference (`&$out`): the
        // callee's aliasing edge IS the binding, chased from the argument's
        // own site (`docs/adr/by-ref-binding.md`). A callee this lane cannot
        // see leaves no edge and answers nothing; a callee that aliases but
        // names no type answers `Unknown`, which still binds.
        for r in unbound {
            let Some(sc) = self.callable_of(r.scope) else { continue };
            // A binding, not a type: a usage observation (`$x + 1` says
            // numeric) types the read without anything ever writing it.
            if self.variable_is_bound_via_bag(&r.target_name, r.span.start, facts.idx) {
                continue;
            }
            // A bare argument of a callee this file does not declare is
            // silence, never a guess either way: the callee's own bag says
            // which positions alias, and a runtime's own functions have no
            // bag here to say it.
            if let Some(callee) = self.argument_callee(r) {
                let declared = self
                    .symbols_named(callee)
                    .iter()
                    .any(|&sid| matches!(self.symbol(sid).kind, SymKind::Sub | SymKind::Method));
                if !declared {
                    continue;
                }
            }
            // `isset($x)` / `empty($x)` / `unset($x)`: the read IS the
            // existence question, the member lanes' probe silence
            if self.pack.probe_regions.iter().any(|p| p.contains(&r.span)) {
                continue;
            }
            if self.dynamic_vars(sc) {
                continue;
            }
            out.push(Finding::new(
                r.span,
                codes::UNDEFINED_VARIABLE,
                FindingData::UndefinedVariable { name: r.target_name.clone() },
            ));
        }

        // A same-named declaration in a nested callable is the capture
        // itself, so a declaration read nowhere is still live when an inner
        // callable declares its own copy of the name.
        let mut decl_chains: HashMap<String, Vec<(u32, Vec<u32>)>> = HashMap::new();
        for sym in self.symbols() {
            if matches!(sym.kind, SymKind::Variable) {
                if let Some(sc) = self.callable_of(sym.scope) {
                    decl_chains
                        .entry(sym.name.clone())
                        .or_default()
                        .push((sc.0, self.callables_up(sym.scope)));
                }
            }
        }
        for sym in self.symbols() {
            if !matches!(sym.kind, SymKind::Variable) {
                continue;
            }
            let Some(sc) = self.callable_of(sym.scope) else { continue };
            // a throwaway is declared never to be read, a parameter is the
            // caller's, and an alias is written to REACH another slot's
            // storage — for all three the write is the point
            if sym.flags.intersects(SymbolFlags::THROWAWAY | SymbolFlags::ALIAS)
                || self.pack.param_regions.iter().any(|p| p.contains(&sym.span))
            {
                continue;
            }
            if self.dynamic_vars(sc) {
                continue;
            }
            let up = self.callables_up(sym.scope);
            let related =
                |chain: &Vec<u32>| chain.contains(&sc.0) || up.iter().any(|c| chain.first() == Some(c));
            let read = read_chains.get(&sym.name).is_some_and(|chains| chains.iter().any(related));
            let captured = decl_chains
                .get(&sym.name)
                .is_some_and(|ds| ds.iter().any(|(owner, chain)| *owner != sc.0 && chain.contains(&sc.0)));
            if read || captured {
                continue;
            }
            out.push(Finding::new(
                sym.selection_span,
                codes::UNUSED_VARIABLE,
                FindingData::UnusedVariable { name: sym.name.clone() },
            ));
        }
        out
    }

    /// An import row binding a name the file never spells. Only for a
    /// language whose import rows bind names at all — a text-splicing
    /// include brings in everything and binds nothing.
    pub fn unused_import_findings(&self) -> Vec<Finding> {
        if !self.pack.imports_bind_names {
            return Vec::new();
        }
        let pack = &self.pack;
        let pins = self.use_map_pins();
        let ns_heads = self.namespace_heads();
        let mut out = Vec::new();
        for row in &pack.include_directives {
            let leaf = name_match_key(&row.raw, self.names());
            let leaf = leaf.as_str();
            // the alias token has a row of its own; the import's row reports
            if split_qualified(&row.raw, self.names()).0.is_none()
                && pack.use_aliases.iter().any(|(alias, _, _)| alias == leaf)
            {
                continue;
            }
            // the name the row binds: its alias when it has one
            let bound = pack
                .use_aliases
                .iter()
                .find(|(_, ns, real)| real == leaf && self.join_name(ns, real) == row.raw)
                .map(|(alias, _, _)| alias.as_str())
                .unwrap_or(leaf);
            // a constant import (`use const FOO`) has no spelling the walker
            // records — silent
            if bound.is_empty() || row.binds == ImportBinds::Const {
                continue;
            }
            if pins.spelled.contains(bound)
                || ns_heads.contains(bound)
                || pack.doc_mentions.iter().any(|m| m == bound)
            {
                continue;
            }
            // the whole statement's rows, when this import is the only one
            // on it: what a remove-the-line fix needs
            let sole_row = pack
                .import_rows
                .iter()
                .find(|r| r.contains(&row.span))
                .filter(|r| pack.include_directives.iter().filter(|i| r.contains(&i.span)).count() == 1)
                .map(|r| (r.start.row, r.end.row));
            out.push(Finding::new(
                row.span,
                codes::UNUSED_IMPORT,
                FindingData::UnusedImport { bound: bound.to_string(), sole_row },
            ));
        }
        out
    }

    /// `namespace` and `leaf` joined the way this file's language spells a
    /// qualified name — the model's own join, so the global namespace gives
    /// the bare leaf and no caller guards a dangling separator.
    pub(crate) fn join_name(&self, namespace: &str, leaf: &str) -> String {
        crate::model::conventions::join_qualified(
            namespace,
            leaf,
            self.names().sep().unwrap_or_default(),
        )
    }

    /// The leading segment of every namespace this file writes as a
    /// QUALIFIER (`Psr7\Utils` → `Psr7`): such a segment names a namespace,
    /// not a type and not an unused import.
    pub(crate) fn namespace_heads(&self) -> std::collections::HashSet<String> {
        let Some(sep) = self.names().sep() else { return Default::default() };
        self.pack
            .qualified_spellings
            .iter()
            .filter_map(|(_, prefix)| prefix.trim_start_matches(sep).split(sep).next())
            .filter(|h| !h.is_empty())
            .map(str::to_string)
            .collect()
    }
}
