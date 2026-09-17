; Laravel framework overlay — the rails and the Eloquent relation
; accessors. Rationale, vocabulary and the honest boundaries:
; docs/adr/laravel-rails.md (§Relations are properties for the lane
; below). Framework VOCABULARY only — every pattern here is spelled in
; the standard captures, so the engine carries no Laravel names.

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

; ---- the routes rail (docs/adr/laravel-rails.md) ----
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

; ---- the event bus (a class-keyed rail, `docs/adr/laravel-rails.md`) ----
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
; The empty list is the parens with no named child between them, which is
; what the tree states; its source text is not (`dispatch( )`, or a
; newline, spells the same empty call).
(scoped_call_expression
  scope: [(name) @ref.dispatch.class.event
          (qualified_name (name) @ref.dispatch.class.event)]
  name: (name) @dispatch.via
  arguments: (arguments "(" . ")")
  (#any-of? @dispatch.via "dispatch" "dispatchIf" "dispatchUnless" "dispatchSync" "dispatchAfterResponse")
  (#not-any-of? @ref.dispatch.class.event "static" "self" "parent"))
; The same with an argument. The two spellings the arms BELOW claim — a
; `new …` and a class constant — are excluded here by their first token
; alone (a query cannot say "not this node kind", and one query's patterns
; have no precedence over each other, so both arms would mint). Anchored
; at the start, so a `::` anywhere later in the argument (`self::$x`) is
; still an emission this arm names.
(scoped_call_expression
  scope: [(name) @ref.dispatch.class.event
          (qualified_name (name) @ref.dispatch.class.event)]
  name: (name) @dispatch.via
  arguments: (arguments . (argument) @_lev_a1)
  (#any-of? @dispatch.via "dispatch" "dispatchIf" "dispatchUnless" "dispatchSync" "dispatchAfterResponse")
  (#not-any-of? @ref.dispatch.class.event "static" "self" "parent")
  (#not-match? @_lev_a1 "^new[^A-Za-z0-9_]")
  (#not-match? @_lev_a1 "^[\\\\A-Za-z_][\\\\A-Za-z0-9_]*::[A-Za-z_]"))
; `Bus::dispatch(Events::X, …)` — a scope whose first argument is a class
; CONSTANT names the event by a value the overlay cannot read; the
; emission is minted with no dispatcher, which the diagnostics lane
; reads as "unnameable" and stays silent on.
(scoped_call_expression
  scope: [(name) @ref.dispatch.class.event
          (qualified_name (name) @ref.dispatch.class.event)]
  name: (name) @_lev_via
  arguments: (arguments . (argument (class_constant_access_expression)))
  (#any-of? @_lev_via "dispatch" "dispatchIf" "dispatchUnless" "dispatchSync" "dispatchAfterResponse"))

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

