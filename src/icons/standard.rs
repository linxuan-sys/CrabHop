
// Rules from the user's yazi theme.toml [icon]; regenerate with tools/gen-standard-icons.py, do not hand-edit.

pub(super) const DIRS: &[(&str, &str, u32)] = &[
	("Desktop", "\u{f108}", 0x99ccfa),
	("Documents", "\u{f0219}", 0x99ccfa),
	("Downloads", "\u{f01da}", 0x99ccfa),
	("Pictures", "\u{f024f}", 0x99ccfa),
	("Music", "\u{f0386}", 0x99ccfa),
	("Videos", "\u{f0567}", 0x99ccfa),
	("Public", "\u{f0c0}", 0x99ccfa),
	(".config", "\u{e5fc}", 0xb8c8da),
	(".git", "\u{f02a2}", 0xb8c8da),
	("Github", "\u{f02a2}", 0xb8c8da),
	("src", "\u{f121}", 0xb8c8da),
];

pub(super) const FILES: &[(&str, &str, u32)] = &[
	("PKGBUILD", "\u{f303}", 0x99ccfa),
	("README.md", "\u{f00ba}", 0xb8c8da),
	("Makefile", "\u{e779}", 0xb8c8da),
	(".bashrc", "\u{e615}", 0xb8c8da),
	(".zshrc", "\u{e615}", 0xb8c8da),
];

pub(super) const EXTS: &[(&str, &str, u32)] = &[
	("png", "\u{f024f}", 0xd1bfe7),
	("jpg", "\u{f024f}", 0xd1bfe7),
	("jpeg", "\u{f024f}", 0xd1bfe7),
	("webp", "\u{f024f}", 0xd1bfe7),
	("svg", "\u{f0721}", 0xd1bfe7),
	("mp4", "\u{f0567}", 0xd1bfe7),
	("mkv", "\u{f0567}", 0xd1bfe7),
	("mp3", "\u{f0386}", 0xd1bfe7),
	("flac", "\u{f0386}", 0xd1bfe7),
	("zip", "\u{f06eb}", 0xd1bfe7),
	("tar", "\u{f06eb}", 0xd1bfe7),
	("gz", "\u{f06eb}", 0xd1bfe7),
	("rar", "\u{f06eb}", 0xd1bfe7),
	("7z", "\u{f06eb}", 0xd1bfe7),
	("conf", "\u{e615}", 0x99ccfa),
	("toml", "\u{e615}", 0x99ccfa),
	("yaml", "\u{e615}", 0x99ccfa),
	("yml", "\u{e615}", 0x99ccfa),
	("json", "\u{e60b}", 0x99ccfa),
	("sh", "\u{e795}", 0x99ccfa),
	("bash", "\u{e795}", 0x99ccfa),
	("py", "\u{e606}", 0x99ccfa),
	("rs", "\u{e68b}", 0x99ccfa),
	("c", "\u{e61e}", 0x99ccfa),
	("cpp", "\u{e61d}", 0x99ccfa),
	("md", "\u{f48a}", 0x99ccfa),
	("txt", "\u{f0219}", 0xe0e2e8),
];

pub(super) fn cond(dir: bool, hidden: bool, orphan: bool, link: bool, exec: bool) -> Option<(&'static str, u32)> {
	if dir && hidden {
		return Some(("\u{f115}", 0xfafdff));
	}
	if dir {
		return Some(("\u{f115}", 0x99ccfa));
	}
	if orphan {
		return Some(("\u{f030c}", 0xffb4ab));
	}
	if link {
		return Some(("\u{f0337}", 0xeddcff));
	}
	if exec && !dir {
		return Some(("\u{f01a7}", 0xb8c8da));
	}
	if hidden {
		return Some(("\u{f0219}", 0xc2c7ce));
	}
	None
}
