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
   refs carrying the FQN as `target_name` when the origin can name it. The
   index registers the FQN; C's flat linkage is its own identity (the hook
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
5. **Spellings resolve once, in extraction.** The extractor resolves every
   class spelling AS IT MINTS it, through one `ident` resolver built from
   the pre-collected `use` rows, aliases and the namespace in force at the
   spelling's position: the pure `UseMap::resolve` ladder
   (`model/file_analysis/use_map.rs`) — absolute spelling → itself; head
   segment → its alias, else its `use` row (an aliased row binds the alias,
   never its leaf), else the file's own namespace; the rest hangs off that.
   A declaration joins its namespace directly (`decl_ident`); a template
   parameter name and the current-class tokens are exempt. Every type the
   pack's annotation predicate returns passes through
   `InferredType::map_class_names`, so a qualified element inside
   `array<int, \App\User>` resolves like a bare one. The query side's
   `leaf_namespace_pins` and `class_spelling_identity` call the same
   ladder, so the pins, the identities and a written receiver's class
   cannot disagree. The capability is `LangPack::namespace_sep`, baked
   into `PackFacts::namespace_sep`; a pack without one is untouched.
6. **The index registers the identity AND its leaf.** `collect_linkage_feed`
   feeds a namespaced symbol under both keys: the identity is the exact
   key, the leaf the widening one (`ScopedLookup::use_map_candidates`).
   A `TargetRef`'s `class_ns` is the identity's own namespace
   (`identity_namespace`), the origin's pin only for a bare spelling.
7. **A class token references the target iff it names its identity.** The
   matcher resolves a `PackageRef` / construction-site spelling through the
   file's use-map (a token inside an import row names the row's class in
   full) and compares identities — `use B\Collection; new Collection()`
   reaches `B\Collection` and never the same-leaf stranger.

## Retired with it

The leaf-era plumbing is gone, not gated: `PackFacts::parent_namespaces`
(the parent edge carries the parent's identity), `TargetRef::class_ns` and
`CrossFileLookup::pinned_namespace` (the matcher compares identities, so a
stranger's spelling never matches), the import-rows-only gate, and the
leaf-space FQ family validation in `implementations_of`.

## Residuals

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
