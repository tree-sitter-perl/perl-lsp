//! The pack lane: the facts only a non-Perl `LangPack` mints.
//!
//! Every field here is empty for a Perl analysis — the native builder has
//! no macros, no include graph, no template parameters, no move tracking.
//! Grouping them makes that emptiness one fact instead of ten, and gives
//! the lane one owner for its heap arm and its assembly seam
//! (`PackDriver::analyze_with_path` fills these post-construction).

use super::*;

/// Everything a pack driver records that Perl has no analog for. Stamped
/// by the pack driver's extract/skeleton pipeline; `Default` (all empty)
/// is what a Perl analysis carries.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PackFacts {
    /// The language's method-RECEIVER param names (Python `self`/`cls`),
    /// from the LangPack. A receiver is lexically inside the class so the
    /// sticky context tags it, but it is NOT a member — member completion
    /// and the outline exclude these names. Perl's receiver convention
    /// lives in `conventions.rs`, so this stays empty there.
    #[serde(default)]
    pub receiver_names: Vec<String>,
    /// Variables the runtime binds without a declaration (php `$this`,
    /// superglobals) — the undefined-variable lane's silence list.
    #[serde(default)]
    pub implicit_variables: Vec<String>,
    /// Methods whose presence makes a class answer any member name (php
    /// `__call`/`__get`) — the undefined-member lanes stay silent on it.
    #[serde(default)]
    pub catch_all_methods: Vec<String>,
    /// The member name that is the class-name literal (php `Foo::class`).
    #[serde(default)]
    pub class_literal_member: String,
    /// Type names are capitalized by convention (an import row with a
    /// lowercase leaf names a function or constant).
    #[serde(default)]
    pub types_are_capitalized: bool,
    /// Members every enum carries by language rule.
    #[serde(default)]
    pub enum_members: Vec<String>,
    /// Whole import-statement spans, in file order.
    #[serde(default)]
    pub import_rows: Vec<Span>,
    /// The import statement template, `{}` standing for the qualified name;
    /// empty when the language has no import quick-fix.
    #[serde(default)]
    pub import_template: String,
    /// The last row of the file preamble (open tag, `declare` rows).
    #[serde(default)]
    pub preamble_end: Option<usize>,
    /// Import rows bind names the file spells (php), so a row nothing
    /// spells is unused; false for text-splicing includes.
    #[serde(default)]
    pub imports_bind_names: bool,
    /// Imported names a doc comment mentions.
    #[serde(default)]
    pub doc_mentions: Vec<String>,

    /// The language's display vocabulary for the engine's value lattice:
    /// `format_inferred_type` tag → this language's spelling (php:
    /// `"HashRef"` → `"array"`, `"Numeric"` → `"int|float"`). Applied by
    /// `FileAnalysis::render_type` / `display_type_of` at every human
    /// surface; a tag not in the map (class names, parametrics) passes
    /// through. Empty for Perl — the engine's tags ARE its vocabulary.
    #[serde(default)]
    pub type_display: Vec<(String, String)>,

    /// The language's constructor-method names (php `__construct`), from
    /// the LangPack — the identity lane marks a Method target with one of
    /// these names as `ctor_of` its class, admitting construction sites
    /// into its references. Empty for Perl (`new` is a convention, not a
    /// keyword — `is_constructor_name` serves the ranking lanes instead).
    #[serde(default)]
    pub constructor_names: Vec<String>,

    /// Template-specialization family edges: canonical spec spelling
    /// (`formatter<int, char>`) → primary base name (`formatter`). NOT an
    /// inheritance edge — a spec REPLACES the primary wholesale (its member
    /// table is its own), so member resolution never falls through it; only
    /// the graph's `Specializes` family view (goto-implementation) traverses.
    #[serde(default)]
    pub specializes: HashMap<String, String>,

    /// Per-class template parameter names, in declaration order — primary
    /// templates keyed by base name (`Box` → `["T"]`), partial specs by
    /// their canonical spelling (`formatter<vector<T>>` → `["T"]`). The
    /// substitution axis instantiation-aware typing reads: a member type
    /// naming a param resolves against the receiver `Instance`'s args at
    /// the param's index (methods via `ParametricOp::ParamOf`, fields via
    /// `substitute_type_params`). A full spec (`template<>`) has no
    /// params, so its members never substitute — correct by construction.
    #[serde(default)]
    pub template_params: HashMap<String, Vec<String>>,

    /// Every `#define` in this file — the macro identity/navigation lane. One
    /// entry per `#define` (config variants share a name). Goto-def consults
    /// this to prefer the `#define` over a use's self-span, rank variants, and
    /// see through delegation wrappers.
    #[serde(default)]
    pub macro_defs: Vec<MacroDef>,

    /// `#include "x.h"` / `<x.h>` directives: (path-token span, raw path text).
    /// Goto-def on the path token resolves the header like `use` resolves a
    /// module.
    #[serde(default)]
    pub include_directives: Vec<(Span, String)>,

    /// `use A\B as C` rows: (alias, namespace, real leaf). The use-map
    /// pins the ALIAS spelling to the namespace and leaves the real leaf
    /// free for the file's own or same-namespace class.
    #[serde(default)]
    pub use_aliases: Vec<(String, String, String)>,

    /// Class spellings written with a qualifier: (leaf, written prefix —
    /// absolute when it starts with `\`, else relative to the file's
    /// namespace). A qualified spelling pins the leaf to that namespace
    /// rather than counting as a bare spelling.
    #[serde(default)]
    pub qualified_spellings: Vec<(String, String)>,

    /// This file's transitive `#include` closure — canonical header paths it
    /// reaches. The cross-file VISIBILITY key: a name resolves preferentially to
    /// a definition in a file this set contains (`ScopedLookup` ranks
    /// `get_cached` candidates by reachability; `docs/adr/macro-handling.md`,
    /// "the include-closure lie"). Empty for Perl, so the ranking is a no-op
    /// there (empty closure → global winner unchanged).
    #[serde(default)]
    pub include_closure: path_intern::ClosureList,

    /// FQ disambiguation rows for the per-package `parents` edges:
    /// `(child leaf, parent leaf, parent namespace)`, minted by
    /// namespace-relative packs (php — an alias/import/current-namespace
    /// resolution decided each edge). The family walks validate a
    /// leaf-keyed chain hop against these so same-named classes in
    /// different namespaces stop conflating; an absent row (Perl, cpp)
    /// means "no claim", never a prune.
    #[serde(default)]
    pub parent_namespaces: Vec<(String, String, String)>,

    /// Raw domain-typing sites: each `slot`-field access that interacts
    /// with a `value` token (`slot == V`, `slot = V`) at `slot_span`. The
    /// value's enum is resolved cross-file at query time (an enumerator
    /// carries its `enum`), then the sites fold onto the language-generic
    /// `Field{owner, name}` subject via `DomainCoherenceFold`. Stored raw
    /// (not pre-resolved) because both the slot owner AND the value's enum
    /// are cross-file for the perl5 `op_type`/`opcode` case — resolution
    /// belongs where the module index is in hand.
    #[serde(default)]
    pub domain_sites: Vec<DomainSite>,

    /// `std::move(x)` sites: (moved var name, move-call span, enclosing scope).
    /// A read of the var after the call and before its next rebind is a
    /// use-after-move bug — see `use_after_move_reads`.
    #[serde(default)]
    pub moved_from: Vec<(String, Span, ScopeId)>,

    /// Control-flow construct spans (`if`/`while`/`for`/`switch`/ternary/preproc
    /// conditionals). `use_after_move_reads` reads these for its straight-line
    /// gate (gate C): a move nested in one of these, relative to its enclosing
    /// scope, is not straight-line and is not flagged.
    #[serde(default)]
    pub control_regions: Vec<Span>,

    /// Parameter-list spans. `use_after_move_reads` gate E: a move of a variable
    /// declared inside one of these (a parameter) is not flagged — a moved
    /// parameter is a forwarding / subobject-move idiom this tier can't tell
    /// from a bug.
    #[serde(default)]
    pub param_regions: Vec<Span>,
}

impl PackFacts {
    /// The import row (`use` / `#include` path token) whose span covers
    /// `span`, if any. The one speller for "is this token inside an import
    /// row": the row's leaf carries its own ref; every other segment is a
    /// namespace no by-name lookup should answer for.
    /// The line an import quick-fix inserts at: right after the last import
    /// row that starts above `row`.
    pub fn import_insertion_line(&self, row: usize) -> Option<usize> {
        self.import_rows
            .iter()
            .filter(|r| r.start.row < row)
            .map(|r| r.end.row + 1)
            .max()
    }

    pub fn import_row_covering(&self, span: &Span) -> Option<&(Span, String)> {
        self.include_directives.iter().find(|(row, _)| {
            (row.start.row, row.start.column) <= (span.start.row, span.start.column)
                && (span.end.row, span.end.column) <= (row.end.row, row.end.column)
        })
    }

    /// Add this lane's footprint to a heap probe: the include bucket (the
    /// header-path duplication), the pack fact vectors, and the per-class
    /// template maps. See [`HeapBreakdown`].
    pub fn heap_add(&self, h: &mut HeapBreakdown) {
        // Sorted path-ids over the global table: 4 bytes per entry; the
        // table's string bytes are process-wide, counted once, not per file.
        h.include += self.include_closure.heap_bytes()
            + vcap(&self.include_directives)
            + self
                .include_directives
                .iter()
                .map(|(_, s)| s.capacity())
                .sum::<usize>();

        h.cpp_extras += vcap(&self.macro_defs)
            + vcap(&self.use_aliases)
            + vcap(&self.qualified_spellings)
            + vcap(&self.parent_namespaces)
            + vcap(&self.domain_sites)
            + vcap(&self.moved_from)
            + vcap(&self.control_regions)
            + vcap(&self.param_regions);

        h.misc += map_str_vec(&self.template_params)
            + mcap(&self.specializes)
            + vcap(&self.receiver_names)
            + vcap(&self.implicit_variables)
            + vcap(&self.catch_all_methods)
            + self.class_literal_member.capacity()
            + vcap(&self.enum_members)
            + vcap(&self.import_rows)
            + self.import_template.capacity()
            + vcap(&self.doc_mentions)
            + vcap(&self.type_display)
            + vcap(&self.constructor_names);
    }
}
