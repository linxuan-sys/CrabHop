
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::config::Config;

const HISTORY_CAP: usize = 200;

#[derive(Default)]
pub(crate) struct Outcome {
	pub code:    Option<i32>,
	pub stdout:  String,
	pub stderr:  String,
}

impl Outcome {
	pub(crate) fn lines(&self) -> Vec<(String, bool)> {
		let mut lines: Vec<(String, bool)> = self.stdout.lines().map(|line| (line.to_owned(), false)).collect();
		if !self.stderr.is_empty() {
			if !lines.is_empty() {
				lines.push((String::new(), false));
			}
			lines.extend(self.stderr.lines().map(|line| (line.to_owned(), true)));
		}

		lines
	}
}

pub(crate) struct Job {
	pub command: String,
	pub started: Instant,
	#[allow(dead_code)]
	pub(crate) cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
	rx:          Receiver<Outcome>,
}

impl Job {
	pub(crate) fn poll(&self) -> Result<Outcome, TryRecvError> {
		self.rx.try_recv()
	}
}

pub(crate) fn spawn(command: String, cwd: PathBuf, cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Job {
	use std::sync::atomic::Ordering;

	let (tx, rx) = std::sync::mpsc::channel();
	let out = command.clone();

	let watch = cancel.clone();
	let check = cancel.clone();
	std::thread::spawn(move || {
		let outcome = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
			Ok(rt) => rt.block_on(async move {
				let child = tokio::process::Command::new("sh")
					.arg("-c")
					.arg(&out)
					.current_dir(&cwd)
					.stdout(std::process::Stdio::piped())
					.stderr(std::process::Stdio::piped())
					.process_group(0)
					.spawn()?;

				if let Some(pid) = child.id() {
					let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
					let inner_watch = watch.clone();
					let inner_done = done.clone();
					std::thread::spawn(move || {
						while !inner_watch.load(Ordering::Relaxed) && !inner_done.load(Ordering::Relaxed) {
							std::thread::sleep(std::time::Duration::from_millis(50));
						}
						if inner_watch.load(Ordering::Relaxed) {
							unsafe { libc::killpg(pid as i32, libc::SIGKILL) };
						}
					});

					let output = child.wait_with_output().await;
					done.store(true, Ordering::Relaxed);
					output
				} else {
					child.wait_with_output().await
				}
			}),
			Err(e) => Err(std::io::Error::other(e)),
		};

		let outcome = match outcome {
			Ok(_) if check.load(Ordering::Relaxed) => Outcome { code: None, stdout: String::new(), stderr: "已取消".to_owned() },
			Ok(output) => Outcome {
				code: Some(output.status.code().unwrap_or(-1)),
				stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
				stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
			},
			Err(e) => Outcome { code: None, stdout: String::new(), stderr: e.to_string() },
		};

		tx.send(outcome).ok();
	});

	Job { command, started: Instant::now(), cancel, rx }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct History {
	pub lines: Vec<String>,
}

impl History {
	pub(crate) fn load() -> Self {
		Config::state("shell-history.toml")
			.and_then(|path| std::fs::read_to_string(path).ok())
			.and_then(|text| toml::from_str(&text).ok())
			.unwrap_or_default()
	}

	pub(crate) fn record(&mut self, line: &str) {
		let line = line.trim();
		if line.is_empty() {
			return;
		}

		self.lines.retain(|known| known != line);
		self.lines.push(line.to_owned());
		if self.lines.len() > HISTORY_CAP {
			self.lines.drain(..self.lines.len() - HISTORY_CAP);
		}
	}

	pub(crate) fn save(&self) {
		let Some(path) = Config::state("shell-history.toml") else { return };
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_cancelled_command_is_killed_and_says_so() {
		use std::sync::atomic::{AtomicBool, Ordering};

		let cancel = std::sync::Arc::new(AtomicBool::new(false));
		let job = spawn("sleep 5".to_owned(), std::env::temp_dir(), cancel.clone());

		std::thread::sleep(std::time::Duration::from_millis(120));
		cancel.store(true, Ordering::Relaxed);

		let outcome = loop {
			match job.poll() {
				Ok(outcome) => break outcome,
				Err(std::sync::mpsc::TryRecvError::Empty) => std::thread::sleep(std::time::Duration::from_millis(30)),
				Err(std::sync::mpsc::TryRecvError::Disconnected) => panic!("the worker left without an answer"),
			}
		};

		assert_eq!(outcome.code, None, "no exit code: it was killed");
		assert!(outcome.stderr.contains("已取消"), "{}", outcome.stderr);
	}

	#[test]
	fn the_history_dedupes_and_caps() {
		let mut history = History::default();
		history.record("ls");
		history.record("pwd");
		history.record("ls");
		assert_eq!(history.lines, vec![ "pwd", "ls" ], "the twin moved to the end, not a new row");

		history.record("   ");
		assert_eq!(history.lines.len(), 2, "a blank line is not a command");

		for i in 0..HISTORY_CAP + 3 {
			history.record(&format!("cmd {i}"));
		}
		assert_eq!(history.lines.len(), HISTORY_CAP);
		assert_eq!(history.lines.last().map(String::as_str), Some("cmd 202"));
	}

	#[test]
	fn an_outcome_reads_as_its_panel() {
		let outcome = Outcome { code: Some(0), stdout: "one\ntwo\n".to_owned(), stderr: "warn\n".to_owned() };
		let lines = outcome.lines();

		assert_eq!(lines[0], ("one".to_owned(), false));
		assert_eq!(lines[1], ("two".to_owned(), false));
		assert!(lines.iter().any(|(line, err)| *err && line == "warn"), "the errors are marked as their own");
	}
}
