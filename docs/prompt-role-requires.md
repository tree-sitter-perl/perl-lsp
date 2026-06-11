# Role `requires` — the composer-mismatch diagnostic (design)

**Status: recorded, not built.** The in-role half LANDED: `requires
NAMES` synthesizes Method symbols (span = each name's atom) so
`$self->name` resolves inside the role — no unresolved-method hint,
goto-def lands on the contract, completion offers it — and the names
land in `FileAnalysis.role_requires` (per-package `requires` lists,
serde-default). This doc is the deferred half: telling a COMPOSER it
doesn't fulfill a contract.

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

## Calibration

Substrate sweep mandatory before shipping. Known noise sources to
calibrate against: dynamically-installed methods (`*name = sub` in
the composer — the codegen recognizers should already cover most),
`can()`-probed optional contracts (Clove::Sheets probes
`$self->can('process_row')` — NOT a requires, correctly out of
scope), and AUTOLOAD composers (suppress when the composer defines
AUTOLOAD, same rule the unresolved-method hint uses).

## crm finding that motivated this

`Clove::Sheets` calls `$self->fetch_raw` without `requires
'fetch_raw'` (it's provided by sub-roles like
`Clove::Sheets::Roles::Source::NamedCSV` that consumers compose) —
the in-role hint now correctly nudges the contract to be declared.
The composer-mismatch diagnostic is the other direction of the same
contract.
