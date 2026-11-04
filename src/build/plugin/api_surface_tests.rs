//! The plugin API's tripwire: every serde type in `plugin/mod.rs` is what a
//! plugin reads (a hook context) or writes (an emission, a manifest entry).
//! A change to any of them fails here until `api_surface.snap` is updated,
//! and the update is where `PLUGIN_API` gets decided: bump it when an old
//! plugin would read or write the new shape wrongly without an error.

use super::PLUGIN_API;

const SOURCE: &str = include_str!("mod.rs");
const SNAPSHOT_PATH: &str = "src/build/plugin/api_surface.snap";

/// The `pub struct` / `pub enum` items under a serde derive, comments and
/// blank lines dropped, so only a shape change moves the text.
fn api_surface() -> String {
    let lines: Vec<&str> = SOURCE.lines().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let starts_item = line.starts_with("pub struct ") || line.starts_with("pub enum ");
        if starts_item && serde_derived(&lines[..i]) {
            let mut j = i;
            loop {
                let l = lines[j].trim();
                if !l.is_empty() && !l.starts_with("//") {
                    out.push_str(l);
                    out.push('\n');
                }
                if lines[j] == "}" || (j == i && line.ends_with(';')) {
                    break;
                }
                j += 1;
            }
            out.push('\n');
            i = j;
        }
        i += 1;
    }
    out
}

/// Whether the attribute run just above an item derives a serde trait.
fn serde_derived(above: &[&str]) -> bool {
    above
        .iter()
        .rev()
        .take_while(|l| l.starts_with("#[") || l.starts_with("///"))
        .any(|l| l.starts_with("#[derive(") && (l.contains("Serialize") || l.contains("Deserialize")))
}

fn rendered() -> String {
    format!("PLUGIN_API {PLUGIN_API}\n\n{}", api_surface())
}

#[test]
fn plugin_api_surface_matches_its_snapshot() {
    let now = rendered();
    if std::env::var_os("PERL_LSP_UPDATE_API_SURFACE").is_some() {
        std::fs::write(SNAPSHOT_PATH, &now).unwrap();
        return;
    }
    let snap = std::fs::read_to_string(SNAPSHOT_PATH).unwrap_or_default();
    assert!(
        snap == now,
        "the plugin API surface changed (types in src/build/plugin/mod.rs). Decide whether an old \
         plugin would now be quietly wrong; if so bump PLUGIN_API and note it in \
         docs/PLUGIN_AUTHORING.md § API versions. Then rerun with PERL_LSP_UPDATE_API_SURFACE=1 \
         to record the new snapshot."
    );
}

#[test]
fn every_bundled_plugin_declares_this_api() {
    for (id, src) in super::rhai_host::bundled_sources() {
        assert!(
            src.lines().any(|l| l.trim() == format!("fn api() {{ {PLUGIN_API} }}")),
            "bundled plugin `{id}` must declare `fn api() {{ {PLUGIN_API} }}`"
        );
    }
}

#[test]
fn a_plugin_for_another_api_is_refused() {
    let engine = std::sync::Arc::new(super::rhai_host::make_engine());
    let src = format!("fn id() {{ \"later\" }}\nfn api() {{ {} }}\nfn triggers() {{ [] }}\n", PLUGIN_API + 1);
    let err = super::rhai_host::RhaiPlugin::from_source(&src, engine).err().expect("refused");
    assert!(err.contains("plugin API"), "{err}");
}

#[test]
fn a_plugin_without_api_reads_as_version_one() {
    let engine = std::sync::Arc::new(super::rhai_host::make_engine());
    let loaded = super::rhai_host::RhaiPlugin::from_source("fn id() { \"old\" }\nfn triggers() { [] }\n", engine);
    assert_eq!(loaded.is_ok(), PLUGIN_API == 1);
}

#[test]
fn a_refused_plugin_is_kept_by_the_registry() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-api-refuse-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("later.rhai"), format!("fn id() {{ \"later\" }}\nfn api() {{ {} }}\nfn triggers() {{ [] }}\n", PLUGIN_API + 1)).unwrap();
    let engine = std::sync::Arc::new(super::rhai_host::make_engine());
    let (loaded, refused) = super::rhai_host::load_plugin_dir(&dir, engine);
    std::fs::remove_dir_all(&dir).ok();
    assert!(loaded.is_empty());
    assert_eq!(refused.len(), 1);
    assert!(refused[0].contains("later.rhai") && refused[0].contains("plugin API"), "{refused:?}");
}
