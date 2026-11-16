---
name: docs-gc
description: Garbage-collect the docs tree — delete landed prompt briefs, convert closed working docs (hitlists, measurement ledgers, spike writeups) into their durable form, repoint references into the owning ADRs, and scrub ADRs to house voice (no historical narrative). Use after a mission/arc lands, whenever docs/prompt-*.md piles up, and in every release PR to audit docs/epics against main.
---

# docs-gc: landed prompts die, ADRs carry the load

The docs tree has three kinds of docs, with different lifecycles:

- `docs/prompt-*.md` — implementation BRIEFS. They exist to drive work.
  When the work lands, the brief is garbage: delete it. Its decisions
  live in ADRs, its landing record in `docs/forks-resolved.md`, its
  history in git. (`docs/open-forks.md` holds ONLY still-open forks;
  entries move to the resolved ledger when their status leaves OPEN.)
- `docs/adr/*.md` — load-bearing DECISIONS. They live forever, but in
  house voice: they describe what IS — contracts, invariants,
  trade-offs, failure modes, measured facts — never what WAS or what
  CHANGED. ("Git remembers; the comment shouldn't" — CLAUDE.md
  Comment style. It governs docs the same as code.)
- **Working docs** — measurement ledgers, hitlists, status pages, spike
  writeups, comparison studies. Live scaffolding while an arc is in
  flight; once it closes they are neither brief nor decision. Deleting
  loses real content, keeping leaves scaffolding in a permanent tree.
  They get CONVERTED into their durable form (Job 3).

**Scope defaults to the current branch's docs** — the files
`git diff --name-only $(git merge-base HEAD <base>)...HEAD -- docs/`
names — plus repo-wide reference FIXING for anything deleted. Widening
to the whole tree is an explicit, separate ask; other subsystems' docs
belong to their own landings. Put the in-scope file list in the
subagent's prompt verbatim: a scope correction delivered mid-flight may
be (rightly) distrusted as an injection, so the boundary has to ride
the original instructions.

Run this pass with a cost-appropriate model (spawn a subagent — this is
mechanical judgment, not architecture).

## Job 1 — classify and delete landed prompts

For each `docs/prompt-*.md`:

- **LANDED** → delete. Evidence: its own progress/status says complete;
  `docs/forks-resolved.md` or an ADR records the landing; no live next
  steps that aren't tracked elsewhere.
- **FORWARD / PARKED** → keep unchanged (it still drives future work).
- **MIXED** (landed phases + live next steps) → keep, but strip the
  landed-progress narrative to a one-line pointer at the owning ADR;
  only the live forward work remains.
- **Unsure → keep.** Deletion must be obviously safe.

Before deleting a file, two obligations:

1. **Rescue load-bearing content.** If the brief holds rationale no ADR
   owns (a decision, an invariant, a failure-mode analysis that code
   comments point at), move it into the most relevant `docs/adr/*.md`
   first. A prompt doc is disposable; the reasoning inside it may not be.
2. **Repoint every reference.** Grep the ENTIRE repo (`src/`,
   `CLAUDE.md`, `docs/`, `gold-corpus/`) for the filename. Code comments
   and CLAUDE.md must point at the owning ADR instead — a dangling doc
   link in a comment is a bug. (`docs/open-forks.md` and
   `docs/forks-resolved.md` are ledgers — their references may stay.)

## Job 2 — scrub ADRs to house voice

Keep every contract, invariant, trade-off, failure mode, and number.
State measurements as current facts ("abseil resident payload:
11.2 MB"), not as deltas with history ("reduced 46→11 in phase 5").

Remove: phase-by-phase landing narratives, "Status: landed <date>"
logs, "replaces/subsumes the old X" framing (at most one
`Supersedes: <adr>` line), spike war stories with no reusable lesson.
Never change technical meaning.

An ADR that is ENTIRELY a landing log with no reusable design content:
fold its load-bearing facts into the related ADR and delete it — same
reference-fixing discipline as Job 1.

## Job 3 — convert closed working docs

A working doc is closed when the arc that produced it is done: its
measurements are recorded, its hitlist is worked through, its spike
reached a verdict. Pick ONE disposition per file:

- Durable content is a DECISION, invariant, contract, or failure mode →
  fold it into the owning `docs/adr/*.md`; promote the file itself to an
  ADR if it is substantial and no ADR owns it. Then delete the working
  doc.
- Durable content is FORWARD work that still needs driving → make it a
  real `docs/prompt-*.md` brief: strip the status logs, progress tables,
  and measurement narration; leave the design problem and what is open.
- Durable content is a REPEATABLE PROCEDURE (how to run the benchmark,
  how to rebuild the corpus) → keep it, scrubbed to house voice like an
  ADR: present tense, what IS, no arc narration.
- Nothing durable survives the scrub → delete, with the usual
  reference-repointing.

**A doc referenced from a user-facing file is load-bearing** — treat it
conservatively and repoint in the same commit. `CHANGELOG.md` ships to
users, so a doc it names cannot silently move or vanish.

Measurements inside a working doc are the content most worth rescuing
and the most likely to be stale. A number with no date and no rerun is
a liability once it outlives the branch that measured it: re-measure it,
or move it with the date attached.

## Job 4 — audit the epics (every release)

The release PR runs this job, beside the `cache-epochs/` roll-up. Follow
`docs/epics/README.md` §"Audit at every release": check each epic's
phases against `main` with source (a grep, a test name, a gold row — not
memory), mark landed phases with that evidence, fix anchors that no
longer resolve, retire an epic with nothing left, refresh the slate's
Status column and date, and give every `docs/prompt-*.md` a coverage-map
row. An epic an open stack is building is that stack's to update.

## Verify (non-negotiable)

```
# every doc reference resolves (open-forks.md exempt):
grep -rn "docs/prompt-\|docs/adr/\|docs/[a-z]" src/ CLAUDE.md CHANGELOG.md docs/ gold-corpus/ \
  --include=*.rs --include=*.md --include=*.pl \
  | grep -v "docs/open-forks.md:" \
  | grep -oE "docs/([a-z]+/)?[A-Za-z0-9_-]+\.md" | sort -u \
  | while read f; do [ -f "$f" ] || echo "DANGLING: $f"; done

cargo test --release   # some tests assert on doc paths
```

Two results are NOT bugs: a forward reference to a doc a live brief
plans to write ("Write `docs/adr/foo.md`" in its own phase list), and a
ledger entry in `open-forks.md` / `forks-resolved.md`. Everything else
dangling is a bug — including one an EARLIER gc left behind. A `src/`
comment pointing at a deleted brief is the exact failure this pass
exists to prevent, so fix it while you are here rather than filing it.

Both must be clean before committing. Commit message pattern:
`docs: gc — <N> landed prompts deleted, <M> ADRs scrubbed to house voice`
(add `, <K> working docs converted` when Job 3 did anything).
