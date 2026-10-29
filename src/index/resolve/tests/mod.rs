use super::*;
use std::path::PathBuf;

mod refs_tests;
use refs_tests::parse;
mod rename_tests;
mod groups_tests;
mod candidate_set_tests;

mod projection_consistency_tests;
#[cfg(all(feature = "php", feature = "python"))]
mod construction_tests;
