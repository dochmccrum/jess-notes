#!/bin/sh
# Drops `libwebkit2gtk-4.1-0` from the .deb's Depends: Tauri's bundler always adds it, but the
# CEF build doesn't link WebKitGTK (DESIGN §23.2), and it would pull ~60 MB onto every install.
#   apps/tauri/cef/fix-deb.sh path/to/app.deb
set -eu
deb=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
cd "$work"
ar x "$deb"
mkdir control
tar xzf control.tar.gz -C control
sed -i -e 's/, libwebkit2gtk-4\.1-0//' -e 's/libwebkit2gtk-4\.1-0, //' control/control
tar czf control.tar.gz -C control .
# Member order matters to dpkg: debian-binary, control, data.
rm -f "$deb"
ar rc "$deb" debian-binary control.tar.gz data.tar.*
grep '^Depends' control/control
