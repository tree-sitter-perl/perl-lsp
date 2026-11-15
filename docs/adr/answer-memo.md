# ADR: The answer memo — one memo across a scope's queries

`QueryState` dedups within one `ReducerRegistry::query`. A build, a pass
over a finished analysis, and the persist path each ask many top-level
queries over one bag, and those queries re-derive the same sub-answers.
The answer memo (`FOLD_MEMO` in `model/witnesses/registry.rs`) shares a
settled sub-answer across every query of one scope.

## Scopes

A memo exists only while a `FoldMemoScope` is open, one per thread:

- `FoldMemoScope::enter()` — a build's fold, and the persist closure in
  `index_perl` around the bake and both Surface projections. Admits any
  bag, but no query that can reach a module index.
- `FoldMemoScope::for_pass(bag, index)` — a pass over a finished analysis
  (the receiver stamp, diagnostics): exactly one bag under exactly one
  lookup, pinned for the scope's life.

`PERL_LSP_NO_FOLD_MEMO=1` leaves every scope closed: the A/B control.

## Key and validity

An entry is keyed by the per-query key (attachment, receiver, arity, read
point) plus the framework, the arguments and the scope table the context
reads. The bag's address is not in the key: the bag's generation names its
content, and a clone or a move keeps it, so the bake (which takes the bag
out of a clone) and the projections (which read it in place) share answers.
The memo holds one generation of the bag, of the parent graph and, for a
pass, of the index's resolution epoch; when any moves on, every entry is
dead and the memo empties. A bag at generation 0 names nothing and is never
shared.

Only answers no frame above them shaped are stored (`Taint::CLEAN`): a
subtree cut by a cycle that closed above it, or by the depth cap, depends
on the path it was reached from. Ladders longer than 64 rungs are not
stored, and the memo empties past 32 MiB per thread.

## What an entry carries

An answer is not only its value. A chase also records, for the bake:

- the ladder rungs it reached at its own opaque depth (`note_exit`), and
  whether anything beneath it poisoned the chase;
- whether it read the receiver it was asked under.

These are the frame's `Effect`, built per frame and folded into the parent
as frames close. Every memo entry (this memo, the per-query memo, the
fan-out memo) carries its subtree's effect, and a hit replays it at the
depth it is re-reached from: transparently it records the rungs a fresh
chase would; under a combining frame (`in_opaque_frame`) any recorded rung
poisons, as it would fresh. The bake reads the top-level effect from
`query_with_residuals`.

**Receiver reads.** `note_receiver_read` marks the open frame wherever the
receiver's value is consumed: a `Receiver` / `ReceiverOr` substitution, a
fresh dispatch derived from it, the first-parameter-is-the-receiver fold,
a baked conclusion evaluated against it, and a hit in the session or sweep
candidate memo (both keyed by the receiver, neither records a read). Forwarding the receiver to a
sub-query is not a read; the sub-query's read is its parent's only when
both were asked under the same receiver. An answer with no read and no cycle
closed in its subtree is filed without its receiver and serves every
receiver; one with either stays that receiver's. A cut counts because where a
cycle closes depends on the receivers along the path, read or not. Without this, N classes reading one inherited slot each chase
that slot's writes under their own receiver, and the memo fills with N
copies of one answer.

A read site that forgets to mark is a wrong answer served to another
receiver, so the equivalence net below runs with the memo on and off, and
`a_memo_answer_is_shared_across_receivers_only_when_it_never_read_one`
fails without the marks.

## Slot owners

`owned_slot_writes` groups a `SlotWrites` bucket's writes by the class each
write's object resolves to. That index is the answer to a root query of the
bucket (no receiver, no arguments, a fresh `QueryState`) and is filed in the same memo as a `MemoAnswer::SlotOwners`
entry, under the same key, stamp and cap. A read folds the index's effect
in under a combining frame. A read that reaches its own bucket while the
bucket is being built (`in_flight`), or that no memo may serve, chases the
bucket on its own path, where the cycle guard cuts it. A top-level query
with no index and no scope open opens one for its own duration, so a slot
read answers by the index wherever it is asked. A `Reentered` read is the
one answer that still depends on what is in flight above it, which `Taint`
does not see.

## What stays separate

| Memo | Why |
|---|---|
| `QueryState.memo` | the only home for path-shaped answers, valid for one query |
| `fan_out_memo` | keyed without the bag, over other files' bags |
| builder `invocant_memo` | keyed by CST node, not by an attachment |
| resolution-session receiver memo | cross-file, path-keyed, owns the consult budget |
| sweep memos | cross-thread, hold rehydrated files |

## Equivalence

`PERL_LSP_FOLD_MEMO_EQUIV=1 cargo test` rebuilds every file the suite
touches with the memo off and asserts the analyses agree, and re-asks the
bake and the Surface at encode time with a memo open and with none
(`without_fold_memo` sets an enclosing scope's memo aside, or the control
would read the answers it checks). The two passes over a
finished analysis — the receiver stamp and diagnostics, the latter under the
module index — re-run with no memo and no resolution session and compare
(`check_pass_equiv`, `fold_memo.equiv.pass_agree` / `pass_differ`); a control
that hits the depth cap is incomplete and is skipped
(`pass_control_truncated`). On a
real run the encode-time check counts `fold_memo.equiv.agree` /
`fold_memo.equiv.differ` and logs a difference; run it with
`PERL_LSP_MINT_LINKS` both unset and set, since `OpenReason` and
`Link.targets` are where a lost rung shows.

## Cost

Measured 2026-11-14 (`--check`, cold, three runs each side): a file of 1000
classes inheriting one base's slot writes went from 25.6 s to 12.7 s wall
(bake 12.0 s → 0.23 s, encode 14.2 s → 0.36 s); FHEM flat; the CPAN
substrate +2.3%, the bookkeeping on files that share nothing.
