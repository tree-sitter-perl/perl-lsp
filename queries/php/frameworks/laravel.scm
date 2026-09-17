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

