//! Composer install-map discovery: the gate, the install map and the
//! degradation path when a vendor tree carries no map.

use super::*;

fn write(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

#[test]
fn no_composer_json_means_no_roots() {
    let dir = std::env::temp_dir().join(format!("composer-none-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("vendor/x")).unwrap();
    assert!(composer_dependency_roots(&dir).is_empty());
}

#[test]
fn installed_json_names_the_package_roots() {
    let dir = std::env::temp_dir().join(format!("composer-pkgs-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    write(&dir.join("composer.json"), "{}");
    write(
        &dir.join("vendor/composer/installed.json"),
        r#"{"packages":[
            {"name":"a/b","install-path":"../a/b"},
            {"name":"c/d","install-path":"../c/d"},
            {"name":"gone/pkg","install-path":"../gone/pkg"}
        ]}"#,
    );
    std::fs::create_dir_all(dir.join("vendor/a/b")).unwrap();
    std::fs::create_dir_all(dir.join("vendor/c/d")).unwrap();
    let roots = composer_dependency_roots(&dir);
    let canon = |p: &str| std::fs::canonicalize(dir.join(p)).unwrap();
    assert_eq!(roots, vec![canon("vendor/a/b"), canon("vendor/c/d")]);
}

#[test]
fn missing_install_map_degrades_to_vendor() {
    let dir = std::env::temp_dir().join(format!("composer-degrade-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    write(&dir.join("composer.json"), "{}");
    std::fs::create_dir_all(dir.join("vendor/x")).unwrap();
    assert_eq!(composer_dependency_roots(&dir), vec![dir.join("vendor")]);
}
