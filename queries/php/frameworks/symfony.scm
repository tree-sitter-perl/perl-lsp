; Symfony framework patterns (bundled overlay).
;
; `EventSubscriberInterface::getSubscribedEvents()` returns a string->method
; map — every method-name string is a real dispatch target (the dispatcher
; calls it by name at runtime), so each mints the self-flavored named-method
; ref (`@ref.method.named.self`: a member ref on the ENCLOSING class — the
; same capture PHPUnit's #[DataProvider] argument uses). Three value shapes:
; 'event' => 'method', 'event' => ['method', $priority], and
; 'event' => [['method1', $p], ['method2']].

(method_declaration
  name: (name) @_gse
  body: (compound_statement
    (return_statement
      (array_creation_expression
        (array_element_initializer
          (_)
          (string (string_content) @ref.method.named.self) .))))
  (#eq? @_gse "getSubscribedEvents"))

(method_declaration
  name: (name) @_gse
  body: (compound_statement
    (return_statement
      (array_creation_expression
        (array_element_initializer
          (_)
          (array_creation_expression
            . (array_element_initializer
                . (string (string_content) @ref.method.named.self)))))))
  (#eq? @_gse "getSubscribedEvents"))

(method_declaration
  name: (name) @_gse
  body: (compound_statement
    (return_statement
      (array_creation_expression
        (array_element_initializer
          (_)
          (array_creation_expression
            (array_element_initializer
              (array_creation_expression
                . (array_element_initializer
                    . (string (string_content) @ref.method.named.self)))))))))
  (#eq? @_gse "getSubscribedEvents"))
