# Laravel rails

Every string-named framework seam is a **rail**: a string-keyed identity
namespace on the `Handler` / `DispatchCall` axis (the identity WordPress
hooks ride). A rail's definitions and uses share a name and an owner,
and nothing else — so definitions, references, rename, completion, call
hierarchy and the undefined-name diagnostic come by construction for
every rail, and a new rail is overlay vocabulary plus a rail document,
never a handler branch.

## Identity

`HandlerOwner` (`model/file_analysis/dispatch.rs`) carries the namespace:

- `Global` — one flat namespace (WordPress hooks).
- `Rail(name)` — a string rail. Name + owner equality is the match;
  renameable, the edit rewriting inside the quotes at every site.
- `ClassRail(name)` — a class-keyed rail: the names are class names and
  the spans sit on class tokens (an emission's `new X`, a listener's
  parameter type). Never renameable — the class rename owns the name —
  and goto-def lists the class AND the handlers rather than picking.

A route name and a view name may coincide and must not connect; the rail
is the reason they do not.

## Where a name comes from

- **The overlay** (`queries/php/frameworks/laravel.scm`) declares rails in
  the capture: `@def.handler.named.<rail>` / `@ref.dispatch.named.<rail>`
  (string rails), `@def.handler.class.<rail>` / `@ref.dispatch.class.<rail>`
  (class rails), `@def.handler.by.<rail>` + `@handler.name` (a handler
  whose span is one token and whose name is another's text — a
  listener's `handle(X $e)` is a handler named `X` on the method's name
  token, so call hierarchy on `handle` walks the bus), and
  `@def.handler.key` + `@key.elem` (string array keys as candidates a
  path rail may promote).
- **The rail document** (`laravel.rails.json`, loaded per pack by
  `rail_conventions_for` / `path_rails_for` / `text_rails_for`):
  - `path_rails`: a file under `under` defines a name derived from its
    path (`resources/views/a/b.blade.php` → `a.b`, a Handler at the
    file's first position); with `keys`, the path names a prefix the
    file's string array keys extend (`config/app.php` → `app.mail.from`,
    nested keys dotted, each a Handler on the key token; `lang/en/auth.php`
    skips the locale segment, every locale a stacked definition); with
    `methods`, every method of the file is a name (a policy's abilities).
    Applied by the driver at `analyze_with_path` — the query does not
    know the path.
  - `text_rails`: a file the grammar reads as text (a Blade template)
    still USES names — `route('x')`, `@extends('x')`, `@include`, `__(`,
    `@can` — scanned as text (`scan_text_rails`) and minted as the same
    `DispatchCall` refs a parsed use gets. `requires` is a substring every
    name must carry (a translation key needs a dot; a bare word is a JSON
    translation string).
  - `labels` phrase the lane's miss; `hints` lists the rails whose miss
    is a hint; `name_seps` gives a rail the separator after which a use
    carries parameters (`throttle:60,1` names `throttle`, span included).

## The rails

| rail | defines | uses |
|---|---|---|
| `route` | `Route::…->name('x')` (a name ending in `.` is a group prefix) | `route`, `to_route`, `redirect()->route` (the member form pinned to a `redirect` receiver: `$request->route('id')` reads a parameter), `URL::route`, `Route::has`, templates |
| `event` (class) | a listener's `handle(X $e)`, a `$listen` key, `Event::listen(X::class)`, a job's own `handle` | `event(new X)`, `X::dispatch(…)` (a `new` first argument names the event; `static::dispatch` names nothing), `dispatch(new Job)`, `broadcast` |
| `view` (path) | `resources/views/**` | `view`, `View::make`, `->view`, `@extends`, `@include`, `@each`, `@component` |
| `config` (path, keys) | `config/*.php` array keys | `config`, `Config::get` and kin |
| `lang` (path, keys, locale skipped) | `lang/<locale>/*.php`, `resources/lang/<locale>/*.php` | `__`, `trans`, `trans_choice`, `Lang::get`, `@lang` — keys with a dot only |
| `middleware` (hint, `:` separator) | `$middlewareAliases` / `$routeMiddleware` / `$middlewareGroups`, `$middleware->alias([…])` / `->group('x', …)`, `Route::aliasMiddleware` / `middlewareGroup`, the framework's `defaultAliases()` | `->middleware('x')`, the array form, `withoutMiddleware`, `Route::middleware` |
| `ability` (hint; path `methods` under `app/Policies/`) | `Gate::define`, `$gate->define`, every policy method | `->authorize`, `->can` / `->cannot` / `->cant`, `Gate::allows` and kin, `@can` / `@cannot` |
| `binding` (hint) | `->singleton('key')` / `bind` / `bindIf` / `singletonIf` / `scoped` / `instance` / `alias`, `App::…`, the core aliases of `registerCoreContainerAliases` | `app('key')`, `resolve`, `->make` / `makeWith` / `bound` / `get` on the app, `App::make` |

The container resolves what the argument spells: `app(Foo::class)`,
`resolve(Foo::class)`, `->make(Foo::class)`, `App::make(Foo::class)` IS a
Foo — `@expr.annot` declares the call's value from the same match's
`@type.annot`, minted as a plugin-priority `Expr → TypeName` witness that
outranks the callee's own return in `expr_type_at_span`.

## Cross-file

A handler feed rides the reverse index under the file's PATH key
(`feed_handlers`), owner-tagged, recorded per path and persisted in the
warm stub — a route name is reachable by name, never offered as an
identifier nor a class-slot winner. `resolve::handler_definitions` is
the lookup goto-def, the lane and call hierarchy share; a rebuild's clear
spares the path-keyed feeds (their only source is the records that
replay them), and a core the warm load fed nothing into skips the
rebuild — its clear-then-refeed window landed under the diagnostics
sweep on a one-shot CLI and read as an empty rail.

## Completion

`Slot::RailName` (`lsp/cursor_slot.rs`): a string that IS a use on a rail
(the document's own ref at the cursor) or would be with the cursor's
position spelled into it (`cursor_sentinel::rail_string_ctx` — the
sentinel through the pack's rail patterns for parsed regions and the
text rails for a template). The items are the rail's names across the
index (`rail_names`, read off the owner-tagged records — never a
rehydration), prefix-filtered, each edit replacing the whole string
content. A translation key completes once its first segment carries a
dot (the overlay's regex is the lane's honesty gate).

## Silence rules (the undefined-name lane)

A name ending in `.` / `_` / `-` is a prefix the caller concatenates
onto; a name containing `::` is a package-namespaced view whose provider
is outside the path rails; a class-keyed emission with no dispatcher
(`Bus::dispatch(Consts::EVENT)`) is an event the overlay could not name;
a `*` is a wildcard. Names a framework synthesizes without a token
(`Route::resource`) have no definition, so the route lane warns rather
than errors; the hint rails' definitions are partly runtime-only
(framework defaults in an absent vendor tree, database-granted
abilities), so their miss is a lead.

## What is deliberately not here

Route URIs and parameters (no identity to connect), `Route::resource`'s
synthesized names and Eloquent scopes (name synthesis — a plugin tier),
`env('KEY')` (`.env` is not a php file), Eloquent fields from migrations
(a migration walker), and bare facade aliases (`use DB;`): across
BookStack, panel and koel there are zero bare-alias spellings and zero
`class_alias()` calls — every facade use imports the FQ class, which the
`@method` lane resolves (`docs/PARKED.md`).

Measured behaviour per round: `bench/RESULTS.md`, "Laravel parity arc".
