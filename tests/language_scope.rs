//! A CLI verb indexes only the language families its answer can consult.
//!
//! The asymmetry these tests guard: over-indexing is wasted work, but
//! under-indexing is a WRONG answer and a quiet one — an unattached pack
//! sub-index does not answer empty, `lookup_for` routes that language's
//! queries to the Perl hub instead. So the C++ cases assert a real
//! cross-file answer, not merely a non-empty one.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_perl-lsp");

/// A workspace with both families: a Perl module chain and a C++ class
/// split across header and body, plus a consumer that must resolve across
/// files.
fn mixed_workspace(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("perl-lsp-scope-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("lib/Syn")).unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();

    std::fs::write(
        dir.join("lib/Syn/Alpha.pm"),
        "package Syn::Alpha;\nsub new { bless {}, shift }\nsub alpha_value { 41 }\n1;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("lib/Syn/Beta.pm"),
        "package Syn::Beta;\nuse Syn::Alpha;\nsub beta_call { Syn::Alpha->new->alpha_value }\n1;\n",
    )
    .unwrap();

    std::fs::write(
        dir.join("src/widget.h"),
        "#ifndef WIDGET_H\n#define WIDGET_H\nclass Widget {\npublic:\n  int compute_area();\n  int width;\n};\n#endif\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/widget.cc"),
        "#include \"widget.h\"\nint Widget::compute_area() { return width * 2; }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/consumer.cc"),
        "#include \"widget.h\"\nint run_it() {\n  Widget w;\n  return w.compute_area();\n}\n",
    )
    .unwrap();
    dir
}

fn run(root: &std::path::Path, args: &[&str]) -> (String, String) {
    let mut cache = root.to_path_buf();
    cache.push(".test-cache");
    let out = Command::new(BIN)
        .args(args)
        .current_dir(root)
        .env("XDG_CACHE_HOME", &cache)
        .output()
        .expect("run perl-lsp");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[cfg(feature = "cpp")]
#[test]
fn a_cpp_query_still_resolves_cross_file_without_the_perl_tree() {
    // THE trap. Scoping startup must not turn a correct C++ answer into a
    // degraded one — and because an unattached pack index falls back to the
    // Perl hub rather than answering nothing, "non-empty" is not enough:
    // the answer has to name the header AND the body.
    let dir = mixed_workspace("cpp");
    let consumer = dir.join("src/consumer.cc");
    let (stdout, stderr) = run(
        &dir,
        &["--definition", dir.to_str().unwrap(), consumer.to_str().unwrap(), "3", "12"],
    );
    assert!(
        stdout.contains("widget.h"),
        "cross-file C++ goto-def lost the declaration.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("widget.cc"),
        "cross-file C++ goto-def lost the definition.\nstdout: {stdout}\nstderr: {stderr}"
    );
    // And it got there without walking the Perl tree.
    assert!(
        !stderr.contains("Indexed 2 Perl files"),
        "a C++ query indexed the Perl family: {stderr}"
    );
}

#[test]
fn a_perl_query_does_not_index_the_pack_family() {
    // The row this exists for: on a CPAN-shaped tree the XS/C files beside
    // the .pm files are a small minority of the files and a majority of the
    // startup, and no Perl answer can consult them.
    let dir = mixed_workspace("perl");
    let beta = dir.join("lib/Syn/Beta.pm");
    let (stdout, stderr) = run(
        &dir,
        &["--definition", dir.to_str().unwrap(), beta.to_str().unwrap(), "1", "6"],
    );
    assert!(
        stdout.contains("Alpha.pm"),
        "Perl goto-def regressed.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stderr.contains("C/C++ files"),
        "a Perl query indexed the pack family: {stderr}"
    );
}

#[cfg(feature = "cpp")]
#[test]
fn a_whole_workspace_verb_still_sees_every_family() {
    // `workspace/symbol` fans across the hub AND every pack sub-index, so
    // its scope stays All — scoping the positional verbs must not quietly
    // narrow the sweeping ones.
    let dir = mixed_workspace("ws");
    let (stdout, stderr) = run(
        &dir,
        &["--workspace-symbol", dir.to_str().unwrap(), "a"],
    );
    assert!(
        stdout.contains("Widget") || stdout.contains("compute_area"),
        "workspace-symbol lost the pack family.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("Syn::Alpha") || stdout.contains("alpha_value"),
        "workspace-symbol lost the Perl family.\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[cfg(feature = "php")]
fn php_workspace(tag: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("perl-lsp-scope-php-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/Greeter.php"),
        "<?php\nclass Greeter\n{\n    public string $prefix;\n\n    public function greet(string $name): string\n    {\n        return $this->prefix . $name;\n    }\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.php"),
        "<?php\n$g = new Greeter();\necho $g->greet(\"x\");\necho $g->greet(\"y\");\n",
    )
    .unwrap();
    dir
}

/// The round-1 regression class, pinned through the REAL CLI (the unit net
/// missed it because store-level tests pass no index, so no visibility
/// axis ever gated them): a name-keyed pack's cross-file goto-def and
/// references must cross the file boundary.
#[cfg(feature = "php")]
#[test]
fn a_php_query_resolves_cross_file_through_the_ctor_typed_receiver() {
    let dir = php_workspace("gd");
    let main = dir.join("src/main.php");
    // gd on `greet` in `$g->greet("x")` — the receiver types via the
    // structural ctor edge, the method resolves in the OTHER file.
    let (stdout, stderr) = run(
        &dir,
        &["--definition", dir.to_str().unwrap(), main.to_str().unwrap(), "2", "9"],
    );
    assert!(
        stdout.contains("Greeter.php"),
        "cross-file PHP goto-def went dark.\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// An INHERITED static resolved through a bareword-scoped call
/// (`Widget::query()` where `query` lives on the parent in another
/// file) — the member lookup walks the leaf-keyed parent edges; the
/// instance-receiver path never covered this lane (round-3 follow-on:
/// Laravel's `Model::query()` shape).
#[cfg(feature = "php")]
#[test]
fn php_inherited_static_resolves_through_parent_file() {
    let dir = php_workspace("inhstatic");
    std::fs::write(
        dir.join("src/BaseModel.php"),
        "<?php\nclass BaseModel\n{\n    public static function query(): string\n    {\n        return \"q\";\n    }\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/widget.php"),
        "<?php\nclass Widget extends BaseModel\n{\n}\n$q = Widget::query();\n",
    )
    .unwrap();
    let widget = dir.join("src/widget.php");
    // 0-based positional coords: line 4, col 13 = the `query` token.
    let (stdout, stderr) = run(
        &dir,
        &["--definition", dir.to_str().unwrap(), widget.to_str().unwrap(), "4", "13"],
    );
    assert!(
        stdout.contains("BaseModel.php"),
        "inherited-static goto-def went dark.\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[cfg(feature = "php")]
#[test]
fn php_references_cross_the_file_boundary() {
    let dir = php_workspace("refs");
    let greeter = dir.join("src/Greeter.php");
    // references on the `greet` DECLARATION (0-based row 5, col 20).
    let (stdout, stderr) = run(
        &dir,
        &["--references", dir.to_str().unwrap(), greeter.to_str().unwrap(), "5", "20"],
    );
    let hits = stdout.matches("main.php").count();
    assert!(
        hits >= 2,
        "expected both call sites in main.php, got {hits}.\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// The composer vendor tier: a gitignored `vendor/` is invisible to the
/// ignore-aware walk, so the php driver's dependency roots must carry it —
/// visible to gd/references, read-only for rename.
#[cfg(feature = "php")]
fn composer_workspace(tag: &str) -> std::path::PathBuf {
    let dir =
        std::env::temp_dir().join(format!("perl-lsp-scope-vendor-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("vendor/acme/widgets/src")).unwrap();
    std::fs::create_dir_all(dir.join("vendor/composer")).unwrap();
    std::fs::write(dir.join(".gitignore"), "vendor/\n").unwrap();
    // the walk honors .gitignore only inside a repo — make it one
    let _ = std::process::Command::new("git").args(["init", "-q"]).current_dir(&dir).output();
    std::fs::write(
        dir.join("composer.json"),
        r#"{"autoload":{"psr-4":{"App\\":"src/"}},"require":{"acme/widgets":"^1.0"}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("vendor/composer/installed.json"),
        r#"{"packages":[{"name":"acme/widgets","install-path":"../acme/widgets"}]}"#,
    )
    .unwrap();
    std::fs::write(
        dir.join("vendor/acme/widgets/src/Widget.php"),
        "<?php\nnamespace Acme\\Widgets;\n\nclass Widget\n{\n    public function spin(): string\n    {\n        return \"spun\";\n    }\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/App.php"),
        "<?php\nnamespace App;\n\nuse Acme\\Widgets\\Widget;\n\nclass App\n{\n    public function run(): string\n    {\n        $w = new Widget();\n        return $w->spin();\n    }\n}\n",
    )
    .unwrap();
    dir
}

#[cfg(feature = "php")]
#[test]
fn composer_vendor_resolves_but_never_rewrites() {
    let dir = composer_workspace("main");
    let app = dir.join("src/App.php");
    // gd on `spin` in `$w->spin()` (0-based row 10, col 21) lands in vendor.
    let (stdout, stderr) = run(
        &dir,
        &["--definition", dir.to_str().unwrap(), app.to_str().unwrap(), "10", "21"],
    );
    assert!(
        stdout.contains("Widget.php"),
        "gd into the vendor tier went dark.\nstdout: {stdout}\nstderr: {stderr}"
    );
    // rename of the vendor-declared method edits ONLY editable space.
    let (stdout, stderr) = run(
        &dir,
        &[
            "--rename",
            dir.to_str().unwrap(),
            "--at",
            "src/App.php:11:20",
            "whirl",
        ],
    );
    assert!(
        stdout.contains("App.php"),
        "the app call site must be in the edit set.\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.contains("Widget.php"),
        "a rename must NEVER rewrite the vendor tier.\nstdout: {stdout}\nstderr: {stderr}"
    );
}

/// Round-4 H8: `parent::` through a same-leaf ALIASED parent
/// (`use Support\Collection as BaseCollection; class Collection extends
/// BaseCollection`) must land on the concrete parent's method — never the
/// child's own override (the leaf-keyed walk collapsed the parent into
/// the origin) and never a deeper interface's abstract stub.
#[cfg(feature = "php")]
#[test]
fn php_parent_call_through_same_leaf_aliased_parent() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-h8-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Enumerable.php"),
        "<?php\nnamespace Illuminate\\Support;\ninterface Enumerable {\n    public function map(callable $cb);\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("BaseCollection.php"),
        "<?php\nnamespace Illuminate\\Support;\nclass Collection implements Enumerable {\n    public function map(callable $cb) { return $this; }\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("EloquentCollection.php"),
        "<?php\nnamespace Illuminate\\Database\\Eloquent;\nuse Illuminate\\Support\\Collection as BaseCollection;\nclass Collection extends BaseCollection {\n    public function map(callable $cb) {\n        return parent::map($cb);\n    }\n}\n",
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
        .args(["--definition", dir.to_str().unwrap(), "EloquentCollection.php", "5", "24"])
        .env("XDG_CACHE_HOME", dir.join(".cache"))
        .output()
        .expect("run gd");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("BaseCollection.php:3"),
        "parent::map lands on the concrete parent: {stdout}"
    );
    assert!(
        !stdout.contains("Enumerable.php") && !stdout.contains("EloquentCollection.php"),
        "never the interface stub or the child's own override: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// References on a TRAIT method must admit call sites that reach the trait
/// through a consumer's `use Trait` edge — including a site whose receiver
/// is an inline CHAIN (`(new Coll(...))->wrapUp(...)->eachSpread(...)`).
/// The chain case regressed silently: the invocant ladder's bareword
/// terminal minted the receiver EXPRESSION text as a ClassName, the frozen
/// garbage edge read as a baked verdict, and the matcher never re-resolved
/// with the index. gd must also prefer the trait's concrete method over an
/// interface's abstract stub declaring the same name.
#[cfg(feature = "php")]
#[test]
fn php_trait_method_refs_admit_chained_consumer_call_sites() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-h2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Helper.php"),
        "<?php\nnamespace App\\Traits;\ntrait Helper\n{\n    public function eachSpread(callable $cb): void {}\n\n    /**\n     * @return static\n     */\n    public function wrapUp(array $items)\n    {\n        return $this;\n    }\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("En.php"),
        "<?php\nnamespace App;\ninterface En\n{\n    public function eachSpread(callable $cb);\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Coll.php"),
        "<?php\nnamespace App;\nuse App\\Traits\\Helper;\nclass Coll implements En\n{\n    use Helper;\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Chain.php"),
        "<?php\nnamespace App;\nfunction run(): void\n{\n    (new Coll([1]))\n        ->wrapUp([2])\n        ->eachSpread(fn ($x) => $x);\n}\n",
    )
    .unwrap();
    // refs from the trait decl: the chained call site must be admitted.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
        .args(["--references", dir.to_str().unwrap(), "Helper.php", "4", "20"])
        .env("XDG_CACHE_HOME", dir.join(".cache"))
        .output()
        .expect("run refs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Chain.php"),
        "chained consumer call site admitted: {stdout}"
    );
    // gd from the chained call site: the trait's concrete method, not the
    // interface stub.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
        .args(["--definition", dir.to_str().unwrap(), "Chain.php", "6", "11"])
        .env("XDG_CACHE_HOME", dir.join(".cache"))
        .output()
        .expect("run gd");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Helper.php:4") && !stdout.contains("En.php"),
        "concrete trait method over interface stub: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A Doctrine-style instance `@method` row is a real declaration: the
/// synthesized symbol spans the NAME TOKEN in the doc line, so references
/// from the token collect typed call sites, and rename rewrites the doc
/// row together with the calls. (The facade `@method static` lane proved
/// the dispatch half; the decl-token half was dark while the symbol sat
/// zero-width at column 0.)
#[cfg(feature = "php")]
#[test]
fn php_doc_method_decl_token_collects_typed_call_sites() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-h9-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Post.php"),
        "<?php\nnamespace App;\nclass Post {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("PostRepository.php"),
        "<?php\nnamespace App;\n/**\n * @method Post|null findOneByTitle(string $postTitle)\n */\nclass PostRepository\n{\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Caller.php"),
        "<?php\nnamespace App;\nfunction probe(PostRepository $repo): void\n{\n    $post = $repo->findOneByTitle('x');\n}\n",
    )
    .unwrap();
    // refs from the @method name token (0-based 3:21).
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
        .args(["--references", dir.to_str().unwrap(), "PostRepository.php", "3", "21"])
        .env("XDG_CACHE_HOME", dir.join(".cache"))
        .output()
        .expect("run refs");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Caller.php") && stdout.contains("PostRepository.php"),
        "doc-method decl + typed call site: {stdout}"
    );
    // gd from the call site lands on the doc name token.
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
        .args(["--definition", dir.to_str().unwrap(), "Caller.php", "4", "19"])
        .env("XDG_CACHE_HOME", dir.join(".cache"))
        .output()
        .expect("run gd");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("PostRepository.php:3:21"),
        "gd lands on the @method name token: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Hover on a method resolved in ANOTHER file renders like a local one —
/// the defining file's signature line, labeled `*method*` — never the
/// kind-agnostic `name: type` member fallback (which dropped the
/// signature and read a vendor method as a property).
#[cfg(feature = "php")]
#[test]
fn php_cross_file_method_hover_renders_the_signature() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-h12-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Model.php"),
        "<?php\nnamespace App;\nclass Model\n{\n    public function save(array $options = []): bool\n    {\n        return true;\n    }\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Repo.php"),
        "<?php\nnamespace App;\nfunction store(Model $m): void\n{\n    $m->save();\n}\n",
    )
    .unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
        .args(["--hover", dir.to_str().unwrap(), "Repo.php", "4", "8"])
        .env("XDG_CACHE_HOME", dir.join(".cache"))
        .output()
        .expect("run hover");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("function save(array $options = [])") && stdout.contains("*method*"),
        "cross-file method hover shows the signature: {stdout}"
    );
    assert!(!stdout.contains("*member*"), "never the member fallback: {stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A constructor's references are the class's `new Foo(...)` sites, in
/// OTHER files too: the relational retrieval keys on the class name for a
/// ctor target (its call sites never spell `__construct`), and the heatmap
/// counts them as fan-in. `$this` hovers as the enclosing class.
#[cfg(feature = "php")]
#[test]
fn php_constructor_references_reach_new_sites_across_files() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-r5ctor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Src.php"), "<?php\nnamespace App;\nclass Src { public function __construct(private string $p) {} public function go(): void { $this->p; } }\n").unwrap();
    std::fs::write(dir.join("Mk.php"), "<?php\nnamespace App;\nfunction mk(): Src { return new Src(\"x\"); }\n").unwrap();
    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
            .args(args).env("XDG_CACHE_HOME", dir.join(".cache")).output().expect("run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let refs = run(&["--references", dir.to_str().unwrap(), "Src.php", "2", "28"]);
    assert!(refs.contains("Mk.php"), "cross-file `new Src(` admitted: {refs}");
    let heat = run(&["--heatmap", dir.to_str().unwrap()]);
    let v: serde_json::Value = serde_json::from_str(&heat).expect("heatmap json");
    let ctor = v["symbols"].as_array().unwrap().iter().find(|s| s["name"] == "__construct").unwrap();
    assert_eq!(ctor["dead_code_candidate"], false, "ctor with a `new` site is live: {ctor}");
    let src_line = "class Src { public function __construct(private string $p) {} public function go(): void { $this->p; } }";
    let col = (src_line.find("$this").unwrap() + 1).to_string();
    let hover = run(&["--hover", dir.to_str().unwrap(), "Src.php", "2", &col]);
    assert!(hover.contains("$this: Src"), "`$this` hovers as the enclosing class: {hover}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Round-5 R5-1: three same-leaf classes, one `use` row. A name-keyed
/// origin's visibility is its OWN use-map (`VisibilityAxis::UseMap`):
/// every shape — the type hint, `new Collection`, the typed receivers —
/// lands on the imported `B\Collection`, and the references/rename walk
/// keeps the stranger `A\Collection`'s files out on BOTH sides (a file in
/// namespace `A` with no `use` means `A\Collection` by the language's
/// rule, so it never joins B's references).
#[cfg(feature = "php")]
#[test]
fn php_use_map_picks_the_imported_same_leaf_class() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-r5usemap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("A")).unwrap();
    std::fs::create_dir_all(dir.join("B")).unwrap();
    std::fs::write(
        dir.join("A/Collection.php"),
        "<?php\nnamespace A;\nclass Collection { public function pick(): int { return 1; } }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("B/Collection.php"),
        "<?php\nnamespace B;\nclass Collection { public function pick(): string { return \"b\"; } }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Use.php"),
        "<?php\nnamespace App;\nuse B\\Collection;\nfunction f(Collection $c): void {\n    $x = new Collection();\n    $x->pick();\n    $c->pick();\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("A/Own.php"),
        "<?php\nnamespace A;\nfunction g(Collection $c): void {\n    $c->pick();\n}\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
            .args(args)
            .env("XDG_CACHE_HOME", dir.join(".cache"))
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let root = dir.to_str().unwrap();
    // `Collection` in `function f(Collection $c)` / `new Collection()` /
    // `$x->pick()` / `$c->pick()` (0-based rows, byte columns).
    for (row, col, what) in [(3, 11, "type hint"), (4, 13, "new"), (5, 8, "ctor-typed receiver"), (6, 8, "hinted receiver")] {
        let gd = run(&["--definition", root, "Use.php", &row.to_string(), &col.to_string()]);
        assert!(gd.contains("B/Collection.php"), "{what}: gd must follow the `use` row: {gd}");
        assert!(!gd.contains("A/Collection.php"), "{what}: the same-leaf stranger leaked: {gd}");
    }
    let hover = run(&["--hover", root, "Use.php", "5", "8"]);
    assert!(hover.contains("string"), "hover reads B's signature: {hover}");
    let b_refs = run(&["--references", root, "B/Collection.php", "2", "35"]);
    assert!(b_refs.contains("Use.php"), "B::pick's consumer file admitted: {b_refs}");
    assert!(!b_refs.contains("Own.php"), "A's own-namespace caller is not a B::pick reference: {b_refs}");
    let a_refs = run(&["--references", root, "A/Collection.php", "2", "35"]);
    assert!(a_refs.contains("Own.php"), "A::pick's own-namespace caller admitted: {a_refs}");
    assert!(!a_refs.contains("Use.php"), "the `use B\\Collection` file is not an A::pick reference: {a_refs}");
    let rename = run(&["--rename", root, "A/Collection.php", "2", "35", "grab"]);
    assert!(rename.contains("Own.php") && !rename.contains("Use.php"), "rename stays inside A's family: {rename}");
    // The class NAME itself: references from B's declaration reach the
    // `use` row and the type hint (a type position spells the class), and
    // never A's declaration or A's own-namespace hint.
    let cls = run(&["--references", root, "B/Collection.php", "2", "6"]);
    assert!(cls.contains("Use.php"), "the importing file's rows are class references: {cls}");
    assert!(!cls.contains("A/"), "the same-leaf stranger's files stay out: {cls}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Round-5 R5-2: a property and a method sharing a name on one class keep
/// their own identity. The written shape (`MemberShape`) rides the ref
/// (`$this->recorded` reads a value, `$f->recorded()` calls), the hop
/// (`ValueHop` prefers the class's value edge) and the target (shape-strict
/// declaration/reference matching, minted only because the class overloads
/// the name): hover/gd on the access land on the property, refs/rename from
/// either declaration stay on their own side.
#[cfg(feature = "php")]
#[test]
fn php_property_and_method_sharing_a_name_keep_their_own_identity() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-r5prop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Factory.php"),
        "<?php\nnamespace App;\nclass Factory\n{\n    /** @var list<string> */\n    protected $recorded = [];\n\n    public function recorded(): Collection\n    {\n        return new Collection($this->recorded);\n    }\n\n    public function record(string $x): void\n    {\n        $this->recorded[] = $x;\n    }\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Collection.php"),
        "<?php\nnamespace App;\nclass Collection { public function __construct(array $a) {} public function count(): int { return 0; } }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Use.php"),
        "<?php\nnamespace App;\nfunction use_it(Factory $f): int\n{\n    return $f->recorded()->count();\n}\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
            .args(args)
            .env("XDG_CACHE_HOME", dir.join(".cache"))
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let root = dir.to_str().unwrap();
    // 0-based rows: property decl (5,15), method decl (7,20), the access in
    // the method body (9,37), the write (14,15), the call in Use.php (4,15).
    let hover = run(&["--hover", root, "Factory.php", "9", "37"]);
    assert!(hover.contains("list<string>") && !hover.contains("Collection"), "the access hovers the property: {hover}");
    let hover = run(&["--hover", root, "Use.php", "4", "15"]);
    assert!(hover.contains("Collection"), "the call hovers the method: {hover}");
    let gd = run(&["--definition", root, "Factory.php", "9", "37"]);
    assert!(gd.contains("Factory.php:5:"), "gd on the access lands on the property: {gd}");
    let gd = run(&["--definition", root, "Use.php", "4", "15"]);
    assert!(gd.contains("Factory.php:7:"), "gd on the call lands on the method: {gd}");
    let lines = |out: &str| -> Vec<u64> {
        let v: serde_json::Value = serde_json::from_str(out).expect("json");
        v.as_array().unwrap().iter().map(|e| e["line"].as_u64().unwrap()).collect()
    };
    let prop = lines(&run(&["--references", root, "Factory.php", "5", "15"]));
    assert_eq!(prop, vec![5, 9, 14], "property references: decl + the two accesses only");
    let method = run(&["--references", root, "Factory.php", "7", "20"]);
    assert!(method.contains("Use.php") && !method.contains("\"line\": 9"), "method references: decl + call site only: {method}");
    let rename = run(&["--rename", root, "Factory.php", "5", "15", "pairs"]);
    assert!(!rename.contains("Use.php") && !rename.contains("\"line\": 7"), "renaming the property leaves the method alone: {rename}");
    let rename = run(&["--rename", root, "Factory.php", "7", "20", "pairs"]);
    assert!(rename.contains("Use.php") && !rename.contains("\"line\": 5") && !rename.contains("\"line\": 14"), "renaming the method leaves the property alone: {rename}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Round-5 R5-3/R5-8: static calls on EXPRESSION receivers (`$this->prop::m()`,
/// `$cls::m()` with `$cls = Helper::class`) dispatch on the receiver's class,
/// and the class NAME's references/rename reach every spelling: the type
/// hint, `new Helper()`, `Helper::class`, and the bareword static receiver.
#[cfg(feature = "php")]
#[test]
fn php_expression_receivers_dispatch_statically_and_class_refs_reach_every_spelling() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-r5recv-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Helper.php"),
        "<?php\nnamespace App;\nclass Helper\n{\n    public function assist(): void {}\n    public static function make(): static { return new static(); }\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Box.php"),
        "<?php\nnamespace App;\nclass Box\n{\n    public Helper $helper;\n    public function go(): void\n    {\n        $this->helper->assist();\n        $this->helper::make();\n        $cls = Helper::class;\n        $cls::make();\n        $h = new Helper();\n        Helper::make();\n    }\n}\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
            .args(args)
            .env("XDG_CACHE_HOME", dir.join(".cache"))
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let root = dir.to_str().unwrap();
    let lines = |out: &str| -> Vec<u64> {
        let v: serde_json::Value = serde_json::from_str(out).expect("json");
        let mut l: Vec<u64> = v.as_array().unwrap().iter()
            .filter(|e| e["file"].as_str().unwrap().ends_with("Box.php"))
            .map(|e| e["line"].as_u64().unwrap()).collect();
        l.sort();
        l
    };
    // `$this->helper::make()` (8,23) and `$cls::make()` (10,14) land on `make`.
    for (row, col) in [(8, 23), (10, 14)] {
        let gd = run(&["--definition", root, "Box.php", &row.to_string(), &col.to_string()]);
        assert!(gd.contains("Helper.php:5:"), "static call on an expression receiver at {row}:{col}: {gd}");
    }
    // `$cls` at its USE site (`$cls::make();`, row 10): the value assigned
    // from `Helper::class` on row 9 reaches the read.
    let hover = run(&["--hover", root, "Box.php", "10", "9"]);
    assert!(hover.contains("Helper"), "`Helper::class` types the variable: {hover}");
    let make = lines(&run(&["--references", root, "Helper.php", "5", "28"]));
    assert_eq!(make, vec![8, 10, 12], "every static-call spelling references `make`");
    let cls = lines(&run(&["--references", root, "Helper.php", "2", "6"]));
    assert_eq!(cls, vec![4, 9, 11, 12], "hint, `::class`, `new`, bareword receiver all spell the class");
    let rename = run(&["--rename", root, "Helper.php", "2", "6", "Aide"]);
    let edits: serde_json::Value = serde_json::from_str(&rename).expect("json");
    let n = edits.as_object().unwrap().values().map(|v| v.as_array().unwrap().len()).sum::<usize>();
    assert_eq!(n, 5, "class rename rewrites its declaration + four spellings: {rename}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Round-5 R5-4: the php tier's heatmap fan-in is pre-pruned from its OWN
/// row store (the pack persist writer shreds every analysis), and the
/// prune is answer-preserving: the resident-only walk (`PERL_LSP_REF_ROWS=0`)
/// and the pruned walk report identical fan-in per symbol — including the
/// constructor, whose references are the class's `new` sites (the class
/// key counts as a reference row for it).
#[cfg(feature = "php")]
#[test]
fn php_heatmap_pre_prune_preserves_every_fan_in() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-r5heat-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Helper.php"),
        "<?php\nnamespace App;\nclass Helper\n{\n    public function __construct(private int $n) {}\n    public function assist(): void {}\n    public static function make(): static { return new static(1); }\n    public function unused(): void {}\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("Box.php"),
        "<?php\nnamespace App;\nclass Box\n{\n    public function go(): void\n    {\n        $h = new Helper(2);\n        $h->assist();\n        Helper::make();\n    }\n}\n",
    )
    .unwrap();
    let run = |rows: &str| -> std::collections::BTreeMap<String, u64> {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
            .args(["--heatmap", dir.to_str().unwrap()])
            .env("XDG_CACHE_HOME", dir.join(".cache"))
            .env("PERL_LSP_REF_ROWS", rows)
            .output()
            .expect("run");
        let v: serde_json::Value =
            serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("heatmap json");
        v["symbols"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["name"].as_str().unwrap().to_string(), s["fan_in"].as_u64().unwrap()))
            .collect()
    };
    // Cold (writes the rows), then the two warm projections.
    let _ = run("1");
    let pruned = run("1");
    let walked = run("0");
    assert_eq!(pruned, walked, "the pre-prune must not change any fan-in");
    assert_eq!(pruned.get("__construct"), Some(&1), "ctor fan-in = its `new` site: {pruned:?}");
    assert_eq!(pruned.get("unused"), Some(&0), "{pruned:?}");
    assert_eq!(pruned.get("make"), Some(&1), "{pruned:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Round-5 R5-7: a nested generic whose innermost element is `mixed`
/// (`array<array<mixed>>`) types as a sequence of arrays instead of
/// collapsing the whole annotation — the inner `array<mixed>` is the bare
/// `array` shape.
#[cfg(feature = "php")]
#[test]
fn php_nested_generic_over_mixed_keeps_the_outer_shape() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-r5nested-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("Grid.php"),
        "<?php\nnamespace App;\nclass Grid\n{\n    /** @var array<array<mixed>> */\n    private array $rows = [];\n    /** @var array<mixed> */\n    private array $bag = [];\n    public function go(): void\n    {\n        foreach ($this->rows as $row) { $row; }\n    }\n}\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
            .args(args)
            .env("XDG_CACHE_HOME", dir.join(".cache"))
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let root = dir.to_str().unwrap();
    let prop = run(&["--hover", root, "Grid.php", "5", "19"]);
    assert!(prop.contains("list<array>"), "the outer generic survives: {prop}");
    let row = run(&["--hover", root, "Grid.php", "10", "40"]);
    assert!(row.contains("$row: array"), "the element is an array: {row}");
    // A top-level `array<mixed>` IS the bare `array` shape (it used to type
    // nothing); the display says so and nothing invents element keys.
    let bag = run(&["--hover", root, "Grid.php", "7", "19"]);
    assert!(bag.contains("bag: array") && !bag.contains("list"), "{bag}");
    let _ = std::fs::remove_dir_all(&dir);
}
