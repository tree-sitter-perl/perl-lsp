# ADR: query-delayed facts

Some sites have a reading the producer can't settle alone: whether a
dispatch verb's receiver is the verb's class, whether a sub's package
reaches a framework role. The answer needs other files, and the builder is
index-free (rule #1). Such a site records the QUESTION that picks its
reading, and the reading stays unreadable until the question is decided
with the index in hand. The pattern or visitor runs at extraction as
always; only the answer is late.

An unknown VALUE is not one of these: "what class is this receiver" is an
`Edge` in the bag. A deferral is for an unknown READING: which meaning a
site has.

## Decision: one closed `Question`, one decider

`model/file_analysis/deferral.rs` declares `Question`, one variant per
kind of wait. Every input is a typed fact the producer minted (a receiver
it typed or the call ref that holds it, a package), so the decider never
searches the analysis for the site.

`Question::decide(facts)` is the only decider; it answers `Verdict::
{Yes, No, Unknown}`. It reads through `DecideFacts` (parents, index, a
call ref's invocant class), which both homes of a deferred fact answer:
the analysis with its index (`InFile`) and the bag's `BagContext`. The bag
has no ref table, so a `ReceiverIsa` on a call ref's invocant decides
`Unknown` there. `Unknown` is a typing gap, not a negative: the opt-in
`unresolved-dispatch` diagnostic reports exactly those.
`Question::when_unknown` says which reading an `Unknown` stands for; it is
an exhaustive match, so a new question doesn't compile until it says.

## Decision: one carrier, readable only by deciding

`Deferred<T> { when, then, otherwise }` has private fields and one
reader, `pick(fa, index) -> Picked { verdict, reading }`. No consumer can
see a reading without deciding its question. Site LOCATION accessors on a
specific carrier (`ProvisionalDispatch::call_span`) are input, not
content, and stay readable undecided.

Today's carriers:

| Field | Question | Reader |
|---|---|---|
| `provisional_dispatches: Vec<Deferred<DispatchCandidate>>` | `ReceiverIsa` | `applicable_dispatches`, `dispatch_at`, `untyped_dispatches` |
| `gated_param_types: Vec<Deferred<TypeConstraint>>` | `PackageIsa` | `gated_param_type_for` |
| `RefBinding::Deferred(Box<Deferred<RefBinding>>)` | `NamesClass` | `class_named_by_call` |
| `WitnessPayload::Choose(Box<Deferred<WitnessPayload>>)` | `NamesClass` | the registry's `materialize` |

A `Choose` witness reads as the payload its question picks under the
query's context, and drops out when it picks neither; a query without a
context reads the undecided payload.

The dispatch half is `docs/adr/receiver-gated-dispatch.md`.

## Decision: patterns state it with a deferred predicate

A deferred predicate (`query_predicates::Deferral`) never filters a
match: the facts the match mints become readings decided at query time.
Each query runner accepts the ones it knows how to mint.

- Plugin patterns: `#receiver-isa? @recv "Class"` makes the match's
  `DispatchCall` emissions `ReceiverIsa` deferrals. Its capture must be
  the invocant of a method call whose ref is the dispatcher; the producer
  names that ref as the receiver.
- Pack documents: `#names-class? @ref.call` says the call constructs the
  class its name spells (python's `User()`, C++'s functional cast `Foo(x)`).
  The extractor settles it when the file declares the class, else binds
  the ref `RefBinding::Deferred(NamesClass(..))` with the call's own
  binding as the other reading, and the call's value is a `Choose` between
  the class's instance and the call's `Invoke`. The question's payload is
  an `ElsewhereClass`, built only through its local proof
  (`ElsewhereClass::unless_local`), so it is never asked about a class the
  file itself declares and the bag, which has no symbols, decides it
  whole. A language whose construction is syntax of
  its own (PHP's `new`) carries no such pattern, so its `Foo()` is a call.
  A call's local callee prefers a like-named class only in such a match.

A deferred `RefBinding` reads as its question's fallback through the
`Ref` accessors (`settled_binding`), so a reader that never decides sees
the undecided site; `class_named_by_call` is the reader that decides.

## Decision: an unknown owner is bound to its call, not deferred

A hash-key argument whose owner needs other files (`Foo->new(name => …)`
with `Foo` elsewhere, or a receiver that didn't type at build) is an
unknown VALUE: the owner is `Sub { the call's invocant class, verb }`, or
the column owner for a column-keyed verb. So it is not a `Question`. The
producer binds the key to its call, `RefBinding::CallArgKey { call }`, and
`deferred_hash_key_owner` and `fix_chain_receiver_hash_key_owners` read the
call off the binding. Neither re-finds a call by span containment, and a
`$h->{k}` deref that merely sits inside a call's arguments is never owned
by that call.

## Decision: held readings live in a table of their own

A site whose ENTRIES (not just a value) wait on a question — a plugin
emission gated on cross-file ancestry, an indirect-object call — mints
every reading into `FileAnalysis::readings` (`readings.rs`), never into
the symbol or ref tables. The tables are the base (what the file states
whatever the answers) followed by the installed readings: the stored
analysis installs each question's fallback, and a decided view cuts back
to the base and installs its own picks. Nothing in a table is a
placeholder: no dormant slot, no impossible span, no empty name.

- A base id or index is the same in every view; an installed entry's is
  its view's own. Held symbols carry no id, and a held witness names its
  symbol by its place among the held ones.
- Readings seat after `finalize_post_walk`, and a view links only its
  installed refs, so no base entry names an id a later cut removes.
- An indirect-object site is the one REPLACING reading. The walk runs its
  undecided reading whole (a framework keyword's declarations, a
  constructor's keys), so its heads stay in base slots; each slot holds
  the head of whichever reading is installed, and the walk's
  `Expression(slot)` facts hold under the fallback.
- The row store lists the base in `refs`/`syms` and held names in `held`,
  a presence table: any retrieval that must not miss a view (candidate
  files, the member/name probes, the dead-export view) consults it, and a
  hit fails open to deciding the file. A listing (workspace/symbol) adds
  a file's held symbols by their picks, without building a view, and
  only where a held name could match.

## Consequences

- The answer is computed at query time through `CrossFileLookup` only, so
  under the whole-program engine every read a deferral makes is a tracked
  read. Nothing is decided at enrichment.
- A deferral is per-file data, serialized in the blob like any other fact.
