
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::complete;
use crate::config::Config;

const HISTORY_CAP: usize = 50;

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Entry {
	pub command: String,
	pub at:      u64,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct History {
	pub entries: Vec<Entry>,
}

impl History {
	pub(crate) fn load() -> Self {
		Config::state("open-history.toml")
			.and_then(|path| std::fs::read_to_string(path).ok())
			.and_then(|text| toml::from_str(&text).ok())
			.unwrap_or_default()
	}

	pub(crate) fn record(&mut self, command: &str) {
		let command = command.trim();
		if command.is_empty() {
			return;
		}

		let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
		self.entries.retain(|entry| entry.command != command);
		self.entries.insert(0, Entry { command: command.to_owned(), at });
		self.entries.truncate(HISTORY_CAP);
	}

	pub(crate) fn save(&self) {
		let Some(path) = Config::state("open-history.toml") else { return };
		if let Some(dir) = path.parent()
			&& Config::ensure(dir).is_err()
		{
			return;
		}

		if let Ok(text) = toml::to_string_pretty(self) {
			let _ = std::fs::write(path, text);
		}
	}
}

pub(crate) fn candidates(line: &str, history: &History, cwd: &Path, cap: usize) -> Vec<String> {
	let from = complete::token_start(line);
	if from > 0 {
		return complete::paths(line, cwd, cap);
	}

	let mut items: Vec<String> = Vec::new();
	for entry in &history.entries {
		if entry.command.starts_with(line) && !items.contains(&entry.command) {
			items.push(entry.command.clone());
		}
	}

	let mut from_path: Vec<String> = on_path_like(line, cap).into_iter().filter(|name| !items.contains(name)).collect();
	from_path.sort();
	items.extend(from_path);
	items.truncate(cap);
	items
}

pub(crate) fn known(command: &str) -> bool {
	let name = command.split_whitespace().next().unwrap_or_default();
	if name.is_empty() {
		return false;
	}
	if name.contains('/') {
		return Path::new(name).is_file();
	}

	std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| executable(&dir.join(name))))
}

fn on_path_like(stem: &str, cap: usize) -> Vec<String> {
	let Some(path) = std::env::var_os("PATH") else { return Vec::new() };
	let mut items: Vec<String> = Vec::new();

	for dir in std::env::split_paths(&path) {
		let Ok(entries) = std::fs::read_dir(dir) else { continue };
		for entry in entries.flatten() {
			let name = entry.file_name().to_string_lossy().into_owned();
			if !name.starts_with(stem) || items.contains(&name) || !executable(&entry.path()) {
				continue;
			}

			items.push(name);
			if items.len() >= cap {
				return items;
			}
		}
	}

	items
}

fn executable(path: &Path) -> bool {
	let Ok(meta) = path.metadata() else { return false };
	if !meta.is_file() {
		return false;
	}

	#[cfg(unix)]
	{
		use std::os::unix::fs::PermissionsExt;
		meta.permissions().mode() & 0o111 != 0
	}
	#[cfg(not(unix))]
	{
		true
	}
}

pub(crate) fn spawn(command: &str, paths: &[PathBuf]) {
	let (command, paths) = (command.to_owned(), paths.to_vec());

	std::thread::spawn(move || {
		let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
		runtime.block_on(async move {
			for path in paths {
				let _ = tokio::process::Command::new("sh")
					.arg("-c")
					.arg(format!("{command} \"$1\""))
					.arg("sh")
					.arg(&path)
					.spawn();
			}
		});
	});
}

pub(crate) fn system_with(tool: &str, path: &Path) -> bool {
	std::process::Command::new(tool)
		.arg(path)
		.stdin(std::process::Stdio::null())
		.stdout(std::process::Stdio::null())
		.stderr(std::process::Stdio::null())
		.spawn()
		.and_then(|mut child| child.wait())
		.is_ok_and(|status| status.success())
}

pub(crate) fn spawn_argv(argv: Vec<String>, cwd: Option<PathBuf>) {
	std::thread::spawn(move || {
		let Some((tool, args)) = argv.split_first() else { return };
		let mut command = std::process::Command::new(tool);
		command
			.args(args)
			.stdin(std::process::Stdio::null())
			.stdout(std::process::Stdio::null())
			.stderr(std::process::Stdio::null());
		if let Some(cwd) = cwd {
			command.current_dir(cwd);
		}

		let _ = command.spawn();
	});
}

pub(crate) fn ask(tool: &str, args: &[&str]) -> Option<String> {
	let out = std::process::Command::new(tool)
		.args(args)
		.stdin(std::process::Stdio::null())
		.stderr(std::process::Stdio::null())
		.output()
		.ok()?;

	out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned()).filter(|line| !line.is_empty())
}

pub(crate) fn system(path: PathBuf, failed: std::sync::mpsc::Sender<PathBuf>) {
	std::thread::spawn(move || {
		if !system_with("xdg-open", &path) {
			let _ = failed.send(path);
		}
	});
}

pub(crate) fn relative(at: u64, now: u64) -> String {
	let ago = now.saturating_sub(at);
	match ago {
		0..=59 => "刚刚".to_owned(),
		60..=3599 => format!("{} 分钟前", ago / 60),
		3600..=86_399 => format!("{} 小时前", ago / 3600),
		_ => format!("{} 天前", ago / 86_400),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_system_open_reports_its_own_failure() {
		assert!(system_with("/bin/true", Path::new("/tmp/crabhop-n2-never/a.zzz")), "a clean exit is an opening");
		assert!(!system_with("/bin/false", Path::new("/tmp/crabhop-n2-never/a.zzz")), "a refusal is a failure");
		assert!(
			!system_with("/tmp/crabhop-n2-never/no-such-opener", Path::new("/tmp/a.zzz")),
			"a launcher that is not there is a failure too"
		);
	}

	#[test]
	fn the_open_history_keeps_the_recency_order() {
		let mut history = History::default();
		history.record("gimp");
		history.record("mpv");
		history.record("gimp");
		assert_eq!(
			history.entries.iter().map(|entry| entry.command.as_str()).collect::<Vec<_>>(),
			vec![ "gimp", "mpv" ],
			"the twin moved to the front, not a new row"
		);
		assert!(history.entries[0].at > 0, "and it carries the moment it ran");
		assert!(history.entries[0].at >= history.entries[1].at, "the front is the most recent");
	}

	#[test]
	fn candidates_are_commands_until_a_blank_says_otherwise() {
		let dir = std::env::temp_dir().join(format!("crabhop-open-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");
		std::fs::write(dir.join("photo.jpg"), b"x").expect("a file");

		let mut history = History::default();
		history.record("gimp");
		history.record("mpv");

		let commands = candidates("gi", &history, &dir, 8);
		assert_eq!(commands.first().map(String::as_str), Some("gimp"), "the history is offered first");

		let paths = candidates("gimp pho", &history, &dir, 8);
		assert_eq!(paths, vec![ "photo.jpg".to_owned() ], "an argument is a path");

		assert!(candidates("definitely-not-a-command-zz", &history, &dir, 8).iter().all(|c| !history.entries.iter().any(|e| e.command == *c)), "no match, no history noise");
		assert!(!known("definitely-not-a-command-zz"));
		assert!(known("sh"), "the shell is on every PATH");
		assert!(known("/bin/sh"), "and an absolute one is checked as it is");

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn the_relative_time_reads_like_a_dropdown() {
		assert_eq!(relative(100, 130), "刚刚");
		assert_eq!(relative(100, 220), "2 分钟前");
		assert_eq!(relative(100, 100 + 3 * 3600), "3 小时前");
		assert_eq!(relative(100, 100 + 3 * 86_400), "3 天前");
		assert_eq!(relative(500, 100), "刚刚", "a clock that went backwards is not a panic");
	}
}
