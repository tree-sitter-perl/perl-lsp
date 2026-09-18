; CMake language pack. The language is COMMAND-dispatched: `set`,
; `add_library`, and user functions are all (normal_command) — so what a
; command DOES is one pattern per family, the effect in the capture and the
; command names in the pattern's own `#match?` (case-insensitive, as CMake
; is: the grammar keeps the identifier as written).
;
; The grammar parses ${VAR} inside quoted strings as real nodes —
; interpolated variable refs are free here.

; ---- function / macro defs: name = first argument, rest = params ----
(function_def
  (function_command
    (argument_list . (argument) @def.sub.name))) @def.sub @scope

(function_def
  (function_command
    (argument_list (argument) @def.var.name @def.var)))

(macro_def
  (macro_command
    (argument_list . (argument) @def.sub.name))) @def.sub @scope

; ---- every command: name + ALL args in ONE match, ordered. The `+`
; quantifier is load-bearing: `(argument)` alone matches once PER argument,
; so each arg landed in its own match at index 0 and `Def{name_arg:0}`
; named every one (`set(V ${X} PARENT_SCOPE)` → V + the ref + the keyword).
; With `+` the driver groups them by command and indexes correctly: arg 0
; is the def, RefArgsFrom refs the rest (keywords filtered). ----
(normal_command
  (identifier) @cmd
  (argument_list (argument)+ @cmd.arg))
; commands with no arguments still get their invocation ref
(normal_command
  (identifier) @cmd)

; ---- what each command family DOES. The argument POSITION is captured
; (`.` anchors the first), never an index a consumer would have to trust. ----
(normal_command
  (identifier) @cmd.def.var
  (argument_list . (argument) @cmd.def.name)
  (#match? @cmd.def.var "^(?i:set|option)$"))
; a target is a callable in everything but name: SymKind::Target is the real
; future; "sub" rides the full rename/refs machinery today.
(normal_command
  (identifier) @cmd.def.sub
  (argument_list . (argument) @cmd.def.name)
  (#match? @cmd.def.sub "^(?i:add_library|add_executable|add_custom_target)$"))
; every argument names something that already exists
(normal_command
  (identifier) @_cmd
  (argument_list (argument)+ @cmd.refargs)
  (#match? @_cmd "^(?i:target_link_libraries|target_include_directories|target_compile_definitions|target_sources)$"))
; the first argument names an included file
(normal_command
  (identifier) @_cmd
  (argument_list . (argument) @cmd.import)
  (#match? @_cmd "^(?i:include|add_subdirectory)$"))

; ---- variable references, including inside quoted strings ----
(variable_ref
  (normal_var (variable) @ref.var))
