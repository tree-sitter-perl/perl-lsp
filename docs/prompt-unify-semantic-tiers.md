# Unify the semantic tiers: one witness engine, per-language emitters

Implementation brief. The serving-tier twin is
`docs/prompt-unify-language-paths.md` (phase 4 here just points at it).
When all phases land, this brief deletes (docs-gc rules); decisions worth
keeping move to `docs/adr/bag-canonical.md` and a new
`docs/adr/emission-drivers.md` written in phase 1.

## Mission

Perl and the pack languages (C++/Python/R/CMake) already share ONE
semantic engine: one `WitnessBag` per `FileAnalysis`, one attachment
vocabulary, one `ReducerRegistry`, one set of query entry points, one
cross-file chase, one storage/residency/freshness stack. What they do NOT
share is the tier that FEEDS the engine:

- **Emission**: Perl emits densely from a stateful walk
  (`builder.rs`); pack emits sparsely from skeleton extraction
  (`query_extract.rs`) + a post-assembly fuel pass
  (`language_driver.rs::emit_return_fuel`). Several witness shapes are
  spelled twice.
- **The fold**: `fold_to_fixed_point` (worklist to fixed point) runs for
  Perl only. Pack compensates with query-time chases.
- **Enrichment**: import-driven cross-file propagation
  (`enrich_imported_types_with_keys`) and the R4 enriched overlay are
  Perl/hub-shaped. Pack cross-file flows through include closures only.

This brief moves those three tiers DOWN into the shared engine, each as a
per-language driver over shared machinery. Direction ratified (session
2026-07-12): converge by generalizing the engine, never by rewriting a
front end.

**Anti-goals (ratified, do not revisit):**

- Do NOT port the Perl walker to tree-sitter queries. The walk does
  order- and state-dependent work (scopes, per-token refs, provenance,
  the emit-first/query-after discipline) that query matching cannot
  express. Queries are the plugin/extraction vehicle, not the core
  extraction goal.
- Do NOT force pack emission to Perl density in one push. Density grows
  fact-by-fact where a gold row wants it.
- Do NOT merge the three tiers' ACT sides (what each language does with
  a fact stays per-language policy; the MACHINERY is what unifies).

## Ground truth (anchors as of spike tip `67b895c`)

Verify each anchor before editing — line numbers drift; the content
strings won't.

**The shared engine (do not fork any of this):**

- `src/model/witnesses/` — `WitnessBag`, `WitnessAttachment` (`Symbol`,
  `SymbolReturnArm`, `Expr`, `BranchArm`, `Variable{name,scope}`,
  `Expression(refidx)`, `TypeName`, `PackageSymbol{package,name}`,
  `SlotType{class,key}`), `ReducerRegistry::with_defaults()`, the
  reducers, `query_variable_type` / `query_sub_return_type`, the
  cross-file fallback hops in `query_rec` (PackageSymbol primary +
  parents + bridged entities; SlotType primary + parents; TypeName
  terminal), `QueryState` pins/memo.
- `src/model/file_analysis/` — the bag rides `FileAnalysis`
  (`#[serde(default)]`, cached in the blob); `inferred_type_via_bag`,
  `sub_return_type_at_arity`, `expr_type_at_span`,
  `symbol_return_type_via_bag`.

**Perl emission (the dense driver):**

- Walk-live pushes via `emit_expr_witness` / `expr_payload`
  (`builder.rs`).
- `Builder::populate_witness_bag` (`builder.rs`, `fn populate_witness_bag`)
  — post-walk: `HashRefAccess` observations, mutation Facts, the
  implicit-return chain (`Symbol(sid) → Edge(SymbolReturnArm(sid))`,
  `SymbolReturnArm(sid) → Edge(Expr(last_expr_span))`), SlotType write
  edges (`SlotType{class,key} → Edge(Expr(rhs_span))`).
- `resolve_forward_expr_witnesses` — deferred `Expr(span) →
  Edge(Symbol(sid))` for forward-defined callees.

**Pack emission (the sparse driver):**

- `src/build/query_extract/` (grep `witnesses.push`): typed decls →
  `Variable{name,scope}` with `Edge(TypeName(class))` payloads; typedef
  chains → `TypeName(alias) → Edge(TypeName(target))`; assorted
  `Expr(span)` witnesses.
- `src/build/language_driver.rs::emit_return_fuel`: per return site
  `SymbolReturnArm(sid) → Edge(Expr(ret_span))` +
  `Symbol(sid) → Edge(SymbolReturnArm(sid))` — the SAME shape as Perl's
  implicit-return chain, spelled independently (source tags
  `"cpp_return_arm"` / `"cpp_return_arm_chain"`); gated on
  `pack.implicit_this_members`: implicit-`this` field reads →
  `Expr(span) → Edge(Variable{field,scope})` + sibling-call
  `resolved_package` pinning.

**The Perl-only fold:**

- `builder.rs::fold_to_fixed_point(&chain_idx)` — worklist driver.
  Each iteration: `ChainPassMode::PreFold` → `resolve_return_types`
  (= `emit_arity_return_witnesses` → `emit_method_call_return_edges` →
  `seed_return_types_from_bag` → `write_back_sub_return_types` →
  `propagate_call_bindings_to_constraints` →
  `fixup_call_bound_hash_key_owners`). Termination: snapshot
  (per-Sub registry answer + bag len + invocant cache size) stops
  moving; `MAX_FOLD_ITERATIONS = 64` debug net. Re-emittable passes are
  clear-and-emit via `WitnessBag::remove_by_source_tag` (tags:
  `arity_detection`, `method_call_return`, `local_return`,
  `plugin_bridge`, `inheritance`, `call_binding`, `array_push`).
  `ChainPassMode::PostFold` fills `invocant_class` once settled.

**Perl-only enrichment:**

- `FileAnalysis::enrich_imported_types_with_keys(module_index)` —
  truncate-to-baseline (`base_witness_count` etc.), import scan over
  exporters (bag_present + the A1 enriched retry), call-binding TC
  pushes, HashKeyAccess owner fixups, cross-file inheritance edge
  projection.
- `ModuleIndex::enriched_snapshot` — the R4 overlay: fingerprint +
  generation keyed, byte-capped (`ENRICHED_CAP=64`,
  `ENRICHED_BYTE_CAP=128MiB`), ENRICHING thread-local cycle guard,
  cached None-declines, `long_lived` gate; consumed via
  `CrossFileLookup::enriched_present` fallback-on-miss.

**Pack capabilities live on `LangPack`** (`query_extract.rs`, one struct
literal per language in `language_driver.rs`): `implicit_this_members`,
`shape_name`, etc. New capabilities go here — a capability is a language
FACT ("this language elides the receiver"), never a feature toggle.

---

## Phase 1 — the shared emission library

**Goal:** one speller per witness shape. Perl and pack drivers call the
same helpers; a new language driver (or a new fact in an existing one)
composes helpers instead of hand-rolling `Witness { .. }` literals.

**New module `src/model/witness_emission.rs`** (the MODEL layer by
directory — it imports `witnesses` + `file_analysis`
types only; NO tree_sitter imports, NO builder imports; both drivers call
INTO it). Functions take `&mut WitnessBag` (or the small state they need)
plus plain data — never a `Node`, never a `Builder`.

Initial API (signatures indicative; keep them data-plain):

```rust
/// The implicit/explicit return chain: Symbol(sid) → SymbolReturnArm(sid)
/// → Expr(arm_span), one arm per call. Idempotent per (sid, arm_span).
pub fn emit_return_arm(bag: &mut WitnessBag, sid: SymbolId,
                       arm_span: Span, source: WitnessSource);

/// A declaration whose type is a class/alias NAME: Variable{name,scope}
/// → Edge(TypeName(type_name)). The one-alias-graph entry.
pub fn emit_typed_decl(bag: &mut WitnessBag, name: &str, scope: ScopeId,
                       type_name: &str, span: Span, source: WitnessSource);

/// A type-alias hop: TypeName(alias) → Edge(TypeName(target)).
pub fn emit_alias_edge(bag: &mut WitnessBag, alias: &str, target: &str,
                       span: Span, source: WitnessSource);

/// An expression whose value IS a named variable's value (implicit-this
/// field read, plain var read used as invocant): Expr(span) →
/// Edge(Variable{name, scope}).
pub fn emit_expr_reads_variable(bag: &mut WitnessBag, span: Span,
                                name: &str, scope: ScopeId,
                                source: WitnessSource);

/// A call-site return edge: Expression(refidx) →
/// Edge(PackageSymbol{package, method}).
pub fn emit_call_return_edge(bag: &mut WitnessBag, refidx: usize,
                             class: &str, method: &str, span: Span,
                             source: WitnessSource);
```

**Migration (mechanical, one commit per side):**

1. `emit_return_fuel`'s return-arm block → `emit_return_arm` (keep the
   `"cpp_return_arm"` source tags — they are load-bearing for
   clear-and-emit and tests). The `for_attachment(&WA::Symbol(sid))
   .is_empty()` declared-return guard stays at the CALLER — it is pack
   policy (declared return wins), not shape.
2. `populate_witness_bag`'s implicit-return block → `emit_return_arm`
   (Perl's own source tags preserved).
3. `query_extract.rs`'s typed-decl / alias pushes → `emit_typed_decl` /
   `emit_alias_edge`.
4. `emit_return_fuel`'s implicit-this field block →
   `emit_expr_reads_variable`.
5. `emit_method_call_return_edges`'s per-site push →
   `emit_call_return_edge` (the clear-and-emit `remove_by_source_tag`
   call stays at the caller — pass discipline, not shape).

**Rules:**

- Helpers push EXACTLY what the call sites push today. Phase 1 is a
  refactor: byte-identical `FileAnalysis` output is the acceptance bar
  (see gate). If a helper wants to "improve" a shape, that is a later
  phase.
- Source tags are parameters, never hardcoded — the tag namespace
  belongs to the drivers.
- No new witness shapes in phase 1.

**Gate:** `cargo test` both feature sets; gold cold+warm
`432/17/0/0/0` (strict residency is on via the harness); byte-identity
spot check — `perl-lsp --dump-package` on 2-3 substrate packages and a
cpp fixture class before/after, diff must be empty. No
`EXTRACT_VERSION` bump (shapes unchanged).

**Definition of done:** zero `Witness { .. }` literals for the five
shapes above outside `witness_emission.rs` (add a source-scan test in
`layering_tests.rs` in the style of
`whole_copy_registration_sites_are_allowlisted` that greps for
`attachment: WitnessAttachment::SymbolReturnArm` outside the module and
fails on new spellings). Write `docs/adr/emission-drivers.md`: the
helper-per-shape contract, the tag-ownership rule, the caller-owns-policy
rule.

---

## Phase 2 — the language-neutral fold

**Goal:** the worklist driver runs for any `FileAnalysis`; what runs
INSIDE an iteration is a set of registered contributors, gated by
language capability. Pack languages get build-time chain typing and
arity handling from the same loop Perl uses.

**Step 2a — extract the driver.** Pull the loop skeleton of
`fold_to_fixed_point` into `witness_emission.rs` (or a sibling
`src/fold.rs`, MODEL layer):

```rust
pub trait FoldContributor {
    /// Stable name for timing + debugging.
    fn name(&self) -> &'static str;
    /// One iteration's contribution. MUST be clear-and-emit if it
    /// pushes witnesses (remove_by_source_tag first) or otherwise
    /// idempotent. Returns whether it changed anything it owns beyond
    /// the bag (the driver already snapshots the bag).
    fn run(&mut self, fa: &mut FoldState<'_>) -> Changed;
}

pub fn fold_to_fixed_point(state: &mut FoldState<'_>,
                           contributors: &mut [Box<dyn FoldContributor>]);
```

`FoldState` carries what today's passes read/write: the bag, symbols,
scopes, refs, `return_types`, the invocant cache, the registry handle,
`package_framework`, provenance sink. Build it as a struct of `&mut`
borrows — do NOT clone `FileAnalysis` pieces into it.

The driver owns: iteration, the termination snapshot (generalize
today's: per-Sub registry answer + bag len + a contributor-supplied
extra key via `fn snapshot_key(&self) -> u64` defaulting to 0), and
`MAX_FOLD_ITERATIONS` (keep 64, keep debug-only).

**Step 2b — Perl becomes contributors.** Wrap the existing passes
one-to-one: `ChainTypingPreFold`, `ArityReturnWitnesses`,
`MethodCallReturnEdges`, `SeedReturnTypes`, `WriteBackSubReturns`,
`CallBindingPropagation`, `HashKeyOwnerFixup`. Registration order = the
order the doc comment on `build_with_plugins` states today. PostFold
stays a post-loop call (it is deliberately once-after-settled — do not
fold it in). Behavior-identical: same gate as phase 1 including the
`extra_re_fold` idempotency test (`build_with_plugins_extra_re_fold`)
which must still land in 1 iteration.

**Step 2c — pack opts in.** Wire the driver into
`PackDriver::analyze_with_path` as a new named phase AFTER
`emit_return_fuel` (the doc comment on that fn enumerates phases —
extend it). First contributors, each behind a `LangPack` capability:

1. **`CallReturnEdges`** (capability: reuse the member-call semantics
   the pack already has — if the language has method calls, it wants
   this; make it default-on for packs with member access). Emits
   `Expression(refidx) → Edge(PackageSymbol{package, method})` for every
   `MethodCall` ref whose invocant class is known from the bag
   (receiver variable's `Variable → TypeName` chase). This is what lets
   `a.b().c()` type at BUILD time instead of per-query sentinel work.
   Tag: `method_call_return` (same tag as Perl — same fact).
2. **`ArityReturnWitnesses`** for overloads (capability:
   `arity_discriminated_returns`; C++ yes, Python no). Reuses the Perl
   contributor if its inputs generalize (it reads return arms per
   Symbol + arity — check `emit_arity_return_witnesses` for
   Perl-specific reads and hoist them behind `FoldState` accessors).

Expected user-visible wins (author gold rows FIRST, as xfail, then flip
to gold when the contributor lands — `gold-corpus/run.pl --emit` to
author): cpp chained-call hover/completion without sentinel reparse;
overloaded-return goto/type-at rows.

**Invariants (verbatim from CLAUDE.md, they all still bind):** witnesses
are monotone within an iteration set; edges-not-values (a contributor
that re-pushes a materialized `InferredType` onto an edge-reachable
attachment is the parallel-store bug); clear-and-emit for anything
re-emittable; walker-only-observes does not apply to contributors (they
ARE the fold) but emit-then-query does.

**Gate:** phase-1 gate PLUS: build-time cost — `--timings` per-module
build on the substrate must not regress >5% wall; cpp fixture builds
(`startup cpp-fixture` line in a gold run) within noise. `EXTRACT_VERSION`
bump IS required here (new witnesses in pack blobs). Bump once at the
END of 2c, not per-commit.

---

## Phase 3 — enrichment as a driver capability

**Goal:** "a closed file answers with its cross-file facts applied" for
every language, through one overlay, with per-language policy on WHICH
facts flow.

**Step 3a — name the policy seam.** New trait (MODEL layer, next to the
overlay):

```rust
pub trait EnrichmentPolicy: Send + Sync {
    /// The provider files this analysis pulls facts from, as canonical
    /// keys the index can resolve (module names for Perl; header paths
    /// for cpp). Drives the overlay's transitive freshness walk too.
    fn providers(&self, fa: &FileAnalysis) -> Vec<ProviderKey>;
    /// Apply cross-file facts in place. Must be idempotent under the
    /// truncate-to-baseline discipline (base_*_count seals).
    fn enrich(&self, fa: &mut FileAnalysis, idx: &dyn CrossFileLookup);
}
```

Perl's impl wraps `enrich_imported_types_with_keys` + the import list
(providers = `fa.imports`). This step is a pure re-plumbing of the hub
path: `enriched_snapshot` calls the policy instead of the hard-coded
method; `enrichment_key`'s transitive provider walk reads
`policy.providers()` instead of the inline import scan. Gate:
behavior-identical (R4 tests, gold).

**Step 3b — a cpp policy.** Providers = the include closure's direct
edges (`include_directives`, NOT the full transitive closure — the
overlay's own transitive walk composes hops, and full-closure keys would
make every header edit move every key). Enrich = project provider facts
the query-time chase currently re-derives per query; start with ONE
fact: imported (included) class methods' return types as local
`PackageSymbol{package,m} → Edge(...)` writeback edges, mirroring what
Perl's enrichment projects for cross-file parents. Measure before
widening: if the query-time chase already answers everything the gold
corpus asks, keep the cpp policy MINIMAL (providers-only, empty
enrich) and record that as the honest boundary — the seam still buys
the overlay's freshness keying for cpp.

**Step 3c — overlay on sub-indexes.** `enriched_snapshot` +
`enriched_present` today live on the hub and gate on `long_lived`. Lift
the overlay cell to a per-index member (it already is — the DashMap is
per-`ModuleIndex`), wire `enriched_present` override for pack sub-indexes
under the SAME `long_lived` gate (thread the flag at
`attach_pack_index`, like the foreign bag cache cell), and byte-account
against the same `ENRICHED_BYTE_CAP` PER INDEX (document: N languages =
N caps; if that's too loose, one shared `AtomicUsize` budget — decide by
measuring abseil warm RSS, budget is flat ±10 MB).

**Honest boundaries to ledger (do not fix in this brief):** pack has no
open-doc surface recorder, so `SurfaceWrite` provenance stays hub-only;
pack enrichment answering OPEN cpp docs stays query-time.

**Gate:** phase-1 gate + R4 seam tests + abseil warm RSS flat + the
enrichment cycle tests (mutual include A↔B terminates, no poisoned
cache — reuse `transitive_enrichment_mutual_import_terminates_without_poison`
as the template).

---

## Phase 4 — serving tier

Covered by `docs/prompt-unify-language-paths.md` (indexing by one seam,
completion machinery, killing `language != "perl"` branches). Do it
AFTER phases 1–3: the semantic unification removes the "but pack types
work differently" excuse every serving-path fork hides behind.

---

## Global rules for the builder

- **Worktree base check (hard-learned):** agent worktrees in this repo
  have spawned on stale bases. Before ANY edit:
  `git log --oneline -1` must show the spike tip you were pointed at;
  if not, `git fetch origin <branch> && git reset --hard origin/<branch>`.
- **Verification per phase:** `cargo test` AND `cargo test --features
  cpp` fully green; `cargo build --release --features cpp` BEFORE any
  gold run (`cargo test --release` without the flag silently rebuilds a
  perl-only binary — 239 rows lang-skip and the run lies); gold
  cold + warm (`perl-lsp --clear-cache gold-corpus/local` first for
  cold) expecting `432 PASS / 17 xfail / 0 FAIL / 0 XPASS / 0 CRASH`
  — strict residency is set by the harness, a CRASH row is your bug;
  `--refs-parity <root>` 0 mismatched when touching anything
  storage-adjacent.
- **CLAUDE.md rules bind everywhere**, especially #10 (no shape
  special-cases — a capability on `LangPack` is the sanctioned form of
  a language fact), the Edges-not-values discipline, the residency
  discipline (any new derived-copy cache is byte-accounted; new
  whole-copy registration sites fail the allowlist test until
  deliberately added), and comment style (why, not what; no history).
- **`EXTRACT_VERSION`** bumps when blob shape or witness RULES change
  (phase 2c, possibly 3b). One bump per landed phase, in the landing
  commit.
- **Ledger discipline:** every genuine fork → `docs/open-forks.md`
  entry (options, pick, undo cost). Every deferred residual → same.
- **Commit shape:** one commit per step above, message states the
  invariant preserved (e.g. "byte-identical FileAnalysis output"), house
  trailers.

## Suggested order and sizing

Phase 1 is one sitting (pure refactor, strong gate). Phase 2a+2b is the
riskiest single step (touching the Perl fold) — land it alone, gate
hard, THEN 2c per-contributor. Phase 3a alone, 3b/3c after measuring.
Each phase is independently shippable and independently revertible;
nothing in a later phase changes a decision in an earlier one.
