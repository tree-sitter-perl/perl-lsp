; PHP stdlib string-callables (bundled overlay).
;
; The engine carries no builtin names — the `#any-of?` predicates here are
; the whole surface (same doctrine as the framework overlays). A string
; argument in a callable-taking builtin's callback SLOT names a function:
; the minted ref is an ordinary FunctionCall riding `@ref.call.named`, so
; references count the site and rename rewrites the characters between the
; quotes. `function_exists`/`is_callable` guards are uses too — a rename
; that skips them leaves a stale guard probing the old name.
;
; Positional scope: only builtins whose callback slot is FIXED (arg 0 or
; arg 1) are listed. The variadic-tail family (`array_udiff`,
; `array_uintersect_assoc`, … — callback LAST) and arbitrary key-position
; forms (`'sanitize_callback' => 'fn'`) stay parked; a positional pattern
; can't name them without over-matching data strings.

; callback / name in argument 0
(function_call_expression
  function: (name) @_cb0
  arguments: (arguments
    . (argument (string (string_content) @ref.call.named)))
  (#any-of? @_cb0
    "array_map"
    "call_user_func"
    "call_user_func_array"
    "forward_static_call"
    "forward_static_call_array"
    "function_exists"
    "is_callable"
    "ob_start"
    "register_shutdown_function"
    "register_tick_function"
    "set_error_handler"
    "set_exception_handler"
    "spl_autoload_register"
    "spl_autoload_unregister"))

; callback in argument 1
(function_call_expression
  function: (name) @_cb1
  arguments: (arguments
    . (argument)
    . (argument (string (string_content) @ref.call.named)))
  (#any-of? @_cb1
    "array_filter"
    "array_reduce"
    "array_walk"
    "array_walk_recursive"
    "preg_replace_callback"
    "uasort"
    "uksort"
    "usort"))
