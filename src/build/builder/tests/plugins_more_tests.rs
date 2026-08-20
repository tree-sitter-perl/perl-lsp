use super::*;

// ---- Plugin type overrides ----
//
// Tests pin the contract: a plugin's `overrides()` manifest patches
// local Sub/Method return types AFTER inference, with provenance
// recorded so debugging can tell asserted from inferred. The
// bundled `mojo-routes` plugin overrides `Mojolicious::Routes::Route::_route`
// to return `$self` because the upstream impl uses an `@_`-shift /
// array-slice idiom inference doesn't model.
//
// Targeting is by exact (class, method) — the override fires on the
// home class only; subclasses still get the type via the existing
// cross-file resolution path.

#[test]
fn plugin_override_patches_return_type_on_matching_method() {
    let src = "\
package Mojolicious::Routes::Route;

sub _route {
    my $self = shift;
    # Real impl uses an array slice that inference can't model.
    return $self;
}

1;
";
    let fa = build_fa(src);
    let route_sym = fa
        .symbols()
        .iter()
        .find(|s| s.name == "_route" && matches!(s.kind, SymKind::Sub | SymKind::Method))
        .expect("_route must be parsed as a sub");
    match &route_sym.detail {
        SymbolDetail::Sub { .. } => {
            assert_eq!(
                fa.symbol_return_type_via_bag(route_sym.id, None),
                Some(InferredType::ClassName(
                    "Mojolicious::Routes::Route".into()
                )),
                "override must rewrite return_type to ClassName(Mojolicious::Routes::Route)",
            );
        }
        other => panic!("_route must be a Sub detail; got {:?}", other),
    }
}

#[test]
fn plugin_override_records_provenance_with_plugin_id_and_reason() {
    // The point of provenance is debug-time introspection: a
    // future inspector should be able to ask "why does the LSP
    // think `_route` returns Mojolicious::Routes::Route?" and
    // get back "because mojo-routes' overrides() said so". We pin
    // the plugin id and assert the reason isn't empty so a future
    // refactor that drops the reason field surfaces here.
    let src = "\
package Mojolicious::Routes::Route;
sub _route { my $self = shift; $self }
1;
";
    let fa = build_fa(src);
    let route_id = fa
        .symbols()
        .iter()
        .find(|s| s.name == "_route")
        .expect("_route present")
        .id;
    match fa.return_type_provenance(route_id) {
        TypeProvenance::PluginOverride { plugin_id, reason } => {
            assert_eq!(plugin_id, "mojo-routes");
            assert!(
                !reason.is_empty(),
                "reason must explain why override exists"
            );
        }
        other => panic!("expected PluginOverride provenance; got {:?}", other),
    }
}

#[test]
fn plugin_override_does_not_touch_unrelated_subs() {
    // Same method NAME, different class → override must NOT apply.
    // The match is (class, method); a same-named method on an
    // unrelated package keeps whatever inference produced.
    let src = "\
package Some::Other::Package;
sub _route { my ($x) = @_; { id => $x } }
1;
";
    let fa = build_fa(src);
    let id = fa
        .symbols()
        .iter()
        .find(|s| s.name == "_route")
        .expect("_route present")
        .id;
    // Override must not apply — provenance can be Inferred or
    // ReducerFold (inference produced the type via the witness
    // fold), but NOT PluginOverride.
    assert!(
        !matches!(
            fa.return_type_provenance(id),
            TypeProvenance::PluginOverride { .. }
        ),
        "override must not bleed across packages; provenance: {:?}",
        fa.return_type_provenance(id),
    );
}

#[test]
fn plugin_override_does_not_touch_other_methods_in_target_class() {
    // Same class, different method name → not the target.
    let src = "\
package Mojolicious::Routes::Route;
sub other_method { my $self = shift; { ok => 1 } }
1;
";
    let fa = build_fa(src);
    let id = fa
        .symbols()
        .iter()
        .find(|s| s.name == "other_method")
        .expect("other_method present")
        .id;
    assert!(
        !matches!(
            fa.return_type_provenance(id),
            TypeProvenance::PluginOverride { .. }
        ),
        "override must not bleed across method names; got {:?}",
        fa.return_type_provenance(id),
    );
}

#[test]
fn plugin_override_visible_via_find_method_return_type() {
    // The user-visible payoff: any code path that asks "what does
    // calling `_route` on a Mojolicious::Routes::Route return?"
    // gets the override answer. find_method_return_type is the
    // primary API every chain-resolver / hover / completion path
    // routes through, so pinning it here covers the downstream
    // features without coupling to their specific internals.
    let src = "\
package Mojolicious::Routes::Route;
sub _route { my $self = shift; $self }
1;
";
    let fa = build_fa(src);
    let rt = fa.find_method_return_type("Mojolicious::Routes::Route", "_route", None, None);
    assert_eq!(
        rt,
        Some(InferredType::ClassName("Mojolicious::Routes::Route".into())),
        "find_method_return_type must surface the override-supplied type",
    );
}

#[test]
fn plugin_override_wins_over_inferred_return_type() {
    // Even if inference DID produce a (different) return type, the
    // override replaces it — the whole point is "inference reaches
    // the wrong answer here". The body explicitly returns a hashref
    // so inference would say HashRef without the override.
    let src = "\
package Mojolicious::Routes::Route;

sub _route {
    return { stub => 1 };
}

1;
";
    let fa = build_fa(src);
    let sym = fa
        .symbols()
        .iter()
        .find(|s| s.name == "_route")
        .expect("_route present");
    match &sym.detail {
        SymbolDetail::Sub { .. } => {
            assert_eq!(
                fa.symbol_return_type_via_bag(sym.id, None),
                Some(InferredType::ClassName(
                    "Mojolicious::Routes::Route".into()
                )),
                "override must replace inferred HashRef, not be skipped",
            );
        }
        _ => unreachable!(),
    }
}

// ---- data-printer plugin ----
//
// Data::Printer monkey-patches `&p` and `&np` into the caller's
// symbol table from inside its custom `import` sub — no
// `@EXPORT` / `@EXPORT_OK`, so the cross-file extractor sees them
// as plain Subs but no caller's import list claims them. The
// plugin's job is to declare the imports plugin-side so call
// sites resolve.
//
// `use DDP` is a literal alias for `use Data::Printer` (DDP.pm
// just `push our @ISA, 'Data::Printer'` and re-uses the import).
// The plugin pins the synthetic Import at Data::Printer (the real
// module) regardless of which name the user typed, so cross-file
// hover/gd/sig-help on `p`/`np` always flow to the real source.

#[test]
fn plugin_data_printer_synthesizes_p_np_on_use_data_printer() {
    // `use Data::Printer;` — empty native qw list. Plugin must
    // emit an additional Import that lists `p` and `np` so
    // resolve_call_package finds them and routes cross-file
    // lookups to Data::Printer.
    let src = "\
use Data::Printer;
p $foo;
np \\%bar;
";
    let fa = build_fa(src);
    let dp_import = fa.imports.iter().find(|i| {
        i.module_name == "Data::Printer" && i.imported_symbols.iter().any(|s| s.local_name == "p")
    });
    assert!(
        dp_import.is_some(),
        "plugin must emit Import for Data::Printer carrying `p`; got: {:?}",
        fa.imports
    );
    let names: Vec<&str> = dp_import
        .unwrap()
        .imported_symbols
        .iter()
        .map(|s| s.local_name.as_str())
        .collect();
    assert!(names.contains(&"p"));
    assert!(names.contains(&"np"));
}

#[test]
fn plugin_data_printer_aliases_ddp_to_data_printer() {
    // `use DDP;` — the alias case. Plugin must still emit a
    // synthetic Import keyed on Data::Printer (NOT DDP) so
    // cross-file `p`/`np` lookups route to the real source
    // module. Otherwise the user gets nothing on hover/gd
    // when they typed `use DDP` instead of `use Data::Printer`.
    let src = "\
use DDP;
p $foo;
";
    let fa = build_fa(src);
    let dp_import = fa.imports.iter().find(|i| {
        i.module_name == "Data::Printer" && i.imported_symbols.iter().any(|s| s.local_name == "p")
    });
    assert!(
        dp_import.is_some(),
        "use DDP must produce an Import for Data::Printer (alias resolution); got: {:?}",
        fa.imports
            .iter()
            .map(|i| (
                i.module_name.clone(),
                i.imported_symbols
                    .iter()
                    .map(|s| s.local_name.clone())
                    .collect::<Vec<_>>(),
            ))
            .collect::<Vec<_>>()
    );
}

#[test]
fn plugin_data_printer_skips_unrelated_use_statements() {
    // Sanity check: an unrelated `use` doesn't pull a synthetic
    // Data::Printer import into the file. Otherwise the plugin
    // would be silently claiming every use statement.
    let src = "use List::Util qw(max);";
    let fa = build_fa(src);
    assert!(
        fa.imports
            .iter()
            .find(|i| i.module_name == "Data::Printer")
            .is_none(),
        "plugin must not synthesize a Data::Printer import unless DDP/Data::Printer was used"
    );
}

// ---- Dancer2 plugin tests ----

/// `use Dancer2` autoimports ~90 DSL keywords — unresolved-function
/// diagnostics must skip all of them. The plugin stashes a
/// `FrameworkImport` per keyword into `framework_imports`.
#[test]
fn plugin_dancer2_autoimports_dsl_keywords() {
    let src = r#"
package main;
use Dancer2;

get '/users' => sub { return template 'users' };
post '/login' => sub { my $u = param('user'); session user => $u; };
"#;
    let fa = build_fa(src);
    for kw in &[
        // Route verbs
        "get", "post", "put", "del", "patch", "any", "options",
        // Route organisation
        "prefix",
        // Lifecycle hooks
        "hook",
        // Request / response
        "request", "response", "param", "params",
        "body_parameters", "query_parameters", "route_parameters",
        // Headers / status
        "header", "headers", "content_type", "status",
        // Response control
        "redirect", "forward", "pass", "halt",
        // Rendering
        "template", "send_file", "send_as",
        // Config
        "config", "set", "setting",
        // Session / cookie
        "session", "cookie", "cookies",
        // Serialisers
        "to_json", "from_json", "to_yaml", "from_yaml",
        // Misc
        "var", "vars", "uri_for", "splat", "captures", "upload",
        "push_response_header",
        // App / DSL
        "app", "dancer_app", "dsl", "engine",
        // Async
        "delayed", "flush",
        // Logging
        "debug", "info", "warning", "error",
        // Boolean constants
        "true", "false",
        // Lifecycle
        "dance", "to_app", "start",
        // Keywords absent from the original set — verified against
        // Dancer2::Core::DSL::dsl_keywords (the authoritative list).
        "content", "send_error", "response_header", "request_header",
        "uri_for_route", "prepare_app", "encode_json", "decode_json",
        "to_dumper", "from_dumper", "push_header", "response_headers",
        "psgi_app", "runner", "done", "context",
        "dancer_version", "dancer_major_version",
        "mime", "request_data",
    ] {
        assert!(
            fa.framework_imports.contains(*kw),
            "`{}` must be autoimported by `use Dancer2`; framework_imports={:?}",
            kw,
            fa.framework_imports,
        );
    }
}

/// `use Dancer2` synthesizes typed Sub symbols for high-value DSL
/// functions so chained calls (`request->path`, `app->config`)
/// resolve against the correct class.
#[test]
fn plugin_dancer2_typed_stubs_have_return_types() {
    use crate::model::file_analysis::InferredType;

    let src = r#"
package main;
use Dancer2;
"#;
    let fa = build_fa(src);

    // `request` must resolve to Dancer2::Core::Request.
    let request_sym = fa
        .symbols()
        .iter()
        .find(|s| s.name == "request" && matches!(s.kind, crate::model::file_analysis::SymKind::Sub));
    assert!(
        request_sym.is_some(),
        "dancer plugin must synthesize a `request` Sub symbol"
    );
    let rt = fa.sub_return_type_at_arity("request", None);
    assert_eq!(
        rt,
        Some(InferredType::ClassName("Dancer2::Core::Request".into())),
        "`request` must return Dancer2::Core::Request; got {:?}",
        rt
    );

    // `app` must resolve to Dancer2::Core::App.
    let rt = fa.sub_return_type_at_arity("app", None);
    assert_eq!(
        rt,
        Some(InferredType::ClassName("Dancer2::Core::App".into())),
        "`app` must return Dancer2::Core::App; got {:?}",
        rt
    );

    // `session` must resolve to Dancer2::Core::Session.
    let rt = fa.sub_return_type_at_arity("session", None);
    assert_eq!(
        rt,
        Some(InferredType::ClassName("Dancer2::Core::Session".into())),
        "`session` must return Dancer2::Core::Session; got {:?}",
        rt
    );

    // `config` returns a HashRef.
    let rt = fa.sub_return_type_at_arity("config", None);
    assert_eq!(
        rt,
        Some(InferredType::HashRef),
        "`config` must return HashRef; got {:?}",
        rt
    );

    // `uri_for_route` returns a String (URL).
    let rt = fa.sub_return_type_at_arity("uri_for_route", None);
    assert_eq!(
        rt,
        Some(InferredType::String),
        "`uri_for_route` must return String; got {:?}",
        rt
    );

    // `encode_json` returns a String (the serialized JSON).
    let rt = fa.sub_return_type_at_arity("encode_json", None);
    assert_eq!(
        rt,
        Some(InferredType::String),
        "`encode_json` must return String; got {:?}",
        rt
    );

    // `decode_json` returns a HashRef (the deserialized structure).
    let rt = fa.sub_return_type_at_arity("decode_json", None);
    assert_eq!(
        rt,
        Some(InferredType::HashRef),
        "`decode_json` must return HashRef; got {:?}",
        rt
    );

    // `runner` returns the Dancer2::Core::Runner singleton.
    let rt = fa.sub_return_type_at_arity("runner", None);
    assert_eq!(
        rt,
        Some(InferredType::ClassName("Dancer2::Core::Runner".into())),
        "`runner` must return Dancer2::Core::Runner; got {:?}",
        rt
    );
}

/// `use Dancer2::Plugin` also gets the full DSL — plugins
/// re-export via import and expect every DSL word to be in scope.
#[test]
fn plugin_dancer2_plugin_also_autoimports() {
    let src = r#"
package MyApp::Plugin::Foo;
use Dancer2::Plugin;

register my_keyword => sub { my $dsl = shift; $dsl->param('x') };
"#;
    let fa = build_fa(src);
    for kw in &["get", "post", "param", "request", "session", "config", "debug"] {
        assert!(
            fa.framework_imports.contains(*kw),
            "`{}` must be autoimported by `use Dancer2::Plugin`; got {:?}",
            kw,
            fa.framework_imports,
        );
    }
}

/// Unrelated `use` statements must NOT inject Dancer2 keywords.
/// Guards against the trigger firing too broadly.
#[test]
fn plugin_dancer2_skips_unrelated_use() {
    let src = r#"
package main;
use Mojolicious::Lite;
"#;
    let fa = build_fa(src);
    // `param` is a Dancer2 keyword — it should NOT appear in
    // framework_imports just because of Mojolicious::Lite.
    // (Mojolicious::Lite does not expose `param` as a standalone function.)
    // We verify via the synthesized Sub symbol: the dancer plugin
    // should not have emitted one.
    let dancer_stubs = fa
        .symbols()
        .iter()
        .filter(|s| {
            s.name == "dancer_app"
                && matches!(
                    &s.namespace,
                    crate::model::file_analysis::Namespace::Framework { id } if id == "dancer"
                )
        })
        .count();
    assert_eq!(
        dancer_stubs, 0,
        "dancer plugin must not emit stubs for `use Mojolicious::Lite`"
    );
}

// ---- Red-pin: regressions caught against the rhai-plugins branch ----

/// `my` is lexical and crosses statement-form `package X;`
/// boundaries. The branch's sibling `ScopeKind::Package` was
/// originally swallowing `my` decls, so a use site under
/// `package main;` couldn't resolve a `my` declared under
/// `package Calculator;` earlier in the same file. e2e
/// `rename: $pi → $tau` turned red on the interpolated-string
/// occurrence (the only `$pi` use under `package main;`); this
/// pins the underlying `resolves_to` linkage so the regression
/// can't sneak back in if the scope tree is reshuffled.
#[test]
fn red_pin_my_resolves_across_statement_packages() {
    let src = "\
package Calculator;
my $pi = 3.14159;
sub circumference { my ($self, $r) = @_; return 2 * $pi * $r }

package main;
print \"pi is $pi\\n\";
";
    let fa = build_fa(src);
    let pi_sym = fa
        .symbols()
        .iter()
        .find(|s| s.name == "$pi" && s.kind == SymKind::Variable)
        .expect("$pi Variable symbol");
    let pi_refs: Vec<_> = fa.refs().iter().filter(|r| r.target_name == "$pi").collect();
    assert_eq!(pi_refs.len(), 3, "decl + body use + interpolation = 3 refs");
    for r in &pi_refs {
        assert_eq!(
            r.resolved_symbol(),
            Some(pi_sym.id),
            "ref at {:?} (scope {:?}) didn't resolve to the lexical decl — \
                 sibling Package scopes are leaking into variable lookup",
            r.span.start,
            r.scope,
        );
    }
}

/// `our` is package-global with a lexical alias — bare `$version`
/// from a sibling `package main;` does NOT reach an `our $version`
/// declared under an earlier `package Calculator;` (you'd have to
/// spell `$Calculator::version`). The mirror of
/// `red_pin_my_resolves_across_statement_packages`: that test
/// guarantees `my` keeps crossing package boundaries; this one
/// guarantees `our` keeps NOT crossing them. Pinned now so the
/// scope-separation refactor — which moves variables onto the
/// real lexical scope tree — doesn't accidentally let `our` leak
/// across siblings the way it would if we forgot to keep `our`
/// attached to the package-context scope.
#[test]
fn red_pin_our_does_not_resolve_across_statement_packages() {
    let src = "\
package Calculator;
our $version = 1;
sub bump { $version++ }

package main;
print \"v=$version\\n\";
";
    let fa = build_fa(src);
    let our_sym = fa
        .symbols()
        .iter()
        .find(|s| s.name == "$version" && s.kind == SymKind::Variable)
        .expect("$version Variable symbol");
    // Under Calculator the bare $version refs SHOULD resolve to
    // the our-decl: that's the lexical alias half of `our`.
    let bump_use = fa
        .refs()
        .iter()
        .find(|r| r.target_name == "$version" && r.span.start.row == 2)
        .expect("ref inside Calculator's bump");
    assert_eq!(
        bump_use.resolved_symbol(),
        Some(our_sym.id),
        "bare $version inside the same package as the `our` decl \
             must still resolve to it (lexical alias)"
    );
    // Under `package main;` the bare $version must NOT resolve.
    let main_use = fa
        .refs()
        .iter()
        .find(|r| r.target_name == "$version" && r.span.start.row == 5)
        .expect("ref inside package main's print");
    assert_eq!(
        main_use.resolved_symbol(), None,
        "bare $version under a sibling `package main;` must not \
             reach Calculator's `our $version` — that's $Calculator::version, \
             a different binding"
    );
}

/// Caller-side `HashKeyAccess` for a method/function call's
/// even-position stringy args. `MooApp->new(name => 'alice')`
/// must emit a HashKeyAccess at the `name` token so
/// cursor-on-key resolves to the `has`-emitted HashKeyDef
/// instead of the broad MethodCall ref. Gated on a matching
/// HashKeyDef existing — emission would otherwise shadow the
/// A lexical hash key (`my %h = (k => …); $h{k}`) is one renameable unit: the
/// literal def, every `$h{k}` access AND write rewrite together — single-file,
/// scoped to the `%h` declaration. A different hash's same-named key (`%other`,
/// or a shadowing `%h`) is NOT touched.
#[test]
fn lexical_hash_key_renames_literal_and_accesses_in_scope() {
    let fa = build_fa(
        "my %opts = (timeout => 30, retries => 3);\n\
         my %other = (timeout => 9);\n\
         my $t = $opts{timeout};\n\
         $opts{timeout} = 60;\n\
         print $other{timeout};\n",
    );
    // Cursor on the `$opts{timeout}` access (row 2).
    let col = "my $t = $opts{".len();
    let edits = fa.rename_at(tree_sitter::Point { row: 2, column: col }, "DELAY").expect("renameable");
    let rows: std::collections::BTreeSet<usize> = edits.iter().map(|(s, _)| s.start.row).collect();
    // %opts literal (0), access (2), write (3) — NOT %opts's `retries`, NOT %other (1, 4).
    assert_eq!(
        rows,
        [0, 2, 3].into_iter().collect(),
        "literal + access + write of %opts.timeout only: {edits:?}",
    );
    assert!(
        edits.iter().all(|(_, t)| t == "DELAY"),
        "every edit writes the new key name",
    );
}

/// A lexical hashref scalar (`my $h = { k => … }; $h->{k}`) gets the same
/// renameable-unit treatment as `my %h`: the literal def + every `$h->{k}`
/// deref rename together, scoped to the `my $h` declaration. (`$h = {…}` takes a
/// hashref; `%h = {…}` would be an uneven-list bug and emits nothing — the RHS
/// shape is sigil-keyed.)
#[test]
fn lexical_hashref_scalar_renames_literal_and_derefs() {
    let src = "my $cfg = { host => 'h', port => 8080 };\n\
my $h = $cfg->{host};\n\
print $cfg->{host};\n";
    let fa = build_fa(src);
    let col = src.lines().nth(1).unwrap().find("host").unwrap();
    let edits = fa
        .rename_at(tree_sitter::Point { row: 1, column: col }, "HOST")
        .expect("renameable");
    let rows: std::collections::BTreeSet<usize> = edits.iter().map(|(s, _)| s.start.row).collect();
    // literal def (0) + both `$cfg->{host}` derefs (1, 2); `port` untouched.
    assert_eq!(rows, [0, 1, 2].into_iter().collect(), "literal + 2 derefs: {edits:?}");
    assert!(edits.iter().all(|(_, t)| t == "HOST"), "every edit writes the new key");
}

/// A QUOTED call-arg key (`{ "name", 2 }`) emits its `HashKeyAccess` at the
/// string CONTENT span, not the whole literal — so rename keeps the quotes
/// (rewriting them yields a bareword, a `strict subs` error in a comma list).
#[test]
fn quoted_call_arg_key_span_is_content_not_quotes() {
    let src = "package R;\n\
        use base 'DBIx::Class::Core';\n\
        __PACKAGE__->add_columns(qw/name/);\n\
        sub b { my $s = shift; $s->search({ \"name\", 2 }); }\n1;\n";
    let fa = build_fa(src);
    let r = fa
        .refs()
        .iter()
        .find(|r| {
            r.target_name == "name"
                && matches!(r.kind, RefKind::HashKeyAccess { .. })
                && r.span.start.row == 3
        })
        .expect("quoted key emits a HashKeyAccess on row 3");
    assert_eq!(
        r.span.end.column - r.span.start.column,
        4,
        "span covers the 4-char content `name`, not the 6-char `\"name\"`: {r:?}",
    );
}

/// `class Foo { field $x :param }` `find_param_field`
/// fallback. e2e `rename: 'name' constructor arg` was the
/// surfacing failure.
#[test]
fn red_pin_call_arg_emits_hash_key_access_when_def_exists() {
    let src = "\
package MooApp;
use Moo;
has name => (is => 'ro');

package main;
my $m = MooApp->new(name => 'alice');
";
    let fa = build_fa(src);
    let name_access: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "name" && matches!(r.kind, RefKind::HashKeyAccess { .. }))
        .collect();
    assert!(
        !name_access.is_empty(),
        "no HashKeyAccess emitted for `name` in MooApp->new(name => 'alice')",
    );
    let Some(owner) = name_access[0].hash_key_owner() else {
        panic!("HashKeyAccess emitted with no owner");
    };
    assert_eq!(
        *owner,
        HashKeyOwner::Sub {
            package: Some("MooApp".to_string()),
            name: "new".to_string()
        },
        "constructor-key owner should be Sub{{class, method}}, matching the has-emitted def",
    );

    // No matching HashKeyDef for `count` → no shadow ref.
    let count_access: Vec<_> = fa
        .refs()
        .iter()
        .filter(|r| r.target_name == "count" && matches!(r.kind, RefKind::HashKeyAccess { .. }))
        .collect();
    let src_no_def = "\
package Plain;
sub run {}

package main;
my $p = Plain->run(count => 1);
";
    let fa2 = build_fa(src_no_def);
    let no_emit: Vec<_> = fa2
        .refs()
        .iter()
        .filter(|r| r.target_name == "count" && matches!(r.kind, RefKind::HashKeyAccess { .. }))
        .collect();
    assert!(
        no_emit.is_empty(),
        "no HashKeyDef registered for Plain::run/count → \
             must not emit a phantom HashKeyAccess (would shadow other resolution paths)",
    );
    // `count` accesses on MooApp (which DOESN'T define count) — should not emit either.
    assert!(
        count_access.is_empty(),
        "MooApp has no `count` HashKeyDef → no HashKeyAccess emission expected",
    );
}

/// `=>` is autoquoting sugar for `,` — `f(name => 'alice')` and
/// `f('name', 'alice')` are the same call. The HashKeyAccess
/// emission must be position-based (every odd-indexed stringy
/// arg is a key), NOT keyed off the `=>` token. Pinning this
/// because the original draft of the helper walked siblings
/// looking for `=>` and would have missed the bare-comma form
/// — letting cursor-on-key in the bare-comma shape land on
/// the broad MethodCall ref, which renames the wrong token.
#[test]
fn red_pin_hash_key_access_emission_is_position_based() {
    // Same `has`-emitted def, two call shapes. Both should land
    // a HashKeyAccess at `name`, with the same owner.
    let fat_comma_src = "\
package MooApp;
use Moo;
has name => (is => 'ro');

package main;
my $a = MooApp->new(name => 'alice');
";
    let bare_comma_src = "\
package MooApp;
use Moo;
has name => (is => 'ro');

package main;
my $a = MooApp->new('name', 'alice');
";
    let fa_fat = build_fa(fat_comma_src);
    let fa_bare = build_fa(bare_comma_src);

    // Constructor call site only — the `has name` declaration
    // synthesizes its own internal-key refs that we're not
    // asserting on here.
    fn name_access_at_call<'a>(fa: &'a FileAnalysis) -> Vec<&'a Ref> {
        fa.refs()
            .iter()
            .filter(|r| {
                r.target_name == "name"
                    && matches!(r.kind, RefKind::HashKeyAccess { .. })
                    && r.span.start.row == 5
            })
            .collect()
    }

    let fat_refs = name_access_at_call(&fa_fat);
    let bare_refs = name_access_at_call(&fa_bare);

    assert_eq!(
        fat_refs.len(),
        1,
        "fat-comma form should emit exactly one HashKeyAccess at the call site",
    );
    assert_eq!(
        bare_refs.len(),
        1,
        "bare-comma form (`'name', 'alice'`) must emit the same HashKeyAccess — \
             `=>` is autoquoting sugar, not a structural marker",
    );

    let owner_of = |r: &Ref| match r.hash_key_owner() {
        Some(o) => o.clone(),
        _ => panic!("expected HashKeyAccess with owner"),
    };
    assert_eq!(
        owner_of(fat_refs[0]),
        owner_of(bare_refs[0]),
        "both forms must produce the same Sub{{MooApp, new}} owner",
    );

    // Even-indexed args ARE keys; odd-indexed (values) must
    // NOT get a HashKeyAccess regardless of whether they happen
    // to look like a key string. `'alice'` at idx 1 stays a value.
    for fa in [&fa_fat, &fa_bare] {
        let alice_access: Vec<_> = fa
            .refs()
            .iter()
            .filter(|r| r.target_name == "alice" && matches!(r.kind, RefKind::HashKeyAccess { .. }))
            .collect();
        assert!(
            alice_access.is_empty(),
            "value-position arg must never become a HashKeyAccess",
        );
    }

    // Multi-pair, all bare commas — `('a', 1, 'b', 2)`. Both
    // `a` and `b` are keys (idx 0 and 2); `1` and `2` aren't
    // stringy so they don't even tempt the helper. Need a def
    // for each so emission isn't gated out.
    let multi_src = "\
package MooApp;
use Moo;
has a => (is => 'ro');
has b => (is => 'ro');

package main;
my $m = MooApp->new('a', 1, 'b', 2);
";
    let fa_multi = build_fa(multi_src);
    let call_keys: Vec<&Ref> = fa_multi
        .refs()
        .iter()
        .filter(|r| {
            matches!(r.kind, RefKind::HashKeyAccess { .. })
                && r.span.start.row == 6
                && (r.target_name == "a" || r.target_name == "b")
        })
        .collect();
    assert_eq!(
        call_keys.len(),
        2,
        "both even-position args (`'a'`, `'b'`) must emit HashKeyAccess",
    );
}

/// Carp's canonical shape: `longmess` (caller) is defined before
/// `longmess_heavy` (callee). Both arms of the if/else return the
/// forward-defined call, so the per-sub fold should agree on `String`
/// — but only if the walk-time symbol-table miss for `longmess_heavy`
/// is recovered post-walk. `resolve_forward_call_targets` is what
/// makes this work; without it the bag has no `Expr(call_span)`
/// witness at all and `longmess` returns `None`.
#[test]
fn forward_reference_call_in_sub_return_resolves() {
    let src = r#"
package main;

sub longmess {
    if ($_[0]) {
        return longmess_heavy(@_);
    }
    else {
        return longmess_heavy(@_);
    }
}

sub longmess_heavy { return "ouch"; }
"#;
    let fa = build_fa(src);
    let rt = fa.sub_return_type_at_arity("longmess", None);
    assert_eq!(
        rt,
        Some(InferredType::String),
        "longmess must fold to String through both arms — \
         got {:?}. Walk-order regression: longmess_heavy is \
         defined after longmess.",
        rt,
    );
}

/// Single-arm forward call: implicit `return forward()` should fold
/// to whatever `forward()` returns. No branch arms — exercises the
/// `Symbol ← branch_arm Edge → Expr(body) → Edge(Symbol(callee))`
/// chain at minimum width.
#[test]
fn forward_reference_implicit_return_resolves() {
    let src = r#"
package main;

sub caller_sub { forward_sub() }

sub forward_sub { return "ok"; }
"#;
    let fa = build_fa(src);
    assert_eq!(
        fa.sub_return_type_at_arity("caller_sub", None),
        Some(InferredType::String),
    );
}

/// Forward reference inside a ternary return. Each arm calls a
/// different forward-defined sub; both must resolve so the ternary's
/// `BranchArmFold` agrees on `String`. Mixes the forward-ref fix with
/// the existing ternary path (`emit_expr_witness` recursion + arm
/// witnesses on the ternary span).
#[test]
fn forward_reference_in_ternary_arms_resolves() {
    let src = r#"
package main;

sub dispatch {
    return $_[0] ? handle_a() : handle_b();
}

sub handle_a { return "a"; }
sub handle_b { return "b"; }
"#;
    let fa = build_fa(src);
    assert_eq!(
        fa.sub_return_type_at_arity("dispatch", None),
        Some(InferredType::String),
    );
}

/// Scoped-identifier call: `Pkg::name()` form. `expr_payload`'s
/// `bareword`/`scoped_identifier` arm does the same walk-time lookup
/// as `function_call_expression`; the queue + post-walk resolve must
/// cover it too.
#[test]
fn forward_reference_scoped_identifier_call_resolves() {
    let src = r#"
package main;

sub bridge { return Helper::canon(); }

package Helper;

sub canon { return "yes"; }
"#;
    let fa = build_fa(src);
    assert_eq!(
        fa.sub_return_type_at_arity("bridge", None),
        Some(InferredType::String),
    );
}

/// Self-method tail with a forward-defined target. `$self->later()`
/// where `later` is declared after the caller. The PackageSymbol
/// chase needs the callee's `Symbol(sid)` writeback, which only fires
/// once the callee's own `Expr(body)` is populated — exercising the
/// forward-ref fix on the inner sub's body, not the call site itself.
#[test]
fn forward_reference_self_method_call_resolves() {
    let src = r#"
package Box;

sub new { my $class = shift; return bless {}, $class; }

sub head {
    my ($self) = @_;
    return $self->tail();
}

sub tail {
    my ($self) = @_;
    return helper();
}

sub helper { return "fin"; }
"#;
    let fa = build_fa(src);
    assert_eq!(
        fa.sub_return_type_at_arity("tail", None),
        Some(InferredType::String),
    );
}
