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
; `redirect()->route('home')` / `$this->redirect()->route(…)`
(member_call_expression
  name: (name) @dispatch.via
  arguments: (arguments
    . (argument (string . (string_content) @ref.dispatch.named.route .)))
  (#eq? @dispatch.via "route"))
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
  (#any-of? @dispatch.via "event" "dispatch" "dispatch_sync" "broadcast"))
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
  (#eq? @_lev_args "()"))
(scoped_call_expression
  scope: [(name) @ref.dispatch.class.event
          (qualified_name (name) @ref.dispatch.class.event)]
  name: (name) @dispatch.via
  arguments: (arguments . (argument) @_lev_a1)
  (#any-of? @dispatch.via "dispatch" "dispatchIf" "dispatchUnless" "dispatchSync" "dispatchAfterResponse")
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
