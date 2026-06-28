//! `--heatmap` contract: per-symbol fan-in over the cross-file reference
//! graph, plus unreferenced-symbol (dead-code-candidate) flagging with the
//! sound over-approximation — exported / constructor / dynamic-dispatch
//! symbols are treated as reachable and never flagged.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_perl-lsp");

fn run_heatmap(root: &std::path::Path) -> serde_json::Value {
    let mut cache = root.to_path_buf();
    cache.push(".test-cache");
    let out = Command::new(BIN)
        .args(["--heatmap", root.to_str().unwrap()])
        .current_dir(root)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("run perl-lsp --heatmap");
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
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

    let _ = std::fs::remove_dir_all(&dir);
}
