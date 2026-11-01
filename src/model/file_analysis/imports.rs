//! Import/export surface types: `Import`, `ImportedSymbol`, `ExportSurface`
//! and the `imported_names` selector logic.

use super::*;

// ---- Import ----

/// One name brought into scope by a `use` statement.
///
/// `local_name` is how the name appears at call sites in this file.
/// `remote_name` is the sub's real name in the source module — usually
/// identical to `local_name` (the common case, encoded as `None` to
/// keep the serialized form compact) and different only for renaming
/// imports: `del` in Mojolicious::Lite is really `delete` on
/// Mojolicious::Routes::Route, `use Exporter::Tiny ( foo => { -as => 'bar' } )`
/// gives a `bar` locally pointing at the real `foo`, etc.
///
/// Cross-file lookups always resolve via `remote()` so hover, gd, sig
/// help, and return-type inference use the real module's `sub_info`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ImportedSymbol {
    pub local_name: String,
    /// `None` means the local and remote names are the same.
    ///
    /// No `skip_serializing_if` — bincode is a non-self-describing
    /// format and needs the field present on the wire regardless.
    /// Self-describing formats (JSON, YAML) happily encode `null`
    /// for None too, so keeping it always-serialized is fine
    /// everywhere.
    #[serde(default)]
    pub remote_name: Option<String>,
}

impl ImportedSymbol {
    /// Same-name import — the overwhelmingly common case.
    pub fn same(name: impl Into<String>) -> Self {
        Self { local_name: name.into(), remote_name: None }
    }
    /// Renaming import — local name differs from the real sub's name.
    ///
    /// Currently constructed only from Rhai plugins via serde-deserialized
    /// maps (`#{ local_name: ..., remote_name: ... }`), so this Rust-side
    /// constructor has no direct caller yet. Keeping it as documented public
    /// API so future Rust callers (e.g. a hand-written parser for renaming
    /// import syntax, or core plugins) have the canonical way to build one.
    #[allow(dead_code)]
    pub fn renamed(local: impl Into<String>, remote: impl Into<String>) -> Self {
        let remote = remote.into();
        let local = local.into();
        // Collapse `remote == local` to the same-name shape so downstream
        // code doesn't need to handle both representations.
        if remote == local {
            Self { local_name: local, remote_name: None }
        } else {
            Self { local_name: local, remote_name: Some(remote) }
        }
    }
    /// Real name in the source module — used for cross-file sub_info lookup.
    pub fn remote(&self) -> &str {
        self.remote_name.as_deref().unwrap_or(&self.local_name)
    }
}

/// A `use Foo::Bar qw(func1 func2)` statement parsed from the source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Import {
    /// Module name, e.g. "List::Util".
    pub module_name: String,
    /// Explicitly imported names from `qw(...)`. Empty = bare `use Foo;`.
    /// Each entry carries local + (optional) remote name for renaming imports.
    pub imported_symbols: Vec<ImportedSymbol>,
    /// Span of the entire `use` statement.
    pub span: Span,
    /// Position of the closing delimiter of the `qw()` list (the `)` character).
    /// Used to insert new imports into an existing qw list.
    #[serde(with = "point_opt_serde")]
    pub qw_close_paren: Option<Point>,
    /// `use Foo ();` — explicit empty parens. Suppresses even `@EXPORT`
    /// (binds nothing), distinct from bare `use Foo;` (empty `imported_symbols`
    /// too, but auto-imports the defaults).
    #[serde(default)]
    pub empty_import: bool,
}

/// A producer module's resolved export surface — see
/// `FileAnalysis::export_surface`. The consumer-side `imported_names` evaluator
/// reads it; it never sees whether a name is the module's own or re-exported.
///
/// **Transitivity.** A module's surface can fold in other modules' surfaces via
/// `reexport_modules` (static `@Other::EXPORT` splice, loop-push, declarative
/// `also`). When built with a `ModuleIndex` (`export_surface_with_index`), the
/// re-export edges are walked transitively (cross-file, seen-set bounded for
/// cycles, fan-out budget) and the producer's default / optional / tag sets are
/// **materialized** to include the re-exported names. The closure is computed
/// here at query time, never baked into `FileAnalysis` — depth stays a
/// query-time edge property, exactly like the inheritance `parents_of` walk.
/// Without an index (`export_surface`, the back-compat path) only the module's
/// own surface is visible.
pub struct ExportSurface<'a> {
    /// The package's own lists as written; `None` = it declares none.
    pub(super) own: Option<&'a PackageExports>,
    /// `@EXPORT` ∪ re-exported defaults. `None` = own-only (no index walk).
    pub(super) default_set: Option<Vec<String>>,
    /// `@EXPORT_OK` ∪ re-exported optionals.
    pub(super) optional_set: Option<Vec<String>>,
    /// `%EXPORT_TAGS` ∪ re-exported tag members (per tag name).
    pub(super) tags: Option<HashMap<String, Vec<String>>>,
    /// Union of all names on the (transitive) surface for `exports()`.
    pub(super) all_names: Option<HashSet<String>>,
}


impl<'a> ExportSurface<'a> {
    /// `@EXPORT` (∪ re-exported defaults when index-walked) — auto-imported by a
    /// bare `use M;`.
    pub fn default_set(&self) -> &[String] {
        self.default_set.as_deref().or(self.own.map(|e| e.export.as_slice())).unwrap_or(&[])
    }

    /// `@EXPORT_OK` (∪ re-exported optionals) — opt-in only; never auto-imported.
    pub fn optional_set(&self) -> &[String] {
        self.optional_set.as_deref().or(self.own.map(|e| e.export_ok.as_slice())).unwrap_or(&[])
    }

    /// Members of a `%EXPORT_TAGS` tag, with `:DEFAULT` synthesized as
    /// `@EXPORT` (the Exporter special-case). The `tag` argument is the bare
    /// tag name (no `:`/`-` prefix). `None` if the tag is unknown.
    pub fn tag_members(&self, tag: &str) -> Option<Vec<&str>> {
        if tag.eq_ignore_ascii_case("DEFAULT") {
            return Some(self.default_set().iter().map(|s| s.as_str()).collect());
        }
        if let Some(tags) = &self.tags {
            return tags.get(tag).map(|v| v.iter().map(|s| s.as_str()).collect());
        }
        self.own?
            .export_tags
            .get(tag)
            .map(|v| v.iter().map(|s| s.as_str()).collect())
    }

    /// True if `name` is anywhere on the (transitive) surface (default ∪
    /// optional ∪ tags) — "the module exports it," independent of any
    /// consumer's `use`.
    pub fn exports(&self, name: &str) -> bool {
        if let Some(all) = &self.all_names {
            return all.contains(name);
        }
        self.own.is_some_and(|e| e.exports(name))
    }

    /// Every name on the surface, materialized into an owned set with the same
    /// membership `exports()` reports. Lets a caller resolving many names
    /// against one producer snapshot the surface once instead of re-walking
    /// re-export edges per name (the diagnostics hot path). Own-only is the
    /// package's `@EXPORT ∪ @EXPORT_OK`; the transitive case returns the
    /// precomputed union.
    pub fn all_names(&self) -> HashSet<String> {
        if let Some(all) = &self.all_names {
            return all.clone();
        }
        self.own
            .map(|e| e.export.iter().chain(&e.export_ok).cloned().collect())
            .unwrap_or_default()
    }
}

/// One import selector parsed from a `use` statement's arg list. The consumer
/// evaluator (`imported_names`) maps each selector against a producer
/// `ExportSurface` to the locally-bound name set.
enum ImportSelector<'a> {
    /// A `:tag` / `-tag` group selector — expands to the tag's members.
    Tag(&'a str),
    /// A `name => { -as => 'local' }` rename — binds `local` to origin `name`.
    Rename { local: &'a str, remote: &'a str },
    /// A bare name — binds it iff it's on the surface (default ∪ optional ∪ tag).
    Name(&'a str),
}

/// Evaluate a consumer's import against a producer's export surface, yielding
/// the locally-bound `(local_name, origin_name)` pairs. The one place Perl
/// import semantics live, so diagnostics and nav can never disagree on the
/// bound set:
///   - bare `use M;` (no selectors, not empty)   → binds `@EXPORT` (defaults).
///   - `use M ();` (explicit empty parens)        → binds nothing.
///   - `use M qw(a b);` / `'a','b'`               → binds those (if on surface).
///   - `use M qw(:tag);` / `:DEFAULT`             → binds the tag's members.
///   - `use M foo => { -as => 'bar' };`           → binds local `bar`→origin `foo`.
///   - mixed specs                                → union.
/// `@EXPORT_OK` is NEVER auto-imported by a bare `use M;` — an opt-in name
/// reached only by a bare use is deliberately left unbound (the GATE-5 hint).
pub fn imported_names(
    import: &Import,
    surface: &ExportSurface<'_>,
) -> std::collections::HashSet<(String, String)> {
    let mut bound = std::collections::HashSet::new();

    // `use M ();` — explicit empty list suppresses even the defaults.
    if import.empty_import {
        return bound;
    }

    // Bare `use M;` — no selectors at all auto-imports the default set.
    //
    // Pure Perl binds `@EXPORT` only here. We also bind `@EXPORT_OK` because the
    // builder cannot distinguish a runtime exporter's *defaults* from a
    // traditional opt-in list: Moose::Exporter / Sub::Exporter / Exporter::Tiny
    // install their default names at `import` time, and the static walker records
    // every such name in `export_ok` (it has no parse-time signal for "runtime
    // default"). Treating `@EXPORT_OK` as unbound on a bare use would flag those
    // as unresolved-function — ~684 FPs across the corpus (Moose::Util::
    // TypeConstraints &c.). The honest failure mode is the explicit `use M ();`
    // above, which binds nothing. Named/`:tag`/`-as` specs below stay precise.
    if import.imported_symbols.is_empty() {
        for name in surface.default_set().iter().chain(surface.optional_set()) {
            bound.insert((name.clone(), name.clone()));
        }
        return bound;
    }

    for sym in &import.imported_symbols {
        let selector = if sym.remote_name.is_some() {
            ImportSelector::Rename { local: &sym.local_name, remote: sym.remote() }
        } else if let Some(tag) = sym
            .local_name
            .strip_prefix(':')
            .or_else(|| sym.local_name.strip_prefix('-'))
        {
            ImportSelector::Tag(tag)
        } else {
            ImportSelector::Name(&sym.local_name)
        };

        match selector {
            ImportSelector::Tag(tag) => {
                if let Some(members) = surface.tag_members(tag) {
                    for m in members {
                        bound.insert((m.to_string(), m.to_string()));
                    }
                }
            }
            ImportSelector::Rename { local, remote } => {
                // The rename is honored as written; the origin's presence on
                // the surface is the producer's concern (an unknown origin
                // simply won't resolve cross-file, same as a bare name).
                bound.insert((local.to_string(), remote.to_string()));
            }
            ImportSelector::Name(name) => {
                // An explicitly-named import binds it; the surface check is the
                // producer's verdict, applied by the caller when known.
                bound.insert((name.to_string(), name.to_string()));
            }
        }
    }
    bound
}

