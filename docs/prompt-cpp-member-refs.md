# cpp member/macro access → the ref core already resolves

cpp member resolution routes through the SAME shape core resolves Perl
`$obj->method` with: a `RefKind::MethodCall { invocant, invocant_span,
method_name_span }`, typed query-time by `method_call_invocant_classes` →
`expr_type_at_span(invocant_span)`, dispatched by `resolve_method_in_ancestors`.
`find_definition` / `refs_to` / rename / hover all flow from that one ref, so
the old cursor-time parallel stack (`pack_member_at`, `member_def_site`, the
per-consumer ancestor walks) is gone and cpp `obj.method` / `(*p)->m` reuse the
core machinery. The resolution seam it feeds: `docs/adr/resolution-candidate-set.md`.

## Residual forward work — each a separate careful change

1. **Full `LangCfg`→`LangPack` fold.** The correctness (Python call kind) is
   already fixed via `call_kinds`/`simple_var_kinds`. Merging `member_kinds`
   is blocked on generalizing the cpp-grammar-coupled `member_access_sites`
   op-DX walk to python's `attribute` node + an explicit `operator_correctable`
   flag — else a naive merge risks a python op-DX regression. The cpp
   `recv_wrapper_kinds` (LangPack) / `wrapper_kinds` (LangCfg) overlap dedups
   in the same move.
2. **Layering-test LSP-layer teeth.** `backend.rs`/`symbols.rs` should name no
   `child_by_field_name`/`TreeCursor`/`descendant_for_*`/`std::fs::read*`
   (route through `cursor_sentinel`/`CrossFileLookup`), or the boundary erodes
   per language. Blocked on PRE-EXISTING Perl `descendant_for_point_range` in
   symbols.rs — strict teeth would force refactoring unrelated Perl first (else
   it's an allowlist).
3. **`==perl`→capability methods.** The `== "perl"` string branches want
   `LanguageDriver` capability methods (`cheap_synchronous_build()`,
   `has_preprocessor()`, `wants_enrichment()`). Per-branch design: some span LSP
   handlers, CLI modes, and caching and are fundamental, not capabilities; a
   blanket `is_pack()` is a half-measure.
4. **Macros (`OP_NULL`/`BASEOP`) as cross-file refs** → a macro usage that
   survives as an identifier should be a ref core resolves cross-file, deleting
   `pack_xfile_word_at` + its `#define`-line re-grep (rule-#10) + the symbols
   cross-file `fs::read` (route through `CrossFileLookup`), once def-ness is a
   modeled symbol property.
