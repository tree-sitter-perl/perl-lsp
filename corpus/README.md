# The scaling corpus

Nine large open-source Perl applications, kept because each one measures
something the others do not. Reconstruct on any box with `corpus/bootstrap.sh`.

```
corpus/bootstrap.sh                  # ~/perl-corpora/{bulk,deps}
corpus/bootstrap.sh /data/corpora    # elsewhere
corpus/bootstrap.sh ~/corpora FHEM   # one repo
```

Then measure with dependencies on `@INC`, **not** in the workspace:

```
PERL5LIB=~/perl-corpora/deps/FHEM/lib/perl5 \
  perl-lsp --check ~/perl-corpora/bulk/FHEM --severity hint
```

## Why deps live outside the repo

The workspace walker indexes everything under the root. A `local/` inside the
repo therefore joins the **workspace** tier, and the measurement stops being
about the application. Measured, not assumed: installing FHEM's dependencies
in-repo took it from **929 to 33,912 indexed files**, 97% of them `Paws` — the
AWS SDK ships one `.pm` per API call. The corpus would have been measuring the
AWS SDK.

Installed to `../deps/<name>` and reached via `PERL5LIB`, the same install moves
FHEM's resolved-module count from **125 to 195** while the workspace stays at
929 files. That is the split the server already models — workspace tier vs
`@INC` tier — and it is what an editor session actually sees.

**Installing deps at all is not optional for honest numbers.** An unresolvable
`use` costs the resolver nothing to answer. A corpus whose imports mostly fail
to resolve systematically understates resolution load, which is exactly how a
5,004-distribution pile produced a "cliff" that no real codebase has.

## The nine, and what each is for

| repo | files | measures |
|---|---:|---|
| [FHEM](https://github.com/fhem/fhem-mirror) | 973 | **`main` monoculture.** 503 of 614 `.pm` files explicitly declare `package main` (+31 declaring none) — 534 providers of one name, genuinely sharing one stash because `fhem.pl` do-loads them all. Exposed a superlinear memory regression invisible on every other corpus. |
| [Foswiki](https://github.com/foswiki/distro) | 917 | High fan-out (~1,128 attempts/file) that is **`other`-driven** — a different mechanism from FHEM's. |
| [Evergreen](https://github.com/evergreen-library-system/Evergreen) | 468 | Same high fan-out, but **262/262 files properly packaged** — proves the Foswiki mechanism is not a packaging artifact. |
| [WeBWorK](https://github.com/openwebwork/webwork2) | 213 | **Worst per-file cost** (36 ms/file), topping a different axis than fan-out. |
| [Znuny](https://github.com/Znuny/Znuny) | 3,093 | **Control: largest corpus, lowest fan-out** (8 attempts/file). Size and fan-out are independent axes. |
| [Webmin](https://github.com/webmin/webmin) | 1,383 | **Control: path-based `require`.** 101 package declarations across 1,383 files; lowest hit rate (95.95%). A genuinely different resolution shape. |
| [BMO](https://github.com/mozilla-bteam/bmo) | 739 | **Control: healthy reference.** 206 attempts/file at 99.90% hit. |
| [openfoodfacts](https://github.com/openfoodfacts/openfoodfacts-server) | 537 | **Control: densest static graph, near-zero fan-out.** A dense import graph does not imply expensive resolution. |
| [Conch](https://github.com/joyent/conch) | 226 | **DBIC on a cross-file base.** All 35 result classes `use base 'Conch::DB::Result'`, which reaches `DBIx::Class::Core` in another file, so every column and relationship accessor is a deferred (gated) plugin emission. None of the other eight has more than two files mentioning `DBIx::Class`. |

Deliberately excluded: `perl5` (44% vendored `cpan/` — a distribution pile
wearing a repo's clothes), and MovableType / Freeside (30% / 28% vendored).
Twenty other repos were measured and dropped: they cluster at 8–206
attempts/file and 98–99.9% hit, telling us nothing the four controls do not.

## Dependency manifests

Six of the nine ship **no machine-readable manifest** — they are applications
distributed as packages, not CPAN dists. `derive-cpanfile.pl` reconstructs one
by scanning for `use`/`require` targets, subtracting what the repo defines and
what is core for the running perl.

It is **over-inclusive on purpose**: a spurious requirement costs one failed
install line, while a missing one silently leaves an import unresolvable — and
unresolvable imports are the bias described above. Expect false positives
(`Foo::Bar` from documentation examples, `PGrandom` from a comment).

## System libraries

Most XS builds fine with no system packages. The failures are missing
**libraries**, not compilers, and they are concentrated in graphics, database
drivers and platform-specific modules:

| need | dists that failed without it |
|---|---|
| `libgd` | GD, GDGraph, GD-Graph3d, GDTextUtil, Template-GD, Chart |
| ImageMagick | Image-Magick, Image-OCR-Tesseract |
| `libdb` | DB_File |
| MySQL client | DBD-mysql |
| aspell | Text-Aspell |
| zbar / zxing | Barcode-ZBar, Imager-zxing |
| libheif / libavif / libwebp | Imager-File-HEIF, Imager-File-AVIF, Imager-File-WEBP |
| apache2 dev | mod_perl, libapreq, libapreq2, Apache2-Connection-XForwardedFor |
| libxslt | XML-LibXSLT |

Windows/VMS-only dists (`Win32-*`, `VMS::Feature`) fail to resolve on Linux and
always will; that is correct, not a gap.

**None of these are needed for language-server measurement.** They are runtime
dependencies of the applications, and a missing one leaves a handful of imports
unresolvable out of thousands resolved. Install them only if a specific
measurement turns out to depend on that import resolving.

If you do want them without root, `nix-shell -p gd imagemagick db mariadb aspell`
gives a local, non-root, reproducible set — no system mutation. `micromamba`
covers most of the same ground. Both are heavier than the problem currently
warrants.

## House plugins

Some of what a corpus does at runtime is invisible to any static reading, and
is the corpus's own code, not a CPAN module's: openfoodfacts'
`ProductOpener::Config` loads its export list from `Config_$flavor` chosen by
an environment variable, and FHEM's modules use the globals `fhem.pl` declares
with `use vars` before it `do`-loads them. A team using perl-lsp on such a
codebase writes a plugin for it; `house/<name>/*.rhai` is that plugin, and
`bootstrap.sh` copies it into the repo's `.perl-lsp/` (git-excluded), which
the server loads for that workspace root with no environment. Without them the
corpus reports the house's magic as `missing-declaration` thousands of times
over, measuring a gap a real user has already closed.

House plugins state names copied from the source at the pinned commit, so a
pin bump re-reads them. Magic that comes from a CPAN module (`use Env`,
Log4perl's tags) is not a house plugin; it ships bundled in `frameworks/`.

## Reproducibility caveat

`bootstrap.sh` pins each clone to a commit (fetched at depth 1) but installs
**current** CPAN, so two runs weeks apart can still resolve different
dependencies. Pin the deps to a snapshot before the corpus gates anything.
