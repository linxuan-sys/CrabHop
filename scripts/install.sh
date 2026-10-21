#!/bin/sh
# Install CrabHop for one user, without root: the binary, the menu entry and the icons go under
# ~/.local, which every desktop already reads. Run it from the unpacked tarball (the files sit
# beside this script) or from a source tree. Undo with ./uninstall.sh.
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)

# The tarball is flat; a source tree keeps the binary under target/release.
bin="$here/crabhop"
[ -x "$bin" ] || bin="$here/target/release/crabhop"
[ -x "$bin" ] || { echo "no crabhop binary here — run this from the unpacked tarball or build first" >&2; exit 1; }

# The tarball's own copies first; a source tree's next; a bare binary says so and stops.
icon="$here/icon.png"
desktop="$here/crabhop.desktop"
[ -f "$icon" ] || icon="$here/assets/icons/crabhop.png"
[ -f "$desktop" ] || desktop="$here/packaging/crabhop.desktop"
{ [ -f "$icon" ] && [ -f "$desktop" ]; } || {
	echo "no icon or menu entry here: this looks like a bare binary, not the release tarball" >&2
	echo "run the tarball's own ./install.sh, or launch ./crabhop directly" >&2
	exit 1
}

data="${XDG_DATA_HOME:-$HOME/.local/share}"
bindir="$HOME/.local/bin"
mkdir -p "$bindir" "$data/applications" "$data/licenses/crabhop"

install -m755 "$bin" "$bindir/crabhop"
install -m644 "$desktop" "$data/applications/crabhop.desktop"

# The menu's Icon=crabhop asks the theme for the name; three sizes are what desktops look for.
for size in 512 256 128; do
	if [ -f "$here/hicolor/${size}x${size}/apps/crabhop.png" ]; then
		install -Dm644 "$here/hicolor/${size}x${size}/apps/crabhop.png" "$data/icons/hicolor/${size}x${size}/apps/crabhop.png"
	elif [ -f "$here/assets/icons/hicolor/${size}x${size}/apps/crabhop.png" ]; then
		install -Dm644 "$here/assets/icons/hicolor/${size}x${size}/apps/crabhop.png" "$data/icons/hicolor/${size}x${size}/apps/crabhop.png"
	fi
done

for name in LICENSE NOTICE LICENSE-ICONS; do
	[ -f "$here/$name" ] && install -m644 "$here/$name" "$data/licenses/crabhop/$name"
done
[ -f "$here/LICENSE-MapleMono.txt" ] && install -m644 "$here/LICENSE-MapleMono.txt" "$data/licenses/crabhop/LICENSE-MapleMono.txt"

# Let the desktop notice right away; both are best-effort and silent where absent.
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database "$data/applications" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -qtf "$data/icons/hicolor" 2>/dev/null || true

# The examples, so a person has something to read and copy before writing their own: the factory
# key table (the same 94 rows the help panel shows) and a commented config. Never overwrite a file
# that is already there — those are the person's edits, not ours.
conf="${XDG_CONFIG_HOME:-$HOME/.config}/crabhop"
mkdir -p "$conf"
for pair in "keymap-default.toml:keymap.toml.example" "config.example.toml:config.toml.example"; do
	src="$here/${pair%%:*}"
	dst="$conf/${pair##*:}"
	[ -f "$src" ] && [ ! -e "$dst" ] && install -m644 "$src" "$dst"
done

printf 'CrabHop installed:\n  %s/crabhop\n  %s/applications/crabhop.desktop\n' "$bindir" "$data"
case ":${PATH}:" in *":$bindir:"*) ;; *) echo "  (note: $bindir is not on PATH — the menu entry still works)" ;; esac
echo "  start it from your application menu, or run: $bindir/crabhop"

# The tarball has done its job; take it away with its unpacked directory's parent in mind, so a
# person who downloaded, unpacked and installed is left with nothing to clean up (P45). Only a
# crabhop tarball, only beside this directory, and the trash when the desktop offers one.
for archive in "$here"/../crabhop-*-linux-*.tar.xz; do
	[ -f "$archive" ] || continue
	if command -v gio >/dev/null 2>&1; then
		gio trash "$archive" 2>/dev/null && { echo "  moved the tarball to the trash: $(basename "$archive")"; continue; }
	fi
	rm -f "$archive" && echo "  removed the tarball: $(basename "$archive")"
done
