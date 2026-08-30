//! A CLI verb indexes only the language families its answer can consult.
//!
//! The asymmetry these tests guard: over-indexing is wasted work, but
//! under-indexing is a WRONG answer and a quiet one — an unattached pack
//! sub-index does not answer empty, `lookup_for` routes that language's
//! queries to the Perl hub instead. So the C++ cases assert a real
//! cross-file answer, not merely a non-empty one.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_perl-lsp");

/// A workspace with both families: a Perl module chain and a C++ class
/// split across header and body, plus a consumer that must resolve across
/// files.
fn mixed_workspace(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("perl-lsp-scope-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("lib/Syn")).unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();

    std::fs::write(
        dir.join("lib/Syn/Alpha.pm"),
        "package Syn::Alpha;\nsub new { bless {}, shift }\nsub alpha_value { 41 }\n1;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("lib/Syn/Beta.pm"),
        "package Syn::Beta;\nuse Syn::Alpha;\nsub beta_call { Syn::Alpha->new->alpha_value }\n1;\n",
    )
    .unwrap();

    std::fs::write(
        dir.join("src/widget.h"),
        "#ifndef WIDGET_H\n#define WIDGET_H\nclass Widget {\npublic:\n  int compute_area();\n  int width;\n};\n#endif\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/widget.cc"),
        "#include \"widget.h\"\nint Widget::compute_area() { return width * 2; }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/consumer.cc"),
        "#include \"widget.h\"\nint run_it() {\n  Widget w;\n  return w.compute_area();\n}\n",
    )
    .unwrap();
    dir
}

fn run(root: &std::path::Path, args: &[&str]) -> (String, String) {
    let mut cache = root.to_path_buf();
    cache.push(".test-cache");
    let out = Command::new(BIN)
        .args(args)
        .current_dir(root)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("run perl-lsp");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[cfg(feature = "cpp")]
#[test]
fn a_cpp_query_still_resolves_cross_file_without_the_perl_tree() {
    // THE trap. Scoping startup must not turn a correct C++ answer into a
    // degraded one — and because an unattached pack index falls back to the
    // Perl hub rather than answering nothing, "non-empty" is not enough:
    // the answer has to name the header AND the body.
    let dir = mixed_workspace("cpp");
    let consumer = dir.join("src/consumer.cc");
    let (stdout, stderr) = run(
        &dir,
        &["--definition", dir.to_str().unwrap(), consumer.to_str().unwrap(), "3", "12"],
    );
    assert!(
        stdout.contains("widget.h"),
        "cross-file C++ goto-def lost the declaration.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("widget.cc"),
        "cross-file C++ goto-def lost the definition.\nstdout: {stdout}\nstderr: {stderr}"
    );
    // And it got there without walking the Perl tree.
    assert!(
        !stderr.contains("Indexed 2 Perl files"),
        "a C++ query indexed the Perl family: {stderr}"
    );
}

#[test]
fn a_perl_query_does_not_index_the_pack_family() {
    // The row this exists for: on a CPAN-shaped tree the XS/C files beside
    // the .pm files are a small minority of the files and a majority of the
    // startup, and no Perl answer can consult them.
    let dir = mixed_workspace("perl");
    let beta = dir.join("lib/Syn/Beta.pm");
    let (stdout, stderr) = run(
        &dir,
        &["--definition", dir.to_str().unwrap(), beta.to_str().unwrap(), "1", "6"],
    );
    assert!(
        stdout.contains("Alpha.pm"),
        "Perl goto-def regressed.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("C/C++ files"),
        "a Perl query indexed the pack family: {stderr}"
    );
}

#[cfg(feature = "cpp")]
#[test]
fn a_whole_workspace_verb_still_sees_every_family() {
    // `workspace/symbol` fans across the hub AND every pack sub-index, so
    // its scope stays All — scoping the positional verbs must not quietly
    // narrow the sweeping ones.
    let dir = mixed_workspace("ws");
    let (stdout, stderr) = run(
        &dir,
        &["--workspace-symbol", dir.to_str().unwrap(), "a"],
    );
    assert!(
        stdout.contains("Widget") || stdout.contains("compute_area"),
        "workspace-symbol lost the pack family.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("Syn::Alpha") || stdout.contains("alpha_value"),
        "workspace-symbol lost the Perl family.\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[cfg(feature = "php")]
fn php_workspace(tag: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("perl-lsp-scope-php-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/Greeter.php"),
        "<?php\nclass Greeter\n{\n    public string $prefix;\n\n    public function greet(string $name): string\n    {\n        return $this->prefix . $name;\n    }\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.php"),
        "<?php\n$g = new Greeter();\necho $g->greet(\"x\");\necho $g->greet(\"y\");\n",
    )
    .unwrap();
    dir
}

/// The round-1 regression class, pinned through the REAL CLI (the unit net
/// missed it because store-level tests pass no index, so no visibility
/// axis ever gated them): a name-keyed pack's cross-file goto-def and
/// references must cross the file boundary.
#[cfg(feature = "php")]
#[test]
fn a_php_query_resolves_cross_file_through_the_ctor_typed_receiver() {
    let dir = php_workspace("gd");
    let main = dir.join("src/main.php");
    // gd on `greet` in `$g->greet("x")` — the receiver types via the
    // structural ctor edge, the method resolves in the OTHER file.
    let (stdout, stderr) = run(
        &dir,
        &["--definition", dir.to_str().unwrap(), main.to_str().unwrap(), "2", "9"],
    );
    assert!(
        stdout.contains("Greeter.php"),
        "cross-file PHP goto-def went dark.\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[cfg(feature = "php")]
#[test]
fn php_references_cross_the_file_boundary() {
    let dir = php_workspace("refs");
    let greeter = dir.join("src/Greeter.php");
    // references on the `greet` DECLARATION (0-based row 5, col 20).
    let (stdout, stderr) = run(
        &dir,
        &["--references", dir.to_str().unwrap(), greeter.to_str().unwrap(), "5", "20"],
    );
    let hits = stdout.matches("main.php").count();
    assert!(
        hits >= 2,
        "expected both call sites in main.php, got {hits}.\nstdout: {stdout}\nstderr: {stderr}"
    );
}
