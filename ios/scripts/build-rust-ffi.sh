#!/usr/bin/env bash
# Build (and commit) the NightKnightFFI xcframework from the Rust workspace.
#
#   bash ios/scripts/build-rust-ffi.sh          # build + refresh the .sha256 sidecar
#   bash ios/scripts/build-rust-ffi.sh --check  # CI staleness guard: recompute the
#                                               # source hash and fail if it differs
#                                               # from the committed sidecar (no Rust
#                                               # toolchain needed — runs on ubuntu)
#
# The xcframework is CHECKED INTO GIT so the app builds without a Rust toolchain.
# The sidecar `ios/Rust/NightKnightFFI.sha256` is a hash of every source input that
# shapes the artifact; CI recomputes it so a Rust analytics change that forgot to
# rebuild + recommit the xcframework is caught instead of shipped stale. Pairs with
# the `nk_abi_version()` runtime assert for defence in depth.
#
# Uses the workspace `ffi` cargo profile (release codegen + `panic = "unwind"`) —
# the FFI's catch_unwind boundary needs unwinding panics; the default release
# profile aborts.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="$ROOT/ios/Rust/NightKnightFFI.xcframework"
SIDECAR="$ROOT/ios/Rust/NightKnightFFI.sha256"

# Hash every source input that shapes the artifact. (Cargo.lock is deliberately
# excluded — unrelated server dep bumps would false-flag; the FFI's only deps are
# serde/serde_json/thiserror and behavioural drift is caught by the golden tests.)
hash_sources() {
    cd "$ROOT"
    local sha256
    if command -v sha256sum >/dev/null 2>&1; then
        sha256="sha256sum"
    else
        sha256="shasum -a 256"
    fi
    find service/crates/nightknight-ffi service/crates/nightknight-core ios/Rust/include \
        -type f \( -name '*.rs' -o -name '*.toml' -o -name '*.h' -o -name '*.modulemap' \) \
        | LC_ALL=C sort \
        | xargs $sha256 \
        | cat - Cargo.toml rust-toolchain.toml \
        | $sha256 | cut -d' ' -f1
}

if [[ "${1:-}" == "--check" ]]; then
    [[ -f "$SIDECAR" ]] || { echo "missing $SIDECAR — run ios/scripts/build-rust-ffi.sh" >&2; exit 1; }
    want="$(cat "$SIDECAR")"
    got="$(hash_sources)"
    if [[ "$want" != "$got" ]]; then
        echo "STALE xcframework: FFI/core sources changed but ios/Rust/NightKnightFFI.xcframework was not rebuilt." >&2
        echo "  committed: $want" >&2
        echo "  computed:  $got" >&2
        echo "Run: bash ios/scripts/build-rust-ffi.sh  (then commit ios/Rust)" >&2
        exit 1
    fi
    echo "xcframework sidecar matches sources ($got)"
    exit 0
fi

# rust-toolchain.toml pins the channel and lists the iOS targets; `rustup target add`
# is a cheap no-op when they are already installed for that channel.
rustup target add aarch64-apple-ios aarch64-apple-ios-sim

export IPHONEOS_DEPLOYMENT_TARGET=17.0

cd "$ROOT"
cargo build -p nightknight-ffi --profile ffi --target aarch64-apple-ios
cargo build -p nightknight-ffi --profile ffi --target aarch64-apple-ios-sim

rm -rf "$OUT"
xcodebuild -create-xcframework \
    -library "$ROOT/target/aarch64-apple-ios/ffi/libnightknight_ffi.a" \
    -headers "$ROOT/ios/Rust/include" \
    -library "$ROOT/target/aarch64-apple-ios-sim/ffi/libnightknight_ffi.a" \
    -headers "$ROOT/ios/Rust/include" \
    -output "$OUT"

# Debug symbols only bloat the committed artifact — linking needs just the global
# symbol table, and the app's release link dead-strips unused code anyway.
strip -S "$OUT"/ios-arm64/libnightknight_ffi.a
strip -S "$OUT"/ios-arm64-simulator/libnightknight_ffi.a

hash_sources > "$SIDECAR"
echo "Built $OUT"
echo "Sidecar $(cat "$SIDECAR")"
