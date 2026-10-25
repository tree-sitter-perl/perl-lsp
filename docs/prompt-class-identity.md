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
   whose FQN equals the origin's resolved spelling is exact; when the exact
   set declares nothing, the walk admits the leaf-keyed set and STAMPS the
   answer (`MethodResolution::CrossFile { widened: true }`). The stamp is
   provenance on the value, so every projection inherits it without a
   handler branch.
4. **The loud half**: a `resolved-by-widening` diagnostics lane reads the
   stamp and reports "member `m` found on `X\Y`, not on the class this
   file names (`A\B`)". Same silence rules as `unresolved-method` (the
   receiver is THE dispatch projection; an unknown receiver says nothing).
5. **Spellings resolve once, in extraction.** A class declaration joins
   the namespace in force at its position, the innermost
   `@context.namespace` (`ExtractState::class_identity`), so a class is
   filed under one key whatever container it is written in, and its
   symbol's `package` is that namespace. A written spelling resolves
   through the pure `UseMap::resolve` ladder
   (`model/file_analysis/use_map.rs`) over the file's `use` rows
   (`PackFacts::use_rows`) at the namespace it was written in, once every
   row is in: absolute spelling → itself; head segment → its alias, else
   its `use` row (an aliased row binds the alias, never its leaf), else
   the namespace in force at the spelling's site; the rest hangs off that.
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
   with the namespace in force there (`PackFacts::namespace_sections`,
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
6. **The index registers the identity AND its leaf.** `collect_linkage_feed`
   feeds a namespaced symbol under both keys: the identity is the exact
   key, the leaf the widening one (`ScopedLookup::use_map_candidates`).
   A `TargetRef`'s `class_ns` is the identity's own namespace
   (`identity_namespace`), the origin's pin only for a bare spelling.
7. **A class token references the target iff it names its identity.** The
   matcher resolves a `PackageRef` / construction-site spelling through the
   file's use-map (`spelled_identity`: a token inside an import row names
   the row's class in full) and compares identities — `use B\Collection; new Collection()`
   reaches `B\Collection` and never the same-leaf stranger. A
   call-shaped token names a class too when its spelled identity is a
   class declared here or in an indexed file (`class_named_by_call`):
   python's `from pkg.models import User` and `User()` are references to
   `pkg.models.User`, and a token naming no class stays a call.

`PackFacts::namespace_sections` is a field of its own (the rule #14
ratchet) because it is neither a witness nor a binding: it types and
resolves nothing, it is the region map a site's resolution reads, the way
`control_regions` is.

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
- **A nested Python class joins the module namespace** (`pkg.mod.Inner`
  for `class Outer: class Inner:`), not Python's qualname
  (`pkg.mod.Outer.Inner`) — the anonymous-class residual below, in
  another language.

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
- **The leaf key double-feeds the edge index.** The linkage feed registers
  a namespaced symbol under its identity AND its leaf, and
  `rebuild_name_registration` feeds the edge indexes once per key, so a
  php class's inheritance and bridge edges are recorded twice (once per
  spelling). Correctness is unaffected (buckets dedup members; walks
  dedup by node); the cost is in `docs/scaling-limits.md` §7, unmeasured
  on a large corpus.
