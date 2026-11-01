// What PHP guarantees every class: `new C(...)` constructs even when no
// class in C's chain declares `__construct`. The implicit constructor is
// reached only by ROLE — a construction — never by name: with no declared
// constructor, `$o->__construct()` and `parent::__construct()` are errors.
// PHP has no root class, so there is no `class` here.

fn id() { "roots-php" }

fn triggers() { [] }

fn language_root() {
    [
        #{
            language: "php",
            members: [
                #{ name: "__construct", constructor: true, reach: "Role", params: [],
                   doc: "Implicit constructor: no class in the chain declares `__construct`, so `new` takes no arguments." },
            ],
        },
    ]
}
