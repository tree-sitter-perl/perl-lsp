//! Tests for the conclusion layer.
//!
//! Split out because `layering_tests` forbids the Model layer importing Build,
//! and these must run a real builder to get a real bag. Test suites are exempt
//! by living in a `*_tests.rs` file, which is the convention here.

use super::*;
use crate::model::file_analysis::FileAnalysis;

fn analyze(src: &str) -> FileAnalysis {
    let mut parser = crate::build::builder::create_parser();
    let tree = parser.parse(src, None).expect("parse");
    crate::build::builder::build(&tree, src.as_bytes())
}

/// The property the whole layer rests on: a baked answer must equal the
/// answer the live chase gives for the same key and binders.
///
/// Anything else is the failure mode this design is arranged against — a
/// stored answer that is well-formed, validates, and disagrees with the
/// derivation it claims to summarize. Checked over every key the bake
/// produced rather than a chosen few, because the interesting cases are
/// the ones nobody thought to pick.
#[test]
fn a_baked_conclusion_agrees_with_the_live_chase() {
    let sources: &[(&str, &str)] = &[
        ("constructor return", "package C;\nsub build { return LWP::UserAgent->new(timeout => 10) }\n1;\n"),
        ("moo accessors", "package M;\nuse Moo;\nhas name => (is => 'rw');\nhas size => (is => 'ro');\n1;\n"),
        ("literal returns", "package L;\nsub s { return 'x' }\nsub n { return 1 }\n1;\n"),
        ("inheritance", "package P;\nsub mk { my $c = shift; return bless {}, $c }\npackage C2;\nour @ISA = ('P');\n1;\n"),
        ("branch arms", "package B;\nsub pick { my $c = shift; if ($c) { return 'a' } else { return 'b' } }\n1;\n"),
    ];

    let registry = ReducerRegistry::with_defaults();
    let mut checked = 0usize;
    for (label, src) in sources {
        let fa = analyze(src);
        let map = bake(&fa.witnesses, &registry, &fa.packages.keys().cloned().collect());

        for att in fa.witnesses.attachments() {
            let Some(key) = ConclusionKey::from_attachment(att) else {
                continue;
            };
            let live = registry.query(
                &fa.witnesses,
                &ReducerQuery {
                    attachment: att,
                    point: None,
                    framework: FrameworkFact::Plain,
                    arity_hint: None,
                    receiver: None,
                    args: Vec::new(),
                    context: None,
                },
            );
            let live = match live {
                ReducedValue::Type(t) => Some(t),
                // Neither is a type answer, so neither is something a
                // conclusion claims to summarize.
                ReducedValue::FactMap(_) | ReducedValue::None => None,
            };
            match map.evaluate(&key, None, None, &[]) {
                // A decode defers to the live path, so it cannot disagree
                // with it — that is the point of keeping it distinct from
                // absent.
                Outcome::Decode | Outcome::Follow { .. } => {}
                Outcome::Answer(baked) => {
                    checked += 1;
                    assert_eq!(
                        Some(baked.clone()),
                        live,
                        "{label}: baked {key:?} as {baked:?} but the live chase says {live:?} \
                         — a stored answer that disagrees with its own derivation"
                    );
                }
                Outcome::None => {
                    checked += 1;
                    assert_eq!(
                        live, None,
                        "{label}: {key:?} is ABSENT from the map (which means None) but the \
                         live chase answers {live:?} — the enumeration missed a key the bag \
                         can answer, which is the silent-wrong-answer case"
                    );
                }
            }
        }
    }
    assert!(
        checked > 0,
        "no conclusion was compared — this test would pass vacuously"
    );
}

/// A receiver-dependent answer must never bake as a constant.
///
/// This is the specific way `Value` goes wrong: a fluent accessor baked
/// from its declaring class hands that class to every subclass caller, and
/// the answer is a plausible class name rather than an obvious error.
#[test]
fn a_receiver_dependent_answer_is_never_baked_as_a_constant() {
    // Mojo::Base accessors return the invocant, so `has` here is the
    // receiver-dependent shape.
    // A constructor, not an accessor. `bless {}, $c` is `ReceiverOr`:
    // with no receiver it yields the enclosing class — a REAL type — so
    // the constant probe is the only thing standing between it and a
    // `Value`. An accessor would pass this test for the wrong reason
    // (its bare probe answers None, so it cannot bake as a constant
    // however the probe behaves).
    let fa = analyze(
        "package R;\nsub new { my ($class, $arg) = @_; bless $arg => $class }\n1;\n",
    );
    let map = bake(
        &fa.witnesses,
        &ReducerRegistry::with_defaults(),
        &fa.packages.keys().cloned().collect(),
    );
    let mut saw_accessor = false;
    for (key, c) in map.0.iter() {
        let ConclusionKey::MethodOnClass { class, name } = key else { continue };
        if class != "R" || name != "new" {
            continue;
        }
        saw_accessor = true;
        assert!(
            !matches!(c, Conclusion::Value(_)),
            "the receiver-polymorphic constructor baked as a constant {c:?} — \
             `Child->new` would be handed the declaring class"
        );
    }
    assert!(
        saw_accessor,
        "the fixture produced no accessor conclusion, so this proves nothing"
    );

}

/// The bake must not depend on map iteration order.
///
/// The sibling of `witnesses_tests::the_fold_does_not_depend_on_map_iteration_order`,
/// and required for the same reason one level up: the diff-propagation
/// driver (`docs/prompt-enrichment-alternatives.md`) cuts its worklist on
/// an EMPTY conclusion diff. An order-dependent bake produces spurious
/// diffs that never cut the chain, and — worse — spuriously empty ones
/// that cut a chain which should have propagated, leaving a consumer on a
/// stale answer with nothing to notice.
///
/// `bake` walks `attachments()`, which is `HashMap::keys()`, so this is
/// not a hypothetical shape — it is the actual iteration the bake does.
/// Every new conclusion kind joins this test.
#[test]
fn the_bake_does_not_depend_on_map_iteration_order() {
    let sources: &[(&str, &str)] = &[
        ("constructor", "package K;\nsub new { my ($c, $a) = @_; bless $a => $c }\n1;\n"),
        ("moo", "package M2;\nuse Moo;\nhas a => (is => 'rw');\nhas b => (is => 'ro');\n1;\n"),
        ("inherit", "package P2;\nsub f { return 'x' }\npackage C3;\nour @ISA = ('P2');\n1;\n"),
        ("slots", "package S;\nsub set { my $s = shift; $s->{n} = 1; $s->{t} = 'x'; return $s }\n1;\n"),
    ];
    let registry = ReducerRegistry::with_defaults();
    // The map is compared as a SORTED key/value list rather than by
    // HashMap equality, so the comparison is over content and cannot be
    // satisfied by two maps that merely hash the same.
    let snapshot = |src: &str| -> Vec<(String, String)> {
        let fa = analyze(src);
        let map = bake(
            &fa.witnesses,
            &registry,
            &fa.packages.keys().cloned().collect(),
        );
        let mut out: Vec<(String, String)> = map
            .0
            .iter()
            .map(|(k, v)| (format!("{k:?}"), format!("{v:?}")))
            .collect();
        out.sort();
        out
    };
    for (label, src) in sources {
        let first = snapshot(src);
        assert!(
            !first.is_empty(),
            "{label}: baked nothing, so this source exercises no ordering"
        );
        for round in 0..6 {
            assert_eq!(
                first,
                snapshot(src),
                "{label}: the bake produced a different map on round {round} with \
                 identical input — an iteration-order dependence, which makes a \
                 conclusion DIFF unsound and the propagation worklist wrong in \
                 both directions"
            );
        }
    }
}

// ---- the Link walk ----

use std::sync::Arc;

fn m(entries: Vec<(ConclusionKey, Conclusion)>) -> Arc<ConclusionMap> {
    Arc::new(ConclusionMap(
        entries.into_iter().collect(),
        Default::default(),
    ))
}

fn moc(class: &str, name: &str) -> ConclusionKey {
    ConclusionKey::MethodOnClass {
        class: class.into(),
        name: name.into(),
    }
}

/// A `Link` chain resolves to the answer at its end, with no bag decoded.
///
/// Exercised directly rather than through the corpus: on the substrate today
/// only 4 `Follow`s fire and all are incomplete, so the success path would
/// otherwise ship untested. A walker that never succeeds in its own test suite
/// is a walker nobody has run.
#[test]
fn a_link_chain_resolves_to_the_answer_at_its_end() {
    let a = m(vec![(
        moc("A", "f"),
        Conclusion::Link {
            target: moc("B", "f"),
            arity: None,
            receiver: ReceiverRule::Thread,
        },
    )]);
    let b = m(vec![(
        moc("B", "f"),
        Conclusion::Value(crate::model::file_analysis::InferredType::HashRef),
    )]);
    let resolve = move |class: &str| match class {
        "A" => vec![("/a.pm".to_string(), Some(a.clone()))],
        "B" => vec![("/b.pm".to_string(), Some(b.clone()))],
        _ => vec![],
    };
    let got = crate::model::witnesses::registry::follow_link_with(&resolve, &moc("A", "f"), &None, None, &[]);
    assert_eq!(
        got,
        Some(crate::model::file_analysis::InferredType::HashRef),
        "a two-hop Link chain did not reach its answer"
    );
}

/// A cycle terminates instead of spinning, and degrades to a decode.
#[test]
fn a_cyclic_link_chain_terminates() {
    let a = m(vec![(
        moc("A", "f"),
        Conclusion::Link {
            target: moc("B", "f"),
            arity: None,
            receiver: ReceiverRule::Thread,
        },
    )]);
    let b = m(vec![(
        moc("B", "f"),
        Conclusion::Link {
            target: moc("A", "f"),
            arity: None,
            receiver: ReceiverRule::Thread,
        },
    )]);
    let resolve = move |class: &str| match class {
        "A" => vec![("/a.pm".to_string(), Some(a.clone()))],
        "B" => vec![("/b.pm".to_string(), Some(b.clone()))],
        _ => vec![],
    };
    let got = crate::model::witnesses::registry::follow_link_with(&resolve, &moc("A", "f"), &None, None, &[]);
    assert_eq!(
        got, None,
        "a cyclic chain produced an answer; it must degrade to the decode instead"
    );
}

/// An `OpenNone` anywhere on the chain degrades the whole walk.
///
/// The walk has no bag, so it cannot resolve what `OpenNone` defers. Returning
/// a partial answer here would be the one failure mode this form has that
/// serves a WRONG answer rather than costing a decode.
#[test]
fn an_open_none_on_the_chain_degrades_to_a_decode() {
    let a = m(vec![(
        moc("A", "f"),
        Conclusion::Link {
            target: moc("B", "f"),
            arity: None,
            receiver: ReceiverRule::Thread,
        },
    )]);
    let b = m(vec![(moc("B", "f"), Conclusion::OpenNone)]);
    let resolve = move |class: &str| match class {
        "A" => vec![("/a.pm".to_string(), Some(a.clone()))],
        "B" => vec![("/b.pm".to_string(), Some(b.clone()))],
        _ => vec![],
    };
    assert_eq!(
        crate::model::witnesses::registry::follow_link_with(&resolve, &moc("A", "f"), &None, None, &[]),
        None,
        "the walk answered past an OpenNone it cannot resolve"
    );
}

/// A candidate that PROVES `None` does not stop the ladder — the next one is
/// tried, exactly as the live chase's candidate loop does.
///
/// "Proves" is the load-bearing word, and it is why the empty map here declares
/// `B` closed. An empty map for a class that is NOT closed returns `Decode`,
/// not `None`: absence is only conclusive for a class whose ancestors are all
/// accounted for. My first version of this test omitted that and expected the
/// ladder to continue over an inconclusive absence — the walker was right and
/// the expectation was wrong.
#[test]
fn a_none_candidate_does_not_stop_the_ladder() {
    let empty = Arc::new(ConclusionMap(
        Default::default(),
        ["B".to_string()].into_iter().collect(),
    ));
    let real = m(vec![(
        moc("B", "f"),
        Conclusion::Value(crate::model::file_analysis::InferredType::ArrayRef),
    )]);
    let resolve = move |class: &str| match class {
        "B" => vec![
            ("/empty.pm".to_string(), Some(empty.clone())),
            ("/real.pm".to_string(), Some(real.clone())),
        ],
        _ => vec![],
    };
    assert_eq!(
        crate::model::witnesses::registry::follow_link_with(&resolve, &moc("B", "f"), &None, None, &[]),
        Some(crate::model::file_analysis::InferredType::ArrayRef),
        "the first candidate's None ended the walk instead of falling through"
    );
}
