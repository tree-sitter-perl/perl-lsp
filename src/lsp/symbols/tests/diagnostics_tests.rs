use super::*;
use crate::build::builder;

pub(super) fn parse_analysis(source: &str) -> FileAnalysis {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&ts_parser_perl::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    builder::build(&tree, source.as_bytes())
}

/// Build a CachedModule by parsing a synthesized Perl source listing the given exports.
/// Used by tests to seed ModuleIndex with known export lists without real @INC files.
pub(super) fn fake_cached(
    path: &str,
    exports: &[&str],
    exports_ok: &[&str],
) -> std::sync::Arc<crate::index::module_index::CachedModule> {
    let mut source = String::from("package Fake;\n");
    if !exports.is_empty() {
        source.push_str(&format!("our @EXPORT = qw({});\n", exports.join(" ")));
    }
    if !exports_ok.is_empty() {
        source.push_str(&format!("our @EXPORT_OK = qw({});\n", exports_ok.join(" ")));
    }
    for n in exports.iter().chain(exports_ok.iter()) {
        source.push_str(&format!("sub {} {{}}\n", n));
    }
    source.push_str("1;\n");
    std::sync::Arc::new(crate::index::module_index::CachedModule::new(
        std::path::PathBuf::from(path),
        std::sync::Arc::new(parse_analysis(&source)),
    ))
}

#[test]
fn test_diagnostics_skips_builtins() {
    let source = "use Carp qw(croak);\nprint 'hello';\ndie 'oops';\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    // print and die are builtins, croak is explicitly imported — no diagnostics
    assert!(
        diags.is_empty(),
        "Expected no diagnostics for builtins/imported, got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

/// The unified surface's drift pin: builtins the retired adapter allowlist
/// missed (`exp` was even TYPED by the builder's first-arg table while the
/// allowlist flagged it) must not produce unresolved-function hints.
#[test]
fn diagnostics_skip_builtins_the_old_allowlist_missed() {
    let source = "my $e = exp(1);\nmy $f = fc('A');\nmy $b = evalbytes('1');\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    assert!(
        diags.is_empty(),
        "exp/fc/evalbytes are builtins; got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

/// `new` rides the constructor CONVENTION (`conventions::is_constructor_name`),
/// not the builtin table — indirect-object `new Foo(...)` still never flags.
#[test]
fn diagnostics_skip_indirect_object_constructor() {
    let source = "my $obj = new Foo::Bar(1);\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    assert!(
        !diags.iter().any(|d| d.message.contains("'new'")),
        "indirect-object constructor call must not flag `new`: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_diagnostics_unresolved_function() {
    let source = "frobnicate();\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    assert_eq!(diags.len(), 1);
    // Quietest visible severity — unresolved barewords are often genuinely
    // dynamic (AUTOLOAD / runtime glob / uninstalled dep), so they shouldn't
    // dominate the Problems panel.
    assert_eq!(diags[0].severity, Some(DiagnosticSeverity::HINT));
    assert!(diags[0].message.contains("frobnicate"));
}

#[test]
fn unresolved_dispatch_fires_only_when_enabled_and_only_on_untyped_receiver() {
    use crate::lsp::symbols::DiagnosticOptions;
    // `$minion` is a non-self sub param with no type annotation — genuinely
    // untyped. The minion `enqueue` dispatch verb fires, but its receiver
    // can't be pinned to any class → `ReceiverUntyped`.
    let source = "package W;\nsub fire {\n  my ($self, $minion) = @_;\n  $minion->enqueue('send_email');\n}\n1;\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();

    // Off by default: no dispatch diagnostic.
    let default_diags = collect_diagnostics(&analysis, &module_index, Default::default());
    assert!(
        !default_diags.iter().any(|d|
            matches!(&d.code, Some(NumberOrString::String(c)) if c == "unresolved-dispatch")),
        "unresolved-dispatch must be off by default; got {:?}",
        default_diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );

    // Enabled: exactly one unresolved-dispatch on the untyped receiver.
    let on = DiagnosticOptions { unresolved_dispatch: true, ..Default::default() };
    let diags = collect_diagnostics(&analysis, &module_index, on);
    let dispatch_diags: Vec<_> = diags.iter().filter(|d|
        matches!(&d.code, Some(NumberOrString::String(c)) if c == "unresolved-dispatch")).collect();
    assert_eq!(
        dispatch_diags.len(), 1,
        "expected one unresolved-dispatch on the untyped receiver; got {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

#[test]
fn unresolved_dispatch_silent_on_does_not_apply() {
    use crate::lsp::symbols::DiagnosticOptions;
    // Receiver typed to a concrete, unrelated class → DoesNotApply, NOT a
    // typing gap. Even with the diagnostic enabled, it must stay silent.
    let source = "package W;\nsub fire {\n  my $x = Some::Other->new;\n  $x->enqueue('send_email');\n}\npackage Some::Other;\nsub new { bless {}, shift }\nsub enqueue { 1 }\n1;\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let on = DiagnosticOptions { unresolved_dispatch: true, ..Default::default() };
    let diags = collect_diagnostics(&analysis, &module_index, on);
    assert!(
        !diags.iter().any(|d|
            matches!(&d.code, Some(NumberOrString::String(c)) if c == "unresolved-dispatch")),
        "DoesNotApply (typed, unrelated receiver) must never diagnose; got {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_diagnostics_skips_local_sub() {
    let source = "sub helper { 1 }\nhelper();\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    assert!(
        diags.is_empty(),
        "Locally defined sub should not produce diagnostic, got: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

#[test]
fn test_diagnostics_skips_package_qualified() {
    let source = "Foo::Bar::baz();\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    assert!(
        diags.is_empty(),
        "Package-qualified calls should not produce diagnostic",
    );
}

// The `not` operator parses as `ambiguous_function_call_expression` because tree-sitter-perl
// has no dedicated node type for it; it must be suppressed by the builtins list, not by CST shape.
#[test]
fn test_diagnostics_no_unresolved_for_not_operator() {
    let source = "my $x = 1;\nmy $y = not $x;\nif (not $x) { }\n";
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let not_diags: Vec<_> = diags.iter().filter(|d| d.message.contains("not")).collect();
    assert!(
        not_diags.is_empty(),
        "`not` should not produce an unresolved-function diagnostic; got: {:?}",
        not_diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

// SUPER::method calls store `target_name = "SUPER::method"` which can never be found
// by a literal MRO walk; the `::` guard must suppress the diagnostic.
#[test]
fn test_diagnostics_no_unresolved_for_super_method() {
    let source = r#"
package Animal;
sub speak { "..." }

package Dog;
use parent -norequire, 'Animal';
sub speak {
    my ($self) = @_;
    my $parent = $self->SUPER::speak();
    return "Woof! $parent";
}
1;
"#;
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &module_index, Default::default());
    let super_diags: Vec<_> = diags
        .iter()
        .filter(|d| d.message.contains("SUPER"))
        .collect();
    assert!(
        super_diags.is_empty(),
        "`SUPER::speak` should not produce an unresolved-method diagnostic; got: {:?}",
        super_diags.iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

/// Codes of all diagnostics carrying a string code.
fn diag_codes(diags: &[Diagnostic]) -> Vec<String> {
    diags
        .iter()
        .filter_map(|d| match &d.code {
            Some(NumberOrString::String(c)) => Some(c.clone()),
            _ => None,
        })
        .collect()
}

fn undef_deref_diags(source: &str) -> Vec<Diagnostic> {
    let analysis = parse_analysis(source);
    let module_index = crate::index::module_index::ModuleIndex::new_for_test();
    collect_diagnostics(&analysis, &module_index, Default::default())
        .into_iter()
        .filter(|d| matches!(&d.code, Some(NumberOrString::String(c)) if c == "undef-deref"))
        .collect()
}

// D1 — method/deref on a provably-`Undef` receiver. The three guard forms
// that drive a subject to the `Undef` bottom (docs/adr/narrowing-diagnostics.md).

#[test]
fn d1_undef_deref_else_of_if_defined() {
    // The `else` arm of `if (defined $x)` — $x is undef there.
    let src = r#"
package P;
sub f {
    my ($self, $x) = @_;
    if (defined $x) {
        return $x->name;
    } else {
        return $x->name;
    }
}
1;
"#;
    let diags = undef_deref_diags(src);
    assert_eq!(diags.len(), 1, "exactly the else-arm deref fires: {:?}", diag_codes(&undef_deref_diags(src)));
    assert_eq!(diags[0].severity, Some(DiagnosticSeverity::WARNING));
    assert!(diags[0].message.contains("$x"), "{}", diags[0].message);
}

#[test]
fn d1_undef_deref_after_return_if_defined() {
    // Fall-through after `return if defined $x` — $x is undef.
    let src = r#"
package P;
sub f {
    my ($self, $x) = @_;
    return if defined $x;
    $x->name;
}
1;
"#;
    let diags = undef_deref_diags(src);
    assert_eq!(diags.len(), 1, "fall-through deref fires");
}

#[test]
fn d1_undef_deref_unless_defined_body() {
    let src = r#"
package P;
sub f {
    my ($self, $x) = @_;
    unless (defined $x) {
        $x->name;
    }
}
1;
"#;
    let diags = undef_deref_diags(src);
    assert_eq!(diags.len(), 1, "unless-defined body deref fires");
}

#[test]
fn d1_undef_deref_hash_form() {
    let src = r#"
package P;
sub f {
    my ($self, $x) = @_;
    return if defined $x;
    return $x->{host};
}
1;
"#;
    let diags = undef_deref_diags(src);
    assert_eq!(diags.len(), 1, "hash deref on undef fires");
    assert!(diags[0].message.contains("hash deref"), "{}", diags[0].message);
}

#[test]
fn d1_no_undef_deref_in_guarded_branch() {
    // The defined branch strips Optional / leaves a live value — no warning.
    let src = r#"
package P;
sub f {
    my ($self, $x) = @_;
    if (defined $x) {
        return $x->name;
    }
    return;
}
1;
"#;
    assert!(
        undef_deref_diags(src).is_empty(),
        "guarded use must not warn: {:?}",
        undef_deref_diags(src).iter().map(|d| &d.message).collect::<Vec<_>>(),
    );
}

#[test]
fn d1_no_undef_deref_without_guard() {
    // An ordinary untyped receiver is not `Undef` — D1 stays silent.
    let src = r#"
package P;
sub f {
    my ($self, $x) = @_;
    return $x->name;
}
1;
"#;
    assert!(undef_deref_diags(src).is_empty(), "no guard, no Undef, no warning");
}

// D8 — extend `unresolved-method` to cross-file-resolvable classes. A
// cached module whose internal package name IS the class queried (so
// resolution keys line up), unlike `fake_cached`'s always-`Fake` package.
fn cached_class(module: &str, methods: &[&str]) -> std::sync::Arc<crate::index::module_index::CachedModule> {
    let mut src = format!("package {};\n", module);
    for m in methods {
        src.push_str(&format!("sub {} {{}}\n", m));
    }
    src.push_str("1;\n");
    std::sync::Arc::new(crate::index::module_index::CachedModule::new(
        std::path::PathBuf::from(format!("/fake/{}.pm", module.replace("::", "/"))),
        std::sync::Arc::new(parse_analysis(&src)),
    ))
}

fn unresolved_method_diags(
    source: &str,
    idx: &crate::index::module_index::ModuleIndex,
    opts: DiagnosticOptions,
) -> Vec<String> {
    let analysis = parse_analysis(source);
    collect_diagnostics(&analysis, idx, opts)
        .into_iter()
        .filter(|d| matches!(&d.code, Some(NumberOrString::String(c)) if c == "unresolved-method"))
        .map(|d| d.message)
        .collect()
}

#[test]
fn d8_narrowed_local_class_fires_by_default() {
    // The local-class narrowing case is always-on (no flag) — the receiver
    // narrows to an in-file class that lacks the method.
    let src = r#"
package Foo;
sub real { 1 }
package Main;
sub g {
    my ($self, $x) = @_;
    if ($x->isa('Foo')) {
        $x->bogus;
    }
}
1;
"#;
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = unresolved_method_diags(src, &idx, DiagnosticOptions::default());
    assert_eq!(diags.len(), 1, "local narrowed bogus fires by default: {:?}", diags);
    assert!(diags[0].contains("bogus") && diags[0].contains("Foo"), "{}", diags[0]);
}

#[test]
fn d8_narrowed_cross_file_class_gated_behind_flag() {
    let src = r#"
package Main;
sub g {
    my ($self, $x) = @_;
    if ($x->isa('My::Dep')) {
        $x->bogus;
    }
}
1;
"#;
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.insert_cache("My::Dep", Some(cached_class("My::Dep", &["known_method"])));

    // Default: cross-file extension off → no diagnostic.
    assert!(
        unresolved_method_diags(src, &idx, DiagnosticOptions::default()).is_empty(),
        "cross-file unresolved-method must stay silent without the opt-in",
    );

    // Opt-in: the cross-file class is known and lacks `bogus` → fires.
    let on = DiagnosticOptions { unresolved_method_cross_file: true, ..Default::default() };
    let diags = unresolved_method_diags(src, &idx, on);
    assert_eq!(diags.len(), 1, "opt-in cross-file bogus fires: {:?}", diags);
    assert!(diags[0].contains("My::Dep"), "{}", diags[0]);
}

#[test]
fn d8_cross_file_existing_method_does_not_fire() {
    let src = r#"
package Main;
sub g {
    my ($self, $x) = @_;
    if ($x->isa('My::Dep')) {
        $x->known_method;
    }
}
1;
"#;
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    idx.insert_cache("My::Dep", Some(cached_class("My::Dep", &["known_method"])));
    let on = DiagnosticOptions { unresolved_method_cross_file: true, ..Default::default() };
    assert!(
        unresolved_method_diags(src, &idx, on).is_empty(),
        "a method that exists cross-file must not fire",
    );
}

#[test]
fn d8_unknown_class_stays_silent_even_with_flag() {
    // Not local, not cached — external/uninstalled. Even opt-in stays silent.
    let src = r#"
package Main;
sub g {
    my ($self, $x) = @_;
    if ($x->isa('Totally::Unknown')) {
        $x->bogus;
    }
}
1;
"#;
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let on = DiagnosticOptions { unresolved_method_cross_file: true, ..Default::default() };
    assert!(
        unresolved_method_diags(src, &idx, on).is_empty(),
        "an unknown class is never flagged — we can't enumerate its methods",
    );
}

// D2 — unguarded `Optional<T>` dereference (opt-in). `maybe_get` returns
// `Optional<Foo>` via the bare-`return` idiom; an unguarded `$r->...` fires,
// a `defined`-guarded one does not.
fn optional_deref_diags(source: &str) -> Vec<Diagnostic> {
    let analysis = parse_analysis(source);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let on = DiagnosticOptions { optional_deref: true, ..Default::default() };
    collect_diagnostics(&analysis, &idx, on)
        .into_iter()
        .filter(|d| matches!(&d.code, Some(NumberOrString::String(c)) if c == "optional-deref"))
        .collect()
}

const OPTIONAL_SRC: &str = r#"
package Foo;
sub new { bless {}, shift }
sub name { "foo" }
package P;
sub maybe_get {
    my ($self) = @_;
    return unless $self->{ok};
    return Foo->new;
}
sub use_it {
    my ($self) = @_;
    my $r = $self->maybe_get;
    return $r->name;
}
1;
"#;

#[test]
fn d2_unguarded_optional_deref_fires_when_enabled() {
    let diags = optional_deref_diags(OPTIONAL_SRC);
    assert_eq!(diags.len(), 1, "unguarded Optional deref fires: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>());
    assert_eq!(diags[0].severity, Some(DiagnosticSeverity::INFORMATION));
    assert!(diags[0].message.contains("may be undef"), "{}", diags[0].message);
}

#[test]
fn d2_off_by_default() {
    let analysis = parse_analysis(OPTIONAL_SRC);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &idx, DiagnosticOptions::default());
    assert!(
        !diags.iter().any(|d| matches!(&d.code, Some(NumberOrString::String(c)) if c == "optional-deref")),
        "optional-deref must be silent without the opt-in",
    );
}

#[test]
fn d2_guarded_optional_does_not_fire() {
    // `defined` strips the Optional → no warning on the guarded use.
    let src = r#"
package Foo;
sub new { bless {}, shift }
sub name { "foo" }
package P;
sub maybe_get {
    my ($self) = @_;
    return unless $self->{ok};
    return Foo->new;
}
sub use_it {
    my ($self) = @_;
    my $r = $self->maybe_get;
    if (defined $r) {
        return $r->name;
    }
    return;
}
1;
"#;
    assert!(optional_deref_diags(src).is_empty(), "defined-guarded Optional must not fire");
}

#[test]
fn d2_quick_fix_inserts_defined_guard() {
    let analysis = parse_analysis(OPTIONAL_SRC);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let on = DiagnosticOptions { optional_deref: true, ..Default::default() };
    let diags = collect_diagnostics(&analysis, &idx, on);
    let uri = Url::parse("file:///t.pl").unwrap();
    let actions = code_actions(&diags, &analysis, "", &uri);
    let action = actions.iter().find_map(|a| match a {
        CodeActionOrCommand::CodeAction(ca) if ca.title.contains("return unless defined") => Some(ca),
        _ => None,
    });
    let action = action.expect("a guard quick-fix is offered");
    let edits = action.edit.as_ref().unwrap().changes.as_ref().unwrap().get(&uri).unwrap();
    assert_eq!(edits.len(), 1);
    assert!(
        edits[0].new_text.contains("return unless defined $r;"),
        "quick-fix inserts the guard: {:?}",
        edits[0].new_text,
    );
}

// D3 (redundant-guard) / D4 (contradictory-guard) — a guard whose outcome
// the lattice already fixes, given the subject's prior type.
fn guard_diags(source: &str) -> Vec<(String, String)> {
    let analysis = parse_analysis(source);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let on = DiagnosticOptions { redundant_guard: true, ..Default::default() };
    collect_diagnostics(&analysis, &idx, on)
        .into_iter()
        .filter_map(|d| match &d.code {
            Some(NumberOrString::String(c))
                if c == "redundant-guard" || c == "contradictory-guard" =>
            {
                Some((c.clone(), d.message))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn d3_defined_guard_on_confident_value_is_redundant() {
    let src = r#"
package Foo;
sub new { bless {}, shift }
sub name { "f" }
package Main;
sub g {
    my $x = Foo->new;
    if (defined $x) {
        return $x->name;
    }
    return;
}
1;
"#;
    let diags = guard_diags(src);
    assert_eq!(diags.len(), 1, "redundant defined guard: {:?}", diags);
    assert_eq!(diags[0].0, "redundant-guard");
}

#[test]
fn d4_defined_guard_on_undef_is_contradictory() {
    let src = r#"
package Main;
sub g {
    my ($self, $x) = @_;
    return if defined $x;
    if (defined $x) {
        return 1;
    }
    return;
}
1;
"#;
    let diags = guard_diags(src);
    assert!(
        diags.iter().any(|(c, _)| c == "contradictory-guard"),
        "defined guard on a proven-undef subject is contradictory: {:?}",
        diags,
    );
}

#[test]
fn d3_isa_redundant_same_and_subclass() {
    let src = r#"
package Base;
sub new { bless {}, shift }
package Foo;
our @ISA = ('Base');
package Main;
sub g {
    my $x = Foo->new;
    if ($x->isa('Foo')) { return 1; }
    if ($x->isa('Base')) { return 2; }
    return;
}
1;
"#;
    let diags = guard_diags(src);
    assert_eq!(diags.len(), 2, "both same-class and ancestor isa are redundant: {:?}", diags);
    assert!(diags.iter().all(|(c, _)| c == "redundant-guard"), "{:?}", diags);
}

#[test]
fn d4_isa_unrelated_class_is_contradictory() {
    let src = r#"
package Foo;
sub new { bless {}, shift }
package Bar;
package Main;
sub g {
    my $x = Foo->new;
    if ($x->isa('Bar')) { return 1; }
    return;
}
1;
"#;
    let diags = guard_diags(src);
    assert_eq!(diags.len(), 1, "{:?}", diags);
    assert_eq!(diags[0].0, "contradictory-guard");
}

#[test]
fn d4_isa_downcast_is_inconclusive() {
    // $x is the BASE; testing isa(child) is a legitimate downcast — not flagged.
    let src = r#"
package Base;
sub new { bless {}, shift }
package Sub;
our @ISA = ('Base');
package Main;
sub g {
    my $x = Base->new;
    if ($x->isa('Sub')) { return 1; }
    return;
}
1;
"#;
    assert!(guard_diags(src).is_empty(), "a downcast guard must not be flagged: {:?}", guard_diags(src));
}

#[test]
fn d3_d4_off_by_default() {
    let src = r#"
package Foo;
sub new { bless {}, shift }
package Main;
sub g {
    my $x = Foo->new;
    if (defined $x) { return 1; }
    return;
}
1;
"#;
    let analysis = parse_analysis(src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &idx, DiagnosticOptions::default());
    assert!(
        !diags.iter().any(|d| matches!(&d.code, Some(NumberOrString::String(c)) if c == "redundant-guard" || c == "contradictory-guard")),
        "guard redundancy is opt-in",
    );
}

// D6 — hash deref on a guard-narrowed array/code-shaped receiver. Reads the
// guard's rep specifically (the deref self-infers HashRef otherwise), so it
// fires only when a `ref…eq` guard proved the non-hash rep.
fn deref_shape_diags(source: &str) -> Vec<String> {
    let analysis = parse_analysis(source);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let on = DiagnosticOptions { deref_shape: true, ..Default::default() };
    collect_diagnostics(&analysis, &idx, on)
        .into_iter()
        .filter(|d| matches!(&d.code, Some(NumberOrString::String(c)) if c == "deref-shape-mismatch"))
        .map(|d| d.message)
        .collect()
}

#[test]
fn d6_hash_deref_on_arrayref_guard_fires() {
    let src = r#"
package Main;
sub g {
    my ($self, $x) = @_;
    if (ref($x) eq 'ARRAY') {
        return $x->{key};
    }
    return;
}
1;
"#;
    let diags = deref_shape_diags(src);
    assert_eq!(diags.len(), 1, "hash deref on a ref-eq-ARRAY-narrowed receiver fires: {:?}", diags);
    assert!(diags[0].contains("array ref"), "{}", diags[0]);
}

#[test]
fn d6_hash_deref_on_coderef_guard_fires() {
    let src = r#"
package Main;
sub g {
    my ($self, $x) = @_;
    if (ref($x) eq 'CODE') {
        return $x->{key};
    }
    return;
}
1;
"#;
    let diags = deref_shape_diags(src);
    assert_eq!(diags.len(), 1, "{:?}", diags);
    assert!(diags[0].contains("code ref"), "{}", diags[0]);
}

#[test]
fn d6_no_guard_does_not_fire() {
    // Not guard-narrowed → D6 stays silent (the literal's rep is masked by the
    // deref's own HashRef belief; firing here is out of scope by design).
    let src = r#"
package Main;
sub g {
    my $x = [1, 2, 3];
    return $x->{key};
}
1;
"#;
    assert!(deref_shape_diags(src).is_empty(), "non-guard deref is the documented residual");
}

#[test]
fn d6_hash_guard_does_not_fire() {
    // `ref eq 'HASH'` then a hash deref is correct — no mismatch.
    let src = r#"
package Main;
sub g {
    my ($self, $x) = @_;
    if (ref($x) eq 'HASH') {
        return $x->{key};
    }
    return;
}
1;
"#;
    assert!(deref_shape_diags(src).is_empty(), "hash deref on a HASH-narrowed receiver is correct");
}

#[test]
fn d6_off_by_default() {
    let src = r#"
package Main;
sub g {
    my ($self, $x) = @_;
    if (ref($x) eq 'ARRAY') {
        return $x->{key};
    }
    return;
}
1;
"#;
    let analysis = parse_analysis(src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let diags = collect_diagnostics(&analysis, &idx, DiagnosticOptions::default());
    assert!(
        !diags.iter().any(|d| matches!(&d.code, Some(NumberOrString::String(c)) if c == "deref-shape-mismatch")),
        "deref-shape is opt-in",
    );
}

// Residual closed: array (`$x->[i]`) and code (`$x->()`) deref receivers now
// flow through the same deref stream as method/hash.

#[test]
fn d1_undef_array_and_code_deref() {
    let src = r#"
package P;
sub f {
    my ($self, $x) = @_;
    return if defined $x;
    $x->[0];
    $x->();
}
1;
"#;
    let diags = undef_deref_diags(src);
    assert_eq!(diags.len(), 2, "array + code deref on undef both fire: {:?}",
        diags.iter().map(|d| &d.message).collect::<Vec<_>>());
}

#[test]
fn d2_optional_array_deref() {
    let src = r#"
package Foo;
sub new { bless [], shift }
package P;
sub maybe_get {
    my ($self) = @_;
    return unless $self->{ok};
    return [1, 2];
}
sub use_it {
    my ($self) = @_;
    my $r = $self->maybe_get;
    return $r->[0];
}
1;
"#;
    let analysis = parse_analysis(src);
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let on = DiagnosticOptions { optional_deref: true, ..Default::default() };
    let n = collect_diagnostics(&analysis, &idx, on)
        .into_iter()
        .filter(|d| matches!(&d.code, Some(NumberOrString::String(c)) if c == "optional-deref"))
        .count();
    assert_eq!(n, 1, "unguarded Optional array deref fires");
}

#[test]
fn d6_array_deref_on_hash_guard_fires() {
    // `ref eq 'HASH'` then an array deref is the mismatch.
    let src = r#"
package Main;
sub g {
    my ($self, $x) = @_;
    if (ref($x) eq 'HASH') {
        return $x->[0];
    }
    return;
}
1;
"#;
    let diags = deref_shape_diags(src);
    assert_eq!(diags.len(), 1, "array deref on a HASH-narrowed receiver fires: {:?}", diags);
    assert!(diags[0].contains("hash ref") && diags[0].contains("array deref"), "{}", diags[0]);
}

#[test]
fn d6_code_call_on_hash_guard_fires() {
    let src = r#"
package Main;
sub g {
    my ($self, $x) = @_;
    if (ref($x) eq 'HASH') {
        return $x->();
    }
    return;
}
1;
"#;
    let diags = deref_shape_diags(src);
    assert_eq!(diags.len(), 1, "{:?}", diags);
    assert!(diags[0].contains("call"), "{}", diags[0]);
}

#[test]
fn d6_array_deref_on_array_guard_does_not_fire() {
    let src = r#"
package Main;
sub g {
    my ($self, $x) = @_;
    if (ref($x) eq 'ARRAY') {
        return $x->[0];
    }
    return;
}
1;
"#;
    assert!(deref_shape_diags(src).is_empty(), "array deref on an ARRAY-narrowed receiver is correct");
}

// --- DiagnosticOptions: the struct is the schema ---

fn camel_to_kebab(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_uppercase() {
            out.push('-');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Every `DiagnosticOptions` field must be settable via its canonical
/// `--kebab` CLI flag, derived from the serde camelCase key. serde itself
/// enumerates the fields (via serialization), so adding a field with no CLI
/// flag — or a mistyped flag — fails this test. Guards the one surface serde
/// can't derive (`from_cli_args`) against drift.
#[test]
fn cli_flags_match_diagnostic_option_fields() {
    let keys: Vec<String> = serde_json::to_value(DiagnosticOptions::default())
        .unwrap()
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert!(!keys.is_empty(), "expected at least one option field");

    for key in &keys {
        let flag = format!("--{}", camel_to_kebab(key));
        let parsed = serde_json::to_value(DiagnosticOptions::from_cli_args(&[flag.clone()])).unwrap();
        let set: Vec<&String> = parsed
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, v)| v.as_bool() == Some(true))
            .map(|(k, _)| k)
            .collect();
        assert_eq!(
            set,
            vec![key],
            "CLI flag `{}` should set exactly the `{}` field (drift between from_cli_args and the struct)",
            flag,
            key,
        );
    }
}

/// The LSP side is pure serde: a camelCase key under `diagnostics` sets its
/// field; absent keys default to false; unknown keys are ignored.
#[test]
fn diagnostic_options_deserialize_from_lsp_shape() {
    let v = serde_json::json!({ "optionalDeref": true, "somethingUnknown": true });
    let opts: DiagnosticOptions = serde_json::from_value(v).unwrap();
    assert!(opts.optional_deref, "camelCase key sets the field");
    assert!(!opts.redundant_guard, "absent key defaults to false");
    assert!(!opts.unresolved_dispatch, "absent key defaults to false");
}
