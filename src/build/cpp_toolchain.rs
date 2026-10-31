//! The zero-config C/C++ toolchain probe.
//!
//! `docs/cpp-stdlib-autoconfig-research.md` L2 — the load-bearing
//! layer. The `<...>` macro-gather in `cpp_reparse.rs` skips system
//! headers because it doesn't know where the stdlib lives; the only
//! portable, build-free source of that truth is the compiler itself.
//! Three one-shot invocations answer it, milliseconds each, stable per
//! toolchain, identical markers across gcc and clang on Linux + macOS:
//!
//!   1. `cc -x<lang> -E -v -`   → stderr's `#include <...>` search block
//!                                = ordered system/stdlib include roots.
//!   2. `cc -x<lang> -dM -E -`  → stdout's every predefined `#define`
//!                                = the EXTERNAL macro seed the gather's
//!                                  `#if` arms need (`__GNUC__`,
//!                                  `__cplusplus`, `_WIN32`, widths, …).
//!   3. `cc -print-resource-dir`→ clang's compiler-owned built-ins
//!                                (`stddef.h`, `stdarg.h`, intrinsics);
//!                                gcc has no such flag — those dirs
//!                                already surface in (1).
//!
//! Consumed through `Toolchain::info`, one probe per toolchain per process:
//! a unit's `include_dirs` are the toolchain roots `cpp_includes::resolve`
//! searches for it, and its `predefined_macros` seed the reachability config
//! (`unit_config`). This module is the self-contained, tested
//! producer; `cpp_toolchain_tests.rs` exercises the parsers on captured
//! compiler output so the tests never depend on the host's compiler.
//!
//! Caching: results are stable per (compiler abs-path, version, `-std`,
//! `--target`, lang) — a toolchain UPGRADE must invalidate this, a
//! project edit must NOT — so it lives in a process-local map keyed on
//! toolchain identity, separate from the per-project module cache. The
//! version rides the cached value so the persisted-blob follow-up (the
//! zstd machinery `PersistedMacros` already uses) can key its on-disk
//! entry on (abs-path, version, std, target) without a per-hit spawn.
//! MSVC's env-diff path (`vcvarsall.bat` → `INCLUDE`) is a follow-up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

pub use crate::model::toolchain::{Lang, Toolchain};

impl Lang {
    /// The `-x` argument selecting the compile language.
    fn x_arg(self) -> &'static str {
        match self {
            Lang::C => "-xc",
            Lang::Cpp => "-xc++",
        }
    }

    /// The language a compiler driver gives a SOURCE file by its extension
    /// (gcc's and clang's rule); `None` for anything else — a header is no
    /// unit, and takes its language from the source it is compiled with.
    pub fn of_source(path: &Path) -> Option<Lang> {
        match path.extension()?.to_str()? {
            "c" => Some(Lang::C),
            "cc" | "cpp" | "cxx" | "c++" | "cp" | "CPP" | "C" => Some(Lang::Cpp),
            _ => None,
        }
    }

    /// The language a `-x` value names, if it names C or C++.
    fn of_x(x: &str) -> Option<Lang> {
        match x {
            "c" | "c-header" | "cpp-output" => Some(Lang::C),
            "c++" | "c++-header" | "c++-cpp-output" => Some(Lang::Cpp),
            _ => None,
        }
    }
}

impl Toolchain {
    /// The toolchain a compile command line names for `file`: its driver,
    /// `-x` when given, else the driver's own language (a `++` driver
    /// compiles every source as C++), else the file's; and its `-std=`.
    pub fn from_command(args: &[String], file: &Path) -> Toolchain {
        let compiler = args.first().filter(|c| !c.starts_with('-')).cloned();
        let mut x = None;
        let mut std = None;
        let mut i = 1;
        while i < args.len() {
            let a = args[i].as_str();
            if let Some(v) = a.strip_prefix("-std=") {
                std = Some(v.to_string());
            } else if a == "-x" {
                x = args.get(i + 1).and_then(|v| Lang::of_x(v));
                i += 1;
            } else if let Some(v) = a.strip_prefix("-x") {
                x = Lang::of_x(v);
            }
            i += 1;
        }
        let driver_lang = compiler
            .as_deref()
            .and_then(|c| Path::new(c).file_name()?.to_str())
            .filter(|n| n.ends_with("++"))
            .map(|_| Lang::Cpp);
        let lang = x.or(driver_lang).or_else(|| Lang::of_source(file)).unwrap_or(Lang::Cpp);
        Toolchain::new(compiler, lang, std)
    }

    /// The probe's answer for this toolchain, probed at most once per
    /// process. A unit's own driver that will not probe (gone, or a wrapper
    /// that won't answer `-E -v`) falls back to its language's default
    /// driver, so a stale database still resolves system headers.
    pub fn info(&self) -> Option<std::sync::Arc<ToolchainInfo>> {
        static MEMO: OnceLock<Mutex<HashMap<Toolchain, Once<Option<std::sync::Arc<ToolchainInfo>>>>>> = OnceLock::new();
        let slot = once_slot(MEMO.get_or_init(Default::default), self);
        slot.get_or_init(|| {
            let probed = |compiler: &str| probe_full(compiler, self.lang(), self.std(), None);
            self.compiler()
                .and_then(probed)
                .or_else(|| default_compiler(self.lang()).and_then(|c| probed(&c)))
                .map(std::sync::Arc::new)
        })
        .clone()
    }

    /// The identity of what the probe answered: compiler version, include
    /// roots and predefined macros, or a distinct sentinel when it failed —
    /// so a degraded generation never validates once the toolchain answers.
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        match self.info() {
            Some(t) => {
                t.compiler_version.hash(&mut h);
                t.include_dirs.hash(&mut h);
                t.predefined_macros.hash(&mut h);
            }
            None => ("no-toolchain", self.lang()).hash(&mut h),
        }
        h.finish()
    }
}

/// One toolchain's discovered stdlib surface. `include_dirs` is in
/// compiler search order (order is load-bearing — earlier wins) and
/// already includes `<resource_dir>/include` when that exists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolchainInfo {
    /// System/stdlib include roots, in search order, deduped.
    pub include_dirs: Vec<PathBuf>,
    /// Every compiler-baked `#define`, as `(name, value)`; `value` is
    /// `""` for a bare `#define FOO`. Function-like macros keep their
    /// parameter list in `name` (`__has_feature(x)`).
    pub predefined_macros: Vec<(String, String)>,
    /// clang's `-print-resource-dir` (compiler-owned built-in headers).
    /// `None` for gcc, which has no such flag.
    pub resource_dir: Option<PathBuf>,
    /// First `--version` line — the toolchain identity component that a
    /// same-path upgrade changes; keys the persisted-blob follow-up.
    pub compiler_version: String,
}

/// The cache identity. `compiler` is the canonical absolute path when
/// resolvable (so `cc` and `/usr/bin/gcc` don't alias); `version` lives
/// on the cached value, recoverable with the other three for the
/// persisted key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ToolchainKey {
    compiler: PathBuf,
    std: Option<String>,
    target: Option<String>,
    lang: Lang,
}

/// A memo slot computed once: concurrent first askers (the parallel landing
/// frontier, the workspace walk) wait on the one probe instead of each
/// shelling out.
type Once<V> = std::sync::Arc<OnceLock<V>>;

/// `key`'s slot in `memo`, made on first ask. The map's lock is held only
/// to find the slot, never across the probe, so toolchains probe
/// independently.
fn once_slot<K: Clone + Eq + std::hash::Hash, V>(memo: &Mutex<HashMap<K, Once<V>>>, key: &K) -> Once<V> {
    let mut m = memo.lock().unwrap_or_else(|e| e.into_inner());
    std::sync::Arc::clone(m.entry(key.clone()).or_default())
}

fn cache() -> &'static Mutex<HashMap<ToolchainKey, Once<Option<ToolchainInfo>>>> {
    static C: OnceLock<Mutex<HashMap<ToolchainKey, Once<Option<ToolchainInfo>>>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Probe the default C++ driver's stdlib surface, cached. `std` is an
/// optional `-std=` value (e.g. `c++20`) forwarded so `__cplusplus` and
/// the `c++/<ver>` roots match the project. Returns `None` when no
/// compiler is on PATH — never panics; a machine may have none.
#[cfg(test)]
pub fn probe(compiler: &str, std: Option<&str>) -> Option<ToolchainInfo> {
    probe_full(compiler, Lang::Cpp, std, None)
}

/// Full-control probe: pick the language and forward an optional
/// `--target=` triple. Cached per (abs-path, version, std, target,
/// lang). Absence is cached too, so a missing compiler is probed once.
pub fn probe_full(
    compiler: &str,
    lang: Lang,
    std: Option<&str>,
    target: Option<&str>,
) -> Option<ToolchainInfo> {
    // Resolve to an absolute path so the key is toolchain identity, not
    // a PATH-relative alias. An unresolved name still keys (and probes)
    // by its raw spelling — `execvp` will fail cheaply and cache None.
    let resolved = resolve_compiler(compiler);
    let key = ToolchainKey {
        compiler: resolved.clone().unwrap_or_else(|| PathBuf::from(compiler)),
        std: std.map(str::to_string),
        target: target.map(str::to_string),
        lang,
    };

    let slot = once_slot(cache(), &key);
    slot.get_or_init(|| run_probe(&key.compiler.to_string_lossy(), lang, std, target)).clone()
}

/// The default driver for a language, honoring `$CC` / `$CXX` first,
/// then the conventional names in preference order. Returns the name to
/// pass to `probe*` (resolution to an abs path happens inside).
pub fn default_compiler(lang: Lang) -> Option<String> {
    let (env_var, candidates): (&str, &[&str]) = match lang {
        Lang::Cpp => ("CXX", &["c++", "clang++", "g++", "cc", "clang", "gcc"]),
        Lang::C => ("CC", &["cc", "clang", "gcc"]),
    };
    if let Ok(v) = std::env::var(env_var) {
        let v = v.trim();
        if !v.is_empty() {
            return Some(v.to_string());
        }
    }
    // `$CC` is the traditional override even for a C++ build's driver
    // discovery when `$CXX` is unset — check it as a secondary.
    if lang == Lang::Cpp {
        if let Ok(v) = std::env::var("CC") {
            let v = v.trim();
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    candidates
        .iter()
        .find(|c| resolve_compiler(c).is_some())
        .map(|c| c.to_string())
}

/// The three shell-outs, assembled. `None` iff the driver won't spawn
/// (no such compiler) — an empty include list from a driver that DID
/// run still yields `Some` (graceful, honest degradation).
fn run_probe(
    driver: &str,
    lang: Lang,
    std: Option<&str>,
    target: Option<&str>,
) -> Option<ToolchainInfo> {
    // `--version` doubles as the presence check and the identity stamp.
    let version = capture(driver, &["--version".to_string()])
        .map(|(out, _)| first_line(&out))?;

    let mut common: Vec<String> = vec![lang.x_arg().to_string()];
    if let Some(s) = std {
        common.push(format!("-std={s}"));
    }
    if let Some(t) = target {
        common.push(format!("--target={t}"));
    }

    // (1) system include roots — parse STDERR of `-E -v -` (empty stdin).
    let mut ev = common.clone();
    ev.extend(["-E".to_string(), "-v".to_string(), "-".to_string()]);
    let mut include_dirs = capture(driver, &ev)
        .map(|(_, err)| parse_include_search_dirs(&err))
        .unwrap_or_default();

    // (2) predefined macros — parse STDOUT of `-dM -E -`.
    let mut dm = common.clone();
    dm.extend(["-dM".to_string(), "-E".to_string(), "-".to_string()]);
    let predefined_macros = capture(driver, &dm)
        .map(|(out, _)| parse_predefined_macros(&out))
        .unwrap_or_default();

    // (3) clang resource dir — its `<dir>/include` holds compiler-owned
    // headers not in `/usr/include`. gcc lacks the flag (returns None).
    let resource_dir = capture(driver, &["-print-resource-dir".to_string()])
        .map(|(out, _)| first_line(&out))
        .map(PathBuf::from)
        .filter(|p| p.as_os_str().len() > 0 && p.is_dir());
    if let Some(rd) = &resource_dir {
        let builtin = rd.join("include");
        if builtin.is_dir() && !include_dirs.contains(&builtin) {
            include_dirs.push(builtin);
        }
    }

    Some(ToolchainInfo { include_dirs, predefined_macros, resource_dir, compiler_version: version })
}

/// Run `driver args…` with empty stdin; return `(stdout, stderr)` on a
/// successful spawn, `None` if the process couldn't start. A non-zero
/// exit is NOT an error here — `-print-resource-dir` on gcc exits
/// non-zero yet we still want the (empty) capture handled by the
/// caller's parse; what matters is whether `execvp` found the binary.
fn capture(driver: &str, args: &[String]) -> Option<(String, String)> {
    let out = output_with_transient_retry(|| {
        Command::new(driver)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
    })?;
    Some((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

/// Spawn with retry on the TRANSIENT failure class (EAGAIN/ENOMEM under
/// load); `NotFound` stays permanent — a machine may have no compiler,
/// and that answer is probed once and cached. The stakes are bigger than
/// one degraded session: `toolchain_fingerprint` folds "probe failed"
/// into the identity that `validate_input_fingerprint` stamps on the
/// pack modules DB, so a single failed spawn hard-clears a HEALTHY
/// cached generation, restamps the degraded identity, and guarantees a
/// second full wipe when the toolchain answers again — plus a clobber
/// window for any concurrently-serving process whose warm-registered
/// skeletons just lost their blobs (strict residency scores that as a
/// crash). Same rationale as the pd-combine spawn retry.
fn output_with_transient_retry(
    mut spawn: impl FnMut() -> std::io::Result<std::process::Output>,
) -> Option<std::process::Output> {
    let mut delay = std::time::Duration::from_millis(20);
    for attempt in 0..3 {
        match spawn() {
            Ok(out) => return Some(out),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(_) if attempt < 2 => {
                crate::util::ghost_stats::count("toolchain.probe.spawn_retried");
                std::thread::sleep(delay);
                delay *= 5;
            }
            Err(_) => {
                crate::util::ghost_stats::count("toolchain.probe.spawn_failed");
                return None;
            }
        }
    }
    None
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").trim().to_string()
}

/// Slice the `#include <...> search starts here:` … `End of search
/// list.` block from `-E -v` stderr. gcc and clang emit identical
/// markers. Preserves order, trims leading space, strips clang's
/// macOS ` (framework directory)` suffix, dedups. The preceding
/// `#include "..."` (quote) block is intentionally excluded — we want
/// the angle-bracket/system roots.
pub fn parse_include_search_dirs(stderr: &str) -> Vec<PathBuf> {
    const START: &str = "#include <...> search starts here:";
    const END: &str = "End of search list.";
    let mut out: Vec<PathBuf> = Vec::new();
    let mut in_block = false;
    for line in stderr.lines() {
        let trimmed = line.trim();
        if !in_block {
            if trimmed == START {
                in_block = true;
            }
            continue;
        }
        if trimmed.starts_with(END) {
            break;
        }
        if trimmed.is_empty() {
            continue;
        }
        // clang tags framework roots; keep the path, drop the tag.
        let path = trimmed
            .strip_suffix(" (framework directory)")
            .unwrap_or(trimmed);
        let pb = PathBuf::from(path);
        if !out.contains(&pb) {
            out.push(pb);
        }
    }
    out
}

/// Split every `#define NAME VALUE` line from `-dM -E` stdout into
/// `(name, value)`. `NAME` is the token after `#define ` (function-like
/// macros keep their `(params)`); `VALUE` is the rest, or `""` for a
/// bare `#define NAME`. Order is preserved.
pub fn parse_predefined_macros(stdout: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in stdout.lines() {
        let Some(rest) = line.trim().strip_prefix("#define ") else {
            continue;
        };
        let rest = rest.trim_start();
        match rest.split_once(char::is_whitespace) {
            Some((name, value)) => out.push((name.to_string(), value.trim().to_string())),
            None => out.push((rest.to_string(), String::new())),
        }
    }
    out
}

/// Resolve a compiler name to a canonical absolute path: an explicit
/// path is canonicalized; a bare name is searched on `$PATH`. `None`
/// when nothing executable matches.
fn resolve_compiler(name: &str) -> Option<PathBuf> {
    let p = Path::new(name);
    if p.components().count() > 1 || p.is_absolute() {
        return p.canonicalize().ok().filter(|c| c.is_file());
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            if let Ok(abs) = cand.canonicalize() {
                return Some(abs);
            }
        }
    }
    None
}

#[cfg(test)]
#[path = "cpp_toolchain_tests.rs"]
mod tests;
