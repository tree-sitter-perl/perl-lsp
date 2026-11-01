// The members Perl guarantees every class: UNIVERSAL, which the method
// search visits after the whole @ISA chain. They answer only after every
// class in the MRO missed, so a class's own `can` (or a file reopening
// `package UNIVERSAL`) always wins.
//
// A plugin dir extends this by declaring `language_root()` with the same
// `language`; its members join these.

fn id() { "roots-perl" }

fn triggers() { [] }

fn language_root() {
    [
        #{
            language: "perl",
            class: "UNIVERSAL",
            members: [
                #{ name: "can", params: ["$method"],
                   doc: "`UNIVERSAL::can` — the code reference `$method` resolves to on this class, or undef." },
                #{ name: "isa", params: ["$class"],
                   doc: "`UNIVERSAL::isa` — true when this object or class inherits from `$class`." },
                #{ name: "DOES", params: ["$role"],
                   doc: "`UNIVERSAL::DOES` — true when this class performs `$role` (defaults to `isa`)." },
                #{ name: "VERSION", params: ["$require"],
                   doc: "`UNIVERSAL::VERSION` — the package's `$VERSION`; dies when it is below `$require`." },
                // `use` calls `import` and `no` calls `unimport` whether or
                // not the module defines them; a missing one is skipped.
                #{ name: "import", params: ["@args"],
                   doc: "`import` — called by `use`; a class that defines none imports nothing." },
                #{ name: "unimport", params: ["@args"],
                   doc: "`unimport` — called by `no`; a class that defines none does nothing." },
            ],
        },
    ]
}
