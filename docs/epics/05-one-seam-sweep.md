# Epic 5 — One-seam sweep: magic tokens + the cst/conventions backlog

> **Status:** scheduled (5th). Small, self-contained, zero unlanded
> prerequisites — the best warm-up epic for a fresh implementer.
> **Design owner-docs:** `docs/prompt-magic-tokens.md` (whole),
> `docs/prompt-cst-migration.md` (ranked backlog items 1–5 and 7).

## Mission

Two flavors of one discipline — *encode the shape once, every consumer
asks the value*:

1. **Magic compile-time tokens** (`__PACKAGE__`, `__SUB__`, `__FILE__`,
   `__LINE__`) resolve to typed values in the canonical expression
   machinery, so dispatch / column-keyed args / goto-def / rename work
   on them with **no per-consumer handling**.
2. **The cst/conventions migration backlog, items 1–5 + 7** — the
   remaining places that re-derive shapes `cst.rs` / `conventions.rs`
   already own or should own.

## Read first

1. `CLAUDE.md` rule #1 (the `cst.rs` paragraph) and rule #10.
2. `docs/prompt-magic-tokens.md` — the token/value table and the
   single-seam fix shape. Note its verified non-gap: `__PACKAGE__->search`
   on a Result class is an ERROR in DBIC, so NOT linking it is correct.
3. `docs/prompt-cst-migration.md` — the ranked list. Items 1–5 and 7
   are this epic; item 6 (the ~400-poke long tail) is a standing
   strangler rule, NOT schedulable.

## Phase breakdown

### Phase A — `__PACKAGE__` uniform resolution

1. Anchors: `grep -rn '__PACKAGE__' src/build/builder/ src/model/conventions.rs`
   — the constructor / bless / classdata paths already mint
   `ClassName(current_package)`; the gap is uniformity.
2. Emit an `Expr(span)` witness with
   `InferredType::ClassName(current_package)` for the token in
   `expr_payload`. **Verify the node kind with `perl-lsp --parse` on a
   snippet first** (it parses as a `func0op_call_expression` with text
   `__PACKAGE__`). `invocant_type_at_node` gets the same answer through
   its existing func0op arm or a sibling — ONE resolution rule, both
   entry points.
3. Consumers must NOT grow token checks. The test of success:
   `__PACKAGE__->new({ name => 1 })` in a DBIC Result class links the
   `name` arg key to the column (the column-keyed seam reads the
   invocant type and never sees the token), and `__PACKAGE__->my_method`
   resolves goto-def / references / rename.
4. **Acceptance:** unit tests for both, plus hover type on the token;
   grep proof that no consumer outside the two typing entry points
   mentions `__PACKAGE__` (except `conventions.rs`, which owns
   recognition).

### Phase B — `__SUB__`, `__FILE__`, `__LINE__`

1. `__SUB__` → `InferredType::CodeRef { return_edge }` pointing at the
   enclosing sub's symbol (the same shape `coderef_return_edge_for`
   builds — grep it). Test: `__SUB__->(@args)` inside a sub types its
   return as the sub's own; goto-def on the token lands on the sub.
2. `__FILE__` → `String`, `__LINE__` → `Numeric` — hover/type only.
3. **Acceptance:** one unit test each.

### Phase C — backlog item 1: the `$self` short-circuit

`invocant_type_at_node`'s literal `"$self"` check is the last
invocant-name site not routed through `is_conventional_invocant_name`,
so `$class` / `$proto` invocants miss it. One-line fix + a
`$proto->method` regression test.

### Phase D — backlog item 2: positional-receiver node predicate

`is_shift_call` / `is_positional_receiver` answer the node-level version
of `InvocantText::PositionalReceiver`. Move the node-shape predicate into
`cst.rs` (`is_positional_receiver_node(node, src)`); the builder keeps
only `shift_is_invocant_here`'s context sensitivity. Pure move — all
existing tests green.

### Phase E — backlog item 3: one text→class resolver

`invocant_text_to_class`, `resolve_invocant_class`, and cursor_context's
`resolve_text_invocant` are three near-duplicates with different
fallbacks. Collapse to one `FileAnalysis` seam; `cursor_context`
composes it (rules #3/#5). **Before collapsing, table the three fallback
behaviors** (`package_at` vs scope-chain vs analysis-optional) and
preserve each caller's semantics — write the table into the commit
message. If the fallbacks genuinely conflict, the seam takes a small
options enum; do NOT silently pick one.

### Phase F — backlog item 4: one string-value extractor

`extract_node_string`, `extract_string_content`, `extract_key_text` (+
`arg_info_for`'s inline copy) overlap. `cst.rs` gains
`string_value(node, src) -> Option<(String, Span)>` encoding the
quote-flavor trap (the `string_content` child; empty literals have
none). Migrate the callers. `extract_key_text` also returns an
`is_dynamic` flag — keep that at its call site, composed on top.

### Phase G — backlog items 5 + 7

- Route `for_each_has_option_pair` and the export-pair detectors through
  `pair_nodes` (they pre-date it). **Remember the fat-comma rule**:
  pair walking is positional and separator-agnostic; gating on `=>`
  silently drops the plain-comma spelling, which is the exact bug that
  hid `use constant { 'GAMMA', 3 }`.
- Add `typed_node!` wrappers as far as the visitors touched in C–F
  warrant: `SubDecl`, `VariableDecl`, `Assignment`, `AnonHash`,
  `UseStatement`. **Only wrap what a migrated call site actually
  uses** — wrappers without consumers are dead weight.

## Non-goals

- Item 6's ~400-poke long tail (strangler rule only).
- Anything DBIC-shaped (Epic 2 owns it).
- `__DATA__` / `__END__` (section markers, not values).

## Language-pack beat

**`cst.rs` is Perl's typed CST view, and it must stay that way.**

The engine has two tree-consuming worlds and they are deliberately
separate: Perl walks the tree through `cst.rs` inside `build()`, and
pack languages extract through queries (`build/query_extract/`) with a
`.scm` skeleton plus predicates — no per-language Rust types. This epic
is entirely in the first world, and the correct answer to "does this
generalize?" is **no, and here is the boundary**:

- `cst.rs`'s `typed_node!` wrappers, `NodeExt`, `pair_nodes` and
  `call_args` encode *tree-sitter-perl* grammar traps. A pack language's
  equivalent trap is encoded in its `.scm` skeleton and predicates.
  Adding a wrapper here does not help C++ and is not supposed to.
- `model/conventions.rs` is likewise Perl name semantics, pure `&str`,
  importable by tree-free layers. It is not a cross-language name
  vocabulary.
- The one genuinely shared piece is the **`Slot` taxonomy**
  (`lsp/cursor_slot.rs`) — one vocabulary over both `cursor_context`
  (Perl) and `cursor_sentinel` (pack). Phase E touches
  `cursor_context`'s `resolve_text_invocant`. **Do not let the collapsed
  seam grow a language branch**; if the unified resolver needs to know
  what it is resolving for, it takes a `Slot`, not a language id.
- Magic tokens (Phases A–B) are the same story from the other side:
  `__PACKAGE__` is a Perl token, but "a compile-time token whose value
  is the enclosing class" is a shape several languages have. The
  ENCODING — an `Expr(span)` witness carrying `ClassName` — is already
  neutral, and that is the whole point: consumers ask the value, and
  the value's language is irrelevant to them. Nothing further to
  generalize.

**One real check:** `src/layering_tests.rs` enforces the model's
Point-only tree-sitter surface and single-point grammar access. Phases D
and F move code *into* `cst.rs`. Run the layering tests — a predicate
moved to the wrong side fails `cargo test`, which is the design working.

## Scaling beat

**This epic is behavior-neutral by construction, which makes its
scaling obligation unusually simple: prove nothing moved.**

- Phases C–G are refactors. Their scaling beat is the **substrate audit
  at exact parity, per code** — not merely "no failures". A pure move
  that changes a count changed semantics.
- Phases A–B *do* emit new witnesses (one per magic token occurrence).
  That is bounded by source syntax and tiny, but it is an
  `EXTRACT_VERSION` bump, which costs every user a cold re-index —
  ~10.5 minutes at CPAN-5k scale (2026-08-17). Land A and B under one
  bump.
- The one place a "pure move" can genuinely cost: Phase F's
  `string_value` gets called from hot walk paths that previously had
  inlined, specialized extraction. If the unified version does more
  work (an extra child lookup, an allocation the inline version avoided),
  it pays that on every string literal in every file. Check
  `--timings` on the substrate for the slowest-modules tail, and use
  `bphase!` if you need per-file attribution — **never a hand-rolled
  `eprintln!` timer**; a per-file region printed per entry cost one
  138k run 3.2M lines and 43 minutes and measured a run that no longer
  resembled the one being measured (`adr/instrument-blindness.md`).
- Phase G's `typed_node!` wrappers are zero-copy by design. If a
  wrapper allocates, it is written wrong.

## Verification gate

`cargo test` (both feature sets) · gold 0 FAIL / 0 XPASS · `./e2e/run.sh` ·
substrate audit at **exact parity per code** for C–G; A–B may move
`unresolved-*` counts DOWNWARD only · `--timings` tail unmoved beyond
noise · `EXTRACT_VERSION` bump for A–B.

## Sizing

Small. A–B one commit each; C is a one-liner; D–G one commit each.
Parallel-safe with the other epics except Phase D touches the
invocant / `@_`-window seam in `builder/infra.rs` — coordinate if Epic 4
is in flight.
