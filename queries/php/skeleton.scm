; PHP language pack. Grammar: tree-sitter-php's `php` flavor, which keeps
; the `<?php` tag and any interleaved HTML as inert nodes.

; ---- namespaces ----
; `namespace A\B;` holds until the next one; the braced form holds for its
; body. Classes declared under either join it to mint their identity.
(namespace_definition
  name: (namespace_name) @def.package.name @context.namespace
  !body) @def.package
(namespace_definition
  name: (namespace_name) @def.package.name @context.namespace
  body: (compound_statement) @scope) @def.package

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

; ---- parents ----
; `extends`, `implements` and a trait `use` are all parents for member
; lookup. The whole written spelling is the capture: it resolves through
; the file's use-map to the parent's identity.
(class_declaration
  name: (name) @def.class.name
  (base_clause [(name) (qualified_name)] @parent))
(class_declaration
  name: (name) @def.class.name
  (class_interface_clause [(name) (qualified_name)] @parent))
(interface_declaration
  name: (name) @def.class.name
  (base_clause [(name) (qualified_name)] @parent))
(class_declaration
  name: (name) @def.class.name
  body: (declaration_list (use_declaration [(name) (qualified_name)] @parent)))
(trait_declaration
  name: (name) @def.class.name
  body: (declaration_list (use_declaration [(name) (qualified_name)] @parent)))

; A parent's name is a class reference: goto-def, references and rename
; reach it. A qualified spelling references by the whole spelling and
; rewrites only its leaf.
(base_clause (name) @ref.type)
(base_clause (qualified_name (name) @ref.type) @ref.spelled)
(class_interface_clause (name) @ref.type)
(class_interface_clause (qualified_name (name) @ref.type) @ref.spelled)
(use_declaration (name) @ref.type)
(use_declaration (qualified_name (name) @ref.type) @ref.spelled)

; ---- the use-map ----
; A row binds a class unless its keyword says it binds a function or a
; constant; those rows name no class, so they never enter the map. The
; keyword is an anonymous token, so the row's own text excludes it. The
; `.` anchor pins the name to the clause's first child, so the alias node
; never matches as a second name.
((namespace_use_declaration
  (namespace_use_clause
    . [(name) (qualified_name)] @import.name
    alias: (name)? @import.alias) @_clause)
 (#not-match? @_clause "^(function|const)\\s"))
; `use A\B\{C, D as E}`: each clause hangs off the shared prefix. A group
; may carry the keyword for all its clauses or each clause its own.
((namespace_use_declaration
  (namespace_name) @import.prefix
  body: (namespace_use_group
    (namespace_use_clause
      . (name) @import.name
      alias: (name)? @import.alias) @_clause)) @_group
 (#not-match? @_group "^use\\s+(function|const)\\s")
 (#not-match? @_clause "^(function|const)\\s"))

; An imported class's leaf references the class the row names, so a
; rename of the class rewrites the `use` line too.
((namespace_use_declaration
  (namespace_use_clause . (qualified_name (name) @ref.type)) @_clause)
 (#not-match? @_clause "^(function|const)\\s"))
((namespace_use_declaration
  (namespace_use_clause . (name) @ref.type) @_clause) @_decl
 (#not-match? @_decl "^use\\s+(function|const)\\s")
 (#not-match? @_clause "^(function|const)\\s"))
((namespace_use_declaration
  body: (namespace_use_group
    (namespace_use_clause . (name) @ref.type) @_clause)) @_decl
 (#not-match? @_decl "^use\\s+(function|const)\\s")
 (#not-match? @_clause "^(function|const)\\s"))
