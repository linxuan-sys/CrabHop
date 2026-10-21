# CrabHop

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

## Licence

MIT — see `LICENSE`. Third-party notices: `NOTICE` (yazi's preset icon table, PDFium),
`LICENSE-ICONS` (nvim-tree, the icon set the table draws on), and
`assets/fonts/LICENSE-MapleMono.txt` for the bundled Maple Mono faces.