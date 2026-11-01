//! The php dependency tier end to end: Composer's map places a class the
//! workspace names, the indexer lands it as DEPENDENCY, and the editor's
//! answers reach it without widening.

use crate::build::language_driver::{LanguageDriver, LanguageRegistry};
use crate::index::module_index::ModuleIndex;
use crate::index::open_docs::{FileKey, OpenDocs};
use crate::index::resolve::{resolve, OverrideScope};
use crate::model::file_analysis::{CrossFileLookup, MethodResolution, ScopedLookup, VisibilityAxis};
use std::path::{Path, PathBuf};
use tree_sitter::Point;

fn php() -> &'static dyn LanguageDriver {
    LanguageRegistry::compiled().for_id("php").expect("php driver")
}

fn project(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("php-deps-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (rel, body) in files {
        write(&dir, rel, body);
    }
    std::fs::canonicalize(&dir).unwrap()
}

fn write(root: &Path, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn indexed(root: &Path) -> ModuleIndex {
    let idx = ModuleIndex::new_for_test();
    super::index_pack_language(php(), root, None, &idx, None, 0);
    // The landings are the resolver thread's.
    idx.wait_idle();
    idx
}

const COMPOSER: &str = r#"{"autoload": {"psr-4": {"App\\": "src/"}}}"#;
const INSTALLED: &str = r#"{"packages": [
  {"name": "acme/model", "install-path": "../acme/model",
   "autoload": {"psr-4": {"Acme\\Model\\": "src/"}}},
  {"name": "old/lib", "install-path": "../old/lib", "autoload": {"classmap": ["legacy/"]}}
]}"#;
const BASE: &str = "<?php\nnamespace Acme\\Model;\nclass Base extends Root {\n    function name() {}\n}\n";
const ROOT: &str = "<?php\nnamespace Acme\\Model;\nclass Root {\n    function root() {}\n}\n";
const UNUSED: &str = "<?php\nnamespace Acme\\Model;\nclass Unused {}\n";
const LEGACY: &str = "<?php\nclass Legacy_Thing {\n    function hi() {}\n}\n";
/// The classmap `composer dump-autoload` writes for INSTALLED.
const CLASSMAP: &str = "<?php\n$vendorDir = dirname(__DIR__);\n$baseDir = dirname($vendorDir);\n\nreturn array(\n    'Legacy_Thing' => $vendorDir . '/old/lib/legacy/thing.php',\n);\n";
const USER: &str = "<?php\nnamespace App;\nuse Acme\\Model\\Base;\nclass User extends Base {\n    function go() { $this->name(); $this->root(); }\n}\n";
const OLD: &str = "<?php\nnamespace App;\nclass Old extends \\Legacy_Thing {\n    function go() { $this->hi(); }\n}\n";
/// A same-leaf class the workspace declares: what `Acme\Model\Base`
/// widens to when nothing places it.
const STRANGER: &str = "<?php\nnamespace App\\Other;\nclass Base {\n    function name() {}\n}\n";

fn composer_project(tag: &str, composer: &str) -> PathBuf {
    project(
        tag,
        &[
            ("composer.json", composer),
            ("vendor/composer/installed.json", INSTALLED),
            ("vendor/acme/model/src/Base.php", BASE),
            ("vendor/acme/model/src/Root.php", ROOT),
            ("vendor/acme/model/src/Unused.php", UNUSED),
            ("vendor/old/lib/legacy/thing.php", LEGACY),
            ("vendor/composer/autoload_classmap.php", CLASSMAP),
            ("src/User.php", USER),
            ("src/Old.php", OLD),
            ("src/Other/Base.php", STRANGER),
        ],
    )
}

fn at(src: &str, needle: &str) -> Point {
    let i = src.find(needle).unwrap();
    let row = src[..i].matches('\n').count();
    Point { row, column: i - src[..i].rfind('\n').map_or(0, |n| n + 1) }
}

/// goto-def's (file, row) answers at `at`, and the hover candidate's file,
/// relative to `root`.
fn landing(idx: &ModuleIndex, root: &Path, rel: &str, at: Point) -> (Vec<(String, usize)>, Option<String>) {
    let p = root.join(rel);
    let fa = idx.workspace_file_for_test(&p).unwrap();
    let store = OpenDocs::new();
    let cs = resolve(&store, &fa, FileKey::Path(crate::util::canonical_path::CanonicalPath::resolve(p)), at, Some(idx), OverrideScope::default());
    let short = |k: &FileKey| match k {
        FileKey::Path(p) => p.strip_prefix(root).unwrap().display().to_string(),
        FileKey::Url(u, _) => u.to_string(),
    };
    (
        cs.definitions().iter().map(|d| (short(&d.key), d.span.start.row)).collect(),
        cs.hover_candidate().map(|d| short(&d.key)),
    )
}

fn member(idx: &ModuleIndex, root: &Path, rel: &str, class: &str, name: &str) -> Option<MethodResolution> {
    let fa = idx.workspace_file_for_test(&root.join(rel)).unwrap();
    let base: &dyn CrossFileLookup = idx;
    let vis = LanguageRegistry::pack_visibility(&fa.language);
    let scoped = ScopedLookup::new(base, &fa.pack.include_closure, None, VisibilityAxis::for_origin(&fa, None, base, vis));
    fa.resolve_method_in_ancestors(class, name, Some(&scoped))
}

fn widening(idx: &ModuleIndex, root: &Path, rel: &str) -> Vec<String> {
    let fa = idx.workspace_file_for_test(&root.join(rel)).unwrap();
    crate::lsp::symbols::widening_diagnostics(&fa, idx).into_iter().map(|d| d.message).collect()
}

fn workspace_files(idx: &ModuleIndex, root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    idx.for_each_workspace_file(&mut |cm| out.push(cm.path.strip_prefix(root).unwrap().display().to_string()));
    out.sort();
    out
}

/// `extends Base` with only `vendor/` providing `Acme\Model\Base`: the
/// member call lands on the vendor declaration, through the parent's own
/// parent too, nothing widens, and the vendor files are dependency — never
/// registered as the project's.
#[test]
fn a_psr_4_dependency_parent_resolves_as_the_dependency_tier() {
    let root = composer_project("psr4", COMPOSER);
    let idx = indexed(&root);
    assert_eq!(workspace_files(&idx, &root), ["src/Old.php", "src/Other/Base.php", "src/User.php"]);

    let (defs, hover) = landing(&idx, &root, "src/User.php", at(USER, "name()"));
    assert_eq!(defs, [("vendor/acme/model/src/Base.php".to_string(), 3)]);
    assert_eq!(hover.as_deref(), Some("vendor/acme/model/src/Base.php"));
    let (defs, _) = landing(&idx, &root, "src/User.php", at(USER, "root()"));
    assert_eq!(defs, [("vendor/acme/model/src/Root.php".to_string(), 3)]);
    assert!(matches!(
        member(&idx, &root, "src/User.php", "App\\User", "name"),
        Some(MethodResolution::CrossFile { ref class, widened: false, .. }) if class == "Acme\\Model\\Base"
    ));
    assert!(widening(&idx, &root, "src/User.php").is_empty());

    let vendor_base = root.join("vendor/acme/model/src/Base.php");
    assert!(idx.is_dependency_path(&crate::util::canonical_path::CanonicalPath::resolve(&vendor_base)));
    assert!(!idx.is_dependency_path(&crate::util::canonical_path::CanonicalPath::resolve(&root.join("src/User.php"))));
    // only what the project asks for is parsed
    assert!(idx.def_candidates("Acme\\Model\\Unused").is_empty());
    assert_eq!(idx.dependency_generation(), 1);
}

/// A class only the generated classmap places lands like a PSR-4 one.
#[test]
fn a_classmap_entry_resolves() {
    let root = composer_project("classmap", COMPOSER);
    let idx = indexed(&root);
    let (defs, _) = landing(&idx, &root, "src/Old.php", at(OLD, "hi()"));
    assert_eq!(defs, [("vendor/old/lib/legacy/thing.php".to_string(), 2)]);
    assert!(idx.is_dependency_path(&crate::util::canonical_path::CanonicalPath::resolve(&root.join("vendor/old/lib/legacy/thing.php"))));
}

/// No usable `composer.json`: today's behaviour — `vendor/` is still not
/// the project's, nothing lands, and the parent widens out loud.
#[test]
fn a_malformed_composer_json_degrades_to_widening() {
    let root = composer_project("malformed", "{not json");
    let idx = indexed(&root);
    assert_eq!(workspace_files(&idx, &root), ["src/Old.php", "src/Other/Base.php", "src/User.php"]);
    assert!(idx.def_candidates("Acme\\Model\\Base").is_empty());
    assert_eq!(
        widening(&idx, &root, "src/User.php"),
        ["nothing indexed declares `Acme\\Model\\Base`; resolved by its leaf to `App\\Other\\Base`"; 2]
    );

    let root = project("nocomposer", &[("src/User.php", USER), ("src/Other/Base.php", STRANGER)]);
    let idx = indexed(&root);
    assert_eq!(widening(&idx, &root, "src/User.php").len(), 2);
}

/// A manifest change is a new generation: the map is re-read, what the old
/// one landed retires, and the project's asks land again under the new
/// one. A saved file's new import lands as the save registers.
#[test]
fn a_manifest_change_moves_the_map_and_a_save_asks_for_its_imports() {
    let root = composer_project("gen", COMPOSER);
    let installed = root.join("vendor/composer/installed.json");
    write(&root, "vendor/composer/installed.json", r#"{"packages": []}"#);
    let idx = indexed(&root);
    let inv = crate::index::pack_invalidator::PackInvalidator::serving(php());
    let docs = OpenDocs::new();
    assert!(idx.def_candidates("Acme\\Model\\Base").is_empty());
    assert_eq!(widening(&idx, &root, "src/User.php").len(), 2);

    write(&root, "vendor/composer/installed.json", INSTALLED);
    assert!(idx.is_dependency_input(&installed));
    inv.file_changed(None, &idx, &docs, &installed, false);
    idx.wait_idle();
    assert_eq!(idx.dependency_generation(), 2);
    assert!(widening(&idx, &root, "src/User.php").is_empty());
    assert_eq!(landing(&idx, &root, "src/User.php", at(USER, "name()")).0[0].0, "vendor/acme/model/src/Base.php");
    // the manifest is not a php file of the project's
    assert!(!workspace_files(&idx, &root).iter().any(|f| f.ends_with(".json")));

    write(&root, "vendor/composer/installed.json", r#"{"packages": [ ]}"#);
    inv.file_changed(None, &idx, &docs, &installed, false);
    idx.wait_idle();
    assert_eq!(idx.dependency_generation(), 3);
    assert!(idx.def_candidates("Acme\\Model\\Base").is_empty(), "the old generation's landing retired");
    assert!(idx.def_candidates("Acme\\Model\\Root").is_empty());

    // a save under the tier's root never registers as the project's
    write(&root, "vendor/composer/installed.json", INSTALLED);
    inv.file_changed(None, &idx, &docs, &installed, false);
    idx.wait_idle();
    inv.file_changed(None, &idx, &docs, &root.join("vendor/acme/model/src/Unused.php"), false);
    idx.wait_idle();
    assert_eq!(workspace_files(&idx, &root), ["src/Old.php", "src/Other/Base.php", "src/User.php"]);
    assert!(idx.def_candidates("Acme\\Model\\Unused").is_empty());

    // a project file that starts naming a dependency asks for it on save
    let uses = "<?php\nnamespace App;\nuse Acme\\Model\\Unused;\nclass Fresh extends Unused {}\n";
    write(&root, "src/Fresh.php", uses);
    inv.file_changed(None, &idx, &docs, &root.join("src/Fresh.php"), false);
    idx.wait_idle();
    assert_eq!(idx.def_candidates("Acme\\Model\\Unused").len(), 1);
}
