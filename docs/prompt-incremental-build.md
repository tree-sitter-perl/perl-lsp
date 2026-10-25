# Incremental analysis — design space

**Status: ideas, not a plan.** Grounded in the giant-file measurements
(FHEM `76_SolarForecast.pm`, 46,522 lines / 2.6 MB; `docs/scaling-limits.md`
§6). The two accidental quadratics in the fold are fixed and the registry
warm is off the first build's critical path, so the numbers below are the
honest residual — what incrementality would actually be buying.

## The measured baseline

Cold build of the 46k file, post-fixes, per `[build-scope]`:

```
parse                 ~350 ms   (already incremental on edit — see below)
build::walk            ~700 ms
build::pattern_dispatch ~900 ms
fold_to_fixed_point    ~980 ms   (chain rhs_probe ~490 ms of it)
build::finalize        ~420 ms
misc (POD, flow, …)    ~400 ms
total                 ~3.5-4 s
```

Nothing left is an accidental term; this is the real cost of analyzing
2.6 MB of Perl from scratch. The problem is WHERE it's paid:

**`did_change` runs the full rebuild synchronously on the message loop.**
Perl's driver declares `synchronous_rebuild` (grandfathered as "cheap
build" — which the giant file falsifies), so every didChange on the 46k
file head-of-line blocks all requests for ~3.5 s. Pack languages already
have the other lane: `update_text_only` (reparse + text swap, positions
stay live) + debounced `spawn_blocking` rebuild + `apply_rebuilt`.

**Incremental parsing already works.** `Document::update` diffs
prefix/suffix, calls `tree.edit(InputEdit)`, and reparses with the old
tree — a keystroke reparse is ms-scale, not the 350 ms cold parse. Only
the ANALYSIS is monolithic: `builder::build` discards everything and
re-derives from the fresh tree.

## Tier 0 — stop blocking, reuse the existing lane (small, ships alone)

Make `synchronous_rebuild` a per-DOCUMENT verdict instead of per-language:
above a size threshold (or better: above a measured last-build wall,
which the Document can remember), a Perl doc takes the pack-language
path — `update_text_only` immediately, debounced `spawn_blocking`
rebuild, verbs serve the previous analysis meanwhile. Zero new
machinery; the threshold is the only new decision. This does not make
analysis incremental — it makes its cost invisible to the editing loop,
which is most of the complaint. Rule-#10 note: gate on the measured
property (last build wall), not on a size heuristic enumerating "big
file" shapes.

## Tier 1 — incremental collect (dirty-subtree re-walk)

Tree-sitter's `Tree::changed_ranges(&old_tree)` names exactly which
byte ranges have different structure after an edit. The walk (~700 ms)
and pattern dispatch (~900 ms) re-derive facts for the WHOLE tree; for
a keystroke, almost all of that output is identical modulo spans.

Shape: keep the previous `FileAnalysis`; re-walk only the subtrees
covering the changed ranges (plus their enclosing sub/package, since
scope extents and implicit-return last-expression change with content);
splice the region's symbols/refs/scopes/witnesses over the old ones.

The two hard parts, named honestly:

* **Span shift.** An edit shifts every span after it. Either every
  stored span becomes anchor-relative (a deep change touching
  `FileAnalysis`, the caches, and every consumer), or a remap pass
  adjusts absolute spans by the edit delta — O(n) over spans but pure
  arithmetic, no tree access; likely tens of ms on the 46k file. The
  remap is the pragmatic first form. Note `Point` is (row, col): a
  single-line edit shifts columns on its own row and rows below only
  when it adds/removes lines — the remap rule is small but must be
  exactly right or every downstream index quietly lies.
* **Region ownership.** Every collected fact must be attributable to a
  region so "delete the dirty region's facts" is sound. Facts that are
  derived ACROSS regions (namespace_ranges trimmed by successor decls,
  constant folds read at distance, `use` effects on later code) need
  either recomputation triggers or conservative widening ("an edit
  inside a package re-collects the package"). Widening to the enclosing
  top-level item (sub / package block / use-block) is probably the
  right first granularity: FHEM-style files are hundreds of small subs,
  so the dirty item is ~1/700th of the file.

## Tier 2 — incremental fold

The worklist fold is NATURALLY incremental in one direction: witnesses
are monotone, reducers are stateless, and the fixed point is a lattice
join — adding the dirty region's re-collected witnesses to a settled
bag and re-running the fold converges to the same answer as from
scratch. The direction that is NOT free is retraction: an edit deletes
facts, and the bag has no per-region undo.

Precedent exists: enrichment already does truncate-to-baseline
(`base_witness_count`, seal + truncate + re-derive). The incremental
form is region-scoped: witnesses carry (or are bucketed by) their
region, "truncate region R" drops exactly its contribution, then the
fold re-runs. The re-emittable passes (clear-and-emit by source tag)
already tolerate re-running; the fold post-fixes is ~1 s on the worst
file and near-nothing when the bag barely changed, because the
snapshot converges in one iteration when nothing moves.

Corollary from the §6 investigation: **do not chunk the solver.** The
superlinearity that motivated chunking was accidental and is gone;
witness edges (`Expr(span)` chains, implicit returns, constant folds)
cross any partition freely, so per-chunk fixed points don't compose.
Incremental-collect + GLOBAL re-fold gets the win without the
soundness burden.

## What incrementality does NOT need to touch

* Cross-file: enrichment, the Surface freshness gate, and the R4
  overlay already scope cross-file work; an in-file incremental rebuild
  ends at `FileAnalysis::new` + the same enrichment entry points.
* The cache: an open doc's incremental state is in-memory only; the
  persisted blob stays a whole-file artifact.
* semanticTokens delta encoding etc. — client-facing incrementality is
  orthogonal and already handled per-verb.

## Sequencing and the bar

Tier 0 alone converts "editor unusable on giant files" into "diagnostics
lag a debounce interval" and is a day of work. Tier 1 without Tier 2 is
already most of the win (walk + dispatch + finalize ≈ 2 s of the 3.5 s;
the fold self-limits when the bag is stable). Tier 2 is only worth it if
profiling after Tier 1 shows the global re-fold dominating keystroke
cost — measure before building it.

Done-criterion candidates (measure per the §6 rules — one claim per
invocation, through the LSP, `[build-scope]` for attribution):

* keystroke → publishDiagnostics on the 46k file under 500 ms warm;
* no pull verb blocked behind a rebuild (Tier 0's property);
* `build_with_plugins` full-vs-incremental equivalence net, same shape
  as `PERL_LSP_PD_EQUIV`: rebuild both ways on a corpus of recorded
  edit scripts, assert identical `FileAnalysis` (the walk-equiv
  precedent already exists in `pipeline.rs`).
