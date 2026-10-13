//! The index's half of `CachedModule`'s seal: the held analysis is a
//! `Held`, a type the model cannot name, so only code above the model opens
//! a module directly. The model reads through `CrossFileLookup`'s present
//! views, which record each read on the engine answer being computed.

use std::sync::Arc;

use crate::model::file_analysis::{CachedModule, FileAnalysis};

struct Held(Arc<FileAnalysis>);

/// A module holding `analysis`.
pub fn cached_module(path: std::path::PathBuf, analysis: Arc<FileAnalysis>) -> CachedModule {
    CachedModule::sealed(path, Arc::new(Held(analysis)))
}

/// Direct reads of a module's analysis, for the index and the layers above
/// it. Nothing an engine answer computes may reach this.
pub trait Unsealed {
    fn analysis(&self) -> &Arc<FileAnalysis>;
}

impl Unsealed for CachedModule {
    fn analysis(&self) -> &Arc<FileAnalysis> {
        // Every module is minted by `cached_module`.
        &self.held().downcast_ref::<Held>().expect("a module minted by index::sealed").0
    }
}
