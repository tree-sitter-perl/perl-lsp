# PARKED — the single deferred-work ledger

THE one place. Session summaries and hitlists may narrate; this file is
the source of truth for what's deliberately not done. Each entry: what,
why parked, what unblocks it. Prune on landing.

## Design-debt tier (candidates for a tightening round)

All entries below re-ratified by the tighten-2 drain (2026-07-17) unless
marked otherwise; the drain re-derived each rationale against current code.

- **Dead node-kind comparisons — a silent-no-op correctness class, not
  tidiness** (found 2026-08-17). A `kind()` comparison against a string
  the grammar doesn't have never matches, and beside a working sibling
  arm it reads as handled behaviour; `"require_statement"` in
  `visit_const_usage`'s skip list wasn't merely dead — it made a real
  skip fail to happen, minting a bogus constant-FunctionCall ref under
  every bareword `require` (fixed with the `require_expression` work).
  Swept against ts-parser-perl 1.1.4's node-types.json. One genuinely
  dead kind remains: `"no_statement"` (removed at the one site;
  `no Foo;` parses as `use_statement`).

  **`"parenthesized_expression"` is NOT debt — do not sweep it.** The 27
  Perl-side arms (`build/builder/`: frameworks 8, visit_method 5,
  visit_use 3, visit_calls 2, extract 2, visit_decl 1, infra 1;
  `cst.rs` 4; `lsp/cursor_context.rs` 1) are deliberate forward-compat:
  the kind is **coming in the next ts-parser-perl release** and has been
  slow-walked because it is a breaking change. Essentially every
  tree-sitter grammar carries this wrapper node — without it, aliases
  and fields misbehave. The arms are inert today and correct the day the
  parser lands; deleting them means writing them again. The 4 pack-side
  comparisons (`build/cpp_reparse/defs.rs` 2,
  `build/query_extract/packs.rs` 2) are live NOW — it is already a real
  tree-sitter-cpp kind.
  The durable fix is worth more than the sweep, but the naive version
  is wrong: a test asserting every `kind()`-compared string exists in
  the grammar would fail on the forward-compat arms above, and the
  obvious response — deleting them — is the harmful outcome. The
  tripwire has to distinguish a TYPO from an ANTICIPATION, and only a
  declaration can do that. So: one named home for
  forward-compat kinds (a `grammar_future` constant per kind, used at
  every anticipating site), and a test asserting each `kind()`-compared
  string is either a current grammar kind or a declared future one
  (`layering_tests.rs` is the precedent for the structural assertion).
  Then `"require_statement"` fails the test the day it is written,
  `"parenthesized_expression"` passes because it is declared, and when
  the parser lands the constant deletes and every site keeps working.
  Without the declaration half, the tripwire is a hazard rather than
  a net.

- **Two include-BFS walkers + two `file_stamp` fns** (cpp_reparse vs
  module_cache): thrice examined, thrice left (different contracts/layers:
  parse-heavy macro gather vs memoized line-scan closure; `(hash,size)`
  columns vs folded i64 stamp). Merge only with a reason. [re-ratified
  2026-07-17]
- **Two C-comment strippers in cpp_reparse** (`strip_c_comments` vs
  `blank_comments_in_range`): distinct contracts — the former COLLAPSES
  whitespace to produce clean body text, the latter is length-preserving
  (spaces over comment bytes, newlines kept) so byte offsets stay in
  original coordinates for member positioning. Not a merge target.
  [re-ratified 2026-07-17]
- **Two "enclosing class" notions in `emit_return_fuel`**: the implicit-
  field half reads the ref's own `scope.package`; the sibling-CALL half
  walks up to the enclosing method SYMBOL's package (so out-of-line bodies,
  whose body scope carries no package, still resolve). Deliberately
  different robustness; unifying is a behavior change, not a cleanup
  (out-of-line bodies would gain implicit-field edges). [re-ratified
  2026-07-17]
- **Two domain/type completion rankers** (`backend::rank_domain_members`
  for pack enum members vs `symbols::rank_candidates_by_expected_type` for
  Perl scope vars): different item types (`CompletionItem` vs
  `CompletionCandidate`) and semantics (enum members verbatim, front-loaded
  vs. type-matching locals kept at `PRIORITY_LOCAL`). No shared gatherer to
  factor. [re-ratified 2026-07-17]
- **`class_isa_prefix` walks `parents_cached`, not `parents_of`**
  (`model/file_analysis/ancestry.rs`): deliberate, verdict recorded. The synthetic
  `APP_SURFACE_CLASS` edge that `parents_of` injects is a method-dispatch
  bridge (Mojo helpers), not an `isa` relation — a plugin `ClassIsa` gate
  must not treat an app-surface consumer as a descendant of the surface.
  Both isa-walk seams (`class_isa`, `class_isa_prefix`) exclude it by
  construction; they share the local-∪-cross-file seam, just not the
  surface edge. Not a `parents_of` drift to fix. [re-ratified 2026-07-17;
  T2-A consolidated all three isa walks onto one `walk_ancestry` — the
  seam distinction is now a parameter, not a copy]
- **CLI workspace-symbol dedup inlines the identity tuple**
  (`lsp/cli/query.rs` `workspace-symbol` vs
  `symbols::dedup_workspace_symbols`):
  type-forced duplicate, leave-alone (mirrors the two-rankers precedent).
  The LSP handler dedups a `Vec<SymbolInformation>` as a post-pass
  (`retain`); the CLI gates inline while building `serde_json` rows from
  raw `(name, kind, span)` before any typed Vec exists. Same 5-field
  identity, different pipeline stage and value shape (u32 uri/line vs
  usize path/row). A shared key helper would be a bare tuple literal each
  caller still populates from different fields — no gatherer to factor.
  [re-ratified 2026-07-17; re-checked 2026-08-17: TWO inline
  `seen.insert` dedups remain in `lsp/cli/query.rs`, deliberately
  different keys — the second at the single-file outline gates framework
  twin accessors on `(kind,name,row,col)` — neither a shared-helper
  candidate]
- **`load_components` DBIC-namespace default lives in core**
  (`build/builder/frameworks.rs::visit_load_components`, bare names default
  to `DBIx::Class`):
  KEEP IN CORE, rationale rewritten 2026-07-17 (the old blocker — no
  parent-edge EmitAction — is obsolete: `EmitAction::PackageParent` exists
  and is wired). The standing reason: `load_components` is generic mixin
  machinery (Catalyst too) and the dbic plugin is `ClassIsa`-gated, so
  moving registration there would drop non-DBIC callers; only the bare-name
  `DBIx::Class` prefix string is DBIC-specific. If ever promoted (est M):
  plugin emits fully-qualified parents via `PackageParent`, core keeps a
  generic non-prefixing fallback; identity risk — the plugin's `+`-strip and
  prefix policy must reproduce core's exact strings or inherited-component
  goto-def/rename silently breaks.
- **Four→one ancestry walkers, GraphView as the end state**
  (`model/file_analysis/ancestry.rs::walk_ancestry`, T2-A 2026-07-17): the three isa DFSes
  now share one predicate-parameterized walker (per-call-site budgets 200/
  200/40 and seam scopes preserved). The recorded end state is collapsing
  `walk_ancestry` onto `GraphView`'s lazy walk (docs/adr/graph-walking.md) —
  do that, not a fifth bespoke helper. The walk contract now carries what
  the collapse needs: the `WalkControl` prune verdict and the maskable
  `APP_SURFACE` edge kind (bare `INHERITS` = surface-excluded, the isa
  gates' seam scope). The DBIC base-name set
  (`class_is_dbic_result`'s Core/Row-vs-Schema/ResultSet polarity) still
  lives in the Model layer; its plugin-manifest move rides the
  `role_makers` precedent (follow-on, est M).
- **`class_is_dbic_result` vs `class_isa_prefix("DBIx::Class")`**: NOT a
  merge — the result gate encodes row-vs-schema POLARITY (accepts Core/Row
  roots, rejects `::Schema`/`::ResultSet`) that a prefix test cannot; a
  schema class is a DBIx::Class descendant but not a result class.
  [recorded 2026-07-17]
- **Three byte-capped LRU eviction cores** (PackBagCache plain; enrichment
  overlay adds entry-count cap; GatherCache adds single-flight condvar):
  shared discipline (`evict_to_cap`, never-evict-just-inserted), genuinely
  different surrounding contracts — forced unification would
  over-parameterize. Strongest DRY signal on the books; re-examine only if
  a FOURTH appears. [recorded 2026-07-19]
- **Deleted-path canonicalization fallback** (`forget_source_gen` /
  `unregister_file` use `canonicalize().unwrap_or(path)`; `remove_surface`
  reconstructs via parent-dir + filename): a delete under a symlinked
  parent leaves a stale entry (one i64 / one registration). Pre-existing
  shared convention, no new hazard class; consistency nit — route all
  three through the reconstruction if ever touched. [recorded 2026-07-19]
- **`RESOLVE_MEMO` vs `PackBagCache`**: same surface shape ("cache of
  computed values"), OPPOSITE contracts — thread-local stack-scoped
  correctness memo cleared on resolve-stack drain vs long-lived
  byte-accounted LRU invalidated on content change. Never unify under one
  cache abstraction. [recorded 2026-07-17]

## Feature tier (each is a fireable slice)

- **Perl domain typing** — needs a constant-group / Type::Tiny enum-domain
  model (`docs/adr/field-projections.md`).
- **Type-constrained completion** — the cpp domain slot (`op_type == |` →
  `OP_*` ranked first) and the Perl ArgPosition consumer LANDED on the
  `Slot::expected_type` seam (`docs/adr/cursor-slots.md`). Residual: the
  switch-`case |:` position (needs the switch-condition climb) and the
  Perl-side domain source, which still wants the constant-group /
  Type::Tiny enum-domain model above.
- **Template rungs**: dependent types (`T::value_type`), value-arg
  deduction, template-template params.
- **Flag-set domains** (`op_flags`/`OPf_*` — subset-of vs one-of).
- **Use-after-move** — the DECIDABLE subset is WIRED (opt-in
  `initializationOptions.diagnostics.useAfterMove` / CLI `--use-after-move`,
  off by default; `docs/adr/use-after-move.md`). Flags only a straight-line,
  in-function, LOCAL moved-then-used, behind three honesty gates
  (`use_after_move_reads`): B (in a function body — kills member-init /
  delegating-ctor floods), C (straight-line — no conditional/loop/switch/
  ternary/preproc between move and read), E (locals only — a moved parameter
  is a forwarding/subobject idiom). Verified 0 FP over the spdlog/fmt/onednn
  headers (was ~17 with the naive check). STILL PARKED, needs true
  path-sensitivity + subobject/interprocedural analysis: a use in a different
  branch arm, a loop-carried move, a `x.member` sibling-read after a
  base-subobject move (`operator=`/move-ctor), and a by-mutable-ref reset
  (`reset(x); x.use()`). Those stay silent by design — the gates trade recall
  for zero false positives.
- **C/C++ narrowing-diagnostics facts** — D1/D2/D3/D4/D6 are Perl-only
  because their fact seams (`deref_receiver_sites`, `guard_sites`,
  `arrow_deref_sites`) are minted only by `src/builder/narrowing.rs`, a
  child of the Perl-only tree-sitter consumer. cpp goes through
  `query_extract` and never runs `build()`. The cpp hover/goto **type**
  tier already narrows (`narrow_guard` refines inside `dynamic_cast` /
  `std::optional` guards — `cpp_dynamic_cast_guard_narrows`); what's missing
  is the **diagnostics** layer. Needs a cpp nullability pass that lowers
  `nullptr` comparisons + `std::optional` engagement state into the
  `Undef`/`Optional` lattice ALONG cpp control flow (the analog of the Perl
  guard/truncation machinery), plus cpp `guard_sites` for D3/D4. Then the
  existing `deref_receiver_sites` / `guard_redundancies` seams and their
  filters light up unchanged. `docs/adr/narrowing-diagnostics.md` (C/C++
  applicability matrix).
- **C/C++ `unresolved-method` (D8)** — the facts resolve (verified: a cpp
  receiver types to its class via `expr_type_at_span`, classes mint
  `SymKind::Class`, inheritance rides `package_parents`, and the
  `class_has_unresolved_ancestor` valve silences the unscanned-base case).
  Blocked on **macro member-injection**: a `#define … void run();` /
  `Q_OBJECT`-style macro in a class body injects a member the skeleton
  walker can't see, so a call to that present method reads as a false
  `unresolved-method` (verified FP; no existing valve catches it). The
  sound valve — silence any class whose body span contains a macro/opaque
  token — is buildable (the `Class` symbol span + body `Block` scope cover
  the full body), but its precision (correctly telling a member-injecting
  macro from a benign one, including object-like macros from unscanned
  headers that surface as bare identifiers) must be **calibrated against the
  macro-heavy real substrate** (spdlog/fmt/onednn), the same bar
  use-after-move cleared. Default-off + opt-in + pack-capability gate
  (declared like `implicit_this_members`, never `lang == cpp`) is understood;
  only the valve + its calibration remain. `docs/adr/narrowing-diagnostics.md`.
- **PR #100** re-extraction onto the projection engine (user closes or
  reworks; the `projection.rs` PoC now rests in git history — the design
  is `docs/adr/cpp-templates.md`).
  - i think this will just be closed; anyways it didn't look like it did the intended PPP,
    which is to have mojo helpers which mint dynamic helpers show their definitions;
    literally no reason to punt on a conrete impl. this branch is leaning towards prod, so
    no reason to duplicate there
- **PR #105** heatmap-viz refresh (pre-rebased on local `tmp-viz-trial`).
  - yalla, let's clean that up; the core of the PR is just the html
- **Per-toolchain global system-header cache**; the cross-language
  "system root" generalization (perl=@INC, python=probe).
- **Instance brands** (per-object dispatch scoping) — downstream of the
  long-distance value-provenance tier (`prompt-type-inference-residual.md`).
- **`monkey_patch`-synthesized methods invisible** (mojo F7 — `$ua->get`):
  `monkey_patch __PACKAGE__, lc $name, sub{...}` in a loop mints methods a
  syntactic walk can't see. Needs loop-unrolled emission (plugin emit-hook
  shaped) — real design work.
- **Raw `$_[N]` / `@_` subs get no param/return inference** (mojo F4 — `on`
  vs `once`): a sub that reads args positionally rather than via `my
  ($self, ...) = @_` produces no arity/return facts. Unblock: an `@_`-index
  → param binding at the walk.
- **`emit('x')` ↔ `on(x =>)` event linking in references** (mojo F9): the
  `dispatchers` field exists on the outline but is unreachable from the
  references verb — emit sites and handler registrations aren't cross-linked.
- **H7-8 inline `->search(...)->first` loses parametric row type** (DBIC F4):
  a RowOf verb composed on a fluent-verb result inside ONE expression types
  nothing; the same composition through an intermediate variable works.
  Wave-3 candidate. Unblock: compose the parametric type across chained
  method calls without an intermediate binding.
- **H7-13 cpp member-field receiver completion doesn't narrow** (leveldb
  task 4, re2 F3): `field_->` / `field.` dumps the in-scope grab-bag instead
  of the field's real members; parameter/local receivers narrow correctly.
  Narrowed lists also leak private + nested-struct members and truncate
  trailing-underscore names (`cleanup_head_` → `cleanup_head`). Wave-3
  candidate.
- **H7-15 DBIC resultset moniker never resolves to the FQ result class**
  (split from H7-7): `$schema->resultset('Artist')->create(...)` types the
  row as the literal source moniker `"Artist"`, not `DBICTest::Schema::
  Artist`, so cross-file goto-def on the row's methods can't start its walk
  (proven: forcing the FQ class reaches Row.pm perfectly). The schema class
  is discarded from `ParametricType::ResultSet{base,row}`. Wave-3 candidate.
  Unblock: moniker→class resolution (schema receiver's class + registered
  source names + cross-file index) at the parametric-type seam.

## Residual-bug tier (pinned, xfail'd where reducible)

- **php `use X as Alias` spellings resolve nothing.** The use-map axis
  pins the FQ row's real leaf, not the alias; a hint/`new`/receiver
  spelled `Alias` finds no candidate (honest empty, never a stranger).
  Translating the alias to the real leaf at extraction is wrong when the
  file also declares that leaf itself; the fix is namespace-qualified
  class identity — `docs/open-forks.md`, "GraphView node identity is
  leaf-keyed", option C. `parent::` through an aliased parent already
  works via `parent_namespaces` rows.

- **Cross-file functional-cast / constructor typing** (callee is NOT a
  local symbol). The name-case ctor heuristic is DEAD: a call's value is now
  the callee's own resolution (`query_extract::into_file_analysis` call-site
  loop → `Expr(call) → Edge(Symbol(callee))`; a `Class` symbol answers
  `ClassName`, a callable its return, an unresolvable name NOTHING —
  `docs/adr/macro-handling.md`). This fixed the `RCPVx(pv)` misfire outright
  (an unresolvable uppercase call leaves an `auto` local honestly untyped;
  gold `ctxparam` + unit `ctor_convention_unresolvable_uppercase_call_no_phantom_class`).
  The residual: a call whose callee resolves ONLY cross-file (Python
  `g = Greeter()` where `Greeter` is a class in another module, or a C++
  functional cast to a header-defined class) types nothing, because the
  callee isn't a local symbol and cross-file classes aren't registered under
  their own name in the module index (Python `Greeter` is registered under
  module `a`). Unblock: index pack classes by name so `get_cached(callee)`
  finds them, then a no-terminal-invent cross-file call-value edge resolves
  at query time (idx present). Xfail-adjacent: unit
  `python_cross_file_method_dispatch_through_mro_walk` now asserts `g` is
  honestly `None` locally (its real subject — cross-file MRO dispatch keyed
  on the class name — is unaffected).
- **json.hpp `basic_json` attribution blast radius — FIXED** (the
  re-anchor invariant landed; `docs/adr/config-superposition-declarations.md`). Trigger
  named: `#if JSON_DIAGNOSTIC_POSITIONS` in ctor-initializer / declaration
  position (6 sites in `basic_json`) truncates the `class_specifier` node
  at the first ctor's body brace (row 21450 vs the true 25771) — every
  member after falls through to the enclosing `nlohmann` namespace. The
  fix is `SkeletonAnalysis::reanchor_truncated_containers` (gated by
  `LangPack::brace_scoped_members`, run post-`remap_spans`): it
  brace-matches the ORIGINAL source (balanced — the macro-expansion
  transform is what unbalances braces, 682/710 vs 646/646) to recover each
  container's true extent, then re-attributes members to the innermost
  container that textually encloses them (upgrade-only; a `::`-qualifier
  attribution and a macro-defined-namespace scope are the two guarded
  cases). `basic_json` member attribution: **92 → 763** (both amalgamated
  and split forms); nested `json_value`/`data`/`patch_operations` members
  preserved. Bounds the blast radius for ANY future misparse that truncates
  a container, not just this construct. Residual (small): specialization
  containers whose shaped name ≠ source text are skipped (conservative);
  the `strip_declaration_position_directives` gate still misses the
  in-context `#if` (a point-repair complement, no longer load-bearing).
- **Slice 2 (config-superposition variant tags) re-scoped** — the spike
  (`docs/adr/config-superposition-declarations.md`, findings 2026-07-05)
  proved slice 2 is NOT needed for Case B (slice-1 exclusion narrowing
  cured it) and does NOT fix Case A's blast radius (a parse corruption,
  above). Variant tags remain justified only for **genuinely superposed
  DECLARATIONS** — a field/def whose SHAPE differs per config, an
  `#else`-twin function with a different body — where the payoff is
  **labeled multi-arm navigation** (gd unions both arms, macro-def
  precedent) and **arm-fold typing on true twins** (a config arm folded
  as a branch arm through the existing reducers). Not motivated by any
  measured darkness after slice 1.
- **Strip-blanked tokens aren't re-minted as refs** (gr misses blanked
  `NS_BEGIN`-style occurrences; splice-blanked ones ARE re-minted).
- **Per-macro-name salvage granularity** — a macro with both good and bad
  uses is kept/blanked wholesale. SIBLING (budget-exhaustion scaling wall)
  and the op.c:633 dark-receiver are now FIXED via the context-free-safe
  expansion verdict (`is_context_free_safe` in `cpp_reparse.rs`): an
  empty-body deletion like `pTHX_`/`aTHX_` is classified context-independently
  safe and is never stranded in a dropped conditional-region body OR bisected
  in salvage (it's kept as the always-applied baseline). Residual: the general
  per-name granularity for a NON-empty, position-DEPENDENT macro with mixed
  good/bad uses — the localization slice (fix #1) — still wants doing.
  `docs/prompt-macro-salvage-scaling.md`.
- **One `private:` leak shape** (raw_hash_set.h:3783 — post-declarator
  attribute in a compound misparse the conservative gate doesn't reach).
- **fmt macro-prefixed members** (`FMT_CONSTEXPR auto data()`) don't
  extract — macro-damage lane; why `memory_buffer.data()` is dark.
- **`extern template` spellings** parse as ERROR (tree-sitter-cpp 0.23).
- **proto.h variadic decls** never register a Sub (`Perl_croak` absent
  from completion/gd).
- **Nested-hash-key completion level leak** (Perl, pre-existing — xfail
  `completion-exact-hash-key-slot-no-nested-leak`).
- **Moo rwp writer at decl-token group answer** (`docs/adr/heatmap.md`).
- **M6/L3 session determinism — cold-open degraded window** (the DEADLOCK,
  POISONED-PERSIST, and now the HEAL-REPUSH + COALESCE halves are FIXED; only a
  bounded-wait in the pull handlers is LEDGERED — see below). The on-open
  analyze is cached-only and the pack index attaches after the lazy background
  walk, so a query in that window can see a degraded answer (pack completion
  falls back to the Perl hub → `@INC` flood; cross-file gd/hover `None`; refs
  from an open def-site return the def only, e.g. `op_free` count=1 in-window vs
  118 warm). Completion self-heals via `isIncomplete`.
  **The HEAL-REPUSH + COALESCE halves are FIXED** (`fix/degraded-window-heal`).
  Two changes in `backend.rs`:
  - **Completion-signal heal.** `ensure_workspace_indexed`'s latch marked
    KICKOFF, not completion; nothing re-derived an open doc after the index
    landed. Now the end of that background walk calls `Backend::heal_open_docs`,
    which re-analyzes every OPEN doc in the family (pack: full off-lock
    re-analysis via `spawn_pack_doc_refresh`, since the `did_open` gather was
    cached-only and the cross-file index is now warm; perl: enrich + diagnostics
    re-publish) — so the doc-baked degradation self-heals on a server-driven
    event, not on a user re-trigger. `spawn_pack_gather_refresh` already heals
    its own doc on gather completion, so BOTH completion signals now fire a
    heal. Guard discipline held: pack URIs are snapshotted under a read guard
    that drops before any re-analyze; the perl branch re-derives enrichment
    through `FileStore::enrich_open` (clone off the store lock, short swap)
    and publishes after. Verified: `heal_open_docs` logs
    `cold-window heal: index landed for pack family` on op.c open, refs heal
    1→118 (`e2e/cold-window-heal-repro.sh` phase 1).
  - **Coalesced `on_refresh`.** The callback fired once PER resolved module (33
    fires opening a Perl file with 14 `use`s), each a full all-open re-enrich +
    publish — CPU + stdout pressure that widens the window. It now bumps a
    `refresh_gen` and debounces 120ms; only the latest fire runs, collapsing the
    burst to ONE execution (measured 33→1, `e2e/cold-window-heal-repro.sh` phase
    2). The final fire always survives the settle, so the fully-resolved state
    is still published.
  **The bounded wait is now FIXED** (`fix/cold-window-wait`). A pull verb
  (gd/hover/references/implementations) arriving while its language family's
  workspace/pack index is IN-FLIGHT — kicked off at `did_open` but not yet
  complete — now blocks briefly (`await_index_ready`) for the completion signal,
  then resolves against the warm cross-file index instead of returning the one
  degraded answer the user never re-triggers. The signal is a per-family
  `AtomicBool` + `tokio::sync::Notify` fired by an `IndexDoneGuard` on EVERY exit
  of the indexing task (no-root / panic included), so a waiter is never stranded.
  **Guard discipline held:** the wait touches ONLY the family's atomic + Notify
  — NO FileStore guard is held across the await (the handler peeks `language`
  under a `get_open` that drops before the await, and snapshots `analysis` fresh
  after). Cap is `initializationOptions.coldWaitMs` (default 400ms, 0 opts out) —
  bounded, so it can never wedge; on timeout the handler degrades exactly as
  before. The warm case pays ZERO added latency (index already `done` → returns
  before awaiting; measured warm re-fire 61ms vs a 2084ms in-window wait). Repro:
  `e2e/cold-window-wait-repro.sh` (self-contained synthetic C workspace; OFF
  coldWaitMs=0 → in-window refs 4 degraded, ON → 16001 healed by the wait, same
  single query un-re-requested). Deadlock lock `e2e/cold-start-repro.sh` stays
  0/20. On a perl5-scale tree whose index alone takes ~22s the default 400ms cap
  times out and degrades safely — the wait targets normal-sized projects whose
  index lands within the cap. The heal-repush (doc-baked answers + diagnostics)
  and coalesce still cover the rest.
  **The deadlock that used to MASK this window is fixed** (`Document::analysis`
  is now `Arc`; handlers snapshot + drop the `get_open` read guard before
  `resolve()` re-locks the open shards — the reentrant-read-behind-a-queued-
  writer deadlock). Repro lock: `e2e/cold-start-repro.sh`
  (pre-fix ~7.5% cold-run failure, post-fix 0). Also: debounce-window staleness
  (mid-typing `doc.analysis` describes prior text). KNOWN-GAPS "LSP session
  determinism".
  **The POISONED-PERSIST half is FIXED — the window's damage is non-sticky.**
  The worry was that a degraded cold-run analysis gets frozen into the SQLite
  pack cache behind a `deps_stamp` that self-validates (the stamp is recomputed
  over the STORED closure at load time, so a truncated/empty closure matches
  itself and never re-derives), re-served on every WARM run until `--clear-cache`.
  Two guards close it: `save_to_db` refuses any `degraded` analysis (H8), and
  `PackDriver::register_post_build` now folds closure-INCOMPLETENESS into
  `degraded` — a skipped cached-only gather OR a truncated include closure (a
  header that RESOLVED and exists yet failed to read: non-UTF-8, transient I/O)
  marks the analysis non-persistable, so a complete gather next session
  re-derives it. `cpp_reparse::include_closure` returns `(closure, complete)`
  and only memoizes a complete walk. Verified under heavy CPU load: every
  persisted blob is the correct full-closure analysis, and a warm run heals the
  transient window WITHOUT `--clear-cache` (a genuine poison would fail every
  warm run). Locks: `include_closure_reports_incomplete_on_unreadable_header`
  (unit), `e2e/persist-poison-repro.sh` (cold-load poison → warm heals, no
  clear-cache). What REMAINS of the TRANSIENT window is only the ledgered
  bounded-wait above — a single un-re-requested in-window pull query under load
  still sees the degraded answer for one session; the doc-baked state and
  diagnostics now self-heal server-side, and nothing sticky survives it.
- **Enum value as template argument** — FIXED. The token always had a ref
  (the `@ref.type` catch-all fires in template args; the grammar guesses
  TYPE for value args), so the fix is resolution-side: gd's PackageRef arms
  fall through type space to value space (pack structural gates), and
  `collect_from_analysis` matches `(Method{class}, PackageRef)` under the
  bare-constant hoist gate. gd/gr/rename all reach the site; plain-constant
  (`Buffer<BUF_LIMIT>`) and nested-qualified (`Run<outer::Mode::kSlow>`)
  covered. Pinned in `tmpl_valarg.cpp` gold rows. Honest residual: the
  `StatusCode::` QUALIFIER token is still ref-less (namespace_identifier —
  gd works via the word fallback; gr on the enum type misses qualifier
  positions in ALL positions, not just template args — the namespace-
  participation completion/gr gap below).
- **Nested-macro-body refs — cross-file reach residual** (the core case
  LANDED). A use of macro `A` inside `B`'s `#define` body now mints a ref:
  `macro_body_name_refs` (`cpp_reparse.rs`) lexically scans each opaque
  `preproc_arg` body for identifier tokens naming a KNOWN macro (this file's
  `#define`s ∪ the include closure's) and mints a read at the original span,
  fed into `skel.var_reads` from `enrich_skeleton`. Params + `#`/`##`
  stringify/paste operands + comments/literals are excluded (precision:
  prefer silence over a wrong ref). Gold `cpp-macro-nested-ref-in-macro-body`
  (+`-from-use`) promoted to gold. perl5 gr: `SvFLAGS` 190→320,
  `SvANY` 111→176 (grep-real ~347 / ~200). **Residual:** a body token naming
  a macro defined in a header the file's include closure doesn't reach (perl5
  headers aren't self-contained — `hv.h` uses `SvFLAGS` but may not resolve
  `sv.h`) still goes unminted, hence 320<347. Unblock: a broader TU-level
  macro universe (or a reverse "who-defines" index) so the `known` set spans
  the real translation unit, not just the resolved include graph.
- **`fmt::` qualified-path completion** — CLOSED. A pack `ns::`/`Class::`
  cursor detects as `Slot::ModulePath` via the same `qualifier_at_point`
  goto-def anchors on; `CandidateSet::complete_qualified_path`'s pack lane
  gathers the owner's members (shared `pack_member_of` predicate with
  `member_def_location`, so "offered" = "resolvable"), nested containers,
  and inline-namespace-lifted members ("inline" attribute minted by the
  `@ns.inline` skeleton capture; EXTRACT_VERSION bumped). Empty gather falls
  through to the bare-identifier universe — so real fmt's OWN `fmt::` drill
  (members unattributed behind `FMT_BEGIN_NAMESPACE`) keeps prior behavior
  until the macro-guarded-namespace-open gap closes; `fmt::detail::` filters
  correctly there today. Gold: `cpp-qualified-completion.json` (4 rows).

- **cpp hover renders methods field-shaped** (leveldb task 4b): a method
  hovers as `Valid: Bool` — no signature, no `const` qualifier — because the
  skeleton extracts members uniformly and hover has no method-vs-field split.
  Unblock: carry a callable shape (params + qualifiers) on the extracted
  member and render it in the cpp hover path.
- **leveldb `db_iter.cc` `k` else-branch dark spot** (leveldb task 4c):
  hover/def/refs all blank on `k` in one else-branch; unreduced (synthetic
  repro attempts failed). Coordinates in `findings-leveldb.md` task 4c.
- **cpp namespace-blind rename identity** (H7-6 cpp half, leveldb task 5b):
  renaming a class like `Iterator` proposes edits inside vendored gtest —
  class-name identity is bare-name, not namespace-qualified, so unrelated
  same-named classes in other namespaces collide. The Perl owner-gate half
  landed (`62426fa`); the cpp half needs namespace-qualified class identity
  in the rename target. Destructive-if-applied.
- **PHP method-level `@template`** (round-3 R6/R10b): a class-level
  `@template T` row feeds the same per-class param axis cpp templates use,
  but a METHOD-level `@template TValue` (e.g. Laravel's `BuildsQueries`
  trait `first()`) is a separate binding the class-keyed axis doesn't
  model. Laravel 12's CONDITIONAL generic returns (`($id is ... ?
  Collection<...> : TModel|null)` on `find`) are beyond the parser and
  correctly rejected — not a target, stays untyped. Rendering doc PROSE on
  hover is also still unread.
- **PHP completion: declared type loses to the bag in one lane** (round-3
  R11 tail): a declared `: int` return annotates as `int|float` in
  completion specifically (the bag beats the decl there; elsewhere the
  decl wins). Multi-line signatures also truncate in completion detail.
- **PHP `global $x` refs are always empty** (round-3 R11 tail): `global
  $wpdb;`-style bindings never collect refs; hover on `$wpdb` answers the
  CLASS by name coincidence, not the global binding.
- **PHP string-callable overlay residuals** (round-4 H7): the
  variadic-tail callback family (`array_udiff` & co — callback LAST
  positional arg) and key-position forms (`'sanitize_callback' => 'fn'`)
  aren't covered by the fixed-position `stdlib.scm` overlay. (The
  `'Class::method'` string spelling resolves on both segments, and
  `[$obj, 'm']` / `[$this, 'm']` instance-array callables mint member refs
  through the skeleton — neither is parked.) Two rename
  residuals from the same family, root-caused but not fixed: LogglyHandler
  (a closure param threaded through an `array_filter` callback) and
  MailHandler's `$highestRecord` (assignment flow into a null-guarded
  accumulator local).
- **PHP vendor-resolved method hover drops the signature** (round-4 H12):
  cross-file method hover through the vendor/dependency tier renders the
  generic member arm instead of the method-signature arm.

## Cross-references
- Gap shapes behind open xfails: `gold-corpus/KNOWN-GAPS.md`
- Open architectural forks: `docs/open-forks.md`; resolved ledger:
  `docs/forks-resolved.md`; deferred storage/residency work:
  `docs/prompt-storage-residuals.md`
