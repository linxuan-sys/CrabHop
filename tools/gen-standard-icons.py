#!/usr/bin/env python3
"""Regenerate src/icons/standard.rs from a yazi theme.toml [icon] section.

Usage: tools/gen-standard-icons.py [path-to-theme.toml]   (default: ~/.config/yazi/theme.toml)
"""
import sys
import tomllib
from pathlib import Path

SRC = Path(sys.argv[1]) if len(sys.argv) > 1 else Path.home() / ".config/yazi/theme.toml"
OUT = Path(__file__).resolve().parent.parent / "src/icons/standard.rs"

with open(SRC, "rb") as f:
	icon = tomllib.load(f)["icon"]

def rules(key):
	return icon.get(f"prepend_{key}", []) + icon.get(key, []) + icon.get(f"append_{key}", [])

def esc(text):
	return "".join(f"\\u{{{ord(c):x}}}" for c in text)

def col(fg):
	return int(fg.lstrip("#"), 16)

def array(name, items):
	lines = [f"pub(super) const {name}: &[(&str, &str, u32)] = &["]
	for r in items:
		lines.append(f'\t("{r["name"]}", "{esc(r["text"])}", {col(r["fg"]):#08x}),')
	lines.append("];")
	return "\n".join(lines)

CONDS_PARAMS = ["dir", "hidden", "orphan", "link", "exec"]

def cond_if(expr):
	parts = []
	for term in expr.split("&"):
		term = term.strip()
		neg = term.startswith("!")
		ident = term.lstrip("!").strip()
		if ident not in CONDS_PARAMS:
			raise SystemExit(f"unknown cond identifier: {ident!r}")
		parts.append(("!" if neg else "") + ident)
	return " && ".join(parts)

def conds(items):
	lines = ["pub(super) fn cond(dir: bool, hidden: bool, orphan: bool, link: bool, exec: bool) -> Option<(&'static str, u32)> {"]
	for r in items:
		lines.append(f'\tif {cond_if(r["if"])} {{')
		lines.append(f'\t\treturn Some(("{esc(r["text"])}", {col(r["fg"]):#08x}));')
		lines.append("\t}")
	lines.append("\tNone")
	lines.append("}")
	return "\n".join(lines)

dirs, files, exts, cds = rules("dirs"), rules("files"), rules("exts"), rules("conds")

body = "\n\n".join([
	"// Rules from the user's yazi theme.toml [icon]; regenerate with tools/gen-standard-icons.py, do not hand-edit.",
	array("DIRS", dirs),
	array("FILES", files),
	array("EXTS", exts),
	conds(cds),
])
OUT.write_text("\n" + body + "\n")
print(f"{OUT}: dirs={len(dirs)} files={len(files)} exts={len(exts)} conds={len(cds)}")
