//! Rhai-scripted plugin support.
//!
//! A `RhaiPlugin` wraps a compiled Rhai script that exposes top-level
//! functions: `id()`, `triggers()`, `patterns()` + `on_match(pattern, m)`,
//! and/or `on_use(ctx)`. Each callback returns an array of emission object-maps
//! that convert back into strongly-typed `EmitAction` values.
//!
//! All context and emission data crosses the script boundary as Rhai `Dynamic`
//! — we never hand out mutable references to the builder. This keeps scripts
//! pure and lets us test them as functions from input to action list.

use std::sync::Arc;

use rhai::{
    serde::{from_dynamic, to_dynamic},
    Array, Dynamic, Engine, AST,
};

use crate::file_analysis::{HashKeyOwner, InferredType, Span};
use tree_sitter::Point;

use super::{
    AttributeMacro, CompletionQueryContext, ConstraintParam, DispatchVerb, EmitAction,
    FrameworkPlugin, ParamType, PluginCompletionAnswer, PluginSigHelpAnswer, SigHelpQueryContext,
    Trigger, TypeOverride, UseContext,
};

/// An engine built with our helpers and type registrations. Engines are
/// cheap to reuse; scripts share one instance across all callbacks.
pub fn make_engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_max_expr_depths(64, 64);
    // Kill switch: a runaway Rhai script (infinite loop, stuck on a bad
    // input) would otherwise hang the LSP build thread indefinitely.
    // 1M operations is comfortably more than any sensible plugin hook
    // needs (emit hooks top out in the hundreds; query hooks lower) and
    // low enough to bail in well under a second on modern hardware.
    // Override via `PERL_LSP_RHAI_MAX_OPS` for debugging heavy plugins.
    let max_ops: u64 = std::env::var("PERL_LSP_RHAI_MAX_OPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    engine.set_max_operations(max_ops);

    // Shorthand constructors for `HashKeyOwner` — Rhai scripts avoid writing
    // enum discriminants by using these helper functions.
    engine.register_fn("owner_sub", |package: String, name: String| {
        let pkg = if package.is_empty() { None } else { Some(package) };
        let owner = HashKeyOwner::Sub { package: pkg, name };
        to_dynamic(owner).unwrap_or(Dynamic::UNIT)
    });
    engine.register_fn("owner_sub_unscoped", |name: String| {
        let owner = HashKeyOwner::Sub { package: None, name };
        to_dynamic(owner).unwrap_or(Dynamic::UNIT)
    });
    engine.register_fn("owner_class", |class: String| {
        let owner = HashKeyOwner::Class(class);
        to_dynamic(owner).unwrap_or(Dynamic::UNIT)
    });
    // A plugin-owned column (DBIC / Class::Accessor) — distinct from `Class` so
    // a `$row->{col}` deref never reaches it (see `HashKeyOwner::Bridged`).
    engine.register_fn("owner_bridged", |class: String| {
        let owner = HashKeyOwner::Bridged { class };
        to_dynamic(owner).unwrap_or(Dynamic::UNIT)
    });

    // `InferredType` convenience constructors — scripts can say
    // `type_class("Foo")` instead of nesting enum variants manually.
    engine.register_fn("type_string", || to_dynamic(InferredType::String).unwrap());
    engine.register_fn("type_numeric", || to_dynamic(InferredType::Numeric).unwrap());
    engine.register_fn("type_hashref", || to_dynamic(InferredType::HashRef).unwrap());
    engine.register_fn("type_arrayref", || to_dynamic(InferredType::ArrayRef).unwrap());
    engine.register_fn("type_coderef", || {
        to_dynamic(InferredType::CodeRef { return_edge: None }).unwrap()
    });
    engine.register_fn("type_regexp", || to_dynamic(InferredType::Regexp).unwrap());
    engine.register_fn("type_undef", || to_dynamic(InferredType::Undef).unwrap());
    engine.register_fn("type_class", |class: String| {
        to_dynamic(InferredType::ClassName(class)).unwrap_or(Dynamic::UNIT)
    });
    // Lift a type to `Optional<T>` (idempotent — an already-Optional value
    // passes through). Unit-in → Unit-out so a fold can pipe a declined
    // inner straight through without re-checking.
    engine.register_fn("type_optional", |inner: Dynamic| -> Dynamic {
        let Ok(t) = from_dynamic::<InferredType>(&inner) else {
            return Dynamic::UNIT;
        };
        let lifted = match t {
            InferredType::Optional(_) => t,
            other => InferredType::Optional(Box::new(other)),
        };
        to_dynamic(lifted).unwrap_or(Dynamic::UNIT)
    });

    // Project a constraint type to what it constrains — the rhai mirror of
    // `InferredType::constrained_inner`. A nested constructor param
    // (`Maybe[InstanceOf['Foo']]`) arrives as a `ConstraintParam.ty` typed
    // `TypeConstraintOf(inner)`; a passthrough fold (`Maybe[T]` → T's inner)
    // asks the value for its inner without destructuring the serde shape.
    // Unit for a non-constraint `ty` (or `()`), so the fold declines cleanly.
    engine.register_fn("constrained_inner", |ty: Dynamic| -> Dynamic {
        let Ok(t) = from_dynamic::<InferredType>(&ty) else { return Dynamic::UNIT; };
        match t.constrained_inner() {
            Some(inner) => to_dynamic(inner.clone()).unwrap_or(Dynamic::UNIT),
            None => Dynamic::UNIT,
        }
    });

    // Mark a param-list's first element as the implicit invocant.
    // Framework callbacks typically receive the receiver as their
    // first positional (`$c` for Mojolicious helpers, `$self_in`
    // for Mojo::EventEmitter handlers, etc.); the plugin knows
    // this, the core does not. Running the array through this
    // helper tells sig help / hover / outline to drop param 0 at
    // display time without the core matching on names.
    engine.register_fn("as_invocant_params", |list: Array| -> Array {
        let mut out = list;
        if let Some(first) = out.get_mut(0) {
            if let Ok(mut m) = first.as_map_mut() {
                m.insert("is_invocant".into(), Dynamic::from(true));
            }
        }
        out
    });

    // Subspan helper: plugins frequently want to narrow a parser-given
    // span (e.g. a whole string literal) down to a portion of its
    // content (the method-name half of `"Controller#action"`). This
    // returns a new span on the same row with columns offset from
    // `base`'s start. Plugins pass column *deltas* — 0 for "start of
    // base", `len(content)` for "end" — and we compute the absolute
    // columns.
    engine.register_fn(
        "subspan_cols",
        |base: Dynamic, col_start_delta: i64, col_end_delta: i64| -> Dynamic {
            let Ok(span) = from_dynamic::<Span>(&base) else { return Dynamic::UNIT; };
            let start = Point::new(
                span.start.row,
                (span.start.column as i64 + col_start_delta).max(0) as usize,
            );
            let end = Point::new(
                span.start.row,
                (span.start.column as i64 + col_end_delta).max(0) as usize,
            );
            to_dynamic(Span { start, end }).unwrap_or(Dynamic::UNIT)
        },
    );

    // `classified_pairs(args, start)` — THE shared keyval-pairing primitive
    // over a flat `ctx.args`. Pairs `args[start]`,`args[start+1]` as
    // `(key, value)`, stepping by two — separator-agnostic (`k => v` and
    // `'k', v` walk identically), `start` skips a leading positional head
    // (a route target / attr name). The key is the even arg's `value_shape`
    // Str; the value is the odd arg's full classified `value_shape`, so a
    // caller branches on `p.value.Str` / `p.value.HashPairs` / etc. Lives in
    // the host (not copied per plugin) so `to`, Moo `has`, and any future
    // keyval verb share one implementation. Args reach here already peeled
    // flat (`Builder::flat_call_args`), so the nested `has 'x' => (...)`
    // form pairs the same as the flat `has 'x', k => v` one.
    fn arg_map_field(d: &Dynamic, key: &str) -> Dynamic {
        d.read_lock::<rhai::Map>()
            .and_then(|m| m.get(key).cloned())
            .unwrap_or(Dynamic::UNIT)
    }
    engine.register_fn("classified_pairs", |args: Array, start: i64| -> Array {
        let mut out = Array::new();
        if start < 0 {
            return out;
        }
        let mut i = start as usize;
        while i + 1 < args.len() {
            let key = arg_map_field(&arg_map_field(&args[i], "value_shape"), "Str");
            if let Ok(key) = key.into_string() {
                let val_arg = &args[i + 1];
                let mut m = rhai::Map::new();
                m.insert("key".into(), key.into());
                m.insert("key_span".into(), arg_map_field(&args[i], "span"));
                m.insert("value".into(), arg_map_field(val_arg, "value_shape"));
                m.insert(
                    "value_content_span".into(),
                    arg_map_field(val_arg, "content_span"),
                );
                m.insert("value_span".into(), arg_map_field(val_arg, "span"));
                out.push(Dynamic::from_map(m));
            }
            i += 2;
        }
        out
    });

    engine
}

pub struct RhaiPlugin {
    id: String,
    triggers: Vec<Trigger>,
    overrides: Vec<TypeOverride>,
    dispatch_verbs: Vec<DispatchVerb>,
    #[cfg_attr(not(feature = "cpp"), allow(dead_code))]
    attribute_macros: Vec<AttributeMacro>,
    load_verbs: Vec<crate::plugin::LoadVerb>,
    param_types: Vec<ParamType>,
    type_constraint_names: Vec<String>,
    app_surface_consumers: Vec<String>,
    role_makers: Vec<String>,
    column_keyed_verbs: Vec<String>,
    meta_methods: Vec<String>,
    fluent_verbs: Vec<String>,
    topic_route_dsl: Option<crate::plugin::TopicRouteDsl>,
    patterns: Vec<crate::plugin::PatternSpec>,
    engine: Arc<Engine>,
    ast: Arc<AST>,
    has_on_match: bool,
    has_type_constraint_inner: bool,
    has_on_use: bool,
    has_on_signature_help: bool,
    has_on_completion: bool,
}

/// Read an optional list-shaped manifest hook: a missing fn is empty, a
/// failed call or bad element logs and skips — the shared fail-safe
/// contract for every manifest family (a broken plugin must not break
/// the build).
fn read_manifest_list<T: serde::de::DeserializeOwned>(
    engine: &Engine,
    ast: &AST,
    signatures: &[String],
    id: &str,
    fn_name: &str,
) -> Vec<T> {
    let mut out = Vec::new();
    if !signatures.iter().any(|n| n == fn_name) {
        return out;
    }
    match engine.call_fn::<Array>(&mut rhai::Scope::new(), ast, fn_name, ()) {
        Ok(arr) => {
            for d in arr {
                match from_dynamic::<T>(&d) {
                    Ok(v) => out.push(v),
                    Err(e) => log::error!("plugin `{}` {}() bad entry: {}", id, fn_name, e),
                }
            }
        }
        Err(e) => log::error!("plugin `{}` {}() failed: {}", id, fn_name, e),
    }
    out
}

impl RhaiPlugin {
    /// Compile a script from source text and interrogate its metadata
    /// (`id()`, `triggers()`) up-front so dispatch is cheap.
    pub fn from_source(
        source: &str,
        engine: Arc<Engine>,
    ) -> Result<Self, String> {
        let ast = engine
            .compile(source)
            .map_err(|e| format!("rhai compile: {}", e))?;

        let id: String = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "id", ())
            .map_err(|e| format!("rhai `id()`: {}", e))?;

        let trig_dyn: Array = engine
            .call_fn(&mut rhai::Scope::new(), &ast, "triggers", ())
            .map_err(|e| format!("rhai `triggers()`: {}", e))?;

        let mut triggers = Vec::with_capacity(trig_dyn.len());
        for d in trig_dyn {
            let t: Trigger = from_dynamic(&d)
                .map_err(|e| format!("bad trigger from `{}`: {}", id, e))?;
            triggers.push(t);
        }

        let signatures: Vec<String> = ast
            .iter_functions()
            .map(|f| f.name.to_string())
            .collect();

        // List-shaped manifest hooks all share one optional, fail-safe
        // contract (see `read_manifest_list`): missing fn == empty, bad
        // shapes log and skip.
        let overrides: Vec<TypeOverride> =
            read_manifest_list(&engine, &ast, &signatures, &id, "overrides");
        let dispatch_verbs: Vec<DispatchVerb> =
            read_manifest_list(&engine, &ast, &signatures, &id, "dispatch_verbs");
        let attribute_macros: Vec<AttributeMacro> =
            read_manifest_list(&engine, &ast, &signatures, &id, "attribute_macros");
        let load_verbs: Vec<crate::plugin::LoadVerb> =
            read_manifest_list(&engine, &ast, &signatures, &id, "load_verbs");
        let param_types: Vec<ParamType> =
            read_manifest_list(&engine, &ast, &signatures, &id, "param_types");
        let type_constraint_names: Vec<String> =
            read_manifest_list(&engine, &ast, &signatures, &id, "type_constraint_names");
        let app_surface_consumers: Vec<String> =
            read_manifest_list(&engine, &ast, &signatures, &id, "app_surface_consumers");
        let role_makers: Vec<String> =
            read_manifest_list(&engine, &ast, &signatures, &id, "role_makers");
        let column_keyed_verbs: Vec<String> =
            read_manifest_list(&engine, &ast, &signatures, &id, "column_keyed_verbs");
        let meta_methods: Vec<String> =
            read_manifest_list(&engine, &ast, &signatures, &id, "meta_methods");
        let fluent_verbs: Vec<String> =
            read_manifest_list(&engine, &ast, &signatures, &id, "fluent_verbs");
        let patterns: Vec<crate::plugin::PatternSpec> =
            read_manifest_list(&engine, &ast, &signatures, &id, "patterns");

        // `topic_route_dsl()` — optional manifest map; bad shapes log
        // and disable rather than fail the plugin.
        let mut topic_route_dsl: Option<crate::plugin::TopicRouteDsl> = None;
        if signatures.iter().any(|n| n == "topic_route_dsl") {
            match engine.call_fn::<Dynamic>(&mut rhai::Scope::new(), &ast, "topic_route_dsl", ()) {
                Ok(d) => match from_dynamic::<crate::plugin::TopicRouteDsl>(&d) {
                    Ok(t) => topic_route_dsl = Some(t),
                    Err(e) => log::error!("plugin `{}` topic_route_dsl() bad shape: {}", id, e),
                },
                Err(e) => log::error!("plugin `{}` topic_route_dsl() failed: {}", id, e),
            }
        }

        Ok(Self {
            has_on_match: signatures.iter().any(|n| n == "on_match"),
            has_type_constraint_inner: signatures.iter().any(|n| n == "type_constraint_inner"),
            has_on_use: signatures.iter().any(|n| n == "on_use"),
            has_on_signature_help: signatures.iter().any(|n| n == "on_signature_help"),
            has_on_completion: signatures.iter().any(|n| n == "on_completion"),
            id,
            triggers,
            overrides,
            dispatch_verbs,
            attribute_macros,
            load_verbs,
            param_types,
            type_constraint_names,
            app_surface_consumers,
            role_makers,
            column_keyed_verbs,
            meta_methods,
            fluent_verbs,
            topic_route_dsl,
            patterns,
            engine,
            ast: Arc::new(ast),
        })
    }

    /// Call a Rhai query hook that returns a single map (sig help)
    /// or nil. Returns `None` if the script's fn returned unit or
    /// the call failed — plugins stay silent unless they have
    /// something to contribute.
    fn call_opt_map<T: serde::de::DeserializeOwned>(&self, fn_name: &str, arg: Dynamic) -> Option<T> {
        let out: Result<Dynamic, _> =
            self.engine.call_fn(&mut rhai::Scope::new(), &self.ast, fn_name, (arg,));
        let v = match out {
            Ok(v) => v,
            Err(e) => {
                // A Rhai call failure is a plugin bug (bad script, kill
                // switch triggered, panic inside the VM) — log at
                // ERROR, not warn. `warn!` gets filtered by default
                // log levels and the user sees missing features with
                // no hint.
                log::error!("plugin `{}`::{} failed: {}", self.id, fn_name, e);
                return None;
            }
        };
        if v.is_unit() { return None; }
        match from_dynamic::<T>(&v) {
            Ok(parsed) => Some(parsed),
            Err(e) => {
                log::error!("plugin `{}`::{} bad return: {}", self.id, fn_name, e);
                None
            }
        }
    }

    fn dispatch(&self, fn_name: &str, arg: Dynamic) -> Vec<EmitAction> {
        self.dispatch_args(fn_name, (arg,))
    }

    /// The N-ary sibling of `dispatch` — `on_match` takes
    /// `(pattern_name, ctx)`. Same fail-safe emission decoding.
    fn dispatch_args(&self, fn_name: &str, args: impl rhai::FuncArgs) -> Vec<EmitAction> {
        let out: Result<Array, _> =
            self.engine.call_fn(&mut rhai::Scope::new(), &self.ast, fn_name, args);
        let arr = match out {
            Ok(a) => a,
            Err(e) => {
                log::error!("plugin `{}`::{} failed: {}", self.id, fn_name, e);
                return Vec::new();
            }
        };
        arr.into_iter()
            .filter_map(|d| {
                from_dynamic::<EmitAction>(&d)
                    .map_err(|e| {
                        log::error!(
                            "plugin `{}`::{} bad emission: {}",
                            self.id,
                            fn_name,
                            e
                        )
                    })
                    .ok()
            })
            .collect()
    }
}

impl FrameworkPlugin for RhaiPlugin {
    fn id(&self) -> &str {
        &self.id
    }

    fn triggers(&self) -> &[Trigger] {
        &self.triggers
    }

    fn overrides(&self) -> &[TypeOverride] {
        &self.overrides
    }

    fn dispatch_verbs(&self) -> &[DispatchVerb] {
        &self.dispatch_verbs
    }

    fn attribute_macros(&self) -> &[AttributeMacro] {
        &self.attribute_macros
    }

    fn load_verbs(&self) -> &[crate::plugin::LoadVerb] {
        &self.load_verbs
    }

    fn param_types(&self) -> &[ParamType] {
        &self.param_types
    }

    fn type_constraint_names(&self) -> &[String] {
        &self.type_constraint_names
    }

    fn app_surface_consumers(&self) -> &[String] {
        &self.app_surface_consumers
    }

    fn role_makers(&self) -> &[String] {
        &self.role_makers
    }

    fn column_keyed_verbs(&self) -> &[String] {
        &self.column_keyed_verbs
    }

    fn meta_methods(&self) -> &[String] {
        &self.meta_methods
    }

    fn fluent_verbs(&self) -> &[String] {
        &self.fluent_verbs
    }

    fn topic_route_dsl(&self) -> Option<crate::plugin::TopicRouteDsl> {
        self.topic_route_dsl.clone()
    }

    fn type_constraint_inner(
        &self,
        name: &str,
        params: &[ConstraintParam],
    ) -> Option<InferredType> {
        if !self.has_type_constraint_inner {
            return None;
        }
        let params_dyn = to_dynamic(params).ok()?;
        let out: Result<Dynamic, _> = self.engine.call_fn(
            &mut rhai::Scope::new(),
            &self.ast,
            "type_constraint_inner",
            (name.to_string(), params_dyn),
        );
        let v = match out {
            Ok(v) => v,
            Err(e) => {
                log::error!("plugin `{}`::type_constraint_inner failed: {}", self.id, e);
                return None;
            }
        };
        if v.is_unit() {
            return None;
        }
        match from_dynamic::<InferredType>(&v) {
            Ok(t) => Some(t),
            Err(e) => {
                log::error!(
                    "plugin `{}`::type_constraint_inner bad return: {}",
                    self.id,
                    e
                );
                None
            }
        }
    }

    fn patterns(&self) -> &[crate::plugin::PatternSpec] {
        &self.patterns
    }

    fn on_match(&self, pattern: &str, m: &crate::plugin::MatchContext) -> Vec<EmitAction> {
        if !self.has_on_match {
            return Vec::new();
        }
        match to_dynamic(m) {
            Ok(d) => self.dispatch_args("on_match", (pattern.to_string(), d)),
            Err(e) => {
                log::warn!("plugin `{}`: match ctx serialize: {}", self.id, e);
                Vec::new()
            }
        }
    }

    fn on_signature_help(&self, ctx: &SigHelpQueryContext) -> Option<PluginSigHelpAnswer> {
        if !self.has_on_signature_help { return None; }
        let d = to_dynamic(ctx).ok()?;
        self.call_opt_map("on_signature_help", d)
    }

    fn on_completion(&self, ctx: &CompletionQueryContext) -> Option<PluginCompletionAnswer> {
        if !self.has_on_completion { return None; }
        let d = to_dynamic(ctx).ok()?;
        self.call_opt_map("on_completion", d)
    }

    fn on_use(&self, ctx: &UseContext) -> Vec<EmitAction> {
        if !self.has_on_use {
            return Vec::new();
        }
        match to_dynamic(ctx) {
            Ok(d) => self.dispatch("on_use", d),
            Err(e) => {
                log::warn!("plugin `{}`: use ctx serialize: {}", self.id, e);
                Vec::new()
            }
        }
    }
}

// ---- Bundled plugins ----

/// Bundled Rhai script sources shipped with the binary. Third-party plugins
/// can be loaded from disk via `load_plugin_dir`.
const BUNDLED: &[(&str, &str)] = &[
    ("mojo-events", include_str!("../../frameworks/mojo-events.rhai")),
    ("mojo-helpers", include_str!("../../frameworks/mojo-helpers.rhai")),
    ("mojo-routes", include_str!("../../frameworks/mojo-routes.rhai")),
    ("mojo-lite", include_str!("../../frameworks/mojo-lite.rhai")),
    ("minion", include_str!("../../frameworks/minion.rhai")),
    ("data-printer", include_str!("../../frameworks/data-printer.rhai")),
    ("dbic-resultddl", include_str!("../../frameworks/dbic-resultddl.rhai")),
    ("dbic", include_str!("../../frameworks/dbic.rhai")),
    ("type-tiny", include_str!("../../frameworks/type-tiny.rhai")),
    ("dancer", include_str!("../../frameworks/dancer.rhai")),
    ("moo", include_str!("../../frameworks/moo.rhai")),
    ("catalyst", include_str!("../../frameworks/catalyst.rhai")),
    ("cpp-attributes", include_str!("../../frameworks/cpp-attributes.rhai")),
    ("monkey-patch", include_str!("../../frameworks/monkey-patch.rhai")),
];

pub fn load_bundled(engine: Arc<Engine>) -> Vec<Box<dyn FrameworkPlugin>> {
    let mut out: Vec<Box<dyn FrameworkPlugin>> = Vec::new();
    for (id, src) in BUNDLED {
        match RhaiPlugin::from_source(src, engine.clone()) {
            Ok(p) => {
                log::info!("loaded bundled plugin `{}`", id);
                out.push(Box::new(p));
            }
            Err(e) => {
                log::warn!("bundled plugin `{}` failed to load: {}", id, e);
            }
        }
    }
    out
}

/// The workspace root, pinned from the SAME value the LSP/CLI hands
/// `ModuleIndex::set_workspace_root` — so repo-local plugin discovery and
/// the per-project SQLite cache agree on what "the project" is. If they
/// were derived independently (e.g. a cwd ancestor-walk), a plugin set
/// loaded against one root could invalidate a cache keyed on another.
static WORKSPACE_ROOT: std::sync::RwLock<Option<std::path::PathBuf>> =
    std::sync::RwLock::new(None);

/// Record the workspace root for repo-local plugin discovery. Accepts the
/// same `file://…` URI (or bare path) passed to the module index; `None`
/// clears it (no project root → no repo-local plugins). Call this before
/// the first `build()` so the process-wide registry sees it.
pub fn set_workspace_root(root: Option<&str>) {
    let path = root.map(|r| {
        std::path::PathBuf::from(r.strip_prefix("file://").unwrap_or(r))
    });
    if let Ok(mut guard) = WORKSPACE_ROOT.write() {
        *guard = path;
    }
}

/// Directories to load user `.rhai` plugins from, in priority order.
///
/// 1. `$PERL_LSP_PLUGIN_DIR` — explicit global opt-in (a power user's
///    personal plugin collection), honored whenever it points at a dir.
/// 2. `<workspace-root>/.perl-lsp/` — a project shipping plugins for its
///    own kits, with zero global config. The root is whatever the client
///    sent at `initialize` (or the CLI's root arg), the exact value the
///    SQLite cache is keyed on — pinned here, never re-derived, so the
///    fingerprint that invalidates the cache and the plugins actually
///    loaded stay in lockstep. With no root set (single-file CLI modes),
///    falls back to `./.perl-lsp` so running inside a project still works.
///
/// Both the loader and the cache fingerprint call this, so "what gets
/// loaded" and "what invalidates the cache" can't drift apart.
pub fn plugin_search_dirs() -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(dir) = std::env::var("PERL_LSP_PLUGIN_DIR") {
        let p = std::path::PathBuf::from(dir);
        if p.is_dir() {
            dirs.push(p);
        }
    }
    let root = WORKSPACE_ROOT
        .read()
        .ok()
        .and_then(|g| g.clone())
        .or_else(|| std::env::current_dir().ok());
    if let Some(root) = root {
        let repo = root.join(".perl-lsp");
        if repo.is_dir() && !dirs.contains(&repo) {
            dirs.push(repo);
        }
    }
    dirs
}

/// Stable hash of the plugin set the next `build()` will load. Used by the
/// SQLite module cache to invalidate stored FileAnalysis blobs when the
/// plugins that produced them have changed — without this, editing a
/// `.rhai` and restarting the LSP serves stale cross-file analyses.
///
/// Inputs:
///   * Every bundled plugin's `(id, source)` pair — catches binary
///     rebuilds whose only change was a `frameworks/*.rhai` edit.
///   * Every `.rhai` file in each `plugin_search_dirs()` entry, with its
///     path — catches user-plugin add / remove / rename / edit across
///     LSP restarts, whether the plugin lives in `$PERL_LSP_PLUGIN_DIR`
///     or a repo-local `.perl-lsp/`.
///
/// Read-only: no compile, no side effects, no log spam. Fails open
/// (returns the bundled-only hash) if a dir can't be read, matching
/// the rest of the loader's silently-tolerant behavior.
pub fn plugin_fingerprint() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();

    for (id, src) in BUNDLED {
        id.hash(&mut hasher);
        src.hash(&mut hasher);
    }

    for path in plugin_search_dirs() {
        // Sort entries by path so the hash is independent of readdir order.
        let mut entries: Vec<std::path::PathBuf> = match std::fs::read_dir(&path) {
            Ok(read) => read
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("rhai"))
                .collect(),
            Err(_) => Vec::new(),
        };
        entries.sort();
        for p in entries {
            // Path is part of the hash so a rename invalidates.
            p.to_string_lossy().hash(&mut hasher);
            if let Ok(src) = std::fs::read_to_string(&p) {
                src.hash(&mut hasher);
            }
        }
    }

    format!("{:016x}", hasher.finish())
}

/// Load all `*.rhai` files from a directory. Used for user-installed plugins.
pub fn load_plugin_dir(
    dir: &std::path::Path,
    engine: Arc<Engine>,
) -> Vec<Box<dyn FrameworkPlugin>> {
    let mut out: Vec<Box<dyn FrameworkPlugin>> = Vec::new();
    let read = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => return out,
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("rhai") {
            continue;
        }
        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("plugin {}: read: {}", path.display(), e);
                continue;
            }
        };
        match RhaiPlugin::from_source(&source, engine.clone()) {
            Ok(p) => {
                log::info!("loaded plugin {} from {}", p.id(), path.display());
                out.push(Box::new(p));
            }
            Err(e) => log::warn!("plugin {}: {}", path.display(), e),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_analysis::Span;
    use tree_sitter::Point;

    fn sp(r1: usize, c1: usize, r2: usize, c2: usize) -> Span {
        Span { start: Point::new(r1, c1), end: Point::new(r2, c2) }
    }

    /// Bare capture for hand-built `MatchContext`s — text + span only,
    /// projections filled by struct-update at the call site.
    fn mcap(text: &str, span: Span) -> crate::plugin::CaptureData {
        crate::plugin::CaptureData {
            text: text.into(),
            span,
            string_value: None,
            string_values: Vec::new(),
            content_span: None,
            inferred_type: None,
            value_shape: None,
            sub_params: Vec::new(),
            callable_return_edge: None,
            list: Vec::new(),
            is_package_receiver: None,
            args: Vec::new(),
            isa: None,
            ref_sub_name: None,
            call_name: None,
            route_defaults: Vec::new(),
        }
    }

    fn one(d: crate::plugin::CaptureData) -> crate::plugin::CaptureValue {
        crate::plugin::CaptureValue::One(Box::new(d))
    }

    #[test]
    fn minimal_plugin_loads_and_dispatches() {
        use crate::plugin::MatchContext;

        let src = r#"
            fn id() { "demo" }
            fn triggers() { [ #{ UsesModule: "Demo" } ] }
            fn patterns() {
                [
                    #{
                        name: "greet_call",
                        query: "(function_call_expression) @call",
                    }
                ]
            }
            fn on_match(pattern, m) {
                if pattern == "greet_call" {
                    return [
                        #{
                            Method: #{
                                name: "hello",
                                span: m.span,
                                selection_span: m.span,
                                params: [],
                                is_method: true,
                                return_type: (),
                                doc: (),
                            }
                        }
                    ];
                }
                []
            }
        "#;

        let engine = Arc::new(make_engine());
        let plugin = RhaiPlugin::from_source(src, engine).expect("compiles");
        assert_eq!(plugin.id(), "demo");
        assert_eq!(plugin.triggers().len(), 1);
        assert!(plugin.patterns().iter().any(|p| p.name == "greet_call"));

        let mut captures = std::collections::HashMap::new();
        captures.insert("call".to_string(), one(mcap("greet('x')", sp(0, 0, 0, 10))));
        let m = MatchContext {
            pattern: "greet_call".into(),
            span: sp(0, 0, 0, 10),
            package: Some("Demo::App".into()),
            package_parents: vec![],
            package_uses: vec!["Demo".into()],
            captures,
        };

        let emissions = plugin.on_match("greet_call", &m);
        assert_eq!(emissions.len(), 1);
        match &emissions[0] {
            EmitAction::Method { name, is_method, .. } => {
                assert_eq!(name, "hello");
                assert!(*is_method);
            }
            other => panic!("unexpected emission: {:?}", other),
        }
    }

    #[test]
    fn plugin_fingerprint_invariants() {
        // Two contracts in one test (env var is process-global, so
        // splitting these into parallel-safe tests would race):
        //
        //   1. Stability — identical inputs must produce identical
        //      hashes, otherwise we'd nuke the SQLite cache on every
        //      LSP startup.
        //   2. Sensitivity — editing a `.rhai` in the user plugin dir
        //      must change the fingerprint, so the cache invalidates
        //      on the next LSP restart and the author can QA their
        //      changes.
        let dir = std::env::temp_dir().join(format!(
            "perl-lsp-fp-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let plugin_path = dir.join("test.rhai");

        let saved = std::env::var("PERL_LSP_PLUGIN_DIR").ok();
        std::env::set_var("PERL_LSP_PLUGIN_DIR", &dir);

        std::fs::write(&plugin_path, r#"fn id() { "v1" } fn triggers() { [] }"#).unwrap();
        let v1a = plugin_fingerprint();
        let v1b = plugin_fingerprint();

        std::fs::write(&plugin_path, r#"fn id() { "v2" } fn triggers() { [] }"#).unwrap();
        let v2 = plugin_fingerprint();

        // Restore env BEFORE asserting so a panic doesn't leak the override.
        match saved {
            Some(v) => std::env::set_var("PERL_LSP_PLUGIN_DIR", v),
            None => std::env::remove_var("PERL_LSP_PLUGIN_DIR"),
        }
        let _ = std::fs::remove_file(&plugin_path);
        let _ = std::fs::remove_dir(&dir);

        assert!(!v1a.is_empty(), "fingerprint should never be empty");
        assert_eq!(v1a, v1b, "fingerprint must be deterministic");
        assert_ne!(v1a, v2, "fingerprint must change when a user plugin's source changes");
    }

    #[test]
    fn bundled_script_compiles() {
        // Diagnostic: if load_bundled drops ANY script due to a compile
        // error, surface the real error instead of the opaque
        // "not found" failure later. Each bundled script is exercised.
        let engine = Arc::new(make_engine());
        for (id, src) in [
            ("mojo-events", include_str!("../../frameworks/mojo-events.rhai")),
            ("mojo-helpers", include_str!("../../frameworks/mojo-helpers.rhai")),
            ("mojo-routes", include_str!("../../frameworks/mojo-routes.rhai")),
            ("mojo-lite", include_str!("../../frameworks/mojo-lite.rhai")),
            ("minion", include_str!("../../frameworks/minion.rhai")),
            ("data-printer", include_str!("../../frameworks/data-printer.rhai")),
            ("dbic-resultddl", include_str!("../../frameworks/dbic-resultddl.rhai")),
            ("dbic", include_str!("../../frameworks/dbic.rhai")),
            ("type-tiny", include_str!("../../frameworks/type-tiny.rhai")),
            ("dancer", include_str!("../../frameworks/dancer.rhai")),
            ("moo", include_str!("../../frameworks/moo.rhai")),
            ("catalyst", include_str!("../../frameworks/catalyst.rhai")),
            ("cpp-attributes", include_str!("../../frameworks/cpp-attributes.rhai")),
        ] {
            RhaiPlugin::from_source(src, engine.clone())
                .unwrap_or_else(|e| panic!("{}.rhai failed to compile: {e}", id));
        }
    }

    #[test]
    fn bundled_mojo_events_loads_and_emits() {
        use crate::plugin::{CaptureData, CaptureValue, MatchContext};

        let engine = Arc::new(make_engine());
        let bundled = load_bundled(engine);
        let plugin = bundled
            .into_iter()
            .find(|p| p.id() == "mojo-events")
            .expect("mojo-events is bundled");

        // mojo-events rides the query-declared capture seam: it must
        // declare a pattern manifest and answer on_match with the
        // dispatch-call + handler + namespace emissions.
        assert!(
            plugin.patterns().iter().any(|p| p.name == "event_call"),
            "mojo-events should declare the event_call pattern"
        );

        let cap = mcap;

        let evt_span = sp(3, 15, 3, 23);
        let cb_span = sp(3, 25, 3, 40);
        let mut captures = std::collections::HashMap::new();
        captures.insert(
            "verb".to_string(),
            CaptureValue::One(Box::new(cap("on", sp(3, 10, 3, 12)))),
        );
        captures.insert(
            "recv".to_string(),
            CaptureValue::One(Box::new(CaptureData {
                inferred_type: Some(InferredType::ClassName("My::Emitter".into())),
                ..cap("$self", sp(3, 4, 3, 9))
            })),
        );
        captures.insert(
            "event".to_string(),
            CaptureValue::One(Box::new(CaptureData {
                string_value: Some("connect".into()),
                ..cap("'connect'", evt_span)
            })),
        );
        captures.insert(
            "callback".to_string(),
            CaptureValue::One(Box::new(cap("sub { ... }", cb_span))),
        );
        let m = MatchContext {
            pattern: "event_call".into(),
            span: sp(3, 4, 3, 45),
            package: Some("My::Emitter".into()),
            package_parents: vec!["Mojo::EventEmitter".into()],
            package_uses: vec![],
            captures,
        };

        let emissions = plugin.on_match("event_call", &m);
        // DispatchCall (ref) + Handler (def) + PluginNamespace (bridge).
        assert_eq!(emissions.len(), 3,
            "dispatch call + handler + namespace; got: {:?}", emissions);

        let has_dispatch = emissions.iter().any(|e| {
            matches!(e, EmitAction::DispatchCall { name, dispatcher, .. }
                if name == "connect" && dispatcher == "on")
        });
        assert!(has_dispatch, "missing DispatchCall for 'connect' via ->on");

        let has_handler = emissions.iter().any(|e| {
            matches!(e, EmitAction::Handler { name, .. } if name == "connect")
        });
        assert!(has_handler, "missing Handler symbol for 'connect'");

        let has_namespace = emissions.iter().any(|e| {
            matches!(e, EmitAction::PluginNamespace { id, kind, entity_names, .. }
                if id == "mojo-events:My::Emitter"
                    && kind == "events"
                    && entity_names.iter().any(|n| n == "connect"))
        });
        assert!(has_namespace,
            "missing PluginNamespace for My::Emitter events; got: {:?}", emissions);
    }

    #[test]
    fn bundled_dbic_resultddl_synthesizes_accessors() {
        use crate::plugin::MatchContext;

        let engine = Arc::new(make_engine());
        let bundled = load_bundled(engine);
        let plugin = bundled
            .into_iter()
            .find(|p| p.id() == "dbic-resultddl")
            .expect("dbic-resultddl is bundled");
        assert!(
            plugin.patterns().iter().any(|p| p.name == "ddl_decl"),
            "dbic-resultddl should declare the ddl_decl pattern"
        );

        // `col text => text;` and `has_many searches => {...};` each install
        // an accessor named by the (autoquoted) first arg.
        let cases = [("col", "text"), ("has_many", "searches"), ("belongs_to", "product")];
        for (func, accessor) in cases {
            let name_span = sp(1, 4, 1, 8);
            let mut captures = std::collections::HashMap::new();
            captures.insert("verb".to_string(), one(mcap(func, sp(1, 0, 1, 3))));
            captures.insert(
                "name".to_string(),
                one(crate::plugin::CaptureData {
                    string_value: Some(accessor.into()),
                    ..mcap(accessor, name_span)
                }),
            );
            let m = MatchContext {
                pattern: "ddl_decl".into(),
                span: sp(1, 0, 1, 20),
                package: Some("My::Schema::Result::Thing".into()),
                package_parents: vec![],
                package_uses: vec!["DBIx::Class::ResultDDL".into()],
                captures,
            };

            let emissions = plugin.on_match("ddl_decl", &m);
            let has_method = emissions.iter().any(|e| {
                matches!(e, EmitAction::Method { name, is_method, .. }
                    if name == accessor && *is_method)
            });
            assert!(has_method,
                "{func} '{accessor}' should synthesize an accessor Method; got: {emissions:?}");
        }
    }

    #[test]
    fn dbic_resultddl_skips_dynamic_name() {
        use crate::plugin::MatchContext;

        let engine = Arc::new(make_engine());
        let bundled = load_bundled(engine);
        let plugin = bundled.into_iter().find(|p| p.id() == "dbic-resultddl").unwrap();

        // Dynamic column name (`col $field => ...`) — no fold, nothing to
        // synthesize. (The non-DSL-verb case — `table 'embeddings';` — is
        // filtered by the PATTERN now, pinned by its expects.)
        let mut captures = std::collections::HashMap::new();
        captures.insert("verb".to_string(), one(mcap("col", sp(1, 0, 1, 3))));
        captures.insert("name".to_string(), one(mcap("$field", sp(1, 4, 1, 10))));
        let m = MatchContext {
            pattern: "ddl_decl".into(),
            span: sp(1, 0, 1, 20),
            package: Some("My::Schema::Result::Thing".into()),
            package_parents: vec![],
            package_uses: vec!["DBIx::Class::ResultDDL".into()],
            captures,
        };
        assert!(plugin.on_match("ddl_decl", &m).is_empty(),
            "dynamic col name must be skipped");
    }

    #[test]
    fn mojo_events_skips_dynamic_event_name() {
        use crate::plugin::MatchContext;

        let engine = Arc::new(make_engine());
        let bundled = load_bundled(engine);
        let plugin = bundled
            .into_iter()
            .find(|p| p.id() == "mojo-events")
            .unwrap();

        // Dynamic name — no `str` fold on the event capture, so the
        // plugin must decline (references still see it, rename skips it).
        let mut captures = std::collections::HashMap::new();
        captures.insert("verb".to_string(), one(mcap("on", sp(0, 7, 0, 9))));
        captures.insert(
            "recv".to_string(),
            one(crate::plugin::CaptureData {
                inferred_type: Some(InferredType::ClassName("Foo".into())),
                ..mcap("$self", sp(0, 0, 0, 5))
            }),
        );
        captures.insert("event".to_string(), one(mcap("$name", sp(0, 10, 0, 15))));
        captures.insert("callback".to_string(), one(mcap("sub {}", sp(0, 17, 0, 23))));
        let m = MatchContext {
            pattern: "event_call".into(),
            span: sp(0, 0, 0, 25),
            package: Some("Foo".into()),
            package_parents: vec!["Mojo::EventEmitter".into()],
            package_uses: vec![],
            captures,
        };

        let emissions = plugin.on_match("event_call", &m);
        assert!(emissions.is_empty(), "dynamic name must not emit");
    }

    #[test]
    fn rhai_overrides_function_is_read_at_compile_time() {
        // A plugin that defines a top-level `overrides()` function:
        // the host reads it once at load and exposes the list via
        // FrameworkPlugin::overrides — no runtime call cost. This
        // pins the static-manifest contract; if a future refactor
        // moves overrides() to a per-build hook, this test breaks
        // and forces a rethink.
        let src = r#"
            fn id() { "demo-overrides" }
            fn triggers() { [] }
            fn overrides() {
                [
                    #{
                        target: #{ Method: #{ class: "Foo", name: "bar" } },
                        return_type: #{ ClassName: "Foo" },
                        reason: "test",
                    }
                ]
            }
        "#;
        let engine = Arc::new(make_engine());
        let plugin = RhaiPlugin::from_source(src, engine).expect("compiles");
        let ovs = plugin.overrides();
        assert_eq!(ovs.len(), 1);
        match &ovs[0].target {
            crate::plugin::OverrideTarget::Method { class, name } => {
                assert_eq!(class, "Foo");
                assert_eq!(name, "bar");
            }
            other => panic!("expected Method target, got {:?}", other),
        }
        assert_eq!(
            ovs[0].return_type,
            InferredType::ClassName("Foo".into())
        );
        assert_eq!(ovs[0].reason, "test");
    }

    #[test]
    fn rhai_attribute_macros_read_at_compile_time_and_unioned() {
        // A plugin declaring `attribute_macros()` exposes the manifest via
        // FrameworkPlugin::attribute_macros (read once at load, like
        // overrides/dispatch_verbs), and a registry collapses the union into
        // the name→signal map the pack analyze path looks tokens up in.
        let src = r#"
            fn id() { "demo-attrs" }
            fn triggers() { [] }
            fn attribute_macros() {
                [
                    #{ name: "MY_EXPORT", signal: "exported" },
                    #{ name: "MY_DEPRECATED", signal: "deprecated" },
                ]
            }
        "#;
        let engine = Arc::new(make_engine());
        let plugin = RhaiPlugin::from_source(src, engine).expect("compiles");
        let macros = plugin.attribute_macros();
        assert_eq!(macros.len(), 2);
        assert_eq!(macros[0].name, "MY_EXPORT");
        assert_eq!(macros[0].signal, "exported");

        let mut reg = crate::plugin::PluginRegistry::new();
        reg.register(Box::new(plugin));
        let signals = reg.attribute_macro_signals();
        assert_eq!(signals.get("MY_EXPORT").map(String::as_str), Some("exported"));
        assert_eq!(signals.get("MY_DEPRECATED").map(String::as_str), Some("deprecated"));
        assert_eq!(signals.get("UNKNOWN"), None);
    }

    #[test]
    fn bundled_cpp_attributes_declares_qt_exports() {
        // The bundled cpp-attributes plugin is the C++ vocabulary: Qt export
        // macros signal "exported", deprecation macros "deprecated".
        let engine = Arc::new(make_engine());
        let bundled = load_bundled(engine);
        let plugin = bundled
            .iter()
            .find(|p| p.id() == "cpp-attributes")
            .expect("cpp-attributes is bundled");
        let macros = plugin.attribute_macros();
        let signal = |name: &str| macros.iter().find(|m| m.name == name).map(|m| m.signal.as_str());
        assert_eq!(signal("Q_CORE_EXPORT"), Some("exported"));
        assert_eq!(signal("Q_DECL_IMPORT"), Some("exported"));
        assert_eq!(signal("Q_DEPRECATED"), Some("deprecated"));
    }

    #[test]
    fn rhai_overrides_missing_function_yields_empty() {
        // Plugins without an `overrides()` function must still load.
        // The default registry uses this default for every plugin
        // that doesn't ship overrides — silent absence, not error.
        let src = r#"
            fn id() { "no-overrides" }
            fn triggers() { [] }
        "#;
        let engine = Arc::new(make_engine());
        let plugin = RhaiPlugin::from_source(src, engine).expect("compiles");
        assert!(plugin.overrides().is_empty());
    }

    #[test]
    fn catalyst_plugin_loads_and_has_overrides() {
        let engine = Arc::new(make_engine());
        let bundled = load_bundled(engine);
        let plugin = bundled
            .into_iter()
            .find(|p| p.id() == "catalyst")
            .expect("catalyst plugin is bundled");

        // Static overrides manifest must declare at least req/res/stash.
        let ovs = plugin.overrides();
        assert!(!ovs.is_empty(), "catalyst must ship return-type overrides");

        let has_req = ovs.iter().any(|o| {
            matches!(&o.target, crate::plugin::OverrideTarget::Method { class, name }
                if class == "Catalyst" && name == "req")
                && o.return_type == InferredType::ClassName("Catalyst::Request".into())
        });
        assert!(has_req, "missing req → Catalyst::Request override");

        let has_res = ovs.iter().any(|o| {
            matches!(&o.target, crate::plugin::OverrideTarget::Method { class, name }
                if class == "Catalyst" && name == "res")
                && o.return_type == InferredType::ClassName("Catalyst::Response".into())
        });
        assert!(has_res, "missing res → Catalyst::Response override");

        let has_stash = ovs.iter().any(|o| {
            matches!(&o.target, crate::plugin::OverrideTarget::Method { class, name }
                if class == "Catalyst" && name == "stash")
                && o.return_type == InferredType::HashRef
        });
        assert!(has_stash, "missing stash → HashRef override");

        let has_log = ovs.iter().any(|o| {
            matches!(&o.target, crate::plugin::OverrideTarget::Method { class, name }
                if class == "Catalyst" && name == "log")
                && o.return_type == InferredType::ClassName("Catalyst::Log".into())
        });
        assert!(has_log, "missing log → Catalyst::Log override");
    }

    /// Hand-built `context_call` MatchContext for the catalyst plugin:
    /// verb + typed receiver + a folded target string.
    fn catalyst_match(
        verb: &str,
        receiver_class: &str,
        target: &str,
        target_span: Span,
    ) -> crate::plugin::MatchContext {
        let mut captures = std::collections::HashMap::new();
        captures.insert("verb".to_string(), one(mcap(verb, sp(0, 4, 0, 9))));
        captures.insert(
            "recv".to_string(),
            one(crate::plugin::CaptureData {
                inferred_type: Some(InferredType::ClassName(receiver_class.into())),
                ..mcap("$c", sp(0, 0, 0, 2))
            }),
        );
        captures.insert(
            "target".to_string(),
            one(crate::plugin::CaptureData {
                string_value: Some(target.into()),
                content_span: Some(target_span),
                ..mcap(&format!("'{target}'"), target_span)
            }),
        );
        crate::plugin::MatchContext {
            pattern: "context_call".into(),
            span: sp(0, 0, 0, 30),
            package: Some("MyApp::Controller::Root".into()),
            package_parents: vec!["Catalyst::Controller".into()],
            package_uses: vec![],
            captures,
        }
    }

    #[test]
    fn catalyst_model_call_emits_method_call_ref() {
        let engine = Arc::new(make_engine());
        let bundled = load_bundled(engine);
        let plugin = bundled
            .into_iter()
            .find(|p| p.id() == "catalyst")
            .expect("catalyst is bundled");

        let m = catalyst_match("model", "Catalyst", "Foo", sp(5, 20, 5, 23));
        let emissions = plugin.on_match("context_call", &m);
        // A MethodCallRef pointing at Foo::new (the component class).
        let has_ref = emissions.iter().any(|e| {
            matches!(e, EmitAction::MethodCallRef { method_name, invocant, .. }
                if method_name == "new" && invocant == "Foo")
        });
        assert!(has_ref,
            "model('Foo') must emit MethodCallRef into class Foo; got: {:?}", emissions);
    }

    #[test]
    fn catalyst_forward_emits_dispatch_call() {
        let engine = Arc::new(make_engine());
        let bundled = load_bundled(engine);
        let plugin = bundled
            .into_iter()
            .find(|p| p.id() == "catalyst")
            .expect("catalyst is bundled");

        let m = catalyst_match("forward", "Catalyst", "/Root/index", sp(7, 20, 7, 38));
        let emissions = plugin.on_match("context_call", &m);
        let has_dispatch = emissions.iter().any(|e| {
            matches!(e, EmitAction::DispatchCall { name, dispatcher, .. }
                if name == "/Root/index" && dispatcher == "forward")
        });
        assert!(has_dispatch,
            "forward('/Root/index') must emit DispatchCall; got: {:?}", emissions);
    }

    #[test]
    fn catalyst_skips_non_catalyst_receiver() {
        let engine = Arc::new(make_engine());
        let bundled = load_bundled(engine);
        let plugin = bundled
            .into_iter()
            .find(|p| p.id() == "catalyst")
            .expect("catalyst is bundled");

        // `$schema->model('Foo')` — DBIx::Class::Schema, NOT Catalyst.
        // The plugin must not emit for non-Catalyst receivers.
        let m = catalyst_match("model", "DBIx::Class::Schema", "Foo", sp(0, 0, 0, 5));
        let emissions = plugin.on_match("context_call", &m);
        assert!(emissions.is_empty(),
            "non-Catalyst receiver must not emit; got: {:?}", emissions);
    }

    #[test]
    fn non_matching_on_match_returns_empty() {
        use crate::plugin::MatchContext;

        let src = r#"
            fn id() { "demo2" }
            fn triggers() { [ #{ Always: () } ] }
            fn patterns() {
                [ #{ name: "wanted_call", query: "(function_call_expression) @call" } ]
            }
            fn on_match(pattern, m) {
                if pattern == "wanted_call" { return [#{ FrameworkImport: #{ keyword: "ok" } }]; }
                []
            }
        "#;
        let engine = Arc::new(make_engine());
        let plugin = RhaiPlugin::from_source(src, engine).unwrap();

        let mut captures = std::collections::HashMap::new();
        captures.insert("call".to_string(), one(mcap("f()", sp(0, 0, 0, 3))));
        let m = MatchContext {
            pattern: "other_call".into(),
            span: sp(0, 0, 0, 3),
            package: None,
            package_parents: vec![],
            package_uses: vec![],
            captures,
        };
        assert!(plugin.on_match("other_call", &m).is_empty());
    }
}
