# Unify Perl into the generic language path (kill the parallel paths)

Design debt, flagged during the C++ cross-file work. Today Perl and the
pack languages (C++/Python/R/…) run on **parallel paths** at several
layers. They were built that way because Perl came first (bespoke) and the
pack tier was added as a generic seam — but the duplication is a smell and
should converge on the generic path, with Perl as just another driver.

## The parallel paths today

| layer | Perl | pack languages |
|---|---|---|
| analyze | `PerlDriver` → `builder::build` (bespoke CST walker + witness bag + plugins) | query-driven `query_extract` driver + `.scm` packs |
| workspace index | `index_workspace_with_index` — by **package** name, into the hub `ModuleIndex` | `index_pack_languages` — by **class** name, into a per-language sub-index |
| cross-file store | the hub `ModuleIndex.cache` | `ModuleIndex.pack_indexes[lang]` (separate instance, own cache + future `modules-{lang}.db`) |
| completion | `cursor_context.rs` (Perl-specific) | `in_scope_completion` + sentinel member completion |
| member resolution | `complete_methods_for_class` (methods + synthesized `new`) | `complete_members_for_class` (methods + data fields) |

The `pack_indexes` field on `ModuleIndex`, and the `language != "perl"`
branches in `backend.rs` / `file_store.rs` / `main.rs`, are the visible
seams of this split.

## The target

One path: **Perl becomes a driver in the generic flow too.** Ideally
`PerlDriver` produces its `FileAnalysis` through the same `LanguageDriver`
seam, is indexed by the same generic indexer (its packages registered the
same way classes are), is completed through the same in-scope + member
machinery, and the cross-file layer is uniformly per-language (Perl's index
is just `pack_indexes["perl"]`, no special hub). Then the
`language != "perl"` branches disappear and there's a single mechanism.

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
2. Make the cross-file store uniformly per-language (`pack_indexes["perl"]`),
   retire the hub/`pack_indexes` asymmetry.
3. Collapse `complete_methods_for_class` / `complete_members_for_class` and
   the two completion entry points once Perl's member/cursor semantics are
   expressible generically (or via driver hooks).
