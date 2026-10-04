#!/usr/bin/env bash
# Fixes the AppImage Tauri's bundler made (DESIGN §22 item 87), in place:
#   - NSS and NSPR come from the system, not the bundle. NSS loads its crypto module
#     (libsoftokn3.so) from the system whatever is bundled, and a system newer than the build
#     machine's needs a newer libnssutil3 than the bundled one ("NSSUTIL_3.108 not found"): CEF
#     then aborts at start, so the app "closes instantly" (Fedora 44 with an Ubuntu 24.04 build).
#     Every desktop distribution has NSS (the .deb depends on it too).
#   - WebKitGTK and JavaScriptCore go: linuxdeploy bundles them for Tauri, but the app runs on
#     CEF and never loads them.
#
#   apps/tauri/cef/fix-appimage.sh target/release/bundle/appimage/*.AppImage
set -euo pipefail
img=$(realpath "$1")
plugin=${LINUXDEPLOY_PLUGIN_APPIMAGE:-$HOME/.cache/tauri/linuxdeploy-plugin-appimage.AppImage}
[ -x "$plugin" ] || { echo "fix-appimage: $plugin not found (it comes with tauri build)" >&2; exit 1; }
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"
"$img" --appimage-extract >/dev/null
lib=squashfs-root/usr/lib
removed=0
for p in 'libnss3.so*' 'libnssutil3.so*' 'libsmime3.so*' 'libssl3.so*' 'libsoftokn3.so*' 'libfreebl*.so*' 'libnspr4.so*' 'libplc4.so*' 'libplds4.so*' \
  'libwebkit2gtk-4.1.so*' 'libjavascriptcoregtk-4.1.so*'; do
  for f in $lib/$p; do
    [ -e "$f" ] || continue
    rm -f "$f"
    removed=$((removed + 1))
  done
done
# The repacked image gets the same name; the plugin runs extracted (no FUSE on CI runners).
OUTPUT="$work/out.AppImage" ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$plugin" --appdir squashfs-root >/dev/null
mv "$work/out.AppImage" "$img"
chmod +x "$img"
echo "fix-appimage: removed $removed libraries; $(du -h "$img" | cut -f1) $(basename "$img")"
