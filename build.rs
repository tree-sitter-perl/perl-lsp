//! Derives the conclusion fingerprint at compile time.
//!
//! A baked conclusion has a staleness class the witness bag does not: editing
//! a reducer changes what the right answer IS while the stored bytes stay
//! perfectly well-formed. Nothing downstream can catch that — the bytes
//! decode, the shape validates, the answer is simply wrong — so the guard has
//! to be a fingerprint that moves on its own when the derivation moves.
//! `docs/prompt-conclusion-layer.md` owns the decision.
//!
//! Two properties this file exists to guarantee, both of which fail silently
//! if broken:
//!
//! 1. **Every hashed file is declared to cargo.** A source edit that does not
//!    re-run this script leaves the constant stale, which is a hand-maintained
//!    version with extra steps and a false sense of safety. `rerun-if-changed`
//!    on a DIRECTORY only watches that directory's own mtime — cargo does not
//!    walk it — so every file is named individually.
//! 2. **The hash is order-independent and rename-sensitive.** Directory
//!    iteration order is not stable, so paths are sorted; and the path is
//!    hashed alongside the bytes, because moving a file between layers changes
//!    behaviour here (`layering_tests`) while leaving byte content identical.

use std::path::{Path, PathBuf};

fn main() {
    let mut files = Vec::new();
    collect(Path::new("src"), &mut files);
    // Dependencies change fold outcomes too — a grammar bump reshapes the CST,
    // which reshapes the witnesses. The lock file is the cheapest complete
    // statement of what we built against.
    if Path::new("Cargo.lock").is_file() {
        files.push(PathBuf::from("Cargo.lock"));
    }
    files.sort();

    let mut acc: u64 = 0xcbf2_9ce4_8422_2325;
    for path in &files {
        // Cargo reruns this script when any declared path changes. Declaring
        // the file we just hashed — rather than a directory above it — is what
        // keeps the constant honest.
        println!("cargo:rerun-if-changed={}", path.display());
        let Ok(bytes) = std::fs::read(path) else { continue };
        // The separator makes the concatenation unambiguous: without it,
        // moving a trailing byte from one file's name into the next file's
        // contents would hash identically.
        fnv(&mut acc, path.to_string_lossy().as_bytes());
        fnv(&mut acc, b"\0");
        fnv(&mut acc, &bytes);
        fnv(&mut acc, b"\0");
    }

    // FNV-1a rather than DefaultHasher: the std hasher's algorithm is
    // explicitly unspecified across releases, and while that would be
    // survivable here (a different toolchain is a legitimately different
    // build), an unspecified hash is a poor thing to hang a cache invariant
    // on when the specified one is four lines.
    println!("cargo:rustc-env=PERL_LSP_CONCLUSION_FINGERPRINT={acc:016x}");
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect(&path, out),
            Ok(t) if t.is_file() => out.push(path),
            _ => {}
        }
    }
}

fn fnv(acc: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *acc ^= *b as u64;
        *acc = acc.wrapping_mul(0x0000_0100_0000_01b3);
    }
}
