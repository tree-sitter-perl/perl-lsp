//! The across-build equivalence dump: every analysis this process builds,
//! written as its canonical projection (`FileAnalysis::equivalence_dump`), so
//! a base binary and a head binary run over the same inputs can be diffed.
//!
//! On under `PERL_LSP_EQUIV_DUMP_DIR=<dir>` (so `cargo test` dumps every input
//! the suite builds) or `--equiv-dump <dir>`. `scripts/equiv/` drives it; the
//! protocol is in its README.
//!
//! Files are named `<input>.<content>.json`. `<input>` hashes what the build
//! is a function of (language, source, the module a path-named file is), never
//! the path itself, so a test's temp dir pairs up across runs. One input built
//! two ways in one run (a test's own plugin set, a pack with and without its
//! include context) leaves one file per distinct result rather than a
//! last-write-wins race, so the SET of names per input is what compares.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::model::file_analysis::FileAnalysis;

static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Turn the dump on for this process (the `--equiv-dump` verb). Must run
/// before the first build; a later call, or one after the env var already
/// chose, is refused.
pub fn enable(dir: PathBuf) -> Result<(), String> {
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    DIR.set(Some(dir)).map_err(|_| "equivalence dump already configured".to_string())
}

fn dir() -> Option<&'static PathBuf> {
    DIR.get_or_init(|| {
        let d = std::env::var_os("PERL_LSP_EQUIV_DUMP_DIR").map(PathBuf::from)?;
        std::fs::create_dir_all(&d).ok()?;
        Some(d)
    })
    .as_ref()
}

/// FNV-1a: stable across processes and toolchains, unlike `DefaultHasher`.
fn fnv(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for &b in *p {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        // Separator so ("ab","c") and ("a","bc") differ.
        h ^= 0xff;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Record `fa`, the analysis of `source`. `module` is the module a
/// path-named file is (packs), the one path-derived build input.
pub(crate) fn record(source: &[u8], module: Option<&str>, fa: &FileAnalysis) {
    let Some(dir) = dir() else { return };
    let input = fnv(&[fa.language.as_bytes(), source, module.unwrap_or("").as_bytes()]);
    // Compact: the dumps of a substrate run are ~1 GB pretty-printed; the
    // differ re-indents only the inputs it explains.
    let body = serde_json::to_string(&fa.equivalence_dump()).expect("dump serializes");
    let content = fnv(&[body.as_bytes()]);
    let name = format!("{input:016x}.{content:016x}");
    let json = dir.join(format!("{name}.json"));
    if json.exists() {
        return;
    }
    // The source rides beside its dumps so a changed input can be read and
    // re-run without knowing which test or file produced it.
    let src = dir.join(format!("{input:016x}.src"));
    if !src.exists() {
        write_atomic(&src, source);
    }
    // Where it came from, when the caller said: never part of the name.
    if let Some(file) = crate::util::timings::current_file() {
        write_atomic(&dir.join(format!("{input:016x}.where")), file.as_bytes());
    }
    write_atomic(&json, body.as_bytes());
}

/// Concurrent writers of one name write identical bytes; rename makes the
/// file appear whole or not at all.
fn write_atomic(path: &std::path::Path, bytes: &[u8]) {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp.{}.{n}", std::process::id()));
    if std::fs::write(&tmp, bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

#[cfg(test)]
mod tests {
    use crate::build::language_driver::LanguageRegistry;
    use crate::model::file_analysis::FileAnalysis;

    /// Every repo fixture the served-language drivers claim.
    fn fixtures() -> Vec<std::path::PathBuf> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut out = Vec::new();
        for dir in ["test_files", "gold-corpus"] {
            for e in ignore::WalkBuilder::new(root.join(dir)).build().flatten() {
                let p = e.path();
                if e.file_type().is_some_and(|t| t.is_file())
                    && LanguageRegistry::compiled().for_path(p).is_some()
                {
                    out.push(p.to_path_buf());
                }
            }
        }
        out.sort();
        out
    }

    /// The dump must not depend on hash order. A bincode round trip gives
    /// every `HashMap`/`HashSet` a fresh `RandomState`, so an unlisted set
    /// (`SET_FIELDS`) shows up here as a reordered array — and the
    /// across-build diff would otherwise report it as a change.
    #[test]
    fn canonical_dump_survives_a_rehash() {
        let files = fixtures();
        assert!(files.len() > 20, "fixture walk found {} files", files.len());
        for path in files {
            let Ok(src) = std::fs::read_to_string(&path) else { continue };
            let driver = LanguageRegistry::compiled().for_path(&path).unwrap();
            let fa = driver.analyze_with_path(&src, Some(&path));
            let want = fa.equivalence_dump();
            let mut cur = fa;
            for round in 0..3 {
                let bytes = bincode::serialize(&cur).expect("serialize");
                cur = bincode::deserialize::<FileAnalysis>(&bytes).expect("deserialize");
                let got = cur.equivalence_dump();
                if got != want {
                    let (a, b) = (want["analysis"].as_object().unwrap(), got["analysis"].as_object().unwrap());
                    let fields: Vec<_> = a.keys().filter(|k| a.get(*k) != b.get(*k)).collect();
                    panic!(
                        "{}: dump changed after rehash round {round}; differing analysis fields {fields:?} \
                         (answers equal: {}) — a HashSet missing from SET_FIELDS?",
                        path.display(),
                        want["answers"] == got["answers"],
                    );
                }
            }
        }
    }
}
