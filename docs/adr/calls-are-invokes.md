# A call's value is an `Invoke`

## Context

A call's value is what its callee returns for its receiver and arguments.
The bag states that as one payload, `WitnessPayload::Invoke { callee,
receiver, args }`, on the call's own attachment. Every operand is an
attachment the registry materializes under the query that holds the index,
so the callee is chosen at query time, never fixed when the file was built.

## Decision: one call shape, three callees

`Callee` names what is called:

- `Method { name, lookup, constructs }`: a named method, looked up on the
  receiver's class, the writer's parents (`SUPER::`), or a named class.
- `Value(att)`: whatever callable `att` holds (`$cb->(…)`).
- `Function { name, local, member_of }`: a free name. `local` is the
  in-file definition the producer resolved the name to, a class named as a
  callable included (a functional cast or constructor). `None` sends the
  lookup across files. `member_of` is the class whose body the call is
  written in, where the language elides the receiver: a member of that
  class, its own or inherited and in any file, answers first, with the
  class as the receiver. A method is never `local`; it is reached only
  through `member_of`.

The producer resolves the local half because it holds the symbol table, and
a cross-file chase into a file whose symbols are evicted still finds the
edge in that file's bag (rule #11). The cross-file half is one ladder,
`ReducerRegistry::free_function_cross_file`: a module exporting the name,
then a pack's linkage-visible definitions gated by include-closure
reachability. `query_sub_return_type` walks the same ladder for a bare
name, so a call and a name probe cannot disagree. The ladder shares the
query's `QueryState`, so a call chain that crosses back into a file already
on the path stops at the cycle guard, and one that reaches the same name's
fan-out again from another file stops at the bag-free fan-out node
(`docs/adr/resolution-session.md`).

A bake has no index, so an unresolved `Function` is an exit no conclusion
key can name. The bake records it and poisons, and the file's answer for
that chase is decoded at query time.

## Decision: arguments are operands, and arity is read from them

`args` lists `CallArg::One(att)` per written argument and `CallArg::Spread`
for one that flattens to any number of values. The registry materializes
each `One` into the positional types `ReturnExpr::Arg(n)` substitutes, and
the call's arity is exact only when no argument spreads. `args: None` means
the arguments were not walked, so there is no arity and there are no
argument types.

A pack's document says what an argument list holds. `@arity.args` is the
list, captured in the same match as the callee, and `@arity.arg` each argument,
its own pattern: `(argument_list (_) @arity.arg) @arity.args`. A callable's
parameter list is `@arity.sig`, and each parameter in it is `@arity.required`,
`@arity.optional` (it has a default) or `@arity.variadic` (it takes any number
of values, and counts none). A list's items are the tags in its subtree, less
those of a list of the same tag nested in it (a call in an argument, a
callback's parameters); the argument pattern tags every list so a nested one
always holds its own. `@arity.spread` marks the children that spread (`*xs`,
`**kw`, `args...`).

Items are separate patterns, not a quantified group in the list's match: a
group's repetitions must be adjacent siblings, so every separator and comment
between two items would have to be listed to keep it going. A wildcard item
still matches the grammar's extras; the extractor drops any capture on an
extra, whatever the pattern.

## Current state

- Pack free calls (`@ref.call` beside `@expr.call`) mint `Function`. A
  qualified pack call (`@ref.qcall`) mints no call value yet.
- Under a use-map a free function is its namespace's
  (`function_identity`): a qualified spelling (`@ref.call` under
  `@ref.spelled`, php's `\Lib\f()`) resolves like a class spelling, and a
  bare name is the namespace in force's own function when the file
  declares one there, else the global one. The call's ref is pinned to
  that namespace and `local` is that declaration; across files the name
  is looked up by its leaf, so two namespaces' same-leaf functions can
  answer for each other. php's `use function` rows are not read yet.
- A body elides its receiver where the document marks its scope
  `@scope.sub.implicit_receiver` and the body belongs to a method. There a
  bare call carries `member_of`, and a bare read the file can't resolve to
  a local projects the class's slot (`ValueHop` on `TypeName(class)`),
  pushed after the read's own `Variable` edge so a name no member answers
  keeps that edge's value. Navigation still binds only members this file
  declares (`TODO(implicit-receiver-nav)`).
- A pack member call (`@ref.member.call` beside `@expr.call`) mints
  `Method` with `Lookup::Receiver` on the receiver's own `Expr`, so a chain
  (`w.get().spin()`) hops through each receiver's value. The dispatch
  class is the receiver's `class_name()`, which is a template's BASE: an
  explicit specialization (`Box<bool>`) answers the primary's return here.
  `expr_type_at_span`'s member-chain arm still asks first, and it walks
  the specializations, so a cursor on the chain sees the specialization;
  a variable the chain flows into sees the primary.
- A receiver token that names a class has that class as its value
  (`Expr(token) → ClassName`), and a call on it dispatches there with
  `Lookup::Named`. The document says which tokens: `@receiver.class` on a
  class spelling (php's `Foo::m()`), resolved through the use-map at the
  site, and `@receiver.enclosing` on a token naming the class whose method
  it is written in (php's `$this`, `self::`, `static::`): the class of
  the nearest named callable around the token, so a closure in a method
  reads the method's class and a free function's body names none.
  `static::` is late-bound in php; here it names the writing class, as
  `$this` does.
- A pack callable's declared return (`@rettype`) is its `Symbol`'s value,
  so every `Invoke` that resolves to it carries that type and a chain types
  hop by hop. The spelling is read like a parameter's declared type (the
  pack's `annot_type`, then the use-map at the return's site), and
  `@type.enclosing` marks a written type naming the class it is written in
  (php's `self`, `static`, nullable or not), in a return, a parameter or a
  property alike: that class.
  Like `static::`, a `static` return names the writing class, not the
  receiver's.
- A receiver is typed by the value at its exact span. Every call states
  one at its whole span (Perl's builder and the packs' `@expr.call` alike),
  so under `f()->a->b` the receiver of `b` is `f()->a`, never `f`, the
  innermost call at its start. The cross-file fallback takes the call
  spanning exactly the receiver (`RefTable::call_spanning`).
- Perl's free calls mint `Function` too. The walk resolves a callee defined
  above the call; `resolve_forward_expr_witnesses` resolves one defined
  below it, and only a name still unresolved after that goes to the
  exporters. A qualified call that names another package (`Other::f()`)
  and a builtin mint no call value: the exporter lookup asks by bare name,
  and a builtin is not a sub.
- A keyword argument (python's `f(x=1)`, php's `f(x: 1)`) is a positional
  `One` in its written place; nothing yet binds it by name.
- A function-like macro whose return is its n-th argument keeps its
  call-site edge to that argument's own `Expr` (`docs/adr/macro-handling.md`),
  which carries more than the argument's type.
- `Ref::arg_count` stays the navigation lanes' arity source and counts every
  argument, spreads included.
