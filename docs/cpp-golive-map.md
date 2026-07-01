# cpp go-live — the altitude map

The `spike/cpp-support` branch's big picture: where each piece sits relative to
the mission, so we don't lose the forest while zoomed into a slice. Status
markers are point-in-time; the *structure* is the durable part.

> **Mission:** go live with C/C++ support, via a hardened LanguagePack /
> query-engine seam. cpp-first; Python is a generality forcer (no hard DX
> runs); everything resolves via ref/edge, never a cursor-time shape pile.

```
ARC 1  cpp seam refactor ............................... ✅ DONE
       member-as-ref, Peel combinator, op-DX-on-ref, LangPack fold

ARC 2  Flow combinator / value-flow tier (FlowEdge spine) 🔵 mostly done
       A–D  @flow minting, list/destructuring, array Sequence ✅
       E  narrowing cutoff-on-edges ..................... ✅
          a narrowing is a SCOPED ASSERTION over a region, not a temporal
          value — must be explicitly region-bounded. `cst::rebinds_scalar`
          deleted; cutoff is the shared `earliest_rebind_in`, edge-driven,
          consumed by Perl AND the query engine (cross-language).
       E0 binding-shape coverage ....................... ✅
       F  folded_from rename provenance ................. ✅ (const-fold
          `$self->$m()` rename rewrites the source string literal)
       G  eager→edge single source ..................... ⬜ BLOCKED
          needs sigil-aware literal typing (`my %h`/`my @a = (…)`) on the
          query FIRST (the slice-D residual); not a cleanup, a two-step chain.

ARC 3  Perl-on-query-engine migration (builder.rs shrink) 🔵 fused with ARC 2

ARC 4  cpp LSP experience .............................. 🔵 IN PROGRESS
       Strategy: docs/cpp-lsp-experience-research.md (market survey + the
       honest flow-vs-compiler line); docs/cpp-stdlib-autoconfig-research.md.

       PERF (the DX blocker — real files, e.g. perl5 op.c @16k lines, were
       unusably slow: >1min first-open):
         · reparse span-remap O(N²)→O(N log N) ............ ✅ ~3×
         · macro expansion two-tier caching (hoist the ext
             fixpoint off every analyze) .................. ✅ ~7× warm
         · lazy per-language workspace index .............. ✅
             op.c first-open 50s→seconds — a cpp session no longer eagerly
             scans the 4000+ `.pm` tree (that eager scan WAS the stall)
         · `cpp.gather` (~1.5s/analyze: include-closure BFS
             + merge, re-run every analyze) ............... ⬜ LAST perf lever
         · stdlib compiler-probe MODULE (`cc -E -v`/`-dM`) . ✅ (gather-wiring ⬜
             — would also cut the gather: op.c `<sys/mman.h>` has no path today)

       FLOW DIFFERENTIATORS (where a flow-aware engine beats clangd):
         · dynamic_cast + `std::optional` engaged narrowing  ✅
         · cpp function-scope coverage (ALL fn shapes) ..... ✅
             one universal `(function_definition) @scope` — operators/ctors/
             conversion/destructor/out-of-line minted NO scope before; fixed
             declared-type inference + documentSymbol nesting + the FP below
         · use-after-move diagnostic ..................... ⚠️ GATED
             84% FP cut (105→17 on real headers) but the residual needs
             PATH-sensitivity (conditional-move-on-returning-branch, switch-
             case, partial/member move) — beyond the flow tier. Function +
             test kept, unwired in `pack_diagnostics`. Re-wire when the FP
             classes close.

       OUT OF REACH (needs a compiler frontend, honestly conceded): full
         overload-resolution LATTICE (ICS/SFINAE/partial-ordering), template
         instantiation types, accurate ADL, preprocessor-exact macro expansion.

       PLUMBING (`==perl`→capability): diagnostics already DISPATCH (cpp gets
         `pack_member_op` + the gated use-after-move), so not fully gated; the
         file-watch glob is still `**/*.pm` only (`backend.rs`) — cpp/py files
         aren't watched for incremental updates. ⬜

ARC 5  SHIP cpp ...................................... ⬜ THE GOAL
```

## The load-bearing insight: the tier is SHARED, not Perl-specific

The **primitive** (FlowEdge) and the **region machinery** (scoped-assertion
narrowing + the rebind cutoff) are language-agnostic seam; only the *surface
shapes* are per-language. C++ has first-class runtime type inspection
(`dynamic_cast`/`typeid`, `variant`, `optional`, null pointers), so narrowing is
a cpp feature, not a Perl quirk. Every tier is exercised across perl + cpp +
python — if a tier only works for Perl, the seam isn't generic yet.

### Cross-language narrowing/bind — LANDED

One shared cutoff (`file_analysis::earliest_rebind_in`, edge-driven), consumed by
both the Perl builder AND the query engine. The grammar scan is gone.

| language | `@flow` assign/decl | bind shapes (rebind) | `narrow_guard` | cutoff |
|----------|---------------------|----------------------|----------------|--------|
| perl     | ✅                  | ✅ `my`/`local`/`foreach` | ✅ defined/ref/blessed | ✅ edges |
| cpp      | ✅ (incl. reassign)  | ✅ range-for + `std::move` (struct-bind ⬜) | ✅ `dynamic_cast` + `optional` (`variant`/`holds_alternative` ⬜) | ✅ edges |
| python   | ✅                  | ✅ `for x in` (`del`/annot ⬜) | ✅ `isinstance` | ✅ edges |

Narrowing FP-audited on real projects → **sound, stays enabled** (the over-broad
patterns are rescued by the type-side gate; the one real FP — scope-blind
same-name optional inner-type — is fixed via `(name, scope)`-keyed `annot_text`).

## On-target discipline

- ARC 1–3 hardened the seam (shared; cpp benefits). Done / mostly done.
- **ARC 4 is now the active front** — and it split cleanly into PERF (the DX
  blocker, largely fixed bar the gather cache) and FLOW DIFFERENTIATORS (the
  narrowing family enabled; use-after-move honestly gated). The OUT-OF-REACH
  line (overload lattice / templates / ADL) is conceded, not promised — that's
  what keeps trust.
- ARC 5 (ship) still ahead; the remaining gates are the gather-cache perf win,
  the file-watch plumbing, and deciding what's "good enough to ship."
