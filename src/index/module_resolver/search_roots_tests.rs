//! Search roots files name, end to end: a registered file's `use lib`
//! (or `sys.path.insert`) joins the dependency map, an asker ranks it by
//! who names it, and an answer standing on another file's root says so.

use crate::index::module_index::ModuleIndex;
use crate::model::file_analysis::{CrossFileLookup, PackVisibility, ScopedLookup, VisibilityAxis};
use std::path::{Path, PathBuf};

fn tree(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("search-roots-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (rel, body) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    std::fs::canonicalize(&dir).unwrap()
}

fn indexed_perl(ws: &Path) -> ModuleIndex {
    let idx = ModuleIndex::new_for_test();
    super::index_workspace_with_index(ws, &idx, None, None);
    // What `cli_full_startup` asks once the walk is in.
    idx.request_dependency_demand();
    idx.wait_idle();
    idx
}

/// Where `asker` sees `module`, and the supplier stamp on that answer.
fn seen_by(idx: &ModuleIndex, asker: &Path, module: &str) -> (Option<PathBuf>, Vec<(String, PathBuf, bool)>) {
    let fa = idx.workspace_file_for_test(asker).unwrap();
    let asker = &crate::util::canonical_path::CanonicalPath::resolve(asker);
    let base: &dyn CrossFileLookup = idx;
    let axis = VisibilityAxis::for_origin(&fa, Some(asker), base, PackVisibility::Host);
    let scoped = ScopedLookup::new(base, &fa.pack.include_closure, Some(asker), axis);
    let winner = scoped.get_cached(module).map(|c| c.path.clone().into_path_buf());
    let supplied = fa
        .imports_supplied_by_other_roots(Some(asker), base, &scoped)
        .into_iter()
        .map(|s| (s.module, s.supplier.namer, s.supplier.stack))
        .collect();
    (winner, supplied)
}

const APP_PM: &str = "package My::App;\nuse My::Dep;\nsub run { My::Dep->go }\n1;\n";
const DEP_PM: &str = "package My::Dep;\nsub go { 1 }\n1;\n";

/// A module only a script's `use lib` reaches lands, resolves for a file
/// that script loads, and the answer names the script.
#[test]
fn a_module_only_a_scripts_use_lib_reaches_resolves_with_its_supplier() {
    let base = tree(
        "stack",
        &[
            ("ws/bin/app.pl", "use FindBin;\nuse lib \"$FindBin::Bin/../../ext/lib\";\nuse My::App;\nMy::App->run;\n"),
            ("ws/lib/My/App.pm", APP_PM),
            ("ext/lib/My/Dep.pm", DEP_PM),
        ],
    );
    let ws = base.join("ws");
    let idx = indexed_perl(&ws);
    let dep = base.join("ext/lib/My/Dep.pm");

    assert!(
        idx.def_candidates("My::Dep").iter().any(|c| c.path.as_path() == dep),
        "the root bin/app.pl names is searched, so My::Dep lands",
    );
    assert!(idx.is_dependency_path(&crate::util::canonical_path::CanonicalPath::resolve(&dep)));

    let app = ws.join("lib/My/App.pm");
    let (winner, supplied) = seen_by(&idx, &app, "My::Dep");
    assert_eq!(winner, Some(dep));
    assert_eq!(supplied, [("My::Dep".to_string(), ws.join("bin/app.pl"), true)]);

    let fa = idx.workspace_file_for_test(&app).unwrap();
    let diags = crate::lsp::symbols::search_root_diagnostics(&fa, Some(&crate::util::canonical_path::CanonicalPath::resolve(&app)), &idx);
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0].range.start.line, 1, "on the `use My::Dep` line");
    assert!(diags[0].message.contains("added by bin/app.pl"), "{}", diags[0].message);
    assert!(!diags[0].message.contains("does not load"), "app.pl loads App.pm: {}", diags[0].message);

    // The script's own root is its own: nothing to say there.
    let script = ws.join("bin/app.pl");
    let fa = idx.workspace_file_for_test(&script).unwrap();
    assert!(crate::lsp::symbols::search_root_diagnostics(&fa, Some(&crate::util::canonical_path::CanonicalPath::resolve(&script)), &idx).is_empty());
}

/// The root a file up the asker's stack names outranks one an unrelated
/// file names, though the unrelated one sorts first; a file nobody loads
/// still resolves, through the registered tier, and says so.
#[test]
fn the_stack_tier_beats_the_registered_tier() {
    let base = tree(
        "tiers",
        &[
            ("ws/bin/app.pl", "use FindBin;\nuse lib \"$FindBin::Bin/../../b-app\";\nuse My::App;\n"),
            ("ws/bin/other.pl", "use FindBin;\nuse lib \"$FindBin::Bin/../../a-other\";\n1;\n"),
            ("ws/lib/My/App.pm", APP_PM),
            ("ws/lib/My/Lone.pm", "package My::Lone;\nuse My::Dep;\n1;\n"),
            ("a-other/My/Dep.pm", DEP_PM),
            ("b-app/My/Dep.pm", DEP_PM),
        ],
    );
    let ws = base.join("ws");
    let idx = indexed_perl(&ws);
    assert_eq!(idx.def_candidates("My::Dep").len(), 2, "both roots' copies land");

    let (winner, supplied) = seen_by(&idx, &ws.join("lib/My/App.pm"), "My::Dep");
    assert_eq!(winner, Some(base.join("b-app/My/Dep.pm")), "app.pl loads App.pm");
    assert_eq!(supplied, [("My::Dep".to_string(), ws.join("bin/app.pl"), true)]);

    let (winner, supplied) = seen_by(&idx, &ws.join("lib/My/Lone.pm"), "My::Dep");
    assert_eq!(winner, Some(base.join("a-other/My/Dep.pm")), "no stack: the first registered root");
    assert_eq!(supplied, [("My::Dep".to_string(), ws.join("bin/other.pl"), false)]);
}

/// A test's `use lib 'lib'` names the project's own lib, which every file
/// searches already: nothing resolved there is another file's answer.
#[test]
fn naming_a_process_root_supplies_nothing() {
    let base = tree(
        "process",
        &[
            ("ws/t/basic.t", "use lib 'lib';\nuse My::App;\n"),
            ("ws/lib/My/App.pm", "package My::App;\nuse My::Other;\n1;\n"),
            ("ws/lib/My/Other.pm", "package My::Other;\n1;\n"),
        ],
    );
    let ws = base.join("ws");
    let idx = indexed_perl(&ws);
    assert!(!idx.root_registry().named.is_empty(), "t/basic.t names lib");
    let app = ws.join("lib/My/App.pm");
    let (winner, supplied) = seen_by(&idx, &app, "My::Other");
    assert_eq!(winner, Some(ws.join("lib/My/Other.pm")));
    assert!(supplied.is_empty(), "{supplied:?}");
}

/// A test's `use lib 't/lib'` mock answers the test itself, but never
/// shadows the project's own copy for a file the test loads: that file
/// runs under every other caller too.
#[test]
fn a_borrowed_root_never_shadows_the_projects_code() {
    let base = tree(
        "mock",
        &[
            ("ws/t/basic.t", "use lib 't/lib';\nuse My::App;\nuse My::Dep;\n"),
            ("ws/t/lib/My/Dep.pm", DEP_PM),
            ("ws/lib/My/App.pm", APP_PM),
            ("ws/lib/My/Dep.pm", DEP_PM),
        ],
    );
    let ws = base.join("ws");
    let idx = indexed_perl(&ws);

    let (winner, supplied) = seen_by(&idx, &ws.join("t/basic.t"), "My::Dep");
    assert_eq!(winner, Some(ws.join("t/lib/My/Dep.pm")), "the test's own root");
    assert!(supplied.is_empty(), "{supplied:?}");

    let (winner, supplied) = seen_by(&idx, &ws.join("lib/My/App.pm"), "My::Dep");
    assert_eq!(winner, Some(ws.join("lib/My/Dep.pm")), "the project's code beats a borrowed root");
    assert!(supplied.is_empty(), "{supplied:?}");
}

/// A patched copy of an installed module under a script's `use lib`
/// beats the installed copy for the files the script loads, and for a
/// file nobody loads (the registered tier), each stamped.
#[test]
fn a_borrowed_root_outranks_an_installed_copy() {
    let base = tree(
        "patch",
        &[
            ("ws/bin/app.pl", "use FindBin;\nuse lib \"$FindBin::Bin/../patches\";\nuse My::App;\n"),
            ("ws/lib/My/App.pm", APP_PM),
            ("ws/lib/My/Lone.pm", "package My::Lone;\nuse My::Dep;\n1;\n"),
            ("ws/patches/My/Dep.pm", DEP_PM),
            ("ws/local/lib/perl5/My/Dep.pm", DEP_PM),
        ],
    );
    let ws = base.join("ws");
    let idx = indexed_perl(&ws);
    let patched = ws.join("patches/My/Dep.pm");

    let (winner, supplied) = seen_by(&idx, &ws.join("lib/My/App.pm"), "My::Dep");
    assert_eq!(winner, Some(patched.clone()), "app.pl loads App.pm");
    assert_eq!(supplied, [("My::Dep".to_string(), ws.join("bin/app.pl"), true)]);

    let (winner, supplied) = seen_by(&idx, &ws.join("lib/My/Lone.pm"), "My::Dep");
    assert_eq!(winner, Some(patched));
    assert_eq!(supplied, [("My::Dep".to_string(), ws.join("bin/app.pl"), false)]);
}

/// A root stays while any file names it; when the last one drops it, what
/// landed under it retires.
#[test]
fn the_last_file_dropping_a_root_retires_its_landings() {
    let base = tree(
        "retire",
        &[
            ("ws/bin/one.pl", "use FindBin;\nuse lib \"$FindBin::Bin/../../ext\";\nuse My::Dep;\n"),
            ("ws/bin/two.pl", "use FindBin;\nuse lib \"$FindBin::Bin/../../ext\";\nuse My::Dep;\n"),
            ("ext/My/Dep.pm", DEP_PM),
        ],
    );
    let ws = base.join("ws");
    let idx = indexed_perl(&ws);
    let landed = |idx: &ModuleIndex| idx.def_candidates("My::Dep").len();
    assert_eq!(landed(&idx), 1);

    idx.unregister_workspace_path(&ws.join("bin/one.pl"));
    idx.wait_idle();
    assert_eq!(landed(&idx), 1, "two.pl still names the root");

    idx.unregister_workspace_path(&ws.join("bin/two.pl"));
    idx.wait_idle();
    assert_eq!(landed(&idx), 0, "nobody names the root: its landing retires");
}

/// Python: a script's `sys.path.insert` joining `__file__`'s directory
/// is a root of the tier, and a module only it reaches lands.
#[cfg(feature = "python")]
#[test]
fn a_python_sys_path_root_lands_what_only_it_reaches() {
    let python = crate::build::language_driver::LanguageRegistry::compiled().for_id("python").expect("python driver");
    let base = tree(
        "python",
        &[
            (
                "ws/scripts/run.py",
                "import os\nimport sys\nsys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '..', 'pyext'))\nimport extmod\n\nclass R(extmod.Ext):\n    def go(self):\n        self.hi()\n",
            ),
            ("pyext/extmod.py", "class Ext:\n    def hi(self):\n        pass\n"),
        ],
    );
    let ws = base.join("ws");
    let idx = ModuleIndex::new_for_test();
    super::index_pack_language(python, &ws, None, &idx, None, 0);
    idx.wait_idle();
    let ext = base.join("pyext/extmod.py");
    assert!(idx.def_candidates("extmod.Ext").iter().any(|c| c.path.as_path() == ext), "the script's root places extmod");
    assert!(idx.is_dependency_path(&crate::util::canonical_path::CanonicalPath::resolve(&ext)));
}

/// The python forms that mint a root, and the computed ones that do not.
#[cfg(feature = "python")]
#[test]
fn python_search_roots_are_minted_from_literals_and_file_joins() {
    use crate::model::file_analysis::{RootAnchor as A, SearchRoot};
    let src = "import os, sys\n\
        sys.path.insert(0, '/opt/lib')\n\
        sys.path.append(\"vendor\")\n\
        sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', 'lib'))\n\
        sys.path.append(os.path.dirname(os.path.realpath(__file__)))\n\
        sys.path.insert(0, os.path.abspath(os.path.join(os.path.dirname(__file__), 'src')))\n\
        sys.path.insert(0, f\"{HOME}/x\")\n\
        sys.path.insert(0, os.environ['ROOT'])\n\
        sys.path.append(os.path.dirname(os.path.dirname(__file__)))\n";
    let python = crate::build::language_driver::LanguageRegistry::compiled().for_id("python").expect("python driver");
    let fa = python.analyze_with_path(src, None);
    let r = |anchor, path: &str| SearchRoot { anchor, path: path.to_string() };
    assert_eq!(
        fa.search_roots,
        vec![
            r(A::Absolute, "/opt/lib"),
            r(A::WorkingDir, "vendor"),
            r(A::FileDir, "../lib"),
            r(A::FileDir, ""),
            r(A::FileDir, "src"),
        ],
    );
}
