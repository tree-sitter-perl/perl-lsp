//! Tests for the conclusion cache.

use super::*;
use crate::model::witnesses::{Conclusion, ConclusionKey};
use std::sync::atomic::AtomicUsize;

fn map_of(n: usize) -> ConclusionMap {
    let mut m = std::collections::HashMap::new();
    for i in 0..n {
        m.insert(
            ConclusionKey::SubByName(format!("f{i}")),
            Conclusion::OpenNone,
        );
    }
    ConclusionMap(m, Default::default())
}

/// A path the store has nothing for must be re-queried at most once.
///
/// While a workspace is still being baked, the negative answer is the one a
/// consult storm hits most. Without the memo, every one of those goes to
/// SQLite — trading the decode this layer exists to avoid for a round trip,
/// which is not obviously better and is certainly not what was measured.
#[test]
fn an_absent_path_is_not_reloaded_forever() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let cache = ConclusionCache::new(1 << 20, move |_p| {
        c.fetch_add(1, Ordering::Relaxed);
        None
    });
    let p = Path::new("/nope.pm");
    for _ in 0..5 {
        assert!(matches!(cache.get(p), Cached::NotBaked));
    }
    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "the loader was asked repeatedly for a path it already reported absent"
    );
}

/// Invalidation must clear the NEGATIVE memo too.
///
/// A file probed before its bake landed is remembered as absent. If
/// invalidation only dropped positive entries, that file would stay "not
/// baked" for the rest of the session and silently keep paying for a decode
/// while its conclusions sat unused in the store.
#[test]
fn invalidating_clears_the_absent_memo() {
    let calls = Arc::new(AtomicUsize::new(0));
    let c = calls.clone();
    let cache = ConclusionCache::new(1 << 20, move |_p| {
        if c.fetch_add(1, Ordering::Relaxed) == 0 {
            None
        } else {
            Some(map_of(1))
        }
    });
    let p = Path::new("/later.pm");
    assert!(matches!(cache.get(p), Cached::NotBaked));
    cache.invalidate(p);
    assert!(
        matches!(cache.get(p), Cached::Map(_)),
        "a file baked after being probed stayed permanently 'not baked'"
    );
}

/// The cache stays within its byte cap.
#[test]
fn the_cache_respects_its_cap() {
    let one = 64 + 10 * 160;
    let cache = ConclusionCache::new(one * 2, |_p| Some(map_of(10)));
    for i in 0..20 {
        cache.get(&PathBuf::from(format!("/f{i}.pm")));
    }
    assert!(
        cache.resident_bytes() <= one * 2,
        "cache held {} bytes against a {} cap — an unbounded derived cache is \
         a memory regression no functional test can see",
        cache.resident_bytes(),
        one * 2
    );
}
