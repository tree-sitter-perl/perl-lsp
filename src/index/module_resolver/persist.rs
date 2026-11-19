//! Residency policy (eviction switch, strict mode, the tripwire) and
//! persistence: per-module generation save, the stamp-guarded analyze
//! protocol, and the shared batched persist-writer harness.

use super::*;

/// Slice-2 eviction off-switch: `PERL_LSP_NO_EVICT` keeps every resident pack
/// bag in memory (the pre-Slice-2 footprint) — an emergency knob and the A/B
/// lever for isolating an eviction-caused regression.
pub(crate) fn eviction_enabled() -> bool {
    std::env::var_os("PERL_LSP_NO_EVICT").is_none()
}

/// `PERL_LSP_STRICT_RESIDENCY=1`: residency invariant breaks (an evicted
/// copy that can't rehydrate, a tripwire overrun) PANIC instead of
/// degrading. The gold harness sets it so a session serving
/// absence-as-answer dies as a CRASH row (hard fail) rather than scoring
/// wrong answers — the cold-flake net. Off by default: a live server
/// prefers degraded-but-useful.
pub(crate) fn strict_residency() -> bool {
    std::env::var_os("PERL_LSP_STRICT_RESIDENCY").is_some_and(|v| v != "0")
}

/// The post-bulk-index residency check, one speller for the pack tier and
/// the Perl workspace tier: fully-resident registered copies beyond the
/// deliberately-accounted ones (degraded/unpersisted analyses) mean a
/// registration path is silently pinning whole analyses — the RAM
/// regression no functional test can see. `debug_assert` catches it
/// in `cargo test`; strict mode makes it fatal in release (the gold net).
pub(super) fn residency_tripwire(tier: &str, whole: usize, expected: usize) {
    if whole <= expected {
        return;
    }
    log::error!(
        "residency tripwire ({tier}): {whole} fully-resident copies, only \
         {expected} accounted (degraded / unpersisted) — a registration \
         path is pinning whole analyses"
    );
    debug_assert!(
        false,
        "residency tripwire ({tier}): {whole} fully-resident > {expected} accounted"
    );
    if strict_residency() {
        panic!(
            "PERL_LSP_STRICT_RESIDENCY: residency tripwire ({tier}): {whole} \
             fully-resident copies, only {expected} accounted"
        );
    }
}

/// Persist one dependency file, path-keyed. The verdict is `true` when a
/// row that can rehydrate it is on disk — already current (the landing took
/// it from that row) or written now — and that verdict is the strip
/// licence. The surface is the one that row describes: what the encode
/// projected, or the row's persisted projection when it was current (a warm
/// landing, recorded as the workspace warm lane records its stub's). `None`
/// when neither is to hand.
pub(super) fn persist_dependency(
    conn: &rusqlite::Connection,
    m: &Arc<CachedModule>,
) -> Result<(bool, Option<crate::model::surface::Surface>), module_cache::PersistFailure> {
    if let Some(stamp) = module_cache::file_stamp(&m.path) {
        if module_cache::dependency_row_is_current(conn, &m.path, stamp) {
            let surface = crate::util::ghost_stats::timed("resolver.load_surface", || {
                module_cache::load_surface(conn, &m.path.to_string_lossy())
            });
            return Ok((true, surface));
        }
    }
    module_cache::save_to_db_projected(conn, &m.path.to_string_lossy(), m, module_cache::DEPENDENCY_SOURCE)
}

/// Stamp-before-read + re-stat-after-parse: capture the disk stamp, run the
/// read+analyze, and return None when the file changed underneath — a
/// write-time stamp would bless a stale parse as the current generation and
/// every future warm would serve it as valid. Both fresh workers route
/// their changed-under-us protocol through here.
pub(super) fn analyze_stamped<T>(
    path: &std::path::Path,
    f: impl FnOnce() -> Option<T>,
) -> Option<(T, (i64, i64))> {
    let stamp = module_cache::file_stamp(path).unwrap_or((0, 0));
    let out = f()?;
    if module_cache::file_stamp(path) != Some(stamp) {
        return None;
    }
    Some((out, stamp))
}

/// Entries per persist transaction. The writer fills a batch with
/// `try_recv` after its first blocking `recv`, so this is also the floor the
/// bounded channel must clear for a chunk to be fillable without stalling.
pub(crate) const PERSIST_CHUNK: usize = 128;

/// Default depth of the bounded persist channel, in ENTRIES. A cold walk on
/// 20 cores outruns SQLite's single writer roughly 4:1, so an unbounded
/// channel parks about four fifths of the corpus in RAM — the cold-index
/// spike. The cap is what makes peak in-flight a property of the design
/// rather than of the corpus size: ~22 KB/entry at 138k Perl files puts this
/// default near 44 MB.
///
/// It does NOT shorten time-to-ready. The writer is on the critical path for
/// the whole run, so throttling the walk to writer rate finishes at about the
/// same wall clock; what changes is the peak and the honesty of the progress
/// bar. Only reducing the writer's WORK moves time-to-ready.
const DEFAULT_WRITE_QUEUE_DEPTH: usize = 2048;

/// `PERL_LSP_WRITE_QUEUE_DEPTH` overrides the default so the cap can be
/// measured against a real corpus. Floored at one chunk: a channel shallower
/// than a transaction would serialize the walk against every commit.
pub(crate) fn write_queue_depth() -> usize {
    std::env::var("PERL_LSP_WRITE_QUEUE_DEPTH")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(DEFAULT_WRITE_QUEUE_DEPTH)
        .max(PERSIST_CHUNK)
}

/// The bounded channel every bulk indexer hands to `run_persist_writer`.
pub(crate) fn bounded_persist_channel<E>() -> (
    std::sync::mpsc::SyncSender<E>,
    std::sync::mpsc::Receiver<E>,
) {
    std::sync::mpsc::sync_channel(write_queue_depth())
}

/// How long a full-queue producer sleeps between attempts. Matched to the
/// writer's drain rate (a slot frees every few ms at measured throughput), so
/// the poll is cheap without adding meaningful latency to the walk.
const QUEUE_PARK: std::time::Duration = std::time::Duration::from_millis(5);

/// A producer stalled this long has stopped being backpressure and started
/// being a symptom; say so once rather than stalling mutely.
const QUEUE_STALL_WARN: std::time::Duration = std::time::Duration::from_secs(30);

/// Hand one entry to the persist writer, parking while the queue is full.
///
/// `try_send` + park rather than a blocking `send` so a stall is observable
/// instead of silent, and so the disconnected case is handled explicitly
/// rather than by whatever `send` happens to do.
///
/// **The deadlock this must not have**: a producer parked here while holding
/// a lock the writer's `on_committed` needs would never be woken, because
/// the writer cannot drain to free a slot. Every bulk send site therefore
/// holds NO index or store guard at its call to this function — the
/// registration tokens (`prepare_*_parts`) and surfaces are fully owned
/// values by then, and the writer's lanes touch only `ModuleIndex` /
/// `OpenDocs` / bag-cache maps that no producer holds across a send. That
/// property is what makes the bound safe; it is not incidental, and a new
/// send site has to preserve it — `filestore-guard-discipline`, the family
/// that has already produced two deadlocks here.
pub(crate) fn send_to_writer<E>(tx: &std::sync::mpsc::SyncSender<E>, entry: E) {
    use std::sync::mpsc::TrySendError;
    let mut entry = entry;
    let mut waited = std::time::Duration::ZERO;
    let mut warned = false;
    loop {
        match tx.try_send(entry) {
            Ok(()) => return,
            // The writer drains until every sender drops, so it never
            // disconnects while work remains — this is the writer-thread-died
            // case. Parking would hang the walk; dropping leaves the file
            // unindexed, which the next run repairs.
            Err(TrySendError::Disconnected(_)) => {
                log::error!("persist writer is gone; dropping a queued entry");
                return;
            }
            Err(TrySendError::Full(returned)) => entry = returned,
        }
        // One per park INTERVAL, so `count * QUEUE_PARK` is roughly aggregate
        // producer stall. Whether backpressure ever engaged is not inferable
        // from wall time — a run where the walk never outran the writer looks
        // identical to one where the depth did nothing — and a drain number
        // from a run with zero parks says nothing about the depth.
        crate::util::ghost_stats::count("persist_queue.producer_parked");
        std::thread::sleep(QUEUE_PARK);
        waited = waited.saturating_add(QUEUE_PARK);
        if !warned && waited >= QUEUE_STALL_WARN {
            warned = true;
            log::warn!(
                "persist queue full for {}s — the walk is throttled to writer rate",
                waited.as_secs()
            );
        }
    }
}

/// The persist-writer harness every persist site shares: batches entries off
/// the channel (≤128 per txn), owns BEGIN IMMEDIATE / COMMIT / ROLLBACK
/// (IMMEDIATE — a deferred txn that reads before writing can hit an
/// unretryable SQLITE_BUSY_SNAPSHOT against a concurrent writer), and hands
/// each entry of a chunk that committed to `on_committed` (deferred
/// registration, the strip) with `write_entry`'s verdict: `Ok(false)` is an
/// entry with nothing to write, never a write that failed. The first write,
/// BEGIN or COMMIT the store refuses rolls the chunk back and ends the
/// writer with that `PersistFailure`, which its caller makes fatal; so does
/// a chunk that panicked, since its entries can no longer be accounted for.
/// Registration runs inside the panic guard, mirroring the txn. With no
/// Connection the channel drains unregistered. `begin_chunk` runs once per
/// chunk inside its transaction and hands `write_entry` whatever
/// chunk-wide state it reads (the stubs-table version).
pub(crate) fn run_persist_writer<E, C>(
    rx: std::sync::mpsc::Receiver<E>,
    conn: Option<&rusqlite::Connection>,
    label: &str,
    begin_chunk: impl Fn(&rusqlite::Connection) -> C,
    write_entry: impl Fn(&rusqlite::Connection, &C, &E) -> Result<bool, module_cache::PersistFailure>,
    mut on_committed: impl FnMut(E, bool),
) -> Result<(), module_cache::PersistFailure> {
    use module_cache::PersistFailure;
    let Some(conn) = conn else {
        while rx.recv().is_ok() {}
        return Ok(());
    };
    let mut batch: Vec<E> = Vec::new();
    let mut process = |batch: &mut Vec<E>| -> Result<(), PersistFailure> {
        if batch.is_empty() {
            return Ok(());
        }
        let n = batch.len();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            conn.execute_batch(module_cache::sql::TXN_BEGIN_IMMEDIATE.registered())
                .map_err(|err| PersistFailure::new(conn, format_args!("{label}: BEGIN: {err}")))?;
            let chunk = begin_chunk(conn);
            let written: Result<Vec<bool>, PersistFailure> =
                batch.iter().map(|e| write_entry(conn, &chunk, e)).collect();
            let committed = written.and_then(|w| {
                conn.execute_batch(module_cache::sql::TXN_COMMIT.registered())
                    .map(|()| w)
                    .map_err(|err| PersistFailure::new(conn, format_args!("{label}: COMMIT: {err}")))
            });
            let written = match committed {
                Ok(w) => w,
                Err(failure) => {
                    let _ = conn.execute_batch(module_cache::sql::TXN_ROLLBACK.registered());
                    return Err(failure);
                }
            };
            for (e, wrote) in batch.drain(..).zip(written) {
                on_committed(e, wrote);
            }
            Ok(())
        }));
        r.unwrap_or_else(|panic| {
            let _ = conn.execute_batch(module_cache::sql::TXN_ROLLBACK.registered());
            let why = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            Err(PersistFailure::new(conn, format_args!("{label}: a chunk of {n} files panicked: {why}")))
        })
    };
    while let Ok(entry) = rx.recv() {
        batch.push(entry);
        while batch.len() < PERSIST_CHUNK {
            match rx.try_recv() {
                Ok(e) => batch.push(e),
                Err(_) => break,
            }
        }
        process(&mut batch)?;
    }
    process(&mut batch)
}
