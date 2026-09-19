---
name: dogfood-loop
description: Run one autonomous dogfood→hitlist→fix→sweep round against real-world corpora for any language perl-lsp supports. Use when hardening a language (new or existing) with real-usage probing instead of synthetic tests.
---

# The dogfood loop

One round = **dogfood → hitlist → xfail-encode → fix → merge-gate → round-close sweep**.
Language-agnostic: parameterize on the target language, its corpus, and its
gold fixture root. Proven over the C/C++ go-live (rounds 1–2 + tightening).

## Inputs (establish before firing anything)

- **Language + corpus**: 3–6 REAL codebases for the target language, cloned
  locally (e.g. cpp used `~/personal/cpp-bench/`: abseil, fmt, folly, json,
  redis; Perl uses `gold-corpus/local/`). Real code finds what fixtures
  can't — the shapes nobody thought to write down.
- **Binary**: `cargo build --release --features all-langs`, and verify the
  binary actually has the language (`perl-lsp --parse <sample>` — a
  default-features rebuild silently drops pack languages).
- **Gold fixture root** for the language (e.g. `gold-corpus/cpp-fixture/`)
  and its fixtures file under `gold-corpus/fixtures/`.

## Phase 1 — dogfood agents (sonnet)

Fire 2–4 agents in parallel worktrees, each owning disjoint corpus repos.
Briefs are **task-driven, not feature-checklists**: "you are a developer
doing <real task: trace this call path / rename this / find all users of X>
in <repo>, using ONLY the perl-lsp CLI (`--workspace-symbol`, `--rename`,
gd/gr/hover/completion via the e2e harness or CLI mirrors)". Agents record
every place the tool lied, undercounted, or went dark.

Non-negotiable disciplines in every brief:

- **Step-0 base guard**: `git fetch && git rebase origin/<branch>` before
  any work — worktree agents branch stale.
- **grep-sanity every gr count**: `gr` says N references → cross-check with
  `grep -rn` (minus comments/strings noise). An unverified count is not a
  finding. Record both numbers.
- **Findings, not fixes**: dogfood agents never patch. They report
  file:line, the capability, expected vs got, and the grep evidence.
- Model: sonnet. Probing doesn't need smarts; it needs diligence.

## Phase 2 — hitlist synthesis (main loop)

Collate findings into `docs/hitlist-<round>.md`: one row per finding with
capability, repro coordinates, evidence, and a first-guess root cause.
Dedup aggressively — 21 raw findings usually collapse into ~5 root causes.

Optionally fire a wave-2 **repro-reducer + root-causer** pair (sonnet):
reduce each finding to a minimal file, then CONFIRM the root cause by
experiment. Guessed root causes get refuted embarrassingly often (the
cpp include-closure hypothesis died this way; the real causes were split
macro identity + namespace-blindness).

## Phase 3 — encode as RED xfail gold rows (before any fix)

Every reducible finding becomes a gold row with `"status": "xfail"` via
`gold-corpus/run.pl --emit --root <fixture-root> <cap> <file> <row> <col>`.
This locks the repro. The harness then enforces the promotion: when a fix
lands, the row XPASSes → flip it to `"status": "gold"` in the same commit.
Non-reducible findings go to `docs/PARKED.md` (residual-bug tier) with the
probe evidence inline.

## Phase 4 — fix slices (opus)

Group root causes into 2–5 disjoint slices; one agent per slice, parallel
worktrees, step-0 base guard in every brief. Architecture-touching slices
(witness bag, resolution core, driver pipeline, extraction) run on **opus**
— sonnet produces layering cruft here (verified: cpp semantics embedded in
the language-generic extraction tier). Pure-mechanical slices may stay
sonnet. Fable is never spawned as an agent.

Brief each slice with the repo's architecture rules that bite: rule #1
(single tree-consumer per tier), rule #10 (no shape special-cases), rules
#11–#14 (mint at the producer; one home per language's spellings; never
reparse a rendered string; facts have one shape), edges-not-values,
clear-and-emit for re-emittable passes. Agents commit to their worktree
branch and do NOT push.

**Vocabulary needs a justification.** Any commit that adds an enum variant,
a `RefKind`, a `WitnessAttachment`, a `PackFacts` field, a `WitnessSource`
tag, or a bool flag on a facts struct carries one sentence in its message
naming the existing discriminator it replaces and why none of them fit.
A fix slice that cannot write that sentence has found a rule-#11 or
rule-#14 case, not a vocabulary gap — the deadline pressure of a fix slice
is exactly when the smallest diff is a tag on an existing shape, and every
such tag in the php arc's review traced back to a fact the producer
already had. Put the sentence in the brief; check for it at the merge gate.

### Operational discipline (learned the hard way — sandbox restarts EAT unpushed work)

- **Push slice branches as backup refs.** The moment a slice commits, push its
  branch (`git push -u origin <slice-branch>`) — side refs never touch the main
  branch, and a container restart/filesystem rollback then costs nothing. A
  round-7 rollback vaporized three COMPLETED slices that existed only in local
  worktrees. Delete the remote ref at cleanup (`git push origin :<branch>`).
  Same for the coordinator: push the main branch after EVERY gated merge, and
  commit+push docs (hitlists, briefs) the moment they're written.
- **Gates run foreground with explicit timeouts.** Never park a merge gate in
  an unbounded background task and "wait for the notification" — a silent
  worker restart kills the task AND the notification, and the wait becomes
  forever. Foreground with `timeout`, or if backgrounded, record the task id
  and CHECK LIVENESS (process table + output mtime) before assuming progress.
- **Agents: no background waits, ever.** Subagents that background a build and
  end their turn "waiting for the notification" never wake — backgrounded
  children die with the turn. Every agent brief gets: "run everything
  foreground and sequential; no monitors, no background waits." (Three agents
  stalled on this in one round.)
- **Gate chains: strict `&&`, no pipes over the verdict.** Two real failures:
  a stray `;` mid-chain let a push run after a timeout-killed suite, and
  `run.pl | grep TALLY` masked run.pl's exit (pipeline exit = grep's) so a
  CRASH-22 run pushed anyway. Every gate step's exit must gate the next:
  capture output to a file and test the command's own exit (`run.pl >out;
  E=$?`), or `set -o pipefail`. Also: two runs sharing a substrate/cache
  (sibling worktree gates) can CRASH each other transiently — a dirty tally
  under sibling contention needs one quiet re-run before it's believed.
- **Version-constant bumps are per-slice declarations.** Parallel slices WILL
  collide on EXTRACT_VERSION/STUB_VERSION (three same-value collisions in one
  round — one poisoned a shared substrate cache into a 163-row crash storm).
  Briefs say "bump and DECLARE it"; the coordinator reconciles to max+1 at
  each cherry-pick.
- **Worktree bases lie.** The Agent tool's managed worktrees may be cut from
  stale main. Prefer self-worktreeing in the brief: `git fetch origin <branch>
  && git worktree add <path> -b <slice> origin/<branch>` as step 0, and have
  the agent VERIFY the tip commit subject before working.
- **Sibling worktrees sharing a target dir are served each other's
  artifacts.** Cargo hashes a path package WITHOUT its path, so two
  worktrees of this crate on one `CARGO_TARGET_DIR` collide on every
  artifact hash and freshness falls back to source mtimes: a fresh
  checkout whose files are older than the other tree's last build is
  "fresh", and its net runs the other tree's binary and test harnesses
  (a branch-6 net ran branch 5's binary, failed three gold rows the
  branch-6 overlay answers, and never ran the branch-6 tests at all).
  One target dir per worktree, or `touch` every source under `src/`,
  `queries/`, `tests/` before the net, and record the tip sha the net
  built. Never symlink a target dir either — the same stale-binary trap
  through a different door.

Architectural forks mid-slice: pick the loosely-coupled/reversible option,
log in `docs/open-forks.md` (options / picked / undo cost / question),
keep moving — never block on ratification.

## Phase 5 — merge gate (main loop, per slice)

Before merging each agent branch:

1. **Diff review against the architecture rules** — read the diff, not the
   agent's summary. Look for: language semantics above/below its tier,
   shape special-cases, parallel stores instead of edges, version-constant
   collisions (two agents bumping `EXTRACT_VERSION` to the same value —
   same-value merges hide incompatibility; reconcile to max+1).
2. Steer in-flight when possible (SendMessage) — cheaper than post-merge
   rework.
3. Merge, then the **full net**: `cargo test --release` both feature sets;
   gold BOTH modes (default + `PERL_LSP_CPP_NO_FASTPATH=1`) cold — 
   `perl-lsp --clear-cache` between; `./e2e/run.sh` + per-language e2e.
   All 0 FAIL / 0 XPASS / 0 CRASH. Rebuild `--features all-langs` LAST
   (a default-features test run leaves a langless binary on disk).
4. Push. Delete the agent branch.

## Phase 6 — round-close sweep (opus, one agent)

One de-cruft/review agent over the round's whole accumulated diff
(`git diff <round-start-sha>..HEAD`): layering leaks, dead code, comment
rot (no history narration), doc currency (`docs/PARKED.md` pruned of
landed items, hitlist rows marked LANDED, KNOWN-GAPS current), warnings.
Leave-alone verdicts get RECORDED so the next sweep doesn't re-litigate.

## Phase 7 — arc-close review (the coordinator, not an agent)

The round-close sweep reads one round's diff. An arc is several rounds,
and the findings that cost the most in review were invisible per round:
each commit's tag, flag, or fallback looked locally justified, and the
sum was a vocabulary nobody would have designed. So before an arc is
handed over for review, the coordinator reads the WHOLE diff from the
arc's base (`git diff <arc-base>..HEAD`, not the last round's) against
CLAUDE.md rules #10–#14 as a checklist, per hunk:

- **#10** — does this branch on a shape (a name, a base, a language, a
  provenance) instead of asking the value?
- **#11** — is this deriving something the producer had (a re-derived
  relation by span/sigil/column; a structural fallback for a scope fact
  the extractor could have stamped)?
- **#12** — is there a separator, sigil, or attribute name as a literal
  outside `conventions.rs` / the pack?
- **#13** — is a rendered string being split, tokenized, or peeled?
- **#14** — is a per-site fact a side table instead of a witness/binding;
  a witness anchored somewhere other than its source site; a source tag
  read for meaning; a per-language constant on a per-file struct?

Every finding is fixed before the push, or recorded in
`docs/open-forks.md` with the reason it is deferred. The layering
tripwires (`language_spellings_have_one_home`,
`rendered_strings_are_not_reparsed`, `source_tags_are_provenance_only`,
`pack_facts_fields_are_ratcheted`) must be green with NO allowlist growth
over the arc — an allowlist entry added during the arc is a finding, not
a fix. Green nets are not evidence here: none of the five root causes
above changes a test's answer.

## Exit criteria for a round

- Hitlist rows all LANDED or explicitly parked with evidence.
- Zero open XPASS (every fixed row promoted).
- Full net green, pushed.
- `docs/PARKED.md` + `docs/open-forks.md` current.
- A round summary appended to the session/brag doc.

## Exit criteria for an arc (before handing over for review)

- Every round's criteria above.
- Phase 7 done: the whole-arc diff read against rules #10–#14, findings
  fixed or recorded as open forks, tripwire allowlists no larger than at
  the arc's base.
- Every vocabulary addition in the arc (variant / ref kind / attachment /
  facts field / source tag / flag) has its justification sentence in a
  commit message.

Then either fire the next round (new corpus repos debut + re-probes of
everything just fixed) or park the language with its limits pinned.
