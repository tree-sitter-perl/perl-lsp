# Flow-sensitive narrowing

**Landed.** Decision record: `docs/adr/flow-narrowing.md`.
Executable spec / playground: `test_files/narrowing_playground.pl`.

## Residual forward work

### Subject coverage

- **Direct element places** (`$h{k}`, `$h[0]`) — **landed.**
  `canonical_place_path` now accepts a `container_variable` base (the
  named `%h` / `@h`), keyed on the named hash/array as the root. The
  invalidation scan recognizes whole-container reassignment (`%h = (...)`)
  and slices (`@h{...}`) of the root container as disturbances; slot reads
  (`$h{k}`) through the `container_variable` do not truncate.
- **Dynamic-key places** (`$self->{$k}` where `$k` is a plain scalar) —
  **landed (Option A).** A plain-scalar subscript is a stable place keyed
  by spelling; `place_dynamic_key_vars` collects the key scalars
  (`$self->{$k}{$j}` → `[$k, $j]`) and the region truncates at the first
  reassignment of any of them (`first_subject_write`). Inherits the
  constant-key aliasing conservatism: a write via a *different* key
  spelling that equals `$k` at runtime doesn't truncate — a precision gap,
  never a crash.
  - *Alternative (Option B), not taken:* treat **any dynamic-key write to
    the container** (`$self->{$j} = …`) as an *escape* that re-widens
    every narrowed slot of that container. That closes the aliasing hole
    (sound) at the cost of precision (over-truncates when the dynamic
    write hit a different key), and applies to constant-key places too.
    It is the general soundness-vs-precision knob for slot narrowing.
    Option A was chosen for parity with the existing constant-key
    conservatism (the whole feature already accepts dynamic-aliasing
    imprecision); flip to B if a motivating soundness case appears — it is
    a localized change to `first_place_invalidation` (scan for *any*
    dynamic-key write to the container, not just the matching spelling).
- **Accessor places** (`$self->name`) — parked. An accessor isn't a
  stable slot (it can return a different object per call, with side
  effects), so soundness needs a stricter no-call-between-guard-and-use
  model.

### Guard recognition

- **Const-folded class-name guards** (`$x->isa($CLASS)` with a folded
  `$CLASS`) — **landed.** `recognize_guards` / `recognize_isa_guard` now
  take `&Builder`, and on a non-literal scalar arg consult
  `Builder::folded_class_name_arg` (the same `resolve_constant_strings`
  fold invocant resolution uses). A multi-valued fold can't name one
  dispatch class, so it stays wide.

### Negation

- **elsif-chain cumulative negation** — **landed.** `narrow_block_guard`
  walks the full `if` / `elsif`* / `else` chain. Each arm narrows by its
  own condition (positive) plus the cumulative negation of every
  preceding condition (the arm runs only when all priors fell through).
  Only representable negations survive — `defined`/`blessed` complement to
  `Undef`, `isa`/`ref-eq` have none and stay wide — so the "intersection
  across conditions" is automatic: non-representable negations contribute
  nothing. An arm's own guard wins over a prior negation on the same
  subject (dedup by source spelling), so a genuinely-unreachable arm
  doesn't emit contradictory witnesses.
- **General `Not` / `Difference` negation** — parked: no positive lookup
  target, no consumer value. "Not Foo" has nothing to dispatch on.

### Consumption

- **Completion peels `Optional`** — **landed.** `complete_methods` for a
  `$x->` receiver typed `Optional<Foo>` offers `Foo`'s methods via
  `InferredType::completion_class_name` (recursive optional peel). This is
  a *completion-only* leniency: dispatch / hover / goto-def still refuse
  an unguarded optional (`class_name()` stays `None`), since an optional
  is not *definitely* an instance — but the author may simply not have
  written the `defined` guard yet, so completion stays suggestive. The
  peel lives in the `symbols.rs` adapter, not the sound query layer; a
  future `initializationOptions.completion.peelOptionals` flag (mirroring
  `diagnostics.unresolvedDispatch`) could gate it if it ever proves noisy.

Diagnostics the lattice now enables: `docs/prompt-narrowing-diagnostics.md`.
