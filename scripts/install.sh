#!/bin/sh
# Install CrabHop for one user, without root: the binary, the menu entry and the icons go under
# ~/.local, which every desktop already reads. `scripts/install.sh` alone uses a binary built in
# this tree; give it a directory (the unpacked release tarball) to install that one instead.
# Undo with scripts/uninstall.sh.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
bin="${1:-$here/target/release}/crabhop"
[ -x "$bin" ] || { echo "no crabhop binary at $bin — build it or unpack the release tarball first" >&2; exit 1; }

data="${XDG_DATA_HOME:-$HOME/.local/share}"
bindir="$HOME/.local/bin"
mkdir -p "$bindir" "$data/applications" "$data/licenses/crabhop"

install -m755 "$bin" "$bindir/crabhop"
install -m644 "$here/packaging/crabhop.desktop" "$data/applications/crabhop.desktop"
for size in 512x512 256x256 128x128; do
	install -Dm644 "$here/assets/icons/hicolor/$size/apps/crabhop.png" "$data/icons/hicolor/$size/apps/crabhop.png"
done
install -m644 "$here/LICENSE" "$data/licenses/crabhop/LICENSE"
install -m644 "$here/NOTICE" "$data/licenses/crabhop/NOTICE"
install -m644 "$here/LICENSE-ICONS" "$data/licenses/crabhop/LICENSE-ICONS"

# Let the desktop notice right away; both are best-effort and silent where absent.
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$data/applications" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -qtf "$data/icons/hicolor" 2>/dev/null || true

printf 'CrabHop installed:\n  %s/crabhop\n  %s/applications/crabhop.desktop\n' "$bindir" "$data"
case ":${PATH}:" in *":$bindir:"*) ;; *) echo "  (note: $bindir is not on PATH)" ;; esac
