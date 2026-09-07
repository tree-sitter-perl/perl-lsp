//! Composer's install map — the PHP dependency tier's root discovery.
//!
//! A composer project gitignores `vendor/`, so the ignore-aware workspace
//! walk never sees the packages the project actually calls into; the
//! dependency roots must be declared, not discovered. Two sources, both
//! plain JSON: the project's own `composer.json` gates the feature (a
//! `vendor/` dir without one is not composer's), and composer 2's
//! `vendor/composer/installed.json` names every installed package with its
//! `install-path` — the authoritative per-package roots, which keeps
//! `vendor/composer`'s autoload plumbing and `vendor/bin` out of the walk.
//! A vendor tree without `installed.json` (a hand-materialized fixture, an
//! ancient composer) degrades to the whole `vendor/` dir.

use std::path::{Path, PathBuf};

/// The dependency roots a composer project declares: one directory per
/// installed package (or the whole `vendor/` when the install map is
/// absent). Empty when `root` is not a composer project or has no vendor
/// tree — the caller walks nothing extra.
pub fn composer_dependency_roots(root: &Path) -> Vec<PathBuf> {
    if !root.join("composer.json").is_file() {
        return Vec::new();
    }
    let vendor = root.join("vendor");
    if !vendor.is_dir() {
        return Vec::new();
    }
    let installed = vendor.join("composer").join("installed.json");
    let packages = std::fs::read_to_string(&installed)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .map(|v| {
            // composer 2 wraps the list in {"packages": [...]}; composer 1
            // wrote the bare array. Accept both — the field read is the same.
            let list = v
                .get("packages")
                .and_then(|p| p.as_array().cloned())
                .or_else(|| v.as_array().cloned())
                .unwrap_or_default();
            list.iter()
                .filter_map(|p| p.get("install-path").and_then(|ip| ip.as_str()).map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if packages.is_empty() {
        return vec![vendor];
    }
    let base = vendor.join("composer");
    let mut out: Vec<PathBuf> = packages
        .iter()
        // `install-path` is relative to vendor/composer (`../monolog/monolog`).
        .map(|ip| base.join(ip))
        .filter_map(|p| std::fs::canonicalize(&p).ok())
        .filter(|p| p.is_dir())
        .collect();
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
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
}
