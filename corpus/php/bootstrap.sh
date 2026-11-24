#!/usr/bin/env bash
# Reconstruct the PHP corpus on a fresh box.
#
#   corpus/php/bootstrap.sh [dest]          # default: $PHP_CORPORA (~/php-corpora)
#   corpus/php/bootstrap.sh dest laravel    # one repo
#
# Each repo is pinned to the commit the recorded measurements used, fetched
# shallow. Idempotent: an existing clone is left alone (a warning names it if it
# sits off its pin).
#
# Every entry (the last field) also needs php + composer: its vendor tree is fetched
# one package at a time with a shallow git fetch, because GitHub dist zipballs
# 403 through the cloud proxy. Without composer it stays a bare checkout, which
# still measures the app files.
set -u
DEST="${1:-${PHP_CORPORA:-$HOME/php-corpora}}"
ONLY="${2:-}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# A pin moves only with a re-measure: every recorded number is against it.
#   name|url|commit|tag|why|vendor
# vendor: `library` for a library: its dependencies, dev ones included (the
# test framework its tests name), from its committed lock or one composer
# resolves at bootstrap time; `resolve` for an app with no committed lock;
# `lock` for an app whose composer.lock is committed, so its vendor tree is
# pinned with the clone.
REPOS=(
"monolog|https://github.com/Seldaek/monolog|d7059e4c691d05ea1222b5124bed0f961be46e15|3.12.1|small, clean baseline: mostly the fixed per-process cost|library"
"composer|https://github.com/composer/composer|472b418967419c6124edd91dc6310a3bec091dae|2.11.1|cross-package imports against absent symfony/semver; committed vendor/ fixtures|library"
"laravel|https://github.com/laravel/framework|1449bf241017087fbd38c118ecaabef3f6797fdc|v13.40.0|magic (facades, __call, Eloquent), the most widening, dense fluent-chain tests|library"
"wordpress|https://github.com/WordPress/WordPress|db96f5eaa77a8d3e8eec8923a518fa2326f0d46a|7.1.4|procedural globals, almost no namespaces, builtin functions vs polyfills|library"
"symfony-console|https://github.com/symfony/console|5a315b092b81f3e920e9daea1a2ae4c66ed298b9|v8.1.11|small strict-OO library|library"
"symfony-di|https://github.com/symfony/dependency-injection|d678f8d0527ed592593901da3ba2e2c17f7d51c1|v8.1.11|compiled-container fixtures, lazy-init readonly properties|library"
"doctrine-orm|https://github.com/doctrine/orm|4487f850401c0e1f27891cce5897fa90b3055c2a|3.7.6|attributes, docblock generics, production names that collide with test classes|library"
"phpunit|https://github.com/sebastianbergmann/phpunit|4821376960d0e1e5a7b7b700955ebcab33ecd71b|13.4.6|modern 8.x: enums, readonly, namespace imports; many small files|library"
"carbon|https://github.com/briannesbitt/Carbon|ce0e5636fae7d1e99856eb70cae4c719a284aa08|3.15.1|traits and mixins; locale-array files full of non-ASCII strings|library"
"guzzle|https://github.com/guzzle/guzzle|7d9b1f430bb65ac664e120af227bf9a795ad6333|8.3.1|huge test files, highest RSS per source byte, curl_* builtins vs namespaced mocks|library"
"laravel-app|https://github.com/laravel/laravel|06d016a364a37430eec9cbc52209adce3d7667ce|v13.11.0|a real project: skeleton app plus its 111-package vendor tree|resolve"
"firefly|https://github.com/firefly-iii/firefly-iii|e1a90bf8ee7499524039b331179e40431b3c84af|main 2026-11-19|a full Laravel app: 1,700 project files over a locked vendor tree|lock"
)

mkdir -p "$DEST" || exit 1
for ENTRY in "${REPOS[@]}"; do
  IFS='|' read -r NAME URL PIN TAG WHY VENDOR <<< "$ENTRY"
  [ -n "$ONLY" ] && [ "$ONLY" != "$NAME" ] && continue
  echo "══ $NAME ($TAG) — $WHY"
  if [ -d "$DEST/$NAME/.git" ]; then
    AT="$(git -C "$DEST/$NAME" rev-parse HEAD 2>/dev/null)"
    if [ "$AT" = "$PIN" ]; then
      echo "   clone: present at pin"
    else
      echo "   clone: present but at ${AT:-?}, NOT the pin ${PIN:0:12} — numbers won't compare"
      echo "          git -C $DEST/$NAME fetch --depth 1 origin $PIN && git -C $DEST/$NAME checkout FETCH_HEAD"
    fi
  else
    { git init -q "$DEST/$NAME" &&
      git -C "$DEST/$NAME" remote add origin "$URL" &&
      git -C "$DEST/$NAME" fetch -q --depth 1 origin "$PIN" &&
      git -C "$DEST/$NAME" checkout -q FETCH_HEAD; } ||
      { echo "   CLONE FAILED"; rm -rf "$DEST/$NAME"; continue; }
    echo "   clone: ok at ${PIN:0:12}"
  fi

  if [ -n "$VENDOR" ]; then
    APP="$DEST/$NAME"
    if [ -f "$APP/vendor/composer/installed.json" ]; then
      echo "   vendor: present"
    elif [ ! -f "$APP/composer.json" ]; then
      echo "   vendor: none (no composer.json)"
    elif ! command -v composer >/dev/null || ! command -v php >/dev/null; then
      echo "   vendor: SKIPPED (needs php + composer); the project files alone are still usable"
    else
      # Composer state stays inside the app, never in the user's home.
      export COMPOSER_HOME="$APP/.composer-home" COMPOSER_CACHE_DIR="$APP/.composer-cache"
      # A library that commits its lock is pinned with its clone, as `lock`.
      [ "$VENDOR" = library ] && [ -f "$APP/composer.lock" ] && git -C "$APP" ls-files --error-unmatch composer.lock >/dev/null 2>&1 &&
        VENDOR=lock
      # A library's checkout is detached, so composer is told the version
      # it is (a dependency that requires the library back needs it), and a
      # library that turns its lock off (`"lock": false`) still gets one, from
      # a copy of its manifest, for the fetch to read.
      # An app is installed and run, so it resolves for this box's php; a
      # library is only read, so it resolves for the platform it declares.
      unset COMPOSER_ROOT_VERSION
      PLATFORM=()
      [ "$VENDOR" = library ] && export COMPOSER_ROOT_VERSION="${TAG#v}" && PLATFORM=(--ignore-platform-reqs)
      ( cd "$APP" &&
        { [ "$VENDOR" = lock ] ||
          { php -r '$j = json_decode(file_get_contents("composer.json"), true); $j["config"]["lock"] = true;
                    file_put_contents(".composer-resolve.json", json_encode($j, JSON_PRETTY_PRINT | JSON_UNESCAPED_SLASHES));' &&
            COMPOSER=.composer-resolve.json composer update --no-install --no-scripts --no-interaction "${PLATFORM[@]}" -q &&
            mv .composer-resolve.lock composer.lock; }; } &&
        mkdir -p vendor &&
        php "$HERE/lock2tsv.php" composer.lock |
          awk -F'\t' '$2 != "-" { print $1, $2, $3 }' |
          xargs -P4 -n3 "$HERE/fetch1.sh" "$APP/vendor" >/dev/null &&
        php "$HERE/installed.php" . &&
        if [ "$VENDOR" != resolve ]; then
          # The packages are already in place at their locked commits, and an
          # install would reach for a dist the proxy refuses.
          composer dump-autoload --no-scripts --ignore-platform-reqs -q
        else
          composer install --no-interaction -q &&
          { [ -f .env ] || cp .env.example .env; } &&
          php artisan key:generate -q
        fi ) &&
        echo "   vendor: ok ($(find "$APP/vendor" -name '*.php' | wc -l) .php files)" ||
        echo "   vendor: FAILED"
      for X in '/.composer-*' /vendor/ /composer.lock; do
        grep -qxF "$X" "$APP/.git/info/exclude" 2>/dev/null ||
          echo "$X" >> "$APP/.git/info/exclude"
      done
    fi
  fi
done
