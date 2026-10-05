//! Keyed access for packs: the identity half of a string-keyed subscript.
//!
//! The drill (`Projected{HashKey}`) types `$a['k']`; this mints the ref and
//! the defs that let goto-def, references, rename and hover follow the key —
//! the same `HashKeyAccess` / `HashKeyDef` facts the Perl builder mints, with
//! the owner bound here from the container's origin rather than recovered
//! later from the container's spelling.

use std::collections::{HashMap, HashSet};

use crate::model::file_analysis::{
    AccessKind, FileAnalysis, FlowEdge, HashKeyOwner, MethodResolution, Ref, RefKind, Scope, ScopeId, Span, SymKind, Symbol,
    SymbolDetail, SymbolId,
};

/// A string-keyed subscript (`$a['k']`): the key token, the whole subscript,
/// and its base expression.
#[derive(Debug, Clone)]
pub struct KeyAccessSite {
    pub key: String,
    pub key_span: Span,
    pub expr: Span,
    pub base: Span,
    pub scope: ScopeId,
    pub write: bool,
}

/// A variable-keyed subscript (`$a[$k]`): the key variable's read and the
/// subscript around it.
#[derive(Debug, Clone)]
pub struct DynamicKeySite {
    pub var: Span,
    pub expr: Span,
    pub base: Span,
    pub scope: ScopeId,
    pub write: bool,
}

/// A string literal assigned to a variable (`$k = 'host'`): the whole
/// literal (the assignment's source) and its content.
#[derive(Debug, Clone)]
pub struct FoldLiteral {
    pub lit: Span,
    pub content: String,
    pub content_span: Span,
}

/// One string key of an array literal (`['k' => v]`) and the literal it
/// sits in.
#[derive(Debug, Clone)]
pub struct KeyedLiteral {
    pub key: String,
    pub key_span: Span,
    pub lit: Span,
}

/// Everything the owner join reads, borrowed from the skeleton as
/// `into_file_analysis` holds it.
pub(crate) struct KeyedInputs<'a> {
    pub accesses: &'a [KeyAccessSite],
    pub literals: &'a [KeyedLiteral],
    /// `$rows[0]` subscripts: (expr, base) — a key subscript over one
    /// reaches the container through it.
    pub index_subscripts: &'a [(Span, Span)],
    pub dynamic_accesses: &'a [DynamicKeySite],
    pub fold_literals: &'a [FoldLiteral],
    pub symbols: &'a [Symbol],
    pub scopes: &'a [Scope],
    pub var_reads: &'a [(String, ScopeId, Span)],
    /// Re-assignments of a function-scoped variable (`$d['k'] = 1` after
    /// `$d = …`): a use of the one declaration, like a read.
    pub var_rebinds: &'a [(String, ScopeId, Span)],
    pub call_sites: &'a [(Span, String)],
    /// The file's import rows: a `use function` row names an imported
    /// function's own namespace.
    pub import_rows: &'a [crate::model::file_analysis::ImportRow],
    pub names: &'a crate::model::file_analysis::NameSpellings,
    pub return_sites: &'a [(ScopeId, Span)],
    pub flow_edges: &'a [FlowEdge],
    /// Member calls: (receiver span, method name, method token span) —
    /// `$b->cfg()` as the skeleton's member refs carry it.
    pub member_calls: Vec<(Span, String, Span)>,
}

fn within(inner: &Span, outer: &Span) -> bool {
    (outer.start.row, outer.start.column) <= (inner.start.row, inner.start.column)
        && (inner.end.row, inner.end.column) <= (outer.end.row, outer.end.column)
}

fn before_or_at(a: tree_sitter::Point, b: tree_sitter::Point) -> bool {
    (a.row, a.column) <= (b.row, b.column)
}

/// The nearest visible declaration of `name` from `scope` at `point`: the
/// latest one at or before the point, innermost scope first.
pub(crate) fn nearest_decl(
    defs_by_name: &HashMap<String, Vec<(ScopeId, Span, SymbolId)>>,
    scope_parent: &HashMap<ScopeId, Option<ScopeId>>,
    name: &str,
    scope: ScopeId,
    point: tree_sitter::Point,
) -> Option<SymbolId> {
    let cands = defs_by_name.get(name)?;
    let mut cur = Some(scope);
    while let Some(sc) = cur {
        let best = cands
            .iter()
            .filter(|(dscope, dspan, _)| *dscope == sc && before_or_at(dspan.start, point))
            .max_by_key(|(_, dspan, _)| (dspan.start.row, dspan.start.column));
        if let Some((_, _, did)) = best {
            return Some(*did);
        }
        cur = scope_parent.get(&sc).copied().flatten();
    }
    None
}

struct Join<'a> {
    inp: &'a KeyedInputs<'a>,
    defs_by_name: HashMap<String, Vec<(ScopeId, Span, SymbolId)>>,
    scope_parent: HashMap<ScopeId, Option<ScopeId>>,
    reads: HashMap<Span, (&'a str, ScopeId)>,
    /// A container that IS a declaration (`$d['k'] = 1` declares `$d`).
    decls: HashMap<Span, SymbolId>,
    calls: HashMap<Span, &'a str>,
    subscripts: HashMap<Span, Span>,
}

const MAX_HOPS: u8 = 8;

/// Where a container's keys come from, as far as extraction can tell.
enum Origin {
    Known(HashKeyOwner),
    /// A method's return: the receiver's type picks the method. `fallback`
    /// owns the keys when it does not resolve.
    Method { method_span: Span, method: String, fallback: Option<HashKeyOwner> },
}

/// A key ref waiting on its receiver's type.
pub(crate) struct PendingKeyRef {
    pub r: Ref,
    pub method_span: Span,
    pub method: String,
    pub fallback: Option<HashKeyOwner>,
}

impl<'a> Join<'a> {
    fn new(inp: &'a KeyedInputs<'a>) -> Self {
        let mut defs_by_name: HashMap<String, Vec<(ScopeId, Span, SymbolId)>> = HashMap::new();
        for s in inp.symbols {
            if matches!(s.kind, SymKind::Variable) {
                defs_by_name.entry(s.name.clone()).or_default().push((s.scope, s.selection_span, s.id));
            }
        }
        Join {
            inp,
            defs_by_name,
            scope_parent: inp.scopes.iter().map(|s| (s.id, s.parent)).collect(),
            reads: inp
                .var_reads
                .iter()
                .chain(inp.var_rebinds)
                .map(|(n, sc, sp)| (*sp, (n.as_str(), *sc)))
                .collect(),
            decls: inp
                .symbols
                .iter()
                .filter(|s| s.kind == SymKind::Variable)
                .map(|s| (s.selection_span, s.id))
                .collect(),
            calls: inp.call_sites.iter().map(|(sp, n)| (*sp, n.as_str())).collect(),
            subscripts: inp
                .accesses
                .iter()
                .map(|a| (a.expr, a.base))
                .chain(inp.index_subscripts.iter().copied())
                .collect(),
        }
    }

    fn scope_chain(&self, from: ScopeId) -> impl Iterator<Item = ScopeId> + '_ {
        std::iter::successors(Some(from), |sc| self.scope_parent.get(sc).copied().flatten())
    }

    /// The scope a point sits in: the innermost scope whose span holds it.
    fn scope_at(&self, at: &Span) -> Option<ScopeId> {
        self.inp
            .scopes
            .iter()
            .filter(|s| within(at, &s.span))
            .max_by_key(|s| (s.span.start.row, s.span.start.column))
            .map(|s| s.id)
    }

    fn sub_owner(&self, sym: &Symbol) -> HashKeyOwner {
        HashKeyOwner::Sub { package: sym.package.clone(), name: sym.name.clone() }
    }

    /// A free function call: the local function it names, else the one a
    /// `use function` row imports, else the bare name — the `Sub{None, name}`
    /// a namespace-less producer mints.
    fn call_owner(&self, callee: &str) -> HashKeyOwner {
        self.inp
            .symbols
            .iter()
            .find(|s| s.kind == SymKind::Sub && s.name == callee)
            .map(|s| self.sub_owner(s))
            .or_else(|| {
                let row = self.inp.import_rows.iter().find(|row| {
                    row.binds == crate::model::file_analysis::ImportBinds::Function
                        && row.bound.as_deref() == Some(callee)
                })?;
                let (ns, leaf) = crate::model::conventions::split_qualified(&row.raw, self.inp.names);
                Some(HashKeyOwner::Sub { package: ns.map(str::to_string), name: leaf.to_string() })
            })
            .unwrap_or(HashKeyOwner::Sub { package: None, name: callee.to_string() })
    }

    /// A member call's keys belong to the method its receiver dispatches
    /// to — a typing question this tier cannot answer, so it is deferred
    /// (`resolve_member_keys`) until the analysis exists.
    fn member_origin(&self, span: &Span) -> Option<Origin> {
        let (_, method, method_span) = self
            .inp
            .member_calls
            .iter()
            .filter(|(rs, _, ms)| rs.start == span.start && before_or_at(ms.end, span.end))
            .max_by_key(|(_, _, ms)| (ms.end.row, ms.end.column))?;
        Some(Origin::Method { method_span: *method_span, method: method.clone(), fallback: None })
    }

    /// What owns the keys of the value at `span` — a call's return, the
    /// variable holding it, or (through a subscript) its container's owner.
    fn value_owner(&self, span: &Span, hops: u8) -> Option<Origin> {
        if hops > MAX_HOPS {
            return None;
        }
        if let Some((name, scope)) = self.reads.get(span) {
            let did = nearest_decl(&self.defs_by_name, &self.scope_parent, name, *scope, span.start)?;
            return Some(self.var_owner(did, span.start, hops + 1));
        }
        if let Some(did) = self.decls.get(span) {
            return Some(self.var_owner(*did, span.start, hops + 1));
        }
        if let Some(callee) = self.calls.get(span) {
            return Some(Origin::Known(self.call_owner(callee)));
        }
        if let Some(base) = self.subscripts.get(span) {
            return self.value_owner(base, hops + 1);
        }
        self.member_origin(span)
    }

    /// A variable's keys belong to where its value came from: the latest
    /// assignment before `at` decides. A keyed literal (or no origin at all)
    /// leaves the variable itself as the owner; a copy follows its source.
    fn var_owner(&self, did: SymbolId, at: tree_sitter::Point, hops: u8) -> Origin {
        let sym = &self.inp.symbols[did.0 as usize];
        let own = HashKeyOwner::Variable { name: sym.name.clone(), def_scope: sym.scope };
        let origin = self
            .inp
            .flow_edges
            .iter()
            .filter(|fe| fe.target_name == sym.name && before_or_at(fe.target_at, at))
            .filter(|fe| {
                nearest_decl(&self.defs_by_name, &self.scope_parent, &fe.target_name, fe.target_scope, fe.target_at)
                    == Some(did)
            })
            .max_by_key(|fe| (fe.target_at.row, fe.target_at.column));
        match origin.filter(|fe| !self.is_literal(&fe.source)).and_then(|fe| self.value_owner(&fe.source, hops)) {
            // An untyped receiver still leaves the variable owning its keys.
            Some(Origin::Method { method_span, method, fallback: None }) => {
                Origin::Method { method_span, method, fallback: Some(own) }
            }
            Some(o) => o,
            None => Origin::Known(own),
        }
    }

    /// The one string literal a key variable folds to: every assignment to
    /// its declaration must be that single literal, the way Perl folds a
    /// method name (`constant_string_source`). A parameter, a loop variable
    /// or a second assignment folds to nothing.
    fn fold(&self, var: &Span) -> Option<&'a FoldLiteral> {
        let (name, scope) = self.reads.get(var)?;
        let did = nearest_decl(&self.defs_by_name, &self.scope_parent, name, *scope, var.start)?;
        let mut writes = self.inp.flow_edges.iter().filter(|fe| {
            fe.target_name == *name
                && nearest_decl(&self.defs_by_name, &self.scope_parent, &fe.target_name, fe.target_scope, fe.target_at)
                    == Some(did)
        });
        let (only, None) = (writes.next()?, writes.next()) else { return None };
        self.inp.fold_literals.iter().find(|f| f.lit == only.source)
    }

    fn is_literal(&self, span: &Span) -> bool {
        self.inp.literals.iter().any(|l| l.lit == *span)
    }

    /// The outermost literal a keyed literal nests in through literal values
    /// only: a call between them (`['a' => f(['b' => 1])]`) breaks the chain.
    fn root_literal(&self, lit: &Span) -> Span {
        self.inp
            .literals
            .iter()
            .map(|l| l.lit)
            .filter(|outer| {
                within(lit, outer)
                    && !self.inp.call_sites.iter().any(|(c, _)| within(c, outer) && within(lit, c) && c != lit)
            })
            .min_by_key(|outer| (outer.start.row, outer.start.column, std::cmp::Reverse((outer.end.row, outer.end.column))))
            .unwrap_or(*lit)
    }

    /// Who owns a literal's keys: the callable that returns it, or the
    /// variable an assignment stores it in. A literal passed anywhere else
    /// owns nothing.
    fn literal_owner(&self, lit: &Span) -> Option<HashKeyOwner> {
        let root = self.root_literal(lit);
        for (ret_scope, ret_span) in self.inp.return_sites {
            if *ret_span != root {
                continue;
            }
            let sid = self
                .scope_chain(*ret_scope)
                .find_map(|sc| self.inp.scopes.get(sc.0 as usize).and_then(|s| s.owner))?;
            return Some(self.sub_owner(&self.inp.symbols[sid.0 as usize]));
        }
        let fe = self.inp.flow_edges.iter().find(|fe| fe.source == root)?;
        let did = nearest_decl(&self.defs_by_name, &self.scope_parent, &fe.target_name, fe.target_scope, fe.target_at)?;
        let sym = &self.inp.symbols[did.0 as usize];
        Some(HashKeyOwner::Variable { name: sym.name.clone(), def_scope: sym.scope })
    }
}

/// Mint the key defs and key refs for a pack file. Defs take ids from
/// `next_id`; every ref arrives owner-bound, and `build_indices` links it to
/// the def with the same (name, owner).
pub(crate) fn mint_keyed_access(
    inp: &KeyedInputs<'_>,
    next_id: usize,
) -> (Vec<Symbol>, Vec<Ref>, Vec<PendingKeyRef>) {
    let join = Join::new(inp);
    let mut defs = Vec::new();
    let mut seen: HashSet<(String, Span)> = HashSet::new();
    for l in inp.literals {
        if !seen.insert((l.key.clone(), l.key_span)) {
            continue;
        }
        let Some(owner) = join.literal_owner(&l.lit) else { continue };
        let scope = join.scope_at(&l.key_span).unwrap_or(ScopeId(0));
        defs.push(Symbol {
            id: SymbolId((next_id + defs.len()) as u32),
            name: l.key.clone(),
            kind: SymKind::HashKeyDef,
            span: l.key_span,
            selection_span: l.key_span,
            scope,
            package: join.scope_chain(scope).find_map(|sc| inp.scopes[sc.0 as usize].package.clone()),
            detail: SymbolDetail::HashKeyDef { owner, is_dynamic: false },
            namespace: Default::default(),
            presentation: Default::default(),
            attributes: Vec::new(),
            flags: Default::default(),
            declared_with: None,
            deref_stack: Vec::new(),
            arity: None,
        });
    }
    let mut refs = Vec::new();
    let mut pending = Vec::new();
    for a in inp.accesses {
        // A base nothing can own mints no ref: an owner-less key ref would
        // claim the cursor and answer nothing.
        let Some(origin) = join.value_owner(&a.base, 0) else { continue };
        let r = Ref {
            // The owner is bound from the container's origin, so no consumer
            // reads its spelling — `var_text` stays empty on pack key refs.
            kind: RefKind::HashKeyAccess { var_text: String::new() },
            span: a.key_span,
            scope: a.scope,
            target_name: a.key.clone(),
            access: if a.write { AccessKind::Write } else { AccessKind::Read },
            binding: None,
            folded_from: None,
            arg_count: None,
            flags: Default::default(),
        };
        match origin {
            Origin::Known(owner) => {
                let mut r = r;
                r.bind_hash_key_owner(owner);
                refs.push(r);
            }
            Origin::Method { method_span, method, fallback } => {
                pending.push(PendingKeyRef { r, method_span, method, fallback })
            }
        }
    }
    // A folded key: the site is the subscript's bracket (`[$k]`, wider than
    // the `$k` read so the variable keeps its own cursor), and the rewrite
    // belongs on the literal it folded from — rename is one-way, from the key.
    for d in inp.dynamic_accesses {
        let Some(lit) = join.fold(&d.var) else { continue };
        let Some(origin) = join.value_owner(&d.base, 0) else { continue };
        let r = Ref {
            kind: RefKind::HashKeyAccess { var_text: String::new() },
            span: Span { start: d.base.end, end: d.expr.end },
            scope: d.scope,
            target_name: lit.content.clone(),
            access: if d.write { AccessKind::Write } else { AccessKind::Read },
            binding: None,
            folded_from: Some(lit.content_span),
            arg_count: None,
            flags: Default::default(),
        };
        match origin {
            Origin::Known(owner) => {
                let mut r = r;
                r.bind_hash_key_owner(owner);
                refs.push(r);
            }
            Origin::Method { method_span, method, fallback } => {
                pending.push(PendingKeyRef { r, method_span, method, fallback })
            }
        }
    }
    (defs, refs, pending)
}

/// Bind each pending key ref to the method its receiver dispatches to, now
/// that the analysis can type the receiver: the class that declares the
/// method owns the keys (an inherited method's return keys are its
/// declaring class's). An unresolved receiver falls back, or mints nothing.
pub(crate) fn resolve_member_keys(fa: &mut FileAnalysis, pending: Vec<PendingKeyRef>) {
    let mut out = Vec::new();
    for p in pending {
        let class = fa
            .refs()
            .iter()
            .find(|r| matches!(r.kind, RefKind::MethodCall { .. }) && r.span == p.method_span)
            .and_then(|call| fa.method_call_invocant_class(call, None));
        let owner = class
            .map(|class| {
                let package = match fa.resolve_method_in_ancestors(&class, &p.method, None) {
                    Some(MethodResolution::Local { class, .. }) => class,
                    _ => class,
                };
                HashKeyOwner::Sub { package: Some(package), name: p.method.clone() }
            })
            .or(p.fallback);
        if let Some(owner) = owner {
            let mut r = p.r;
            r.bind_hash_key_owner(owner);
            out.push(r);
        }
    }
    fa.adopt_key_refs(out);
}
