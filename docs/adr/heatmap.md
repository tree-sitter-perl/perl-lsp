# Code-usage heatmap (`perl-lsp --heatmap`)

A reporting view over the existing cross-file reference graph, not a new
analysis tier. Per-symbol fan-in is a projection of the resolution CandidateSet
(`docs/adr/resolution-candidate-set.md`): the set is minted at each symbol's
declaration and `references()` is counted, so the heatmap answers exactly what
`textDocument/references` answers there — and inherits every construction axis
(visibility masks, group/attr field splats, override families, closure/
delegation gating) without heatmap-side changes. The incumbent it mirrors is
SciTools Understand's "Butterfly" view (callers + callees = fan-in / fan-out).
It is an insight / DX deliverable, not a defect catalog, so it carries none of
the compliance / tool-qualification apparatus a MISRA checker would.

## Invocation

```
perl-lsp --heatmap <root> [--csv|--html] [--include-deps] [--all]
```

- `<root>` — workspace root; runs `cli_full_startup(root)` (workspace index +
  @INC resolve + SQLite warm).
- `--csv` — CSV instead of JSON. Mutually exclusive with `--html`; if both are
  passed `--csv` wins (it returns first).
- `--html` — a self-contained offline HTML viewer over the same report.
- `--include-deps` — also count references in cached `@INC` dependency modules
  (default: open + workspace files only).
- `--all` — keep every counted symbol in `symbols`; by default the array is
  trimmed to callables and dead candidates.

Output goes to stdout; startup chatter goes to stderr, so
`--heatmap … 2>/dev/null | jq` is clean.

## Metrics

Per symbol (subs, methods, packages/classes/modules; anonymous and
non-identifier-named symbols are skipped — no nameable graph):

- **`fan_in`** — reference *sites* across the searched roles, the symbol's own
  declaration(s) excluded. A reference site is any mention the builder records:
  call sites, qualified accesses, import-spec mentions (`use M qw(foo)`),
  export-list mentions. This is deliberately the broad definition; it never
  *under*-counts a live symbol's reachability (the safe direction for dead-code).
- **`fan_out`** — distinct callees a sub/method references in its own body
  (intra-file span containment over `FunctionCall`/`MethodCall`/`DispatchCall`
  refs; self-recursion excluded). `null` for packages.
- **`exported`** — whether the symbol is in the file's export surface.
- **`dead_code_candidate`** — `fan_in == 0` and no reachability guard fired.
- **`reachable_guard`** — when `fan_in == 0` but the symbol is not flagged, the
  reason it is treated as reachable. `null` otherwise.

## The over-approximation (honest labelling)

A `dead_code_candidate` is an **unreferenced symbol**: a reachability
*heuristic*, not MISRA C:2012 Rule 2.2 dead code (which is undecidable and would
invite a tool-qualification burden). The output `label` and `soundness` fields
say so in-band, and the `--heatmap` help text repeats it.

Reachability is over-approximated: the analysis errs toward *reachable* (may
under-report dead code) so it never falsely flags a live symbol. A zero-fan-in
symbol is shielded from the dead list — with `reachable_guard` set — when any of
these hold (checked most-specific first):

| guard | rule |
|---|---|
| `exported` | name is in the file's export surface — an external consumer may import it |
| `constructor` | conventional constructor (`new`) — frameworks instantiate it |
| `class-referenced` | a pack constructor whose CLASS is named anywhere in the row store (a type hint, `Foo::class`, a `use` row) with no `new` site of its own — a container or factory instantiates it (DI). Keyed by the bare class leaf, so two same-leaf classes in different namespaces shield each other: over-approximate, the sound side |
| `framework-synthesized` | symbol is plugin-minted (Moo accessors, routes, DBIC rels), not user-written; the framework calls it through machinery the static graph doesn't model |
| `package-implicit-use` | packages/classes/modules — reachable via `require`, app entrypoints, dynamic class strings; too many invisible vectors to flag |
| `dynamic-dispatch` | a **method-shaped** sub (declared in a non-`main` package) when the workspace contains **any** `$obj->$method` dispatch — see below |

### Dynamic dispatch is the load-bearing soundness gate

Perl method dispatch is fundamentally dynamic. The builder records a
`dynamic_dispatch_sites` count per file: every `$obj->$method(...)` whose method
name is a scalar rather than a bareword. Such a call produces no nameable
`MethodCall` ref (the dispatched method is unknown at build time unless
constant-folding resolves it), so it is invisible to the static reference graph.

When that count is `> 0` anywhere in the workspace, a sub that *could* be a
method (declared in a class — a non-`main` package) cannot be proven
unreferenced: an unresolved dynamic dispatch could target it, so it is shielded.
`main`-script free functions are excluded from this shield — they aren't class
methods, so their `FunctionCall` graph is authoritative.

## Honest failure modes

Even for a flagged candidate, "unreferenced" ≠ "safe to delete". The static
graph cannot see:

- **Symbolic code refs** — `\&name`, `&{$name}`, `*{"${pkg}::name"}` — invoke a
  sub by a string the analysis doesn't track. A flagged *function* candidate
  assumes none of these reach it.
- **`->$method` with an unresolved name** — counted as a `dynamic_dispatch_site`
  (which shields methods workspace-wide) but the *specific* target is unknown.
- **`AUTOLOAD`** — methods materialized at call time have no declaration to count.
- **String `eval`** — code (and calls) built at runtime are opaque.
- **External callers** — anything outside the indexed workspace (and, without
  `--include-deps`, outside open+workspace files). Exported symbols are guarded
  for exactly this reason.
- **Entrypoint-script free-subs** — a top-level `sub` in package `main` of an
  executable script is flagged when nothing calls it within the static graph,
  but a script is itself an entrypoint: its subs may be exercised by the runtime
  flow, a test/spec harness, or `\&main::foo` introspection. Proving these
  reachable is the job of a deferred entrypoint-analysis tier (the same tier
  `scan_entrypoint_scripts` / `file_analysis.rs`'s entrypoint-scan lint
  anticipate). Until it lands these are deliberately listed rather than
  blanket-shielded — under-shielding a script's own dead helpers is the honest
  direction, and `main`-script funcs are already excluded from the
  dynamic-dispatch shield. A `main` package heavy with zero-fan-in subs (common
  in spec/fixture scripts) is expected output, not a bug.

Treat the dead list as a **review queue**, not a delete list.

### C/C++ (pack languages)

Pack-language files light up the heatmap on the same machinery: symbols come
from the per-language sub-indexes (`ModuleIndex::for_each_pack_index` →
`for_each_registered_file`), fan-in is the identical `references()` projection
routed through the pack sub-index (construction-derived pack routing,
VISIBLE-wide because pack workspace files ride the DEPENDENCY role). Free functions group by file (like
Perl's `main`); class / namespace members group by `sym.package`. No language
branch.

C/C++ dead-code is more over-approximate than Perl's — a zero-fan-in symbol has
more invisible reachability vectors. Two are cheaply shielded:

- **`main`** — the runtime enters through it over the ABI, never a source call
  site (guard `entry-point`).
- **Address-taken / used-as-value functions** — `&fn` or a bare function-pointer
  decay is a *reference* (not a call), so it lands in `fan_in` and the symbol is
  never a candidate. No special guard: the reference graph already carries it.

The remaining vectors are not cheaply decidable, so a zero-fan-in symbol that
hits one is still listed honestly as a review-queue entry:

- **Exported / `extern "C"` ABI surface** — public functions are called by
  consumers outside the indexed tree. External-linkage functions are *not*
  blanket-shielded: that would silently drop every genuinely-unused internal
  helper, the actual C dead-code use case.
- **Function-pointer callbacks** — a callback registered into a table/struct the
  graph doesn't follow reads as unreferenced unless its name appears at the
  registration site.
- **Templates instantiated in an unscanned translation unit** — a template used
  only from an out-of-workspace TU reads as dead.
- **Prototype vs definition** — a function declared in a header and defined in a
  `.c`/`.cpp` lists as two rows, exactly as a Perl package reopened across files
  does; fan-in is identical on both.

## Identity invariant

Identity + counting go through `resolve::resolve(...)` at the symbol's declared
name token, then `references()` — the heatmap never maps a `Symbol` to a target
itself, so its counts cannot diverge from the references verb (the N-path
asymmetry the CandidateSet ADR exists to prevent). `heatmap_symbol_eligible` in
`lsp/cli/heatmap.rs` is only a listing policy (which kinds a usage report
shows), not an identity decision. Should whole-workspace scale ever demand a bulk path, it must
be built as a CandidateSet-based enumeration (one construction shared with the
projections), never a parallel walk over raw refs.

The dynamic-dispatch signal rides `FileAnalysis.dynamic_dispatch_sites` (`u32`,
`#[serde(default)]` on the bincode blob), populated in
`Builder::visit_method_call` when the method name is a scalar.

**Known references-side asymmetry**: a Moo `rwp`/`writer` synthesized method
shares the attr's declaration token, and the decl-side group answer does not
include the writer's call sites (references at the call site does link back). Its
heatmap row reports the attr-group image, not the writer's name-keyed count; the
dynamic-dispatch guard keeps it off the dead list.

## Visualization (`--html`)

`--html` renders the same report (no new computation — `heatmap_html()` wraps
the shared JSON value) as one self-contained HTML document. The template
`src/heatmap.html` is compiled in via `include_str!` and the report JSON is
inlined into a `<script type="application/json">` blob, so the file opens off a
`file://` URL with no server, CDN, or build step. The embed escapes every `<`
as its JSON unicode escape so a hostile path can't close the script element
early; drawing is dependency-free SVG. Three views over one `symbols[]` dataset:
a squarified treemap (grouped by package, tile area `fan_in + 1`, sqrt-lifted
heat ramp, dashed-amber outline for dead candidates), a back-to-back fan-in /
fan-out butterfly for the hottest symbols, and the dead-code review-queue table.
The `label` / `soundness` strings and `dynamic_dispatch_sites` count render in
the header so the viewer can't be mistaken for a sound dead-code prover.

## Deferred work

The two highest-value residuals are the same generalization: the framework
plugin already knows an edge the static call graph can't see, and the reference
machinery already computes the count — the heatmap just isn't consuming it. Both
must stay generic and plugin-owned (rule #10); neither is a per-verb or per-name
allowlist in core.

1. **Unblock Handlers via a plugin-owned "definition site."**
   `heatmap_symbol_eligible` admits only `Sub|Method|Package|Class|Module` and
   elides `SymKind::Handler` (routes / Minion tasks / events), yet `references()`
   on a Handler already returns every wire-up *and* dispatch site — a
   never-enqueued Minion task returns only its definition, an enqueued one
   returns two. So orphan-route / never-enqueued-task / never-emitted-event
   detection is already latent in the graph. The blocker for a correct fan-in is
   that a Handler's registration (`add_task(cleanup => …)`, `->to('X#y')`,
   `->on(evt => …)`) is itself one of its refs, so the "subtract
   `AccessKind::Declaration` + decl name-token span" logic won't exclude it.
   *Which* arg/span is the definition is plugin knowledge, so the plugin that
   mints the Handler must also stamp its definition span — a generic tag, the
   Handler-shaped equivalent of `AccessKind::Declaration` — and the heatmap
   subtracts that. Never a per-verb definition rule in core.

2. **Plugin-declared "framework-consumed" reachability.** The `dynamic-dispatch`
   shield is workspace-global and coarse: it shields every non-`main` method
   when any `$obj->$method` exists, yet misses a framework lifecycle hook with
   zero static callers and no dynamic dispatch anywhere (verified false-positive:
   Mojolicious `sub startup ($self)`, invoked by Mojo core out-of-workspace, is
   flagged dead — a "never falsely flags a live symbol" violation). The framework
   plugin knows which method names/roles it invokes (`startup`, `run`,
   `BUILD`/`DEMOLISH`, Moose triggers, DBIC `sqlt_deploy_hook`, …); let it mark a
   symbol framework-consumed (a witness/attribute asserting an invisible
   framework edge reaches it). Consumers then treat it as reachable
   (`reachable_guard = "framework-consumed"`, narrower than the blanket shield)
   and likely skip it for fan-out. This is the dead-code-reachability projection
   of the same edge the graph-walking `APP_SURFACE_CLASS` seam already models.

Also deferred: SARIF 2.1.0 output (`--format sarif`); transitive fan-out over
the cross-file call graph (the `GraphView` seam exists); a precision split
separating call-site fan-in from declaration-adjacent import/export mentions
once `RefLocation` carries its `RefKind`.
