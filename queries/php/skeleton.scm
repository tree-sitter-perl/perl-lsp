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
; `#[Attr]` annotations ride @sym.attr onto Symbol.attributes (the same
; lane cpp storage-class specifiers use): hover renders them, and the
; framework-entry machinery reads them as invocation evidence.
(class_declaration
  attributes: (attribute_list
    (attribute_group
      (attribute [(name) (qualified_name (name))] @sym.attr)+)+)?
  name: (name) @def.class.name @context.package) @def.class @scope
(interface_declaration
  attributes: (attribute_list
    (attribute_group
      (attribute [(name) (qualified_name (name))] @sym.attr)+)+)?
  name: (name) @def.class.name @context.package) @def.class @scope
(trait_declaration
  attributes: (attribute_list
    (attribute_group
      (attribute [(name) (qualified_name (name))] @sym.attr)+)+)?
  name: (name) @def.class.name @context.package) @def.class @scope
(enum_declaration
  attributes: (attribute_list
    (attribute_group
      (attribute [(name) (qualified_name (name))] @sym.attr)+)+)?
  name: (name) @def.class.name @context.package) @def.class @scope

; anonymous classes: `new class(...) extends Base implements I { ... }`.
; No name node — the `class` keyword anchors a @def.class.anchor: the pack
; synthesizes a position-keyed identity, the body's members key by it
; (never by the enclosing container), `$this` inside resolves to it, and
; the keyword is the constructor's call site.
(anonymous_class
  "class" @def.class.anchor @context.package
  body: (declaration_list) @scope) @def.class
(anonymous_class
  "class" @def.class.anchor
  (base_clause (name) @parent @ref.type))
(anonymous_class
  "class" @def.class.anchor
  (base_clause (qualified_name (name) @parent @ref.type) @parent.fq @ref.qualified))
(anonymous_class
  "class" @def.class.anchor
  (class_interface_clause (name) @parent @ref.type))
(anonymous_class
  "class" @def.class.anchor
  (class_interface_clause (qualified_name (name) @parent @ref.type) @parent.fq @ref.qualified))
(anonymous_class
  "class" @def.class.anchor
  body: (declaration_list (use_declaration (name) @parent @ref.type)))
(anonymous_class
  "class" @def.class.anchor
  body: (declaration_list (use_declaration (qualified_name (name) @parent @ref.type) @parent.fq @ref.qualified)))

; inheritance: `extends Base` — one @parent per base; the name is also a
; live type use (goto-def on the base rides the PackageRef lane). Every
; clause has a qualified sibling (`extends \App\Base`, `use
; Concerns\HasAttributes`) whose LEAF is the identity classes key by.
(class_declaration
  name: (name) @def.class.name
  (base_clause (name) @parent @ref.type))
(class_declaration
  name: (name) @def.class.name
  (base_clause (qualified_name (name) @parent @ref.type) @parent.fq @ref.qualified))
(interface_declaration
  name: (name) @def.class.name
  (base_clause (name) @parent @ref.type))
(interface_declaration
  name: (name) @def.class.name
  (base_clause (qualified_name (name) @parent @ref.type) @parent.fq @ref.qualified))
; `implements Contract` — an interface is a parent for method-resolution
; purposes (the contract's declarations answer hover/completion).
(class_declaration
  name: (name) @def.class.name
  (class_interface_clause (name) @parent @ref.type))
(class_declaration
  name: (name) @def.class.name
  (class_interface_clause (qualified_name (name) @parent @ref.type) @parent.fq @ref.qualified))
; `use SomeTrait;` inside a class body — trait methods resolve through the
; same ancestor walk (PHP flattening ≈ role composition ≈ parent edge).
(class_declaration
  name: (name) @def.class.name
  body: (declaration_list (use_declaration (name) @parent @ref.type)))
(class_declaration
  name: (name) @def.class.name
  body: (declaration_list (use_declaration (qualified_name (name) @parent @ref.type) @parent.fq @ref.qualified)))

; container flavor marks (name-span post-join like @nonpublic.target):
; interfaces and traits are SymKind::Class in the model, but SUPER
; resolution must prefer a concrete parent over an interface's abstract
; stub, and trait identity feeds the consumer-side reference walk.
(interface_declaration name: (name) @classattr.interface)
(trait_declaration name: (name) @classattr.trait)
(enum_declaration name: (name) @classattr.enum)

; access modifiers -> the model's non_public attribute (the same gate
; cpp access regions stamp): a private/protected member completes only
; from inside its own class's body. Joined to the def by NAME-SPAN
; post-pass (the ns.inline precedent) so the def patterns stay
; modifier-blind; the vocabulary lives in the #any-of?, not in engine code.
(method_declaration
  (visibility_modifier) @nonpublic.mark
  name: (name) @nonpublic.target
  (#any-of? @nonpublic.mark "private" "protected"))
(property_declaration
  (visibility_modifier) @nonpublic.mark
  (property_element name: (variable_name (name) @nonpublic.target))
  (#any-of? @nonpublic.mark "private" "protected"))
(const_declaration
  (visibility_modifier) @nonpublic.mark
  (const_element (name) @nonpublic.target)
  (#any-of? @nonpublic.mark "private" "protected"))
(property_promotion_parameter
  visibility: (visibility_modifier) @nonpublic.mark
  name: (variable_name (name) @nonpublic.target)
  (#any-of? @nonpublic.mark "private" "protected"))

; enum cases: real enumerators — parent-enum typing + container tagging
; come generically from the engine's enumerator lane.
(enum_case
  name: (name) @def.enumerator.name) @def.enumerator

; ---- callables ----
; @rettype carries the declared return type → method-return chaining
; through PackageSymbol, same chase Perl and C++ use.
(function_definition
  attributes: (attribute_list
    (attribute_group
      (attribute [(name) (qualified_name (name))] @sym.attr)+)+)?
  name: (name) @def.sub.name
  return_type: (_)? @rettype) @def.sub
(method_declaration
  attributes: (attribute_list
    (attribute_group
      (attribute [(name) (qualified_name (name))] @sym.attr)+)+)?
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
; closure captures: `function () use ($y)` re-declares $y in the closure;
; `use (&$y)` binds by reference — the same declaration.
(anonymous_function_use_clause
  (variable_name) @def.var.name @def.var)
; by reference, php creates the variable in the ENCLOSING scope when it
; does not exist — the declaration hoists there (`@hoist`).
(anonymous_function_use_clause
  (by_ref (variable_name) @def.var.name @def.var @hoist))
; `static $map = [...]` declares a function-static local; `$rows[] = $x`
; auto-vivifies `$rows` — both declare.
(static_variable_declaration
  name: (variable_name) @def.var.name @def.var)
(assignment_expression
  left: (subscript_expression (variable_name) @def.var.name @def.var))
; `catch (E $e)` binds the exception variable.
(catch_clause
  name: (variable_name) @def.var.name @def.var)

; ---- imports ----
(namespace_use_declaration
  (namespace_use_clause (qualified_name) @import.name)) @import
(namespace_use_declaration
  (namespace_use_clause (name) @import.name)) @import
; the imported leaf is a live class reference — cross-file rename
; rewrites the use line too.
(namespace_use_clause (qualified_name (name) @ref.type))
(namespace_use_group (namespace_use_clause . (name) @ref.type))
; A type position (`Collection $c`, `?Request $r`, `: static`, a union's
; class arms) spells the class: references/rename on the class reach the
; hints, and the file's use-map counts the leaf as spelled here.
; Primitives (`int`, `array`) are `primitive_type`, never matched.
(named_type (name) @ref.type)
(named_type (qualified_name (name) @ref.type) @ref.qualified)

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
      alias: (name)? @use.alias))) @import

; a member on the LEFT of an assignment: php declares a property by
; writing it (`$this->x = ...`) — the undefined-property lane treats the
; write as its declaration.
(assignment_expression
  left: (member_access_expression name: (name) @member.write))
(assignment_expression
  left: (scoped_property_access_expression name: (variable_name (name) @member.write)))

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
; The collection joins the same match (`@seq.source`) so the bound var
; types as the collection's ELEMENT — the `Projected{base, Element}`
; witness peels a doc-typed sequence (`@var list<Handler>`); the
; key=>value pair form stays untyped (the key needs its own axis).
(foreach_statement
  . (_) @seq.source
  "as"
  .
  (variable_name) @def.var.name @def.var @flow.rebind)
(foreach_statement
  . (_) @seq.source
  "as"
  .
  (by_ref (variable_name) @def.var.name @def.var @flow.rebind))
; pair form: the KEY (first child) peels the collection's key axis, the
; VALUE (last child) its element — same source join, different step.
(foreach_statement
  . (_) @seq.source.key
  (pair . (variable_name) @def.var.name @def.var @flow.rebind))
(foreach_statement
  . (_) @seq.source
  (pair (variable_name) @def.var.name @def.var @flow.rebind .))

; ---- return sites ----
; The returned expression's own witness (literal / read / call / tuple
; literal) types the enclosing function through the driver's return-fuel
; phase when the signature declares nothing — or declares only a bare
; container the value refines (`: array` over `return [$q, $a]`,
; docs/adr/destructuring.md).
(return_statement (_) @expr.return.value)

; ---- destructuring (docs/adr/destructuring.md) ----
; `[$a, $b] = f()` / `list($a, $b) = f()`: every slot is a declaration
; bound POSITIONALLY off the RHS through the same FlowEdge lowering Perl's
; `my ($a, $b) = f()` uses (Extraction::Positional → ArrayIndex(n)); the
; position is counted over the list text's top-level commas (`[, $b]`).
; A keyed list (`['k' => $v]`) declares but never binds positionally;
; nested list slots are not direct children and stay out.
(assignment_expression
  left: (list_literal
    (variable_name) @def.var.name @def.var @flow.slot) @flow.slot.list
  right: (_) @flow.source)
; `foreach ($pairs as [$k, $v])` / `foreach ($m as $i => [$a, $b])`: the
; list IS the collection's element — slots peel Element, then index.
(foreach_statement
  . (_) @seq.source
  "as"
  . (list_literal
      (variable_name) @def.var.name @def.var @flow.slot) @flow.slot.list)
(foreach_statement
  . (_) @seq.source
  (pair
    (variable_name)
    (list_literal
      (variable_name) @def.var.name @def.var @flow.slot) @flow.slot.list .))

; A key-less array literal is a positional TUPLE of its elements' edges
; (`return [$queue, $agent]`): one match per element, grouped by the
; array span in extraction; a keyed element or a spread disqualifies the
; literal (it is a map / open list, never a tuple).
(array_creation_expression
  (array_element_initializer . (_) @tuple.elem .) @tuple.init) @tuple.arr
(array_creation_expression
  (array_element_initializer (_) (_) @tuple.keyed)) @tuple.arr

; ---- references ----
(function_call_expression
  function: (name) @ref.call
  arguments: (arguments) @arity.args) @expr.call
(function_call_expression
  function: (qualified_name (name) @ref.call) @ref.qualified
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
  scope: (name) @member.recv @ref.type
  name: (name) @ref.member
  arguments: (arguments) @arity.args) @hop.call
; `self::` / `static::` / `parent::` — the call token still gets a ref
; (rule #7); `parent::` receiver substitution is a documented residual.
(scoped_call_expression
  scope: (relative_scope) @member.recv
  name: (name) @ref.member
  arguments: (arguments) @arity.args) @hop.call
; `$this->helper::make()` / `$cls::make()` / `static::$inst::run()` — a
; scoped call on an EXPRESSION receiver: the receiver types like any member
; access (its property type, its class-string value) and the call
; dispatches on that class.
(scoped_call_expression
  scope: [(variable_name) (member_access_expression) (scoped_property_access_expression)] @member.recv
  name: (name) @ref.member
  arguments: (arguments) @arity.args) @hop.call
; `Helper::class` — the class-string literal IS the class: the value a
; later `$cls::make()` dispatches on. Bareword class receivers
; (`Helper::make()`, `Helper::VERSION`, `Helper::$inst`, `Helper::class`)
; also spell the class (@ref.type) — the class's references and rename
; reach them, and the use-map counts the leaf as spelled.
((class_constant_access_expression
  . (name) @classref.name
  (name) @_clsk .) @expr.classref
  (#eq? @_clsk "class"))
((class_constant_access_expression
  . (qualified_name (name) @classref.name)
  (name) @_clskq .) @expr.classref
  (#eq? @_clskq "class"))

; `User::VERSION` / `self::LIMIT` / `Level::Debug` — class-constant and
; enum-case ACCESS rides the same member lane as a scoped call (the
; receiver dispatches as the class), arg-less. Anchored on both ends so
; the receiver `(name)` can never re-match as the constant of a second
; combination (the use-map poison, same lesson).
(class_constant_access_expression
  . (name) @member.recv @ref.type
  (name) @ref.member .) @hop.call
(class_constant_access_expression
  . (relative_scope) @member.recv
  (name) @ref.member .) @hop.call

; `[UserController::class, 'index']` / `array(Listener::class, 'handle')`:
; php's class-array callable — the exactly-two-element pair NAMES a
; dispatchable method (Laravel routes, event maps, callable args). The
; class token rides @member.recv (the bareword-dispatches-as-class rule)
; and the string content mints the method ref — the array($this, 'm')
; shape with a class receiver. Language convention, not framework
; vocabulary, so it lives in the base skeleton.
; Each element must be BARE (`. (x) .` inside the initializer): a keyed
; pair `'book' => $chapter->book` also contains a string and a receiver,
; and a two-pair view-data array read as a callable — its key token
; became a method reference a rename would rewrite.
(array_creation_expression
  . (array_element_initializer
      . (class_constant_access_expression
        . (name) @member.recv
        (name) @_ccls .) .)
  . (array_element_initializer . (string (string_content) @ref.method.named) .) .
  (#eq? @_ccls "class"))
(array_creation_expression
  . (array_element_initializer
      . (class_constant_access_expression
        . (qualified_name (name) @member.recv)
        (name) @_cclsq .) .)
  . (array_element_initializer . (string (string_content) @ref.method.named) .) .
  (#eq? @_cclsq "class"))

; `[$this, 'method']` / `[$listener, 'method']` — the instance-array
; callable: the variable is the receiver (typed like any member access),
; the string names the method. Event listeners and PHPUnit callbacks live
; here; a rename that misses them breaks the dispatch at runtime.
(array_creation_expression
  . (array_element_initializer . (variable_name) @member.recv .)
  . (array_element_initializer . (string (string_content) @ref.method.named) .) .)

; `static::$records` / `self::$records` / `Foo::$prop` — scoped STATIC
; property access rides the same member lane (`static::$prop`
; lost the property's own @var doc because no hop existed here; the
; `$this->prop` twin always had one). The field name is the inner
; (name), sigil-stripped like instance access; relative scopes
; canonicalize via member.recv shaping.
(scoped_property_access_expression
  scope: (relative_scope) @member.recv
  name: (variable_name (name) @ref.member) @var.member) @hop.call
(scoped_property_access_expression
  scope: (name) @member.recv @ref.type
  name: (variable_name (name) @ref.member) @var.member) @hop.call

; `new User(...)`: the value is an instance of User by SYNTAX — the ctor
; edge rides the alias graph (TypeName → the defining file, or the bare
; ClassName terminal), so a class declared in another file still types
; the variable. The name stays a call ref so references-on-User count
; instantiation sites.
(object_creation_expression
  (name) @ref.call) @expr.ctor
(object_creation_expression
  (qualified_name (name) @ref.call) @ref.qualified) @expr.ctor

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

; ---- branch arms: `match` / ternary type as their ARMS' agreement ----
; One match per arm, each carrying the whole expression's span
; (`@branch.expr`) so extraction joins arm → expression: the expression's
; `Expr` edges to `BranchArm(span)`, every arm edges its own `Expr` there,
; and `BranchArmFold` answers only when the arms agree. Without this the
; assignment's literal-narrowing picked the largest literal INSIDE the
; match — a discriminant string — as the value's type.
(match_expression
  body: (match_block
    (match_conditional_expression return_expression: (_) @branch.arm))) @branch.expr
(match_expression
  body: (match_block
    (match_default_expression return_expression: (_) @branch.arm))) @branch.expr
(conditional_expression
  body: (_) @branch.arm) @branch.expr
(conditional_expression
  alternative: (_) @branch.arm) @branch.expr

; ---- subscripts: `f()[0]` / `$row['name']` project off the base ----
; An integer index peels a tuple/sequence slot (`list<T>` → T); a literal
; string key drills a keyed shape (`array{name: string}` → string).
(subscript_expression
  . (_) @subscript.base
  (integer) @subscript.int .) @subscript.expr
(subscript_expression
  . (_) @subscript.base
  (string (string_content) @subscript.key) .) @subscript.expr

; ---- guard narrowing: `if ($x instanceof User) { ... }` ----
; The class token may be bare or namespace-qualified (`Op\Install` —
; `annot_type` leafs it). The guard may sit alone, be a conjunct of `&&`
; (both operands hold inside the body, so either side narrows), or open
; an `elseif` arm. A negated guard narrows only when its body exits
; (the `@narrow.after` shapes below).
(if_statement
  condition: (parenthesized_expression
    (binary_expression
      left: (variable_name) @narrow.var
      "instanceof" @narrow.guard
      right: [(name) (qualified_name)] @narrow.type))
  body: (compound_statement) @scope)
(else_if_clause
  condition: (parenthesized_expression
    (binary_expression
      left: (variable_name) @narrow.var
      "instanceof" @narrow.guard
      right: [(name) (qualified_name)] @narrow.type))
  body: (compound_statement) @scope)
(if_statement
  condition: (parenthesized_expression
    (binary_expression
      left: (binary_expression
        left: (variable_name) @narrow.var
        "instanceof" @narrow.guard
        right: [(name) (qualified_name)] @narrow.type)
      "&&"))
  body: (compound_statement) @scope)
(if_statement
  condition: (parenthesized_expression
    (binary_expression
      "&&"
      right: (binary_expression
        left: (variable_name) @narrow.var
        "instanceof" @narrow.guard
        right: [(name) (qualified_name)] @narrow.type)))
  body: (compound_statement) @scope)
(if_statement
  condition: (parenthesized_expression
    (binary_expression
      left: (binary_expression
        left: (binary_expression
          left: (variable_name) @narrow.var
          "instanceof" @narrow.guard
          right: [(name) (qualified_name)] @narrow.type)
        "&&")
      "&&"))
  body: (compound_statement) @scope)

;; A NEGATED guard whose body leaves the scope (`if (!$x instanceof T)
;; { return; }`, `throw`, `continue`, `break`) narrows the REMAINDER of
;; the enclosing scope: `@narrow.after` marks the statement the refinement
;; starts after. The body is the exit itself or a block whose LAST
;; statement exits.
(if_statement
  condition: (parenthesized_expression
    (unary_op_expression
      "!"
      argument: [
        (binary_expression
          left: (variable_name) @narrow.var
          "instanceof" @narrow.guard
          right: [(name) (qualified_name)] @narrow.type)
        (parenthesized_expression
          (binary_expression
            left: (variable_name) @narrow.var
            "instanceof" @narrow.guard
            right: [(name) (qualified_name)] @narrow.type))]))
  body: [
    (return_statement)
    (continue_statement)
    (break_statement)
    (expression_statement (throw_expression))
    (compound_statement [(return_statement) (continue_statement) (break_statement) (expression_statement (throw_expression))] .)]) @narrow.after
;; `assert($x instanceof T);` — the pack's `narrow_assertions` decide
;; which callees assert (the capture fires for any call; core gates).
(expression_statement
  (function_call_expression
    function: (name) @narrow.assert
    arguments: (arguments
      (argument
        (binary_expression
          left: (variable_name) @narrow.var
          "instanceof" @narrow.guard
          right: [(name) (qualified_name)] @narrow.type))))) @narrow.after
;; Expression-level regions: the refinement holds WITHIN the marked node —
;; the right operand of `&&`, the ternary's true arm, a `match` arm's
;; return expression.
(binary_expression
  left: (binary_expression
    left: (variable_name) @narrow.var
    "instanceof" @narrow.guard
    right: [(name) (qualified_name)] @narrow.type)
  "&&"
  right: (_) @narrow.within)
(conditional_expression
  condition: [
    (binary_expression
      left: (variable_name) @narrow.var
      "instanceof" @narrow.guard
      right: [(name) (qualified_name)] @narrow.type)
    (parenthesized_expression
      (binary_expression
        left: (variable_name) @narrow.var
        "instanceof" @narrow.guard
        right: [(name) (qualified_name)] @narrow.type))]
  body: (_) @narrow.within)
(match_conditional_expression
  conditional_expressions: (match_condition_list
    (binary_expression
      left: (variable_name) @narrow.var
      "instanceof" @narrow.guard
      right: [(name) (qualified_name)] @narrow.type))
  return_expression: (_) @narrow.within)

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
