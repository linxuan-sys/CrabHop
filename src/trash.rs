
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

pub(crate) use trash::TrashItem;

use std::cmp::Reverse;

pub(crate) struct Job<T> {
	rx: Receiver<Result<T, String>>,
}

impl<T> Job<T> {
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

pub(crate) fn delete(paths: Vec<PathBuf>) -> Job<()> {
	spawn(move || trash::delete_all(&paths).map_err(|e| e.to_string()))
}

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

pub(crate) fn list() -> Job<Vec<trash::TrashItem>> {
	spawn(|| {
		let mut items = trash::os_limited::list().map_err(|e| e.to_string())?;
		items.sort_by_key(|item| Reverse(item.time_deleted));
		Ok(items)
	})
}

pub(crate) fn in_trash(item: &TrashItem) -> Option<PathBuf> {
	let info = Path::new(&item.id);
	let root = info.parent()?.parent()?;

	Some(root.join("files").join(info.file_stem()?))
}

pub(crate) fn is_dir(item: &TrashItem) -> bool {
	in_trash(item).is_some_and(|path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()))
}

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

pub(crate) fn taken(path: &Path) -> bool {
	std::fs::symlink_metadata(path).is_ok()
}

fn numbered(path: &Path, n: usize) -> PathBuf {
	let stem = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
	let count = match path.extension() {
		Some(ext) => format!("{stem} ({n}).{}", ext.to_string_lossy()),
		None => format!("{stem} ({n})"),
	};

	path.with_file_name(count)
}

pub(crate) fn restore_as(item: &TrashItem, to: PathBuf) -> Job<()> {
	let item = item.clone();
	spawn(move || {
		let Some(from) = in_trash(&item) else {
			return Err("回收站里找不到这一项的内容".to_owned());
		};

		std::fs::rename(&from, &to).map_err(|e| format!("{}: {e}", crate::frecency::display(&to)))?;

		std::fs::remove_file(Path::new(&item.id)).map_err(|e| e.to_string())
	})
}

pub(crate) fn purge(items: Vec<trash::TrashItem>) -> Job<()> {
	spawn(move || trash::os_limited::purge_all(items).map_err(|e| e.to_string()))
}

pub(crate) fn empty() -> Job<()> {
	spawn(|| {
		let items = trash::os_limited::list().map_err(|e| e.to_string())?;
		trash::os_limited::purge_all(items).map_err(|e| e.to_string())
	})
}

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

	fn item(name: &str) -> TrashItem {
		let root = PathBuf::from("/home/x/.local/share/Trash");
		TrashItem {
			id:              root.join("info").join(format!("{name}.trashinfo")).into_os_string(),
			name:            name.into(),
			original_parent: PathBuf::from("/home/x/work"),
			time_deleted:    1_700_000_000,
		}
	}

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

	#[test]
	fn a_numbered_name_keeps_the_stem_and_the_extension_apart() {
		assert_eq!(numbered(Path::new("/a/b.txt"), 1), PathBuf::from("/a/b (1).txt"));
		assert_eq!(numbered(Path::new("/a/b.txt"), 12), PathBuf::from("/a/b (12).txt"));
		assert_eq!(numbered(Path::new("/a/noext"), 1), PathBuf::from("/a/noext (1)"));
		assert_eq!(numbered(Path::new("/a/archive.tar.gz"), 2), PathBuf::from("/a/archive.tar (2).gz"));
		assert_eq!(numbered(Path::new("/a/.bashrc"), 1), PathBuf::from("/a/.bashrc (1)"));
	}

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

		let dangling = dir.join("d.txt");
		std::os::unix::fs::symlink(dir.join("nowhere"), &dangling).expect("a link");
		assert_eq!(free_beside(&dangling), dir.join("d (1).txt"), "a dangling link is a collision too");

		assert_eq!(free_beside(&dir.join("fresh.txt")), dir.join("fresh.txt"), "a free path is itself");

		let _ = std::fs::remove_dir_all(&dir);
	}

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
