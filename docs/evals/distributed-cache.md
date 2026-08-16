# Eval: distributed analysis cache — CI-published blobs, locally-overlaid

> Status: **direction recommended, not yet built** — a forward-design
> investigation (prior-art census + failure-mode contract), not a landed
> decision. The recommended shape is at the end ("The recommended shape");
> the failure-mode rules are the contract any implementation must satisfy.
> Revisit triggers: (a) an implementation starts — graduate the recommended
> shape to an ADR and leave this as the paper trail; (b) a shop actually
> deploys against a Chromium-scale tree and the fetch-vs-rebuild economics
> can be measured instead of estimated.

## The question

For a huge codebase (Chromium-scale C++), can the backend become
distributed — a central store holding FileAnalyses for `main`, each dev box
overlaying its branch's changes — so cold start and whole-project queries
stop costing every developer the full local index? And: did the shops that
tried this keep it, or walk it back?

## Verdict

The shape is sound and well-precedented: three independent organizations
(JetBrains, Meta, Google) shipped exactly "CI publishes per-file analysis
for main; the developer's machine overlays local changes" and kept it.
The failures in the record cluster around five things — live query servers
over-centralizing, global merge steps, session/machine-coupled artifacts,
under-specified cache keys, and unguarded write paths — each avoidable by
construction here. Our per-file `FileAnalysis` + query-time cross-file
resolution + span-free Surface is already the artifact shape the successful
systems converged on; the missing piece is cache **identity** (today:
path-keyed, mtime+size-validated, absolute paths, stat-based
`closure_stamp` — machine-local by construction).

## Prior-art census

### JetBrains Shared Indexes — the closest match, still alive (self-hosted)

- CI runs a headless indexer (`ij-shared-indexes-tool-cli`), uploads chunks
  to a **dumb** file server / S3 / CDN — no live service. A committed
  `intellij.yaml` points IDEs at the base URL.
  <https://github.com/JetBrains/intellij-shared-indexes-tool-example>,
  <https://www.jetbrains.com/help/idea/shared-indexes.html>
- **The load-bearing decision: attachment is per-file content-hash, not
  per-commit.** The IDE downloads a chunk, attaches every file whose
  content hash matches, and locally indexes the rest. A stale chunk still
  hits most files; branch edits are just the non-matching remainder —
  overlay for free, no commit-matching logic.
  <https://blog.jetbrains.com/idea/2020/07/shared-indexes-plugin-unveiled/>
- Compatibility key: identical IDE build required (every upgrade
  invalidates the corpus); chunks were OS-specific and that was walked
  back to OS-independent.
- Recorded failure modes: multi-GB chunk downloads
  (<https://youtrack.jetbrains.com/issue/IDEA-317465>), re-download on
  every startup from a cache-identity bug
  (<https://youtrack.jetbrains.com/issue/PY-59850>), producer-side
  statefulness trap (incremental upload needed the previous index present,
  <https://youtrack.jetbrains.com/issue/IDEA-251328>), and the docs' own
  concession that wins require "a sufficiently fast network." The public
  OSS/Maven index CDN quietly disappeared; no retrospective published.

### Meta Glean / Google Kythe — batch-central + local overlay, documented

- Both: index post-commit code centrally, serve from a symbol server, thin
  local layer covers the delta. Kythe's language server **diff-patches**:
  unchanged code keeps served cross-references, changed code returns
  nothing until reindexed.
  <https://kythe.io/docs/kythe-overview.html>,
  <https://groups.google.com/d/topic/kythe/k9kUZE05vpw>
- Glean makes the overlay a first-class storage primitive: incremental DBs
  **stack** on a base DB, hiding excluded units via ownership bitmaps.
  Published costs ~7% size / 2–3% index-time overhead — but per-fact
  *ownership propagation* machinery was required to keep derived facts
  sound, and incremental **derivation** across stacks was admitted
  unfinished. The hard part of an overlay is not per-file facts; it is
  derived/cross-file facts whose provenance spans base+overlay.
  <https://glean.software/blog/incremental/>,
  <https://github.com/facebookincubator/Glean/blob/main/glean/website/docs/implementation/incrementality.md>
- Staleness stance (SWE-at-Google ch. 17): acceptable by default, dangerous
  exactly for the user's own recent edits; **prefer "no answer" over a
  stale-span answer.** <https://abseil.io/resources/swe-book/html/ch17.html>

### clangd remote index — the live-server shape, deliberately coarse

- Only the **global symbol index** is remoted (gRPC `SymbolIndex`: Lookup /
  FuzzyFind / Refs / Relations). Preambles, ASTs, per-TU semantics stay
  local; open-file dynamic index shadows the remote via `MergedIndex`.
  Path relocation = strip server `--project-root`, prepend client
  `MountPoint` — possible only because payloads are `file:line:col`, not
  machine-coupled blobs. <https://clangd.llvm.org/design/remote-index>
- **AST/preamble sharing evaluated and documented infeasible** (ASTContext
  coupling, thread-unsafety) — the recorded reason only the coarse tier is
  shared. <https://github.com/clangd/clangd/discussions/1240>. That
  limitation does not apply here: `FileAnalysis` is serde/machine-portable
  by design, so we can share the rich artifact clangd can't.
- Chromium's deployment: daily cron indexer VM (builds only generated
  files, `clangd-indexer --executor=all-TUs`), six per-platform indexes
  served from 48 GiB VMs behind a TCP LB; index "about a day old";
  staleness accepted with **no position remapping**; branch skew is
  user-borne. What it replaces per-developer: ~2–3 h background index on a
  48-core box, ~2.7 GB RSS in clangd.
  <https://github.com/clangd/chrome-remote-index/blob/main/docs/index.md>,
  <https://raw.githubusercontent.com/chromium/chromium/main/docs/clangd.md>
- The instructive mistake: configuring an external index **silently
  disables local background indexing** — a dead server shrinks coverage to
  open files with no local fallback.
  <https://raw.githubusercontent.com/llvm/clangd-www/main/config.md>
- Rejected on privacy grounds: letting *project* config point at a remote
  server (queries leak identifiers; only user-level config may).
  <https://github.com/clangd/clangd/issues/1329>
- Predecessors (cquery/ccls/rtags): all treated the index as a private
  local cache; none solved sharing. ccls' best effort was manually copying
  a cache and fixing paths with `clang.pathMappings`.
  <https://github.com/MaskRay/ccls/issues/151>

### Sourcegraph LSIF→SCIP — CI-indexed main + conservative drift mapping

- CI indexes main, uploads; unindexed commits answer from the nearest
  indexed ancestor/descendant within 100 commits. Query-time commit-graph
  traversal was **walked back twice** (recursive CTE → precomputed
  visibility; the precomputation then OOM-crashed frontends on monorepos
  until bounded). <https://sourcegraph.com/blog/optimizing-a-code-intelligence-commit-graph-part-2>
- Position drift: one `git diff -U0` per commit pair, hunk-shift by
  cumulative delta, and **hard conservatism** — any position inside an
  edited hunk is dropped (falls to search-based), renames skipped
  entirely, columns never adjusted.
  `internal/codeintel/codenav/gittree_translator.go` in
  <https://github.com/sourcegraph/sourcegraph-public-snapshot>
- **LSIF abandoned wholesale for SCIP**: graph encoding with opaque IDs
  made indexes bloated and "incremental updates nearly impossible"; SCIP =
  per-file protobuf documents keyed by stable global symbol strings.
  Endorses: per-file documents + name-keyed cross-file resolution, not a
  monolithic stored graph. <https://sourcegraph.com/blog/announcing-scip>
- scip-clang (Chromium-scale C++ indexer, works: Chromium index 375 MB raw
  / 53 MB compressed): "incremental indexing" **closed not-planned, Jan
  2026** — killed by a whole-project merge step that must always rerun.
  <https://github.com/sourcegraph/scip-clang/issues/183>. We have no such
  step (per-file blobs + per-file stub rows), so CI can re-index main
  incrementally with the same overlay logic dev boxes use.

### rust-analyzer / rustc — the on-record counterarguments

- rust-analyzer **rejected disk persistence (2020)** on the argument that a
  cache lets the cold path rot ("forces the codebase to keep the
  non-incremental path reasonably fast"); still unshipped mid-2026 — the
  new-Salsa persistence prototype had to flatten unserializable query
  dependencies. Persisting a demand-driven query graph is much harder than
  persisting per-file documents; our architecture is Glean-shaped, not
  Salsa-shaped. <https://github.com/rust-lang/rust-analyzer/issues/4712>,
  <https://github.com/salsa-rs/salsa/pull/967>
- rustc incremental + shared caches: effectively dead. Rust 1.52.1 shipped
  because fingerprint verification exposed **pre-existing silent
  miscompilations** in incremental caches; sccache refuses to cache
  incremental artifacts (path- and session-coupled).
  <https://blog.rust-lang.org/2021/05/10/Rust-1.52.1/>,
  <https://github.com/mozilla/sccache/issues/236>
- Cargo's successor design restricts shared caching to immutable AND
  idempotent packages "until sandboxing can prove idempotence," and its
  trust model is CI-writes/dev-reads.
  <https://rust-lang.github.io/rust-project-goals/2024h2/user-wide-cache.html>

### Build-system remote caches — the poisoning and economics file

- Bazel: nondeterminism/undeclared inputs poison the shared cache and the
  only remedy is **wiping** ("no way to distinguish which output belongs
  to a specific build"); ACL guidance is one sentence — only CI writes.
  <https://github.com/bazelbuild/bazel/blob/master/site/en/remote/caching.md>
- Caches are an active attack channel: the Angular pipeline compromise ran
  through GitHub Actions cache poisoning; "Cacheract" is malware living
  inside cache entries.
  <https://adnanthekhan.com/2024/12/21/cacheract-the-monster-in-your-build-cache/>
- "Remote slower than rebuild" is universal enough that every ecosystem
  grew a countermeasure: Bazel builds-without-bytes + **dynamic execution
  races local vs remote**; Gradle states it precisely — fetch-vs-rebuild
  is a per-artifact decision (download+unpack estimate vs execution
  estimate), never a global switch.
  <https://bazel.build/remote/dynamic>,
  <https://github.com/gradle/gradle/issues/12319>
- Trust: Nix's model — content-addressed artifacts are self-verifying; the
  *mapping* (narinfo) carries a detached signature verified against pinned
  keys; untrusted builders never hold signing keys.
  <https://wiki.nixos.org/wiki/Binary_Cache>

### GitHub stack-graphs — the adjacent graveyard entry

Per-file position-independent graphs with zero cross-file work at index
time — architecturally the closest bet to ours — **archived Sep 2025**;
precise nav unshipped in favor of search-based. The per-file incremental
part worked; what killed it was per-language rule-authoring cost and
unbounded query-time path-finding. We differ on exactly those axes
(languages we own; cross-file resolution over typed per-file facts, not
stored-graph path search). See `docs/evals/stack-graphs.md` for our own
adoption eval. <https://github.com/github/stack-graphs>

## Failure modes → design rules

Ordered by severity. Each rule is the contract an implementation must meet.

1. **Silently-wrong answers from an under-specified key.** Analysis output
   depends on more than file bytes: the probed toolchain
   (`cpp_toolchain.rs`: `compiler_version`, `predefined_macros`,
   `include_dirs` — per-machine by construction), `@INC` (`inc_hash`),
   plugin fingerprint, `EXTRACT_VERSION`, the include closure — and two
   subtle axes: disk bytes ≠ git bytes (autocrlf/filters; digest disk
   bytes, use git hashes only as a prefetch hint) and undeclared inputs
   (Bazel's core lesson; the `deps_stamp` truncated-closure blind spot,
   `cpp_reparse.rs::include_closure`, is a small local instance).
   **Rule:** the shared key is a digest over the builder's *observed read
   set* (own bytes + closure members' content digests + toolchain identity
   + `EXTRACT_VERSION` + plugin fingerprint + `inc_hash`); the local
   stat-based stamp survives only as a fast-path validator. Anything
   degraded/incomplete refuses publication — the existing
   save-refuses-degraded-rows gate promoted to a hard invariant.

2. **Poisoned key→blob mapping.** Content-addressed blobs self-verify; the
   key→digest mapping is the poisonable part (Bazel's action-cache
   analogue), and post-poisoning forensics don't exist — prevention only.
   **Rule:** CI-only writers; signed manifest (Nix narinfo shape); a CI
   verify lane that builds the same key on two independent runners and
   diffs blob identity (the `--refs-parity` A/B discipline applied to
   determinism), so nondeterminism residue alarms instead of forking the
   cache. Dev boxes never upload by default — branch blobs leaving the
   machine is code exfiltration, not caching.

3. **Derived facts spanning base+overlay** — Glean's admitted hard part
   (ownership propagation; cross-stack derivation unfinished).
   **Rule:** the shared tier carries only closure-pure per-file build
   artifacts. Every cross-file-derived fact (enrichment, the R4 overlay,
   `MethodOnClass` walks) is computed locally at query time and never
   published. We already do this; the rule makes it load-bearing.

4. **Stale-span answers.** Mostly designed away: files differing from the
   snapshot rebuild locally, and cross-file resolution is name-keyed at
   query time, so unchanged files' refs into changed files resolve against
   fresh local analysis. The residual is any *served aggregate* tier
   (refs shard / heatmap): **rule:** adopt Sourcegraph's conservatism
   verbatim — hunk-shift lines, drop anything inside an edited hunk, skip
   renames — or recompute locally for touched files. Google's principle
   throughout: no answer beats a stale-span answer.

5. **Remote dependence.** clangd's real mistake: external index silently
   disables the local lane, so a dead server shrinks coverage with no
   fallback. **Rule:** the remote is only ever another *populator of the
   local content-addressed store* — correctness never depends on it. On a
   miss, race fetch vs local rebuild (per-blob economics; with small zstd
   blobs vs a cheap single-file parse the race is genuinely close —
   measure, don't assume). Network down degrades to exactly today's
   behavior, riding the existing degraded-window + heal machinery
   (`backend.rs::degraded_open`). The same store means branch switches and
   past local builds hit with no server at all.

6. **Version skew.** JetBrains requires identical IDE builds; every
   upgrade invalidates the corpus. **Rule:** `EXTRACT_VERSION` (+ blob
   format) namespaces the key so skewed clients *miss* (degrade local)
   rather than decode garbage; keep the lying-stamp shape-probe
   discipline. Treat remote blobs as untrusted bincode input: length-cap,
   version-gate, decode failure = cache miss, never an error.

7. **Fetch economics.** JetBrains' break-even ("if your network is fast"),
   Gradle's per-artifact rule, Bazel's races. **Rule:** stubs (small,
   registration-critical) fetch eagerly; full blobs lazily on miss;
   per-blob size/latency thresholds; the local mirror is byte-capped LRU
   like every other derived-copy cache here (`GatherCache` shape:
   single-flight + byte-accounted eviction).

8. **Cold-path rot** — rust-analyzer's on-record reason to reject
   persistence. **Rule:** the from-scratch path stays benchmark-gated
   (`edit-bench` cold-start rounds) so the shared cache can never mask a
   cold regression.

## The recommended shape

1. **Identity first (useful standalone):** machine-portable blobs
   (root-relative paths) + content-digest identity + closure content
   digest replacing the stat fold as the cross-machine key. Also fixes the
   stat-stamp fragility class (git-checkout mtime churn, the deps_stamp
   blind spot).
2. **Published snapshot, no server logic:** CI indexes main per commit (or
   Nth), uploads blobs + stub rows + a signed manifest to dumb storage.
   Client: attach per-file by content hash (JetBrains' decision — the
   merge-base manifest is just a prefetch hint), stream-register stubs as
   the warm lane does today, locally rebuild `git diff` files plus their
   Surface-dirty consumers (`FreshnessIndex` already computes the
   closure).
3. **Lazy CAS fetch:** on-demand blob fetch into a byte-capped
   single-flight mirror; race fetch vs rebuild per blob.
4. **(Measured need only)** a served aggregate tier for whole-project
   queries (references/heatmap) over the relational shred, with rule-4
   drift conservatism — or equivalently, ship the per-commit shredded
   SQLite and overlay dirty rows locally.

The hot-header edit remains the honest worst case (branch edits a core
header → thousands of TUs' closures dirty; no snapshot helps because the
inputs genuinely changed). Mitigations in order: the Surface equality gate
(comment/body-local header edits re-analyze one file), lazy + background
closure rebuild, and only if measurement demands it, remote execution —
a real distributed system, deliberately last.
