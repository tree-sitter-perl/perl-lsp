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
    let loc = locator(&root);
    assert_eq!(loc.inputs(), [root.join("composer.json"), root.join("lib/deps/composer/installed.json")]);
    assert_eq!(loc.roots(), [root.join("lib/deps")]);
}

#[test]
fn a_missing_or_malformed_manifest_locates_nothing_but_still_names_its_inputs() {
    let root = project("bad", &[("composer.json", "{not json"), ("vendor/acme/model/src/Base.php", "<?php")]);
    assert!(ResolvedConfig::load(&root, true).is_err());
    let loc = locator(&root);
    assert!(loc.locate("Acme\\Model\\Base").is_empty());
    assert_eq!(loc.inputs(), [root.join("composer.json"), root.join("vendor/composer/installed.json")]);

    let root = project("none", &[("vendor/composer/installed.json", V2)]);
    assert!(ResolvedConfig::load(&root, true).is_err());
    assert!(locator(&root).always_loaded().is_empty());

    // a malformed installed.json is a project with nothing installed
    let root = project("badinst", &[("composer.json", ROOT_JSON), ("vendor/composer/installed.json", "[{")]);
    assert_eq!(ResolvedConfig::load(&root, true).unwrap().packages.len(), 1);
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
        ],
    );
    let loc = locator(&root);
    let v = |rel: &str| root.join("vendor").join(rel);
    assert_eq!(loc.locate("Acme\\Model\\Base"), [v("acme/model/src/Base.php")]);
    assert_eq!(loc.locate("\\Acme\\Model\\Base"), [v("acme/model/src/Base.php")]);
    // both prefixes place a file; the longer one answers first
    assert_eq!(
        loc.locate("Acme\\Model\\Deep\\Thing"),
        [v("acme/model/deep/Thing.php"), v("acme/model/src/Deep/Thing.php")]
    );
    assert_eq!(loc.locate("Old_Tool_Hammer"), [v("old/lib/lib/Old/Tool/Hammer.php")]);
    assert!(loc.locate("Acme\\Model\\Missing").is_empty());
    // the classmap answers what the prefixes cannot, from the class the file declares
    assert_eq!(loc.locate("Legacy_Widget"), [v("old/lib/legacy/anything.php")]);
    assert_eq!(loc.always_loaded(), [v("acme/model/helpers.php")]);
    assert_eq!(loc.roots(), [root.join("vendor")]);
}
