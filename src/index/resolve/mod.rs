//! Unified query surface across FileStore + ModuleIndex.
//!
//! `resolve(cursor) → CandidateSet` is the one resolution entry point:
//! identity (`resolve_symbol_scoped`'s Target/Group/Local verdict),
//! visibility (RoleMask), edges, and per-site policy are owned by the set,
//! and every navigation verb — goto-def, references, rename, prepareRename,
//! implementations — is a projection of it. Handlers and CLI mirrors are
//! one-liners over a projection; none re-derives identity or the per-tier
//! walk inline (that's how the CLI and LSP used to disagree on hash-key
//! references, and how visibility axes used to reach one feature and miss
//! its siblings). See `docs/adr/resolution-candidate-set.md`.
//!
//! `refs_to` / `group_refs` / `references_mask_for` are the set's internals
//! (still exercised directly by tests); new axes go into CandidateSet
//! construction, never into a handler.

use std::path::PathBuf;

use tower_lsp::lsp_types::Url;

use crate::model::file_analysis::{
    AccessKind, CompletionCandidate, CrossFileLookup, FileAnalysis, HandlerOwner, RefKind, Span,
    SymKind,
};
use crate::index::file_store::{FileKey, FileStore};

bitflags::bitflags! {
    /// Which file roles a query should search. Handlers pick the mask that
    /// fits their semantics: rename is EDITABLE (skip deps, they're read-only);
    /// references is VISIBLE (include deps, read-only reads are fine).
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct RoleMask: u8 {
        const OPEN       = 1 << 0;
        const WORKSPACE  = 1 << 1;
        const DEPENDENCY = 1 << 2;
        const BUILTIN    = 1 << 3;

        const EDITABLE = Self::OPEN.bits() | Self::WORKSPACE.bits();
        const VISIBLE  = Self::OPEN.bits() | Self::WORKSPACE.bits() | Self::DEPENDENCY.bits() | Self::BUILTIN.bits();
    }
}

mod target;
mod identity;
mod projections;
mod definitions;
mod completion;
mod hierarchy;
mod imports;
mod refs;
mod collect;
pub use target::*;
pub use identity::*;
pub use hierarchy::*;
pub(crate) use imports::*;
pub use refs::*;
pub use collect::*;

/// The canonical answer to "what does this name mean, from here" — and the
/// one object every navigation feature projects from. Identity (what the
/// cursor resolves to), visibility (which file roles a walk may see), edges
/// (override families / groups / descendants), and per-site policy
/// (`rewritable`, per-member rename texts) are all owned here; goto-def,
/// references, rename, and implementations are projections of the same set,
/// so an axis added to construction is inherited by every feature at once.
/// See `docs/adr/resolution-candidate-set.md`.
///
/// Borrow discipline: the set only ever READS the stores (projections walk
/// via `FileStore::for_each_open`), so an LSP handler may hold its open-doc
/// read guard for the set's whole lifetime.
pub struct CandidateSet<'a> {
    files: &'a FileStore,
    origin: &'a FileAnalysis,
    origin_key: FileKey,
    point: tree_sitter::Point,
    /// The routed base index (the Perl hub, or a per-language pack
    /// sub-index). Backward walks (`refs_to`, group walks) take THIS —
    /// `collect_from_analysis` re-scopes per scanned file.
    module_index: Option<&'a dyn CrossFileLookup>,
    /// The origin's include-closure scope over `module_index`, built once at
    /// construction — the per-origin visibility rule every forward
    /// resolution (identity minting, goto-def, implementations) reads, so no
    /// entry point re-applies the `ScopedLookup` decorator (arc-review C1's
    /// root shape). Transparent for Perl (empty closure).
    scoped: Option<crate::model::file_analysis::ScopedLookup<'a>>,
    /// The origin is pack-served — derived at construction from the
    /// stamped `FileAnalysis.language`, never declared by a caller. Two
    /// policy consequences, applied at the set level so every projection
    /// agrees: visibility widens to VISIBLE (pack workspace files ride
    /// the DEPENDENCY role — a storage artifact of the per-language
    /// cache, which registers only workspace-walk files), and rename
    /// REFUSES on alias-spelled sites instead of silently skipping.
    pack: bool,
    /// The origin document's raw text, when the caller has it. Feeds the
    /// raw-word candidate lanes (macro variants): a macro use can vanish
    /// from the reparsed analysis (expand-and-reparse), so the byte-level
    /// word is the reliable key. `None` = those lanes stay silent.
    source: Option<&'a str>,
    scope: OverrideScope,
    /// Identity, minted once via `resolve_symbol_scoped` — lazily, so a
    /// projection answering off its own forward path (goto-def, until every
    /// lane misses) doesn't pay the override-family walk. `None` = nothing
    /// cross-file-resolvable
    /// at the cursor; local projections still answer from `origin`.
    resolution: std::sync::OnceLock<Option<ResolvedTarget>>,
    /// Visibility for a `Target` resolution, memoized — computed by
    /// `references_mask_for` on first use (group members keep their
    /// per-member masks inside the group projections).
    visibility: std::sync::OnceLock<RoleMask>,
    /// Construction-time visibility override: when set, EVERY projection
    /// (references, rename, group walks) scopes to it — the seam future
    /// axes (closure visibility, language boundaries) plug into.
    visibility_override: Option<RoleMask>,
}

/// Cursor → CandidateSet: the single resolution entry point. Handlers and
/// CLI mirrors construct the set once and project; none of them re-derive
/// identity, visibility, or per-site policy on their own.
pub fn resolve<'a>(
    files: &'a FileStore,
    origin: &'a FileAnalysis,
    origin_key: FileKey,
    point: tree_sitter::Point,
    module_index: Option<&'a dyn CrossFileLookup>,
    scope: OverrideScope,
) -> CandidateSet<'a> {
    // The per-origin closure scope is a construction fact: forward
    // resolutions see the names THIS file's preprocessor would (C's flat
    // linkage), and Perl origins pass through untouched (empty closure).
    let self_path = match &origin_key {
        FileKey::Path(p) => Some(p.clone()),
        FileKey::Url(u) => u.to_file_path().ok(),
    };
    // The routing fact names the scope's AXIS, and `for_origin` owns the
    // derivation — include-path packs scope by include closure, name-keyed
    // packs are transparent, Perl by the asker's own search path (`use lib`
    // roots ahead of the process @INC).
    let pack =
        crate::build::language_driver::LanguageRegistry::is_pack_language(&origin.language);
    let scoped = module_index.map(|idx| {
        let axis = crate::model::file_analysis::VisibilityAxis::for_origin(
            origin,
            self_path.as_deref(),
            idx,
            crate::build::language_driver::LanguageRegistry::pack_visibility(&origin.language),
        );
        crate::model::file_analysis::ScopedLookup::new(
            idx,
            &origin.pack.include_closure,
            self_path.as_deref(),
            axis,
        )
    });
    CandidateSet {
        files,
        origin,
        origin_key,
        point,
        module_index,
        scoped,
        // The routing fact rides the origin (its driver stamped
        // `language`), so every projection inherits pack policy by
        // construction — no per-handler declaration to forget.
        pack,
        source: None,
        scope,
        resolution: std::sync::OnceLock::new(),
        visibility: std::sync::OnceLock::new(),
        visibility_override: None,
    }
}

#[cfg(test)]
mod tests;
