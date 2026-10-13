# Epic 4 — Value provenance, tier 1 (residual Parts 1, 2, 5a)

> **Status:** scheduled, fourth. The largest of the type-intelligence
> epics; run it after Epic 3 so the before/after substrate-diff habit
> is already in muscle memory.
> **Design owner-doc:** `docs/prompt-type-inference-residual.md`
> (Parts 1, 2, 5a — read the whole doc; Parts 3, 4, 7 are NOT here).
> **Strategic payoff:** this tier is the named gate for un-parking
> instance brands (`prompt-graph-walking.md` §PARKED) and for the
> untyped-receiver residual (`prompt-method-resolution-residuals.md`
> §4). Do not build those here — build the tier they wait on.

## Mission

Three fact classes that trace VALUES (not declarations) through the
program, each landing as **an emitter + reducer pair on the witness
bag**, with no `InferredType` enum expansion:

1. **Part 1 — invocant mutations, consumer wiring.** The facts exist;
   nothing user-facing consumes them.
2. **Part 2 — hash-key unions.** `{ %$defaults, %$overrides }` — keys
   of a merged hash resolve to their source hashes.
3. **Part 5a — value-indexed returns.** `get_config('host')` types from
   a literal-keyed return table.

## The decision Phase A actually makes

CLAUDE.md is blunt about Part 1's starting state:

> `mutated_keys_on_class(class)` — **no production caller** (tests + the
> layering allowlist only), and every `HashKeyDef` minting site passes
> `is_dynamic: false`. The dynamic-key lane is dormant, not wired…
> **Delete it or revive it deliberately; do not cite it as a live
> feature.**

Phase A is that deliberate revival. If the completion payoff does not
survive the noise guard, **the honest outcome of this epic is deleting
the lane**, and that is a legitimate result to ship — record it and
move on to Phases C/D. Do not leave it dormant for a third time.

## Read first, in this order

1. `CLAUDE.md` — "Type inference (witness bag)" and "Worklist
   invariants" in full. This epic lives entirely inside those rules.
2. `docs/adr/bag-canonical.md` — the bag is the only source of types.
3. `docs/adr/structural-shapes.md` — `HashWithKeys`, `Projected`
   drills, mutation extension, the whole-story trust gate. Part 2
   composes with these; do not duplicate them. Note `SharedKeys`
   (`b0fb6309`) — the clone product was deleted at the type, so a key
   set is shared, not copied. Part 2's union must not reintroduce a
   clone.
4. `docs/adr/return-expr.md` — `ReturnExpr` variants; `UnionOnArgs`
   dispatching on `arity_hint` is the pattern Part 5a copies.
5. `docs/adr/conclusion-layer.md` — new witness shapes must be
   representable (or honestly `Open`) in a conclusion bake, or every
   cross-file consult of them falls back to a full decode.
6. `docs/prompt-long-distance.md` — A4 v2 already landed cross-file
   slot-read narrowing; Part 1 must NOT rebuild it.
7. `docs/prompt-cfg-tier.md` §3.6 — **before writing Phase C's cycle
   guard.** It is three paragraphs and it names the one thing that
   guard must not make harder.

## Current state — exact anchors

| Existing piece | Where | Find it |
| --- | --- | --- |
| Class-keyed mutated-key union (read API) | `model/file_analysis/queries.rs` | `grep -rn 'fn mutated_keys_on_class' src/` |
| Mutation Facts + slot-type seeds | `build/builder/pipeline.rs` | `grep -rn 'FACT_MUTATION\|slot_writes' src/build/builder/` (in `populate_witness_bag`) |
| `SlotTypeFold` | `model/witnesses/reducers.rs` | `grep -rn 'SlotTypeFold' src/model/witnesses/` |
| Completion sources | `index/resolve/completion.rs` | completion SOURCES go through the CandidateSet's `complete()`; cursor-context slot detection stays put (`adr/resolution-candidate-set.md`'s honest boundary) |
| Moo `is => 'ro'` knowledge | `frameworks/moo.rhai` | the plugin owns the vocabulary; core sees classified pairs |
| Hash-literal shape builder | `build/builder/` | `grep -rn 'fn visit_anon_hash\|hash_literal' src/build/builder/` |
| `HashKeyOwner` enum | `model/file_analysis/core_types.rs` | `grep -rn 'enum HashKeyOwner' src/`; index rebuild in `rebuild_enrichment_indices` |
| Constant folding for string lists | `build/builder/` | `grep -rn 'declared_constants\|resolve_constant_strings' src/build/builder/` |
| Arity-hint threading (the 5a pattern) | `model/witnesses/query.rs` | `grep -rn 'arity_hint' src/model/witnesses/ \| head` |

## Non-goals

- No instance brands, no birth-site chase, no `home` qualifiers.
- No Parts 3 (method loops), 4 (map/grep), 5b (superseded by the landed
  narrowing lattice — verify before touching), 7 (Rhai reducers).
- **No new `InferredType` variants.** If a design wants one, the design
  is wrong for this epic.
- No parallel reverse indexes (rule #8).
- `delete $self->{k}` drop-tracking: out.

## Phase breakdown

### Phase A — Part 1: dynamic-key completion (or the deletion)

1. At the completion source where declared `HashKeyDef`s are offered
   for the invocant class, also merge `mutated_keys_on_class(class)`,
   deduped against the declared set and marked with a distinct
   `CompletionItemKind`/detail ("observed write") so users can tell
   contract from observation.
2. Cross-file: `mutated_keys_on_class` reads the local bag. Mirror
   however declared keys already cross files — **find that path first
   and do it the same way**; if declared keys do NOT cross files here,
   do not fix that in this epic, just note it.
3. **Noise guard is mandatory, not optional.** Author a gold row
   pinning the union with `exact_labels`, AND one proving an unrelated
   class does not see these keys. See the Scaling beat — completion
   payload is a measured, previously-regressed axis.
4. **Acceptance:** a two-method class (one `has`, one
   `$self->{observed} = 1`) completes both, the observed one flagged;
   gold rows green. **Or:** the documented decision to delete the lane,
   with the layering allowlist entry and the dead API removed.

### Phase B — Part 1: ro-write hint

1. New opt-in diagnostic `roWrite`. Grep `optional_deref` for every
   site a flag touches — struct field, CLI flag, ADR ladder text,
   tests — and hit exactly those.
2. The fact: a `HashKeyAccess` Write on `$self->{attr}` where `attr` is
   an attribute whose `is` is `ro`. **The `ro` knowledge lives in the
   plugin**: extend the `has` synthesis to record read-only-ness on the
   emitted symbol/HashKeyDef (an EmitAction field), NOT a core name
   table. Direct slot writes only to start.
3. Severity HINT; the message names the attribute and its declaring
   `has`.
4. **Acceptance:** tests both directions; substrate audit — expect
   near-zero hits. If it shows >10, check whether `_build_*` / `BUILD`
   / `BUILDARGS` contexts need exemption and document the choice.

### Phase C — Part 2: hash-key unions

**Goal:** `my $full = { %$defaults, key => 1 };` — `$full->{host}`
resolves into `$defaults`'s key set.

1. In the anon-hash visitor (rule #1 — the only tree consumer), detect
   splice elements (`%$var` / `%hash` inside the literal). **Get the
   node kinds from `perl-lsp --parse` on a snippet first; do not
   guess.**
2. Encode as a UNION witness, not an eager copy: the existing
   `HashWithKeys` path for the literal keys, PLUS an Edge per splice
   from the literal's shape attachment to the spliced variable's. A
   splice makes the shape OPEN (unknown extra keys) even when the
   source resolves — set that flag. `SharedKeys` means the key set is
   shared at the type; do not clone it into the union.
3. Reducer side: when a `Projected { base, HashKey(k) }` misses the
   literal keys, chase the splice edges. The registry already chases
   Edges — confirm the attachment shapes line up and add a cycle guard
   via the existing `QueryState` visited set. `$a = { %$b }; $b = { %$a }`
   is the test.
   **Do not entrench the silent drop.** `query_rec`'s visited guard
   resolves an on-path revisit to *nothing*, so the cyclic arm vanishes
   from the fold. That is harmless for types — a cyclic edge carries no
   information, which is why it is correct here — but Epic 16 §3.6 has
   to replace it with an explicit unknown-marker witness, because for
   must/may facts a silently-dropped arm folds into a **stronger claim
   than the paths justify**. Write this guard so the marker can be added
   later without re-deriving where the cuts happen: cut in ONE place,
   and leave a comment naming `docs/epics/16-cfg-tier.md` Phase C
   step 3. A cycle guard spread across three call sites is the retrofit
   that epic cannot afford.
4. Owner expansion for the linker (goto-def on a key): **prefer
   index-time expansion** (one def per member) over a
   `HashKeyOwner::Union` enum variant — no serde/rename/match ripple.
   Expand in `rebuild_enrichment_indices`. Record the choice and why in
   the commit message.
5. Cross-file splices defer to enrichment: emit the edge and let the
   registry chase it with a module index, exactly like slot types. **No
   special enrichment pass.**
6. **Acceptance:** merged-literal key goto-def lands on the source
   hash's key def; completion on `$full->{` offers spliced and literal
   keys; the cycle case terminates; `EXTRACT_VERSION` bump.

### Phase D — Part 5a: value-indexed returns

**Goal:** `sub get_config { my ($key) = @_; return $TABLE->{$key} }`
types `get_config('host')` per key.

1. Recognize the two shapes the owner doc names: `return { ... }->{$param}`
   and `my %t = (...); return $t{$param}`. Anything else is an honest
   miss.
2. **Do NOT add `keyed_returns` to `SymbolDetail::Sub`** — the owner
   doc predates bag-canonical, and CLAUDE.md's "the bag is the only
   source of types" wins. Follow `UnionOnArgs`: a
   `ReturnExpr::KeyedOnFirstArg(HashMap<String, InferredType>)` payload
   on `Symbol(sid)`, mirrored to `PackageSymbol` by the existing
   writeback. Verify the writeback mirrors `ReturnExpr` payloads; if
   not, that is a writeback gap to fix **generically**, not to
   special-case.
3. Hint threading: `ReducerQuery` carries `arity_hint`; add
   `first_arg_lit: Option<String>` beside it, populated at the SAME
   call sites. `ReturnExprReducer` dispatches on it; no hint or unknown
   key falls through to the agreement of the table's value types, else
   `None`.
4. Serde: `ReturnExpr` rides the cache blob — `EXTRACT_VERSION` bump.
5. **Acceptance:** literal-arg call types per key; unknown key hits the
   agreed-or-None fallback; no-arg call unchanged; a method-form test.
   One gold row if the substrate exhibits the idiom (grep for it —
   config tables are likely); if none, unit tests suffice, say so.

### Phase E — verification + docs

Full gate; update `prompt-type-inference-residual.md` (1, 2, 5a landed
with pointers), the epics README's coverage map, and the
instance-brands PARKED note in `prompt-graph-walking.md`.

**Be accurate about what that note still says.** Instance brands have
TWO gates, not one: this value tier, *and* the CFG tier's P1 obligation
— "brand = the birth-site of the instance, and the birth-site rule is
this chase in provenance mode", with binding-keyed `Place`s seeing
through the aliasing that killed the spike's spelling keys, and φ joins
folding multi-arm receivers to an honest may-set of birth sites
(`prompt-cfg-tier.md` §8.1; the xref is already in
`prompt-graph-walking.md`). So this epic **shrinks** the prerequisite
list, it does not empty it. Say what remains: constructor/field value
flow, the CFG tier's P1 chase, the brand-key design round
(`(birth-site, Option<home>)`), and the dispatch-gating consumer.

## Language-pack beat

**The hash-key and structural-shape lanes are the engine's, not
Perl's — and this epic is where that gets tested.**

`HashWithKeys`, `Projected` drills, `HashKeyAccess`/`HashKeyDef`/
`HashKeyOwner` and `SharedKeys` all live in the Model layer and are
reached by any language whose extractor mints keyed-container
witnesses. A pack language with string-keyed maps gets Part 2's union
behavior for free *if* the union is expressed as edges between
attachments — and gets nothing if it is expressed as a Perl-visitor
special case.

Obligations:

1. **Phase C's emitter is Perl-side (rule #1, the anon-hash visitor) but
   its ENCODING must be language-neutral.** The splice edge is
   "attachment → attachment"; the openness flag is a property of the
   shape. Neither should mention Perl. A pack language's extractor
   mints the same shape from its own syntax and inherits the reducer.
2. **Phase D is the opposite case, and should say so.** "A sub whose
   return expression is a literal table subscripted by its first param"
   is a *grammar* shape recognized in the Perl walk. Its ENCODING
   (`ReturnExpr::KeyedOnFirstArg` + the `first_arg_lit` binder) is
   neutral, so a pack language could publish it later from its own
   recognizer. Add the binder to `ReducerQuery` in the neutral way —
   beside `arity_hint`, not behind a Perl check.
3. **`first_arg_lit` on `ReducerQuery` is a cross-language change.**
   Every reducer sees it. Confirm `cargo test --features cpp` and the
   cpp gold rows: a new binder that silently changes which conclusion
   a bake can represent is the failure mode, and it is invisible to
   Perl tests.
4. Part 1's completion merge goes through the CandidateSet's
   `complete()` — which pack languages already use for their symbol-
   table completion. A merge added there is inherited; a merge added in
   the Perl completion handler is not. Put it in the projection.

## Scaling beat

Three distinct costs, and they land in different places.

**1. The bag grows, and the bag rides the cache blob.**
Every witness added here is serialized per file into `modules.db`
(bincode+zstd). CPAN-5k: 138,822 files, 1.73 GB, 13.9 KB/file
(2026-08-17). Part 2 adds an edge per splice and Part 5a adds a table
per recognized sub — both bounded by syntax, so the expected delta is
small, but **measure it**: re-run a cold index on Koha and report the
`modules.db` size and per-file cost delta in the PR. A blob-size
regression is a warm-start regression for every user.

**2. Part 2's chase is a graph walk with a cycle risk.**
The `$a`/`$b` mutual-splice case is in the acceptance criteria for a
reason. Use the existing `QueryState` visited set — do not add a second
one. And note the interaction with the conclusion layer: a splice edge
whose source is cross-file residualizes as a `Link`, which is the
representable case; a splice that cannot be represented bakes as
`Open`, which costs a full decode at every consult. `OpenReason`'s
own tally exists to measure exactly this — check the bake's open-reason
distribution before and after (`docs/adr/conclusion-layer.md`).

**3. Completion payload is a measured, previously-regressed axis.**
`prompt-scale-validation-hitlist.md` Tier 1 #4: completion at 138k
files was **7.29 MB / 236 ms per keystroke**, fixed to 55.9 KB / 4 ms
(`b6312ea2`). Phase A adds observed-write keys to a completion list.
That is precisely the shape that regressed.

- The gold `exact_labels` / `max_items` rows are the functional guard.
- The **payload** guard is separate and also required: run
  `bench/lsp_bench.py` with a completion scenario and report the
  per-response byte size, three runs, dated
  (`bench/editor-baseline.sh` is the wrapper; quiet box only). If the
  merge inflates a response measurably, gate it — prefix-matched
  candidates only, `is_incomplete: true`, or observed keys behind a
  config flag.

**4. Phase B adds a diagnostic**, which means `--check` pays for it on
every workspace file. It is opt-in, so the default cost is zero; keep
it that way until an audit justifies promotion (the ladder in
`adr/narrowing-diagnostics.md`).

## Invariants that MUST survive

- Bag-canonical: production is `bag.push`, consumption is the registry.
  No side table of types — this kills the owner doc's `keyed_returns`
  field idea.
- Monotone witnesses; clear-and-emit for anything a fold pass
  re-derives; new source tags into `witnesses::tags`.
- Edges, not values, for anything reachable through an attachment.
- Rule #10: nothing keys on hash names, sub names, or "looks like a
  config table". The two recognized shapes are grammar shapes, not name
  shapes.
- Completion additions always ship with a noise-guard gold row AND a
  payload measurement.

## Verification gate

`cargo test` (both feature sets) · gold 0 FAIL / 0 XPASS · `./e2e/run.sh` ·
substrate audit at parity (completion changes do not show there — hence
the gold rows) · cold-index blob-size delta on Koha · completion payload
bytes, three runs, dated.

## Sizing & sequencing

A (small) → B (small) → C (large) → D (medium) → E. A/B are the warm-up
and independently shippable; C and D are independent of each other. C
may want two commits (emitter, then reducer + linker).
