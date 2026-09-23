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

Peel **where the node is still live**. A document marks a declarator chain
with one `@nested.target` capture; `DerefCaps::peel`
(`query_extract/extract.rs`) walks it to the leaf identifier and the per-level
deref stack, then emits the leaf as a **synthetic** `@flow.target`/`@def.local`
event carrying the same `match_id`. Downstream is unchanged: the `@type.annot`
join still fires, the symbol is created, goto-def/references/witnesses all
work — and arbitrary depth needs no enumerated patterns. The stack rides to
`Symbol.deref_stack` (serde-default, travels the cache, so cross-file hover
gets the stars too).

## Generic by construction

Core branches on no grammar name (rule #15). The document says what a
declarator level IS, one capture per level:

- `@deref.pointer` / `@deref.ref` — this node is a level, contributing that
  deref (cpp: `pointer_declarator` / `reference_declarator`);
- `@deref.annot` — a per-level annotation (`type_qualifier` →
  `const`/`volatile`/`restrict`), collected onto each `DerefStep.annotations`
  as **free strings** (not typed flags) so new qualifiers and
  const-correctness diagnostics needn't reshape the type;
- `@deref.leaf.<kind>` — the bottom, whose suffix names the def the synthetic
  leaf event mints (`@deref.leaf.field` → `def.field`, so a pointer member
  outlines as a member).

`DerefCaps` is the one home for those captures: the extraction pass fills it
as it flattens matches, and a consumer holding a tree the extractor never saw
(the member-block reparse of a macro body) fills it by running the same
document over that tree. One walk, one vocabulary. A language with its own
chained-wrapper shape spells the captures in its own document; no core change.

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
