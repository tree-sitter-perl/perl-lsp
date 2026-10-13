# Pointer-stack capture (`@nested.target` → `Symbol.deref_stack`)

## Problem

C is pointer-heavy and the depth is unbounded (`Box`, `Box*`, `Box**`,
`Box*&`, `char****`). Type *resolution* deliberately drops pointer-ness — a
`Box* p` resolves as `ClassName(Box)` so `p->m` finds Box's members. But two
consumers need the real shape back:

- **hover** wants the exact written type (`pp: Box**`, not `pp: Box`);
- **member-access DX** wants the depth to know which operator the access
  requires (`pp->` should be `(*pp)->`).

The query can't enumerate unbounded nesting, and the extraction driver is
capture-**event**-based (it sees `(span, text, match_id)`, not live nodes), so
it can't peel the chain after the fact either.

## Decision

Peel **where the node is still live** — at event construction. A document
marks a declarator chain with one `@nested.target` capture, the root of the
`deref` link walk (`docs/adr/capture-vocabulary.md`). `Links::peel_declarator`
(`query_extract/extract/links.rs`) follows the walk to the leaf identifier,
collecting the per-level deref stack, then emits the leaf as a **synthetic**
`@flow.target`/`@def.<kind>` event carrying the same `match_id`. Downstream is
unchanged: the `@type.annot` join still fires, the symbol is created,
goto-def/references/witnesses all work, and arbitrary depth needs no
enumerated patterns. The stack rides to `Symbol.deref_stack` (serde-default,
travels the cache, so cross-file hover gets the stars too).

## Generic by construction

Core branches on no grammar name (rule #15). The document states each level
once and marks what it is:

- `@link.from.deref` + `@link.inner` — a level links to its operand;
- `@deref.pointer` / `@deref.ref` — the level contributes that deref (cpp:
  `pointer_declarator` / `reference_declarator`);
- `@deref.annot` — a per-level annotation among the level's children
  (`type_qualifier` → `const`/`volatile`/`restrict`), collected onto each
  `DerefStep.annotations` as **free strings** (not typed flags) so new
  qualifiers and const-correctness diagnostics needn't reshape the type;
- `@deref.leaf.<kind>` — the bottom, whose suffix names the def it mints
  (`@deref.leaf.field` → a member, `@deref.leaf.local` → a local).
- `@deref.pass` — a level the walk descends without a step of its own
  (cpp: the grouping `parenthesized_declarator` and a function pointer's
  `function_declarator`, so `int (*read)(char *)` is one pointer over the
  field `read`).

A tree the extractor never saw (the member-block reparse of a macro body) is
read the same way: `DeclaratorPeel::of_tree` runs the document over it and
peels through the same walk, so a field's stars come out the same however it
was written. A language with its own chained-wrapper shape spells the
captures in its own document; no core change.

## Stack shape

`Vec<DerefStep>`, outermost→leaf, which is also left-to-right display order
after the base (`Box*&` → `[Pointer, Reference]` → renders `Box*&`). Each step
carries `kind` + `annotations`. cv-qualifiers never affect deref depth or
navigation — display + diagnostics only.

## Consumers

- **hover** — `base + stack.render()` → `Box* const`, `Box****`.
- **member-access DX** — `expected_member_op(stack)` validates / auto-corrects
  the single-level case (`p.` on `[Pointer]` → offer `->`); a DEEP stack
  (`Box**`) falls to `deref_peel` for a show-only wrap hint (`(*pp)->`), since
  that fix is an expression wrap, not a token swap.
- **diagnostics** *(future)* — const-correctness reads `annotations`.

Resolution itself never consults the stack — it stays "drop to the leaf
class," so member completion is unchanged.

## Deferred

- **return types** — a function's pointer-returning shape (`Box* foo()`) is
  the *return type's* stack, a different axis than a variable's; not modelled
  here.
- **base-type cv** — `const Box* p` keeps `const` on the base type, captured
  at the declaration level, not in the pointer stack (which holds only
  per-pointer-level qualifiers).
