# Epic 4 — One-seam sweep: magic tokens + the cst/conventions backlog

> **Status:** scheduled (4th). Small, self-contained, zero unlanded
> prerequisites — a good warm-up epic for a fresh implementer.
> **Design owner-docs:** `docs/prompt-magic-tokens.md` (whole thing),
> `docs/prompt-cst-migration.md` (ranked backlog items 1–5 and 7).

## Mission

Two flavors of the same discipline — "encode the shape once, every
consumer asks the value":

1. **Magic compile-time tokens** (`__PACKAGE__`, `__SUB__`, `__FILE__`,
   `__LINE__`) resolve to typed values in the canonical expression
   machinery, so dispatch / column-keyed args / goto-def / rename work
   on them with **no per-consumer handling**.
2. **The cst/conventions migration backlog, ranked items 1–5 + 7** —
   the remaining places that re-derive shapes `cst.rs`/`conventions.rs`
   already own or should own.

## Read first

1. `CLAUDE.md` rule #1 (the cst.rs paragraph) and rule #10.
2. `docs/prompt-magic-tokens.md` — the token/value table and the
   "single seam" fix shape. Note its verified non-gap:
   `__PACKAGE__->search` on a Result class is an ERROR in DBIC, so NOT
   linking it is correct.
3. `docs/prompt-cst-migration.md` — the ranked list. Items 1–5 and 7
   are this epic; item 6 (the ~400-poke long tail) is a standing
   strangler rule, NOT this epic.

## Phase breakdown

### Phase A — `__PACKAGE__` uniform resolution

1. Anchors: `grep -n '__PACKAGE__' src/builder.rs | head -30` — the
   constructor/bless/`mk_classdata` paths already mint
   `ClassName(current_package)`; the gap is uniformity.
2. Emit an `Expr(span)` witness with
   `InferredType::ClassName(current_package)` for the token in
   `expr_payload` (it parses as `func0op_call_expression`, text
   `__PACKAGE__` — verify with `perl-lsp --parse` on a snippet first).
   `invocant_type_at_node` gets the same answer through its existing
   func0op arm or a sibling — ONE resolution rule, both entry points.
3. Consumers must NOT grow token checks. The test of success:
   `__PACKAGE__->new({ name => 1 })` in a DBIC Result class links the
   `name` arg key to the column (the column-keyed seam reads the
   invocant type and never sees the token), and
   `__PACKAGE__->my_method` resolves goto-def/references/rename.
4. **Acceptance:** unit tests for the two cases above + hover type on
   the token; grep proof that no consumer outside the two typing entry
   points mentions `__PACKAGE__` (except `conventions.rs`, which owns
   recognition).

### Phase B — `__SUB__`, `__FILE__`, `__LINE__`

1. `__SUB__` → `InferredType::CodeRef { return_edge }` pointing at the
   enclosing sub's symbol (the same `return_edge` shape
   `coderef_return_edge_for` builds — grep it). Test:
   `__SUB__->(@args)` inside a sub types its return as the sub's own
   return type; goto-def on the token lands on the sub.
2. `__FILE__` → `String`, `__LINE__` → `Numeric` — hover/type only.
3. **Acceptance:** one unit test each.

### Phase C — cst backlog item 1: the `$self` short-circuit

`invocant_type_at_node`'s literal `"$self"` check is the last
invocant-name site not routed through
`is_conventional_invocant_name` (`$class`/`$proto` invocants miss the
short-circuit). One-line fix + a `$proto->method` regression test.

### Phase D — cst backlog item 2: positional-receiver node predicate

`is_shift_call` / `is_positional_receiver` (builder) answer the
node-level version of `InvocantText::PositionalReceiver`. Move the
node-shape predicate into `cst.rs`
(`is_positional_receiver_node(node, src)`); the builder keeps only
`shift_is_invocant_here`'s context sensitivity. All existing tests
green — this is a pure move.

### Phase E — cst backlog item 3: one text→class resolver

`FileAnalysis::invocant_text_to_class`,
`FileAnalysis::resolve_invocant_class`, and cursor_context's
`resolve_text_invocant` are three near-duplicates with different
fallbacks. Collapse to one `FileAnalysis` seam; `cursor_context`
composes it (rules #3/#5). Before collapsing, table the three
fallback behaviors (package_at vs scope-chain vs analysis-optional)
and preserve each caller's semantics — write the table into the
commit message. If the fallbacks genuinely conflict, the seam takes a
small options enum; do NOT silently pick one.

### Phase F — cst backlog item 4: one string-value extractor

`extract_node_string`, `extract_string_content`, `extract_key_text`
(+ `arg_info_for`'s inline copy) overlap. cst.rs gains
`string_value(node, src) -> Option<(String, Span)>` encoding the
quote-flavor trap (the `string_content` child; empty literals have
none). Migrate the four callers. `extract_key_text` also returns an
`is_dynamic` flag — keep that at its call site, composed on top.

### Phase G — cst backlog items 5 + 7

- Route `for_each_has_option_pair` and the export-pair detectors
  through `pair_nodes` (they pre-date it).
- Add `typed_node!` wrappers as far as the visitors you touched in
  C–F warrant: `SubDecl`, `VariableDecl` (the paren-list trap),
  `Assignment` (the `child_by_field_name("right")`-returns-paren
  trap), `AnonHash`, `UseStatement`. Only wrap what a migrated call
  site actually uses — wrappers without consumers are dead weight.

## Non-goals

- Item 6's ~400-poke long tail (strangler rule only).
- Anything DBIC-shaped (Epic 1 owns it; the cst doc's own "Not cst
  work" list applies).
- `__DATA__`/`__END__` (section markers, not values).

## Verification gate

`cargo test` green; gold harness 0 FAIL / 0 XPASS; behavior-neutral
phases (C–G) additionally: substrate diagnostic audit at exact parity
per code (commands in `docs/epics/01-dbic-phase-3.md` Phase E).
Phases A–B may move counts — only DOWNWARD for unresolved-*.
`EXTRACT_VERSION` bump if any witness emission changed (Phases A–B: yes).

## Sizing

Small. A–B one commit each; C one-liner; D–G one commit each. Fully
parallel-safe with Epics 1–3 except Phase D touches
`shift_denotes_invocant`'s neighborhood — coordinate if Epic 3 is
in flight.
