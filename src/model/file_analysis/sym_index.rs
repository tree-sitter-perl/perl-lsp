//! Raw symbol/ref index accessors over the tables' rebuilt lookups (the
//! symbol name/scope indices, the ref name index, and the
//! linkage-visibility gate they share with cross-file registration).

use super::*;

impl FileAnalysis {
    /// Find all symbols with a given name.
    pub fn symbols_named(&self, name: &str) -> &[SymbolId] {
        self.symbols.named(name)
    }

    /// The local callable a package-scoped `FunctionCall` targets: the
    /// Sub/Method whose `package` equals `resolved_package`, keyed by the
    /// call's unqualified tail (symbols are stored under the bare name).
    /// A free function (`Sub`) is preferred; a class `Method` is the
    /// fallback so an implicit-`this` sibling call — a bare `foo()` inside a
    /// method body whose `resolved_package` the model pinned to the enclosing
    /// class — lands on the member. This is C++ name lookup: the member wins
    /// over a same-named free function INSIDE the class body (the pin only
    /// happens there), but a name with no member in that package leaves the
    /// `Sub` path untouched, so a free-function-only call still resolves free.
    pub(super) fn package_scoped_callable(&self, name: &str, resolved_package: Option<&str>) -> Option<SymbolId> {
        let mut method_fallback = None;
        for &sid in self.symbols_named(name) {
            let sym = self.symbol(sid);
            if sym.package.as_deref() != resolved_package {
                continue;
            }
            match sym.kind {
                SymKind::Sub => return Some(sid),
                SymKind::Method if method_fallback.is_none() => method_fallback = Some(sid),
                _ => {}
            }
        }
        method_fallback
    }

    /// The C-linkage "everything exported" test: is `sym` part of this
    /// file's cross-file surface? Types (class/struct/typedef), functions,
    /// and FILE-SCOPE values (globals, object-like macros, enum constants).
    /// A file-scope value is a Variable whose scope is the file — locals
    /// (function-scoped) and struct/namespace members (their own body
    /// scope) are excluded by scope alone. A C enum constant leaks to file
    /// scope yet carries its parent enum as a *type* `package` (for hover);
    /// that annotation must NOT hide it, so key off scope, not package.
    /// Shared by cross-file registration (`ModuleIndex::register_symbols`)
    /// and include-closure completion gathering, so "resolvable" and
    /// "offered" never drift apart.
    pub fn is_linkage_visible(&self, sym: &Symbol) -> bool {
        match sym.kind {
            SymKind::Class | SymKind::Sub => true,
            // An anonymous-enum constant leaks to file scope the same way an
            // unqualified global does; a NAMED enum's constants leak to the
            // enum's own enclosing scope (also File for a top-level enum) —
            // either way the File-scope gate is the same test as Variable.
            SymKind::Variable | SymKind::Enumerator => self
                .scopes
                .iter()
                .find(|s| s.id == sym.scope)
                .is_some_and(|s| matches!(s.kind, ScopeKind::File)),
            _ => false,
        }
    }

    /// Find all symbols in a given scope.
    #[allow(dead_code)]
    pub fn symbols_in_scope(&self, scope: ScopeId) -> &[SymbolId] {
        self.symbols.in_scope(scope)
    }

    /// Indexes (into `refs()`) of every ref whose `match_key` is `key`, in
    /// ref order — the in-file half of the relational narrowing. A key
    /// spelled through `name_match_key` retrieves everything any matcher
    /// arm could match for that name (see `RefTable::by_key`).
    pub fn ref_indices_keyed(&self, key: &str) -> &[usize] {
        self.refs.by_key(key)
    }

    /// Find all refs that resolve to a specific symbol.
    #[allow(dead_code)]
    pub fn refs_to(&self, target: SymbolId) -> Vec<&Ref> {
        self.refs.iter()
            .filter(|r| r.resolved_symbol() == Some(target))
            .collect()
    }

    /// Find all hash key accesses/definitions for a given owner.
    #[allow(dead_code)]
    pub fn hash_keys_for_owner(&self, owner: &HashKeyOwner) -> Vec<&Ref> {
        self.refs.iter()
            .filter(|r| r.hash_key_owner() == Some(owner))
            .collect()
    }

    /// Find all hash key definition symbols for a given owner.
    pub fn hash_key_defs_for_owner(&self, owner: &HashKeyOwner) -> Vec<&Symbol> {
        self.symbols.iter()
            .filter(|s| {
                if let SymbolDetail::HashKeyDef { owner: ref o, .. } = s.detail {
                    o.found_by(owner)
                } else {
                    false
                }
            })
            .collect()
    }

}
