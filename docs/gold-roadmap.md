# Gold roadmap — from spikes to shipped

Where the session's PoCs graduate into real builds ("gold"). Two
strategic worlds share one engine:

- **Perl-LSP product** — incrementally shippable today.
- **C/C++ static-analysis engine** — the Mobileye play
  (`~/personal/resume/research-static-analysis.md`); gated on one
  keystone, after which the spikes cascade in.

The **heatmap** straddles both — real on either world now.

Status legend: ✅ landed+verified · 🚧 PoC (branch `spike/cpp-support`,
not wired) · 📋 designed, unbuilt.

---

## Tier 0 — ship-close (LANDED, verified)

| project | branch | state | follow-up to user-facing |
|---|---|---|---|
| **Heatmap** — per-symbol fan-in/out + dead-code-by-reachability over the existing reference graph | `feat/heatmap` ✅ | ran live (29 files, 243 syms); sound over-approximation (dynamic-dispatch/exported/ctor shielded); honest "unreferenced symbol, NOT MISRA 2.2" label; reuses `refs_to` | `--format sarif`; transitive fan-out (`GraphView`); HTML butterfly viewer |
| **PPP** — plugin-declared symbol generators (`make_crud_helpers('user')` → real `user_id`/`get_user`/`set_user` symbols, provenance to the call site) | `feat/perl-generators` ✅ | tests green; declarative `generators()` manifest (worklist for free, rule #10); `Namespace::Framework` symbols, `span = call site` | **trigger/module gate**; a real bundled CPAN generator; e2e rename/goto verification; richer args |

Both merge to main low-risk. PPP is inert until a generator is declared,
so the foundation lands safely; the gate is the bar before shipping a
real generator plugin.

---

## Tier 1 — THE KEYSTONE (first slice LANDED)

**`LanguageDriver` foundation** 🚧→✅ first slice on `spike/cpp-support`
(`src/language_driver.rs`).

LANDED (additive, Perl path untouched, default suite 1062/0):
- `LanguageDriver` trait + `LanguageRegistry`; `PerlDriver` (wraps the
  builder) + generic `PackDriver` (grammar + `LangPack` + optional
  pre-parse transform).
- **Feature-gated distribution**: `cpp`/`python`/`r`/`cmake` features
  (optional grammar deps); default build links no pack grammar. A
  `cpp-lsp` = `cargo build --features cpp`; `--features all-langs` serves
  all five. `--languages` / `--lang-analyze <file>` prove it at the CLI
  (a macro-heavy `.cpp` routes through the reparse seam and outlines).

STILL DEFERRED (the risky part — needs e2e, do with review, NOT blind):
- **Async LSP backend routing.** `document.rs` / `backend.rs`
  `did_open`/`did_change` still hardwire Perl; routing them through the
  registry (by `language_id` / extension) + watcher globs is what makes
  `cpp-lsp` work *in an editor* (not just the CLI). Per
  `docs/prompt-multi-language.md` §"Touch points".
- **`FileAnalysis.language` tag + the two cross-file filters** (refs_to /
  workspace-symbol) so a Python `helper` doesn't match a Perl `helper`.
- **Per-language `ModuleIndex`** (keyspace isolation).

Why keystone: **every C/C++ spike below is a standalone module that the
driver now hosts.** The CLI slice proves the seam; the backend routing +
language-tag is what turns the spikes into editor features.

Spikes waiting on the keystone (all 🚧 on `spike/cpp-support`, all green):

| spike | what it gives | research tie |
|---|---|---|
| reparse seam — Perl prototypes, C++ macro expansion (+ validate gate) | parses past the preprocessor; declarator-position macros recovered | Class A |
| B1 lexer hack (`a*b;`) via symbol-table reparse, cross-file | typedef-vs-multiply, the C context-sensitivity, resolved | Class B1 |
| A2 `#ifdef` config-selection (blank-in-place) + superposition | construct-splitting `#if` recovered; both arms as a presence-tagged union | Class A2 |
| multidispatch + ranking lattice (rung-1) | overload resolution = dispatch + rank | moat build #1 |
| template witnesses + projection | one body per instantiation, dependent types dissolve | moat build #2 |
| **template→lattice join** | same body, different overload per witness → correct per-witness call-graph edge | §1a call-graph kicker |

---

## Tier 2 — the rigor track (parallel to Tier 1)

**Deepen the ranking lattice** 🚧→📋 — the "honest risk." Rung-1
(exact/convertible) exists; the real ladder is `exact ≻ promotion ≻
standard-conversion ≻ user-defined ≻ ellipsis`, tuple-combined, with
cv-qualification + reference-binding + template partial-ordering. This is
where C++ wrongness hides and where a compiler engineer will poke. Grind,
not discovery — but it's the gap between "resolves the common case" and
"passes a MISRA-grade suite." Needed before any room-claim; shared by
overload resolution, SFINAE selection, and template partial-ordering.

The widen-trio (smaller, "yours + widen"): SFINAE = duck predicate; ADL =
widen the lookup set; ODR = cross-TU definition compare (surface-only,
IFNDR → added value).

---

## Tier 3 — opportunistic engine wins

- **`ReturnExpr::Arg` + `ParametricOp::BinOp`** into real `witnesses.rs`
  (Perl overload typing; the overload-Π PoC is the spec). Medium effort.
- **Metaprogram-witness tier doc** — lock the framing: templates + PPP =
  two projection backends (C++ substitute-types-then-resolve; Perl
  substitute-literals-then-plugin-synthesize) over one
  witness/substitute/worklist/seen-set spine; chain-to-root provenance;
  execute-probe (`importbase-plugin-gen`) as the opaque-only fallback.

---

## Sequencing

1. Merge Tier 0 (heatmap now; PPP foundation + gate-as-follow-up).
2. **Tier 1 keystone** (`LanguageDriver`) — unlocks the C/C++ cascade.
3. Lift the reparse/B1/A2/dispatch/template/join spikes into a real C++
   driver, on top of the keystone.
4. Tier 2 lattice rigor runs alongside (needed before a Mobileye claim).
5. Tier 3 as opportunity allows.

The C-vs-C++ ratio of the actual target (still open in the research)
decides whether to lead with the C story (near-full semantic correctness,
no Clang) or the C++ story (one well-scoped lattice + witness projection
away).
