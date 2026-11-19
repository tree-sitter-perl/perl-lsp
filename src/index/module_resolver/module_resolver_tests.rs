use super::*;
use crate::build::dependency_map::Ask;
use crate::build::inc::{discover_inc_paths, map_over};
use crate::index::module_index::{strip_import_copy_one, FrontDoor};

/// The file a `require` of `name` loads under `roots`: the map's first.
fn winner(roots: &[PathBuf], name: &str) -> Option<PathBuf> {
    map_over(roots.to_vec()).locate(&Ask::Name(name.to_string())).into_iter().next().map(|l| l.path.into_path_buf())
}

#[test]
fn test_resolve_module_list_util() {
    let inc_paths = discover_inc_paths();
    if inc_paths.is_empty() {
        return;
    }
    let path = winner(&inc_paths, "List::Util");
    assert!(path.is_some(), "List::Util should be resolvable");
    let p = path.unwrap();
    assert!(p.to_str().unwrap().contains("List/Util.pm"));
}

#[test]
fn test_extract_exports_qw() {
    let source = r#"
package Foo;
use Exporter 'import';
our @EXPORT_OK = qw(alpha beta gamma);
our @EXPORT = qw(delta);
1;
"#;
    let mut parser = create_parser();
    let tree = parser.parse(source, None).unwrap();
    let analysis = crate::build::builder::build(&tree, source.as_bytes());
    assert_eq!(analysis.export, vec!["delta"]);
    assert_eq!(analysis.export_ok, vec!["alpha", "beta", "gamma"]);
}

#[test]
fn test_extract_exports_parenthesized() {
    let source = r#"
package Bar;
our @EXPORT_OK = ('foo', 'bar', 'baz');
1;
"#;
    let mut parser = create_parser();
    let tree = parser.parse(source, None).unwrap();
    let analysis = crate::build::builder::build(&tree, source.as_bytes());
    assert_eq!(analysis.export_ok, vec!["foo", "bar", "baz"]);
}

#[test]
fn test_discover_inc_paths() {
    let paths = discover_inc_paths();
    if !paths.is_empty() {
        assert!(paths.iter().all(|p| p.is_dir()));
    }
}

#[test]
fn insert_resolved_none_does_not_clobber_indexed_module() {
    // A workspace-indexed module (built with plugins, carries a Handler
    // symbol) must survive a later on-demand @INC miss for the same name.
    // The miss happens for project modules under a relative `use lib` the
    // resolver's @INC doesn't cover; clobbering with `None` while leaving
    // the reverse index pointing at it orphaned cross-file Handler /
    // dispatch lookup (mojo-events goto-def + sig help).
    let source = r#"
package Demo::Has::Event;
use parent 'Mojo::EventEmitter';
sub new {
    my $self = bless {}, shift;
    $self->on('ready', sub { my ($s, $ts) = @_; });
    $self;
}
1;
"#;
    let mut parser = create_parser();
    let tree = parser.parse(source, None).unwrap();
    let analysis = std::sync::Arc::new(crate::build::builder::build(&tree, source.as_bytes()));
    assert!(
        analysis.symbols().iter().any(|s| matches!(s.kind, crate::model::file_analysis::SymKind::Handler)),
        "fixture should synthesize a Handler symbol via the mojo-events plugin",
    );

    let core = IndexCore::new();
    let cached = Arc::new(CachedModule::new(crate::util::canonical_path::CanonicalPath::fixture(PathBuf::from("/x/Demo/Has/Event.pm")), analysis));

    // Workspace-index style insert: a resolved module.
    core.land_providers("Demo::Has::Event", Some(vec![cached]), false, false, 1, FrontDoor::Hub);
    assert!(core.cache.get("Demo::Has::Event").as_deref().unwrap().is_some());

    // On-demand resolver miss: `None`. Must NOT clobber the indexed copy.
    core.land_providers("Demo::Has::Event", None, false, false, 1, FrontDoor::Hub);
    assert!(
        core.cache.get("Demo::Has::Event").as_deref().unwrap().is_some(),
        "a None on-demand miss clobbered an already-indexed module",
    );

    // A genuine resolved entry still updates (sanity: the guard only
    // protects against None-over-Some).
    let tree2 = parser.parse(source, None).unwrap();
    let analysis2 = std::sync::Arc::new(crate::build::builder::build(&tree2, source.as_bytes()));
    let cached2 = Arc::new(CachedModule::new(crate::util::canonical_path::CanonicalPath::fixture(PathBuf::from("/y/Demo/Has/Event.pm")), analysis2));
    core.land_providers("Demo::Has::Event", Some(vec![cached2]), false, false, 1, FrontDoor::Hub);
    assert_eq!(
        core.cache.get("Demo::Has::Event").as_deref().unwrap().as_ref().unwrap().path,
        crate::util::canonical_path::CanonicalPath::fixture(PathBuf::from("/y/Demo/Has/Event.pm")),
    );
}

#[test]
fn test_uri_to_path() {
    assert_eq!(
        uri_to_path("file:///Users/foo/project"),
        Some(PathBuf::from("/Users/foo/project"))
    );
    assert_eq!(uri_to_path("http://example.com"), None);
}

#[test]
fn entrypoint_scan_finds_shebang_scripts_in_conventional_dirs() {
    let dir = std::env::temp_dir().join(format!("qx-entry-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    std::fs::create_dir_all(dir.join("script")).unwrap();
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    // root-level Perl entrypoint (no extension) — found
    std::fs::write(dir.join("jobs"), "#!/usr/bin/env perl\nuse Mojolicious::Lite;\n").unwrap();
    // bin/ + script/ entrypoints — found
    std::fs::write(dir.join("bin/login"), "#! /usr/bin/perl\n").unwrap();
    std::fs::write(dir.join("script/cron"), "#!/usr/bin/env perl\n").unwrap();
    // non-Perl shebang — not found
    std::fs::write(dir.join("deploy"), "#!/bin/bash\n").unwrap();
    // extensionless script buried in lib/ — NOT scanned by default
    std::fs::write(dir.join("lib/buried"), "#!/usr/bin/env perl\n").unwrap();

    let mut found: Vec<String> = scan_entrypoint_scripts(&dir, &[])
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    found.sort();
    assert_eq!(found, vec!["cron", "jobs", "login"]);

    // the config seam: an `extra` dir brings its entrypoints in.
    std::fs::create_dir_all(dir.join("daemons")).unwrap();
    std::fs::write(dir.join("daemons/worker"), "#!/usr/bin/env perl\n").unwrap();
    let mut with_extra: Vec<String> = scan_entrypoint_scripts(&dir, &["daemons".into()])
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    with_extra.sort();
    assert_eq!(with_extra, vec!["cron", "jobs", "login", "worker"]);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn workspace_index_progress_is_throttled_monotone_and_completes() {
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::Mutex;

    // A real-ish tree: enough files that per-file emission would be a storm,
    // so the throttle's effect is observable.
    let dir = std::env::temp_dir().join(format!("qx-progress-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let n_files = 240usize;
    for i in 0..n_files {
        std::fs::write(
            dir.join(format!("Mod{i}.pm")),
            format!("package Mod{i};\nsub run {{ my ($self) = @_; return {i}; }}\n1;\n"),
        )
        .unwrap();
    }

    // Mirror the backend's throttle: emit only on a >=2% advance or the final
    // tick. `emitted` is what a client would see as Report notifications.
    let last_pct = AtomicU8::new(0);
    let emitted: Mutex<Vec<(u8, usize, usize)>> = Mutex::new(Vec::new());
    let raw_ticks = std::sync::atomic::AtomicUsize::new(0);
    let cb = |done: usize, total: usize| {
        raw_ticks.fetch_add(1, Ordering::Relaxed);
        let pct = if total == 0 {
            100u8
        } else {
            ((done * 100 / total).min(100)) as u8
        };
        let prev = last_pct.fetch_max(pct, Ordering::Relaxed);
        if pct >= prev.saturating_add(2) || done >= total {
            emitted.lock().unwrap().push((pct, done, total));
        }
    };

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let indexed =
        index_workspace_with_index(&dir, &idx, Some(&cb as &(dyn Fn(usize, usize) + Sync)), None);
    std::fs::remove_dir_all(&dir).ok();

    assert_eq!(indexed, n_files, "all files indexed");
    // The callback fires once per file (no matter success/skip).
    assert_eq!(raw_ticks.load(Ordering::Relaxed), n_files);

    let emitted = emitted.into_inner().unwrap();
    // Bounded: a >=2% throttle caps Reports well under the file count. With 240
    // files this is ~50 max, never hundreds.
    assert!(
        emitted.len() <= 60,
        "throttled emission count should be bounded, got {}",
        emitted.len()
    );
    assert!(!emitted.is_empty(), "at least one Report");

    // Percentages are monotone non-decreasing (the client bar never rewinds).
    for w in emitted.windows(2) {
        assert!(w[1].0 >= w[0].0, "percent must not decrease: {:?}", emitted);
    }
    // The stream ends at 100% with done == total (the final Report before End).
    let (last_pct, last_done, last_total) = *emitted.last().unwrap();
    assert_eq!(last_pct, 100);
    assert_eq!(last_done, n_files);
    assert_eq!(last_total, n_files);
}

/// The @INC registration-owned strip: a persisted, non-degraded module's
/// resident copy drops its bag (rehydratable via the hub LRU); unpersisted
/// or eviction-off copies stay whole (the bag would be unrecoverable).
#[test]
fn import_tier_strip_gates_on_persistence() {
    let source = "package Strip;\nsub go { my $s = shift; return bless {}, 'X' }\n1;\n";
    let mut parser = create_parser();
    let tree = parser.parse(source, None).unwrap();
    let fa = crate::build::builder::build(&tree, source.as_bytes());
    assert!(!fa.witnesses.is_empty());
    let cm = Arc::new(CachedModule::new(
        crate::util::canonical_path::CanonicalPath::fixture(PathBuf::from("/inc/Strip.pm")),
        Arc::new(fa),
    ));

    let stripped = strip_import_copy_one(&cm, true, true);
    assert!(stripped.analysis.bag_is_evicted(), "persisted + eviction → bag drops");
    assert!(!stripped.analysis.symbols_are_evicted(), "symbols stay resident this slice");
    assert!(!stripped.analysis.refs_are_evicted(), "refs stay resident this slice");

    let whole = strip_import_copy_one(&cm, false, true);
    assert!(!whole.analysis.bag_is_evicted(), "unpersisted → bag unrecoverable → keep");
    let whole2 = strip_import_copy_one(&cm, true, false);
    assert!(!whole2.analysis.bag_is_evicted(), "NO_EVICT → keep");
}


/// A map move sets a flag, not a queue entry, so with nothing asked the
/// parked drain must still wake and hand back an empty batch — otherwise the
/// thread adopts the new map only when some unrelated ask arrives, and
/// `wait_idle` waits on a flag nobody clears. The batch marks the queue busy
/// as it is taken, so an idle check cannot slip between the two.
#[test]
fn a_map_move_wakes_a_parked_drain() {
    use std::sync::mpsc;

    let queue = Arc::new(ResolveQueue::new());
    let (tx, rx) = mpsc::channel();
    let q = Arc::clone(&queue);
    std::thread::spawn(move || {
        let _ = tx.send(drain_next_batch(&q));
    });

    // Let the drain park in `wait(pending)`; an earlier flag would be caught
    // by its first check and hide a lost wake.
    std::thread::sleep(std::time::Duration::from_millis(300));

    queue.map_moved.store(true, std::sync::atomic::Ordering::Release);
    queue.wake();

    let batch = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("a map move must wake the parked drain");
    assert!(batch.is_empty(), "a move alone is an empty batch, got {batch:?}");
    assert!(queue.busy.load(std::sync::atomic::Ordering::Acquire), "the taken batch marks the queue busy");
}

// ---- The @INC tier's candidate relation ----
//
// A module name maps to a SET of files, not to one file — XS/PP twins, a
// project `lib/` shadowing an installed copy, `t/lib` vs `lib` per
// entrypoint. `gold-corpus/KNOWN-GAPS.md`, "the @INC tier is still
// single-provider".

/// Two `@INC` roots, each providing `Twin`, with a sub only ONE of them
/// defines. Returns (root_a, root_b).
fn twin_roots(tag: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("perl-lsp-twin-{}-{}", tag, std::process::id()));
    let (a, b) = (base.join("a"), base.join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(
        a.join("Twin.pm"),
        "package Twin;\nsub only_in_a { 1 }\nsub shared { 1 }\n1;\n",
    )
    .unwrap();
    std::fs::write(
        b.join("Twin.pm"),
        "package Twin;\nsub only_in_b { 1 }\nsub shared { 1 }\n1;\n",
    )
    .unwrap();
    (a, b)
}

#[test]
fn inc_resolution_enumerates_every_providing_root_in_inc_order() {
    let (a, b) = twin_roots("paths");
    let found: Vec<_> =
        map_over(vec![a.clone(), b.clone()]).locate(&Ask::Name("Twin".into())).into_iter().map(|l| l.path).collect();
    assert_eq!(
        found,
        vec![crate::util::canonical_path::CanonicalPath::resolve(a.join("Twin.pm")), crate::util::canonical_path::CanonicalPath::resolve(b.join("Twin.pm"))],
        "both roots provide Twin; the relation must hold both, @INC order first",
    );
    // The winner is still exactly what `require` would load.
    assert_eq!(
        winner(&[a.clone(), b.clone()], "Twin"),
        Some(crate::util::canonical_path::CanonicalPath::resolve(a.join("Twin.pm")).into_path_buf()),
    );
    // Reversing @INC reverses the winner, not the membership.
    assert_eq!(
        winner(&[b.clone(), a.clone()], "Twin"),
        Some(crate::util::canonical_path::CanonicalPath::resolve(b.join("Twin.pm")).into_path_buf()),
    );
}

#[test]
fn a_name_keeps_every_provider_not_just_the_last_inserted() {
    // Base behavior (single-provider tier): the second insert REPLACED the
    // first in the one name-keyed slot and `def_candidates` fell back to
    // that winner — one candidate, and the other provider's subs were
    // unreachable. Both providers must survive as candidates.
    use crate::model::file_analysis::CrossFileLookup;
    let (a, b) = twin_roots("relation");
    let mut parser = create_parser();
    let providers =
        resolve_and_parse_all(&map_over(vec![a.clone(), b.clone()]), "Twin", &mut parser).expect("Twin resolves");
    assert_eq!(providers.len(), 2, "both providers parse");

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    // One insert per provider — the single-copy front door, called twice,
    // is exactly the shape that used to lose the first provider.
    for m in &providers {
        idx.insert_cache("Twin", Some(Arc::clone(m)));
    }

    let cands = idx.def_candidates("Twin");
    assert_eq!(
        cands.len(),
        2,
        "the relation dropped a provider: {:?}",
        cands.iter().map(|c| c.path.clone()).collect::<Vec<_>>(),
    );

    // The payoff: a sub that lives ONLY in the shadowed provider still
    // resolves to the file that defines it.
    let owner = idx
        .candidate_defining_sub("Twin", "only_in_b")
        .expect("only_in_b is defined by the shadowed provider");
    assert_eq!(owner.path, crate::util::canonical_path::CanonicalPath::resolve(b.join("Twin.pm")));
    let owner_a = idx
        .candidate_defining_sub("Twin", "only_in_a")
        .expect("only_in_a is defined by the winning provider");
    assert_eq!(owner_a.path, crate::util::canonical_path::CanonicalPath::resolve(a.join("Twin.pm")));
}

#[test]
fn a_workspace_file_still_shadows_an_inc_provider_of_the_same_name() {
    // The two tiers now SHARE `all_defs`, so the winner pick must stay
    // tier-aware: project code shadows an installed copy, and the path
    // tie-break has no opinion about which tier a candidate came from.
    use crate::model::file_analysis::CrossFileLookup;
    let base = std::env::temp_dir().join(format!("perl-lsp-shadow-{}", std::process::id()));
    let (inc, ws) = (base.join("inc"), base.join("ws"));
    std::fs::create_dir_all(&inc).unwrap();
    std::fs::create_dir_all(&ws).unwrap();
    // The @INC path sorts BEFORE the workspace path, so a tier-blind
    // smallest-path tie-break would hand it the slot.
    let inc_pm = inc.join("Shadowed.pm");
    let ws_pm = ws.join("Shadowed.pm");
    assert!(inc_pm < ws_pm, "fixture needs the @INC path to sort first");
    let inc_src = "package Shadowed;\nsub from_inc { 1 }\n1;\n";
    let ws_src = "package Shadowed;\nsub from_workspace { 1 }\n1;\n";
    std::fs::write(&inc_pm, inc_src).unwrap();
    std::fs::write(&ws_pm, ws_src).unwrap();

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let mut parser = create_parser();
    let mut build = |src: &str| {
        let tree = parser.parse(src, None).unwrap();
        Arc::new(crate::build::builder::build(&tree, src.as_bytes()))
    };
    let inc_fa = build(inc_src);
    let ws_fa = build(ws_src);

    idx.insert_cache("Shadowed", Some(Arc::new(CachedModule::new(crate::util::canonical_path::CanonicalPath::resolve(inc_pm.clone()), inc_fa))));
    idx.register_workspace_module(ws_pm.clone(), ws_fa);

    assert_eq!(
        idx.get_cached("Shadowed").map(|c| c.path.to_path_buf()),
        Some(std::fs::canonicalize(&ws_pm).unwrap_or(ws_pm.clone())),
        "the workspace file must hold the name slot",
    );
    // Both remain candidates — shadowing decides the winner, not membership.
    assert_eq!(idx.def_candidates("Shadowed").len(), 2);
}

#[test]
fn use_lib_declares_the_files_own_search_path_roots() {
    // `use lib` is the per-asker half of module visibility: a test with
    // `use lib 't/lib'` and an app file without it do NOT see the same
    // provider for the same module name.
    let src = r#"
use lib 't/lib';
use lib qw(lib local/lib/perl5);
use lib "$FindBin::Bin/../lib";
use strict;
package App;
1;
"#;
    let mut parser = create_parser();
    let tree = parser.parse(src, None).unwrap();
    let fa = crate::build::builder::build(&tree, src.as_bytes());
    assert_eq!(
        fa.lib_roots,
        vec!["t/lib", "lib", "local/lib/perl5", "$FindBin::Bin/../lib"],
        "both the single-string and qw spellings count; roots are stored as \
         written, and one that names no directory drops out at resolution",
    );
    // `use strict` is not a search-path declaration.
    assert!(!fa.lib_roots.iter().any(|r| r == "strict"));
}

#[test]
fn two_askers_with_different_use_lib_see_different_providers() {
    // The point of the exercise: `@INC` is per-entrypoint, so the SAME
    // module name means different files to different askers. A test file
    // with `use lib 't/lib'` must resolve `Twin` to the t/lib copy while
    // the app file resolves it to the lib copy — one relation, two
    // visibility rules, decided at CandidateSet construction so every
    // projection inherits it.
    use crate::model::file_analysis::{CrossFileLookup, ScopedLookup};
    // Fixed-width pid: the path is spelled into an analyzed source, and the
    // equivalence dump compares that source's spans across two test runs.
    let base = std::env::temp_dir().join(format!("perl-lsp-asker-{:010}", std::process::id()));
    let (applib, testlib) = (base.join("lib"), base.join("t").join("lib"));
    std::fs::create_dir_all(&applib).unwrap();
    std::fs::create_dir_all(&testlib).unwrap();
    std::fs::write(applib.join("Twin.pm"), "package Twin;\nsub which { 'app' }\n1;\n").unwrap();
    std::fs::write(testlib.join("Twin.pm"), "package Twin;\nsub which { 'test' }\n1;\n").unwrap();

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let mut parser = create_parser();
    // The process @INC has only the app lib: `t/lib` is a root ONLY the
    // test file declares, which is exactly the discriminator.
    idx.set_search_roots_for_test(&[applib.clone()]);
    let providers = resolve_and_parse_all(&map_over(vec![applib.clone(), testlib.clone()]), "Twin", &mut parser)
        .expect("Twin resolves");
    assert_eq!(providers.len(), 2);
    idx.insert_cache_providers("Twin", Some(providers));

    let mut build_origin = |src: &str| {
        let tree = parser.parse(src, None).unwrap();
        crate::build::builder::build(&tree, src.as_bytes())
    };
    let app_fa = build_origin("package App;\nuse Twin;\n1;\n");
    let test_fa = build_origin(&format!(
        "use lib '{}';\nuse Twin;\n1;\n",
        testlib.display()
    ));

    let resolved_by = |fa: &crate::model::file_analysis::FileAnalysis| {
        let scoped = ScopedLookup::for_origin(&idx, fa, None, crate::model::file_analysis::PackVisibility::Host);
        scoped.get_cached("Twin").map(|c| c.path.to_path_buf())
    };

    assert_eq!(
        resolved_by(&app_fa),
        Some(applib.join("Twin.pm")),
        "the app file sees only the process @INC, so Twin is the lib copy",
    );
    assert_eq!(
        resolved_by(&test_fa),
        Some(testlib.join("Twin.pm")),
        "`use lib 't/lib'` puts that root FIRST for this asker, so the same \
         name means the t/lib copy",
    );
}

// ---- The bounded persist queue ----

/// The cap has to be a property of the design, not of the corpus: an
/// unbounded channel parks whatever the walk outruns, which at 138k files is
/// about four fifths of the corpus in RAM. With a bound, a producer that gets
/// ahead is throttled to writer rate, so peak in-flight can never exceed the
/// depth however large the tree is.
#[test]
fn a_full_queue_throttles_the_producer_to_drain_rate() {
    use std::sync::atomic::{AtomicUsize, Ordering as O};

    // Depth is floored at one chunk, so ask for exactly that.
    std::env::set_var("PERL_LSP_WRITE_QUEUE_DEPTH", "1");
    let depth = write_queue_depth();
    std::env::remove_var("PERL_LSP_WRITE_QUEUE_DEPTH");
    assert_eq!(depth, PERSIST_CHUNK, "depth floors at one transaction");

    let (tx, rx) = std::sync::mpsc::sync_channel::<usize>(depth);
    let sent = Arc::new(AtomicUsize::new(0));
    let received = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));

    let total = depth * 4;
    let (s, r, p) = (Arc::clone(&sent), Arc::clone(&received), Arc::clone(&peak));
    let producer = std::thread::spawn(move || {
        for i in 0..total {
            send_to_writer(&tx, i);
            // Sampled AFTER the send lands, so it can only over-report.
            let in_flight = s.fetch_add(1, O::SeqCst) + 1 - r.load(O::SeqCst);
            p.fetch_max(in_flight, O::SeqCst);
        }
    });

    let mut drained = 0usize;
    while let Ok(_e) = rx.recv() {
        received.fetch_add(1, O::SeqCst);
        drained += 1;
        // Slow consumer: the producer must be the one that waits.
        std::thread::sleep(std::time::Duration::from_micros(200));
    }
    producer.join().unwrap();

    assert_eq!(drained, total, "every entry still reaches the writer");
    let observed = peak.load(O::SeqCst);
    assert!(
        observed <= depth + 1,
        "peak in-flight {observed} exceeded the {depth}-entry bound — the queue is not \
         throttling the producer"
    );
}

/// A writer that dies must not wedge the walk. `send_to_writer` returns on a
/// disconnected receiver instead of parking forever, so the index degrades to
/// "these files are not persisted this run" rather than hanging.
#[test]
fn a_dead_writer_releases_the_producer_instead_of_hanging() {
    let (tx, rx) = std::sync::mpsc::sync_channel::<usize>(PERSIST_CHUNK);
    drop(rx);
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        // More entries than the depth: with a blocking send against a live
        // but stalled receiver this would never return.
        for i in 0..(PERSIST_CHUNK * 2) {
            send_to_writer(&tx, i);
        }
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("a disconnected receiver must release the producer");
}

/// End-to-end over the real harness: more entries than the queue holds still
/// all reach `on_committed`. Pins that bounding did not introduce a stall or
/// drop between the throttled producer and the batching drain.
#[test]
fn the_writer_drains_more_entries_than_the_queue_holds() {
    use std::sync::atomic::{AtomicUsize, Ordering as O};
    std::env::set_var("PERL_LSP_WRITE_QUEUE_DEPTH", "1"); // → PERSIST_CHUNK
    let (tx, rx) = bounded_persist_channel::<usize>();
    std::env::remove_var("PERL_LSP_WRITE_QUEUE_DEPTH");

    let committed = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&committed);
    let total = PERSIST_CHUNK * 3 + 7;
    let producer = std::thread::spawn(move || {
        for i in 0..total {
            send_to_writer(&tx, i);
        }
    });
    // No connection: the harness drains unregistered, which is enough to
    // prove the producer/consumer pairing. The committed lane is exercised by
    // the persist-lane tests in `pack_invalidator_tests`.
    run_persist_writer(rx, None, "test", |_c| (), |_c, (), _e: &usize| true, |_e| { c.fetch_add(1, O::SeqCst); }, |_e| {});
    producer.join().unwrap();
    assert_eq!(committed.load(O::SeqCst), 0, "no connection ⇒ drained unregistered");
}

/// A bulk index marks the consumers of every file whose surface CHANGED.
///
/// The gap this pins was a discarded return value. The bulk walk already
/// RECORDED each file's surface — it called `record_surface` and threw the
/// verdict away — so the freshness engine's write half ran and its read half
/// never did. Nothing downstream could notice: the records were correct, the
/// index was complete, and the only symptom was a re-stamp gate that stayed
/// inert forever because no bulk ever marked anyone.
///
/// Two files, one importing the other. Index once to establish records and
/// the consumer edge, edit the provider, index again: the consumer must come
/// back marked, and marked at an epoch strictly newer than a stamp taken
/// before the second bulk.
#[test]
fn a_bulk_index_marks_the_consumers_of_what_changed() {
    use crate::model::file_analysis::CrossFileLookup;

    let dir = std::env::temp_dir().join(format!("plsp_bulkmark_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let provider = dir.join("BulkProv.pm");
    let consumer = dir.join("BulkCons.pm");
    std::fs::write(
        &provider,
        "package BulkProv;\nsub build { return bless {}, 'Widget::One' }\n1;\n",
    )
    .unwrap();
    std::fs::write(
        &consumer,
        "package BulkCons;\nuse BulkProv;\nsub run { my $w = BulkProv->build; return $w->go }\n1;\n",
    )
    .unwrap();

    let idx = crate::index::module_index::ModuleIndex::new_for_cli();
    index_workspace_with_index(&dir, &idx, None, None);

    let canon_cons = std::fs::canonicalize(&consumer).unwrap();
    let before = idx.flush_epoch();

    // The provider's surface moves: a different return class.
    std::fs::write(
        &provider,
        "package BulkProv;\nsub build { return bless {}, 'Widget::Two' }\n1;\n",
    )
    .unwrap();
    index_workspace_with_index(&dir, &idx, None, None);

    let after = idx.flush_epoch();
    assert!(
        after > before,
        "the bulk minted a mark epoch; without one the clock never moves"
    );

    // The DISCRIMINATING assertion, and the reason the obvious one is not.
    // "a pre-bulk stamp is owed" is satisfied by the gate's fail-open default
    // — an unmarked path is owed too — so it passes whether or not the bulk
    // marked anything, and a first draft of this test did exactly that.
    // Only the positive direction separates them: a stamp at the post-mark
    // clock is COVERED, which requires a mark to exist at or below it.
    assert!(
        !idx.restamp_owed(&canon_cons, Some(after)),
        "the consumer carries no mark, so the gate is failing open rather \
         than answering — the bulk never routed its Changed verdict to \
         dirty_consumers"
    );
    assert!(
        idx.restamp_owed(&canon_cons, Some(before)),
        "and a stamp predating the bulk is still owed"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A file whose own blob fails to save inside a chunk that still commits
/// must stay WHOLE: a copy stripped against a blob that is not on disk
/// answers empty. Its chunk siblings landed and must strip. The failure is
/// injected with a statement-scoped `RAISE(ABORT)` trigger, so the
/// transaction commits with the siblings' rows.
#[test]
fn a_workspace_file_whose_save_failed_in_a_committed_chunk_stays_whole() {
    let base = std::env::temp_dir().join(format!("ws-perfile-strip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (root, cache) = (base.join("root"), base.join("cache"));
    std::fs::create_dir_all(&root).unwrap();
    for name in ["Alpha", "Beta", "Gamma"] {
        std::fs::write(
            root.join(format!("{name}.pm")),
            format!("package {name};\nsub run {{ my $self = shift; return {{ k => 1 }}; }}\n1;\n"),
        )
        .unwrap();
    }
    let failing = std::fs::canonicalize(root.join("Beta.pm")).unwrap();

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    crate::index::module_cache::with_test_cache_dir(&cache, std::time::Duration::from_secs(10), || {
        {
            let conn = crate::index::module_cache::open_cache_db(None, "perl").expect("test cache db");
            let target = failing.to_string_lossy().replace('\'', "''");
            conn.execute_batch(&format!(
                "CREATE TRIGGER kill_one BEFORE INSERT ON modules \
                 WHEN NEW.path = '{target}' \
                 BEGIN SELECT RAISE(ABORT, 'injected'); END;"
            ))
            .unwrap();
        }
        index_workspace_with_index(&root, &idx, None, None);
    });

    let mut seen = 0;
    idx.for_each_registered_file(&mut |cm| {
        seen += 1;
        let stripped = cm.analysis.symbols_are_evicted() || cm.analysis.bag_is_evicted();
        if cm.path.as_path() == failing.as_path() {
            assert!(!stripped, "{:?} was stripped though its blob never landed", cm.path);
        } else {
            assert!(stripped, "{:?} landed but was not stripped", cm.path);
        }
    });
    assert_eq!(seen, 3, "every file must stay registered");
    let _ = std::fs::remove_dir_all(&base);
}

/// The tier's rows are path-keyed and are the landing's parse cache: a
/// first landing writes one `dependency` row keyed by the file's path; a
/// second landing in a fresh index takes the file from that row instead of
/// parsing it; a miss writes nothing.
#[test]
fn a_landing_reads_the_files_row_in_place_of_parsing_it() {
    let root = std::env::temp_dir().join(format!("d3_land_rows_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Row")).unwrap();
    let pm = root.join("Row/Cached.pm");
    std::fs::write(&pm, "package Row::Cached;\nsub aa { 1 }\n1;\n").unwrap();
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    module_cache::init_schema(&conn).unwrap();
    let inc = map_over(vec![root.clone()]);
    let mut parser = Parse::Hub(create_parser());

    let core = IndexCore::new();
    let (_, stored) = land_module(&core, Some(&conn), &inc, 1, "Row::Cached", &mut parser, &mut Prepared::new(), false);
    assert!(stored.is_some());
    let canon = crate::util::canonical_path::CanonicalPath::resolve(pm.clone());
    let (key, source): (String, String) = conn
        .query_row("SELECT module_name, source FROM modules", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap();
    assert_eq!((key.as_str(), source.as_str()), (&*canon.to_string_lossy(), module_cache::DEPENDENCY_SOURCE));

    // Same size, same mtime, different body: only a row read can still
    // answer `aa`.
    let mtime = std::fs::metadata(&pm).unwrap().modified().unwrap();
    std::fs::write(&pm, "package Row::Cached;\nsub bb { 1 }\n1;\n").unwrap();
    std::fs::File::options().write(true).open(&pm).unwrap().set_modified(mtime).unwrap();
    let core = IndexCore::new();
    let (whole, _) = land_module(&core, Some(&conn), &inc, 1, "Row::Cached", &mut parser, &mut Prepared::new(), false);
    let names: Vec<String> = whole.unwrap()[0].analysis.symbols().iter().map(|s| s.name.clone()).collect();
    assert!(names.iter().any(|n| n == "aa") && !names.iter().any(|n| n == "bb"), "{names:?}");

    let (_, missed) = land_module(&core, Some(&conn), &inc, 1, "Row::Absent", &mut parser, &mut Prepared::new(), false);
    assert!(missed.is_none());
    let rows: i64 = conn.query_row("SELECT COUNT(*) FROM modules", [], |r| r.get(0)).unwrap();
    assert_eq!(rows, 1, "a miss is never remembered");
    let _ = std::fs::remove_dir_all(&root);
}

/// A batch prepared in the pool lands what the serial landing lands: the
/// same providers, in the same order, from the same rows. The rows are read
/// through the reader pool, not the resolver's connection: a body changed
/// under an unchanged stamp still answers from its row.
#[test]
fn a_prepared_batch_lands_what_a_serial_one_does() {
    let root = std::env::temp_dir().join(format!("d9b_prepared_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (a, b) = (root.join("a"), root.join("b"));
    for (dir, body) in [(&a, "sub in_a { 1 }"), (&b, "sub in_b { 1 }")] {
        std::fs::create_dir_all(dir.join("Prep")).unwrap();
        std::fs::write(dir.join("Twin.pm"), format!("package Twin;\n{body}\n1;\n")).unwrap();
    }
    std::fs::write(a.join("Prep/One.pm"), "package Prep::One;\nsub aa { 1 }\n1;\n").unwrap();
    std::fs::write(a.join("Prep/Two.pm"), "package Prep::Two;\nuse parent 'Twin';\nsub two { 1 }\n1;\n").unwrap();
    let db_path = root.join("modules.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    module_cache::init_schema(&conn).unwrap();
    let map = map_over(vec![a.clone(), b.clone()]);
    let batch: Vec<Ask> = ["Prep::One", "Twin", "Prep::Two", "Prep::Absent"].map(|n| Ask::Name(n.to_string())).to_vec();
    let driver = crate::build::language_driver::LanguageRegistry::served();
    let reader = module_cache::RetainedReader::new();

    let land_all = |core: &IndexCore, prepared: &mut Prepared| -> Vec<(String, Vec<String>)> {
        let mut parse = Parse::of(driver);
        let mut out = Vec::new();
        for ask in &batch {
            let Ask::Name(n) = ask else { unreachable!() };
            for m in landing_names(core, &map, n) {
                let (whole, _) = land_module(core, Some(&conn), &map, 1, &m, &mut parse, prepared, false);
                for cm in whole.into_iter().flatten() {
                    let mut syms: Vec<String> = cm.analysis.symbols().iter().map(|s| s.name.clone()).collect();
                    syms.sort();
                    out.push((cm.path.to_string_lossy().into_owned(), syms));
                }
            }
        }
        out
    };

    let serial = land_all(&IndexCore::new(), &mut Prepared::new());
    assert_eq!(serial.len(), 4, "One, both Twins, Two: {serial:?}");

    let core = IndexCore::new();
    let mut prepared = prepare_batch(&core, &map, 1, &batch, driver, None, &reader);
    assert_eq!(prepared.len(), 4, "every located file is prepared once");
    assert_eq!(land_all(&core, &mut prepared), serial, "parsed in the pool");
    assert!(prepared.is_empty(), "every prepared file was landed");

    // Same size, same stamp, new body: only a row read answers `aa`.
    let one = a.join("Prep/One.pm");
    let mtime = std::fs::metadata(&one).unwrap().modified().unwrap();
    std::fs::write(&one, "package Prep::One;\nsub bb { 1 }\n1;\n").unwrap();
    std::fs::File::options().write(true).open(&one).unwrap().set_modified(mtime).unwrap();
    let core = IndexCore::new();
    let mut prepared = prepare_batch(&core, &map, 1, &batch, driver, Some(&db_path), &reader);
    assert_eq!(land_all(&core, &mut prepared), serial, "read from the rows");

    // A held file is left to its landing.
    let core = IndexCore::new();
    land_all(&core, &mut Prepared::new());
    assert!(prepare_batch(&core, &map, 1, &batch, driver, None, &reader).is_empty());
    let _ = std::fs::remove_dir_all(&root);
}

/// A located file the project registered is the walk's: the tier does not
/// land a second copy of it, and the project's record stays.
#[test]
fn the_tier_does_not_land_a_project_file() {
    use crate::model::file_analysis::CrossFileLookup;
    let source = "package Mine::Here;\nsub f { 1 }\n1;\n";
    let mut parser = create_parser();
    let tree = parser.parse(source, None).unwrap();
    let analysis = Arc::new(crate::build::builder::build(&tree, source.as_bytes()));
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let path = PathBuf::from("/proj/lib/Mine/Here.pm");
    idx.register_workspace_module(path.clone(), Arc::clone(&analysis));
    let canon = crate::util::canonical_path::CanonicalPath::resolve(path);
    let copy = Arc::new(CachedModule::new(canon.clone(), analysis));
    idx.insert_cache("Mine::Alias", Some(copy));
    assert!(idx.get_cached("Mine::Alias").is_none(), "the walk's file is not landed again");
    assert_eq!(idx.file_role(&canon), Some(crate::model::file_analysis::FileRole::Workspace));
}

/// Carton's `local/` is the dependency map's tier root: the walk leaves it
/// out even though nothing ignores it, so its files reach the index only as
/// dependencies, through the map.
#[test]
fn the_walk_leaves_the_map_s_tier_root_to_the_tier() {
    use crate::model::file_analysis::{CrossFileLookup, FileRole};
    let dir = std::env::temp_dir().join(format!("d4_tier_root_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("lib")).unwrap();
    std::fs::create_dir_all(dir.join("local/lib/perl5")).unwrap();
    std::fs::write(dir.join("lib/App.pm"), "package App;\nuse Dep;\n1;\n").unwrap();
    std::fs::write(dir.join("local/lib/perl5/Dep.pm"), "package Dep;\nsub d { 1 }\n1;\n").unwrap();
    let dir = crate::util::canonical_path::CanonicalPath::resolve(dir).into_path_buf();

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    index_workspace_with_index(&dir, &idx, None, None);
    let app = crate::util::canonical_path::CanonicalPath::resolve(dir.join("lib/App.pm"));
    let dep = crate::util::canonical_path::CanonicalPath::resolve(dir.join("local/lib/perl5/Dep.pm"));
    assert_eq!(idx.file_role(&app), Some(FileRole::Workspace));
    assert_eq!(idx.file_role(&dep), None, "the walk does not register the tier's file");

    let map = idx.dependency_map_for(&crate::build::language_driver::PerlDriver, Some(&dir)).expect("perl reads a map");
    assert_eq!(map.search_roots()[0], dir.join("local/lib/perl5"));
    let mut parser = create_parser();
    let (_, stored) = idx.land_module(None, &map, "Dep", &mut parser);
    assert!(stored.is_some());
    assert_eq!(idx.file_role(&dep), Some(FileRole::Dependency), "the map lands it as a dependency");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A php project's `vendor/` is the Composer map's tier root: the walk
/// leaves it out though nothing ignores it, and a class the map places
/// lands through the pack door as a dependency, its own parent edge
/// published as a project file's would be.
#[cfg(feature = "php")]
#[test]
fn a_vendor_class_lands_through_the_pack_door_from_composer_s_map() {
    use crate::model::file_analysis::{CrossFileLookup, FileRole};
    let php = crate::build::language_driver::LanguageRegistry::compiled().for_id("php").expect("php driver");
    let dir = std::env::temp_dir().join(format!("d7_vendor_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let files = [
        ("composer.json", r#"{"autoload": {"psr-4": {"App\\": "src/"}}}"#),
        (
            "vendor/composer/installed.json",
            r#"{"packages": [{"name": "acme/base", "install-path": "../acme/base",
                "autoload": {"psr-4": {"Acme\\": "src/"}}}]}"#,
        ),
        ("vendor/acme/base/src/Root.php", "<?php\nnamespace Acme;\nclass Root {}\n"),
        ("vendor/acme/base/src/Base.php", "<?php\nnamespace Acme;\nclass Base extends Root { public function hello() {} }\n"),
        ("src/User.php", "<?php\nnamespace App;\nuse Acme\\Base;\nclass User extends Base {}\n"),
    ];
    for (rel, body) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    let dir = crate::util::canonical_path::CanonicalPath::resolve(dir).into_path_buf();
    let canon = |rel: &str| crate::util::canonical_path::CanonicalPath::resolve(dir.join(rel));

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    index_pack_language(php, &dir, None, &idx, None, 0);
    assert_eq!(idx.file_role(&canon("src/User.php")), Some(FileRole::Workspace));
    assert_eq!(idx.file_role(&canon("vendor/acme/base/src/Base.php")), None, "the walk leaves `vendor/` to the tier");

    let map = idx.dependency_map().expect("the walk read php's map");
    let (_, stored) = idx.land_pack_module(&map, "Acme\\Base", php);
    assert!(stored.is_some(), "Acme\\Base lands");
    let base = canon("vendor/acme/base/src/Base.php");
    assert_eq!(idx.file_role(&base), Some(FileRole::Dependency));
    assert_eq!(idx.get_cached("Acme\\Base").map(|m| m.path.clone()), Some(base));
    assert!(
        idx.direct_children_of("Acme\\Root").iter().any(|(child, _)| child == "Acme\\Base"),
        "a landed class's parent edge is published"
    );
    assert!(idx.land_pack_module(&map, "App\\User", php).1.is_none(), "the project's own class is the walk's");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A Composer `files` entry is declared demand: the map hands it out by
/// path, and it lands under every name it declares — a guarded global
/// helper and a namespaced one — so a call with no import resolves. A
/// class a project file only references (no `use`, no `extends`) is
/// demand too.
#[cfg(feature = "php")]
#[test]
fn a_composer_files_entry_lands_under_every_name_it_declares() {
    use crate::build::dependency_map::Ask;
    use crate::model::file_analysis::{CrossFileLookup, FileRole};
    let php = crate::build::language_driver::LanguageRegistry::compiled().for_id("php").expect("php driver");
    let dir = std::env::temp_dir().join(format!("d9_files_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let files = [
        ("composer.json", r#"{"autoload": {"psr-4": {"App\\": "src/"}}}"#),
        (
            "vendor/composer/installed.json",
            r#"{"packages": [{"name": "acme/support", "install-path": "../acme/support",
                "autoload": {"psr-4": {"Acme\\": "src/"}, "files": ["src/helpers.php"]}}]}"#,
        ),
        (
            "vendor/acme/support/src/helpers.php",
            "<?php\nnamespace Acme {\nfunction tap($v) { return $v; }\n}\nnamespace {\nif (! function_exists('env')) {\n    function env($k) { return $k; }\n}\n}\n",
        ),
        ("src/Config.php", "<?php\nnamespace App;\nclass Config { function get() { return \\Acme\\Str::of(env('X')); } }\n"),
    ];
    for (rel, body) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    let dir = crate::util::canonical_path::CanonicalPath::resolve(dir).into_path_buf();
    let canon = |rel: &str| crate::util::canonical_path::CanonicalPath::resolve(dir.join(rel));

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    index_pack_language(php, &dir, None, &idx, None, 0);
    let config = idx.workspace_file_for_test(&dir.join("src/Config.php")).unwrap();
    assert!(config.used_identities().iter().any(|u| u == "Acme\\Str"), "a referenced-only class is demand: {:?}", config.used_identities());

    let map = idx.dependency_map().expect("the walk read php's map");
    let helpers = canon("vendor/acme/support/src/helpers.php");
    assert_eq!(map.declared(), [Ask::File(helpers.clone())]);
    let (_, stored) = idx.land_pack_file(&map, &helpers, php);
    assert!(stored.is_some(), "the files entry lands");
    assert_eq!(idx.file_role(&helpers), Some(FileRole::Dependency));
    for name in ["env", "Acme\\tap"] {
        assert_eq!(
            idx.def_candidates(name).iter().map(|m| m.path.clone()).collect::<Vec<_>>(),
            [helpers.clone()],
            "{name} answers from the files entry"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// An ask for a module MEMBER lands its module, which only imports it:
/// the acquisition then asks for the identity the member names.
#[cfg(feature = "python")]
#[test]
fn an_ask_for_a_module_member_asks_for_what_it_names() {
    use crate::build::dependency_map::Ask;
    let python = crate::build::language_driver::LanguageRegistry::compiled().for_id("python").expect("python driver");
    let dir = std::env::temp_dir().join(format!("d9_member_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    const SITE: &str = ".venv/lib/python3.12/site-packages";
    let files = [
        (".venv/pyvenv.cfg".to_string(), "home = /usr/bin\n"),
        (format!("{SITE}/acme/__init__.py"), "from .models import Base as Base\n"),
        (format!("{SITE}/acme/models.py"), "class Base:\n    pass\n"),
        ("app.py".to_string(), "import acme\n\nclass User(acme.Base):\n    pass\n"),
    ];
    for (rel, body) in &files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    let dir = crate::util::canonical_path::CanonicalPath::resolve(dir).into_path_buf();
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    index_pack_language(python, &dir, None, &idx, None, 0);
    let map = idx.dependency_map().expect("the walk read python's map");
    let located = map.locate(&Ask::Name("acme.Base".into()));
    assert_eq!(located.iter().filter_map(|l| l.loads_as.as_deref()).collect::<Vec<_>>(), ["acme"], "the member loads its module");
    let (_, stored) = idx.land_pack_module(&map, "acme", python);
    let stored = stored.expect("acme lands");
    assert_eq!(member_targets(&stored, "acme.Base"), [Ask::Name("acme.models.Base".into())]);
    assert!(member_targets(&stored, "acme.Other").is_empty(), "a name the module does not bind asks nothing");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Python's map is each environment's `site-packages`: the walk leaves the
/// environment out, an import the map places lands as a dependency, and a
/// module outside any package is named from its search root, not from the
/// workspace root it also lies under.
#[cfg(feature = "python")]
#[test]
fn a_site_packages_module_lands_named_from_its_search_root() {
    use crate::model::file_analysis::{CrossFileLookup, FileRole};
    let python = crate::build::language_driver::LanguageRegistry::compiled().for_id("python").expect("python driver");
    let dir = std::env::temp_dir().join(format!("d8_venv_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    const SITE: &str = ".venv/lib/python3.12/site-packages";
    let files = [
        (".venv/pyvenv.cfg".to_string(), "home = /usr/bin\n"),
        (format!("{SITE}/acme/__init__.py"), ""),
        (format!("{SITE}/acme/models.py"), "class Root:\n    pass\n\nclass Base(Root):\n    pass\n"),
        (format!("{SITE}/six.py"), "class Six:\n    pass\n"),
        ("app/__init__.py".to_string(), ""),
        ("app/user.py".to_string(), "from acme.models import Base\nimport six\n\nclass User(Base):\n    pass\n"),
    ];
    for (rel, body) in &files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    let dir = crate::util::canonical_path::CanonicalPath::resolve(dir).into_path_buf();
    let canon = |rel: &str| crate::util::canonical_path::CanonicalPath::resolve(dir.join(rel));

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    index_pack_language(python, &dir, None, &idx, None, 0);
    assert_eq!(idx.file_role(&canon("app/user.py")), Some(FileRole::Workspace));
    assert_eq!(idx.file_role(&canon(&format!("{SITE}/six.py"))), None, "the walk leaves the environment to the tier");

    let map = idx.dependency_map().expect("the walk read python's map");
    let module_of = |m: &str| {
        let (_, stored) = idx.land_pack_module(&map, m, python);
        let stored = stored.unwrap_or_else(|| panic!("{m} lands"));
        assert_eq!(idx.file_role(&stored[0].path), Some(FileRole::Dependency));
        stored[0].analysis.pack.file_module.as_ref().map(|f| f.segments.join("."))
    };
    assert_eq!(module_of("acme.models").as_deref(), Some("acme.models"));
    // the module's members are its candidates too, so an identity a project
    // names (`acme.models.Base`, a parent) is declared by something indexed
    let models = canon(&format!("{SITE}/acme/models.py"));
    for class in ["acme.models.Base", "acme.models.Root"] {
        assert_eq!(idx.def_candidates(class).iter().map(|m| m.path.clone()).collect::<Vec<_>>(), [models.clone()], "{class}");
        assert_eq!(idx.get_cached(class).map(|m| m.path.clone()), Some(models.clone()), "{class}");
    }
    assert_eq!(module_of("six").as_deref(), Some("six"), "named from site-packages, not the workspace root");
    assert_eq!(idx.get_cached("six").map(|m| m.path.clone()), Some(canon(&format!("{SITE}/six.py"))));
    assert!(idx.land_pack_module(&map, "app.user", python).1.is_none(), "the project's own module is the walk's");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A one-shot run against an unmoved Composer project takes the map it
/// kept: the generated classmap is not read again until an input's stamp
/// moves, and then it is read afresh and kept over the old row.
#[cfg(feature = "php")]
#[test]
fn a_kept_composer_map_is_served_until_an_input_moves() {
    use crate::build::dependency_map::{Ask, DependencyMap};
    use crate::index::module_index::{load_kept_map, read_and_keep};
    let php = crate::build::language_driver::LanguageRegistry::compiled().for_id("php").expect("php driver");
    let dir = std::env::temp_dir().join(format!("d7_kept_map_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let classmap = |class: &str| {
        format!("<?php\n$vendorDir = dirname(__DIR__);\nreturn array(\n    '{class}' => $vendorDir . '/old/legacy.php',\n);\n")
    };
    let files = [
        ("composer.json".to_string(), r#"{"autoload": {"psr-4": {"App\\": "src/"}}}"#.to_string()),
        (
            "vendor/composer/installed.json".to_string(),
            r#"{"packages": [{"name": "old/lib", "install-path": "../old", "autoload": {"classmap": ["."]}}]}"#.to_string(),
        ),
        ("vendor/composer/autoload_classmap.php".to_string(), classmap("Legacy_Widget")),
        ("vendor/old/legacy.php".to_string(), "<?php\nclass Legacy_Widget {}\n".to_string()),
    ];
    for (rel, body) in &files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    let dir = crate::util::canonical_path::CanonicalPath::resolve(&dir).into_path_buf();
    let cache = dir.join(".cache");
    let located = |map: &DependencyMap, class: &str| !map.locate(&Ask::Name(class.into())).is_empty();
    crate::index::module_cache::with_test_cache_dir(&cache, std::time::Duration::from_secs(10), || {
        let conn = crate::index::module_cache::open_cache_db(None, php.id()).expect("test store");
        let root = Some(dir.as_path());
        assert!(load_kept_map(php, root, Some(&conn)).is_none(), "nothing kept yet");
        let fresh = read_and_keep(php, root, Some(&conn)).unwrap();
        let kept = load_kept_map(php, root, Some(&conn)).expect("the read was kept");
        assert_eq!(kept.generation(), fresh.generation());
        assert_eq!(kept.generation(), php.dependency_map(root).unwrap().generation());

        // The classmap rewritten under the same size and mtime: its stamp
        // holds, so the kept map answers as it was read: not re-read.
        let cm = dir.join("vendor/composer/autoload_classmap.php");
        let installed = dir.join("vendor/composer/installed.json");
        let mtime = |p: &std::path::Path| std::fs::metadata(p).unwrap().modified().unwrap();
        let was = mtime(&cm);
        std::fs::write(&cm, classmap("Legacy_Gadget")).unwrap();
        std::fs::File::options().write(true).open(&cm).unwrap().set_modified(was).unwrap();
        let kept = load_kept_map(php, root, Some(&conn)).expect("its stamps hold");
        assert!(located(&kept, "Legacy_Widget") && !located(&kept, "Legacy_Gadget"));

        // Touching `installed.json` moves an input: the row is not served,
        // and the read that replaces it reads the classmap afresh.
        let later = mtime(&installed) + std::time::Duration::from_secs(5);
        std::fs::File::options().write(true).open(&installed).unwrap().set_modified(later).unwrap();
        assert!(load_kept_map(php, root, Some(&conn)).is_none(), "a moved input is not served");
        let reread = read_and_keep(php, root, Some(&conn)).unwrap();
        assert!(located(&reread, "Legacy_Gadget") && !located(&reread, "Legacy_Widget"));
        let kept = load_kept_map(php, root, Some(&conn)).expect("rewritten in place");
        assert_eq!(kept.generation(), reread.generation());
        assert!(located(&kept, "Legacy_Gadget"));
    });
    let _ = std::fs::remove_dir_all(&dir);
}
