# rename

Generated from `fixtures/rename.json` (the source of truth) against the cpm-installed, snapshot-pinned substrate (`gold-corpus/local/lib/perl5`).
Positions are 0-based on input, 1-based on output. Run via `gold-corpus/run.pl rename`.

| id | difficulty | semantic_area | cursor | expect.all / expect.none | status | actual |
|----|------------|---------------|--------|--------------------------|--------|--------|
| rename-01-mkopt-def | simple | exporter | `Exporter/Tiny.pm:429:4` | all: Tiny.pm; "line":"8"; "line":"429"; "line":"456"; "line":"69"; "line":"104"; "line":"167" | xfail | Tiny.pm: line 8 (@EXPORT_OK), 429 (def), 456 (recursive call); Type/Registry.pm: 13,74. MISSES 69,104,167. |
| rename-02-mkopt-callsite | tricky | exporter | `Exporter/Tiny.pm:69:12` | all: Tiny.pm; "line":"69"; "line":"104"; "line":"167"; "line":"429"; "line":"8"; "line":"456" | xfail | Tiny.pm: line 69,104,167 only. MISSES def 429, @EXPORT_OK 8, recursive 456. |
| rename-03-mkopt-hash | simple | exporter | `Exporter/Tiny.pm:453:4` | all: Tiny.pm; "line":"8"; "line":"453" / none: "line":"429" | gold | Tiny.pm: line 8 (@EXPORT_OK, col 27-37), 453 (def). mkopt (429) untouched. |
| rename-04-croak | tricky | exporter | `Exporter/Tiny.pm:19:4` | all: Tiny.pm; Shiny.pm; "line":"19"; "line":"8"; "line":"360"; "line":"361"; "line":"362"; "line":"24"; "line":"31" / none: "line":"340" | gold | Tiny.pm: 8,19,145,164,259,298,327,360,361,362,370,402,408; Shiny.pm: 24,31. Comment line 340 skipped. |
| rename-05-setup-exporter | tricky | exporter | `Sub/Exporter.pm:590:4` | all: Exporter.pm; "line":"590"; "line":"931"; "line":"933"; "line":"937"; Setup.pm | gold | Exporter.pm: 590 (def), 931 (call), 933 (qw bareword), 937 (group-list bareword). Cross-file FQ: Setup.pm:218, Moose/Util.pm:34, Test/Moose.pm:19. |
| rename-06-build-exporter | tricky | exporter | `Sub/Exporter.pm:703:4` | all: Exporter.pm; "line":"703"; "line":"933"; "line":"934"; "line":"602" | xfail | Exporter.pm: 703 (def), 933 (export bareword), 934 (call); cross-file FQ. MISSES in-body call at 602. |
| rename-08-authority | tricky | oo-isa | `URI/_generic.pm:37:4` | all: _generic.pm; _server.pm; file.pm; ldapi.pm; "line":"37" | provisional | _generic.pm:37 (def); _server.pm:54,65,74,91,114,120,137; file.pm:38; ldapi.pm:13,18. MISSES intra-file dynamically-typed locals. |
| rename-09-base-constant | simple | constants | `URI/_punycode.pm:14:13` | all: _punycode.pm; "line":"14"; "line":"79" | xfail | _punycode.pm: 14 (def), 47,48,49,51,71(x2),118(x2),122,124. MISSES line 79: $w *= (BASE - $t). |
| rename-11-urn-canonical-precision | tricky | oo-isa | `URI/urn.pm:90:4` | all: urn.pm; "line":"90"; isbn.pm; "line":"97" / none: _generic.pm; "line":"94" | gold | Renames the URI::urn::canonical def (urn.pm:90) AND the subclass SUPER call targeting it (isbn.pm:97, `$self->SUPER::canonical`). NOT isbn's own override (isbn.pm:94) nor _generic.pm's `$rel->canonical` (different dispatch class). |

## Dropped (non-lib, absent from installed tree)

- rename-07-path-segments: REJECT in source md (expected was incorrect; tool correctly renames only def + genuinely-inheriting smb caller). Excluded from corpus per md rejection note.
- rename-10-moo-has-accessor: cursor lived in t/accessor-coerce.t (Moo dist test file, not under lib/) -> absent from the cpm-installed tree (local/lib/perl5 has only Moo.pm and lib modules, no t/). No installed location for the Foo 'has plus_three' decl; cannot reproduce.
