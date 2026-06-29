; CMake language pack. The language is COMMAND-dispatched: `set`,
; `add_library`, and user functions are all (normal_command) — so defs
; come from the @cmd/@cmd.arg family, classified by the pack's
; cmd_effects predicate (case-insensitive, as CMake is).
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

; ---- every command: name + its FIRST argument (the entity it declares —
; `set(VAR ...)`, `add_library(TARGET ...)`; all cmake Def effects are
; name_arg:0). The `.` anchor is load-bearing: without it the pattern
; matched once PER argument, so `set(V ${X} PARENT_SCOPE)` named the var
; ref + the keyword as defs too. ----
(normal_command
  (identifier) @cmd
  (argument_list . (argument) @cmd.arg))
; commands with no arguments still get their invocation ref
(normal_command
  (identifier) @cmd)

; ---- variable references, including inside quoted strings ----
(variable_ref
  (normal_var (variable) @ref.var))
