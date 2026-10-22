#!/bin/sh
# 把编译好的二进制放进平台子包，供 npm publish 使用（二进制不进 git）。
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/../.." && pwd)
bin="$root/target/release/crabhop"
[ -x "$bin" ] || { echo "先编译：cargo build --release" >&2; exit 1; }
mkdir -p "$here/platform-linux-x64/bin"
cp "$bin" "$here/platform-linux-x64/bin/crabhop"
chmod 755 "$here/platform-linux-x64/bin/crabhop"
ls -lh "$here/platform-linux-x64/bin/crabhop"
