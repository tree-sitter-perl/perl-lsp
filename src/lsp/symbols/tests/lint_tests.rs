//! role-requires + helper-not-loaded lints, pack macro goto, D5-via-D3 pin.

use super::*;

// ---- role-requires-unfulfilled (composer-mismatch) ----

fn role_requires_diags(source: &str) -> Vec<String> {
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    collect_diagnostics(&analysis, &module_index, Default::default())
        .into_iter()
        .filter(|d| {
            matches!(&d.code, Some(tower_lsp::lsp_types::NumberOrString::String(c))
                if c == "role-requires-unfulfilled")
        })
        .map(|d| d.message)
        .collect()
}

#[test]
fn test_role_requires_unfulfilled_fires_on_missing_def() {
    let msgs = role_requires_diags(
        "package My::Role;\nuse Moo::Role;\nrequires 'fetch';\n\
         package My::Broken;\nuse Moo;\nwith 'My::Role';\nsub other { 1 }\n1;\n",
    );
    assert_eq!(
        msgs,
        vec!["role My::Role requires 'fetch'; My::Broken does not provide it"],
    );
}

#[test]
fn test_role_requires_satisfied_stays_quiet() {
    // Local sub, has-accessor, and a sibling role's def all count as
    // provided; the role itself is never diagnosed.
    let msgs = role_requires_diags(
        "package My::Role;\nuse Moo::Role;\nrequires 'fetch';\n\
         package My::Provider;\nuse Moo::Role;\nsub fetch { 9 }\n\
         package My::Ok;\nuse Moo;\nwith 'My::Role';\nsub fetch { 1 }\n\
         package My::Attr;\nuse Moo;\nwith 'My::Role';\nhas fetch => (is => 'ro');\n\
         package My::Sibling;\nuse Moo;\nwith 'My::Role', 'My::Provider';\n1;\n",
    );
    assert!(msgs.is_empty(), "expected no diagnostics, got: {:?}", msgs);
}

#[test]
fn test_role_requires_transitive_and_marker_not_a_def() {
    // SubRole composes Role and re-requires the contract: the marker
    // must not satisfy Deep, but Deep's real def does — while Broken2
    // composing SubRole is told about the (inherited) contract.
    let msgs = role_requires_diags(
        "package My::Role;\nuse Moo::Role;\nrequires 'fetch';\n\
         package My::SubRole;\nuse Moo::Role;\nwith 'My::Role';\nrequires 'fetch';\n\
         package My::Deep;\nuse Moo;\nwith 'My::SubRole';\nsub fetch { 7 }\n\
         package My::Broken2;\nuse Moo;\nwith 'My::SubRole';\n1;\n",
    );
    assert_eq!(msgs.len(), 1, "only Broken2 should fire, got: {:?}", msgs);
    assert!(msgs[0].contains("My::Broken2 does not provide it"), "got: {:?}", msgs);
}

#[test]
fn test_role_requires_honest_silence() {
    // AUTOLOAD anywhere in the MRO and unresolvable ancestors both
    // suppress — the contract may be satisfied where we can't see.
    let msgs = role_requires_diags(
        "package My::Role;\nuse Moo::Role;\nrequires 'fetch';\n\
         package My::Auto;\nuse Moo;\nwith 'My::Role';\nsub AUTOLOAD { }\n\
         package My::Mystery;\nuse Moo;\nextends 'Vendor::Unknown';\nwith 'My::Role';\n1;\n",
    );
    assert!(msgs.is_empty(), "expected honest silence, got: {:?}", msgs);
}

#[test]
fn test_role_requires_default_implementation_provides() {
    // The GenericCo::Sheets pattern: the role both requires AND defines
    // the name (requires as documentation, def as default). The real
    // def must count as provision — only the marker is excluded.
    let msgs = role_requires_diags(
        "package My::Role;\nuse Moo::Role;\nrequires 'fetch';\nsub fetch { 'default' }\n\
         package My::Composer;\nuse Moo;\nwith 'My::Role';\n1;\n",
    );
    assert!(msgs.is_empty(), "default impl in the role provides, got: {:?}", msgs);
}

#[test]
fn test_role_requires_dynamic_parent_honest_silence() {
    // `with ReportProxy(type => ...)` — a runtime-generated role we
    // can't fold. The recorded parent list is incomplete, so neither
    // the composer-mismatch warning nor the unresolved-method hint
    // may fire on this package.
    let analysis = parse_analysis(
        "package My::Role;\nuse Moo::Role;\nrequires 'fetch';\n\
         package My::Dynamic;\nuse Moo;\nwith RoleGen(type => 'x');\nwith 'My::Role';\n\
         sub run { my $self = shift; $self->fetch }\n1;\n",
    );
    assert!(
        analysis.has_dynamic_parents("My::Dynamic"),
        "unfoldable with-arg must mark the package dynamic",
    );
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let role_or_method: Vec<&String> = diags
        .iter()
        .filter(|d| {
            matches!(&d.code, Some(tower_lsp::lsp_types::NumberOrString::String(c))
                if c == "role-requires-unfulfilled" || c == "unresolved-method")
        })
        .map(|d| &d.message)
        .collect();
    assert!(
        role_or_method.is_empty(),
        "dynamic parents must suppress, got: {:?}",
        role_or_method,
    );
}

// ---- helper-not-loaded (entrypoint-scan lint) ----

fn helper_lint_setup() -> (crate::index::module_index::ModuleIndex, String) {
    // A workspace Mojolicious plugin registering a helper. The bundled
    // mojo plugins synthesize the helper entity bridged to the
    // controller surface.
    let plugin_src = "package My::Plugin::WasLoaded;\nuse Mojo::Base 'Mojolicious::Plugin';\n\
        sub register {\n    my ($self, $app, $conf) = @_;\n    $app->helper(was_loaded => sub { 1 });\n}\n1;\n";
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(plugin_src, None).unwrap();
    let fa = crate::build::builder::build(&tree, plugin_src.as_bytes());
    idx.register_workspace_module(
        std::path::PathBuf::from("/fake/lint/My/Plugin/WasLoaded.pm"),
        std::sync::Arc::new(fa),
    );
    let consumer_src = "package MyApp::C;\nuse Mojo::Base 'Mojolicious::Controller';\n\
        sub act {\n    my $self = shift;\n    $self->was_loaded;\n}\n1;\n"
        .to_string();
    (idx, consumer_src)
}

fn lint_messages(idx: &crate::index::module_index::ModuleIndex, src: &str) -> Vec<String> {
    let analysis = parse_analysis(src);
    collect_diagnostics(&analysis, idx, Default::default())
        .into_iter()
        .filter(|d| {
            matches!(&d.code, Some(tower_lsp::lsp_types::NumberOrString::String(c))
                if c == "helper-not-loaded")
        })
        .map(|d| d.message)
        .collect()
}

#[test]
fn test_helper_not_loaded_fires_for_unloaded_workspace_plugin() {
    let (idx, consumer) = helper_lint_setup();
    let msgs = lint_messages(&idx, &consumer);
    assert_eq!(
        msgs,
        vec!["'was_loaded' is provided by My::Plugin::WasLoaded, which no workspace entrypoint loads"],
    );
}

#[test]
fn test_helper_not_loaded_suppressed_when_an_entrypoint_loads_it() {
    let (idx, consumer) = helper_lint_setup();
    // A packageless entrypoint script loading the plugin — exactly the
    // file shape (lite app) that never enters the module cache; the
    // loaded-set feed must run before the packageless early-return.
    let entry_src = "use My::Plugin::WasLoaded;\nprint 1;\n";
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(entry_src, None).unwrap();
    let fa = crate::build::builder::build(&tree, entry_src.as_bytes());
    idx.register_workspace_module(
        std::path::PathBuf::from("/fake/lint/app.pl"),
        std::sync::Arc::new(fa),
    );
    assert!(lint_messages(&idx, &consumer).is_empty());
}

#[test]
fn test_helper_not_loaded_exempts_installed_plugins() {
    // Same plugin arriving via insert_cache (the @INC path, not
    // workspace registration): the "downloaded = intended" policy —
    // no lint.
    let plugin_src = "package My::Plugin::WasLoaded;\nuse Mojo::Base 'Mojolicious::Plugin';\n\
        sub register {\n    my ($self, $app, $conf) = @_;\n    $app->helper(was_loaded => sub { 1 });\n}\n1;\n";
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(plugin_src, None).unwrap();
    let fa = crate::build::builder::build(&tree, plugin_src.as_bytes());
    idx.insert_cache(
        "My::Plugin::WasLoaded",
        Some(std::sync::Arc::new(crate::model::file_analysis::CachedModule::new(
            std::path::PathBuf::from("/inc/My/Plugin/WasLoaded.pm"),
            std::sync::Arc::new(fa),
        ))),
    );
    let consumer = "package MyApp::C;\nuse Mojo::Base 'Mojolicious::Controller';\n\
        sub act {\n    my $self = shift;\n    $self->was_loaded;\n}\n1;\n";
    assert!(lint_messages(&idx, consumer).is_empty());
}


#[cfg(feature = "cpp")]
mod pack_macro_goto {
    /// The macro variant lane via the CandidateSet: pack routing + source.
    fn macro_defs_at(src: &str, point: tree_sitter::Point) -> Vec<crate::index::resolve::RefLocation> {
        let fa = crate::build::language_driver::LanguageRegistry::with_enabled()
            .for_id("cpp")
            .unwrap()
            .analyze(src);
        let store = crate::index::file_store::FileStore::new();
        let idx = crate::index::module_index::ModuleIndex::new_for_test();
        let key = crate::index::file_store::FileKey::Path(std::path::PathBuf::from("/fake/macro.c"));
        crate::index::resolve::resolve(
            &store,
            &fa,
            key,
            point,
            Some(&idx),
            crate::index::resolve::OverrideScope::default(),
        )
        .with_source(src)
        .definitions()
    }

    /// L1 lock: `#define S S` (self-delegation) must offer exactly the
    /// definition — no duplicate "delegates to S" location pointing at the
    /// same `#define`.
    #[test]
    fn self_delegating_macro_offers_single_location() {
        let src = "#define S S\nint f(void) { return S; }\n";
        let locs = macro_defs_at(src, tree_sitter::Point { row: 1, column: 21 });
        assert_eq!(
            locs.len(),
            1,
            "self-delegation must not add a duplicate see-through offer: {:?}",
            locs.iter().map(|l| (l.span, l.label.clone())).collect::<Vec<_>>()
        );
    }

    /// The see-through offer itself stays: a real delegation (`#define F G`)
    /// offers the def AND the delegate.
    #[test]
    fn real_delegation_still_offers_delegate() {
        let src = "void G(void) { }\n#define F G\nvoid h(void) { F(); }\n";
        let locs = macro_defs_at(src, tree_sitter::Point { row: 2, column: 15 });
        assert!(
            locs.iter().any(|l| l.label.as_deref() == Some("delegates to G")),
            "a real delegation keeps its see-through offer: {:?}",
            locs.iter().map(|l| (l.span, l.label.clone())).collect::<Vec<_>>()
        );
    }
}

// D5 — redundant RE-narrowing. Subsumed by D3: an earlier guard's narrowing
// becomes the subject's prior type at a later same-type guard (the narrowing
// witness survives because no reassignment truncated it), so D3 flags the
// second guard. No separate diagnostic path — this test pins that coverage.
#[test]
fn d5_sequential_renarrow_is_flagged_by_d3() {
    let src = r#"
package Foo;
sub new { bless {}, shift }
sub frob { 1 }
package Main;
sub f {
    my ($self, $x) = @_;
    return unless $x->isa('Foo');
    $x->frob;
    return unless $x->isa('Foo');
    return 1;
}
1;
"#;
    let analysis = parse_analysis(src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let on = DiagnosticOptions { redundant_guard: true, ..Default::default() };
    let redundant: Vec<_> = collect_diagnostics(&analysis, &idx, on)
        .into_iter()
        .filter(|d| matches!(&d.code, Some(NumberOrString::String(c)) if c == "redundant-guard"))
        .collect();
    // Exactly the SECOND guard (line 9, 0-based) is redundant; the first
    // (line 6) narrows from an un-typed prior and is not.
    assert_eq!(
        redundant.len(),
        1,
        "second guard redundant, first not: {:?}",
        redundant.iter().map(|d| (d.range.start.line, &d.message)).collect::<Vec<_>>(),
    );
    assert_eq!(redundant[0].range.start.line, 9);
}
