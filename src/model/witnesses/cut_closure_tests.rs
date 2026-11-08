//! A cycle that closes at or beneath a frame is part of that frame's own
//! answer, so the fold memo shares it; one that closes above it is not.
//! Every cut — the visited guard's and a fan-out's — has to say which frame
//! it closed at for that test to mean anything.

use super::*;
use crate::model::file_analysis::Span;
use tree_sitter::Point;

fn at(row: usize, col: usize) -> Span {
    Span { start: Point { row, column: col }, end: Point { row, column: col + 1 } }
}

fn expr(row: usize, col: usize) -> WitnessAttachment {
    WitnessAttachment::Expr(at(row, col))
}

fn witness(row: usize, col: usize, payload: WitnessPayload) -> Witness {
    Witness { attachment: expr(row, col), source: WitnessSource::Builder("test".into()), payload, span: at(row, col) }
}

/// `read(i) → call(i) → { read(i), read(i-1) }`: each read's chase comes back
/// to itself through its own call, the shape of `x = x.add()` when the write
/// lands before its value. `read(0)` is the `A`.
fn self_cycling_chain(links: usize) -> WitnessBag {
    let mut bag = WitnessBag::new();
    bag.push(witness(0, 0, WitnessPayload::InferredType(InferredType::ClassName("A".into()))));
    for i in 1..=links {
        bag.push(witness(i, 0, WitnessPayload::Edge(expr(i, 4))));
        bag.push(witness(i, 4, WitnessPayload::Edge(expr(i, 0))));
        bag.push(witness(i, 4, WitnessPayload::Edge(expr(i - 1, 0))));
    }
    bag
}

/// Ask every read in order under one memo scope, as the stamp does; the
/// answers and the attachments reduced.
fn ask_every_read(bag: &WitnessBag, links: usize) -> (Vec<ReducedValue>, u64) {
    let reg = ReducerRegistry::with_defaults();
    let before = query_rec_bodies();
    let _scope = FoldMemoScope::enter();
    let answers = (0..=links)
        .map(|i| {
            let att = expr(i, 0);
            reg.query(
                bag,
                &ReducerQuery {
                    attachment: &att,
                    point: None,
                    framework: FrameworkFact::Plain,
                    arity_hint: None,
                    receiver: None,
                    args: Vec::new(),
                    context: None,
                },
            )
        })
        .collect();
    (answers, query_rec_bodies() - before)
}

#[test]
fn a_self_closed_answer_is_shared_and_agrees_with_no_memo() {
    let bag = self_cycling_chain(40);
    let (answers, _) = ask_every_read(&bag, 40);
    let (control, _) = without_fold_memo(|| ask_every_read(&bag, 40));
    assert_eq!(answers, control);
    let a = ReducedValue::Type(InferredType::ClassName("A".into()));
    assert!(answers.iter().all(|v| *v == a), "{answers:?}");
}

/// Deterministic cost pin: an unshared self-closed answer makes every read
/// re-walk the chain beneath it (four times the work at twice the links).
#[test]
fn a_chain_of_self_closing_reads_reduces_each_link_a_bounded_number_of_times() {
    let cost = |links| ask_every_read(&self_cycling_chain(links), links).1;
    let (short, long) = (cost(60), cost(120));
    assert!(long < short * 3, "60 links reduced {short} attachments, 120 reduced {long}: superlinear");
}

fn fan_out(name: &str) -> FanOutKey {
    (FanOut::Free { name: name.into(), class: None }, None, None)
}

/// A frame's taint, with `inner` run as its subtree.
fn frame_taint(st: &mut QueryState, inner: impl FnOnce(&mut QueryState)) -> u32 {
    let (pos, outer) = st.enter_frame();
    inner(st);
    st.leave_frame(pos, outer, QUERY_REC_TRUNCATIONS.with(|c| c.get()))
}

/// A fan-out re-entered beneath itself closes at the fan-out's own frame:
/// a frame between the root and the fan-out is not shaped by it.
#[test]
fn a_fan_out_cycle_beneath_a_frame_leaves_it_clean() {
    let mut st = QueryState::new();
    let taint = frame_taint(&mut st, |st| {
        st.fan_out_node(fan_out("f"), |st| st.fan_out_node(fan_out("f"), |_| unreachable!()));
    });
    assert_eq!(taint, Taint::CLEAN);
}

/// A fan-out re-entered from a frame beneath it closes ABOVE that frame, so
/// the frame's answer depends on its path and must not be shared.
#[test]
fn a_fan_out_cycle_above_a_frame_taints_it() {
    let mut st = QueryState::new();
    let mut inner = None;
    st.fan_out_node(fan_out("f"), |st| {
        inner = Some(frame_taint(st, |st| {
            st.fan_out_node(fan_out("f"), |_| unreachable!());
        }));
        None
    });
    assert_eq!(inner, Some(Taint::PATH));
    // The fan-out's own answer closed its cycle itself, and a later reach of
    // it replays that: the enclosing frame stays clean.
    let taint = frame_taint(&mut st, |st| {
        st.fan_out_node(fan_out("f"), |_| unreachable!());
    });
    assert_eq!(taint, Taint::CLEAN);
}

/// A fan-out whose own answer was cut above it replays that cut when it is
/// reached again: the memoized value is the path's, not the key's.
#[test]
fn a_memoized_fan_out_replays_its_taint() {
    let mut st = QueryState::new();
    st.fan_out_node(fan_out("g"), |st| {
        st.fan_out_node(fan_out("f"), |st| st.fan_out_node(fan_out("g"), |_| unreachable!()));
        None
    });
    let mut replayed = None;
    let root = frame_taint(&mut st, |st| {
        replayed = Some(frame_taint(st, |st| {
            st.fan_out_node(fan_out("f"), |_| unreachable!());
        }));
    });
    assert_eq!(replayed, Some(Taint::PATH));
    assert_eq!(root, Taint::CLEAN, "the root's answer is a fresh query by definition");
}
