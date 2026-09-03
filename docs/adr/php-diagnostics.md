# PHP diagnostics: the pack symbol lanes

Scope: `--features php`; the lanes run for any pack language whose
`LangPack` declares the facts they read.

Pack documents publish through `symbols::pack_diagnostics`, which now
carries the **pack symbol lanes** (`diagnostics::pack_symbol_diagnostics`)
beside the member-operator and use-after-move checks. The Perl hub's
always-on `unresolved-function` / `unresolved-method` stay on their own
path; the pack lanes read facts only packs mint — `Ref::arg_count`,
`Symbol::arity`, the use-map's namespace pins, `Ref::binding` on every
variable read — and the pack's own declarations (`receiver_names`,
`implicit_variables`, `catch_all_methods`, `constructor_names`,
`enum_members`, `class_literal_member`, `types_are_capitalized`).

## Lanes

| code | fires when | severity |
|---|---|---|
| `unresolved-method` | a member CALL on a receiver whose class the dispatch projection names, the class declares members, and no ancestor declares the method | error |
| `undefined-property` | the same, for a member READ (`MemberShape::Value`) | error |
| `non-public-access` | the member resolves to a `non_public` symbol and the enclosing class is neither the owner nor one of its descendants | error |
| `arity-mismatch` | a resolved callable's `ParamArity` rejects the written count: fewer than `required` (error) or more than `total` on a non-variadic list (warning) | error / warning |
| `undefined-variable` | a variable read with no binding, inside a callable, read exactly once there | error |
| `undefined-type` | a class reference (a type hint, a `use` row, `new Foo`, a static receiver) whose namespace — pinned by the file's `use` rows, else its own — declares no such class in this file or the settled workspace index | error |

The receiver is THE dispatch projection (`method_call_invocant_class`):
the bag's type for a variable, the class body's receiver witness for
`$this` (the extractor registers every class body with its class as the
scope's package and a `Variable("$this") = ClassName` witness, so a chain
based on the receiver resolves through the registry like any typed
variable), the expression's own witnesses for `$a->b->c()`. Nothing is
manufactured from an untyped receiver.

A pack can declare its member shapes STRICT (`LangPack::
member_shapes_are_strict`, php): the syntax itself decides call vs read,
so `unresolved-method`/`undefined-property` fire independently even
when the class overloads the name across kinds — the diagnostics lanes
need no `TargetRef::member_shape` gate. Goto-def and hover read the
same declaration: a cursor on a strict pack's method-call ref mints
`TargetRef::member_shape` from the written shape (`identity.rs`), and
`member_value_type` skips the method arm for a value read, so a value
read of a method-only name is an undeclared property on every verb.
Declaration cursors keep the overload-only gate (an Eloquent
`$chapter->book` still references `book()`).

## Silence rules (precision first)

Every lane names the case it cannot see and stays silent there:

- **A parent's namespace is what the `extends` clause wrote** (`extends
  \Exception` is the global one even from a class itself named
  `Exception`), so a same-leaf child never stands in for a builtin parent
  the workspace carries no stubs for — that parent is unreadable, below.
- **An ancestor the workspace cannot read** — at any depth, resolved by
  the CHILD file's own namespace pins (a same-leaf stranger is not the
  parent) — may declare the member: silent. This is what keeps every
  PHPUnit-derived test quiet without vendored PHPUnit.
- **A trait's `$this`** is whatever class composes it — every member the
  trait does not declare may live there — so a trait body's undefined
  members stay silent.
- **A `$this` call a descendant declares** is the template-method idiom
  (WordPress's `ftp_base` calling `$this->_exec()` that only `ftp_pure` /
  `ftp_sockets` implement): the runtime class may be that subclass, so
  the call is silent when any dispatch participant below the class
  resolves the member. A foreign receiver (`$ftp->_exec()`) still reports —
  its declared type is the contract.
- **A catch-all class** (php `__call`/`__callStatic`/`__get` anywhere in
  the ancestry) answers any member name — Perl's `AUTOLOAD` rule.
- **An interface-typed receiver** names any implementation; php code
  narrows with `instanceof` before calling what the interface lacks.
  Narrowing retypes a VARIABLE receiver (the `if` block, a negated
  guard that exits, `assert`, the `&&`/ternary/`match` regions), but a
  member subject (`$this->x instanceof T`), a method guard (`->isT()`)
  or `is_a()` leave the interface standing — so an interface stays
  silent on undefined members (resolved ones still check arity).
- **A closure's `$this`** may be rebound (`Closure::bind`, `->call($obj)`
  — the private-access idiom tests live on): the non-public lane is
  silent inside anonymous functions.
- **A class with no declared constructor** has the default one.
- **A method named by a string** (`[$obj, 'name']`, the class-array
  callable) is data until dispatch proves it a callable: a reference and
  rename target when it resolves, never a finding when it does not
  (`RefKind::MethodCall::named_by_string`).
- **A spread argument** (`f(...$args)`, the pack's `spread_arg_kind`)
  makes the call's count unknowable; the arity lane stands down.
- **A read inside an existence probe** (`isset($tax->helps)`,
  `empty($this->x)`) is the question of whether the member exists, not a
  claim that it does — the undefined-property lane stays silent there
  (`PackFacts::probe_regions`, the pack's `@probe.region` capture).
- **A property declared by writing it** (`$this->x = …` anywhere in the
  file, on the same class) is declared.
- **An enum's language-given members** (`->value`, `->name`,
  `::cases()`, `::from()`, `::tryFrom()`).
- **`Foo::class`** is the class-name literal, never a member.
- **A callable reading its arguments dynamically** (`func_get_args`)
  accepts any count; a first-class callable `f(...)` mints no count.
- **A write binds** (`$x = …`, `$rows[] = …`, `static $map = …` declare;
  a by-reference capture `use (&$x)` declares in the ENCLOSING scope —
  the declaration hoists there, as php creates the variable there).
- **A member whose resolved kind disagrees with the read** (a property
  read that only finds a same-named method) is no access violation.
- **An import row whose leaf the file never spells bare** imports a
  namespace (`use GuzzleHttp\Psr7;` then `Psr7\Utils`) or nothing —
  no type to assert.
- **A variable read more than once** in its callable is presumed bound
  by reference (`preg_match($re, $s, $m)` — the walker cannot see the
  out-parameter); the single stray read is the typo the lane names.
  Runtime-bound names (`$this`, superglobals) never fire. A callable
  that materializes variables dynamically (`extract`, `compact`,
  `get_defined_vars`, `parse_str`, `eval`) is silent.
- **The global namespace** is the builtins no stubs are carried for:
  `\Foo`, `use Foo;` and any name pinned to `""` are silent; a file with
  no namespace is silent for the whole lane. A segment used as a
  namespace prefix in the file (`Psr7\Utils`) names a namespace, not a
  type; an import row with a lowercase leaf names a function or constant
  (`use function A\b;` parses as a class row).
- **The undefined-type lane needs a settled index**: `--check` is
  settled by construction; every editor publish passes the pack family's
  ready gate (`index_ready.pack.is_open()`), so the lane publishes once
  the workspace index has landed and never before — an unindexed
  workspace would flag every type.
- **An unused import** (`unused-import`, a hint tagged unnecessary) is a
  row whose bound name — the leaf, or the alias — the file never spells
  as a class token, a function call, a namespace prefix, or a docblock
  word (`PackFacts::doc_mentions`, gathered at extraction). Only packs
  whose imports bind names (`LangPack::imports_bind_names`) run it: an
  `#include` splices text. Constant imports (no lowercase letter) are
  silent — the walker records no spelling for them. The quick-fix deletes
  the row when it binds only that name.
- **The throwaway name** (`$_` in `foreach ($a as $k => $_)`, the pack's
  `throwaway_names`) is written to be discarded and is never unused.
- **An unused variable** (`unused-variable`, a hint tagged unnecessary)
  is a local the callable writes and never reads — a read counts for its
  callable, every enclosing one (a closure's `use ($x)` reads the outer
  `$x` through its own copy) and every nested one (a by-reference capture
  is written inside the closure and read around it), and a same-named
  declaration in a nested callable is the capture itself. A parameter
  (the pack's `param_regions`) is
  the caller's contract, never a local; a callable that materializes
  variables dynamically is silent, as for the undefined-variable lane.
- **A deprecated declaration** (`deprecated`, a hint tagged deprecated)
  flags each use: the declaration's `@deprecated [text]` or the pack's
  deprecation attribute (`LangPack::deprecated_attribute`, php
  `#[Deprecated]`) lands as the `deprecated` symbol attribute with the
  notice in `Presentation::deprecation`; both ride the symbols axis, so a
  dependency's declaration answers through `symbols_present`. Members
  read the resolved owner symbol; functions and classes look up the leaf
  locally, then across the visible candidates.
- **The global namespace** (an absolute name, a namespace-less file) is
  silent for the classes php itself provides (`LangPack::builtin_types`:
  core, SPL, the bundled extensions — names, not stubs) and for any leaf
  the workspace declares globally; a leaf the workspace declares only
  under a namespace is a real type missing its import, and reports with
  its candidates.
- **An undefined type is a quick-fix**: the diagnostic carries every
  namespace that declares the leaf (`data.candidates`, the same set the
  existence test reads), and `codeAction` offers one import per
  candidate in the pack's own statement (`LangPack::import_template`),
  inserted after the last import row above the site, else after the
  namespace declaration, else after the first line.

## What the walker had to stop minting

The lanes surfaced reads the skeleton minted wrongly, fixed at the
source rather than filtered: a static property's `$name` (`Foo::$bar`)
and a declaration's own `$name` token are not variable reads
(`@var.member`, and any `.name` capture's end byte); `catch ($e)` and
`use (&$x)` are declarations; `\Throwable` keeps its absolute spelling
in the use-map pins (the leading separator IS the prefix); an aliased or
imported namespace prefix (`P\Promise`, `Psr7\Utils`) resolves through
the `use` rows.

## Measured (2026-09-02, corpora without vendored PHPUnit/Symfony)

See `bench/RESULTS.md` for the per-corpus counts against Intelephense on
the same files. Known residuals: `createMock()`-typed receivers
(`$mock->expects()` — PHPUnit's mock intersection type needs a framework
overlay), and `instanceof` narrowing.
