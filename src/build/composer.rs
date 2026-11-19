//! php's reader of the dependency map: Composer's static autoload data,
//! read from what Composer wrote (the root `composer.json`,
//! `<vendor-dir>/composer/installed.json` and the classmap `composer
//! dump-autoload` generated). Nothing here runs PHP or Composer. The map
//! is lanes in Composer's own lookup order — the generated classmap, then
//! PSR-4 by the longest prefix, then PSR-0 — plus every package's `files`,
//! with the vendor dir as the tier's root.

use crate::build::dependency_map::{DependencyMap, Lane, ModuleSpelling, Spells};
use crate::model::file_analysis::NameSpellings;
use std::path::{Component, Path, PathBuf};

/// One package's autoload lanes, paths absolute.
#[derive(Debug, Clone, Default)]
struct PackageAutoload {
    root: PathBuf,
    psr_4: Vec<(String, Vec<PathBuf>)>,
    psr_0: Vec<(String, Vec<PathBuf>)>,
    files: Vec<PathBuf>,
}

/// The root package then every installed one, in the order Composer's
/// generator merges them (the root's directories come first under a
/// prefix both declare).
#[derive(Debug, Clone)]
struct ResolvedConfig {
    vendor_dir: PathBuf,
    packages: Vec<PackageAutoload>,
}

impl ResolvedConfig {
    /// The root `composer.json` (its `autoload`, plus `autoload-dev` when
    /// `include_dev`) and every package `installed.json` lists. A missing
    /// or malformed `composer.json` is an error; a missing or malformed
    /// `installed.json` is a project with nothing installed.
    fn load(root: &Path, include_dev: bool) -> Result<Self, String> {
        let manifest = read_json(&root.join("composer.json"))?;
        let vendor_dir = vendor_dir(root, Some(&manifest));
        let mut sections = vec![manifest.get("autoload")];
        if include_dev {
            sections.push(manifest.get("autoload-dev"));
        }
        let mut packages = vec![package(root, &sections)];
        // Composer never reads a dependency's `autoload-dev`.
        if let Ok(installed) = read_json(&installed_json(&vendor_dir)) {
            // v2 wraps the list as `{"packages": [...]}`; v1 is the bare list.
            let listed = installed.get("packages").unwrap_or(&installed);
            for p in listed.as_array().into_iter().flatten() {
                let Some(name) = p.get("name").and_then(|n| n.as_str()) else { continue };
                // v2 records where it installed each package, relative to
                // `vendor/composer`; v1 always installed to `vendor/<name>`.
                let pkg_root = match p.get("install-path").and_then(|i| i.as_str()) {
                    Some(rel) => normalize(&vendor_dir.join("composer"), rel),
                    None => vendor_dir.join(name),
                };
                packages.push(package(&pkg_root, &[p.get("autoload")]));
            }
        }
        Ok(ResolvedConfig { vendor_dir, packages })
    }
}

/// `config.vendor-dir`, else `vendor`, under `root`.
fn vendor_dir(root: &Path, manifest: Option<&serde_json::Value>) -> PathBuf {
    let rel = manifest
        .and_then(|m| m.get("config"))
        .and_then(|c| c.get("vendor-dir"))
        .and_then(|v| v.as_str())
        .unwrap_or("vendor");
    normalize(root, rel)
}

fn installed_json(vendor_dir: &Path) -> PathBuf {
    vendor_dir.join("composer").join("installed.json")
}

/// The class → file map `composer dump-autoload` writes. It already applies
/// every package's `classmap` and `exclude-from-classmap` (and, optimized,
/// every PSR class too), so reading it is exact where scanning would not
/// be, and the reader applies neither rule again.
fn autoload_classmap(vendor_dir: &Path) -> PathBuf {
    vendor_dir.join("composer").join("autoload_classmap.php")
}

fn read_json(path: &Path) -> Result<serde_json::Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn package(root: &Path, sections: &[Option<&serde_json::Value>]) -> PackageAutoload {
    let mut out = PackageAutoload { root: root.to_path_buf(), ..Default::default() };
    for sec in sections.iter().flatten() {
        prefix_map(sec.get("psr-4"), root, &mut out.psr_4);
        prefix_map(sec.get("psr-0"), root, &mut out.psr_0);
        out.files.extend(strings(sec.get("files")).map(|p| normalize(root, p)));
    }
    out
}

/// A `{prefix: path | [paths]}` object, merged into `out` by prefix.
fn prefix_map(v: Option<&serde_json::Value>, root: &Path, out: &mut Vec<(String, Vec<PathBuf>)>) {
    let Some(obj) = v.and_then(|v| v.as_object()) else { return };
    for (prefix, paths) in obj {
        let dirs: Vec<PathBuf> = match paths {
            serde_json::Value::String(s) => vec![normalize(root, s)],
            other => strings(Some(other)).map(|p| normalize(root, p)).collect(),
        };
        merge(out, prefix.clone(), dirs);
    }
}

fn merge(out: &mut Vec<(String, Vec<PathBuf>)>, prefix: String, dirs: Vec<PathBuf>) {
    match out.iter_mut().find(|(p, _)| *p == prefix) {
        Some((_, existing)) => existing.extend(dirs),
        None => out.push((prefix, dirs)),
    }
}

fn strings(v: Option<&serde_json::Value>) -> impl Iterator<Item = &str> {
    v.and_then(|v| v.as_array()).into_iter().flatten().filter_map(|s| s.as_str())
}

/// `base` joined with `rel`, `.` and `..` resolved lexically — never
/// through the filesystem, so a path-repository symlink stays under
/// `vendor/`.
fn normalize(base: &Path, rel: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for c in base.join(rel).components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// PHP's spellings, as its pack declares them: the separator a PSR rule
/// turns into a directory.
fn names() -> &'static NameSpellings {
    static NAMES: std::sync::OnceLock<NameSpellings> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| crate::build::packs::php_pack().names)
}

fn sep() -> &'static str {
    names().sep().unwrap_or_default()
}

/// PSR-4: the name past the prefix, each separator a directory.
const PSR_4: ModuleSpelling = ModuleSpelling {
    module_paths: |rest| vec![format!("{}.php", rest.replace(sep(), "/"))],
    module_of: |rel| from_rel(rel),
};

/// PSR-0: the whole name, and a `_` in the class leaf (not the namespace)
/// is a directory too. Asked by path, a file names its namespaced
/// spelling: the `_` a directory stood for is not recoverable.
const PSR_0: ModuleSpelling = ModuleSpelling {
    module_paths: |name| {
        let (ns, leaf) = crate::model::conventions::split_qualified(name, names());
        let dir = ns.map(|ns| format!("{}/", ns.replace(sep(), "/"))).unwrap_or_default();
        vec![format!("{dir}{}.php", leaf.replace('_', "/"))]
    },
    module_of: |rel| from_rel(rel),
};

fn from_rel(rel: &Path) -> Option<String> {
    if rel.extension().is_none_or(|e| e != "php") {
        return None;
    }
    let parts: Vec<String> =
        rel.with_extension("").components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    Some(parts.join(sep()))
}

/// The map for a project at `root`; `None` with no project open. Always a
/// map with a project, even with no `composer.json`: it then places
/// nothing, and its inputs are still the files whose appearance would
/// change that. The generated classmap is read here, once per map.
pub fn read_map(root: Option<&Path>) -> Option<DependencyMap> {
    let root = root?;
    let config = crate::util::timings::phase("composer::manifests", || ResolvedConfig::load(root, true));
    let vendor_dir = match &config {
        Ok(c) => c.vendor_dir.clone(),
        Err(_) => vendor_dir(root, None),
    };
    let inputs = vec![root.join("composer.json"), installed_json(&vendor_dir), autoload_classmap(&vendor_dir)];
    let config = match config {
        Ok(c) => c,
        Err(e) => {
            log::debug!("composer: no autoload map ({e})");
            return Some(DependencyMap::new(Vec::new(), Vec::new(), inputs));
        }
    };
    let classmap = crate::util::timings::phase("composer::classmap", || {
        let path = autoload_classmap(&vendor_dir);
        // No generated classmap (`dump-autoload` never ran) is an empty
        // table, not a scan of the packages' classmap directories: Composer
        // itself would load none of those classes.
        let Ok(source) = crate::util::text::read_source(&path) else { return Vec::new() };
        let root_dir = config.packages.first().map(|p| p.root.as_path()).unwrap_or(root);
        read_classmap(CLASSMAP_DOCUMENT, &source, &vendor_dir, root_dir).unwrap_or_else(|e| {
            log::error!("composer: {}: {e}", path.display());
            Vec::new()
        })
    });
    let mut psr_4: Vec<(String, Vec<PathBuf>)> = Vec::new();
    let mut psr_0: Vec<(String, Vec<PathBuf>)> = Vec::new();
    let mut files: Vec<PathBuf> = Vec::new();
    for p in config.packages {
        for (prefix, dirs) in p.psr_4 {
            merge(&mut psr_4, prefix, dirs);
        }
        for (prefix, dirs) in p.psr_0 {
            merge(&mut psr_0, prefix, dirs);
        }
        files.extend(p.files);
    }
    // Longest prefix first, as Composer's loader tries them; stable, so a
    // tie keeps merge order. The empty fallback prefix sorts last.
    psr_4.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    psr_0.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
    let mut lanes = vec![Lane::Table(classmap)];
    lanes.extend(
        psr_4
            .into_iter()
            .map(|(prefix, dirs)| Lane::Prefix { prefix, dirs, spelling: PSR_4, spells: Spells::Remainder }),
    );
    lanes.extend(
        psr_0
            .into_iter()
            .map(|(prefix, dirs)| Lane::Prefix { prefix, dirs, spelling: PSR_0, spells: Spells::WholeName }),
    );
    lanes.push(Lane::AlwaysLoad(files));
    Some(crate::util::timings::phase("composer::assemble", || DependencyMap::new(lanes, vec![vendor_dir], inputs)))
}

/// The classmap Composer generates is read through this document.
const CLASSMAP_DOCUMENT: &str = include_str!("../../queries/php/composer-classmap.scm");

/// The generated classmap's entries through `document`, in file order. An
/// entry whose path is spelled any other way than the document admits is
/// skipped, never guessed at; a document whose captures the classmap
/// vocabulary does not hold is an error.
fn read_classmap(
    document: &str,
    source: &str,
    vendor_dir: &Path,
    base_dir: &Path,
) -> Result<Vec<(String, PathBuf)>, String> {
    use crate::build::query_extract::{document_matches, ClassmapCap};
    let php = crate::build::language_driver::LanguageRegistry::compiled().for_id("php").ok_or("no php driver")?;
    let mut rows = Vec::new();
    for m in document_matches::<ClassmapCap>(php, document, source)? {
        let (mut class, mut path) = (None, None);
        for (cap, text) in m {
            let quoted = || single_quoted(text);
            match cap {
                ClassmapCap::Class => class = quoted(),
                ClassmapCap::Anchor => {}
                ClassmapCap::VendorPath => path = quoted().map(|t| normalize(vendor_dir, t.trim_start_matches('/'))),
                ClassmapCap::BasePath => path = quoted().map(|t| normalize(base_dir, t.trim_start_matches('/'))),
                ClassmapCap::AbsolutePath => path = quoted().map(|t| normalize(Path::new("/"), &t)),
            }
        }
        if let (Some(class), Some(path)) = (class, path) {
            rows.push((class, path));
        }
    }
    Ok(rows)
}

/// A single-quoted PHP string's value: `\\` and `\'` are its only escapes.
/// `None` for any other spelling, which the generated file never writes.
fn single_quoted(text: &str) -> Option<String> {
    let inner = text.strip_prefix('\'')?.strip_suffix('\'')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => match chars.next() {
                Some(e @ ('\\' | '\'')) => out.push(e),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            },
            c => out.push(c),
        }
    }
    Some(out)
}

#[cfg(test)]
#[path = "composer_tests.rs"]
mod tests;
