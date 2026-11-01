# ADR: the capture vocabulary is a closed enum

A pack language's extraction is a query document plus a generic driver
(`query-extraction-rings.md`). The document owns syntax: which node is a
def, which token names it, which node opens a scope. The driver owns
meaning, and it reads meaning off the capture's name: `@def.sub`,
`@ref.call`, `@flow.target`. The capture name is the whole contract
between the two, so the set of names is a vocabulary both sides must agree
on.

## Decision: one enum is the vocabulary

`query_extract/vocab.rs` declares `Capture`: one variant per capture
family, each carrying a sub-enum of exactly the suffixes the documents
use, named `<Family>Cap`. The families that name an entity kind stay
typed past the driver: `DefCap` is `SkelSymbol.kind` and `RefCap` is
`SkelRef.kind`, so the projection to `FileAnalysis` matches on the enum
too. The `Cap` suffix keeps them apart from the model's `RefKind` /
`SymKind`, which they project to.

Each enum's spelling table is written once, in a `spelled!` invocation,
which generates `parse`, `spelling` and `Display` from it. The two
directions cannot disagree.

## Decision: a capture name is read as text in one place

`Capture::parse` turns a name into a `Capture`. Nothing else reads a
capture name: the driver, the pack hooks and the projection all match on
the enum. A pack hook that depends on a kind takes the typed kind
(`default_name: fn(DefCap)`).

## Decision: the vocabulary is checked when the query compiles

A compiled pack query (`CompiledPack`) carries its capture table beside
it, one `Option<Capture>` per capture id, built when the query first
compiles. A name outside the vocabulary fails the compile with
`unknown capture @<name>`, so a typo or an unlearned capture in a bundled
document fails that pack's tests instead of extracting nothing.

A name starting with `_` follows tree-sitter's predicate-only convention
(`@_const` under `#eq?`). It maps to `None`, and the driver never sees it.

## Decision: structural predicates are a closed set too

Core tree-sitter evaluates text predicates (`#eq?`, `#match?`,
`#any-of?`) and hands every other predicate to the caller unevaluated.
A document needs one more kind of filter: where a node sits. A member
read is a `field_expression` that is not a call's callee, and that fact
lives on the parent, which a negated field (`!arguments`) cannot test.

`build/query_predicates.rs` evaluates Neovim's structural forms, with
the same names and arguments: `#has-parent?`, `#not-has-parent?`,
`#has-ancestor?`, `#not-has-ancestor?`, each `@capture kind ...`, plus
`#is-kind?` / `#not-is-kind?` on the captured node itself. A wildcard
cannot exclude a kind, so a pattern that splits one slot by its kind (a
member receiver written as a bare name is `@member.recv.name`, any other
is `@member.recv`) states the name pattern with the kind and the other
with `#not-is-kind?`, and each match carries its fact. Every
node the capture holds must satisfy it. `check` runs when a query
compiles and fails it on any other predicate name, a malformed argument
list, or a kind the grammar lacks, because an unread predicate is a
filter that silently matches everything. `admits` runs per match, before
any capture is recorded. Both query runners use them: pack extraction,
and the Perl plugin patterns, where `#receiver-isa?` is the one predicate
deferred to query time.

Ancestry is read by one descent from the walked root
(`child_with_descendant`), because `Node::parent` descends from the root
too and would repeat the walk per hop. Only patterns that carry a
structural predicate pay it.

## Decision: a kind's meaning lives on the kind

What a kind means is a method on its enum, matched exhaustively:
`DefCap::sym_kind` (the `SymKind` it projects to), `DefCap::marker`
(the `union` / `reexport` / `macro` attribute and the `SymbolFlags` it
implies), `MarkCap::flag` (the `SymbolFlags` a `@mark.<flag>` states about
the declaration whose name token it captures), `LitKind::value_type`,
`ObsKind::observation`. A new kind does not compile until it says what it
is.

## Decision: each capture family has one handler file

`query_extract/extract/` holds the driver (`mod.rs`: the event flatten,
the sort, the shared `ExtractState`) and one file per family: `scopes`,
`defs`, `refs`, `members`, `flow`, `narrowing`, `commands`, `imports`,
plus `links`, which the driver reads while flattening (below).
`family(Capture)` is an exhaustive match, so a new variant does not
compile until it names its family. A family file may expose up to three
entry points, each run by the driver at a fixed point:

- a flatten hook, for a capture that needs its live node (a declarator to
  peel, a receiver to unwrap). What it reads that the text cannot carry
  rides on the event as a `Payload`;
- `handle`, called once per event in source order, with the scope stack
  and the ancestor stack in force;
- post-passes, which the driver lists in the order their rows and
  witnesses land. That order is load-bearing: the witness bag is
  latest-wins for some reducers, and a flow join reads the symbol table
  only after def dedup and the command defs.

## Decision: the extractor joins on the syntax tree, not on tables

Every capture is a fact on its node in the core syntax tree
(`docs/adr/core-syntax-tree.md`): a `NodeFacts` entry on the node's site,
the head of the same-range chain it sits in, so the tags other patterns put
on one node are that site's facts (`on`). A node's subtree is an id
interval, so the captures inside it are one slice (`within`, and
`list_items`, which skips a nested list of the same tag by its subtree
end); the captures holding it are on its enclosing sites (`around`). The
walk visits sites in preorder, a site's captures by tie rank then match
order. Beside the tree sits only match → captures, because a match spans
nodes: every capture a pattern made is one lookup from any other
(`in_match`). The walk records what it minted on each capture (`Minted`: a
value witness, a declaring name, a class's identity, a member read's
projection), so a capture of any match asks the node it names.

A handler therefore keeps no join table. The state a family keeps is only
what the walk alone knows when it reaches an event: the scope a target
binds in, a context waiting for its body's scope, a narrowing waiting for
the flow edges. A new cross-pattern fact is a field on `Minted` or a
`Payload`, never a map keyed by match id or span.

A feature that teaches the extractor a capture touches the variant, the
family routing line, and the family file it lives in. When a language
needs extraction a query cannot express, its frontend adds a typed Rust
handler in the same shape rather than a string-keyed hook on the pack.

## Decision: a scope capture says what the scope does

A scope's name-lookup rule is the document's to state, never something a
consumer reads off the shape of the scope or the language serving it.
Python's class body is in force for the code written directly in it and
not for a method of the class or a class nested in it; PHP's function
body sees no variable of the scopes around it but the ones it captures.
Both are rules ABOUT a scope, so each is a `ScopeCap` spelling the
document opts into (`@scope.direct`), minted as a field on the model's
`Scope` (`Scope::direct`) — rule #11, at the producer — and read by ONE
walk, `lookup_chain`, which every name lookup takes: a variable read, a
bare class spelling. A pack that says nothing keeps the plain lexical
chain.

The walk is a type, so a lookup cannot route around it. `lookup_chain`
returns a `LookupChain`, and that value is the only way to list the
names a scope binds for lookup (`LookupChain::bindings`, which also
drops a member declared through a receiver). The containment climb,
`scope_chain_of` — what a scope is inside: its class, its callable, its
package — returns bare ids and lists no names; what a scope DECLARES,
for the outline and a class's members, is `SymbolTable::
declared_directly_in`, named so no one reads it as a lookup. Every raw
climb up `Scope::parent` outside the walkers is allowlisted with its
containment reason (`layering_tests::raw_scope_climbs_are_containment_only`,
count-exact, shrink-only): a new one fails `cargo test` until it moves
onto the lookup chain or says why it is containment. In-scope
completion, `visible_symbols`, the receiver a `self.x` write joins, and
Perl's binders (`resolve_variable_refs`, the hash-key owner fallback,
`visible_variable_decl`, gated parameter types) walk it; Perl marks no
scope `direct`, so its answers are unchanged.

Lookup is not ownership. Whose member a declaration is comes from the
scope a class's own match opened (`@context.class` with its `@scope`),
and stops at a callable whatever the document marks; how a name resolves
comes from `lookup_chain`. The two share one fact, the callable boundary
(`callable_reach`: a site's scope out to the nearest callable). A
declaration joins the class whose body is on its reach, and a direct
scope is in force for a use only as the innermost direct scope on its
reach. Neither is a namespace section: the lane holds namespaces only.

Where code runs is a scope fact too. A def's header — Python's base
list, parameter annotations and defaults, `-> T` — is evaluated in the
scope the def is written in, though the def's own `@scope` covers it.
The document marks each header region `@scope.header`, and the extractor
mints it as a scope whose parent is the def's declaring scope: a use
written there, and a lookup at any point in it, starts outside the def
by the ordinary walk, with no consumer comparing a position against a
body. A name the header DECLARES (a parameter) still binds in the def's
own scope (`ExtractState::bind_scope`). One fact for every header
expression, not an arm per construct: the base list is one more region.

The spelling names the behaviour, not the construct: `direct` is a scope
whose names are in force only for code written directly in it, whatever
the language calls that scope. A generic capture may never carry one
language's rule — `@scope.sub` once minted a Python-only section kind,
which is why the rule moved here.

## Decision: a member is declared where the document says, once

Python's document captures two member shapes, and each reuses a model
the rest of the server already reads.

A name a class body binds (`count = 0`, `a: int`, `b: int = 5`) is two
names from one token: the class's data member (`@def.field`,
`Counter.count`) and a name in the class scope (`@def.var`), which code
written directly in the body reads bare through `lookup_chain`. The match
says so with `@def.pair`: the two defs carrying one match id are linked
each way as `Symbol::declared_with`, the relation Perl's `has` already
mints. The twin is no class content of its own. The pair is a field group
(`field-projections.md`): the member is its reader, read rather than
called, and the twin's uses are its variable spellings, so a rename from
either spelling, in any file, rewrites both. A
binding with a value is `@mark.static`; a bare annotation declares an
instance's attribute and stays unmarked.

A write through a receiver (`self.n = …`, `@def.slot`, receiver token
`@def.slot.receiver`) declares an instance slot: a `HashKeyDef` owned by
`HashKeyOwner::Class`, Perl's own model for `$self->{n}`. The owner is the
class the receiver is TYPED as at the write — the receiver parameter's
class, an annotated parameter's or local's annotation, a local's value
(`o = Counter()`) — asked of the assembled analysis's own registry
(`skeleton::declare_slots`, the last step of `into_file_analysis`, the
slots adopted into the sealed baseline). A write declares only when
neither its class nor an ancestor already declares the name (a class-body
binding, a member, an earlier slot); every other write, augmented or not,
and any write through an untyped receiver, stays a member reference. The
write's own token stays a member reference too.

What a write puts in a member is a separate fact, and not a slot's: an
assignment writes the place its left side denotes. `@flow.place` marks
the left side, whatever it is, beside `@flow.source`; the vocabulary has
no lvalue kinds. The place is what the left side's own read projects, so
a write lands where a read of the same expression looks, at any depth
(`self.a.b.c = v` writes `c` of the class `self.a.b` is). A name's place
is its binding, which `@flow.target` writes and rebinds; a left side no
read projects (a subscript, until element reads are minted; a tuple,
until destructuring pairs its elements) takes nothing. The member token
of a place a member read projects is a `Write` ref, whether or not a value
flows in: C++ marks every assignment's left side a place and gives only
`=` its `@flow.source`, so `q->n += 1` writes `n` and states no value. A
member read's place is the member of the class the receiver's value is
(`skeleton::settle_member_writes`, after the slots, adopted into the
sealed baseline with `FileAnalysis::adopt_witnesses`). A
member the file declares on that class or an ancestor takes the write
into its declaration's `Variable`, as a variable takes one, so a declared
type outranks what flows in. Any other member — an instance slot, or one
declared in another file — takes it on `Field{owner, name}`, the
attachment a C++ or php member declaration's value rides and a member
read's `ValueHop` chases, where each write is an arm and the arms agree
as a hash slot's writes do: two classes are no answer. The witness spans
the class body, as a declaration's does, so a method above `__init__`
reads what `__init__` writes.
`TODO(member-write-declared-elsewhere)`: a write to a member declared in
another file lands on the writer's `Field` and shadows the declared type
until the declaration is decided with the index.

`TODO(slot-owner-deferred)`: an owner only the index can name (a receiver
typed by another file, a parent declared in another file) is still decided
without it. The target is Perl's shape — the write is a ref whose owner is
a late binding (`resolve_hash_key_owners`,
`fixup_call_bound_hash_key_owners`, `deferred_hash_key_owner`), and
"declared" is the first write whose decided owner is the class — landing
with the php variables stack (`unify_assigned`, `TODO(assigned-unify)`).

A slot is reached only through a receiver: it is no bare name anywhere,
because the lexical walk binds Variables alone. Member lookup reads it
first — `NameSpellings::instance_slots` is the language's data saying its
instance slots shadow the class's members (`BeforeClass`), so
`resolve_member_in_ancestors` walks the ancestry for a class-owned slot
before it walks it for members, and `slot_class_of` makes a slot class
content for goto-def, references, rename, hover and completion. Perl
keeps its keys apart from its methods (`Apart`), so nothing changes there.

A receiver parameter holds an instance of its method's class. The
extractor states that as the parameter's value at its binding site, so a
member written or read through `self` dispatches on the class.

## Decision: a fact the grammar parses is a capture, not a text probe

When the grammar already gives a fact its own node, the document captures
that node and the driver reads the capture; it never re-derives the fact
from the text of a wider capture. Python parses `from ..x import C` as
`relative_import(import_prefix "..", dotted_name "x")`, so the dots are
`@import.relative` and the module is `@import.prefix`, joined to each
`@import.name` by match. Whether an import is relative is whether the
capture fired, and its level is the number of separators the capture
holds. What a level climbs from is the language's `ImportPrefix` rule
(`PackageRelative` for Python); a language without relative imports
(PHP's `use \A\B\{C}`) never captures one.

## Decision: depth is chained from links, never walked

A tree-sitter pattern has a fixed depth: no descendant axis, no recursion,
and quantifiers repeat siblings, never nesting. Some structure nests
without bound, such as a declarator wrapped any number of times
(`Foo**& f()`) or a name qualified any number of times (`A::B::m`). One
pattern per depth would silently miss the next depth.

So the document states each level once, as a link: `@link.from.<walk>` on
a node and `@link.<rel>` on the node one level away, by a closed set of
relations (`inner`, `declares`, `scope`, `name`, `base`). The `<walk>` suffix
names the walk the link belongs to, from a closed set (`ool`, `qualifier`,
`deref`, `spelled`), and each walk has a root capture (`@ool.def`,
`@qualifier`, `@nested.target`, `@ref.spelled`). The `spelled` walk carries
a reference its match names by the whole spelling from the node the
document tagged down to the token it rewrites (`f` in `a::b::f<T>`), and
mints the scopes it passes as the reference's qualifier, so no consumer
splits the spelling to recover them. The `deref` walk also reads `@deref.*` marks, which say
what a level is rather than where it leads (`docs/adr/pointer-stack.md`). The driver
records every link while it flattens the matches, keyed by walk and node id.
A walk starts at its root's node and follows only its own links
(`extract/links.rs`), so two walks never read each other's edges, and a
reader of the document sees which construct each link serves. Rust reads no
node kind and no field to do it; which node links to which is the
document's statement. The compile fails on a pattern with a link but no
single `@link.from.<walk>`, a `from` with no link, or a walk whose root
capture the document never uses.

## Consequences

- A new capture starts as a new variant. The vocabulary change is an enum
  diff, reviewable apart from the handler that reads it.
- Links are read before any other capture, because a level inside a
  definition is matched after the definition. The driver keeps the whole
  file's captures until every match is in, then flattens them.
- A variant can have no reader. `@flow.assign` names a pattern's root so
  the document can anchor `@flow.target` and `@flow.source` under it; the
  driver reads the two halves, not the anchor.
- The enum covers the pack skeleton documents only. `perl/flow.scm` (read
  by `builder/narrowing.rs`), the rhai plugin patterns
  (`pattern_dispatch.rs`) and cpp's internal reparse queries name their own
  captures, read by their own code.
- The capture table lives as long as its compiled query: one per pack,
  leaked to `&'static` for the life of the process.
