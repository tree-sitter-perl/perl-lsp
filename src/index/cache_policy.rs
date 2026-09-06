//! Cache sizing derivations — the one home for "how big should this pool
//! be", so each pool's scale signal is named in one place and none of them
//! share a summed budget (independent caps fail independently; a shared
//! budget forces an exchange rate between incommensurable miss costs).
//!
//! Sizing adapts to the PROJECT baseline in bytes, never to box RAM: the
//! project only grows within a session, so derived caps are monotone and the
//! shrink-below-one-entry collapse never arises. RAM is a ceiling here, not
//! a signal. The stock caps stay as floors.

/// Resident footprint of a decoded analysis per byte of its source, the
/// a-priori estimator the `--check` sweep's admission budget also relies on
/// (measured on the estimator probe; whole copies, bag included — a
/// rows-lane copy is roughly half, so a cap derived from this is a ceiling
/// with headroom, which is the safe direction for a cap).
pub const SOURCE_TO_ANALYSIS_EXPANSION: u64 = 65;

/// Share of physical RAM a single derived cap may reach. Per POOL: the hub
/// and each pack sub-index derive independently (no summed budget, by
/// design), so on a mixed-language corpus the ceilings can sum past RAM —
/// the ceiling bounds one runaway derivation, it is not a global budget.
const RAM_CEILING_SHARE: u64 = 2;

/// Physical RAM in bytes, from `/proc/meminfo`; `None` where unreadable.
pub fn physical_ram_bytes() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kb * 1024)
}

/// The rehydration-LRU cap for a BATCH SWEEP over the whole index (one
/// backward walk per declaration — `--heatmap`): the sweep's working set is
/// the corpus by construction, and an LRU smaller than a cyclically scanned
/// working set evicts every entry just before its next use (measured on BMO:
/// 274 MB working set under the stock 128 MiB — a 12% miss rate that cost
/// 3× the wall AND doubled peak RSS through decode churn). Derived from the
/// persisted source bytes × the expansion estimator, floored at `stock`
/// (never shrinks), ceilinged at half of physical RAM.
pub fn sweep_bag_cache_cap(persisted_source_bytes: u64, stock: usize) -> usize {
    let derived = persisted_source_bytes.saturating_mul(SOURCE_TO_ANALYSIS_EXPANSION);
    let ceiling = physical_ram_bytes()
        .map(|ram| ram / RAM_CEILING_SHARE)
        .unwrap_or(u64::MAX);
    let capped = derived.min(ceiling);
    usize::try_from(capped).unwrap_or(usize::MAX).max(stock)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_cap_floors_at_stock_and_scales_with_source_bytes() {
        let stock = 128 * 1024 * 1024;
        assert_eq!(sweep_bag_cache_cap(0, stock), stock);
        assert_eq!(sweep_bag_cache_cap(1, stock), stock);
        // 10 MB of source (BMO-sized) derives well above the stock floor.
        let ten_mb = 10 * 1024 * 1024;
        let cap = sweep_bag_cache_cap(ten_mb, stock);
        assert!(cap > stock);
        assert!(cap as u64 <= ten_mb * SOURCE_TO_ANALYSIS_EXPANSION);
    }

    #[test]
    fn sweep_cap_never_exceeds_half_of_ram() {
        if let Some(ram) = physical_ram_bytes() {
            let cap = sweep_bag_cache_cap(u64::MAX / 100, 0);
            assert!(cap as u64 <= ram / RAM_CEILING_SHARE);
        }
    }
}
