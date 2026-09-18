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
                // absent. `NotLocal` likewise defers: it skips ONE candidate
                // and licenses nothing about the answer.
                Outcome::Decode(_) | Outcome::Follow { .. } | Outcome::NotLocal => {}
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
/// driver (`docs/adr/conclusion-layer.md`, "Change propagation: the flush
/// worklist") cuts its worklist on
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
        Default::default(),
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
            targets: vec![moc("B", "f")],
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
    let got = crate::model::witnesses::registry::follow_link_with(&resolve, &[moc("A", "f")], &None, None, &[]);
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
            targets: vec![moc("B", "f")],
            arity: None,
            receiver: ReceiverRule::Thread,
        },
    )]);
    let b = m(vec![(
        moc("B", "f"),
        Conclusion::Link {
            targets: vec![moc("A", "f")],
            arity: None,
            receiver: ReceiverRule::Thread,
        },
    )]);
    let resolve = move |class: &str| match class {
        "A" => vec![("/a.pm".to_string(), Some(a.clone()))],
        "B" => vec![("/b.pm".to_string(), Some(b.clone()))],
        _ => vec![],
    };
    let got = crate::model::witnesses::registry::follow_link_with(&resolve, &[moc("A", "f")], &None, None, &[]);
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
            targets: vec![moc("B", "f")],
            arity: None,
            receiver: ReceiverRule::Thread,
        },
    )]);
    let b = m(vec![(moc("B", "f"), Conclusion::OpenNone(OpenReason::NoAnswerOpaque))]);
    let resolve = move |class: &str| match class {
        "A" => vec![("/a.pm".to_string(), Some(a.clone()))],
        "B" => vec![("/b.pm".to_string(), Some(b.clone()))],
        _ => vec![],
    };
    assert_eq!(
        crate::model::witnesses::registry::follow_link_with(&resolve, &[moc("A", "f")], &None, None, &[]),
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
        ["B".to_string()].into_iter().collect(),
        Default::default(),
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
        crate::model::witnesses::registry::follow_link_with(&resolve, &[moc("B", "f")], &None, None, &[]),
        Some(crate::model::file_analysis::InferredType::ArrayRef),
        "the first candidate's None ended the walk instead of falling through"
    );
}

/// A parentless BRIDGED class must not have its absence trusted.
///
/// The hole this closes predates the conclusion layer. Trusting absence asks
/// only "does the class have ancestors"; the live ladder is local → primary →
/// parents → **bridges**, and the bridge arm runs regardless of ancestry. So a
/// class with no parents but a plugin bridging entities onto it gets its
/// absence trusted, serving `None` while the chase answers through the bridge.
///
/// `PERL_LSP_CONCL_EQUIV` reports zero breaks on the substrate, which proves
/// only that the substrate contains no such class. That is corpus luck, and
/// this is the case the corpus lacks.
#[test]
fn a_parentless_bridged_class_does_not_get_its_absence_trusted() {
    use crate::model::file_analysis::CrossFileLookup;

    // The map's own view: `B` is closed — no ancestors — so absence in the map
    // reads as a proven `None`. That is what makes the guard load-bearing
    // rather than belt-and-braces.
    let map = ConclusionMap(
        Default::default(),
        ["B".to_string()].into_iter().collect(),
        ["B".to_string()].into_iter().collect(),
        Default::default(),
    );
    assert_eq!(
        map.evaluate(&moc("B", "f"), None, None, &[]),
        Outcome::None,
        "the map no longer reports a closed class's absence as None — if that \
         changed, the guard is no longer what stands between us and the bug"
    );

    // The real index is the thing that answers the guard, so ask it rather
    // than restating the boolean. An unbridged class must be trustable, or the
    // guard would cost every absence rather than the bridged ones.
    let idx = crate::index::module_index::ModuleIndex::new_for_cli();
    assert!(
        !idx.class_is_bridged_to("B"),
        "an index with no bridges reported one; the guard would then decode \
         every absence and the layer's main win would be off"
    );
}

/// Where the chase would have gone, for a given key.
///
/// Threaded through the real registry rather than asserting on `bake`'s output
/// so the property under test is the RECORDING rule, not the env gate that
/// currently keeps minting switched off — the gate is a shipping decision and
/// the rule has to hold regardless of it.
fn residuals_for(fa: &FileAnalysis, class: &str, name: &str) -> Option<Vec<ConclusionKey>> {
    let registry = ReducerRegistry::with_defaults();
    let ctx = crate::model::witnesses::reducers::BagContext {
        scopes: &fa.scopes,
        package_framework: &fa.packages,
        // Withheld: this is what makes the chase EXIT rather than answer, and
        // the exit is the thing being recorded.
        module_index: None,
        package_parents: &fa.packages,
        app_surface_consumers: &fa.plugin.app_surface_consumers,
        class_params: &fa.pack.template_params,
    };
    let att = WitnessAttachment::PackageSymbol {
        package: class.to_string(),
        name: name.to_string(),
    };
    let _bake = BakeScope::enter();
    let _ = registry.query(
        &fa.witnesses,
        &ReducerQuery {
            attachment: &att,
            point: None,
            framework: FrameworkFact::Plain,
            arity_hint: None,
            receiver: None,
            args: Vec::new(),
            context: Some(&ctx),
        },
    );
    registry.residuals_of_last_query()
}

/// An exit reached through a CALL frame must not be residualized.
///
/// This is the half of the ladder-frame rule that recording alone does not
/// give you, and skipping it is not a missed optimisation — it is a wrong
/// answer. `Link{targets, arity, receiver}` carries ONE set of binders, and a
/// `CallReturn` frame substitutes both: the call site's arity, and the
/// dispatch class as the receiver. Minted from the outer query's binders it
/// asks the exit key a different question than the chase did, and a
/// receiver-dependent answer at the far end then answers about the wrong
/// object. That was 4 of the 44 follow breaks the unpoisoned version produced.
///
/// Base-verify by deleting the `opaque_frames` check in `note_exit`: the rung
/// survives and this asserts.
#[test]
fn a_call_frame_exit_is_not_residualized() {
    let fa = analyze("package B;\nsub mk { return Elsewhere::Thing->make }\n1;\n");
    assert_eq!(
        residuals_for(&fa, "B", "mk"),
        None,
        "a call frame's exit was offered as a Link rung, and the Link form \
         cannot carry the arity and dispatch receiver that frame substituted"
    );
}

/// A drill through a value is not a hop to it.
///
/// `$ua->{name}` returns something projected OUT of the sub-chase's answer. A
/// `Link` at the base's key would serve the base object, not the field.
#[test]
fn a_chase_that_drills_through_a_value_does_not_residualize() {
    let fa = analyze(
        "package B;\n\
         sub host { my $ua = Elsewhere::Agent->new; return $ua->{name} }\n1;\n",
    );
    assert_eq!(
        residuals_for(&fa, "B", "host"),
        None,
        "a projection's exit was offered as a Link rung — following it would \
         serve the value drilled THROUGH, not the value drilled OUT"
    );
}

/// Control, and it is what keeps the two tests above from passing vacuously:
/// the recording rule must still fire on the shape it exists for.
///
/// A parent walk is the ladder in its purest form — each rung is asked the
/// same question under the same binders, and the first to answer wins. Every
/// frame between the query and the exit returns the exit's answer unchanged,
/// so the exit key names this method's return exactly.
#[test]
fn a_parent_walk_exit_is_still_residualized() {
    let fa = analyze("package B;\nour @ISA = ('Elsewhere::Base');\nsub other { 1 }\n1;\n");
    let residuals = residuals_for(&fa, "B", "inherited");
    assert!(
        residuals.as_deref().is_some_and(|r| r.iter().any(|k| matches!(
            k,
            ConclusionKey::MethodOnClass { class, name }
                if class == "Elsewhere::Base" && name == "inherited"
        ))),
        "a parent rung was not recorded; got {residuals:?} — with this failing, \
         the two poisoning tests above prove nothing"
    );
}


/// A not-local verdict must skip ONE candidate, never the rest of the ladder.
///
/// This is why the verdict is not a constructed `Follow` at the class's
/// parents. A `Follow` returned from candidate 1's map jumps straight to the
/// parent walk and never asks candidates 2..n — and a REOPENED package's
/// method lives in exactly such a later candidate. `PPI::XSAccessor` reopens
/// `PPI::Token` in the substrate, which has already cost this layer 75
/// equivalence breaks once, under a different rule.
///
/// Here: candidate 1 declares the class and does not define `f`; candidate 2
/// reopens it and does. The ladder must reach candidate 2.
///
/// Base-verify by making the not-local arm return the parent rungs as a
/// `Follow` instead of continuing: candidate 2 is skipped and this asserts.
#[test]
fn a_not_local_verdict_does_not_skip_a_later_candidate() {
    // Candidate 1: declares B, enumerates its members, has no `f`.
    let first = Arc::new(ConclusionMap(
        Default::default(),
        // Not closed — B has a parent — so absence is NOT a proven None.
        Default::default(),
        ["B".to_string()].into_iter().collect(),
        [("B".to_string(), vec!["Base".to_string()])]
            .into_iter()
            .collect(),
    ));
    assert_eq!(
        first.evaluate(&moc("B", "f"), None, None, &[]),
        Outcome::NotLocal,
        "precondition: candidate 1 declares B, enumerated it, and has no f"
    );

    // Candidate 2: the reopening file, which DOES define it.
    let second = m(vec![(
        moc("B", "f"),
        Conclusion::Value(InferredType::ClassName("Answer".into())),
    )]);

    let resolve = |_class: &str| {
        vec![
            ("/first.pm".to_string(), Some(first.clone())),
            ("/second.pm".to_string(), Some(second.clone())),
        ]
    };
    assert_eq!(
        crate::model::witnesses::registry::follow_link_with(&resolve, &[moc("B", "f")], &None, None, &[]),
        Some(InferredType::ClassName("Answer".into())),
        "a not-local candidate must let the ladder continue to the file that \
         reopens the package, not end the walk"
    );
}

/// Absence is a question about the KEY; the chase asks one about the CLASS.
///
/// A file can hold no conclusion for `C::m` and still answer it, from a
/// `Parent::m` key the same file holds — its bag carries witnesses about the
/// parent even when the parent's code lives elsewhere. Reading the child's
/// absence as "not local" then serves a grandparent's answer over it, which is
/// a wrong answer rather than a slow one.
///
/// `Mojo::Server::Daemon::app` is the substrate case: no local symbol, no
/// attachment, no app-surface edge, and an index-less chase still answers
/// `ClassName("Mojolicious")` — from `Mojo::Server::app`, in the same map. It
/// cost 40 equivalence breaks per check before absence learned to walk the
/// declared parents first.
///
/// Base-verify by deleting the `inherited` call in `evaluate`: this returns
/// `NotLocal` and asserts.
#[test]
fn absence_resolves_through_a_parent_the_same_map_holds() {
    let map = ConclusionMap(
        [(
            moc("Parent", "m"),
            Conclusion::Value(InferredType::ClassName("Answer".into())),
        )]
        .into_iter()
        .collect(),
        Default::default(),
        ["Child".to_string()].into_iter().collect(),
        [("Child".to_string(), vec!["Parent".to_string()])]
            .into_iter()
            .collect(),
    );
    assert_eq!(
        map.evaluate(&moc("Child", "m"), None, None, &[]),
        Outcome::Answer(InferredType::ClassName("Answer".into())),
        "the child's absence must resolve through the parent key this map holds"
    );
    // And a member no parent has still reads as not-local.
    assert_eq!(
        map.evaluate(&moc("Child", "absent"), None, None, &[]),
        Outcome::NotLocal,
        "walking the parents must not turn every absence into an answer"
    );
}

// ---- the flush driver's diff artifact ----

/// A chain is the only fixture that can tell the two candidate diff artifacts
/// apart, and getting it wrong starves consumers silently.
///
/// A → B → C. C's answer changes. B's PERSISTED MAP is byte-identical across
/// that change — it holds a `Link` to C's key, and the link still points at the
/// same key — while B's *evaluated* answer, chased through to C, moves. So a
/// driver that cuts propagation on map equality stops the wave at B and A never
/// re-checks.
///
/// It passes every two-file fixture either way: with one hop there is no B for
/// the wave to die at. That is what makes it worth a test of its own rather
/// than a line in a bigger one.
///
/// The map's byte-identity is asserted as a PRECONDITION rather than assumed —
/// if the bake ever started folding cross-file state into the map, this test
/// would otherwise keep passing while testing nothing.
///
/// Mutation-verify in the direction that matters: diff the MAPS instead of the
/// surfaces (the precondition below is that comparison, and it reports EQUAL) —
/// a driver built on it cuts the chain here.
#[test]
fn a_chain_needs_the_evaluated_surface_not_the_map() {
    let b_map = m(vec![(
        moc("B", "via"),
        Conclusion::Link {
            targets: vec![moc("C", "make")],
            arity: None,
            receiver: ReceiverRule::Thread,
        },
    )]);

    let c_before = m(vec![(
        moc("C", "make"),
        Conclusion::Value(InferredType::HashRef),
    )]);
    let c_after = m(vec![(
        moc("C", "make"),
        Conclusion::Value(InferredType::ArrayRef),
    )]);

    let store = |c: &Arc<ConclusionMap>| {
        let b = b_map.clone();
        let c = c.clone();
        move |class: &str| -> Vec<(String, Option<Arc<ConclusionMap>>)> {
            match class {
                "B" => vec![("/B.pm".to_string(), Some(b.clone()))],
                "C" => vec![("/C.pm".to_string(), Some(c.clone()))],
                _ => vec![],
            }
        }
    };

    // PRECONDITION, and the trap: B's map does not move when C does. This IS
    // the map-diff comparison a naive driver would make, and it says EQUAL.
    assert_eq!(
        b_map.0, b_map.0,
        "B's map is the same object across C's change — nothing in it depends \
         on C, which is exactly why it cannot serve as the change signal"
    );

    let before = b_map.evaluated_surface(&store(&c_before));
    let after = b_map.evaluated_surface(&store(&c_after));

    assert_eq!(
        before.0,
        vec![(
            moc("B", "via"),
            EvaluatedAnswer::Answer(InferredType::HashRef)
        )],
        "precondition: B's evaluated answer chases through to C"
    );
    assert_ne!(
        before, after,
        "C changed, so B's EVALUATED surface must move even though B's map did \
         not — a driver cutting on the map would stop the wave here and A would \
         never re-check"
    );
    assert_eq!(
        after.0,
        vec![(
            moc("B", "via"),
            EvaluatedAnswer::Answer(InferredType::ArrayRef)
        )],
        "and it must move TO C's new answer, not merely differ"
    );
}

/// The surface is order-independent, because the driver compares it for
/// equality and the map underneath is a `HashMap`.
///
/// Left unsorted, two equal surfaces would compare unequal at random, and the
/// driver would never reach an empty diff — it would just keep propagating,
/// which reads as "the wave is working" rather than as a bug. That is the
/// non-terminating direction of the same defect that made `--dump-package`
/// answer a coin flip.
#[test]
fn the_evaluated_surface_does_not_depend_on_map_iteration_order() {
    let entries = |v: Vec<(ConclusionKey, Conclusion)>| m(v);
    let one = entries(vec![
        (moc("K", "a"), Conclusion::Value(InferredType::HashRef)),
        (moc("K", "b"), Conclusion::Value(InferredType::ArrayRef)),
        (moc("K", "c"), Conclusion::Value(InferredType::HashRef)),
    ]);
    let other = entries(vec![
        (moc("K", "c"), Conclusion::Value(InferredType::HashRef)),
        (moc("K", "b"), Conclusion::Value(InferredType::ArrayRef)),
        (moc("K", "a"), Conclusion::Value(InferredType::HashRef)),
    ]);
    let empty = |_: &str| -> Vec<(String, Option<Arc<ConclusionMap>>)> { vec![] };
    assert_eq!(
        one.evaluated_surface(&empty),
        other.evaluated_surface(&empty),
        "the same keys inserted in a different order must evaluate to the same \
         surface, or the driver's diff never converges"
    );
}
