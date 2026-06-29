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

Peel **where the node is still live** — at event construction. A pack marks a
declarator chain with one `@nested.target` capture; `peel_nested` (in
`query_extract.rs`) walks it to the leaf identifier and the per-level deref
stack, then emits the leaf as a **synthetic** `@flow.target`/`@def.local`
event carrying the same `match_id`. Downstream is unchanged: the `@type.annot`
join still fires, the symbol is created, goto-def/references/witnesses all
work — and arbitrary depth needs no enumerated patterns. The stack rides to
`Symbol.deref_stack` (serde-default, travels the cache, so cross-file hover
gets the stars too).

## Generic by construction

Core branches on no grammar name. The LangPack declares the rule:

- `nested_peel: &[(node_kind, DerefKind)]` — which kinds nest, and the deref
  each contributes (cpp: `pointer_declarator`→Pointer, `reference_declarator`
  →Reference);
- `nested_leaf` — the bottom kind (`identifier`);
- `nested_annot_kinds: &[node_kind]` — per-level annotation kinds
  (`type_qualifier` → `const`/`volatile`/`restrict`), collected onto each
  `DerefStep.annotations` as **free strings** (not typed flags) so new
  qualifiers and const-correctness diagnostics needn't reshape the type.

A future pack with its own chained-wrapper shape declares its own rule; no
core change.

## Stack shape

`Vec<DerefStep>`, outermost→leaf, which is also left-to-right display order
after the base (`Box*&` → `[Pointer, Reference]` → renders `Box*&`). Each step
carries `kind` + `annotations`. cv-qualifiers never affect deref depth or
navigation — display + diagnostics only.

## Consumers

- **hover** — `base + stack.render()` → `Box* const`, `Box****`.
- **member-access DX** *(next)* — `(stack, operator-typed)` → validate /
  auto-correct (`p.` on `[Pointer]` → offer `->`).
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
