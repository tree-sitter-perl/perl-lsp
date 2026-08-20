# ADR: The span-free Surface and the freshness engine

Related: `docs/adr/relational-ref-index.md` (the refs/symbols relational
shred this composes with), `docs/adr/memory-slice-2-lru.md` (the
completeness invariant and eviction lifecycle this reuses).

## Context

Two residency/correctness problems share one missing piece. First, the
relational shred (`docs/adr/relational-ref-index.md`) needs a stable,
span-free row source — re-shredding a file's rows on every body-local edit
would be wasteful and would defeat early cutoff. Second, cross-file
enrichment (propagated return types, synthetic hash-key defs) has no
consumer→dependency edge: when file B changes, nothing knows which files'
enrichment is now stale, so enrichment can only run for open documents,
brute-forced on every resolver tick.

## Decision: the Surface — a position-independent per-file projection

`Surface::project(&FileAnalysis)` (`src/model/surface.rs`) produces a
per-file projection of cross-file-VISIBLE facts:

```
Surface {
    packages: [ {
        name,
        parents,          // resolved isa/roles/bridges, post-fold
        methods: [ { name, kind, arity_shape, return: InferredType,
                     hash_keys, provenance } ],
    } ],
    imports, exports, reexports,
    export_tags,             // tag → members: `:tag` grouping is
                              // cross-file semantics even when the flat
                              // export set is unchanged
    plugin_bridges, app_surface_consumers,
    dbic_source_name,        // consumers' resultset('X') resolve through it
    values, free_values,     // linkage-visible non-callables
    macros,                  // name/params/body/guards — the body IS
                              // cross-file semantics under textual inclusion
    include_specs,           // raw #include specs
}
```

Contract (each clause is load-bearing; a Surface field addition without an
equality-net arm is a review reject, and the FileAnalysis→Surface direction
is compiler-enforced: `FileAnalysis::surface_feed` destructures every field
with no `..` rest pattern, so a new FileAnalysis field cannot compile until
its author classifies it — bound into the feed and projected, or discarded
with a stated not-cross-file-visible reason):

- **No spans, no `Point`s, no byte offsets, no `ScopeId`/`SymbolId`/
  `RefIdx`, anywhere.** Equality of two Surfaces must mean "no
  cross-file-visible change". A body edit, a reformat, a comment, a
  private-local rename yields an **equal** Surface — that equality is the
  early-cutoff firewall (rust-analyzer's "typing in a body never
  invalidates global data"). One smuggled span collapses the firewall
  silently; the equality-net tests (reformat/body-edit → equal;
  return-type/method/parent/export/import change → unequal; bincode
  roundtrip) are the regression net. File-internal attachment identities
  inside a type (a `CodeRef` body edge) are sanitized by `despan`.
- **Typed fields, not display strings** (rule #10's lossy-string form).
  `return: Option<InferredType>`, never `"returns Foo"`. Consumers project.
- **Derived from the post-fold `FileAnalysis`**, emitted by the builder as
  a sibling output — a pure projection, produced once per build,
  immediately after `finalize_post_walk()`.
- **Canonical ordering.** Everything is sorted so builder iteration order
  can never masquerade as a semantic change.
- **Language-neutral shape.** Perl packages, C++ classes/namespaces,
  Python classes all fill the same struct; the `language_driver` seam
  gives each driver its own extraction, and the Surface is the shared
  vocabulary above it. A consumer switching on language to read a Surface
  is the same rule-#10 bug as switching on shape.

### Surface ≠ outline (don't ride documentSymbol)

Tempting and wrong: the LSP outline (`OutlineSymbol`) looks like "the
symbols in this file" too. The change-sets are orthogonal, and both
mismatch directions bite:

- **Under-invalidation (silent wrongness):** the outline can't see a
  resolved return-type change (`return $x` → `return {...}`), hash-key
  changes, or `@ISA`/`with` edits that keep the symbol list identical.
  Keyed on outline, dependents keep stale types with no crash to notice.
- **Over-invalidation (perf):** the outline is span-bearing by
  construction (`span`, `selection_span`) and changes on private-helper
  adds and on every sub that moves. Keyed on outline, unrelated edits
  stampede dependents.

The relationship is inverted, rust-analyzer's `ItemTree`/`AstIdMap` split:
the Surface is the **lower**, position-independent layer; the outline is a
span-bearing *sibling* projection sharing only the stable symbol-identity
spine.

## Decision: the freshness engine — hand-rolled reverse-dep, not Salsa

`FreshnessIndex` (`src/model/surface.rs`) is a name-keyed reverse-dependency
index over Surface records: file A's enrichment depends on `Surface(B)`
for each B in A's imports ∪ parent chain ∪ bridges. Recording a fresh
build's Surface against the prior one yields a `SurfaceVerdict`
(Unchanged/Changed); a Changed verdict walks `dirty_consumers`
transitively (seen-set guarded) and re-enriches exactly that closure.
`FreshnessIndex` retains fingerprint + provided names per file, never full
Surfaces resident — macro bodies resident at tens of thousands of files
would rebuild the stripped-payload wall this design exists to avoid.

The engine is hand-rolled (a reverse-dep index + dirty-set walk on Surface
inequality, no external dependency) rather than an incremental-computation
framework (Salsa): the design already owns the reverse-index discipline
elsewhere in `ModuleIndex`, and the choice sits entirely behind the
Surface/`SurfaceVerdict` boundary — swapping engines later touches only
the recording sites, not consumers. This trade-off is ratified in
`docs/forks-resolved.md` ("Freshness engine: hand-rolled reverse-dep vs
Salsa"); revisit if the query graph deepens past what a dirty-set walk
comfortably serves (e.g. the phase-4 materialized views below).

Consumers: `Backend::republish_open_docs_in` re-enriches open Perl
documents on a Changed verdict (through `FileStore::enrich_open`, the one
open-doc enrichment writer; the open doc's own record is its build-time
`Document::baseline_surface`, so enrichment never reaches a freshness
record); `PackInvalidator::file_changed`'s gate
(`src/index/pack_invalidator.rs`) re-analyzes the changed file first and
records its surface — an Unchanged verdict skips consumer eviction and
re-analysis entirely, open-document re-gathers included (a deep-header
comment edit re-parses one file, not every translation unit). Consumer
discovery for the pack tier stays include-closure-based (the one
`is_consumer` rule in the invalidator); `FreshnessIndex` needed no closure
edge kind, only its equality half gates the pack tier.

## Residency: the R4 enrichment overlay

Closed workspace/dependency files answer enriched through
`ModuleIndex::enriched_snapshot` / `CrossFileLookup::enriched_present`
(`src/index/module_index/`, `src/model/file_analysis/`): derived,
fingerprint-and-generation-keyed copies, byte-capped and LRU-bounded,
cycle-guarded, consulted FALLBACK-ON-MISS after the raw bag answers.
Consumers: `query_sub_return_type`'s imported recursion and the
`PackageSymbol` cross-file primary (`src/model/witnesses/`), the diagnostics
sweep, and `--check`/`--dump-package`. See CLAUDE.md's "Cross-file
enrichment" section for the full consumer/derivation contract — this ADR
owns the Surface/freshness design that overlay is keyed on, not its
mechanics.

## Warm registration (the stub lane)

Each persisted pack file gets a warm stub in a separate `stubs` table
(separate from `modules` so reading a stub never drags the analysis
blob's overflow pages) — registration feed + specialization edges +
projected Surface + a stripped skeleton. Warm start registers from stubs
without decoding full blobs (`warm_pack_stream_with_stubs`); declined
lanes (rows missing, `NO_EVICT`, decode break, `stub_version` gate) fall
back to a point full-decode that backfills the stub. Any modules-row
rewrite deletes the path's stub (inside `save_to_db`/
`save_blob_to_db_stamped`, so writers can't forget); hard-clears wipe via
`clear_derived_rows`.

Measured (abseil): warm start 0.4 s, warm peak RSS 34 MB (cold unchanged);
references byte-identical, `--refs-parity` clean, gold unaffected.

## Registration inverse under symbol eviction

`unregister_file` cannot walk `old.analysis.symbols` for the names to
remove — under symbol eviction that vec is empty, and rehydrating would
fetch the NEW generation's names (a wrong inverse after an edit
persists). `ModuleIndex.registered_names` records the (name, is-class)
pairs per path at registration time: an exact inverse by construction
with no read-path cost, and the class-rank source for the cache-slot
tie-break (which also read evicted symbols). Cost: one
`Vec<(String, bool)>` per registered file, bounded and measured into the
resident floor. The map is private to `module_index.rs`; a self-healing
read-side validation could replace it without touching call sites
outside registration/unregister (`docs/forks-resolved.md`, "Unregister
inverse under symbol eviction").

## Deferred

**Materialized SQL views over the shred** — the "interesting data" query
surface (unused exports, implementors-of-a-role, callers-by-arg-type) that
the relational shred (`docs/adr/relational-ref-index.md`) makes possible
once the freshness engine keeps it perpetually true. Not yet built;
revisit whether the hand-rolled dirty-set walk above still suffices once
this query graph exists, or whether it's the point to reconsider Salsa.
