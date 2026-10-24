# Unify Perl into the generic language path (kill the parallel paths)

> Prerequisite: `docs/prompt-unify-semantic-tiers.md` (one witness
> engine, per-language emitters — emission library, neutral fold,
> enrichment policy). Land that first: every serving-path fork below
> hides behind "pack types work differently", and the semantic
> unification removes the excuse.

Design debt, flagged during the C++ cross-file work. Today Perl and the
pack languages (C++/Python/R/…) run on **parallel paths** at several
layers. They were built that way because Perl came first (bespoke) and the
pack tier was added as a generic seam — but the duplication is a smell and
should converge on the generic path, with Perl as just another driver.

## The parallel paths today

| layer | Perl | pack languages |
|---|---|---|
| analyze | `PerlDriver` → `builder::build` (bespoke CST walker + witness bag + plugins) | query-driven `query_extract` driver + `.scm` packs |
| workspace index | `index_workspace_with_index` — by **package** name | `index_pack_language` — by **class** name |
| cross-file store | `ModuleIndex.cache` (+ `@INC` tier, resolver thread) | `ModuleIndex.all_files` (own `modules-{lang}.db`, no resolver) |
| completion | `cursor_context.rs` (Perl-specific) | `in_scope_completion` + sentinel member completion |
| member resolution | `complete_methods_for_class` (methods + synthesized `new`) | `complete_members_for_class` (methods + data fields) |

A process serves one language through one `ModuleIndex`
(`docs/prompt-multi-language.md`), so the split is now in which parts of
that index a language fills, and in the `caps().hub_enrichment` branches
in the backend and CLI startup.

## The target

One path: **Perl becomes a driver in the generic flow too.** Ideally
`PerlDriver` produces its `FileAnalysis` through the same `LanguageDriver`
seam, is indexed by the same generic indexer (its packages registered the
same way classes are), is completed through the same in-scope + member
machinery, and the cross-file layer fills the index the same way for every
language. Then the `hub_enrichment` branches disappear and there's a single
mechanism.

## Why it's deferred, not done

The Perl builder is load-bearing and irreplaceable in the short term — the
witness-bag fold, the plugin system (emit/query hooks), framework
detection, enrichment. Folding it into the generic path is a large refactor
that must keep the Perl gold + e2e byte-identical throughout. So: converge
opportunistically (every new shared method takes `&dyn CrossFileLookup`,
not `&ModuleIndex`; every new query method is language-agnostic), and do
the full unification as its own deliberate project — not by bolting Perl
onto the pack path or vice versa.

## Concretely, next steps when this is picked up
1. Generalize the indexer: one `index_workspace` that routes every file
   (Perl included) through its driver + registers by the right key.
2. Make the index's registration uniform (Perl's packages land where a
   pack language's classes do), retiring the name-keyed/path-keyed split.
3. Collapse `complete_methods_for_class` / `complete_members_for_class` and
   the two completion entry points once Perl's member/cursor semantics are
   expressible generically (or via driver hooks).
