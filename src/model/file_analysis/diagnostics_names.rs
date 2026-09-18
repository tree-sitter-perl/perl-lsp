//! The declaration family: findings about the NAMES a file writes rather
//! than about a member access — a deprecated callee, a type its namespace
//! cannot supply, a contract it does not meet, a return it never spells.

use super::*;

impl FileAnalysis {
    /// A use of a deprecated function or class, declared here or in the
    /// namespace this file means by the leaf.
    pub fn deprecated_use_findings(&self, facts: &LaneFacts<'_>) -> Vec<Finding> {
        let mut out = Vec::new();
        for r in self.refs() {
            let want_class = match r.kind {
                RefKind::FunctionCall => false,
                RefKind::PackageRef => true,
                _ => continue,
            };
            let leaf = r.unqualified_target_name(self.names());
            if leaf.is_empty() || self.pack.import_row_covering(&r.span).is_some() {
                continue;
            }
            let is_kind = |s: &Symbol| {
                if want_class {
                    matches!(s.kind, SymKind::Class)
                } else {
                    matches!(s.kind, SymKind::Sub)
                }
            };
            let local = self
                .symbols_named(leaf)
                .iter()
                .map(|&sid| self.symbol(sid))
                .find(|s| is_kind(s))
                .and_then(FileAnalysis::deprecation_of);
            // Cross-file: only a declaration in the namespace THIS file
            // means by the leaf (its pin, else its own namespace) — a
            // same-leaf stranger elsewhere in the workspace is a different
            // declaration.
            let found = local.or_else(|| {
                let i = facts.idx?;
                // A class token names an identity (the use-map's answer); a
                // function keeps the leaf under the namespace this file means.
                if want_class {
                    let ident = self.class_spelling_identity(leaf);
                    let declaring = |a: &FileAnalysis| {
                        a.symbols()
                            .iter()
                            .find(|s| is_kind(s) && (s.name == ident || s.name == leaf))
                            .map(|s| s.id)
                    };
                    return i
                        .defining_analysis(&ident, &|a| declaring(a).is_some())
                        .and_then(|a| {
                            declaring(&a).and_then(|id| FileAnalysis::deprecation_of(a.symbol(id)))
                        });
                }
                let want_ns = self
                    .leaf_namespace(leaf)
                    .or_else(|| self.use_map_pins().own_namespace.clone());
                let declaring = |a: &FileAnalysis| {
                    a.symbols_named(leaf)
                        .iter()
                        .map(|&sid| a.symbol(sid))
                        .find(|s| is_kind(s) && (want_ns.is_none() || s.package == want_ns))
                        .map(|s| s.id)
                };
                i.defining_analysis(leaf, &|a| declaring(a).is_some())
                    .and_then(|a| declaring(&a).and_then(|id| FileAnalysis::deprecation_of(a.symbol(id))))
            });
            if let Some(note) = found {
                out.push(Finding::new(
                    r.span,
                    codes::DEPRECATED,
                    FindingData::Deprecated { name: leaf.to_string(), note },
                ));
            }
        }
        out
    }

    /// A class name this file's namespace evidence cannot supply — reported
    /// only against a settled index, since absence in a warming one says
    /// nothing.
    pub fn undefined_type_findings(&self, facts: &LaneFacts<'_>) -> Vec<Finding> {
        let Some(idx) = facts.settled_lookup() else { return Vec::new() };
        let pins = self.use_map_pins();
        // a namespace-less file lives in the global namespace
        let own_ns = pins.own_namespace.clone().unwrap_or_default();
        let own = own_ns.as_str();
        let ns_heads = self.namespace_heads();
        let mut reported: std::collections::HashSet<(usize, usize)> = Default::default();
        let mut out = Vec::new();
        for r in self.refs() {
            // the class token — a construction site mints one of its own, so
            // `new Foo()` arrives here as the class it names
            if !matches!(r.kind, RefKind::PackageRef) {
                continue;
            }
            let written = r.target_name.as_str();
            // absolute names reach the global namespace (builtins we carry no
            // stubs for) — silent, as is a name qualified with the language's
            // MEMBER separator: it names a member, not a type this namespace
            // must supply
            let sep = self.names().sep().unwrap_or_default();
            let member_qualified = self
                .names()
                .member_sep()
                .is_some_and(|m| m != sep && written.contains(m));
            if (!sep.is_empty() && written.starts_with(sep)) || member_qualified {
                continue;
            }
            let leaf = name_match_key(written, self.names());
            let leaf = leaf.as_str();
            // a name that resolves off the enclosing class rather than out of
            // a namespace (`self`, `static`, `parent`) — the document said so
            if leaf.is_empty() || r.names_relative_scope() {
                continue;
            }
            // a segment used as a NAMESPACE prefix in this file (`Psr7\Utils`)
            // names a namespace, not a type
            if ns_heads.contains(leaf) {
                continue;
            }
            if let Some(row) = self.pack.import_row_covering(&r.span) {
                // an import row naming a function/constant, not a type
                if row.binds != ImportBinds::Type {
                    continue;
                }
                // a row whose leaf the file never spells bare imports a
                // NAMESPACE (`use GuzzleHttp\Psr7;` then `Psr7\Utils`) or
                // nothing — no type to assert
                if !pins.spelled.contains(leaf) {
                    continue;
                }
            }
            // conflicting evidence about what the leaf names: the use-map's
            // own answer, and the lane's silence rule
            if matches!(pins.pins.get(leaf), Some(None)) {
                continue;
            }
            // The namespace this file's evidence gives the leaf: its PIN
            // where it has one — a qualified spelling names its namespace
            // outright, and an absolute one (`\Throwable`) names the global
            // namespace, neither of which survives on the leaf token the ref
            // carries — and the bare use-map resolve otherwise.
            let (identity, ns) = match pins.pins.get(leaf) {
                Some(Some(ns)) => (self.join_name(ns, leaf), ns.clone()),
                _ => {
                    let identity = self.class_spelling_identity(written);
                    let ns = self.identity_namespace(&identity).unwrap_or_else(|| own.to_string());
                    (identity, ns)
                }
            };
            // the global namespace is the builtins we carry no stubs for —
            // silent, unless the workspace declares the leaf under a
            // namespace and nowhere global: then the type is real and missing
            // its import
            let declared = self.type_namespaces(idx, leaf);
            if ns.is_empty() {
                if declared.is_empty()
                    || declared.iter().any(|d| d.is_empty())
                    || facts.is_builtin_type(leaf)
                {
                    continue;
                }
            } else if declared.contains(&ns) {
                continue;
            }
            if !reported.insert((r.span.start.row, r.span.start.column)) {
                continue;
            }
            // every namespace that DOES declare the leaf is an import the
            // quick-fix can offer
            let candidates: Vec<String> =
                declared.iter().map(|d| self.join_name(d, leaf)).collect();
            out.push(Finding::new(
                r.span,
                codes::UNDEFINED_TYPE,
                FindingData::UndefinedType { identity, candidates },
            ));
        }
        out
    }

    /// Every namespace declaring a type named `leaf`: this file's own
    /// declaration plus every workspace/dependency candidate — the set the
    /// undefined-type lane tests membership in and the import quick-fix
    /// lists.
    fn type_namespaces(&self, idx: &dyn CrossFileLookup, leaf: &str) -> Vec<String> {
        let mut out: Vec<String> = self.declared_class_namespace(leaf).into_iter().collect();
        for c in idx.def_candidates(leaf) {
            if let Some(ns) = idx.symbols_present(&c).declared_class_namespace(leaf) {
                if !out.contains(&ns) {
                    out.push(ns);
                }
            }
        }
        out
    }

    /// A concrete class that neither declares nor inherits the contract
    /// callables its roles require. An interface / trait / abstract class is
    /// a role; silent for an ancestor this workspace cannot see and for a
    /// composer that defers (abstract). A catch-all member does NOT silence
    /// it: the contract is checked where the class is declared, before any
    /// call could be caught.
    pub fn contract_findings(&self, facts: &LaneFacts<'_>) -> Vec<Finding> {
        let Some(idx) = facts.idx else { return Vec::new() };
        let mut by_class: std::collections::BTreeMap<String, Vec<UnfulfilledRequire>> =
            Default::default();
        for u in self.unfulfilled_role_requires(Some(idx)) {
            by_class.entry(u.package.clone()).or_default().push(u);
        }
        let mut out = Vec::new();
        for (class, missing) in by_class {
            let Some(sym) = self
                .symbols()
                .iter()
                .find(|s| s.kind == SymKind::Class && s.name == class)
            else {
                continue;
            };
            out.push(Finding::new(
                sym.selection_span,
                codes::UNIMPLEMENTED_METHOD,
                FindingData::UnimplementedContracts { class, missing },
            ));
        }
        out
    }

    /// A callable with a body, no native return annotation, and an inferred
    /// return this language can spell natively — in a file that writes
    /// native return types already (its own convention; a docblock-typed
    /// codebase is not asked to change style). Skipped: constructors,
    /// contracts, and any return the spelling cannot name (ambiguous
    /// numerics, unions, a leaf that means another class here).
    pub fn missing_return_type_findings(&self) -> Vec<Finding> {
        if self.spellings().return_annotation_template.is_empty() {
            return Vec::new();
        }
        // The declaration's own annotation is the structural fact — a type
        // witness cannot carry it: `: void` names no type.
        let declared = |s: &Symbol| s.declared_return().is_some();
        let callables: Vec<&Symbol> = self
            .symbols()
            .iter()
            .filter(|s| matches!(s.kind, SymKind::Sub | SymKind::Method))
            .collect();
        if !callables.iter().any(|s| declared(s)) {
            return Vec::new();
        }
        let mut out = Vec::new();
        for s in callables {
            // no annotation to add: a constructor, a contract, a documented
            // declaration, a closure; and none wanted for a declared one
            if s.is_constructor()
                || s.flags.intersects(
                    SymbolFlags::CONTRACT | SymbolFlags::DOC_DECLARED | SymbolFlags::ANONYMOUS,
                )
                || declared(s)
            {
                continue;
            }
            let Some(ty) = self.total_inferred_return(s.id) else { continue };
            // a fluent `return $this` wants `static`, `new self()` wants
            // `self` — the fold cannot tell them apart, so the enclosing
            // class is never spelled from inference
            if matches!(&ty, InferredType::ClassName(n)
                if s.package.as_deref().is_some_and(|p| p == n || name_match_key(p, self.names()) == *n))
            {
                continue;
            }
            let Some(spelling) = self.native_type_spelling(&ty) else { continue };
            out.push(Finding::new(
                s.selection_span,
                codes::MISSING_RETURN_TYPE,
                FindingData::MissingReturnType { name: s.name.clone(), spelling },
            ));
        }
        out
    }
}
