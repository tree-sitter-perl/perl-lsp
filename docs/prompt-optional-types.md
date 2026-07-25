# Optional / Maybe types

**Landed, including the production gaps.** Decision record:
`docs/adr/optional-types.md`.

Production covers: bare `return;` / `return undef` / `return ()` arms
(each an undef arm; `{T, undef} → Optional<T>`); all-undef subs typing
the definitive `Undef` (gated on the per-arm `value_arm` Fact count so an
untypeable value arm or a fallthrough tail blocks the verdict); slot
writes (`SlotTypeFold` strips undef-ness — a literal `undef` write or an
`Optional` RHS — into a flag, agrees the value arms, re-lifts); and both
`isa` spellings (quoted `'Maybe[T]'` and the bareword `Maybe[Int]` /
`Maybe[InstanceOf['X']]` constructor forms, via `type_optional` and the
0-arity base constants in `frameworks/type-tiny.rhai`).

### Consumption

Diagnostics on unguarded `Optional` / known-`Undef` derefs (landed):
`docs/adr/narrowing-diagnostics.md`.
