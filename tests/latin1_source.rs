//! A source file that isn't UTF-8 (FHEM ships Latin-1 modules) is read as
//! Latin-1: indexed, answerable, and never the end of a `--batch` run.

use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_perl-lsp");

/// `package Old; sub caf { 'café' }` with the é as the single Latin-1 byte
/// 0xE9, then a UTF-8 caller in a second file.
fn workspace(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("perl-lsp-latin1-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    let mut old = b"package Old;\nsub caf { 'caf".to_vec();
    old.push(0xE9);
    old.extend_from_slice(b"' }\n1;\n");
    std::fs::write(dir.join("lib/Old.pm"), old).unwrap();
    std::fs::write(dir.join("lib/New.pm"), "package New;\nuse Old;\nOld::caf();\n1;\n").unwrap();
    dir
}

#[test]
fn batch_answers_every_row_past_a_latin1_file() {
    let dir = workspace("batch");
    let mut child = Command::new(BIN)
        .args(["--batch", dir.to_str().unwrap()])
        .env("XDG_CACHE_HOME", dir.join(".test-cache"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        let old = dir.join("lib/Old.pm");
        let new = dir.join("lib/New.pm");
        writeln!(stdin, r#"{{"id":"a","q":"definition","file":"{}","line":2,"col":5}}"#, new.display()).unwrap();
        writeln!(stdin, r#"{{"id":"b","q":"outline","file":"{}"}}"#, old.display()).unwrap();
        writeln!(stdin, r#"{{"id":"c","q":"definition","file":"{}","line":2,"col":5}}"#, new.display()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8(out.stdout).unwrap();
    let rows: Vec<serde_json::Value> =
        stdout.lines().map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l}"))).collect();
    assert_eq!(rows.len(), 3, "one answer per request: {stdout}");
    assert_eq!(rows[1]["ok"], true, "the Latin-1 file answers: {stdout}");
    assert!(rows[1]["out"].as_str().unwrap().contains("caf"), "{stdout}");
    // The cross-file definition lands in the Latin-1 file: it was indexed.
    assert!(rows[0]["out"].as_str().unwrap_or_default().contains("Old.pm"), "{stdout}");
    std::fs::remove_dir_all(&dir).ok();
}
