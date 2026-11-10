# A call's value is an `Invoke`

## Context

A call's value is what its callee returns for its receiver and arguments.
The bag states that as one payload, `WitnessPayload::Invoke { callee,
receiver, args }`, on the call's own attachment. Every operand is an
attachment the registry materializes under the query that holds the index,
so the callee is chosen at query time, never fixed when the file was built.

## Decision: one call shape, four callees

`Callee` names what is called:

- `Method { name, lookup, constructs }`: a named method, looked up on the
  receiver's class, the writer's parents (`SUPER::`), or a named class.
- `Value(att)`: whatever callable `att` holds (`$cb->(…)`).
- `Construct { lookup }`: a construction, an instance of the class `lookup`
  names. It runs that class's constructor, which the site never spells.
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
`ReducerRegistry::free_name_cross_file`: a module exporting the name,
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
  qualified pack call (`@ref.call` named by its `@ref.spelled`) mints
  no call value yet outside a use-map.
- Under a use-map a free function is its namespace's identity
  (`docs/prompt-class-identity.md`, "A free name is its identity too"): a
  qualified spelling (`@ref.call` under `@ref.spelled`, php's `\Lib\f()`)
  resolves like a class spelling, a `use function` row binds its leaf,
  and a bare name walks `UseMap::free_name_ladder` — the namespace in
  force's function, then the global one. The call's ref is pinned to the
  first rung the file declares and `local` is that declaration; across
  files each rung is looked up by its identity, and a call whose identity
  nothing declares widens to its leaf out loud. A free constant
  (`@ref.constant`) walks the same ladder in its own table; a read of one
  the file does not declare is a `FreeValue` the registry looks up across
  files on that ladder, asking the declaration what it holds
  (`FreeAsk::Holds`) where a call asks what it returns.
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
  it is written in (php's `self::`): the class of the nearest named
  callable around the token, so a closure in a method reads the method's
  class and a free function's body names none. `@receiver.called` is the
  late-bound token (php's `$this`, `static::`, `new static`): the class
  the call running the method is made on, the same class where no call
  says. Its value is `ReturnExpr::ReceiverOr(class)`, Perl's `$self`
  shape, and the registry threads each call's receiver into the callee's
  body (`ReturnExprReducer`), so an inherited `static::m()` dispatches on
  the subclass and `return new static` is an instance of it.
  `@receiver.super` names the parent the writing class's `@parent.super`
  edge marks (php's `parent`, its `extends`: never an interface or a
  trait), and `@receiver.declared` the class the same token anchors (php's
  anonymous `class`). A call whose receiver token the document marks
  `@member.forward` (php's `self::m()` and `parent::m()`) forwards: it is
  `Method { lookup: Named(class) }`, the class the token names, with the
  calling method's own receiver place (`Binding::Receiver`) as its
  receiver, the shape of Perl's `$self->Base::m()`. That place holds
  `ReceiverOr` of its method's class, stated at the method once a call
  forwards it, so a `static` in `m` is still the class the outer call was
  made on (`Kid::viaParent()` → `parent::make()` → `new static` is a
  `Kid`), and the writing class where no call says. The token keeps its
  own value and its navigation: `self::X`, `parent::$x` and goto-def on
  `parent::m` still read the class it names. An operand that is a storage
  location, like that place, is read at the call that reads it, not at the
  point an outer query asks from.
- A construction (`@expr.construct`, php's `new Foo(…)`) is
  `Construct { lookup }` on its `@member.recv`: its value is an instance
  of the class the lookup names, whatever the constructor declares. A
  receiver token that names a class (`Foo`, `self`, `parent`) is a
  `Named` lookup; `new $cls` dispatches on the variable's value, so
  `$cls = Foo::class; new $cls` is a `Foo`. The constructor it runs is the
  member flagged `CONSTRUCTOR` (`@mark.constructor`) in that class's MRO,
  local or cross-file (`resolve_constructor_in_ancestors`), asked by role
  (`MemberKey::Constructor`), never by a name. The site's reference is a
  `MethodCall` with `constructs: true` on the receiver token. The stamp
  that freezes the class it dispatches on (`stamp_method_call_targets`)
  also keys it by that class, so the key is decided where the receiver's
  value settles: `new $cls` keys by the class `$cls` holds, and a class
  token by the identity it spells, the same key as the class's own
  `PackageRef` there, so it adds no ref row. The language states that its constructions key
  by class (`LanguageConventions::construction`, `ConstructionKey::Class`), so a
  constructor's references retrieve under its class and every class
  inheriting it (`TargetRef::retrieval_keys`, from the in-memory class
  graph) and match a site whose class's constructor walk reaches it; Perl
  calls `new` by name and retrieves under the name alone. The companion is a cursor companion, so the token
  navigates to the class, and never rewritable, since it spells the
  class, so a rename of the constructor refuses. A receiver that settles
  to several classes, or to one only the index knows, keeps the token's
  spelling as its key, so no constructor retrieves it
  (`TODO(construction-union-retrieval)`). Python's `Foo()` and C++'s
  `Foo(x)` are a `Function` whose `constructs` names the class: a class
  that answers the call answers with an instance of itself. Python also
  keys its constructions by class, but mints no companion: whether a call
  names a class is known only at query time (the class may live in another
  file), so the matcher admits, for a constructor target, a call whose
  name resolves to a class
  (`class_named_by_call`) whose constructor walk reaches it. Such a site
  is never rewritable either. An imported name (`@ref.import`, python's
  `from m import Foo`) is a `FunctionCall` reference to the def it names,
  pinned like a call, that the extractor marks `Ref::import_name`: it
  calls nothing, so it constructs nothing. C++ keys by constructor until a constructor
  walk stops at the declaring class (`TODO(construction-navigation)`).
- A class literal (`@expr.class`, php's `Foo::class`) is an edge to its
  `@receiver.*` token's value, the class.
- A pack callable's declared return (`@rettype`) is its `Symbol`'s value,
  so every `Invoke` that resolves to it carries that type and a chain types
  hop by hop. The spelling is read like a parameter's declared type (the
  pack's `annot_type`, then the use-map at the return's site), and
  `@type.enclosing` marks a written type naming the class it is written in
  (php's `self`, nullable or not), in a return, a parameter or a property
  alike: that class. `@type.called` marks one naming the class the call is
  made on (php's `static`): a callable declaring it returns
  `ReceiverOr(that class)`. `@type.super` marks one naming the class its
  `@parent.super` edge names (php's `parent`), resolved with the receiver
  mark of the same name. A callable that declares no return returns what
  its body returns (`@expr.return.value`), each site an arm of the nearest
  callable around it, so a closure's `return` is the closure's.
  TODO(generator-returns): a php or python function that yields returns a
  generator whatever its `return` says; its arms do not know it yields.
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
