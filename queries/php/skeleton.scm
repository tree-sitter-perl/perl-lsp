; PHP language pack — same capture vocabulary, same driver, same engine.
; Grammar: tree-sitter-php's `php` flavor (handles `<?php` tags + embedded
; HTML as inert `text` nodes, so mixed template files degrade safely).
;
; PHP reads like Perl with the sigils sanded down: `$var` everywhere,
; classes as packages, `->` dispatch, `::` static dispatch, arrays as
; ordered hash maps, and a DISTINCT string-concat operator (`.`) that
; leaks operand types at usage sites exactly like Perl's — the @obs arms
; below are the Perl edge no other pack language has had.

; ---- namespaces: flat sticky context, Perl's `package Foo;` shape ----
(namespace_definition
  name: (namespace_name) @def.package.name @context.package) @def.package

; ---- type containers: class / interface / trait / enum ----
; All four are @def.class + their own body context — members tag with the
; container's (unqualified) name, the identity the engine keys dispatch by.
(class_declaration
  name: (name) @def.class.name @context.package) @def.class @scope
(interface_declaration
  name: (name) @def.class.name @context.package) @def.class @scope
(trait_declaration
  name: (name) @def.class.name @context.package) @def.class @scope
(enum_declaration
  name: (name) @def.class.name @context.package) @def.class @scope

; inheritance: `extends Base` — one @parent per base; the name is also a
; live type use (goto-def on the base rides the PackageRef lane). Every
; clause has a qualified sibling (`extends \App\Base`, `use
; Concerns\HasAttributes`) whose LEAF is the identity classes key by.
(class_declaration
  name: (name) @def.class.name
  (base_clause (name) @parent @ref.type))
(class_declaration
  name: (name) @def.class.name
  (base_clause (qualified_name (name) @parent @ref.type) @parent.fq))
(interface_declaration
  name: (name) @def.class.name
  (base_clause (name) @parent @ref.type))
(interface_declaration
  name: (name) @def.class.name
  (base_clause (qualified_name (name) @parent @ref.type) @parent.fq))
; `implements Contract` — an interface is a parent for method-resolution
; purposes (the contract's declarations answer hover/completion).
(class_declaration
  name: (name) @def.class.name
  (class_interface_clause (name) @parent @ref.type))
(class_declaration
  name: (name) @def.class.name
  (class_interface_clause (qualified_name (name) @parent @ref.type) @parent.fq))
; `use SomeTrait;` inside a class body — trait methods resolve through the
; same ancestor walk (PHP flattening ≈ role composition ≈ parent edge).
(class_declaration
  name: (name) @def.class.name
  body: (declaration_list (use_declaration (name) @parent @ref.type)))
(class_declaration
  name: (name) @def.class.name
  body: (declaration_list (use_declaration (qualified_name (name) @parent @ref.type) @parent.fq)))

; enum cases: real enumerators — parent-enum typing + container tagging
; come generically from the engine's enumerator lane.
(enum_case
  name: (name) @def.enumerator.name) @def.enumerator

; ---- callables ----
; @rettype carries the declared return type → method-return chaining
; through PackageSymbol, same chase Perl and C++ use.
(function_definition
  name: (name) @def.sub.name
  return_type: (_)? @rettype) @def.sub
(method_declaration
  name: (name) @def.method.name
  return_type: (_)? @rettype) @def.method

; sub-body content is shielded from outline + class-content (a method
; local carries the sticky class package; the Sub boundary marks it local).
(function_definition) @scope.sub
(method_declaration) @scope.sub
(anonymous_function) @def.anon @scope.sub
(arrow_function) @def.anon @scope.sub

; declared-parameter arity: overload-family ranking fuel (a call's written
; arg count floats the fitting signature above a same-named stub).
(formal_parameters) @arity.sig

; docblocks: the pack's `doc_types` parses `@return`/`@param`/`@var` out of
; the comment; the engine joins each to the def directly below. Declared
; types win — the doc lane fills only what the syntax left untyped.
(comment) @doc.comment

; ---- properties: class data members, typed ----
; The field keys SIGIL-LESS (the inner name token): declared `$name`,
; accessed `$this->name` — the access site drops the `$`, so a sigil-ful
; symbol would never join its own uses (and the class-content gate
; rightly reads sigils as Perl shapes). kind "field" → class-wide type
; extent (member lookup is not sequential).
(property_declaration
  type: (_) @type.annot
  (property_element name: (variable_name (name) @def.field.name @def.field @flow.target)))
(property_declaration
  (property_element name: (variable_name (name) @def.field.name @def.field)))
; PHP 8 constructor promotion: `__construct(private string $name)` declares
; BOTH the property (sigil-less member) and the ctor-body local (`$name`).
(property_promotion_parameter
  type: (_) @type.annot
  name: (variable_name (name) @def.field.name @def.field @flow.target))
(property_promotion_parameter
  name: (variable_name) @def.var.name @def.var)

; class constants: `const VERSION = '1.0';` — compile-time constants,
; outlined as enum members (not callables: Perl's `use constant` shape
; would render them as methods inside a class).
(const_declaration
  (const_element (name) @def.const.name) @def.const)

; ---- parameters (typed → a direct annot witness) ----
(simple_parameter
  type: (_) @type.annot
  name: (variable_name) @def.var.name @def.var @flow.target)
(simple_parameter
  name: (variable_name) @def.var.name @def.var)
; closure captures: `function () use ($y)` re-declares $y in the closure.
(anonymous_function_use_clause
  (variable_name) @def.var.name @def.var)

; ---- imports ----
(namespace_use_declaration
  (namespace_use_clause (qualified_name) @import.name)) @import
(namespace_use_declaration
  (namespace_use_clause (name) @import.name)) @import
; the imported leaf is a live class reference — cross-file rename
; rewrites the use line too.
(namespace_use_clause (qualified_name (name) @ref.type))

; ---- the file's use-map (alias- and group-aware) ----
; What each imported leaf/alias MEANS — parents resolve through it
; before the namespace-relative default. Direct clauses anchor on the
; declaration so the group form (whose clauses are bare names under a
; shared prefix) never double-mints.
; the leading `.` anchors pin the import name to the clause's FIRST child:
; without them the un-fielded (name) alternative also matches the alias
; node as its own combination, and that poison row races the real one for
; the same use-map key (HashMap order decided the winner — flaky by build).
(namespace_use_declaration
  (namespace_use_clause
    . (qualified_name) @use.fqn
    alias: (name)? @use.alias))
(namespace_use_declaration
  (namespace_use_clause
    . (name) @use.fqn
    alias: (name)? @use.alias))
(namespace_use_declaration
  (namespace_name) @use.prefix
  body: (namespace_use_group
    (namespace_use_clause
      . (name) @use.leaf
      alias: (name)? @use.alias)))

; ---- assignment IS declaration (Perl-loose, Python-identical) ----
(assignment_expression
  left: (variable_name) @def.var.name @def.var @flow.target
  right: (_) @flow.source) @flow.assign

; `global $wpdb;` BINDS the global into this function — a declaration
; the uses hang off, and the anchor a `@global wpdb $wpdb` docblock row
; types (the Param-style doc join): WordPress's whole `$wpdb->` surface.
(global_declaration
  (variable_name) @def.var.name @def.var @flow.target)

; foreach BINDS its loop vars — real declarations (refs/hover/highlight/
; rename all hang off the def) that rebind per element (the narrowing
; cutoff). The `"as" .` anchor keeps the ITERATED SOURCE out: `$items` in
; `foreach ($items as $item)` is a read of an existing variable, and a
; pseudo-def there would steal the real declaration's later references.
(foreach_statement
  "as"
  .
  (variable_name) @def.var.name @def.var @flow.rebind)
(foreach_statement
  "as"
  .
  (by_ref (variable_name) @def.var.name @def.var @flow.rebind))
(foreach_statement
  (pair (variable_name) @def.var.name @def.var @flow.rebind))

; ---- references ----
(function_call_expression
  function: (name) @ref.call
  arguments: (arguments) @arity.args) @expr.call
(function_call_expression
  function: (qualified_name (name) @ref.call)
  arguments: (arguments) @arity.args) @expr.call

; `$obj->method()` / `$obj?->method()` / `$obj->prop` / `User::method()`:
; all one MethodCall lane — the receiver types query-time via its own Expr
; witness; a bareword receiver dispatches as the class (Perl `User->make`).
; `@hop.call` = the WHOLE call expression: the chain-hop witness attaches
; to its span, so an outer call's receiver (`object:` = this node) chains
; through it — `$a->b()->c()` types with no intermediate variable.
(member_call_expression
  object: (_) @member.recv
  name: (name) @ref.member
  arguments: (arguments) @arity.args) @hop.call
(nullsafe_member_call_expression
  object: (_) @member.recv
  name: (name) @ref.member
  arguments: (arguments) @arity.args) @hop.call
; A plain property ACCESS is a value too (`$this->query->where(...)`
; chains through it): the hop dispatches the field on the receiver's
; class and answers its declared type — arity-less, same lane.
(member_access_expression
  object: (_) @member.recv
  name: (name) @ref.member) @hop.call
(scoped_call_expression
  scope: (name) @member.recv
  name: (name) @ref.member
  arguments: (arguments) @arity.args) @hop.call
; `self::` / `static::` / `parent::` — the call token still gets a ref
; (rule #7); `parent::` receiver substitution is a documented residual.
(scoped_call_expression
  scope: (relative_scope) @member.recv
  name: (name) @ref.member
  arguments: (arguments) @arity.args) @hop.call

; `User::VERSION` / `self::LIMIT` / `Level::Debug` — class-constant and
; enum-case ACCESS rides the same member lane as a scoped call (the
; receiver dispatches as the class), arg-less. Anchored on both ends so
; the receiver `(name)` can never re-match as the constant of a second
; combination (the use-map poison, same lesson).
(class_constant_access_expression
  . (name) @member.recv
  (name) @ref.member .) @hop.call
(class_constant_access_expression
  . (relative_scope) @member.recv
  (name) @ref.member .) @hop.call

; `new User(...)`: the value is an instance of User by SYNTAX — the ctor
; edge rides the alias graph (TypeName → the defining file, or the bare
; ClassName terminal), so a class declared in another file still types
; the variable. The name stays a call ref so references-on-User count
; instantiation sites.
(object_creation_expression
  (name) @ref.call) @expr.ctor
(object_creation_expression
  (qualified_name (name) @ref.call)) @expr.ctor

(variable_name) @expr.read.var

; ---- literals ----
(string) @expr.lit.string
(encapsed_string) @expr.lit.string
(heredoc) @expr.lit.string
(integer) @expr.lit.number
(float) @expr.lit.number
(boolean) @expr.lit.bool
; a PHP array is an ordered hash map whatever the bracket style.
(array_creation_expression) @expr.lit.hashref

; keyed literal shape: `['timeout' => 30]` → HashWithKeys{timeout} — the
; structural-shape lane (one match per string-keyed element; a pure list
; literal fires none and stays a plain hashref).
(array_creation_expression
  "[" @shape.ctor
  (array_element_initializer (string (string_content) @shape.key))) @expr.shape
(array_creation_expression
  "array" @shape.ctor
  (array_element_initializer (string (string_content) @shape.key))) @expr.shape

; ---- guard narrowing: `if ($x instanceof User) { ... }` ----
(if_statement
  condition: (parenthesized_expression
    (binary_expression
      left: (variable_name) @narrow.var
      "instanceof" @narrow.guard
      right: (name) @narrow.type))
  body: (compound_statement) @scope)

; ---- operator evidence (the Perl edge, alive in PHP) ----
; `.` is string-only, arithmetic is numeric-only: usage sites leak the
; operand's type even when the initializer is unknowable.
(binary_expression
  left: (variable_name) @obs.string
  ".")
(binary_expression
  "."
  right: (variable_name) @obs.string)
(augmented_assignment_expression
  left: (variable_name) @obs.string
  ".=")
(binary_expression
  left: (variable_name) @obs.numeric
  ["+" "-" "*" "/" "%" "**" "<=>"])
(binary_expression
  ["+" "-" "*" "/" "%" "**" "<=>"]
  right: (variable_name) @obs.numeric)
