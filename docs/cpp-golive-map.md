# cpp go-live — the altitude map

The `spike/cpp-support` branch's big picture: where each piece sits relative to
the mission, so we don't lose the forest while zoomed into a slice. Status
markers are point-in-time; the *structure* is the durable part.

> **Mission:** go live with C/C++ support, via a hardened LanguagePack /
> query-engine seam. cpp-first; Python is a generality forcer (no hard DX
> runs); everything resolves via ref/edge, never a cursor-time shape pile.

## THE MACRO/SEMANTIC ARC — landed (the dogfooding→design→queue run)

The full arc, from "cpp-lsp is completely useless" (hitlist.md, real op.c/fmt
dogfooding) to a semantic macro layer. The durable shape: **every C construct,
correctly named, turned out to BE a Perl construct** — config-variant macro =
superposition (arm-fold), field-block copypasta = role (`with` edge), include =
import (`use`), field slot = one shared subject. Same machinery, C surface.

- **Foundations:** determinism smoking-gun (two RandomState-order type
  decisions: DashMap class-winner race + HashMap witness order — now
  order-independent/sorted); query-compile memoize (85s hang → 2.2s suite) +
  init pre-warm (10× first-goto-def); lazy per-language index (op.c first-open
  50s→s); parallel memoized gather (warm 1413→106ms); lifecycle exit-on-EOF +
  CLI malformed-flag guard (orphan leaks).
- **Macro arc** (`docs/adr/macro-handling.md`): goto-def overhaul (`#define`-
  preference, cross-file registration, reachability-RANKED multi-location —
  never prune, portability — delegation see-through); provenance-leaf hover
  (typing = the join abstraction, display = the config-active concrete leaf);
  member-block macros = ROLES (blank-don't-expand; `BASEOP` a navigable Class;
  op_type: 235-ref splat via the ordinary ancestor walk); expansion flip
  (leave/blank/expand, per-use parse-damage gate) + function-like macros typed
  as global subs (delegation returns via Edge).
- **Semantic tier:** enum members carry their enum (`OP_CONST: opcode`);
  cross-file gd for bare value reads (enum variants + globals); DOMAIN typing
  (`Field{owner,name}` — the project-wide storage-slot fold; `op_type: opcode`
  headline + storage drill-down; bidirectional bridge, 944 sites); Perl fields
  bridged onto the same subject (AttrProjection ∪ Field — source-agnostic
  splat); include-closure visibility (rank-not-filter; same-name TU collision
  fixed CORRECTLY, not just deterministically) + `#include` goto-def.
- **Working style that made it go:** design locked with the user BEFORE
  agents (the role/blank/edges forks); one slice per agent, disjoint files,
  merge+full-verify+push between; xfail rows authored RED from the hitlist,
  promoted on landing; honest partials (use-after-move gated, Perl domain
  deferred) recorded, not forced.

## READY QUEUE

1–5 ✅ LANDED (see the arc record above). Remaining:

6. **refs symmetry audit** 🔵 IN PROGRESS (agent, re-fired after an API-outage
   kill; seeded with the predecessor's `resolve.rs` fragment) — **invariant:
   any resolution gd does forward (use→def), gr MUST mirror backward
   (def→uses) on the SAME key.** Audit kind × {gd, gr} on real perl5 +
   fixtures, close every gd-only asymmetry through the `refs_to`/
   `resolve_symbol` seam, lock each kind as a GOLD PAIR (gd row + gr row).
   Known-dark: enum-variant gr, macro-at-`#define` gr.
6b. **cross-file identifier completion** ✅ LANDED — the completion face of
   "C = Perl, everything exported": bare-identifier candidates now include the
   file-scope symbols (enum constants, functions, typedefs, globals) of every
   header in the include closure. Gathering rides the visibility slice —
   `ModuleIndex::visible_defs_with_prefix` enumerates `all_defs` gated to the
   closure (NO global fallback: a non-includer never sees a header's names),
   sharing `FileAnalysis::is_linkage_visible` with `register_symbols` so
   "resolvable" and "offered" can't drift. Prefix-gated server-side (like
   macros) + `is_incomplete: true` so clients re-request per keystroke;
   own-file wins dedup and ranks first. op.c `OP_` → 417 opcode enumerators
   (`opcode — opnames.h`), gather ~2 ms. Gold: cpp-cross-file.json
   bare-identifier trio (enum constants / function / non-includer negative).
   Residual: proto.h variadic decls (`Perl_croak(pTHX_ ..., ...)`) never
   register a Sub symbol — an extraction gap, so they're absent from BOTH
   goto-def and completion (same set, by construction).
7. **cruft cleanup pass** ⬜ NEXT (after #6) — the arc accumulated fast:
   back-compat wrappers, superseded comments, dead gates, duplicated fixture
   shapes, always-`None` fields (e.g. `NominalDomain.storage`). A /simplify-
   style sweep over the arc's touched files, guarded by the full net.

**Deferred (recorded, not queued):** Perl domain typing (needs a synthetic
constant-group / `Type::Tiny` enum-domain model — `docs/adr/field-projections.md`);
type-constrained completion at domain slots (`op_type == |` → `OP_*`; needs
cursor-context work); use-after-move re-wire (needs path-sensitivity);
per-toolchain global system-header cache (behind toolchain discovery);
parametric macro return; flag-set domains (`op_flags`/`OPf_*`).

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
         · `cpp.gather` rework: PARALLEL async-background
             work-queue — memoize `header_info` per
             `(path, mtime)` (shared across the closure AND
             across files), remove the 1000-header cap, run
             ahead of interaction (NOT on-demand: lazy just
             relocates the stall to the hover) ............. 🔵 IN PROGRESS (agent)
             Parallel is strictly better regardless of the
             cache; the cache still matters (a hit skips the
             gather) — cold≈warm today means tier-2 isn't
             hitting, being diagnosed alongside.
         · stdlib compiler-probe MODULE (`cc -E -v`/`-dM`) . ✅ (gather-wiring
             🔵 IN PROGRESS — feeds `resolve_include` so op.c
             `<sys/mman.h>` resolves; predefined_macros exposed
             for the macro-model `#if` eval)
         · per-TOOLCHAIN global system-header cache ........ ⬜ PARKED
             (behind toolchain discovery — "almost-global",
             keyed per toolchain; the in-process memoize above
             is the down-payment)

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
         · TYPE-CONSTRAINED completion .................... ⬜ (sick)
             at a typed slot (`x.` where x:T, a `T`-typed arg position, a
             return slot), offer only members/values whose type matches the
             EXPECTED type — rank/filter completions by the type tier we
             already have. Flow-aware, additive; clangd does a weak version.

       KNOWN LIVE BUGS (op.c stress — ⬜, being investigated):
         · macro/type goto-def resolves the WRONG def: `PERL_BITFIELD16` use
             in op.h jumps to win32.h's `#define` instead of surfacing ALL
             three config-variant defs. MODEL LANDED (233a71f, unmerged):
             every `#define` carries its `#if` guard trail; 3-valued
             reachability (ACTIVE/UNKNOWN/UNREACHABLE) seeded by the def
             UNIVERSE, not a hardcoded platform list (rule #10 clean); the
             variant JOIN for typing; nothing pruned. SURFACES RESIDUAL —
             the model has the data, the LSP doesn't read it yet:
               - multi-location ranked goto-def + hover ⬜ (needs guard
                 storage on `Symbol` + cross-file same-name enum + the probe's
                 predefined_macros as the ACTIVE seed) → the reported bug is
                 NOT user-visibly fixed yet.
               - join→typing ⬜ (`op_type` still untyped: `PERL_BITFIELD16 →
                 U16` is the TYPEDEF case; needs typedef resolution `U16 →
                 unsigned short → Numeric` + a join override seam).
         · `op_p` member completion peel `(*op_p)->` not firing.
         · `op_type` hover shows a spurious/random line.
         · op.c still slow per-analyze (the `cpp.gather` lever, above).

       TABLE STAKES — the ship gate (dogfooding, hitlist.md). The honest
         read: we built DIFFERENTIATORS (narrowing, use-after-move,
         function-scope) on a core tier that under-emits for cpp, so the
         first surfaces anyone touches — outline, references, macro-as-symbol,
         `#include` nav, completion — are broken. NOT six features: ONE
         core-emission gap wearing six hats. The LSP surfaces are thin
         adapters over `FileAnalysis` (rules #2/#3/#7) — sharpen the EMISSION
         to the Perl bar and they light up for free. Each hitlist symptom is
         the same sentence "the model doesn't emit X, so nothing flows":
           - macro USES aren't Refs → the macro Symbol (+ provenance to the
             inner def) → fixes no-gr, no-callers, wrapper-gd opacity, and
             transparent see-through, all from one emission. ⬜
           - `#include` path isn't ONE claimed import edge w/ a resolvable
             target → gd dead + a sub-token (`h`) leaks as a stray var ref.
             Claim whole path + resolve to header (like `use`). ⬜
           - outline noise = template-wrapped defs unextracted +
             every `#define` mints a kindless `@def.var`. Extract through
             `template_declaration` + give macros a real `SymbolKind`. ⬜
           - enum members ARE symbols already (skeleton.scm:77); op.c:185
             fails because completion doesn't know the SLOT wants an OP value
             → the type-constrained/flow tier, one level up. ⬜
         This IS the "sharpen the core so it flows" thesis. Table stakes gate
         ARC 5; lock each hitlist line as an e2e/gold row so it can't regress
         back to "useless" silently.

       ADDITIVE DEPTH (spiked — NOT out of reach): overload resolution, ADL,
         and template instantiation are ADDITIVE layers, each a per-depth
         accuracy/cost tradeoff we evaluate rather than a wall. Templates are
         framed as PROJECTIONS (lands well). We don't have to be compiler-grade
         at every corner to be useful at the common one; the honest line is
         "which depth is worth it here", not "impossible".

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

### The "system root" is cross-language too

The header-gather's memoize-and-cache machinery is generic; only the *source*
of the "system dependency root" is per-language — cpp = toolchain include
roots (`cc -E -v`), perl = `@INC`, python = the interpreter probe. Same
`header_info` memoization, same per-root (almost-global, machine/toolchain-
stable) cache; you just pick your "system." Another instance of shared
mechanism + per-language surface. The cpp gather-rework is the first mover;
don't hard-code cpp assumptions that block the perl/python reuse.

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
  narrowing family enabled; use-after-move honestly gated). Overload / ADL /
  templates(-as-projections) are ADDITIVE depth we've spiked — evaluated as a
  per-level tradeoff, not conceded. Trust comes from being honest about WHICH
  depth we've turned on, not from pretending the ceiling is a wall.
- ARC 5 (ship) still ahead; the remaining gates are the gather-cache perf win,
  the file-watch plumbing, and deciding what's "good enough to ship."
