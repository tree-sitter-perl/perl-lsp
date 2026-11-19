//! The map locates by lane order and names what a file loads as.

use super::*;

fn spelling() -> ModuleSpelling {
    ModuleSpelling {
        module_paths: |m| vec![format!("{}.pm", m.replace("::", "/"))],
        module_of: |rel| {
            rel.extension()
                .is_some_and(|e| e == "pm")
                .then(|| rel.with_extension("").to_string_lossy().replace('/', "::"))
        },
        member_of: |_| None,
    }
}

fn tree(tag: &str, files: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dependency-map-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for rel in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "1;\n").unwrap();
    }
    CanonicalPath::resolve(dir).into_path_buf()
}

fn at(dir: &Path, rel: &str, name: Option<&str>) -> Located {
    Located { path: CanonicalPath::resolve(dir.join(rel)), loads_as: name.map(str::to_string) }
}

#[test]
fn a_name_s_files_come_back_in_lane_order_then_each_lane_s_order() {
    let dir = tree("order", &["table/Pinned.pm", "a/A/B.pm", "b/A/B.pm", "src/B.pm"]);
    let map = DependencyMap::new(
        vec![
            Lane::Table(vec![("A::B".into(), dir.join("table/Pinned.pm"))]),
            Lane::Prefix { prefix: "A::".into(), dirs: vec![dir.join("src")], spelling: spelling(), spells: Spells::Remainder },
            Lane::SearchRoots { roots: vec![dir.join("a"), dir.join("b"), dir.join("a")], spelling: spelling() },
            Lane::AlwaysLoad(vec![dir.join("table/Pinned.pm")]),
        ],
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(
        map.locate(&Ask::Name("A::B".into())),
        [
            at(&dir, "table/Pinned.pm", Some("A::B")),
            at(&dir, "src/B.pm", Some("A::B")),
            at(&dir, "a/A/B.pm", Some("A::B")),
            at(&dir, "b/A/B.pm", Some("A::B")),
        ],
        "a root listed twice places its file once",
    );
    assert!(map.locate(&Ask::Name("C".into())).is_empty());
    assert_eq!(*map.search_roots(), [dir.join("a"), dir.join("b")]);
}

#[test]
fn a_file_ask_names_what_the_file_loads_as_in_each_lane() {
    let dir = tree("file", &["inc/X/Y.pm", "boot.pm"]);
    let map = DependencyMap::new(
        vec![
            Lane::Table(vec![("Alias".into(), dir.join("inc/X/Y.pm"))]),
            Lane::Prefix { prefix: "P::".into(), dirs: vec![dir.join("inc")], spelling: spelling(), spells: Spells::Remainder },
            Lane::SearchRoots { roots: vec![dir.join("inc")], spelling: spelling() },
            Lane::AlwaysLoad(vec![dir.join("boot.pm")]),
        ],
        Vec::new(),
        Vec::new(),
    );
    let y = CanonicalPath::resolve(dir.join("inc/X/Y.pm"));
    assert_eq!(
        map.locate(&Ask::File(y)),
        [at(&dir, "inc/X/Y.pm", Some("Alias")), at(&dir, "inc/X/Y.pm", Some("P::X::Y")), at(&dir, "inc/X/Y.pm", Some("X::Y"))],
    );
    assert_eq!(map.locate(&Ask::File(CanonicalPath::resolve(dir.join("boot.pm")))), [at(&dir, "boot.pm", None)]);
}

#[test]
fn the_generation_moves_with_an_input_s_stamp_and_the_tier_owns_its_roots() {
    let dir = tree("gen", &["local/lib/X.pm", "lib/Y.pm"]);
    let manifest = dir.join("manifest");
    std::fs::write(&manifest, "a").unwrap();
    let map = || DependencyMap::new(Vec::new(), vec![dir.join("local")], vec![manifest.clone()]);
    let before = map().generation();
    assert_eq!(before, map().generation(), "a re-read of the same map is the same generation");
    std::fs::write(&manifest, "ab").unwrap();
    assert_ne!(before, map().generation());
    assert!(map().owns(&dir.join("local/lib/X.pm")));
    assert!(!map().owns(&dir.join("lib/Y.pm")));
}

#[test]
fn a_whole_name_prefix_lane_spells_the_prefix_too_and_places_only_names_under_it() {
    let dir = tree("whole", &["lib/Old/Tool.pm", "lib/New/Tool.pm"]);
    let map = DependencyMap::new(
        vec![Lane::Prefix { prefix: "Old::".into(), dirs: vec![dir.join("lib")], spelling: spelling(), spells: Spells::WholeName }],
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(map.locate(&Ask::Name("Old::Tool".into())), [at(&dir, "lib/Old/Tool.pm", Some("Old::Tool"))]);
    assert!(map.locate(&Ask::Name("New::Tool".into())).is_empty());
    let old = CanonicalPath::resolve(dir.join("lib/Old/Tool.pm"));
    assert_eq!(map.locate(&Ask::File(old)), [at(&dir, "lib/Old/Tool.pm", Some("Old::Tool"))]);
    let new = CanonicalPath::resolve(dir.join("lib/New/Tool.pm"));
    assert!(map.locate(&Ask::File(new)).is_empty(), "a file whose name is not under the prefix is not the lane's");
}

#[test]
fn a_table_keeps_a_name_s_first_row() {
    let dir = tree("table", &["first.pm", "second.pm"]);
    let map = DependencyMap::new(
        vec![Lane::Table(vec![
            ("Z".into(), dir.join("second.pm")),
            ("A".into(), dir.join("first.pm")),
            ("A".into(), dir.join("second.pm")),
        ])],
        Vec::new(),
        Vec::new(),
    );
    assert_eq!(map.locate(&Ask::Name("A".into())), [at(&dir, "first.pm", Some("A"))]);
    assert_eq!(map.locate(&Ask::Name("Z".into())), [at(&dir, "second.pm", Some("Z"))]);
}
