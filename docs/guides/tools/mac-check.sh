#!/bin/sh
# Type-check a macOS-only crate on a Linux box (no linking, nothing runs).
# usage: mac-check.sh <path/to/Cargo.toml> [extra cargo args]
# Stub cc/ar stand in for Apple's toolchain; CARGO_FEATURE_PURE=1 keeps build
# scripts (blake3 etc.) from compiling C. Tauri apps also need a placeholder
# sidecar first: touch src-tauri/binaries/<name>-aarch64-apple-darwin
set -e
H=$(cd "$(dirname "$0")" && pwd)
M=$1; shift
CC_aarch64_apple_darwin=$H/fake-cc.sh AR_aarch64_apple_darwin=$H/fake-ar.sh CARGO_FEATURE_PURE=1 \
  cargo clippy --manifest-path "$M" --target aarch64-apple-darwin --all-targets "$@" -- -D warnings
