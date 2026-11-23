# ADR: Named arguments — one shape for every language

Perl's fat-comma call keys (`connect(timeout => 30)`), Perl's named
signature parameters (`sub f(:$name)`), PHP's named arguments
(`f(b: 2)`) and Python's keyword arguments (`f(b=2)`, `**kw`) are one
thing: a call passes a value under a name, and the callee's parameter
list says which names it takes. The engine models that once.

## A parameter says how a call reaches it

`ParamInfo::pass` is a closed `ParamPass`:

| mode | reached by | Perl | PHP | Python |
|---|---|---|---|---|
| `Positional` | place | `$x`, an `@_` unpack's scalar | — | before a `/` |
| `Either` | place or name | — | every parameter | a plain parameter |
| `Named` | name | `:$name` | — | after a bare `*` |
| `RestPositional` | the leftover places | `@rest` | — | `*args` |
| `RestNamed` | the leftover names | `%opts` | — | `**kwargs` |
| `Rest` | both leftovers | — | `...$args` | — |

A parameter list is therefore two carriers: a positional sequence
(`takes_position`) and a keyed map (`takes_name`); `Either` sits in both.
The extractor mints the mode from the signature's syntax. A consumer asks
the mode and never the parameter's sigil or name: "which parameter takes
the pairs" is `pass == RestNamed`.

Arity counts only the places: a by-name parameter's key and value ride in
whatever count the call passes, so a `Named` or rest parameter makes the
callable variadic and adds nothing to `total` or `required`.

A plugin may state a parameter's `pass`; its `is_slurpy` shorthand reads
the rest's kind from the name's sigil (`@` places, `%` pairs).

## A key argument binds to its call

A key written in a call's argument list (`f(name => …)`) carries
`RefBinding::CallArgKey { call }`: the call that receives it, minted where
the key is extracted. Its owner is the callee's, read off that call; a
key read inside the arguments (`f(x => $h->{name})`) belongs to its own
hash, not to the call.

## A callable's input keys are their own owner

`HashKeyOwner::Input { package, name, slot }` owns the keys a callable
takes; `Sub { package, name }` keeps the keys it returns. A call's key
(`connect(timeout => 30)`, `Foo->new(size => 1)`) is an `Input` use, and
what declares it is whatever says the callable takes that name:

- a framework's constructor key (`has`, Class::Tiny, Corinna `:param`),
  under `Input{C, new}`;
- a collecting parameter's read: inside a callable whose `RestNamed`
  parameter is `%args`, each direct `$args{k}` is owned by that callable's
  `Input`, and the first read of a key declares it (`bind_collected_keys`).

So `Foo->new(name => 1)` reaches `$args{name}` in a hand-written `new`,
and never the `bless { name => … }` slot `new` returns, nor a key of the
hash a `cfg()` returns. The slot is reached from the read only by
following the value, which waits for flow. A `bless` key of class `C`
still anchors its slot group (`$self->{name}` reads) on its own.

The surface carries both sets per callable (`MethodSurface::hash_keys`,
`input_keys`), and call-argument completion offers only `input_keys`.

A `Class(C)` lookup (`$obj->{k}`) still finds `Input{C, _}` keys, as it
finds `Sub{C, _}` ones. Plain calls emit their argument keys after the walk,
once each call is pinned to its package and the collecting reads have
declared their keys, so a call above its `sub` links like one below it.
