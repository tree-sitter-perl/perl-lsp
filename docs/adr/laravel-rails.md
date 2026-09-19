# Laravel rails

Every string-named framework seam is a **rail**: a string-keyed identity
namespace on the `Handler` / `DispatchCall` axis (the identity WordPress
hooks ride). A rail's definitions and uses share a name and an owner,
and nothing else — so definitions, references, rename, completion, call
hierarchy and the undefined-name diagnostic come by construction for
every rail, and a new rail is overlay vocabulary plus a rail document,
never a handler branch.

## Identity

`HandlerOwner` (`model/file_analysis/dispatch.rs`) carries the namespace,
and every namespace is NAMED:

- `Class(name)` — the handler is registered on a class.
- `Rail(name)` — a receiver-less namespace one framework owns, named by
  the overlay capture's suffix. Name + owner equality is the entire
  match: a route name and a view name may coincide and must not connect,
  and the rail is the reason they do not.

**What a rail's names DENOTE is the rail document's own declaration** —
this section is its one home; every other site points here. The document
carries `names_are` beside `labels` / `hints` (`"names_are": { "event":
"class" }`; a rail absent from it names strings), the loader bakes it onto
every file of the pack as `PackFacts::class_named_rails`, and
`HandlerOwner::names_are` answers `RailNames`:

- `Strings`, for every rail nothing declares otherwise: the name is the
  string the framework matches, renameable, the edit rewriting inside the
  quotes at every site.
- `Classes`, Laravel's `event`: the names are class identities and the
  spans sit on class tokens (an emission's `new X`, a listener's
  parameter type). Navigable — goto-def lists the class AND the handlers
  rather than picking — and never renameable, because the class rename
  owns the name.

The class-keyed capture family (`@def.handler.class.<rail>`,
`@def.handler.by.<rail>`, `@ref.dispatch.class.<rail>`) is how the
overlay MINTS those handlers; it is not where the fact lives. Baking the
declaration per pack is what covers the span-free minting paths
(`scan_text_rails`, `adopt_path_rails`), which no query reaches, and what
keeps one rail from answering differently in two files. The two halves
are pinned to each other: a `.class.` capture on a rail no document
declares, or a declared class rail no capture family mints, is a finding
(`class_rail_capture_findings` / `class_rail_declaration_findings` —
`--plugin-check`'s overlay arm and the bundled-documents test).

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
- **The rail document** (`laravel.rails.json` bundled per pack, or
  `<plugin-dir>/<name>/rails.json`, discovered once and projected by
  `rail_conventions_for` / `path_rails_for` / `text_rails_for` — every
  family of one document loads or fails to load together):
  - `path_rails`: a file under `under` defines a name derived from its
    path (`resources/views/a/b.blade.php` → `a.b`, a Handler at the
    file's first position); with `keys`, the path names a prefix the
    file's string array keys extend (`config/app.php` → `app.mail.from`,
    nested keys dotted, each a Handler on the key token; `lang/en/auth.php`
    skips the locale segment, every locale a stacked definition); with
    `methods`, every method of the file is a name (a policy's abilities),
    the Handler linked to the declaration it stands on
    (`rail_handler_twin`) so a consumer asks the relation instead of
    rediscovering it from a span — that link is what keeps a policy method
    off the heatmap's dead queue (`docs/adr/heatmap.md`, `rail-handler`).
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
    carries parameters (`throttle:60,1` names `throttle`, span included);
    `names_are` declares what a rail's names denote (§Identity).

## The rails

| rail | defines | uses |
|---|---|---|
| `route` | `Route::…->name('x')` (a name ending in `.` is a group prefix) | `route`, `to_route`, `redirect()->route` (the member form pinned to a `redirect` receiver: `$request->route('id')` reads a parameter), `URL::route`, `Route::has`, templates |
| `event` (class) | a listener's `handle(X $e)`, a `$listen` key, `Event::listen(X::class)`, a job's own `handle` | `event(new X)`, `X::dispatch(…)` (a `new` first argument names the event; `static::dispatch` names nothing), `dispatch(new Job)`, `broadcast` |
| `view` (path) | `resources/views/**` | `view`, `View::make`, `->view`, `@extends`, `@include` (+ `@includeIf` / `@includeWhen` / `@includeFirst`), `@each`, `@component` |
| `config` (path, keys) | `config/*.php` array keys | `config`, `Config::get` and kin |
| `lang` (path, keys, locale skipped) | `lang/<locale>/*.php`, `resources/lang/<locale>/*.php` | `__`, `trans`, `trans_choice`, `Lang::get`, `@lang`, `@choice` — keys with a dot only |
| `middleware` (hint, `:` separator) | `$middlewareAliases` / `$routeMiddleware` / `$middlewareGroups`, `$middleware->alias([…])` / `->group('x', …)`, `Route::aliasMiddleware` / `middlewareGroup`, the framework's `defaultAliases()` | `->middleware('x')`, the array form, `withoutMiddleware`, `Route::middleware` |
| `ability` (hint; path `methods` under `app/Policies/`) | `Gate::define`, `$gate->define`, every policy method | `->authorize`, `->can` / `->cannot` / `->cant`, `Gate::allows` and kin, `@can` / `@cannot` / `@elsecan` / `@elsecannot` |
| `binding` (hint) | `->singleton('key')` / `bind` / `bindIf` / `singletonIf` / `scoped` / `instance` / `alias`, `App::…`, the core aliases of `registerCoreContainerAliases` | `app('key')`, `resolve`, `->make` / `makeWith` / `bound` / `get` on the app, `App::make` |

The container resolves what the argument spells: `app(Foo::class)`,
`resolve(Foo::class)`, `->make(Foo::class)`, `App::make(Foo::class)` IS a
Foo — `@expr.annot` declares the call's value from the same match's
`@type.annot`, minted as a plugin-priority `Expr → TypeName` witness that
outranks the callee's own return in `expr_type_at_span`.

## Relations are properties

The same overlay carries a second lane, and it is not a rail: an Eloquent
relation method declares a PROPERTY of its own name, because Eloquent's
`__get` serves `$book->cover` from `cover()`. The overlay mints the field
at the method's own name token (`@def.field` on
`name: (name)`), so `$book->cover` navigates, completes and hovers.

- A **to-one** relation (`belongsTo` / `hasOne` / `morphOne` /
  `hasOneThrough`) carries the related class as the property's type
  (`@type.annot` off the `X::class` argument), so `$page->book->name`
  chains. One chained modifier
  (`$this->belongsTo(Book::class)->withTrashed()`) is the same relation —
  the modifier returns it — and types the same way.
- A **to-many** relation (`hasMany` / `belongsToMany` / `morph*` /
  `hasManyThrough`) is a Collection, and no token in the declaration
  spells that class, so the property stays untyped: the element type is
  the generics residual. The field is still minted, which is what
  goto-def and completion need.
- The `#any-of?` method lists ARE the framework's vocabulary — the one
  place a relation-builder name is written. Nothing checks that the
  declaring class is a `Model`: a class with a `belongsTo` method behaves
  like one whether or not the ancestry is readable, and an `isa` gate
  would be a shape branch that silently drops every model whose base
  lives in an absent vendor tree (rule #10).

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
