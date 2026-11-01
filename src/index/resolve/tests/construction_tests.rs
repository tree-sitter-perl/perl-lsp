//! A construction runs its class's constructor in every language that
//! keys constructions by class (`ConstructionKey::Class`): php's
//! `new Foo` and python's `Foo()` answer every feature the same way, from
//! the same fixture written in each.

use std::path::PathBuf;

use crate::index::open_docs::{FileKey, OpenDocs};
use crate::index::resolve::{resolve, CandidateSet, OverrideScope, RefLocation};

/// One language's workspace, on disk so a path-named language files its
/// modules as it would in a project.
struct Ws {
    root: PathBuf,
    idx: crate::index::module_index::ModuleIndex,
    store: OpenDocs,
    files: Vec<(PathBuf, &'static str)>,
}

impl Ws {
    fn new(tag: &str, files: &[(&str, &'static str)]) -> Self {
        // One tree per workspace: the tests run in parallel in one process.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("construction-{tag}-{}-{n}", std::process::id()));
        let drivers = crate::build::language_driver::LanguageRegistry::compiled();
        let idx = crate::index::module_index::ModuleIndex::new_for_test();
        let mut out = Vec::new();
        for (rel, src) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, src).unwrap();
            out.push((p, *src));
        }
        // Every file is on disk before any is analyzed: a package marker
        // names the modules beside it.
        for (p, src) in &out {
            if !src.is_empty() {
                idx.insert_workspace_for_test(p.clone(), drivers.for_path(p).unwrap().analyze_with_path(src, Some(p)));
            }
        }
        Ws { root, idx, store: OpenDocs::new(), files: out }
    }

    fn ask<T>(&self, file: &str, needle: &str, offset: usize, scope: OverrideScope, f: impl FnOnce(&CandidateSet) -> T) -> T {
        let (path, src) = self.files.iter().find(|(p, _)| p.ends_with(file)).expect("a workspace file");
        let fa = self.idx.workspace_file_for_test(path).unwrap();
        let i = src.find(needle).unwrap_or_else(|| panic!("`{needle}` in {file}")) + offset;
        let row = src[..i].matches('\n').count();
        let point = tree_sitter::Point { row, column: i - src[..i].rfind('\n').map_or(0, |n| n + 1) };
        f(&resolve(&self.store, &fa, FileKey::Path(crate::util::canonical_path::CanonicalPath::fixture(path.clone())), point, Some(&self.idx), scope))
    }

    fn rel(&self, key: &FileKey) -> String {
        let FileKey::Path(p) = key else { panic!("a workspace file") };
        p.strip_prefix(&self.root).unwrap().to_string_lossy().replace('\\', "/")
    }

    /// (file, row) of each location, sorted.
    fn sites(&self, locs: impl IntoIterator<Item = RefLocation>) -> Vec<(String, usize)> {
        let mut out: Vec<_> = locs.into_iter().map(|l| (self.rel(&l.key), l.span.start.row)).collect();
        out.sort();
        out.dedup();
        out
    }
}

impl Drop for Ws {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).ok();
    }
}

/// The fixture in one language: where its constructor, its classes and
/// its constructions are written. Rows match across the two languages.
struct Fixture {
    ws: Ws,
    /// The base file, the subclasses' file and the constructions' file.
    base: &'static str,
    models: &'static str,
    makes: &'static str,
    ctor: &'static str,
    /// A construction of `User` in `makes`, at the class token.
    user_call: &'static str,
}

fn php() -> Fixture {
    Fixture {
        ws: Ws::new(
            "php",
            &[
                ("Lib/Model.php", "<?php\nnamespace Lib;\nclass Model {\n    public function __construct($a = null) {}\n}\nfunction fresh() {\n    return new Model();\n}\n"),
                ("App/Models.php", "<?php\nnamespace App;\nuse Lib\\Model;\nclass User extends Model {\n}\nclass Post extends Model {\n    public function __construct() {}\n}\nclass Admin extends User {\n}\n"),
                ("App/Makes.php", "<?php\nnamespace App;\n// constructions\nuse Lib\\Model;\nfunction make() {\n    $u = new User();\n    $p = new Post();\n    $a = new Admin();\n    $m = new Model([]);\n}\n"),
            ],
        ),
        base: "Lib/Model.php",
        models: "App/Models.php",
        makes: "App/Makes.php",
        ctor: "__construct",
        user_call: "new User",
    }
}

fn python() -> Fixture {
    Fixture {
        ws: Ws::new(
            "python",
            &[
                ("lib/__init__.py", ""),
                ("lib/model.py", "#\n#\nclass Model:\n    def __init__(self, a=None):\n        pass\ndef fresh():\n    return Model()\n"),
                ("app/__init__.py", ""),
                ("app/models.py", "#\n#\nfrom lib.model import Model\nclass User(Model):\n    pass\nclass Post(Model):\n    def __init__(self):\n        pass\nclass Admin(User):\n    pass\n"),
                ("app/makes.py", "#\n#\nfrom app.models import User, Post, Admin\nfrom lib.model import Model\ndef make():\n    u = User()\n    p = Post()\n    a = Admin()\n    m = Model([])\n"),
            ],
        ),
        base: "lib/model.py",
        models: "app/models.py",
        makes: "app/makes.py",
        ctor: "__init__",
        user_call: "User()",
    }
}

fn both() -> [Fixture; 2] {
    [php(), python()]
}

fn the_constructor_of<T>(fx: &Fixture, scope: OverrideScope, f: impl FnOnce(&CandidateSet) -> T) -> T {
    fx.ws.ask(fx.base, fx.ctor, 0, scope, f)
}

/// A constructor's references are the constructions of the classes that
/// inherit it, from files that never spell its name; under the override
/// family a subclass's own constructor and its constructions join, under
/// dispatch only the constructions that run it.
#[test]
fn a_constructors_references_are_its_constructions() {
    for fx in both() {
        let (base, models, makes) = (fx.base.to_string(), fx.models.to_string(), fx.makes.to_string());
        let family = the_constructor_of(&fx, OverrideScope::Hierarchy, |cs| fx.ws.sites(cs.references()));
        assert_eq!(
            family,
            [
                (makes.clone(), 5),
                (makes.clone(), 6),
                (makes.clone(), 7),
                (makes.clone(), 8),
                (base.clone(), 3),
                (base.clone(), 6),
                (models.clone(), 6),
            ]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>(),
            "{} family",
            fx.ctor
        );
        let dispatch = the_constructor_of(&fx, OverrideScope::Dispatch, |cs| fx.ws.sites(cs.references()));
        assert_eq!(
            dispatch,
            [(makes.clone(), 5), (makes.clone(), 7), (makes.clone(), 8), (base.clone(), 3), (base.clone(), 6)]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>(),
            "{} dispatch",
            fx.ctor
        );
        // The subclass's own constructor runs for its own constructions only.
        let post = fx.ws.ask(fx.models, fx.ctor, 0, OverrideScope::Dispatch, |cs| fx.ws.sites(cs.references()));
        assert_eq!(post, [(makes.clone(), 6), (models.clone(), 6)], "{} on Post", fx.ctor);
    }
}

/// Highlights are the references image narrowed to the cursor's file: the
/// declaration and the construction beside it.
#[test]
fn a_constructors_highlights_take_in_its_files_constructions() {
    for fx in both() {
        let rows: Vec<usize> = the_constructor_of(&fx, OverrideScope::Hierarchy, |cs| {
            let mut rows: Vec<usize> = cs.highlights().into_iter().map(|l| l.span.start.row).collect();
            rows.sort();
            rows
        });
        assert_eq!(rows, [3, 6], "{}", fx.ctor);
    }
}

/// A construction's token spells its class, so renaming the constructor
/// would leave it behind: the rename refuses rather than edit part of the
/// set.
#[test]
fn a_constructor_rename_refuses() {
    for fx in both() {
        let edits = the_constructor_of(&fx, OverrideScope::Hierarchy, |cs| cs.rename_edits("renamed").map(|e| e.len()));
        assert!(edits.is_err(), "{}: {edits:?}", fx.ctor);
    }
}

/// Incoming calls group the constructions by the function they sit in.
#[test]
fn a_constructors_incoming_calls_are_its_constructions_callers() {
    for fx in both() {
        let mut callers: Vec<(String, usize)> = the_constructor_of(&fx, OverrideScope::Dispatch, |cs| {
            cs.incoming_calls().into_iter().map(|e| (e.item.name, e.sites.len())).collect()
        });
        callers.sort();
        let named = |n: &str| callers.iter().find(|(c, _)| c.ends_with(n)).map(|(_, k)| *k);
        assert_eq!((named("fresh"), named("make"), callers.len()), (Some(1), Some(3), 2), "{}: {callers:?}", fx.ctor);
    }
}

/// From a construction the cursor is on the class token: goto-def and
/// hover land on the class, as for any spelling of it.
#[test]
fn a_construction_site_lands_on_its_class() {
    for fx in both() {
        let models = fx.models.to_string();
        let defs = fx.ws.ask(fx.makes, fx.user_call, fx.user_call.find("User").unwrap(), OverrideScope::Hierarchy, |cs| {
            fx.ws.sites(cs.definitions())
        });
        assert_eq!(defs, [(models.clone(), 3)], "{}", fx.ctor);
        let hover = fx.ws.ask(fx.makes, fx.user_call, fx.user_call.find("User").unwrap(), OverrideScope::Hierarchy, |cs| {
            cs.hover_candidate().map(|l| (fx.ws.rel(&l.key), l.span.start.row))
        });
        assert_eq!(hover, Some((models, 3)), "{}", fx.ctor);
    }
}

/// The heatmap's fan-in is the references image, so a constructor's
/// constructions count and it is never a dead-code candidate for want of
/// a call spelling its name.
#[test]
fn a_constructors_fan_in_counts_its_constructions() {
    for fx in both() {
        let (path, _) = fx.ws.files.iter().find(|(p, _)| p.ends_with(fx.base)).unwrap();
        let fa = fx.ws.idx.workspace_file_for_test(path).unwrap();
        let sym = fa.symbols().iter().find(|s| s.name == fx.ctor).expect("the constructor");
        let row = crate::lsp::cli::heatmap_row_for_test(&fx.ws.store, &fx.ws.idx, path, &fa, sym);
        // `fresh` and `make`'s four constructions, from the override family
        // the default scope takes in: `Post`'s own construction joins.
        assert_eq!(row["fan_in"], 5, "{}: {row}", fx.ctor);
        assert_eq!(row["dead_code_candidate"], false, "{}", fx.ctor);
        assert_eq!(row["reachable_guard"], serde_json::Value::Null, "{}", fx.ctor);
    }
}

