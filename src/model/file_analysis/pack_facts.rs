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
    /// rail → how the undefined-name lane phrases a miss on it (`"event"`
    /// → `No listener for event`); default `Undefined <rail>`.
    #[serde(default)]
    pub rail_labels: Vec<(String, String)>,
    /// Rails whose miss is a hint: their definitions are partly
    /// runtime-only, so an unmatched name is a lead, not an error.
    #[serde(default)]
    pub rail_hints: Vec<String>,
    /// Rails whose names are CLASS identities (the rail document's
    /// `names_are: class` — Laravel's event bus). Per-overlay data the file
    /// carries, like `rail_labels`: which overlays load is a property of the
    /// workspace, not of the language, so it is not a language convention
    /// reached by id. Baked from the DECLARATION for every file of the pack,
    /// so the span-free minting paths (`scan_text_rails`, `adopt_path_rails`)
    /// carry it by construction and a rail cannot answer differently in two
    /// files. Read through `HandlerOwner::names_are`.
    #[serde(default)]
    pub class_named_rails: Vec<String>,
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

    /// `#include "x.h"` / `<x.h>` / `use A\B` rows. Goto-def on the path
    /// token resolves the header like `use` resolves a module; the use-map
    /// resolves class spellings through the same rows.
    #[serde(default)]
    pub include_directives: Vec<ImportRow>,

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
    /// This language's write and display spellings, attached by id rather
    /// than serialized: the same value for every file of a language, so it
    /// is not a per-file fact (rule #14). `None` until attached — read it
    /// through `FileAnalysis::spellings()`, which answers the neutral
    /// defaults for a language that declares none.
    #[serde(skip)]
    pub spellings: Option<&'static PackSpellings>,
    /// Slots whose docblock type no value can share with the declared one
    /// (`: int` + `@return string`). Per-FILE by nature — it is a property
    /// of the two spellings the author wrote at one site, not of the
    /// language — and minted at the merge that chose the declaration, so
    /// the hint lane never re-reads a comment to find out.
    #[serde(default)]
    pub doc_disagreements: Vec<DocDisagreement>,

    /// Existence-probe argument spans (`@probe.region`: php `isset(…)` /
    /// `empty(…)`). A member read inside one IS the question of whether
    /// the member exists; the undefined-member lanes stay silent there.
    #[serde(default)]
    pub probe_regions: Vec<Span>,
}

impl PackFacts {
    /// The import row (`use` / `#include` path token) whose span covers
    /// `span`, if any. The one speller for "is this token inside an import
    /// row": the row's leaf carries its own ref; every other segment is a
    /// namespace no by-name lookup should answer for.
    pub fn import_row_covering(&self, span: &Span) -> Option<&ImportRow> {
        self.include_directives.iter().find(|r| {
            (r.span.start.row, r.span.start.column) <= (span.start.row, span.start.column)
                && (span.end.row, span.end.column) <= (r.span.end.row, r.span.end.column)
        })
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
                .map(|r| r.raw.capacity())
                .sum::<usize>();

        h.cpp_extras += vcap(&self.macro_defs)
            + vcap(&self.use_aliases)
            + vcap(&self.qualified_spellings)
            + vcap(&self.domain_sites)
            + vcap(&self.moved_from)
            + vcap(&self.control_regions)
            + vcap(&self.param_regions)
            + vcap(&self.probe_regions)
            + vcap(&self.doc_disagreements);

        h.misc += map_str_vec(&self.template_params)
            + mcap(&self.specializes)
            + vcap(&self.import_rows)
            + self.rail_labels.iter().map(|(a, b)| a.capacity() + b.capacity()).sum::<usize>()
            + self.rail_hints.iter().map(|a| a.capacity()).sum::<usize>()
            + self.class_named_rails.iter().map(|a| a.capacity()).sum::<usize>()
            + vcap(&self.doc_mentions);
    }
}

/// A documented type and the declared type it contradicts, at the declaration
/// whose declared type won. The `doc-type-mismatch` hint renders both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocDisagreement {
    /// The declaration site — where the two spellings meet.
    pub span: Span,
    pub declared: InferredType,
    pub documented: InferredType,
}

/// A language's WRITE and DISPLAY spellings — what a quick-fix inserts and
/// what a human surface renders. Every field is the same for every file of
/// the language, so these are reached by language id
/// (`LanguageRegistry::spellings`) and attached to an analysis as a
/// pointer; serializing them would put one language's constants in every
/// blob (rule #14). A pack declares one `const`; `NONE` is what a language
/// without a pack answers, and it is what the engine assumed before any
/// pack declared spellings.
#[derive(Debug, Clone, Copy)]
pub struct PackSpellings {
    /// Engine type tag → this language's spelling (php `"HashRef"` →
    /// `"array"`). Applied by `render_type` / `display_type_of` at every
    /// human surface; an unmapped tag passes through.
    pub type_display: &'static [(&'static str, &'static str)],
    /// Engine type tag → the spelling a DECLARATION is written with. Unlike
    /// `type_display` this is what goes into the source, so a tag with no
    /// unambiguous native spelling is absent rather than guessed.
    pub native_type_spellings: &'static [(&'static str, &'static str)],
    /// The member name that is the class-name literal (php `Foo::class`).
    pub class_literal_member: &'static str,
    /// The import statement that brings a qualified name into scope, `{}`
    /// standing for the name; empty = no import quick-fix.
    pub import_template: &'static str,
    /// How a stub for one unimplemented contract declarator is spelled
    /// (`{}` = the declarator); empty = no quick-fix.
    pub contract_stub: &'static str,
    /// How a native return annotation is spelled after the parameter list
    /// (`{}` = the type); empty = the language writes none.
    pub return_annotation_template: &'static str,
    /// The sigil a static property carries after the scope operator (php
    /// `self::$count`); empty = the bare name in both positions.
    pub static_property_sigil: &'static str,
    /// A member declaration belongs to the container that encloses it and
    /// nothing else — no cross-package installs (Perl's typeglobs), so
    /// contract provision is package-attributed.
    pub members_are_package_bound: bool,
    /// Reading a member IS calling it: Perl's `$o->name` invokes the
    /// accessor, so a call may legitimately land on a stored slot and a
    /// callable ask admits a value declaration. A language that spells the
    /// call (`$obj->name()` vs `$obj->name`) says `false` — there the two
    /// syntaxes name two different members, and admitting the value one is
    /// how a missing `()` resolves to a property instead of being reported.
    pub member_reads_are_calls: bool,
}

impl PackSpellings {
    /// What a language that declares no spellings answers.
    pub const NONE: PackSpellings = PackSpellings {
        type_display: &[],
        native_type_spellings: &[],
        class_literal_member: "",
        import_template: "",
        contract_stub: "",
        return_annotation_template: "",
        static_property_sigil: "",
        members_are_package_bound: false,
        // The SAFE answer, not the lenient one: a language that has not
        // said its member read is a call gets the strict rule, where
        // `$obj->name()` does not resolve to a property `name`. Leniency
        // is what hides a missing `()`, so it is opted INTO — Perl opts in
        // (`conventions::PERL_PACK_SPELLINGS`), and a new pack that forgets
        // to declare inherits the answer that reports rather than the one
        // that goes quiet.
        member_reads_are_calls: false,
    };
}

/// `PackSpellings::NONE` with a `'static` address, so `spellings()` can
/// hand out a reference without the caller owning one.
pub static NEUTRAL_SPELLINGS: PackSpellings = PackSpellings::NONE;

/// One import row as the file wrote it: the path/name token's span, the raw
/// text, and what the row BINDS. A use-map language spells the binding out
/// (php `use function`, `use const`), so the producer states it rather than
/// leaving a consumer to guess from the leaf's capitalization — a guess that
/// is wrong for every lower-case class and every upper-case constant
/// (rule #11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportRow {
    pub span: Span,
    pub raw: String,
    #[serde(default)]
    pub binds: ImportBinds,
}

/// What an import row brings into the file's namespace. `Type` is the
/// default because it is what an unqualified row means in every language
/// that has one — a bare `use A\B`, a C `#include`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ImportBinds {
    #[default]
    Type,
    Function,
    Const,
}
