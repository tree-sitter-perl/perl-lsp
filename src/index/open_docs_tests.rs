use super::*;

#[cfg(feature = "cpp")]
#[test]
fn open_analyzes_through_the_store_driver() {
    // The backend seam: a store serving C++ opens through the C++ driver,
    // so the document is language-tagged "cpp" and its analysis is the
    // C++ outline (here, a macro-recovered class). A Perl store opens Perl.
    let reg = crate::build::language_driver::LanguageRegistry::compiled();
    let store = OpenDocs::serving(reg.for_id("cpp").unwrap());
    let cpp = Url::parse("file:///tmp/route_test.cpp").unwrap();
    assert!(store.open(
        cpp.clone(),
        "#define API __attribute__((visibility(\"default\")))\nclass API Box { public: int width; };\n"
            .to_string()
    ));
    {
        // Scoped: `get_open` hands back a DashMap shard guard — holding it
        // across the next `open()` deadlocks whenever both URLs hash to the
        // same shard (seed-dependent, so it hangs one run in ~eight).
        let doc = store.get_open(&cpp).unwrap();
        assert_eq!(doc.language, "cpp");
        assert!(doc.analysis.symbols().iter().any(|s| s.name == "Box"), "cpp class via the driver");
    }

    let store = OpenDocs::serving(reg.for_id("perl").unwrap());
    let perl = Url::parse("file:///tmp/route_test.pm").unwrap();
    assert!(store.open(perl.clone(), "package Foo; sub bar { 1 }\n".to_string()));
    assert_eq!(store.get_open(&perl).unwrap().language, "perl");
}

fn parse(src: &str) -> FileAnalysis {
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(src, None).unwrap();
    crate::build::builder::build(&tree, src.as_bytes())
}

/// With no plugin gate holding, `condition_open` serves the pristine build
/// itself — no copy — and a cross-file type is answered at query time
/// through the index, never stored. The build-time `baseline_surface` is
/// untouched, so freshness records stay conditioning-invariant.
#[test]
fn condition_open_serves_the_build_and_types_cross_file_at_query_time() {
    use crate::index::module_index::ModuleIndex;

    let producer_src = r#"
package B;
use Exporter 'import';
our @EXPORT_OK = qw(make_b);

sub new    { return bless {}, shift }
sub make_b { return B->new }
1;
"#;
    let consumer_src = r#"
use B qw(make_b);
my $b = make_b();
1;
"#;
    let idx = ModuleIndex::new_for_test();
    idx.register_workspace_module(
        PathBuf::from("/tmp/condition_open_producer.pm"),
        Arc::new(parse(producer_src)),
    );

    let store = OpenDocs::new();
    let url = Url::parse("file:///tmp/enrich_open_consumer.pm").unwrap();
    assert!(store.open(url.clone(), consumer_src.to_string()));
    let base = Arc::clone(&store.get_open(&url).unwrap().analysis);
    let baseline = store
        .get_open(&url)
        .unwrap()
        .baseline_surface
        .clone()
        .expect("a perl doc retains its build-time surface");
    assert_eq!(
        baseline,
        crate::model::surface::Surface::project(&base),
        "baseline is the pristine build's projection"
    );

    let view = store.condition_open(&url, &idx).expect("doc is open");
    assert!(Arc::ptr_eq(&view, &base), "no gate holds, so there is nothing to derive");
    let pt = tree_sitter::Point { row: 2, column: 0 };
    assert_eq!(view.inferred_type_via_bag("$b", pt), None, "nothing cross-file is stored");
    let b_type = view.inferred_type_via_bag_ctx("$b", pt, Some(&idx));
    assert_eq!(
        b_type.as_ref().and_then(|t| t.class_name()),
        Some("B"),
        "the imported return resolves through the index; got {:?}",
        b_type,
    );
    assert_eq!(
        store.get_open(&url).unwrap().baseline_surface.as_ref(),
        Some(&baseline),
        "the freshness source is untouched by conditioning"
    );
}

#[cfg(feature = "cpp")]
#[test]
fn condition_open_leaves_pack_docs_untouched() {
    use crate::index::module_index::ModuleIndex;
    let reg = crate::build::language_driver::LanguageRegistry::compiled();
    let store = OpenDocs::serving(reg.for_id("cpp").unwrap());
    let url = Url::parse("file:///tmp/enrich_open_pack.cpp").unwrap();
    assert!(store.open(url.clone(), "class Box { public: int width; };\n".to_string()));
    let base = Arc::clone(&store.get_open(&url).unwrap().analysis);
    let idx = ModuleIndex::new_for_test();
    let out = store.condition_open(&url, &idx).expect("doc is open");
    assert!(Arc::ptr_eq(&out, &base), "pack analyses carry no plugin gates");
    assert!(store.get_open(&url).unwrap().baseline_surface.is_none());
}

#[test]
fn test_close_forgets_the_document() {
    let store = OpenDocs::new();
    let url = Url::parse("file:///tmp/close_test.pm").unwrap();

    assert!(store.open(url.clone(), "package Foo; 1;\n".to_string()));
    assert_eq!(store.open_count(), 1);

    store.close(&url);
    assert_eq!(store.open_count(), 0);
    assert!(store.get_open(&url).is_none());
}
