# docs/evals

Investigations of "should we adopt X?" — a framework, a dependency, an
architecture swap — that may include **throwaway spike code** and benchmarks,
and whose outcome is a recommendation (often "change nothing").

## evals vs adr

- **`docs/adr/`** — Architecture **Decision** Records. Binding decisions that
  describe what the code *is* and why. Load-bearing; consumers rely on them.
- **`docs/evals/`** — the *investigation* behind a decision. Keeps the evidence
  (spike, measurements, coverage census) next to its writeup so the call can be
  re-litigated later without redoing the legwork. If an eval concludes "adopt,"
  the resulting design graduates to an ADR; the eval stays as the paper trail.

## conventions

- One eval per topic: `docs/evals/<topic>.md`, with any spike code beside it
  (`<topic>-spike.rs`, etc.).
- Spike code here is **not built by the crate** — each file's header says how to
  run it (typically: drop into a fresh crate / `examples/` and `cargo run`).
  In-tree runnable experiments belong in `examples/` or `benches/` instead;
  these live here only because they need a separate dependency tree.
- State the verdict and a concrete **revisit trigger** up top, so a stale eval
  is obvious.

## index

- [`stack-graphs.md`](stack-graphs.md) — stack graphs / scope graphs for name
  resolution. Verdict: **do not adopt** (spike: `stack-graphs-spike.rs`).
