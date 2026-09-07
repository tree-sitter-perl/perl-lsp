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

/// Physical RAM in bytes. POSIX `sysconf` on unix (Linux and mac alike),
/// `GlobalMemoryStatusEx` on Windows — kernel32 is linked by every Windows
/// target, so no crate. `None` only on a platform with neither, or when the
/// call itself fails; the ceiling is then skipped, never guessed.
pub fn physical_ram_bytes() -> Option<u64> {
    #[cfg(unix)]
    {
        // SAFETY: `sysconf` reads a constant and touches nothing else.
        let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
        let page = unsafe { libc::sysconf(libc::_SC_PAGE_SIZE) };
        if pages <= 0 || page <= 0 {
            return None;
        }
        return (pages as u64).checked_mul(page as u64);
    }
    #[cfg(windows)]
    {
        // MEMORYSTATUSEX, field for field.
        #[repr(C)]
        struct MemoryStatusEx {
            length: u32,
            memory_load: u32,
            total_phys: u64,
            avail_phys: u64,
            total_page_file: u64,
            avail_page_file: u64,
            total_virtual: u64,
            avail_virtual: u64,
            avail_extended_virtual: u64,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GlobalMemoryStatusEx(buffer: *mut MemoryStatusEx) -> i32;
        }
        let mut status = MemoryStatusEx {
            length: std::mem::size_of::<MemoryStatusEx>() as u32,
            memory_load: 0,
            total_phys: 0,
            avail_phys: 0,
            total_page_file: 0,
            avail_page_file: 0,
            total_virtual: 0,
            avail_virtual: 0,
            avail_extended_virtual: 0,
        };
        // SAFETY: the struct is `repr(C)` in the documented layout with
        // `length` set, which is the API's whole contract.
        let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
        return (ok != 0).then_some(status.total_phys);
    }
    #[allow(unreachable_code)]
    None
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

    /// The ceiling must not vanish silently on a shipped platform: a
    /// `None` here would make the bound test below pass empty.
    #[test]
    fn physical_ram_is_readable_on_shipped_platforms() {
        if cfg!(any(unix, windows)) {
            assert!(physical_ram_bytes().is_some_and(|b| b > 0));
        }
    }

    #[test]
    fn sweep_cap_never_exceeds_half_of_ram() {
        if let Some(ram) = physical_ram_bytes() {
            let cap = sweep_bag_cache_cap(u64::MAX / 100, 0);
            assert!(cap as u64 <= ram / RAM_CEILING_SHARE);
        }
    }
}
