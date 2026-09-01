; PHPUnit framework overlay — attribute arguments that NAME sibling methods.
;
; `#[DataProvider('providerMethod')]` / `#[Depends('testOther')]`: the
; string names a method of the ENCLOSING class, invoked by the runner.
; @ref.method.named.self mints the same MethodCall ref a written
; `$this->providerMethod()` carries, with the enclosing class as the
; invocant — so providers gain real fan-in, gd/references connect both
; directions, and rename rewrites the name inside the quotes.
(attribute
  (name) @_pua
  parameters: (arguments
    (argument (string (string_content) @ref.method.named.self)))
  (#any-of? @_pua "DataProvider" "Depends"))
