; Laravel framework overlay — the rails and the Eloquent relation
; accessors. Rationale, vocabulary and the honest boundaries:
; docs/adr/laravel-rails.md (§Relations are properties for the lane
; below). Framework VOCABULARY only — every pattern here is spelled in
; the standard captures, so the engine carries no Laravel names.

; Four patterns, two relation families (to-one / to-many) x two chain
; shapes (bare / ONE modifier). The chain depth is the limit the query
; medium imposes: a pattern names a fixed nesting, so
; `belongsTo(X::class)->withTrashed()->withDefault()` — two modifiers —
; matches none of these and the property stays untyped, silently. Read
; the four arms as the shapes we cover, not as the shapes that exist;
; a third nesting level is a fifth and sixth pattern, which is where
; this stops being worth writing by hand.
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

