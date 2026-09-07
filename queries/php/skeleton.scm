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
; The type is optional (`protected $stream`) and the name may be
; by-reference (`protected &$container`); both spellings declare the
; property and the ctor-body local.
(property_promotion_parameter
  type: (_)? @type.annot
  name: [(variable_name (name) @def.field.name @def.field @flow.target)
         (by_ref (variable_name (name) @def.field.name @def.field @flow.target))])
(property_promotion_parameter
  name: [(variable_name) @def.var.name @def.var
         (by_ref (variable_name) @def.var.name @def.var)])

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
;; `$x instanceof Foo` names the class; `#[Foo]` / `#[Ns\Foo(...)]` names an
;; attribute class — both are class references (goto-def, rename, the
;; use-map's spelled set).
(binary_expression "instanceof" right: (name) @ref.type)
(binary_expression "instanceof" right: (qualified_name (name) @ref.type) @ref.qualified)
(attribute (name) @ref.type)
(attribute (qualified_name (name) @ref.type) @ref.qualified)

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
; `$d = &$this->x` binds `$d` to the value's storage — the same
; declaration, typed by the same flow; `@alias.target` (on the inner
; name — a query step holds three captures) marks it, so a write through
; it (`$d[] = 1`) counts as a use of the storage.
(reference_assignment_expression
  left: (variable_name (name) @alias.target) @def.var.name @def.var @flow.target
  right: (_) @flow.source) @flow.assign
; `$this->x = <value>`: a property typed by what is written to it. The
; flow edge lands at the CLASS-BODY scope (`@flow.target.member`), where
; the field's readers look; `$this` is the receiver spelling.
(assignment_expression
  left: (member_access_expression
    object: (variable_name) @_prop_recv
    name: (name) @flow.target.member)
  right: (_) @flow.source
  (#eq? @_prop_recv "$this"))

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

