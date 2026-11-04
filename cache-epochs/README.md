# Cache epochs

Each directory is one cache gate. `build.rs` hashes the FILE NAMES in it
(never the contents) into the gate's version, so a change that makes a
cached artifact stale adds one empty file named for itself:

    cache-epochs/extract/2026-11-04-anon-sub-tail-arms

Two PRs that each add a file never conflict. Merge order does not matter:
every reader compares the version for equality.

| Directory         | Invalidates                                   | On mismatch                              |
|-------------------|-----------------------------------------------|------------------------------------------|
| `extract`         | analysis blobs (`modules.extract_version`)    | rows re-analyze; @INC rows load stale and re-resolve |
| `stub`            | warm-scan stubs                               | `stubs` wiped, backfilled from blobs     |
| `ref-rows`        | relational ref/sym row format                 | row tables rebuilt, re-shredded          |
| `conclusion-rows` | `conclusions` row shape                       | table rebuilt, re-baked                  |
| `schema`          | `modules` table DDL                           | `modules` dropped                        |
| `macro-cache`     | persisted C/C++ macro tables                  | tables re-gathered                       |

Add a file when your change alters what a cache STORES (its shape or its
meaning). A pure refactor, a query-time change, or a perf change that leaves
the stored bytes identical adds nothing.

## Cleanup: roll up at release, never plain-delete

Deleting files can restore a name set an older build already shipped — drop
the newest file and the hash is the previous build's again, so caches that
build wrote are trusted by code that no longer matches them. The release PR
instead replaces every file in every directory with ONE new file named for
the release (`release-0.9.1`). That name has never existed, so the hash is
new; and a release invalidates every cache anyway, so the roll-up costs
nothing extra. Only the release PR does this, so nothing races it.
