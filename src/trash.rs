//! The FreeDesktop recycler (the keymap's `d` / `D` / `R b`), through the `trash` crate: the
//! system's own bin, not a private one. Every call here moves real bytes about, so each runs on
//! a thread of its own and the window reads the answer when it lands.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

/// The bin's own item, re-exported so the window can name what the view holds without the crate
/// and this module fighting over the same noun.
pub(crate) use trash::TrashItem;

/// The newest first is the order both the undo and the view want to read a bin in.
use std::cmp::Reverse;

/// A trash operation running off this thread: listing a bin touches every info file in it, and
/// the window is not going to stand still for that.
pub(crate) struct Job<T> {
	rx: Receiver<Result<T, String>>,
}

impl<T> Job<T> {
	/// The answer, if it has landed: `None` means the work is still going.
	pub(crate) fn poll(&self) -> Option<Result<T, String>> {
		match self.rx.try_recv() {
			Ok(result) => Some(result),
			Err(TryRecvError::Empty) => None,
			Err(TryRecvError::Disconnected) => Some(Err("垃圾桶操作意外中断".to_owned())),
		}
	}
}

fn spawn<T: Send + 'static>(run: impl FnOnce() -> Result<T, String> + Send + 'static) -> Job<T> {
	let (tx, rx) = std::sync::mpsc::channel();
	std::thread::spawn(move || {
		tx.send(run()).ok();
	});
	Job { rx }
}

/// `d`: into the system bin — links are removed as links, and their targets stay.
pub(crate) fn delete(paths: Vec<PathBuf>) -> Job<()> {
	spawn(move || trash::delete_all(&paths).map_err(|e| e.to_string()))
}

/// The undo of a delete: the newest trashed twin of each original goes back where it came from.
pub(crate) fn restore(originals: Vec<PathBuf>) -> Job<()> {
	spawn(move || {
		let items = trash::os_limited::list().map_err(|e| e.to_string())?;
		let mut wanted: Vec<trash::TrashItem> = items
			.into_iter()
			.filter(|item| originals.contains(&item.original_path()))
			.collect();

		wanted.sort_by_key(|item| Reverse(item.time_deleted));
		wanted.dedup_by(|a, b| a.original_path() == b.original_path());
		if wanted.is_empty() {
			return Err("回收站里没有这些条目了".to_owned());
		}

		trash::os_limited::restore_all(wanted).map_err(|e| e.to_string())
	})
}

/// `R b`: the bin's own list, newest first.
pub(crate) fn list() -> Job<Vec<trash::TrashItem>> {
	spawn(|| {
		let mut items = trash::os_limited::list().map_err(|e| e.to_string())?;
		items.sort_by_key(|item| Reverse(item.time_deleted));
		Ok(items)
	})
}

/// Where an item's bytes actually sit: the freedesktop spec keeps them beside the info file, under
/// the bin's own `files/`, and the crate reads the bin the same way — the info file's stem is the
/// name in there. `None` for an item whose id is not a path under a bin. Pure, so the rule can be
/// read without a bin (P19's preview needs the real path).
pub(crate) fn in_trash(item: &TrashItem) -> Option<PathBuf> {
	let info = Path::new(&item.id);
	let root = info.parent()?.parent()?;

	Some(root.join("files").join(info.file_stem()?))
}

/// Whether an item is a directory — read from the bin's own copy, for the icon its row wears
/// (P19 reads it once when the list lands, never in a draw).
pub(crate) fn is_dir(item: &TrashItem) -> bool {
	in_trash(item).is_some_and(|path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()))
}

/// A name beside `path` that nothing stands at: `b (1).txt`, `b (2).txt`, … — the way every file
/// manager spells a second copy. A path that is free is itself.
pub(crate) fn free_beside(path: &Path) -> PathBuf {
	if !taken(path) {
		return path.to_owned();
	}

	for n in 1..=9999 {
		let candidate = numbered(path, n);
		if !taken(&candidate) {
			return candidate;
		}
	}

	numbered(path, 10_000)
}

/// Whether anything stands at the path at all — a file, a directory, or a link whose target is
/// gone (the bin's own restore refuses a dangling link too, so a collision it is).
pub(crate) fn taken(path: &Path) -> bool {
	std::fs::symlink_metadata(path).is_ok()
}

/// The n-th name beside `path`: the stem, the counter in brackets, and the extension kept.
fn numbered(path: &Path, n: usize) -> PathBuf {
	let stem = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
	let count = match path.extension() {
		Some(ext) => format!("{stem} ({n}).{}", ext.to_string_lossy()),
		None => format!("{stem} ({n})"),
	};

	path.with_file_name(count)
}

/// `Enter` on a name the original directory already holds: the bytes go back under the free name
/// the caller worked out, and the bin's own record is purged — nothing standing at the original
/// path is touched or overwritten (P19).
pub(crate) fn restore_as(item: &TrashItem, to: PathBuf) -> Job<()> {
	let item = item.clone();
	spawn(move || {
		let Some(from) = in_trash(&item) else {
			return Err("回收站里找不到这一项的内容".to_owned());
		};

		std::fs::rename(&from, &to).map_err(|e| format!("{}: {e}", crate::frecency::display(&to)))?;

		// the bin's record goes with it: the bytes are out, and the info file is what lists an
		// item — purging after a rename would look for bytes that are no longer there
		std::fs::remove_file(Path::new(&item.id)).map_err(|e| e.to_string())
	})
}

/// `D` inside the view: out of the bin for good.
pub(crate) fn purge(items: Vec<trash::TrashItem>) -> Job<()> {
	spawn(move || trash::os_limited::purge_all(items).map_err(|e| e.to_string()))
}

/// Emptying the bin is purging everything in it.
pub(crate) fn empty() -> Job<()> {
	spawn(|| {
		let items = trash::os_limited::list().map_err(|e| e.to_string())?;
		trash::os_limited::purge_all(items).map_err(|e| e.to_string())
	})
}

/// `D`: the permanent delete, made with the filesystem's own hand — `remove_dir_all` for a
/// directory, and the file for a file. Links go as links.
pub(crate) fn remove(paths: Vec<PathBuf>) -> Job<()> {
	spawn(move || {
		for path in &paths {
			let result = match std::fs::symlink_metadata(path) {
				Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
				Ok(_) => std::fs::remove_file(path),
				Err(e) => Err(e),
			};

			if let Err(e) = result {
				return Err(format!("{}: {e}", crate::frecency::display(path)));
			}
		}

		Ok(())
	})
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use super::*;

	/// The bin's own item, as the crate hands it over: an id (the info file), a name, where it came
	/// from and when it went.
	fn item(name: &str) -> TrashItem {
		let root = PathBuf::from("/home/x/.local/share/Trash");
		TrashItem {
			id:              root.join("info").join(format!("{name}.trashinfo")).into_os_string(),
			name:            name.into(),
			original_parent: PathBuf::from("/home/x/work"),
			time_deleted:    1_700_000_000,
		}
	}

	/// An item's bytes are found the way the crate finds them: the info file's stem, under the
	/// bin's own `files/` (P19 — the preview column reads the real path).
	#[test]
	fn an_items_bytes_are_found_by_its_info_file() {
		assert_eq!(
			in_trash(&item("a.txt")),
			Some(PathBuf::from("/home/x/.local/share/Trash/files/a.txt")),
			"the stem keeps the extension out of it"
		);
		assert_eq!(in_trash(&item("a.tar.gz")), Some(PathBuf::from("/home/x/.local/share/Trash/files/a.tar.gz")));
		assert_eq!(in_trash(&item("没有扩展名")), Some(PathBuf::from("/home/x/.local/share/Trash/files/没有扩展名")));

		let mut odd = item("a.txt");
		odd.id = "x".into();
		assert_eq!(in_trash(&odd), None, "an id that is not a path has no bytes to find");
	}

	/// The name a conflicting restore takes: the stem, the counter in brackets, the extension kept
	/// — including the dotless and the hidden-name cases.
	#[test]
	fn a_numbered_name_keeps_the_stem_and_the_extension_apart() {
		assert_eq!(numbered(Path::new("/a/b.txt"), 1), PathBuf::from("/a/b (1).txt"));
		assert_eq!(numbered(Path::new("/a/b.txt"), 12), PathBuf::from("/a/b (12).txt"));
		assert_eq!(numbered(Path::new("/a/noext"), 1), PathBuf::from("/a/noext (1)"));
		assert_eq!(numbered(Path::new("/a/archive.tar.gz"), 2), PathBuf::from("/a/archive.tar (2).gz"));
		assert_eq!(numbered(Path::new("/a/.bashrc"), 1), PathBuf::from("/a/.bashrc (1)"));
	}

	/// A free name beside a taken one: the first counter that nothing stands at — and a path that
	/// is already free is itself.
	#[test]
	fn a_free_name_is_the_first_one_nothing_stands_at() {
		let dir = std::env::temp_dir().join(format!("crabhop-restore-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");

		let taken = dir.join("b.txt");
		std::fs::write(&taken, b"x").expect("a file");
		assert_eq!(free_beside(&taken), dir.join("b (1).txt"), "the first counter beside it");

		std::fs::write(dir.join("b (1).txt"), b"x").expect("a file");
		std::fs::write(dir.join("b (2).txt"), b"x").expect("a file");
		assert_eq!(free_beside(&taken), dir.join("b (3).txt"), "and it walks until one is free");

		// a link whose target is gone still stands there: the restore must not clobber its name
		let dangling = dir.join("d.txt");
		std::os::unix::fs::symlink(dir.join("nowhere"), &dangling).expect("a link");
		assert_eq!(free_beside(&dangling), dir.join("d (1).txt"), "a dangling link is a collision too");

		assert_eq!(free_beside(&dir.join("fresh.txt")), dir.join("fresh.txt"), "a free path is itself");

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// A permanent delete takes the file, a directory with its children, and stops at the first
	/// thing it cannot take — the error names the path a person typed.
	#[test]
	fn a_permanent_delete_takes_files_and_directories() {
		let dir = std::env::temp_dir().join(format!("crabhop-trash-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(dir.join("deep/inside")).expect("a directory");
		std::fs::write(dir.join("deep/inside/file.txt"), b"x").expect("a file");
		std::fs::write(dir.join("lonely.txt"), b"x").expect("a file");

		let job = remove(vec![ dir.join("deep"), dir.join("lonely.txt") ]);
		let answer = loop {
			if let Some(answer) = job.poll() {
				break answer;
			}
			std::thread::sleep(std::time::Duration::from_millis(5));
		};

		assert!(answer.is_ok(), "{answer:?}");
		assert!(!dir.join("deep").exists());
		assert!(!dir.join("lonely.txt").exists());

		let job = remove(vec![ dir.join("not-there") ]);
		let answer = loop {
			if let Some(answer) = job.poll() {
				break answer;
			}
			std::thread::sleep(std::time::Duration::from_millis(5));
		};
		assert!(answer.is_err(), "a path that is not there is an error to report");

		let _ = std::fs::remove_dir_all(&dir);
	}
}