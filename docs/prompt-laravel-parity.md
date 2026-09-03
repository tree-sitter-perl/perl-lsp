# Laravel parity — brief

Feature box, not a time box: parity with JetBrains' Laravel Idea on the
navigation and diagnostics axes, done the perl-lsp way — every
string-named framework seam is a **rail** (the `Handler` / `DispatchCall`
identity the WordPress hooks ride), so definitions, references, rename,
completion and the undefined-name diagnostic come by construction for
each rail, and the event bus is a graph the call hierarchy walks. Laravel
Idea completes strings; we connect them.

Corpora (`$S/php-corpus`): **pterodactyl panel** (104 named routes, 59
`route()` uses in PHP + 223 in Blade, 42 views, 105 translations, a
middleware alias map), **koel** (9 events, 9 listeners, a `$listen` map,
`event(new X)` emissions, `Dispatcher::dispatch(new Job)`), **BookStack**
(134 views, 83 config keys, 339 translations, `Theme::dispatch` own bus).

## The matrix

| Laravel Idea | shape | ours today | round |
|---|---|---|---|
| route names: completion, goto, usages, rename | `Route::get(..)->name('x')` ↔ `route('x')`, `to_route`, `redirect()->route`, `URL::route`, `Route::has`, Blade `{{ route('x') }}` | nothing | 1 |
| controller actions | `[Ctrl::class, 'm']` | class-array callables: goto + references | landed |
| route URIs, parameters | `'/users/{user}'` | — | parked (no identity to connect) |
| `Route::resource` generated names | `photos.index` … | — | parked (Tier-2 plugin: name synthesis) |
| views: goto file, usages, undefined view, completion | `view('a.b')`, `View::make`, `@extends`, `@include`, `<x-name>` ↔ `resources/views/a/b.blade.php` | nothing | 3 |
| config keys | `config('app.name')` ↔ `config/app.php` array key | nothing | 3 |
| translation keys | `__('auth.failed')`, `trans`, `@lang` ↔ `lang/en/auth.php` key | nothing | 3 |
| env keys | `env('APP_KEY')` ↔ `.env` | — | parked (dotenv is not a php file) |
| events ↔ listeners; jobs | `event(new X)`, `X::dispatch()` ↔ `$listen` map, `Event::listen`, `handle(X $e)`; `dispatch(new Job)` ↔ `Job::handle` | class refs only | 2 |
| gates / policies | `Gate::define('x')` ↔ `can('x')`, `authorize('x')`, `@can` | nothing | 4 |
| middleware aliases | Kernel `$middlewareAliases` ↔ `->middleware('auth')` | nothing | 4 |
| container bindings | `$this->app->bind('x')` ↔ `app('x')`; `app(Foo::class)` typed Foo | nothing | 4 |
| facades → real class | `Route::get` → `Router` via `getFacadeAccessor` / `@method` | `@method` docs only (FQ spelling) | 4 |
| Eloquent fields from migrations / DB | `$user->name` | relations only | parked (needs a migration walker) |
| scopes `scopeActive` → `->active()` | | — | parked (Tier-2 plugin) |
| validation rules, request fields, Livewire, Inertia, artisan generation | | — | out of box |

## The mechanism: rails

A rail is a string-keyed identity namespace declared by an overlay
query. Today the WordPress overlay mints `@def.handler.named` /
`@ref.dispatch.named` into ONE flat namespace (`HandlerOwner::Global`).
Laravel needs one namespace per kind — a route name and a view name may
coincide and must not connect — so:

- `HandlerOwner::Rail(String)` — a named flat namespace. The overlay
  names it in the capture: `@def.handler.named.route` /
  `@ref.dispatch.named.route` (the `.self` suffix precedent on
  `ref.method.named`). No receiver gate, name + owner equality is the
  match — every `Global` consumer treats `Rail` the same way.
- **Name from another token** (`@handler.named.by`): a handler whose
  span is one token and whose name is another's text — a listener's
  `handle(X $e)` is a handler named `X` on the `event` rail sitting on
  the method name, so call hierarchy on `handle` walks the bus.
- **Path-defined names** (`laravel.rails.json`, data beside the overlay):
  a file under `resources/views` defines `a.b` on the `view` rail; a file
  under `config/` defines the prefix `app` on the `config` rail and every
  string key of its array literal (nested by containment) extends it;
  `lang/<locale>/auth.php` skips the locale segment. The driver applies
  the table at `analyze_with_path` (it knows the path; the query does
  not). Blade files are already indexed by the pack (`*.blade.php`).
- **Text rails** (same json): a `.blade.php` file is HTML text to the
  grammar, so its `route('x')`, `__('x')`, `@extends('x')`, `@include`,
  `@can('x')`, `<x-name>` uses are a regex lane minting `DispatchCall`
  refs on the named rail — references reach templates, rename rewrites
  them.
- **Diagnostics per rail**: `undefined-<rail>` (a use with no definition
  anywhere in the settled index) and the unused definition through the
  heatmap's dead queue. A rail name carrying a parameter
  (`throttle:api`) resolves on the text before the rail's declared
  separator.
- **Completion**: the string slot of a rail use offers the rail's names
  from the index (`Handler` symbols by owner).
- **Event bus**: the `event` rail is keyed by CLASS name. Emissions
  (`event(new X)`, `X::dispatch()`, `Event::dispatch(new X)`,
  `dispatch(new Job)`, `broadcast(new X)`) are `DispatchCall`s named X;
  handlers are `handle(X $e)` methods (named by the param type), `$listen`
  rows (`X::class => [...]`), `Event::listen(X::class, …)` and a job's own
  `handle()` (named by the enclosing class). Call hierarchy incoming on a
  `handle` = the emission sites; outgoing on an emission = the handlers;
  `unlistened-event` when an emission has no handler in the index.

## Rounds

1. Rail namespaces + the routes rail (defs, uses, Blade text lane for
   `route()`), `undefined-route`; battery on panel and BookStack; gold.
2. Event bus (name-from-token handlers, emissions, `$listen` rows, call
   hierarchy, `unlistened-event`); battery on koel.
3. Path-defined rails: views, config, lang keys (+ Blade `@extends` /
   `@include` / `__()` / `@lang`), goto to a file, `undefined-view` /
   `-config-key` / `-translation`; battery on BookStack and panel.
4. Gates, middleware aliases (parameter separator), container bindings
   (+ `app(Foo::class)` typed), facade accessor resolution for the bare
   alias spelling (`use DB;` — `docs/PARKED.md`).
5. Rail-name completion in the string slot; the Laravel battery vs
   Laravel Idea's claims; gold rows per rail; `docs/adr/laravel-rails.md`
   as the durable contract; this brief deleted.

Every round: full net, corpus counts grep-verified, RESULTS ledger
section, CHANGELOG bullet.
