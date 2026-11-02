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

/// `enrich_open` is THE open-doc enrichment writer: it derives the
/// enriched analysis as a fresh Arc, swaps it in, returns it — and the
/// build-time `baseline_surface` stays untouched, so freshness records
/// are enrichment-invariant by construction (no record-before-publish
/// ordering contract).
#[test]
fn enrich_open_swaps_derived_copy_and_keeps_baseline_surface() {
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
        std::path::PathBuf::from("/tmp/enrich_open_producer.pm"),
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

    let enriched = store.enrich_open(&url, &idx).expect("doc is open");
    assert!(
        !Arc::ptr_eq(&enriched, &base),
        "enrichment derives a fresh artifact, never mutates the stored one"
    );
    assert!(
        Arc::ptr_eq(&store.get_open(&url).unwrap().analysis, &enriched),
        "the derived copy is swapped in for query handlers"
    );
    let b_type = enriched.inferred_type_via_bag(
        "$b",
        tree_sitter::Point { row: 2, column: 0 },
    );
    assert_eq!(
        b_type.as_ref().and_then(|t| t.class_name()),
        Some("B"),
        "cross-file enrichment landed in the derived copy; got {:?}",
        b_type,
    );
    assert_eq!(
        store.get_open(&url).unwrap().baseline_surface.as_ref(),
        Some(&baseline),
        "the freshness source is untouched by enrichment"
    );
}

#[cfg(feature = "cpp")]
#[test]
fn enrich_open_leaves_pack_docs_untouched() {
    use crate::index::module_index::ModuleIndex;
    let reg = crate::build::language_driver::LanguageRegistry::compiled();
    let store = OpenDocs::serving(reg.for_id("cpp").unwrap());
    let url = Url::parse("file:///tmp/enrich_open_pack.cpp").unwrap();
    assert!(store.open(url.clone(), "class Box { public: int width; };\n".to_string()));
    let base = Arc::clone(&store.get_open(&url).unwrap().analysis);
    let idx = ModuleIndex::new_for_test();
    let out = store.enrich_open(&url, &idx).expect("doc is open");
    assert!(Arc::ptr_eq(&out, &base), "pack analyses have no import enrichment");
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
