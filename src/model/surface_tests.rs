use super::*;

fn build(source: &str) -> crate::model::file_analysis::FileAnalysis {
    use tree_sitter::Parser;
    let mut parser = Parser::new();
    parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
    let tree = parser.parse(source, None).unwrap();
    crate::build::builder::build(&tree, source.as_bytes())
}

fn surface(source: &str) -> Surface {
    Surface::project(&build(source))
}

/// The Surface CANNOT gate `stamp_method_call_targets`, and this is why.
///
/// `MethodSurface::ret` is a LOCAL conclusion — projection runs with no module
/// index, so a return type that depends on another file is honestly `None`.
/// That collapses distinct cross-file answers onto the same projected value:
/// two versions of a provider whose enriched return types differ can have a
/// byte-identical Surface.
///
/// The consequence is not academic. `SurfaceVerdict::Unchanged` sets
/// `skip_consumers` — "consumers stay fresh, the walk stops here" — so an edit
/// like this invalidates nobody, while a consumer that froze a `MethodTarget`
/// derived from this sub's return type is now holding a stale answer. Gating
/// the re-stamp on freshness would therefore reintroduce exactly the silent
/// verb-disagreement the eager re-stamp exists to prevent.
///
/// The two bodies below differ in which cross-file call they return. Locally
/// neither resolves, so both project `ret: None`.
#[test]
fn a_cross_file_dependent_return_change_is_invisible_to_the_surface() {
    let a = "package Acme::P;\nour @EXPORT_OK = qw(pick);\n\
             sub pick {\n    my $x = Other::Alpha::make();\n    return $x;\n}\n1;\n";
    let b = "package Acme::P;\nour @EXPORT_OK = qw(pick);\n\
             sub pick {\n    my $x = Other::Beta::make();\n    return $x;\n}\n1;\n";

    let sa = surface(a);
    let sb = surface(b);

    // Precondition: the return really is unresolved locally in both. If a
    // future change makes these resolve without an index, this test is
    // measuring nothing and must be re-authored rather than deleted.
    let ret_of = |s: &Surface| {
        s.packages
            .iter()
            .find(|p| p.name == "Acme::P")
            .and_then(|p| p.methods.iter().find(|m| m.name == "pick"))
            .map(|m| m.ret.clone())
    };
    assert_eq!(ret_of(&sa), Some(None), "precondition: `pick` must not resolve locally");
    assert_eq!(ret_of(&sb), Some(None), "precondition: `pick` must not resolve locally");

    assert_eq!(
        sa, sb,
        "two provider bodies with DIFFERENT cross-file return types project the \
         same Surface — so `Unchanged` cannot mean `no consumer's frozen \
         MethodTarget moved`"
    );
}

/// R1 regression net: edits with NO cross-file-visible effect yield an
/// EQUAL Surface — this equality is the freshness firewall. Every Surface
/// field addition needs an arm here.
#[test]
fn body_edits_reformat_and_comments_keep_the_surface_equal() {
    let base = "package Acme::W;\nuse List::Util qw(sum);\nour @EXPORT_OK = qw(area);\nsub area {\n    my ($self, $w) = @_;\n    return $w * 2;\n}\nsub _private_helper { my $x = 1; return $x }\n1;\n";
    let s0 = surface(base);

    // Body-only edit: different math, same contract.
    let body_edit = base.replace("return $w * 2;", "my $tmp = $w + $w;\n    return $tmp;");
    assert_ne!(base, body_edit);
    assert_eq!(s0, surface(&body_edit), "body edit must not change the surface");

    // Reformat: whitespace + comment padding shifts every span.
    let reformatted = "package Acme::W;\n\n# a comment banner\nuse List::Util qw(sum);\n\nour @EXPORT_OK = qw(area);\n\nsub area {\n        my ($self, $w) = @_;\n        # doubled\n        return $w * 2;\n}\n\nsub _private_helper { my $x = 1; return $x }\n1;\n";
    assert_eq!(s0, surface(reformatted), "reformat must not change the surface");

    // Renaming a body-local variable.
    let local_rename = base.replace("$x", "$y");
    assert_eq!(s0, surface(&local_rename), "local rename must not change the surface");
}

/// The inverse net: every cross-file-visible edit class must FLIP equality.
#[test]
fn surface_changing_edits_are_unequal() {
    let base = "package Acme::W;\nour @EXPORT_OK = qw(area);\nsub area { my ($self, $w) = @_; return $w * 2; }\n1;\n";
    let s0 = surface(base);

    // Return-type change (number -> hashref) — the outline-blind case.
    let ret_edit = base.replace("return $w * 2;", "return { w => $w };");
    assert_ne!(s0, surface(&ret_edit), "return-type change must change the surface");

    // New public sub.
    let add_sub = base.replace("1;\n", "sub perimeter { my ($self) = @_; return 0 }\n1;\n");
    assert_ne!(s0, surface(&add_sub), "added method must change the surface");

    // Parent change.
    let add_parent = base.replace(
        "package Acme::W;\n",
        "package Acme::W;\nuse parent 'Acme::Base';\n",
    );
    assert_ne!(s0, surface(&add_parent), "@ISA change must change the surface");

    // Export-list change.
    let add_export = base.replace("qw(area)", "qw(area area2)");
    assert_ne!(s0, surface(&add_export), "export change must change the surface");

    // New import (a freshness EDGE change even with no member change).
    let add_import = base.replace(
        "package Acme::W;\n",
        "package Acme::W;\nuse Scalar::Util qw(blessed);\n",
    );
    assert_ne!(s0, surface(&add_import), "import change must change the surface");
}

/// `%EXPORT_TAGS` grouping is cross-file semantics on its own: moving a
/// member between tags keeps the flat `exports_ok` set identical while
/// changing what a consumer's `use Foo qw(:tag)` binds — the verdict must
/// flip to Changed on a tags-only header edit.
#[test]
fn export_tag_regrouping_flips_the_verdict() {
    use std::path::PathBuf;
    let base = "package Acme::T;\nour %EXPORT_TAGS = (math => ['area', 'perim'], io => ['slurp']);\nsub area { return 1 }\nsub perim { return 2 }\nsub slurp { return 3 }\n1;\n";
    // Move `perim` from :math to :io.
    let moved = "package Acme::T;\nour %EXPORT_TAGS = (math => ['area'], io => ['slurp', 'perim']);\nsub area { return 1 }\nsub perim { return 2 }\nsub slurp { return 3 }\n1;\n";
    let s0 = surface(base);
    let s1 = surface(moved);
    assert!(
        !s0.export_tags.is_empty(),
        "tags must project: {:?}",
        s0.export_tags
    );
    assert_eq!(
        s0.exports_ok, s1.exports_ok,
        "the flat export set is identical — only the grouping moved"
    );
    assert_ne!(s0, s1, "tag regrouping must change the surface");

    let idx = FreshnessIndex::default();
    let lib = PathBuf::from("/w/Tags.pm");
    assert_eq!(idx.record(&lib, s0), SurfaceVerdict::FirstSeen);
    assert_eq!(idx.record(&lib, s1), SurfaceVerdict::Changed);

    // Tag RENAME with unchanged membership flips too — the selector is
    // what consumers spell.
    let renamed = base.replace("math =>", "geometry =>");
    assert_ne!(surface(base), surface(&renamed), "tag rename must change the surface");
}

/// DBIC `source_name` is cross-file semantics: consumers' `resultset('X')`
/// resolve through it, and the edit changes no other projected field —
/// the verdict must flip to Changed on the header-only edit.
#[test]
fn dbic_source_name_edit_flips_the_verdict() {
    use std::path::PathBuf;
    let base = "package Acme::Schema::Result::Widget;\n__PACKAGE__->source_name('Widget');\nsub table { return 'widgets' }\n1;\n";
    let edited = base.replace("source_name('Widget')", "source_name('Gadget')");
    let s0 = surface(base);
    let s1 = surface(&edited);
    assert_eq!(s0.dbic_source_name.as_deref(), Some("Widget"));
    assert_eq!(s1.dbic_source_name.as_deref(), Some("Gadget"));
    assert_ne!(s0, s1, "source_name edit must change the surface");

    let idx = FreshnessIndex::default();
    let f = PathBuf::from("/w/Widget.pm");
    assert_eq!(idx.record(&f, s0), SurfaceVerdict::FirstSeen);
    assert_eq!(idx.record(&f, s1), SurfaceVerdict::Changed);
}

/// Surfaces ride bincode (the cache blob) — the projection must round-trip.
#[test]
fn surface_serde_roundtrip() {
    let s0 = surface(
        "package Acme::W;\nuse parent 'Acme::Base';\nsub area { my ($s,$w)=@_; return $w*2 }\n1;\n",
    );
    let bin = bincode::serialize(&s0).unwrap();
    let back: Surface = bincode::deserialize(&bin).unwrap();
    assert_eq!(s0, back);
}

/// The freshness engine: verdicts, the reverse-dep walk, transitivity
/// through a parent chain, and edge maintenance on re-record.
#[test]
fn freshness_dirty_closure_walks_imports_and_parent_chains() {
    use std::path::PathBuf;
    let idx = FreshnessIndex::default();
    let base = PathBuf::from("/w/Base.pm");
    let mid = PathBuf::from("/w/Mid.pm");
    let app = PathBuf::from("/w/App.pm");

    // Base::Level0 <- Mid extends it <- App imports Mid.
    let s_base = surface("package Base;\nsub greet { my ($s)=@_; return 'hi' }\n1;\n");
    let s_mid = surface("package Mid;\nuse parent 'Base';\nsub own { my ($s)=@_; return 1 }\n1;\n");
    let s_app = surface("package App;\nuse Mid;\nsub run { my ($s)=@_; return 2 }\n1;\n");

    assert_eq!(idx.record(&base, s_base.clone()), SurfaceVerdict::FirstSeen);
    assert_eq!(idx.record(&mid, s_mid), SurfaceVerdict::FirstSeen);
    assert_eq!(idx.record(&app, s_app), SurfaceVerdict::FirstSeen);

    // Re-recording an identical surface is the firewall.
    assert_eq!(idx.record(&base, s_base), SurfaceVerdict::Unchanged);

    // A surface CHANGE to Base dirties Mid (extends Base) and App
    // (imports Mid, whose enrichment reads through to Base) — transitive.
    let s_base2 = surface(
        "package Base;\nsub greet { my ($s)=@_; return 'hi' }\nsub extra { my ($s)=@_; return 3 }\n1;\n",
    );
    assert_eq!(idx.record(&base, s_base2), SurfaceVerdict::Changed);
    let dirty = idx.dirty_consumers(&base);
    assert!(dirty.contains(&mid), "direct extender is dirty");
    assert!(dirty.contains(&app), "transitive importer is dirty");
    assert!(!dirty.contains(&base), "the changed file itself is not in the closure");

    // Mid drops its parent edge — re-record maintains edges; Base's
    // closure loses the chain.
    let s_mid2 = surface("package Mid;\nsub own { my ($s)=@_; return 1 }\n1;\n");
    assert_eq!(idx.record(&mid, s_mid2), SurfaceVerdict::Changed);
    let dirty = idx.dirty_consumers(&base);
    assert!(!dirty.contains(&mid), "edge removed with the parent");
    assert!(!dirty.contains(&app), "chain broken");

    // Removal drops records + edges.
    idx.remove(&app);
    assert!(idx.dirty_consumers(&mid).is_empty());
}

/// cpp surfaces: file-scope values, class fields, and enum constants are
/// cross-file-visible (C linkage exports everything) — adding one must
/// flip equality, while a function-body edit must not.
#[cfg(feature = "cpp")]
#[test]
fn cpp_values_are_on_the_surface_and_bodies_are_not() {
    let build_cpp = |src: &str| {
        let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
        let driver = reg
            .for_path(std::path::Path::new("/fake/surface_test.cpp"))
            .expect("cpp driver");
        driver.analyze_with_path(src, None)
    };
    let base = "int counter = 0;\nenum Color { RED, GREEN };\nclass Box {\npublic:\n    int width;\n    int area() { return width * 2; }\n};\n";
    let s0 = Surface::project(&build_cpp(base));

    // Body edit inside a method: surface-equal.
    let body = base.replace("return width * 2;", "int w = width;\n        return w + w;");
    assert_eq!(s0, Surface::project(&build_cpp(&body)), "cpp body edit must not change the surface");

    // New file-scope global: unequal.
    let global = base.replace("int counter = 0;", "int counter = 0;\nint other = 1;");
    assert_ne!(s0, Surface::project(&build_cpp(&global)), "new global must change the surface");

    // New enum constant: unequal.
    let variant = base.replace("RED, GREEN", "RED, GREEN, BLUE");
    assert_ne!(s0, Surface::project(&build_cpp(&variant)), "new enum constant must change the surface");

    // New class field: unequal.
    let field = base.replace("int width;", "int width;\n    int height;");
    assert_ne!(s0, Surface::project(&build_cpp(&field)), "new field must change the surface");

    // Macro BODY change (same name): unequal — textual inclusion makes
    // the body cross-file semantics.
    let with_macro = format!("#define LIMIT 10\n{base}");
    let sm = Surface::project(&build_cpp(&with_macro));
    let macro_edit = with_macro.replace("#define LIMIT 10", "#define LIMIT 20");
    assert_ne!(
        sm,
        Surface::project(&build_cpp(&macro_edit)),
        "macro body change must change the surface"
    );
}

/// Free (package-less) callables — C's dominant export shape — are on the
/// surface: adding one, or changing one's signature, must flip equality;
/// a body edit must not. Perl duplicate definitions (last wins at runtime)
/// each surface, so editing the SECOND definition's contract flips too.
#[cfg(feature = "cpp")]
#[test]
fn cpp_free_functions_are_on_the_surface() {
    let build_cpp = |src: &str| {
        let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
        let driver = reg
            .for_path(std::path::Path::new("/fake/surface_free.c"))
            .expect("c driver");
        driver.analyze_with_path(src, None)
    };
    let base = "int helper(int x) { return x + 1; }\nint get(void) { return 0; }\n";
    let s0 = Surface::project(&build_cpp(base));

    // Body edit: equal.
    let body = base.replace("return x + 1;", "int y = x;\n    return y + 1;");
    assert_eq!(
        s0,
        Surface::project(&build_cpp(&body)),
        "free-function body edit must not change the surface"
    );

    // New free function: unequal.
    let added = format!("{base}int helper2(int a, int b) {{ return a + b; }}\n");
    assert_ne!(
        s0,
        Surface::project(&build_cpp(&added)),
        "new free function must change the surface"
    );

    // Arity change on an existing free function: unequal.
    let arity = base.replace("int helper(int x)", "int helper(int x, int y)");
    assert_ne!(
        s0,
        Surface::project(&build_cpp(&arity)),
        "free-function arity change must change the surface"
    );
}

/// A RENAMED (or deleted) package's consumers are exactly the files its
/// departure breaks — the dirty walk must seed from the names the
/// re-record DROPPED, not just the new surface's names.
#[test]
fn freshness_dirty_walk_covers_renamed_away_providers() {
    use std::path::PathBuf;
    let idx = FreshnessIndex::default();
    let lib = PathBuf::from("/w/Lib.pm");
    let user = PathBuf::from("/w/User.pm");

    let s_lib = surface("package Foo;\nsub make { my ($s)=@_; return 1 }\n1;\n");
    let s_user = surface("package User;\nuse Foo;\nsub go { my ($s)=@_; return 2 }\n1;\n");
    idx.record(&lib, s_lib);
    idx.record(&user, s_user);

    // Rename Foo -> Bar: User (consumer of the DEPARTED name) must dirty.
    let s_renamed = surface("package Bar;\nsub make { my ($s)=@_; return 1 }\n1;\n");
    assert_eq!(idx.record(&lib, s_renamed), SurfaceVerdict::Changed);
    let dirty = idx.dirty_consumers(&lib);
    assert!(
        dirty.contains(&user),
        "consumer of the renamed-away package must be in the dirty set"
    );

    // The NEXT re-record replaces the stale set: an unrelated change to
    // the renamed file no longer drags Foo's old consumers around forever
    // once they stopped depending on it... but User still imports Foo, so
    // its edge keeps it dirty through the consumers map regardless.
    let s_renamed2 =
        surface("package Bar;\nsub make { my ($s)=@_; return 1 }\nsub extra { my ($s)=@_; return 2 }\n1;\n");
    assert_eq!(idx.record(&lib, s_renamed2), SurfaceVerdict::Changed);
    let dirty2 = idx.dirty_consumers(&lib);
    assert!(
        !dirty2.contains(&user),
        "stale-provided names last exactly one re-record: Bar has no consumers"
    );
}
