#!/bin/sh
# 发一个版本，一条命令：
#     scripts/release.sh 1.0.4
# 依次做：编译 → 改 npm 版本号 → 打安装包 → 提交推送 → 打 tag → 建 GitHub Release 并上传安装包
#         → 发两个 npm 包（这一步会弹浏览器，点一次授权）
# 需要环境变量 GITHUB_TOKEN（建 release 用）。npm 的登录由 npm 自己管。
set -eu

version=${1:?用法: scripts/release.sh <版本号>，例如 1.0.4}
here=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$here"

echo "== 1/6 编译"
cargo build --release

echo "== 2/6 改 npm 版本号 -> $version"
node - "$version" <<'JS'
const fs = require("fs");
const version = process.argv[2];
for (const path of ["packaging/npm/package.json", "packaging/npm/platform-linux-x64/package.json"]) {
	const pkg = JSON.parse(fs.readFileSync(path, "utf8"));
	pkg.version = version;
	if (pkg.optionalDependencies) pkg.optionalDependencies["crabhop-linux-x64"] = version;
	fs.writeFileSync(path, JSON.stringify(pkg, null, 2) + "\n");
}
JS
sh packaging/npm/prepare.sh >/dev/null

echo "== 3/6 打安装包"
CRABHOP_VERSION="$version" sh scripts/make-tarball.sh "target/crabhop-$version-linux-x86_64.tar.xz"

echo "== 4/6 提交并推送"
git add -A packaging scripts README.md
git commit -m "release: $version" || echo "（没有要提交的改动）"
git push origin main
git tag -f "v$version"
git push -f origin "v$version"

echo "== 5/6 建 GitHub Release 并上传安装包"
: "${GITHUB_TOKEN:?需要 GITHUB_TOKEN 环境变量}"
release=$(curl -s -X POST -H "Authorization: token $GITHUB_TOKEN" -H "Content-Type: application/json" \
	-d "{\"tag_name\":\"v$version\",\"name\":\"v$version\",\"body\":\"CrabHop $version\"}" \
	https://api.github.com/repos/linxuan-sys/CrabHop/releases)
id=$(printf '%s' "$release" | python3 -c "import json,sys; print(json.load(sys.stdin)['id'])")
curl -sS --http1.1 --retry 3 -X POST -H "Authorization: token $GITHUB_TOKEN" -H "Content-Type: application/x-xz" \
	--data-binary @"target/crabhop-$version-linux-x86_64.tar.xz" \
	"https://uploads.github.com/repos/linxuan-sys/CrabHop/releases/$id/assets?name=crabhop-$version-linux-x86_64.tar.xz" >/dev/null
echo "   release: https://github.com/linxuan-sys/CrabHop/releases/tag/v$version"

echo "== 6/6 发 npm（会弹浏览器，各点一次授权）"
(cd packaging/npm/platform-linux-x64 && npm publish --registry=https://registry.npmjs.org)
(cd packaging/npm && npm publish --registry=https://registry.npmjs.org)

echo "== 完成：v$version"
