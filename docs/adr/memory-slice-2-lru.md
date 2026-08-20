# Memory: evict the witness bag, rehydrate on demand

## The dominant resident cost is the witness bag, not the refs

Dropping the per-file macro gather caches (`evict_gather_caches_keep_headers`)
holds the abseil cold peak at 1.22 GB. Of that ~1.2 GB, measurement shows the
dominant resident bucket is the witness bag, not the `FileAnalysis` **refs**
held resident for every workspace file.

A new env-gated heap-composition probe (`PERL_LSP_HEAP_DUMP`,
`FileAnalysis::heap_estimate`) walks the live pack `ModuleIndex` after a cold
abseil index and sums each `FileAnalysis` bucket. Cold full-index over abseil
(877 C/C++ TUs: 488 `.cc` + 387 `.h`, `--references`, post-Slice-1 binary):

```
[heap-dump] FileAnalysis heap composition (877 files, ~857.2 MB estimated payload):
  refs                     157.7 MB  (18.4%)
  rebuilt_indices           20.4 MB  ( 2.4%)
  witness_vec              417.7 MB  (48.7%)
  witness_index            195.5 MB  (22.8%)
  symbols                   22.1 MB  ( 2.6%)
  include_closure           32.1 MB  ( 3.7%)
  scopes                     4.6 MB  ( 0.5%)
  bindings                   0.0 MB  ( 0.0%)
  cpp_extras                 5.5 MB  ( 0.6%)
  misc_maps                  0.3 MB  ( 0.0%)
  struct_shell               1.5 MB  ( 0.2%)
  TOTAL --------------     857.2 MB
```

Peak RSS for the same run: **1,265,240 KB = 1.207 GB** (matches the coordinator's
~1.23 GB). The ~350 MB gap between the 857 MB estimated payload and the 1.21 GB
RSS is allocator arena retention + transient parse trees + the `header_cache`
(2.1 MB) + the binary itself — the estimate is a heap-payload lower bound, not RSS.

### The headline

**The witness bag is 71.5% of the resident payload** —
`witness_vec` (417.7 MB, the `Vec<Witness>`) + `witness_index` (195.5 MB, the
bag's rebuilt attachment `HashMap`) = **613.2 MB**. `refs` is a distant second
at 18.4%.

Two facts make the bag the *ideal* eviction target — better than refs would
have been:

1. **`witness_vec` is already fully on disk.** `WitnessBag` is
   `#[serde(default)]` and rides the bincode+zstd blob in `modules-cpp.db`
   (26 MB on disk, whole tree). Dropping it resident costs nothing to reverse —
   it is a keyed decode away.
2. **`witness_index` is `#[serde(skip)]` — it is not even on disk.** It is
   rebuilt by `after_deserialize`. So 195.5 MB of the 613 MB is pure resident
   overhead that exists *only because we keep the bag resident and index it*.
   Dropping the bag makes it vanish with zero disk or recompute debt until a
   query actually rehydrates.

### Why the bag is dead weight for a workspace file

The witness bag is a **build-time type-inference scaffold** (see
`docs/adr/bag-canonical.md`). During `build()` the fold consumes it to a fixed
point and **bakes its conclusions into ordinary `FileAnalysis` fields**:

- `return_types` (name-keyed map) — `seed_return_types_from_bag`.
- `Ref::binding` — the build-time resolution outcome (dispatch edge,
  invocant class, package pin), filled PostFold.
- `Ref::arg_count`, `Symbol::arity`, `Symbol::deref_stack`, etc.

After the fold, a **non-open** workspace/dependency file's bag is re-read only
by *query-time type inference* — `expr_type_at_span` (reads `Expr(span)`
witnesses), `inferred_type_via_bag`, and the cross-file `PackageSymbol`
return-type chase (`find_method_return_type`). None of those run while sweeping
the tree for references/definitions/rename/workspace-symbol; they run only when
a query needs a *type* out of that specific file. For a file the user never
opened, that is rare — and when it happens, the exact bag is one keyed decode
from the 26 MB blob.

## Decision

**Slice 2 = evict the witness bag from every resident *pack workspace*
`FileAnalysis` after the fold bakes its results, and rehydrate the exact file's
bag on demand from the existing SQLite blob into a small byte-capped LRU.**

Nothing else changes: the analysis results are identical, only *where the bag
bytes live* changes (on disk + LRU-on-demand instead of resident-always).

### Pinned (never evicted — this is the completeness guarantee)

Held resident for **every** indexed file, exactly as today:

- the `RefTable` (refs + their name/target/call indices) — answers
  `references` / `documentHighlight` with exact ranges.
- the `SymbolTable` (symbols + their name/scope indices) — answers `goto-def` (via
  `Ref::binding`'s method target + symbol lookup), `hover` on a definition,
  `rename` targets.
- The pack `ModuleIndex`'s `all_files` / `all_defs` name→file index — answers
  `workspace/symbol` and "which files could reference X" without touching any
  body.
- `PackFacts` (the include closure, specialization edges, template params),
  `packages`, `return_types` — the cross-file visibility / inheritance /
  baked-return metadata the graph walk and MRO need.

The completeness-critical, whole-tree queries are **bag-free by construction**
(none of them call a reducer). So dropping the bag cannot make references or
symbols incomplete — the abseil 13-refs-incl-`_test.cc` case rides
the `RefTable` target index, which stays pinned.

### Evicted (the 71.5%)

`FileAnalysis::witnesses` (both the `Vec<Witness>` and its rebuilt `index`),
dropped from the **resident pack-index copy** of each workspace/dependency file.
The **disk blob keeps the full bag** (persisted before the strip), so
rehydration is lossless.

Open documents (`FileStore::open`) keep their full analysis **with** the bag —
hover / completion / signature-help re-query it live. Only the pack
`ModuleIndex` workspace copies are stripped. (A file can be in both: the small
open set holds the fat copy, the index copy is thin.)

### The eviction seam

In `module_resolver::index_pack_languages`, both feed paths converge on
`pack_index.register_symbols(path, Arc<FileAnalysis>)`. Strip the bag on the
resident copy immediately before that `Arc::new`, **after** the full blob is
serialized to disk:

- **Fresh path** (`par_iter` body): today it does
  `Arc::new(analysis)` → `register_symbols` → later `save_to_db`. Reorder so the
  blob is encoded from the full analysis *first*, then
  `analysis.evict_witness_bag()` (a new `&mut self` method that clears
  `witnesses`), then `Arc::new` + register. The disk write must see the full
  bag; the resident Arc must not.
- **Warm path** (`decode_analysis` → `register_symbols`): the blob on disk is
  already full; call `evict_witness_bag()` on the decoded `FileAnalysis` before
  wrapping it for registration.

`evict_witness_bag` lives on `FileAnalysis` (model layer, rule #2): it sets
`self.witnesses = WitnessBag::default()` and a `bag_evicted: bool`
(`#[serde(skip)]`) flag so consumers can tell "empty because evicted" from
"genuinely no type facts". It does **not** touch any pinned field.

### The rehydration path + LRU

A new `PackBagCache` (owned by the pack `ModuleIndex`, or a sibling keyed by
`(lang, path)`):

```
DashMap<PathBuf, Arc<FileAnalysis>>   // full, bag-present, transient
+ recency stamps (atomic clock)       // LRU
+ byte-cap accounting                 // initializationOptions.maxCacheMb
```

Query-time flow when a *type* query needs file `F`'s bag (the only consumers
are the reducers / `expr_type_at_span` / `find_method_return_type` reaching
cross-file):

1. If the resident index copy of `F` has a non-evicted bag (open doc, or never
   stripped), use it — no rehydration.
2. Else look up `F` in `PackBagCache`. Hit → touch recency, return the full Arc.
3. Miss → `SELECT analysis FROM modules WHERE path = ?1` on the pack conn →
   `decode_analysis` (zstd → bincode → `after_deserialize` rebuilds the bag
   index) → `Arc::new` → insert, evicting the lowest-recency entries while over
   `maxCacheMb`.

Because bags average ~700 KB/file (613 MB / 877), a **128 MB default cap** holds
~180 files' bags — far more than any single interactive type query fans into.
Resident floor becomes: bag-less index (~244 MB payload) + LRU cap (≤128 MB) ≈
**~0.4 GB payload → RSS well under the 0.5 GB target**, versus 857 MB / 1.21 GB
today.

`maxCacheMb` is surfaced via `initializationOptions` (default 128); 0 disables
the LRU (rehydrate-and-drop, never retain) for the most aggressive footprint.

### Why not a lightweight name index + whole-body LRU?

The alternative evicts the *whole* `FileAnalysis` and keeps a separate compact
`{name → file}` table pinned. Measurement makes it strictly more work for less:
refs are only 18.4%, so evicting them buys little while
forcing a rehydrate on the *hot* references path (the completeness differentiator
we must keep instant) and demanding a whole new pinned-index structure. Evicting
the bag instead (a) targets 71.5%, (b) leaves the hot path fully resident, and
(c) needs no new pinned index (`all_defs` already is one). The whole-body LRU is
kept as an optional **Slice 3** to chase clangd's ~320 MB, not needed to hit
0.5 GB.

## Completeness is preserved — the proof

| Query | Projection reads | Bag? | Evicted-file behavior |
|---|---|---|---|
| `references` / `documentHighlight` | `RefTable` (refs + target index) | no | complete, resident |
| `goto-def` | `Ref::binding` (method target), `symbols` | no | complete, resident |
| `rename` / `prepareRename` | `refs`, `symbols` | no | complete, resident |
| `workspace/symbol` | `all_defs` (name→file) | no | complete, resident |
| `hover` on a def / signature | `symbols`, `return_types` | no | complete, resident |
| `hover`-type / type-constrained completion | reducers / `expr_type_at_span` | **yes** | rehydrate exact bag from SQLite |
| cross-file method-return chain | `find_method_return_type` → `PackageSymbol` | **yes** | rehydrate target file's bag |

The whole-tree completeness invariant lives entirely in the first five rows, all bag-free
— so it holds by construction, resident or not. The two bag-consuming rows are
*type-inference value-adds*, not the completeness differentiator; they rehydrate
the **exact** persisted bag and therefore return byte-identical answers to
today. The transparency invariant holds: a rehydrated `FileAnalysis` is the same
blob → same struct → same `after_deserialize`, so no projection can observe the
cache.

## Concurrency

- The pack `ModuleIndex` I/O runs on its dedicated `std::thread`; async handlers
  call `_cached` methods only. Rehydration is SQLite I/O, so it must **not** run
  inline in an async handler — route it through the module-index thread (or
  `spawn_blocking`), never holding a `FileStore`/`DashMap` guard across the read
  (`filestore-guard-discipline`: snapshot `Arc::clone`, drop the guard, then
  rehydrate).
- `PackBagCache` is a `DashMap` + atomic recency stamps: the LRU touch/evict
  runs under a short per-shard lock, never across an `.await`. No global lock;
  no new guard-across-await hazard.
- Rehydration is idempotent and racy-safe: two threads decoding `F` produce
  equal Arcs; last insert wins, the other is dropped — no correctness impact
  (monotone bag, same blob).

## EXTRACT_VERSION verdict

**No bump. Stays 162.** The serialized SQLite shape is unchanged — the full bag
is still what gets written and read. Slice 2 changes only the *resident
lifecycle* (strip after persist; rehydrate on demand) and adds a `#[serde(skip)]`
`bag_evicted` flag. Neither touches the bincode/zstd blob layout, so existing
`modules-cpp.db` caches warm without re-resolution.

## Measurement instrumentation (inert by default)

- `FileAnalysis::heap_estimate() -> HeapBreakdown` (`file_analysis.rs`) — per-
  bucket resident-payload estimate for one analysis; `HeapBreakdown::add` sums
  across files; `Display` prints the table above.
- `WitnessBag::heap_bytes_estimate()` (`witnesses.rs`) — `(vec, index)` bytes.
- `PERL_LSP_HEAP_DUMP`-gated aggregate at the end of
  `index_pack_languages`, walking `hub.for_each_pack_registered_file`.

Methodology (honest bounds): flat `size_of<T>() × capacity` for every
collection (so `Vec`/`HashMap` slack is counted) plus deep `String` capacities
for the dominant string buckets (ref target-names, symbol names, include
closure, reverse-index keys). Deep strings inside long-tail structs are not
drilled — a modest undercount concentrated in `cpp_extras`/`misc`, not in the
bag or refs. `witness_vec` is the most reliable bucket (a flat `Vec<Witness>`,
no string undercount), so the 71.5% headline is robust. Reproduce:

```
perl-lsp --clear-cache <abseil-root>
PERL_LSP_HEAP_DUMP=1 PERL_LSP_MEM_REPORT=1 /usr/bin/time -v \
  perl-lsp --references <abseil-root> <abseil>/absl/strings/string_view.h 41 15
```

---

**Scale follow-up:** at Chromium scale (131K C++ files) per-file resident cost
is linear at ~0.51 MB/file (bag eviction cuts it ~3.4× vs the bag-resident
model), projecting whole-tree Chromium to ~67 GB. The pinned `refs`/`symbols`
are the wall, and
the analysis argues the next step is a **relational SQLite reverse-index**
(shred refs into indexed tables, query on disk) rather than a Slice-3 ref-LRU.
