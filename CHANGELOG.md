# Changelog

All notable changes to perl-lsp are documented here. Format loosely follows
[Keep a Changelog](https://keepachangelog.com/); versions are the published
crate / VS Code extension versions.

## Unreleased

A large accumulation: a full second language (C/C++ in beta, plus alpha-tier
Python/R/CMake packs) now serves alongside Perl, cross-file analysis persists
to disk with bounded memory, and a long tail of hangs, crashes, and
stale/partial answers closed — surfaced by dogfooding against real corpora
(curl, redis, re2, Bugzilla, Mojolicious, DBIx::Class, abseil, a 138k-file
synthetic monorepo).

### C/C++ support — beta (opt-in build feature `cpp`)

perl-lsp now serves C/C++ alongside Perl from one server. Default builds
stay Perl-only; `cargo build --release --features cpp` turns the rest on.
The same query-driven extraction seam hosts alpha-tier Python, R, and CMake
packs. `perl-lsp --languages` reports each language's maturity tier
(Stable / beta / alpha), so a lightly-tested pack isn't mistaken for Perl's
coverage.

**Beta means the answers are covered, not the scale.** Small and mid-size
projects work well. Very large ones don't finish indexing yet — a few huge
vendored headers dominate, and excluding `thirdparty/` avoids most of it.
See `docs/cpp-status.md`.

- **Navigation.** Goto-definition, references (cross-translation-unit),
  hover, outline, semantic tokens, and `#include` navigation. Goto-def
  reaches the actual out-of-line definition — not just the header
  prototype — for both free functions and class members.
- **Templates.** Lazy instantiation typing and a partial-specialization
  selection ladder; overload resolution by argument count.
- **Macros.** Function-like macros type from their argument, `#ifdef`-guarded
  config variants are modeled, and goto-def prefers a `#define` over a
  same-named symbol; include-guard `#define`s no longer pollute
  outline/workspace-symbol.
- **Type narrowing.** `dynamic_cast` guards and `std::optional`
  engaged-state checks narrow, the same way Perl's `defined`/`blessed`
  guards do.
- **New diagnostic: use-after-move** — a decidable, zero-false-positive
  subset of the check.
- **Completion.** Member completion is inheritance-aware; qualified-path
  completion (`ns::`, `Class::`) filters by owner; member-receiver chains
  (`this->`, `field_->`) narrow correctly.

### PHP support — alpha (opt-in build feature `php`)

A PHP language pack joins the alpha tier (`--features php`, included in
`all-langs`), dogfooded in its first round against WordPress core,
laravel/framework, monolog, and guzzle — zero crashes, zero parse
failures across PHP 5-era legacy through 8.x syntax (enums, attributes,
promoted constructor properties, first-class callables, HTML-interleaved
templates). Market case and build-out plan in `docs/prompt-php-target.md`.

- **Navigation.** Goto-definition, references, hover, outline, and
  workspace-symbol — cross-file, through namespaces, `use` imports,
  `extends`/`implements`/trait composition, static calls, and `$this`.
  Measured on WordPress core: references on `esc_attr` finds all ~1300
  call sites across 267 files in ~1.5s warm.
- **Typing.** Declared types (params, properties, returns) drive
  hover/completion; `new ClassName()` types the variable even when the
  class lives in another file; `: static`/`$this` returns chain fluent
  builders; `instanceof` guards narrow; PHP's `.`/`.=` concat operators
  type untyped variables from use, Perl-style. Types display in PHP's
  own vocabulary (`string`, `array`, `int|float`).
- **Completion.** Member completion after `->` is receiver-typed and
  inheritance/trait-aware, labeled with provenance
  (`touch — Post (from HasTimestamps) → string`).
- **Duplicate declarations** (WordPress's `noop.php` stubs) answer the
  full ranked definition family — the real signature first by arity fit —
  instead of one confidently-wrong winner.

### Storage engine — warm starts, bounded memory

The on-disk cache (`~/.cache/perl-lsp`) now covers your whole workspace, not
just installed modules:

- **Warm starts.** Unchanged files are never re-parsed at startup, and each
  language's index loads on demand, so opening a C++ file doesn't wait on
  the rest of the tree.
- **Bounded memory.** Analyses live on disk and are pulled in as needed
  rather than all held in RAM, so a big workspace costs a fraction of what
  it used to and stays steady while you work in it.
- **References and rename** resolve through the index rather than sweeping
  the whole workspace.
- **Edits propagate.** Saving a file refreshes the open files that depend on
  it, and an edit that changes nothing other files can see no longer
  triggers a re-analysis of them.
- `perl-lsp --clear-cache [<root>]` wipes a project's cache (or all of it);
  `--gc-cache <root>` reclaims space from deleted files.

### Query-declared plugins

Framework plugins (`.rhai`) declare the syntax they care about as
tree-sitter query patterns with `on_match` handlers, and can publish their
own diagnostics. Editing a plugin invalidates the affected cache.
`--plugin-check` validates a bundle; older hook styles are rejected with a
porting hint.

### Usage heatmap

- `--heatmap` now covers C/C++ and the other pack languages, not just Perl.
- New dead-exports queue: exported subs no other file references.

### Cross-file type inference

Closed files now answer type queries with their imports applied
(previously only open documents did), and that enrichment is transitive
across module chains (A → B → C). Inheritance-aware method resolution,
receiver-gated dispatch, and structural hash-shape typing all ride the
same witness engine.

- **A Perl package is a set of files, not one "winner."** Perl lets one
  package be declared or reopened across multiple files; goto-def/hover/
  completion on a symbol living in a non-"winning" file used to silently
  answer from the wrong file (or nothing), and which file won was racy
  across cold starts. Package lookups now resolve to the full set.
- **Per-asker module resolution.** A module name can mean different files
  to different files asking about it (Perl's `@INC`/`use lib` search-path
  semantics) — goto-def/references/hover now resolve it relative to the
  asking file's own search path (longest-prefix ranking), not one global
  answer for everyone.
- Mojo helpers whose names come from a structural enumeration (a `for`
  loop over a list of names) now resolve, matching the hardcoded-list case.

### Reliability — hangs and crashes closed

- **One expensive file can no longer freeze the server.** Its analysis used
  to block hover, goto-def, and completion for every other file too.
- **`references` stays bounded on huge workspaces** — it returns instead of
  running away, and warns you when the answer had to be capped.
- A handler panic degrades one answer instead of crashing the server. POD
  containing non-English text could take out a whole file's analysis; fixed.
- Fixed several ways the process could hang instead of starting or exiting:
  client attach, teardown on EOF, and a malformed CLI flag.
- Diagnostics on a just-opened file no longer report false positives while
  the workspace is still loading.
- **A deeply nested file no longer takes the server down.** Generated files
  with thousands of nesting levels could overflow the stack — an abort
  nothing could catch. They get analyzed now; anything past a very high
  ceiling degrades with a `cst-too-deep` diagnostic instead of dying.

### Answer honesty

References, rename, and outline could return a quietly incomplete answer
while the workspace was still loading. They now wait for the full index,
and show "Waiting for workspace index…" if that wait is long enough to
notice.

### Performance & responsiveness

- Cross-file return-type resolution no longer blows up exponentially on
  deep call chains.
- `references` on a warm workspace returns in milliseconds.
- Repeat CLI runs start warm — module resolution comes from cache.
- The `--check` diagnostics sweep runs in parallel.
- A request that arrives while a file is still doing its first analysis now
  tells the client to retry, instead of returning an empty answer it might
  cache as the real one.
- A warm server sitting idle stays idle — it no longer burns CPU in the
  background re-checking dependencies nothing asked about.
- Startup indexing scales linearly with file count, and only indexes the
  languages a query can actually consult.

### Builds are reproducible

Building the same file twice could produce different cached output,
churning cache writes for files that never changed. Fixed.

### Four new LSP verbs

- **Go to type definition** — `$obj` jumps to the definition of its inferred
  class. When nothing infers, it answers nothing rather than guessing.
  CLI: `--type-definition`.
- **Type hierarchy** — supertypes and subtypes, one inheritance level per
  request. CLI: `--type-hierarchy`.
- **Call hierarchy** — incoming and outgoing calls, each resolved to its
  definition. CLI: `--call-hierarchy`.
- **Document links** — POD `L<...>` links, URLs in comments and POD, and
  file paths that actually exist (`require "path.pl"`, `use lib 'lib'`).
  Module names stay goto-def's job. CLI: `--document-link`.

### Completion

- Bare-cursor variable completion no longer drops the sigil (`self`
  instead of `$self`).
- Completion ordering is stable run-to-run.
- An empty result while the server is still warming up no longer gets cached
  by the client as the real answer.
- In-scope local variables could rank below unrelated workspace symbols;
  fixed.

### CLI

- **`--at <line>:<col>`** takes editor-native cursor input, and every verb's
  output now matches its input convention — no more 0-based/1-based mixups.
- **`--languages`** lists every supported language with its maturity tier.
- The workspace-index startup spinner now reports incremental percentage
  instead of sitting at an opaque "indexing…".

### Correctness — goto-def / references / hover / rename agreement

A long tail of cases where these verbs disagreed with each other on the
same target, found by dogfooding against real corpora (curl, redis, re2,
Bugzilla, Mojolicious, DBIx::Class):

- Qualified calls (`CORE::stat()`, `Other::Pkg::stat()`) no longer resolve
  to an unrelated same-named local sub.
- `map { BLOCK }` (not just `map EXPR, LIST`) folds for role-list goto-def;
  bareword `require Foo::Bar` is navigable.
- Rename no longer over-fans a synthesized DBIC column / Moo accessor
  rename into unrelated sibling classes.
- `--implementations` surfaces sibling/mixin overrides assembled via
  multiple inheritance, not just direct descendants; C/C++ base-class
  edges feed it too.
- References agrees with `--implementations` on a sibling-role target it
  previously missed; references no longer misses a package's own
  qualified declaration.
- Hover, goto-def, and references agreed to disagree on a handful of
  targets — an inherited hash key, a template method defined only in a
  subclass, a module name, the qualifier segment of `SUPER::name`/
  `Foo::Bar::name` — all now resolve the same way everywhere.
- Builtin calls (`shift`, `uc`, `time`, …) mint their own reference bound
  to a `CORE` namespace, fixing goto-def on `shift->SUPER::new`'s
  invocant position.
- Several DBIC / Mojolicious / receiver-polymorphic-constructor
  type-inference gaps closed.

### Non-ASCII files

The server now negotiates UTF-8 position encoding with clients that
support it (LSP 3.17) — previously every position on a line with a
non-ASCII character before the cursor was silently misaligned.

## v0.6.1 - 2026-07-20

### No linked-editing

- linked editing is no longer a capability advertised by the server; turns out the DX is
  bad since the feature is far too agressive when on by default (prevents having variables
  with shared prefixes). 

## v0.6.0 — 2026-07-05

### Type narrowing

The flow-sensitive type narrowing begun in v0.5.2 is now complete. A `defined`
/ `blessed` / `isa` guard refines a value to its concrete type inside the branch
it guards, and `Optional<T>` tracks maybe-undef values. That refined type flows
through hover, completion, and goto — and, new this release, into the
diagnostics below.

- **Reassignment clears narrowing.** A narrowed type is dropped the moment its
  scalar is rebound, so a later use never inherits a stale type.
- **Completion peels `Optional<T>`.** Members complete on a maybe-undef value;
  the optional-deref lint below then offers the guard that makes the access safe.
- **Optional receivers dispatch leniently** — no spurious unresolved-method
  warning on a call through a maybe-undef value.

### Diagnostics (new)

Lints that catch runtime errors by reading a value's inferred type at the point
of use. Undef-deref is always on; the rest are opt-in, each a
key under `initializationOptions.diagnostics` or a `--<kebab>` CLI flag.

- **Undef dereference** (always on). Warns when you dereference a value the
  flow analysis proves is `undef` — a `->method`, `->{k}`, `->[i]`, or `->()`
  on the failing side of a `defined` guard — each a guaranteed runtime die.
- **Unguarded optional dereference** (`optionalDeref`). Flags a deref of a
  maybe-undef `Optional<T>` that no `defined`/`blessed` guard covers, with a
  one-click "add `return unless defined …`" quick-fix.
- **Redundant / contradictory guards** (`redundantGuard`). Flags a
  `defined`/`isa`/`DOES` check whose outcome the value's known type already
  decides — always true (dead `else`) or always false (dead branch).
- **Dereference-shape mismatch** (`derefShape`). Flags a `->{k}` / `->[i]` /
  `->()` whose form contradicts a rep a `ref … eq` guard just proved.
- **Cross-file unresolved method** (`unresolvedMethodCrossFile`). Extends the
  unresolved-method warning to narrowed and imported receivers.

### Rename

- **Cross-file symmetry.** Renaming a sub or method updates every importing
  and calling file, in both directions across the boundary.
- **Configurable method-override scope.** Renaming an overridden method renames
  the whole override family by default — the base declaration, every override
  up and down the hierarchy, and all call sites (matched over proven `@ISA` /
  role edges, never by name). Set `rename.overrideScope: "dispatch"` to rename
  only the definition under the cursor and the calls that dispatch to *it*
  (including `SUPER::`), leaving sibling overrides untouched.
- **`our` package variables** rename across their package, with UX guards.
- **Lexical hash keys** — `my %h = (k => …); $h{k}` renames the key everywhere.
- **Framework-aware groups.** Moo/Moose accessor groups (inheritance-aware),
  Corinna field privacy, and DBIC column-keyed verbs each rename as a unit.

### Heatmap (new)

- **`perl-lsp --heatmap <root>`** — a per-symbol fan-in / fan-out and
  dead-code view over the whole reference graph. JSON by default, `--csv` for
  a flat table, `--html` for a self-contained offline viewer (treemap heat,
  fan-in/out butterfly, dead-code queue).

### Type inference

- **DBIC**: result sources type their rows; `search` / `find` return typed
  result sets and columns resolve as keys.
- **Moo**: a constant array-ref of names expands into individual `has`
  attributes.
- **Mojolicious**: plugin-supplied route invocants resolve generically, so
  goto and hover follow routes built through helper / plugin verbs.

### Fixes

- Skip the unresolved-method warning when `AUTOLOAD` is in the MRO.

## v0.5.3 - 2026-06-23

### Licensing

- Switched to MIT instead of Artistic license (more permissive)

## v0.5.2 — 2026-06-22

### Type narrowing

- **Flow-sensitive guards.** A `defined $x` or `blessed $x` test narrows `$x`
  to its concrete type inside the branch — hover, completion, and goto follow
  the guard. The negative side narrows too (a failed `defined` ⇒ undef).
- **Nested slots, not just variables.** Guards narrow places like
  `$self->{a}{b}`, which hold their narrowed type through the branch.

### Optional types

- **`Optional<T>`.** A maybe-undef value is tracked as `Optional<T>` instead of
  collapsing to unknown. Sources: Type::Tiny `Maybe[T]` / `Optional[T]`, a bare
  `return;`, an `undef` or empty-list (`()`) ternary arm, and mixed undef/value
  return joins.
- A `defined` / `blessed` guard **strips** `Optional<T>` back to `T` inside the
  branch.

### Performance

- Tail POD docs parse each block once instead of re-parsing per sub.

## v0.5.1 - 2026-06-20

### Perltidy

- bugfix for a deadlock between perltidy + internal change handling

## v0.5.0 — 2026-06-18

### Mojolicious

- Multi-arg `->to(...)` route targets resolve — goto-def / references / hover
  land on the action even with trailing stash key/value pairs, and when the
  action is supplied as a keyval (`->to('Ctrl#', action => 'x')`).
- Route values from Mojolicious::Lite *function* verbs (`under` / `get` /
  `any`) now brand the same way the `$r->under(...)` method form does, so a
  partial `->to('#action')` on a downstream route variable resolves.

### Outline

- Mojo helpers, Minion tasks, Mojo::EventEmitter handlers, and Lite routes pin
  in editor sticky-scroll and breadcrumbs while the cursor is inside their
  body (the outline extent now encloses the callback).

### Plugin authoring (`.perl-lsp` / `perl-gen`)

- New per-arg `value_shape` classification and a shared `classified_pairs`
  host fn for keyval / argument-shape-polymorphic verbs. Arg lists reach
  plugins fully flattened.
- `CallContext.arg_pairs` is removed (breaking for external plugins that read
  it) — pair the args with `classified_pairs` over `value_shape` instead.

### Distribution

- Static binaries: Linux musl (x86_64 + aarch64) and static-CRT Windows.

## v0.4.2 — 2026-06-17

The crate and the VS Code extension are republished together at this version.
(0.4.1 was a vscode extension-only republish and as such is skipped.)

### Frameworks & plugins

- **tree-sitter-perl 1.1.3.** Fat-comma keys on continuation lines of a
  multi-line list (`add_columns(\n  name => …,\n  status => …,\n)`, including
  keyword-like names) now parse as literal keys, so those columns resolve.
- DBIC support has moved out of core and into a bundled plugin.
- **New plugin-manifest hooks** (for `.perl-lsp` / `perl-gen` plugins):
  - `arg_name_verbs()` + `arg_pairs` / `receiver_is_package` — a generic keyval
    surface, so list/DSL plugins read their columns / options / names without
    any DSL verb hardcoded in core.
  - `load_verbs()` — declares which method names load a module (Mojo's
    `plugin`), so load tracking is trigger-independent: every `$app->plugin(…)`
    in a nested-plugin cascade is seen, including `for qw/A B C/` loops.
  - `role_makers()` — declares modules whose `use` makes the consumer a role,
    so a house role engine is one manifest line in a `.perl-lsp` plugin away.

### Helper-load & cross-file lint

- **`helper-not-loaded` lint** (hint). A method call whose only resolution is a
  plugin bridge from a workspace module that *no workspace file loads* — the
  helper exists but nothing pulls it in. Honest-quiet: exact-or-tail load
  matching (a default-namespace guess never suppresses an app-custom provider),
  installed CPAN plugins exempt ("downloaded = intended").
- **Loader-config param typing.** `plugin 'X', { … }` types module `X`'s
  `register` config param from the config shapes at every call site
  (agreement-folded across callers; keyed disagreement opens the shape).
- **Cross-file slot typing.** `$self->{k}` reads narrow through `$self->{k} = …`
  writes in *other* files and up the ancestry chain.
- **Extensionless entrypoints indexed.** A shallow shebang scan over the repo
  root + `bin/` + `script/` finds Perl entrypoint scripts with no `.pl`
  extension (Mojo::Lite apps), so `plugin 'X'` loads wired only through them are
  visible.

### Navigation & outline

- **package goto-def.** The module name in `use Foo::Bar;` now jumps to its `.pm`
  (or the in-file package).
- **Outline nests under packages.** `documentSymbol` folds each file-scope sub
  under its owning `package`/`class`, so the outline, sticky-scroll, and
  breadcrumb nest correctly. Plain scripts stay flat.
- **Regex semantic token dropped.** `qr//` / regex literals no longer emit a
  `regexp` token, letting the editor's TextMate `string.regexp` scope keep
  escape-sequence highlighting.

### Role contracts, part 2 — long-distance intelligence

- **Go-to-implementation** (`textDocument/implementation`, new capability).
  On a role's `requires 'name'` marker — or on `$self->name` inside the role
  body — fans out to every transitive composer's definition of the contract.
  Goto-def stays on the contract; references stay call sites. On a plain
  class method it returns subclass overrides. CLI mirror:
  `perl-lsp --implementations <root> <file> <line> <col>`.
- **Composer-mismatch diagnostic** (`role-requires-unfulfilled`, WARNING).
  A package composing a role must provide every `requires`d name — local
  sub, inherited method, `has` accessor, or a sibling role's def. Anchored
  on the `with 'Role'` ref. Honest-silent when it can't know: roles defer
  obligations to their class composers, AUTOLOAD satisfies anything, an
  unresolved ancestor may provide anything. A role that both requires AND
  defines a name (the default-implementation pattern) counts as providing
  it. Calibrated to zero false positives on the 2,293-module substrate and
  on a large production codebase.
- **Runtime-generated parents recorded as incomplete ancestry.** `with
  ParametricRole(type => ...)` — a parent edge that doesn't fold to a literal
  name — now marks the package's ancestry incomplete, suppressing
  inheritance-dependent diagnostics on it (removed 41 false
  `unresolved-method` hints on the calibration codebase).
- **`children_index`** — new reverse edge in the module index (parent
  class/role → composing/inheriting modules), the shared primitive for the
  long-distance family. All reverse maps now live in one bundle fed
  atomically by every registration path (insert, warm rebuild, workspace
  scan), making cold/warm index divergence unrepresentable.

## v0.4.0

A large release: cross-file intelligence everywhere, framework and exporter
awareness, an extensible plugin system, and a reproducible regression net.
Robustness-swept clean across 2,293 real CPAN modules (no crashes, panics, or
hangs).

### Highlights

- **Cross-file everything.** Go-to-definition, references, rename, hover,
  completion, and signature help follow imports, inheritance, re-exported
  surfaces, and dynamic dispatch across files — not just the open buffer.
- **Type inference.** A witness-bag engine infers variable types, method-call
  return types, and chained-expression types — through constructors, accessors,
  fluent setters, and cross-file calls.
- **Framework intelligence.** Moo, Moose, Mojolicious (Mojo::Base), DBIx::Class,
  Class::Tiny, and MooX::Options: `has`/`extends`/`with`/`use parent`
  inheritance, generated accessors with typed returns, and constructors.
- **Field & attribute groups.** A `has` attr or Corinna field and ALL its
  spellings — decl, accessor calls, constructor keys, `$self->{slot}` pokes,
  even derived names like `has_size` — rename and find-references as one
  entity, across files.
- **Structural shapes.** Hash and array literals carry their keys/slots in the
  type — drills narrow per key, mutation extends or opens the shape, and the
  `unknown-hash-key` hint catches key typos at **zero false positives** across
  the 2,293-module substrate.
- **Role contracts.** `requires` declares what a role demands: calls on those
  methods resolve inside the role, and a method a role uses WITHOUT declaring
  it gets a hint — surfacing years-old undeclared contracts in real codebases.
- **Bundled plugins + a plugin system.** Ships plugins for Catalyst, Dancer2,
  Mojolicious (helpers, events, Minion tasks), and DBIx::Class ResultDDL; you can
  drop your own `.rhai` plugins in a repo-local `.perl-lsp/`, or generate one from
  an `Import::Base` kit with `perl-gen`.

### Navigation & references

- Cross-file go-to-definition / references / rename for functions, methods, and
  packages (workspace-indexed), inheritance- and role-aware.
- Fully-qualified calls (`Foo::Bar::baz()`), `goto &Foo::bar` / `&Foo::bar()`,
  and imported functions resolve to their defining sub.
- Fully-qualified and SUPER method dispatch: `$obj->Foo::Bar::m`, `SUPER::m`
  (resolved over the full parent MRO, lazily and cross-file), and the `->::m`
  `main::` shorthand work across goto-def, references, rename, hover, and
  signature help.
- **Re-export surfaces:** goto-def through a module that re-exports another's
  `@EXPORT` (static-splice and loop-push forms) lands on the original sub.
- Dynamic dispatch & events resolve cross-file: Mojo helpers / plugin app
  surface, event emitters (`->on` / `->emit`), and Minion tasks.
- Refs reach into more places: chained hash-ref keys at any depth, `use constant`
  usages, `s///e` replacement code, forward-reference calls (call above its sub),
  and sigil-deref spellings.
- **Roles connect both ways**: references from a role method find every
  composing class's call sites, and map-built role lists
  (`with map "My::Roles::$_", qw/CSV DB/`) fold statically — goto-def lands on
  each role from its own qw word.
- **Barewords naming an in-scope sub are calls** (Perl's own reading): hover,
  goto-def, references, rename, and semantic tokens all work on
  `my $x = get_config;` and the `get_config->{host}` base — and `sub INFINITY ()`
  prototype constants link their usages.
- Interpolated code is code: `${\ $self->filetype }` inside a string OR a
  regex pattern carries real refs — rename and references reach into
  `s/_to_${\ $self->filetype }$//`.
- **Mojolicious plugin names** goto-def: `plugin 'Thing'` lands on the plugin
  class's `register`, whatever namespace it lives in (`Mojolicious::Plugin::*`
  and app-specific trees both work).

### Field & attribute projection groups

- **Moo/Moose/Mojo::Base** `has` attrs tie the decl token, accessor calls,
  constructor keys (`Widget->new(size => …)`), and internal `$self->{size}`
  slot pokes into one rename/references entity — subclasses reaching into the
  parent's hashref included (ancestry-aware, while unrelated same-named arg
  keys stay out).
- **Corinna** (`use v5.38; class`): `field $x :param :reader` ties the field
  variable, constructor key, and reader calls; `:param` fields synthesize
  constructor keys so goto-def from `Point->new(x => …)` lands on the field.
- **Derived accessor names re-derive on rename**: `predicate => 1`'s
  `has_size` becomes `has_extent` when the attr renames — at call sites, in
  consumer files, and on user-written `sub _build_size` definitions. Plugins
  enroll synthesized methods with a single `attr:` field.
- Constructor keys in files that only `use` the class are indexed (the
  build-time gate defers to query time), so cross-file references, rename,
  and goto-def see them.

### Type inference

- Constructors via `bless` (including the `bless { %{ $_[0] } }, ref $_[0]` clone
  idiom) type to the right class; fluent setters (`return $self`) chain.
- Receiver-polymorphic constructors (`bless {}, ref($class) || $class`,
  including through `SUPER::new`) type as the call-site receiver, cross-file.
- Constant-folded invocants dispatch: `my $c = 'Counter'; $c->bump` resolves
  to `Counter::bump` — the same fold dynamic method names get.
- Type::Tiny / Types::Standard / Types::Common `isa` constraints map to types
  (`InstanceOf['X']`, `Maybe[...]` unwrapping).
- `$self` / `$c` controller typing for Mojolicious and Catalyst actions.
- Parameters declared as `my ($self, $name) = (shift, shift)` are recognized for
  signature help
- **Framework-assigned Mojo attrs type**: `$c->app`, `$c->tx`, `$c->ua`,
  `$c->req`/`res` (and the `$tx->req`/`res` delegations) chain without an
  indexed Mojo tree, and a plugin's `register($self, $app, $conf)` knows
  `$app` is the application.

### Mojolicious routing & helpers

- **Lite apps inherit route targets**: `under('/notifications')->to('notifications#under')`
  sets the controller the following `get(...)->to('#action')` partials use,
  `group { }` scopes it — goto-def works on nested route groups, and the
  base restores after the group.
- **Default helpers are discovered from source** — no hardcoded list:
  DefaultHelpers/TagHelpers' own registration loops
  (`$app->helper($_ => …) for qw(...)`) expand, so all ~50 built-in helpers
  resolve, including dotted chains (`$c->proxy->get_p`). Framework-loaded
  plugins resolve unconditionally; `plugin 'X'` pulls X in — "you loaded it".
- Helpers registered the lite way (`helper name => sub {…}`, no `$app->`)
  resolve like the method spelling.

### Structural hash & array shapes

- **Hash literals carry their keys in the type.** `{ host => 'x', port => 5432 }`
  is a shape, not a bare HashRef: `->{key}` narrows to the per-key type through
  assignment hops, double-drills (`$config->{db}{host}` → String), sub-return
  literals, and imports (cross-file drill hops included). Array literals type as
  per-index tuples — the mixed drill `$obj->{users}[0]{name}` chains end-to-end,
  and hover shows `Sequence<…>`.
- **`$row->{name}` on a typed row** (DBIC and friends) resolves to the column
  def — goto-def, references, hover.
- **Both spellings.** `my %h = (k => v)` types through the same shape builder
  as the hashref literal; `$h{k}` reads check against `%h`'s shape. Spreads —
  hash and array (`my %opts = (default => 1, @_)`) — mark the shape open.
- **Mutation is modeled, not guessed.** An unconditional `$v->{k} = …` write
  extends the shape (the read drills back typed); a conditional, dynamic-key,
  closure, or slice write (every slice spelling: `@h{…}`, `%h{k}`, `@$h{…}`,
  `$h->@{…}`, `$h->%{…}`) switches it open.
- **New diagnostic: `unknown-hash-key`** (hint severity). A read of a key a
  CLOSED literal shape doesn't define — the typo catcher — naming the known
  keys. Covers variables in both spellings AND expression bases
  (`cfg()->{kye}`, `$obj->get_config->{kye}`); calibrated to **zero false
  positives across the 2,293-module substrate**. Design:
  `docs/adr/structural-shapes.md`.
- **Escapes widen temporally instead of suppressing.** Passing `$config` to a
  call opens its shape AT that point — a typo read *before* the escape still
  hints, instead of the whole variable going dark. Sequence tuples learn from
  direct index writes too (slot retype, append at len).
- **Batch/CI diagnostics match the editor.** `--check` and `--batch`
  diagnostics now run the same cross-file enrichment as open buffers
  (memoized per process), so imported shapes hint there too.

### Exporters & imports

- Models `Exporter`, `Exporter::Tiny`, `Sub::Exporter`, `Exporter::Extensible`,
  and `Importer` / `Exporter::Declare`, plus `%EXPORT_TAGS`, `@EXPORT_OK`, and
  runtime-exporter setups — so imported names resolve and unimported-but-exported
  names get an actionable hint.
- One import-binding evaluator backs diagnostics and navigation, so "what does
  this `use` bring into scope" is answered the same way everywhere.

### Completion, outline & tokens

- `$self->` completion offers inherited methods (cross-file via `use parent` /
  `use Mojo::Base -base`), plus hash keys, imported subs, module names on `use`
  lines, and `qw()` import lists. Auto-import code action for unresolved calls.
- Outline collapses each Moo/Mojo `has` accessor to a single entry, hides
  sub-body lexicals, drops `use` statements, and de-dupes forward declarations.
- Semantic tokens: `use constant` names emit as enum members; `qr//` literals as
  regexp.

### Codegen & dynamic Perl

- Recognizes typeglob-assigned subs (`*name = \&x`, `*name = $cond ? \&a : sub
  {…}`), `AutoLoader`/`SelfLoader` `__DATA__` subs, and accessor codegen
  (`mk_classdata`, `mk_group_accessors`, Class::Tiny).

### Parser

- Adopted **ts-parser-perl 1.1.0 → 1.1.2**: many more valid Perl constructs parse
  cleanly — braced variable declarations (`my ${foo}`), sub/method forward
  declarations (`sub NAME;`), bare dotted versions (`use 5.14.0`), glued
  `x`-repetition, the `not` operator, `\&subname` refgen, arrow-chained subscripts
  in interpolated strings, and correct hashref-vs-block disambiguation
  (`bless {@_}, $class`). Fixes the long-standing `s{}{}` external-scanner abort
  on large modules; 1.1.2 parses `return bless {…}, CLASS` with both args under
  the call, retiring the last bless workaround in the builder.

### Diagnostics

- Framework-aware false-positive suppression (incomplete ISA chains,
  `@EXPORT_OK` on a bare `use`, runtime exporters, XS/typeglob installs).
- Quieter by design: unresolved-function/method are emitted at **hint**
  severity, since dynamic Perl (AUTOLOAD, runtime glob installs, uninstalled
  deps) is common and shouldn't flood the Problems panel. This is frequently
  solvable by using a custom plugin.
- **Role-contract hints**: a role calling `$self->fetch_raw` without
  `requires 'fetch_raw'` keeps its hint — the diagnostic teaches the contract
  the role should declare (declared requirements resolve silently).

### Editors & CLI

- **VS Code extension** auto-downloads the matching binary (config path → cached
  → `PATH` → GitHub Release), so install-and-go works with no separate setup.
- New CLI query modes for scripting/CI: `--batch` (one startup, many queries on
  stdin), plus `--completion`, `--signature-help`, `--semantic-tokens`,
  `--document-highlight`, `--linked-editing`, and `--timings`.
- **One-shot CLI sessions resolve @INC modules themselves** (headless
  resolver: same scan, project-local libs, and SQLite cache as the editor) —
  CLI answers match the editor instead of depending on what an editor session
  happened to cache. Workspace-symbol search no longer lists anonymous subs.

### Under the hood

- One cursor→target resolution entry point shared by every LSP handler and CLI
  mode — references and rename can no longer disagree on what the cursor means.
- A typed CST layer and a Perl-conventions module encode each grammar trap and
  name rule once (qualified method tokens, invocant shapes, canonical variable
  spellings — `${ sner }` ≡ `$sner`).
- Lazy tree resolution is gone: every query answers from build-time facts.
- Cold-start and hot-path performance: memoized witness-bag edge chases, indexed
  fold loops, and a memoized per-pass export surface.
- The four-layer architecture is enforced by tests: imports flow down only,
  the data model's tree-sitter surface is `Point`-only, and the grammar has a
  single entry point (`builder::create_parser`).
- A CI-gated **gold corpus** — exact-assertion LSP checks (179 gold rows + 12
  pinned known gaps) against a version-pinned CPAN substrate — guards every
  release, and every suite run ends with a **cost report** (per-capability
  timing, startup latency, CPU, peak RSS; `METRICS_OUT=` for run-over-run
  comparison) so feature work sees what it costs as it lands.

### Known gaps

A handful of inference edge cases degrade gracefully (a plain symbol instead of
a typed one), tracked in
[`gold-corpus/KNOWN-GAPS.md`](gold-corpus/KNOWN-GAPS.md) — notably
first-param-self over-reach for helpers/callbacks inside OO classes.
