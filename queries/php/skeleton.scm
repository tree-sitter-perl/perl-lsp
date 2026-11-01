; PHP language pack. Grammar: tree-sitter-php's `php` flavor, which keeps
; the `<?php` tag and any interleaved HTML as inert nodes.

; ---- namespaces ----
; `namespace A\B;` holds until the next one; the braced form holds for its
; body. Classes declared under either join it to mint their identity, and
; either is the section its `use` rows bind in.
(namespace_definition
  name: (namespace_name) @def.package.name @context.namespace
  !body) @def.package
(namespace_definition
  name: (namespace_name) @def.package.name @context.namespace
  body: (compound_statement) @scope) @def.package
; The global namespace's braced form opens a scope too, so the rows it
; imports bind in its body alone.
(namespace_definition
  !name
  body: (compound_statement) @scope)

; ---- classes, interfaces, traits ----
; One model kind; the body's members file under the class's identity. The
; keyword or modifier that says which flavour of class it is rides
; @sym.attr, whose spelling is the flag it mints.
(class_declaration
  (abstract_modifier)? @sym.attr
  name: (name) @def.class.name @context.class) @def.class @scope
(interface_declaration
  "interface" @sym.attr
  name: (name) @def.class.name @context.class) @def.class @scope
(trait_declaration
  "trait" @sym.attr
  name: (name) @def.class.name @context.class) @def.class @scope

; ---- callables ----
(function_definition name: (name) @def.sub.name) @def.sub
(method_declaration name: (name) @def.method.name) @def.method
; Parameters and locals are body content, not members. A closure's
; parameters are its own.
(function_definition) @scope.sub
(method_declaration) @scope.sub
(anonymous_function) @scope.sub
(arrow_function) @scope.sub

; ---- parameters ----
; A parameter is a local of its callable, named like every variable by
; the token after its `$`: a read of `$x` references `x` and leaves the
; `$` alone. Its declared type (a single one) is its value from the binding
; site on. A variadic's declared type is its elements', so the local stays
; untyped.
(simple_parameter
  type: [(named_type) (primitive_type) (optional_type)]? @type.annot
  name: (variable_name (name) @def.local.name @flow.target)) @def.local
(variadic_parameter
  name: (variable_name (name) @def.local.name)) @def.local

; ---- properties ----
; A property is a data member of its class, named without the `$` it is
; declared with: `$obj->x` reads the member `x`. The `$x` it is written as
; is the spelling it is listed by. A declared type is the member's value;
; a union or intersection names no single type, so only a single type is
; captured. `readonly` is display vocabulary, no flag.
(property_declaration
  (readonly_modifier)? @sym.attr
  type: [(named_type) (primitive_type) (optional_type)]? @type.annot
  (property_element
    name: (variable_name (name) @def.field.name @flow.target) @def.field.spelled) @def.field)
; A constructor parameter with a visibility or `readonly` is also a
; property of the class (PHP 8 promotion); the member belongs to the class
; body, not to the constructor whose parameter list declares it.
; The same token is the constructor's local too, so the match declares
; the two as one (`@def.pair`): a rename of either rewrites both, and the
; declared type is the value of each. (A node holds at most three
; captures; tree-sitter drops the rest silently.)
(property_promotion_parameter
  readonly: (readonly_modifier)? @sym.attr
  type: [(named_type) (primitive_type) (optional_type)]? @type.annot
  name: [
    (variable_name (name) @def.field.name @def.local.name @flow.target) @def.field.spelled
    (by_ref
      (variable_name (name) @def.field.name @def.local.name @flow.target) @def.field.spelled)
  ]) @def.field @def.local @def.pair

; ---- what a declaration is, stated on its name ----
; Modifiers come in any order, so each fact is a pattern of its own.
(method_declaration
  (visibility_modifier) @_visibility
  name: (name) @mark.non_public
  (#any-of? @_visibility "private" "protected"))
(property_declaration
  (visibility_modifier) @_visibility
  (property_element name: (variable_name (name) @mark.non_public))
  (#any-of? @_visibility "private" "protected"))
(property_promotion_parameter
  visibility: (visibility_modifier) @_visibility
  name: [
    (variable_name (name) @mark.non_public)
    (by_ref (variable_name (name) @mark.non_public))
  ]
  (#any-of? @_visibility "private" "protected"))
(method_declaration (static_modifier) name: (name) @mark.static)
(property_declaration
  (static_modifier)
  (property_element name: (variable_name (name) @mark.static)))
(method_declaration (abstract_modifier) name: (name) @mark.abstract)
; A method declared without a body (an interface's, an abstract one) is
; a contract the composing class must provide.
(method_declaration name: (name) @mark.contract !body)
; The one method `new Foo(...)` invokes; its name belongs to the language.
((method_declaration name: (name) @mark.constructor)
 (#eq? @mark.constructor "__construct"))
; A class declaring one of these answers ANY member name at runtime, so it
; has no closed member surface to check a name against.
((class_declaration
  name: (name) @mark.dynamic_members
  body: (declaration_list (method_declaration name: (name) @_catch_all)))
 (#any-of? @_catch_all "__call" "__callStatic" "__get"))
((trait_declaration
  name: (name) @mark.dynamic_members
  body: (declaration_list (method_declaration name: (name) @_catch_all)))
 (#any-of? @_catch_all "__call" "__callStatic" "__get"))

; ---- parents ----
; `extends`, `implements` and a trait `use` are all parents for member
; lookup. The whole written spelling is the capture: it resolves through
; the file's use-map to the parent's identity. The same name is a class
; reference — goto-def, references and rename reach it — and a qualified
; spelling references by the whole spelling while rewriting only its leaf.
(class_declaration
  name: (name) @def.class.name
  (base_clause
    [(name) @parent @ref.type
     (qualified_name (name) @ref.type) @parent @ref.spelled]))
(class_declaration
  name: (name) @def.class.name
  (class_interface_clause
    [(name) @parent @ref.type
     (qualified_name (name) @ref.type) @parent @ref.spelled]))
(interface_declaration
  name: (name) @def.class.name
  (base_clause
    [(name) @parent @ref.type
     (qualified_name (name) @ref.type) @parent @ref.spelled]))
(class_declaration
  name: (name) @def.class.name
  body: (declaration_list
    (use_declaration
      [(name) @parent @ref.type
       (qualified_name (name) @ref.type) @parent @ref.spelled])))
(trait_declaration
  name: (name) @def.class.name
  body: (declaration_list
    (use_declaration
      [(name) @parent @ref.type
       (qualified_name (name) @ref.type) @parent @ref.spelled])))

; ---- type positions ----
; Every class a type names is a class reference, wherever the type is
; written (a parameter, a property, a return) and inside a nullable or a
; union: rename of the class rewrites it. `self`, `parent` and `static`
; name the class by where they are written, not by spelling.
((named_type (name) @ref.type)
 (#not-any-of? @ref.type "self" "parent" "static"))
(named_type (qualified_name (name) @ref.type) @ref.spelled)

; ---- variables ----
; A variable read is the whole `$x`'s value and a reference to the name
; `x`. A static property's `Foo::$x` is a member, not a variable.
((variable_name (name) @expr.read.var.name) @expr.read.var
 (#not-has-parent? @expr.read.var scoped_property_access_expression))

; ---- the use-map ----
; A row binds a class unless its keyword says it binds a function or a
; constant; those rows name no class, so they never enter the map. The
; keyword is an anonymous token, so the row's own text excludes it. The
; `.` anchor pins the name to the clause's first child, so the alias node
; never matches as a second name. An imported class's leaf references the
; class the row names, so a rename of the class rewrites the `use` line
; too; the ref is the leaf alone, since the row it sits in already names
; the class in full (`spelled_identity`).
((namespace_use_declaration
  (namespace_use_clause
    . [(name) @import.name @ref.type
       (qualified_name (name) @ref.type) @import.name]
    alias: (name)? @import.alias) @_clause)
 (#not-match? @_clause "^(function|const)\\s"))
; `use A\B\{C, D as E}`: each clause hangs off the shared prefix. A group
; may carry the keyword for all its clauses or each clause its own.
((namespace_use_declaration
  (namespace_name) @import.prefix
  body: (namespace_use_group
    (namespace_use_clause
      . (name) @import.name @ref.type
      alias: (name)? @import.alias) @_clause)) @_group
 (#not-match? @_group "^use\\s+(function|const)\\s")
 (#not-match? @_clause "^(function|const)\\s"))

; ---- the receiver ----
; `$this` is the receiver of the method around it: an object of that
; method's class.
((variable_name (name) @_this) @receiver
 (#eq? @_this "this"))
