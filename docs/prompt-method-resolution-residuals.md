# Method-resolution residuals (forward work)

The `unresolved-method` diagnostic — the always-on local form and its opt-in
cross-file extension (`unresolvedMethodCrossFile`) — doubles as a
**knowledge-gap finder**: every false positive names a method we failed to
synthesize or a receiver we failed to type. Auditing its output across a large
real codebase, the always-on false positives are closed (AUTOLOAD-in-MRO skip;
the narrowing-rebind truncation) and the `has \@const_array` synthesis gap is
closed. The remaining false positives cluster into four gaps below — most also
improve completion / hover / goto-def, not just the lint. Until they're closed,
the cross-file form stays opt-in.

## 1. `monkey_patch`-generated methods → bundled plugin — ✅ LANDED

`frameworks/monkey-patch.rhai` synthesizes the named methods from
`monkey_patch $class => $name => sub {…}` registrations (string class,
`__PACKAGE__`, multi-pair calls, and loop registrations via the
`string_values` fan-out). Dynamic class / unfolded names are honest
misses. Residual: the `Sub::Install` / `Class::Method::Modifiers` family
uses a hashref-options shape (`install_sub({ code, into, as })`) — it
rides the same plugin once `CallContext` classifies hashref args for
function calls.

## 2. `Sub::HandlesVia` `handles => { … }` delegations → bundled Moo plugin — ✅ LANDED

The Moo/Moose `handles` family (hashref `local => remote`, arrayref
`[qw/m/]`) already synthesized; the Sub::HandlesVia curried shape
(`handles => { local => [remote, @args] }`) now does too — the HashPairs
classification pair-walks via `cst::pair_nodes`, an arrayref value
contributes its first string element as the remote, and a non-string
value no longer shifts alignment of the pairs after it. The vocabulary
stays in `frameworks/moo.rhai` (rule #10), the CST walk in core (rule #1).

## 3. Opaque generated / XS classes → scoping + probe-gen

Fully generated API clients (OpenAPI/Swagger codegen), XS classes, and
Exporter-injected method sets have no statically visible method declarations at
all — there is nothing in source to read. Two complementary mitigations:

- **WORKSPACE-only scoping for the cross-file lint.** ✅ LANDED — the lint
  fires only for classes registered from the workspace tree
  (`ModuleIndex::is_workspace_module`); pure `@INC`/DEPENDENCY classes,
  where generated/XS methods you can't see are common, stay silent. A
  principled gate, not whack-a-mole.
- **Probe-based plugin generation.** Run the generator against a recording
  probe to capture the produced method surface and emit a plugin (see the
  generator-coderef probe direction).

## 4. Untyped-receiver value-flow → long-distance provenance

Methods called on a receiver whose type the walker can't pin: an exported
resolver sub (`Exporter::Shiny` and friends) whose first parameter is the live
framework object passed in by the caller; a record/row handed in as a
parameter; etc. The method exists on the runtime receiver, but the static type
is unknown. This rides the long-distance value-provenance tier
(`docs/prompt-type-inference-residual.md`) — typing the parameter from its call
sites — plus the receiver-gated dispatch seam for role bodies that call methods
the composing class (or a sibling role) provides.

---

These are also the gating list for promoting the cross-file `unresolved-method`
lint past opt-in: each closed gap removes a false-positive cluster and the lint
gets correspondingly safer to default on.
