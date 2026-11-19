//! A resolver batch's reads and parses, run ahead of its landings in the
//! global Rayon pool. Only the parse moves: the landings stay on the
//! resolver thread in ask order, each taking its file from here or parsing
//! it itself, so what lands, in what order, and what the memo records are
//! the serial landing's whichever parse finishes first.

use super::*;
use crate::build::dependency_map::{Ask, DependencyMap};
use crate::build::language_driver::LanguageDriver;
use crate::util::canonical_path::CanonicalPath;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Condvar, Mutex};

/// The parses a batch runs ahead of its landing cursor.
///
/// Whole analyses wait here until their landing persists and strips them,
/// so the wait is a derived-copy holding area and is byte-capped: a parse
/// is started only while the finished-but-unlanded analyses weigh less
/// than the cap (`cache_policy::prepare_ahead_cap`) and fewer parses than
/// the pool has workers are running. A full budget pauses the parse-ahead
/// until a landing takes a file; nothing is dropped. The cap bounds what
/// waits, so the peak is the cap plus the analyses still being built, one
/// per worker at most.
#[derive(Default)]
pub(crate) struct Prepared {
    /// Not started yet, in the order the landings will ask for them.
    queue: VecDeque<Job>,
    /// Started and not yet taken.
    started: HashSet<CanonicalPath>,
    /// Each job's place in landing order.
    order: HashMap<CanonicalPath, usize>,
    shared: Arc<Ahead>,
    cap: usize,
    workers: usize,
    source: Option<Source>,
}

struct Job {
    label: String,
    path: CanonicalPath,
}

/// What a worker reads and parses with.
#[derive(Clone)]
struct Source {
    driver: &'static dyn LanguageDriver,
    db_path: Option<std::path::PathBuf>,
    reader: Arc<module_cache::RetainedReader>,
}

#[derive(Default)]
struct Ahead {
    state: Mutex<AheadState>,
    finished: Condvar,
}

#[derive(Default)]
struct AheadState {
    /// Finished parses waiting for their landing, with their heap estimate.
    done: HashMap<CanonicalPath, (Option<Arc<CachedModule>>, usize)>,
    held: usize,
    peak: usize,
    running: usize,
}

/// The largest `held` any batch reached, for the ghost report: each batch
/// adds only what it raised the maximum by, so the counter reads as a max.
static HELD_PEAK: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl Prepared {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The jobs not yet started and the parses not yet taken.
    #[cfg(test)]
    pub(crate) fn outstanding(&self) -> usize {
        self.queue.len() + self.started.len()
    }

    /// The parse of `path` if this batch prepared it, waiting for it if it
    /// is still running; `None` for a file the caller parses itself. A job
    /// the parse-ahead has not reached is the caller's too: the cursor is
    /// at it, so parsing it here is what the pool would do next.
    pub(crate) fn take(&mut self, path: &CanonicalPath) -> Option<Option<Arc<CachedModule>>> {
        let at = *self.order.get(path)?;
        if let Some(i) = self.queue.iter().position(|j| &j.path == path) {
            self.queue.remove(i);
            self.order.remove(path);
            self.release_behind(at);
            self.start_more();
            return None;
        }
        if !self.started.remove(path) {
            return None;
        }
        self.order.remove(path);
        let done = crate::util::ghost_stats::timed("resolver.prepare_wait", || {
            let mut st = self.shared.state.lock().unwrap();
            loop {
                if let Some((done, bytes)) = st.done.remove(path) {
                    st.held -= bytes;
                    return done;
                }
                st = self.shared.finished.wait(st).unwrap();
            }
        });
        self.release_behind(at);
        self.start_more();
        Some(done)
    }

    /// Finished parses of jobs before the cursor that no landing took (an
    /// ask the batch settled first, a file another name landed): landings
    /// ask in job order, so none of them is asked for again, and each
    /// would hold budget the jobs ahead need.
    fn release_behind(&mut self, cursor: usize) {
        let behind: Vec<CanonicalPath> =
            self.order.iter().filter(|(_, &i)| i < cursor).map(|(p, _)| p.clone()).collect();
        if behind.is_empty() {
            return;
        }
        let mut st = self.shared.state.lock().unwrap();
        for path in behind {
            if let Some((_, bytes)) = st.done.remove(&path) {
                st.held -= bytes;
                self.started.remove(&path);
                self.order.remove(&path);
                crate::util::ghost_stats::count("resolver.prepare_released");
            }
        }
    }

    fn start_more(&mut self) {
        let Some(source) = &self.source else { return };
        loop {
            {
                let st = self.shared.state.lock().unwrap();
                if st.running >= self.workers || self.queue.is_empty() {
                    return;
                }
                if st.held >= self.cap {
                    crate::util::ghost_stats::count("resolver.prepare_paused");
                    return;
                }
            }
            let job = self.queue.pop_front().expect("checked non-empty");
            self.started.insert(job.path.clone());
            self.shared.state.lock().unwrap().running += 1;
            let (shared, source) = (Arc::clone(&self.shared), source.clone());
            rayon::spawn(move || {
                let done = crate::util::ghost_stats::timed("resolver.prepare", || source.parse(&job));
                let bytes = done.as_ref().map_or(0, |m| m.analysis.heap_estimate().total());
                let mut st = shared.state.lock().unwrap();
                st.running -= 1;
                st.held += bytes;
                st.peak = st.peak.max(st.held);
                st.done.insert(job.path, (done, bytes));
                shared.finished.notify_all();
            });
        }
    }
}

impl Source {
    fn parse(&self, job: &Job) -> Option<Arc<CachedModule>> {
        let mut parse = Parse::of(self.driver);
        let open = || self.db_path.as_deref().and_then(|p| module_cache::open_reader_retrying(p).ok());
        match self.db_path {
            Some(_) => self
                .reader
                .with(open, |conn| parse_one_provider(&job.label, job.path.clone(), &mut parse, Some(conn)))
                .unwrap_or_else(|| parse_one_provider(&job.label, job.path.clone(), &mut parse, None)),
            None => parse_one_provider(&job.label, job.path.clone(), &mut parse, None),
        }
    }
}

impl Drop for Prepared {
    /// A parse still running belongs to a job no landing took; it is
    /// waited out so its analysis is freed with the batch, not beside the
    /// next one.
    fn drop(&mut self) {
        let mut st = self.shared.state.lock().unwrap();
        while st.running > 0 {
            st = self.shared.finished.wait(st).unwrap();
        }
        let peak = st.peak;
        drop(st);
        if self.source.is_some() {
            crate::util::ghost_stats::timed("resolver.prepare_trim", release_freed_heap);
        }
        let prev = HELD_PEAK.fetch_max(peak, std::sync::atomic::Ordering::Relaxed);
        if peak > prev {
            crate::util::ghost_stats::count_by("resolver.prepare_held_peak_kib", ((peak - prev) / 1024) as u64);
        }
    }
}

/// The batch's analyses were built on the pool's threads and freed on this
/// one once landed, and glibc keeps each worker arena's freed pages
/// resident: on laravel-app that held 30 MB over the serial landing's peak,
/// against under 10 MB of analyses ever waiting at once. Handing them back
/// after the batch returns the peak to the serial landing's.
fn release_freed_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` only returns free memory to the kernel.
    unsafe {
        libc::malloc_trim(0);
    }
}

/// Start the parse-ahead for every file the batch's unsettled asks would
/// land that the tier does not hold yet. A held file is left to its
/// landing (a restamp, or a re-parse if it changed), and a batch of one
/// file is parsed where it lands.
#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_batch(
    core: &IndexCore,
    map: &DependencyMap,
    map_gen: u64,
    batch: &[Ask],
    driver: &'static dyn LanguageDriver,
    db_path: Option<&std::path::Path>,
    reader: &Arc<module_cache::RetainedReader>,
    stock_cap: usize,
) -> Prepared {
    let mut queue: VecDeque<Job> = VecDeque::new();
    let mut seen: HashSet<CanonicalPath> = HashSet::new();
    let mut job = |label: &str, path: CanonicalPath| {
        if core.records.role_of(&path).is_none() && seen.insert(path.clone()) {
            queue.push_back(Job { label: label.to_string(), path });
        }
    };
    for ask in batch {
        if core.asked.get(ask).is_some_and(|e| e.0 == map_gen) {
            continue;
        }
        match ask {
            Ask::File(path) => job(&path.to_string_lossy(), path.clone()),
            Ask::Name(name) => {
                for module_name in landing_names(core, map, name) {
                    if core.project_answers(&module_name) {
                        continue;
                    }
                    for path in located_as(map, &module_name) {
                        job(&module_name, path);
                    }
                }
            }
        }
    }
    if queue.len() < 2 {
        return Prepared::new();
    }
    crate::util::ghost_stats::count_by("resolver.prepared", queue.len() as u64);
    let workers = rayon::current_num_threads();
    let source_bytes: u64 = queue.iter().filter_map(|j| std::fs::metadata(&j.path).ok()).map(|m| m.len()).sum();
    let mut prepared = Prepared::new();
    prepared.order = queue.iter().enumerate().map(|(i, j)| (j.path.clone(), i)).collect();
    prepared.cap = crate::index::cache_policy::prepare_ahead_cap(workers, source_bytes, queue.len(), stock_cap);
    prepared.queue = queue;
    prepared.workers = workers;
    prepared.source = Some(Source { driver, db_path: db_path.map(|p| p.to_path_buf()), reader: Arc::clone(reader) });
    prepared.start_more();
    prepared
}
