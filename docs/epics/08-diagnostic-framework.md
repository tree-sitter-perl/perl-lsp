# Epic 8 — The diagnostic framework: PL-codes, config, suppressions, SARIF

> **Status:** scheduled (8th). The CI-readiness gate.
> **Design owner-docs:** `docs/prompt-cli-tools.md` §"Diagnostic
> framework" (codes table, config JSON, suppression comments, SARIF)
> and `docs/prompt-config-schema.md` (whose "forcing function" section
> says THIS epic is the moment its three deferred pieces land).

## Mission

`--check` and the LSP diagnostics grow a real framework: stable
diagnostic codes, per-code severity config from `.perl-lsp.json` +
LSP `initializationOptions`, in-source comment suppressions, and SARIF
output for CI annotation. The config-schema doc's deferred pieces —
the owning `Config` struct and the generated editor schema — land here
because per-code objects are their named forcing function.

## Read first

1. `docs/prompt-cli-tools.md` — the PL-code table (PL001–PL010), the
   config JSON shape, the suppression comment grammar, SARIF.
2. `docs/prompt-config-schema.md` — WHOLE doc; it prescribes the
   `Config` shape (own at top, pass slices) and the schemars plan, and
   warns off the `define_options!` macro.
3. `docs/adr/narrowing-diagnostics.md` — the existing flag ladder; the
   new framework must express it, not replace its semantics.
4. `grep -n 'struct DiagnosticOptions' -A 20 src/symbols.rs` and the
   `cli_flags_match_diagnostic_option_fields` drift test — the pattern
   every config surface here must keep.

## Ordering constraint

Do NOT renumber or re-key existing diagnostics' string codes
(`unresolved-function`, `undef-deref`, `optional-deref`, …) — editors
and gold rows key on them. PL-codes are an ADDITIONAL stable alias
(SARIF ruleId + suppression key); the LSP `code` field can carry
`PLxxx` with the descriptive name in `codeDescription`/message, but
decide ONE presentation and write it in the ADR. The owner doc's table
maps PL001/PL002 onto the two existing unresolved lints; extend the
table with rows for every diagnostic that exists TODAY (the narrowing
family: undef-deref, optional-deref, redundant/contradictory-guard,
deref-shape; helper-not-loaded; composer-mismatch; shadowed-package
if Epic 5 landed) BEFORE adding any new lint — registering the
existing surface is Phase A; new lints (PL003+) are LAST.

## Phase breakdown

### Phase A — the registry + codes for existing diagnostics

1. `src/diagnostics.rs` (new; add to `layering_tests.rs` `layer_map` —
   it sits beside symbols.rs in the LSP-adapter layer): a static
   registry `DiagnosticCode { pl: &'static str, name: &'static str,
   default_severity, default_enabled }` — one row per EXISTING
   diagnostic. Every `Diagnostic { code: … }` construction site in
   `symbols.rs` routes through the registry (grep
   `NumberOrString::String` to find all sites).
2. A drift test in the spirit of
   `cli_flags_match_diagnostic_option_fields`: every emitted code
   string appears in the registry, and PL numbers are unique.
3. **Acceptance:** `--check` output unchanged by default except each
   diagnostic now also carries its PL code.

### Phase B — `Config` + `.perl-lsp.json` + per-code severity

1. Per `prompt-config-schema.md` piece 1: one owning
   `struct Config { diagnostics: DiagnosticsConfig, exclude: Vec<String> }`,
   parsed ONCE (workspace root `.perl-lsp.json` + LSP
   `initializationOptions` + `didChangeConfiguration`, later sources
   overriding earlier field-wise). Backend holds `Arc<RwLock<Config>>`.
   **Call sites keep taking the narrow slice** — the doc is explicit
   and gives the reasons; keep `collect_diagnostics(&cfg.diagnostics, …)`.
2. `DiagnosticsConfig`: per-code `"error"|"warning"|"info"|"hint"|"off"`
   keyed by either the PL code or the descriptive name (accept both;
   normalize through the registry). The existing `DiagnosticOptions`
   bools become the LEGACY spelling — keep deserializing them (serde
   alias or a merge step) so current users' configs keep working;
   document the mapping.
3. `exclude` globs honored by `--check` and workspace indexing's
   diagnostic pass (NOT by indexing itself — resolution still needs
   excluded files' symbols; only reporting is filtered).
4. **Acceptance:** unit tests for precedence (file < init options <
   didChange), per-code override, `"off"`, legacy-bool compatibility;
   the drift test extended to assert every registry row is
   configurable.

### Phase C — comment suppressions

1. Grammar (owner doc): `# perl-lsp: ignore(PL001)`,
   `ignore-next-line(PL001)`, `ignore-file(PL004)` (file-form only in
   the first 10 lines). Also accept descriptive names in the parens.
2. Rule #1: comments are scanned during build — collect
   `FileAnalysis.suppressions: Vec<Suppression>` (serde;
   EXTRACT_VERSION bump) in the builder's comment handling; the
   registry lookup/validation happens at DIAGNOSTIC time (the builder
   stores the raw code string; unknown codes get their own
   `unknown-suppression` hint from the framework — self-hosting).
3. `collect_diagnostics` filters through suppressions before emitting.
4. **Acceptance:** unit tests per form + the unknown-code hint; a gold
   diagnostics row exercising `ignore-next-line`.

### Phase D — SARIF 2.1.0 (`--check --format sarif`)

1. Serialize the same diagnostics to SARIF: `runs[0].tool.driver`
   from the registry (rules = registry rows, with help text from the
   registry's doc strings), `results` with ruleId = PL code,
   locations with 1-based line/col matching `--check`'s existing
   coordinate contract, level from severity.
2. Validate against the SARIF schema in a test (embed the minimal
   checks: required fields, enum values — do not pull a validator
   dependency; hand-assert the shape on a golden file).
3. Wire a CI smoke: the repo's own workflow uploads the SARIF of a
   fixture run (optional; if touching CI is out of scope for the
   implementer, note it in the PR).
4. **Acceptance:** golden-file test; `jq` sanity commands in the doc.
5. This also discharges the heatmap doc's deferred SARIF note — record
   that in `docs/prompt-heatmap.md` (heatmap SARIF stays deferred;
   only `--check` gains it here).

### Phase E — schemars + new lints (only now)

1. `prompt-config-schema.md` piece 2: `#[derive(schemars::JsonSchema)]`
   on the config structs + `--dump-options-schema`. Field `///` docs
   become descriptions — write them as user-facing.
2. New lints from the table, EACH as its own commit with a substrate
   audit before/after (the Epic-1 Phase-E commands) and default
   severity per the table: PL003 unused-import, PL004 unused-variable,
   PL007 shadow-variable, PL010 deprecated-pattern (`use base` →
   `use parent`). PL005 unused-export / PL006 dead-sub REUSE the
   heatmap's guards verbatim (`exported`, constructor,
   framework-synthesized, dynamic-dispatch — grep
   `reachable_guard` in `src/main.rs`) — a lint that flags what the
   heatmap shields is a bug. PL008 missing-import and PL009
   circular-dependency: only if the audit shows clean signal;
   otherwise leave registered-but-default-off with a note.
3. **Acceptance per lint:** unit tests + substrate hit count recorded
   in the PR + zero hits on the repo's own `t/`-style fixtures unless
   genuinely justified.

## Non-goals

- `define_options!` macro (config-schema doc says the drift test is
  cheaper; it stays).
- `--migrate` and the analysis subcommands (Epic 11).
- Changing any diagnostic's SEMANTICS — this epic is packaging.

## Verification gate

cargo test + gold + substrate audit at exact parity for Phases A–D
(pure packaging), per-lint audited deltas in Phase E. The promotion
states from `adr/narrowing-diagnostics.md` (post-Epic-2) must survive
the config migration exactly.

## Sizing

Large-ish but mechanical; A→B→C→D→E strictly ordered. E is droppable
to a follow-up without hurting A–D's value.
