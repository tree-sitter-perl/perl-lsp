//! The plugin lane: what plugins declared, emitted, and deferred for this
//! file.
//!
//! Native Perl resolution never mints any of it — every field is fed by
//! the plugin registry (`docs/adr/plugin-system.md`), so the lane has one
//! owner for its assembly, its Surface bindings and its heap arm.

use super::*;

/// Everything plugin machinery contributed to one file's analysis.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginFacts {
    /// Plugin-declared namespaces. Each is a scope managed by a plugin
    /// (a Mojolicious app, a Minion instance, an event-emitter subclass,
    /// …). Declares bridges into Perl-space and owns a set of entities.
    /// Lookups union these with native Perl resolution — see
    /// `ModuleIndex::for_each_entity_bridged_to` for the cross-file primitive.
    #[serde(default)]
    pub namespaces: Vec<PluginNamespace>,

    /// Caller-side loader facts: this file loads plugin `name` and
    /// passes the value at `config_span`. Joined at enrichment with
    /// the loaded module's `loader_config_params` markers.
    #[serde(default)]
    pub loads: Vec<PluginLoadFact>,

    /// Plugin-emitted diagnostics (`EmitAction::Diagnostic` from a
    /// pattern's `on_match`). `collect_diagnostics` converts them to
    /// LSP diagnostics alongside the native channels; provenance rides
    /// on `plugin_id` (surfaced as the diagnostic source).
    #[serde(default)]
    pub diagnostics: Vec<PluginDiagnostic>,

    /// Plugin pattern emissions deferred because a `ClassIsa` trigger
    /// couldn't be confirmed against LOCAL-only ancestry at build (rule #1).
    /// Re-fired by `enrich_imported_types_with_keys` when the package's
    /// cross-file ancestry resolves a gate prefix — the emission analog of
    /// `provisional_dispatches`. See `GatedEmission`.
    #[serde(default)]
    pub gated_emissions: Vec<GatedEmission>,

    /// Manifest-declared app-surface consumer classes
    /// (`FrameworkPlugin::app_surface_consumers`), baked from the plugin
    /// registry at build so the query-time ancestor walk can inject the
    /// synthetic `APP_SURFACE_CLASS` parent (`parents_of`) without
    /// re-reading the registry.
    #[serde(default)]
    pub app_surface_consumers: Vec<String>,

    /// Manifest-declared framework meta-methods
    /// (`FrameworkPlugin::meta_methods`), baked from the registry at build.
    /// The unresolved-method diagnostic consults this union so core keeps
    /// only the true `UNIVERSAL::` surface — a per-framework name list in a
    /// consumer is the rule-#10 shape this exists to prevent.
    #[serde(default)]
    pub meta_methods: Vec<String>,
}

impl PluginFacts {
    /// Add this lane's footprint to a heap probe: the emission vectors and
    /// the declared-name lists. See [`HeapBreakdown`].
    pub fn heap_add(&self, h: &mut HeapBreakdown) {
        h.cpp_extras += vcap(&self.loads) + vcap(&self.gated_emissions);
        h.misc += vcap(&self.namespaces)
            + vcap(&self.diagnostics)
            + vcap(&self.app_surface_consumers)
            + vcap(&self.meta_methods);
    }
}
