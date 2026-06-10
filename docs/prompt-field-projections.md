# Field projections: the rename tie, what's done and what's left

A framework field decl is ONE name spelled several ways. Corinna:
`field $x :param :reader` ↔ ctor key `Point->new(x => …)` ↔ reader
`$p->x` ↔ body uses of `$x`. Moo: `has name` ↔ accessor `$obj->name` ↔
ctor key ↔ internal `$self->{name}`.

## Done (June 2026)

- `FileAnalysis::field_group_at` / `rename_field_group`: in-file rename
  from ANY Corinna spelling (field decl, body use, ctor key at a call
  site) rewrites the whole group, sigil-preserved. Wired as the first
  branch of `rename_at`, so LSP + CLI single-file renames both get it.
- Synthesized projections (`:param` ctor HashKeyDef, `:reader`/`:writer`
  methods) select the **bare-name sub-span** of the `$x` token — a bare
  replacement can no longer eat the sigil.
- `:reader` call sites are included in the group's edit set.

## Landed (the union, June 2026 overnight session)

1–4 of the original gaps are DONE: `ResolvedTarget::Group` rides
`resolve_symbol` (reader-call cursors included), `resolve::group_refs`
walks the group cross-file for both references and rename (backend +
both CLI mirrors), in-file `find_references` unions (highlights /
linked-editing inherit), and Moo `has` attrs join via the pair
signature (accessor Method + ctor HashKeyDef sharing name, package,
and selection span). Consumer files' ctor keys exist now too:
`Gate::StrictOrDefer` emits `owner: None` candidates when the class
isn't local, and `refs_to`'s key arm / goto-def re-derive the owner at
query time (`FileAnalysis::deferred_hash_key_owner`) — the
receiver-gated discipline applied to hash keys.

## Remaining gaps

1. **Internal `$self->{name}` keys aren't in the group** —
   CONTENTIOUS, deliberately deferred: collecting them via
   `HashKeyOwner::found_by` broadening would also admit *other subs'*
   same-named arg keys (`$obj->search(name => …)`) into the attr's
   rename. Needs a strict-Class-owner membership design first.
2. **Consumer-side cursor, class elsewhere**: rename/references from a
   ctor key or accessor call in a file that only `use`s the class falls
   back to narrower behavior (the group is detected against the local
   analysis only). Fix direction: in `resolve_symbol`, on a deferred
   key / cross-file accessor, fetch the class's cached analysis via
   `CrossFileLookup` and mint the group from THERE (its variable spans
   become `RefLocation`s in the class file).
3. **`:writer` (`set_x`) and Moo `predicate`/`clearer`/custom-named
   accessors** aren't group members — their names differ from the attr;
   tying them means a name-mapping on the group (plugin-owned for Moo
   per the accessor-vocabulary split).

## Internal `$self->{name}` membership — design options (pick one)

Decision so far (veesh): YES for classic Perl OO where the underlying
repr is known to be a hash. Corinna is excluded outright — fields are
not hash entries, so `->{name}` on a Corinna class is unrelated (or a
bug). The gating options:

**A — strict-owner matching at query time (no new entity).** Internal
keys already carry `HashKeyOwner::Class(class)`; group membership =
HashKeyAccess refs whose owner is EXACTLY `Class(class)` (strict eq,
never `found_by` — that broadening is what would leak other subs' arg
keys into the group) + name == attr, included only when the class's
repr is hash (FrameworkFact Moo/Moose/MojoBase, or a bless-hashref
witness). Zero schema change; ships in an afternoon. Weakness: the
repr check is a side condition, not a modeled fact, and untyped
derefs (owner None) stay out silently.

**B — a proper repr entity: `ClassRepr` on FileAnalysis.**
`enum ClassRepr { HashRef, ArrayRef, Opaque }` derived at build (bless
shape, framework fact), serde/cache-borne. The group asks
`repr(class) == HashRef` before including the Class-owner arm — and
the entity is reusable: completion on `$self->{`, a phase-6-style
diagnostic for `->{...}` on an Opaque class, blessed-array support
later. Costs a schema field + EXTRACT bump + derivation rules
(multi-bless classes, mixed shapes → Opaque, honest).

**C — first-class projection entity (the "proper entity instead of
reusing HashKeyDef" instinct).** Generalize `AttrAccessor` into
`AttrProjection { kind: CtorKey | InternalKey | Accessor { affix } }`
emitted by the synthesis that already knows the repr; the group is
then ONE stored constellation instead of query-time re-derivation
across Field symbol + HashKeyDef + Method + AttrAccessor. Biggest
lift; subsumes B (the entity still needs repr knowledge to be minted).

Recommendation: **B as substrate + A's strict-eq membership rule**,
deferring C until a second repr flavor (array-based, Object::Pad) is
actually wanted — don't build the generalization before the second
customer.

## Remaining sliver from the mapped-member work

Cross-file mapped call sites: `ResolvedTarget::Group` targets carry one
replacement text, so a consumer file's `$w->has_size` joins references
but not rename yet — needs per-member replacement on group targets
(`Vec<GroupMember { target, rename: Bare | Affixed | Skip }>`).
