# Flow-sensitive narrowing

**Landed.** Decision record: `docs/adr/flow-narrowing.md`.
Executable spec / playground: `test_files/narrowing_playground.pl`.

## Residual forward work

### Subject coverage

- **Accessor places** (`$self->name`) — parked. An accessor isn't a
  stable slot (it can return a different object per call, with side
  effects), so soundness needs a stricter no-call-between-guard-and-use
  model.

### Dynamic-key aliasing — soundness/precision knob

Dynamic-key places (`$self->{$k}`) currently take **Option A**: keyed by
spelling, the region truncates when a key scalar is reassigned, and a
write via a *different* key spelling that equals `$k` at runtime doesn't
truncate (a precision gap, never a crash — the existing constant-key
conservatism). **Option B** closes that aliasing hole: treat **any**
dynamic-key write to the container (`$self->{$j} = …`) as an *escape* that
re-widens every narrowed slot of that container — sound, but over-truncates
when the dynamic write hit a different key, and it applies to constant-key
places too. It is the general soundness-vs-precision knob for slot
narrowing; flipping to B is a localized change to `first_place_invalidation`
(truncate on *any* dynamic-key write to the container, not just the
matching spelling). Revisit if a motivating soundness case appears.

### Truthiness guards

- **`StripOptionalTruthy`** — a truthiness test on an optional subject
  narrows the region where it holds: `if (my $x = f())`,
  `return $self unless my $parent = $self->parent`, plain `if ($x)`.
  Common enough in real code that its absence is a visible share of the
  guard-lint noise, and cheap: the subject is already recognised, and
  the region machinery already exists.

  **It is NOT `StripOptional` with a different recognizer**, and the
  difference is the whole design. `defined`/`blessed` negate to
  `To(Undef)` — if the guard is false, the subject IS undef. Truthiness
  does not: `0` and `''` are false and perfectly defined, so the
  negative arm proves nothing and must emit nothing. A variant that
  reuses `StripOptional`'s `negated()` would claim `Undef` in the else
  branch and manufacture exactly the confident-wrong verdicts the lint
  exists to avoid.

  It must also stay OUT of the D3/D4 guard-site records: a truthiness
  test is not a definedness assertion, so a later `defined` check on the
  same subject is not redundant and a contradicting one is not
  contradictory.

  Not ported from anywhere — the description of an earlier attempt
  claimed it, but no commit in any branch ever contained it, so this is
  a design to write rather than a patch to recover.

  **Substrate evidence, and a coupling to `Unknown`.** `Unknown` is
  absorbing in return-arm agreement (`docs/adr/flow-narrowing.md`)
  because letting an undef arm wrap it as `Optional<Unknown>` minted
  `optional-deref` at every deref behind a guard of exactly this family:
  `my $enc = $self->encoding; return $value unless $enc; $enc->decode`
  (`Catalyst.pm`), `unless ($smtp) { $self->_throw(…) } … $smtp->message`
  (12 sites in `Email::Sender::Transport::SMTP`). The first shape is the
  exit form this item covers; the second needs "this call does not
  return" for a plain method, which nothing models. Once the exit form
  narrows, `Optional<Unknown>` becomes the more informative answer and
  the absorbing rule should be re-measured, not assumed.

### Negation

- **General `Not` / `Difference` negation** — parked: no positive lookup
  target, no consumer value. "Not Foo" has nothing to dispatch on.

Diagnostics the lattice enables (landed): `docs/adr/narrowing-diagnostics.md`.
