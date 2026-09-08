# Class identity is the fully-qualified name — for every language

## The problem

Perl mints class identity as the FQN: `ClassName("Foo::Bar")`, a method
symbol's `package` is `Foo::Bar`, `PackageSymbol{package: "Foo::Bar"}`.
The use-map packs (PHP) mint the LEAF: `ClassName("Collection")`, method
`package = "Collection"`, `PackageSymbol{package: "Collection"}`, and the
namespace rides beside it on the class symbol alone. Every consumer that
needs the namespace back re-derives it from the origin file's use-map pins
(`VisibilityAxis::UseMap`, `TargetRef::class_ns`, the matcher's
`pinned_namespace` gate). That is a lossy projection minted where the
information was in hand: the extractor SAW `namespace App\Support;` and
`use Illuminate\Support\Collection as BaseCollection;` and threw the
qualifier away.

The cost is a silent wrong answer. `resolve_member_in_ancestors` walks
`visible_def_candidates(leaf)` and takes the first class declaring the
member; when the pinned identity declares nothing and a same-leaf stranger
does, the answer lands on the stranger. Hover, goto-def and the
`unresolved-method` lane all read Some/None and say nothing. Real-world
code is dirty by design (a class edited half-way, an import not yet
written), so the over-approximation is WANTED — but it has to be loud.

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
5. **Spellings resolve once, in extraction.** A post-walk pass over the
   skeleton resolves every class spelling through the pack's
   `class_identity(written, ctx)` hook, the pure `UseMap::resolve` ladder
   (`model/file_analysis/use_map.rs`): absolute spelling → itself; head
   segment → its alias, else its `use` row, else the file's own namespace;
   the rest hangs off that. `leaf_namespace_pins` calls the same resolver,
   so the pins and the identities cannot disagree.

## Landing across the stacked branches

Additive per layer, so each branch stays reviewable and the last stage
flips the behaviour:

- **model** (`claude/split-1-model`): `UseMap` resolver; `split_qualified`
  learns the separator (`REF_ROWS_VERSION` bump — the key function
  changed for qualified spellings); `MethodResolution::CrossFile.widened`
  (constructed `false` everywhere; `widened()` accessor); the qualified-
  spelling pin arm routes through the resolver.
- **build** (`split-2-build`): the extraction post-pass and the
  `LangPack::class_identity` hook (identity by default).
- **index** (`split-3-index`): the ancestry walk narrows on FQN, widens on
  leaf, stamps `widened`; `class_ns` and the stranger gate become no-ops
  by construction and are retired after merge.
- **lsp** (`split-4-lsp`): the `resolved-by-widening` lane; hover and
  type-at render the FQN; completion label = leaf, detail = FQN.
- **php** (`split-5-php`): the php hook (`\`); `ClassName("Leaf")`
  assertions re-keyed to FQNs; gold rows re-verified.
- **frameworks / laravel**: overlays re-verified against the FQN key.

## What does NOT change

`VisibilityAxis::UseMap` and the pins stay: they answer visibility (what
a bare spelling MEANS here), which is the origin's question; identity is
the target's. The `visible` widening for aliased imports stays a rank on
the candidate table, never a filter.
