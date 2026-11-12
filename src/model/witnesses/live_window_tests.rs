//! A temporal read through `WitnessBag::live_at` answers what a read over
//! every witness answers, and costs what the window holds, not what the
//! variable's whole life holds.

use super::*;

fn at(row: usize, column: usize) -> Point {
    Point { row, column }
}

fn var() -> WitnessAttachment {
    WitnessAttachment::Variable { name: "$x".into(), scope: ScopeId(0) }
}

fn witness(attachment: WitnessAttachment, payload: WitnessPayload, start: Point, end: Point) -> Witness {
    Witness { attachment, source: WitnessSource::Builder("test".into()), payload, span: Span { start, end } }
}

/// A small deterministic generator, so a failing seed replays.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) as usize) % n
    }
}

/// One variable's life: writes, point facts, regions, uses, a deferred
/// bless, edges that resolve and one that doesn't, pushed out of source
/// order the way later passes land them.
fn random_bag(seed: u64) -> WitnessBag {
    let mut g = Lcg(seed);
    let mut bag = WitnessBag::new();
    let class = |i: usize| InferredType::ClassName(format!("C{i}"));
    for _ in 0..12 + g.next(30) {
        let row = g.next(14);
        let start = at(row, g.next(3));
        let region_end = at(row + 1 + g.next(3), 0);
        let point = |w: WitnessPayload| (w, start, start);
        let (payload, s, e) = match g.next(11) {
            0 | 1 => point(WitnessPayload::Reset),
            2 => point(WitnessPayload::InferredType(class(g.next(4)))),
            3 => (WitnessPayload::InferredType(class(g.next(4))), start, region_end),
            4 => point(WitnessPayload::Observation(TypeObservation::HashRefAccess)),
            5 => (WitnessPayload::Observation(TypeObservation::NumericUse), start, region_end),
            6 => point(WitnessPayload::ReturnExpr(ReturnExpr::Concrete(class(9)))),
            // Uses repeat, and two reps can disagree.
            9 => point(WitnessPayload::Observation(TypeObservation::ArrayRefAccess)),
            10 => {
                let obs = if g.next(2) == 0 { TypeObservation::HashRefAccess } else { TypeObservation::NumericUse };
                (WitnessPayload::Observation(obs), start, at(row + g.next(6), 2))
            }
            7 => {
                let rhs = Span { start: at(row, 5), end: at(row, 9) };
                let target = WitnessAttachment::Expr(rhs);
                if g.next(4) > 0 {
                    bag.push(witness(target.clone(), WitnessPayload::InferredType(class(g.next(4))), rhs.start, rhs.end));
                }
                point(WitnessPayload::Edge(target))
            }
            _ => point(WitnessPayload::InferredType(InferredType::ArrayRef)),
        };
        bag.push(witness(var(), payload, s, e));
    }
    bag
}

#[test]
fn a_read_answers_the_same_through_the_window() {
    let reg = ReducerRegistry::with_defaults();
    let att = var();
    let mut windowed = 0;
    for seed in 0..400 {
        let bag = random_bag(seed);
        // The order a push files and the one a rebuild sorts are the same.
        let mut rebuilt = bag.clone();
        rebuilt.rebuild_index();
        for row in 0..18 {
            for column in [0, 1, 4] {
                let q = ReducerQuery {
                    attachment: &att,
                    point: Some(ReadPoint::position(at(row, column))),
                    framework: FrameworkFact::Plain,
                    arity_hint: None,
                    receiver: None,
                    args: Vec::new(),
                    context: None,
                };
                let through = reg.query(&bag, &q);
                let whole = without_live_window(|| reg.query(&bag, &q));
                assert_eq!(through, whole, "seed {seed} at {row}:{column}");
                assert_eq!(reg.query(&rebuilt, &q), whole, "rebuilt, seed {seed} at {row}:{column}");
                windowed += usize::from(bag.live_at(&att, at(row, column)).is_some());
            }
        }
    }
    // The net is only as good as the reads that took the window.
    assert!(windowed > 4000, "only {windowed} reads were windowed");
}

/// The window's soundness rests on one reducer reading windowed payloads on
/// a temporal attachment, and reading them through `LiveWindow`. A new
/// claimant has to say how it reads them first.
#[test]
fn only_the_windowed_fold_claims_windowed_payloads() {
    let reg = ReducerRegistry::with_defaults();
    let here = at(0, 0);
    for att in [var(), WitnessAttachment::Place(Box::new(crate::model::file_analysis::Place::variable(
        crate::model::file_analysis::Binding::Package("main".into(), "$x".into()),
    )))] {
        for payload in [
            WitnessPayload::Reset,
            WitnessPayload::InferredType(InferredType::HashRef),
            WitnessPayload::Observation(TypeObservation::NumericUse),
            WitnessPayload::Edge(WitnessAttachment::Expr(Span { start: here, end: here })),
        ] {
            assert!(payload.is_windowed());
            let claimants = reg.claimants(&witness(att.clone(), payload.clone(), here, here));
            assert!(
                claimants.iter().all(|c| *c == "framework_aware_type_fold"),
                "{payload:?} on {att:?} is claimed by {claimants:?}"
            );
        }
    }
}

fn perl_chain(links: usize) -> String {
    let mut src = String::from(
        "package A; sub new { bless {}, shift } sub add { my $s = shift; return $s }\n\
         package main; sub f { my $x = A->new;\n",
    );
    for _ in 0..links {
        src.push_str("$x = $x->add;\n");
    }
    src.push_str("}\n1;\n");
    src
}

/// Deterministic cost pin: every link of `$x = $x->add` reads `$x` at its
/// own point, and the variable holds every write of the chain. Reading
/// only the window keeps the witnesses a build reads linear in the chain;
/// reading them all is quadratic.
#[test]
fn a_chain_build_reads_each_write_a_bounded_number_of_times() {
    // Under either equivalence net every build has a twin (window off, or
    // memo off) that pays the quadratic read this pins; the nets check
    // answers, not cost.
    if live_window_equiv_enabled() || fold_memo_equiv_enabled() {
        return;
    }
    let cost = |links| {
        let src = perl_chain(links);
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
        let tree = parser.parse(&src, None).unwrap();
        let before = materialize_reads();
        crate::build::builder::build(&tree, src.as_bytes());
        materialize_reads() - before
    };
    let (short, long) = (cost(60), cost(120));
    assert!(long < short * 3, "60 links read {short} witnesses, 120 read {long}: superlinear");
}

fn perl_uses(uses: usize) -> String {
    let mut src = String::from("package G; sub new { bless {}, shift } sub foo {}\nsub init { my $self = shift;\n");
    for i in 0..uses {
        src.push_str(&format!("$self->{{k{i}}} = 1;\n"));
    }
    src.push_str("$self->foo;\n}\n1;\n");
    src
}

/// Deterministic cost pin: every `$self->{k}` reads `$self` at its own
/// point and leaves one more use of it behind. Keeping only the latest use
/// of each kind keeps the witnesses a build reads linear in the uses.
#[test]
fn a_build_reads_each_use_a_bounded_number_of_times() {
    if live_window_equiv_enabled() || fold_memo_equiv_enabled() {
        return;
    }
    let cost = |uses| {
        let src = perl_uses(uses);
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_parser_perl::LANGUAGE.into()).unwrap();
        let tree = parser.parse(&src, None).unwrap();
        let before = materialize_reads();
        crate::build::builder::build(&tree, src.as_bytes());
        materialize_reads() - before
    };
    let (short, long) = (cost(100), cost(200));
    assert!(long < short * 3, "100 uses read {short} witnesses, 200 read {long}: superlinear");
}
