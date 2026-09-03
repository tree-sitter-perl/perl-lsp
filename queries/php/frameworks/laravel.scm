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
