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

/// Rewrite `rel` in place — same size, same mtime — so only a parse can
/// see the change; a warm row still reads as current.
fn rewrite_unseen(root: &Path, rel: &str, from: &str, to: &str) {
    assert_eq!(from.len(), to.len());
    let p = root.join(rel);
    let mtime = std::fs::metadata(&p).unwrap().modified().unwrap();
    let body = std::fs::read_to_string(&p).unwrap().replace(from, to);
    std::fs::write(&p, body).unwrap();
    std::fs::File::options().write(true).open(&p).unwrap().set_modified(mtime).unwrap();
}

/// The store's import-tier rows for files whose path is `like`.
fn import_rows(store: &Path, like: &str) -> i64 {
    crate::index::module_cache::with_test_cache_dir(store, std::time::Duration::from_secs(5), || {
        crate::index::module_cache::open_cache_db(None, "php")
            .unwrap()
            .query_row(
                "SELECT count(*) FROM modules WHERE source = 'import' AND path LIKE ?1",
                [like],
                |r| r.get(0),
            )
            .unwrap()
    })
}

/// Landings persist: a second run against the same store warms every
/// landed vendor file instead of parsing it — each was rewritten unseen
/// between the runs, so a parse would show the rewrite and the warm copy
/// cannot. A map move then takes the retired landings' rows with it.
#[test]
fn a_warm_start_parses_no_dependency_file_and_a_map_move_drops_its_rows() {
    let root = composer_project("warm", COMPOSER);
    let store = std::env::temp_dir().join(format!("php-deps-warm-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&store);
    std::fs::create_dir_all(&store).unwrap();
    let busy = std::time::Duration::from_secs(5);
    let resolves = |idx: &ModuleIndex, rel: &str, class: &str, name: &str| {
        matches!(
            member(idx, &root, rel, class, name),
            Some(MethodResolution::CrossFile { widened: false, .. })
        )
    };

    crate::index::module_cache::with_test_cache_dir(&store, busy, || {
        let idx = indexed(&root);
        assert!(resolves(&idx, "src/User.php", "App\\User", "name"));
        assert!(resolves(&idx, "src/User.php", "App\\User", "root"));
        assert!(resolves(&idx, "src/Old.php", "App\\Old", "hi"));
    });
    assert_eq!(import_rows(&store, "%/vendor/%"), 3, "Base, Root and Legacy_Thing persisted");

    rewrite_unseen(&root, "vendor/acme/model/src/Base.php", "name()", "nmae()");
    rewrite_unseen(&root, "vendor/acme/model/src/Root.php", "root()", "rote()");
    rewrite_unseen(&root, "vendor/old/lib/legacy/thing.php", "hi()", "ih()");

    crate::index::module_cache::with_test_cache_dir(&store, busy, || {
        let idx = indexed(&root);
        assert!(resolves(&idx, "src/User.php", "App\\User", "name"), "Base.php was parsed again");
        assert!(resolves(&idx, "src/User.php", "App\\User", "root"), "Root.php was parsed again");
        assert!(resolves(&idx, "src/Old.php", "App\\Old", "hi"), "thing.php was parsed again");

        let installed = root.join("vendor/composer/installed.json");
        write(&root, "vendor/composer/installed.json", r#"{"packages": []}"#);
        let inv = crate::index::pack_invalidator::PackInvalidator::serving(php());
        inv.file_changed(None, &idx, &OpenDocs::new(), &installed, false);
        idx.wait_idle();
        assert_eq!(idx.dependency_generation(), 2);
    });
    assert_eq!(import_rows(&store, "%/acme/%"), 0, "the retired landings' rows went with their map");
}

/// What completion offers at the `|` in `typed`, typed into `rel`.
fn completes(idx: &ModuleIndex, root: &Path, rel: &str, typed: &str) -> Vec<(String, Option<String>)> {
    let path = root.join(rel);
    let fa = idx.workspace_file_for_test(&path).unwrap();
    let cursor = typed.find('|').unwrap();
    let typed = typed.replacen('|', "", 1);
    let before = &typed[..cursor];
    let point = Point { row: before.matches('\n').count(), column: cursor - before.rfind('\n').map_or(0, |n| n + 1) };
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&tree_sitter_php::LANGUAGE_PHP.into()).unwrap();
    let tree = parser.parse(&typed, None).unwrap();
    let (items, _) =
        crate::lsp::backend::pack_completion(&OpenDocs::new(), &fa, &typed, &tree, point, "php", Some(&crate::util::canonical_path::CanonicalPath::resolve(&path)), idx);
    items.into_iter().map(|i| (i.label, i.detail)).collect()
}

/// A vendor class nothing uses completes from Composer's map, unparsed:
/// in a `use`, and as an absolute reference; a used one ranks as indexed.
/// Each statement is closed: an unterminated one the grammar recovers as
/// an ERROR no pattern names.
#[test]
fn an_unused_vendor_class_completes_from_the_map() {
    let root = composer_project("names", COMPOSER);
    let idx = indexed(&root);
    assert!(idx.def_candidates("Acme\\Model\\Unused").is_empty(), "nothing parsed it");
    let unused = ("Acme\\Model\\Unused".to_string(), Some("available".to_string()));
    let in_use = completes(&idx, &root, "src/User.php", "<?php\nnamespace App;\nuse Acme\\Model\\Un|;");
    assert!(in_use.contains(&unused), "{in_use:?}");
    let in_new = completes(&idx, &root, "src/User.php", "<?php\nnamespace App;\nfunction f() { return new \\Acme\\Model\\Un|; }");
    assert!(in_new.contains(&unused), "{in_new:?}");
    let base = completes(&idx, &root, "src/User.php", "<?php\nnamespace App;\nuse Acme\\Model\\Ba|;");
    assert!(base.contains(&("Acme\\Model\\Base".to_string(), Some("indexed".to_string()))), "{base:?}");
    // the map's classmap names complete too
    let legacy = completes(&idx, &root, "src/User.php", "<?php\nnamespace App;\nuse Legacy_|;");
    assert!(legacy.iter().any(|(l, _)| l == "Legacy_Thing"), "{legacy:?}");
}

/// The listing is the map's: a package Composer installs later is offered
/// once the map moves, with no restart.
#[test]
fn the_listing_moves_with_the_map() {
    let root = composer_project("names-move", COMPOSER);
    let idx = indexed(&root);
    let typed = "<?php\nnamespace App;\nuse Fresh\\|;";
    assert!(completes(&idx, &root, "src/User.php", typed).iter().all(|(l, _)| !l.starts_with("Fresh\\")));
    write(&root, "vendor/fresh/pkg/src/Thing.php", "<?php\nnamespace Fresh;\nclass Thing {}\n");
    let installed = INSTALLED.replace(
        "\n]}",
        ",\n  {\"name\": \"fresh/pkg\", \"install-path\": \"../fresh/pkg\", \"autoload\": {\"psr-4\": {\"Fresh\\\\\": \"src/\"}}}\n]}",
    );
    write(&root, "vendor/composer/installed.json", &installed);
    let manifest = root.join("vendor/composer/installed.json");
    crate::index::pack_invalidator::PackInvalidator::serving(php()).file_changed(None, &idx, &OpenDocs::new(), &manifest, false);
    idx.wait_idle();
    let got = completes(&idx, &root, "src/User.php", typed);
    assert!(got.iter().any(|(l, _)| l == "Fresh\\Thing"), "{got:?}");
}

/// A class named with no `use` — a construction and a parameter type
/// spelled absolute — is demand like an import: it lands as dependency.
#[test]
fn a_class_named_with_no_use_lands() {
    const MAKE: &str = "<?php\nnamespace App;\nclass Make {\n    function go(\\Acme\\Model\\Root $r) { return new \\Acme\\Model\\Unused(); }\n}\n";
    let root = project(
        "unused",
        &[
            ("composer.json", COMPOSER),
            ("vendor/composer/installed.json", INSTALLED),
            ("vendor/acme/model/src/Root.php", ROOT),
            ("vendor/acme/model/src/Unused.php", UNUSED),
            ("src/Make.php", MAKE),
        ],
    );
    let idx = indexed(&root);
    let fa = idx.workspace_file_for_test(&root.join("src/Make.php")).unwrap();
    let used = fa.used_identities();
    assert!(used.iter().any(|u| u == "Acme\\Model\\Unused"), "{used:?}");
    assert!(used.iter().any(|u| u == "Acme\\Model\\Root"), "{used:?}");
    for (class, rel) in [("Acme\\Model\\Unused", "Unused.php"), ("Acme\\Model\\Root", "Root.php")] {
        assert!(!idx.def_candidates(class).is_empty(), "{class} did not land");
        assert!(idx.is_dependency_path(&crate::util::canonical_path::CanonicalPath::resolve(&root.join("vendor/acme/model/src").join(rel))));
    }
}

/// Composer's `files` are declared demand, asked by their own path: they
/// land in every session, persist under that path, and a warm start loads
/// them instead of parsing them again.
#[test]
fn an_always_loaded_file_lands_once_and_warms() {
    const WITH_FILES: &str = r#"{"packages": [
  {"name": "acme/model", "install-path": "../acme/model",
   "autoload": {"psr-4": {"Acme\\Model\\": "src/"}, "files": ["helpers.php"]}}
]}"#;
    const HELPERS: &str = "<?php\nfunction acme_help() {}\n";
    let root = project(
        "files",
        &[
            ("composer.json", COMPOSER),
            ("vendor/composer/installed.json", WITH_FILES),
            ("vendor/acme/model/helpers.php", HELPERS),
            ("src/User.php", USER),
            ("vendor/acme/model/src/Base.php", BASE),
            ("vendor/acme/model/src/Root.php", ROOT),
        ],
    );
    let helpers = root.join("vendor/acme/model/helpers.php");
    let store = std::env::temp_dir().join(format!("php-deps-files-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&store);
    std::fs::create_dir_all(&store).unwrap();
    let busy = std::time::Duration::from_secs(5);
    let declares = |idx: &ModuleIndex, name: &str| {
        idx.def_candidates(&helpers.display().to_string())
            .iter()
            .any(|c| idx.symbols_present(c).symbols().iter().any(|s| s.name == name))
    };

    crate::index::module_cache::with_test_cache_dir(&store, busy, || {
        let idx = indexed(&root);
        assert!(idx.is_dependency_path(&crate::util::canonical_path::CanonicalPath::resolve(&helpers)));
        assert!(declares(&idx, "acme_help"));
    });
    assert_eq!(import_rows(&store, "%/helpers.php"), 1, "filed under its own path");

    rewrite_unseen(&root, "vendor/acme/model/helpers.php", "acme_help", "acme_hlep");
    crate::index::module_cache::with_test_cache_dir(&store, busy, || {
        let idx = indexed(&root);
        assert!(declares(&idx, "acme_help"), "helpers.php was parsed again");
    });
}

/// A fresh store directory for a two-session test.
fn store_dir(tag: &str) -> PathBuf {
    let store = std::env::temp_dir().join(format!("php-deps-{tag}-store-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&store);
    std::fs::create_dir_all(&store).unwrap();
    store
}

/// A cache never changes an answer: a warm start lands what its own demand
/// asks, never every name an earlier session stored. `Legacy_Thing` was
/// landed and persisted for `Old.php`; with that file gone nothing asks for
/// it, so the next session leaves it unlanded, as a cold one would.
#[test]
fn a_warm_start_lands_only_what_its_demand_asks() {
    let root = composer_project("warm-asked", COMPOSER);
    let store = store_dir("warm-asked");
    let busy = std::time::Duration::from_secs(5);
    crate::index::module_cache::with_test_cache_dir(&store, busy, || {
        let idx = indexed(&root);
        assert!(idx.get_cached("Legacy_Thing").is_some());
    });
    std::fs::remove_file(root.join("src/Old.php")).unwrap();
    crate::index::module_cache::with_test_cache_dir(&store, busy, || {
        let idx = indexed(&root);
        assert!(idx.get_cached("Acme\\Model\\Base").is_some(), "what is asked warms");
        assert!(idx.get_cached("Legacy_Thing").is_none(), "stored, never asked, and landed anyway");
    });
}

/// An install moves no map input (`installed.json` already listed the
/// package), so a miss the store remembered would outlive it: the class
/// would never land until the store was cleared. A miss is located again
/// by every session instead.
#[test]
fn a_class_installed_after_a_miss_lands_in_the_next_session() {
    let root = project(
        "installed-later",
        &[
            ("composer.json", COMPOSER),
            ("vendor/composer/installed.json", INSTALLED),
            ("vendor/acme/model/src/Root.php", ROOT),
            ("src/User.php", USER),
        ],
    );
    let store = store_dir("installed-later");
    let busy = std::time::Duration::from_secs(5);
    crate::index::module_cache::with_test_cache_dir(&store, busy, || {
        let idx = indexed(&root);
        assert!(idx.get_cached("Acme\\Model\\Base").is_none(), "not installed yet");
    });
    write(&root, "vendor/acme/model/src/Base.php", BASE);
    crate::index::module_cache::with_test_cache_dir(&store, busy, || {
        let idx = indexed(&root);
        assert!(idx.get_cached("Acme\\Model\\Base").is_some(), "the install never landed");
    });
}
