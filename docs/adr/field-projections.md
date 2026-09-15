# ADR: Field projections — one decl, every spelling, one rename group

> **Shared identity with C:** `AttrProjection { class, attr }` IS the Perl
> realization of the language-generic `WitnessAttachment::Field { owner, name }`
> bag primitive (the C struct-member domain subject) — same `(owner, name)`
> shape. `FileAnalysis::field_subject(class, name)` mints that canonical subject
> for any Perl field (owner = declaring class via the ancestor walk);
> `field_subject_of_ref` routes any access shape (accessor call / `$self->{k}` /
> Corinna field var) onto it without seeing the flavor. The refs-splat below and
> the C domain fold are two consumers of the ONE subject, and a pack field's
> VALUE edge (`Field → Edge(Variable{decl})`, chased by `ValueHop`) is a third —
> `member-kinds.md`. Perl *domain* typing on that subject is deferred — see
> `docs/cpp-golive-map.md` item 3.

A framework field declaration is ONE name spelled several ways. Moo:
`has size` ↔ accessor `$w->size` ↔ ctor key `Widget->new(size => …)` ↔
internal `$self->{size}`. Corinna: `field $x :param :reader` ↔ ctor key
↔ reader call ↔ body `$x` uses. Rename and references must treat the
constellation as one entity — and must NOT admit lookalikes (another
sub's `name =>` argument is not the attr).

## The entity: `AttrProjection`, minted at synthesis

`AttrProjection { class, attr, kind: CtorKey | InternalKey | Accessor }`
rows on `FileAnalysis`, emitted by the synthesis that already knows the
semantics — Moo/Moose/Mojo::Base `has` mints CtorKey + Accessor +
InternalKey; Corinna fields mint CtorKey/Accessor only (fields are not
hash entries, so `->{name}` on a Corinna class is unrelated — or a
bug). The repr gate IS whether InternalKey was minted: no query-time
"is this class hash-backed" side condition exists, because the minting
site is the one place that knows. Plugins enroll name-mapped
projections (`predicate => 1` → `has_size`) through the same entity.

## The pair is a minted relation

A `has` accessor and its constructor key are two symbols from one token;
so are a DBIC column accessor and its bridged key, and a php promoted
constructor parameter and its field. The minting site records the pair as
`Symbol::declared_with` (each way), and every consumer — `attr_pair_group`,
the promoted-param twin, the rename group's variable-use fold — reads that
relation. Span equality and sigil-column arithmetic are never the signal
(CLAUDE.md rule #11): a real `sub name` sharing a class with someone's ctor
key is not a pair, and the minting site is the only place that knows.

## Membership is strict, never `found_by`

Internal-key membership matches `HashKeyOwner::Class(class)` by strict
equality. The rejected broadening — collecting via `found_by` — would
admit *other subs'* same-named arg keys (`$obj->search(name => …)`)
into the attr's rename. This is the load-bearing line: the group grows
only through projections minted by synthesis or owners resolved to the
exact class.

## Query side: `ResolvedTarget::Group` with per-member rename

`resolve_symbol` from ANY spelling returns the whole group:
`local_spans` (the class file's own spellings), `pinned_spans` (class
file spans when the cursor sat in a consumer — the group is minted from
the CLASS's analysis, fetched via `CrossFileLookup`, so consumer-side
cursors see the full constellation), and `members`, each a walkable
`TargetRef` carrying its own `MemberRename`:

- `Bare` — the spelling takes the new name verbatim;
- `Affixed { prefix, suffix }` — derived names re-derive (`has_size` →
  `has_extent`), including user-written `sub _build_size` defs;
- `Skip` — members whose names don't embed the attr join references
  but honestly decline rename.

Rename and references read the SAME group, so they cannot disagree —
the same single-sourcing rule as `resolve_symbol`/`refs_to`
(`adr/file-store-and-resolve.md`).

## Consumer-file keys without the class

Files that only `use` the class still index ctor keys:
`Gate::StrictOrDefer` emits `owner: None` candidates at build, and
query-time re-derives the owner (`deferred_hash_key_owner`) — the
receiver-gated discipline applied to hash keys. This is the one
deliberately lazy resolution left in the ref model: dependency files
are never enriched, so candidates ride the cache and resolve when
asked.

## Domain fold on the C side: the threshold is not a tuning problem

The C `DomainCoherenceFold` over `Field { owner, name }` folds per-site
evidence into a nominal domain (a field compared against `OP_*` constants is
that enum's domain). The coherence threshold is measured, not tuned: on op.c,
85.7% of `op_type` sites are strong opcode evidence and the genuine raw-int
noise is 2 sites out of 1759, so any threshold in (0.5, 0.99) fires cleanly.
The fold is a majority-agreement check on the field subject, not a per-field
allowlist — the same subject the refs-splat and Perl domain typing consume.
