; Python language pack — same capture vocabulary, same driver, same
; engine. ~40 lines for outline + lexical typing through the
; production witness bag.

; A class is filed under its home — the class whose scope it is written
; in (`pkg.mod.Outer.Inner`), else its module; its body's members file
; under the class's identity. `@scope.direct` is Python's class-scope rule:
; the body's names, nested classes included, are in force for the code
; written directly in it, so a method of the class, or a class nested in
; it, looks a bare name up past the class.
(class_definition
  name: (identifier) @def.class.name @context.class) @def.class @scope.direct

; A def's header runs where the def is written: a class's base list, a
; function's parameter annotations and defaults and its `-> T` are code of
; the scope around the def, so a method's `x: Inner = Inner()` names the
; class body's `Inner`. The parameter names still bind in the function.
(class_definition superclasses: (argument_list) @scope.header)
(function_definition parameters: (parameters) @scope.header)
(function_definition return_type: (type) @scope.header)

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

; A def's `-> T` is its declared return; without one, what it returns is.
(function_definition
  name: (identifier) @def.sub.name
  return_type: (type)? @rettype) @def.sub

; The method calling the class runs; its name belongs to the language.
((class_definition
  body: (block
    [(function_definition name: (identifier) @mark.constructor)
     (decorated_definition
       definition: (function_definition name: (identifier) @mark.constructor))]))
 (#eq? @mark.constructor "__init__"))

; `@scope.sub`: params/locals are sub-body content — shielded from the
; outline and the class-content lane (a method local carries the sticky
; class package; the Sub boundary is what marks it a local, not a member).
(function_definition) @scope.sub

; A name a class body binds — `count = 0`, `a: int`, `b: int = 5` — is two
; things from one token: a data member of the class (`Counter.count`) and a
; name in the class scope (code written directly in the body reads it
; bare). The match declares the two as one (`@def.pair`), so a rename of
; either rewrites both. Its value is the assignment's flow target below,
; typed by an annotation where there is one.
(class_definition
  body: (block
    (expression_statement
      (assignment
        left: (identifier) @def.field.name @def.var.name @def.pair) @def.field @def.var)))

; A class-body binding with a value is the class's own attribute, read
; through the class itself; a bare annotation declares an instance's.
(class_definition
  body: (block
    (expression_statement
      (assignment left: (identifier) @mark.static right: (_)))))

; `self.attr = ...` → an instance slot of the class the receiver is typed
; as (`self`, or an annotated `other: Counter`), declared by a write only
; when neither the class nor an ancestor declares the name already. THE
; dominant Python member idiom — without it `obj.` offers no data
; attributes. It is no bare name anywhere; a later write, or one through an
; untyped receiver, is a use of it.
(assignment
  left: (attribute
    object: (identifier) @def.slot.receiver
    attribute: (identifier) @def.slot.name) @def.slot)

; An assignment writes the place its left side denotes, whatever that is; a
; compound one writes it too, with a value it does not name.
(assignment
  left: (_) @flow.place
  right: (_) @flow.source) @flow.assign
(augmented_assignment left: (_) @flow.place)

(parameters
  (identifier) @def.var.name @def.var)
(typed_parameter . (identifier) @def.var.name @def.var)
(default_parameter name: (identifier) @def.var.name @def.var)
(typed_default_parameter name: (identifier) @def.var.name @def.var)

; An annotated parameter holds a value of its annotation, as an annotated
; assignment's target does.
(typed_parameter . (identifier) @flow.target type: (type) @type.annot)
(typed_default_parameter name: (identifier) @flow.target type: (type) @type.annot)

; An annotation names a class: a dotted one by its whole spelling,
; rewriting only its last name.
(type (identifier) @ref.type)
(type (attribute attribute: (identifier) @ref.type) @ref.spelled)

; A method's receiver is its first parameter, whatever it is called: the
; object (the class, for a classmethod), not a member of the class. A method
; is a `def` written directly in a class body; a `@staticmethod` has none.
(class_definition
  body: (block
    (function_definition
      parameters: (parameters
        . [(identifier) @receiver (typed_parameter . (identifier) @receiver)]))))
((class_definition
  body: (block
    (decorated_definition
      (decorator)+ @_decorators
      definition: (function_definition
        parameters: (parameters
          . [(identifier) @receiver (typed_parameter . (identifier) @receiver)])))))
 (#not-match? @_decorators "^@staticmethod$"))

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

; ---- search roots: a directory a statement adds to `sys.path` ----
; `sys.path.insert(i, D)` / `sys.path.append(D)` where D is a literal path
; (relative to the working directory) or a path under the file's own
; directory: `os.path.dirname(__file__)`, alone or joined with up to three
; literal segments, either wrapped in `abspath`/`realpath`. The segments are
; spelled one pattern per count: a quantified segment under the anchors
; that keep an f-string out does not repeat. A computed path matches
; nothing.
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (string . (string_start) . (string_content) @root.path . (string_end) .) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append")) @root
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (call function: (attribute attribute: (identifier) @_dirname) arguments: (argument_list . (identifier) @root.file_dir .)) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append") (#eq? @_dirname "dirname") (#eq? @root.file_dir "__file__")) @root
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (call function: (attribute attribute: (identifier) @_vw) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_dirname) arguments: (argument_list . (identifier) @root.file_dir .)) .)) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append") (#eq? @_dirname "dirname") (#eq? @root.file_dir "__file__") (#any-of? @_vw "abspath" "realpath" "normpath")) @root
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (call function: (attribute attribute: (identifier) @_join) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_dirname) arguments: (argument_list . (identifier) @root.file_dir .)) . (string . (string_start) . (string_content) @root.path . (string_end) .) .)) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append") (#eq? @_dirname "dirname") (#eq? @root.file_dir "__file__") (#eq? @_join "join")) @root
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (call function: (attribute attribute: (identifier) @_vw) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_join) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_dirname) arguments: (argument_list . (identifier) @root.file_dir .)) . (string . (string_start) . (string_content) @root.path . (string_end) .) .)) .)) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append") (#eq? @_dirname "dirname") (#eq? @root.file_dir "__file__") (#eq? @_join "join") (#any-of? @_vw "abspath" "realpath" "normpath")) @root
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (call function: (attribute attribute: (identifier) @_join) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_dirname) arguments: (argument_list . (identifier) @root.file_dir .)) . (string . (string_start) . (string_content) @root.path . (string_end) .) . (string . (string_start) . (string_content) @root.path . (string_end) .) .)) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append") (#eq? @_dirname "dirname") (#eq? @root.file_dir "__file__") (#eq? @_join "join")) @root
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (call function: (attribute attribute: (identifier) @_vw) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_join) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_dirname) arguments: (argument_list . (identifier) @root.file_dir .)) . (string . (string_start) . (string_content) @root.path . (string_end) .) . (string . (string_start) . (string_content) @root.path . (string_end) .) .)) .)) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append") (#eq? @_dirname "dirname") (#eq? @root.file_dir "__file__") (#eq? @_join "join") (#any-of? @_vw "abspath" "realpath" "normpath")) @root
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (call function: (attribute attribute: (identifier) @_join) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_dirname) arguments: (argument_list . (identifier) @root.file_dir .)) . (string . (string_start) . (string_content) @root.path . (string_end) .) . (string . (string_start) . (string_content) @root.path . (string_end) .) . (string . (string_start) . (string_content) @root.path . (string_end) .) .)) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append") (#eq? @_dirname "dirname") (#eq? @root.file_dir "__file__") (#eq? @_join "join")) @root
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (call function: (attribute attribute: (identifier) @_vw) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_join) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_dirname) arguments: (argument_list . (identifier) @root.file_dir .)) . (string . (string_start) . (string_content) @root.path . (string_end) .) . (string . (string_start) . (string_content) @root.path . (string_end) .) . (string . (string_start) . (string_content) @root.path . (string_end) .) .)) .)) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append") (#eq? @_dirname "dirname") (#eq? @root.file_dir "__file__") (#eq? @_join "join") (#any-of? @_vw "abspath" "realpath" "normpath")) @root
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (call function: (attribute attribute: (identifier) @_dirname) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_fw) arguments: (argument_list . (identifier) @root.file_dir .)) .)) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append") (#eq? @_dirname "dirname") (#eq? @root.file_dir "__file__") (#any-of? @_fw "abspath" "realpath")) @root
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (call function: (attribute attribute: (identifier) @_vw) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_dirname) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_fw) arguments: (argument_list . (identifier) @root.file_dir .)) .)) .)) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append") (#eq? @_dirname "dirname") (#eq? @root.file_dir "__file__") (#any-of? @_fw "abspath" "realpath") (#any-of? @_vw "abspath" "realpath" "normpath")) @root
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (call function: (attribute attribute: (identifier) @_join) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_dirname) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_fw) arguments: (argument_list . (identifier) @root.file_dir .)) .)) . (string . (string_start) . (string_content) @root.path . (string_end) .) .)) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append") (#eq? @_dirname "dirname") (#eq? @root.file_dir "__file__") (#any-of? @_fw "abspath" "realpath") (#eq? @_join "join")) @root
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (call function: (attribute attribute: (identifier) @_vw) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_join) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_dirname) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_fw) arguments: (argument_list . (identifier) @root.file_dir .)) .)) . (string . (string_start) . (string_content) @root.path . (string_end) .) .)) .)) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append") (#eq? @_dirname "dirname") (#eq? @root.file_dir "__file__") (#any-of? @_fw "abspath" "realpath") (#eq? @_join "join") (#any-of? @_vw "abspath" "realpath" "normpath")) @root
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (call function: (attribute attribute: (identifier) @_join) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_dirname) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_fw) arguments: (argument_list . (identifier) @root.file_dir .)) .)) . (string . (string_start) . (string_content) @root.path . (string_end) .) . (string . (string_start) . (string_content) @root.path . (string_end) .) .)) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append") (#eq? @_dirname "dirname") (#eq? @root.file_dir "__file__") (#any-of? @_fw "abspath" "realpath") (#eq? @_join "join")) @root
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (call function: (attribute attribute: (identifier) @_vw) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_join) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_dirname) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_fw) arguments: (argument_list . (identifier) @root.file_dir .)) .)) . (string . (string_start) . (string_content) @root.path . (string_end) .) . (string . (string_start) . (string_content) @root.path . (string_end) .) .)) .)) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append") (#eq? @_dirname "dirname") (#eq? @root.file_dir "__file__") (#any-of? @_fw "abspath" "realpath") (#eq? @_join "join") (#any-of? @_vw "abspath" "realpath" "normpath")) @root
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (call function: (attribute attribute: (identifier) @_join) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_dirname) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_fw) arguments: (argument_list . (identifier) @root.file_dir .)) .)) . (string . (string_start) . (string_content) @root.path . (string_end) .) . (string . (string_start) . (string_content) @root.path . (string_end) .) . (string . (string_start) . (string_content) @root.path . (string_end) .) .)) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append") (#eq? @_dirname "dirname") (#eq? @root.file_dir "__file__") (#any-of? @_fw "abspath" "realpath") (#eq? @_join "join")) @root
(call
  function: (attribute
    object: (attribute object: (identifier) @_sys attribute: (identifier) @_syspath)
    attribute: (identifier) @_verb)
  arguments: (argument_list (call function: (attribute attribute: (identifier) @_vw) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_join) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_dirname) arguments: (argument_list . (call function: (attribute attribute: (identifier) @_fw) arguments: (argument_list . (identifier) @root.file_dir .)) .)) . (string . (string_start) . (string_content) @root.path . (string_end) .) . (string . (string_start) . (string_content) @root.path . (string_end) .) . (string . (string_start) . (string_content) @root.path . (string_end) .) .)) .)) .)
  (#eq? @_sys "sys") (#eq? @_syspath "path") (#any-of? @_verb "insert" "append") (#eq? @_dirname "dirname") (#eq? @root.file_dir "__file__") (#any-of? @_fw "abspath" "realpath") (#eq? @_join "join") (#any-of? @_vw "abspath" "realpath" "normpath")) @root

; Imported names are references to the remote def — this single
; pattern is what makes cross-file RENAME rewrite the import line too.
(import_from_statement
  name: (dotted_name (identifier) @ref.import))

; A variable belongs to the function it is assigned in: the first write
; declares it, and every later write and every read in the body is that
; one variable. A write lands once the assignment has read what it reads.
(assignment
  left: (identifier) @def.assigned.name @flow.target
  right: (_) @flow.source) @def.assigned @flow.assign

; Annotated assignment: `x: int = ...` — ring 3 is partly IN the tree
; here; the annotation emits a direct type witness via the pack's
; annot_type predicate.
(assignment
  left: (identifier) @flow.target
  type: (type) @type.annot) @flow.assign

; `for x in items:` — the loop var rebinds per element (a Rebind, no inflowing
; type yet) so the narrowing cutoff ends a region at the loop.
(for_statement
  left: (identifier) @flow.rebind)

; ---- calls: a call's value is an Invoke of its callee on its arguments. The
; argument list rides the callee's match, and a starred argument spreads. A
; generator argument (`f(x for x in xs)`) is one value, not a list. ----
(call
  function: (identifier) @ref.call
  arguments: [(argument_list ((_) @arity.arg ","?)*) @arity.args (generator_expression)]) @expr.call
(argument_list [(list_splat) (dictionary_splat)] @arity.spread)
(call
  function: (attribute attribute: (identifier) @ref.method))

; ---- member access (`recv.attr`, and `recv.m(...)`): the receiver subtree is
; the invocant, typed at query time through expr_type_at_span — the same
; MethodCall ref core resolves for Perl `$obj->m`. The document states the
; kind: an attribute that is a call's callee is a member call, any other is a
; value read. The read's value is the receiver's slot (@expr.member); the
; call's is an Invoke of the member on the receiver (@expr.call). A receiver
; written as a bare name is @member.recv.name (it may spell a class); any
; other is an expression, @member.recv. ----
((attribute
  object: (identifier) @member.recv.name
  attribute: (identifier) @ref.member.value) @expr.member
 (#not-has-parent? @expr.member call))
((attribute
  object: (_) @member.recv
  attribute: (identifier) @ref.member.value) @expr.member
 (#not-has-parent? @expr.member call)
 (#not-is-kind? @member.recv identifier))
(call
  function: (attribute
    object: (identifier) @member.recv.name
    attribute: (identifier) @ref.member.call)
  arguments: [(argument_list ((_) @arity.arg ","?)*) @arity.args (generator_expression)]) @expr.call
((call
  function: (attribute
    object: (_) @member.recv
    attribute: (identifier) @ref.member.call)
  arguments: [(argument_list ((_) @arity.arg ","?)*) @arity.args (generator_expression)]) @expr.call
 (#not-is-kind? @member.recv identifier))
; A bare name read. An attribute's name (`count` in `o.count`) is the member
; read above, never a bare name; its receiver is.
((identifier) @expr.read.var
 (#not-has-parent? @expr.read.var attribute))
(attribute object: (identifier) @expr.read.var)

; Parentheses hold the value they wrap.
((parenthesized_expression (_) @expr.wrapper.inner) @expr.wrapper
 (#not-is-kind? @expr.wrapper.inner comment))
(string) @expr.lit.string
(integer) @expr.lit.number
(float) @expr.lit.number
(list) @expr.lit.arrayref
(dictionary) @expr.lit.hashref

; Guard narrowing: `isinstance(x, Foo)` refines x to Foo over the arm it
; enters, tagged `@narrow.arm` — an `if`/`elif` body, the value a conditional
; expression yields when the test holds, the right of `and`. The pack's
; narrow_guard maps the guard + type token to the refinement; core attaches
; it in the guard's scope over the arm (an `if` body is no scope in Python).
(if_statement
  condition: (call
    function: (identifier) @narrow.guard
    arguments: (argument_list (identifier) @narrow.var (identifier) @narrow.type))
  consequence: (block) @narrow.arm)
(elif_clause
  condition: (call
    function: (identifier) @narrow.guard
    arguments: (argument_list (identifier) @narrow.var (identifier) @narrow.type))
  consequence: (block) @narrow.arm)
(conditional_expression
  (_) @narrow.arm
  .
  "if"
  .
  (call
    function: (identifier) @narrow.guard
    arguments: (argument_list (identifier) @narrow.var (identifier) @narrow.type)))
(boolean_operator
  left: (call
    function: (identifier) @narrow.guard
    arguments: (argument_list (identifier) @narrow.var (identifier) @narrow.type))
  operator: "and"
  right: (_) @narrow.arm)

; `return EXPR` — the returned expression carries its own witness; this marks
; the site, an arm of the enclosing def's return when the def declares none.
(return_statement (_) @expr.return.value)
