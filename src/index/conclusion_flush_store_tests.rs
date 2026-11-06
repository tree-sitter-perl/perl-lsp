//! The flush over a WORLD: the overlay, the seed rule, and the atomic publish.
//!
//! `conclusion_flush_tests` covers the loop's own properties (cutoff,
//! termination, fan-in). What is tested here is everything the loop needs a
//! world for — which maps a round evaluates against, and what a converged
//! round writes.

use super::*;
use crate::model::file_analysis::InferredType;
use crate::model::witnesses::{Conclusion, ConclusionKey, ReceiverRule};
use std::collections::{BTreeMap, HashMap};

fn key(class: &str, name: &str) -> ConclusionKey {
    ConclusionKey::returns_of(class, name)
}

/// A map holding one key, with `class` enumerated so absence elsewhere in it
/// reads as `NotLocal` rather than a proven `None`.
fn map_of(class: &str, name: &str, c: Conclusion) -> ConclusionMap {
    let mut m = HashMap::new();
    m.insert(key(class, name), c);
    let mut enumerated = HashSet::new();
    enumerated.insert(class.to_string());
    ConclusionMap(m, HashSet::new(), enumerated, HashMap::new())
}

/// One flush seed. Spelled here so the publication tests and the propagation
/// test agree on how a seed is built.
fn seed(path: &PathBuf, map: ConclusionMap, fingerprint: u64) -> FreshBake {
    FreshBake { path: path.clone(), map, source_fingerprint: fingerprint }
}

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

/// The hop that only an overlay can make.
///
/// B's map holds a `Link` into A's class, so B's bytes say the same thing
/// before and after A changes — that is what makes the map useless as a change
/// signal. B's ANSWER moves only if the resolver B is evaluated against sees
/// A's fresh bake. Frozen-only resolution reproduces B's old surface exactly,
/// cuts, and starves C.
///
/// Base-verify by flipping `resolve`'s `overlay` argument to `false` in
/// `evaluate`: B then cuts, `evaluated` drops to 2, and C is never reached —
/// with every other assertion in the suite still passing.
#[test]
fn a_consumer_is_evaluated_against_the_seeds_fresh_bake_not_the_frozen_one() {
    let a_old = map_of("A", "m", Conclusion::Value(InferredType::ClassName("Old".into())));
    let a_new = map_of("A", "m", Conclusion::Value(InferredType::ClassName("New".into())));
    // B answers by walking to A. Byte-identical in both worlds, deliberately.
    let b = map_of(
        "B",
        "m",
        Conclusion::Link {
            targets: vec![key("A", "m")],
            arity: None,
            receiver: ReceiverRule::Thread,
        },
    );
    let c = map_of("C", "m", Conclusion::Value(InferredType::String));

    let frozen_src = |path: &Path| match path.to_string_lossy().as_ref() {
        "/A.pm" => Some(a_old.clone()),
        "/B.pm" => Some(b.clone()),
        "/C.pm" => Some(c.clone()),
        _ => None,
    };
    let re_bake = |path: &Path| match path.to_string_lossy().as_ref() {
        "/A.pm" => Some(a_new.clone()),
        "/B.pm" => Some(b.clone()),
        "/C.pm" => Some(c.clone()),
        _ => None,
    };
    let candidates_of = |class: &str| match class {
        "A" => vec![p("/A.pm")],
        "B" => vec![p("/B.pm")],
        "C" => vec![p("/C.pm")],
        _ => Vec::new(),
    };
    let consumers_of = |path: &Path| match path.to_string_lossy().as_ref() {
        "/A.pm" => vec![p("/B.pm")],
        "/B.pm" => vec![p("/C.pm")],
        _ => Vec::new(),
    };

    // Precondition, asserted rather than assumed: B's map really is unchanged.
    // Without this the test could pass for the wrong reason — B moving because
    // B's own bytes moved proves nothing about the overlay.
    assert_eq!(
        re_bake(Path::new("/B.pm")),
        frozen_src(Path::new("/B.pm")),
        "precondition: B's map is byte-identical across the change"
    );

    let world = FlushWorld::new(&frozen_src, &re_bake, &candidates_of, vec![p("/A.pm")]);
    let outcome = flush_over_world(&world, vec![p("/A.pm")], &consumers_of);

    assert!(!outcome.non_convergent);
    let moved: Vec<&PathBuf> = outcome.changed.iter().map(|(q, _)| q).collect();
    assert_eq!(
        moved,
        vec![&p("/A.pm"), &p("/B.pm")],
        "A moved, and B moved THROUGH it — B's own bytes never changed"
    );
    assert_eq!(outcome.evaluated, 3, "the wave reached C, which then cut");

}

/// A seed whose surface did not move stops the wave dead — its consumers are
/// not even enumerated.
///
/// The reverse-dep walk is part of what the cutoff saves, not just the
/// evaluation. A driver that computed the consumer set first and then asked
/// whether it needed it would have paid the walk on every no-op save.
#[test]
fn a_seed_that_did_not_move_never_enumerates_its_consumers() {
    let same = map_of("A", "m", Conclusion::Value(InferredType::String));
    let frozen_src = |_: &Path| Some(same.clone());
    let re_bake = |_: &Path| Some(same.clone());
    let candidates_of = |_: &str| vec![p("/A.pm")];
    let reached = std::cell::RefCell::new(Vec::<PathBuf>::new());
    let consumers_of = |path: &Path| {
        reached.borrow_mut().push(path.to_path_buf());
        vec![p("/B.pm")]
    };

    let world = FlushWorld::new(&frozen_src, &re_bake, &candidates_of, vec![p("/A.pm")]);
    let outcome = flush_over_world(&world, vec![p("/A.pm")], &consumers_of);

    assert!(outcome.changed.is_empty(), "the surface did not move");
    assert_eq!(outcome.evaluated, 1);
    assert!(reached.borrow().is_empty());
}

/// A file with no map at all is not a change — it is an absence of evidence.
///
/// Inventing a move for it would propagate noise from a file we cannot read,
/// and the wave's whole value is that it stops where the answers stop moving.
/// A DELETED file is handled at the caller instead, by entering the wave as
/// its direct consumers: they resolve through a file the store has forgotten,
/// which is a real move with real evidence behind it.
#[test]
fn a_file_with_no_map_cuts_rather_than_inventing_a_move() {
    let frozen_src = |_: &Path| None;
    let re_bake = |_: &Path| None;
    let candidates_of = |_: &str| Vec::new();
    let consumers_of = |_: &Path| vec![p("/B.pm")];

    let world = FlushWorld::new(&frozen_src, &re_bake, &candidates_of, vec![p("/A.pm")]);
    let outcome = flush_over_world(&world, vec![p("/A.pm")], &consumers_of);
    assert_eq!(outcome.evaluated, 1);
    assert!(outcome.changed.is_empty());
}

/// The propagation decodes maps, never blobs.
///
/// A map goes stale only when its own file's blob changes — the bake runs with
/// the index withheld, so a cross-file answer is a `Link` chased at read time
/// rather than a value baked in. Re-baking past the frontier would therefore
/// pay a blob decode and a bake per reached file to reproduce, byte for byte,
/// the map already in the store.
///
/// `PERL_LSP_FLUSH_EQUIV` is the switch that checks that assumption on a real
/// corpus; this is the switch's default side.
#[test]
fn the_propagation_never_re_bakes_past_the_frontier() {
    let bakes = std::cell::RefCell::new(BTreeMap::<String, usize>::new());
    let seed = map_of("A", "m", Conclusion::Value(InferredType::ClassName("New".into())));
    let old = map_of("A", "m", Conclusion::Value(InferredType::ClassName("Old".into())));
    let sink = map_of("Z", "m", Conclusion::Value(InferredType::String));

    // Only A has a frozen map. Everything downstream is new to the store, so
    // its first visit moves and its second cuts — which drives the wave all
    // the way to Z without contriving a value that oscillates.
    let frozen_src = |path: &Path| match path.to_string_lossy().as_ref() {
        "/A.pm" => Some(old.clone()),
        _ => Some(sink.clone()),
    };
    let re_bake = |path: &Path| {
        *bakes
            .borrow_mut()
            .entry(path.to_string_lossy().into_owned())
            .or_default() += 1;
        match path.to_string_lossy().as_ref() {
            "/A.pm" => Some(seed.clone()),
            _ => Some(sink.clone()),
        }
    };
    let candidates_of = |_: &str| Vec::new();
    let consumers_of = |path: &Path| match path.to_string_lossy().as_ref() {
        "/A.pm" => vec![p("/B.pm"), p("/C.pm")],
        "/B.pm" => vec![p("/Z.pm")],
        "/C.pm" => vec![p("/D.pm")],
        _ => Vec::new(),
    };

    let world = FlushWorld::new(&frozen_src, &re_bake, &candidates_of, vec![p("/A.pm")]);
    let outcome = flush_over_world(&world, vec![p("/A.pm")], &consumers_of);
    assert!(!outcome.non_convergent);
    assert!(outcome.evaluated > 1, "precondition: the wave did reach past A");
    assert_eq!(
        bakes.borrow().keys().cloned().collect::<Vec<_>>(),
        vec!["/A.pm".to_string()],
        "only the seed is re-baked; every other reached file is read from the \
         store as it stands"
    );
}

/// A seed revisited by a cycle is evaluated twice and baked once.
///
/// The bake is a pure function of the file's own blob, so nothing about it
/// depends on the round. Only the evaluation does.
#[test]
fn a_seed_revisited_by_a_cycle_is_baked_once() {
    let bakes = std::cell::Cell::new(0usize);
    let old = map_of("A", "m", Conclusion::Value(InferredType::ClassName("Old".into())));
    let new = map_of("A", "m", Conclusion::Value(InferredType::ClassName("New".into())));
    // B answers by walking into A, so A's fresh bake moves B and B re-enqueues
    // A — a cycle whose second visit to A has something real to re-evaluate.
    let b = map_of(
        "B",
        "m",
        Conclusion::Link {
            targets: vec![key("A", "m")],
            arity: None,
            receiver: ReceiverRule::Thread,
        },
    );

    let frozen_src = |path: &Path| match path.to_string_lossy().as_ref() {
        "/A.pm" => Some(old.clone()),
        _ => Some(b.clone()),
    };
    let re_bake = |path: &Path| {
        assert_eq!(path, Path::new("/A.pm"), "only the seed is ever re-baked");
        bakes.set(bakes.get() + 1);
        Some(new.clone())
    };
    let candidates_of = |class: &str| match class {
        "A" => vec![p("/A.pm")],
        _ => Vec::new(),
    };
    let consumers_of = |path: &Path| match path.to_string_lossy().as_ref() {
        "/A.pm" => vec![p("/B.pm")],
        "/B.pm" => vec![p("/A.pm")],
        _ => Vec::new(),
    };

    let world = FlushWorld::new(&frozen_src, &re_bake, &candidates_of, vec![p("/A.pm")]);
    let outcome = flush_over_world(&world, vec![p("/A.pm")], &consumers_of);
    assert!(
        !outcome.non_convergent,
        "the cycle terminates by cutting, not by the round cap"
    );
    assert_eq!(outcome.evaluated, 3, "A, B, then A again — which cuts");
    assert_eq!(bakes.get(), 1, "two evaluations of A, one bake");
}

// ---- against a real store ----

fn store_db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::index::module_cache::init_schema(&conn).unwrap();
    conn
}

/// `flush_refresh_set` end to end, over a real store: the caller's fresh map
/// beats the stored one, and the wave reaches the consumer that answers
/// through it.
///
/// Also pins the thing that made this entry point worth reshaping: it reads
/// the caller's map for the seed and the store's map for everyone else, and
/// decodes no blob at either. At the seam it is wired to, the seed's blob has
/// already been invalidated, so a version that re-derived the seed's map from
/// the store would quietly find nothing and cut on its own frontier.
#[test]
fn flush_refresh_set_moves_a_consumer_over_a_real_store() {
    use crate::index::module_cache::{publish_generation, Generation};
    let conn = store_db();
    let a = p("/store/A.pm");
    let b = p("/store/B.pm");

    let a_old = map_of("A", "m", Conclusion::Value(InferredType::ClassName("Old".into())));
    let a_new = map_of("A", "m", Conclusion::Value(InferredType::ClassName("New".into())));
    let b_map = map_of(
        "B",
        "m",
        Conclusion::Link {
            targets: vec![key("A", "m")],
            arity: None,
            receiver: ReceiverRule::Thread,
        },
    );
    publish_generation(
        &conn,
        Generation(1),
        &[
            (a.to_string_lossy().into_owned(), a_old, 1),
            (b.to_string_lossy().into_owned(), b_map, 2),
        ],
    )
    .expect("baseline");

    let candidates_of = |class: &str| match class {
        "A" => vec![a.clone()],
        "B" => vec![b.clone()],
        _ => Vec::new(),
    };
    let consumers_of = |path: &Path| {
        if path == a.as_path() { vec![b.clone()] } else { Vec::new() }
    };

    let out = flush_refresh_set(
        &conn,
        vec![seed(&a, a_new, 11)],
        vec![a.clone()],
        &consumers_of,
        &candidates_of,
    );

    let moved: Vec<PathBuf> = out.changed.into_iter().map(|(q, _)| q).collect();
    assert_eq!(
        moved,
        vec![a.clone(), b.clone()],
        "A moved, and B moved through it — B's stored map never changed"
    );
}

/// Publication, end to end: the seed's fresh map is in the store afterwards,
/// at the next generation, carrying the fingerprint the caller supplied.
///
/// This is what makes a consult on a just-edited file cheap. Without it the
/// flush computes the right answer and throws it away — the edited file's row
/// still holds the pre-edit bake, so every consult against it either serves
/// the old map or (once the stamp rejects it) decodes, forever.
#[test]
fn a_flush_publishes_its_seeds_at_the_next_generation() {
    use crate::index::module_cache::{load_conclusions_stamped, publish_generation, Generation};
    let conn = store_db();
    let a = p("/pub/A.pm");
    let old_map = map_of("A", "m", Conclusion::Value(InferredType::ClassName("Old".into())));
    let new_map = map_of("A", "m", Conclusion::Value(InferredType::ClassName("New".into())));
    publish_generation(&conn, Generation(1), &[(a.to_string_lossy().into_owned(), old_map, 1)])
        .expect("baseline");

    let none = |_: &str| Vec::new();
    let no_consumers = |_: &Path| Vec::new();
    let out = flush_refresh_set(
        &conn,
        vec![seed(&a, new_map.clone(), 42)],
        vec![a.clone()],
        &no_consumers,
        &none,
    );

    assert_eq!(out.published, Some(Generation(2)), "the seed round must land");
    let (stored, stamp) =
        load_conclusions_stamped(&conn, &a.to_string_lossy(), Generation(2)).expect("published row");
    assert_eq!(stored, new_map, "the published row holds the FRESH map");
    assert_eq!(
        stamp.source_fingerprint, 42,
        "the caller's fingerprint must ride through untouched — a stamp \
         gathered anywhere else can describe a different state than the map"
    );
    assert_eq!(stamp.flush_generation, Generation(2));
}

/// A wave that never settled publishes nothing.
///
/// Its refresh set is a half-finished propagation. A consult pinned to that
/// generation would compose answers from a wave that did not converge, which
/// is strictly worse than the decode it would otherwise have paid.
///
/// Driven at `publish_seeds` rather than through `flush_refresh_set`: a
/// store-shaped world cannot be made to diverge on demand — `MAX_FOLLOW_HOPS`
/// truncates a long chain, and the flush's own cutoff terminates a cycle,
/// which is exactly what those are for. Fabricating divergence by defeating
/// them would test the fabrication. `run_flush`'s own cap is covered by
/// `conclusion_flush_tests::a_non_convergent_flush_reports_no_refresh_set`;
/// this covers what publication does when it is told so.
#[test]
fn a_non_convergent_flush_publishes_nothing() {
    use crate::index::module_cache::{current_generation, publish_generation, Generation};
    let conn = store_db();
    let a = p("/nonconv/A.pm");
    let m = |v: &str| map_of("A", "m", Conclusion::Value(InferredType::ClassName(v.into())));
    publish_generation(&conn, Generation(1), &[(a.to_string_lossy().into_owned(), m("Old"), 1)])
        .expect("baseline");

    let supplied: HashMap<PathBuf, ConclusionMap> = [(a.clone(), m("New"))].into_iter().collect();
    let fingerprints: HashMap<PathBuf, u64> = [(a.clone(), 42)].into_iter().collect();

    let abandoned = FlushOutcome { non_convergent: true, ..Default::default() };
    assert_eq!(
        publish_seeds(&conn, Generation(1), &abandoned, &supplied, &fingerprints),
        None,
        "a wave that never settled must not publish"
    );
    assert_eq!(
        current_generation(&conn),
        Generation(1),
        "the generation must not advance past a wave that was abandoned"
    );

    // The control: identical inputs, convergent outcome. Without it the
    // assertion above would hold against a publish that never works.
    let settled = FlushOutcome::default();
    assert_eq!(
        publish_seeds(&conn, Generation(1), &settled, &supplied, &fingerprints),
        Some(Generation(2))
    );
}

/// Publishing reclaims what it supersedes.
///
/// Retention existed so a reader pinned to a generation kept finding it. With
/// per-row content fingerprints there is nothing to pin: a reader that loses
/// an older row either finds the newer one — same fingerprint compare, and the
/// bake is deterministic, so the same content — or decodes. So the table must
/// not grow a row per file per edit for the life of a session.
#[test]
fn publishing_reclaims_superseded_rows() {
    use crate::index::module_cache::{publish_generation, Generation};
    let conn = store_db();
    let a = p("/prune/A.pm");
    let m = |s: &str| map_of("A", "m", Conclusion::Value(InferredType::ClassName(s.into())));
    publish_generation(&conn, Generation(1), &[(a.to_string_lossy().into_owned(), m("v1"), 1)])
        .expect("baseline");
    let none = |_: &str| Vec::new();
    let no_consumers = |_: &Path| Vec::new();
    for (i, v) in ["v2", "v3", "v4"].iter().enumerate() {
        let out = flush_refresh_set(
            &conn,
            vec![seed(&a, m(v), 100 + i as u64)],
            vec![a.clone()],
            &no_consumers,
            &none,
        );
        assert_eq!(out.published, Some(Generation(2 + i as i64)));
    }
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM conclusions WHERE path = ?1", [a.to_string_lossy()], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        rows, 1,
        "four publications left {rows} rows for one file — the table grows \
         per edit for the life of the session"
    );
}
