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

; ---- written types ----
; `self` and `static` name the class they are written in, nullable or not,
; wherever a type is written: a return, a parameter, a property.
(([
  (named_type (name) @_class)
  (optional_type (named_type (name) @_class))
 ] @type.enclosing)
 (#any-of? @_class "self" "static"))

; ---- callables ----
; A declared return (a single type) is the callable's value at every call.
(function_definition name: (name) @def.sub.name return_type: [(named_type) (primitive_type) (optional_type)]? @rettype) @def.sub
(method_declaration name: (name) @def.method.name return_type: [(named_type) (primitive_type) (optional_type)]? @rettype) @def.method
; Parameters and locals are body content, not members. A closure's
; parameters are its own. A function, a method or a closure sees no
; variable of the code around it but the ones its `use` clause names; an
; arrow function sees every one.
(function_definition) @scope.sub.closed
(method_declaration) @scope.sub.closed
(anonymous_function) @scope.sub.closed
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
 (#not-has-parent? @expr.read.var scoped_property_access_expression)
 (#not-eq? @expr.read.var "$this"))
; `$this` is no local: it is the object the enclosing method runs on, an
; instance of the method's class.
((variable_name) @receiver.enclosing
 (#eq? @receiver.enclosing "$this"))

; Parentheses hold the value they wrap.
((parenthesized_expression (_) @expr.wrapper.inner) @expr.wrapper
 (#not-is-kind? @expr.wrapper.inner comment))

; ---- writes ----
; A variable belongs to its function: the first write declares it, and
; every later write and every read in the body is that one variable. A
; write lands once the assignment has read what it reads, and replaces
; what the variable held.
(assignment_expression
  left: (variable_name (name) @def.assigned.name @flow.target) @def.assigned
  right: (_) @flow.source) @flow.assign
(reference_assignment_expression
  left: (variable_name (name) @def.assigned.name @flow.target) @def.assigned
  right: (_) @flow.source) @flow.assign
; A compound write keeps the kind of value the variable holds; a
; destructuring and a loop write values they do not name.
(augmented_assignment_expression
  left: (variable_name (name) @def.assigned.name @flow.rebind) @def.assigned)
(assignment_expression
  left: (list_literal (variable_name (name) @def.assigned.name @flow.rebind) @def.assigned)) @flow.assign
(assignment_expression
  left: (list_literal (pair (_) . (variable_name (name) @def.assigned.name @flow.rebind) @def.assigned .))) @flow.assign
(foreach_statement . (_) . [
  (variable_name (name) @def.assigned.name @flow.rebind) @def.assigned
  (by_ref (variable_name (name) @def.assigned.name @flow.rebind) @def.assigned)
  (pair . (variable_name (name) @def.assigned.name @flow.rebind) @def.assigned)
  (pair (_) . [
    (variable_name (name) @def.assigned.name @flow.rebind) @def.assigned
    (by_ref (variable_name (name) @def.assigned.name @flow.rebind) @def.assigned)
  ] .)
])
; A caught exception is an instance of the class the clause names; a
; clause naming several (`A | B`) names no single type.
(catch_clause
  type: (type_list . (named_type) @type.annot .)
  name: (variable_name (name) @def.assigned.name @flow.target) @def.assigned)
(global_declaration (variable_name (name) @def.assigned.name) @def.assigned)
(static_variable_declaration
  name: (variable_name (name) @def.assigned.name @flow.target) @def.assigned
  value: (_)? @flow.source)
; An assignment writes the place its left side denotes, whatever that is
; (`$x->p = v` writes the property); a compound one writes it too, with a
; value it does not name.
(assignment_expression left: (_) @flow.place right: (_) @flow.source) @flow.assign
(reference_assignment_expression left: (_) @flow.place right: (_) @flow.source) @flow.assign
(augmented_assignment_expression left: (_) @flow.place)

; ---- closures ----
; `use ($x)` reads the variable of the code around the closure, and the
; closure body sees it by that name. `use (&$x)` binds it there,
; declaring it when nothing there does yet.
(anonymous_function_use_clause (variable_name (name) @closure.capture))
(anonymous_function_use_clause
  (by_ref (variable_name (name) @closure.capture @def.assigned.name @def.hoist)) @def.assigned)

; ---- calls ----
; A call's value is an Invoke of its callee on its arguments, and the
; argument list rides the callee's match. `...$xs` passes any number of
; values. A named argument (`f(a: 1)`) is one more argument in its written
; place; nothing binds it by name yet.
(function_call_expression
  function: (name) @ref.call
  arguments: (arguments ((_) @arity.arg ","?)*) @arity.args) @expr.call
; A qualified function is named by its whole spelling and rewritten at its
; leaf.
(function_call_expression
  function: (qualified_name (name) @ref.call) @ref.spelled
  arguments: (arguments ((_) @arity.arg ","?)*) @arity.args) @expr.call
(arguments (argument (variadic_unpacking)) @arity.spread)

; ---- members ----
; `$x->m()` calls the method `m` of the receiver's class and `$x->p` reads
; its property `p`, named without the `$` it is declared with; the nullsafe
; `?->` reaches the same members. A dynamic name (`$x->$m()`) names none.
(member_call_expression
  object: (_) @member.recv
  name: (name) @ref.member.call
  arguments: (arguments ((_) @arity.arg ","?)*) @arity.args) @expr.call
(nullsafe_member_call_expression
  object: (_) @member.recv
  name: (name) @ref.member.call
  arguments: (arguments ((_) @arity.arg ","?)*) @arity.args) @expr.call
(member_access_expression
  object: (_) @member.recv
  name: (name) @ref.member.value) @expr.member
(nullsafe_member_access_expression
  object: (_) @member.recv
  name: (name) @ref.member.value) @expr.member
; `Foo::m()` calls the method of the class `Foo` names; `self::` and
; `static::` name the class whose method the call is written in. `parent::`
; dispatches above it, which is no named class.
(scoped_call_expression
  scope: [(name) (qualified_name)] @member.recv @receiver.class
  name: (name) @ref.member.call
  arguments: (arguments ((_) @arity.arg ","?)*) @arity.args) @expr.call
((scoped_call_expression
  scope: (relative_scope) @member.recv @receiver.enclosing
  name: (name) @ref.member.call
  arguments: (arguments ((_) @arity.arg ","?)*) @arity.args) @expr.call
 (#any-of? @receiver.enclosing "self" "static"))
; The class a static call names is a class reference.
(scoped_call_expression scope: (name) @ref.type)
(scoped_call_expression scope: (qualified_name (name) @ref.type) @ref.spelled)

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
