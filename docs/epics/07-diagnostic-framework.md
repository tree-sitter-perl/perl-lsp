# Epic 7 — The diagnostic framework: PL-codes, config, suppressions, SARIF

> **Status:** scheduled (7th). The CI-readiness gate.
> **Design owner-docs:** `docs/prompt-cli-tools.md` §"Diagnostic
> framework" (codes table, config JSON, suppression comments, SARIF) and
> `docs/prompt-config-schema.md` (whose "forcing function" section says
> THIS epic is the moment its deferred pieces land).

## Mission

`--check` and the LSP diagnostics grow a real framework: stable
diagnostic codes, per-code severity config from `.perl-lsp.json` + LSP
`initializationOptions`, in-source comment suppressions, and SARIF
output for CI annotation. The config-schema doc's deferred pieces — the
owning `Config` struct and the generated editor schema — land here
because per-code objects are their named forcing function.

## Read first

1. `docs/prompt-cli-tools.md` — the PL-code table, config JSON shape,
   suppression grammar, SARIF.
2. `docs/prompt-config-schema.md` — WHOLE doc; it prescribes the
   `Config` shape (own at top, pass slices) and the schemars plan, and
   warns off the `define_options!` macro.
3. `docs/adr/narrowing-diagnostics.md` — the existing flag ladder. The
   framework must EXPRESS it, not replace its semantics.
4. `grep -n 'struct DiagnosticOptions' -A 20 src/lsp/symbols/diagnostics.rs`
   and the `cli_flags_match_diagnostic_option_fields` drift test — the
   pattern every config surface here must keep.
5. `docs/adr/config-superposition-declarations.md` — the landed
   declaration model; do not contradict it.
6. `docs/prompt-cfg-tier.md` §7 ("Consumers this is built for") — the
   automation tier this framework is the delivery vehicle for. Three of
   its four items are packaging decisions **this epic makes**, and two
   of them are cheap now and expensive later. See the ordering
   constraint below.

## Two forward constraints from the CFG tier

`prompt-cfg-tier.md` §7 names the automation tier these seams deliver,
and it calls SARIF "the highest leverage-to-effort item on the list".
Two of its requirements are representation decisions that cost nothing
to honor now and are painful retrofits:

1. **Finding fingerprints key on `(rule, function symbol, Place path)`
   — never line numbers.** Baselines and inline suppressions ride the
   fingerprint, so a code edit that shifts a line must not orphan a
   suppression. `Place` exists now (Epic 16 Phase B landed it in
   `model/file_analysis/place.rs`), so a fingerprint can carry it from
   the start. Phase C's suppression key and Phase D's SARIF
   `ruleId`/location shape must let it be the key **without a format
   break**. Concretely: do not make the line number part of the
   suppression's identity, and leave the SARIF result shape room for a
   `partialFingerprints` entry.
2. **must/may is a structured confidence field, not a severity.** The
   brief's framing: "must gates CI, may informs review — the zero-FP
   discipline and a Klocwork-style recall mode become one engine at two
   thresholds, and **the finding says which it is**." Severity and
   confidence are different axes; collapsing them into one enum in
   Phase B's config is the retrofit. Reserve the axis even though
   nothing populates it yet.

The fourth item, differential CI, is already built — Surface equality +
`dirty_consumers` + the persisted `modules.db` artifact — so this epic
inherits it rather than owing it anything.

## Ordering constraint

**Do NOT renumber or re-key existing diagnostics' string codes**
(`unresolved-function`, `undef-deref`, `optional-deref`, …) — editors
and gold rows key on them. PL-codes are an ADDITIONAL stable alias
(SARIF `ruleId` + suppression key). The LSP `code` field may carry
`PLxxx` with the descriptive name in `codeDescription`/message — decide
ONE presentation and write it in the ADR.

Extend the table with a row for **every diagnostic that exists today**
(the narrowing family: `undef-deref`, `optional-deref`,
`redundant-guard`, `contradictory`, `deref-shape`; `helper-not-loaded`;
`composer-mismatch`; the C++ `use-after-move` channel) **before adding
any new lint.** Registering the existing surface is Phase A; new lints
are LAST.

## Phase breakdown

### Phase A — the registry + codes for existing diagnostics

1. New `src/lsp/diagnostics.rs` (add it to `src/layering_tests.rs`'
   layer map — an `.rs` outside a layer directory fails the walk, so
   placing the file places it in the architecture). A static registry:
   ```rust
   struct DiagnosticCode {
       pl: &'static str, name: &'static str,
       default_severity: Severity, default_enabled: bool,
       languages: LanguageApplicability,   // see the Language-pack beat
   }
   ```
   Every `Diagnostic { code: … }` construction site routes through it
   (`grep -n 'NumberOrString::String' src/lsp/symbols/`).
2. A drift test in the spirit of
   `cli_flags_match_diagnostic_option_fields`: every emitted code string
   appears in the registry, and PL numbers are unique.
3. **Acceptance:** `--check` output unchanged by default, except each
   diagnostic now also carries its PL code.

### Phase B — `Config` + `.perl-lsp.json` + per-code severity

1. Per `prompt-config-schema.md` piece 1: one owning
   `struct Config { diagnostics: DiagnosticsConfig, exclude: Vec<String> }`,
   parsed ONCE (workspace-root file + `initializationOptions` +
   `didChangeConfiguration`, later sources overriding earlier
   field-wise). Backend holds `Arc<RwLock<Config>>`.
   **Call sites keep taking the narrow slice** — the doc is explicit and
   gives the reasons; keep `collect_diagnostics(&cfg.diagnostics, …)`.
2. `DiagnosticsConfig`: per-code `"error"|"warning"|"info"|"hint"|"off"`,
   keyed by either the PL code or the descriptive name (accept both,
   normalize through the registry). The existing `DiagnosticOptions`
   bools become the LEGACY spelling — keep deserializing them so current
   configs keep working; document the mapping.
3. `exclude` globs honored by `--check` and the workspace diagnostic
   pass — **NOT by indexing itself**: resolution still needs excluded
   files' symbols, only reporting is filtered.
4. **Acceptance:** precedence tests (file < init options < didChange),
   per-code override, `"off"`, legacy-bool compatibility; the drift test
   extended to assert every registry row is configurable.

### Phase C — comment suppressions

1. Grammar: `# perl-lsp: ignore(PL001)`, `ignore-next-line(PL001)`,
   `ignore-file(PL004)` (file-form only in the first 10 lines). Accept
   descriptive names in the parens too.
2. Rule #1: comments are scanned during build — collect
   `FileAnalysis` suppressions in the builder's comment handling
   (serde; `EXTRACT_VERSION` bump; the field goes in its lane, and
   `surface_feed` will not compile until its Surface fate is decided —
   suppressions are file-local, not cross-file-visible, so discard with
   a reason). The builder stores the raw code string; registry
   lookup/validation happens at DIAGNOSTIC time, and an unknown code
   gets its own `unknown-suppression` hint from the framework
   (self-hosting).
3. `collect_diagnostics` filters through suppressions before emitting.
4. **Acceptance:** unit tests per form + the unknown-code hint; a gold
   diagnostics row exercising `ignore-next-line`.

### Phase D — SARIF 2.1.0 (`--check --format sarif`)

1. `runs[0].tool.driver` from the registry (rules = registry rows, help
   text from their doc strings); `results` with `ruleId` = PL code;
   locations with 1-based line/col matching `--check`'s existing
   coordinate contract; level from severity.
2. Validate the shape in a test against a golden file — hand-assert
   required fields and enum values; **do not pull a validator
   dependency**.
3. Wire a CI smoke if in scope; if not, note it in the PR.
4. **Acceptance:** golden-file test; `jq` sanity commands in the doc.
5. This discharges the heatmap doc's deferred SARIF note — record that
   in `docs/adr/heatmap.md` (heatmap SARIF stays deferred; only
   `--check` gains it here).

### Phase E — schemars + new lints (only now)

1. `#[derive(schemars::JsonSchema)]` on the config structs +
   `--dump-options-schema`. Field `///` docs become descriptions —
   write them as user-facing.
2. New lints, **EACH its own commit with a before/after substrate
   audit**: PL003 unused-import, PL004 unused-variable, PL007
   shadow-variable, PL010 deprecated-pattern (`use base` → `use
   parent`). PL005 unused-export / PL006 dead-sub **REUSE the heatmap's
   guards verbatim** (`grep -rn 'reachable_guard' src/lsp/cli/heatmap.rs`)
   — a lint that flags what the heatmap shields is a bug. PL008
   missing-import and PL009 circular-dependency: only if the audit shows
   clean signal, else registered-but-default-off with a note.
3. **Acceptance per lint:** unit tests + the substrate hit count in the
   PR + zero hits on the repo's own fixtures unless genuinely justified.

## Non-goals

- The `define_options!` macro (the config-schema doc says the drift test
  is cheaper; it stays).
- `--migrate` and the analysis subcommands (Epic 10).
- Changing any diagnostic's SEMANTICS — this epic is packaging.

## Language-pack beat

**This is the epic that decides whether pack-language diagnostics are
possible, and it is the single most consequential language decision on
the slate.** Get it wrong and Epic 13 has to rebuild the framework.

The state today: pack languages have **no diagnostics by deliberate
policy** — `prompt-multi-language.md` says none exist until a calibrated
substrate does, with the zero-false-positive sweep as the ship gate. The
one exception is the C++ `use-after-move` channel, off by default
(`lsp/symbols/diagnostics.rs`). So the framework is being designed while
its second tenant is still hypothetical, which is exactly when a
Perl-shaped assumption gets baked in.

Non-negotiables:

1. **The registry row carries language applicability from Phase A.**
   The `languages` field above is not speculative — `use-after-move`
   already exists and is C++-only. A registry that cannot express "this
   code applies to these languages" will be rewritten by Epic 13.
   Model it as an explicit set or "all", never as an implicit default.
2. **The PL namespace is the engine's, not Perl's.** `use-after-move`
   gets a PL number in Phase A alongside the Perl codes, from the same
   sequence. Do NOT create a parallel `CPPxxx` space — one namespace,
   because a user's config and a CI's SARIF should not care which
   language produced a finding.
3. **`.perl-lsp.json` is now a misnomer and this epic should say so.**
   Do not rename it in this epic (it is a shipped, user-visible
   filename), but pick the config KEY names to be language-neutral, and
   record in the ADR that a neutral filename is a future migration with
   the old name kept as an alias. `perl-lsp` "becomes the
   Perl-configured distribution of that binary, name and install base
   intact" — the doc's own framing.
4. **Per-language severity defaults must be expressible.** A lint that
   is `warning` for Perl and `off` for a pack language until its
   substrate calibrates is the exact ladder Epic 13 needs. If Phase B's
   config can only set one default per code, Epic 13 cannot promote a
   pack language incrementally.
5. **Suppressions (Phase C) are comment-syntax-dependent.** `#` is not
   every language's comment. The builder scans comments; the pack side
   extracts through queries. Put the suppression *grammar* in the
   registry-adjacent code and let each language supply its comment
   token, or explicitly scope Phase C to Perl and say so — either is
   fine, silence is not.
6. SARIF (Phase D) is the payoff for all of the above: one SARIF run
   over a mixed-language workspace, one tool driver, one rule set. That
   only works if 1–4 held.

## Scaling beat

**`--check` is a batch verb, and it is already the pathological one.**

`scaling-limits.md` §1, measured: FHEM (991k LOC, 973 files, 87% of them
providing `package main`) **does not complete `--check` on a 31 GB
machine**. The documented workaround is
`RAYON_NUM_THREADS=4 MALLOC_MMAP_THRESHOLD_=65536`, which cuts peak 67%
for 4.9% wall. `prompt-scale-validation-hitlist.md`: the CLI one-shot at
138k files was DNF, killed at 42:32 / 7.11 GB; PR #125 made it finite at
350 s.

So every phase here lands on a verb whose scaling is already the
constraint:

1. **Phase A is free** — a static registry lookup per diagnostic
   construction. Keep it that way: a registry that does a hash lookup by
   `String` per emission on a workspace-wide sweep is a real cost. Key
   by a `&'static str` or an index, and let the drift test prove the
   mapping.
2. **Phase B's `exclude` globs are the one place this epic can make
   `--check` *faster*** — and users with vendored trees will reach for
   it first. Compile the glob set ONCE into the `Config`, not per file.
   Note the interaction with `scaling-limits.md` §2 (vendored dependency
   piles are MISLEADING, not slow) — excluding them changes what is
   *reported*, not what is *indexed*, and the doc's honest framing must
   survive this epic.
3. **Phase C's suppressions ride the FA and the cache blob.** A file
   with no suppressions must carry an empty vec that costs nothing —
   default-empty, in its lane, joined to a `heap_estimate` bucket.
   `EXTRACT_VERSION` bump: bundle it with any other bump landing nearby;
   a cold re-index is ~10.5 min at CPAN-5k (2026-08-17).
4. **Phase D's SARIF materializes every finding into a JSON document.**
   On a corpus where `--check` produces tens of thousands of
   diagnostics, that is a large in-memory value. Stream it if the shape
   allows, and at minimum **measure the peak RSS delta of
   `--format sarif` versus `--format json`** on Koha and report it.
5. **Phase E's new lints each add a per-file pass.** Report per lint:
   the substrate hit count AND the `--check` wall delta on Koha, three
   runs, dated. A lint that costs 15% of `--check` for six findings is
   a bad trade and the number is how you know.

## Verification gate

`cargo test` (both feature sets) · gold 0 FAIL / 0 XPASS · `./e2e/run.sh` ·
substrate audit at **exact parity** for Phases A–D (pure packaging),
per-lint audited deltas in Phase E · Koha `--check` wall + peak RSS,
three runs, dated, for D and E · the promotion states from
`adr/narrowing-diagnostics.md` must survive the config migration
exactly.

## Sizing

Large-ish but mechanical; A→B→C→D→E strictly ordered. E is droppable to
a follow-up without hurting A–D's value.
