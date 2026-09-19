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
(trait_declaration
  name: (name) @def.class.name
  body: (declaration_list (use_declaration (name) @parent @ref.type)))
(trait_declaration
  name: (name) @def.class.name
  body: (declaration_list (use_declaration (qualified_name (name) @parent @ref.type) @parent.fq @ref.qualified)))
(enum_declaration
  name: (name) @def.class.name
  body: (enum_declaration_list (use_declaration (name) @parent @ref.type)))
(enum_declaration
  name: (name) @def.class.name
  body: (enum_declaration_list (use_declaration (qualified_name (name) @parent @ref.type) @parent.fq @ref.qualified)))

; container flavor marks (name-span post-join like @nonpublic.target):
; interfaces and traits are SymKind::Class in the model, but SUPER
; resolution must prefer a concrete parent over an interface's abstract
; stub, and trait identity feeds the consumer-side reference walk.
; The methods whose presence makes a class answer ANY member name: a class
; declaring one has no static member surface to check against, so the
; undefined-member lanes stay silent on it (as they do on Perl's AUTOLOAD).
((method_declaration name: (name) @def.method.catch_all)
 (#any-of? @def.method.catch_all "__call" "__callStatic" "__get"))

; The CONSTRUCTOR: the one method a `new Foo(...)` invokes, the one whose
; name belongs to the language (nothing renames it). Named here, so the
; construction sites and the rename policy read one fact.
((method_declaration name: (name) @def.method.ctor)
 (#eq? @def.method.ctor "__construct"))
; the constructor NAMED at a call site (`parent::__construct(...)`): a class
; that declares none still has the default one, so the reference carries the
; fact and no lane compares the spelling back.
((scoped_call_expression name: (name) @ref.method.ctor)
 (#eq? @ref.method.ctor "__construct"))
((member_call_expression name: (name) @ref.method.ctor)
 (#eq? @ref.method.ctor "__construct"))

(interface_declaration name: (name) @classattr.interface)
(trait_declaration name: (name) @classattr.trait)
(enum_declaration name: (name) @classattr.enum)

; access modifiers -> the model's non_public attribute (the same gate
; cpp access regions stamp): a private/protected member completes only
; from inside its own class's body. Joined to the def by NAME-SPAN
; post-pass (the ns.inline precedent) so the def patterns stay
; modifier-blind; the vocabulary lives in the #any-of?, not in engine code.
(method_declaration
  (visibility_modifier) @_nonpublic_mark
  name: (name) @nonpublic.target
  (#any-of? @_nonpublic_mark "private" "protected"))
(property_declaration
  (visibility_modifier) @_nonpublic_mark
  (property_element name: (variable_name (name) @nonpublic.target))
  (#any-of? @_nonpublic_mark "private" "protected"))
(const_declaration
  (visibility_modifier) @_nonpublic_mark
  (const_element (name) @nonpublic.target)
  (#any-of? @_nonpublic_mark "private" "protected"))
(property_promotion_parameter
  visibility: (visibility_modifier) @_nonpublic_mark
  name: (variable_name (name) @nonpublic.target)
  (#any-of? @_nonpublic_mark "private" "protected"))
; `static` members: the same post-pass stamp (`static` attribute) — what a
; scoped access (`Foo::`) completes.
(method_declaration
  (static_modifier)
  name: (name) @static.target)
(property_declaration
  (static_modifier)
  (property_element name: (variable_name (name) @static.target)))

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
; arg count floats the fitting signature above a same-named stub). WHICH
; children are parameters, and what each does to the count, the document
; states: @arity.param must be written, @arity.param.optional carries a
; default, @arity.param.variadic absorbs the rest and makes the signature
; variadic. The parts a parameter carries ride their own captures —
; @arity.param.name is the token a by-reference argument binds through,
; @arity.param.byref marks the parameter that aliases its caller's variable,
; @arity.param.default and @arity.param.type travel as source text.
(formal_parameters) @arity.sig
(formal_parameters (simple_parameter !default_value) @arity.param)
(formal_parameters (simple_parameter default_value: (_)) @arity.param.optional)
(formal_parameters (property_promotion_parameter !default_value) @arity.param)
(formal_parameters (property_promotion_parameter default_value: (_)) @arity.param.optional)
(formal_parameters (variadic_parameter) @arity.param.variadic)
(formal_parameters (simple_parameter reference_modifier: (_)) @arity.param.byref)
(formal_parameters (property_promotion_parameter name: (by_ref)) @arity.param.byref)
(formal_parameters (_ name: (variable_name) @arity.param.name))
(formal_parameters (_ name: (by_ref (variable_name) @arity.param.name)))
(formal_parameters (_ default_value: (_) @arity.param.default))
(formal_parameters (_ type: (_) @arity.param.type))

; docblocks: the pack's `doc_types` parses `@return`/`@param`/`@var` out of
; the comment. The bare capture feeds the mention scan (a name spelled only
; in a docblock is a used import); the JOIN is the anchored patterns below.
; Declared types win — the doc lane fills only what the syntax left untyped.
(comment) @doc.comment

; the def a docblock documents is its NEXT SIBLING, stated as one match so
; the pair meets in the query instead of by row arithmetic — an attribute
; line between the two (`/** */ #[Attr] protected array $x;`) joins like any
; other. @doc.subject lands on exactly the node its `@def.*` twin does, so
; the two meet at one point and nothing downstream measures a distance.
((comment) @doc.comment . (function_definition) @doc.subject
  (#match? @doc.comment "^/\\*\\*"))
((comment) @doc.comment . (method_declaration) @doc.subject
  (#match? @doc.comment "^/\\*\\*"))
((comment) @doc.comment . (class_declaration) @doc.subject
  (#match? @doc.comment "^/\\*\\*"))
((comment) @doc.comment . (interface_declaration) @doc.subject
  (#match? @doc.comment "^/\\*\\*"))
((comment) @doc.comment . (trait_declaration) @doc.subject
  (#match? @doc.comment "^/\\*\\*"))
((comment) @doc.comment . (enum_declaration) @doc.subject
  (#match? @doc.comment "^/\\*\\*"))
((comment) @doc.comment
  . (property_declaration
      (property_element name: (variable_name (name) @doc.subject)))
  (#match? @doc.comment "^/\\*\\*"))
((comment) @doc.comment
  . (const_declaration (const_element) @doc.subject)
  (#match? @doc.comment "^/\\*\\*"))
; `/** @var Concrete $x */ $x = Factory::make();` — the subject is the
; assignment's target, whether it declares the variable or rebinds it.
((comment) @doc.comment
  . (expression_statement
      (assignment_expression left: (variable_name) @doc.subject))
  (#match? @doc.comment "^/\\*\\*"))
((comment) @doc.comment
  . (global_declaration (variable_name) @doc.subject)
  (#match? @doc.comment "^/\\*\\*"))

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
; BOTH the property (sigil-less member) and the ctor-body local (`$name`)
; from ONE token — captured in ONE match so the two are minted as a
; co-declared pair and nothing downstream re-derives the relation. The type
; is optional (`protected $stream`) and the name may be by-reference
; (`protected &$container`).
(property_promotion_parameter
  type: (_)? @type.annot
  name: (variable_name (name) @def.field.name @def.field.declared_with @flow.target)
        @def.var.name @def.var.declared_with)
(property_promotion_parameter
  type: (_)? @type.annot
  name: (by_ref
          (variable_name (name) @def.field.name @def.field.declared_with @flow.target)
          @def.var.name @def.var.declared_with))

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
; `...$args` declares the variadic parameter (its type is the element's,
; never the parameter's).
(variadic_parameter
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
  left: (subscript_expression . (variable_name) @def.var.name @def.var))
; `catch (E $e)` binds the exception variable.
(catch_clause
  name: (variable_name) @def.var.name @def.var)

; A parameter list is a region, not a body: a parameter is the caller's
; contract, never an unused local.
(formal_parameters) @param.region
; `isset($x->p)` / `empty($x->p)`: the read IS the existence question, so
; the undefined-member lanes stay silent inside the probe's argument list.
((function_call_expression
   function: (name) @_probe
   arguments: (arguments) @probe.region)
 (#any-of? @_probe "isset" "empty"))
; `unset($x)` asks the same question of a variable (and answers it).
(unset_statement) @probe.region

; The file preamble an import may not precede: the open tag and
; `declare(...)` rows (php requires `declare(strict_types=1)` first).
(php_tag) @preamble
(declare_statement) @preamble

; ---- imports ----
; What a row BINDS rides its capture suffix: `use function` binds a
; callable, `use const` a constant, an unsuffixed row a type. The keyword
; is an anonymous token, so the unsuffixed arms exclude it by the clause's
; own text rather than letting both arms mint the same row. The leading
; anchor pins the un-fielded `(name)` to the clause's FIRST child: without
; it the alias node matches that alternative too and `use G as H` mints a
; second row for `H`.
; `@import.binds` is the NAME the row brings into the file — the alias when
; the clause writes one, the leaf otherwise. The two spellings ride one
; capture and the later byte wins, so a reader never asks which arm fired.
(namespace_use_declaration
  (namespace_use_clause "function" (qualified_name (name) @import.binds) @import.name.function
    alias: (name)? @import.binds)) @import
(namespace_use_declaration
  (namespace_use_clause "function" . (name) @import.name.function @import.binds
    alias: (name)? @import.binds)) @import
(namespace_use_declaration
  (namespace_use_clause "const" (qualified_name (name) @import.binds) @import.name.const
    alias: (name)? @import.binds)) @import
(namespace_use_declaration
  (namespace_use_clause "const" . (name) @import.name.const @import.binds
    alias: (name)? @import.binds)) @import
(namespace_use_declaration
  (namespace_use_clause (qualified_name (name) @import.binds) @import.name
    alias: (name)? @import.binds) @_plain_row
  (#not-match? @_plain_row "^(function|const)[ \t\r\n]")) @import
(namespace_use_declaration
  (namespace_use_clause . (name) @import.name @import.binds
    alias: (name)? @import.binds) @_plain_row
  (#not-match? @_plain_row "^(function|const)[ \t\r\n]")) @import
; the imported leaf is a live class reference — cross-file rename
; rewrites the use line too.
(namespace_use_clause (qualified_name (name) @ref.type))
; a group clause that binds a callable or a constant names no class — the
; same keyword exclusion the unsuffixed row arms use.
(namespace_use_group
  (namespace_use_clause . (name) @ref.type) @_plain_clause
  (#not-match? @_plain_clause "^(function|const)[ \t\r\n]"))
; A type position (`Collection $c`, `?Request $r`, `: static`, a union's
; class arms) spells the class: references/rename on the class reach the
; hints, and the file's use-map counts the leaf as spelled here.
; Primitives (`int`, `array`) are `primitive_type`, never matched.
(named_type (name) @ref.type)
; `self` / `static` / `parent` written where a class NAME goes: they name the
; class this code is written in, or its parent, and resolve off the enclosing
; scope rather than out of a namespace. The reference says so, so the lane
; that reports a name its namespace cannot supply never matches the spelling.
((named_type (name) @receiver.self) (#any-of? @receiver.self "self" "static"))
((named_type (name) @receiver.super) (#eq? @receiver.super "parent"))
((binary_expression "instanceof" right: (name) @receiver.self)
 (#any-of? @receiver.self "self" "static"))
((binary_expression "instanceof" right: (name) @receiver.super)
 (#eq? @receiver.super "parent"))
((object_creation_expression (name) @receiver.super)
 (#eq? @receiver.super "parent"))
(named_type (qualified_name (name) @ref.type) @ref.qualified)
;; `$x instanceof Foo` names the class; `#[Foo]` / `#[Ns\Foo(...)]` names an
;; attribute class — both are class references (goto-def, rename, the
;; use-map's spelled set).
(binary_expression "instanceof" right: (name) @ref.type)
(binary_expression "instanceof" right: (qualified_name (name) @ref.type) @ref.qualified)
(attribute (name) @ref.type)
(attribute (qualified_name (name) @ref.type) @ref.qualified)
;; `#[Deprecated]` is the attribute spelling of the `@deprecated` docblock
;; tag — the declaration below it carries the `deprecated` attribute.
((attribute (name) @sym.attr.deprecated)
 (#eq? @sym.attr.deprecated "Deprecated"))

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
; A group clause binds what its own keyword says, exactly as a flat row
; does: the binds suffix rides the row capture of the arm that matched, so
; a mixed group (`use A\{Z, function b, const C}`) gives each clause its
; own `ImportBinds` — one match per clause, one arm per keyword.
(namespace_use_declaration
  (namespace_name) @use.prefix
  body: (namespace_use_group
    (namespace_use_clause "function"
      . (name) @use.leaf @import.binds
      alias: (name)? @use.alias @import.binds))) @import.function
(namespace_use_declaration
  (namespace_name) @use.prefix
  body: (namespace_use_group
    (namespace_use_clause "const"
      . (name) @use.leaf @import.binds
      alias: (name)? @use.alias @import.binds))) @import.const
(namespace_use_declaration
  (namespace_name) @use.prefix
  body: (namespace_use_group
    (namespace_use_clause
      . (name) @use.leaf @import.binds
      alias: (name)? @use.alias @import.binds) @_plain_group_clause)
  (#not-match? @_plain_group_clause "^(function|const)[ \t\r\n]")) @import

