//! A self-imposed RSS / wall cap that exits through the front door.
//!
//! An externally-imposed cap (`ulimit -v`, `SIGKILL`) stops the process but
//! throws away everything it learned: the allocation fails, the runtime
//! aborts, and the instrumentation sinks never run. For a measurement harness
//! that is the worst outcome — the pathological corpus is exactly the one
//! whose counters you wanted, and a killed run leaves no evidence of why.
//!
//! So the process caps ITSELF. A poller watches `/proc/self/status` and the
//! clock; on breach it writes the JSON sinks and exits with a distinct code,
//! so a capped run lands in the data as a capped run with partial numbers
//! rather than as an absence.
//!
//! The same poller also carries the SIGTERM path. A handler cannot safely
//! write files itself — almost nothing is async-signal-safe — so it sets a
//! flag and the poller does the work. That is why the two live together:
//! external stop and self-imposed stop want the identical exit, and one
//! owner means they cannot drift.
//!
//! SIGKILL is deliberately NOT handled, because it cannot be: it is
//! uncatchable by design. Anything sending it takes the instrumentation with
//! it, so a harness should send SIGTERM first and escalate only if the
//! process fails to leave.
//!
//! Keep an external hard limit well ABOVE the soft cap. The watchdog protects
//! the measurement; only the kernel can protect the host against a single
//! allocation that outruns a 250ms poll.

/// Exit codes chosen to not collide with `timeout` (124) or a signal death
/// (128+n), so the harness can tell "I stopped myself" from "something else
/// stopped me".
pub const EXIT_RSS_CAP: i32 = 90;
pub const EXIT_TIME_CAP: i32 = 91;
/// Distinct from 143 (128+SIGTERM): 143 means "died on the signal", this
/// means "left cleanly BECAUSE of the signal, with its instrumentation
/// written". A harness needs to tell those apart.
pub const EXIT_SIGTERM: i32 = 92;

static TERM: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The only thing a signal handler here does. Setting an `AtomicBool` is
/// async-signal-safe; writing files, taking locks, or allocating is not, and
/// the poller 250ms later is a fine place to do the unsafe parts.
extern "C" fn on_term(_sig: i32) {
    TERM.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn install_term_handler() {
    // SAFETY: `on_term` only stores to an AtomicBool, which is
    // async-signal-safe. SIGTERM's default is termination, so replacing it
    // cannot lose a stop — it only makes the stop graceful.
    unsafe {
        libc::signal(libc::SIGTERM, on_term as extern "C" fn(libc::c_int) as libc::sighandler_t);
    }
}

fn rss_kb() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/status").ok()?;
    s.lines()
        .find_map(|l| l.strip_prefix("VmRSS:"))
        .and_then(|v| v.split_whitespace().next()?.parse().ok())
}

/// Arm the caps named by `PERL_LSP_MAX_RSS_MB` / `PERL_LSP_MAX_SECONDS`.
///
/// No-op when neither is set, so a normal run pays one env read and nothing
/// else. Idempotent in effect: arming twice just costs a second poller.
pub fn arm() {
    let max_rss_mb: Option<u64> = std::env::var("PERL_LSP_MAX_RSS_MB")
        .ok()
        .and_then(|v| v.parse().ok());
    let max_secs: Option<u64> = std::env::var("PERL_LSP_MAX_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok());

    // The SIGTERM path arms unconditionally: an external stop should always
    // flush what was learned, whether or not a self-cap was asked for.
    install_term_handler();

    let start = std::time::Instant::now();
    std::thread::Builder::new()
        .name("perl-lsp-watchdog".into())
        .spawn(move || loop {
            if TERM.load(std::sync::atomic::Ordering::SeqCst) {
                trip(EXIT_SIGTERM, "SIGTERM received");
            }
            if let Some(cap) = max_secs {
                if start.elapsed().as_secs() >= cap {
                    trip(EXIT_TIME_CAP, &format!("wall cap {cap}s reached"));
                }
            }
            if let (Some(cap), Some(now)) = (max_rss_mb, rss_kb()) {
                if now / 1024 >= cap {
                    trip(
                        EXIT_RSS_CAP,
                        &format!("RSS cap {cap} MB reached ({} MB)", now / 1024),
                    );
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        })
        .ok();
}

/// Flush what we learned, say why, and leave.
///
/// `process::exit` rather than a panic: a panic unwinds worker threads mid-
/// analysis and can deadlock on a lock the poller does not hold, and the
/// sinks are already written by then anyway.
fn trip(code: i32, why: &str) -> ! {
    eprintln!("[watchdog] {why} — writing instrumentation and exiting {code}");
    super::ghost_stats::write_json();
    super::timings::write_json();
    std::process::exit(code);
}
