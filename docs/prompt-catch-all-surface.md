# Narrow what a catch-all answers

A class with a runtime catch-all (Perl `AUTOLOAD`, php `__call` /
`__callStatic` / `__get`) answers member names its declarations do not
list, so the undefined-member lanes stay silent for it. Today that
silence is total: one catch-all anywhere up the MRO hides every member
name, for that class and every descendant. Real catch-alls answer a
narrow surface. This brief turns the one bit into a closed enum that
says which names the catch-all answers, so a typo below an `AUTOLOAD`
can be reported.

## Scope

In: the undefined-member lanes — the model's undefined-member lane
(`diagnostics_members.rs`) and the unresolved-method adapter lane
(`lsp/symbols/diagnostics.rs`).

Out: role contracts. `AUTOLOAD` never discharges a `requires` (Role::Tiny
checks `can`, Moose `find_method_by_name`), and a forward declaration
does; that is settled in `docs/adr/role-contracts.md` and does not read
the surface.

## Read first

1. `CLAUDE.md` — rules #10, #11, #14, and "Residency discipline".
2. `docs/adr/symbol-flags.md` — `DYNAMIC_MEMBERS`, `FORWARD_DECL`.
3. `docs/adr/role-contracts.md` — the forward-declaration stub.
4. `docs/adr/narrowing-diagnostics.md` — the default-off flag ladder.
5. `docs/epics/03-openness.md` — the verdict this feeds.
6. `docs/adr/plugin-system.md` — `EmitAction`, `patterns()` / `on_match`.

## Current state — anchors

**Prerequisite: main carries a catch-all fact to narrow.** That means a
class-level flag minted from Perl's `sub AUTOLOAD` and from a pack's
catch-all capture, one MRO query the undefined-member lanes ask, and
forward-declaration stubs minted as symbols. Without those there is nothing
for this brief to narrow. The names below are the ones this brief was
designed against. If a rewrite renamed them, find the fact in the left
column, not the name.

| Fact | Find it (as designed) |
| --- | --- |
| Perl mints the flag on the package | `grep -n 'mark_package_answers_any_member' src/build/builder/visit_decl.rs` |
| Pack capture mints it on the class | `grep -n 'def.method.catch_all' src/build/query_extract/extract.rs` |
| php's catch-all spellings | `queries/php/skeleton.scm`, the `@def.method.catch_all` pattern |
| The one query every lane asks | `grep -n 'fn class_answers_any_member' src/model/file_analysis/ancestry.rs` |
| Its readers | `grep -rn 'class_answers_any_member' src/ \| grep -v _tests` |
| Forward-declaration stubs | `grep -n 'fn mint_forward_declarations' src/build/builder/infra.rs` |
| AutoLoader / SelfLoader synthesis | `grep -n 'is_loader' src/build/builder/pipeline.rs` |
| php `@method` / `@property` synthesis | `grep -n '@method' src/build/packs/php/doc.rs` |

Two defects in the current shape, beyond the blanket silence:

- **The family is lost.** The pack capture stamps the class for any
  catch-all, so a php class whose only catch-all is `__get` (properties)
  is silent on undefined METHOD calls, which php rejects at runtime.
- **Declared surfaces don't narrow.** php `@method` tags and a plugin's
  `EmitAction::Method` synthesize real symbols, but the class flag still
  silences every other name.

## What real catch-alls answer

A sample of the 35 packages declaring `sub AUTOLOAD` in a stock
perl 5.38 install (`/usr/share/perl*`, `/usr/lib/x86_64-linux-gnu/perl`):

| Shape | Example | Surface |
| --- | --- | --- |
| Accessor over `$self->{$name}` | `Debconf::Base` | the object's hash keys |
| Delegation to a held object | `Test::Tester::Delegate` | the delegate's methods |
| XS constant / lazy-compile loader | `POSIX`, `Time::HiRes` | a fixed table |
| Genuinely open | `Digest` (`Digest->MD5`) | any name |

## Design

### The surface is a closed enum, carried as data

```rust
pub enum CatchAllSurface {
    /// Answers any name in its family. The default for an unrecognised
    /// catch-all; today's behaviour.
    Any,
    /// Answers exactly the names the class otherwise declares: stubs,
    /// synthesized methods, doc-tag members. The names are symbols
    /// already, so the variant carries none.
    Declared,
}
```

`Declared` carries no name list on purpose: every name it admits is
already a symbol (a `FORWARD_DECL` stub, an `EmitAction::Method`, a
php `@method` row), so ordinary member resolution answers those, and
`Declared` only says "and nothing else". A `Names(Vec<String>)` payload
would be a second copy of facts the producer minted as symbols
(rule #11).

Body-shape variants — `DelegatesTo(..)` (an `Edge` to the held
object's type) and `HashKeysOf(..)` (the invocant's structural shape) —
join the enum in Phase C, with their producer. A variant nothing
produces is dead weight.

It is data, not a query-time plugin hook: a hook can't be cached,
and the cross-file lanes read stripped copies through
`symbols_present`, which never runs plugin code.

### Where it lives

On the catch-all's own symbol, not the class. A class can have several
catch-alls of different families (`__get` answers properties, `__call`
instance methods, `__callStatic` static methods), and each has its own
surface. `DYNAMIC_MEMBERS` stays on the class as the cheap "has any
catch-all" gate. Decide the concrete slot against rule #14 and record
it in `docs/adr/symbol-flags.md`. The expected answer is a field
beside the member's family on the symbol, readable from
`symbols_present` with no bag rehydrate.

### The query

`class_answers_any_member(class, idx)` becomes
`catch_all_answers(class, family, name, idx) -> bool`: walk `INHERITS`
as today; at each class with `DYNAMIC_MEMBERS`, ask each catch-all whose
family admits the member being looked up. `Any` answers; `Declared`
doesn't, because the name already failed ordinary resolution. The walk
stays one `GraphView` walk; the per-language family list comes from the
capture (below), never a name list in the model.

### Who states the surface

1. **Forward declarations (Perl).** A package whose `AUTOLOAD` sits
   beside at least one `FORWARD_DECL` stub is `Declared`. `sub frob;`
   next to an `AUTOLOAD` is the idiom for "AUTOLOAD serves these names",
   and it is what makes `can` answer. `use subs qw(frob)` is the same
   declaration: mint the same `FORWARD_DECL` symbol from the `use` walk
   so both spellings are one fact.
2. **Plugin-declared.** A new `EmitAction` sets a named class's
   catch-all surface. A plugin that knows a framework's catch-all
   synthesizes the methods with `EmitAction::Method` and sets `Declared`.
3. **php doc tags.** `@method` / `@property` rows on a class with a
   catch-all of the matching family. Whether a tag list counts as
   complete is an open question (below); until it is answered they stay
   `Any`.
4. Otherwise `Any`.

### The family comes from the query document

The php capture splits by family:
`@def.method.catch_all` (instance), `@def.method.catch_all.static`,
`@def.property.catch_all`. The spellings stay in `skeleton.scm`
(rule #15); the extractor reads the capture suffix. Perl's `AUTOLOAD`
answers instance and class method calls alike.

### Narrowing makes things louder, so it enters on the ladder

A wrong surface turns today's silence into a false positive. A name a
narrowed catch-all does not answer reports on a new default-off lane,
`notAnsweredByCatchAll`, never on the existing lanes. Promotion follows
`docs/adr/narrowing-diagnostics.md` after a substrate audit. The
family fix (a `__get`-only class reporting undefined methods) is not a
narrowing: it lands on the existing lane directly.

## Phases

### Phase A — the enum and the query, behaviour-frozen

1. `CatchAllSurface { Any, Declared }` on the catch-all symbol; every
   producer mints `Any`.
2. `catch_all_answers` replaces `class_answers_any_member` at every
   reader.
3. **Acceptance:** `cargo test`, gold and e2e identical; no diagnostic
   count moves on the substrate audit.

### Phase B — families, forward declarations, the new lane

1. Split the php capture by family; the extractor stamps each catch-all
   symbol's family.
2. Perl: `Declared` when the package has a `FORWARD_DECL` stub; mint
   stubs from `use subs`.
3. `notAnsweredByCatchAll` in `DiagnosticOptions`, default off.
4. **Acceptance:**
   - php: `__get`-only class, `$o->typo()` reports on the existing lane;
     `$o->anyProp` stays silent.
   - Perl: `sub frob; sub AUTOLOAD {...}` — `$o->frob` silent,
     `$o->frbo` reports on the new lane only when the flag is on.
   - Perl: `AUTOLOAD` alone — `$o->anything` silent, flag on or off.
   - A descendant in another file inherits the parent's surface
     (cross-file, stripped copy).
   - Substrate audit with the flag on: triage every hit in the PR.

### Phase C — plugin-declared and body-recognised surfaces

1. The `EmitAction` for a plugin-declared surface; one real consumer in
   a bundled plugin, or the phase waits for one.
2. Body recognition as a plugin `patterns()` query, not in core:
   accessor over `$self->{$name}` → `HashKeysOf`; delegation through a
   held object's `can` / `->$name` → `DelegatesTo`. Each variant lands
   with its producer and tests.
3. **Acceptance:** per-shape unit tests plus the audit delta.

## Non-goals

- No name lists in the model or core: not `AUTOLOAD`, not `__call`, not
  AutoLoader/SelfLoader beyond what the builder already reads.
- No `Names(Vec<String>)` payload; declared names are symbols.
- No query-time plugin hook.
- Do not touch the contract lane.
- Do not report on the existing lanes from a narrowed surface; only the
  family fix does.

## Language-pack beat

The seam is neutral and packs inherit it: the enum, the family, and
the walk are language-free, and each pack states its catch-all
spellings and families in its query document. php is the first pack
consumer. A pack with no catch-all capture produces no
`DYNAMIC_MEMBERS` and is unaffected.

## Scaling beat

The lanes call this per unresolved member site. The walk stays one
`INHERITS` walk that stops at the first answering class, as today. The
per-name check adds a scan over the catch-alls of each flagged class,
which is at most three per class. Memoize per `(class, family)` for a
diagnostics pass if the audit shows repeat walks; the answer is pure
over (class, family, analysis, index generation). The surface must be
readable from `symbols_present`: a bag rehydrate per flagged class on
`--check` is the cost to avoid.

## Relation to Epic 3 (Openness)

`OpenCause::Autoload` is this fact. With this brief, openness becomes
per name: a class with a `Declared` catch-all is Open for its declared
names and Closed for the rest. Epic 3 Phase A builds `openness_of` to
take the member name and family for that reason. Whichever lands
second folds `catch_all_answers` into the verdict as the `Autoload`
cause, not a parallel rule.

## Open questions for Veesh

- Does a php `@method` / `@property` list on a class with a matching
  catch-all count as a complete declared surface? Generated lists
  (Laravel IDE helper) tend to be complete; hand-written ones often are
  not.
- Is an imported `AUTOLOAD` (`use AutoLoader 'AUTOLOAD'`,
  `*AUTOLOAD = \&...`) in scope? Today it mints no flag at all, the
  opposite error.

## Invariants

- `DYNAMIC_MEMBERS` stays the class-level gate; the surface never
  requires a bag read.
- `parents_of` / `GraphView` stays the single ancestor walk.
- A catch-all nothing narrows behaves exactly as today.

## Verification gate

`cargo test` (both feature sets) · gold 0 FAIL / 0 XPASS, warm pass
included · `./e2e/run.sh` · substrate audit before and after with the
new flag on and off, per-code deltas and triage.
