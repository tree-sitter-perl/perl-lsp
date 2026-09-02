# PHP dogfood round 5 — hitlist

Three agents, ~150 grep-cross-checked probes: composer + Slim (D, new
corpora), phpMyAdmin (E, new, 1232 files), and an adversarial re-probe of
every round-4 fix on the old corpora (F). Binary: release cpp,php at
c13b5b4. Findings files: `scratchpad/round5-findings-{D,E,F}.md`
(session-local).

**Round-4 wins that HELD under fresh coordinates** (F): `@inheritDoc` /
`@phpstan-*` param inheritance (7/7 new sites incl. a 3-hop chain),
destructuring safe subset (property-tuple doc → foreach-as-list), `parent::`
vs interface stub (4/4, a different class family), PHPUnit
`#[DataProvider]`/`@dataProvider` (8/8), `getSubscribedEvents` (4/4 incl. a
`::class`-keyed map), Doctrine `@method` rows, lexical-visibility
completion (exhaustive 224-vs-218 diff: exactly the 6 protected members),
stdlib string-callables (5/5 + `'Class::method'` resolving on both
segments), heatmap counts on guzzle/demo byte-identical to round 4.

## LANDED in this round's first slice

- D1 `$this` never hovered → hovers as the enclosing class.
- D2/D3/D6e narrowing shapes: `instanceof` as an `&&` conjunct (either
  side), `elseif` arms, namespace-qualified class tokens (the guard
  leafs the class like every other spelling).
- D6a/E1 `new Foo(...)` never counted as a use of `__construct` — 275/438
  of phpMyAdmin's dead queue, 63%. Two causes: a decl-side cursor minted
  a Sub target without `ctor_of`, and the relational retrieval keyed only
  on `__construct` (files holding `new Foo(` were never scanned). Composer
  dead 266 → 228; the fixture ctor is live with fan-in 2.
- D6c `[$this, 'm']` / `[$obj, 'm']` instance-array callables mint member
  refs (the class-array form already did).
- D5 nested tuple mistyped → root cause was KEYED destructuring
  (`['k' => $v] = f()`) binding nothing; `Extraction::KeyOf` now lowers to
  `Projected{Expr(rhs), HashKey(k)}` (assignment + foreach forms) and the
  composer site reads `$message: string`.
- E4 `match` typed by its discriminant → match/ternary publish
  `BranchArm` arms (the literal-narrowing heuristic no longer sees inside).
- E5 `f()[0]` / `$row['name']` subscripts project off the base
  (ArrayIndex / HashKey).
- E2 `/** @var Sub $x */` above a RE-assignment ignored → casts the local
  from that site; named `@var` rides at annot priority (the call-binding
  edge pushed later no longer overrides it).
- E3 hover at a rebind showed another branch's assignment line → a
  variable hovered off its def row renders its name only.
- D7 completion detail leaked engine spellings (`Sequence<String>`) →
  token-wise type-label translation (`list<string>`).
- E1-adjacent: `--references` on a ctor decl now includes cross-file `new`
  sites (pinned).
- R5-1 same-leaf classes (three `Collection`s, three `Request`s, two
  `Factory`s): php visibility was `VisibilityAxis::Flat` (a placeholder),
  so every leaf-keyed lookup saw every same-leaf class and rename fanned
  across unrelated ones. Now `VisibilityAxis::UseMap`: the origin's own
  `use` rows + class decls + own namespace pin each leaf; a pinned leaf's
  candidates are the declarations under that namespace only (an
  un-indexed pinned class answers EMPTY, never a stranger), an unpinned
  leaf ranks the own namespace first. The pin rides `TargetRef::class_ns`
  and every scanned php file is matched under ITS OWN axis, so the
  references/rename walk drops files whose same leaf means another class
  (`Process\Factory` out of `Http\Client\Factory::$recorded`'s rename;
  a no-`use` file in namespace `A` never joins `B\Collection`'s
  references; the class NAME's own references walk is gated the same
  way). The own-namespace default claims only leaves the file SPELLS as
  class tokens (type positions now mint a class ref — `Collection $c`
  joins the class's references and rename — plus parent clauses, `new X`,
  `X::m()` receivers): a file reaching a class only through another
  class's dispatch makes no claim about it. A leaf with conflicting
  evidence (declared here AND imported: `use Support\Collection as
  BaseCollection; class Collection extends BaseCollection`) pins to
  nothing. Known lies, all "no claim" (never a wrong pin): `use X as
  Alias` (the alias spelling has no pin — the raw import row carries
  only the FQ name), inline FQ spellings (`new \B\Collection` in a file
  that pins `Collection` elsewhere), files declaring several namespaces
  (no own-namespace default), a function or method named like a class
  leaf counting as a spelling.
- R5-2 property and method sharing a name (`Factory::$recorded` +
  `Factory::recorded()`): member resolution was name-keyed across kinds.
  The written shape is now value-borne — `MemberShape` on the ref (an
  argument list or a callable-string form = Callable, a bare member read
  = Value; Perl's `$o->m` stays Unknown), `ProjectionStep::ValueHop` for
  the arity-less hop (the registry prefers the class's `field_edge` over
  the method's return chain when both exist), and `TargetRef::member_shape`
  minted ONLY when the class overloads the name across kinds
  (`member_kinds_overloaded`), gating declaration and reference matching.
  `resolve_member_in_ancestors` prefers the agreeing kind on every class
  of the walk with the other kind as fallback, so a class that does not
  overload answers exactly as before (cpp callable fields keep working).
- R5-3 receiver shapes: `$this->prop::m()`, `$cls::m()` (with `$cls =
  Foo::class`) — the scoped-call lane had only bareword and relative
  scopes; expression scopes now ride the same member lane (the receiver
  types like any member access) and `Foo::class` types as the class
  (`expr.classref`, the ctor's alias-graph edge). Trait-internal
  self-calls and `$this->prop->m()` already resolved on re-probe.
- R5-8 class-name references: `new Foo(...)` sites are admitted for a
  Package target in a pack with a constructor convention (the ctor
  FunctionCall spells the class), and bareword static receivers
  (`Foo::m()`, `Foo::CONST`, `Foo::$p`, `Foo::class`) mint a class ref —
  the class's references and rename reach every spelling.

## OPEN — next slices

### R5-4 (MAJOR, PARTIAL) — `--heatmap` on 1232 files: ~2 min cold AND warm
Attributed 2026-09-02 (phpMyAdmin, 1232 php files, quiet box, one run each):
indexing is 3.0 s cold / 0.2 s warm; the rest is the per-symbol
`references()` walk — 9,707 walks for 7,282 listed symbols, ~14 ms each.
`Modules: 0 cached` is the Perl @INC line and is irrelevant here.
LANDED: the pack tier now pre-prunes from its own row store (it never had
the hub's prune — the comment believed pack refs were absent from the row
store; the pack persist writer shreds them): walks 9,707 → 5,771, cold
2m29s → 2m03s, warm 2m20s → 1m58s, every fan-in identical (7,282 rows, 0
differing, resident-only walk vs pruned). A constructor counts its class
key as a reference row (its references are the `new` sites).
The pre-round-5 binary (before the use-map axis) measures 2m03s cold /
1m57s warm on the same corpus — the prune paid for exactly what the
per-walk axis construction and the member-shape gate added (~25 s), and
438 of 7,282 fan-ins differ between the two binaries, all the same-leaf
separations the axis exists for (`tests/unit/Stubs/ResponseRenderer.php
::response` 520 → 12: the stub's own sites, no longer the real class's).
Memoizing each file's use-map pins per analysis (the walk built an axis
per scanned file per query) takes warm to 1m50s, fan-ins identical.
OPEN: the remaining 5,771 walks are names with SOME reference row
somewhere; each still costs ~19 ms — the same per-walk cost the
pre-round-5 binary paid, i.e. the baseline matcher (candidate retrieval
is already row-narrowed; the cost is rehydration + the per-ref match on
each candidate). Next lever: attribute one walk with `ghost_stats`
(`refs.matcher_upgrade` vs rows-view counts, `mroc.candidate_fetched`)
before touching the matcher.

### R5-5 (design, open for the user) — union types
D4: `list<A|B>` (a union INSIDE a generic) kills element typing for the
whole loop var — hover/gd/refs/completion all dark, isolated against a
working `list<Single>` control. `InferredType` has no union; the honest
options (a `Union` variant with dispatch = intersection of the arms'
members; or "first arm wins"; or stay dark) are a lattice decision.
Logged in `docs/open-forks.md`.

### R5-6 (design, open) — dead-code queue: out-of-tree callers
D6b/d: public Plugin/Event API, PSR interface implementations, Symfony
Console overrides — callers live outside the indexed root. LANDED (data):
`symfony.entry.json` shields the Console `Application` overrides
(`getDefaultCommands`, `getDefaultInputDefinition`, `getDefaultHelperSet`,
`getLongVersion`, `doRun`, `doRunCommand`, `doRenderThrowable`). OPEN: the
general "public API of a library" question needs a policy (e.g. a
`--heatmap` `--library` mode that never flags public members) —
`docs/open-forks.md`.

### R5-7 (MINOR, LANDED) — `array<array<mixed>>` nested generics were dark
The inner `array<mixed>` parsed to nothing (`mixed` has no lattice value)
and the `?` collapsed the whole annotation; a generic whose element is
`mixed` now IS the bare `array` shape, so the outer types as `list<array>`
and the foreach element as `array`. `list<array<string,int>>` and
`array<string, list<Cell>>` already resolved on re-probe.
The `self::$prop` / `static::$prop` / `Foo::$prop` hover half (E7)
resolved on re-probe: all three render the declared `@var` type and gd/refs
land on the property. Rendering note: a php `array<string, int>` doc type
hovers as `array<string, int|float>` — `Numeric` is one lattice value and
the php display spells it as the union.

## Verification contradictions resolved
- D2 "keyed foreach breaks narrowing": the minimal keyed case narrows;
  the composer site's guard was an `&&` CONJUNCT — the conjunct shape was
  the gap, keyed foreach never was.
- D5 "nested tuple over a literal collapses to array": the inner tuple's
  first element was bound by a keyed destructure that bound nothing; with
  `KeyOf` lowered the site types exactly.
