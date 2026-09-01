use super::*;
use crate::index::module_index::strip_import_copy_one;

#[test]
fn test_resolve_module_list_util() {
    let inc_paths = discover_inc_paths();
    if inc_paths.is_empty() {
        return;
    }
    let path = resolve_module_path(&inc_paths, "List::Util");
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
    let cached = Arc::new(CachedModule::new(PathBuf::from("/x/Demo/Has/Event.pm"), analysis));

    // Workspace-index style insert: a resolved module.
    core.insert_resolved("Demo::Has::Event", Some(vec![cached]), false, false);
    assert!(core.cache.get("Demo::Has::Event").as_deref().unwrap().is_some());

    // On-demand resolver miss: `None`. Must NOT clobber the indexed copy.
    core.insert_resolved("Demo::Has::Event", None, false, false);
    assert!(
        core.cache.get("Demo::Has::Event").as_deref().unwrap().is_some(),
        "a None on-demand miss clobbered an already-indexed module",
    );

    // A genuine resolved entry still updates (sanity: the guard only
    // protects against None-over-Some).
    let tree2 = parser.parse(source, None).unwrap();
    let analysis2 = std::sync::Arc::new(crate::build::builder::build(&tree2, source.as_bytes()));
    let cached2 = Arc::new(CachedModule::new(PathBuf::from("/y/Demo/Has/Event.pm"), analysis2));
    core.insert_resolved("Demo::Has::Event", Some(vec![cached2]), false, false);
    assert_eq!(
        core.cache.get("Demo::Has::Event").as_deref().unwrap().as_ref().unwrap().path,
        PathBuf::from("/y/Demo/Has/Event.pm"),
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

    let files = crate::index::file_store::FileStore::new();
    let indexed =
        index_workspace_with_index(&dir, &files, None, Some(&cb as &(dyn Fn(usize, usize) + Sync)), None);
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
        PathBuf::from("/inc/Strip.pm"),
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


/// The priority lane is guarded by its OWN mutex while the drain waits on the
/// `pending` one, so a `request_resolve` for a stale module can land after the
/// drain's priority check and before it parks — the notify reaches nobody and
/// the wait loop, which only re-checked `pending`, never looks at priority
/// again. In an all-stale workload (an `EXTRACT_VERSION` bump: every
/// `request_resolve` takes the priority branch) nothing ever pushes `pending`,
/// so the resolver sleeps for the rest of the session and cross-file
/// resolution silently never completes.
#[test]
fn priority_push_wakes_a_parked_drain() {
    use std::sync::mpsc;
    use std::sync::{Condvar, Mutex};

    let queue = Arc::new(ResolveQueue {
        priority: Mutex::new(Vec::new()),
        pending: Mutex::new(Vec::new()),
        condvar: Condvar::new(),
    });
    let (tx, rx) = mpsc::channel();
    let q = Arc::clone(&queue);
    std::thread::spawn(move || {
        let _ = tx.send(drain_next_batch(&q));
    });

    // Let the drain get past its priority check and park in `wait(pending)`.
    // Generous: the drain reaches the park in microseconds, so an early push
    // (which the first priority check would catch, hiding the bug) is not a
    // realistic outcome here.
    std::thread::sleep(std::time::Duration::from_millis(300));

    // Exactly what `ModuleIndex::request_resolve` does for a stale module.
    {
        let mut p = queue.priority.lock().unwrap();
        p.push("Stale::Module".to_string());
    }
    queue.condvar.notify_one();

    let batch = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("a priority push must wake the parked drain");
    assert_eq!(batch, vec!["Stale::Module".to_string()]);
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
    let found = resolve_module_paths(&[a.clone(), b.clone()], "Twin");
    assert_eq!(
        found,
        vec![a.join("Twin.pm"), b.join("Twin.pm")],
        "both roots provide Twin; the relation must hold both, @INC order first",
    );
    // The winner is still exactly what `require` would load.
    assert_eq!(
        resolve_module_path(&[a.clone(), b.clone()], "Twin"),
        Some(a.join("Twin.pm")),
    );
    // Reversing @INC reverses the winner, not the membership.
    assert_eq!(
        resolve_module_path(&[b.clone(), a.clone()], "Twin"),
        Some(b.join("Twin.pm")),
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
    let mut memo: ParseMemo = HashMap::new();
    let providers =
        resolve_and_parse_with_memo(&[a.clone(), b.clone()], "Twin", &mut parser, &mut memo)
            .expect("Twin resolves");
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
    assert_eq!(owner.path, b.join("Twin.pm"));
    let owner_a = idx
        .candidate_defining_sub("Twin", "only_in_a")
        .expect("only_in_a is defined by the winning provider");
    assert_eq!(owner_a.path, a.join("Twin.pm"));
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

    idx.insert_cache("Shadowed", Some(Arc::new(CachedModule::new(inc_pm.clone(), inc_fa))));
    idx.register_workspace_module(ws_pm.clone(), ws_fa);

    assert_eq!(
        idx.get_cached("Shadowed").map(|c| c.path.clone()),
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
    use crate::model::file_analysis::{CrossFileLookup, ScopedLookup, VisibilityAxis};
    let base = std::env::temp_dir().join(format!("perl-lsp-asker-{}", std::process::id()));
    let (applib, testlib) = (base.join("lib"), base.join("t").join("lib"));
    std::fs::create_dir_all(&applib).unwrap();
    std::fs::create_dir_all(&testlib).unwrap();
    std::fs::write(applib.join("Twin.pm"), "package Twin;\nsub which { 'app' }\n1;\n").unwrap();
    std::fs::write(testlib.join("Twin.pm"), "package Twin;\nsub which { 'test' }\n1;\n").unwrap();

    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let mut parser = create_parser();
    let mut memo: ParseMemo = HashMap::new();
    // The process @INC has only the app lib: `t/lib` is a root ONLY the
    // test file declares, which is exactly the discriminator.
    idx.set_inc_roots_for_test(&[applib.clone()]);
    let providers = resolve_and_parse_with_memo(
        &[applib.clone(), testlib.clone()],
        "Twin",
        &mut parser,
        &mut memo,
    )
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

    let empty = Default::default();
    let resolved_by = |fa: &crate::model::file_analysis::FileAnalysis| {
        let axis = VisibilityAxis::for_origin(fa, None, &idx, crate::model::file_analysis::PackVisibility::Host);
        let scoped = ScopedLookup::new(&idx, &empty, None, axis);
        scoped.get_cached("Twin").map(|c| c.path.clone())
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
    run_persist_writer(rx, None, "test", |_c, _b: &[usize]| {}, |_e| { c.fetch_add(1, O::SeqCst); }, |_e| {});
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
    let files = crate::index::file_store::FileStore::new();
    index_workspace_with_index(&dir, &files, Some(&idx), None, None);

    let canon_cons = std::fs::canonicalize(&consumer).unwrap();
    let before = idx.flush_epoch();

    // The provider's surface moves: a different return class.
    std::fs::write(
        &provider,
        "package BulkProv;\nsub build { return bless {}, 'Widget::Two' }\n1;\n",
    )
    .unwrap();
    index_workspace_with_index(&dir, &files, Some(&idx), None, None);

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

/// The Flat axis contract (name-keyed packs): scope-less BY RULE — no
/// `visibility_scope` (so `pack_def_paths` mints no closure gate and the
/// backward walk stays unfiltered), `flat_scope()` set (so the cross-file
/// return arm admits the full candidate table), full `visible_def_candidates`.
/// Transparent keeps the host's pre-existing answers: scope present,
/// `flat_scope` false — an unwarmed Perl origin never sweeps pack tables.
#[test]
fn flat_axis_is_scopeless_by_rule_transparent_is_not() {
    use crate::model::file_analysis::{CrossFileLookup, ScopedLookup, VisibilityAxis};
    let idx = crate::index::module_index::ModuleIndex::new_for_test();
    let empty = Default::default();
    let self_path = std::path::PathBuf::from("/w/a.php");

    let flat = ScopedLookup::new(&idx, &empty, Some(self_path.as_path()), VisibilityAxis::Flat);
    assert!(flat.visibility_scope().is_none(), "Flat mints no def_paths gate");
    assert!(flat.flat_scope());

    let transparent =
        ScopedLookup::new(&idx, &empty, Some(self_path.as_path()), VisibilityAxis::Transparent);
    assert!(transparent.visibility_scope().is_some(), "host behavior unchanged");
    assert!(!transparent.flat_scope());

    let closure =
        ScopedLookup::new(&idx, &empty, Some(self_path.as_path()), VisibilityAxis::IncludeClosure);
    assert!(closure.visibility_scope().is_some());
    assert!(!closure.flat_scope());

    // A use-map axis is Flat's scope-less contract PLUS the pins: the
    // imported leaf answers its `use` row, an unpinned leaf the origin's
    // own namespace, and every other axis makes no claim at all.
    let pins = crate::model::file_analysis::UseMapPins {
        pins: [
            ("Collection".to_string(), Some("B".to_string())),
            ("Factory".to_string(), None),
        ]
        .into_iter()
        .collect(),
        own_namespace: Some("App".to_string()),
        spelled: ["Request".to_string()].into_iter().collect(),
    };
    let usemap = ScopedLookup::new(
        &idx,
        &empty,
        Some(self_path.as_path()),
        VisibilityAxis::UseMap(std::sync::Arc::new(pins)),
    );
    assert!(usemap.visibility_scope().is_none(), "UseMap mints no def_paths gate");
    assert!(usemap.flat_scope());
    assert_eq!(usemap.pinned_namespace("Collection").as_deref(), Some("B"));
    assert_eq!(usemap.pinned_namespace("Request").as_deref(), Some("App"), "spelled leaf: own namespace");
    assert!(usemap.pinned_namespace("Factory").is_none(), "conflicting evidence: no claim");
    assert!(usemap.pinned_namespace("Helper").is_none(), "never spelled: no claim");
    assert!(flat.pinned_namespace("Collection").is_none());
    assert!(closure.pinned_namespace("Collection").is_none());
}
