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
    std::fs::write(
        dir.join("Bag.php"),
        "<?php\nnamespace App;\nclass Bag implements \\Countable\n{\n    public function count(): int { return 0; }\n}\n",
    )
    .unwrap();
    // A service nobody `new`s — a container does (the type hint names it).
    std::fs::write(
        dir.join("Svc.php"),
        "<?php\nnamespace App;\nclass Svc\n{\n    public function __construct(private Helper $h) {}\n    public function run(): void { $this->h->assist(); }\n}\nclass Consumer\n{\n    public function __construct(private Svc $svc) {}\n}\n",
    )
    .unwrap();
    let run_full = |rows: &str| -> serde_json::Value {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
            .args(["--heatmap", dir.to_str().unwrap()])
            .env("XDG_CACHE_HOME", dir.join(".cache"))
            .env("PERL_LSP_REF_ROWS", rows)
            .output()
            .expect("run");
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).expect("heatmap json")
    };
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
            .map(|s| (format!("{}::{}", s["package"].as_str().unwrap_or(""), s["name"].as_str().unwrap()), s["fan_in"].as_u64().unwrap()))
            .collect()
    };
    // Cold (writes the rows), then the two warm projections.
    let _ = run("1");
    let pruned = run("1");
    let walked = run("0");
    assert_eq!(pruned, walked, "the pre-prune must not change any fan-in");
    // `new Helper(2)` in Box.php plus `new static(1)` inside `make()`.
    assert_eq!(pruned.get("Helper::__construct"), Some(&2), "ctor fan-in = its `new` sites: {pruned:?}");
    assert_eq!(pruned.get("Helper::unused"), Some(&0), "{pruned:?}");
    assert_eq!(pruned.get("Helper::make"), Some(&1), "{pruned:?}");
    // R6-9: `Svc::__construct` has no `new` site, but `Svc` is named by a
    // type hint — a container instantiates it, so the ctor is shielded
    // (`class-referenced`) when the row store can answer; `Consumer`'s
    // ctor (its class named nowhere) stays a candidate.
    let full = run_full("1");
    let ctor_of = |class: &str| -> serde_json::Value {
        full["symbols"].as_array().unwrap().iter()
            .find(|s| s["name"] == "__construct" && s["package"] == class)
            .cloned().expect(class)
    };
    let svc = ctor_of("Svc");
    assert_eq!(svc["reachable_guard"].as_str(), Some("class-referenced"), "{svc}");
    assert_eq!(svc["dead_code_candidate"], false, "{svc}");
    let consumer = ctor_of("Consumer");
    assert_eq!(consumer["dead_code_candidate"], true, "{consumer}");
    // An SPL contract method (`Countable::count`) is runtime-invoked, never dead.
    let count = full["symbols"].as_array().unwrap().iter()
        .find(|s| s["name"] == "count" && s["package"] == "Bag").cloned().expect("Bag::count");
    assert_eq!(count["reachable_guard"].as_str(), Some("runtime-invoked"), "{count}");
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

/// Round-6 R6-1/R6-2: an aliased import pins the ALIAS spelling, never the
/// real leaf (`use B\Event as ScriptEvent;` in namespace `A` leaves the
/// bare `Event` meaning `A\Event`), a `use` row references only the class
/// its own namespace names, and a qualified spelling
/// (`new Downloader\DownloadManager()` inside `Composer`) pins the leaf to
/// `Composer\Downloader` instead of counting as a bare spelling.
#[cfg(feature = "php")]
#[test]
fn php_aliased_imports_and_qualified_spellings_pin_the_right_class() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-r6pins-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for d in ["A", "B", "Composer/Downloader", "Other"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("A/Event.php", "<?php\nnamespace A;\nclass Event { public function name(): string { return \"a\"; } }\n");
    w("B/Event.php", "<?php\nnamespace B;\nuse A\\Event as BaseEvent;\nclass Event extends BaseEvent { public function name(): string { return \"b\"; } }\n");
    w("A/Dispatcher.php", "<?php\nnamespace A;\nuse B\\Event as ScriptEvent;\nfunction dispatch(): void\n{\n    $e = new Event();\n    $e->name();\n    $s = new ScriptEvent();\n    $s->name();\n}\n");
    w("Composer/Factory.php", "<?php\nnamespace Composer;\nclass Factory\n{\n    public function make(): void\n    {\n        $dm = new Downloader\\DownloadManager();\n        $dm->go();\n    }\n}\n");
    w("Composer/Abs.php", "<?php\nnamespace Composer;\nclass Abs\n{\n    public function make(): void\n    {\n        $o = new \\Other\\DownloadManager();\n        $o->go();\n    }\n}\n");
    w("Composer/Downloader/DownloadManager.php", "<?php\nnamespace Composer\\Downloader;\nclass DownloadManager { public function go(): int { return 1; } }\n");
    w("Other/DownloadManager.php", "<?php\nnamespace Other;\nclass DownloadManager { public function go(): string { return \"o\"; } }\n");
    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
            .args(args)
            .env("XDG_CACHE_HOME", dir.join(".cache"))
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let root = dir.to_str().unwrap();
    let sites = |out: &str| -> Vec<(String, u64)> {
        let v: serde_json::Value = serde_json::from_str(out).expect("json");
        let mut l: Vec<(String, u64)> = v.as_array().unwrap().iter().map(|e| {
            let f = e["file"].as_str().unwrap();
            (f[f.rfind('/').map(|i| i + 1).unwrap_or(0)..].to_string(), e["line"].as_u64().unwrap())
        }).collect();
        l.sort();
        l
    };
    let gd = run(&["--definition", root, "A/Dispatcher.php", "5", "13"]);
    assert!(gd.contains("A/Event.php") && !gd.contains("B/Event.php"), "bare `Event` in namespace A is A's: {gd}");
    let a = sites(&run(&["--references", root, "A/Event.php", "2", "6"]));
    assert_eq!(a, vec![("Dispatcher.php".into(), 5), ("Event.php".into(), 2), ("Event.php".into(), 2)], "A\\Event: the `new`, its decl, and B's `use A\\Event` row — never `use B\\Event`: {a:?}");
    let b = sites(&run(&["--references", root, "B/Event.php", "3", "6"]));
    assert_eq!(b, vec![("Dispatcher.php".into(), 2), ("Event.php".into(), 3)], "B\\Event: the aliased use row and its decl only: {b:?}");
    let d = sites(&run(&["--references", root, "Composer/Downloader/DownloadManager.php", "2", "40"]));
    assert_eq!(d, vec![("DownloadManager.php".into(), 2), ("Factory.php".into(), 7)], "relative-qualified `new` site admitted, absolute `\\Other` site not: {d:?}");
    let o = sites(&run(&["--references", root, "Other/DownloadManager.php", "2", "40"]));
    assert_eq!(o, vec![("Abs.php".into(), 7), ("DownloadManager.php".into(), 2)], "{o:?}");
    // Goto-def on the `Event` leaf of B's `use A\Event as BaseEvent;` row
    // (row 2) names A's class in full — never B's own same-leaf `Event`.
    let row = run(&["--definition", root, "B/Event.php", "2", "8"]);
    assert!(row.contains("A/Event.php") && !row.contains("B/Event.php"), "an import row's leaf resolves by the row's namespace: {row}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Round-7 R7-5: a group-use row (`use A\{Foo, Bar as Baz};`) is an
/// import row like the flat spelling — each clause pins its leaf (or
/// alias) to the group's namespace, the leaf token is a reference site
/// goto-def lands from, and references/rename on the class reach the
/// row and the `new` sites it enables.
#[cfg(feature = "php")]
#[test]
fn php_group_use_rows_answer_like_flat_rows() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-r7group-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for d in ["A", "B"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("A/Foo.php", "<?php\nnamespace A;\nclass Foo { public function go(): int { return 1; } }\n");
    w("A/Bar.php", "<?php\nnamespace A;\nclass Bar { public function run(): int { return 2; } }\n");
    w("B/Use.php", "<?php\nnamespace B;\nuse A\\{Foo, Bar as Baz};\nclass Use1 {\n    public function m(): int { $f = new Foo(); $b = new Baz(); return $f->go() + $b->run(); }\n}\n");
    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
            .args(args)
            .env("XDG_CACHE_HOME", dir.join(".cache"))
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let root = dir.to_str().unwrap();
    let sites = |out: &str| -> Vec<(String, u64)> {
        let v: serde_json::Value = serde_json::from_str(out).expect("json");
        let mut l: Vec<(String, u64)> = v.as_array().unwrap().iter().map(|e| {
            let f = e["file"].as_str().unwrap();
            (f[f.rfind('/').map(|i| i + 1).unwrap_or(0)..].to_string(), e["line"].as_u64().unwrap())
        }).collect();
        l.sort();
        l
    };
    // the `Foo` leaf inside the group row (0-based row 2, col 7)
    let row = run(&["--definition", root, "B/Use.php", "2", "7"]);
    assert!(row.contains("A/Foo.php"), "group-use leaf resolves by the group's namespace: {row}");
    let bar = run(&["--definition", root, "B/Use.php", "2", "12"]);
    assert!(bar.contains("A/Bar.php"), "aliased group clause's real leaf: {bar}");
    let new_site = run(&["--definition", root, "B/Use.php", "4", "40"]);
    assert!(new_site.contains("A/Foo.php"), "`new Foo()` under a group use: {new_site}");
    let foo = sites(&run(&["--references", root, "A/Foo.php", "2", "6"]));
    assert_eq!(foo, vec![("Foo.php".into(), 2), ("Use.php".into(), 2), ("Use.php".into(), 4)], "decl, group row, `new` site: {foo:?}");
    let bar = sites(&run(&["--references", root, "A/Bar.php", "2", "6"]));
    assert_eq!(bar, vec![("Bar.php".into(), 2), ("Use.php".into(), 2)], "decl and the aliased group clause: {bar:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Round-7 R7-4: an anonymous class (`new class(...) extends Base {...}`)
/// is its own identity — a position-keyed synthesized Class symbol its
/// members key by — never a member of the enclosing container: an outer
/// class's same-named property/method has no references inside the
/// anonymous body, `$this` inside it resolves to its own members, the
/// `class` keyword is its constructor's call site, and `extends Base`
/// makes its override an implementation of the base method.
#[cfg(feature = "php")]
#[test]
fn php_anonymous_class_is_its_own_identity() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-r7anon-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("Outer.php", "<?php\nnamespace T;\nclass Outer {\n    private int $n = 1;\n    public function get(): int { return $this->n; }\n    public function make(): object {\n        return new class(3) {\n            public function __construct(private int $n) {}\n            public function get(): int { return $this->n; }\n        };\n    }\n}\n");
    w("Handler.php", "<?php\nnamespace T;\nclass Handler { public function handle(int $x): int { return $x; } }\nfunction make(): Handler {\n    return new class(3) extends Handler {\n        public function __construct(private int $n) {}\n        public function handle(int $x): int { return $x + $this->n; }\n    };\n}\n");
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
        let mut l: Vec<u64> = v.as_array().unwrap().iter().map(|e| e["line"].as_u64().unwrap()).collect();
        l.sort();
        l
    };
    // (positional CLI coordinates and answers are 0-based)
    // Outer's `$n` (row 3) and `get()` (row 4): the anonymous body's
    // same-named members are NOT references to them.
    assert_eq!(lines(&run(&["--references", root, "Outer.php", "3", "17"])), vec![3, 4], "outer `$n` stays outside the anonymous class");
    assert_eq!(lines(&run(&["--references", root, "Outer.php", "4", "20"])), vec![4], "outer `get()` stays outside the anonymous class");
    // the anonymous class's own promoted `$n` (row 7) is read by ITS `get()`
    assert_eq!(lines(&run(&["--references", root, "Outer.php", "7", "52"])), vec![7, 8], "anonymous `$n`: decl + its own `$this->n`");
    let gd = run(&["--definition", root, "Outer.php", "8", "55"]);
    assert!(gd.contains("Outer.php:7:"), "`$this->n` inside the anonymous body lands on its promoted property: {gd}");
    // `extends Handler`: the anonymous override is an implementation of the base method
    let impls = run(&["--implementations", root, "Handler.php", "2", "33"]);
    assert!(impls.contains("\"line\": 6"), "anonymous override implements Handler::handle: {impls}");
    // the `class` keyword is the constructor's call site: neither ctor is dead
    let heat: serde_json::Value = serde_json::from_str(&run(&["--heatmap", root])).expect("heatmap json");
    let rows = heat["symbols"].as_array().or_else(|| heat["rows"].as_array()).expect("rows");
    let ctors: Vec<(String, u64)> = rows.iter().filter(|r| r["name"] == "__construct").map(|r| (r["package"].as_str().unwrap_or("").to_string(), r["fan_in"].as_u64().unwrap_or(0))).collect();
    assert_eq!(ctors.len(), 2, "{ctors:?}");
    assert!(ctors.iter().all(|(pkg, fan_in)| pkg.starts_with("class_anonymous_") && *fan_in == 1), "each anonymous ctor keyed by its synthesized class with its `new class(...)` site as fan-in: {ctors:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Day-2 diagnostics axis: the php lanes `--check` reports on a project —
/// undefined method / property (through a typed `$this->prop` chain, with
/// non-public access told apart), argument-count mismatch both ways,
/// undefined variable, undefined type — and the shapes every lane must
/// stay SILENT on: `catch ($e)`, `use (&$x)`, `Foo::$static`, property
/// declarations, `Foo::class`, `f(...)`, `\Throwable`, `use function`,
/// enum members, a property declared by writing it, a class whose parent
/// the workspace cannot see.
#[cfg(feature = "php")]
#[test]
fn php_diagnostic_lanes_report_the_real_findings_and_nothing_else() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-d2diag-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("composer.json", "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"src/\"}}}");
    w("src/Mailer.php", "<?php\nnamespace App;\n\nclass Mailer\n{\n    private string $from = 'noreply@example.com';\n\n    public function send(string $to, string $subject, string $body = ''): bool\n    {\n        return $to !== '' && $subject !== '';\n    }\n}\n");
    w("src/Service.php", "<?php\nnamespace App;\n\nclass Service\n{\n    public function __construct(private Mailer $mailer) {}\n\n    public function run(string $who): void\n    {\n        $this->mailer->send($who);\n        $this->mailer->sendLater($who, 'x');\n        $this->mailer->from;\n        $this->missingMethod();\n        $count = strlen($who) + $undefinedVar;\n        $req = new Request('GET', '/');\n        Helper::go();\n        $this->mailer->send($who, 'subject', 'body', 'extra');\n    }\n}\n");
    w("src/Quiet.php", "<?php\nnamespace App;\nuse function Other\\helper;\nuse Exception;\nenum Level: int { case Low = 1; public function label(): string { return $this->name . $this->value; } }\nclass Quiet extends Unseen\n{\n    public static $count = 0;\n    private $dyn;\n    public function go(array $rows): int\n    {\n        try { $x = 1; } catch (\\Throwable $e) { return $e->getCode(); }\n        $seen = 0;\n        $cb = function () use (&$seen) { $seen++; };\n        $cb();\n        self::$count++;\n        Quiet::$count = 2;\n        $this->created = true;\n        $this->created;\n        $name = Quiet::class;\n        $f = strlen(...);\n        $this->inherited();\n        $lvl = Level::from(1);\n        return $seen + \\count($rows) + $lvl->value;\n    }\n}\n");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
        .args(["--check", dir.to_str().unwrap()])
        .env("XDG_CACHE_HOME", dir.join(".cache"))
        .output()
        .expect("run");
    let err = String::from_utf8_lossy(&out.stderr);
    let rows: Vec<&str> = err.lines().filter(|l| l.contains("[")).collect();
    let has = |file: &str, code: &str, needle: &str| rows.iter().any(|l| l.contains(file) && l.contains(&format!("[{code}]")) && l.contains(needle));
    assert!(has("Service.php", "arity-mismatch", "Expected 2. Found 1"), "{err}");
    assert!(has("Service.php", "arity-mismatch", "Expected 3. Found 4"), "{err}");
    assert!(has("Service.php", "unresolved-method", "sendLater"), "{err}");
    assert!(has("Service.php", "unresolved-method", "missingMethod"), "{err}");
    assert!(has("Service.php", "non-public-access", "'from'"), "{err}");
    assert!(has("Service.php", "undefined-variable", "$undefinedVar"), "{err}");
    assert!(has("Service.php", "undefined-type", "App\\Helper"), "{err}");
    assert!(has("Service.php", "undefined-type", "App\\Request"), "{err}");
    // and nothing beyond those eight — a duplicated row is a regression
    let service = rows.iter().filter(|l| l.contains("Service.php")).count();
    assert_eq!(service, 8, "{err}");
    // the ONE real finding in Quiet.php is its unseen parent; every other
    // shape there is a silence rule
    let quiet: Vec<&&str> = rows.iter().filter(|l| l.contains("Quiet.php")).collect();
    assert_eq!(quiet.len(), 1, "the silence rules: {quiet:?}");
    assert!(quiet[0].contains("[undefined-type]") && quiet[0].contains("App\\Unseen"), "{quiet:?}");
    let mailer: Vec<&&str> = rows.iter().filter(|l| l.contains("Mailer.php")).collect();
    assert!(mailer.is_empty(), "{mailer:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Day-2: a call through a callable VARIABLE (`$r = $handler(new Foo(), [])`)
/// has no known value — it must not take an argument's constructor type
/// (the assignment narrowing only descends into a literal the right-hand
/// side merely wraps in parentheses).
#[cfg(feature = "php")]
#[test]
fn php_callable_variable_call_does_not_take_its_arguments_type() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-d2callvar-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("T.php"), "<?php\nnamespace T;\nclass Foo { public function go(): int { return 1; } }\nclass Bar { public function run(): int { return 2; } }\nfunction mk(): Bar { return new Bar(); }\nfunction test(callable $a): void {\n    $r = $a(new Foo(), []);\n    $s = mk(new Foo());\n    $t = (new Foo());\n}\n").unwrap();
    let run = |line: &str, col: &str| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
            .args(["--hover", dir.to_str().unwrap(), "--at", &format!("{}:{}:{}", dir.join("T.php").display(), line, col)])
            .env("XDG_CACHE_HOME", dir.join(".cache"))
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let r = run("7", "6");
    // untyped: the hover shows the source line, never a `$r: Foo` type line
    assert!(!r.contains("$r: "), "`$r` took the argument's type: {r}");
    let s = run("8", "6");
    assert!(s.contains("Bar"), "a named function's declared return: {s}");
    let t = run("9", "6");
    assert!(t.contains("Foo"), "a parenthesized literal is the value: {t}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Round-6 R6-3 / R6-4 / R6-5 (BookStack, composer): the build-time
/// method-call stamp honors the written shape, so goto-def on a same-file
/// `$this->hasAuth()` lands on the method while `$this->hasAuth` reads the
/// property; a keyed two-pair array (`['chapter' => $c, 'book' => $c->book]`)
/// is NOT the `[$obj, 'method']` callable shape, so its key is no method
/// reference; an Eloquent relation behind a chained modifier
/// (`->belongsTo(Book::class)->withTrashed()`) still types the property.
#[cfg(feature = "php")]
#[test]
fn php_round6_shape_stamp_keyed_arrays_and_chained_relations() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-r6b-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("Svn.php", "<?php\nnamespace App;\nclass Svn\n{\n    /** @var bool */\n    protected $hasAuth = false;\n\n    public function go(): bool\n    {\n        return $this->hasAuth();\n    }\n\n    protected function hasAuth(): bool\n    {\n        return $this->hasAuth;\n    }\n}\n");
    w("Chapter.php", "<?php\nnamespace App;\nclass Chapter\n{\n    public function book(): int { return 1; }\n    public function show(Chapter $chapter): array\n    {\n        return view(\"x\", [\n            \"chapter\" => $chapter,\n            \"book\" => $chapter->book,\n        ]);\n    }\n}\nfunction view(string $n, array $d): array { return $d; }\n");
    w("Models.php", "<?php\nnamespace App;\nclass BelongsTo { public function withTrashed(): BelongsTo { return $this; } }\nabstract class Model { public function belongsTo(string $c): BelongsTo { return new BelongsTo(); } }\nclass Book extends Model { public function getUrl(): string { return \"u\"; } }\nabstract class BookChild extends Model\n{\n    public function book(): BelongsTo\n    {\n        return $this->belongsTo(Book::class)->withTrashed();\n    }\n}\nclass Page extends BookChild {}\n");
    w("Use.php", "<?php\nnamespace App;\nfunction f(Page $page): void\n{\n    $page->book->getUrl();\n}\n");
    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
            .args(args)
            .env("XDG_CACHE_HOME", dir.join(".cache"))
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let root = dir.to_str().unwrap();
    // `$this->hasAuth()` (row 9) is the method (row 12); `$this->hasAuth` (row 14) the property (row 5).
    let call = run(&["--definition", root, "Svn.php", "9", "22"]);
    assert!(call.contains("Svn.php:12:"), "the call lands on the method: {call}");
    let read = run(&["--definition", root, "Svn.php", "14", "22"]);
    assert!(read.contains("Svn.php:5:"), "the read lands on the property: {read}");
    // The `"book"` key (row 9, col 13) is no reference; `book()`'s references
    // are its declaration and the `$chapter->book` read only.
    let key = run(&["--hover", root, "Chapter.php", "9", "13"]);
    assert!(!key.contains("function book"), "an array key is not a method reference: {key}");
    let refs: serde_json::Value = serde_json::from_str(&run(&["--references", root, "Chapter.php", "4", "20"])).unwrap();
    let cols: Vec<(u64, u64)> = refs.as_array().unwrap().iter().map(|e| (e["line"].as_u64().unwrap(), e["col"].as_u64().unwrap())).collect();
    assert_eq!(cols, vec![(4, 20), (9, 32)], "{cols:?}");
    // The chained relation types the inherited property's chain.
    let hover = run(&["--hover", root, "Use.php", "4", "17"]);
    assert!(hover.contains("getUrl(): string"), "chain through a chained-modifier relation: {hover}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Round-6 R6-6 / R6-7 / R6-8: a `'A\\F::cb'` string callable is a Callable
/// member reference on `F` (references from the method reach it); `new
/// self(...)` names the enclosing class (its ctor's references and
/// hover see it); a middle segment of a `use` row is a namespace and
/// answers nothing rather than a same-named class elsewhere.
#[cfg(feature = "php")]
#[test]
fn php_string_callables_new_self_and_import_row_segments() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-r6c-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("A/Sub")).unwrap();
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("A/F.php", "<?php\nnamespace A;\nclass F\n{\n    public static function mk(): self { return new self(1); }\n    public function __construct(int $n) {}\n    public static function cb(): void {}\n}\n");
    w("A/Use.php", "<?php\nnamespace A;\nuse A\\Sub\\Thing;\ncall_user_func('A\\F::cb');\n$f = F::mk();\n");
    w("A/Sub/Thing.php", "<?php\nnamespace A\\Sub;\nclass Thing {}\n");
    w("A/Sub.php", "<?php\nnamespace A;\nclass Sub {}\n");
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
        let mut l: Vec<u64> = v.as_array().unwrap().iter().map(|e| e["line"].as_u64().unwrap()).collect();
        l.sort();
        l
    };
    let ctor = lines(&run(&["--references", root, "A/F.php", "5", "20"]));
    assert_eq!(ctor, vec![4, 5], "`new self(1)` (row 4) is a construction site of F");
    let hover = run(&["--hover", root, "A/F.php", "4", "53"]);
    assert!(hover.contains("__construct"), "`self` in `new self` is the constructor call: {hover}");
    let gd = run(&["--definition", root, "A/F.php", "4", "53"]);
    assert!(gd.contains("F.php:5:"), "goto-def on `self` lands on the constructor: {gd}");
    let rename = run(&["--rename", root, "A/F.php", "5", "20", "build"]);
    assert!(!rename.contains("\"line\": 4"), "a constructor-convention name is not renameable: {rename}");
    let cb = run(&["--references", root, "A/F.php", "6", "28"]);
    assert!(cb.contains("Use.php"), "the string callable site references `cb`: {cb}");
    // Renaming `cb` rewrites exactly the method tail inside the string
    // (`'A\\F::cb'` → `'A\\F::run'`), never the class qualifier.
    let rename: serde_json::Value = serde_json::from_str(&run(&["--rename", root, "A/F.php", "6", "28", "run"])).expect("json");
    let use_edits = rename.as_object().unwrap().iter().find(|(k, _)| k.ends_with("Use.php")).map(|(_, v)| v.clone()).expect("Use.php edited");
    let e = &use_edits.as_array().unwrap()[0];
    let src_line = "call_user_func('A\\F::cb');";
    let cb_col = src_line.find("cb'").unwrap() as u64;
    assert_eq!((e["line"].as_u64(), e["col"].as_u64(), e["end_col"].as_u64()), (Some(3), Some(cb_col), Some(cb_col + 2)), "{use_edits}");
    let mid = run(&["--definition", root, "A/Use.php", "2", "8"]);
    assert!(!mid.contains("Sub.php"), "a `use` row's middle segment is a namespace, not class `A\\Sub`: {mid}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// `instanceof` narrows beyond the `if` block: a negated guard whose body
/// exits narrows the rest of the scope, `assert()` does too, and the
/// expression regions (`&&` right operand, ternary true arm, `match` arm)
/// narrow within themselves. A negated guard whose body does NOT exit
/// narrows nothing.
#[cfg(feature = "php")]
#[test]
fn php_instanceof_narrows_exits_assertions_and_expression_regions() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-narrow2-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let src = "<?php\nnamespace App;\ninterface Shape { public function area(): float; }\nclass Circle implements Shape { public function area(): float { return 1.0; } public function radius(): int { return 2; } }\nclass Square implements Shape { public function area(): float { return 1.0; } public function side(): int { return 3; } }\nclass Use1 {\n    public function a(Shape $s): int {\n        if (!$s instanceof Circle) { return 0; }\n        return $s->radius();\n    }\n    public function b(Shape $s): int {\n        if (!($s instanceof Circle)) throw new \\RuntimeException('x');\n        return $s->radius();\n    }\n    public function c(Shape $s): int {\n        assert($s instanceof Square);\n        return $s->side();\n    }\n    public function d(Shape $s): int {\n        return $s instanceof Circle && $s->radius() > 1 ? 1 : 0;\n    }\n    public function e(Shape $s): int {\n        return $s instanceof Square ? $s->side() : 0;\n    }\n    public function f(Shape $s): int {\n        return match (true) { $s instanceof Circle => $s->radius(), default => 0 };\n    }\n    public function h(Shape $s): float {\n        foreach ([$s] as $i) { if (!$i instanceof Square) continue; return $i->side(); }\n        return $s->area();\n    }\n    public function i(Shape $s): int {\n        if (!$s instanceof Circle) { error_log('not a circle'); }\n        return $s->radius();\n    }\n}\n";
    std::fs::write(dir.join("Narrow.php"), src).unwrap();
    let lines: Vec<&str> = src.lines().collect();
    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
            .args(args)
            .env("XDG_CACHE_HOME", dir.join(".cache"))
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let root = dir.to_str().unwrap();
    // the receiver's hover type at the member call on `row` — goto-def would
    // find a uniquely-named member with no narrowing at all
    let receiver = |row: usize, var: &str| -> String {
        let col = lines[row].find(&format!("{var}->")).unwrap();
        let out = run(&["--hover", root, "Narrow.php", &row.to_string(), &col.to_string()]);
        out.lines().find(|l| l.starts_with(var)).unwrap_or("").to_string()
    };
    assert_eq!(receiver(8, "$s"), "$s: Circle", "negated block exit");
    assert_eq!(receiver(12, "$s"), "$s: Circle", "negated brace-less throw");
    assert_eq!(receiver(16, "$s"), "$s: Square", "assert()");
    assert_eq!(receiver(19, "$s"), "$s: Circle", "`&&` right operand");
    assert_eq!(receiver(22, "$s"), "$s: Square", "ternary true arm");
    assert_eq!(receiver(25, "$s"), "$s: Circle", "match arm");
    assert_eq!(receiver(28, "$i"), "$i: Square", "`continue` inside a loop body");
    assert_eq!(receiver(29, "$s"), "$s: Shape", "the loop's narrowing stays inside the loop");
    assert_eq!(receiver(33, "$s"), "$s: Shape", "a negated guard that does not exit narrows nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The unused-import lane's silence rules: a name spelled only as an
/// `instanceof` operand, an attribute, a namespace prefix or a docblock
/// word is used; the one row nothing spells is the finding.
#[cfg(feature = "php")]
#[test]
fn php_unused_import_lane_counts_every_spelling() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-d2unused-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("composer.json", "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"src/\"}}}");
    w("src/Use1.php", "<?php\nnamespace App;\n\nuse App\\Guard;\nuse App\\Attr\\Route;\nuse App\\Psr7;\nuse App\\Doc\\Shape;\nuse App\\Never;\nuse App\\Aliased as Other;\nuse const App\\LIMIT;\n\nclass Use1\n{\n    /** @var Shape */\n    private $shape;\n\n    #[Route('/x')]\n    public function run($x): int\n    {\n        if ($x instanceof Guard) { return 1; }\n        $o = new Other();\n        return Psr7\\Utils::count($o);\n    }\n}\n");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
        .args(["--check", dir.to_str().unwrap(), "--severity", "hint"])
        .env("XDG_CACHE_HOME", dir.join(".cache"))
        .output()
        .expect("run");
    let err = String::from_utf8_lossy(&out.stderr);
    let unused: Vec<&str> = err.lines().filter(|l| l.contains("[unused-import]")).collect();
    assert_eq!(unused.len(), 1, "{err}");
    assert!(unused[0].contains("'Never'"), "{unused:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// PHPUnit mocks are the class they double: `createMock(Foo::class)`,
/// `createStub`, a `getMockBuilder(...)->...->getMock()` chain, and the
/// `$this->foo = $this->createMock(...)` property form all type the target
/// as `Foo`, so member navigation reaches Foo's methods.
#[cfg(feature = "php")]
#[test]
fn php_phpunit_mocks_type_as_the_doubled_class() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-d2mock-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("vendor/phpunit/PHPUnit/Framework/MockObject")).unwrap();
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("composer.json", "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"src/\", \"App\\\\Tests\\\\\": \"tests/\"}}}");
    w("vendor/phpunit/PHPUnit/Framework/MockObject/MockObject.php", "<?php\nnamespace PHPUnit\\Framework\\MockObject;\ninterface MockObject { public function expects($m); }\n");
    w("vendor/phpunit/PHPUnit/Framework/TestCase.php", "<?php\nnamespace PHPUnit\\Framework;\nuse PHPUnit\\Framework\\MockObject\\MockObject;\nclass TestCase\n{\n    protected function createMock(string $c): MockObject { return null; }\n    protected function createStub(string $c): object { return null; }\n    protected function getMockBuilder(string $c): MockBuilder { return new MockBuilder(); }\n}\nclass MockBuilder { public function disableOriginalConstructor(): self { return $this; } public function onlyMethods(array $m): self { return $this; } public function getMock(): MockObject { return null; } }\n");
    w("src/Foo.php", "<?php\nnamespace App;\nclass Foo\n{\n    public function bar(): int { return 1; }\n}\n");
    let test = "<?php\nnamespace App\\Tests;\nuse App\\Foo;\nuse PHPUnit\\Framework\\TestCase;\nclass FooTest extends TestCase\n{\n    private $foo;\n    protected function setUp(): void\n    {\n        $this->foo = $this->createMock(Foo::class);\n    }\n    public function testIt(): void\n    {\n        $m = $this->createMock(Foo::class);\n        $m->bar();\n        $s = $this->createStub(Foo::class);\n        $s->bar();\n        $b = $this->getMockBuilder(Foo::class)->disableOriginalConstructor()->onlyMethods(['bar'])->getMock();\n        $b->bar();\n        $this->foo->bar();\n    }\n}\n";
    w("tests/FooTest.php", test);
    let lines: Vec<&str> = test.lines().collect();
    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
            .args(args)
            .env("XDG_CACHE_HOME", dir.join(".cache"))
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let root = dir.to_str().unwrap();
    let receiver = |row: usize, var: &str| -> String {
        let col = lines[row].find(&format!("{var}->bar")).unwrap();
        let out = run(&["--hover", root, "tests/FooTest.php", &row.to_string(), &col.to_string()]);
        out.lines().find(|l| l.starts_with(var)).unwrap_or("").to_string()
    };
    assert_eq!(receiver(14, "$m"), "$m: Foo", "createMock");
    assert_eq!(receiver(16, "$s"), "$s: Foo", "createStub");
    assert_eq!(receiver(18, "$b"), "$b: Foo", "getMockBuilder chain");
    let col = lines[19].find("bar()").unwrap();
    let def = run(&["--definition", root, "tests/FooTest.php", "19", &col.to_string()]);
    assert!(def.contains("src/Foo.php:4:"), "the property form reaches Foo::bar: {def}");
    let col = lines[14].find("bar()").unwrap();
    let def = run(&["--definition", root, "tests/FooTest.php", "14", &col.to_string()]);
    assert!(def.contains("src/Foo.php:4:"), "{def}");
    let _ = std::fs::remove_dir_all(&dir);
}


/// The deprecation lane: `@deprecated` (with and without text) and the
/// `#[Deprecated]` attribute on a method, a function and a class, used from
/// another file — each use is a deprecated-tagged hint; nothing else is.
#[cfg(feature = "php")]
#[test]
fn php_deprecation_lane_flags_every_use_of_a_deprecated_declaration() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-d2depr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("composer.json", "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"src/\"}}}");
    w("src/Legacy.php", "<?php\nnamespace App;\n\n/** @deprecated use Modern instead */\nclass Legacy\n{\n    /** @deprecated */\n    public function old(): int { return 1; }\n    #[Deprecated]\n    public function older(): int { return 2; }\n    public function fine(): int { return 3; }\n}\n\n/** @deprecated since 2.0 */\nfunction legacy_helper(): int { return 4; }\nfunction fine_helper(): int { return 5; }\n");
    w("src/Caller.php", "<?php\nnamespace App;\n\nclass Caller\n{\n    public function run(Legacy $l): int\n    {\n        return $l->old() + $l->older() + $l->fine() + legacy_helper() + fine_helper();\n    }\n}\n");
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
        .args(["--check", dir.to_str().unwrap(), "--severity", "hint"])
        .env("XDG_CACHE_HOME", dir.join(".cache"))
        .output()
        .expect("run");
    let err = String::from_utf8_lossy(&out.stderr);
    let rows: Vec<&str> = err.lines().filter(|l| l.contains("[deprecated]") && l.contains("Caller.php")).collect();
    let has = |needle: &str| rows.iter().any(|l| l.contains(needle));
    assert!(has("'old' is deprecated."), "{err}");
    assert!(has("'older' is deprecated."), "{err}");
    assert!(has("'legacy_helper' is deprecated: since 2.0"), "{err}");
    assert!(has("'Legacy' is deprecated: use Modern instead"), "{err}");
    assert!(!has("fine"), "{rows:?}");
    assert_eq!(rows.len(), 4, "{rows:?}");
    let _ = std::fs::remove_dir_all(&dir);
}


/// A property typed by what the constructor writes to it — no declared
/// type, no docblock: `$this->mailer = new Mailer()` types `$this->mailer`
/// for every reader in the class.
#[cfg(feature = "php")]
#[test]
fn php_properties_are_typed_by_assignment() {
    let dir = std::env::temp_dir().join(format!("perl-lsp-d2assign-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let w = |rel: &str, src: &str| std::fs::write(dir.join(rel), src).unwrap();
    w("composer.json", "{\"autoload\": {\"psr-4\": {\"App\\\\\": \"src/\"}}}");
    w("src/Mailer.php", "<?php\nnamespace App;\nclass Mailer\n{\n    public function send(): bool { return true; }\n}\n");
    let svc = "<?php\nnamespace App;\nclass Service\n{\n    private $mailer;\n    public function __construct()\n    {\n        $this->mailer = new Mailer();\n    }\n    public function run(): bool\n    {\n        return $this->mailer->send();\n    }\n}\n";
    w("src/Service.php", svc);
    let lines: Vec<&str> = svc.lines().collect();
    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_perl-lsp"))
            .args(args)
            .env("XDG_CACHE_HOME", dir.join(".cache"))
            .output()
            .expect("run");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let root = dir.to_str().unwrap();
    let col = lines[11].find("mailer->").unwrap();
    let hover = run(&["--hover", root, "src/Service.php", "11", &col.to_string()]);
    assert!(hover.contains("mailer: Mailer"), "{hover}");
    let col = lines[11].find("send()").unwrap();
    let def = run(&["--definition", root, "src/Service.php", "11", &col.to_string()]);
    assert!(def.contains("src/Mailer.php:4:"), "{def}");
    let _ = std::fs::remove_dir_all(&dir);
}
