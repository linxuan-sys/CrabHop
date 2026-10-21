//! The bookmarks (§5.1–§5.3), natively — whoosh's job without a plugin: a key, a name and a
//! path, in `$XDG_DATA_HOME/crabhop/bookmarks.toml`. A key is assigned automatically and can be
//! changed by hand in the file; a temp bookmark lives only in this run.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Bookmark {
	/// The entry's own name: what the index panel lists, renames and deletes by (P14's
	/// re-decision — no more keys, no more temp marks).
	pub name: String,
	pub path: PathBuf,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Bookmarks {
	pub entries: Vec<Bookmark>,
}

impl Bookmarks {
	pub(crate) fn load() -> Self {
		Config::state("bookmarks.toml")
			.and_then(|path| std::fs::read_to_string(path).ok())
			.and_then(|text| toml::from_str(&text).ok())
			.unwrap_or_default()
	}

	pub(crate) fn save(&self) {
		let Some(path) = Config::state("bookmarks.toml") else { return };
		if let Some(dir) = path.parent()
			&& Config::ensure(dir).is_err()
		{
			return;
		}

		if let Ok(text) = toml::to_string_pretty(&Self { entries: self.entries.clone() }) {
			let _ = std::fs::write(path, text);
		}
	}

	/// Index a path (P14's re-decision): its own name is the entry's identity, and a path
	/// already indexed is returned as it stands rather than added twice.
	pub(crate) fn add(&mut self, path: PathBuf) -> Bookmark {
		if let Some(known) = self.entries.iter().find(|entry| entry.path == path) {
			return known.clone();
		}

		let bookmark = Bookmark { name: name_of(&path), path };

		self.entries.push(bookmark.clone());
		bookmark
	}

	/// An undo of a delete puts the bookmark back as it was.
	pub(crate) fn put(&mut self, bookmark: Bookmark) {
		self.entries.push(bookmark);
	}

	pub(crate) fn remove(&mut self, name: &str) -> Option<Bookmark> {
		let at = self.entries.iter().position(|entry| entry.name == name)?;
		Some(self.entries.remove(at))
	}

	pub(crate) fn get(&self, name: &str) -> Option<&Bookmark> {
		self.entries.iter().find(|entry| entry.name == name)
	}

}

/// What a bookmark is called until someone renames it: the item's own name, or the path itself
/// when it has no name to take.
pub(crate) fn name_of(path: &Path) -> String {
	path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Indexing is by path: the entry takes the item's own name, an old entry comes back when the
	/// path is already indexed (never a duplicate), and removal and rename work by that name.
	#[test]
	fn indexing_is_by_path_and_identity_is_the_name() {
		let mut books = Bookmarks::default();
		let first = books.add(PathBuf::from("/home/x/dev"));
		let second = books.add(PathBuf::from("/home/x/Downloads"));
		assert_eq!(first.name, "dev");
		assert_eq!(second.name, "Downloads");

		let again = books.add(PathBuf::from("/home/x/dev"));
		assert_eq!(again.name, "dev");
		assert_eq!(books.entries.len(), 2, "a path is never indexed twice");

		assert_eq!(books.get("dev").map(|entry| entry.path.as_path()), Some(Path::new("/home/x/dev")), "the index knows its name");
		assert!(books.get("gone").is_none(), "and a name nobody gave it is not there");

		let removed = books.remove("dev").expect("the entry");
		assert_eq!(removed.path, PathBuf::from("/home/x/dev"));
		assert_eq!(books.entries.len(), 1);
	}

	/// The file round trips without any keys: what is saved is what is read back.
	#[test]
	fn the_file_round_trips() {
		let mut books = Bookmarks::default();
		books.add(PathBuf::from("/home/x/dev"));
		books.add(PathBuf::from("/tmp"));

		let text = toml::to_string_pretty(&books).expect("writable");
		let back: Bookmarks = toml::from_str(&text).expect("readable");
		assert_eq!(back.entries.len(), 2);
		assert_eq!(back.entries[1].path, PathBuf::from("/tmp"));
		assert_eq!(back.entries[1].name, "tmp");
	}

	/// A path with no name of its own reads as the path itself.
	#[test]
	fn a_name_comes_from_the_item_or_the_path() {
		assert_eq!(name_of(Path::new("/home/x/dev")), "dev");
		assert_eq!(name_of(Path::new("/")), "/");
	}
}
