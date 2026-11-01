//! Composer's static autoload data: which file declares a PHP class
//! identity the workspace does not.
//!
//! The ONE seam the index's dependency tier asks: [`locator`] builds a
//! [`DependencyLocator`] from `composer.json`,
//! `<vendor-dir>/composer/installed.json` and the classmap Composer
//! generated beside it, and the index never sees Composer. Nothing here
//! runs PHP or Composer. [`ResolvedConfig`] mirrors
//! `composr`'s `config::ResolvedConfig` field for field, so that crate's
//! `ResolvedConfig::load` can replace ours without touching the locator or
//! the tier plumbing.

use crate::build::language_driver::{Declared, DependencyLocator, LanguageDriver, Located, LocatedBy};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// One package's autoload contribution, paths absolute. Field for field
/// `composr`'s; the locator reads the autoload lanes, the rest is the shape.
#[allow(dead_code)]
#[derive(Debug, Clone, Default)]
pub struct PackageAutoload {
    pub name: String,
    pub root: PathBuf,
    pub psr_4: Vec<(String, Vec<PathBuf>)>,
    pub psr_0: Vec<(String, Vec<PathBuf>)>,
    pub classmap: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
    pub exclude_from_classmap: Vec<String>,
    pub target_dir: Option<String>,
    pub reference: Option<String>,
}

/// The root package plus every installed package, as Composer would
/// autoload them.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ResolvedConfig {
    pub root: PathBuf,
    pub vendor_dir: PathBuf,
    pub packages: Vec<PackageAutoload>,
}

impl ResolvedConfig {
    /// The root `composer.json` (its `autoload`, plus `autoload-dev` when
    /// `include_dev`) and every package `installed.json` lists. A missing
    /// or malformed `composer.json` is an error; a missing or malformed
    /// `installed.json` is a project with nothing installed.
    pub fn load(root: &Path, include_dev: bool) -> Result<Self, String> {
        let composer_json = root.join("composer.json");
        let manifest = read_json(&composer_json)?;
        let vendor_dir = normalize(
            root,
            manifest
                .get("config")
                .and_then(|c| c.get("vendor-dir"))
                .and_then(|v| v.as_str())
                .unwrap_or("vendor"),
        );
        let mut sections = vec![manifest.get("autoload")];
        if include_dev {
            sections.push(manifest.get("autoload-dev"));
        }
        let mut packages = vec![package(&manifest, root, &sections, None)];
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
                packages.push(package(p, &pkg_root, &[p.get("autoload")], Some(name)));
            }
        }
        Ok(ResolvedConfig { root: root.to_path_buf(), vendor_dir, packages })
    }
}

fn installed_json(vendor_dir: &Path) -> PathBuf {
    vendor_dir.join("composer").join("installed.json")
}

/// The class → file map `composer dump-autoload` writes. It already applies
/// every package's `classmap` and `exclude-from-classmap` (and, optimized,
/// every PSR class too), so reading it is exact where scanning was not.
fn autoload_classmap(vendor_dir: &Path) -> PathBuf {
    vendor_dir.join("composer").join("autoload_classmap.php")
}

fn read_json(path: &Path) -> Result<serde_json::Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn package(
    json: &serde_json::Value,
    root: &Path,
    sections: &[Option<&serde_json::Value>],
    name: Option<&str>,
) -> PackageAutoload {
    let mut out = PackageAutoload {
        name: name
            .or_else(|| json.get("name").and_then(|n| n.as_str()))
            .map(str::to_string)
            .unwrap_or_else(|| root.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()),
        root: root.to_path_buf(),
        target_dir: json.get("target-dir").and_then(|t| t.as_str()).map(str::to_string),
        reference: reference(json),
        ..Default::default()
    };
    for sec in sections.iter().flatten() {
        prefix_map(sec.get("psr-4"), root, &mut out.psr_4);
        prefix_map(sec.get("psr-0"), root, &mut out.psr_0);
        out.classmap.extend(strings(sec.get("classmap")).map(|p| normalize(root, p)));
        out.files.extend(strings(sec.get("files")).map(|p| normalize(root, p)));
        out.exclude_from_classmap.extend(strings(sec.get("exclude-from-classmap")).map(str::to_string));
    }
    out
}

/// `dist.reference`, else `source.reference`; none for a path install,
/// whose contents move without it.
fn reference(p: &serde_json::Value) -> Option<String> {
    let dist = p.get("dist");
    if dist.and_then(|d| d.get("type")).and_then(|t| t.as_str()) == Some("path") {
        return None;
    }
    dist.and_then(|d| d.get("reference"))
        .or_else(|| p.get("source").and_then(|s| s.get("reference")))
        .and_then(|r| r.as_str())
        .filter(|r| !r.is_empty())
        .map(str::to_string)
}

/// A `{prefix: path | [paths]}` object, merged into `out` by prefix.
fn prefix_map(v: Option<&serde_json::Value>, root: &Path, out: &mut Vec<(String, Vec<PathBuf>)>) {
    let Some(obj) = v.and_then(|v| v.as_object()) else { return };
    for (prefix, paths) in obj {
        let dirs: Vec<PathBuf> = match paths {
            serde_json::Value::String(s) => vec![normalize(root, s)],
            other => strings(Some(other)).map(|p| normalize(root, p)).collect(),
        };
        match out.iter_mut().find(|(p, _)| p == prefix) {
            Some((_, existing)) => existing.extend(dirs),
            None => out.push((prefix.clone(), dirs)),
        }
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

/// The project's Composer locator for `driver`'s language. Always a
/// locator, even with no `composer.json`: it then locates nothing, and its
/// inputs are still the files whose appearance would change that.
pub fn locator(root: &Path, driver: &'static dyn LanguageDriver) -> Arc<dyn DependencyLocator> {
    let config = ResolvedConfig::load(root, true);
    if let Err(e) = &config {
        log::debug!("composer: no autoload map ({e})");
    }
    Arc::new(ComposerLocator::new(root, driver, config.ok()))
}

/// Class identity → candidate files, in Composer's own order: the generated
/// classmap, then PSR-4 (longest prefix first), then PSR-0.
pub struct ComposerLocator {
    /// The language the map serves: its namespace separator spells the
    /// identities, and the generated classmap is parsed in it.
    driver: &'static dyn LanguageDriver,
    names: crate::model::file_analysis::NameSpellings,
    sep: String,
    psr_4: Vec<(String, Vec<PathBuf>)>,
    psr_0: Vec<(String, Vec<PathBuf>)>,
    /// `$vendorDir` and `$baseDir` as the generated classmap spells them.
    vendor_dir: PathBuf,
    base_dir: PathBuf,
    /// Read on the first ask. No generated classmap (`dump-autoload` never
    /// ran) is an empty map, not a scan of the packages' classmap
    /// directories: Composer itself would load none of those classes.
    classmap: OnceLock<HashMap<String, PathBuf>>,
    /// Composer's `files`, each asked by its path: Composer itself names
    /// them by file, not by anything they declare.
    files: Vec<Declared>,
    roots: Vec<PathBuf>,
    inputs: Vec<PathBuf>,
}

impl ComposerLocator {
    fn new(root: &Path, driver: &'static dyn LanguageDriver, config: Option<ResolvedConfig>) -> Self {
        let names = driver.lang_pack().map(|p| p.names).unwrap_or_default();
        let sep = names.use_map_sep().map(str::to_string).unwrap_or_default();
        let vendor_dir = config.as_ref().map(|c| c.vendor_dir.clone()).unwrap_or_else(|| root.join("vendor"));
        let mut loc = ComposerLocator {
            driver,
            names,
            sep,
            psr_4: Vec::new(),
            psr_0: Vec::new(),
            vendor_dir: vendor_dir.clone(),
            base_dir: root.to_path_buf(),
            classmap: OnceLock::new(),
            files: Vec::new(),
            roots: vec![std::fs::canonicalize(&vendor_dir).unwrap_or_else(|_| vendor_dir.clone())],
            inputs: vec![root.join("composer.json"), installed_json(&vendor_dir), autoload_classmap(&vendor_dir)],
        };
        for p in config.into_iter().flat_map(|c| c.packages) {
            loc.psr_4.extend(p.psr_4);
            loc.psr_0.extend(p.psr_0);
            loc.files.extend(p.files.iter().map(|f| Declared::Loaded(f.display().to_string())));
        }
        // Longest prefix first; the empty fallback prefix sorts last.
        loc.psr_4.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        loc.psr_0.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        loc
    }

    fn psr_4(&self, identity: &str, out: &mut Vec<PathBuf>) {
        for (prefix, dirs) in &self.psr_4 {
            let Some(rest) = identity.strip_prefix(prefix.as_str()) else { continue };
            let rel = format!("{}.php", rest.replace(self.sep.as_str(), "/"));
            out.extend(dirs.iter().map(|d| d.join(&rel)).filter(|p| p.is_file()));
        }
    }

    /// PSR-0 keeps the whole identity in the path, and a `_` in the class
    /// leaf (not the namespace) is a directory too.
    fn psr_0(&self, identity: &str, out: &mut Vec<PathBuf>) {
        let (ns, leaf) = crate::model::conventions::split_qualified(identity, &self.names);
        let dir = ns.map(|ns| format!("{}/", ns.replace(self.sep.as_str(), "/"))).unwrap_or_default();
        let rel = format!("{dir}{}.php", leaf.replace('_', "/"));
        for (prefix, dirs) in &self.psr_0 {
            if identity.starts_with(prefix.as_str()) {
                out.extend(dirs.iter().map(|d| d.join(&rel)).filter(|p| p.is_file()));
            }
        }
    }

    fn classmap(&self) -> &HashMap<String, PathBuf> {
        self.classmap.get_or_init(|| {
            let path = autoload_classmap(&self.vendor_dir);
            let Ok(source) = std::fs::read_to_string(&path) else { return HashMap::new() };
            read_classmap(self.driver, CLASSMAP_DOCUMENT, &source, &self.vendor_dir, &self.base_dir).unwrap_or_else(|e| {
                log::error!("composer: {}: {e}", path.display());
                HashMap::new()
            })
        })
    }
}

/// The classmap Composer generates is read through this document.
const CLASSMAP_DOCUMENT: &str = include_str!("../../queries/php/composer-classmap.scm");

/// The generated classmap's entries, through `document`. An entry whose
/// path is spelled any other way than the document admits is skipped,
/// never guessed at; a document whose captures the classmap vocabulary
/// does not hold is an error.
fn read_classmap(
    driver: &dyn LanguageDriver,
    document: &str,
    source: &str,
    vendor_dir: &Path,
    base_dir: &Path,
) -> Result<HashMap<String, PathBuf>, String> {
    use crate::build::query_extract::{document_matches, ClassmapCap};
    let mut map = HashMap::new();
    for m in document_matches::<ClassmapCap>(driver, document, source)? {
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
            map.entry(class).or_insert(path);
        }
    }
    Ok(map)
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

impl DependencyLocator for ComposerLocator {
    /// Every hit is exact: a `files` entry is its own path, and the
    /// classmap, PSR-4 and PSR-0 each spell the class's own file.
    fn locate(&self, identity: &str) -> Vec<Located> {
        if self.files.iter().any(|d| matches!(d, Declared::Loaded(p) if p == identity)) {
            let path = PathBuf::from(identity);
            return if path.is_file() { vec![Located { path, by: LocatedBy::Exact }] } else { Vec::new() };
        }
        let identity = identity.trim_start_matches(self.sep.as_str());
        let mut out: Vec<PathBuf> = self.classmap().get(identity).filter(|p| p.is_file()).cloned().into_iter().collect();
        self.psr_4(identity, &mut out);
        self.psr_0(identity, &mut out);
        let mut seen = std::collections::HashSet::new();
        out.retain(|p| seen.insert(p.clone()));
        out.into_iter().map(|path| Located { path, by: LocatedBy::Exact }).collect()
    }
    fn declared_demand(&self) -> &[Declared] {
        &self.files
    }
    fn roots(&self) -> &[PathBuf] {
        &self.roots
    }
    fn inputs(&self) -> &[PathBuf] {
        &self.inputs
    }
    /// The names `locate` answers, in its order: the generated classmap's
    /// classes, then every file under a PSR-4 or PSR-0 directory, named the
    /// way `locate` spells it back.
    fn for_each_name(&self, workspace: Option<&Path>, visit: &mut dyn FnMut(&str)) {
        use crate::build::search_path::{listable, module_files, rel_path};
        for class in self.classmap().keys() {
            visit(class);
        }
        let stems = |dir: &PathBuf| -> Vec<String> {
            if !listable(dir, workspace) {
                return Vec::new();
            }
            module_files(dir, self.driver.extensions())
                .iter()
                .filter_map(|p| rel_path(dir, p)?.strip_suffix(".php").map(str::to_string))
                .collect()
        };
        for (prefix, dirs) in &self.psr_4 {
            for stem in dirs.iter().flat_map(stems) {
                visit(&format!("{prefix}{}", stem.replace('/', &self.sep)));
            }
        }
        // A PSR-0 path spells its namespace with directories and its leaf's
        // `_` with them too, so a file answers both pure spellings; each is
        // listed when it carries the prefix the directory serves.
        for (prefix, dirs) in &self.psr_0 {
            for stem in dirs.iter().flat_map(stems) {
                let namespaced = stem.replace('/', &self.sep);
                let underscored = stem.replace('/', "_");
                for name in [namespaced, underscored] {
                    if name.starts_with(prefix.as_str()) {
                        visit(&name);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "composer_tests.rs"]
mod tests;
