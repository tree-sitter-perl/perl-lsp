# Pack plugins: the capture-event framework tier

**Tier 1 is LANDED** — the named-reference captures, the bundled
WordPress overlay, and the overlay loader (discovery, per-overlay
compile isolation, content-hash query cache, fingerprint fold, the
`--plugin-check` `.scm` arm) all shipped; acceptance verified on real
WP core (see `docs/hitlist-php-round3.md` R8). Tier 2 still waits on
its first tenant, per the gate below.

The design carried as "the open round" in `docs/prompt-multi-language.md`
— how pack languages (php, cpp, python, r, cmake) get a framework tier —
resolved into a shape the Laravel arc already proved half of. This brief
is the whole design: what ships now, what waits, and the evidence that
drew the line where it is.

## The finding that shapes everything

The Laravel framework tier landed with **zero hook machinery**:

- Eloquent relations are a QUERY (`queries/php/frameworks/laravel.scm`)
  — standard capture vocabulary (`@def.field`, `@type.annot`,
  `@flow.target`) plus `#any-of?` text predicates carrying the
  framework's method-name vocabulary. The engine gained nothing
  Laravel-shaped.
- Facades are a GENERIC phpdoc lane (`@method` rows) that any library
  benefits from.
- Builder generics are GENERIC phpdoc lanes (`@template`,
  `@return X<static>`) riding the cpp template axis.

And the WordPress hooks tenant — the finding that originally motivated a
rhai callback design — turns out to need only ONE new thing: a way for a
query to say *"this string literal's content names a function."* That is
a vocabulary entry, not a callback.

So the design is two tiers with a hard line between them, and tier 2
deliberately does not build until a tenant forces it.

## Tier 1 (build now): loadable query overlays + named-reference captures

### The overlay unit

A pack plugin is a directory:

```
<plugin-dir>/wordpress/
  pack.toml              # name, description, maturity tier
  queries/php.scm        # additive patterns, standard capture vocabulary
  entry.json             # optional: framework-entry rules (see below)
<plugin-dir>/laravel/
  pack.toml
  queries/php.scm        # today's queries/php/frameworks/laravel.scm, verbatim
```

`entry.json` declares which annotation names / method conventions mean
"a runner invokes this" for the heatmap's dead-code report — matched by
attribute (`#[Test]`, via the `@sym.attr` symbol lane), method
name/prefix, and a leaf-keyed isa gate evaluated through the ancestry
walk at report time. Bundled documents (PHPUnit, Laravel) ship on
`LangPack::bundled_entry_markers` the same way bundled `.scm` overlays
ship; plugin-dir documents extend the set, and a malformed one is
dropped alone with a diagnostic. The rules are DATA — the evaluator
(`heatmap.rs::framework_entry_claims`) never compares framework names.

Discovery reuses `plugin_search_dirs()` (`$PERL_LSP_PLUGIN_DIR` + the
project-local dir) — one search path for both plugin worlds. A plugin
may carry BOTH `.rhai` (Perl hooks) and `queries/*.scm` (pack overlays);
the loader routes by extension.

`queries/<lang>.scm` is keyed by the language id the registry serves
(`php.scm`, `cpp.scm`). At pack construction, every discovered overlay
for a language is concatenated onto the bundled query — exactly what the
bundled laravel.scm concat does today, generalized:

- **Compile-failure isolation.** Each overlay is test-compiled ALONE
  against the language's grammar first; one that fails is dropped with a
  diagnostic naming the plugin, and the base skeleton + surviving
  overlays still serve. This mirrors the rhai combined-query rule ("one
  malformed `.rhai` cannot take dispatch out for the rest") — same
  failure posture, same reason.
- **The query cache re-keys by content hash.** `cached_query` keys by
  `&'static str` pointer today, which is correct only while every query
  is a compile-time constant. A runtime-assembled (base + overlays)
  string keys by hash of the concatenation; the compiled query is still
  leaked-once per distinct set, so the cost model is unchanged (one
  compile per (language, plugin-set), not per file).
- **Invalidation rides the existing plugin fingerprint.** Overlay bytes
  fold into the same hash that already covers `.rhai` sources; a
  mismatch hard-clears the modules table (the machinery exists —
  `docs/adr/plugin-system.md`). Editing an overlay invalidates every
  cached analysis it could have shaped, and nothing else needs to know.
- **Unknown captures are inert** (already true of the event loop) — an
  overlay written against a newer vocabulary degrades to silence, never
  to error. A `--plugin-check` arm compiles each overlay and lists any
  capture names the vocabulary doesn't know, so silence is diagnosable.

The engine carries no framework names anywhere in this tier: the
overlay's `#any-of?` predicates are the framework vocabulary, evaluated
by tree-sitter itself (`matches()` applies text predicates — verified,
it is what gates the relation patterns today).

### The one vocabulary addition: string-named references

WordPress hooks, in evidence (round 3): 993
`add_action('init', 'wp_cron')`-shaped sites plus 161
`array($this, 'method')` callbacks in wp-includes+wp-admin, dark in both
directions, poisoning heatmap fan-in for every hook-driven function.
What the query cannot say today is only the LAST step: the matched
string's *content* is a function reference.

Two captures close it, defined on the STRING-CONTENT node (php
`string_content`; every grammar has the equivalent):

- `@ref.call.named` — mint a `FunctionCall` ref whose `target_name` is
  the captured node's text and whose SPAN is the captured node (the
  content between the quotes). References connect both directions by
  construction; rename rewrites exactly the characters inside the
  quotes — the span IS the name, so no folded-from indirection is
  needed (rule #9's provenance is degenerate here: the string is both
  the source and the site).
- `@ref.method.named` — the same mint as a `MethodCall` ref, paired
  with the existing `@member.recv` in the same match for the
  `array($this, 'method')` / `[$this, 'method']` forms (the receiver
  types the dispatch; `$this` rides the established hop.recv shaping).

Implementation is one arm each in the extract event loop, beside the
existing `ref.*` arm — the events already carry span+text; the only
difference from a normal ref is that the NAME comes from content rather
than an identifier token. Nothing downstream changes: these are ordinary
refs, so gd/references/rename/heatmap inherit them like any other
(the ctor-link lane just demonstrated exactly this inheritance).

The WordPress plugin is then pure query:

```scheme
(function_call_expression
  function: (name) @_hook
  arguments: (arguments
    (argument)
    . (argument (string (string_content) @ref.call.named)))
  (#any-of? @_hook "add_action" "add_filter" "remove_action" "remove_filter"))
```

plus the array-callback twin. Deliberately v1-scoped to the CALLBACK
edge; hook-NAME identity (`do_action('init')` ↔ `add_action('init', …)`)
is the natural follow-on and has a rail waiting — the `Handler`
TargetKind (Perl's dispatch-handler lane is the same shape: a string
key connecting registration to firing) — but it needs a `@def`-flavored
named capture and an owner story, so it ships as its own slice.

### What tier 1 deliberately cannot do

A capture can only NAME what appears in the source. The moment an
emission's name requires surgery — Laravel scopes (`scopeActive` →
`->active()`), Moo `handles` maps, DBIC's typed column synthesis — the
declarative lane is over. That is the honest boundary, and it is the
same boundary the Perl side already draws between `patterns()` (query)
and `on_match` (code).

## Tier 2 (build when a tenant forces it): the rhai capture-event hook

The API, so the first tenant doesn't redesign it:

```rhai
// pack.toml declares:  language = "php", hooks = "hooks.rhai"
fn patterns() {
    // tree-sitter query text, same as tier 1 — but each pattern may name
    // a handler instead of relying on vocabulary captures alone
    [ #{ name: "scope_method",
         query: "(method_declaration name: (name) @name (#match? @name \"^scope[A-Z]\"))",
         handler: "on_scope" } ]
}
fn on_scope(m) {
    // m: capture name -> #{ text, start, end } — the EVENT, not the tree.
    let bare = m.name.text.sub_string(5);
    let lowered = bare.sub_string(0,1).to_lower() + bare.sub_string(1);
    [ #{ Method: #{ name: lowered, span: m.name } } ]
}
```

Three commitments, all inherited from the Perl plugin system rather than
invented:

1. **Handlers see capture EVENTS, never the tree** — (text, span) per
   capture, exactly what the extract event loop itself sees. Rule #1
   stays intact: tree traversal remains inside the one walk; a handler
   is a pure function from match to emissions.
2. **Emissions reuse the `EmitAction` vocabulary** (the 25-variant enum
   Perl plugins already speak: `Method`, `MethodCallRef`, `ImportRef`,
   witness pushes, …), translated into SkeletonAnalysis rows by ONE
   generic adapter. New capability = new EmitAction variant, shared by
   both plugin worlds — never a pack-only action enum drifting beside
   the Perl one.
3. **Same host, same fingerprint, same failure isolation** as `.rhai`
   plugins today — a broken script drops its own patterns only.

Sequencing gate, stated so it survives: tier 2 does not start until a
concrete tenant needs name surgery (Laravel scopes are the likely
first). Every tenant that CAN be an overlay MUST be an overlay — the
laravel.scm precedent is the null hypothesis, and this arc falsified
"hooks need callbacks" once already.

## Migration and sequencing

1. **Overlay loader** — LANDED (dir discovery, per-language concat,
   isolation, hash-keyed query cache, fingerprint fold, `--plugin-check`
   `.scm` arm; the loader test copies laravel.scm into a plugin dir
   verbatim beside a broken sibling and both contracts hold).
2. **Named-reference captures** — LANDED (`@ref.call.named`,
   `@ref.method.named`) + the bundled WordPress overlay. Verified on
   real WP core: every `'wp_cron'` registration is a reference,
   `'wptexturize'` answers all 11 default-filters sites, rename
   rewrites exactly the string contents, the `array($this, 'm')` form
   dispatches through the receiver.
3. **Hook-name identity** — LANDED on the `Handler` rail:
   `@def.handler.named` (registration first-arg → stacked
   `HandlerOwner::Global` Handler) + `@ref.dispatch.named` /
   `@dispatch.via` (firing sites). `'init'` connects 190 sites across
   127 WP files, references = the grep count exactly.
4. **Tier 2** when its first real tenant lands, against the API above.

## Non-goals

- No per-plugin Rust: the grammar stays the only compiled-in piece per
  language (`docs/prompt-multi-language.md`'s runtime-packs step is a
  separate arc).
- No new security surface in tier 1: an overlay can only match and mint
  within the fixed vocabulary — it is data, not code. Tier 2 inherits
  the rhai sandbox posture as-is.
- No overlay-to-overlay ordering semantics: emissions are additive and
  order-independent (the extract dedup passes own collisions, as they
  already do for base-vs-overlay def pairs — the field-ness dedup key).
