# ADR: Use-after-move — the decidable, zero-false-positive subset

`std::move(x)` leaves `x` in a valid-but-unspecified moved-from state; a
subsequent READ of `x` before it is reassigned is a bug (clang-tidy's
`bugprone-use-after-move`). The machinery to record moves and the FlowEdge
rebind-cutoff already existed (`moved_from`, `RebindIndex` — the same
cutoff the narrowing tier uses). The naive check on top of it emitted ~17 false
positives over baseline library headers (spdlog/fmt/onednn) and was parked
dark.

A full check is path-sensitive (clang-tidy runs a CFG). We don't have a CFG.
The design question is therefore not "how do we match clang-tidy" but "what is
the largest subset we can flag with ZERO false positives" — because for a
diagnostic, a false positive is far worse than a false negative (it trains the
user to ignore the channel). When unsure, stay silent.

## What the FPs actually were

Categorized over the real headers (not hypothesized): every FP fell into one
of four classes, all downstream of the move being attributed to too broad a
region or the moved value not fully escaping:

1. **Member-initializer / delegating-ctor moves** — `: field(std::move(other))`
   sits OUTSIDE the ctor body `{}`, so the move lands in the *class* scope and
   floods every same-named param and member. The dominant class.
2. **Partial / subobject moves** — a move-`operator=` does
   `*static_cast<Base*>(this) = std::move(other); msg_type = other.msg_type;`.
   `other` is moved *as a base subobject*; sibling-member reads are safe. The
   read `other.msg_type` is a member projection, not a whole-object use — but a
   field access and a method call are indistinguishable at the ref level in the
   C++ pack (both mint a `MethodCall` ref on the receiver).
3. **Conditional moves with no bounding scope** — braceless `if`, ternary, and
   switch-`case` moves. Braced if/else arms are their own `@scope` (already
   handled); these constructs are not, so the move leaks to the function scope
   and a post-construct read false-flags.
4. **Loop-carried moves** — a move in a loop body; the back-edge makes
   move-vs-read ordering path-sensitive.

## The three gates (`FileAnalysis::use_after_move_reads`)

Each gate is a silence rule verified to remove a class of real-header FPs
without breaking the true-positive tests. All three are `&str`/span reads over
data the builder already records — no tree, no CFG.

- **Gate B — in a function body.** The move's scope chain must contain a
  `Sub`/`Method` scope. A member-init-list move lands in a class/namespace
  `Block` scope with no function ancestor → not flagged. Kills class 1.

- **Gate C — straight-line.** The builder records every control construct
  (`if`/`while`/`for`/`switch`/`do`/ternary/`preproc_if…`) as a span in
  `control_regions`. A move is straight-line iff NO control region is BOTH
  inside the move's enclosing scope AND contains the move. Braced arms are
  their own scope, so their `if_statement` region starts *before* the arm and
  is not `contains`ed by it — same-arm reads stay flaggable; only the non-scope
  constructs (braceless arm, loop/switch body, ternary, preproc) gate. Kills
  classes 3 and 4.

- **Gate E — locals only.** The builder records `parameter_list` spans in
  `param_regions`. A move whose variable's declaration lands in a param region
  is a parameter — overwhelmingly a forwarding / subobject-move idiom
  (move-ctors, `operator=`, perfect-forwarding wrappers) whose sibling-member
  reads this tier can't tell from a bug. Only moves of LOCALS are flagged.
  Kills class 2.

Result over spdlog/fmt/onednn: 17 → 0 FP, with the canonical true positives
(a straight-line local moved then `x.use()`d, cleared by reassignment or a
reset method) still flagged.

## Moves of places

A move's subject is a `Place` (`model/file_analysis/place.rs`): `x`, or a
member of a local (`std::move(p.msg)`), rooted at the local's declaration
when the skeleton assembles (`query_extract/places.rs`). A bare member name
in a member function (`std::move(name_)`) is rooted at the body's elided
receiver (`Binding::Receiver`, the method), which nothing in the body
rebinds. The assembly also mints the move's own accesses on its `MoveSite`:
the reads that read into the moved part (`p.msg`, `p.msg.size()`; not
`p.id`, and not `p` handed on whole, which reads none of the moved state
itself — the false positive is the costlier error here) and the
place writes that give it a value again (`p.msg = …`, `p.msg.clear()`). So
the check matches reads by declaration and path, never by spelling, and a
sibling member of a moved member is never flagged. Gate E still applies to
a part of a parameter: the base-subobject move of class 2 moves `other`
whole, and which part it feeds is decided by the callee's parameter type,
which no capture states.

## Why gates, not a smarter analysis

Classes 2–4 are exactly the cases that need path-sensitivity + subobject +
interprocedural reasoning. Rather than approximate those (and re-introduce
FPs), each gate encodes a property of the move/read SITE — "is it in a
function body", "is it straight-line", "is it a local" — and asks that
property, never the shape of a particular idiom (rule #10). A move that can't
answer "yes" to all three is not flagged. That keeps the check honest and the
residual a clean, documented false-negative set rather than a noisy
false-positive one.

## Wiring

Opt-in, off by default (`DiagnosticOptions.use_after_move`, from
`initializationOptions.diagnostics.useAfterMove` or CLI `--use-after-move`) —
it is a heuristic-adjacent lane, and the always-on pack channel stays the
member-access operator check. `pack_diagnostics` extends its output with
`pack_use_after_move_diagnostics` only when the toggle is set.
`control_regions` / `param_regions` ride the FileAnalysis cache blob
(`#[serde(default)]`); the `EXTRACT_VERSION` bump invalidates stale blobs.
