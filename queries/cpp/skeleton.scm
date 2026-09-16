; C++ language pack — Tier 1 (ring-1 skeleton): outline, scopes,
; namespaces, includes, calls. The capture vocabulary is the
; language-neutral contract the driver reads; node kinds are C++'s.
;
; What the preprocessor costs this tier is MEASURED by the obstacle
; course (cpp_obstacle.rs): declaration-generating macros (ring 3) are
; invisible here by construction, and declarator-position macros
; corrupt the parse (a `class API_EXPORT Foo` reparses as a function) —
; that damage is the input to the preprocessing-design question, not a
; bug in these patterns.

; ---- includes: the import edge (header path is the module name).
; capture the string CONTENT for quoted paths so the cache key is the
; clean relative path ("util.h", not "\"util.h\""); system <...>
; headers have no content node, so keep the whole token. ----
(preproc_include path: (string_literal (string_content) @import.name))
(preproc_include path: (system_lib_string) @import.name)

; ---- #define macros become SYMBOLS (completion / goto-def / outline).
; For a macro-heavy API (perl5: Newx/SvPV; embedded HALs) the macros ARE
; the surface. Object-like (`#define MAX 1`) → a constant (Variable);
; function-like (`#define MIN(a,b) ...`) → a callable (Sub).
(preproc_def name: (identifier) @def.var.name) @def.var
; a distinct skel-kind (not the plain "sub" a real function uses) so the
; Symbol can be tagged "macro" (hover/completion say "macro", not
; "function") while still resolving as a callable Sub everywhere else.
(preproc_function_def name: (identifier) @def.macro.name) @def.macro
; An object-like macro whose body is a bare type spelling is a TYPE ALIAS the
; same as a `typedef` — `#define PERL_BITFIELD16 U16` / `#define BITF16 unsigned`.
; The alias graph resolves it (incl. cross-file, since the #define is a
; file-scope symbol): a field typed `PERL_BITFIELD16` in another header chases
; through to `unsigned short`. Non-type bodies (`#define MAX 100`) are gated out
; at emission by `annot_type`.
(preproc_def name: (identifier) @macro.alias.name value: (preproc_arg) @macro.alias.of)

; ---- namespaces: a Package SYMBOL (so its members nest under it in the
; outline) + a sticky context + a real scope for its body ----
(namespace_definition
  name: (namespace_identifier) @def.package.name @context.namespace
  body: (declaration_list) @scope) @def.package

; `inline namespace v11` — a name-only sibling (the def/scope/context still
; come from the base pattern above; an optional "inline"? capture there would
; kill the pattern's other captures). The extractor joins by name span and
; tags the Package symbol "inline", so the qualified-completion gather lifts
; its members into the enclosing namespace (C++ inline-namespace
; transparency).
(namespace_definition
  "inline"
  name: (namespace_identifier) @ns.inline)

; ---- type defs: class / struct / union / enum ----
; @context.class tags the body's members with the class name (package),
; so member completion (`obj.`) and symbol_in_class resolve them.
(class_specifier
  name: (type_identifier) @def.class.name @context.class
  body: (field_declaration_list) @scope) @def.class
(struct_specifier
  name: (type_identifier) @def.class.name @context.class
  body: (field_declaration_list) @scope) @def.class

; out-of-line nested class definition `class Outer::Inner { ... }` — the
; name is a qualified_identifier; @qualifier carries the `Outer::` owner.
(class_specifier
  name: (qualified_identifier
    scope: (_) @qualifier
    name: (type_identifier) @def.class.name @context.class)
  body: (field_declaration_list) @scope) @def.class

; ---- class template specializations (full `template<> struct X<A>` and
; partial `struct X<T*>`): the name is a `template_type`, and the spec is
; its OWN Class (per-spec identity — a spec REPLACES the primary's members
; wholesale, so it must own a distinct member table; fork 4 of
; docs/adr/cpp-templates.md). The symbol/package name is the canonical
; template spelling (`formatter<int, char>` — see
; `canonical_template_spelling`); @spec.primary records the base name so
; extraction mints the `Specializes` family edge (goto-implementation
; traverses it; member resolution never does). The inner `type_identifier`
; also fires the @ref.type catch-all, so gr/rename on the primary reach
; each spec's name token. ----
(class_specifier
  name: (template_type name: (type_identifier) @spec.primary) @def.class.name @context.class
  body: (field_declaration_list) @scope) @def.class
(struct_specifier
  name: (template_type name: (type_identifier) @spec.primary) @def.class.name @context.class
  body: (field_declaration_list) @scope) @def.class

; ---- template parameter names, joined to the class they parameterize
; (`template <typename T, class U> class Box` → Box's params [T, U];
; a partial spec `template <typename T> struct fmt<vector<T>>` keys its
; params under the spec's canonical spelling). One match per param; the
; driver orders by source position. This is the substitution axis
; instantiation-aware typing reads (`FileAnalysis.template_params`).
(template_declaration
  parameters: (template_parameter_list
    [(type_parameter_declaration (type_identifier) @tmpl.param)
     (optional_type_parameter_declaration name: (type_identifier) @tmpl.param)])
  [(class_specifier name: (_) @tmpl.owner)
   (struct_specifier name: (_) @tmpl.owner)])

; ---- inheritance: `class Circle : public Shape` → Circle parent Shape.
; A dedicated pattern (non-inheriting classes keep matching the body
; pattern above); one @parent per base, so multiple inheritance works.
(class_specifier
  name: (type_identifier) @def.class.name
  (base_class_clause (type_identifier) @parent))
(struct_specifier
  name: (type_identifier) @def.class.name
  (base_class_clause (type_identifier) @parent))
; template BASE (`struct D : base<T>` / the fmt idiom `formatter<X> :
; formatter<string_view>`): TWO parent edges per base — the canonical
; template spelling (joins a per-spec Class when one exists) and the bare
; base name (joins the primary; the dependent `base<T>` spelling names no
; class, so member resolution falls through to the primary). The walk's
; seen-set dedups; a miss on either edge resolves to nothing, harmlessly.
(class_specifier
  name: [(type_identifier) (template_type)] @def.class.name
  (base_class_clause (template_type name: (type_identifier) @parent) @parent))
(struct_specifier
  name: [(type_identifier) (template_type)] @def.class.name
  (base_class_clause (template_type name: (type_identifier) @parent) @parent))
; spec defs inherit through plain bases too
(class_specifier
  name: (template_type) @def.class.name
  (base_class_clause (type_identifier) @parent))
(struct_specifier
  name: (template_type) @def.class.name
  (base_class_clause (type_identifier) @parent))
; namespace-QUALIFIED bases (`: public detail::buffer<T>`, `: detail::tag`)
; — the fmt idiom. The qualifier drops (classes key unqualified, same as
; `annot_type`); the template form gets the same two edges as above.
(class_specifier
  name: [(type_identifier) (template_type)] @def.class.name
  (base_class_clause
    (qualified_identifier
      name: (template_type name: (type_identifier) @parent) @parent)))
(struct_specifier
  name: [(type_identifier) (template_type)] @def.class.name
  (base_class_clause
    (qualified_identifier
      name: (template_type name: (type_identifier) @parent) @parent)))
(class_specifier
  name: [(type_identifier) (template_type)] @def.class.name
  (base_class_clause
    (qualified_identifier name: (type_identifier) @parent)))
(struct_specifier
  name: [(type_identifier) (template_type)] @def.class.name
  (base_class_clause
    (qualified_identifier name: (type_identifier) @parent)))
; out-of-line nested class WITH a base (`class Block::Iter : public Iterator`,
; `class Version::LevelFileNumIterator : public Iterator`): the class NAME is a
; qualified_identifier (the def pattern above files it under its inner
; type_identifier). The base patterns above only match a bare/template name, so
; a qualified-named subclass never minted its `@parent` edge — invisible to the
; INHERITS_INV implementations walk. Capture the SAME inner name as
; @def.class.name (so the child edge joins the identity the class filed under)
; plus every base spelling (bare / template / namespace-qualified).
(class_specifier
  name: (qualified_identifier name: (type_identifier) @def.class.name)
  (base_class_clause (type_identifier) @parent))
(struct_specifier
  name: (qualified_identifier name: (type_identifier) @def.class.name)
  (base_class_clause (type_identifier) @parent))
(class_specifier
  name: (qualified_identifier name: (type_identifier) @def.class.name)
  (base_class_clause (template_type name: (type_identifier) @parent) @parent))
(struct_specifier
  name: (qualified_identifier name: (type_identifier) @def.class.name)
  (base_class_clause (template_type name: (type_identifier) @parent) @parent))
(class_specifier
  name: (qualified_identifier name: (type_identifier) @def.class.name)
  (base_class_clause
    (qualified_identifier name: (type_identifier) @parent)))
(struct_specifier
  name: (qualified_identifier name: (type_identifier) @def.class.name)
  (base_class_clause
    (qualified_identifier name: (type_identifier) @parent)))
(union_specifier name: (type_identifier) @def.class.name) @def.class
; a BODIED named union additionally scopes its members (outline nesting +
; the hover overlay's sibling group) and tags them with the union's name.
; `@def.union` (not `@def.class`) so the Symbol carries the "union"
; attribute the overlay/outline consumers key on; the bare pattern above
; still fires, and the type-kind family dedup keeps the union-tagged row.
(union_specifier
  name: (type_identifier) @def.union.name @context.class
  body: (field_declaration_list) @scope) @def.union
(enum_specifier name: (type_identifier) @def.class.name) @def.class

; ---- C typedef'd aggregates: `typedef struct { ... } Name;` — the
; dominant C type idiom (the anonymous struct has no name of its own, so
; the typedef NAME is the type). The name comes AFTER the body, so
; @context.class can't tag the members (already walked) — a body-scope
; post-pass in into_file_analysis does that instead.
(type_definition
  type: (struct_specifier body: (field_declaration_list) @scope)
  declarator: (type_identifier) @def.class.name) @def.class
(type_definition
  type: (union_specifier body: (field_declaration_list) @scope)
  declarator: (type_identifier) @def.union.name) @def.union
(type_definition
  type: (enum_specifier)
  declarator: (type_identifier) @def.class.name) @def.class

; ---- field-unions: a union declared inline as a struct member. The
; members stay flat on the ENCLOSING struct for completion/refs (C's
; access model — sticky @context.class tags them), but the outline nests
; them under a container: the field itself when named (`op_pmreplrootu`),
; a synthetic `(union)` node when anonymous. The body @scope is the
; overlay's sibling group: members sharing it overlay the same storage.
(field_declaration
  type: (union_specifier body: (field_declaration_list) @scope)
  declarator: (field_identifier)? @def.unionfield.name) @def.unionfield
; a field typed by an ANONYMOUS aggregate (`struct { int ping; } data;`
; inside a named union/struct): the anon members are flattened onto the
; enclosing NAMED container (sticky @context.class), so the field's own
; type IS that container — the anon hop is identity. `u->data.ping` then
; resolves `ping` where the model put it, including cross-file.
(field_declaration
  type: [(struct_specifier !name body: (_)) (union_specifier !name body: (_))]
  declarator: (field_identifier) @anonagg.member)
; enum CONSTANTS (RED, GREEN) — named values, findable + completable.
; @def.enumerator (not @def.var) marks them so the extractor can tag each
; with its parent enum (span-contained): `enum Color { RED }` → RED's
; container + type is `Color`, so hover renders `RED: Color` the same
; `name: type` way a struct field does. They stay in the ENCLOSING scope
; (C enumerators leak out of the enum body — no @scope), so a bare
; `x = RED` still resolves to the enumerator.
(enumerator name: (identifier) @def.enumerator.name) @def.enumerator
; scalar / function-pointer typedefs: `typedef uint32_t u32;`,
; `typedef void (*CB)(int);` — the named alias is a findable type. (The
; struct/union/enum forms above already matched with a @scope; the
; name-dedup in into_file_analysis collapses the overlap.)
(type_definition
  declarator: (type_identifier) @def.class.name) @def.class
; the fn-ptr name sits inside a parenthesized_declarator:
; `(*CB)` = parenthesized_declarator > pointer_declarator > type_identifier.
; A pointer-RETURNING fn-ptr (`typedef void *(*loader)(int);`) wraps the
; whole function_declarator in one more pointer_declarator — second pattern.
(type_definition
  declarator: (function_declarator
    declarator: (parenthesized_declarator
      (pointer_declarator
        declarator: (type_identifier) @def.class.name)))) @def.class
(type_definition
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (parenthesized_declarator
        (pointer_declarator
          declarator: (type_identifier) @def.class.name))))) @def.class

; ---- scalar / primitive / alias-chain typedefs → the alias EDGE. ----
; `typedef unsigned short U16;`, `typedef uint32_t u32;`, `typedef V16 W16;`
; — the underlying is a SCALAR (a struct/union/enum, bodied or bare tag,
; aliases through @parent above, so it's excluded here). @alias.of carries
; the underlying type TEXT, joined to @alias.name by match; the extractor
; mints a `TypeName(alias) → <underlying>` witness so a declared `U16 x;`
; chases the alias to its leaf type (`unsigned short`) for hover / typing.
; A `template_type` underlying (`typedef vec<int> IntVec;`) rides the same
; edge — `annot_type` peels the spelling into the `Instance` flavor, so an
; `IntVec v; v.size()` dispatches through the template base.
(type_definition
  type: [(primitive_type) (sized_type_specifier) (type_identifier) (template_type)] @alias.of
  declarator: (type_identifier) @alias.name)
; C++ `using U16 = unsigned short;` — same alias, `alias_declaration` shape.
(alias_declaration
  name: (type_identifier) @alias.name
  type: (type_descriptor) @alias.of)

; typedef of a NAMED tag whose body is elsewhere: `typedef struct op OP;`
; (perl5's dominant idiom — `struct op` is defined in op.h, OP is the public
; name). OP is an ALIAS for the tag, so record the tag as OP's @parent: member
; completion + goto-def then see through the alias to `struct op`'s fields via
; the cross-file ancestor walk. (The bodied forms above already give the tag
; its own members; this only adds the alias edge.)
(type_definition
  type: (struct_specifier name: (type_identifier) @parent)
  declarator: (type_identifier) @def.class.name) @def.class
(type_definition
  type: (union_specifier name: (type_identifier) @parent)
  declarator: (type_identifier) @def.class.name) @def.class
(type_definition
  type: (enum_specifier name: (type_identifier) @parent)
  declarator: (type_identifier) @def.class.name) @def.class

; ---- free functions & out-of-line / inline method definitions ----
; the name lives at the bottom of the declarator chain; one pattern per
; shape it can take (plain / member / qualified / pointer-return).
;
; @scope is minted SEPARATELY, by the universal `(function_definition) @scope`
; below — one pattern for EVERY body shape (operator[]/operator=/conversion
; operators/constructors/destructors/templated/out-of-line), so no function's
; body ever leaks into the enclosing class scope. The name patterns here only
; carry @def; they are not the scope source (a name-shaped scope source would
; miss the operator/cast/in-class-destructor declarator shapes).
; A free function carries its declared return type like a method does, so a
; call (and a function-like macro delegating to it) types through the sub-return
; path. A type-less definition (a constructor `Foo(){}`, K&R) still matches the
; second, rettype-free pattern; @def.sub dedups by name in `into_file_analysis`.
(function_definition
  type: (_) @rettype
  declarator: (function_declarator
    declarator: (identifier) @def.sub.name)) @def.sub
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @def.sub.name)) @def.sub
(function_definition
  type: (_) @rettype
  declarator: (function_declarator
    declarator: (field_identifier) @def.method.name)) @def.method
; Trailing returns (`auto f() -> T`) — SIBLING patterns, not an optional
; tail on the ones above (a quantified capture silently kills the whole
; pattern's other captures). The descriptor's `type:` field (not the whole
; descriptor) drops cv-qualifiers/pointers, matching how leading returns
; extract. The leading pattern still fires with `auto` (annot_type → None);
; the name-span dedup in `into_file_analysis` keeps this rettype-bearing
; row. No @scope.sub here — the base patterns already mint the scope.
(function_definition
  declarator: (function_declarator
    declarator: (identifier) @def.sub.name
    (trailing_return_type (type_descriptor type: (_) @rettype)))) @def.sub
(function_definition
  declarator: (function_declarator
    declarator: (field_identifier) @def.method.name
    (trailing_return_type (type_descriptor type: (_) @rettype)))) @def.method
(field_declaration
  declarator: (function_declarator
    declarator: (field_identifier) @def.method.name
    (trailing_return_type (type_descriptor type: (_) @rettype)))) @def.method
; out-of-line definitions `RetT Class::method(...) { ... }` (incl.
; pointer/reference returns, multi-level qualifiers `A::B::m`, and
; ctors/dtors with no return type). ONE general capture per return-type
; presence — the driver peels the declarator (pointer/reference/parenthesized,
; any depth) to the function declarator, then walks the qualified name to its
; leaf (the member name) + owning class. This fires for EVERY function_definition;
; a non-qualified declarator (free function / in-class method, owned by the
; patterns above/below) yields nothing. `!type` splits the ctor/dtor shape off
; (a `?`-quantified type capture would kill the sibling capture). No @scope here
; — the universal `(function_definition) @scope.sub` mints it.
(function_definition type: (_) @rettype) @ool.def
(function_definition !type) @ool.def
(function_definition
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (identifier) @def.sub.name))) @def.sub
; rettype-bearing sibling: a pointer-returning free function's declared
; type (`Widget *mkStruct(...)`) rides on the `type:` field, sibling to the
; `pointer_declarator` — so `mkStruct()->field` types the call receiver
; through the same sub-return path a value-returning def already uses.
; Name-span dedup (`upgrade_ret`) keeps THIS copy over the rettype-free one.
(function_definition
  type: (_) @rettype
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (identifier) @def.sub.name))) @def.sub

; every function body is a lexical scope — one node-kind, so operator methods
; (`operator[]`/`operator=`), conversion operators (`operator bool()`),
; constructors (with or without member-init lists), destructors (in-class
; `~S()` + out-of-line `S::~S()`), templated methods, and out-of-line
; `Ret Class::m()` bodies ALL mint a @scope, not just the plain/field/qualified
; declarator shapes the name patterns above enumerate. Params sit inside the
; function_definition span, so they scope to the function (drives declared-type
; inference); the scope-based moved-from region + narrowing cutoff stay
; bounded instead of leaking across scope-less sibling functions.
; `@scope.sub` (not plain `@scope`): a function's params/locals are
; sub-body content — `scope_within_sub_body` shields them from the outline
; and keeps them out of the class-content lane a sticky class package
; would otherwise drag them into.
(function_definition) @scope.sub

; ---- top-level / namespaced function prototypes (the bulk of any
; header file) — a `declaration`, not a `function_definition`. A
; prototype has no body, so its parameter_list is the whole signature
; region: `@scope.sub` there keeps the params (referenced by nothing)
; out of the outline and the class-content lane. ----
(declaration
  declarator: (function_declarator
    declarator: (identifier) @def.sub.name
    parameters: (parameter_list) @scope.sub)) @def.sub
(declaration
  declarator: (function_declarator
    declarator: (qualified_identifier
      scope: (_) @qualifier
      name: (identifier) @def.method.name)
    parameters: (parameter_list) @scope.sub)) @def.method
; rettype-bearing siblings for the prototypes above: a header-only decl
; (`Widget *mkStruct(void);`) is the ONLY declaration of a function whose
; body lives elsewhere, so its `type:` is the sole return-type source — a
; call receiver (`mkStruct()->field`) has nothing else to type through.
; Rettype-free sibling stays as the fallback (implicit-int / typeless), and
; name-span dedup keeps this rettype-bearing copy when both fire.
(declaration
  type: (_) @rettype
  declarator: (function_declarator
    declarator: (identifier) @def.sub.name
    parameters: (parameter_list) @scope.sub)) @def.sub
(declaration
  type: (_) @rettype
  declarator: (function_declarator
    declarator: (qualified_identifier
      scope: (_) @qualifier
      name: (identifier) @def.method.name)
    parameters: (parameter_list) @scope.sub)) @def.method
; pointer-returning prototypes (`struct T *make_t(int a);`): the
; function_declarator nests inside a pointer_declarator, same as the
; definition form above — without this the decl is dropped and its
; params leak as top-level Variables.
(declaration
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (identifier) @def.sub.name
      parameters: (parameter_list) @scope.sub))) @def.sub
(declaration
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (qualified_identifier
        scope: (_) @qualifier
        name: (identifier) @def.method.name)
      parameters: (parameter_list) @scope.sub))) @def.method
; rettype-bearing siblings for the pointer-returning prototypes: same story
; as the plain prototype above — `type:` is the sole return-type source for a
; header-only pointer-returning decl, so the call-receiver field-access path
; can resolve. Rettype-free sibling stays as fallback; dedup keeps this copy.
(declaration
  type: (_) @rettype
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (identifier) @def.sub.name
      parameters: (parameter_list) @scope.sub))) @def.sub
(declaration
  type: (_) @rettype
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (qualified_identifier
        scope: (_) @qualifier
        name: (identifier) @def.method.name)
      parameters: (parameter_list) @scope.sub))) @def.method

; ---- in-class method declarations (prototypes) & member fields ----
; @rettype carries the declared return type → the method's return-type
; witness (drives `box.getInner().` chaining through MethodOnClass).
(field_declaration
  type: (_) @rettype
  declarator: (function_declarator
    declarator: (field_identifier) @def.method.name
    parameters: (parameter_list) @scope.sub)) @def.method
; pointer- / reference-returning methods (`Foo* m()`, `Foo& m()`):
; the function_declarator nests inside a pointer/reference wrapper.
(field_declaration
  type: (_) @rettype
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (field_identifier) @def.method.name
      parameters: (parameter_list) @scope.sub))) @def.method
(field_declaration
  type: (_) @rettype
  declarator: (reference_declarator
    (function_declarator
      declarator: (field_identifier) @def.method.name
      parameters: (parameter_list) @scope.sub))) @def.method
; operator overloads: the declarator name is an `operator_name` token
; (`operator+`, `operator<<`), not an identifier/field_identifier, so the
; name patterns above never see them. In-class decls are Methods; free
; forms mint @def.sub (the Sub-owned-by-class reclassification in
; `into_file_analysis` flips in-class definitions to Method). Out-of-line
; `Ret Vec2::operator+(...)` joins its class via @qualifier.
(field_declaration
  type: (_) @rettype
  declarator: (function_declarator
    declarator: (operator_name) @def.method.name
    parameters: (parameter_list) @scope.sub)) @def.method
(declaration
  declarator: (function_declarator
    declarator: (operator_name) @def.sub.name
    parameters: (parameter_list) @scope.sub)) @def.sub
(function_definition
  type: (_) @rettype
  declarator: (function_declarator
    declarator: (operator_name) @def.sub.name)) @def.sub
(function_definition
  declarator: (function_declarator
    declarator: (operator_name) @def.sub.name)) @def.sub
; reference- / pointer-returning inline operator DEFINITIONS
; (`T& operator[](...) { ... }`, `T* operator->() { ... }`): the return
; wrapper nests the function_declarator, exactly like the field-decl operator
; forms above. Without these the inline body's method symbol is never minted,
; so the implicit-`this` sibling-pin's enclosing-class lookup (which joins a
; body scope to its owning method SYMBOL) dead-ends and a bare sibling call
; inside the operator body resolves nowhere.
(function_definition
  type: (_) @rettype
  declarator: (reference_declarator
    (function_declarator
      declarator: (operator_name) @def.sub.name))) @def.sub
(function_definition
  type: (_) @rettype
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (operator_name) @def.sub.name))) @def.sub
; pointer-/reference-returning operator decls (`Vec2& operator+=(...)`)
(field_declaration
  type: (_) @rettype
  declarator: (reference_declarator
    (function_declarator
      declarator: (operator_name) @def.method.name
      parameters: (parameter_list) @scope.sub))) @def.method
(field_declaration
  type: (_) @rettype
  declarator: (pointer_declarator
    declarator: (function_declarator
      declarator: (operator_name) @def.method.name
      parameters: (parameter_list) @scope.sub))) @def.method

; destructor `~Widget()` — tree-sitter parses it as a `declaration` (no
; return type), with a `destructor_name` declarator, so the field_declaration
; method patterns above miss it. @def.sub + the in-class method
; reclassification make it a Method. (Constructors are field_identifiers
; and already match.)
(declaration
  declarator: (function_declarator
    declarator: (destructor_name) @def.sub.name)) @def.sub
; (out-of-line `Class::~Class() {...}` / `Class::Class() {...}` definitions are
; captured by the general `@ool.def` patterns above — the qualifier walk reaches
; a `destructor_name` / ctor-`identifier` leaf like any other member name.)

; ---- explicit instantiation (`template struct X<int>;` / `template void
; f<char>(..);` — fmt's src/format.cc is entirely this shape). It is a USE
; of the named template, not a def-with-body — but a deliberate,
; enumerable one, so it mints an outline symbol (fork 2 of
; docs/adr/cpp-templates.md): the class form under the canonical
; instantiation spelling, the function form under the function's name
; (qualified forms join their class via @qualifier, whose template_type
; is peeled to the base name — `buffer<char>::append` files under
; `buffer`). The template NAME token inside fires the @ref.type /
; @expr.read catch-alls, so gr on the primary reaches the site; renaming
; the primary rewrites it. The node-wide `@scope.sub` swallows the
; signature's parameter_declarations, so this shape can't leak `loc`/`x`
; as top-level Variables. ----
(template_instantiation) @scope.sub
(template_instantiation
  type: (struct_specifier name: (template_type) @def.class.name)) @def.class
(template_instantiation
  type: (class_specifier name: (template_type) @def.class.name)) @def.class
(template_instantiation
  type: (union_specifier name: (template_type) @def.class.name)) @def.class
(template_instantiation
  declarator: (function_declarator
    declarator: (identifier) @def.sub.name)) @def.sub
(template_instantiation
  declarator: (pointer_declarator
    (function_declarator
      declarator: (identifier) @def.sub.name))) @def.sub
(template_instantiation
  declarator: (function_declarator
    declarator: (template_function name: (identifier) @def.sub.name))) @def.sub
(template_instantiation
  declarator: (function_declarator
    declarator: (qualified_identifier
      scope: (_) @qualifier
      name: (identifier) @def.method.name))) @def.method
(template_instantiation
  declarator: (function_declarator
    declarator: (qualified_identifier
      scope: (_) @qualifier
      name: (template_function name: (identifier) @def.method.name)))) @def.method

; ---- `using X = T;` aliases (plain and template): the name is a findable,
; renamable TYPE symbol; the @alias.name/@alias.of witness below carries
; the same edge it always did (the symbol adds identity, not typing). ----
(alias_declaration
  name: (type_identifier) @def.class.name) @def.class

; ---- `using Base::insert;` inside a class body: a member RE-EXPORT — an
; import edge, not a definition. It mints an outline entry under the class
; (the re-exported name IS part of the class's API) carried as a Method
; with the "reexport" attribute; member resolution sees THROUGH it to the
; origin (`method_resolution_on_class` skips reexports), so hover/gd land
; on the ancestor's real def. Scoped to field_declaration_list so a
; namespace-level `using std::swap;` mints nothing. ----
(field_declaration_list
  (using_declaration
    (qualified_identifier
      name: (identifier) @def.reexport.name)) @def.reexport)

; ---- concepts: the name is a findable type-level symbol; the
; requires-expression's parameters (`requires(T a, T b)`) are signature
; machinery, not file structure — `@scope.sub` shields them like any
; other sub-body content. ----
(concept_definition
  name: (identifier) @def.class.name) @def.class
(requires_expression) @scope.sub

; a struct/class DATA MEMBER — structurally distinct from a plain variable
; (the `field_declaration` node only appears inside a class/struct body),
; so it gets its own kind (hover/outline "field", not "variable") without
; any name-based guessing.
(field_declaration
  declarator: (field_identifier) @def.field.name) @def.field
; a data member's TYPE — the type witness needs field_declaration (the
; `declaration` patterns below only see locals). Only plain-field
; declarators match (a function_declarator is a method, not a field), so
; member-access chains (`box.inner.`) can type `inner` on its class.
(field_declaration
  type: (_) @type.annot
  declarator: (field_identifier) @flow.target)
; pointer / reference data members of any depth (`Box* inner;`, `Node** next;`).
; The leaf is a field_identifier, so core mints a @def.var (a class member),
; not a @def.local — `peel_nested`'s leaf→def-capture map handles that.
(field_declaration
  type: (_) @type.annot
  declarator: [(pointer_declarator) (reference_declarator)] @nested.target)

; ---- C goto labels: `done:` is a nav target, `goto done;` jumps to it.
; The def is an unpackaged Variable symbol (outline-hidden, like a local);
; the goto resolves to it function-wide (order-independent — forward gotos).
(labeled_statement label: (statement_identifier) @def.label)
(goto_statement label: (statement_identifier) @ref.label)

; ---- calls ----
(call_expression function: (identifier) @ref.call) @expr.call

; qualified calls (`fmt::format_to(...)`, `detail::vformat_to(...)`,
; `Widget::create(...)`): the whole path is captured; extraction narrows the
; ref span to the bare name token and carries the qualifier as
; `resolved_package` — the namespace-participation half of gr identity.
(call_expression function: (qualified_identifier) @ref.qcall) @expr.call

; ---- overload arity ranking fuel (structural counts only; "which overload
; fits" is downstream interpretation, `ParamArity::fit`). An arg list is
; joined to its callee ref by adjacency (`ref.end == arglist.start`); a
; def's parameter list is joined to the def by span containment. ----
(argument_list) @arity.args
(function_declarator parameters: (parameter_list) @arity.sig)

; ---- member access (`recv.field` / `recv->field`, AND `recv.method(...)`):
; the field is the "method", the receiver subtree the invocant. Mints the same
; MethodCall ref core resolves for Perl `$obj->m` — goto-def / hover /
; references / rename all flow from it. @member.recv carries the receiver
; (span+text) for query-time typing via expr_type_at_span. The trailing `()`
; of a method call doesn't change the reference, so calls + plain field access
; share one pattern.
(field_expression
  argument: (_) @member.recv
  operator: _ @member.op
  field: (field_identifier) @ref.member)

; The CALLED form additionally mints a chain-hop witness on the whole call's
; span (`@hop.call` + `@hop.member` — deliberately NOT `@ref.member`, the
; pattern above already minted the ref): `w.get().spin()` types through the
; receiver span's own hop with no intermediate variable.
(call_expression
  function: (field_expression
    argument: (_) @member.recv
    field: (field_identifier) @hop.member)
  arguments: (argument_list) @arity.args) @hop.call

; ---- domain typing (int-used-as-enum): a struct-field SLOT compared or
; assigned against ANY value. `o->op_type == OP_CONST` / `o->op_type =
; OP_FREED` / `o->op_targ = pad_alloc(...)`. @domain.slot is the field access,
; @domain.value the other operand — an enumerator carries its `enum`, resolved
; cross-file at query time, then the sites fold onto `Field{owner,name}`
; (op_type → opcode). The value is deliberately UNGATED (`(_)`): a site whose
; operand is NOT an enumerator (integer literal, arithmetic, a call) is
; counter-evidence in the coherence vote's denominator — capturing only
; enum-shaped operands made a dominantly-plain-int slot look 100% coherent.
; Both operand orders; no operator gate. ----
(binary_expression
  left: (field_expression field: (field_identifier) @domain.slot)
  right: (_) @domain.value)
(binary_expression
  left: (_) @domain.value
  right: (field_expression field: (field_identifier) @domain.slot))
(assignment_expression
  left: (field_expression field: (field_identifier) @domain.slot)
  right: (_) @domain.value)

; ---- type witnesses: C++ leaks types at every DECLARATION site (its
; static-typing richness — the annot_type predicate carries the load).
; `T x = init;` emits both the declared-type witness and a flow edge to
; the initializer; `T x;` emits the declared type alone. `auto` defers
; to the edge (annot_type returns None), driving the cross-var chase. ----
(declaration
  type: (_) @type.annot
  declarator: (init_declarator
    declarator: (identifier) @flow.target @def.local
    value: (_) @flow.source))
; The bare (initializer-less) form also captures the storage class as a
; symbol attribute: `extern struct redisServer server;` DECLARES, the
; initializer-less `struct redisServer server;` DEFINES — goto-def's
; decl→def ranking asks the symbol (`attributes` carries "extern"), never
; a header-vs-TU shape branch. An initialized `extern int x = 5;` is a
; definition regardless, so the init form above deliberately skips the
; capture.
(declaration
  (storage_class_specifier)? @sym.attr
  type: (_) @type.annot
  declarator: (identifier) @flow.target @def.local)

; array declarations (`extern const unsigned char kPropertyBits[256];`,
; `int table[8] = {...};`): the declarator is an array_declarator wrapping the
; leaf identifier, which the plain-identifier forms above never see — so a
; NAMESPACE/file-scope array global was minted as NO symbol at all (dead
; goto-def, absent from outline). Capture the leaf as @def.local: the sticky
; namespace context tags it with its owning namespace (package) and a
; file/namespace-scope decl outlines, while a function-body array stays a
; scope-hidden local — the same scope-driven local-vs-global distinction the
; scalar forms already ride. (Bare + extern first, then the braced-init form.)
(declaration
  (storage_class_specifier)? @sym.attr
  type: (_) @type.annot
  declarator: (array_declarator
    declarator: (identifier) @flow.target @def.local))
(declaration
  type: (_) @type.annot
  declarator: (init_declarator
    declarator: (array_declarator
      declarator: (identifier) @flow.target @def.local)
    value: (_) @flow.source))

; pointer / reference locals of ANY depth, bare and initialized — `T* p;`,
; `T** pp;`, `T& r = x;`, `if (Derived* d = dynamic_cast<...>(b))`. The
; @nested.target chain is unravelled by core (see params). The init form also
; carries @flow.source so the initializer's type flows to the leaf.
(declaration
  (storage_class_specifier)? @sym.attr
  type: (_) @type.annot
  declarator: [(pointer_declarator) (reference_declarator)] @nested.target)
(declaration
  type: (_) @type.annot
  declarator: (init_declarator
    declarator: [(pointer_declarator) (reference_declarator)] @nested.target
    value: (_) @flow.source))

; ---- function PARAMETERS carry a type too (the dominant embedded site:
; `void f(Handle *h) { h->... }`). Value / pointer / reference forms;
; pointer-/reference-ness dropped for navigation, like locals. ----
(parameter_declaration
  type: (_) @type.annot
  declarator: (identifier) @flow.target @def.local)
; pointer / reference parameters of ANY depth — `Handle* h`, `OP** op_p`,
; `char**** x`, `Box*& rp`. One @nested.target capture; core (`peel_nested`)
; unravels the declarator chain to the leaf identifier + the deref stack
; (arbitrary depth, cv-qualifiers per level), emitting the leaf as a synthetic
; @flow.target/@def.local. The plain-value form above has no chain.
(parameter_declaration
  type: (_) @type.annot
  declarator: [(pointer_declarator) (reference_declarator)] @nested.target)

; ---- type-name uses: every `type_identifier` in type position is a
; REFERENCE to the named type (rule #7 — `Widget w;`, `struct op* o`, a
; base-class clause, a typedef/alias spelling, a type-alias macro like
; `PERL_BITFIELD16`). Minted as the same `PackageRef` a Perl package-name
; use carries, so type goto-def / find-references flow through the
; existing Package machinery. Def-site name tokens (a class/enum/typedef
; declaring its own name) are suppressed at extraction — a declaration is
; the Symbol, not a use of itself. ----
(type_identifier) @ref.type

; ---- literals + variable reads (the edge-chase substrate) ----
(number_literal) @expr.lit.number
(string_literal) @expr.lit.string
(true) @expr.lit.bool
(false) @expr.lit.bool
; A comparison / logical operator yields `bool` in C++ (unlike Perl's
; value-preserving `&&`/`||`, C++'s ARE boolean). Gate on the operator so
; an arithmetic `a + b` (same node kind) stays untyped and defers to the
; edge chase.
(binary_expression
  operator: ["==" "!=" "<" ">" "<=" ">=" "&&" "||"]) @expr.lit.bool
(identifier) @expr.read.var

; `return EXPR;` — the returned expression's OWN span already carries
; whatever witness the general rules above minted for its shape (a literal,
; a variable read, a member access, a call); this capture just marks the
; site so `into_file_analysis` can chain the enclosing function's `Symbol`
; onto it when the function has no declared return (`auto`) — cpp's side of
; Perl's implicit-return machinery, one arm per `return` statement.
(return_statement (_) @expr.return.value)

; ---- bind shapes + guard narrowing (the value-flow tier, cpp side) ----

; `for (auto x : items)` — the range-for var rebinds per element (a Rebind, no
; inflowing type yet) so the narrowing cutoff ends a region at the loop.
(for_range_loop
  declarator: (identifier) @flow.rebind)

; a plain `x = rhs` reassignment rebinds x with the rhs value — mints a Whole
; FlowEdge (like a declaration's init) so the moved-from region AND the
; narrowing cutoff end at the reassignment, via the same edge-driven cutoff.
(assignment_expression
  left: (identifier) @flow.target
  right: (_) @flow.source) @flow.assign

; `std::move(x)` leaves x in a moved-from (valid-but-unspecified) state: a
; subsequent READ of x before it is reassigned is a use-after-move bug.
; Capture the moved var + the whole call span; the minter checks scope/name
; against std/move (the driver has no query predicates). The moved-from region
; runs from the call to the first @flow rebind of x (or scope end) — the same
; cutoff the narrowing tier uses (`earliest_rebind_in`).
(call_expression
  function: (qualified_identifier
    scope: (namespace_identifier) @move.scope
    name: (identifier) @move.name)
  arguments: (argument_list (identifier) @move.var)) @move.call

; unevaluated operands — `noexcept(...)` / `sizeof(...)` / `decltype(...)`
; don't RUN their operand, so a `std::move` inside one never moves anything.
; Extraction records these regions and drops moves whose call sits inside one
; (the noexcept-specifier `noexcept(noexcept(T(std::move(b))))` is the dominant
; real-world spelling — the move there is a type-trait, not a move).
(noexcept) @unevaluated
(sizeof_expression) @unevaluated
(decltype) @unevaluated

; control-flow constructs — a `std::move` nested in one of these (relative to
; its enclosing @scope) is NOT straight-line, so the moved-from region can't be
; bounded without path-sensitivity and the use-after-move check stays silent
; (`FileAnalysis::use_after_move_reads` gate C). Braced if/else arms are their
; OWN @scope, so their region starts BEFORE the arm — the containment test
; (region strictly inside the move's scope) excludes them; only braceless
; arms, loops (bodies aren't scopes), switch (cases aren't scopes), the ternary,
; and preprocessor conditionals actually gate a move. Capturing the whole
; construct (not just braceless bodies) keeps the pattern uniform; the
; containment test does the braced/braceless discrimination.
(if_statement) @guard.region
(while_statement) @guard.region
(for_statement) @guard.region
(for_range_loop) @guard.region
(do_statement) @guard.region
(switch_statement) @guard.region
(conditional_expression) @guard.region
(preproc_if) @guard.region
(preproc_ifdef) @guard.region
(preproc_elif) @guard.region

; parameter lists — the use-after-move check reads these to tell a moved LOCAL
; (`Widget x;` in the body) from a moved PARAMETER. A moved parameter is
; overwhelmingly a forwarding / subobject-move idiom (move-constructors,
; `operator=`, perfect-forwarding wrappers) that reads sibling members after
; the move; separating those from a genuine bug needs subobject + path
; analysis this tier lacks, so parameter moves are not flagged (gate E).
(parameter_list) @param.region

; `if (dynamic_cast<Derived*>(b)) { b->... }` narrows b to Derived INSIDE the
; block — the cpp analog of python `isinstance`. The pack's narrow_guard maps
; `dynamic_cast` + the template type to the refinement; core scopes it to
; @scope and the edge-driven cutoff ends it at any rebind of b.
(if_statement
  condition: (condition_clause
    value: (call_expression
      function: (template_function
        name: (identifier) @narrow.guard
        arguments: (template_argument_list
          (type_descriptor type: (type_identifier) @narrow.type)))
      arguments: (argument_list (identifier) @narrow.var)))
  consequence: (compound_statement) @narrow.block)

; `std::optional<T>` engaged-state narrowing. Guard-testing an optional as
; engaged proves it HOLDS a T inside the block, so `opt->m` / `*opt` resolve on
; T there. No type token rides these guards (unlike dynamic_cast) — the pack's
; narrow_guard reads the subject's DECLARED type (std::optional<T>) and peels T,
; so the refinement keys on the type being optional, not on the guard name (a
; bare `if (ptr)` over a non-optional declares no inner type → no narrowing).
; Two clean engagement shapes: bare truthiness `if (opt)` (no @narrow.guard),
; and `if (opt.has_value())` (guard token gates the method — an arbitrary
; `opt.foo()` won't narrow). `!= std::nullopt` needs both operator + operand
; checks the one-token hook can't express, so it's left out.
(if_statement
  condition: (condition_clause value: (identifier) @narrow.var)
  consequence: (compound_statement) @narrow.block)
(if_statement
  condition: (condition_clause
    value: (call_expression
      function: (field_expression
        argument: (identifier) @narrow.var
        field: (field_identifier) @narrow.guard)))
  consequence: (compound_statement) @narrow.block)

; ---- branch arms are lexical scopes (conditional-move soundness) ----
; if/else arm bodies each mint a @scope, so a `std::move` in one arm bounds its
; moved-from region to THAT arm — a read in a sibling arm (or after the if) is
; in a different scope subtree and never false-flags. This is ALSO the scope a
; guard narrowing above attaches to: extraction joins the @narrow.block (the
; condition-tagged consequence) to the general arm @scope by block position, so
; the block mints exactly ONE scope (no fragile duplicate). Switch cases are not
; compound_statements, so a per-case region is a residual (a move+read across
; two `case:` labels still shares the switch-body scope).
(if_statement consequence: (compound_statement) @scope)
(if_statement alternative: (else_clause (compound_statement) @scope))
