# Ref Provenance: Residual Forward Work

> CLAUDE.md rules 7 (every meaningful token gets a ref) + 8 (provenance) are
> the principles. Phase 1 of the original ref-coverage doc — narrowest-span
> `ref_at`, fat-comma key emission for call args, `RenameKind` dispatch — is
> in. This doc is the residual: derivation chains where rename can find the
> derived ref but can't update the source.

Constant-fold provenance (`Ref.folded_from` — `my $m = 'process';
$self->$m()` rename rewrites the source string literal) and framework-attribute
unified rename (accessor ∪ constructor key ∪ internal hash key as one group)
landed: `docs/adr/field-projections.md`.

Inheritance override scoping (renaming `Animal::speak` surfacing
`Dog::speak` without touching an unrelated same-named sub) is landed:
`method_override_family` (`model/file_analysis/ancestry.rs`) is the
reverse-parent walk, and `OverrideScope::Hierarchy` (the
`rename.overrideScope` setting, `index/resolve/collect.rs`) is what
`rename_edits()` consults — see `docs/adr/destructuring.md`'s H1 record
for the scoping decision.

## What's still missing

### Import list rename verification

`use Foo qw(bar)` — the builder emits a `FunctionCall` ref for `bar` via
`emit_refs_for_strings`. When `sub bar` in `Foo` is renamed, `rename_edits`
should find this ref and update the import list. **May already work** —
needs a regression test, then either pin or fix.

### Package rename → file rename (stretch)

Renaming `MyApp::Controller::Users` should offer to rename
`lib/MyApp/Controller/Users.pm`. LSP's `WorkspaceEdit.documentChanges`
supports `RenameFile`. Compute expected path from package name; include in
edit if the file exists. Not implemented — no `RenameFile` support exists
in the codebase yet.
