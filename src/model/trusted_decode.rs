//! Exact-capacity `Vec` decode for blobs this process wrote.
//!
//! serde caps a sequence's preallocation at 1 MiB because a length prefix
//! from untrusted input could ask for any amount of memory. Past that cap a
//! large lane (a file's refs, its witness bag, its symbols) grows by
//! doubling, and every doubling copies and touches the whole lane again:
//! on a warm FHEM `--heatmap` that growth was the largest single source of
//! page faults (`/mnt/project-files/design/heatmap-page-faults.md`).
//!
//! The cache blob is our own output, so its length prefixes are trusted —
//! but a corrupted row must still fail rather than reserve without bound.
//! [`with_input_len`] opens a decode of `n` input bytes, and inside it a
//! [`vec`] lane reserves its exact length, clamped to `n`: every element
//! these lanes hold encodes to at least one byte, so no honest length
//! exceeds it. Outside a scope (JSON, tests, any other deserializer) [`vec`]
//! keeps serde's cautious cap.

use std::cell::Cell;
use std::fmt;
use std::marker::PhantomData;

use serde::de::{Deserialize, Deserializer, SeqAccess, Visitor};

thread_local! {
    static INPUT_LEN: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Run `decode` with exact-capacity lanes, bounded by the `input_len`
/// bytes it reads. Nests: an inner scope's bound is restored on exit, even
/// when `decode` unwinds.
pub fn with_input_len<R>(input_len: usize, decode: impl FnOnce() -> R) -> R {
    struct Restore(Option<usize>);
    impl Drop for Restore {
        fn drop(&mut self) {
            INPUT_LEN.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(INPUT_LEN.with(|c| c.replace(Some(input_len))));
    decode()
}

/// `#[serde(deserialize_with = "...")]` for a large `Vec` lane of a type
/// whose encoding is never empty.
pub fn vec<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct ExactVec<T>(PhantomData<T>);

    impl<'de, T: Deserialize<'de>> Visitor<'de> for ExactVec<T> {
        type Value = Vec<T>;

        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a sequence")
        }

        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<T>, A::Error> {
            let hint = seq.size_hint().unwrap_or(0);
            let cap = match INPUT_LEN.with(Cell::get) {
                Some(bound) => hint.min(bound),
                None => hint.min(CAUTIOUS_BYTES / std::mem::size_of::<T>().max(1)),
            };
            let mut out = Vec::with_capacity(cap);
            while let Some(v) = seq.next_element()? {
                out.push(v);
            }
            Ok(out)
        }
    }

    deserializer.deserialize_seq(ExactVec(PhantomData))
}

/// serde's own preallocation cap, kept for untrusted input.
const CAUTIOUS_BYTES: usize = 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Lane {
        #[serde(deserialize_with = "vec")]
        items: Vec<u64>,
    }

    #[test]
    fn scoped_decode_reserves_the_exact_length() {
        let lane = Lane { items: (0..300_000).collect() };
        let bin = bincode::serialize(&lane).unwrap();
        let back: Lane = with_input_len(bin.len(), || bincode::deserialize(&bin)).unwrap();
        assert_eq!(back, lane);
        assert_eq!(back.items.capacity(), back.items.len());
    }

    #[test]
    fn a_corrupt_length_is_clamped_to_the_input() {
        let mut bin = bincode::serialize(&Lane { items: vec![1, 2, 3] }).unwrap();
        // bincode's fixint length prefix: claim u64::MAX / 2 elements.
        bin[..8].copy_from_slice(&(u64::MAX / 2).to_le_bytes());
        let r: Result<Lane, _> = with_input_len(bin.len(), || bincode::deserialize(&bin));
        assert!(r.is_err(), "a lying length fails instead of reserving it");
    }

    #[test]
    fn the_scope_is_restored_after_it_closes() {
        with_input_len(10, || {
            with_input_len(20, || assert_eq!(INPUT_LEN.with(Cell::get), Some(20)));
            assert_eq!(INPUT_LEN.with(Cell::get), Some(10));
        });
        assert_eq!(INPUT_LEN.with(Cell::get), None);
    }
}
