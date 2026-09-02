//! `--heatmap` contract: per-symbol fan-in over the cross-file reference
//! graph, plus unreferenced-symbol (dead-code-candidate) flagging with the
//! sound over-approximation — exported / constructor / dynamic-dispatch
//! symbols are treated as reachable and never flagged.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_perl-lsp");

fn run_heatmap_raw(root: &std::path::Path, extra: &[&str]) -> String {
    let mut cache = root.to_path_buf();
    cache.push(".test-cache");
    let mut args = vec!["--heatmap", root.to_str().unwrap()];
    args.extend_from_slice(extra);
    let out = Command::new(BIN)
        .args(&args)
        .current_dir(root)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("run perl-lsp --heatmap");
    String::from_utf8(out.stdout).expect("utf8 stdout")
}

fn run_heatmap(root: &std::path::Path) -> serde_json::Value {
    let stdout = run_heatmap_raw(root, &[]);
    serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("heatmap JSON parse ({e}): {stdout}"))
}

/// Find the symbol row for `name`, panicking with context if absent.
fn sym<'a>(report: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    report["symbols"]
        .as_array()
        .expect("symbols array")
        .iter()
        .find(|s| s["name"].as_str() == Some(name))
        .unwrap_or_else(|| panic!("no symbol {name} in {}", report["symbols"]))
}

#[test]
fn fan_in_counts_and_unreferenced_subs_flagged() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-heatmap-fns-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib").join("Calc");
    std::fs::create_dir_all(&lib).unwrap();

    std::fs::write(
        lib.join("Util.pm"),
        "package Calc::Util;\n\
         use Exporter 'import';\n\
         our @EXPORT_OK = qw(add subtract);\n\
         sub add { my ($a, $b) = @_; return $a + $b; }\n\
         sub subtract { my ($a, $b) = @_; return $a - $b; }\n\
         sub orphan_helper { return 42; }\n\
         1;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("script.pl"),
        "use lib 'lib';\n\
         use Calc::Util qw(add subtract);\n\
         sub main_run {\n\
         \x20   my $x = add(2, 3);\n\
         \x20   return add($x, 10);\n\
         }\n\
         main_run();\n",
    )
    .unwrap();

    let report = run_heatmap(&dir);
    assert_eq!(report["dynamic_dispatch_sites"].as_u64(), Some(0));

    // `add` is called twice (plus mentioned in the import / export lists) —
    // referenced, never a candidate.
    let add = sym(&report, "add");
    assert!(
        add["fan_in"].as_u64().unwrap() >= 2,
        "add fan_in should count its call sites: {add}"
    );
    assert_eq!(add["dead_code_candidate"].as_bool(), Some(false));

    // `subtract` is exported and referenced (import list + `@EXPORT_OK`
    // mention + one call): a reference site is any mention, so it carries
    // nonzero fan-in and is never a dead candidate.
    let subtract = sym(&report, "subtract");
    assert!(subtract["fan_in"].as_u64().unwrap() >= 1);
    assert_eq!(subtract["dead_code_candidate"].as_bool(), Some(false));

    // `main_run` references three distinct callees (add, plus the implicit
    // recursion is excluded) — fan_out is intra-body.
    let main_run = sym(&report, "main_run");
    assert!(main_run["fan_out"].as_u64().unwrap() >= 1);

    // `orphan_helper`: never referenced, not exported, no dynamic dispatch —
    // the one true dead-code candidate.
    let orphan = sym(&report, "orphan_helper");
    assert_eq!(orphan["fan_in"].as_u64(), Some(0));
    assert_eq!(orphan["dead_code_candidate"].as_bool(), Some(true));

    let dead: Vec<&str> = report["dead_code_candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert_eq!(dead, vec!["orphan_helper"], "exactly one dead candidate");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The dead-export queue: an EXPORTED sub with no cross-file use is listed,
/// an exported sub a consumer references is not, and a non-exported sub is
/// never a dead export (though it may still be a dead-code candidate). This
/// drives the full CLI — the relational store's unused-exports view when it
/// covers the workspace, the references projection as the sound fallback —
/// and both must yield the same verdict.
#[test]
fn dead_exports_flagged_and_scoped_to_cross_file_use() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-heatmap-deadexp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib").join("My");
    std::fs::create_dir_all(&lib).unwrap();

    std::fs::write(
        lib.join("Api.pm"),
        "package My::Api;\n\
         use Exporter 'import';\n\
         our @EXPORT_OK = qw(used_export lonely_export);\n\
         sub used_export { return 1; }\n\
         sub lonely_export { return 2; }\n\
         sub internal_only { return 3; }\n\
         1;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("script.pl"),
        "use lib 'lib';\n\
         use My::Api qw(used_export);\n\
         used_export();\n",
    )
    .unwrap();

    let report = run_heatmap(&dir);

    let dead_exports: Vec<&str> = report["dead_exports"]
        .as_array()
        .expect("dead_exports array")
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert!(
        dead_exports.contains(&"lonely_export"),
        "an exported sub with no cross-file use is a dead export: {dead_exports:?}"
    );
    assert!(
        !dead_exports.contains(&"used_export"),
        "a cross-file consumer keeps the export live: {dead_exports:?}"
    );
    assert!(
        !dead_exports.contains(&"internal_only"),
        "a non-exported sub is never a dead export: {dead_exports:?}"
    );

    // The per-symbol flag agrees with the top-level queue.
    assert_eq!(sym(&report, "lonely_export")["dead_export"].as_bool(), Some(true));
    assert_eq!(sym(&report, "used_export")["dead_export"].as_bool(), Some(false));
    assert_eq!(sym(&report, "internal_only")["dead_export"].as_bool(), Some(false));

    // A dead export is orthogonal to the dead-code queue: exported symbols are
    // shielded there (the `exported` guard), so `lonely_export` is a dead
    // EXPORT but not a dead-code candidate.
    assert_eq!(
        sym(&report, "lonely_export")["dead_code_candidate"].as_bool(),
        Some(false)
    );
    assert_eq!(
        report["summary"]["dead_exports"].as_u64(),
        Some(dead_exports.len() as u64)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn arity_variant_accessors_count_once_and_dsl_imports_are_hidden() {
    // A Mojo::Base `rw` accessor is synthesized as a getter + a fluent writer
    // sharing the same name/span (arity-discriminated for type inference); the
    // writer carries `hide_in_outline`. The heatmap is a symbol-listing view,
    // so it must fold the twin away — one logical method, one row — and must
    // not list the DSL keywords `use Mojolicious::Lite` injects (`app`, etc.).
    let dir = std::env::temp_dir().join(format!("perl-lsp-heatmap-arity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("widget.pl"),
        "package Widget;\n\
         use Mojo::Base -base;\n\
         has 'color';\n\
         1;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("app.pl"),
        "use Mojolicious::Lite;\n\
         get '/' => sub { shift->render(text => 'hi') };\n",
    )
    .unwrap();

    let report = run_heatmap(&dir);
    let syms = report["symbols"].as_array().unwrap();

    let colors: Vec<_> = syms
        .iter()
        .filter(|s| s["name"].as_str() == Some("color") && s["package"].as_str() == Some("Widget"))
        .collect();
    assert_eq!(
        colors.len(),
        1,
        "the rw accessor's getter+writer twin must collapse to one row: {colors:?}"
    );

    assert!(
        !syms.iter().any(|s| s["name"].as_str() == Some("app")),
        "Mojolicious::Lite DSL imports must not be listed as symbols: {syms:?}"
    );
}

#[test]
fn html_view_is_self_contained_and_embeds_the_report() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-heatmap-html-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("script.pl"),
        "sub used { return 1; }\n\
         sub orphan_helper { return 42; }\n\
         used();\n",
    )
    .unwrap();

    let html = run_heatmap_raw(&dir, &["--html"]);
    assert!(html.starts_with("<!DOCTYPE html>"), "must be an HTML document");

    // Self-contained: no remote assets to fetch. (`http://www.w3.org/2000/svg`
    // appears as the SVG createElementNS namespace — that is not a fetch.)
    for needle in ["<link", "src=\"http", "href=\"http", "@import", "url(http"] {
        assert!(!html.contains(needle), "viewer must not reference external asset ({needle})");
    }

    // The same report rides inside the page as embedded JSON, and the data
    // blob never closes the script element early.
    let open = "<script id=\"report\" type=\"application/json\">";
    let start = html.find(open).expect("embedded report script") + open.len();
    let end = start + html[start..].find("</script>").expect("report script close");
    let blob = &html[start..end];
    assert!(!blob.contains("</script"), "data must not break out of the script tag");
    let report: serde_json::Value =
        serde_json::from_str(blob).unwrap_or_else(|e| panic!("embedded JSON parse ({e}): {blob}"));

    assert_eq!(report["schema"].as_str(), Some("perl-lsp.heatmap.v1"));
    let names: Vec<&str> = report["symbols"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"orphan_helper") && names.contains(&"used"));

    let _ = std::fs::remove_dir_all(&dir);
}

/// C/C++ symbols live in a per-language sub-index, not the Perl FileStore.
/// The heatmap must gather them the same way (`for_each_pack_registered_file`)
/// and project fan-in through the SAME `references()` set the cpp
/// references/rename verbs use. `main` is the ABI entry point (shielded), an
/// unreferenced free function IS a dead candidate, and a called function's
/// fan-in equals its call-site count.
#[cfg(feature = "cpp")]
#[test]
fn cpp_fan_in_entry_point_and_dead_code() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-heatmap-cpp-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    std::fs::write(
        dir.join("math.h"),
        "int add(int a, int b);\nint helper_unused(int x);\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("math.cpp"),
        "#include \"math.h\"\n\
         int add(int a, int b) { return a + b; }\n\
         int helper_unused(int x) { return x * 2; }\n",
    )
    .unwrap();
    // `main` calls `add` twice; nobody calls `helper_unused`.
    std::fs::write(
        dir.join("main.cpp"),
        "#include \"math.h\"\n\
         int main() {\n\
         \x20   int r = add(1, 2);\n\
         \x20   int s = add(r, 3);\n\
         \x20   return r + s;\n\
         }\n",
    )
    .unwrap();

    let report = run_heatmap(&dir);
    assert_eq!(
        report["files_indexed"].as_u64(),
        Some(3),
        "all three cpp files gathered: {}",
        report["files_indexed"]
    );

    // Fan-in IS the references() image: `add`'s two call sites, declaration
    // (header proto) and definition excluded.
    let add = sym(&report, "add");
    assert_eq!(
        add["fan_in"].as_u64(),
        Some(2),
        "add is called twice from main: {add}"
    );
    assert_eq!(add["dead_code_candidate"].as_bool(), Some(false));

    // `main` has no static caller (the runtime enters over the ABI), so it is
    // shielded, never flagged dead.
    let main = sym(&report, "main");
    assert_eq!(main["fan_in"].as_u64(), Some(0));
    assert_eq!(main["reachable_guard"].as_str(), Some("entry-point"));
    assert_eq!(main["dead_code_candidate"].as_bool(), Some(false));

    // `main` calls `add` — fan-out is intra-body distinct callees.
    assert!(main["fan_out"].as_u64().unwrap() >= 1, "main calls add: {main}");

    // The unreferenced free function surfaces as a dead candidate; `main`
    // never does.
    let dead: Vec<&str> = report["dead_code_candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["name"].as_str().unwrap())
        .collect();
    assert!(
        dead.contains(&"helper_unused"),
        "unreferenced cpp function is a dead candidate: {dead:?}"
    );
    assert!(
        !dead.contains(&"main"),
        "the C entry point must never be flagged dead: {dead:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dynamic_dispatch_shields_unreferenced_methods() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-heatmap-dyn-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();

    // `handle` is never called by name, but the workspace dispatches
    // dynamically (`$w->$action`), so a sound analysis cannot prove it dead.
    std::fs::write(
        lib.join("Widget.pm"),
        "package Widget;\n\
         sub new { return bless {}, shift; }\n\
         sub handle { my $self = shift; return 'handled'; }\n\
         sub run {\n\
         \x20   my ($self, $action) = @_;\n\
         \x20   return $self->$action();\n\
         }\n\
         1;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("app.pl"),
        "use lib 'lib';\n\
         use Widget;\n\
         my $w = Widget->new;\n\
         $w->run('handle');\n",
    )
    .unwrap();

    let report = run_heatmap(&dir);
    assert!(
        report["dynamic_dispatch_sites"].as_u64().unwrap() >= 1,
        "the $self->$action call must register as a dynamic-dispatch site: {}",
        report["dynamic_dispatch_sites"]
    );

    let handle = sym(&report, "handle");
    assert_eq!(handle["fan_in"].as_u64(), Some(0), "handle has no static caller");
    assert_eq!(
        handle["reachable_guard"].as_str(),
        Some("dynamic-dispatch"),
        "dynamic dispatch must shield the unreferenced method: {handle}"
    );
    assert_eq!(handle["dead_code_candidate"].as_bool(), Some(false));

    let dead = report["dead_code_candidates"].as_array().unwrap();
    assert!(
        dead.iter().all(|d| d["name"].as_str() != Some("handle")),
        "handle must not appear among dead candidates: {dead:?}"
    );

    // Fan-in is the CandidateSet's references() image: a `sub` declared in a
    // class carries the method override family, so the `$w->run('handle')`
    // METHOD call site counts toward `run` — the divergence the old
    // symbol-side target minting had (a class sub with only method-call
    // callers read as fan_in 0).
    let run = sym(&report, "run");
    assert!(
        run["fan_in"].as_u64().unwrap() >= 1,
        "method-call sites must count for a class sub: {run}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Framework-entry guard: declared `entry.json` rules (PHPUnit bundled)
/// shield runner-invoked symbols from the dead queue — by attribute
/// (`#[Test]`), by convention (`test*` + isa TestCase, lifecycle methods)
/// — while a genuinely unreferenced method still flags.
#[cfg(feature = "php")]
#[test]
fn php_framework_entry_symbols_leave_the_dead_queue() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-heatmap-entry-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("TestCase.php"),
        "<?php\nclass TestCase {\n    public function expect(): void {}\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("UserTest.php"),
        "<?php\n\
         class UserTest extends TestCase {\n\
             protected function setUp(): void {}\n\
             public function testAdd(): void {}\n\
             #[Test]\n\
             public function edgeCases(): void {}\n\
             public function neverCalledHelper(): int { return 1; }\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("BlogController.php"),
        "<?php\n\
         class BlogController {\n\
             #[Route('/blog', name: 'blog_index')]\n\
             public function index(): string { return 'x'; }\n\
         }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Console.php"),
        "<?php\n\
         class Application {}\n\
         class Console extends Application {\n\
             protected function getDefaultCommands(): array { return []; }\n\
             public function getLongVersion(): string { return 'v'; }\n\
         }\n",
    )
    .unwrap();
    let report = run_heatmap(&dir);
    for name in ["setUp", "testAdd", "edgeCases", "index", "getDefaultCommands", "getLongVersion"] {
        let row = sym(&report, name);
        assert_eq!(
            row["reachable_guard"].as_str(),
            Some("framework-entry"),
            "{name}: {row}"
        );
        assert_eq!(row["dead_code_candidate"].as_bool(), Some(false), "{name}: {row}");
    }
    let helper = sym(&report, "neverCalledHelper");
    assert_eq!(helper["dead_code_candidate"].as_bool(), Some(true), "{helper}");
    let _ = std::fs::remove_dir_all(&dir);
}
