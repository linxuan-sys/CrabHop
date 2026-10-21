//! The bulk editor's plan (§4.1): what a column of names means before anything is written — the
//! lines that will be created, the ones the disk already carries, and the ones a later line
//! repeats. The window only draws this; the plan is decided here, where it can be tested.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Kind {
	File,
	Dir,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Fate {
	/// Will be written.
	Create,
	/// Already on disk: skipped, and said so in `err`.
	Exists,
	/// A later line names the same target, and the last one wins: this one is skipped, in `warn`.
	Repeat,
	/// Not a relative name at all: skipped.
	Invalid,
}

#[derive(Clone)]
pub(crate) struct Line {
	/// The line as typed, trimmed — what the preview reads out.
	pub(crate) text:   String,
	/// Where it would land, when it can at all.
	pub(crate) target: Option<PathBuf>,
	pub(crate) kind:   Kind,
	pub(crate) fate:   Fate,
}

/// What the editor's text means, read against the directory it would write into. A line ending in
/// `/` is a directory; a path with parents is fine — the commit makes them. An absolute line is
/// refused: this editor writes inside the current directory, like the keymap says.
pub(crate) fn parse(text: &str, dir: &Path) -> Vec<Line> {
	let mut lines: Vec<Line> = text
		.lines()
		.map(str::trim)
		.filter(|line| !line.is_empty())
		.map(|line| {
			let (name, kind) = match line.strip_suffix('/') {
				Some(stem) if !stem.is_empty() => (stem, Kind::Dir),
				_ => (line, Kind::File),
			};

			let relative = Path::new(name);
			let invalid = name.is_empty() || relative.is_absolute();
			let target = (!invalid).then(|| dir.join(relative));
			let fate = if invalid {
				Fate::Invalid
			} else if target.as_deref().is_some_and(|target| target.symlink_metadata().is_ok()) {
				Fate::Exists
			} else {
				Fate::Create
			};

			Line { text: line.to_owned(), target, kind, fate }
		})
		.collect();

	// the last line naming a target is the one that writes it; the ones before are repeats
	for at in 0..lines.len() {
		let Some(target) = lines[at].target.clone() else { continue };
		if lines[at + 1..].iter().any(|later| later.target.as_deref() == Some(target.as_path())) {
			lines[at].fate = Fate::Repeat;
		}
	}

	lines
}

/// How many lines would be written and how many are skipped — the button's own words, and the
/// only tally either the preview or the commit uses.
pub(crate) fn tally(lines: &[Line]) -> (usize, usize) {
	let create = lines.iter().filter(|line| line.fate == Fate::Create).count();
	(create, lines.len() - create)
}

/// What a commit did: how many landed, and the lines that did not, with the reason.
#[derive(Default)]
pub(crate) struct Report {
	pub(crate) created:  usize,
	pub(crate) failures: Vec<(String, String)>,
}

/// Write the plan: directories with their parents (the upstream semantics), files with their
/// parents made first and the file created new — never overwriting what exists.
pub(crate) fn commit(lines: &[Line]) -> Report {
	let mut report = Report::default();

	for line in lines.iter().filter(|line| line.fate == Fate::Create) {
		let Some(target) = &line.target else { continue };

		let result = match line.kind {
			Kind::Dir => std::fs::create_dir_all(target),
			Kind::File => target
				.parent()
				.map(std::fs::create_dir_all)
				.unwrap_or(Ok(()))
				.and_then(|_| std::fs::OpenOptions::new().write(true).create_new(true).open(target).map(|_| ())),
		};

		match result {
			Ok(()) => report.created += 1,
			Err(e) => report.failures.push((line.text.clone(), e.to_string())),
		}
	}

	report
}

#[cfg(test)]
mod tests {
	use super::*;

	fn scratch(name: &str) -> PathBuf {
		let dir = std::env::temp_dir().join(format!("crabhop-bulk-{name}-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a scratch directory");
		dir
	}

	/// A line's own reading: the slash makes a directory, a line already on disk is a conflict, and
	/// an empty or absolute line is refused.
	#[test]
	fn a_line_reads_as_a_target_and_a_fate() {
		let dir = scratch("read");
		std::fs::write(dir.join("taken.txt"), b"x").expect("a file");

		let lines = parse("docs/\ndocs/readme.md\ntaken.txt\n\n/abs.txt\n", &dir);
		assert_eq!(lines.len(), 4, "the blank line is not one");
		assert_eq!(lines[0].kind, Kind::Dir);
		assert_eq!(lines[0].fate, Fate::Create);
		assert_eq!(lines[1].target.as_deref(), Some(dir.join("docs/readme.md").as_path()));
		assert_eq!(lines[2].fate, Fate::Exists, "the disk has the last word");
		assert_eq!(lines[3].fate, Fate::Invalid, "this editor writes inside the directory");

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// A repeated name keeps its last line: the earlier ones are marked and skipped, the last is
	/// the one the commit would write.
	#[test]
	fn a_repeated_name_keeps_its_last_line() {
		let dir = scratch("repeat");
		let lines = parse("same.txt\nother.txt\nsame.txt", &dir);

		assert_eq!(lines[0].fate, Fate::Repeat);
		assert_eq!(lines[1].fate, Fate::Create);
		assert_eq!(lines[2].fate, Fate::Create, "the last one is the one that writes");
		assert_eq!(tally(&lines), (2, 1));

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// The commit makes parents for both kinds, a new file never overwrites, and every failure
	/// comes back with its line and its reason.
	#[test]
	fn the_commit_writes_the_plan_and_reports_the_rest() {
		let dir = scratch("commit");
		let lines = parse("docs/\nnested/readme.md\n", &dir);
		let report = commit(&lines);

		assert_eq!(report.created, 2);
		assert!(report.failures.is_empty());
		assert!(dir.join("docs").is_dir());
		assert_eq!(std::fs::read(dir.join("nested/readme.md")).expect("the file"), b"");

		// a name that appeared between the plan and the commit is not overwritten: made by hand
		std::fs::write(dir.join("later.txt"), b"keep").expect("a file");
		let lines = parse("later.txt\n", &dir);
		assert_eq!(lines[0].fate, Fate::Exists, "the plan itself refuses it");
		let report = commit(&lines);
		assert_eq!(report.created, 0);
		assert_eq!(std::fs::read(dir.join("later.txt")).expect("still there"), b"keep");

		// a file where a parent should be: the failure is the line's own
		std::fs::write(dir.join("blocker"), b"x").expect("a file");
		let lines = parse("blocker/child.txt\n", &dir);
		let report = commit(&lines);
		assert_eq!(report.failures.len(), 1);
		assert_eq!(report.failures[0].0, "blocker/child.txt");

		let _ = std::fs::remove_dir_all(&dir);
	}
}