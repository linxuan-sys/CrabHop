#!/usr/bin/env node
const { spawn } = require("node:child_process");
const path = require("node:path");

const binary = path.join(__dirname, "..", "vendor", "crabhop");
const child = spawn(binary, process.argv.slice(2), { stdio: "inherit" });
child.on("exit", (code, signal) => {
	if (signal) process.kill(process.pid, signal);
	else process.exit(code ?? 1);
});
