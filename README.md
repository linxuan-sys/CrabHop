# CrabHop

English | [中文](README.zh-CN.md)

A native, keyboard-first file manager: the window owns its state and draws its own list — three
columns, a preview stack (pictures, PDF, office documents, archives, media), the desktop's own
open-with list, and a headless harness that replays keys and paints frames without a window.
Extracted from an earlier tree as this clean cut; the design notes live in the source's comments.

## Build

	cargo build --release

The PDF preview links a statically fetched PDFium; fetch it once with `scripts/fetch-pdfium.sh`
(it lands, gitignored, in `libs/pdfium`).

## Test

	cargo test

## Run

	cargo run --release

The window opens on the current directory. The factory keymap is `keymap-default.toml` beside
this file — that table is the keys' own source of truth (the help overlay and the `which` hints
read it). A replay or a screenshot runs headless, no window:

	CRABHOP_KEYS="key j;key j;shot" CRABHOP_SHOT=/tmp/crabhop cargo run
	tools/run-crabhop.sh /a/directory /tmp/crabhop   # the same, from anywhere

## Install

- **From the AUR**: `yay -S crabhop` (the packaging lives in `packaging/arch/PKGBUILD`).
- **From a release tarball**: unpack `crabhop-<version>-x86_64.tar.gz` and put `crabhop` on your `PATH`.
- **From source**: `cargo build --release` — fetch PDFium first, as **Build** above says.

Or, without root: `scripts/install.sh` puts the binary, the menu entry and the icons under
`~/.local` (`scripts/uninstall.sh` takes them back). The AUR package, `packaging/arch/PKGBUILD`,
holds the same files for a system-wide install.

## Community-welcome items

The configuration surface is deliberately two-layered — the factory's own files here, overridden
by `~/.config/crabhop/config.toml` and `~/.config/crabhop/keymap.toml` — and the list below is
what this project hands to the community: extensions whose specs were already written down (the
earlier tree's `docs/design/native-ui.md` is the archive; the § numbers are its own), each one
landing in that same two-layer structure.

- **Theme switching — Mocha / Latte**: the dark palette is `src/tokens.rs`'s `Tokens::MOCHA`; a
  second batch and the key that picks between them (dark / light / follow the system) is the
  whole job — spec §9 (the light-mode tables) and §7.2 (the appearance page's three-way control).
- **Font family and size**: `src/tokens.rs` installs the bundled Maple faces by name today; the
  family and a 14–20px size — the line height following at 1.5 — want to come from
  `config.toml` — spec §7.2's typography row.
- **Preview cache caps and a preload switch**: `src/preview.rs` caches text by path and mtime in
  the window's cache directory, and warms neighbouring pictures while the window is idle; both
  want their caps and their on/off in `config.toml` — spec §3 (the preview rules).
- **Startup defaults — sort, linemode, hidden files, the confirm policy**: the window starts on
  its own choices; `config.toml` should carry them so a person's first frame is already theirs —
  spec §7.1 (the general page — hidden files and the confirm policy are its own rows; sort and
  the linemodes want rows beside them) and §1.1 (the two densities).
- **A user icon table**: `src/icons/table.rs` is the factory's own (the preset table, drawn on
  §1's icon box); an `[icons]` table in `config.toml` would let a person add or override rows —
  spec §1, and `NOTICE` for where the factory table comes from.

And the two items the earlier tree already parked for the community, kept here so they are not
lost:

- **Drag and drop, both directions**: receiving a drop into the window (~1 day) and initiating a
  drag out of it (~3–5 days) — the desktop protocols are the work; the list is ready for it.
- **The clipboard's inbound half**: reading an external `text/uri-list` into the window (~3
  days) — the reverse of what `src/uri_clipboard.rs` already offers file managers.

## Licence

MIT — see `LICENSE`. Third-party notices: `NOTICE` (yazi's preset icon table, PDFium),
`LICENSE-ICONS` (nvim-tree, the icon set the table draws on), and
`assets/fonts/LICENSE-MapleMono.txt` for the bundled Maple Mono faces.