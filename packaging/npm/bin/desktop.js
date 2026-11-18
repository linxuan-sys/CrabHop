#!/usr/bin/env node
// npm 生命周期钩子：装完立刻有桌面入口，卸完不留残骸。
// postinstall → install；preuninstall → uninstall。
// 非 Linux、或文件操作失败都只吱一声不报错——二进制首次启动时的
// desktop::register() 兜底，npm install 绝不能被这个脚本搞挂。
const fs = require("node:fs");
const path = require("node:path");
const os = require("node:os");
const { spawnSync } = require("node:child_process");

if (process.platform !== "linux") process.exit(0);

const share = path.join(__dirname, "..", "share");
const sizes = ["128x128", "256x256", "512x512"];

// root（sudo npm i -g）装进系统目录，普通用户装进自己的 ~/.local/share
function userData() {
	const xdg = process.env.XDG_DATA_HOME;
	if (xdg) return xdg;
	return path.join(os.homedir(), ".local", "share");
}
const isRoot = process.getuid && process.getuid() === 0;
const target = isRoot ? "/usr/local/share" : userData();

function refresh(apps, icons) {
	// 最佳努力：工具不在就算了，桌面环境自己也会扫
	try { spawnSync("update-desktop-database", [apps], { stdio: "ignore" }); } catch {}
	try { spawnSync("gtk-update-icon-cache", ["-q", "-t", "-f", icons], { stdio: "ignore" }); } catch {}
}

function install() {
	const apps = path.join(target, "applications");
	const icons = path.join(target, "icons", "hicolor");
	fs.mkdirSync(apps, { recursive: true });
	fs.copyFileSync(path.join(share, "crabhop.desktop"), path.join(apps, "crabhop.desktop"));
	for (const size of sizes) {
		const dir = path.join(icons, size, "apps");
		fs.mkdirSync(dir, { recursive: true });
		fs.copyFileSync(path.join(share, "icons", size, "crabhop.png"), path.join(dir, "crabhop.png"));
	}
	refresh(apps, icons);
	console.log(`crabhop: 桌面入口已注册（${apps}）`);
}

function uninstall() {
	// 系统和用户两处都扫一遍：只删我们自己的文件，空目录顺手收，非空不动
	for (const data of ["/usr/local/share", userData()]) {
		const apps = path.join(data, "applications");
		const icons = path.join(data, "icons", "hicolor");
		try { fs.unlinkSync(path.join(apps, "crabhop.desktop")); } catch {}
		for (const size of sizes) {
			try { fs.unlinkSync(path.join(icons, size, "apps", "crabhop.png")); } catch {}
			try { fs.rmdirSync(path.join(icons, size, "apps")); } catch {}
			try { fs.rmdirSync(path.join(icons, size)); } catch {}
		}
		refresh(apps, icons);
	}
	console.log("crabhop: 桌面入口已清理");
}

try {
	if (process.argv[2] === "uninstall") uninstall();
	else install();
} catch (e) {
	console.warn(`crabhop: 桌面入口处理失败（不影响使用，首次启动会补注册）：${e.message}`);
}