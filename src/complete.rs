//! The little completion the command line and open-with share: the token at the end of a line,
//! its candidates, and which one is standing. The candidates themselves are read from the disk
//! for paths, and from the history and `$PATH` for commands (in `open.rs`).

use std::path::Path;

use crate::edit::Edit;

/// A candidate list and the seat the active one holds.
pub(crate) struct Completions {
	/// The byte offset the candidates replace: everything from here to the end of the line.
	pub from:  usize,
	pub items: Vec<String>,
	pub at:    usize,
}

impl Completions {
	pub(crate) fn new(from: usize, items: Vec<String>) -> Option<Self> {
		(!items.is_empty()).then_some(Self { from, items, at: 0 })
	}

	pub(crate) fn move_by(&mut self, step: isize) {
		let len = self.items.len() as isize;
		self.at = (self.at as isize + step).rem_euclid(len) as usize;
	}

	/// The active candidate written into the line, and the caret at its end.
	pub(crate) fn apply(&self, edit: &mut Edit) {
		let Some(item) = self.items.get(self.at) else { return };
		edit.text.truncate(self.from.min(edit.text.len()));
		edit.text.push_str(item);
		edit.cursor = edit.text.len();
	}
}

/// Where the token a completion replaces begins: after the last blank of the line.
pub(crate) fn token_start(line: &str) -> usize {
	line.rfind(char::is_whitespace).map_or(0, |at| at + 1)
}

/// Path candidates for a line's last token, read from the disk under `cwd`: the entries of the
/// token's own directory whose names carry on what was typed, directories marked with their
/// slash. A token that is already a complete name still lists the names under it.
pub(crate) fn paths(line: &str, cwd: &Path, cap: usize) -> Vec<String> {
	let token = &line[token_start(line)..];
	let (prefix, dir, stem) = match token.rfind('/') {
		Some(at) => (&token[..=at], cwd.join(&token[..at]), &token[at + 1..]),
		None => ("", cwd.to_owned(), token),
	};

	let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
	let mut items: Vec<String> = entries
		.flatten()
		.filter_map(|entry| {
			let name = entry.file_name().to_string_lossy().into_owned();
			if !name.starts_with(stem) {
				return None;
			}

			let dir = entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false);
			Some(format!("{prefix}{name}{}", if dir { "/" } else { "" }))
		})
		.take(cap)
		.collect();

	items.sort();
	items
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::PathBuf;

	/// The token is what follows the last blank, and applying a candidate rewrites exactly it.
	#[test]
	fn a_completion_replaces_the_last_token_only() {
		assert_eq!(token_start("cat doc"), 4);
		assert_eq!(token_start("doc"), 0);
		assert_eq!(token_start("cat "), 4, "an empty token is a token");

		let mut edit = Edit::new("cat doc".to_owned());
		let completion = Completions::new(token_start("cat doc"), vec![ "docs/".to_owned(), "docker".to_owned() ])
			.expect("two candidates");
		completion.apply(&mut edit);
		assert_eq!((edit.text.as_str(), edit.cursor), ("cat docs/", 9));

		let mut completion = completion;
		completion.move_by(1);
		completion.apply(&mut edit);
		assert_eq!(edit.text, "cat docker");
		completion.move_by(1);
		assert_eq!(completion.at, 0, "the ring wraps");
	}

	/// Path candidates come off the disk: the prefix that was typed stays, a directory keeps its
	/// slash, and only what carries on the name is listed.
	#[test]
	fn path_candidates_read_the_directory_the_token_points_at() {
		let dir = std::env::temp_dir().join(format!("crabhop-complete-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(dir.join("docs/deep")).expect("a directory");
		std::fs::write(dir.join("doc.txt"), b"x").expect("a file");
		std::fs::write(dir.join("other.txt"), b"x").expect("a file");

		let items = paths("ls doc", &dir, 8);
		assert_eq!(items, vec![ "doc.txt".to_owned(), "docs/".to_owned() ], "sorted, and the directory marked");

		let items = paths("ls docs/", &dir, 8);
		assert_eq!(items, vec![ "docs/deep/".to_owned() ], "the typed prefix stays in every candidate");

		assert!(paths("ls zzz", &dir, 8).is_empty());
		assert_eq!(paths("ls doc", &PathBuf::from("/definitely/not/here"), 8), Vec::<String>::new());
		assert!(Completions::new(0, Vec::new()).is_none(), "no candidates, no list");

		let _ = std::fs::remove_dir_all(&dir);
	}
}