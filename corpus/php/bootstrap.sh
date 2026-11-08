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
# laravel-app also needs php + composer: its vendor tree is fetched one package
# at a time with a shallow git fetch, because GitHub dist zipballs 403 through
# the cloud proxy. Without composer it stays a bare skeleton, which still
# measures the app files.
set -u
DEST="${1:-${PHP_CORPORA:-$HOME/php-corpora}}"
ONLY="${2:-}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# A pin moves only with a re-measure: every recorded number is against it.
#   name|url|commit|tag|why
REPOS=(
"monolog|https://github.com/Seldaek/monolog|d7059e4c691d05ea1222b5124bed0f961be46e15|3.12.1|small, clean baseline: mostly the fixed per-process cost"
"composer|https://github.com/composer/composer|472b418967419c6124edd91dc6310a3bec091dae|2.11.1|cross-package imports against absent symfony/semver; committed vendor/ fixtures"
"laravel|https://github.com/laravel/framework|1449bf241017087fbd38c118ecaabef3f6797fdc|v13.40.0|magic (facades, __call, Eloquent), the most widening, dense fluent-chain tests"
"wordpress|https://github.com/WordPress/WordPress|db96f5eaa77a8d3e8eec8923a518fa2326f0d46a|7.1.4|procedural globals, almost no namespaces, builtin functions vs polyfills"
"symfony-console|https://github.com/symfony/console|5a315b092b81f3e920e9daea1a2ae4c66ed298b9|v8.1.11|small strict-OO library"
"symfony-di|https://github.com/symfony/dependency-injection|d678f8d0527ed592593901da3ba2e2c17f7d51c1|v8.1.11|compiled-container fixtures, lazy-init readonly properties"
"doctrine-orm|https://github.com/doctrine/orm|4487f850401c0e1f27891cce5897fa90b3055c2a|3.7.6|attributes, docblock generics, production names that collide with test classes"
"phpunit|https://github.com/sebastianbergmann/phpunit|4821376960d0e1e5a7b7b700955ebcab33ecd71b|13.4.6|modern 8.x: enums, readonly, namespace imports; many small files"
"carbon|https://github.com/briannesbitt/Carbon|ce0e5636fae7d1e99856eb70cae4c719a284aa08|3.15.1|traits and mixins; locale-array files full of non-ASCII strings"
"guzzle|https://github.com/guzzle/guzzle|7d9b1f430bb65ac664e120af227bf9a795ad6333|8.3.1|huge test files, highest RSS per source byte, curl_* builtins vs namespaced mocks"
"laravel-app|https://github.com/laravel/laravel|06d016a364a37430eec9cbc52209adce3d7667ce|v13.11.0|a real project: skeleton app plus its 111-package vendor tree"
)

mkdir -p "$DEST" || exit 1
for ENTRY in "${REPOS[@]}"; do
  IFS='|' read -r NAME URL PIN TAG WHY <<< "$ENTRY"
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

  if [ "$NAME" = "laravel-app" ]; then
    APP="$DEST/$NAME"
    if [ -f "$APP/vendor/composer/installed.json" ]; then
      echo "   vendor: present"
    elif ! command -v composer >/dev/null || ! command -v php >/dev/null; then
      echo "   vendor: SKIPPED (needs php + composer); the skeleton alone is still usable"
    else
      # Composer state stays inside the app, never in the user's home.
      export COMPOSER_HOME="$APP/.composer-home" COMPOSER_CACHE_DIR="$APP/.composer-cache"
      ( cd "$APP" &&
        composer update --no-install --no-scripts --no-interaction -q &&
        mkdir -p vendor &&
        php "$HERE/lock2tsv.php" composer.lock |
          awk -F'\t' '$2 != "-" { print $1, $2, $3 }' |
          xargs -P4 -n3 "$HERE/fetch1.sh" "$APP/vendor" >/dev/null &&
        php "$HERE/installed.php" . &&
        composer install --no-interaction -q &&
        { [ -f .env ] || cp .env.example .env; } &&
        php artisan key:generate -q ) &&
        echo "   vendor: ok ($(find "$APP/vendor" -name '*.php' | wc -l) .php files)" ||
        echo "   vendor: FAILED"
      grep -qx '/.composer-*' "$APP/.git/info/exclude" 2>/dev/null ||
        echo '/.composer-*' >> "$APP/.git/info/exclude"
    fi
  fi
done
