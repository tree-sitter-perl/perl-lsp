//! Cache-gate versions, derived by `build.rs` from the file names under
//! `cache-epochs/` (the contract is `cache-epochs/README.md`).
//!
//! Compared for equality only: a value is a hash, so it has no order.

include!(concat!(env!("OUT_DIR"), "/cache_epochs.rs"));
