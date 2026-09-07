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

