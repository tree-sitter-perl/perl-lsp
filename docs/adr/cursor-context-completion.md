# ADR: Pack-language completion — in-scope symbols + sentinel-reparse member access

Completion has two halves, and they split cleanly along the
language-agnostic / cursor-context line. The Perl path (`cursor_context.rs`,
rule #6) answers both at once with a Perl-specific tree walk. Pack
languages (the query-driven tier) get them as two independent pieces, so
no Perl semantics leak in.

## Half 1: in-scope symbols — no cursor context

`symbols::in_scope_completion(analysis, point)` lists every symbol visible
from the cursor: top-level defs (functions/classes/packages, globally
addressable) plus the locals/params/members a scope on the cursor's
lookup chain binds (`FileAnalysis::bindings_seen_from`, so a Python
method is not offered its class body's names). The client filters by
the typed prefix — names are stored verbatim (sigils and all), so
`$sn` → `$sner`/`$snerful` and `box.`'s prefix matching are the
editor's job, not ours. This is pure `FileAnalysis` (`scope_at` →
`lookup_chain` → `bindings`); zero per-language code, no tree.

It is the fallback whenever the cursor is not a member access.

A local a write declares for its whole body (php's, python's and R's,
`SymbolFlags::SCOPE_WIDE`) binds from the top of that body, but is offered
only below its first write in the body that writes it, as Perl offers a
`my` below its declaration (`FileAnalysis::in_completion_view`). A body
nested in it runs later, so there the local is offered wherever it is
written; a python function is offered the module globals written below it.
Binding, goto-def and types keep the scope-wide view. In a loop, a local
read at the top and first written further down is not offered above that
write (`TODO(cfg-loop-locals)`): source order stands in for control flow
until the scope work has a CFG.

## Half 2: member access — the crux is the erased operator

At the instant a user triggers completion the buffer reads `box.` /
`box->` / `obj.` — a member access with **no member**. tree-sitter
produces an `ERROR` and the typed `field_expression` / `attribute` node
the rest of the engine speaks never forms; `box.` and `box->` parse to a
byte-identical `(ERROR (identifier))`, so even `.` vs `->` is gone. A query
can't match a node the parser never built, and a node-walk has nothing to
climb. Every production server normalizes the buffer instead (clangd's
completion token, rust-analyzer's dummy-identifier insertion, pyright's
recovery parser) — none read the broken tree.

### The seam: sentinel reparse (`cursor_sentinel.rs`)

A member of the reparse family (`cpp_reparse.rs`, `reparse.rs`): a source
edit + reparse + span remap. The others fix a parse corrupted by a
*declaration* (a macro, a prototype); this one fixes a parse corrupted by
*incompleteness*. Splice a placeholder identifier (`__CURSOR__`) at the
cursor so the access becomes syntactically complete, reparse, find the
placeholder, and take its member node's receiver (`named_child(0)`).

The production entry point folds the receiver step and the type
resolution together; a test-only `Receiver{text,start,end,arrow}` probe
isolates just the sentinel mechanics for assertions:

```rust
pub fn member_completion_ctx_incremental(parser, cfg, src, old_tree, cursor, analysis, module_index) -> Option<MemberCompletionCtx>
pub struct MemberCompletionCtx { receiver_type: Option<InferredType>, op_fix: Option<(Span, String)>, op: MemberOp }
```

Two properties make it cheap and exact:

- **Free anchor.** The splice lands *at* the cursor, strictly after the
  receiver, so every receiver byte offset is identical in patched and
  original source. The `SpliceMap`/`AnchorMap` the other reparse siblings
  must carry, this one gets for free — no remap.
- **Incremental.** The splice is a pure insertion, so the `InputEdit` is
  exact: tree-sitter reparses only the damaged region around the cursor,
  reusing the document tree `document.rs` already holds.

Which node is a member access is the language's query document's answer,
never a table: the sentinel runs the pack's compiled document over the
reparse (`query_extract::CursorDocument`) and takes the match whose
`@ref.member.value` / `@ref.member.call` token holds the sentinel. Its
`@member.recv` is the receiver, its `@member.op` the operator token, and a
receiver that is itself an access (`a.b.`, `f().`, `$x->m()->`) is typed by
the same read, one hop at a time. `LanguageDriver::lang_pack(language)`
maps a driver id to its `LangPack`; `None` means the language gets
in-scope completion only. The "don't splice into strings/comments" set
(`skip_kinds`) and the receiver peel are still pack tables.

### What an operator offers

A member written after an operator lives in some of the language's member
namespaces (`member-kinds.md`), and the document says which: `@space.<name>`
on the operator token. php's `->` and `?->` reach an object's methods and
properties, `::` a class's methods, constants and static properties, and a
`#set! member.word` on the same pattern adds a word that names no member
(`Foo::class`). A token that states none (C++, Python) offers every
namespace. Each namespace is gathered on its own
(`complete_members_for_class`), so a child shadows its parent within a
namespace and php's `count()` and `$count` are two members.

Two marks on the operator's namespaces (`capture-vocabulary.md`) shape what
is offered. php's `::` reaches methods `.forward`: an instance method is
offered only where the call would run on the object the cursor's code runs
on, an instance method (or a closure in one, `runs_on_receiver_at`) of the
receiver's class or one it extends (`NamingSite::in_lineage`); elsewhere
only static methods, since `Foo::m()` on an instance method is an Error.
A class left with nothing reachable still answers the operator's words
(`Foo::class`). C++ states no `::` member access, so its `Foo::` is a qualified path and
`&Foo::m` is unaffected. php's `::` reaches static properties `.spelled`:
each is listed and inserted as declared (`$count`, the symbol's
`listed_name`, minted by `@def.field.spelled`). Every member item replaces
the member as written up to the cursor, from the `@member.spelled` token
where the document states one (`Foo::$` parses the `$` into the member), so
`Foo::` and `Foo::$` both complete to `Foo::$count`. What a typed `$`
narrows to is the client's filter over that range.

### A receiver the written buffer leaves untyped

The receiver types on the document's analysis of the buffer as written.
At the end of a block, `Foo::` / `parent::` parse as an `ERROR` and no
pattern says what `Foo` is, so nothing types it. Then the receiver types on
an analysis of the buffer the sentinel finished (`completed`), which is the
same document over the same receiver bytes: no second resolver of class
spellings. It costs one analysis of the file, paid only when the written
buffer has no answer AND failed to parse the access: the receiver through
its operator is, sits in, or holds an `ERROR` node of the written tree.
A site the written buffer parsed was matched there already, so the
finished buffer would type it no differently; an untyped `$u` in `$u->go()`
stays untyped without the second analysis. Ungated, that analysis ran for
every untyped receiver and took the composer completion census from 899 s
to 1,209 s.

### Receiver → members: tree-free, reusing the bag

The backend owns the tree work (it has the parser + cached tree); once it
has the receiver span it hands off to the language-agnostic half:

```
sentinel receiver span
  → expr_type_at_span(span)            // the bag, via the @expr.read.var witness
  → InferredType::class_name()
  → complete_members_for_class(class)  // ancestor-walk methods + data fields
```

`complete_members_for_class` is the pack sibling of
`complete_methods_for_class`: methods come from the shared ancestor walk
(inheritance for free), data fields are the class's `Variable`/`Field`
symbols, and no `new` is synthesized — member access lists real members.
Members resolve to a class because the cpp pack's `@context.class` tags
class-body symbols with the class name (`symbol_in_class` reads `package`).

## The protocol gate

`completionProvider.triggerCharacters` is the union of every served
language's `LanguageDriver::trigger_chars()` (cpp adds `.`; `->`/`::` ride
the existing `>`/`:`; php adds `$` for a variable and `Foo::$x`). The client auto-fires completion on the trigger and
names it in `CompletionContext` — a free "is-this-member-access" signal at
one slice-literal per language. It is a gate, not a solution: it never
carries the receiver, which is exactly what the sentinel exists to
recover.

## Why this shape

The five candidates (query `.scm` vocabulary, generic node-walk, lexical
text scan, sentinel reparse, protocol signal) were spiked in parallel. The
erased-operator crux eliminates the two tree-reading approaches on
incomplete input and demotes the lexical scan to a lossy heuristic we
declined to make load-bearing. Sentinel reparse is the one that *removes*
the damage rather than interpreting it, it reuses a seam the engine
already has, and its receiver→type→members tail is the same bag +
inheritance walk Perl uses. The protocol signal rides on top as the cheap
gate.

Deferred: inheritance across namespaces (qualified class names).
