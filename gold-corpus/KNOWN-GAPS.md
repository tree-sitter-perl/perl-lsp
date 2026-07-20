# Gold-corpus known gaps (xfail rows)

The harness pins each gap as an `xfail` fixture row: the assertion is the
**correct** expected behavior, marked as currently *not holding*. When a gap is
fixed the row flips to **XPASS** and the harness fails until you promote it to
`gold` — so a fix can't land silently and a gap can't rot. This file is the
prose write-up of the open xfails (the README's "Known failing" list is the
one-line index). Verify any one with:

```sh
gold-corpus/run.pl --emit <capability> <file> <line> <col>   # 0-based line/col
```

None are ref-graph (those are all closed).

---

## 1. Type-system / runtime codegen

### `def-16-codegen-type-function` — goto-def on a Type::Library-minted type
- **Capability:** definition · **Cursor:** `Type/Tiny.pm:414:56` (a use of `Any`)
- **Expect:** resolves to the type's *declaration*, `Standard.pm:215` (`name => "Any"`).
- **Actual:** degrades to the package decl `Standard.pm:1:1` (`expect.none`).
- **Root cause:** `Types::Standard::Any` has no literal `sub Any`. `Type::Library`
  mints a sub per type **at runtime** from an `add_type(name => "Any", ...)` table
  entry. We resolve the call to the package but there's no static symbol at the
  declaration to land on, so goto-def falls back to the package top.
- **Fix sketch:** a Type::Library-aware synthesis (plugin or core framework pass)
  that reads `add_type`/`declare` tables and synthesizes a Sub symbol per type at
  the `name =>` span — the same shape as the Moo `has`/DBIC `add_columns`
  accessor synthesis. Medium: needs the table-walk + the type-name → decl-span map.
- **Difficulty:** medium. Framework-specific synthesis; well-trodden pattern.

---

## 2. Dynamic-install recognition (diagnostics false-positives)

These three are unresolved-X diagnostics fired on subs/methods that *do* exist
but are installed by a mechanism we don't yet treat as a definition. The
assertion is "no diagnostic at this site."

### `diag-08` — XS `bootstrap` flagged unresolved-function
- **Cursor:** `Net/SSLeay.pm:1023:1` · **Expect.none:** a diagnostic at `SSLeay.pm:1023`.
- **Construct:** `bootstrap Net::SSLeay $VERSION;` — the XS loader. `bootstrap` is
  a builtin-ish loader call; the subs it installs are XS (no Perl body).
- **Root cause:** `bootstrap` isn't in our resolved set, so the call flags
  unresolved-function. (Separately, real XS-installed subs have no Perl def — the
  reference NOT-a-bug case.) The narrow fix here is recognizing `bootstrap` itself.
- **Fix sketch:** treat `bootstrap`/`XSLoader::load`/`DynaLoader` as
  defining/loader builtins (suppress unresolved on the loader call, and optionally
  mark the package as XS-backed so its missing-body subs aren't flagged).
- **Difficulty:** low (the loader call); the broader "this package is XS, don't
  flag its absent bodies" is medium.

### `diag-09` / `diag-10` — Log4perl typeglob-codegen accessors flagged unresolved-method
- **Cursors:** `Log/Log4perl/Logger.pm:879` (`is_warn`), `:883` (`warn`).
- **Construct:** Log4perl generates its level accessors (`warn`, `is_warn`,
  `error`, …) by installing closures into the symbol table in a `for` loop
  (`*{"$class\::$level"} = sub {...}`), then calls `$self->warn(...)` /
  `$self->is_warn` elsewhere.
- **Root cause:** the typeglob install is a *dynamic* `*{"...interp..."} = sub`
  inside a loop over a runtime list, so we don't synthesize symbols for the
  generated method names → the call sites flag unresolved-method.
- **Fix sketch:** extend the existing typeglob-codegen recognizer (it already
  handles `*name = sub` / `*{'literal'} = ...` / `*$m = sub`) to the
  loop-over-known-list case where the level names are a statically-derivable list
  (`qw(debug info warn error fatal)` constant-folded). Mints a Method symbol per
  generated name. Same provenance machinery as the current synthesis.
- **Difficulty:** medium. The names must be statically recoverable (constant-fold
  the loop list); truly-runtime lists stay out of scope (rule: no guessing).

---

## 3. Completion harvest

### `completion-datetime-hashkey` — `$self->{` offers too few keys
- **Cursor:** `DateTime.pm:315:30` (inside a `$self->{` in a method)
- **Expect:** the constructor-assigned keys appear — `local_rd_days`,
  `local_rd_secs`, `formatter`, `locale`, `offset_modifier`, `rd_nanosecs`,
  `utc_year`, … (offered as `key\tDateTime->{key}`).
- **Actual:** only a couple keys offered (the harvest finds ~2 of ~13).
- **Root cause:** the mutated-key set for a class is harvested from `$self->{k} =
  ...` writes, but DateTime's keys are assigned in a separate constructor helper
  (`_new` / `_recalc_*`) and via patterns the harvest doesn't walk (e.g.
  `@{$self}{@keys} = ...` hash-slice assignment, or keys set on a differently
  named lexical that becomes `$self`). So most slots never enter
  `mutated_keys_on_class`.
- **Fix sketch:** broaden slot-write harvesting — hash-slice writes
  (`@{$self}{...} = `), keys assigned on the blessed lexical before `return`, and
  keys flowing through a constructor helper (A4's cross-procedural tail). Overlaps
  the deferred "A4 v2 cross-file/cross-proc slot writes."
- **Difficulty:** medium–high. The cross-procedural part is the narrowing/flow
  frontier; the hash-slice-write part is a contained emission add.

### `completion-typetiny-imported-blessed` — imported subs absent from bareword completion
- **Cursor:** `Type/Tiny.pm:165:15` (a partial bareword in statement position)
- **Expect:** `blessed` (imported via `use Scalar::Util qw(blessed)`) is offered.
- **Actual:** only local subs are offered; imported names are missing.
- **Root cause:** bareword-statement completion sources candidates from local
  symbols (and maybe builtins) but doesn't fold the file's import surface
  (`imports[].imported_symbols`) into the candidate set.
- **Fix sketch:** add imported names to the bareword/function completion
  candidates (they're already in `analysis.imports`; the goto-def/diagnostic paths
  use them — completion just doesn't). Contained.
- **Difficulty:** low.

---

## 4. Signature-help invocant elision

### `sig-uri-check-path-function-noinvocant` — first arg dropped on a plain call
- **Cursor:** `URI/_generic.pm:58:13` (inside `_check_path( ... )`)
- **Expect:** `_check_path($path, $pre)` with `active param: 0 ($path)`.
- **Actual:** signature shows only `($pre)` — `$path` is wrongly elided as if it
  were an invocant.
- **Construct:** a **plain function call** `_check_path($rest, $$self)` (not a
  method call) whose sub is `sub _check_path { my ($path, $pre) = @_; ... }`.
- **Root cause:** signature-help drops the first parameter as the implicit
  invocant (`$self`) — correct for `$obj->meth(...)`, wrong for a plain
  `func(...)`. The call-shape (method vs function) isn't consulted before eliding.
- **Fix sketch:** gate the first-param-is-invocant elision on the call actually
  being a method call (`->`); plain function calls keep all params. The call ref
  already knows its kind (FunctionCall vs MethodCall) — signature-help should ask.
- **Difficulty:** low. A shape the producer already has; just consult it.

---

## 5. `--type-at` single-file CLI boundary

### `ti-12` — `$self = shift->SUPER::new` types only via the cross-file path
- **Cursor:** `Minion.pm:108:6` (the `$self` in `my $self = shift->SUPER::new;`)
- **Expect:** `$self` typed `Minion` (the enclosing class).
- **Resolved in the LSP.** Explicitly-qualified method dispatch (`SUPER::X` and
  fully-qualified `Foo::Bar::X`) now composes end-to-end. `emit_method_call_
  return_edges` peels the dispatch class from any `::`-bearing method token via
  one seam — `SUPER` → the *enclosing package's parents*, any other qualifier →
  that literal class — and emits a `QualifiedCallReturn` witness that looks the
  method up on the named class while typing the result relative to the invocant.
  `Mojo::Base::new` is receiver-polymorphic (`bless …, ref $class || $class` →
  `ReturnExpr::ReceiverOr`), so the parent ctor blesses into `Minion` and `$self`
  types `Minion`. Locked in by the **gold** `hover-minion-super-new` and the
  unit tests `test_super_new_types_to_calling_class`,
  `test_fq_method_call_dispatches_from_named_class`.
- **Why this row stays xfail:** the `--type-at` CLI mode is *single-file*
  (`cli_type_at` parses one file, no module index), so it cannot reach
  `Mojo::Base` cross-file to resolve `SUPER::new`. This is an inherent boundary
  of that debug CLI mode, not an LSP limitation — every cross-file query mode
  (`--hover`, `--definition`, the LSP server) takes a `<root>` and resolves it.
- **Reproduce:** `gold-corpus/run.pl hover` → `hover-minion-super-new` passes
  (`Minion`); `--type-at Minion.pm 108 6` returns `No type info` (no root).

---

## 6. Big QA sweep (mined against the snapshot substrate)

New gaps surfaced while mining gold from fresh CPAN modules. Each is pinned at
xfail (expected-correct confirmed from source; tool genuinely wrong).

### `hover-mojo-url-clone-via-new` / `ti-mojo-url-abs-clone-chain` — clone sub's stored *return type*
`Mojo::URL::clone` does `my $clone = $self->new; @$clone{…}=…; return $clone`.
The **variable** `$clone` types `Mojo::URL` correctly. The fixture cursors the
`sub clone` declaration, which reports the sub's *declared* return type — still
`HashRef`/null, NOT `Mojo::URL`. Root cause is **build-time vs query-time**:
`clone`'s `return_types` entry is seeded in the fold
(`seed_return_types_from_bag`) at build time, where the module index isn't
consulted, so the cross-file `$self->new → SUPER::new → Mojo::Base::new` chain
the variable resolves *at query time* isn't visible to the build-time seed —
only the local `@$clone{…}` hash-slice rep survives. Same class of gap as
`ClassIsa`/`param_types` ancestry-gated *emission* deferred to the ReceiverGated
seam: a sub-return whose value depends on a cross-file chain must resolve on a
query-time seam, not the build-time `return_types` map. **Subsystem:** build-time
`return_types` seed vs query-time cross-file method-return composition.
**Difficulty:** medium–high.

### `diag-mojo-cookiejar-helper-fp` — first-param-self over-reach in OO classes
In an OO class, a plain helper (`sub _compare { my ($cookie,…)=@_ }`) has its
first param typed as the enclosing class, so a method call on it
(`$cookie->path`) fires a false `unresolved-method`. The `-strict` (non-OO
module) case is fixed, and the anonymous-callback case
(`diag-mojo-daemon-callback-fp`, `on(request => sub { my $tx = shift; … })`)
was promoted to gold — the shift gate keeps first-param-self out of anon subs
(a callback's first arg is a payload, not a receiver). The named in-OO-class
helper is the residual — there's no clean static signal distinguishing a
method from a helper in an OO class. **Subsystem:** first-param-self heuristic
(`detect_first_param_type`). **Difficulty:** high (inherently ambiguous).

---

## Triage summary

| gap | subsystem | difficulty |
|---|---|---|
| sig-uri-check-path-function-noinvocant | signature-help invocant | **low** |
| completion-typetiny-imported-blessed | completion candidates | **low** |
| diag-08 (loader call) | XS loader recognition | **low** |
| diag-09 / diag-10 | typeglob-codegen synthesis | medium |
| def-16-codegen-type-function | Type::Library synthesis | medium |
| completion-datetime-hashkey | slot-write harvest (A4 tail) | medium–high |
| mojo-url clone *sub-return* (variable is fixed) | build-time `return_types` seed vs query-time cross-file method-return | medium–high |
| diag-mojo-cookiejar first-param-self (named helper) | invocant heuristic in OO class | **high** (ambiguous) |

Quickest wins: the signature-help invocant gate, imported-names in completion,
the `bootstrap` loader recognition, and the `(shift, shift)` param extraction —
all "the producer already has the signal, just consult it."

## C++ (multi-language tier)

Cross-file macro resolution landed (include-closure gather + expansion +
the structural strip/salvage lanes), so the original "namespace-wrapping
macro from another header" class mostly resolves: a wrapping macro whose
defining header is in the include closure expands, and macro-guarded
namespace reopenings attribute. Two xfail rows remain open:

| row | residual shape |
|---|---|
| `cpp-xfail-cross-file-namespace-macro` | the UNRESOLVABLE wrapping macro: no `#include` reaches a definition (generated/out-of-tree header) and the token sits in statement position before `class` — the structural strip covers the before-`namespace` and before-constructor positions, not this one, so `class Logger` is still lost and `info` leaks as a free Sub |
| `cpp-hitlist-marker-macro-outline` | a bodyless marker `#define` (`FMT_HEADER_ONLY`) still appears as an outline Variable; the class after it extracts fine — pure outline noise |
| `cpp-svmacrotag-cross-file-goto-def` / `cpp-svmacrotag-cross-file-completion` | **macro-named struct tag, cross-file** (perl5 sv.h: `#define STRUCT_SV sv` then `struct STRUCT_SV {...}`). The struct is DEFINED in a header with no `STRUCT_SV` macro in scope → its symbol is named `STRUCT_SV`. A using file that DOES have the macro in scope expands a `struct STRUCT_SV *` receiver to `struct sv`, so the tag lookup misses and member gd/completion go dark. General cross-file macro-named-tag asymmetry (reproduces on a plain struct too — not member-block-specific; the member-block edge itself attaches correctly, names matching). The `SV`-typedef path is unaffected, so the daily-driver `SV *sv; sv->sv_flags` resolves — only the raw `struct STRUCT_SV *` spelling is dark. Fix needs tag-name canonicalization across the macro alias (register the struct under both spellings, or resolve the receiver's tag through known object-like macros). |

**Out-of-line members:** out-of-line definition extraction
now handles the declarator/qualifier shapes the narrow patterns dropped —
pointer/reference returns (`Regexp* Regexp::Simplify()`), multi-level
qualifiers (`Prog::Inst::InitAlt`, 3-level `Prefilter::Info::Walker::
ShortVisit`), and out-of-line constructors (`RE2::RE2(...)`); registered
out-of-line methods now carry their owning class, not the enclosing
namespace (H7-2). A header declaration and its out-of-line definition in
another `.cc` are now linked in goto-def, so a call site reaches the bodied
definition across files instead of stopping at the prototype (H7-3). Gold
locks: `cpp-oolmember-definition` / `-references` / `-rename` +
`cpp-outline` rows. Residuals, PARKED: cpp macro
transform is position-blind (`#define Simplify DontCallSimplify` rewrites
occurrences before the directive too — a 2-ref shortfall on the references
acceptance; extraction itself is correct), and cpp class-name rename
identity is namespace-blind (renaming `Iterator` proposes edits in vendored
gtest — needs namespace-qualified identity).

**hitlist-6 Family A note:** the probe's headline finding — "a union-bearing stacked member-block loses its `(struct → macro)` parent edge, all SV member navigation dark" — did **not** reproduce at the spike tip. The member-block edge attaches correctly even with an anonymous union / nested braces in the pasted body (`svunion.c` is the gold lock: gd/hover/completion on `sv->sv_flags` via an `SV *` receiver all resolve). The probe's "dark" symptom was a bounded-root/cache artifact. The one genuine residual it surfaced is the macro-named-tag row above (a distinct, general seam).

### hitlist-2 fix-run residuals (dogfood round 2, slices A–E landed)

The measured residue after the dogfood round-2 fixes; none are pinned as rows
yet — they were observed on real corpora (json.hpp/abseil), not minimal repros:

- json.hpp namespace attribution still stops at an `#if` inside a class
  body (extraction lane).
- One `private:` leak survives the scope-desync repair.
- Tokens blanked by `strip_unresolved_structural_macros` are not re-minted
  as refs — the site vanishes from gr (the between-splice diff re-mint
  covers only the declarator-strip lane).
- Salvage granularity is per-MACRO-NAME: a name with mixed good/bad use
  sites degrades as a whole group.
- Member gd/hover/completion go dark inside config-INACTIVE regions
  (`#ifdef DEBUGGING`, `#ifdef PERL_DEBUG_READONLY_OPS`) — the receiver's
  type doesn't reach the superposed body. This is what made perl5's
  `op_targ`/`op_opt`/`op_flags`/`sv_flags` look "field-dropped": every
  field works in active code and dies in the same inactive block
  (`op_slabbed` works at op.c:394, dark at op.c:633 in the same function's
  `#ifdef` twin). NOT a per-field synthesis gap — the config-superposition-
  on-declarations tier (PARKED).
- References on a macro miss uses inside OTHER macros' `#define` bodies
  (`FLAGS` used inside `IS_OK`; perl5 `SvFLAGS` 190/347, `SvANY` 111/207) —
  macro definition bodies are preproc-excluded from ref minting. goto-def
  through the same nested sites works; index-population only. Pinned
  `cpp-macro-nested-ref-in-macro-body` (xfail).

## Refs symmetry (gd↔gr) — honest residuals

The symmetry invariant (every forward use→def resolution has a matching
def→uses backward walk on the SAME key) holds for macros (object- and
function-like, incl. config variants), enum constants, struct/role members,
type names (struct/typedef/class), file-scope globals, `#include`
(who-includes-this-header), and macro delegation (wrapper call sites are
references of the wrapped function). Deliberately out of scope / honest gaps:

| case | note |
|---|---|
| rename through a macro alias / expanded use | alias call sites and expansion-erased uses are listed by references but marked non-rewritable (the token spells the macro's name); pack rename is full-or-refuse — a target whose reference set contains an alias-spelled site refuses outright rather than emitting a partial edit (macros/globals/enum constants/members with plain spellings rename cross-file) |
| role-macro (`BASEOP`) gr does not list composer structs as "uses" | the standalone-in-struct-body use IS listed (the blanked token is re-minted); the composing struct itself shows via goto-implementation semantics, not references |
| template extraction (slice a) landed | primaries + per-spec Class identity (`formatter<int, char>`), out-of-line `Buf<T>::` join, explicit-instantiation outline items, `using` alias + concept symbols — `thousands_sep_result` gd/gr green (7 refs / 3 files on fmt); residuals: `extern template` spellings parse as ERROR in tree-sitter-cpp (refs come from catch-alls only), instantiation typing is slice (c) |
| template instance joins the class (slice b) landed | `Box<Widget> b; b.size()` — declared spelling peels to `ParametricType::Instance{base, args}`; dispatch keys the base (exact canonical spelling wins when a per-spec class exists), so member gd/gr/completion ride the plain-class machinery; hover keeps the full spelling; typedef/`using`-to-template-spelling chases through (`tmpl_instance.cpp` rows). Residuals: args carried un-consumed (`T get()` returns untyped until slice (c) substitutes); on fmt itself the landing line for inherited members is spoiled by pre-existing macro-damage orphans (`buffer::size` extracts unowned in base.h) + package-blind `sub_info` — the clean 3-file replay of the same shape (alias header + qualified template base + lambda-in-template use) lands exactly |
| instantiation-aware typing (slice c) landed | `Crate<int> ci; ci.get()` → `int` — param-shaped member returns substitute the receiver's instance args lazily (`ReturnExpr::ParamOf` beside `RowOf`; fields via `substitute_type_params`; trailing `-> T*` returns extract), chains compose (`cw.get().spin()` gd), and spec selection runs the ladder exact > partial-pattern (`codec<T*, char>` matches `codec<Widget*, char>`, binding `T`) > primary with ranked never-pruned family goto-def (`tmpl_typing.cpp` / `tmpl_dangle.cpp` rows). Real-fmt pin: `iterator_buffer<double*, double>` gd offers the `<T*, T>` partial spec's `out()` first + primary kept, hover substitutes `double`; `basic_memory_buffer<char>`'s `data` stays dark on fmt itself — `FMT_CONSTEXPR`-prefixed members don't extract (macro-arc lane, parked). Parked rungs: dependent types (`T::value_type`), value-arg deduction, template-template params |

## LSP session determinism (C++ tier)

| case | note |
|---|---|
| ~~cold-start deadlock (first query hangs, "5 compute assertions miss", self-heals on rerun)~~ **FIXED** | was a DashMap shard-reentrancy deadlock: a handler held a `get_open` read guard across `resolve()`, which re-locks the open shards via `for_each_open`; a concurrent `on_refresh` `for_each_open_mut` writer queuing on that shard (parking_lot writer preference) wedged the reentrant read behind the writer, behind the first read. The Perl cpanfile resolver fired ~45 refresh writers in a 400ms burst post-`didOpen`, so a mixed repo hit it intermittently; each wedged handler consumed a worker thread until the runtime starved. Fix: `Document::analysis` is `Arc`; handlers snapshot + drop the guard before `resolve()`. Repro lock: `e2e/cold-start-repro.sh` (7.5%→0). |
| cold-open goto-def/hover return `None` / pack completion floods the Perl hub, then answers after warm | the on-open analyze is cached-only (no cold header gather blocking `didOpen`) and the pack index attaches after the lazy background walk; a def/hover served in that window is `None`, and pack completion (no pack index yet) falls back to the Perl hub → `@INC` flood — with no client re-request signal for the pull verbs (completion self-heals via `isIncomplete`). Normally <500ms; under a cold cache + resolver-storm CPU pressure it can outrun a fast query burst. A defer needs a completion signal on BOTH the gather refresh (`spawn_pack_gather_refresh`) and the pack index (`ensure_workspace_indexed`'s latch marks kickoff, not completion) plus a bounded wait in the handlers — deliberate design gap, recorded not queued (arc-review M6). No longer masked by the deadlock (fixed above). |
| debounce-window stale analysis | between a keystroke and the debounced rebuild, `doc.analysis` describes the previous text; positions can misattribute mid-typing. Inherent to the debounce design (arc-review L3) |

## C++ dogfood residuals (observed on real corpora, not pinned as rows)

Behavior gaps surfaced dogfooding op.c/op.h, fmt, and abseil. Not minimal
repros; recorded here so a fix flips a real observation, not just narrative.

- **Function-designator ref emission.** A function passed by name without `()`
  — `absl::ascii_isspace` handed to `std::find_if_not` — is never emitted as a
  `FunctionCall`/use ref, so `references` misses those sites (4 in-file on the
  abseil case). Builder ref-emission gap.
- **Member-name references over-report across unrelated classes.** `references`
  on `ordered_map::key_type` hits every class's `key_type` in the workspace —
  matched by bare name with weak class scoping (the over-count twin of the
  under-emission above).
- **Field/member uses inside `#define` bodies undercounted.** `op_next` counts
  85 vs grep's 134 (~37% miss): `collect_macro_body_uses` recovers macro-*name*
  uses inside macro bodies but not `->field` / identifier member drills. Broader
  than the tracked `cpp-macro-nested-ref-in-macro-body` xfail (macro-name only).
- **Enumerator hover shows type but drops value.** `OP_NULL` hovers as `opcode`
  but not `OP_NULL = 0: opcode`; extraction doesn't capture the enumerator value.
- **`workspace/symbol` exact short-name ranking.** `workspace/symbol "OP"`
  doesn't rank the exact-name typedef first among fuzzy matches.
- **goto-def on C local variables.** A C local variable reference doesn't
  resolve to its declaration. Needs a distinct `@def.local` capture, locals
  emitted as scoped `Variable` symbols, `@expr.read.var` emitted as resolvable
  Refs, and outline-skip so they don't flood `--outline`.
- **C `goto` labels.** `label:` / `goto label;` are real navigation targets;
  not yet handled.
