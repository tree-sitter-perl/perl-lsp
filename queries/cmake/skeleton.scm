; CMake language pack. The language is COMMAND-dispatched: `set`,
; `add_library`, and user functions are all (normal_command), so what a
; command DOES is one pattern per family: the effect is the capture, and the
; command names are the pattern's own `#match?` (case-insensitive, as CMake
; is; the grammar keeps the identifier as written).
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

; ---- every command is a call of its name. The `+` keeps all of a
; command's arguments in ONE match, so the driver counts them. ----
(normal_command
  (identifier) @cmd
  (argument_list (argument)+ @cmd.arg))
; commands with no arguments still get their invocation ref
(normal_command
  (identifier) @cmd)

; ---- what each command family does. `.` anchors the first argument, so the
; name a def or an import names is captured at its position. ----
(normal_command
  (identifier) @_cmd
  (argument_list . (argument) @def.var.name)
  (#match? @_cmd "^(?i:set|option)$")) @def.var
; a target is a callable in everything but name, so it rides `sub`'s
; rename and references.
(normal_command
  (identifier) @_cmd
  (argument_list . (argument) @def.sub.name)
  (#match? @_cmd "^(?i:add_library|add_executable|add_custom_target)$")) @def.sub
; every argument names a target or a source, except CMake's ALL-CAPS
; keyword arguments (PRIVATE, STATIC) and interpolations, which name no
; one symbol.
(normal_command
  (identifier) @_cmd
  (argument_list (argument) @ref.call)
  (#match? @_cmd "^(?i:target_link_libraries|target_include_directories|target_compile_definitions|target_sources)$")
  (#not-match? @ref.call "^[A-Z_]+$")
  (#not-match? @ref.call "\\$\\{"))
; the first argument names an included file or directory
(normal_command
  (identifier) @_cmd
  (argument_list . (argument) @import.arg)
  (#match? @_cmd "^(?i:include|add_subdirectory)$"))

; ---- variable references, including inside quoted strings ----
(variable_ref
  (normal_var (variable) @ref.var))
