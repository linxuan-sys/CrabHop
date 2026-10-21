
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};

use grep_regex::RegexMatcher;
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::WalkBuilder;

pub(crate) const CAP: usize = 4000;
const BATCH: usize = 64;

#[derive(Clone, Debug)]
pub(crate) struct Hit {
	pub path: PathBuf,
	pub line: Option<u64>,
	pub text: Option<String>,
}

pub(crate) struct Job {
	cancel: Arc<AtomicBool>,
	pub rx: Receiver<Vec<Hit>>,
}

impl Job {
	pub(crate) fn cancel(&self) {
		self.cancel.store(true, Ordering::Relaxed);
	}
}

fn spawn(run: impl FnOnce(&AtomicBool, &Sender<Vec<Hit>>) + Send + 'static) -> Job {
	let cancel = Arc::new(AtomicBool::new(false));
	let (tx, rx) = std::sync::mpsc::channel();
	let flag = cancel.clone();

	std::thread::spawn(move || run(&flag, &tx));
	Job { cancel, rx }
}

pub(crate) fn names(root: &Path, query: &str, cap: usize) -> Job {
	let (root, query) = (root.to_owned(), query.to_owned());

	spawn(move |cancel, tx| {
		let exact = query.chars().any(char::is_uppercase);
		let needle = if exact { query.clone() } else { query.to_lowercase() };
		let (mut batch, mut sent) = (Vec::new(), 0usize);

		let walker = WalkBuilder::new(&root).build();
		for entry in walker.flatten() {
			if cancel.load(Ordering::Relaxed) || sent >= cap {
				break;
			}

			let name = entry.file_name().to_string_lossy();
			let haystack = if exact { name.to_string() } else { name.to_lowercase() };
			if !haystack.contains(&needle) {
				continue;
			}

			batch.push(Hit { path: entry.path().to_owned(), line: None, text: None });
			sent += 1;
			if batch.len() >= BATCH {
				tx.send(std::mem::take(&mut batch)).ok();
			}
		}

		if !batch.is_empty() {
			tx.send(batch).ok();
		}
	})
}

pub(crate) fn contents(root: &Path, query: &str, cap: usize) -> Job {
	let (root, pattern) = (root.to_owned(), query.to_owned());

	spawn(move |cancel, tx| {
		let Ok(matcher) = RegexMatcher::new(&pattern) else { return };
		let mut searcher = SearcherBuilder::new().line_number(true).build();
		let (mut batch, mut sent) = (Vec::new(), 0usize);

		let walker = WalkBuilder::new(&root).build();
		for entry in walker.flatten() {
			if cancel.load(Ordering::Relaxed) || sent >= cap {
				break;
			}
			if entry.file_type().is_some_and(|kind| !kind.is_file()) {
				continue;
			}

			let path = entry.path().to_owned();
			let mut hits = Vec::new();
			let sink = Collect { path: &path, hits: &mut hits, remaining: cap - sent };
			if searcher.search_path(&matcher, &path, sink).is_err() {
				continue;
			}

			sent += hits.len();
			batch.append(&mut hits);
			if batch.len() >= BATCH {
				tx.send(std::mem::take(&mut batch)).ok();
			}
		}

		if !batch.is_empty() {
			tx.send(batch).ok();
		}
	})
}

struct Collect<'a> {
	path:      &'a Path,
	hits:      &'a mut Vec<Hit>,
	remaining: usize,
}

impl Sink for Collect<'_> {
	type Error = std::io::Error;

	fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, Self::Error> {
		let text = String::from_utf8_lossy(mat.bytes()).trim_end().to_owned();
		self.hits.push(Hit { path: self.path.to_owned(), line: mat.line_number(), text: Some(text) });
		self.remaining = self.remaining.saturating_sub(1);
		Ok(self.remaining > 0)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn scratch(name: &str) -> PathBuf {
		let dir = std::env::temp_dir().join(format!("crabhop-search-{name}-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(dir.join("docs")).expect("a directory");
		std::fs::write(dir.join("docs/readme.md"), b"# hello\n").expect("a file");
		std::fs::write(dir.join("alphabet.txt"), b"first\nsecond needle\nthird\n").expect("a file");
		std::fs::write(dir.join(".hidden-needle"), b"x").expect("a hidden file");
		std::fs::write(dir.join("NeedleCaps.txt"), b"x").expect("a file");
		dir
	}

	fn collect(job: &Job) -> Vec<Hit> {
		let mut hits = Vec::new();
		while let Ok(batch) = job.rx.recv() {
			hits.extend(batch);
		}
		hits
	}

	#[test]
	fn a_name_search_finds_by_substring_with_smart_case() {
		let dir = scratch("names");

		let hits = collect(&names(&dir, "read", 100));
		assert_eq!(hits.len(), 1);
		assert!(hits[0].path.ends_with("docs/readme.md"));

		let hits = collect(&names(&dir, "Needle", 100));
		assert_eq!(hits.len(), 1, "a capital query answers exactly");
		assert!(hits[0].path.ends_with("NeedleCaps.txt"));

		let hits = collect(&names(&dir, "needle", 100));
		assert_eq!(hits.len(), 1, "a lowercase one widely");
		assert!(
			!hits.iter().any(|hit| hit.path.to_string_lossy().contains(".hidden")),
			"but the dotfile stays out, the way fd does it: {hits:?}"
		);

		assert!(collect(&names(&dir, "nothing-here", 100)).is_empty());

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn a_content_search_brings_lines_and_numbers() {
		let dir = scratch("lines");

		let hits = collect(&contents(&dir, "needle", 100));
		assert_eq!(hits.len(), 1);
		assert!(hits[0].path.ends_with("alphabet.txt"));
		assert_eq!(hits[0].line, Some(2));
		assert_eq!(hits[0].text.as_deref(), Some("second needle"));

		let _ = collect(&contents(&dir, "first\nsecond", 100));

		assert!(collect(&contents(&dir, "([unclosed", 100)).is_empty(), "an unparseable pattern is quiet");

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn the_cap_is_where_a_search_stops() {
		let dir = scratch("cap");
		for at in 0..10 {
			std::fs::write(dir.join(format!("many-{at}.txt")), b"x\n").expect("a file");
		}

		let hits = collect(&names(&dir, "many", 4));
		assert_eq!(hits.len(), 4, "the cap is the cap");

		let _ = std::fs::remove_dir_all(&dir);
	}
}
