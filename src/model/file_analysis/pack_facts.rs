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
    /// Whole import-statement spans, in file order.
    #[serde(default)]
    pub import_rows: Vec<Span>,
    /// The import statement template, `{}` standing for the qualified name;
    /// empty when the language has no import quick-fix.
    #[serde(default)]
    pub import_template: String,
    /// Engine type name → native spelling a declaration is written with.
    #[serde(default)]
    pub native_type_spellings: Vec<(String, String)>,
    /// Rails whose names are CLASS identities (the rail document's
    /// `names_are: class` — Laravel's event bus). Per-overlay data the
    /// file carries: which overlays load is a property of the workspace,
    /// not of the language, so it is not a language convention reached by
    /// id. Read through `HandlerOwner::names_are`.
    #[serde(default)]
    pub class_named_rails: Vec<String>,
    /// The last row of the file preamble (open tag, `declare` rows).
    #[serde(default)]
    pub preamble_end: Option<usize>,
    /// A member declaration belongs only to its enclosing container (no
    /// cross-package installs): contract provision is package-attributed.
    #[serde(default)]
    pub members_are_package_bound: bool,
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

    /// `#include "x.h"` / `<x.h>` directives and import paths as written:
    /// (path-token span, raw path text). Goto-def on the path token
    /// resolves the header like `use` resolves a module. A use-map
    /// language's `use` rows are `use_rows` instead.
    #[serde(default)]
    pub include_directives: Vec<(Span, String)>,

    /// A use-map language's `use` rows, as the (namespace, leaf, alias)
    /// parts the extractor read them in — the table `UseMap` resolves
    /// through. An aliased row binds its ALIAS and leaves the real leaf
    /// free for the file's own or same-namespace class.
    #[serde(default)]
    pub use_rows: Vec<UseRow>,

    /// Class spellings written with a qualifier: (leaf, written prefix —
    /// absolute when it starts with `\`, else relative to the file's
    /// namespace). A qualified spelling pins the leaf to that namespace
    /// rather than counting as a bare spelling.
    #[serde(default)]
    pub qualified_spellings: Vec<(String, String)>,

    /// How this analysis's language spells names — its separator and its
    /// sigils — the data every key function reads (rule #12). Perl's
    /// builder bakes `conventions::PERL_SPELLINGS`; a pack bakes
    /// `LangPack::names`. The use-map questions (`identity_namespace`,
    /// `class_spelling_identity`) gate on its `class_spelling`.
    ///
    /// A per-language constant carried per file ON PURPOSE (the rule #14
    /// exception): a key is computed wherever an analysis is in hand — the
    /// row store's probes, a target minted from an origin — and the
    /// alternative is a process-wide language registry, which cannot say
    /// which language a given name belongs to. A few bytes per blob.
    #[serde(default)]
    pub names: NameSpellings,


    /// This file's transitive `#include` closure — canonical header paths it
    /// reaches. The cross-file VISIBILITY key: a name resolves preferentially to
    /// a definition in a file this set contains (`ScopedLookup` ranks
    /// `get_cached` candidates by reachability; `docs/adr/macro-handling.md`,
    /// "the include-closure lie"). Empty for Perl, so the ranking is a no-op
    /// there (empty closure → global winner unchanged).
    #[serde(default)]
    pub include_closure: path_intern::ClosureList,


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

    /// `std::move` sites. A read of the moved part after the call and
    /// before it is reset is a use-after-move bug — see
    /// `use_after_move_reads`.
    #[serde(default)]
    pub moved_from: Vec<MoveSite>,

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
    /// Existence-probe argument spans (`@probe.region`: php `isset(…)` /
    /// `empty(…)`). A member read inside one IS the question of whether
    /// the member exists; the undefined-member lanes stay silent there.
    #[serde(default)]
    pub probe_regions: Vec<Span>,
    /// The module this file was filed under, for a language that names
    /// modules by path — the one input the analysis read from outside its
    /// source. Per file and not a witness: the warm scan compares it with
    /// the module the path names NOW (`LanguageDriver::path_inputs_current`),
    /// so a blob built before a package marker appeared re-analyzes
    /// instead of serving classes under the old name.
    #[serde(default)]
    pub file_module: Option<FileModule>,
}

/// The module a file is, for a language whose module names come from the
/// file's place on disk (python's `pkg/models.py` is `pkg.models`): its
/// path segments, and whether the file is its package's own module
/// (`pkg/__init__.py` is `pkg`, and relative imports start from `pkg`
/// itself rather than its parent). Derived by the driver, which has the
/// path; the extractor only reads it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileModule {
    pub segments: Vec<String>,
    pub is_package: bool,
}

impl FileModule {
    /// The package relative imports resolve from.
    pub fn package(&self) -> &[String] {
        match self.is_package {
            true => &self.segments,
            false => &self.segments[..self.segments.len().saturating_sub(1)],
        }
    }
}

impl PackFacts {
    /// Is `span` inside an import row (a `use` row's name or an
    /// `#include` path token)? The one speller for that question: the
    /// row's leaf carries its own ref; every other segment is a namespace
    /// no by-name lookup should answer for.
    pub fn in_import_row(&self, span: &Span) -> bool {
        self.use_row_covering(span).is_some()
            || self.include_directives.iter().any(|(row, _)| span_within(span, row))
    }

    /// The `use` row whose name token covers `span`, if any.
    pub fn use_row_covering(&self, span: &Span) -> Option<&UseRow> {
        self.use_rows.iter().find(|row| span_within(span, &row.span))
    }

    /// The line an import quick-fix inserts at: right after the last import
    /// row that starts above `row`.
    pub fn import_insertion_line(&self, row: usize) -> Option<usize> {
        self.import_rows
            .iter()
            .filter(|r| r.start.row < row)
            .map(|r| r.end.row + 1)
            .max()
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
            + vcap(&self.use_rows)
            + vcap(&self.qualified_spellings)
            + vcap(&self.domain_sites)
            + vcap(&self.moved_from)
            + vcap(&self.control_regions)
            + vcap(&self.param_regions)
            + vcap(&self.probe_regions);

        h.misc += map_str_vec(&self.template_params)
            + mcap(&self.specializes)
            + vcap(&self.import_rows)
            + self.import_template.capacity()
            + vcap(&self.native_type_spellings)
            + self.class_named_rails.iter().map(|a| a.capacity()).sum::<usize>()
            + vcap(&self.doc_mentions)
            + vcap(&self.type_display)
            + self.file_module.as_ref().map_or(0, |m| vcap(&m.segments) + m.segments.iter().map(String::capacity).sum::<usize>());
    }
}

fn span_within(inner: &Span, outer: &Span) -> bool {
    (outer.start.row, outer.start.column) <= (inner.start.row, inner.start.column)
        && (inner.end.row, inner.end.column) <= (outer.end.row, outer.end.column)
}

/// A `std::move` of a place (`x`, `other.msg`): what it moved, the call,
/// the scope it runs in, and the accesses that touch the moved part,
/// decided where the places are built so the check never re-reads a
/// spelling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveSite {
    pub place: super::Place,
    /// The moved subject as written, for the rendered message.
    pub spelling: String,
    pub call: Span,
    pub scope: ScopeId,
    /// The variable token of every read reaching the moved part: a read of
    /// a sibling member (`other.id` after moving `other.msg`) is not one.
    pub reads: Vec<Span>,
    /// Writes of the moved part or of what holds it (`other.msg = …`),
    /// each of which gives it a value again.
    pub resets: Vec<Span>,
}
