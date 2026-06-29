#!/usr/bin/env bash
# Quick-launch nvim with perl-lsp for manual testing.
# Routes by file type: C/C++ files build --features cpp and use the cpp
# config; everything else is the Perl path.
#
# Usage:
#   ./dev.sh                                  # Perl: test_files/frameworks.pl
#   ./dev.sh test_files/sample.pl             # a specific Perl file
#   ./dev.sh ~/personal/perl5/sv.c            # C/C++ → cpp-lsp
#   PERL_LSP_DEBUG=1 ./dev.sh <file>          # debug log → tail -f /tmp/perl-lsp.log
set -euo pipefail
cd "$(dirname "$0")"

file="${1:-test_files/frameworks.pl}"
case "$file" in
  *.c|*.h|*.cc|*.cpp|*.cxx|*.hpp|*.hh|*.hxx)
    echo "building --features cpp (this is cpp-lsp)…"
    cargo build --release --features cpp 2>&1 | tail -1
    exec nvim --clean -u e2e/init_cpp.lua "$file"
    ;;
  *)
    cargo build --release 2>&1 | tail -1
    export PERL5LIB="${PERL5LIB:-}:$PWD/test_files/lib"
    exec nvim --clean -u e2e/init.lua "$file"
    ;;
esac
