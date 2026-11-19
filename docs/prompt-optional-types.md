# Optional / Maybe types

**Landed.** Decision record: `docs/adr/optional-types.md`.

## Residual forward work

### Production gaps

- **`FieldValueReducer` production** — `{T, undef}` slot writes (`$self->{x} =
  $maybe`) → `Optional` on the slot.

### Consumption

Diagnostics on unguarded `Optional` / known-`Undef` derefs (landed):
`docs/adr/narrowing-diagnostics.md`.
