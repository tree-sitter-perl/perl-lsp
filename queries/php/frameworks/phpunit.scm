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

; ---- mocks are the class they double ----
; `$m = $this->createMock(Foo::class)` (and createStub / createConfiguredMock /
; createPartialMock / getMockForAbstractClass): the value is an instance of
; the class the argument names — PHPUnit spells it `MockObject&Foo`; the
; `Foo` side is what member completion, goto-def and the lanes need. The
; `@type.annot` joins the assignment target exactly as a declared type does.
(assignment_expression
  left: (variable_name) @flow.target
  right: (member_call_expression
    object: (variable_name)
    name: (name) @_pum
    arguments: (arguments
      . (argument (class_constant_access_expression . [(name) (qualified_name)] @type.annot))))
  (#any-of? @_pum "createMock" "createStub" "createConfiguredMock" "createPartialMock" "getMockForAbstractClass"))
; `$this->foo = $this->createMock(Foo::class)` in setUp() — the property.
(assignment_expression
  left: (member_access_expression
    object: (variable_name) @_pumr
    name: (name) @flow.target.member)
  right: (member_call_expression
    object: (variable_name)
    name: (name) @_pumf
    arguments: (arguments
      . (argument (class_constant_access_expression . [(name) (qualified_name)] @type.annot))))
  (#eq? @_pumr "$this")
  (#any-of? @_pumf "createMock" "createStub" "createConfiguredMock" "createPartialMock" "getMockForAbstractClass"))
; `$this->getMockBuilder(Foo::class)->…->getMock()` with zero, one or two
; chained builder modifiers.
(assignment_expression
  left: (variable_name) @flow.target
  right: (member_call_expression
    object: (member_call_expression
      object: (variable_name)
      name: (name) @_pumb0
      arguments: (arguments
        . (argument (class_constant_access_expression . [(name) (qualified_name)] @type.annot))))
    name: (name) @_pumg0)
  (#eq? @_pumb0 "getMockBuilder") (#eq? @_pumg0 "getMock"))
(assignment_expression
  left: (variable_name) @flow.target
  right: (member_call_expression
    object: (member_call_expression
      object: (member_call_expression
        object: (variable_name)
        name: (name) @_pumb1
        arguments: (arguments
          . (argument (class_constant_access_expression . [(name) (qualified_name)] @type.annot)))))
    name: (name) @_pumg1)
  (#eq? @_pumb1 "getMockBuilder") (#eq? @_pumg1 "getMock"))
(assignment_expression
  left: (variable_name) @flow.target
  right: (member_call_expression
    object: (member_call_expression
      object: (member_call_expression
        object: (member_call_expression
          object: (variable_name)
          name: (name) @_pumb2
          arguments: (arguments
            . (argument (class_constant_access_expression . [(name) (qualified_name)] @type.annot))))))
    name: (name) @_pumg2)
  (#eq? @_pumb2 "getMockBuilder") (#eq? @_pumg2 "getMock"))
