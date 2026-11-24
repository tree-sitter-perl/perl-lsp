# The PHP corpus

Twelve open-source PHP codebases, picked for variety the way the Perl nine
were (`../README.md`). Reconstruct on any box with `corpus/php/bootstrap.sh`.

```
corpus/php/bootstrap.sh                 # ~/php-corpora ($PHP_CORPORA)
corpus/php/bootstrap.sh /data/php       # elsewhere
corpus/php/bootstrap.sh ~/php laravel   # one repo
```

Then measure with the PHP server:

```
perl-lsp --lang php --check ~/php-corpora/laravel
```

## The twelve, and what each is for

Measured 2026-11-08 on the pins below (cold `--check`, n=3, 4 vCPU), before
PHP had a dependency tier, so every import of an absent package widened.

| repo | files | cold wall / RSS | measures |
|---|---:|---|---|
| [monolog](https://github.com/Seldaek/monolog) 3.12.1 | 228 | 0.75 s / 98 MiB | **Baseline.** Small and clean; mostly the fixed per-process cost. |
| [composer](https://github.com/composer/composer) 2.11.1 | 651 | 2.2 s / 306 MiB | **Cross-package imports** against absent symfony/semver, and committed `vendor/` fixture dirs the walker indexes. |
| [laravel](https://github.com/laravel/framework) v13.40.0 | 3,177 | 9.3 s / 1.2 GiB | **Magic and volume.** Facades, `__call`, Eloquent builders; the most widening, and dense fluent-chain tests (`DatabaseQueryBuilderTest.php`, 2.2 s). |
| [wordpress](https://github.com/WordPress/WordPress) 7.1.4 | 1,900 | 9.3 s / 952 MiB | **Procedural globals.** Almost no namespaces; builtin functions against polyfills; the worst single file (`sodium_compat/src/Core32/Curve25519.php`, 3.6 s). |
| [symfony-console](https://github.com/symfony/console) v8.1.11 | 368 | 1.2 s / 173 MiB | **Strict OO library**, small. |
| [symfony-di](https://github.com/symfony/dependency-injection) v8.1.11 | 681 | 1.5 s / 216 MiB | **Compiled-container fixtures** and lazy-init readonly properties. |
| [doctrine-orm](https://github.com/doctrine/orm) 3.7.6 | 1,536 | 3.1 s / 398 MiB | **Attributes and docblock generics**, and production names that collide with test classes. |
| [phpunit](https://github.com/sebastianbergmann/phpunit) 13.4.6 | 3,211 | 3.7 s / 443 MiB | **Modern PHP 8**: enums, readonly, namespace imports; many small files. |
| [carbon](https://github.com/briannesbitt/Carbon) 3.15.1 | 2,015 | 2.2 s / 251 MiB | **Traits and mixins**, plus locale-array files full of non-ASCII strings. |
| [guzzle](https://github.com/guzzle/guzzle) 8.3.1 | 145 | 1.2 s / 267 MiB | **Huge test files**, the highest RSS per source byte, and `curl_*` builtins against namespaced test mocks. |
| [laravel-app](https://github.com/laravel/laravel) v13.11.0 | 27 (+ 8,147 vendor) | 0.4 s / 25 MiB | **A real project**: the skeleton app plus its 111-package vendor tree, for the dependency tier. |
| [firefly](https://github.com/firefly-iii/firefly-iii) main at e1a90bf8 | 1,742 (+ 15,003 vendor) | 3.4 s / 392 MiB | **A full application**: a Laravel app's own controllers, models and jobs over a locked 15,000-file vendor tree; the shape of a real work repo, for the dependency tier and the check sweep at once. |

firefly was added 2026-11-19 and measured that day on main at 73405da4 (cold
`--check`, n=3, 4 vCPU), still without a dependency tier.

The full survey, with diagnostics by code and the worst findings, is
`design/php-corpora-survey-2026-11-08.md` in the project files.

## Vendor trees

`bootstrap.sh` builds a vendor tree for every repo with a `composer.json`
(all but wordpress) when `php` and `composer` are on `PATH`, so the
dependency tier has the packages each repo imports. Without one, every
import of an absent package widens: doctrine-orm's `resolved-by-widening`
rows fell from 1,056 to 17 once its vendor tree was in place (2026-11-24).

A library (`vendor` field `library`) resolves its own `composer.json` with
`require-dev`, as its CI does: composer is told the checkout's version
(`COMPOSER_ROOT_VERSION`, the pinned tag) and ignores platform requirements,
and the resolve runs against a copy of `composer.json` with `config.lock`
forced on, since several libraries set it off. A library that commits its
`composer.lock` reads that lock instead. Either way the tree ends with
`composer dump-autoload`. Vendor trees are git-excluded in each clone.

For laravel-app,
`composer update --no-install` resolves the lock, `fetch1.sh` shallow-fetches
each locked package and exports it the way a GitHub zipball would (zipballs
403 through the cloud proxy), `installed.php` writes
`vendor/composer/installed.json`, and `composer install` builds the autoloader.
Composer's home and cache stay inside the app (git-excluded). `vendor/` is in
the app's `.gitignore`, so a walk of the app root indexes the 27 app files
and leaves the vendor tree to the dependency tier.

firefly commits its `composer.lock`, so the resolve step is skipped and the
same fetch reads the committed lock: its vendor tree is pinned with the clone.
It ends with `composer dump-autoload`, not an install, because the packages
are already in place and an install reaches for a dist the proxy refuses.

## Reproducibility caveat

Each clone is pinned to a commit, and firefly's vendor tree with it, but
`composer update` resolves laravel-app's packages and every library's
(unless it commits a lock) at bootstrap time, so two runs weeks apart can
lock different versions. Pin the lock before a vendor tree gates anything.
The table above was measured on bare checkouts, before any repo but the
apps had a vendor tree.
