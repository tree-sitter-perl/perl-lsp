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
/// deliberately-accounted ones (writer fallbacks, degraded/unpersisted
/// analyses) mean a registration path is silently pinning whole analyses —
/// the RAM regression no functional test can see. `debug_assert` catches it
/// in `cargo test`; strict mode makes it fatal in release (the gold net).
pub(super) fn residency_tripwire(tier: &str, whole: usize, expected: usize) {
    if whole <= expected {
        return;
    }
    log::error!(
        "residency tripwire ({tier}): {whole} fully-resident copies, only \
         {expected} accounted (writer fallbacks / degraded) — a registration \
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

/// Persist one module's generation: blob + its relational ref rows, always
/// together (`docs/adr/relational-ref-index.md` — rows and blob describe the
/// same analysis or neither exists). `save_to_db` skips degraded analyses;
/// mirror that here so no rows exist for an unpersisted blob.
/// Returns whether the blob row landed (the strip-legality signal).
/// Persist EVERY provider of `module_name` — one row per file, because the
/// name maps to a set. `true` only when all of them landed: a partial write
/// must not license the resident strip, or the un-persisted provider loses
/// the axes it can no longer rehydrate.
pub(super) fn save_module_generation(
    conn: &rusqlite::Connection,
    module_name: &str,
    result: &Option<Providers>,
) -> bool {
    let Some(providers) = result else {
        return save_one_provider(conn, module_name, &None);
    };
    let mut all = true;
    for m in providers {
        all &= save_one_provider(conn, module_name, &Some(m.clone()));
    }
    all
}

fn save_one_provider(
    conn: &rusqlite::Connection,
    module_name: &str,
    result: &Option<Arc<CachedModule>>,
) -> bool {
    if let Some(m) = result {
        // A bag-evicted copy IS the already-persisted generation —
        // re-encoding it would overwrite the good blob with a bagless one.
        if m.analysis.bag_is_evicted() {
            return true;
        }
    }
    let persisted = module_cache::save_to_db(conn, module_name, result, module_cache::NAME_KEYED_SOURCE);
    if !persisted {
        // Blob didn't land (busy/encode failure): shredding rows now would
        // pair a NEW generation's rows with an OLD (or absent) blob —
        // "blob + rows describe one generation" is the write invariant.
        return false;
    }
    if let Some(m) = result {
        if !m.analysis.degraded {
            let seeds: Vec<_> = m.analysis.ref_row_seeds();
            let sym_seeds = m.analysis.sym_row_seeds();
            if let Err(e) = module_cache::shred_derived_rows(
                conn,
                &m.path.to_string_lossy(),
                module_cache::NAME_KEYED_SOURCE,
                &seeds,
                &sym_seeds,
            ) {
                log::warn!("Failed to shred derived rows for '{}': {}", module_name, e);
            }
        }
    }
    persisted
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

/// Byte budget for whole `FileAnalysis` copies the persist writer retains
/// when a chunk fails to commit (disk full) or panics. The strip is licensed
/// only by a landed blob, so a fallback keeps copies WHOLE — and a
/// persistently failing writer would otherwise pin the ENTIRE tree resident
/// (the docs/forks-resolved.md "writer fallback budget" entry). Past the cap
/// we DROP the resident copy rather than register a stripped one: the chunk
/// didn't commit, so a stripped copy's blob isn't on disk and could only
/// rehydrate to wrong-empty. Dropping is honest absence — the file reads as
/// "not indexed this session" and the next index/warm re-registers it; it
/// never serves wrong data and never leaves an evicted copy unrehydratable
/// (nothing is evicted — nothing is registered). Byte-accounted like the
/// enrichment overlay (`ENRICHED_BYTE_CAP`); 128 MiB per writer thread — a
/// transient failure degrades gracefully, a permanent one can't OOM.
pub(super) const FALLBACK_WHOLE_BYTE_CAP: usize = 128 * 1024 * 1024;

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
/// `FileStore` / bag-cache maps that no producer holds across a send. That
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
        // Park TIME, not just count: whether backpressure costs wall depends
        // on how long producers actually sleep, which the count alone can't
        // attribute.
        crate::util::ghost_stats::timed("persist_queue.park_wait", || {
            std::thread::sleep(QUEUE_PARK)
        });
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
/// every entry to exactly one of `on_committed` (deferred registration) or
/// `on_fallback` (commit failure OR chunk panic — the whole-copy self-heal;
/// a panic must not kill the writer, workers keep stripping copies whose
/// sends would silently fail). With no Connection the channel drains
/// unregistered. Registration runs inside the panic guard, mirroring the
/// txn: entries a mid-batch registration panic leaves behind take the
/// fallback lane instead of vanishing.
pub(crate) fn run_persist_writer<E>(
    rx: std::sync::mpsc::Receiver<E>,
    conn: Option<&rusqlite::Connection>,
    label: &str,
    write_batch: impl Fn(&rusqlite::Connection, &[E]),
    mut on_committed: impl FnMut(E),
    mut on_fallback: impl FnMut(E),
) {
    let Some(conn) = conn else {
        while rx.recv().is_ok() {}
        return;
    };
    let mut batch: Vec<E> = Vec::new();
    let mut process = |batch: &mut Vec<E>| {
        if batch.is_empty() {
            return;
        }
        let n = batch.len();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let txn_open = conn.execute_batch("BEGIN IMMEDIATE").is_ok();
            crate::util::ghost_stats::timed("writer.write_batch", || {
                write_batch(conn, batch)
            });
            let committed = txn_open
                && match crate::util::ghost_stats::timed("writer.commit", || {
                    conn.execute_batch("COMMIT")
                }) {
                    Ok(()) => true,
                    Err(err) => {
                        let _ = conn.execute_batch("ROLLBACK");
                        log::error!(
                            "{label}: commit failed ({n} files, registering whole copies): {err}"
                        );
                        false
                    }
                };
            if committed {
                for e in batch.drain(..) {
                    crate::util::ghost_stats::timed("writer.on_committed", || {
                        on_committed(e)
                    });
                }
            } else {
                for e in batch.drain(..) {
                    on_fallback(e);
                }
            }
        }));
        if r.is_err() {
            // A panic can leave the txn open; roll back defensively so the
            // NEXT chunk's BEGIN isn't poisoned.
            let _ = conn.execute_batch("ROLLBACK");
            log::error!("{label}: chunk panicked ({n} files) — registering whole copies");
            for e in batch.drain(..) {
                on_fallback(e);
            }
        }
    };
    // `writer.recv_idle` separates "writer starved" (walk is the bottleneck)
    // from "writer saturated" (its work throttles the walk via the bound).
    while let Ok(entry) = crate::util::ghost_stats::timed("writer.recv_idle", || rx.recv()) {
        batch.push(entry);
        while batch.len() < PERSIST_CHUNK {
            match rx.try_recv() {
                Ok(e) => batch.push(e),
                Err(_) => break,
            }
        }
        process(&mut batch);
    }
    process(&mut batch);
}
