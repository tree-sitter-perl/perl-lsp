//! Composer's autoload data → the locator's answers.

use super::*;

/// A scratch project: `files` are (relative path, contents).
pub(crate) fn project(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("composer-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (rel, body) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
    }
    std::fs::canonicalize(&dir).unwrap()
}

/// The driver a Composer map serves.
fn php() -> &'static dyn LanguageDriver {
    crate::build::language_driver::LanguageRegistry::compiled().for_id("php").expect("php driver")
}

const ROOT_JSON: &str = r#"{"name": "app/app",
  "autoload": {"psr-4": {"App\\": "src/"}},
  "autoload-dev": {"psr-4": {"App\\Tests\\": ["tests/", "tests2/"]}}}"#;

const V2: &str = r#"{"packages": [
  {"name": "acme/model", "install-path": "../acme/model",
   "dist": {"type": "zip", "reference": "abc123"},
   "autoload": {"psr-4": {"Acme\\Model\\": "src/", "Acme\\Model\\Deep\\": "deep/"},
                "files": ["helpers.php"]},
   "autoload-dev": {"psr-4": {"Acme\\Model\\Tests\\": "tests/"}}},
  {"name": "old/lib", "install-path": "../old/lib",
   "autoload": {"psr-0": {"Old_": "lib/"}, "classmap": ["legacy/"]}}
], "dev": true}"#;

#[test]
fn installed_json_v2_lists_each_packages_autoload_at_its_install_path() {
    let root = project("v2", &[("composer.json", ROOT_JSON), ("vendor/composer/installed.json", V2)]);
    let cfg = ResolvedConfig::load(&root, true).unwrap();
    assert_eq!(cfg.vendor_dir, root.join("vendor"));
    let names: Vec<&str> = cfg.packages.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["app/app", "acme/model", "old/lib"]);
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
    assert_eq!(acme.reference.as_deref(), Some("abc123"));
    assert!(acme.psr_4.iter().all(|(p, _)| !p.contains("Tests")), "a dependency's autoload-dev is never read");
    let old = &cfg.packages[2];
    assert_eq!(old.psr_0, [("Old_".to_string(), vec![root.join("vendor/old/lib/lib")])]);
    assert_eq!(old.classmap, [root.join("vendor/old/lib/legacy")]);

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
fn a_configured_vendor_dir_moves_the_tier_and_its_manifest() {
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
    let loc = locator(&root, php());
    assert_eq!(
        loc.inputs(),
        [
            root.join("composer.json"),
            root.join("lib/deps/composer/installed.json"),
            root.join("lib/deps/composer/autoload_classmap.php")
        ]
    );
    assert_eq!(loc.roots(), [root.join("lib/deps")]);
}

#[test]
fn a_missing_or_malformed_manifest_locates_nothing_but_still_names_its_inputs() {
    let root = project("bad", &[("composer.json", "{not json"), ("vendor/acme/model/src/Base.php", "<?php")]);
    assert!(ResolvedConfig::load(&root, true).is_err());
    let loc = locator(&root, php());
    assert!(loc.locate("Acme\\Model\\Base").is_empty());
    assert_eq!(
        loc.inputs(),
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
    let map = read_classmap(php(), CLASSMAP_DOCUMENT, CLASSMAP, vendor, base).unwrap();
    let got = |c: &str| map.get(c).cloned();
    assert_eq!(got("Acme\\Model\\Base"), Some(vendor.join("acme/model/src/Base.php")));
    assert_eq!(got("Legacy_Widget"), Some(vendor.join("old/lib/legacy/anything.php")));
    assert_eq!(got("App\\Seeds\\Seeder"), Some(base.join("database/Seeder.php")));
    assert_eq!(got("Outside\\Thing"), Some(PathBuf::from("/opt/shared/Thing.php")));
    assert_eq!(got("It's"), Some(vendor.join("quote.php")));
    assert_eq!(got("Chained\\One"), None);
    assert_eq!(got("Computed\\One"), None);
    assert_eq!(map.len(), 5);
}

/// A capture the classmap vocabulary does not hold fails the read: a
/// renamed or misspelt `@classmap.*` would otherwise drop every entry it
/// spells and leave the classmap silently empty.
#[test]
fn a_classmap_document_capture_outside_the_vocabulary_fails_the_read() {
    let (vendor, base) = (Path::new("/p/vendor"), Path::new("/p"));
    let misspelt = CLASSMAP_DOCUMENT.replace("@classmap.path.vendor", "@classmap.path.vendr");
    let err = read_classmap(php(), &misspelt, CLASSMAP, vendor, base).unwrap_err();
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
    let loc = locator(&root, php());
    assert!(loc.locate("Legacy_Widget").is_empty());
    // the file Composer would write is an input, so writing it moves the map
    assert!(loc.inputs().contains(&root.join("vendor/composer/autoload_classmap.php")));
}

#[test]
fn psr_4_answers_by_the_longest_prefix_and_psr_0_keeps_the_whole_identity() {
    let root = project(
        "locate",
        &[
            ("composer.json", ROOT_JSON),
            ("vendor/composer/installed.json", V2),
            ("vendor/acme/model/src/Base.php", "<?php"),
            ("vendor/acme/model/deep/Thing.php", "<?php"),
            ("vendor/acme/model/src/Deep/Thing.php", "<?php"),
            ("vendor/old/lib/lib/Old/Tool/Hammer.php", "<?php"),
            ("vendor/old/lib/legacy/anything.php", "<?php\nclass Legacy_Widget {}\n"),
            ("vendor/composer/autoload_classmap.php", CLASSMAP),
        ],
    );
    let loc = locator(&root, php());
    let v = |rel: &str| root.join("vendor").join(rel);
    // Every hit spells the class's own file.
    assert!(loc.locate("Acme\\Model\\Deep\\Thing").iter().all(|l| l.by == LocatedBy::Exact));
    let at = |id: &str| loc.locate(id).into_iter().map(|l| l.path).collect::<Vec<_>>();
    assert_eq!(at("Acme\\Model\\Base"), [v("acme/model/src/Base.php")]);
    assert_eq!(at("\\Acme\\Model\\Base"), [v("acme/model/src/Base.php")]);
    // both prefixes place a file; the longer one answers first
    assert_eq!(
        at("Acme\\Model\\Deep\\Thing"),
        [v("acme/model/deep/Thing.php"), v("acme/model/src/Deep/Thing.php")]
    );
    assert_eq!(at("Old_Tool_Hammer"), [v("old/lib/lib/Old/Tool/Hammer.php")]);
    assert!(at("Acme\\Model\\Missing").is_empty());
    // the generated classmap answers what the prefixes cannot
    assert_eq!(at("Legacy_Widget"), [v("old/lib/legacy/anything.php")]);
    // and answers first, as Composer's loader asks it first
    assert_eq!(at("Acme\\Model\\Base"), [v("acme/model/src/Base.php")]);
    assert_eq!(loc.roots(), [root.join("vendor")]);
}
