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

## What's still missing

### Import list rename verification

`use Foo qw(bar)` — the builder emits a `FunctionCall` ref for `bar` via
`emit_refs_for_strings`. When `sub bar` in `Foo` is renamed, `rename_sub`
should find this ref and update the import list. **May already work** —
needs a regression test, then either pin or fix.

### Package rename → file rename (stretch)

Renaming `MyApp::Controller::Users` should offer to rename
`lib/MyApp/Controller/Users.pm`. LSP's `WorkspaceEdit.documentChanges`
supports `RenameFile`. Compute expected path from package name; include in
edit if the file exists.

### Inheritance override scoping (stretch)

Renaming `Animal::speak` should surface `Dog::speak` (intentional API) and
NOT rename `unrelated::speak` (accidental name collision). The family is
landed: `OverrideScope::Hierarchy` (the rename default) gathers it over
`@ISA` edges, never by name. What is left is a regression test pinning
the unrelated same-named method as untouched.

## Test coverage to add

```rust
#[test]
fn test_constant_fold_rename_updates_source_string() {
    // my $m = 'process'; $self->$m()
    // Rename 'process' → updates sub def, $self->$m() call site, AND
    // the 'process' string literal.
}

#[test]
fn test_framework_attribute_unified_rename() {
    // package Foo; use Moo; has name => (is => 'ro');
    // Foo->new(name => 'x'); $foo->name; $self->{name}
    // Rename from any position → updates all four.
}

#[test]
fn test_import_list_renamed_with_sub() {
    // use Foo qw(bar); bar();
    // Rename sub bar in Foo → updates 'bar' in qw(), bar() call site,
    // and sub bar def.
}
```
