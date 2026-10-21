#!/bin/sh
# Take back what ./install.sh put down. The window's own state (~/.config/crabhop,
# ~/.local/share/crabhop, ~/.cache/crabhop) is left alone on purpose — that is the person's, not
# the package's. Remove those three by hand if they are not wanted any more.
set -eu

data="${XDG_DATA_HOME:-$HOME/.local/share}"

rm -f "$HOME/.local/bin/crabhop"
rm -f "$data/applications/crabhop.desktop"
rm -f "$data/icons/hicolor/512x512/apps/crabhop.png" \
	"$data/icons/hicolor/256x256/apps/crabhop.png" \
	"$data/icons/hicolor/128x128/apps/crabhop.png"
rm -rf "$data/licenses/crabhop"

command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$data/applications" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -qtf "$data/icons/hicolor" 2>/dev/null || true
echo "CrabHop removed from ~/.local (state left in place)."
