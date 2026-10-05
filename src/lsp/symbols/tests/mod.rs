//! symbols adapter tests, split along the suite's own section seams. Topic
//! files are `*_tests.rs` so the layering walker's test exemption applies;
//! shared parse/index helpers live in `diagnostics_tests`.

use super::*;

mod diagnostics_tests;
use diagnostics_tests::{fake_cached, parse_analysis};
mod actions_tests;
mod dispatch_tests;
mod chain_tests;
mod exports_tests;
mod lint_tests;
#[cfg(feature = "php")]
mod keyed_access_tests;
