#!/usr/bin/env bash
# The headless runner: a replay's keys and a screenshot's frames, no window.
#
#   CRABHOP_KEYS="key j;shot" tools/run-crabhop.sh [directory] [shot-prefix]
#
# `CRABHOP_KEYS` takes the replay's own steps (`key j`, `menu 0`, `type chrom`, `wheel -5`,
# `shot`, … — src/app.rs's replay has the whole vocabulary); `CRABHOP_SHOT` is the prefix the
# frames land under (`<prefix>-N.png`). Without a directory the window stands where the caller
# stands. The harness needs no display: a replay asks for it by itself.
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
dir="${1:-$PWD}"
shot="${2:-}"

# the PDFium path lives in the repository's own .cargo/config.toml, which cargo only reads from
# there: the runner hands it over itself, so it works from any directory
export PDFIUM_STATIC_LIB_PATH="${PDFIUM_STATIC_LIB_PATH:-$here/libs/pdfium/lib}"

cd "$dir"
if [ -n "$shot" ]; then
	CRABHOP_SHOT="$shot" cargo run --manifest-path "$here/Cargo.toml"
else
	cargo run --manifest-path "$here/Cargo.toml"
fi