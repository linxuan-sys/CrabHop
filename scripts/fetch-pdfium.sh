#!/usr/bin/env bash
# PDFium, pinned: the static library the preview renders PDF pages with.
#
# Upstream publishes the tarball as a release asset; the proxy is only a mirror of the same bytes,
# and the hash below is what makes that acceptable. The library is not committed — run this once
# after cloning. The directory it lands in (`duckfm/libs`) is ignored by git.
#
#   duckfm/scripts/fetch-pdfium.sh
set -euo pipefail

SHA256="13908bb2d40a6e017c4c5a6a7baecc6efd7b1c30392c8a79e80072d2b48b18eb"
URL="https://github.com/kognitos/pdfium-static/releases/download/chromium%2F7809/pdfium-linux-x64-static.tgz"
REF="chromium/7809"

cd "$(dirname "$0")/.."
mkdir -p libs

if [ -f libs/pdfium/.sha256 ] && [ "$(cat libs/pdfium/.sha256)" = "$SHA256" ]; then
	echo "pdfium: $REF already fetched"
	exit 0
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "pdfium: downloading $REF"
if ! curl -fL --retry 3 --connect-timeout 15 -o "$tmp/pdfium.tgz" "https://gh-proxy.com/$URL"; then
	curl -fL --retry 3 --connect-timeout 15 -o "$tmp/pdfium.tgz" "$URL"
fi

echo "$SHA256  $tmp/pdfium.tgz" | sha256sum -c -

rm -rf libs/pdfium
mkdir -p libs/pdfium
tar -xzf "$tmp/pdfium.tgz" -C libs/pdfium
echo "$SHA256" > libs/pdfium/.sha256
echo "pdfium: ready in duckfm/libs/pdfium ($(cat libs/pdfium/VERSION))"
