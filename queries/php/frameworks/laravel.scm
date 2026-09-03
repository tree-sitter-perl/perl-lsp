; Laravel framework overlay — Eloquent relation accessors.
;
; This is framework VOCABULARY, kept out of the base skeleton on the
; plugin doctrine: everything here is expressed in the standard capture
; vocabulary plus text predicates, so the engine carries no Laravel
; names. Bundled by concatenation into the php pack's query (the same
; way rhai plugins bundle for Perl); a dynamically-loaded pack-plugin
; dir is the follow-on seam.
;
; `public function pages() { return $this->hasMany(Page::class); }`
; declares BOTH the method (the base skeleton already minted it) and —
; through Eloquent's __get — a PROPERTY of the same name. Mint the
; field so `$book->pages` navigates and completes.
;
; To-ONE relations carry the related class as the property's type
; (`$page->book->name` chains). To-MANY properties are Collections —
; there is no token spelling that class here, so they stay untyped
; (the element type is the generics residual); the field still gives
; gd/completion the name.

(method_declaration
  name: (name) @def.field.name @def.field @flow.target
  body: (compound_statement
    (return_statement
      (member_call_expression
        object: (variable_name)
        name: (name) @_lrel1
        arguments: (arguments
          . (argument (class_constant_access_expression . [(name) (qualified_name)] @type.annot))))))
  (#any-of? @_lrel1 "belongsTo" "hasOne" "morphOne" "hasOneThrough"))
; The same relation behind ONE chained modifier
; (`$this->belongsTo(Book::class)->withTrashed()`) — the modifier returns
; the relation, so the property is still the related model.
(method_declaration
  name: (name) @def.field.name @def.field @flow.target
  body: (compound_statement
    (return_statement
      (member_call_expression
        object: (member_call_expression
          object: (variable_name)
          name: (name) @_lrel1c
          arguments: (arguments
            . (argument (class_constant_access_expression . [(name) (qualified_name)] @type.annot)))))))
  (#any-of? @_lrel1c "belongsTo" "hasOne" "morphOne" "hasOneThrough"))

(method_declaration
  name: (name) @def.field.name @def.field
  body: (compound_statement
    (return_statement
      (member_call_expression
        object: (variable_name)
        name: (name) @_lrel2
        arguments: (arguments
          . (argument (class_constant_access_expression))))))
  (#any-of? @_lrel2 "hasMany" "belongsToMany" "morphMany" "morphToMany" "hasManyThrough"))
(method_declaration
  name: (name) @def.field.name @def.field
  body: (compound_statement
    (return_statement
      (member_call_expression
        object: (member_call_expression
          object: (variable_name)
          name: (name) @_lrel2c
          arguments: (arguments
            . (argument (class_constant_access_expression)))))))
  (#any-of? @_lrel2c "hasMany" "belongsToMany" "morphMany" "morphToMany" "hasManyThrough"))

; ---- the routes rail (docs/prompt-laravel-parity.md) ----
; `Route::get('/x', …)->name('home')` DECLARES the route name on the
; `route` rail (a Handler named by the string content, owner
; `Rail("route")`); `route('home')`, `to_route`, `redirect()->route`,
; `URL::route`, `Route::has` USE it (DispatchCalls on the same rail).
; The receiver text pins the chain to the router (`Route::…` /
; `$router->…`); a name ending in `.` is a group PREFIX
; (`Route::prefix('/a')->name('a.')->group(…)`), not a route.
(member_call_expression
  object: (_) @_lr_chain
  name: (name) @_lr_name
  arguments: (arguments
    . (argument (string . (string_content) @def.handler.named.route .)))
  (#eq? @_lr_name "name")
  (#match? @_lr_chain "^(Route::|\\$router->|\\$this->router->)")
  (#not-match? @def.handler.named.route "\\.$"))

(function_call_expression
  function: (name) @dispatch.via
  arguments: (arguments
    . (argument (string . (string_content) @ref.dispatch.named.route .)))
  (#any-of? @dispatch.via "route" "to_route"))
; `redirect()->route('home')` / `$this->redirect()->route(…)` — the
; receiver is a `redirect` call: `$request->route('id')` reads a route
; PARAMETER, not a name.
(member_call_expression
  object: [(function_call_expression function: (name) @_lr_redir)
           (member_call_expression name: (name) @_lr_redir)]
  name: (name) @dispatch.via
  arguments: (arguments
    . (argument (string . (string_content) @ref.dispatch.named.route .)))
  (#eq? @dispatch.via "route")
  (#eq? @_lr_redir "redirect"))
(member_call_expression
  name: (name) @dispatch.via
  arguments: (arguments
    . (argument (string . (string_content) @ref.dispatch.named.route .)))
  (#eq? @dispatch.via "redirectToRoute"))
; `URL::route('home')`, `Redirect::route`, `Route::has('home')`,
; `URL::signedRoute` — wildcard matchers (`Route::is('admin.*')`) are
; not uses of one name and stay out.
(scoped_call_expression
  scope: (name) @_lr_facade
  name: (name) @dispatch.via
  arguments: (arguments
    . (argument (string . (string_content) @ref.dispatch.named.route .)))
  (#any-of? @_lr_facade "URL" "Redirect" "Route")
  (#any-of? @dispatch.via "route" "has" "signedRoute" "temporarySignedRoute"))

; ---- the event bus (a class-keyed rail, `docs/prompt-laravel-parity.md`) ----
; Names are CLASS names: an emission `event(new X(…))` / `X::dispatch(…)`
; / `dispatch(new Job)` / `broadcast(new X)` is a DispatchCall named X on
; the `event` rail whose span is the class token (goto-def there lists
; the class AND the handlers); a handler is a listener's `handle(X $e)`
; (named by the parameter type, sitting on the method's name token, so
; call hierarchy on `handle` walks the bus), a `$listen` map key, an
; `Event::listen(X::class, …)` / `->listen(X::class, …)` registration, or
; a job's own `handle` (named by its class). Rename never touches the
; rail — the class rename owns the name.
(function_call_expression
  function: (name) @dispatch.via
  arguments: (arguments
    . (argument (object_creation_expression
        . [(name) @ref.dispatch.class.event
           (qualified_name (name) @ref.dispatch.class.event)])))
  (#any-of? @dispatch.via "event" "dispatch" "dispatch_sync" "broadcast")
  (#not-any-of? @ref.dispatch.class.event "static" "self" "parent"))
; `Anything::dispatch(new Y(…))` / `$bus->dispatch(new Y(…))` — a `new`
; first argument names the event; the dispatcher is whoever dispatches.
(scoped_call_expression
  name: (name) @dispatch.via
  arguments: (arguments
    . (argument (object_creation_expression
        . [(name) @ref.dispatch.class.event
           (qualified_name (name) @ref.dispatch.class.event)])))
  (#any-of? @dispatch.via "dispatch" "dispatchSync" "dispatchNow"))
(member_call_expression
  name: (name) @dispatch.via
  arguments: (arguments
    . (argument (object_creation_expression
        . [(name) @ref.dispatch.class.event
           (qualified_name (name) @ref.dispatch.class.event)])))
  (#any-of? @dispatch.via "dispatch" "dispatchSync" "dispatchNow"))
; `X::dispatch(…)` (the Dispatchable trait): the scope IS the event when
; the first argument is not a `new …` — an empty list, or any other value.
(scoped_call_expression
  scope: [(name) @ref.dispatch.class.event
          (qualified_name (name) @ref.dispatch.class.event)]
  name: (name) @dispatch.via
  arguments: (arguments) @_lev_args
  (#any-of? @dispatch.via "dispatch" "dispatchIf" "dispatchUnless" "dispatchSync" "dispatchAfterResponse")
  (#not-any-of? @ref.dispatch.class.event "static" "self" "parent")
  (#eq? @_lev_args "()"))
(scoped_call_expression
  scope: [(name) @ref.dispatch.class.event
          (qualified_name (name) @ref.dispatch.class.event)]
  name: (name) @dispatch.via
  arguments: (arguments . (argument) @_lev_a1)
  (#any-of? @dispatch.via "dispatch" "dispatchIf" "dispatchUnless" "dispatchSync" "dispatchAfterResponse")
  (#not-any-of? @ref.dispatch.class.event "static" "self" "parent")
  (#not-match? @_lev_a1 "^new\\s")
  (#not-match? @_lev_a1 "::"))
; `Bus::dispatch(Events::X, …)` — a scope whose first argument is a class
; CONSTANT names the event by a value the overlay cannot read; the
; emission is minted with no dispatcher, which the diagnostics lane
; reads as "unnameable" and stays silent on.
(scoped_call_expression
  scope: [(name) @ref.dispatch.class.event
          (qualified_name (name) @ref.dispatch.class.event)]
  name: (name) @_lev_via
  arguments: (arguments . (argument) @_lev_a1)
  (#any-of? @_lev_via "dispatch" "dispatchIf" "dispatchUnless" "dispatchSync" "dispatchAfterResponse")
  (#match? @_lev_a1 "::"))

; listeners: `public function handle(X $event)` — any class's `handle`
; whose first parameter is typed; a job's `handle(Dependency $d)` mints a
; handler named by an injected type, which no emission ever names.
(method_declaration
  name: (name) @def.handler.by.event
  parameters: (formal_parameters
    . (simple_parameter
        type: [(named_type (name) @handler.name)
               (named_type (qualified_name (name) @handler.name))]))
  (#eq? @def.handler.by.event "handle"))
; a job's own `handle()` is the handler of `dispatch(new Job)`
(class_declaration
  name: (name) @handler.name
  body: (declaration_list
    (method_declaration
      name: (name) @def.handler.by.event
      parameters: (formal_parameters)))
  (#eq? @def.handler.by.event "handle"))
(class_declaration
  name: (name) @handler.name
  body: (declaration_list
    (method_declaration
      name: (name) @def.handler.by.event
      parameters: (formal_parameters . (simple_parameter type: (_)) .)))
  (#eq? @def.handler.by.event "handle"))
; `protected $listen = [ X::class => [ L::class ] ]`
(property_element
  name: (variable_name) @_lev_listen
  default_value: (array_creation_expression
    (array_element_initializer
      . (class_constant_access_expression
          . [(name) @def.handler.class.event
             (qualified_name (name) @def.handler.class.event)]
          (name) @_lev_k .)))
  (#eq? @_lev_listen "$listen")
  (#eq? @_lev_k "class"))
; `Event::listen(X::class, …)` / `$events->listen(X::class, …)`
(scoped_call_expression
  scope: (name) @_lev_ev
  name: (name) @_lev_listen_m
  arguments: (arguments
    . (argument (class_constant_access_expression
        . [(name) @def.handler.class.event
           (qualified_name (name) @def.handler.class.event)]
        (name) @_lev_k2 .)))
  (#eq? @_lev_ev "Event")
  (#eq? @_lev_listen_m "listen")
  (#eq? @_lev_k2 "class"))
(member_call_expression
  name: (name) @_lev_listen_mm
  arguments: (arguments
    . (argument (class_constant_access_expression
        . [(name) @def.handler.class.event
           (qualified_name (name) @def.handler.class.event)]
        (name) @_lev_k3 .)))
  (#eq? @_lev_listen_mm "listen")
  (#eq? @_lev_k3 "class"))

; ---- path-defined rails: views, config keys, translation keys ----
; A string key heading an array element is a KEY CANDIDATE; the driver
; promotes it to a rail name when the file's path rail says so
; (`config/app.php` → `app.name`, `lang/en/auth.php` → `auth.failed`).
(array_element_initializer
  . (string . (string_content) @def.handler.key .)) @key.elem

; `view('a.b')`, `View::make('a.b')`, `response()->view('a.b')`
(function_call_expression
  function: (name) @dispatch.via
  arguments: (arguments . (argument (string . (string_content) @ref.dispatch.named.view .)))
  (#eq? @dispatch.via "view"))
(scoped_call_expression
  scope: (name) @_lv_facade
  name: (name) @dispatch.via
  arguments: (arguments . (argument (string . (string_content) @ref.dispatch.named.view .)))
  (#eq? @_lv_facade "View")
  (#any-of? @dispatch.via "make" "exists" "first"))
(member_call_expression
  name: (name) @dispatch.via
  arguments: (arguments . (argument (string . (string_content) @ref.dispatch.named.view .)))
  (#eq? @dispatch.via "view"))
; `config('app.name')` / `Config::get('app.name')`
(function_call_expression
  function: (name) @dispatch.via
  arguments: (arguments . (argument (string . (string_content) @ref.dispatch.named.config .)))
  (#eq? @dispatch.via "config"))
(scoped_call_expression
  scope: (name) @_lc_facade
  name: (name) @dispatch.via
  arguments: (arguments . (argument (string . (string_content) @ref.dispatch.named.config .)))
  (#eq? @_lc_facade "Config")
  (#any-of? @dispatch.via "get" "has" "set" "string" "integer" "boolean" "array"))
; `__('auth.failed')`, `trans`, `trans_choice`, `Lang::get` — a key with
; whitespace is a JSON translation STRING (`lang/en.json`), not a key path.
((function_call_expression
  function: (name) @dispatch.via
  arguments: (arguments . (argument (string . (string_content) @ref.dispatch.named.lang .))))
  (#any-of? @dispatch.via "__" "trans" "trans_choice")
  (#match? @ref.dispatch.named.lang "^[A-Za-z0-9_/:-]+\\.[A-Za-z0-9_./:-]+$"))
((scoped_call_expression
  scope: (name) @_ll_facade
  name: (name) @dispatch.via
  arguments: (arguments . (argument (string . (string_content) @ref.dispatch.named.lang .))))
  (#eq? @_ll_facade "Lang")
  (#any-of? @dispatch.via "get" "has" "choice")
  (#match? @ref.dispatch.named.lang "^[A-Za-z0-9_/:-]+\\.[A-Za-z0-9_./:-]+$"))

; ---- the container resolves what the argument spells ----
; `app(Foo::class)` / `resolve(Foo::class)` / `->make(Foo::class)` /
; `App::make(Foo::class)` IS a Foo: the call's value is declared on the
; expression (`@expr.annot`), so a chain off it dispatches on Foo.
((function_call_expression
  function: (name) @_lapp
  arguments: (arguments
    . (argument (class_constant_access_expression
        . [(name) (qualified_name)] @type.annot (name) @_lapp_k .)))) @expr.annot
  (#any-of? @_lapp "app" "resolve")
  (#eq? @_lapp_k "class"))
((member_call_expression
  name: (name) @_lmake
  arguments: (arguments
    . (argument (class_constant_access_expression
        . [(name) (qualified_name)] @type.annot (name) @_lmake_k .)))) @expr.annot
  (#any-of? @_lmake "make" "makeWith")
  (#eq? @_lmake_k "class"))
((scoped_call_expression
  scope: (name) @_lA
  name: (name) @_lAmake
  arguments: (arguments
    . (argument (class_constant_access_expression
        . [(name) (qualified_name)] @type.annot (name) @_lA_k .)))) @expr.annot
  (#eq? @_lA "App")
  (#any-of? @_lAmake "make" "makeWith")
  (#eq? @_lA_k "class"))

; ---- the middleware rail (aliases and groups by name) ----
; Definitions: the kernel's alias/group maps, `$middleware->alias([…])` /
; `->group('name', […])` (Laravel 11's bootstrap), `Route::aliasMiddleware`
; / `Route::middlewareGroup`, and the framework's own `defaultAliases()`.
; A use names the head before the parameter separator (`throttle:60,1`).
(property_element
  name: (variable_name) @_lmw_prop
  default_value: (array_creation_expression
    (array_element_initializer
      . (string . (string_content) @def.handler.named.middleware .) (_)))
  (#any-of? @_lmw_prop "$middlewareAliases" "$routeMiddleware" "$middlewareGroups"))
(member_call_expression
  name: (name) @_lmw_alias
  arguments: (arguments
    . (argument (array_creation_expression
        (array_element_initializer
          . (string . (string_content) @def.handler.named.middleware .) (_)))))
  (#eq? @_lmw_alias "alias"))
(member_call_expression
  object: (variable_name) @_lmw_recv
  name: (name) @_lmw_g
  arguments: (arguments
    . (argument (string . (string_content) @def.handler.named.middleware .)))
  (#eq? @_lmw_recv "$middleware")
  (#any-of? @_lmw_g "group" "appendToGroup" "prependToGroup"))
(scoped_call_expression
  name: (name) @_lmw_s
  arguments: (arguments
    . (argument (string . (string_content) @def.handler.named.middleware .)))
  (#any-of? @_lmw_s "aliasMiddleware" "middlewareGroup"))
(member_call_expression
  name: (name) @_lmw_m
  arguments: (arguments
    . (argument (string . (string_content) @def.handler.named.middleware .)))
  (#any-of? @_lmw_m "aliasMiddleware" "middlewareGroup"))
(method_declaration
  name: (name) @_lmw_def
  body: (compound_statement
    (expression_statement
      (assignment_expression
        right: (array_creation_expression
          (array_element_initializer
            . (string . (string_content) @def.handler.named.middleware .) (_))))))
  (#eq? @_lmw_def "defaultAliases"))
; uses: `->middleware('auth')`, `->middleware(['auth', 'throttle:x'])`,
; `->withoutMiddleware(…)`, `Route::middleware(…)`
(member_call_expression
  name: (name) @dispatch.via
  arguments: (arguments
    . (argument (string . (string_content) @ref.dispatch.named.middleware .)))
  (#any-of? @dispatch.via "middleware" "withoutMiddleware"))
(member_call_expression
  name: (name) @dispatch.via
  arguments: (arguments
    . (argument (array_creation_expression
        (array_element_initializer
          . (string . (string_content) @ref.dispatch.named.middleware .) .))))
  (#any-of? @dispatch.via "middleware" "withoutMiddleware"))
(scoped_call_expression
  scope: (name) @_lmw_R
  name: (name) @dispatch.via
  arguments: (arguments
    . (argument (string . (string_content) @ref.dispatch.named.middleware .)))
  (#eq? @_lmw_R "Route")
  (#any-of? @dispatch.via "middleware" "withoutMiddleware"))
(scoped_call_expression
  scope: (name) @_lmw_R2
  name: (name) @dispatch.via
  arguments: (arguments
    . (argument (array_creation_expression
        (array_element_initializer
          . (string . (string_content) @ref.dispatch.named.middleware .) .))))
  (#eq? @_lmw_R2 "Route")
  (#any-of? @dispatch.via "middleware" "withoutMiddleware"))

; ---- the ability rail (gates and policies) ----
; `Gate::define('name', …)` / `$gate->define(…)` define; a policy's methods
; define through the `/app/Policies/` path rail. Uses: `->authorize`,
; `->can` / `->cannot` / `->cant`, `Gate::allows` and kin, Blade `@can`
; (text lane). Abilities a database grants never have a token, so a miss
; is a hint.
(scoped_call_expression
  scope: (name) @_lg_G
  name: (name) @_lg_def
  arguments: (arguments
    . (argument (string . (string_content) @def.handler.named.ability .)))
  (#eq? @_lg_G "Gate")
  (#eq? @_lg_def "define"))
(member_call_expression
  object: (variable_name) @_lg_recv
  name: (name) @_lg_mdef
  arguments: (arguments
    . (argument (string . (string_content) @def.handler.named.ability .)))
  (#eq? @_lg_recv "$gate")
  (#eq? @_lg_mdef "define"))
(member_call_expression
  name: (name) @dispatch.via
  arguments: (arguments
    . (argument (string . (string_content) @ref.dispatch.named.ability .)))
  (#any-of? @dispatch.via "authorize" "can" "cannot" "cant"))
(scoped_call_expression
  scope: (name) @_lg_G2
  name: (name) @dispatch.via
  arguments: (arguments
    . (argument (string . (string_content) @ref.dispatch.named.ability .)))
  (#eq? @_lg_G2 "Gate")
  (#any-of? @dispatch.via "allows" "denies" "check" "any" "none" "authorize" "inspect" "has"))

; ---- the binding rail (string-keyed container entries) ----
; `->singleton('key', …)` / `->bind` / `->bindIf` / `->singletonIf` /
; `->scoped` / `->instance` / `->alias('key', …)` define (a class-keyed
; entry is the class's own ref); the framework's core aliases are the keys
; of `registerCoreContainerAliases`. Uses: `app('key')`, `resolve('key')`,
; `->make('key')` on the app, `App::make('key')`.
(member_call_expression
  name: (name) @_lb_def
  arguments: (arguments
    . (argument (string . (string_content) @def.handler.named.binding .)))
  (#any-of? @_lb_def "singleton" "bind" "bindIf" "singletonIf" "scoped" "scopedIf" "instance" "alias"))
(scoped_call_expression
  scope: (name) @_lb_A
  name: (name) @_lb_sdef
  arguments: (arguments
    . (argument (string . (string_content) @def.handler.named.binding .)))
  (#eq? @_lb_A "App")
  (#any-of? @_lb_sdef "singleton" "bind" "bindIf" "singletonIf" "scoped" "scopedIf" "instance" "alias"))
(method_declaration
  name: (name) @_lb_core
  body: (compound_statement
    (foreach_statement
      (array_creation_expression
        (array_element_initializer
          . (string . (string_content) @def.handler.named.binding .) (_)))))
  (#eq? @_lb_core "registerCoreContainerAliases"))
(function_call_expression
  function: (name) @dispatch.via
  arguments: (arguments
    . (argument (string . (string_content) @ref.dispatch.named.binding .)))
  (#any-of? @dispatch.via "app" "resolve"))
(member_call_expression
  object: [(member_access_expression name: (name) @_lb_recv)
           (variable_name) @_lb_recv
           (function_call_expression function: (name) @_lb_recv)]
  name: (name) @dispatch.via
  arguments: (arguments
    . (argument (string . (string_content) @ref.dispatch.named.binding .)))
  (#match? @_lb_recv "^\\$?app$")
  (#any-of? @dispatch.via "make" "makeWith" "bound" "get"))
(scoped_call_expression
  scope: (name) @_lb_A2
  name: (name) @dispatch.via
  arguments: (arguments
    . (argument (string . (string_content) @ref.dispatch.named.binding .)))
  (#eq? @_lb_A2 "App")
  (#any-of? @dispatch.via "make" "makeWith" "bound"))
