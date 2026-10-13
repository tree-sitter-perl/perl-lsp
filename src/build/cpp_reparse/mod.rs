//! The C++ reparse seam — macro expansion before extraction.
//!
//! The C++ instance of `docs/adr/reparse-stratification.md`'s reparse-hook
//! flavor. The obstacle course proved the worst, most common damage is
//! a declarator-position macro: `class API_EXPORT Widget {...}` reparses
//! as a `function_definition`, so the class evaporates. The fix is not
//! clang — it is *expansion*: replace the macro with its body and
//! re-parse. The probe (`dbg_cpp_attr_probe`) showed tree-sitter-cpp
//! handles the real attribute syntax (`__attribute__((...))`,
//! `__declspec(...)`) fine — the macro was merely hiding it. So the
//! transform is generic: **expand to body, let the parser validate.**
//!
//! Two flavors fall out of one pass:
//!   - object-like declarator macros (`API_EXPORT`) — the reparse-hook:
//!     expansion fixes a corrupted parse.
//!   - function-like declaration macros (`DECLARE_DYNAMIC(cls)`) — the
//!     emit-hook outcome achieved BY expansion: the body's member
//!     declarations become real, extractable symbols. Expansion
//!     subsumes the emit-hook for C++ (the doc's bet).
//!
//! Soundness is the stratified seam: this runs strictly upstream of
//! extraction (and of any witness bag), so it never interleaves with a
//! type fixpoint. A `SpliceMap` (transformed byte → original byte, the
//! Zed-anchor idea) carries every recovered span back to user text.
//!
//! Honest scope (measured, not hidden): single source-level pass with
//! pre-expanded bodies. Macros whose expansion itself contains further
//! macro CALLS (X-macros: `COLOR_LIST(X)` → `X(RED) X(GREEN)`) need
//! iterative source passes — out of scope here; that nested tail is
//! exactly the "amortize full cpp to once" case. Wired into the `cpp`
//! feature's `PackDriver` (`language_driver.rs::cpp_driver`); measured by
//! `cpp_reparse_tests.rs`.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;
use tree_sitter::{Query, QueryCursor, StreamingIterator, Tree};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Macro {
    /// `Some(params)` = function-like; `None` = object-like.
    pub params: Option<Vec<String>>,
    pub body: String,
    /// Enclosing `#if`/`#ifdef`/`#else` conditions at the `#define`, OUTERMOST
    /// first — the config guard trail (`docs`: `cpp_macro_model`). Empty =
    /// unconditional. Rides the expansion-side rep so a config-variant macro
    /// carries WHICH config each body belongs to. `#[serde(default)]` for cache
    /// blobs written before guards existed.
    #[serde(default)]
    pub guards: Vec<String>,
    /// 0-based line of the `#define` — the variant's def site.
    #[serde(default)]
    pub def_line: usize,
    /// The delegates of every OTHER `#define` of this name that a merge
    /// shadowed (`absorb_variant`): a table keeps one body per name, but a
    /// use forwards to each config variant's callee, and the references
    /// see-through must count all of them.
    #[serde(default)]
    pub variant_delegates: Vec<String>,
}

/// One source replacement: original `[start,end)` → `replacement`.
#[derive(Debug, Clone)]
struct Splice {
    start: usize,
    end: usize,
    replacement: String,
    /// The macro NAME this splice expands — the salvage's grouping key
    /// (a broken body breaks every use, so validation is per-macro).
    name: String,
}

/// Transformed-source ↔ original-source map under arbitrary splices.
/// `to_original(t)` collapses any byte inside a replacement to the
/// splice site (per-region granularity), and otherwise subtracts the
/// net length change of all earlier splices.
///
/// Both lookups run per extracted span in `remap_spans` (O(symbols)), so
/// they must be sub-linear in the edit count. The edits partition the
/// TRANSFORMED axis into ordered, disjoint regions — replacement
/// `[ts_i, ts_i + nlen_i)` interleaved with pass-through gaps — so `ts`
/// (the transformed start of each replacement) is non-decreasing and a
/// binary search over it lands the containing region in O(log E). The
/// prefix state each region needs (`ts`, the shift accumulated *after*
/// each edit) is precomputed in `apply`; see `binary_search` for the
/// exact correspondence to the former linear scan.
#[derive(Debug, Default, Clone)]
pub struct SpliceMap {
    /// (orig_start, orig_end, replacement_len), sorted by orig_start.
    edits: Vec<(usize, usize, usize)>,
    /// `ts[i]` = transformed-axis start of edit `i`'s replacement
    /// (`orig_start + shift_before_i`). Non-decreasing — the search key.
    ts: Vec<usize>,
    /// `shift_after[i]` = cumulative `nlen - (oe - os)` through edit `i`
    /// inclusive (`trans = orig + shift`); the shift that applies in the
    /// pass-through gap *after* edit `i`.
    shift_after: Vec<isize>,
}

/// Where a transformed offset lands relative to the splice regions.
enum Region {
    /// Before every replacement, or in a pass-through gap: `orig = trans - shift`.
    PassThrough(isize),
    /// Inside edit `k`'s replacement — collapses to that macro-call site.
    Inside(usize),
}

impl SpliceMap {
    #[cfg(test)]
    pub(crate) fn edits_for_test(&self) -> &[(usize, usize, usize)] {
        &self.edits
    }

    /// The raw `(orig_start, orig_end, new_len)` edit list, ordered. The
    /// erased-use re-mint walks the BETWEEN-edit segments with it to find
    /// tokens the transform changed outside any recorded splice (the
    /// length-preserving declarator-macro strip).
    pub(crate) fn edits(&self) -> &[(usize, usize, usize)] {
        &self.edits
    }

    /// Locate `transformed`'s region. `partition_point` returns the count
    /// of edits whose replacement starts at or before `transformed`; the
    /// last of them (`pp - 1`) is the only edit that can contain it
    /// (regions are disjoint and ordered). `<=` with `pp - 1` also picks
    /// the LATER of two edits sharing a `ts` — a zero-width replacement
    /// followed by a real one — matching the linear scan's in-order
    /// processing, where the empty region never claims a byte.
    fn region(&self, transformed: usize) -> Region {
        let pp = self.ts.partition_point(|&t| t <= transformed);
        if pp == 0 {
            return Region::PassThrough(0); // before every splice: shift is 0
        }
        let k = pp - 1;
        let (_os, _oe, nlen) = self.edits[k];
        if transformed < self.ts[k] + nlen {
            Region::Inside(k)
        } else {
            Region::PassThrough(self.shift_after[k])
        }
    }

    pub fn to_original(&self, transformed: usize) -> usize {
        match self.region(transformed) {
            Region::PassThrough(shift) => (transformed as isize - shift) as usize,
            Region::Inside(k) => self.edits[k].0, // collapse to the call site
        }
    }

    /// Every expansion's ORIGINAL byte extent, in order. Each edit IS a
    /// macro use the transform erased from the parsed text — the driver
    /// re-mints a reference at each site so an expanded use still answers
    /// find-references (rule #7: every meaningful token gets a ref; rule #9:
    /// derived facts trace to source).
    pub fn expansion_sites(&self) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.edits.iter().map(|&(os, oe, _)| (os, oe))
    }

    /// If `transformed` falls INSIDE a replacement (a macro expansion),
    /// return the replacement's ORIGINAL extent `(orig_start, orig_end)` —
    /// the macro-call site. A symbol/ref that came out of an expansion
    /// (`newThing(5)` → `Perl_newThing(aTHX_ 5)`) collapses to a zero-width
    /// point under `to_original`; callers use this to give it the call
    /// site's span instead, so goto-def/hover land on the macro call.
    pub fn replacement_at(&self, transformed: usize) -> Option<(usize, usize)> {
        match self.region(transformed) {
            Region::Inside(k) => Some((self.edits[k].0, self.edits[k].1)),
            Region::PassThrough(_) => None,
        }
    }
}

/// Gather macros from a C++ file's transitively `#include`d headers, so a
/// macro `#define`d in another header (the `SPDLOG_NAMESPACE_BEGIN` idiom)
/// can be expanded in this file. Quoted includes resolve relative to the
/// file's dir, walking ancestor dirs as include roots (the classic search
/// path, discovered not configured). Bounded: depth + visited + header
/// caps; best-effort — unresolvable includes are skipped. The file's OWN
/// macros are NOT included here (the caller collects those).
/// Cached transitive-macro table, keyed by (file, its #include set). The
/// gather walks the whole include closure (perl.h reaches ~2000 macros over
/// hundreds of headers — seconds cold), so re-running it per completion
/// keystroke is untenable. The analyze pass warms this on open; completion
/// reuses it for free. Invalidates when the file's `#include` lines change;
/// header *content* edits evict through `evict_analysis_caches` (the
/// did_save / watched-files invalidation path).
type MacroTable = BTreeMap<String, Macro>;

mod cache;
mod defs;
mod expand;
mod gather;
mod synthetic;
mod validate;

pub use cache::*;
pub use defs::*;
pub use expand::*;
pub use gather::*;
pub use synthetic::*;
pub use validate::*;

#[cfg(test)]
#[path = "../cpp_reparse_tests.rs"]
mod tests;
