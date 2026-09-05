# ADR: One file store, role-tagged. One resolve(). One refs_to().

The LSP has one role-tagged store, not three parallel ones
(`Backend.documents`, `Backend.workspace_index`, `ModuleIndex.cache`) with
per-query tier walks that each independently pick which subset to consult.
Parallel stores make every query a place where a tier can be forgotten — the
source of "find-references mysteriously misses dep call sites" bugs.

A lossy `@INC` `ModuleExports` summary (no refs, no type constraints, no call
bindings) degrades cross-file features at the module boundary even when both
files have been parsed; there is no such summary here.

## Decisions worth keeping

### Full `FileAnalysis` everywhere

There is no `ModuleExports` summary type. Workspace files, dep modules, and open files all
store an `Arc<FileAnalysis>`. The SQLite cache (`module_cache.rs`,
schema v10) stores `zstd(bincode(FileAnalysis))` — full fidelity round-
trips through the cache. Cross-file refs, type constraints, call bindings,
imports all survive the module boundary.

When you need data from another file: get the file's `FileAnalysis` and
read the field. No "summary type" detour.

### Single store, role-tagged: `FileStore`

```rust
pub enum FileRole { Open, Workspace, Dependency, BuiltIn }
```

`src/index/file_store.rs` holds every parsed file, in two role-keyed maps:
`open: DashMap<Url, Document>` and `workspace: DashMap<PathBuf, Arc<FileAnalysis>>`.
Role is implicit in which map holds the path — `FileRole` names the tags but
isn't itself stored on an entry. A workspace file that becomes open is
promoted by removing it from `workspace` and inserting it into `open` (and
demoted the same way on close); readers hold Arcs and see consistent
snapshots either way.

Rule: **don't add a fourth store**. New file sources (REPL buffers, virtual
docs, whatever) become a new `FileRole`, not a new map.

### Role mask: explicit at every call site

```rust
bitflags::bitflags! {
    pub struct RoleMask: u8 {
        const OPEN       = 1 << 0;
        const WORKSPACE  = 1 << 1;
        const DEPENDENCY = 1 << 2;
        const BUILTIN    = 1 << 3;
        const EDITABLE   = OPEN | WORKSPACE;       // rename stops here
        const VISIBLE    = EDITABLE | DEPENDENCY | BUILTIN;
    }
}
```

Every cross-file query passes a mask. Rename uses `EDITABLE` (deps are
read-only). References uses `VISIBLE` (yes, search deps too). Diagnostics
use `VISIBLE`. Forgetting a tier is now visible at the type level — you
have to write the mask down.

### Single `refs_to`, single resolution entry point

`src/index/resolve/` is the only place tier-walking lives. LSP handlers do not
iterate `FileStore` directly. Adding a new cross-file query = picking a
mask and calling `refs_to`.

The walk: a mask-gated sweep over open/workspace/dependency files, each file
admitted or skipped by `file_sees_target_ids` (the inclusion-closure check),
with `GraphView` (`docs/adr/graph-walking.md`) consulted for ancestor/bridge
reasoning at candidate sites. `refs_to` reads each admitted file's `RefTable`
target index (`refs_to_symbol`) for O(1) per-file lookup.

The inverse direction — `resolve_symbol_scoped` (cursor → target) — is the
identity step for "what does this position refer to, cross-file", invoked
internally by `resolve(cursor) → CandidateSet`
(`docs/adr/resolution-candidate-set.md`), the actual single entry point every
LSP handler and CLI mirror routes through. It returns
`ResolvedTarget::Target(TargetRef)` for walkable targets (callables,
packages, handlers, owner-resolved hash keys) or `ResolvedTarget::Local`
for inherently file-local ones (lexical variables, unowned hash keys).
Routing every handler and CLI mirror through the same `CandidateSet`
construction is what keeps target identity from diverging by entry point —
the bug class this killed was CLI references silently dropping owned hash
keys to single-file while LSP walked them cross-file. Per-feature *policy*
on a resolved target is asked of the value
(`TargetRef::supports_cross_file_rename`), never re-encoded per handler.
`resolve_symbol` itself survives only as a thin `resolve_symbol_scoped`
wrapper for tests probing identity directly.

### Lazy enrichment, idempotent

Workspace files aren't enriched at index time — only on-demand when a query
needs cross-file types. The `SymbolTable` / `RefTable` enrichment baselines
and `base_witness_count` mark the post-build seal so re-enrichment truncates
back to baseline before re-deriving. `FileStore::enrich_open` re-derives on
every call — clone off the lock, re-run `enrich_imported_types_with_keys`,
swap in behind an `Arc::ptr_eq` guard that only protects against clobbering
a concurrent rebuild — idempotency comes from the baseline truncation, not
a stored flag that skips the work.

## Where this is going

The `Namespace` enum (`model/file_analysis/core_types.rs`) landed. Forward
residual, tracked where each now lives:

- Eager `Ref.target: SymbolId` (not `Option`) — the resolved symbol still
  rides `Ref.binding` (`RefBinding`, read via `resolved_symbol()`);
  tracked in `prompt-cst-migration.md`.
- `Openness` classification + unified diagnostic rule — tracked as the
  Scope-node taxonomy's Openness diagnostic in `prompt-graph-walking.md`
  and `ROADMAP.md`.
- Framework emission rules into framework Namespaces (the Mojo
  intelligence path) — not yet started, no tracking doc.

CLAUDE.md "Cross-file resolution" + "Cross-file enrichment" sections
describe the live behavior in operational detail.
