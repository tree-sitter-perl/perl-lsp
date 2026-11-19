//! Composer's autoload data → the map's answers.

use super::*;
use crate::build::dependency_map::{Ask, Located};
use crate::util::canonical_path::CanonicalPath;

/// A scratch project: `files` are (relative path, contents).
fn project(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("composer-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (rel, body) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    }
    CanonicalPath::resolve(dir).into_path_buf()
}

const ROOT_JSON: &str = r#"{"name": "app/app",
  "autoload": {"psr-4": {"App\\": "src/"}},
  "autoload-dev": {"psr-4": {"App\\Tests\\": ["tests/", "tests2/"]}}}"#;

const V2: &str = r#"{"packages": [
  {"name": "acme/model", "install-path": "../acme/model",
   "autoload": {"psr-4": {"Acme\\Model\\": "src/", "Acme\\Model\\Deep\\": "deep/"},
                "files": ["helpers.php"]},
   "autoload-dev": {"psr-4": {"Acme\\Model\\Tests\\": "tests/"}}},
  {"name": "old/lib", "install-path": "../old/lib",
   "autoload": {"psr-0": {"Old_": "lib/"}, "classmap": ["legacy/"]}}
], "dev": true}"#;

fn names(map: &DependencyMap, ask: Ask) -> Vec<PathBuf> {
    map.locate(&ask).into_iter().map(|l| l.path.into_path_buf()).collect()
}

fn at(map: &DependencyMap, name: &str) -> Vec<PathBuf> {
    names(map, Ask::Name(name.to_string()))
}

#[test]
fn installed_json_v2_lists_each_packages_autoload_at_its_install_path() {
    let root = project("v2", &[("composer.json", ROOT_JSON), ("vendor/composer/installed.json", V2)]);
    let cfg = ResolvedConfig::load(&root, true).unwrap();
    assert_eq!(cfg.vendor_dir, root.join("vendor"));
    let app = &cfg.packages[0];
    assert_eq!(app.root, root);
    assert_eq!(
        app.psr_4,
        [
            ("App\\".to_string(), vec![root.join("src")]),
            ("App\\Tests\\".to_string(), vec![root.join("tests"), root.join("tests2")]),
        ]
    );
    let acme = &cfg.packages[1];
    assert_eq!(acme.root, root.join("vendor/acme/model"));
    assert_eq!(acme.files, [root.join("vendor/acme/model/helpers.php")]);
    assert!(acme.psr_4.iter().all(|(p, _)| !p.contains("Tests")), "a dependency's autoload-dev is never read");
    let old = &cfg.packages[2];
    assert_eq!(old.psr_0, [("Old_".to_string(), vec![root.join("vendor/old/lib/lib")])]);

    // without dev, the root's autoload-dev is gone too
    let cfg = ResolvedConfig::load(&root, false).unwrap();
    assert_eq!(cfg.packages[0].psr_4.len(), 1);
}

#[test]
fn installed_json_v1_is_a_bare_list_installed_under_vendor_by_name() {
    let v1 = r#"[{"name": "acme/model", "autoload": {"psr-4": {"Acme\\Model\\": "src"}}}]"#;
    let root = project("v1", &[("composer.json", "{}"), ("vendor/composer/installed.json", v1)]);
    let cfg = ResolvedConfig::load(&root, true).unwrap();
    assert_eq!(cfg.packages.len(), 2);
    assert_eq!(cfg.packages[1].root, root.join("vendor/acme/model"));
    assert_eq!(cfg.packages[1].psr_4, [("Acme\\Model\\".to_string(), vec![root.join("vendor/acme/model/src")])]);
}

#[test]
fn a_configured_vendor_dir_moves_the_tier_and_its_manifests() {
    let root = project(
        "vdir",
        &[
            ("composer.json", r#"{"config": {"vendor-dir": "lib/deps"}}"#),
            ("lib/deps/composer/installed.json", r#"{"packages": [{"name": "a/b", "install-path": "../a/b"}]}"#),
        ],
    );
    let cfg = ResolvedConfig::load(&root, true).unwrap();
    assert_eq!(cfg.vendor_dir, root.join("lib/deps"));
    assert_eq!(cfg.packages[1].root, root.join("lib/deps/a/b"));
    let map = read_map(Some(&root)).unwrap();
    assert_eq!(
        map.inputs(),
        [
            root.join("composer.json"),
            root.join("lib/deps/composer/installed.json"),
            root.join("lib/deps/composer/autoload_classmap.php")
        ]
    );
    assert!(map.owns(&root.join("lib/deps/a/b/src/X.php")));
    assert!(!map.owns(&root.join("vendor/a/b/src/X.php")));
}

#[test]
fn a_missing_or_malformed_manifest_places_nothing_and_owns_nothing_but_names_its_inputs() {
    let root = project("bad", &[("composer.json", "{not json"), ("vendor/acme/model/src/Base.php", "<?php")]);
    assert!(ResolvedConfig::load(&root, true).is_err());
    let map = read_map(Some(&root)).unwrap();
    assert!(at(&map, "Acme\\Model\\Base").is_empty());
    assert!(map.tier_roots().is_empty(), "with no manifest the walk owns `vendor/`");
    assert_eq!(
        map.inputs(),
        [
            root.join("composer.json"),
            root.join("vendor/composer/installed.json"),
            root.join("vendor/composer/autoload_classmap.php")
        ]
    );

    let root = project("none", &[("vendor/composer/installed.json", V2)]);
    assert!(ResolvedConfig::load(&root, true).is_err());

    // a malformed installed.json is a project with nothing installed
    let root = project("badinst", &[("composer.json", ROOT_JSON), ("vendor/composer/installed.json", "[{")]);
    assert_eq!(ResolvedConfig::load(&root, true).unwrap().packages.len(), 1);

    assert!(read_map(None).is_none(), "no project, no map");
}

/// What `composer dump-autoload` writes: both anchors, an absolute path
/// for a file under neither, a class PSR-0 would also place, and two
/// spellings Composer never writes.
const CLASSMAP: &str = r#"<?php

// autoload_classmap.php @generated by Composer

$vendorDir = dirname(__DIR__);
$baseDir = dirname($vendorDir);

return array(
    'Acme\\Model\\Base' => $vendorDir . '/acme/model/src/Base.php',
    'Legacy_Widget' => $vendorDir . '/old/lib/legacy/anything.php',
    'App\\Seeds\\Seeder' => $baseDir . '/database/Seeder.php',
    'Outside\\Thing' => '/opt/shared/Thing.php',
    'It\'s' => $vendorDir . '/quote.php',
    'Chained\\One' => $vendorDir . '/a' . '/b.php',
    'Computed\\One' => dirname($vendorDir) . '/c.php',
);
"#;

#[test]
fn the_generated_classmap_reads_both_anchors_and_skips_any_other_spelling() {
    let (vendor, base) = (Path::new("/p/vendor"), Path::new("/p"));
    let rows = read_classmap(CLASSMAP_DOCUMENT, CLASSMAP, vendor, base).unwrap();
    let got = |c: &str| rows.iter().find(|(n, _)| n == c).map(|(_, p)| p.clone());
    assert_eq!(got("Acme\\Model\\Base"), Some(vendor.join("acme/model/src/Base.php")));
    assert_eq!(got("Legacy_Widget"), Some(vendor.join("old/lib/legacy/anything.php")));
    assert_eq!(got("App\\Seeds\\Seeder"), Some(base.join("database/Seeder.php")));
    assert_eq!(got("Outside\\Thing"), Some(PathBuf::from("/opt/shared/Thing.php")));
    assert_eq!(got("It's"), Some(vendor.join("quote.php")));
    assert_eq!(got("Chained\\One"), None);
    assert_eq!(got("Computed\\One"), None);
    assert_eq!(rows.len(), 5);
}

/// A capture the classmap vocabulary does not hold fails the read: a
/// renamed or misspelt `@classmap.*` would otherwise drop every entry it
/// spells and leave the classmap silently empty.
#[test]
fn a_classmap_document_capture_outside_the_vocabulary_fails_the_read() {
    let (vendor, base) = (Path::new("/p/vendor"), Path::new("/p"));
    let misspelt = CLASSMAP_DOCUMENT.replace("@classmap.path.vendor", "@classmap.path.vendr");
    let err = read_classmap(&misspelt, CLASSMAP, vendor, base).unwrap_err();
    assert!(err.contains("classmap.path.vendr"), "{err}");
}

/// No generated classmap: a package's `classmap` directories are never
/// scanned, so a class only they declare is not located.
#[test]
fn without_a_generated_classmap_nothing_is_located_by_classmap() {
    let root = project(
        "nocm",
        &[
            ("composer.json", ROOT_JSON),
            ("vendor/composer/installed.json", V2),
            ("vendor/old/lib/legacy/anything.php", "<?php\nclass Legacy_Widget {}\n"),
        ],
    );
    let map = read_map(Some(&root)).unwrap();
    assert!(at(&map, "Legacy_Widget").is_empty());
    // the file Composer would write is an input, so writing it moves the map
    assert!(map.inputs().contains(&root.join("vendor/composer/autoload_classmap.php")));
}

#[test]
fn lanes_answer_in_composer_s_order_and_files_load_under_no_name() {
    let root = project(
        "locate",
        &[
            ("composer.json", ROOT_JSON),
            ("vendor/composer/installed.json", V2),
            ("vendor/acme/model/src/Base.php", "<?php"),
            ("vendor/acme/model/deep/Thing.php", "<?php"),
            ("vendor/acme/model/src/Deep/Thing.php", "<?php"),
            ("vendor/acme/model/helpers.php", "<?php"),
            ("vendor/old/lib/lib/Old/Tool/Hammer.php", "<?php"),
            ("vendor/old/lib/legacy/anything.php", "<?php\nclass Legacy_Widget {}\n"),
            ("vendor/composer/autoload_classmap.php", CLASSMAP),
            ("src/Models/User.php", "<?php"),
        ],
    );
    let map = read_map(Some(&root)).unwrap();
    let v = |rel: &str| root.join("vendor").join(rel);
    assert_eq!(at(&map, "Acme\\Model\\Base"), [v("acme/model/src/Base.php")], "the classmap and PSR-4 agree");
    // both prefixes place a file; the longer one answers first
    assert_eq!(
        at(&map, "Acme\\Model\\Deep\\Thing"),
        [v("acme/model/deep/Thing.php"), v("acme/model/src/Deep/Thing.php")]
    );
    // PSR-0 spells the whole name, and a `_` in the leaf is a directory
    assert_eq!(at(&map, "Old_Tool_Hammer"), [v("old/lib/lib/Old/Tool/Hammer.php")]);
    assert!(at(&map, "Acme\\Model\\Missing").is_empty());
    // the generated classmap answers what the prefixes cannot
    assert_eq!(at(&map, "Legacy_Widget"), [v("old/lib/legacy/anything.php")]);
    // the root package's own prefixes place the project's files too; the
    // tier leaves those to the walk
    assert_eq!(at(&map, "App\\Models\\User"), [root.join("src/Models/User.php")]);
    assert!(map.owns(&v("acme/model/src/Base.php")));
    assert!(!map.owns(&root.join("src/Models/User.php")));

    // a `files` entry loads whatever is asked: no name places it
    let helpers = CanonicalPath::resolve(v("acme/model/helpers.php"));
    assert_eq!(map.locate(&Ask::File(helpers.clone())), [Located { path: helpers, loads_as: None }]);
    // asked by path, a PSR-4 file names the class it loads as
    let base = CanonicalPath::resolve(v("acme/model/src/Base.php"));
    assert!(map
        .locate(&Ask::File(base))
        .iter()
        .any(|l| l.loads_as.as_deref() == Some("Acme\\Model\\Base")));
}
