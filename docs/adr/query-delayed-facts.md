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

`Question::decide(fa, index)` is the only decider; it answers `Verdict::
{Yes, No, Unknown}`. `Unknown` is a typing gap, not a negative: the opt-in
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
  The extractor settles it when the file declares the class
  (`declares_class`, the same local proof `decide` runs), else binds the
  ref `RefBinding::Deferred(NamesClass { identity })` with the call's own
  binding as the other reading. A language whose construction is syntax of
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

## Consequences

- The answer is computed at query time through `CrossFileLookup` only, so
  under the whole-program engine every read a deferral makes is a tracked
  read. Nothing is decided at enrichment.
- A deferral is per-file data, serialized in the blob like any other fact.
