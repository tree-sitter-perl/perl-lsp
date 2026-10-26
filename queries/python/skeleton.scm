; Python language pack — same capture vocabulary, same driver, same
; engine. ~40 lines for outline + lexical typing through the
; production witness bag.

; A class is filed under its module's namespace; its body's members file
; under the class's identity.
(class_definition
  name: (identifier) @def.class.name @context.class) @def.class @scope

; inheritance: `class Dog(Animal):` → Dog parent Animal (one @parent per
; base; member completion + method resolution walk the ancestors). The
; written spelling resolves through the file's imports to the parent's
; identity, and is a class reference — a dotted one by its whole spelling,
; rewriting only its last name.
(class_definition
  name: (identifier) @def.class.name
  superclasses: (argument_list
    [(identifier) @parent @ref.type
     (attribute attribute: (identifier) @ref.type) @parent @ref.spelled]))

(function_definition
  name: (identifier) @def.sub.name) @def.sub

; `@scope.sub`: params/locals are sub-body content — shielded from the
; outline and the class-content lane (a method local carries the sticky
; class package; the Sub boundary is what marks it a local, not a member).
(function_definition) @scope.sub

; `self.attr = ...` → an instance attribute: a member of the enclosing
; class (the sticky @context.class tags it). THE dominant Python member
; idiom — without it `obj.` offers no data attributes.
(assignment
  left: (attribute attribute: (identifier) @def.var.name @def.var))

(parameters
  (identifier) @def.var.name @def.var)

; a method's leading `self` / `cls` is its receiver: the object, not a member
; of the class it is declared in.
((parameters . (identifier) @param.receiver)
  (#match? @param.receiver "^(self|cls)$"))

; ---- imports: the module a statement loads, and the names it binds ----
(import_statement
  name: [(dotted_name) @import.module
         (aliased_import name: (dotted_name) @import.module)]) @import
(import_from_statement
  module_name: (dotted_name) @import.module) @import
; `import a.b` binds its head `a`; `import a.b as c` binds `c` to `a.b`.
(import_statement
  name: (dotted_name . (identifier) @import.name))
(import_statement
  name: (aliased_import
    name: (dotted_name) @import.name
    alias: (identifier) @import.alias))
; `from m import C [as D]`: each name hangs off the module, a relative one
; (`from .m`, `from ..`) climbing from the file's package by its dots.
(import_from_statement
  module_name: [(dotted_name) @import.prefix
                (relative_import
                  (import_prefix) @import.relative
                  (dotted_name)? @import.prefix)]
  name: (dotted_name) @import.name)
(import_from_statement
  module_name: [(dotted_name) @import.prefix
                (relative_import
                  (import_prefix) @import.relative
                  (dotted_name)? @import.prefix)]
  name: (aliased_import
    name: (dotted_name) @import.name
    alias: (identifier) @import.alias))

; Imported names are references to the remote def — this single
; pattern is what makes cross-file RENAME rewrite the import line too. The
; def may be a class (#names-class?).
((import_from_statement
  name: (dotted_name (identifier) @ref.call))
 (#names-class? @ref.call))

; Assignment IS declaration in Python — the same identifier is both
; the def and the flow target.
(assignment
  left: (identifier) @def.var.name @def.var @flow.target
  right: (_) @flow.source) @flow.assign

; Annotated assignment: `x: int = ...` — ring 3 is partly IN the tree
; here; the annotation emits a direct type witness via the pack's
; annot_type predicate.
(assignment
  left: (identifier) @flow.target
  type: (type) @type.annot)

; `for x in items:` — the loop var rebinds per element (a Rebind, no inflowing
; type yet) so the narrowing cutoff ends a region at the loop.
(for_statement
  left: (identifier) @flow.rebind)

; ---- calls: a call's value is an Invoke of its callee on its arguments. The
; argument list rides the callee's match, and a starred argument spreads. A
; generator argument (`f(x for x in xs)`) is one value, not a list. Calling a
; class constructs it, so a callee that names a class is a reference to it
; (#names-class?, decided with the index in hand when the class is
; elsewhere). ----
((call
  function: (identifier) @ref.call
  arguments: [(argument_list ((_) @arity.arg ","?)*) @arity.args (generator_expression)]) @expr.call
 (#names-class? @ref.call))
(argument_list [(list_splat) (dictionary_splat)] @arity.spread)
(call
  function: (attribute attribute: (identifier) @ref.method))

; ---- member access (`recv.attr`, and `recv.m(...)`): the receiver subtree is
; the invocant, typed at query time through expr_type_at_span — the same
; MethodCall ref core resolves for Perl `$obj->m`. The document states the
; kind: an attribute that is a call's callee is a member call, any other is a
; value read. The read's value is the receiver's slot (@expr.member); the
; call's is an Invoke of the member on the receiver (@expr.call). ----
((attribute
  object: (_) @member.recv
  attribute: (identifier) @ref.member.value) @expr.member
 (#not-has-parent? @expr.member call))
(call
  function: (attribute
    object: (_) @member.recv
    attribute: (identifier) @ref.member.call)
  arguments: [(argument_list ((_) @arity.arg ","?)*) @arity.args (generator_expression)]) @expr.call
(identifier) @expr.read.var

(string) @expr.lit.string
(integer) @expr.lit.number
(float) @expr.lit.number
(list) @expr.lit.arrayref
(dictionary) @expr.lit.hashref

; Guard narrowing: `if isinstance(x, Foo): <body>` refines x to Foo
; inside the block. The pack's narrow_guard predicate maps the guard
; (isinstance) + type token to the refinement; core scopes it to @scope.
(if_statement
  condition: (call
    function: (identifier) @narrow.guard
    arguments: (argument_list (identifier) @narrow.var (identifier) @narrow.type))
  consequence: (block) @scope)
