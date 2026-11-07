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
; @sym.attr, whose spelling is the flag it mints. `@scope.direct` is PHP's
; class-body rule: a member is only ever named through a receiver
; (`$this->x`, `self::$x`, `static::f()`), never bare, so the body's names
; are not in force inside its methods — a method's bare `$x` or `f()` looks
; past the class.
(class_declaration
  (abstract_modifier)? @sym.attr
  name: (name) @def.class.name @context.class) @def.class @scope.direct
(interface_declaration
  "interface" @sym.attr
  name: (name) @def.class.name @context.class) @def.class @scope.direct
(trait_declaration
  "trait" @sym.attr
  name: (name) @def.class.name @context.class) @def.class @scope.direct

; ---- written types ----
; `self` names the class it is written in, and `parent` the class it
; extends, nullable or not, wherever a type is written: a return, a
; parameter, a property. `static` is late-bound: the class the call is made
; on, the class it is written in where no call says.
(([
  (named_type (name) @_class)
  (optional_type (named_type (name) @_class))
 ] @type.enclosing)
 (#eq? @_class "self"))
(([
  (named_type (name) @_class)
  (optional_type (named_type (name) @_class))
 ] @type.called)
 (#eq? @_class "static"))
(([
  (named_type (name) @_class)
  (optional_type (named_type (name) @_class))
 ] @type.super)
 (#eq? @_class "parent"))

; ---- callables ----
; A declared return is the callable's value at every call; without one,
; what its body returns is. A closure's `return` is the closure's own.
(function_definition name: (name) @def.sub.name return_type: [(named_type) (primitive_type) (optional_type) (union_type)]? @rettype) @def.sub
(method_declaration name: (name) @def.method.name return_type: [(named_type) (primitive_type) (optional_type) (union_type)]? @rettype) @def.method
(return_statement (_) @expr.return.value)
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
; `$` alone. Its declared type is its value from the binding site on. A
; variadic's declared type is its elements', so the local stays untyped.
(simple_parameter
  type: [(named_type) (primitive_type) (optional_type) (union_type)]? @type.annot
  name: (variable_name (name) @def.local.name @flow.target)) @def.local
(variadic_parameter
  name: (variable_name (name) @def.local.name)) @def.local

; ---- properties ----
; A property is a data member of its class, named without the `$` it is
; declared with: `$obj->x` reads the member `x`. The `$x` it is written as
; is the spelling it is listed by. A declared type is the member's value,
; a union the join of its arms; an intersection names no single type, so
; it is not captured. `readonly` is the attribute that makes the member
; write-once.
(property_declaration
  (readonly_modifier)? @sym.attr
  type: [(named_type) (primitive_type) (optional_type) (union_type)]? @type.annot
  (property_element
    name: (variable_name (name) @def.field.name @flow.target) @def.field.spelled
    default_value: (_)? @flow.source) @def.field)
; A static property lives apart from the instance ones: `$obj->x` and
; `Foo::$x` land in different namespaces.
(property_declaration
  (static_modifier)
  (property_element name: (variable_name (name) @space.static_property)))
; A class constant is a member of its own namespace: `const X` beside a
; property `$X` and a method `X()` is a third member. It never changes, so
; its value is the one it is declared with.
(declaration_list
  (const_declaration
    (const_element (name) @def.field.name @space.constant @flow.target . (_) @flow.source) @def.field))
; A constructor parameter with a visibility or `readonly` is also a
; property of the class (PHP 8 promotion); the member belongs to the class
; body, not to the constructor whose parameter list declares it.
; The same token is the constructor's local too, so the match declares
; the two as one (`@def.pair`): a rename of either rewrites both, and the
; declared type is the value of each. (A node holds at most three
; captures; tree-sitter drops the rest silently.)
(property_promotion_parameter
  readonly: (readonly_modifier)? @sym.attr
  type: [(named_type) (primitive_type) (optional_type) (union_type)]? @type.annot
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
; A `(set)` visibility says where the member may be written from, apart
; from where it may be read. A readonly member stating none is set by its
; class and the classes extending it.
((property_declaration
  (visibility_modifier "public" (operation) @_set)
  (property_element name: (variable_name (name) @mark.set_public)))
 (#eq? @_set "set"))
((property_declaration
  (visibility_modifier "protected" (operation) @_set)
  (property_element name: (variable_name (name) @mark.set_protected)))
 (#eq? @_set "set"))
((property_declaration
  (visibility_modifier "private" (operation) @_set)
  (property_element name: (variable_name (name) @mark.set_private)))
 (#eq? @_set "set"))
((property_promotion_parameter
  visibility: (visibility_modifier "public" (operation) @_set)
  name: [
    (variable_name (name) @mark.set_public)
    (by_ref (variable_name (name) @mark.set_public))
  ])
 (#eq? @_set "set"))
((property_promotion_parameter
  visibility: (visibility_modifier "protected" (operation) @_set)
  name: [
    (variable_name (name) @mark.set_protected)
    (by_ref (variable_name (name) @mark.set_protected))
  ])
 (#eq? @_set "set"))
((property_promotion_parameter
  visibility: (visibility_modifier "private" (operation) @_set)
  name: [
    (variable_name (name) @mark.set_private)
    (by_ref (variable_name (name) @mark.set_private))
  ])
 (#eq? @_set "set"))
(method_declaration (static_modifier) name: (name) @mark.static)
(property_declaration
  (static_modifier)
  (property_element name: (variable_name (name) @mark.static)))
(method_declaration (abstract_modifier) name: (name) @mark.abstract)
; Every property a `readonly class` declares is readonly, promoted ones too.
(class_declaration
  (readonly_modifier)
  body: (declaration_list
    (property_declaration
      (property_element name: (variable_name (name) @mark.readonly)))))
(class_declaration
  (readonly_modifier)
  body: (declaration_list
    (method_declaration
      parameters: (formal_parameters
        (property_promotion_parameter
          name: [
            (variable_name (name) @mark.readonly)
            (by_ref (variable_name (name) @mark.readonly))
          ])))))
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
; lookup; a class's `extends` is the one `parent` names. The whole written
; spelling is the capture: it resolves through the file's use-map to the
; parent's identity. The same name is a class reference — goto-def,
; references and rename reach it — and a qualified spelling references by
; the whole spelling while rewriting only its leaf.
(class_declaration
  name: (name) @def.class.name
  (base_clause
    [(name) @parent.super @ref.type
     (qualified_name (name) @ref.type) @parent.super @ref.spelled]))
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
; instance of the class the call is made on, and its name token refers to
; that object.
((variable_name (name) @ref.receiver) @receiver.called
 (#eq? @receiver.called "$this"))

; A literal is a value of its own kind, stated only where that value flows
; on: into a write, a return, an argument, a declared initializer or an
; operator. A literal nothing reads that way (an array element or key, a
; `case` label, a parameter's default) would cost a fact no query asks for.
([(integer) (float)] @expr.lit.number
 (#has-parent? @expr.lit.number assignment_expression augmented_assignment_expression
   return_statement argument const_element property_element
   static_variable_declaration binary_expression unary_op_expression))
([(string) (encapsed_string)] @expr.lit.string
 (#has-parent? @expr.lit.string assignment_expression augmented_assignment_expression
   return_statement argument const_element property_element
   static_variable_declaration binary_expression unary_op_expression))
((boolean) @expr.lit.bool
 (#has-parent? @expr.lit.bool assignment_expression augmented_assignment_expression
   return_statement argument const_element property_element
   static_variable_declaration binary_expression unary_op_expression))

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
; `??=` writes only while the place holds nothing: it initialises it.
(augmented_assignment_expression left: (_) @flow.init operator: "??=")
; An increment writes the place it reads, and an unset clears it.
(update_expression argument: (_) @flow.place)
(unset_statement (_) @flow.place)

; ---- closures ----
; `use ($x)` reads the variable of the code around the closure, and the
; closure body sees it by that name. `use (&$x)` binds it there,
; declaring it when nothing there does yet.
(anonymous_function_use_clause (variable_name (name) @closure.capture))
(anonymous_function_use_clause
  (by_ref (variable_name (name) @closure.capture @def.assigned.name @def.hoist)) @def.assigned)

; ---- guards ----
; An atom states what a test proves of its subject, a local `$x` or a
; property `$this->x`: `instanceof` that it is the class named, `isset`
; and `!== null` that it holds a value, `=== null` and `is_null` that it
; holds none. A multi-argument `isset` is a conjunction, so only a single
; argument is an atom. Loose `==` / `!=` prove nothing: `0 == null` holds.
; `self`, `static` and `parent` name a class by where they are written,
; which an atom's type does not read, so they narrow nothing.
((binary_expression
   left: [(variable_name (name) @narrow.var) (member_access_expression) @narrow.var]
   operator: "instanceof"
   right: [(name) (qualified_name)] @narrow.type) @narrow.is
 (#not-eq? @narrow.var "this")
 (#not-any-of? @narrow.type "self" "static" "parent"))
((function_call_expression
   function: (name) @_fn
   arguments: (arguments . (argument [(variable_name (name) @narrow.var) (member_access_expression) @narrow.var]) .)) @narrow.set
 (#eq? @_fn "isset")
 (#not-eq? @narrow.var "this"))
((binary_expression
   left: [(variable_name (name) @narrow.var) (member_access_expression) @narrow.var]
   operator: "!=="
   right: (null)) @narrow.set
 (#not-eq? @narrow.var "this"))
((binary_expression
   left: (null)
   operator: "!=="
   right: [(variable_name (name) @narrow.var) (member_access_expression) @narrow.var]) @narrow.set
 (#not-eq? @narrow.var "this"))
((binary_expression
   left: [(variable_name (name) @narrow.var) (member_access_expression) @narrow.var]
   operator: "==="
   right: (null)) @narrow.unset
 (#not-eq? @narrow.var "this"))
((binary_expression
   left: (null)
   operator: "==="
   right: [(variable_name (name) @narrow.var) (member_access_expression) @narrow.var]) @narrow.unset
 (#not-eq? @narrow.var "this"))
((function_call_expression
   function: (name) @_fn
   arguments: (arguments . (argument [(variable_name (name) @narrow.var) (member_access_expression) @narrow.var]) .)) @narrow.unset
 (#eq? @_fn "is_null")
 (#not-eq? @narrow.var "this"))
; A condition built of atoms decides them through its connectives.
(unary_op_expression operator: "!") @narrow.not
(binary_expression operator: ["&&" "and"]) @narrow.and
(binary_expression operator: ["||" "or"]) @narrow.or
(parenthesized_expression) @narrow.group
; Where a condition decides what runs: an `if` or `elseif` body and a
; loop body run where it holds, every later clause of the chain where it
; fails; a ternary yields its body or its alternative; the right of `&&`
; runs where the left holds, of `||` where it fails. An `if` whose body
; only leaves the block, with no clause after it, leaves the rest of the
; block to run where its condition fails.
(if_statement condition: (_) @narrow.cond body: (_) @narrow.then)
(if_statement condition: (_) @narrow.cond alternative: (_) @narrow.else)
(else_if_clause condition: (_) @narrow.cond body: (_) @narrow.then)
(if_statement
  alternative: (else_if_clause condition: (_) @narrow.cond)
  alternative: (_) @narrow.else)
(while_statement condition: (_) @narrow.cond body: (_) @narrow.then)
(conditional_expression condition: (_) @narrow.cond body: (_) @narrow.then)
(conditional_expression condition: (_) @narrow.cond alternative: (_) @narrow.else)
(binary_expression left: (_) @narrow.cond operator: ["&&" "and"] right: (_) @narrow.then)
(binary_expression left: (_) @narrow.cond operator: ["||" "or"] right: (_) @narrow.else)
(if_statement
  condition: (_) @narrow.cond
  body: [
    (return_statement)
    (continue_statement)
    (break_statement)
    (exit_statement)
    (expression_statement (throw_expression))
    (compound_statement
      . [(return_statement)
         (continue_statement)
         (break_statement)
         (exit_statement)
         (expression_statement (throw_expression))] .)]
  !alternative) @narrow.exit

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
  "->" @member.op
  name: (name) @ref.member.call
  arguments: (arguments ((_) @arity.arg ","?)*) @arity.args) @expr.call
(nullsafe_member_call_expression
  object: (_) @member.recv
  "?->" @member.op
  name: (name) @ref.member.call
  arguments: (arguments ((_) @arity.arg ","?)*) @arity.args) @expr.call
(member_access_expression
  object: (_) @member.recv
  "->" @member.op
  name: (name) @ref.member.value) @expr.member
(nullsafe_member_access_expression
  object: (_) @member.recv
  "?->" @member.op
  name: (name) @ref.member.value) @expr.member
; `->` reads an instance property; finding a static one there is PHP's
; error, so it is looked for only when no instance property answers.
(member_access_expression name: (name) @space.property @space.static_property.illegal)
(nullsafe_member_access_expression name: (name) @space.property @space.static_property.illegal)
; `Foo::$x` reads a static property of the class `Foo` names (`self::`
; the class the code is written in, `static::` the class the call is made
; on, `parent::` the one it extends, `$obj::` the object's class), named
; without its `$`; finding an instance property there is PHP's error.
(scoped_property_access_expression
  scope: [(name) (qualified_name)] @member.recv @receiver.class
  "::" @member.op
  name: (variable_name (name) @ref.member.value)) @expr.member
((scoped_property_access_expression
  scope: (relative_scope) @member.recv @receiver.enclosing
  "::" @member.op
  name: (variable_name (name) @ref.member.value)) @expr.member
 (#eq? @receiver.enclosing "self"))
((scoped_property_access_expression
  scope: (relative_scope) @member.recv @receiver.called
  "::" @member.op
  name: (variable_name (name) @ref.member.value)) @expr.member
 (#eq? @receiver.called "static"))
((scoped_property_access_expression
  scope: (relative_scope) @member.recv @receiver.super
  "::" @member.op
  name: (variable_name (name) @ref.member.value)) @expr.member
 (#eq? @receiver.super "parent"))
(scoped_property_access_expression
  scope: (variable_name) @member.recv
  "::" @member.op
  name: (variable_name (name) @ref.member.value)) @expr.member
(scoped_property_access_expression
  name: (variable_name (name) @space.static_property @space.property.illegal))
; `Foo::X` reads a class constant, through the same receivers.
((class_constant_access_expression
   . [(name) (qualified_name)] @member.recv @receiver.class
   "::" @member.op
   . (name) @ref.member.value @space.constant) @expr.member
 (#not-eq? @ref.member.value "class"))
((class_constant_access_expression
   . (relative_scope) @member.recv @receiver.enclosing
   "::" @member.op
   . (name) @ref.member.value @space.constant) @expr.member
 (#eq? @receiver.enclosing "self")
 (#not-eq? @ref.member.value "class"))
((class_constant_access_expression
   . (relative_scope) @member.recv @receiver.called
   "::" @member.op
   . (name) @ref.member.value @space.constant) @expr.member
 (#eq? @receiver.called "static")
 (#not-eq? @ref.member.value "class"))
((class_constant_access_expression
   . (relative_scope) @member.recv @receiver.super
   "::" @member.op
   . (name) @ref.member.value @space.constant) @expr.member
 (#eq? @receiver.super "parent")
 (#not-eq? @ref.member.value "class"))
((class_constant_access_expression
   . (variable_name) @member.recv
   "::" @member.op
   . (name) @ref.member.value @space.constant) @expr.member
 (#not-eq? @ref.member.value "class"))
; `Foo::m()` calls the method of the class `Foo` names; `self::` names the
; class whose method the call is written in, `static::` the class the call
; running that method is made on, `parent::` the class it extends. A call
; through `self::` or `parent::` forwards: it runs on the calling method's
; own object, so a `static` in the method it reaches is still the class
; the outer call was made on.
(scoped_call_expression
  scope: [(name) (qualified_name)] @member.recv @receiver.class
  "::" @member.op
  name: (name) @ref.member.call
  arguments: (arguments ((_) @arity.arg ","?)*) @arity.args) @expr.call
((scoped_call_expression
  scope: (relative_scope) @member.recv @member.forward @receiver.enclosing
  "::" @member.op
  name: (name) @ref.member.call
  arguments: (arguments ((_) @arity.arg ","?)*) @arity.args) @expr.call
 (#eq? @receiver.enclosing "self"))
((scoped_call_expression
  scope: (relative_scope) @member.recv @receiver.called
  "::" @member.op
  name: (name) @ref.member.call
  arguments: (arguments ((_) @arity.arg ","?)*) @arity.args) @expr.call
 (#eq? @receiver.called "static"))
((scoped_call_expression
  scope: (relative_scope) @member.recv @member.forward @receiver.super
  "::" @member.op
  name: (name) @ref.member.call
  arguments: (arguments ((_) @arity.arg ","?)*) @arity.args) @expr.call
 (#eq? @receiver.super "parent"))
; `$cls::m()` calls the method of whatever class the variable holds.
(scoped_call_expression
  scope: (variable_name) @member.recv
  "::" @member.op
  name: (name) @ref.member.call
  arguments: (arguments ((_) @arity.arg ","?)*) @arity.args) @expr.call
; The class a static call names is a class reference.
(scoped_call_expression scope: (name) @ref.type)
(scoped_call_expression scope: (qualified_name (name) @ref.type) @ref.spelled)

; What an operator reaches: after `->` or `?->` one of the object's methods
; or properties, after `::` one of the class's methods or constants, or
; `class`, the class itself (`Foo::class`). A static property is written
; `::$x`, its own form. Completion offers what the operator reaches.
"->" @member.op.arrow @space.method @space.property
"?->" @member.op.arrow @space.method @space.property
("::" @space.method @space.constant
 (#set! member.word "class"))

; ---- construction ----
; `new Foo(…)` calls the constructor of the class `Foo` names on its
; arguments, and its value is an instance of that class; with no argument
; list it passes none. The class token is a reference to the class. `self`
; names the class the method is written in, `static` the class the call
; running it is made on, `parent` the class it extends, and `new $cls`
; constructs whatever class the variable holds.
((object_creation_expression
   . (name) @member.recv @receiver.class @ref.type
   (arguments ((_) @arity.arg ","?)*)?) @expr.construct @arity.args
 (#not-any-of? @receiver.class "self" "static" "parent"))
(object_creation_expression
  . (qualified_name (name) @ref.type) @member.recv @receiver.class @ref.spelled
  (arguments ((_) @arity.arg ","?)*)?) @expr.construct @arity.args
((object_creation_expression
   . (name) @member.recv @receiver.enclosing
   (arguments ((_) @arity.arg ","?)*)?) @expr.construct @arity.args
 (#eq? @receiver.enclosing "self"))
((object_creation_expression
   . (name) @member.recv @receiver.called
   (arguments ((_) @arity.arg ","?)*)?) @expr.construct @arity.args
 (#eq? @receiver.called "static"))
((object_creation_expression
   . (name) @member.recv @receiver.super
   (arguments ((_) @arity.arg ","?)*)?) @expr.construct @arity.args
 (#eq? @receiver.super "parent"))
(object_creation_expression
  . (variable_name) @member.recv
  (arguments ((_) @arity.arg ","?)*)?) @expr.construct @arity.args
; `new class(…) {…}` declares a class with no name, one per place it is
; written, and constructs it there: its `class` keyword anchors both. Its
; parents and its body are a class's, its body's names in force only for
; code written directly in it, as a named class's are.
(anonymous_class "class" @def.class.anchor @context.class) @def.class @scope.direct
(anonymous_class
  "class" @def.class.anchor
  (base_clause
    [(name) @parent.super @ref.type
     (qualified_name (name) @ref.type) @parent.super @ref.spelled]))
(anonymous_class
  "class" @def.class.anchor
  (class_interface_clause
    [(name) @parent @ref.type
     (qualified_name (name) @ref.type) @parent @ref.spelled]))
(anonymous_class
  "class" @def.class.anchor
  body: (declaration_list
    (use_declaration
      [(name) @parent @ref.type
       (qualified_name (name) @ref.type) @parent @ref.spelled])))
(object_creation_expression
  (anonymous_class
    "class" @member.recv @receiver.declared
    (arguments ((_) @arity.arg ","?)*)?)) @expr.construct @arity.args

; ---- class literals ----
; `Foo::class` is the class `Foo` names, as a value; `self::class` is the
; class the method is written in, `static::class` the class the call
; running it is made on, `parent::class` the class it extends. The class
; token
; of a class literal and of a class constant `Foo::BAR` is a class
; reference.
((class_constant_access_expression
   . [(name) (qualified_name)] @receiver.class
   . (name) @_member) @expr.class
 (#eq? @_member "class"))
((class_constant_access_expression
   . (relative_scope) @receiver.enclosing
   . (name) @_member) @expr.class
 (#eq? @receiver.enclosing "self")
 (#eq? @_member "class"))
((class_constant_access_expression
   . (relative_scope) @receiver.called
   . (name) @_member) @expr.class
 (#eq? @receiver.called "static")
 (#eq? @_member "class"))
((class_constant_access_expression
   . (relative_scope) @receiver.super
   . (name) @_member) @expr.class
 (#eq? @receiver.super "parent")
 (#eq? @_member "class"))
(class_constant_access_expression . (name) @ref.type)
(class_constant_access_expression . (qualified_name (name) @ref.type) @ref.spelled)

; ---- the use-map ----
; A row binds in the table its keyword names: none the class table,
; `function` the function table, `const` the constant table, so a class and
; a function imported under one name never meet. A group's keyword covers
; every clause in it; a clause outside a group, or in a group without one,
; carries its own. The `.` anchor pins the name to the clause's first named
; child, so the alias node never matches as a second name. An imported
; class's leaf references the class the row names, so a rename of the class
; rewrites the `use` line too; the ref is the leaf alone, since the row it
; sits in already names the class in full (`spelled_identity`). An imported
; function's leaf references the function the row names the same way.
(namespace_use_declaration
  (namespace_use_clause
    !type
    . [(name) @import.name @ref.type
       (qualified_name (name) @ref.type) @import.name]
    alias: (name)? @import.alias))
(namespace_use_declaration
  (namespace_use_clause
    type: "function" @import.function
    . [(name) @import.name @ref.import
       (qualified_name (name) @ref.import) @import.name]
    alias: (name)? @import.alias))
(namespace_use_declaration
  (namespace_use_clause
    type: "const" @import.constant
    . [(name) (qualified_name)] @import.name
    alias: (name)? @import.alias))
; `use A\B\{C, D as E}`: each clause hangs off the shared prefix.
(namespace_use_declaration
  !type
  (namespace_name) @import.prefix
  body: (namespace_use_group
    (namespace_use_clause
      !type
      . (name) @import.name @ref.type
      alias: (name)? @import.alias)))
(namespace_use_declaration
  type: "function" @import.function
  (namespace_name) @import.prefix
  body: (namespace_use_group
    (namespace_use_clause
      . (name) @import.name @ref.import
      alias: (name)? @import.alias)))
(namespace_use_declaration
  type: "const" @import.constant
  (namespace_name) @import.prefix
  body: (namespace_use_group
    (namespace_use_clause
      . (name) @import.name
      alias: (name)? @import.alias)))
(namespace_use_declaration
  !type
  (namespace_name) @import.prefix
  body: (namespace_use_group
    (namespace_use_clause
      type: "function" @import.function
      . (name) @import.name @ref.import
      alias: (name)? @import.alias)))
(namespace_use_declaration
  !type
  (namespace_name) @import.prefix
  body: (namespace_use_group
    (namespace_use_clause
      type: "const" @import.constant
      . (name) @import.name
      alias: (name)? @import.alias)))
