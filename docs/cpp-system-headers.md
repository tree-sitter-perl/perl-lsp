# C/C++ system-header resolution — challenges (parked)

`iswspace` (and any libc/STL symbol) gets no goto-def/hover because we index
only *project* files: the workspace walk skips non-project trees and the
macro gather skips `<...>` includes. Closing that is **not** "index system
headers like Perl indexes @INC" — the tradeoffs are different on every axis.

## Why the Perl model doesn't transfer

| | Perl `@INC` | C/C++ system headers |
|---|---|---|
| **What** | `.pm` source, *same parser*, semantically identical to project code | toolchain-owned headers, heavy macros + templates our skeleton tier reads poorly |
| **Where** | filesystem paths (`@INC`, cpanfile) | the **compiler** owns the search paths, not the filesystem |
| **How much** | bounded (deps + @INC), all cheap | tens of thousands of decls; STL is template metaprogramming |
| **Accuracy** | exact (it's just more Perl) | C stdlib tractable; C++ STL templates largely opaque to a declaration-oriented extractor |

## The hard parts

1. **Discovering the include paths.** Not `/usr/include` by assumption — the
   real set is per-toolchain, per-platform, per-cross-compile-target:
   - `compile_commands.json` (the `-I`/`-isystem` flags per TU) — the
     authoritative source when present (CMake/Bear/clangd already emit it).
     Should be the **primary** input.
   - Fallback: probe the compiler — `cc -E -v -` / `clang -print-search-dirs`
     / `gcc -print-search-dirs` — and parse the system include list. Platform-
     and compiler-specific parsing.
   - No build context → we're guessing, and a wrong guess indexes the wrong
     libc (multilib, sysroots, cross toolchains).

2. **Scope: C stdlib vs C++ STL.** The **C** standard library (`iswspace`,
   `printf`, `memcpy`, the `<*type.h>` families) is plain functions + typedefs
   — exactly what the skeleton tier handles well. The **C++ STL** is template
   metaprogramming (`std::vector<T>`, allocator rebinds, SFINAE) that a
   declaration-oriented extractor can't meaningfully type. Realistic target:
   nail C stdlib, degrade gracefully on STL templates (don't choke, don't
   pretend).

3. **Eager vs lazy.** Indexing the full system include closure at startup is
   expensive and mostly wasted (a TU touches a sliver of it). **Lazy** is the
   right shape: when a symbol misses in the project, search the system roots
   on demand — find the declaring header, parse just that one. The macro
   gather already does this style of bounded header-walk; this extends the
   search roots, it isn't a new engine.

4. **Caching + invalidation.** System headers change with the toolchain, not
   the project. A system-header cache keys on the toolchain identity
   (compiler version + sysroot + flags), separate from the per-project module
   cache, or a libc upgrade serves stale decls.

5. **Volume control.** Even lazy, a single `<iostream>` pulls a huge closure.
   Need the same parse-damage/size guards the project gather uses, plus a
   "system header" role so these never pollute project diagnostics or
   workspace-symbol noise.

## Recommended shape (when we pick it up)

Toolchain-discovered roots (`compile_commands.json` first, compiler-probe
fallback) + **lazy** per-symbol resolution into those roots, scoped to a new
DEPENDENCY-like role. Gets `iswspace → wctype.h` for one lazy lookup, makes
the C stdlib solid, and treats STL templates as best-effort rather than a
correctness goal.

Parked: revisit after the pointer-stack capture work.
