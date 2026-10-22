#!/usr/bin/env node
const { spawn } = require("node:child_process");
const path = require("node:path");

let binary;
try {
	binary = path.join(path.dirname(require.resolve("crabhop-linux-x64/package.json")), "bin", "crabhop");
} catch {
	console.error("crabhop: 没有找到本平台的二进制包（目前只有 linux x64）");
	process.exit(1);
}

const child = spawn(binary, process.argv.slice(2), { stdio: "inherit" });
child.on("exit", (code, signal) => {
	if (signal) process.kill(process.pid, signal);
	else process.exit(code ?? 1);
});
