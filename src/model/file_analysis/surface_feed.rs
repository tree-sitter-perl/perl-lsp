//! The FileAnalysis → Surface classification gate.
//!
//! `surface_feed` destructures EVERY `FileAnalysis` field — no `..` rest
//! pattern — so a new field is a compile error HERE until its Surface fate
//! is decided: bind it into the feed (cross-file-visible → project it in
//! `Surface::project`, with an equality-net arm in `surface_tests.rs`) or
//! discard it under the group whose stated reason covers it. This is the
//! compiler-enforced half of R1 (`docs/adr/storage-engine.md`); the
//! equality-net tests remain the Surface-side half.

use super::*;

/// Everything `Surface::project` reads, borrowed from one `FileAnalysis`.
/// Field reads go through the named borrows. The `analysis` handle exists
/// ONLY for the derived queries whose conclusions land in projected values
/// (`symbol_return_type_via_bag`, `is_linkage_visible`,
/// `symbol_is_class_content`) — never for direct field access, which would
/// bypass the classification gate.
pub struct SurfaceFeed<'a> {
    pub symbols: &'a [Symbol],
    pub packages: &'a HashMap<String, PackageFacts>,
    pub imports: &'a [Import],
    pub plugin_loads: &'a [PluginLoadFact],
    pub export: &'a [String],
    pub export_ok: &'a [String],
    pub export_tags: &'a HashMap<String, Vec<String>>,
    pub reexport_modules: &'a [String],
    pub plugin_namespaces: &'a [PluginNamespace],
    pub app_surface_consumers: &'a [String],
    pub macro_defs: &'a [MacroDef],
    pub include_directives: &'a [(Span, String)],
    pub dbic_source_name: &'a Option<String>,
    /// Derived-query handle — see the struct doc for its narrow license.
    pub analysis: &'a FileAnalysis,
}

impl FileAnalysis {
    /// Classify every field for `Surface::project`. Exhaustive by
    /// construction: adding a `FileAnalysis` field fails to compile here
    /// until the author decides whether it is cross-file-visible.
    pub fn surface_feed(&self) -> SurfaceFeed<'_> {
        let Self {
            // ---- Cross-file-visible: bound into the feed and projected.
            // The whole symbol axis — declarations, their name/scope
            // indices, their baseline and eviction flag; the feed carries
            // the declarations.
            symbols,
            // Per-package entry: `parents`/`is_role` project; its
            // file-internal lanes are classified where it is read —
            // `Surface::project` destructures `PackageFacts` exhaustively,
            // so a new per-package fact is a compile error there.
            packages,
            imports,
            export,
            export_ok,
            export_tags,
            reexport_modules,
            dbic_source_name,

            // The pack and plugin lanes, destructured exhaustively for the
            // same reason the outer struct is: a new pack/plugin fact is a
            // compile error here until its Surface fate is decided.
            pack:
                PackFacts {
                    macro_defs,
                    include_directives,
                    // File-internal pack lanes: read LIVE from the
                    // provider's re-registered analysis per query.
                    receiver_names: _receiver_names, // LangPack-wide convention, identical across the pack's files
                    implicit_variables: _implicit_variables, // pack-wide convention
                    catch_all_methods: _catch_all_methods, // pack-wide convention
                    class_literal_member: _class_literal_member, // pack-wide convention
                    types_are_capitalized: _types_are_capitalized, // pack-wide convention
                    enum_members: _enum_members, // pack-wide convention
                    type_display: _type_display,     // LangPack-wide display vocabulary, same reasoning
                    constructor_names: _constructor_names, // LangPack-wide convention, identical across the pack's files
                    specializes: _specializes,       // family edges; the file re-registers on its own rebuild even when Unchanged
                    parent_namespaces: _parent_namespaces, // rides the parents edges (already projected); read live per query like specializes
                    use_aliases: _use_aliases,             // this file's OWN spelling table; a consumer never sees an alias
                    qualified_spellings: _qualified_spellings, // same: how THIS file spells classes
                    template_params: _template_params, // instantiation substitution reads the provider live
                    include_closure: _include_closure, // this file's OWN visibility ranking key; its freshness lane is the closure dep-stamp (`closure_stamp`)
                    domain_sites: _domain_sites,     // raw sites; domains resolve live at query time
                    moved_from: _moved_from,         // own-file use-after-move input
                    control_regions: _control_regions, // own-file straight-line gate spans
                    param_regions: _param_regions,   // own-file parameter-region spans
                },
            plugin:
                PluginFacts {
                    namespaces: plugin_namespaces,
                    loads: plugin_loads,
                    app_surface_consumers,
                    diagnostics: _plugin_diagnostics, // own-file diagnostics presentation
                    gated_emissions: _gated_emissions, // re-fired by this file's OWN enrichment
                },

            // ---- Projection inputs consumed through the derived-query
            // handle: their cross-file-visible conclusions land in
            // projected values, so an edit that changes what a consumer
            // can observe changes the projection.
            scopes: _scopes,                     // linkage-visibility gate + the return-type scope walk → `values`/`ret`
            witnesses: _witnesses,               // return-type resolution → `MethodSurface::ret`

            // ---- File-internal use-sites and span tables. Cross-file
            // readers (refs_to, groups, diagnostics scans) reach these
            // LIVE through the current analysis per query; no consumer
            // bakes them into cached enrichment, so freshness needs no
            // Surface edge for them.
            refs: _refs,                         // the whole reference axis — use-sites, their indices, their baseline and eviction flag
            fold_ranges: _fold_ranges,
            package_ranges: _package_ranges,
            call_bindings: _call_bindings,       // this file's call sites — the CONSUMER half of the edge; the provider names ride `imports`
            method_call_bindings: _method_call_bindings, // same consumer half, method form
            guard_sites: _guard_sites,           // own-file narrowing diagnostics
            arrow_deref_sites: _arrow_deref_sites, // own-file deref diagnostics
            key_writes: _key_writes,             // own-file mutation-extension input; resulting shapes surface via `ret`
            reassigned_scalars: _reassigned_scalars, // own-file shape trust gate
            flow_edges: _flow_edges,             // own-file value provenance
            unrowed_attachment_names: _unrowed_attachment_names, // derived residency digest for the consult pre-filter; changes only when the bag/tables it derives from change, so it carries no Surface edge of its own

            // ---- Consumer-side / own-file enrichment inputs and local
            // policy state: they shape THIS file's answers, not what
            // another file's cached state observes of this file.
            gated_param_types: _gated_param_types, // types this file's own params, query-gated
            provisional_dispatches: _provisional_dispatches, // this file's call-site candidates; foreign readers resolve them live
            loader_config_params: _loader_config_params, // callee markers joined by this file's OWN enrichment
            framework_imports: _framework_imports, // local diagnostic suppression set
            type_provenance: _type_provenance,   // read-only debugging aid (`--dump-package`)
            contract_symbols: _contract_symbols, // SymbolIds (banned from the Surface); the markers they tag project as methods
            column_keyed_verbs: _column_keyed_verbs, // baked from the plugin registry, not this file's source; the plugin fingerprint owns invalidation
            language: _language,                 // the origin's serving-language identity; `resolve()` reads it live per query, never baked into a consumer's cached state
            lib_roots: _lib_roots,               // `use lib` changes what THIS file sees of @INC, never what a consumer sees of this file
            dynamic_dispatch_sites: _dynamic_dispatch_sites, // heatmap soundness counter, read live
            attr_projections: _attr_projections, // grouping metadata for live rename/reference queries; every member is a synthesized symbol already projected

            // ---- Residency / lifecycle bookkeeping — no semantics.
            bag_evicted: _bag_evicted,
            degraded: _degraded,
            use_map_pins: _use_map_pins, // sessional memo of a projection over the tables above
            base_witness_count: _base_witness_count,
            stamped_at: _stamped_at, // sessional re-stamp bookkeeping; a consumer's view of this file does not change when we last stamped it

            // ---- Derived indices, rebuilt from the tables above.
            scope_starts: _scope_starts,
            export_lookup: _export_lookup,
        } = self;
        SurfaceFeed {
            symbols: symbols.as_slice(),
            packages,
            imports,
            plugin_loads,
            export,
            export_ok,
            export_tags,
            reexport_modules,
            plugin_namespaces,
            app_surface_consumers,
            macro_defs,
            include_directives,
            dbic_source_name,
            analysis: self,
        }
    }
}
