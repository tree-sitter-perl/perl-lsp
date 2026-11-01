//! `@cond.*`: conditional-compilation arms, fed to the arm builder
//! (`query_extract/cond.rs`) as the walk reaches them.

use super::*;

/// One arm per node. An `#elif` is its own match's `@cond.if` and its
/// predecessor's `@cond.else` on one node, so the arm is read from the
/// node's first arm capture: what it tests through the `@cond.if` match,
/// whether it is an alternative from the node. A test is read through
/// its arm; a guard names its token.
pub(super) fn handle(st: &mut ExtractState, ix: usize, e: &Event) {
    let ad = st.adorned;
    let is_arm = |c: Capture| matches!(c, Capture::Cond(CondCap::If | CondCap::Else));
    match e.cap {
        Capture::Cond(CondCap::Guard) => st.arms.guard(e.start, e.end),
        c if is_arm(c) => {
            if ad.on_ixs(e).find(|&j| is_arm(ad.events()[j].cap)) != Some(ix) {
                return;
            }
            let alternative = ad.on(e).any(|c| c.cap == Capture::Cond(CondCap::Else));
            let test = ad.on(e).filter(|c| c.cap == Capture::Cond(CondCap::If)).find_map(|arm| {
                ad.of_match(arm.match_id).find_map(|t| match t.cap {
                    Capture::Cond(k @ (CondCap::Test | CondCap::TestDefined | CondCap::TestUndefined)) => {
                        Some(ArmTest { cap: k, start: t.start, end: t.end, text: &t.text })
                    }
                    _ => None,
                })
            });
            st.arms.arm(e.start, e.end, test, alternative);
        }
        _ => {}
    }
}
