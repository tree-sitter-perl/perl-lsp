# cpp go-live — the altitude map

The `spike/cpp-support` branch's big picture: where each piece sits relative to
the mission, so we don't lose the forest while zoomed into a slice. Status
markers are point-in-time; the *structure* is the durable part.

> **Mission:** go live with C/C++ support, via a hardened LanguagePack /
> query-engine seam. cpp-first; Python is a generality forcer (no hard DX
> runs); everything resolves via ref/edge, never a cursor-time shape pile.

## THE MACRO/SEMANTIC ARC — landed (the dogfooding→design→queue run)

The full arc, from "cpp-lsp is completely useless" (real op.c/fmt
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

## STATE @ 2026-07-03 — templates DONE, CandidateSet everywhere

The template arc landed end-to-end: (a) extraction hygiene ✅ (specs/
explicit-inst/aliases/concepts/unions/ScopeKind), (b) instances join their
class ✅ (`ParametricType::Instance`), (c) lazy projection + partial-spec
selection + the unified projection engine ✅ (PoC; design in
`docs/adr/cpp-templates.md`, code rests in git history). THE
MIGRATION landed: the resolution CandidateSet (PR #107, merged to main)
now runs the spike too — visibility/edges/ranking are construction facts,
inherited by every projection, both languages. Heatmap #99 migrated onto
`references()`. Parked list: `docs/PARKED.md`; dogfood round 2 landed all
five slices A–E (see queue #9; residuals pinned in
`gold-corpus/KNOWN-GAPS.md`).

## STATE @ 2026-07-05 — hitlist-4 families + heatmap-cpp + refs reach

A dogfood round on op.c/op.h root-caused six findings into four families,
all landed. Durable structure:

- **Family A/C/D** are the KNOWN-LIVE-BUGS flips below (`OP` gd fn-like-macro
  arbitration; macro-body member field payload; DEEP-receiver peel hint +
  Mode-B CLI parity). **Family B** = the first-open degraded window, healed
  server-side (heal-repush + coalesced refresh; `docs/PARKED.md` retains the
  ledgered bounded-wait residual).
- **heatmap-into-cpp** — `--heatmap` lights up for pack languages: per-symbol
  fan-in/fan-out over the pack sub-indexes (fan-in routes through each pack's
  own cache, not the Perl hub), a pack-language usage/dead-code view.
  `docs/adr/heatmap.md`.
- **macro-body ref indexing** — a macro name used inside another macro's
  `#define` body mints a read at its span (`macro_body_name_refs`), so gr on
  `SvFLAGS`/`SvANY` reaches nested-macro uses. Cross-file-closure reach is the
  residual (`docs/PARKED.md`).
- **call-receiver field** — single-file `mkStruct()->field` resolves: the
  free-function return type carries through `expr_type_at_span`'s member-chain
  arm. Cross-file (prototype in an included header) is the pinned residual
  (xfail `cpp-call-receiver-field-crossfile-call`).

## READY QUEUE

1–5 ✅ LANDED (see the arc record above). Remaining:

6. ✅ **refs symmetry audit** — **invariant: any resolution gd does forward
   (use→def), gr mirrors backward (def→uses) on the SAME key.** Landed
   through the `refs_to`/`resolve_symbol` seam: enum constants + struct/role
   members resolve their DEF to the same `Method{class}` target their uses
   resolve to (structural class-content gate — a pack local carrying the
   sticky class package never fans out); macros + globals are name-keyed
   `FileScopeValue` targets (every `#define` variant is a decl; expansion-
   erased and blanked uses are re-minted as reads off the splice map / blank
   diff); type names emit `PackageRef` refs (gd AND gr were dark); `#include`
   gr = who-includes-this-header on the resolved path; macro delegation
   traverses BACKWARD (wrapper call sites are references of the wrapped
   function — `Perl_op_prune_chain_head` finds its `op_prune_chain_head`
   sites), and see-through gd prefers the DEFINITION over a prototype
   (`fix_optchain` → peep.c, not proto.h). Whole-project sweeps ride
   `for_each_cached_file` (the name-keyed cache view hides tie-losers and
   symbol-less files). Real-perl5: `op_type` def → 396-site splat; `OP_SCOPE`
   def → 976 uses; `OPf_KIDS` 141; `OpTYPE_set` 58. Gold pairs per kind in
   `cpp-references.json`/`cpp-definition.json` (+`sympair/` fixtures); both
   hitlist xfails promoted. Residuals in `gold-corpus/KNOWN-GAPS.md` ("Refs
   symmetry"): template-wrapped symbols (next arc, dark BOTH ways), pack
   rename through aliases (listed, non-rewritable, not renamed).
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
6c. **resolution CandidateSet** (`docs/adr/resolution-candidate-set.md`) —
   THE structural fix for the recurring symmetry disease (5 instances: gr
   matrix, completion gathering, C1 visibility, C2 rename, win32 ranking).
   One semantic core `resolve(ctx, name) → CandidateSet` (candidates ∪
   visibility ∪ edges ∪ ranking, computed once); every feature a projection.
   Symmetry by construction, not diligence — the resolution tier's witness
   bag. **Lands on MAIN first** (the seam isn't cpp-specific), then
   main→spike merge migrates the cpp axes (ScopedLookup/delegation/
   FileScopeValue) into it as the template arc's opening slice.
6d. ✅ **arc-review fix waves** — wave 1:
   C1 gr visibility gating + C2 rename full-or-refuse + H2 path/range
   splice + H3 brace-init + H4 span-remap; wave 2a: cache lifecycle
   (H1/M1/M2/H8/M7 — in-session header edits propagate, trustworthy persist
   keys, degraded-gen guard, progress gating); wave 2b: H5 bodyless
   `#define` config knobs + H6 honest domain vote + H7 owner-gated Field
   subjects. Remaining findings: M5 (predefined-macro seed in navigation),
   L1 (self-delegation duplicate offer), L2 (enum rename no-op) fixed in
   the cleanup pass; M6 (cold-open None→warm flip) recorded in
   `gold-corpus/KNOWN-GAPS.md`; L3 (debounce-window stale analysis) is
   inherent to the debounce design, listed for awareness.
7. ✅ **cruft cleanup pass** — dead spike-superseded surface removed
   (`preprocess`/`preprocess_validated`, the `MacroVariants` model,
   `module_paths` driver method, `scope_depth`, `NominalDomain.storage`),
   kept-as-spike PoC modules explicitly annotated, history-narrating
   comments rewritten, both feature builds warning-clean.
8. ✅ **TEMPLATE ARC** (`docs/adr/cpp-templates.md`) — all three
   slices landed. (a) spec identity + Specializes edge, explicit-inst
   outline, aliases/concepts, ScopeKind fix, union DX. (b) the instance
   joins the class (`ParametricType::Instance`, exact-spelling dispatch).
   (c) instantiation-aware typing: lazy `ParamOf`/`InstanceOf` receiver
   substitution beside `RowOf` (methods, incl. trailing returns) +
   `substitute_type_params` (fields), the partial-pattern spec-selection
   ladder (exact > partial > primary; `match_template_pattern` binds the
   spec's params from the concrete spelling), ranked never-pruned family
   goto-def, the tree-free pack member-chain arm of `expr_type_at_span`
   (chained gd/completion), and the unified projection engine (PoC —
   Perl generators + template monomorphization share the
   worklist/seen-set/provenance spine; `docs/adr/cpp-templates.md`).
   Parked residue: the
   deduction/dependent-type rungs, template-template params,
   `extern template` ERROR parse (see the brief's parked list).
9. ✅ **dogfood round 2 fix run** (five slices, all landed). A: one canonical `FileScopeValue` macro identity
   from every spelling (gr grep-exact on the abseil guards). B: one-symbol
   verbs keyed on owner class / namespace / spec ladder (member gr 1621 →
   17 real; qualified calls participate; arity evaluated-not-taken). C:
   extraction structurals — operators, pointer prototypes, fn-ptr
   typedefs, anon-aggregate members, the structural macro strip +
   per-splice salvage with the structure-count gate (basic_json /
   raw_hash_set extract). D: hover joins the CandidateSet
   (`hover_candidate()` — hover and gd answer ONE resolution) + decl→def
   ranking + the Function-lane visibility gate re-activated. E: `.def`
   content sniff, guard-label suppression, access-filtered completion,
   kind labels, (name, span) class dedup. Residuals pinned in
   `gold-corpus/KNOWN-GAPS.md`; open forks in `docs/open-forks.md`.

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
          deleted; cutoff is the shared `RebindIndex`, edge-driven,
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
         · `cpp.gather` rework: PARALLEL memoized gather —
             `header_info` memoized per (path, mtime), shared
             across the closure AND across files ............ ✅ (warm 1413→106ms)
         · stdlib compiler-probe MODULE (`cc -E -v`/`-dM`) . ✅ wired:
             `include_dirs` feeds `resolve_include` (op.c
             `<sys/mman.h>` resolves); `predefined_macros`
             seeds the reachability config for BOTH variant
             minting and goto-def/hover navigation
         · per-TOOLCHAIN global system-header cache ........ ⬜ PARKED
             (behind toolchain discovery — "almost-global",
             keyed per toolchain; the in-process memoize above
             is the down-payment)
         · BOUNDED MEMORY (abseil) .......... ✅ Slice 1 ✅, Slice 2 ✅
             Slice 1 evicted per-file macro gather caches (4.02→1.22 GB,
             `evict_gather_caches_keep_headers`). Slice 2 (measured: the
             witness bag is 71.5% of the 1.2 GB resident, NOT the refs) =
             evicts the bag from pack-workspace analyses + rehydrates on
             demand from the 26 MB SQLite blob into a byte-capped LRU
             (`PackBagCache`, `src/index/pack_bag_cache.rs`); hit 0.61 GB,
             completeness-preserving (refs/symbols/name-index stay pinned).
             `docs/adr/memory-slice-2-lru.md`. EXTRACT_VERSION unchanged.

       FLOW DIFFERENTIATORS (where a flow-aware engine beats clangd):
         · dynamic_cast + `std::optional` engaged narrowing  ✅
         · cpp function-scope coverage (ALL fn shapes) ..... ✅
             one universal `(function_definition) @scope` — operators/ctors/
             conversion/destructor/out-of-line minted NO scope before; fixed
             declared-type inference + documentSymbol nesting + the FP below
         · use-after-move diagnostic ............ ✅ OPT-IN (decidable subset)
             Wired behind `diagnostics.useAfterMove` (off by default). Three
             honesty gates on `use_after_move_reads` — B (in-function),
             C (straight-line), E (locals only) — take the real-header FPs
             17→0 (spdlog/fmt/onednn) while keeping the straight-line-local
             true positives. Path-sensitive residuals (cross-branch use,
             loop-carried move, partial/subobject move, by-ref reset) stay
             silent by design. `docs/adr/use-after-move.md`.
         · narrowing DIAGNOSTICS (D1/D2/D3/D4/D6/D8) ....... ⬜ PARKED for cpp
             The narrowing FACTS are landed (the `dynamic_cast`/`optional`
             row above + the narrowing TABLE at ~L300, ✅); this is the
             DIAGNOSTICS layer on top, per D-code. All Perl-only today —
             D1 `undef-deref`, D2 `optional-deref`, D3/D4 redundant/
             contradictory-guard, D6 `deref-shape` read seams
             (`deref_receiver_sites`/`guard_sites`) minted only by
             `builder/narrowing.rs`; cpp uses `query_extract`, never runs
             `build()`, so mints none. UNBLOCK: a cpp nullability pass
             lowering `nullptr` compares + `std::optional` engagement into
             the `Undef`/`Optional` lattice along cpp control flow, plus
             cpp `guard_sites` (D3/D4). D8 `unresolved-method` is the
             exception — its facts DO resolve for cpp (receiver→class via
             `expr_type_at_span`, `SymKind::Class`, `package_parents` MRO,
             `class_has_unresolved_ancestor` silences unscanned bases) — but
             is parked on ONE valve: macro member-injection
             (`#define … void run();` / `Q_OBJECT` in a class body) reads a
             present method as absent (verified FP). Sound valve (silence any
             class whose body span holds a macro/opaque token) is buildable;
             needs spdlog/fmt/onednn calibration, same bar UAM cleared.
             Pack-CAPABILITY gated, not `lang == cpp`.
             `docs/adr/narrowing-diagnostics.md`, `docs/PARKED.md`.
         · TYPE-CONSTRAINED completion .................... ✅ (domain slots)
             at a typed slot, rank the expected type's members first. The
             `Slot::expected_type` seam drives both the cpp domain-compare
             (`op_type == |` → `OP_*` ranked, `detail: "opcode"`) and the
             Perl ArgPosition consumer (`docs/adr/cursor-slots.md`). Never
             prunes the global pool. Residual: the switch-`case |:` position
             (needs the switch-condition climb) + the Perl enum-domain source
             (`Type::Tiny` model — `docs/PARKED.md`).

       KNOWN LIVE BUGS (op.c stress):
         · config-variant macro goto-def ✅ — every `#define` carries its
             `#if` guard trail; 3-valued reachability (ACTIVE/UNKNOWN/
             UNREACHABLE) seeded by the def UNIVERSE ∪ the toolchain's
             predefined macros (rule #10 clean, one seeding point for
             minting AND navigation); multi-location RANKED goto-def +
             provenance-leaf hover consume the same ranking. Residual:
               - join→typing ⬜ (`op_type` still untyped: `PERL_BITFIELD16 →
                 U16` is the TYPEDEF case; needs typedef resolution `U16 →
                 unsigned short → Numeric` + a join override seam).
         · `op_p` member completion peel `(*op_p)->` ✅ (hitlist-4 D)
             — DEEP-receiver peel hint ("wrap, not swap") now has a producer;
             Mode-B diagnostics reach the CLI (`--batch`/`--check`) too, so
             gold sees the same answers the LSP publishes.
         · `op_type`/`op_next` macro-body member payload ✅ (hitlist-4 C)
             — a field declared inside a `#define BASEOP` body now mints as a
             `Field` of `op` with its full payload: the deref_stack survives
             (`op_next: OP*`, not `OP`), owner is the class not the macro
             package, and def-site and use-site hover agree.
         · gd on `OP` (the type) hijacked by a fn-like macro ✅ (hitlist-4 A)
             — a parenless type-position `OP` no longer resolves to a
             function-like `#define OP(p)`: fn-like macros are shape-gated
             (C's own rule — a fn-like macro expands ONLY before `(`), so the
             `typedef struct op OP` wins the candidate lane. rule #10 clean
             (the macro's own arity gates it, no name allowlist).

       TABLE STAKES — the ship gate (dogfooding). The honest
         read: the DIFFERENTIATORS (narrowing, use-after-move, function-scope)
         sat on a core tier that under-emitted for cpp — ONE core-emission gap
         wearing six hats. The LSP surfaces are thin adapters over
         `FileAnalysis` (rules #2/#3/#7); sharpening the EMISSION to the Perl
         bar lit them up:
           - macro USES are Refs (+ provenance to the inner def): gr,
             callers, wrapper-gd see-through — the macro arc + queue #6
             symmetry. ✅
           - `#include` is one claimed import edge with a resolvable target
             (goto-def + who-includes-this-header gr) — queue #4/#6. ✅
           - outline: extraction reaches through `template_declaration`
             (template arc slice a); macros carry a real `SymbolKind`. ✅
           - enum members ARE symbols; bare-identifier completion offers the
             include closure's file-scope names (queue 6b). The SLOT-aware
             refinement (op.c:185 wants an OP value at `op_type == |`) is
             the type-constrained/flow tier, one level up. ⬜
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
         `pack_member_op`; use-after-move stays gated); file watchers cover
         every served language's extensions, and a pack change runs the
         eviction + open-consumer refresh path. ✅

ARC 5  SHIP cpp ...................................... ⬜ THE GOAL
       Status answered for now: BETA (correctness) with a scaling caveat — see docs/cpp-status.md. Godot
       (7,041 files) does not complete; 30-66s stalls on large vendored
       headers. Memory is fine; the gate is per-file wall.
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

One shared cutoff (`file_analysis::RebindIndex`, edge-driven), consumed by
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
