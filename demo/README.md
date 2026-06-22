# perl-lsp screencast

A short, scripted demo of perl-lsp's editor features, rendered headlessly.

## Artifacts

- `perl-lsp.mp4` / `perl-lsp.webm` / `perl-lsp.gif` — the demo (~23s)
- `perl-lsp.cast` — the raw [asciinema](https://asciinema.org) recording

## How it's made

We record the terminal **byte stream** (not a screenshot of a rendered screen),
so the LSP popup menu — which is just escape sequences in that stream — is
captured deterministically. Screenshot-based recorders (vhs, kitty + x11grab)
lost a GPU/capture race against the popup's brief redraw; asciinema doesn't.

```
asciinema_drive.sh   nvim in a sized tmux pane, driven by `tmux send-keys`,
                     recorded with `asciinema rec`
demo_init.lua        nvim config: the real perl-lsp setup (test_nvim_init.lua)
                     + captions + determinism helpers
agg                  renders the .cast -> gif; ffmpeg -> mp4/webm
```

## Regenerate

Needs `tmux`, `asciinema`, `agg`, `ffmpeg` on PATH, and a release build
(`cargo build --release`).

```sh
demo/asciinema_drive.sh /tmp/demo.cast
agg --idle-time-limit 2 --font-size 26 --theme asciinema /tmp/demo.cast demo/perl-lsp.gif
ffmpeg -i demo/perl-lsp.gif -pix_fmt yuv420p demo/perl-lsp.mp4
ffmpeg -i demo/perl-lsp.gif -c:v libvpx-vp9 -b:v 0 -crf 34 demo/perl-lsp.webm
```

## The scene

`app.pl` uses `Account` (`lib/Account.pm`, a Moo class). The demo shows hover
(inferred cross-file type), completion (the real method list), and a rename of a
`has` accessor in `Account.pm` cascading back into `app.pl`.

## Determinism notes (why the config looks the way it does)

Scripted PTY input races the async LSP, so the recording fakes nothing but pins
timing-sensitive bits:

- **Completion menu** via `complete()` with the literal (verified) method list,
  invoked through `inoremap <C-l> <C-r>=…<CR>` — the textlock-safe form.
- **Caption** in the global tabline (a window-local winbar desyncs across `:b`).
- **Rename** calls `vim.lsp.buf.rename(newname)` directly (no `ui.input` prompt
  to mis-drive).
- The driver clears **both** files' swap files each run (a stale swap pops a
  prompt that turns subsequent keystrokes into buffer garbage).
