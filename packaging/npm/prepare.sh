#!/bin/sh
# 把编译好的二进制放进 npm 包（二进制不进 git）
set -eu
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH= cd -- "$here/../.." && pwd)
bin="$root/target/release/crabhop"
[ -x "$bin" ] || { echo "先编译：cargo build --release" >&2; exit 1; }
mkdir -p "$here/vendor"
cp "$bin" "$here/vendor/crabhop"
chmod 755 "$here/vendor/crabhop"
ls -lh "$here/vendor/crabhop"

# 桌面入口 + 图标随包走：postinstall 立刻注册，preuninstall 清掉
mkdir -p "$here/share/icons"
cp "$root/packaging/crabhop.desktop" "$here/share/crabhop.desktop"
for size in 128x128 256x256 512x512; do
	mkdir -p "$here/share/icons/$size"
	cp "$root/assets/icons/hicolor/$size/apps/crabhop.png" "$here/share/icons/$size/crabhop.png"
done
