; Python language pack — same capture vocabulary, same driver, same
; engine. ~40 lines for outline + lexical typing through the
; production witness bag.

(class_definition
  name: (identifier) @def.class.name @context.package) @def.class @scope

; inheritance: `class Dog(Animal):` → Dog parent Animal (one @parent per
; base; member completion + method resolution walk the ancestors).
(class_definition
  name: (identifier) @def.class.name
  superclasses: (argument_list (identifier) @parent))

(function_definition
  name: (identifier) @def.sub.name) @def.sub

; docstrings: the first statement of a body IS the doc for the def that
; owns it, so the pair rides one match — the same join php's `/** */`
; siblings make, written for a language whose docs live INSIDE the def.
((function_definition
  body: (block . (expression_statement (string) @doc.comment))) @doc.subject)
((class_definition
  body: (block . (expression_statement (string) @doc.comment))) @doc.subject)

; `@scope.sub`: params/locals are sub-body content — shielded from the
; outline and the class-content lane (a method local carries the sticky
; class package; the Sub boundary is what marks it a local, not a member).
(function_definition) @scope.sub

; `self.attr = ...` → an instance attribute: a member of the enclosing
; class (the sticky @context.package tags it). THE dominant Python member
; idiom — without it `obj.` offers no data attributes.
(assignment
  left: (attribute attribute: (identifier) @def.var.name @def.var))

(parameters
  (identifier) @def.var.name @def.var)
; The method RECEIVER parameter: lexically inside the class body, so the
; sticky class context tags it — but it is the object, not a member. The
; capture is what the outline and member completion ask (the symbol carries
; the fact), and what witnesses the receiver as an instance of its class.
((parameters . (identifier) @param.receiver)
 (#any-of? @param.receiver "self" "cls"))

(import_statement
  name: (dotted_name) @import.name) @import
(import_from_statement
  module_name: (dotted_name) @import.name) @import
; What each row BINDS. `import a.b` binds the head package, which no token
; of the row spells on its own, so that row states no binding and the
; never-spelled lane stays silent on it. The rows that DO bind a name get
; one row apiece, spanning the bound token, so a report lands on the name
; the file never spells rather than on the module it came from.
(import_from_statement
  name: (dotted_name (identifier) @import.name @import.binds)) @import
(import_from_statement
  name: (aliased_import alias: (identifier) @import.name @import.binds)) @import
(import_statement
  name: (aliased_import name: (dotted_name) @import.name
    alias: (identifier) @import.binds)) @import

; Imported names are references to the remote def — this single
; pattern is what makes cross-file RENAME rewrite the import line too.
(import_from_statement
  name: (dotted_name (identifier) @ref.call))

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

(call
  function: (identifier) @ref.call) @expr.call
(call
  function: (attribute attribute: (identifier) @ref.method))
; `recv.attr` is python's member access; its `object:` is the receiver the
; cursor's member completion types.
(attribute object: (_) @member.recv)
; the attribute TOKEN names a member, not a local — without this the
; identifier read pattern below claims it and `self.x` reads as a variable
; `x` nothing declares.
(attribute attribute: (identifier) @var.member)
(identifier) @expr.read.var

(string) @expr.lit.string
(integer) @expr.lit.number
(float) @expr.lit.number
(list) @expr.lit.arrayref
(dictionary) @expr.lit.hashref

; Guard narrowing: `if isinstance(x, Foo): <body>` refines x to Foo
; inside the block. The guard name is the pattern's own `#eq?`; the type
; token names the refinement and core scopes it to @scope.
(if_statement
  condition: (call
    function: (identifier) @narrow.guard
    arguments: (argument_list (identifier) @narrow.var (identifier) @narrow.type))
  consequence: (block) @scope
  (#eq? @narrow.guard "isinstance"))

; ---- cursor-time shapes ----
; Where a cursor may not splice: a string or a comment is not code.
(string) @skip
(string_content) @skip
(concatenated_string) @skip
(comment) @skip
; A transparent receiver wrapper denotes the same value as its operand.
(parenthesized_expression) @recv.peel
