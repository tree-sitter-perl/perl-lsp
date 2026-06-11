# Role `requires` — the composer-mismatch diagnostic

**Status: LANDED (both halves).** The in-role half: `requires NAMES`
synthesizes Method symbols (span = each name's atom) so `$self->name`
resolves inside the role — no unresolved-method hint, goto-def lands
on the contract, completion offers it — and the names land in
`FileAnalysis.role_requires`. The composer half below shipped as
`role-requires-unfulfilled` (`FileAnalysis::unfulfilled_role_requires`,
emitted in `collect_diagnostics`). Load-bearing decisions from the
build, found by calibration:

- **Markers are identified by SymbolId** (`FileAnalysis.
  contract_symbols`), never by name. A role that both requires AND
  defines a name (`Clove::Sheets`'s default-implementation pattern)
  provides it — the name-keyed check ate the real def beside the
  marker, and the typeglob-install provision arm conversely saw the
  marker itself through the names index ("every requires satisfies
  itself"). Both directions only stay correct symbol-keyed.
- **Unfoldable parent edges are recorded** (`dynamic_parent_packages`
  via `cst::string_list_with_residue`): `with ReportProxy(type =>
  ...)` generates a role at runtime, so the recorded parent list is
  not the whole ancestry. The fact feeds
  `class_has_unresolved_ancestor` — the single incompleteness seam —
  which keeps this diagnostic AND the unresolved-method hint
  honest-silent on such classes.
- **`is_role_package` derives from `package_uses`** (Moo::Role /
  Moose::Role / Mouse::Role / Role::Tiny; `Role::Tiny::With` is
  deliberately absent — it grants `with` to plain classes).

## The diagnostic

For each package P with role parents (the `with` edges already in
`package_parents`), for each composed role R (cross-file via the
index), for each name in R's `role_requires`: does P provide it?
Missing → `role-requires-unfulfilled` on P's `with 'R'` ref span:
"role R requires 'name'; P does not provide it". Perl dies at
composition time for this, so WARNING severity is honest (not the
quiet-by-design HINT tier).

"Provides" must mean what Moo means:

- a local `sub`/`method` in P;
- an inherited method (full ancestor walk — `parents_of` is the seam);
- a `has`-synthesized accessor (incl. plugin-enrolled projections);
- a method provided by ANOTHER role in the same composition — roles
  satisfy each other's requires at compose time;
- NOT satisfied by an `around`/`before`/`after` modifier alone
  (modifiers wrap, they don't provide).

## Role-composing-role

A role that composes a role inherits its unfulfilled requires:
`role_requires(R) ∪ role_requires(R')` minus what R itself provides.
This is a query-time walk with a seen-set, NOT baked at build —
mirrors the inheritance edge-walk discipline (depth stays a
query-time property).

## Where it runs

`collect_diagnostics`, behind the module index (same enrichment
parity as the rest: batch/--check get it via the enriched-copy pass).
The check is ancestry-shaped, so it composes with the `ReceiverGated`
applicability machinery if gating is ever needed; start without it.

## Calibration (done)

Substrate: 0 hits across 2,293 modules. crm: 0 false positives after
the two fixes above (the raw first sweep produced 84, all from the
default-implementation and runtime-generated-parent patterns), plus
41 pre-existing false unresolved-method hints retired by the
dynamic-parents seam. `can()`-probed optional contracts
(`$self->can('process_row')`) are NOT requires and stay out of scope.

## crm finding that motivated this

`Clove::Sheets` calls `$self->fetch_raw` without `requires
'fetch_raw'` (it's provided by sub-roles like
`Clove::Sheets::Roles::Source::NamedCSV` that consumers compose) —
the in-role hint now correctly nudges the contract to be declared.
The composer-mismatch diagnostic is the other direction of the same
contract.
