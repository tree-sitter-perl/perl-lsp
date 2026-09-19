//! The cursor Slot taxonomy — one vocabulary for "what kind of hole is the
//! cursor in", per `docs/adr/cursor-slots.md`. Two detectors answer into
//! it: Perl's wraps `cursor_context`'s tree/text detectors; the pack
//! languages' wraps `cursor_sentinel`'s sentinel-reparse member access.
//! Neither detector's internals change here — this module only
//! re-expresses their existing outputs as `Slot` verdicts, so consumers
//! (completion, sig-help) switch on `Slot`, never on language.
//!
//! `detect_slot` answers the identity question (Member / Key / Identifier
//! / Import / ModulePath). It does NOT answer the arg-position question —
//! `detect_call_slot` does, orthogonally: a receiver's Member slot and an
//! enclosing call's ArgPosition can both hold at once (`foo($x->|)`), so
//! folding them into one mutually-exclusive detector would change which
//! candidates a nested-call cursor gets. Two questions, two entries, one
//! `Slot` vocabulary.

use tree_sitter::{Point, Tree};

use crate::lsp::cursor_context::{self, CursorContext};
use crate::model::file_analysis::{CrossFileLookup, FileAnalysis, InferredType, MemberOp, Span};

/// A call/method context enclosing the cursor — Perl only; pack languages
/// have no call/arg-position slot today (no sig-help for them). Alias of
/// `cursor_context::CallContext`: already tree-free, so `Slot::ArgPosition`
/// carries it directly rather than re-shaping the same fields twice.
pub type CalleeCtx = cursor_context::CallContext;

/// The receiver of a `Slot::Member` — its resolved type, source text when
/// the detector captured one (Perl always does; the pack sentinel
/// resolves a span, not text, so this is `None` there), and — pack
/// languages only — the single-token `.`/`->` operator-swap fix a
/// mismatched pointer depth wants. Perl's `->` is always correct, so
/// `op_fix` is always `None` there.
#[derive(Debug, Clone, PartialEq)]
pub struct ReceiverCtx {
    pub receiver_type: Option<InferredType>,
    pub receiver_text: Option<String>,
    pub op_fix: Option<(Span, String)>,
    /// `Foo::|` / `self::|` — a scoped access completes the class's
    /// constants and static members; `->`/`.` completes the instance ones.
    pub scoped: bool,
}

/// The owner of a `Slot::Key` — `$h->{|`'s hash, resolved by type when
/// known, else by the owning sub when the hash is a bare literal passed
/// as a call argument (`foo($x, { | })`).
#[derive(Debug, Clone, PartialEq)]
pub struct OwnerCtx {
    pub owner_type: Option<InferredType>,
    pub var_text: String,
    pub source_sub: Option<String>,
}

/// What kind of hole the cursor sits in (`docs/adr/cursor-slots.md`). Each
/// variant declares its candidate question; the slot never enumerates
/// names itself.
#[derive(Debug)]
pub enum Slot {
    /// `obj.|` / `obj->|` / `$x->|` — members of the receiver. `op` is
    /// metadata (which operator was written) — no migrated consumer
    /// branches on it yet, same "seam, not yet consumed" status as
    /// `expected_type`.
    Member {
        receiver: ReceiverCtx,
        #[allow(dead_code)]
        op: MemberOp,
    },
    /// `$h->{|` — keys of the owner.
    Key { owner: OwnerCtx },
    /// A bare identifier, including a bare sigil trigger (`$|`/`@|`/`%|`)
    /// — the visible-universe projection (`CandidateSet::complete`).
    /// `prefix` is exactly what's been typed since the last non-identifier
    /// boundary; a lone sigil in `prefix` is Perl's variable-sigil trigger
    /// (`Slot::sigil` decodes it — the fact is data on the slot, not a
    /// shape a consumer re-derives from the tree).
    Identifier { prefix: String },
    /// `use Foo qw(|` — the named module's import surface.
    Import { module: Option<String> },
    /// `use |` (typing the module name) or `Foo::|` (a qualified-path
    /// drill; pack languages' `ns::|` detects here too) — loadable modules
    /// and/or the qualifier's members/sub-packages. The two behaviors this
    /// prefix-shaped slot folds together are told apart by the slot's
    /// `DetectorArm` (`UseModule` vs `QualifiedPath`), not a local field —
    /// the arm is the generic "which detector fired" fact every slot carries.
    ModulePath { prefix: String },
    /// A type is expected here. No current detector populates this —
    /// reserved for pack languages' declaration positions.
    #[allow(dead_code)]
    TypePosition { prefix: String },
    /// `f(a, |)` and `x == |` — sig-help AND type-constrained completion.
    /// `expected` carries a PRE-RESOLVED expected type when the detector
    /// already knew it (the pack domain-comparison slot resolves the
    /// field's DOMAIN eagerly); a Perl call-arg slot leaves it `None` and
    /// `expected_type` resolves the callee's param type lazily.
    ///
    /// Calling a comparison RHS an "ArgPosition" is a drop of a lie —
    /// there's no callee and no index — but both shapes ask the identical
    /// `expected_type` question of the identical consumer, and a dedicated
    /// `Slot::Comparison` variant would sprawl the closed vocabulary for
    /// one producer. Ratified as-is (docs/forks-resolved.md); split only if a
    /// comparison ever needs consumer behavior a call-arg doesn't share.
    ArgPosition {
        callee: Option<CalleeCtx>,
        index: usize,
        expected: Option<InferredType>,
    },
}

/// Which detector arm produced a `Slot` — the generic "which detector
/// fired" fact every detected slot carries. A slot shape that folds two
/// behaviors (today `ModulePath`: the `use`-module render vs the
/// qualified-path drill) is disambiguated by asking the arm, never a
/// per-variant bool. The arms mirror the detector's own cases: Perl's
/// `CursorContext` discriminants plus the pack sentinel's member /
/// qualifier / domain arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectorArm {
    Variable,
    Member,
    HashKey,
    ImportList,
    /// `use Foo::|` — typing a loadable module name.
    UseModule,
    /// `Foo::|` / pack `ns::|` — a qualified-path drill into an owner.
    QualifiedPath,
    DomainCompare,
    CallArg,
    General,
}

/// A detected `Slot` paired with the `DetectorArm` that produced it. The
/// pairing is how the arm stays generic — carried by every slot the
/// detectors emit — rather than living as a bool on the one variant that
/// needed it first.
#[derive(Debug)]
pub struct DetectedSlot {
    pub slot: Slot,
    pub arm: DetectorArm,
}

impl Slot {
    /// Decode a bare sigil trigger (`$|`/`@|`/`%|`, nothing else typed)
    /// out of an `Identifier` slot. `None` for every other slot, and for
    /// an `Identifier` whose prefix is empty, a real word, or longer than
    /// one char — i.e. `None` means "run the general identifier path",
    /// matching `cursor_context::CursorContext::General`'s old fallthrough.
    pub fn sigil(&self) -> Option<char> {
        let Slot::Identifier { prefix } = self else { return None };
        let mut chars = prefix.chars();
        let c = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        matches!(c, '$' | '@' | '%').then_some(c)
    }

    /// The type expected at this slot, when derivable — the seam for
    /// type-constrained completion (`docs/adr/cursor-slots.md`). Two
    /// producers: an `ArgPosition` carrying a pre-resolved `expected`
    /// (the pack domain-comparison slot) returns it verbatim; a Perl
    /// call-arg slot resolves a LOCAL callee's param type at `index` via
    /// the witness-bag path signature-help's own param-type rendering uses
    /// (`inferred_type_via_bag` at the sub body's end). A cross-file callee
    /// (param types are display tags, not `InferredType`s) and every other
    /// slot answer `None`. Consumed by completion ranking (backend +
    /// symbols) and locked by `cursor_slot_tests.rs`.
    pub fn expected_type(
        &self,
        analysis: &FileAnalysis,
        point: Point,
        module_index: Option<&dyn CrossFileLookup>,
    ) -> Option<InferredType> {
        let Slot::ArgPosition { callee, expected, index } = self else { return None };
        // A detector that already knew the type (the pack domain-comparison
        // slot) carries it; otherwise resolve the local callee's param type.
        if let Some(t) = expected {
            return Some(t.clone());
        }
        let c = callee.as_ref()?;
        let sig = analysis.signature_for_call(
            &c.name, c.is_method, c.invocant.as_deref(), point, module_index,
        )?;
        if sig.param_types.is_some() {
            return None; // cross-file: pre-resolved as display tags, not InferredType
        }
        let param = sig.params.get(*index)?;
        if param.is_invocant || crate::model::conventions::is_conventional_invocant_name(&param.name) {
            return None;
        }
        analysis.inferred_type_via_bag(&param.name, sig.body_end)
    }
}

/// The one identity-question entry (`docs/adr/cursor-slots.md`). Which
/// detector serves a language is the DRIVER's answer, not this function's:
/// `DriverCaps::cursor_context` means "my slots come from the live document
/// tree via `lsp/cursor_context`", and its absence means the sentinel-reparse
/// pack path. Asking the cap rather than comparing the id keeps a second
/// tree-native language from having to be named here to be served. Falls back
/// to a bare `Identifier` slot when the language isn't registered, or is a
/// pack language with no `LangPack`.
pub fn detect_slot(
    analysis: &FileAnalysis,
    tree: &Tree,
    source: &str,
    point: Point,
    language: &str,
    module_index: Option<&dyn CrossFileLookup>,
) -> DetectedSlot {
    let reg = crate::build::language_driver::LanguageRegistry::with_enabled();
    let cursor = crate::build::cursor_sentinel::point_to_byte(source, point);
    let bare_identifier = || DetectedSlot {
        slot: Slot::Identifier { prefix: identifier_prefix(source, cursor).to_string() },
        arm: DetectorArm::General,
    };
    let Some(driver) = reg.for_id(language) else {
        return bare_identifier();
    };
    if driver.caps().cursor_context {
        return detect_slot_tree_native(analysis, tree, source, point, module_index);
    }
    let Some(lang_pack) = driver.lang_pack() else {
        return bare_identifier();
    };
    let mut parser = driver.make_parser();
    if let Some(ctx) = crate::build::cursor_sentinel::member_completion_ctx_incremental(
        &mut parser, &lang_pack, source, tree, cursor, analysis, module_index,
    ) {
        return DetectedSlot {
            slot: Slot::Member {
                receiver: ReceiverCtx {
                    receiver_type: ctx.receiver_type,
                    receiver_text: None,
                    op_fix: ctx.op_fix,
                    scoped: ctx.scoped,
                },
                op: ctx.op,
            },
            arm: DetectorArm::Member,
        };
    }
    // `fmtx::|` / `fmtx::f|` — a `::`-qualified path names its OWNER
    // explicitly. Same qualifier detection owner-anchored goto-def resolves
    // through (`resolve::qualifier_at_point`), so the completion filter and
    // gd anchor on the identical owner. The qualifier is a hard filter by
    // meaning; ahead of the domain-compare slot, which only re-ranks the
    // global pool.
    if let Some(owner) = crate::index::resolve::qualifier_at_point(source, point) {
        return DetectedSlot {
            slot: Slot::ModulePath { prefix: owner.to_string() },
            arm: DetectorArm::QualifiedPath,
        };
    }
    // `field == |` — the equality's field operand carries an enum DOMAIN.
    // The slot hands that expected type to completion so the domain's
    // members rank first (`docs/adr/cursor-slots.md`). No callee: this is
    // a comparison, not a call, but it wants the same `expected_type` seam.
    if let Some(expected) = crate::build::cursor_sentinel::domain_compare_ctx_incremental(
        &mut parser, &lang_pack, source, tree, cursor, analysis, module_index,
    ) {
        return DetectedSlot {
            slot: Slot::ArgPosition { callee: None, index: 0, expected: Some(expected) },
            arm: DetectorArm::DomainCompare,
        };
    }
    bare_identifier()
}

/// The `cursor_context` arm: slots read off the LIVE document tree, with the
/// text-scan detector as the incomplete-source fallback. Serves any driver
/// whose `DriverCaps::cursor_context` is set — Perl today, by cap and not by
/// name.
fn detect_slot_tree_native(
    analysis: &FileAnalysis,
    tree: &Tree,
    source: &str,
    point: Point,
    module_index: Option<&dyn CrossFileLookup>,
) -> DetectedSlot {
    let ctx = cursor_context::detect_cursor_context_tree_with_index(
        tree, source.as_bytes(), point, analysis, module_index,
    )
    .unwrap_or_else(|| cursor_context::detect_cursor_context(source, point, Some(analysis)));
    slot_from_cursor_context(ctx)
}

fn slot_from_cursor_context(ctx: CursorContext) -> DetectedSlot {
    let (slot, arm) = match ctx {
        CursorContext::Variable { sigil } => {
            (Slot::Identifier { prefix: sigil.to_string() }, DetectorArm::Variable)
        }
        CursorContext::Method { invocant_type, invocant_text } => (
            Slot::Member {
                receiver: ReceiverCtx {
                    receiver_type: invocant_type,
                    receiver_text: Some(invocant_text),
                    op_fix: None,
                    scoped: false,
                },
                op: MemberOp::Arrow, // Perl method dispatch is always `->`
            },
            DetectorArm::Member,
        ),
        CursorContext::HashKey { owner_type, var_text, source_sub } => (
            Slot::Key { owner: OwnerCtx { owner_type, var_text, source_sub } },
            DetectorArm::HashKey,
        ),
        CursorContext::UseStatement { module_prefix, in_import_list, module_name } => {
            if in_import_list {
                (Slot::Import { module: module_name }, DetectorArm::ImportList)
            } else {
                (Slot::ModulePath { prefix: module_prefix }, DetectorArm::UseModule)
            }
        }
        CursorContext::QualifiedPath { package } => {
            (Slot::ModulePath { prefix: package }, DetectorArm::QualifiedPath)
        }
        CursorContext::General => (Slot::Identifier { prefix: String::new() }, DetectorArm::General),
    };
    DetectedSlot { slot, arm }
}

/// The enclosing call's arg-position slot, when the cursor sits inside
/// one. Orthogonal to `detect_slot` (see the module doc) — sig-help's
/// entire question is this slot; wraps `cursor_context::find_call_context`
/// unchanged.
pub fn detect_call_slot(tree: &Tree, source: &[u8], point: Point) -> Option<DetectedSlot> {
    let call_ctx = cursor_context::find_call_context(tree, source, point)?;
    let index = call_ctx.active_param;
    Some(DetectedSlot {
        slot: Slot::ArgPosition { callee: Some(call_ctx), index, expected: None },
        arm: DetectorArm::CallArg,
    })
}

/// The identifier chars immediately before the byte cursor — the typed
/// prefix bare-identifier / cross-file gathering filters on server-side.
/// Moved here from `backend.rs` so both `detect_slot`'s pack fallback and
/// the macro/closure completion sources share one implementation.
pub(crate) fn identifier_prefix(source: &str, cursor: usize) -> &str {
    let bytes = source.as_bytes();
    let cursor = cursor.min(bytes.len());
    let mut start = cursor;
    while start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
        start -= 1;
    }
    &source[start..cursor]
}

#[cfg(test)]
#[path = "cursor_slot_tests.rs"]
mod tests;
