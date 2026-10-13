# Epic 13 — The pack-language ceiling: calibration, diagnostics, framework tier

> **Status:** scheduled (13th) but **pullable to first at any time** —
> it is the epic that turns a pack language from "parses and answers"
> into a product.
> **Design owner-doc:** `docs/prompt-multi-language.md` §"What pack
> languages still don't get" and §"Calibration is the ship gate".
> **Reference implementation:** `docs/cpp-golive-map.md` is the record
> of doing this once, for C/C++. Read its arc structure before planning
> phases; it is the only evidence anyone has about how long this takes.

## Mission

Pack languages (C/C++, Python, R, CMake) ship behind opt-in Cargo
features with a `PackDriver` constructor, a `.scm` skeleton and
predicates — no new Rust types per language. That machinery is landed
and works. What they do **not** get is the ceiling:

1. **A calibration substrate** — the gold-corpus sibling. This is the
   ship gate, not a nice-to-have, and `prompt-multi-language.md`
   budgets it as **half the work**.
2. **Diagnostics** — deliberately NONE for pack languages until (1)
   exists, because a diagnostic without a zero-false-positive sweep is
   a product liability.
3. **A framework tier** — keying plugin hooks on capture events the way
   rhai hooks key on `CallContext` today. This is the named open design
   round.

Maturity is the scoreboard: `Maturity::{Stable, Beta, Alpha}` in
`build/language_driver.rs`, reported by `--languages`. Beta means
*"broad gold coverage, known gaps documented"* — which is precisely
(1). **This epic is how a language earns its next tier.**

## Read first

1. `docs/prompt-multi-language.md` — whole.
2. `docs/cpp-golive-map.md` — the arc record: what was done, in what
   order, and what the review waves caught. Note especially the
   dogfood → hitlist → fix → promote cycle; it is the method, not an
   anecdote.
3. `gold-corpus/README.md` — the harness. **`run.pl` is
   language-agnostic; it shells the binary.** The fixture format is
   reused verbatim. This is the single biggest reason calibration is
   tractable.
4. `docs/cpp-status.md` — what an honest per-language status page looks
   like, including the scaling caveat it refuses to hide.
5. `src/build/language_driver.rs` — `Maturity`, `LanguageDriver`,
   `PackDriver`, `LanguageRegistry`, `dependency_roots`, the capability
   surface, and `PackDriver::analyze_with_path`'s eight named phases.
6. `docs/adr/cursor-context-completion.md` — **the completion half is
   further along than `prompt-multi-language.md` says.** The two-half
   pack completion (in-scope symbols + sentinel-reparse member access +
   the trigger-char gate) is landed, and `lsp/cursor_slot.rs` gives one
   `Slot` vocabulary over both `cursor_context` (Perl) and
   `cursor_sentinel` (pack). Update the stale doc as part of Phase A.

## Phase breakdown

### Phase A — the calibration substrate (the ship gate)

This is the phase. Everything else is gated on it.

1. **A pinned substrate**, per language, snapshot-pinned the way
   `gold-corpus/local/` is for Perl — a package-manager-materialized
   tree at a recorded snapshot (CRAN via `renv` for R, top-N by
   downloads for each language's registry). Pinned, because a moving
   substrate turns every regression into an archaeology exercise.
   The recipe goes in the `dev-setup` skill, not in the repo.
2. **Fixture rows in the existing format.** `run.pl` already shells the
   binary and is language-agnostic; C/C++ proved it with 255 rows. The
   statuses are unchanged: `gold` (must hold), `xfail` (known gap →
   XPASS when fixed, promote the row), `provisional` (reported, never
   fails). A crash is always a hard fail.
3. **`lang-skip 0` is the acceptance criterion, and it is easy to
   miss.** A build without the language's feature lang-skips its rows
   and reports them as *skips, not failures* — CLAUDE.md warns that a
   plain release build silently leaves half the corpus unexercised.
   Every gate in this epic checks the `lang-skip` line.
4. **The dogfood loop is how rows get authored.** The `dogfood-loop`
   skill owns the protocol: task-driven probe agents against real
   projects, findings become RED xfail rows *before* the fix, promote
   on green. `docs/hitlist-*.md` files are its working product.
5. **Corpus entries for the real-project axis too.** Gold asserts
   answers; `corpus/` measures behaviour at scale. Add the language's
   projects to `corpus/bootstrap.sh` so `edit-bench` can measure them.
6. **Acceptance:** the language's row count and pass/xfail split
   reported in its status doc; `lang-skip 0`; the substrate recipe
   reproducible on a fresh box per `dev-setup`.

### Phase B — diagnostics, promoted one code at a time

Gated on Phase A. The rule is the one already in force:
**zero-false-positive sweep before a diagnostic exists.**

1. The registry from Epic 7 carries per-language applicability and
   per-language severity defaults — **this epic is the reason those
   fields exist.** If Epic 7 has not landed, either land its Phase A
   first or accept that this phase rebuilds it.
2. Each code promotes on its own evidence, exactly like the Perl
   ladder in `adr/narrowing-diagnostics.md`: registered → default-off →
   swept over the substrate → per-site triage → default-on at a stated
   severity. The C++ `use-after-move` channel is already registered and
   off by default; it is the template and the first candidate.
3. **Calibration is one gate; the facts are another.** Several pack
   codes are not blocked on a substrate at all — they are blocked on
   analysis that does not exist. `adr/narrowing-diagnostics.md` records
   that **"D1/D2/D3/D4/D6 have no cpp facts"** and that nothing records
   cpp guard sites for redundancy; `adr/use-after-move.md` ships a
   decidable subset explicitly *because* there is no CFG. That tier is
   **Epic 16**, and its motivating consumer is C++. So when a code fails
   to promote, say which gate it is waiting on — a substrate, an
   openness fact (Epic 3), or the path-sensitivity facts (Epic 16).
   Three different answers with three different owners.
3. The sweep is over the Phase-A substrate, and the number goes in the
   PR. A code with unexplained hits does not promote — it gets its
   noise class written into `gold-corpus/KNOWN-GAPS.md`.
4. **Acceptance per code:** the sweep count, the triage, the ladder
   position recorded in the language's status doc, and — for anything
   still off — the named gate it waits on.

### Phase C — the framework tier (the open design round)

**Read `prompt-multi-language.md`'s framing before designing:** the
question is keying plugin hooks on CAPTURE EVENTS the way rhai hooks
key on `CallContext`. The tenants are real (tidyverse for R, CMake
module conventions) and they are what makes a pack language feel
*known* rather than merely parsed.

Design constraints, from the existing system:

- **Every pack predicate already written is trivially rhai**
  (`ctor_class`, `module_paths`, `annot_type`), and the rhai host with
  fingerprinted cache invalidation already ships. The distance is smaller than it
  looks.
- **`EmitAction` is the shared vocabulary** and must stay shared. A
  framework tier that mints a second action vocabulary for pack
  languages is the fork this whole architecture exists to avoid.
- **Walk-phase patterns run as ONE query.** Perl's plugin patterns are
  concatenated into a single tree-sitter `Query` with `pattern_index`
  routing matches back to owners, because a `QueryCursor::matches` call
  is a full tree traversal. A pack framework tier must inherit that
  discipline from day one — one query per overlay walked every file
  once per overlay is the regression. `PERL_LSP_PD_NO_COMBINE=1` and
  `PERL_LSP_PD_EQUIV=1` are the existing escape hatch and A/B control;
  the pack tier should have the equivalents.
- **A malformed overlay must not take the tier down.** Perl's rule: a
  spec whose query fails to compile is dropped *before* the
  concatenation. Same here.

**Sequencing advice:** a declarative overlay (patterns in the standard
capture vocabulary, no callbacks) covers more tenants than it looks
like it will, and it needs no engine at query time. Build that first,
find the tenant it genuinely cannot serve, and let that tenant justify
the imperative hook. Epic 9's PR is asked to record which of its Mojo
phases could have been a pure pattern match — that list is this phase's
requirements document.

**Acceptance:** one real framework served end-to-end for one pack
language, with gold rows, and the engine carrying none of that
framework's names.

### Phase D — status pages and the maturity bump

Each language gets a `docs/<lang>-status.md` in the shape of
`cpp-status.md`: the tier and why it meets the bar, what works, the
measured limits with dates, what would lift each caveat. **Then bump
`Maturity` in the driver** — the tier is a promise about whether to
trust the answers, and it is not allowed to run ahead of the evidence.

## Non-goals

- **The `lsp-engine` crate cut.** Parked by `prompt-multi-language.md`'s
  own text: it does not start before a second pack language's ceiling
  work forces the split's cost to pay for itself. That is *this epic* —
  so the crate cut is the epic AFTER this one, and only if this one
  shows the seam hurting. The `workspace-split` branch is the
  mechanical playbook; a crate split was already executed once and
  REJECTED, with layering tests enforcing the DAG instead.
- Runtime-loadable packs (`{grammar, skeleton.scm, predicates.rhai,
  pack.toml}` installable like `.perl-lsp/` plugins). Compiled-in
  first; dynamic loading is the step after.
- Rewriting `symbols.rs`' verb/intelligence split. That disentanglement
  is named as the biggest single line item of the crate cut, and it
  belongs with it.

## Language-pack beat

This epic **is** the language-pack beat, so the section inverts: what
does it owe **Perl**?

1. **Nothing may regress for Perl, and Perl is the harder constraint.**
   Perl is `Stable`; it has the largest gold corpus and the only
   substrate audit. Every seam this epic generalizes must leave the
   Perl path byte-identical. The substrate audit at exact parity is the
   proof.
2. **Generalizing a seam is allowed to make Perl's spelling of it
   simpler, and that is the win to look for.** Every pack seam that
   generalizes shrinks `prompt-unify-language-paths.md`'s parked
   cleanup — it is parked precisely because it has no user-visible
   product, and it gets cheaper each time this epic touches something.
   Re-read that doc at the end of this epic and record how much is
   left.
3. **Do not "generalize" by moving a Perl rule into a shared layer.**
   `model/builtins.rs` is Perl-driver-scoped on purpose; pack languages
   never consult it. `model/conventions.rs` likewise. The layering
   tests catch a file in the wrong *layer*; nothing catches a Perl
   *semantic* in a shared function. That is a review obligation.

## Scaling beat

**Calibration and scale are the same gate wearing two hats, and
`cpp-status.md` is the cautionary tale: C/C++ meets the correctness bar
for beta and explicitly does not promise scaling.**

1. **A language does not reach Beta on gold rows alone.** The maturity
   enum's bar is about answers, so gold coverage is what it asks for —
   but the status doc must carry the measured scaling envelope beside
   it, dated, the way `cpp-status.md` does. A tier claim with no
   measurement behind it is the thing that gets discovered by a user.
2. **Phase A's corpus entries are the instrument.** `corpus/` is a
   SEPARATE axis from gold: gold asserts answers, corpora measure wall,
   RSS, and the pathologies no fixture reproduces. **Every scaling limit
   in `docs/scaling-limits.md` came from them, and none reproduced on
   synthetic input.** A pack language with gold rows and no corpus entry
   has a correctness story and no idea what it costs.
3. **Measure with the protocol, not with a stopwatch.** `edit-bench`
   owns it. Three runs minimum; stamp the date. Land rows in the JSONL
   store via `bench/measure.sh` and the editor surface via
   `bench/lsp_bench.py` / `bench/editor-baseline.sh` (quiet box only —
   run lines record loadavg).
4. **Watch the two known pack-side residency hazards specifically:**
   - `PackBagCache` — the 13.9 GB P0 came from a denormalized byte
     counter plus a lock-free map collapsing the LRU to one entry.
     Re-soak it; `prompt-scale-validation-hitlist.md` lists that re-soak
     as **OWED**.
   - Warm stubs — the pack warm scan registers from the `stubs` table
     rather than decoding full blobs, with declared fallback lanes. A
     new pack lane that is not in the stub is a silent full-decode per
     file on warm start.
5. **The residency tripwire is not optional for a new pack language.**
   A post-bulk-index count of fully-resident pack copies against the
   deliberate whole-copy sites errors on unexplained pins — functional
   tests cannot see a RAM regression, and this can.
6. **Phase B's diagnostics run on `--check`**, the batch verb that is
   already the constrained one. A new language's diagnostics multiply
   the sweep. Report `--check` wall and peak RSS on that language's
   corpus entry, three runs, dated.
7. **Phase C's framework tier adds query work per file.** The
   one-combined-query discipline above is the scaling requirement, not
   a style preference: it is the difference between one traversal per
   file and one per overlay.

## Verification gate

`cargo test` for every feature combination the change touches · gold
built with the language's feature, **`lang-skip 0` confirmed in the
summary**, 0 FAIL / 0 XPASS · the pack e2e lane · Perl substrate audit
at **exact parity** (this epic must be invisible to Perl) · the corpus
measurements above, three runs, dated · `docs/<lang>-status.md` current,
with the `Maturity` value it justifies.

## Sizing

Large, and honestly so — `prompt-multi-language.md` budgets calibration
alone as half the work, and the C/C++ arc is the evidence for that
estimate. A is the gate and the bulk; B is incremental and shippable per
code; C is a design round followed by a build; D is a day.
