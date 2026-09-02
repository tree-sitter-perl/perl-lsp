# PHP diagnostics: the pack symbol lanes

Status: landed 2026-09-02 (day-2 arc). Scope: `--features php`; the lanes
run for any pack language whose `LangPack` declares the facts they read.

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

## Silence rules (precision first)

Every lane names the case it cannot see and stays silent there:

- **An ancestor the workspace cannot read** — at any depth, resolved by
  the CHILD file's own namespace pins (a same-leaf stranger is not the
  parent) — may declare the member: silent. This is what keeps every
  PHPUnit-derived test quiet without vendored PHPUnit.
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
- **The undefined-type lane needs a settled index**: `--check` passes
  `index_settled = true`; the editor's first publish after `didOpen`
  passes `false` and the post-resolution refresh carries the lane, so an
  unindexed workspace never flags every type.

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
