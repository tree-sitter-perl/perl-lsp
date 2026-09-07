# PHP as a language target — forward work

`--features php` landed (`CLAUDE.md`, `docs/adr/php-diagnostics.md`,
`docs/adr/resolution-candidate-set.md`); the market case and build-out
narrative are in git history.

What's still open:

1. **FQ identity residual.** Inheritance edges, `refs_to`, and rename
   stay leaf-keyed (over-approximate, never wrong-file for gd since
   goto-def ranks); full FQ symbol identity waits for a real need —
   `docs/open-forks.md`, "GraphView node identity is leaf-keyed".
2. **Stdlib tier.** phpstorm-stubs (Apache-2.0) as the builtin surface —
   consumable the way `builtins.pod` feeds the Perl BUILTIN tier. Not
   started.
3. **Docblock residuals.** PHPStan array-shapes → `HashWithKeys`,
   `@template` beyond the class level (`docs/PARKED.md`, "PHP
   method-level `@template`"); hover already renders the doc PROSE.
4. **Framework-plugin tier 2** waits for a tenant needing name surgery
   (Laravel scopes) — tracked in `docs/prompt-pack-plugins.md`.
5. **Calibration.** The gold-corpus sibling: a packagist-pinned
   substrate (top-N packages via composer), the same exact-assertion
   fixture format, corpus entries for a Laravel app + WordPress core in
   the `bench/` stack. Ship gate, budgeted as half the work. Not
   started — no PHP fixtures exist in `gold-corpus/` yet.

Known residuals: `require`/`include` path imports; a class const's VALUE
stays untyped (typing it as the class would be wrong — thread the value
span to fix; a true enum case's value already types as its enum through
the same hop lane); heredoc/encapsed interpolation refs exist but
interpolated member completion doesn't; `list()`/array destructuring;
global functions are namespace-blind. Array-element flow through
`foreach` is the engine's declared sequence-types residual —
`docs/prompt-sequence-types.md` — and when it lands, `phpdoc_type`
should map `X[]`/`list<X>` to the parametric array-of-X instead of bare
`array`.
