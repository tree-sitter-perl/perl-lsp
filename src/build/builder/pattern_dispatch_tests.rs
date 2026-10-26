//! Tests for the query-declared plugin capture
//! (`src/builder/pattern_dispatch.rs`, design in
//! `docs/adr/plugin-system.md`). A child of the `tests` module so
//! it shares `build_fa` and the bundled registry.

use super::*;
use crate::build::plugin::{
    EmitAction, FrameworkPlugin, MatchContext, PatternSpec, PluginRegistry, Trigger,
};
use std::sync::Arc;

/// Every bundled plugin pattern's `expect` snippets must hold against
/// the real grammar. This is the ring-2 verification story from the
/// design doc, live: a pattern edit that silently stops matching (the
/// field-queryability trap) fails HERE, not in production.
#[test]
fn bundled_pattern_expects_hold() {
    let reg = crate::build::plugin::default_plugin_registry();
    let mut checked = 0usize;
    for p in reg.all() {
        for spec in p.patterns() {
            assert!(
                !spec.expect.is_empty(),
                "plugin `{}` pattern `{}` has no expect snippets — every \
                 pattern must ship self-verification",
                p.id(),
                spec.name
            );
            crate::build::builder::pattern_dispatch::verify_pattern_expects(spec)
                .unwrap_or_else(|e| panic!("plugin `{}`: {}", p.id(), e));
            checked += 1;
        }
    }
    assert!(
        checked >= 1,
        "at least mojo-events declares patterns; the loop must have run"
    );
}

// ---- Fixed-point gating ----
//
// A pattern emission can change trigger inputs (PackageParent →
// ClassIsa). The dispatch loop must re-evaluate gates and dispatch
// newly-applicable plugins' matches in a later round.

/// Always-on plugin whose pattern emission makes the enclosing package
/// isa `Widget::Base`.
struct BridgePlugin {
    triggers: Vec<Trigger>,
    patterns: Vec<PatternSpec>,
}

impl BridgePlugin {
    fn new() -> Self {
        Self {
            triggers: vec![Trigger::Always],
            patterns: vec![PatternSpec {
                name: "make".into(),
                language: "perl".into(),
                phase: "walk".into(),
                query: r#"
                    (method_call_expression
                      method: (_) @verb (#eq? @verb "make_widget")
                    ) @call
                "#
                .into(),
                projections: Default::default(),
                expect: vec![],
            }],
        }
    }
}

impl FrameworkPlugin for BridgePlugin {
    fn id(&self) -> &str {
        "test-bridge"
    }
    fn triggers(&self) -> &[Trigger] {
        &self.triggers
    }
    fn patterns(&self) -> &[PatternSpec] {
        &self.patterns
    }
    fn on_match(&self, _pattern: &str, m: &MatchContext) -> Vec<EmitAction> {
        vec![EmitAction::PackageParent {
            package: m.package.clone().unwrap_or_default(),
            parent: "Widget::Base".into(),
        }]
    }
}

/// Gated plugin: fires only for packages isa `Widget::Base` — which
/// only becomes true through BridgePlugin's round-1 emission.
struct WidgetPlugin {
    triggers: Vec<Trigger>,
    patterns: Vec<PatternSpec>,
}

impl WidgetPlugin {
    fn new() -> Self {
        Self {
            triggers: vec![Trigger::ClassIsa("Widget::Base".into())],
            patterns: vec![PatternSpec {
                name: "finish".into(),
                language: "perl".into(),
                phase: "walk".into(),
                query: r#"
                    (method_call_expression
                      method: (_) @verb (#eq? @verb "finish_widget")
                    ) @call
                "#
                .into(),
                projections: Default::default(),
                expect: vec![],
            }],
        }
    }
}

impl FrameworkPlugin for WidgetPlugin {
    fn id(&self) -> &str {
        "test-widget"
    }
    fn triggers(&self) -> &[Trigger] {
        &self.triggers
    }
    fn patterns(&self) -> &[PatternSpec] {
        &self.patterns
    }
    fn on_match(&self, _pattern: &str, m: &MatchContext) -> Vec<EmitAction> {
        vec![EmitAction::Method {
            name: "made".into(),
            span: m.span,
            selection_span: m.span,
            params: vec![],
            is_method: true,
            return_type: None,
            doc: None,
            on_class: None,
            display: None,
            hide_in_outline: false,
            opaque_return: false,
            outline_label: None,
            return_via_edge: None,
            attr: None,
        }]
    }
}

#[test]
fn pattern_dispatch_reaches_fixed_point_over_gating() {
    let mut reg = PluginRegistry::new();
    reg.register(Box::new(BridgePlugin::new()));
    reg.register(Box::new(WidgetPlugin::new()));
    let source = "package P;\n$w->make_widget;\n$w->finish_widget;\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let fa = crate::build::builder::build_with_plugins(&tree, source.as_bytes(), Arc::new(reg));

    // Round 1: bridge (Always) fires, minting the parent edge.
    assert!(
        fa.declared_parents("P").iter().any(|p| p == "Widget::Base"),
        "bridge plugin's PackageParent emission must land; got {:?}",
        fa.packages
    );
    // Round 2: widget's ClassIsa gate is now true, so its match — same
    // tree, same round-1 query pass — dispatches on re-evaluation.
    assert!(
        fa.symbols().iter().any(|s| s.name == "made"
            && matches!(&s.namespace, crate::model::file_analysis::Namespace::Framework { id } if id == "test-widget")),
        "widget plugin gated on the bridged parent must dispatch in a later round; symbols: {:?}",
        fa.symbols().iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

/// Trigger gating is per MATCH SITE package, not per file: the same
/// verb in a non-firing package stays silent.
#[test]
fn pattern_dispatch_gates_per_package() {
    let mut reg = PluginRegistry::new();
    reg.register(Box::new(WidgetPlugin::new()));
    let source = "package A;\nuse parent 'Widget::Base';\n$w->finish_widget;\npackage B;\n$w->finish_widget;\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    let fa = crate::build::builder::build_with_plugins(&tree, source.as_bytes(), Arc::new(reg));

    let made: Vec<_> = fa
        .symbols()
        .iter()
        .filter(|s| s.name == "made")
        .collect();
    assert_eq!(
        made.len(),
        1,
        "only package A (isa Widget::Base) fires the gated pattern; got {:?}",
        made
    );
    assert_eq!(
        made[0].package.as_deref(),
        Some("A"),
        "the emission lands in the firing package"
    );
}

/// End-to-end through the bundled registry: the ResultDDL DSL's
/// paren-less `col`/relationship calls synthesize accessors via the
/// ported pattern (UsesModule gating + ambiguous-call shape + `str`
/// projection, all through the real dispatcher).
#[test]
fn resultddl_pattern_synthesizes_accessors_end_to_end() {
    let fa = build_fa(
        "package My::Schema::Result::Thing;\n\
         use DBIx::Class::ResultDDL -V2;\n\
         table 'things';\n\
         col text => text;\n\
         has_many searches => { text => 'SearchTerm.text' };\n",
    );
    let ddl_syms: Vec<&str> = fa
        .symbols()
        .iter()
        .filter(|s| {
            matches!(&s.namespace, crate::model::file_analysis::Namespace::Framework { id } if id == "dbic-resultddl")
        })
        .map(|s| s.name.as_str())
        .collect();
    assert!(
        ddl_syms.contains(&"text") && ddl_syms.contains(&"searches"),
        "col + has_many accessors expected; got {:?}",
        ddl_syms
    );
    assert!(
        !ddl_syms.contains(&"things"),
        "`table` is not a declarator verb; got {:?}",
        ddl_syms
    );
}

/// Pluggable diagnostics: a pattern selects the sites, on_match
/// decides, `EmitAction::Diagnostic` rides `FileAnalysis` into the
/// publish path. The data-printer debug-left-in lint is the demo.
#[test]
fn plugin_diagnostic_lint_end_to_end() {
    let fa = build_fa("use DDP;\nmy $x = { a => 1 };\np $x;\nnp($x);\nprint $x;\n");
    let codes: Vec<(&str, &str)> = fa
        .plugin.diagnostics
        .iter()
        .map(|d| (d.code.as_str(), d.plugin_id.as_str()))
        .collect();
    assert_eq!(
        codes,
        vec![
            ("ddp-debug-left", "data-printer"),
            ("ddp-debug-left", "data-printer")
        ],
        "one lint per p/np call, none for print; got {:?}",
        fa.plugin.diagnostics
    );
    assert_eq!(fa.plugin.diagnostics[0].severity, "info");
    assert!(fa.plugin.diagnostics[0].message.contains("`p`"));

    // The gate: same calls in a file that never imports DDP stay silent
    // (the plugin's trigger is Always for its completion hook, so the
    // import check lives in on_match).
    let fa = build_fa("my $x = 1;\np $x;\n");
    assert!(
        fa.plugin.diagnostics.is_empty(),
        "no DDP import, no lint; got {:?}",
        fa.plugin.diagnostics
    );
}

/// The render half: plugin diagnostics come out of `collect_diagnostics`
/// with the plugin id as source and the severity mapped.
#[test]
fn plugin_diagnostics_render_through_collect() {
    let fa = build_fa("use DDP;\np $x;\n");
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = crate::lsp::symbols::collect_diagnostics(&fa, &idx, Default::default());
    let lint = diags
        .iter()
        .find(|d| {
            matches!(&d.code, Some(tower_lsp::lsp_types::NumberOrString::String(c)) if c == "ddp-debug-left")
        })
        .expect("ddp lint should render");
    assert_eq!(lint.source.as_deref(), Some("perl-lsp/data-printer"));
    assert_eq!(
        lint.severity,
        Some(tower_lsp::lsp_types::DiagnosticSeverity::INFORMATION)
    );
}

// ---- #receiver-isa? deferred predicate ----
//
// The gate is NOT a match-time filter: it routes DispatchCall
// emissions onto the `Deferred` query-time seam
// (docs/adr/receiver-gated-dispatch.md), same as dispatch_verbs().

struct QueuePlugin {
    triggers: Vec<Trigger>,
    patterns: Vec<PatternSpec>,
}

impl QueuePlugin {
    fn new() -> Self {
        Self {
            triggers: vec![Trigger::Always],
            patterns: vec![PatternSpec {
                name: "enqueue".into(),
                language: "perl".into(),
                phase: "walk".into(),
                query: r#"
                    (method_call_expression
                      invocant: (_) @recv
                      method: (_) @verb (#eq? @verb "enqueue_thing")
                      (#receiver-isa? @recv "Widget::Queue")
                      arguments: [
                        (list_expression . (_) @task)
                        (string_literal) @task
                      ]
                    ) @call
                "#
                .into(),
                projections: {
                    let mut m = std::collections::HashMap::new();
                    m.insert("task".to_string(), vec!["str".to_string()]);
                    m
                },
                expect: vec![],
            }],
        }
    }
}

impl FrameworkPlugin for QueuePlugin {
    fn id(&self) -> &str {
        "test-queue"
    }
    fn triggers(&self) -> &[Trigger] {
        &self.triggers
    }
    fn patterns(&self) -> &[PatternSpec] {
        &self.patterns
    }
    fn on_match(&self, _pattern: &str, m: &MatchContext) -> Vec<EmitAction> {
        let Some(crate::build::plugin::CaptureValue::One(task)) = m.captures.get("task") else {
            return vec![];
        };
        let Some(name) = task.string_value.clone() else {
            return vec![];
        };
        vec![EmitAction::DispatchCall {
            name,
            dispatcher: "enqueue_thing".into(),
            owner: crate::model::file_analysis::HandlerOwner::Class("Widget::Queue".into()),
            span: task.span,
            var_text: String::new(),
        }]
    }
}

fn build_queue_fa(source: &str) -> FileAnalysis {
    let mut reg = PluginRegistry::new();
    reg.register(Box::new(QueuePlugin::new()));
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    crate::build::builder::build_with_plugins(&tree, source.as_bytes(), Arc::new(reg))
}

#[test]
fn receiver_isa_gate_defers_dispatch_to_query_time() {
    let fa = build_queue_fa(
        "package P;\nmy $q = Widget::Queue->new;\n$q->enqueue_thing('resize');\n",
    );

    // The DispatchCall was NOT applied directly — no ref exists yet.
    assert!(
        !fa.refs().iter().any(|r| matches!(
            &r.kind,
            crate::model::file_analysis::RefKind::DispatchCall { .. }
        )),
        "a gated match's DispatchCall must not materialize at build time"
    );
    // It landed as a `ReceiverIsa` deferral instead.
    assert_eq!(
        fa.provisional_dispatches.len(),
        1,
        "one provisional dispatch candidate expected"
    );
    assert_eq!(fa.provisional_dispatches[0].gate(), "Widget::Queue");
    // The receiver types as exactly the gate class, so query-time
    // resolution applies it (local walk, no module index needed).
    let applied = fa.applicable_dispatches(None);
    assert_eq!(
        applied.len(),
        1,
        "receiver isa gate should resolve at query time; got {:?}",
        applied
    );
    assert_eq!(applied[0].name, "resize");
}

#[test]
fn receiver_isa_gate_blocks_foreign_receiver() {
    let fa = build_queue_fa(
        "package P;\nmy $q = My::Other->new;\n$q->enqueue_thing('resize');\n",
    );
    // Candidate recorded (build time never judges) …
    assert_eq!(fa.provisional_dispatches.len(), 1);
    // … but a receiver typed as an unrelated class never unlocks it.
    assert!(
        fa.applicable_dispatches(None).is_empty(),
        "a foreign receiver class must not unlock the gated payload"
    );
}
