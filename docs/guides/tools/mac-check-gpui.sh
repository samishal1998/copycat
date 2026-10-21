#!/bin/sh
# Same, for a GPUI app: gpui-pre-apple's build script compiles Metal shaders,
# which a Linux box cannot do, so patch in a copy of the crate whose build.rs
# writes empty outputs. Stage Cargo.lock changes first: it is restored after.
# usage: mac-check-gpui.sh <app dir containing Cargo.toml>
set -e
APP=$(cd "$1" && pwd)
H=$(cd "$(dirname "$0")" && pwd)
W=${TMPDIR:-/tmp}/gpui-apple-stub
if [ ! -d "$W" ]; then
  SRC=$(ls -d ~/.cargo/registry/src/*/gpui-pre-apple-* | tail -1)   # run `cargo fetch` first
  cp -r "$SRC" "$W"
  cat > "$W/build.rs" <<'RS'
fn main() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    std::fs::write(out.join("stitched_shaders.metal"), "").unwrap();
    std::fs::write(out.join("shaders.metallib"), b"").unwrap();
    std::fs::write(out.join("scene.h"), "").unwrap();
}
RS
fi
cd "$APP"
set +e
CC_aarch64_apple_darwin=$H/fake-cc.sh AR_aarch64_apple_darwin=$H/fake-ar.sh CARGO_FEATURE_PURE=1 \
  CARGO_TARGET_DIR=${TMPDIR:-/tmp}/gpui-check-target \
  cargo clippy --target aarch64-apple-darwin --all-targets \
  --config "patch.crates-io.gpui-pre-apple.path=\"$W\"" -- -D warnings
st=$?
git checkout -q Cargo.lock 2>/dev/null
exit $st
