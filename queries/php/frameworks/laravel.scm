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
