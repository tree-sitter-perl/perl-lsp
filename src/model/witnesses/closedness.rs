//! World-level closedness: a per-class certificate that makes the bake's
//! silence about a member a TRUSTED None instead of a decode.
//!
//! `docs/adr/conclusion-layer.md` ("World-level closedness") owns the
//! design. The
//! population this serves is `OpenReason::AbsentNotClosed` — a class the
//! baking file never declared, so its map's silence says nothing at all.
//! Measured at 22.8–27.6% of open reasons and 96.8% WASTED: the decode that
//! silence forces answers nothing in nearly every case.
//!
//! **This is the arc's first fact whose staleness yields a WRONG answer.**
//! Everywhere else absence costs a decode; here, trusted silence about a
//! method that has since come into existence is a confident lie. The rule
//! that discharges it is the one 6b's rows already use: the certificate
//! self-validates against the live index, correctness never depends on an
//! eraser, and anything that fails to validate reads as not-closed and falls
//! open to exactly today's decode.
//!
//! The certificate is a lane consulted BESIDE the map, never a conclusion
//! kind inside it — the bake stays index-free and both EQUIV disciplines
//! stand unchanged.

use crate::model::file_analysis::{CachedModule, CrossFileLookup, FileAnalysis};
use std::path::PathBuf;
use std::sync::Arc;

/// How many ancestor names a certificate will cover before it gives up.
///
/// A closure this wide is not a class hierarchy, it is a pathological graph,
/// and certifying it would spend more on validation than the decodes it
/// saves. Declining is free: the caller decodes, which is where it already
/// was.
const MAX_CLOSURE: usize = 64;

/// A proof that a class's ancestry was fully enumerable at mint time, and the
/// evidence needed to notice when it stops being so.
///
/// The validity key is ONE structure covering both halves the design names:
/// per ancestor name, the sorted `(provider path, surface fingerprint)` list.
/// The sorted path list IS the provider-set identity — which is what catches
/// a NEW file arriving to provide an ancestor name, the case per-provider
/// fingerprints structurally cannot see. The fingerprints ARE the
/// per-provider half, catching edits to providers already known (a parent
/// list change surfaces here because `Surface` carries parents).
///
/// Keeping them in one structure is deliberate: there is no way to record one
/// half without the other, so a future edit cannot quietly drop the arrival
/// case and leave a certificate that still looks like it validates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosednessCertificate {
    /// Sorted by name, so two certificates over the same world compare equal
    /// regardless of the order the walk happened to enumerate them in.
    closure: Vec<(String, Vec<(PathBuf, u64)>)>,
}

impl ClosednessCertificate {
    /// Mint a certificate for `class`, or decline.
    ///
    /// Declining is always safe and always cheap for the caller — it is the
    /// state every class is in today. Every `None` below is an honest "this
    /// cannot be vouched for", never an optimisation.
    ///
    /// `origin` is the analysis whose map raised the absent verdict; it owns
    /// the ancestry walk. The walk is the expensive part and the caller has
    /// already paid it (it is bound for a decode), which is what makes lazy
    /// minting at this site right — see §6j on why eager enumeration repeats
    /// level-indexing's mistake.
    pub fn mint(
        idx: &dyn CrossFileLookup,
        origin: &FileAnalysis,
        class: &str,
    ) -> Option<Self> {
        // The world must not move UNDER the mint. The closure is read from
        // the registered candidates and the fingerprints from the freshness
        // index — two reads of shared mutable state, and a registration
        // records a surface BEFORE it publishes the candidate. Straddle one
        // and this mints NEW fingerprints against an OLD closure: every
        // recorded pair is current, so it validates forever over an ancestry
        // it never enumerated. The epoch is the same counter the resolution
        // memo rides; moving it means decline, not repair.
        let epoch_before = idx.resolution_epoch();

        let mut names: Vec<String> = Vec::new();
        let truncated = origin.for_each_ancestor_class_reporting_truncation(class, Some(idx), |a| {
            names.push(a.to_string());
            if names.len() > MAX_CLOSURE {
                std::ops::ControlFlow::Break(())
            } else {
                std::ops::ControlFlow::Continue(())
            }
        });
        if names.is_empty() || names.len() > MAX_CLOSURE {
            crate::util::ghost_stats::count("closed.decline_closure_size");
            return None;
        }
        // Width declines above; depth would otherwise pass silently. A walk
        // cut off by the graph bound saw a PREFIX of the ancestry, and the
        // names below the cut are absent from the closure — so their
        // providers can change without anything here noticing.
        if truncated {
            crate::util::ghost_stats::count("closed.decline_truncated_walk");
            return None;
        }
        names.sort();
        names.dedup();

        let mut closure = Vec::with_capacity(names.len());
        for name in names {
            let providers = Self::providers_of(idx, &name)?;
            closure.push((name, providers));
        }
        closure.sort_by(|a, b| a.0.cmp(&b.0));

        // Both reads have happened; if anything registered across them the
        // pair may be incoherent. Declining costs the caller nothing — it is
        // already bound for the decode.
        if idx.resolution_epoch() != epoch_before {
            crate::util::ghost_stats::count("closed.decline_epoch_moved");
            return None;
        }
        Some(ClosednessCertificate { closure })
    }

    /// The evidence for one name: its provider set, each with the fingerprint
    /// the index currently records. `None` if any provider cannot be vouched
    /// for, which makes the whole certificate decline.
    ///
    /// Every exclusion lives HERE, in the one path `mint` and `is_valid` both
    /// take, for the same reason the validity key is one structure: an
    /// exclusion checked only at mint holds forever on a world that has since
    /// moved. A bridge arriving after minting moves no provider and no
    /// fingerprint, so nothing in the recorded key can see it — only asking
    /// again can.
    fn providers_of(
        idx: &dyn CrossFileLookup,
        name: &str,
    ) -> Option<Vec<(PathBuf, u64)>> {
        // Exclusions ride the VALUE, not the name (rule #10): we ask each
        // class whether it can be vouched for rather than matching a list of
        // classes we happen to know about.
        if idx.class_is_bridged_to(name) {
            // A plugin namespace can bridge content onto this class without
            // any file declaring it, so the ancestry walk does not see the
            // whole world. v1 declines rather than guessing; whether
            // bridge-set identity can join the key is a measured follow-up,
            // not an assumption.
            crate::util::ghost_stats::count("closed.decline_bridged");
            return None;
        }
        let cands: Arc<Vec<Arc<CachedModule>>> =
            super::session::visible_def_candidates(idx, name);
        let mut out = Vec::with_capacity(cands.len());
        for c in cands.iter() {
            // A declared-dynamic parent list means the ancestry walk cannot
            // know the closure at all — no fingerprint would make that safe.
            if c.analysis.has_dynamic_parents(name) {
                crate::util::ghost_stats::count("closed.decline_dynamic_parents");
                return None;
            }
            // No freshness record means the index cannot vouch for this
            // provider. Same fail-open direction as a conclusions row on an
            // unrecorded path: decline, never trust.
            let fp = idx.surface_fingerprint_of(&c.path)?;
            out.push((c.path.clone(), fp));
        }
        out.sort();
        Some(out)
    }

    /// Does this certificate still describe the world?
    ///
    /// O(closure) lookups, and within a walk they are close to free: the
    /// provider sets come from `ResolutionSession`'s per-walk memo, which the
    /// ancestry walk that minted this certificate populated for the same
    /// classes.
    ///
    /// Any disagreement — a provider arriving, leaving, or moving — reads as
    /// NOT closed. There is no partial validity and no repair here; the
    /// caller decodes and re-mints, which is the state it was in before this
    /// lane existed.
    pub fn is_valid(&self, idx: &dyn CrossFileLookup) -> bool {
        self.closure.iter().all(|(name, recorded)| {
            Self::providers_of(idx, name).is_some_and(|now| &now == recorded)
        })
    }

    /// Byte cost, for the store's accounting. A derived cache that is not
    /// byte-accounted is a memory regression no functional test can see.
    pub fn heap_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self
                .closure
                .iter()
                .map(|(n, ps)| {
                    n.len()
                        + std::mem::size_of::<(String, Vec<(PathBuf, u64)>)>()
                        + ps.iter()
                            .map(|(p, _)| {
                                p.as_os_str().len() + std::mem::size_of::<(PathBuf, u64)>()
                            })
                            .sum::<usize>()
                })
                .sum::<usize>()
    }

    /// A certificate over a single name with no providers — for tests that
    /// exercise the STORE (sizing, eviction, round trip) rather than
    /// validation, which needs a world.
    #[cfg(test)]
    pub fn empty_for_test(name: &str) -> Self {
        ClosednessCertificate { closure: vec![(name.to_string(), Vec::new())] }
    }

    /// How many ancestor names this certificate covers.
    #[cfg(test)]
    pub fn closure_len(&self) -> usize {
        self.closure.len()
    }
}

/// Is `class`'s ancestry closed, as far as the live index can prove right now?
///
/// The consult-side entry point: cached certificate if there is one, minted
/// lazily otherwise — this caller is bound for a decode, so it has already
/// paid the ancestry walk a mint needs.
///
/// **Every path out of here that is not a validated certificate returns
/// false**, which means "not closed", which means the decode the caller was
/// already going to do. There is no arm where uncertainty becomes trust.
pub fn class_is_closed(
    idx: &dyn CrossFileLookup,
    origin: &FileAnalysis,
    class: &str,
) -> bool {
    if closedness_disabled() {
        return false;
    }
    if let Some(cert) = idx.closedness_certificate(class) {
        if cert.is_valid(idx) {
            crate::util::ghost_stats::count("closed.cert_valid");
            return true;
        }
        // The world moved under it. Fall through to a re-mint rather than
        // just dropping it: the walk is the same one this consult was about
        // to pay for, so re-minting here costs nothing extra and the next
        // consult of this class gets a hit instead of another miss.
        crate::util::ghost_stats::count("closed.cert_invalid");
    }
    match ClosednessCertificate::mint(idx, origin, class) {
        Some(cert) => {
            crate::util::ghost_stats::count("closed.certified");
            idx.store_closedness_certificate(class, Arc::new(cert));
            true
        }
        None => {
            crate::util::ghost_stats::count("closed.uncertifiable");
            false
        }
    }
}

/// Run the decode anyway on every trusted absence and score the disagreement.
///
/// Same discipline as `PERL_LSP_CONCL_EQUIV` one tier down: a fast path whose
/// justification is empirical ships WITH the means to re-check it. This one
/// ablates the READ — the certificate lane has a single consumer, so unlike
/// `NO_BAKE` there is no second-producer hole for it to miss.
pub fn verify_closedness() -> bool {
    static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("PERL_LSP_CLOSED_EQUIV").is_ok())
}

/// Escape hatch and A/B control: turn trusted absence off without a rebuild.
///
/// `is_ok()` semantics, stated because the tally on #120 caught
/// `MINT_LINKS` reading `=0` as ON: **any value, including `0`, disables
/// trusted absence.** Presence is the signal.
pub fn closedness_disabled() -> bool {
    static OFF: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *OFF.get_or_init(|| std::env::var("PERL_LSP_NO_CLOSED").is_ok())
}

#[cfg(test)]
#[path = "closedness_tests.rs"]
mod tests;
