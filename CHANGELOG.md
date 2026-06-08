# Changelog

All notable changes to perl-lsp are documented here. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/); versions are the published
crate / VS Code extension versions.

## v0.4.0

The usability sprint — cross-file intelligence everywhere, framework awareness,
and a reproducible regression net. Robustness-swept clean across 2,293 real CPAN
modules (no crashes, panics, or hangs).

### Highlights

- **Cross-file navigation & type inference.** Go-to-definition, references,
  rename, hover, and completion now follow imports, inheritance, and
  re-exported surfaces across files — not just within the open buffer.
- **Framework intelligence.** Moo / Moose / Mojolicious (Mojo::Base) / DBIx::Class
  accessors, roles, and constructors are understood: `has`-generated accessors,
  `with`/`extends`/`use parent` inheritance, and typed method returns.
- **Inherited-method completion.** `$self->` offers methods inherited from parent
  classes (`use parent`, `use Mojo::Base -base`), resolved cross-file.
- **Re-export surfaces.** Goto-def through a module that re-exports another's
  `@EXPORT` (static splice and loop-push forms) lands on the original sub.

### Parser

- Adopted **ts-parser-perl 1.1.1**: more valid Perl parses cleanly — braced
  variable declarations (`my ${foo}`), sub/method forward declarations
  (`sub NAME;`), bare dotted versions (`use 5.14.0`), glued `x`-repetition,
  arrow-chained subscripts in interpolated strings, and correct hashref-vs-block
  disambiguation (`bless {@_}, $class`). Fixes the long-standing `s{}{}`
  external-scanner abort on large modules.

### Improvements & fixes

- Constructors via `bless`, including the `bless { %{ $_[0] } }, ref $_[0]`
  clone idiom, type to the right class.
- `goto &Foo::bar` and `&Foo::bar()` resolve to the target sub.
- Parameters declared as `my ($self, $name) = (shift, shift)` are recognized.
- Glob-installed subs (`*name = \&x`, `*name = $cond ? \&a : sub {…}`) register
  as definitions instead of being flagged unresolved.
- Document outline collapses each Moo/Mojo `has` accessor to a single entry.
- Forward declarations no longer duplicate the real definition.
- Diagnostics are quieter by design: unresolved-function/method are emitted at
  **hint** severity, since dynamic Perl (AUTOLOAD, runtime glob installs,
  uninstalled deps) is common and shouldn't flood the Problems panel.

### Editors

- **VS Code extension** auto-downloads the matching binary (config path → cached
  → `PATH` → GitHub Release), so install-and-go works with no separate setup.

### Known gaps

A handful of inference edge cases degrade gracefully (a plain symbol instead of
a typed one), tracked in
[`gold-corpus/KNOWN-GAPS.md`](gold-corpus/KNOWN-GAPS.md) — notably cross-file
receiver-polymorphic constructors (`bless {}, ref $self || $self` through
`SUPER::new`) and first-param-self over-reach for helpers/callbacks inside OO
classes.
