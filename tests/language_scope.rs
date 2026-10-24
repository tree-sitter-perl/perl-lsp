//! A process serves one language: a file-target verb takes its file's
//! language, a sweeping verb serves `--lang` (Perl without one), and startup
//! indexes only that language. The C++ cases assert a real cross-file
//! answer, not merely a non-empty one.

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
    // Locking to the file's language must not degrade a C++ answer, so the
    // answer has to name the header AND the body, not merely be non-empty.
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
fn a_whole_workspace_verb_serves_the_lang_it_was_given() {
    // A sweep has no file to take a language from: it serves `--lang`, and
    // sees only that language's files.
    let dir = mixed_workspace("ws");
    let (stdout, stderr) = run(
        &dir,
        &["--lang", "cpp", "--workspace-symbol", dir.to_str().unwrap(), "a"],
    );
    assert!(
        stdout.contains("Widget") || stdout.contains("compute_area"),
        "workspace-symbol --lang cpp lost the C++ files.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.contains("Syn::Alpha") && !stdout.contains("alpha_value"),
        "workspace-symbol --lang cpp answered from Perl files.\nstdout: {stdout}\nstderr: {stderr}"
    );

    let (stdout, stderr) = run(&dir, &["--workspace-symbol", dir.to_str().unwrap(), "a"]);
    assert!(
        stdout.contains("Syn::Alpha") || stdout.contains("alpha_value"),
        "workspace-symbol lost the Perl files.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.contains("Widget") && !stdout.contains("compute_area"),
        "workspace-symbol without --lang answered from C++ files.\nstdout: {stdout}\nstderr: {stderr}"
    );
}
