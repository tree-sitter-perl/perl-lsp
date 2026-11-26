#!/usr/bin/env bash
# Build perl-lsp for WebAssembly: `scripts/wasm/build.sh [cargo build args]`.
#
# Needs, beyond the Rust target (`rustup target add wasm32-wasip1-threads`):
#   - clang with the wasm32 backend (stock distro clang has it)
#   - WASI_SYSROOT pointing at a wasi-sdk sysroot, for the C dependencies'
#     libc headers (tree-sitter, the grammars, zstd, SQLite)
#
# The threaded target is the default because the server is threaded: the
# resolver, the stdio bridge and the blocking pool are all `std` threads.
set -euo pipefail

target="${WASM_TARGET:-wasm32-wasip1-threads}"
: "${WASI_SYSROOT:?set WASI_SYSROOT to a wasi-sdk sysroot (github.com/WebAssembly/wasi-sdk releases)}"
[ -d "$WASI_SYSROOT/include/$target" ] || {
    echo "WASI_SYSROOT=$WASI_SYSROOT has no include/$target" >&2
    exit 1
}

ar="${WASM_AR:-}"
if [ -z "$ar" ]; then
    for cand in llvm-ar llvm-ar-20 llvm-ar-19 llvm-ar-18 llvm-ar-17; do
        if command -v "$cand" >/dev/null; then ar="$cand"; break; fi
    done
fi
: "${ar:?no llvm-ar found; set WASM_AR}"

# cc-rs reads the per-target spellings, so a native build in the same shell
# is untouched.
key="${target//-/_}"
cflags="--sysroot=$WASI_SYSROOT"
case "$target" in *-threads) cflags="$cflags -pthread" ;; esac
export "CC_$key=${WASM_CC:-clang}" "AR_$key=$ar" "CFLAGS_$key=$cflags"

exec cargo build --target "$target" "$@"
