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

    /// The linkage-visible feed a registration extracts from a WHOLE
    /// analysis: (name, declares-a-Class) per visible symbol. Collected
    /// before any strip so the feeds and tie-breaks never read an emptied
    /// `symbols`.
    pub fn linkage_feed(&self) -> Vec<(String, bool)> {
        let mut index: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        let mut feed: Vec<(String, bool)> = Vec::new();
        for sym in self.symbols() {
            // The same predicate completion gathering uses, so every name
            // registered here is also offerable and vice versa.
            if !self.is_linkage_visible(sym) {
                continue;
            }
            let is_class = matches!(sym.kind, SymKind::Class);
            match index.get(sym.name.as_str()) {
                // A file declaring both a value AND a Class under one name
                // ranks as a Class.
                Some(&i) => feed[i].1 |= is_class,
                None => {
                    index.insert(sym.name.as_str(), feed.len());
                    feed.push((sym.name.clone(), is_class));
                }
            }
        }
        // Class rank is visibility-INDEPENDENT: a non-linkage-visible Class
        // sharing a visible value's name still ranks the file as declaring
        // that Class.
        for sym in self.symbols() {
            if matches!(sym.kind, SymKind::Class) {
                if let Some(&i) = index.get(sym.name.as_str()) {
                    feed[i].1 = true;
                }
            }
        }
        feed
    }

    /// Every `package X;` / `class X` this file declares, in declaration
    /// order, deduped — `(name, is-class)`, the `linkage_feed` shape. A
    /// multi-package `.pm` is ordinary Perl, and keying on the first name
    /// alone leaves every later package unreachable by name. Empty for
    /// scripts with no explicit package declaration.
    pub fn package_names(&self) -> Vec<(String, bool)> {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut out = Vec::new();
        for sym in self.symbols() {
            if matches!(sym.kind, SymKind::Package | SymKind::Class) && seen.insert(sym.name.as_str()) {
                out.push((sym.name.clone(), matches!(sym.kind, SymKind::Class)));
            }
        }
        out
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
    /// "offered" never drift apart. A def the pack named for it
    /// (`ANONYMOUS`: an anonymous class, union or sub) has no spelling
    /// another file could write, so it is never part of the surface.
    pub fn is_linkage_visible(&self, sym: &Symbol) -> bool {
        if sym.flags.contains(SymbolFlags::ANONYMOUS) {
            return false;
        }
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
