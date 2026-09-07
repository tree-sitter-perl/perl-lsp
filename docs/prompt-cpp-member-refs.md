# cpp member/macro access → the ref core already resolves

cpp member resolution routes through the SAME shape core resolves Perl
`$obj->method` with: a `RefKind::MethodCall { invocant, invocant_span,
method_name_span }`, typed query-time by `method_call_invocant_class` →
`expr_type_at_span(invocant_span)`, dispatched by `resolve_method_in_ancestors`.
`find_definition` / `refs_to` / rename / hover all flow from that one ref, so
the old cursor-time parallel stack (`pack_member_at`, `member_def_site`, the
per-consumer ancestor walks) is gone and cpp `obj.method` / `(*p)->m` reuse the
core machinery. The resolution seam it feeds: `docs/adr/resolution-candidate-set.md`.

## Residual forward work — each a separate careful change

The `LangCfg`→`LangPack` fold is landed (one config, one lookup;
`member_kinds` and the `recv_wrapper_kinds`/`wrapper_kinds` overlap are
gone with it). What remains:

1. **Layering-test LSP-layer teeth.** `backend.rs`/`symbols.rs` should name no
   `child_by_field_name`/`TreeCursor`/`descendant_for_*`/`std::fs::read*`
   (route through `cursor_sentinel`/`CrossFileLookup`), or the boundary erodes
   per language. Blocked on PRE-EXISTING Perl `descendant_for_point_range` in
   symbols.rs — strict teeth would force refactoring unrelated Perl first (else
   it's an allowlist).
2. **`==perl`→capability methods.** `has_preprocessor_macros()` is the first
   `LanguageRegistry` capability method; three raw `== "perl"` string branches
   remain (`backend/indexing.rs` ×2, `builder/pattern_dispatch.rs`). Per-branch
   design: some span LSP handlers, CLI modes, and caching and are fundamental,
   not capabilities; a blanket `is_pack()` is a half-measure.
3. **Macros (`OP_NULL`/`BASEOP`) as cross-file refs** → a macro usage that
   survives as an identifier should be a ref core resolves cross-file, deleting
   `pack_xfile_word_at` + its `#define`-line re-grep (rule-#10) + the symbols
   cross-file `fs::read` (route through `CrossFileLookup`), once def-ness is a
   modeled symbol property.
