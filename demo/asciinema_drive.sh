#!/bin/bash
# Record the perl-lsp demo by driving nvim inside a sized tmux pane and capturing
# the TERMINAL STREAM with asciinema (the pum is in the byte stream, so it's
# captured deterministically — no screenshot/GL race like vhs/kitty). agg then
# renders the .cast to gif headlessly.
#
#   demo/asciinema_drive.sh [out.cast]
set -u
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"
CAST="${1:-/tmp/perl-lsp-demo.cast}"
export PATH="$HOME/miniconda3/envs/demotools/bin:/usr/bin:$PATH"

# Clean swaps for BOTH demo files — a stale Account.pm swap (left when a prior
# take's nvim was killed mid-rename) pops a prompt on `:e`, and the keystrokes
# after it cascade into the buffer as garbage.
rm -f "$HOME/.local/state/nvim/swap/"*app.pl.swp "$HOME/.local/state/nvim/swap/"*Account.pm.swp 2>/dev/null
./target/release/perl-lsp --check "$REPO/demo" --severity warning >/dev/null 2>&1 || true

tmux kill-session -t rec 2>/dev/null; sleep 1
tmux new-session -d -s rec -x 150 -y 40
# asciinema records nvim INSIDE the 150x40 pane -> cast is 150x40
tmux send-keys -t rec "cd '$REPO' && asciinema rec --overwrite -c 'nvim -u demo/demo_init.lua demo/app.pl' '$CAST'" Enter
sleep 12  # asciinema + nvim + LSP attach + cross-file workspace resolution
          # (goto-def into Account.pm needs the module index, which lags attach).
          # The idle wait is compressed away by agg's --idle-time-limit.

S(){ tmux send-keys -t rec "$@"; }   # special keys (Escape/Enter/C-n…)
L(){ tmux send-keys -t rec -l "$@"; } # literal text

# Beat 1 — hover (inferred type, cross-file)
L ';c'; sleep 0.3
L '6Gw'; sleep 0.5
L 'K'; sleep 2.5
S Escape; sleep 0.8

# Beat 2 — completion on $acct-> then accept deposit.
# Filter to `dep` (uniquely matches deposit; describe is `des…`) so the accepted
# item is DETERMINISTIC — `de` is ambiguous and the async menu state varies per take.
L ';c'; sleep 0.3
L 'Go'; sleep 0.5
L '$acct->'; sleep 0.8
S C-l; sleep 2.0          # sync LSP fetch -> full method menu via complete()
L 'de'; sleep 1.5         # narrows the open menu natively to deposit/describe
S C-n; sleep 1.0          # select deposit (first)
S C-y; sleep 1.2          # accept
S Escape; sleep 0.8

# Beat 3 — open Account.pm and rename the `has balance` accessor (cascades
# cross-file). Opening explicitly also shows the dependency file on screen.
L ';c'; sleep 0.3
L ':e demo/lib/Account.pm'; S Enter; sleep 1.8   # show the dependency file
L 'gg'; sleep 0.3
L '/has balance'; S Enter; sleep 0.6
L 'w'; sleep 0.5                     # cursor onto `balance`
L ';r'; sleep 2.5                    # direct rename -> available, cascades

# Beat 5 — flip back to app.pl to prove the cross-file rename
L ';c'; sleep 0.3
L ':b app.pl'; S Enter; sleep 0.7
L '/available'; S Enter; sleep 3.0

# end: quit nvim -> asciinema finalizes the cast
L ':qa!'; S Enter
sleep 3
tmux kill-session -t rec 2>/dev/null
echo "cast written:"; ls -la "$CAST"
