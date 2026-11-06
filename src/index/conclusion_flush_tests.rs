use super::*;
use crate::model::file_analysis::InferredType;
use crate::model::witnesses::{ConclusionKey, EvaluatedAnswer, EvaluatedSurface};

fn surface(v: &[(&str, &str)]) -> EvaluatedSurface {
    EvaluatedSurface(
        v.iter()
            .map(|(name, ty)| {
                (
                    ConclusionKey::returns_of("K", *name),
                    EvaluatedAnswer::Answer(InferredType::ClassName((*ty).into())),
                )
            })
            .collect(),
    )
}

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

/// An unchanged surface CUTS: its consumers are not enqueued.
///
/// This is the entire point of the driver. Without the cut a single edit walks
/// the whole reverse-dependency closure, which is the "re-enrich everything"
/// behaviour the flush exists to replace — and it would still look correct,
/// just slow, which is why it needs a test rather than an eyeball.
#[test]
fn an_unchanged_surface_cuts_the_chain() {
    let moved = surface(&[("m", "After")]);
    let same = surface(&[("m", "Same")]);
    let evaluate = |path: &Path| -> Option<EvaluatedSurface> {
        match path.to_str().unwrap() {
            "/a.pm" => Some(moved.clone()),
            _ => Some(same.clone()),
        }
    };
    // B's baseline already equals what it evaluates to: A moved, but B's
    // answers did not follow.
    let baseline = |path: &Path| -> Option<EvaluatedSurface> {
        match path.to_str().unwrap() {
            "/a.pm" => Some(surface(&[("m", "Before")])),
            _ => Some(same.clone()),
        }
    };
    let consumers = |path: &Path| -> Vec<PathBuf> {
        match path.to_str().unwrap() {
            "/a.pm" => vec![p("/b.pm")],
            "/b.pm" => vec![p("/c.pm")],
            _ => vec![],
        }
    };
    let out = run_flush([p("/a.pm")], &evaluate, &baseline, &consumers);
    assert_eq!(
        out.changed.iter().map(|(x, _)| x.clone()).collect::<Vec<_>>(),
        vec![p("/a.pm")],
        "only A moved"
    );
    assert_eq!(
        out.evaluated, 2,
        "A and B are evaluated; C is never reached, because B cut"
    );
}

/// A change that keeps moving keeps propagating — the other half of the cut.
///
/// Paired with the test above deliberately: a driver that cut everything would
/// pass that one and fail this, and a driver that cut nothing would pass this
/// and fail that. Neither test is meaningful alone.
#[test]
fn a_moving_surface_propagates_to_the_end_of_the_chain() {
    let evaluate = |_: &Path| -> Option<EvaluatedSurface> { Some(surface(&[("m", "After")])) };
    let baseline = |_: &Path| -> Option<EvaluatedSurface> { Some(surface(&[("m", "Before")])) };
    let consumers = |path: &Path| -> Vec<PathBuf> {
        match path.to_str().unwrap() {
            "/a.pm" => vec![p("/b.pm")],
            "/b.pm" => vec![p("/c.pm")],
            _ => vec![],
        }
    };
    let out = run_flush([p("/a.pm")], &evaluate, &baseline, &consumers);
    assert_eq!(out.evaluated, 3, "the wave reaches C");
    assert_eq!(out.changed.len(), 3);
    assert!(!out.non_convergent);
}

/// A dependency CYCLE terminates, and terminates by cutting rather than by
/// hitting the cap.
///
/// This is what the compare-against-this-flush's-record rule buys. Compared to
/// the pre-flush baseline every time, A's second visit would look like a change
/// again — A's surface differs from its baseline, permanently — and the pair
/// would ping-pong until `MAX_FLUSH_ROUNDS`. The cap would contain it, so the
/// bug would present as "flushes are slow", not as a hang.
///
/// Base-verify by comparing against `baseline` only: `rounds` runs to the cap
/// and `non_convergent` is set.
#[test]
fn a_dependency_cycle_terminates_by_cutting_not_by_the_cap() {
    let evaluate = |_: &Path| -> Option<EvaluatedSurface> { Some(surface(&[("m", "After")])) };
    let baseline = |_: &Path| -> Option<EvaluatedSurface> { Some(surface(&[("m", "Before")])) };
    let consumers = |path: &Path| -> Vec<PathBuf> {
        match path.to_str().unwrap() {
            "/a.pm" => vec![p("/b.pm")],
            "/b.pm" => vec![p("/a.pm")],
            _ => vec![],
        }
    };
    let out = run_flush([p("/a.pm")], &evaluate, &baseline, &consumers);
    assert!(
        !out.non_convergent,
        "a cycle must terminate by cutting, not by exhausting the round cap"
    );
    assert!(
        out.rounds < MAX_FLUSH_ROUNDS,
        "took {} rounds — a two-file cycle should cut on the second visit",
        out.rounds
    );
    assert_eq!(out.changed.len(), 2, "both files moved, once each");
}

/// A surface that never settles is ABANDONED, and reports an EMPTY refresh
/// set rather than the half of one it managed to compute.
///
/// A partial refresh set is worse than none: a caller acting on it refreshes
/// some consumers and leaves the rest silently stale, with nothing in the
/// result saying which is which. An empty set with `non_convergent` set is at
/// least legible — the caller can fall back to its own coarser rule, which is
/// exactly what the one-hop `dirty_consumers` walk still provides.
#[test]
fn a_non_convergent_flush_reports_no_refresh_set() {
    // Per-PATH, not per-call. A global flip alternates with the visit order and
    // can line up so each file sees the same value twice running — which cuts,
    // and the fixture then passes while testing nothing. My first version did
    // exactly that.
    let visits: std::cell::RefCell<std::collections::HashMap<PathBuf, usize>> =
        Default::default();
    let evaluate = |path: &Path| -> Option<EvaluatedSurface> {
        let mut v = visits.borrow_mut();
        let n = v.entry(path.to_path_buf()).or_insert(0);
        *n += 1;
        Some(surface(&[("m", if *n % 2 == 1 { "Odd" } else { "Even" })]))
    };
    let baseline = |_: &Path| -> Option<EvaluatedSurface> { None };
    let consumers = |path: &Path| -> Vec<PathBuf> {
        match path.to_str().unwrap() {
            "/a.pm" => vec![p("/b.pm")],
            _ => vec![p("/a.pm")],
        }
    };
    let out = run_flush([p("/a.pm")], &evaluate, &baseline, &consumers);
    assert!(out.non_convergent, "an oscillating surface must be detected");
    assert!(
        out.changed.is_empty(),
        "an abandoned flush must publish NOTHING — a half-propagated generation \
         is a wrong answer, where a stale one is only a cost"
    );
}

/// One re-bake per file per round, however many consumers reach it.
///
/// A wide fan-in — one utility module imported by two hundred files — would
/// otherwise multiply a round's cost by its width for no extra information,
/// and the answer would be identical every time.
#[test]
fn a_fan_in_evaluates_each_file_once_per_round() {
    let calls = std::cell::Cell::new(0usize);
    let evaluate = |_: &Path| -> Option<EvaluatedSurface> {
        calls.set(calls.get() + 1);
        Some(surface(&[("m", "After")]))
    };
    let baseline = |_: &Path| -> Option<EvaluatedSurface> { Some(surface(&[("m", "Before")])) };
    // Three roots all consumed by one file.
    let consumers = |path: &Path| -> Vec<PathBuf> {
        match path.to_str().unwrap() {
            "/hub.pm" => vec![],
            _ => vec![p("/hub.pm")],
        }
    };
    let out = run_flush(
        [p("/a.pm"), p("/b.pm"), p("/c.pm")],
        &evaluate,
        &baseline,
        &consumers,
    );
    assert_eq!(
        calls.get(),
        4,
        "three roots plus ONE evaluation of the hub they share, not three"
    );
    assert_eq!(out.changed.len(), 4);
}

/// The enqueued set includes consumers the wave then CUT.
///
/// It is not `changed`, and the difference is the whole point of the re-stamp
/// gate it feeds. A consumer whose own conclusion answers did not move can
/// still DISPATCH differently now that its provider changed, because a
/// dispatch target resolves through the index rather than off a surface.
/// Marking only what moved would skip exactly those — silently, since a frozen
/// `MethodTarget` that is never re-derived just keeps answering.
#[test]
fn the_enqueued_set_covers_consumers_that_cut() {
    let base = surface(&[("m", "Old")]);
    let moved = surface(&[("m", "New")]);
    let evaluate = |p: &Path| {
        Some(if p == Path::new("/A.pm") { moved.clone() } else { base.clone() })
    };
    let baseline = |_: &Path| Some(base.clone());
    let consumers_of = |p: &Path| {
        if p == Path::new("/A.pm") { vec![PathBuf::from("/B.pm")] } else { Vec::new() }
    };

    let out = run_flush(vec![PathBuf::from("/A.pm")], &evaluate, &baseline, &consumers_of);

    assert_eq!(
        out.changed.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
        vec![PathBuf::from("/A.pm")],
        "B's surface matched its baseline, so B did not move"
    );
    assert_eq!(
        out.enqueued,
        vec![PathBuf::from("/B.pm")],
        "B was still enqueued because its provider moved — that is what the \
         re-stamp gate needs to know, and `changed` does not say it"
    );
}
