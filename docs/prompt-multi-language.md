# Multi-language serving — the wiring design

**The `LanguageDriver`/`PackDriver`/`LanguageRegistry` seam this doc
designed is landed** (`src/build/language_driver.rs`; file-map entry in
CLAUDE.md). C++/Python/R/CMake/PHP ship as pack languages behind opt-in
Cargo features, each a `PackDriver` constructor plus a `.scm` skeleton
and predicates — no new types per language. PHP is the designated
next serve-in-anger target — market case, fit, and build-out sequencing
in `docs/prompt-php-target.md`. Per-language `ModuleIndex`
instances and the `FileAnalysis.language` tag (cross-language ref
pollution guard) are both landed. This brief now carries only what
hasn't shipped.

## What pack languages still don't get

Per language, all pack-side: completion context (`cursor_context` is
Perl-only; pack languages serve symbol-table completion generically
from `FileAnalysis` — acceptable v1), diagnostics (deliberately NONE
for pack languages until a calibrated substrate exists — see
"Calibration is the ship gate" below), and framework plugins (requires
keying rhai hooks on capture events — the open design round, below).

## Calibration is the ship gate

A pack language ships when it has the gold-corpus sibling: a pinned
substrate (CRAN snapshot for R via `renv`, top-N packages), the
exact-assertion fixture format reused verbatim (`run.pl` is
language-agnostic — it shells the binary), and the zero-false-positive
sweep for any diagnostic before it exists. "Best in class" is this
harness, not the feature list. Budget it as half the work.

## Shipping shape: the engine as a product

Three pieces, phased so no crate is cut before shipping demands it
(consistent with the rejected workspace split — crates when a second
consumer exists, tests for layering until then):

**The core crate (`lsp-engine`).** the `src/` layer directories ARE
the manifest: model (file_analysis, witnesses), the generic driver
(query_extract + capture vocabulary), cross-file (file_store, resolve,
module_index, module_cache), the rhai host, and the GENERIC half of
the LSP adapter. The honest cost: `symbols.rs` must split — the verbs
(documentSymbol/references/rename/workspace-symbol) are pure
FileAnalysis reads and belong to core; the intelligence (diagnostics,
import classification, hover rendering, cursor_context) is
language-flavored and moves driver-side. That disentanglement is the
biggest single line item, bigger than the routing work above.

**The middle: language packs as runtime artifacts (eventually).**
Every pack predicate written in the spikes (ctor_class, module_paths,
cmd_effects, annot_type, shape_ctor, import_call) is trivially rhai —
and the rhai host with fingerprinted cache invalidation already
ships. End state: a pack is a directory `{grammar, skeleton.scm,
predicates.rhai, pack.toml}`, installable like `.perl-lsp/` plugins.
The grammar is the only hard part: compiled-in (crate dep) first;
dynamically loaded (.so / wasm, the Helix/Zed model) later.

**The binary: one multiplexing server.** The registry serves N
languages from one process (LSP document selectors handle this).
perl-lsp doesn't die — it becomes the Perl-configured distribution of
that binary, name and install base intact.

Open design round carried from the spikes: keying framework-plugin
hooks on CAPTURE EVENTS the way rhai hooks key on CallContext today —
that is what gives pack languages a framework tier (tidyverse, CMake
module conventions). Everything else on this page is enumerable work.

## Sequencing

Registry/driver wiring, `FileAnalysis.language`, and pack languages
shipping in-editor (steps 1–4) are done. Remaining: the crate cut
(shipping shape above) — step 5 is `lsp-engine` split along the
layer-test seam with the `workspace-split` branch as the mechanical
playbook; step 6 is runtime packs. Neither starts before a second pack
language's ceiling work (diagnostics, framework tier) forces the
split's cost to pay for itself.
