//! The symbol table: every symbol this file declares, together with the
//! indices derived from them, their eviction axis, and their enrichment
//! baseline.
//!
//! Symbols and their indices are one axis, so they live in one owner: an
//! `evict()` that forgot an index, or a rebuild that missed one, is not
//! expressible from outside. `FileAnalysis` holds exactly one of these and
//! delegates its symbol-shaped queries to it.

use super::*;

/// Every symbol in one file, plus the derived lookups over them.
///
/// Serialized fields are the symbols themselves and the enrichment
/// baseline; the two indices are `serde(skip)` and rebuilt after load, and
/// the eviction flag is `serde(skip)` so a rehydrated table is
/// symbols-present (an empty `symbols` on an evicted table means "on
/// disk", never "no symbols" — see `docs/adr/relational-ref-index.md`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SymbolTable {
    symbols: Vec<Symbol>,

    /// Enrichment baseline: `enrich_imported_types_with_keys` re-derives
    /// synthetic symbols (imported hash keys, gated emissions) on every
    /// run, so it truncates back to this length first and stays idempotent.
    #[serde(default)]
    base_count: usize,

    #[serde(skip, default)]
    evicted: bool,

    #[serde(skip, default)]
    by_name: HashMap<String, Vec<SymbolId>>,

    #[serde(skip, default)]
    by_scope: HashMap<ScopeId, Vec<SymbolId>>,
}

impl SymbolTable {
    /// Adopt a builder's walk-time symbol vec. Indices are built by
    /// `rebuild_indices`, which the owning `FileAnalysis` runs.
    pub fn from_vec(symbols: Vec<Symbol>) -> Self {
        SymbolTable { symbols, ..Default::default() }
    }

    pub fn as_slice(&self) -> &[Symbol] {
        &self.symbols
    }

    pub fn as_mut_slice(&mut self) -> &mut [Symbol] {
        &mut self.symbols
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Symbol> {
        self.symbols.iter()
    }

    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    pub fn get(&self, i: usize) -> Option<&Symbol> {
        self.symbols.get(i)
    }

    /// Append a symbol. Callers that append post-build (enrichment, gated
    /// emissions) refresh the indices afterwards.
    pub fn push(&mut self, s: Symbol) {
        self.symbols.push(s);
    }

    /// SymbolIds declared under `name`.
    pub fn named(&self, name: &str) -> &[SymbolId] {
        self.by_name.get(name).map_or(&[], |v| v.as_slice())
    }

    /// SymbolIds whose declaring scope is `scope`.
    pub fn in_scope(&self, scope: ScopeId) -> &[SymbolId] {
        self.by_scope.get(&scope).map_or(&[], |v| v.as_slice())
    }

    /// Every symbol's relational row seed, in symbol order. The two
    /// analysis-level predicates are injected because they read tables
    /// this one doesn't own — linkage visibility needs the declaring
    /// scope's kind, exportedness the `@EXPORT`/`@EXPORT_OK` surface.
    pub fn row_seeds(
        &self,
        linkage_visible: impl Fn(&Symbol) -> bool,
        exported: impl Fn(&str) -> bool,
    ) -> Vec<SymRowSeed> {
        self.symbols
            .iter()
            .map(|s| {
                let mut flags = 0u8;
                if linkage_visible(s) {
                    flags |= SymRowSeed::FLAG_LINKAGE_VISIBLE;
                }
                if s.hidden_in_outline() {
                    flags |= SymRowSeed::FLAG_HIDDEN_IN_OUTLINE;
                }
                if matches!(&s.detail, SymbolDetail::Sub { lexical: true, .. }) {
                    flags |= SymRowSeed::FLAG_LEXICAL_SUB;
                }
                if exported(&s.name) {
                    flags |= SymRowSeed::FLAG_EXPORTED;
                }
                SymRowSeed {
                    name: s.name.clone(),
                    kind: sym_kind_code(&s.kind),
                    span: s.selection_span,
                    container: s.package.clone(),
                    flags,
                }
            })
            .collect()
    }

    /// Seal the enrichment baseline at the current length.
    pub fn seal_baseline(&mut self) {
        self.base_count = self.symbols.len();
    }

    /// Drop everything enrichment appended, restoring the build-time prefix.
    pub fn truncate_to_baseline(&mut self) {
        self.symbols.truncate(self.base_count);
    }

    /// Rebuild the name and scope lookups from the current symbols.
    pub fn rebuild_indices(&mut self) {
        self.by_name.clear();
        self.by_scope.clear();
        for sym in &self.symbols {
            self.by_name.entry(sym.name.clone()).or_default().push(sym.id);
            self.by_scope.entry(sym.scope).or_default().push(sym.id);
        }
    }

    /// Strip the resident symbols and every index over them — the symbols
    /// eviction axis. Lossless: the on-disk analysis keeps the full vec,
    /// enumeration (workspace/symbol) answers from the `syms` rows, and
    /// detail reads rehydrate through `whole_present`. Idempotent.
    pub fn evict(&mut self) {
        self.symbols = Vec::new();
        self.by_name = HashMap::new();
        self.by_scope = HashMap::new();
        self.evicted = true;
    }

    /// True when `evict` stripped this copy.
    pub fn is_evicted(&self) -> bool {
        self.evicted
    }

    /// Add this table's footprint to a heap probe: the symbols bucket (vec
    /// + deep name/package/attribute strings) and the table's share of the
    /// rebuilt indices bucket. See [`HeapBreakdown`].
    pub fn heap_add(&self, h: &mut HeapBreakdown) {
        h.symbols += self.symbols.capacity() * std::mem::size_of::<Symbol>()
            + self
                .symbols
                .iter()
                .map(|s| {
                    s.name.capacity()
                        + s.package.as_ref().map_or(0, |p| p.capacity())
                        + s.attributes.capacity() * std::mem::size_of::<String>()
                        + s.attributes
                            .iter()
                            .map(|a| a.capacity() + std::mem::size_of::<String>())
                            .sum::<usize>()
                        + s.deref_stack.capacity() * std::mem::size_of::<DerefStep>()
                })
                .sum::<usize>();

        let mut b = mcap(&self.by_name) + mcap(&self.by_scope);
        for (k, v) in &self.by_name {
            b += k.capacity() + v.capacity() * std::mem::size_of::<SymbolId>();
        }
        for v in self.by_scope.values() {
            b += v.capacity() * std::mem::size_of::<SymbolId>();
        }
        h.rebuilt_indices += b;
    }
}

impl std::ops::Index<usize> for SymbolTable {
    type Output = Symbol;

    fn index(&self, i: usize) -> &Symbol {
        &self.symbols[i]
    }
}

impl std::ops::IndexMut<usize> for SymbolTable {
    fn index_mut(&mut self, i: usize) -> &mut Symbol {
        &mut self.symbols[i]
    }
}

impl<'a> IntoIterator for &'a SymbolTable {
    type Item = &'a Symbol;
    type IntoIter = std::slice::Iter<'a, Symbol>;

    fn into_iter(self) -> Self::IntoIter {
        self.symbols.iter()
    }
}

impl FileAnalysis {
    /// Every symbol this file declares. Empty on an evicted copy — see
    /// `symbols_are_evicted`.

    /// Adopt build-time symbols minted after assembly (the driver's path
    /// rails): ids assigned in order, indexed, and sealed into the
    /// enrichment baseline — facts of the build, not enrichment.
    pub fn adopt_path_symbols(&mut self, symbols: Vec<Symbol>) {
        if symbols.is_empty() {
            return;
        }
        for mut s in symbols {
            s.id = SymbolId(self.symbols.len() as u32);
            self.symbols.push(s);
        }
        self.symbols.rebuild_indices();
        self.symbols.seal_baseline();
    }

    pub fn symbols(&self) -> &[Symbol] {
        self.symbols.as_slice()
    }

    /// Mutable symbol view for the post-build stamping passes. Bindings
    /// only — the vec's shape is fixed here, so `SymbolId`s (positional)
    /// and the indices stay valid.
    // Its callers are the pack lane's attribute/access-region stamps,
    // compiled behind the pack language features; a Perl-only build has no
    // mutator outside `model/`.
    #[allow(dead_code)]
    pub fn symbols_mut(&mut self) -> &mut [Symbol] {
        self.symbols.as_mut_slice()
    }

    /// Project every symbol into its relational row seed
    /// (`docs/adr/relational-ref-index.md`) — the shredder's input, twin
    /// of `ref_row_seeds`. Exportedness reads the SAME `export`/`export_ok`
    /// surface the Surface projection does (`exports_name` →
    /// `export_lookup`), so "exported" never drifts between the two.
    pub fn sym_row_seeds(&self) -> Vec<SymRowSeed> {
        self.symbols
            .row_seeds(|s| self.is_linkage_visible(s), |n| self.exports_name(n))
    }
}
