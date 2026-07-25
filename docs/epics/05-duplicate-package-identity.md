# Epic 5 — Duplicate-package identity (QA item H1)

> **Status:** scheduled (5th). Small and self-contained; scheduled
> early because H1 blocks re-testing QA item B4 (a shadowed package
> exporting the wrong `@EXPORT` surface may be H1 in disguise).
> **Design owner-doc:** `docs/qa-design-items.md` §H1 — including its
> options analysis and the recommendation this epic implements.

## Mission

Two files both declare `package Bugzilla;` (`contrib/Bugzilla.pm`
shadowing the root `Bugzilla.pm`); today whichever the resolver happens
to pick wins, breaking type inference and exports for the real one.
Implement the owner-doc's recommendation: a **typed canonicality
ranking computed once at index time and carried on the indexed entry**,
so the resolver asks the entry "are you canonical?" and the entry
answers — never a path-string branch at the resolution site (rule #10).

## Read first

1. `docs/qa-design-items.md` §H1 — options and recommendation.
2. `CLAUDE.md` "Workspace indexing" and "Cross-file resolution" (the
   documents → workspace_index → module_index priority; duplicates
   WITHIN a tier are the gap).
3. `src/module_index.rs` — `register_workspace_module` (where a
   module-name collision currently last-write-wins into `cache`) and
   `src/file_store.rs` (`FileRole` — note it is currently only
   Open/Workspace/Dependency/BuiltIn; H1 needs a finer rank, which may
   or may not belong on this enum — see Phase A decision).

## Phase breakdown

### Phase A — the rank, as data

1. Compute, at index/registration time, a canonicality rank per
   (module_name, file) pair:
   1. **Path–package agreement**: does the file's path end with the
      package name's path form (`Bugzilla::Foo` →
      `.../Bugzilla/Foo.pm`)? Longest-suffix match wins; a `lib/`
      prefix strengthens it.
   2. **Directory role**: `lib/` (and the workspace root for root-level
      `.pm`) outranks `t/`, `xt/`, `contrib/`, `examples/`,
      `inc/`, `local/`. Encode as an ordered enum
      (`DirRole::Lib > Root > Other > Contrib > Test`), computed ONCE
      from the path at index time. The resolver never sees a path
      string.
2. Where the rank lives: on the indexed entry (the `CachedModule` or a
   sibling field in the registration map). DECISION to record: extend
   `FileRole` vs a separate `Canonicality` type. Default to a separate
   type — `FileRole` is a visibility/lifecycle tag consumed by
   `RoleMask`, and overloading it risks changing rename/visibility
   semantics. Write the decision and why in the ADR (Phase C).
3. Registration keeps ALL duplicates (a secondary map
   `module_name → Vec<entry>` or rank-compare-and-swap on insert —
   compare-and-swap is simpler and sufficient: keep the best, remember
   only that a conflict existed + the loser paths for diagnostics).

### Phase B — resolution + diagnostics

1. `get_cached`/`register_workspace_module` collision behavior becomes:
   highest rank wins deterministically (ties: stable on path sort, so
   re-index order can't flip winners — determinism is the point).
2. A HINT diagnostic on the SHADOWED file's package statement:
   "package Bugzilla is also declared in <winner>; this file is not
   the canonical provider" — only when ranks differ; equal-rank
   genuine conflicts get the hint on both.
3. `.perl-lsp`-config override (owner doc option 2) is OUT of this
   epic — note it as the escape hatch in the ADR; it lands with the
   config epic (Epic 8) if demand appears.

### Phase C — verification + ADR

1. Unit tests: contrib-shadows-lib picks lib; path-agreement beats
   directory role when they conflict (construct such a case and DECIDE
   the precedence — the owner doc implies agreement first; pin it);
   determinism under reversed registration order.
2. Re-test B4 against the substrate (the Bugzilla fixture is in the
   gold substrate — `grep -rn 'package Bugzilla' gold-corpus/local`
   to find it; if the substrate lacks the contrib shadow, reproduce in
   a test fixture).
3. `docs/adr/duplicate-package-identity.md`: the rank order, where it
   lives, the FileRole decision, and the explicit non-goal (no
   runtime-`@INC` emulation).
4. Full gate: cargo test, gold 0 FAIL / 0 XPASS, substrate audit at
   parity (resolution changes CAN move unresolved-* counts — each
   moved site gets a triage note; expect improvements around the
   Bugzilla cluster).

## Non-goals

- No `@INC`-order emulation, no per-project config this round.
- No path-string checks at resolution sites — if you write
  `path.contains("contrib")` anywhere but the one index-time rank
  function, stop.

## Sizing

Small-to-medium: A+B ~one day-scale session, C the second. One PR.
