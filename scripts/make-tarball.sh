#!/bin/sh
# Build the release tarball: everything a stranger needs, flat in one directory —
#   crabhop          the binary
#   install.sh       puts the binary, the menu entry and the icons under ~/.local
#   uninstall.sh     takes them back
#   crabhop.desktop  the menu entry itself
#   icon.png         the source icon, for whatever the desktop asks
#   hicolor/…        the three sizes a theme reads
#   LICENSE NOTICE LICENSE-ICONS LICENSE-MapleMono.txt
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out=${1:-$here/target/crabhop-1.0.0-linux-x86_64.tar.xz}
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

bin="$here/target/release/crabhop"
[ -x "$bin" ] || { echo "build first: cargo build --release" >&2; exit 1; }

install -m755 "$bin" "$stage/crabhop"
install -m755 "$here/scripts/install.sh" "$stage/install.sh"
install -m755 "$here/scripts/uninstall.sh" "$stage/uninstall.sh"
install -m644 "$here/packaging/crabhop.desktop" "$stage/crabhop.desktop"
install -m644 "$here/assets/icons/crabhop.png" "$stage/icon.png"
for size in 512x512 256x256 128x128; do
	install -Dm644 "$here/assets/icons/hicolor/$size/apps/crabhop.png" "$stage/hicolor/$size/apps/crabhop.png"
done
for name in LICENSE NOTICE LICENSE-ICONS; do
	install -m644 "$here/$name" "$stage/$name"
done
install -m644 "$here/assets/fonts/LICENSE-MapleMono.txt" "$stage/LICENSE-MapleMono.txt"

tar -C "$stage" -cJf "$out" .
ls -l "$out"
sha256sum "$out"
