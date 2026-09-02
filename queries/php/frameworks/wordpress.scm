; WordPress framework overlay — hook callback references.
;
; Framework VOCABULARY on the plugin doctrine (see laravel.scm's header):
; the engine carries no WordPress names — the `#any-of?` predicates here
; are the whole framework surface, and the minted refs are ordinary
; FunctionCall/MethodCall refs riding the string-named capture vocabulary
; (`@ref.call.named` / `@ref.method.named`, docs/prompt-pack-plugins.md).
;
; `add_action('init', 'wp_cron')`: the second argument's string CONTENT
; names a function. The ref's span is the content between the quotes, so
; references connect both directions and rename rewrites exactly those
; characters. Deliberately v1-scoped to the CALLBACK edge; hook-NAME
; identity (`do_action('init')` ↔ its registrations) is the Handler-rail
; follow-on.

(function_call_expression
  function: (name) @_wphook
  arguments: (arguments
    . (argument)
    . (argument (string . (string_content) @ref.call.named .)))
  (#any-of? @_wphook "add_action" "add_filter" "remove_action" "remove_filter"))

; Hook-NAME identity: `add_action('init', …)`'s FIRST argument declares
; the hook (a stacked Handler named by the string content), and
; `do_action('init')` / `apply_filters('the_title', …)` fire it (a
; DispatchCall labeled by the firing function via @dispatch.via).
; remove_* first-args are uses of the name too, so they join as
; dispatch refs. gd/references/rename connect registration ↔ firing.
(function_call_expression
  function: (name) @_wphook_def
  arguments: (arguments
    . (argument (string (string_content) @def.handler.named)))
  (#any-of? @_wphook_def "add_action" "add_filter"))

(function_call_expression
  function: (name) @dispatch.via
  arguments: (arguments
    . (argument (string (string_content) @ref.dispatch.named)))
  (#any-of? @dispatch.via
    "do_action" "do_action_ref_array"
    "apply_filters" "apply_filters_ref_array"
    "remove_action" "remove_filter"
    "has_action" "has_filter" "did_action" "doing_action" "doing_filter"))

; `add_action('save_post', array($this, 'on_save'))` / `[$this, 'on_save']`:
; the array's second element names a METHOD on the first element's object.
; The receiver rides `@member.recv` in the same match, so dispatch types
; through it exactly like a written `$this->on_save()` member ref.
(function_call_expression
  function: (name) @_wphook_m
  arguments: (arguments
    . (argument)
    . (argument (array_creation_expression
        . (array_element_initializer . (variable_name) @member.recv .)
        . (array_element_initializer . (string (string_content) @ref.method.named) .))))
  (#any-of? @_wphook_m "add_action" "add_filter" "remove_action" "remove_filter"))
