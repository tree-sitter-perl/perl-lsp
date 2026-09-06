# Epic 17 — Builder decomposition: parts that name one concern

> **Status:** scheduled. Tech debt, behavior-frozen throughout — no
> phase here may change a single answer.
> **Design owner-doc:** none, and it does not need one. CLAUDE.md
> already states the rule this epic applies: *"Oversized modules are
> directories of focused parts behind an unchanged public path
> (`crate::build::builder`, `crate::model::file_analysis`, …); `mod.rs`
> holds the type definitions + entry points and glob-re-exports its
> parts, so item paths never encode the internal file layout."*

## Mission

`build/builder/` was already split once, and the parts have grown back
into grab-bags. The split rule is per-concern, not per-line-count, and
several parts now carry three or four unrelated concerns that happen to
have been extracted on the same day.

The cost is not aesthetic. A part whose name does not predict its
contents is a part nobody greps before adding to, which is how the next
concern lands in it too. The isa/constraint work is the worked example:
core's `map_isa_to_type` sat in `frameworks.rs` beside Moo accessor
synthesis and Sub::Exporter modeling, and duplicated the plugin's
vocabulary for long enough that 12 of Moose's registered types typed as
nonexistent classes. Nobody looking at "frameworks" had reason to think
a type table lived there.

## Current shape (measure again before starting — these move)

```
2161  fold.rs
1722  visit_calls.rs
1715  visit_use.rs
1661  visit_decl.rs
1373  pattern_dispatch.rs
1310  frameworks.rs
1039  pipeline.rs
 954  emit.rs
```

`frameworks.rs`, 31 functions, at least four concerns:

| lines | concern |
| --- | --- |
| 16–217 | class-relationship verbs (`requires`, `extends`, `load_components`) + the accessor-witness plumbing |
| 218–649 | `visit_has_call` — **432 lines in one function**, the Moo/Moose/Mojo attribute synthesis |
| 650–900 | option/value-shape extraction helpers, and the constraint parameter walk |
| 919–1220 | Sub::Exporter modeling — 9 functions, zero entanglement with anything above |
| 1221–end | the isa/constraint fold and its re-parse path |

## Phase breakdown

Each phase is one commit, and each is a **pure move**. The verification
bar below is what makes that claim checkable rather than asserted.

### Phase A — `exporters.rs`

The strongest case and the one with no argument against it: nine
Sub::Exporter functions (`detect_sub_exporter_use` through
`detect_exporter_setup_call`) that share no state and no vocabulary
with attribute synthesis. They are in `frameworks.rs` because runtime
exporter modeling arrived while someone had that file open.

### Phase B — `constraints.rs`

The isa/constraint machinery: `isa_type_of`, `isa_type_of_string`,
`fold_constraint_tree`, `walk_constraint_params`,
`constraint_param_nodes`, `constraint_param_for`,
`extract_constraint_params`, `find_constraint_call`,
`bare_identifier_text`.

**This part earns its name by what it will NOT contain.** The gate that
is scheduled to leave — `type_constraint_names()`, the `Builder` field,
`constraint_name_imported`, the pre-lookup arm in `expr_payload` — lives
in `emit.rs` and `mod.rs` and stays there. What moves here is what core
permanently owns, because Moose type strings never touch imports and so
can never be served by a minted symbol (`prompt-type-constraint-flow.md`).
Putting the permanent half in a named part makes the departing half
visibly not part of it.

### Phase C — `visit_has_call` is not one function

432 lines with a `match mode` spanning most of it. Split by what the
arms actually do — attribute-name extraction, the isa projection, the
per-flavor accessor synthesis, the constructor-key emission — not by
cutting it at a line count. This is the phase most likely to surface a
real tangle; if an arm cannot be lifted without threading six
parameters, that is the finding, and it belongs in the commit message
rather than being forced.

### Phase D — the rest, only where a name is available

`fold.rs` at 2161 lines is the largest part, and the candidates are
visible (`bag_query_*` is a query surface; the chain-typing pass is a
concern; `emit_*_witnesses` are re-emittable fold passes). Do **not**
split it just because it is biggest — split it where a part can be
named for a concern, and stop where it cannot. A part called
`fold_misc.rs` is worse than a long file.

Same test for `visit_calls.rs` / `visit_use.rs` / `visit_decl.rs`:
propose a name first; if the name is a list, the split is wrong.

## Non-goals

- **No behavior change.** Not a fix, not a rename of a public item, not
  a signature change that "obviously" improves something. Those are
  separate commits before or after, never inside a move.
- **No new public paths.** `mod.rs` glob-re-exports its parts, so
  `crate::build::builder::X` keeps working and no call site changes.
- Splitting for line count alone (see Phase D).
- Touching `model/file_analysis/` or `index/` — same disease may exist
  there; different epic, different audit.

## Language-pack beat

**Perl-only, and the boundary is worth stating because it is the one
thing a decomposition can quietly get wrong.**

`build/builder/` is the Perl tree consumer. Pack languages do not walk
trees — they extract through `build/query_extract/` with a `.scm`
skeleton and predicates, and share the engine from `FileAnalysis`
inward. So no part created here is reachable from a pack language, and
none should be given a name that suggests otherwise (`constraints.rs`
under `builder/` is Perl's isa handling; a pack language's type
annotations are its skeleton's business).

The one live risk: a "shared-looking" helper lifted into a new part and
later reached for by pack code because its name reads generic.
`src/layering_tests.rs` enforces the layer DAG and the model's
Point-only tree-sitter surface, so a cross-layer reach fails
`cargo test` — but nothing catches a Perl *semantic* wearing a general
name. Keep the parts named for Perl concerns.

## Scaling beat

**A pure move should cost nothing, and the way to know is to measure
rather than assume.**

- **Compile time is the real risk, not runtime.** Rust compiles a crate
  as a unit; more files do not slow it, but a badly-placed `pub(super)`
  can widen inlining decisions. Record `cargo build --release` wall
  before and after the epic, three runs, dated. If it moves, say so.
- **`--timings` tail on the substrate unmoved** beyond noise, per phase.
  A move that changes a hot path's inlining is still a move that
  changed something.
- No `EXTRACT_VERSION` bump — no FA shape or bag rule changes. If a
  phase thinks it needs one, that phase is not a pure move.
- Nothing here touches residency, caches, or the query paths, so the
  corpora are not the instrument; the substrate audit is.

## Verification gate

The bar is higher than a normal epic's, because "pure move" is a claim
and claims get checked:

- `cargo test` and `cargo test --features cpp`, unchanged counts.
- gold 0 FAIL / 0 XPASS built `--features cpp`, `lang-skip 0`.
- `./e2e/run.sh`.
- **Substrate audit at EXACT parity, per code, every phase.** Not
  "no failures" — identical counts. A move that changes a count changed
  behavior.
- **`git diff --stat` should be near-symmetric** (lines out of one file,
  the same lines into another). A phase whose insertions materially
  exceed its deletions rewrote something; either justify it in the
  commit message or split it out.

## Sizing

A is small and self-contained. B is small. C is medium and is where the
surprises are. D is open-ended by design — take the parts that name
themselves and stop.
