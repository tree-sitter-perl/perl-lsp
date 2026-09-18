//! CLI cursor-coordinate contract: **output matches input dialect**.
//!
//! Two input forms select two output dialects (the fix for the recurring
//! 0-based-vs-1-based foot-gun):
//!   * positional `<file> <line> <col>`      → 0-based line, byte column (engine)
//!   * `--at <file>:<line>:<col>`            → 1-based line, char column (editor)
//! The editor form's `path:line:col` output round-trips straight back into the
//! next query's `--at`. The fixtures deliberately put a call site after a
//! unicode string literal so byte≠char is exercised on the multibyte line.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_perl-lsp");

/// Line layout (1-based display lines) shared by these tests:
///   1: package Greeter;
///   2: (blank)
///   3: sub greet { return "hi" }
///   4: (blank)
///   5: my $msg = "héllo wörld→"; greet();
///   6: greet();
/// Line 5 has multi-byte UTF-8 (é, ö, →) BEFORE the `greet()` call.
const SRC: &str = "package Greeter;\n\nsub greet { return \"hi\" }\n\nmy $msg = \"héllo wörld→\"; greet();\ngreet();\n";

fn write_fixture(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("perl-lsp-cli-pos-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::write(lib.join("Greeter.pm"), SRC).unwrap();
    dir
}

/// `(file, line, col)` of every reported reference (as rendered by the tool).
fn parse_refs(stdout: &str) -> Vec<(String, u64, u64)> {
    let parsed: serde_json::Value = serde_json::from_str(stdout)
        .unwrap_or_else(|e| panic!("references JSON parse ({e}): {stdout}"));
    parsed
        .as_array()
        .expect("references array")
        .iter()
        .map(|r| {
            (
                r["file"].as_str().unwrap().to_string(),
                r["line"].as_u64().unwrap(),
                r["col"].as_u64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn positional_references_render_zero_based_byte_columns() {
    let dir = write_fixture("posref");
    let mut cache = dir.clone();
    cache.push(".test-cache");

    // Positional cursor on the `greet` def name: row 2 (0-based), byte col 4.
    let out = Command::new(BIN)
        .args(["--references", dir.to_str().unwrap(), "lib/Greeter.pm", "2", "4"])
        .current_dir(&dir)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("run perl-lsp --references");
    let refs = parse_refs(&String::from_utf8(out.stdout).expect("utf8 stdout"));

    // Ground truth in ENGINE coordinates: 0-based row, BYTE column of each hit.
    let expected: Vec<(u64, u64)> = SRC
        .lines()
        .enumerate()
        .flat_map(|(row, line)| {
            let mut hits = Vec::new();
            let mut from = 0usize;
            while let Some(bi) = line[from..].find("greet") {
                let abs = from + bi;
                hits.push((row as u64, abs as u64)); // 0-based row, byte col
                from = abs + "greet".len();
            }
            hits
        })
        .collect();

    let mut got: Vec<(u64, u64)> = refs.iter().map(|(_, l, c)| (*l, *c)).collect();
    got.sort();
    let mut want = expected.clone();
    want.sort();
    assert_eq!(got, want, "positional input must render 0-based/byte output");

    // Pin the unicode line: the `greet()` call is at BYTE offset 30 on row 4
    // (é/ö are 2 bytes, → is 3) — the engine column, not the char column (26).
    let unicode = refs.iter().find(|(_, l, _)| *l == 4);
    assert_eq!(
        unicode.map(|(_, _, c)| *c),
        Some(30),
        "positional output is the byte column on the multibyte line"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn editor_at_references_render_one_based_char_columns() {
    let dir = write_fixture("atref");
    let mut cache = dir.clone();
    cache.push(".test-cache");

    // `--at` cursor on the `greet` def name: editor line 3, char col 5
    // (`sub greet` → the `g` of greet is the 5th character, 1-based).
    let out = Command::new(BIN)
        .args(["--references", dir.to_str().unwrap(), "--at", "lib/Greeter.pm:3:5"])
        .current_dir(&dir)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("run perl-lsp --references --at");
    let refs = parse_refs(&String::from_utf8(out.stdout).expect("utf8 stdout"));

    // Ground truth in EDITOR coordinates: 1-based line, 1-based CHAR column.
    let expected: Vec<(u64, u64)> = SRC
        .lines()
        .enumerate()
        .flat_map(|(row, line)| {
            let mut hits = Vec::new();
            let mut from = 0usize;
            while let Some(bi) = line[from..].find("greet") {
                let abs = from + bi;
                let char_col = line[..abs].chars().count() + 1;
                hits.push(((row + 1) as u64, char_col as u64));
                from = abs + "greet".len();
            }
            hits
        })
        .collect();

    let mut got: Vec<(u64, u64)> = refs.iter().map(|(_, l, c)| (*l, *c)).collect();
    got.sort();
    let mut want = expected.clone();
    want.sort();
    assert_eq!(got, want, "--at input must render 1-based/char output");

    // The unicode-line call is char col 27 (byte 30 would be wrong).
    let unicode = refs.iter().find(|(_, l, _)| *l == 5);
    assert_eq!(
        unicode.map(|(_, _, c)| *c),
        Some(27),
        "--at output is the character column on the multibyte line"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The tool's own `--definition` `path:line:col` output must paste straight
/// back into a `--at` query and resolve to the same definition — the headline
/// round-trip AX win.
#[test]
fn definition_output_round_trips_into_at() {
    let dir = write_fixture("roundtrip");
    let mut cache = dir.clone();
    cache.push(".test-cache");

    // Goto-def from the call on editor line 6, char col 1.
    let def = Command::new(BIN)
        .args(["--definition", dir.to_str().unwrap(), "--at", "lib/Greeter.pm:6:1"])
        .current_dir(&dir)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("run perl-lsp --definition --at");
    let def_out = String::from_utf8(def.stdout).expect("utf8 stdout");
    let def_line = def_out.trim();
    // `sub greet` name → 1-based line 3, char col 5.
    assert!(def_line.ends_with(":3:5"), "definition (editor dialect) should be 3:5, got {def_line:?}");

    // Feed the ABSOLUTE `path:line:col` straight back into `--at`.
    let round = Command::new(BIN)
        .args(["--definition", dir.to_str().unwrap(), "--at", def_line])
        .current_dir(&dir)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("run perl-lsp --definition --at <prev output>");
    let round_out = String::from_utf8(round.stdout).expect("utf8 stdout");
    assert!(
        round_out.trim().ends_with(":3:5"),
        "pasting --definition output into --at must resolve to the same def, got {round_out:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The `[pos]` annotation (stderr) names the dialect, the internal 0-based
/// point, and the landed token — for both a hit and a whitespace miss.
#[test]
fn pos_annotation_reports_dialect_and_landed_token() {
    let dir = write_fixture("annot");
    let mut cache = dir.clone();
    cache.push(".test-cache");

    // Editor form, landing on `greet`.
    let hit = Command::new(BIN)
        .args(["--references", dir.to_str().unwrap(), "--at", "lib/Greeter.pm:3:5"])
        .current_dir(&dir)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("run");
    let hit_err = String::from_utf8(hit.stderr).expect("utf8 stderr");
    assert!(hit_err.contains("read as EDITOR (1-based, char col)"), "annot dialect: {hit_err}");
    assert!(hit_err.contains("internal 2:4"), "annot internal point: {hit_err}");
    assert!(hit_err.contains("landed on token: \"greet\""), "annot token: {hit_err}");

    // Positional form landing on whitespace (row 1 is blank) → loud hint that
    // names the OTHER base.
    let miss = Command::new(BIN)
        .args(["--references", dir.to_str().unwrap(), "lib/Greeter.pm", "1", "0"])
        .current_dir(&dir)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("run");
    let miss_err = String::from_utf8(miss.stderr).expect("utf8 stderr");
    assert!(miss_err.contains("read as POSITIONAL (0-based, byte col)"), "annot dialect: {miss_err}");
    assert!(
        miss_err.contains("whitespace / no token") && miss_err.contains("--at lib/Greeter.pm:2:1"),
        "whitespace miss must hint the 1-based --at form: {miss_err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// E2 (inline-callback form): a Mojo helper registered as
/// `$app->helper(name => sub ($c, ...) {...})` types the callback's first
/// positional as `Mojolicious::Controller`, not the enclosing class. Driven
/// through `--dump-package` so the real bundled `mojo-helpers` plugin runs.
/// The named-sub form (`$app->helper(name => \&_h); sub _h { my $c = shift }`)
/// is covered by `mojo_helper_named_sub_first_param_is_controller` below.
#[test]
fn mojo_helper_callback_first_param_is_controller() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-cli-helper-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let src = "package MyApp;\nuse Mojo::Base 'Mojolicious';\n\nsub startup {\n    my $self = shift;\n    $self->helper(greet => sub ($c, $name) {\n        my $x = $c;\n        return $name;\n    });\n}\n\n1;\n";
    std::fs::write(lib.join("MyApp.pm"), src).unwrap();

    let mut cache = dir.clone();
    cache.push(".test-cache");

    let out = Command::new(BIN)
        .args(["--dump-package", dir.to_str().unwrap(), "MyApp"])
        .current_dir(&dir)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("run perl-lsp --dump-package");
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let dump: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("dump JSON parse ({e}): {stdout}"));

    // Find the anonymous helper callback sub and assert its `$c` param +
    // the `$x = $c` binding both type as the controller.
    let subs = dump["subs"].as_array().expect("subs array");
    let anon = subs
        .iter()
        .find(|s| s["name"] == "(anon)")
        .expect("anonymous callback sub in dump");
    let c_param = anon["params"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "$c")
        .expect("$c param");
    assert_eq!(
        c_param["inferred_type"], "Mojolicious::Controller",
        "helper callback $c should type as the controller"
    );
    let x_var = anon["vars_in_scope"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["var"] == "$x")
        .expect("$x in scope");
    assert_eq!(
        x_var["type"], "Mojolicious::Controller",
        "binding from $c should propagate the controller type"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// E2 (named-sub form): `$app->helper(name => \&_greet); sub _greet { my $c =
/// shift; ... }` types `_greet`'s first positional as `Mojolicious::Controller`
/// — the same override the inline-callback form gets, carried to the named sub
/// by registration shape (not a name allowlist). Driven through
/// `--dump-package` so the bundled `mojo-helpers` plugin runs end-to-end.
#[test]
fn mojo_helper_named_sub_first_param_is_controller() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-cli-helper-named-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let src = "package MyApp;\nuse Mojo::Base 'Mojolicious';\n\nsub startup {\n    my $self = shift;\n    $self->helper(greet => \\&_greet);\n}\n\nsub _greet {\n    my $c = shift;\n    my $x = $c;\n    return $x;\n}\n\n1;\n";
    std::fs::write(lib.join("MyApp.pm"), src).unwrap();

    let mut cache = dir.clone();
    cache.push(".test-cache");

    let out = Command::new(BIN)
        .args(["--dump-package", dir.to_str().unwrap(), "MyApp"])
        .current_dir(&dir)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("run perl-lsp --dump-package");
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");
    let dump: serde_json::Value =
        serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("dump JSON parse ({e}): {stdout}"));

    let subs = dump["subs"].as_array().expect("subs array");
    let greet = subs
        .iter()
        .find(|s| s["name"] == "_greet")
        .expect("_greet sub in dump");
    let c_param = greet["params"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "$c")
        .expect("$c param on _greet");
    assert_eq!(
        c_param["inferred_type"], "Mojolicious::Controller",
        "named-sub helper's $c (my $c = shift) types as the controller"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn positional_definition_renders_engine_coordinates() {
    // Positional input → 0-based/byte output, so the def-name coordinates match
    // the engine point the positional cursor also speaks.
    let dir = std::env::temp_dir().join(format!("perl-lsp-cli-def-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let lib = dir.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let src = "package Greeter;\n\nsub greet { return \"hi\" }\n\ngreet();\n";
    std::fs::write(lib.join("Greeter.pm"), src).unwrap();

    let mut cache = dir.clone();
    cache.push(".test-cache");

    // Goto-def from the call on row 4 (0-based), byte col 0, cursor on `greet`.
    let out = Command::new(BIN)
        .args(["--definition", dir.to_str().unwrap(), "lib/Greeter.pm", "4", "0"])
        .current_dir(&dir)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("run perl-lsp --definition");
    let stdout = String::from_utf8(out.stdout).expect("utf8 stdout");

    // `sub greet` — name `greet` at engine row 2, byte col 4.
    let trimmed = stdout.trim();
    assert!(
        trimmed.ends_with(":2:4"),
        "positional definition should print 0-based/byte 2:4, got {trimmed:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `--plugin-check` lints a document against ANY served language, pack or
/// not. Perl's native builder owns its documents, so the driver declares no
/// pack — the compile gate and the payload findings still hold, and only
/// the served-vocabulary comparison (which needs a pack to name the
/// vocabulary) says it is skipped instead of calling every capture unknown.
#[test]
fn plugin_check_lints_a_document_of_a_pack_less_language() {
    for doc in ["queries/perl/skeleton.scm", "queries/perl/flow.scm"] {
        let out = Command::new(BIN)
            .args(["--plugin-check", doc, "--format", "json"])
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("run --plugin-check");
        assert!(out.status.success(), "{doc}: {}", String::from_utf8_lossy(&out.stderr));
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{doc}: {e}"));
        assert_eq!(v["language"], "perl", "{doc}");
        assert_eq!(v["ok"], true, "{doc}: {v}");
        assert!(v["patterns"].as_u64().unwrap_or(0) > 0, "{doc}: {v}");
        assert_eq!(
            v["vocabulary_checked"], false,
            "{doc}: a language with no pack names no served vocabulary"
        );
    }
}
