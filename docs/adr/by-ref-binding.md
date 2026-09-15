# ADR: By-reference parameters bind through the bag, not a bit

A callee that declares a parameter by reference (php `&$out`, C++ `T&`)
WRITES the caller's variable. The caller's variable is therefore bound
at the call site — for the undefined-variable lane, for its type after
the call, for every consumer that asks "what does `$out` hold here".

## The fact has one shape

The by-reference-ness is a fact about the callee's declaration, so the
callee's extraction mints it, as a witness on a dedicated attachment:

```
Param{package, name, index}  →  Edge(Variable{param, body_scope})
```

pushed for exactly the positions declared by reference and for nothing
else. There is no `by_ref` bitmask on `ParamArity`, no boolean a
consumer tests: the aliasing edge IS the binding mode. A position with
no `Param` witness is by-value by construction.

The call site pushes, for every bare variable it passes,

```
Variable(arg)  →  Edge(Param{ns, f, i})                       // a function call
Variable(arg)  →  Projected{base: receiver, ParamOf{member, i}} // a dispatch
```

anchored ZERO-WIDTH at the argument token (rule #14: the fact's own
site; and to the fold a spanned type witness is a narrowing region, a
point is a binding — the same convention every write witness follows).
The chase answers only where the callee aliases the position, so a by-value
argument's edge resolves to nothing and drops out; the caller never
learns which positions alias, and never needs to.

## Bound but untyped is `Unknown`, never silence

`function execute($cmd, &$output)` with no type and no nameable write
still binds `$output`. The registry answers `Unknown` ("a value flowed
here") when the `Param` attachment carries a witness that materializes
to nothing. Silence would read as by-value, and the undefined-variable
lane would report the out-parameter — the exact false positive the
side-table design carried.

## Cross-file is the `Field` ladder

`ParamOf` resolves the receiver's class, then chases `Param{class,
member, i}`; the registry's fallback walks the class's candidate files
(the callee's own bag is the authority) and then its parents, sharing
the visited set with every other hop. A `Child` receiver dispatching an
inherited `execute` reaches the parent's aliasing edge the way a field
read reaches a parent's slot.

## What this replaced, and why

A `variable_arg_sites` table on `PackFacts` (every bare-variable
argument with its list and position) joined by the diagnostics lane
through the call ref's span to `ParamArity::binds_arg(position)`. Three
homes for one fact — the bitmask, the site table, the join in a
consumer — and the answer was only as good as that one consumer's
join. The bag carries it once; every reader of the variable's value
sees the binding by construction, and a type the callee leaves in the
parameter (`$output = []`) flows to the caller for free.
