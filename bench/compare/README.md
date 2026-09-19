# Cross-tool answer comparison

`lspq.py` is a minimal stdio LSP client that opens a workspace, waits for the
server to answer one goto-definition (readiness), then plays a probe list —
definition / references / hover / completion / rename / implementation at
0-based positions — and writes normalized JSON (locations as
`{file, line, char}`, hover text, completion labels, rename edits, per-verb
latency, RSS). It answers the server→client requests a real editor answers
(`workspace/configuration`, `client/registerCapability`, progress creation)
so a server that awaits them cannot wedge.

`mkspec.py <scratch>` builds probe specs from `(name, file, 1-based line,
token, occurrence, verbs)` rows — the character offset is computed from the
token, so a spec never encodes a hand-counted column. `report.py <scratch>`
prints the side-by-side table with the grep-derived expectations inline.

```
python3 bench/compare/lspq.py --label ours --cmd target/release/perl-lsp \
    --root <repo> --probes spec-<repo>.json --out res-<repo>-ours.json
python3 bench/compare/lspq.py --label intelephense \
    --cmd "node <path>/intelephense.js --stdio" --root <repo> ...
python3 bench/compare/lspq.py --label phpactor \
    --cmd "php phpactor.phar language-server" --root <repo> --settle 10 ...
```

phpactor answers references from its index — run `php phpactor.phar
index:build` in the root first, or its counts are whatever the background
indexer reached. Intelephense's free tier answers rename with zero edits
(a licensed feature), not an error. Findings land in `bench/RESULTS.md`.
