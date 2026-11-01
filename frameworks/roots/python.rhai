// What Python guarantees every class: `object`, the last class of every
// C3 linearization. Its members answer only after every class in the MRO
// missed, so a class's own `__init__` or `__eq__` always wins.

fn id() { "roots-python" }

fn triggers() { [] }

fn dunder(name, params, doc) {
    #{ name: name, params: params, doc: "`object." + name + "` — " + doc }
}

fn language_root() {
    [
        #{
            language: "python",
            class: "object",
            members: [
                // `Foo()` runs `object.__init__` when no class declares one.
                #{ name: "__init__", constructor: true, reach: "Both", params: [],
                   doc: "`object.__init__` — the implicit initializer; takes no arguments." },
                dunder("__new__", [], "allocates the instance."),
                dunder("__repr__", [], "the default `<Class object at 0x…>` representation."),
                dunder("__str__", [], "defaults to `__repr__`."),
                dunder("__format__", ["format_spec"], "defaults to `str(self)` for an empty spec."),
                dunder("__eq__", ["value"], "identity comparison."),
                dunder("__ne__", ["value"], "the inverse of `__eq__`."),
                dunder("__lt__", ["value"], "`NotImplemented`."),
                dunder("__le__", ["value"], "`NotImplemented`."),
                dunder("__gt__", ["value"], "`NotImplemented`."),
                dunder("__ge__", ["value"], "`NotImplemented`."),
                dunder("__hash__", [], "identity hash."),
                dunder("__getattribute__", ["name"], "attribute lookup."),
                dunder("__setattr__", ["name", "value"], "attribute assignment."),
                dunder("__delattr__", ["name"], "attribute deletion."),
                dunder("__dir__", [], "the attribute names."),
                dunder("__sizeof__", [], "the instance's size in bytes."),
                dunder("__reduce__", [], "pickle support."),
                dunder("__reduce_ex__", ["protocol"], "pickle support."),
                dunder("__getstate__", [], "pickle support."),
                dunder("__init_subclass__", [], "called when a subclass is created."),
                dunder("__subclasshook__", ["subclass"], "customizes `issubclass`."),
            ],
        },
    ]
}
