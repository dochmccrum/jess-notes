#!/bin/sh
# Stages the CEF runtime for the Linux packages (DESIGN §23): the `cef` crate's build script
# unpacks CEF into the Cargo target directory; this copies what the app needs at run time into
# apps/tauri/cef/dist (mapped to /usr/lib/jess-notes by tauri.conf.json), with libcef.so stripped
# (1.4 GB with symbols, ~260 MB without).
#   apps/tauri/cef/stage.sh [target/release]
set -eu
here=$(cd "$(dirname "$0")" && pwd)
src=${1:-$here/../../../target/release}
out=$here/dist
[ -f "$src/libcef.so" ] || { echo "no libcef.so in $src: build the app first" >&2; exit 1; }
rm -rf "$out"
mkdir -p "$out/locales"
for f in libcef.so libEGL.so libGLESv2.so libvk_swiftshader.so libvulkan.so.1 vk_swiftshader_icd.json \
  chrome_100_percent.pak chrome_200_percent.pak resources.pak icudtl.dat v8_context_snapshot.bin; do
  cp "$src/$f" "$out/"
done
cp "$src"/locales/*.pak "$out/locales/"
strip --strip-unneeded "$out/libcef.so"
du -sh "$out"
