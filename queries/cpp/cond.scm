; Conditional-compilation arms. Every arm of a conditional is extracted;
; these captures say which arm each declaration sits in, so a query can
; rank it against the asker's config (`build/query_extract/cond.rs`).
; Read by the skeleton document and by the macro gather's `#define` walk:
; this file is the one home for the arm shapes.

; An arm that tests, and what it tests. `#ifndef X` is `#ifdef X` negated.
(preproc_if condition: (_) @cond.test) @cond.if
(preproc_elif condition: (_) @cond.test) @cond.if
(preproc_ifdef "#ifdef" name: (identifier) @cond.test.defined) @cond.if
(preproc_ifdef "#ifndef" name: (identifier) @cond.test.undefined) @cond.if
(preproc_elifdef "#elifdef" name: (identifier) @cond.test.defined) @cond.if
(preproc_elifdef "#elifndef" name: (identifier) @cond.test.undefined) @cond.if

; The arm that runs where the one it hangs off fails: `#elif…` / `#else`.
(preproc_if alternative: (_) @cond.else)
(preproc_ifdef alternative: (_) @cond.else)
(preproc_elif alternative: (_) @cond.else)
(preproc_elifdef alternative: (_) @cond.else)

; An include guard: `#ifndef X` that `#define`s X itself. Its test holds for
; the rest of the file, so it guards nothing inside it.
(preproc_ifdef
  "#ifndef"
  name: (identifier) @cond.guard
  (preproc_def name: (identifier) @_guard_def)
  (#eq? @cond.guard @_guard_def))
