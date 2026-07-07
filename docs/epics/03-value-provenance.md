# Epic 3 — Value provenance, tier 1 (residual Parts 1, 2, 5a)

> **Status:** scheduled, third. Independent of Epics 1–2 in code, but
> run it last: it is the largest, and Epic 2's audit tooling habits
> (before/after substrate diffs) are the safety net you will want.
> **Design owner-doc:** `docs/prompt-type-inference-residual.md`
> (Parts 1, 2, 5a — read the whole doc; Parts 3, 4, 5b, 7 are NOT in
> this epic).
> **Strategic payoff:** this tier is the named gate for un-parking
> instance brands (`docs/prompt-graph-walking.md` §PARKED) and for the
> untyped-receiver residual (`docs/prompt-method-resolution-residuals.md`
> §4). Do not build those here — build the tier they wait on.

## Mission

Three fact classes that trace VALUES (not just declarations) through
the program, each landing as **an emitter + reducer pair on the witness
bag** with no `InferredType` enum expansion:

1. **Part 1 — invocant mutations, consumer wiring.** The facts exist
   (`mutated_keys_on_class`, `SlotType` folds); nothing user-facing
   consumes them. Wire dynamic-key completion and the ro-write hint.
2. **Part 2 — hash-key unions.** `{ %$defaults, %$overrides }` — keys
   of a merged hash resolve to their source hashes.
3. **Part 5a — value-indexed returns.** `get_config('host')` types from
   the literal-keyed return table.

## Read first, in this order

1. `CLAUDE.md` — "Type inference (witness bag)" and "Worklist
   invariants" in full. This epic lives entirely inside those rules.
2. `docs/adr/bag-canonical.md` — the bag is the only source of types.
3. `docs/adr/structural-shapes.md` — `HashWithKeys`, `Projected`
   drills, mutation extension, the whole-story trust gate. Part 2
   composes with these; do not duplicate them.
4. `docs/adr/return-expr.md` — `ReturnExpr` variants and how
   `UnionOnArgs` dispatches on `arity_hint`; Part 5a adds the same
   pattern on a literal-arg hint.
5. `docs/prompt-long-distance.md` — A4 v2 already landed cross-file
   slot-read narrowing; Part 1 must NOT rebuild it.

## Current state — exact anchors

| Existing piece | Where | Find it |
| --- | --- | --- |
| Class-keyed mutated-key union (read API) | `src/file_analysis.rs` | `grep -n 'fn mutated_keys_on_class' src/file_analysis.rs` |
| Mutation Facts + slot-type seeds (emitters) | `src/builder.rs` | `grep -n 'FACT_MUTATION\|slot_writes' src/builder.rs` (in `populate_witness_bag`) |
| `SlotTypeFold` (typed slot reads, incl. cross-file/ancestry) | `src/witnesses.rs` | `grep -n 'SlotTypeFold' src/witnesses.rs` |
| `$self->{` completion path (where Part 1 wires in) | `src/file_analysis.rs` + `src/backend.rs` | `grep -rn 'hash_key' src/cursor_context.rs src/backend.rs \| grep -i complet` — find where hash-key candidates are gathered; completion SOURCES go through the CandidateSet's `complete()` projections (see `docs/adr/resolution-candidate-set.md` for the honest boundary) |
| Moo `is => 'ro'` knowledge | `frameworks/moo.rhai` + `has_options` | the plugin owns the vocabulary; core sees classified pairs |
| Hash-literal shape builder | `src/builder.rs` | `grep -n 'fn hash_literal_type\|fn visit_anon_hash' src/builder.rs` |
| `HashKeyOwner` enum + linker | `src/file_analysis.rs` | `grep -n 'enum HashKeyOwner' src/file_analysis.rs`; index rebuild in `rebuild_enrichment_indices` |
| Constant folding for string lists | `src/builder.rs` | `grep -n 'declared_constants\|resolve_constant_strings' src/builder.rs` |
| Arity-hint threading (the pattern 5a copies) | `src/witnesses.rs` | `grep -n 'arity_hint' src/witnesses.rs \| head` — how a call-site hint reaches reducers |

## Non-goals — do NOT do these

- No instance brands, no birth-site chase, no `home` qualifiers — that
  is the NEXT epic's candidate, explicitly parked until this one plus
  constructor/field flow exist.
- No Parts 3 (method loops), 4 (map/grep), 5b (already superseded by
  the landed flow-narrowing lattice — verify before touching: the
  narrowing ADR covers `ref…eq`/`defined` regions; Part 5b's table is
  historical), 7 (Rhai reducers).
- No new `InferredType` variants. If a design wants one, the design is
  wrong for this epic — encode as witnesses/owners (the doc's header
  says exactly this).
- No parallel reverse indexes (rule #8) — Part 2's union expansion
  reuses the existing `(target_name, owner)` index machinery.
- `delete $self->{k}` drop-tracking: out (owner doc says ignore).

## Phase breakdown

### Phase A — Part 1: dynamic-key completion

**Goal:** typing `$self->{` inside a class completes the union of
Moo/Moose/framework-declared keys (already works via `HashKeyDef`) plus
every key the class's methods were OBSERVED writing.

1. Find the completion gathering site for `$self->{` (anchors above).
   Where declared `HashKeyDef`s are offered for the invocant class,
   also merge `mutated_keys_on_class(class)`, deduped against the
   declared set, marked with a distinct `CompletionItemKind`/detail
   ("observed write") so users can tell contract from observation.
2. Cross-file: `mutated_keys_on_class` reads the local bag. For a class
   whose methods span files, ask the same question of the cached
   module's FA (mirror how declared keys already cross files — find
   that path first and do it the same way; if declared keys DON'T cross
   files here, do not fix that in this epic, just note it).
3. Ordering guard: completion noise is a regression class. The gold
   harness has `exact_labels`/`max_items` assertion modes
   (`gold-corpus/README.md`) — author one row that pins the union AND
   one that proves an unrelated class does NOT see these keys.
4. **Acceptance:** unit test on a two-method class (one `has`, one
   `$self->{observed} = 1`) → completion contains both, observed one
   flagged; gold rows green.

### Phase B — Part 1: ro-write hint

1. New opt-in diagnostic `roWrite` (follow `DiagnosticOptions` serde +
   `from_cli_args` + docs pattern — grep `optional_deref` for every
   site a flag touches; there are exactly: struct field, CLI flag,
   ADR ladder text, tests).
2. The fact: a `HashKeyAccess` Write (or accessor-method call with args
   — start with the direct slot write ONLY) on `$self->{attr}` where
   `attr` is a Moo/Moose attribute whose `is` is `ro`. The `ro`
   knowledge lives in the plugin: extend the `has` synthesis to record
   read-only-ness on the emitted symbol/HashKeyDef (an EmitAction
   field), NOT a core name-table.
3. Severity HINT, message names the attribute and the declaring `has`.
4. **Acceptance:** unit tests both directions (`ro` flags, `rw`
   silent); substrate audit — expect near-zero hits (direct writes to
   ro-backed slots inside the owning class are usually intentional
   builder patterns like `_build_*`; if the audit shows >10 hits,
   check whether `_build`/BUILD/BUILDARGS contexts need exemption and
   document what you chose).

### Phase C — Part 2: hash-key unions

**Goal:** `my $full = { %$defaults, key => 1 };` — `$full->{host}`
resolves (goto-def/completion) into `$defaults`'s key set.

1. In `visit_anon_hash` (rule #1 — this is the only tree consumer),
   detect splice elements (`%$var` / `%hash` inside the literal — get
   the exact node kinds from `perl-lsp --parse` on a snippet FIRST,
   do not guess).
2. Encode as a UNION-witness, not an eager copy: push
   `HashWithKeys`-shaped structure for the literal keys (existing
   path) PLUS an Edge-per-splice from the literal's shape attachment
   to the spliced variable's attachment. Check `structural-shapes.md`
   for whether `HashWithKeys` already supports an `open` flag — a
   splice makes the shape OPEN (unknown extra keys) even when the
   source resolves; set it.
3. Reducer side: when a `Projected { base, HashKey(k) }` misses the
   literal keys, chase the splice edges (registry materialization
   already chases Edges — confirm the attachment shapes line up, and
   add a cycle guard via the existing `QueryState` visited set;
   the owner doc's `$a = { %$b }; $b = { %$a }` case is the test).
4. Owner expansion for the LINKER (`(target_name, owner)` lookups —
   goto-def on a key): the owner doc offers
   `HashKeyOwner::Union(Vec<HashKeyOwner>)` OR index-time expansion of
   one def per member. PREFER index-time expansion (no enum change
   ripples through serde/rename/match sites); expand in
   `rebuild_enrichment_indices` where owner indexes are already built.
   Record the choice + why in the commit message.
5. Cross-file splices (`%$imported_config`) defer to enrichment —
   emit the edge; if the source is cross-file it resolves when the
   registry chases with a module index, exactly like slot types. Do
   not build a special enrichment pass.
6. **Acceptance:** unit tests: merged-literal key goto-def lands on the
   source hash's key def; completion on `$full->{` offers both spliced
   and literal keys; the cycle case terminates (test with the exact
   `$a`/`$b` shape); `EXTRACT_VERSION` bump if the cached shape grew.

### Phase D — Part 5a: value-indexed returns

**Goal:** `sub get_config { my ($key) = @_; return $TABLE->{$key} }`
with a literal-keyed table types `get_config('host')` per key.

1. Recognition in the walk (rule #1): a sub whose return expression is
   `<hash-literal or literal-initialized my %t>` subscripted by the
   sub's first param. Start with the two shapes the owner doc names:
   `return { ... }->{$param}` and `my %t = (...); return $t{$param}`.
   Anything else is an honest miss.
2. Encoding: do NOT add `keyed_returns` to `SymbolDetail::Sub` (the
   owner doc predates bag-canonical; CLAUDE.md's "the bag is the only
   source of types" wins). Instead follow the `UnionOnArgs` pattern:
   a `ReturnExpr::KeyedOnFirstArg(HashMap<String, InferredType>)`
   payload pushed on `Symbol(sid)` (and mirrored to `MethodOnClass` by
   the existing writeback — verify it mirrors ReturnExpr payloads; if
   not, that is a writeback gap to fix generically, not to special-case).
3. Hint threading: `ReducerQuery` carries `arity_hint`; add
   `first_arg_lit: Option<String>` beside it, populated at the SAME
   call sites that populate arity (grep `arity_hint` construction
   sites; `method_call_arity` shows how call-site facts are recorded).
   `ReturnExprReducer` dispatches `KeyedOnFirstArg` on it; no hint or
   unknown key → fall through to the agreement of the table's value
   types if they agree, else None.
4. Serde: `ReturnExpr` rides the cache blob — bump `EXTRACT_VERSION`.
5. **Acceptance:** unit tests: literal-arg call types per key; unknown
   key → the agreed-or-None fallback; no-arg call unchanged; a
   method-form test through `MethodOnClass`. One gold row on the
   substrate if a real module exhibits the idiom (search the substrate
   with grep — Mojo and Plack config tables are likely candidates; if
   none found, unit tests suffice, say so in the PR).

### Phase E — verification + docs

1. Full gate: `cargo test`, gold harness (0 FAIL / 0 XPASS or promote),
   substrate diagnostic audit vs a pre-epic binary (always-on parity;
   completion changes don't show there, hence the gold completion rows
   in Phases A/C).
2. Update `docs/prompt-type-inference-residual.md` (mark 1, 2, 5a
   landed with pointers), `docs/ROADMAP.md`, and the instance-brands
   PARKED note in `docs/prompt-graph-walking.md` — its prerequisite
   list shrinks to "constructor/field value flow", which becomes the
   candidate Epic 4.

## Invariants that MUST survive

- Bag-canonical: type production is `bag.push`, consumption is the
  registry. No side-table of types (this kills the owner doc's
  `keyed_returns` field idea — see Phase D step 2).
- Monotone witnesses + clear-and-emit for anything a fold pass
  re-derives; new source tags into `witnesses::tags`.
- Edges, not values, for anything reachable through an attachment.
- Rule #10: nothing keys on hash names, sub names, or "looks like a
  config table" heuristics beyond the two recognized syntactic shapes,
  which are grammar shapes, not name shapes.
- Completion additions always come with a noise-guard gold row
  (`exact_labels`/`max_items`).

## Sizing & sequencing

A (small) → B (small) → C (large) → D (medium) → E. A/B are the warm-up
and independently shippable; C and D are independent of each other.
Each phase is one reviewable commit; C may want two (emitter, then
reducer+linker).
