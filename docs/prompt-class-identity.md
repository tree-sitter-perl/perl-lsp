# Class identity is the fully-qualified name — for every language

## The problem

Perl mints class identity as the FQN: `ClassName("Foo::Bar")`, a method
symbol's `package` is `Foo::Bar`, `PackageSymbol{package: "Foo::Bar"}`.
A use-map pack (PHP) that minted the LEAF instead had every consumer
re-derive the namespace from the origin file's use-map pins — a lossy
projection minted where the extractor had the qualifier in hand, and the
source of a silent wrong answer: `resolve_member_in_ancestors` walked the
leaf's candidates and took the first class declaring the member, so when
the named class lacked it and a same-leaf stranger had it, the answer
landed on the stranger and every lane read Some/None and said nothing.
Real-world code is dirty by design (a class edited half-way, an import not
yet written), so the over-approximation is WANTED — but it has to be loud.

## The decision

1. **Identity is the FQN, spelled with the language's own separator.**
   `ClassName("App\\Support\\Collection")`, method symbols with
   `package = "App\\Support\\Collection"`, `PackageSymbol{package: FQN}`,
   while a class token's ref keeps the spelling as written (a qualified
   one in full, its span on the leaf alone, so a rename rewrites only the
   leaf) and names its identity through `spelled_identity`. The index
   registers the FQN; C's flat linkage is its own identity (the hook
   is the identity function there).
2. **The relational key stays the leaf.** `split_qualified` / `name_match_key`
   know the namespace separator, so SQLite `refs`/`syms` rows and
   `RefTable::by_key` buckets are keyed by the leaf as before — a written
   spelling and its identity land in one bucket, and retrieval is never
   narrower than the matcher.
3. **Resolution narrows on the FQN and widens on the leaf.** A candidate
   whose FQN equals the origin's resolved spelling is exact; when nothing
   indexed declares the FQN, the walk admits the leaf-keyed set and STAMPS
   the answer (`MethodResolution::CrossFile { widened: true }`). The stamp
   is provenance on the value, so every projection inherits it without a
   handler branch. The rule lives in the origin's scope: a pack whose
   class spellings go through a use-map is routed `NameKeyed`
   (`LanguageRegistry::pack_visibility`, read off its `names`), so its
   `ScopedLookup` carries the `UseMap` axis and `use_map_candidates`
   decides. A class target's goto-def (and so hover) lands on the
   stand-ins only after every exact lane missed, labeled `widened: X
   stands in for Y` (`widened_class_stand_ins`); references and rename
   stay exact, so a rename never rewrites a stranger.
4. **The loud half**: the `resolved-by-widening` diagnostics lane
   (`lsp::symbols::widening_diagnostics`, part of `pack_diagnostics`)
   fires on each class token whose identity nothing indexed declares and
   a same-leaf class answers for (`FileAnalysis::widened_class_references`):
   "nothing indexed declares `A\B`; resolved by its leaf to `X\B`". It
   sits on the reference that names the class — the parent clause, the
   `use` row's leaf — because that is where the fix is written. A member
   found through such a class carries the stamp, but no lane reads it at
   the member site: that would type every receiver on every publish.
5. **Spellings resolve once, in extraction.** A class declaration joins
   the namespace in force at its position — the section of the namespace
   lane there (`ExtractState::class_identity`) — so a class is filed
   under one key whatever container it is written in, and its symbol's
   `package` is that namespace. A written spelling resolves
   through the pure `UseMap::resolve` ladder
   (`model/file_analysis/use_map.rs`) over the file's `use` rows
   (`PackFacts::use_rows`) at the namespace it was written in, once every
   row and class is in: absolute spelling → itself; head segment → the
   nested class the enclosing class body declares (below), else its alias,
   else its `use` row (an aliased row binds the alias, never its leaf),
   else the namespace in force at the spelling's site; the rest hangs off
   that.
   A row is minted as the parts the extractor read (`@import.prefix` and
   the name's qualifier as its namespace, the name's last segment as its
   leaf, `@import.alias`)
   and the ladder answers parts too (`QualifiedName`), so the only strings
   it splits are written spellings; the joined FQN is formed only where a
   string key is needed (`QualifiedName::identity`).
   **Rows are lexically scoped.** Each row carries its `reach` — the
   narrower of the scope it was written in and its namespace section
   (`namespace A;` to the next one, `namespace A { … }` its body, the
   global `namespace { … }` too) — minted by the extractor
   (`imports::bind_reach`). A map is built for a site
   (`FileAnalysis::use_map_at`) and sees only the rows that reach it,
   with the namespace in force there (`FileAnalysis::namespace_ranges`,
   `namespace_at`) as its own; so every consumer passes a site — a
   parent its clause, a class token its ref, an invocant its point — and
   two sections of one file never read each other's rows. A language
   whose imports bind from the import onward narrows the reach's start;
   the consumers only ask `UseRow::binds_at`. How a row binds is the
   language's data (`NameSpellings::imports`), read by the extractor as
   it mints the row: PHP's reach their whole section and a leading
   separator is absolute; Python's bind from the import onward and climb
   from the file's package once per leading `.` past the first. A
   language that names modules by path gets its namespace from the
   driver (`FileModule`, gathered in phase 1 from the `__init__.py` chain,
   else the workspace root): one whole-file section, so `pkg/models.py`
   files `class User` as `pkg.models.User`. Parent clauses
   resolve this way (`scopes::resolve_parents`); every other class
   spelling the extractor mints goes through the same ladder — an
   annotation type through
   `InferredType::map_class_names`, so a qualified element inside
   `array<int, \App\User>` resolves like a bare one — with template
   parameter names and the current-class tokens exempt. The query side's
   `leaf_namespace_pins` and `class_spelling_identity` call the same
   ladder, so the pins, the identities and a written receiver's class
   cannot disagree. The capability is `ClassSpelling::UseMap` in
   `LangPack::names`, baked into `PackFacts::names`; a pack without it is
   untouched.
6. **The index registers the identity AND its leaf, in different
   tables.** `collect_linkage_feed` feeds the identity into the candidate
   table and the name slot, the exact key every lookup reads. The leaf of
   each use-map class identity (`widening_leaves`, the row store's
   `name_match_key`) goes into a relation of its own, `IndexCore::leaf_defs`,
   read only through `CrossFileLookup::leaf_def_candidates` by
   `use_map_candidates` once the exact set is empty. A leaf in the
   candidate table would answer every bare-name lookup and compete for
   the name slot, so a same-leaf stranger could win where the identity is
   declared; kept apart, it never competes.
   A `TargetRef`'s `class_ns` is the identity's own namespace
   (`identity_namespace`), the origin's pin only for a bare spelling.
7. **A class token references the target iff it names its identity.** The
   matcher resolves a `PackageRef` / construction-site spelling through the
   file's use-map (`spelled_identity`: a token inside an import row names
   the row's class in full) and compares identities — `use B\Collection; new Collection()`
   reaches `B\Collection` and never the same-leaf stranger. In a
   language where calling a class constructs it — language data,
   `NameSpellings::class_call`, which Python and C++ declare `Constructs`
   — a call-shaped token names a class too when its spelled identity is a
   class declared here or in an indexed file (`class_named_by_call`):
   python's `from pkg.models import User` and `User()` are references to
   `pkg.models.User`, and a token naming no class stays a call. PHP
   declares `FunctionCall`: construction is `new Foo`, so `Foo()` is a
   call of the function `Foo` whatever class shares its name, and is
   neither a reference to the class nor a value of it. The same property
   decides whether a file's own class is a free callee whose call yields
   an instance (C++'s functional cast `Foo(x)`); the reference half reads
   it only through a use-map, so C++, whose spellings are identities, keeps
   its calls calls.

**A class body can be a namespace.** A document that captures a class's
body as `@section.class` (Python's) makes it a section of the lane,
`SectionKind::ClassBody`, named by the class's identity: `class Outer:
class Inner:` in `pkg/mod.py` files `Inner` as `pkg.mod.Outer.Inner`, so
Django's per-model `class Meta:` is one class per model. The section
covers the body, not the header, so a parent clause and the class's own
name are written in the namespace around it. Its names are in force only
for code written directly in it, as in Python's class scope: a bare head
written there resolves to the nested class the body declares
(`UseMap::class_body`, asked through `DeclaredClasses` — the symbol table,
never a second list) before the ladder above, and otherwise falls through
to the section around it (`namespace_range_around`). A callable written in
the body opens a `SectionKind::Callable` section naming the namespace
around the class, so a method never reaches its class's names bare, and
a class nested one level further sees past its enclosing class to the
module. A dotted spelling hangs off its head as any qualified spelling
does: `Outer.Inner`, in the file or after `from pkg.mod import Outer`, is
`pkg.mod.Outer.Inner`. PHP and C++ capture no class section; their lanes
are unchanged.

The namespace sections are the namespace lane every language shares,
`FileAnalysis::namespace_ranges` — Perl's packages are written to it by the
same section-closing rule (`OpenSections`) and read by the same lookup
(`namespace_range_at`). It is neither a witness nor a binding: it types and
resolves nothing, it is the region map a site's resolution reads, the way
`control_regions` is. `namespace_at` reads it for every language; the
enclosing class (`package_at`) reads it only where the language says a
namespace IS the class (`NameSpellings::enclosing_class` —
`NamespaceInForce` for Perl, `ScopeChain` for PHP and Python, whose
classes are the bodies a namespace holds).

## Retired with it

The leaf-era plumbing is gone, not gated: `PackFacts::parent_namespaces`
(the parent edge carries the parent's identity), `TargetRef::class_ns` and
`CrossFileLookup::pinned_namespace` (the matcher compares identities, so a
stranger's spelling never matches), the import-rows-only gate, and the
leaf-space FQ family validation in `implementations_of`.

## Residuals

- **A Python module's name reads the filesystem, its blob does not.**
  The `__init__.py` chain and the workspace root decide `FileModule`,
  but the cache keys on the file's source: adding or removing an
  `__init__.py` re-files a module's classes only once the file is
  re-analyzed.
- **A Python class body is a section over its whole span after the
  header.** A method's parameter annotations and defaults are evaluated in
  the class scope, but the method's `Callable` section covers its whole
  definition, so they resolve past the class; an import written directly
  in a class body reaches its methods too. A class inside a method is
  filed under the module (`pkg.mod.Local`), not Python's
  `Outer.build.<locals>.Local`.
- **`Outer.Inner()` is a member call.** A call through an attribute is a
  `MethodCall` on the receiver `Outer`, and a nested class is no member
  of its enclosing class there, so the call is untyped and goto-def finds
  nothing; the class reference spellings (a parent clause, an annotation)
  resolve.
- **A dotted annotation mints no class reference.** `v: Outer.Inner`
  types `v` as `pkg.models.Outer.Inner`, but its tokens are a
  `FieldAccess` on `Outer`, not a `PackageRef`, so goto-def on `Inner`
  there finds nothing; the parent clause `class V(Outer.Inner)` is a
  `PackageRef` and lands on the declaration.

- **The visibility pins have no site.** `UseMapPins` is per ORIGIN
  (`VisibilityAxis::for_origin` takes a file, not a position), so it
  folds every section's rows into one leaf table: a leaf two sections
  import from different namespaces conflicts and pins to nothing (the
  full table, unranked), and a file of several namespaces claims no own
  namespace. An over-approximation, never a wrong pin; a class token's
  own resolution (`spelled_identity`) is per-site and exact.
  `qualified_spellings` carries no site either, so the pins resolve it
  over the whole-file view (`UseMap { site: None }`).

- **An anonymous class's identity joins the file namespace, not the
  enclosing class** (`T\class_anonymous_7_20` inside `T\Outer::make`),
  and its symbol's `package` is that namespace. The identity is unique by
  construction (row and column), so nothing collides; it only reads
  oddly in an outline.
- **A global-namespace class never widens.** Widening reads
  a QUALIFIED identity's leaf; a bare identity (a PHP class with no
  namespace, `new \User`) is its own leaf and never widens to `X\User`.
- **The leaf relation is a second candidate table.** One `leaf_defs`
  entry per use-map class, an `Arc` clone beside its `all_defs` one, and
  nothing in the edge indexes; `docs/scaling-limits.md` §7, unmeasured on
  a large corpus.
