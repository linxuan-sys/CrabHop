//! `p` / `P`: what the yank state holds, put down beside the cursor — on a thread of its own, the
//! way every other byte-mover in this window works. A copy walks a directory tree; a cut is a
//! rename, and only falls back to a copy when the destination is on another filesystem.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, TryRecvError};

pub(crate) struct Job<T> {
	rx: Receiver<Result<T, String>>,
}

impl<T> Job<T> {
	/// The answer, if it has landed: `None` means the work is still going.
	pub(crate) fn poll(&self) -> Option<Result<T, String>> {
		match self.rx.try_recv() {
			Ok(result) => Some(result),
			Err(TryRecvError::Empty) => None,
			Err(TryRecvError::Disconnected) => Some(Err("粘贴意外中断".to_owned())),
		}
	}
}

/// What a paste left behind: how many landed, and the (source, destination) pairs in the order
/// they were walked — the undo of a change-type task reads the pairs back (P33⑫).
#[derive(Debug)]
pub(crate) struct Done {
	pub(crate) count:  usize,
	pub(crate) pairs: Vec<(PathBuf, PathBuf)>,
}

/// Put `paths` into `dest`: a cut moves them, a copy leaves them where they are. `force` overwrites
/// a name already standing at the destination; without it, the caller has asked and got its answer.
/// Nothing standing is ever removed before the new bytes have landed under a temporary name: a
/// paste that fails halfway leaves both the source and the old destination exactly as they were.
pub(crate) fn run(paths: Vec<PathBuf>, dest: PathBuf, cut: bool, force: bool) -> Job<Done> {
	let (tx, rx) = std::sync::mpsc::channel();
	std::thread::spawn(move || {
		let mut landed: Vec<(PathBuf, PathBuf)> = Vec::new();
		for path in &paths {
			let Some(name) = path.file_name() else { continue };
			let to = dest.join(name);

			// a name pasted onto itself would eat the very file it carries: refuse, touch nothing
			if same_file(path, &to) {
				tx.send(Err(format!("{}: 来源与目标是同一个", crate::frecency::display(path)))).ok();
				return;
			}
			if to.symlink_metadata().is_ok() && !force {
				tx.send(Err(format!("{} 已存在", crate::frecency::display(&to)))).ok();
				return;
			}
			// a directory in the way cannot be renamed over: with force it goes first, and only it
			if force && to.symlink_metadata().is_ok_and(|m| m.is_dir())
				&& let Err(e) = remove(&to)
			{
				tx.send(Err(format!("{}: {e}", crate::frecency::display(&to)))).ok();
				return;
			}

			let result = if cut {
				// one rename hands the name over, replacing what force allowed; only another
				// filesystem falls back to a copy that then takes the original away
				match std::fs::rename(path, &to) {
					Ok(()) => Ok(()),
					Err(_) => copy(path, &to).and_then(|()| remove(path)),
				}
			} else {
				copy(path, &to)
			};

			if let Err(e) = result {
				tx.send(Err(format!("{}: {e}", crate::frecency::display(path)))).ok();
				return;
			}
			landed.push((path.clone(), to));
		}

		let count = landed.len();
		tx.send(Ok(Done { count, pairs: landed })).ok();
	});

	Job { rx }
}

/// Walk a moved set back where it came from (P33⑫'s undo of a cut-paste): one rename per pair,
/// with the same cross-device fallback a cut paste has. A destination that is already gone is
/// skipped — there is nothing to walk back — and a name standing at the source stops the walk
/// rather than overwriting it.
pub(crate) fn move_back(pairs: Vec<(PathBuf, PathBuf)>) -> Job<Done> {
	let (tx, rx) = std::sync::mpsc::channel();
	std::thread::spawn(move || {
		let mut walked: Vec<(PathBuf, PathBuf)> = Vec::new();
		for (from, to) in &pairs {
			if to.symlink_metadata().is_err() {
				continue;
			}
			if from.symlink_metadata().is_ok() {
				tx.send(Err(format!("{} 已有同名", crate::frecency::display(from)))).ok();
				return;
			}

			let result = std::fs::rename(to, from).or_else(|_| copy(to, from).and_then(|()| remove(to)));
			if let Err(e) = result {
				tx.send(Err(format!("{}: {e}", crate::frecency::display(to)))).ok();
				return;
			}
			walked.push((from.clone(), to.clone()));
		}

		let count = walked.len();
		tx.send(Ok(Done { count, pairs: walked })).ok();
	});

	Job { rx }
}

/// The same file under two spellings (a destination reached through a link, a `.` in the road):
/// only a destination that stands can be compared, an absent one is simply not the source.
fn same_file(path: &Path, to: &Path) -> bool {
	path == to || matches!((std::fs::canonicalize(path), std::fs::canonicalize(to)), (Ok(a), Ok(b)) if a == b)
}

/// A file, a directory with its children, or a link — links go as links. The bytes land under a
/// temporary sibling and one rename hands the name over, so a failure halfway leaves whatever
/// stood at `to` untouched and no half-built tree behind.
fn copy(from: &Path, to: &Path) -> Result<(), String> {
	let meta = std::fs::symlink_metadata(from).map_err(|e| e.to_string())?;
	let tmp = temp_name(to);

	let built = if meta.file_type().is_symlink() {
		let target = std::fs::read_link(from).map_err(|e| e.to_string())?;
		std::os::unix::fs::symlink(target, &tmp).map_err(|e| e.to_string())
	} else if meta.is_dir() {
		std::fs::create_dir_all(&tmp).map_err(|e| e.to_string()).and_then(|()| {
			let entries = std::fs::read_dir(from).map_err(|e| e.to_string())?;
			for entry in entries.flatten() {
				copy_tree(&entry.path(), &tmp.join(entry.file_name()))?;
			}
			Ok(())
		})
	} else {
		std::fs::copy(from, &tmp).map(|_| ()).map_err(|e| e.to_string())
	};

	match built {
		Ok(()) => std::fs::rename(&tmp, to).map_err(|e| {
			let _ = remove(&tmp);
			e.to_string()
		}),
		Err(e) => {
			let _ = remove(&tmp);
			Err(e)
		},
	}
}

/// Straight into a tree that is already ours (the temporary one): no standing names to replace.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
	let meta = std::fs::symlink_metadata(from).map_err(|e| e.to_string())?;

	if meta.file_type().is_symlink() {
		let target = std::fs::read_link(from).map_err(|e| e.to_string())?;
		return std::os::unix::fs::symlink(target, to).map_err(|e| e.to_string());
	}
	if meta.is_dir() {
		std::fs::create_dir_all(to).map_err(|e| e.to_string())?;
		let entries = std::fs::read_dir(from).map_err(|e| e.to_string())?;
		for entry in entries.flatten() {
			copy_tree(&entry.path(), &to.join(entry.file_name()))?;
		}
		return Ok(());
	}

	std::fs::copy(from, to).map(|_| ()).map_err(|e| e.to_string())
}

/// A hidden sibling name nobody holds yet.
fn temp_name(to: &Path) -> PathBuf {
	static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
	let mut name = std::ffi::OsString::from(".duckfm-paste-");
	name.push(to.file_name().unwrap_or_default());
	name.push("-");
	name.push(SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed).to_string());
	to.with_file_name(name)
}

fn remove(path: &Path) -> Result<(), String> {
	let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
	if meta.is_dir() {
		std::fs::remove_dir_all(path).map_err(|e| e.to_string())
	} else {
		std::fs::remove_file(path).map_err(|e| e.to_string())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A copy carries a directory with its children and leaves the original standing; a cut takes
	/// it away — and a name already at the destination is an error unless the caller forced it.
	#[test]
	fn a_copy_leaves_the_original_and_a_cut_does_not() {
		let dir = std::env::temp_dir().join(format!("duckfm-native-paste-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(dir.join("src/sub")).expect("a tree");
		std::fs::write(dir.join("src/sub/a.txt"), b"x").expect("a file");
		std::fs::create_dir_all(dir.join("dst")).expect("a destination");

		let answer = |job: Job<Done>| loop {
			if let Some(answer) = job.poll() {
				break answer;
			}
			std::thread::sleep(std::time::Duration::from_millis(5));
		};

		let copied = answer(run(vec![ dir.join("src") ], dir.join("dst"), false, false));
		assert_eq!(copied.map(|done| done.count), Ok(1), "the copy landed");
		assert!(dir.join("dst/src/sub/a.txt").exists(), "the tree came along");
		assert!(dir.join("src/sub/a.txt").exists(), "and the original stays");

		let refused = answer(run(vec![ dir.join("src") ], dir.join("dst"), false, false));
		assert!(refused.is_err(), "a name already there is refused without force");

		let forced = answer(run(vec![ dir.join("src") ], dir.join("dst"), true, true));
		assert_eq!(forced.map(|done| done.count), Ok(1), "the forced cut landed");
		assert!(!dir.join("src").exists(), "a cut takes the original away");

		// and P33⑫'s undo walks the pieces of a cut back: the pairs are (source, destination)
		std::fs::create_dir_all(dir.join("back")).expect("a second destination");
		let there = answer(run(vec![ dir.join("dst/src") ], dir.join("back"), true, true));
		assert_eq!(there.map(|done| done.count), Ok(1), "a cut into a second destination");
		assert!(dir.join("back/src/sub/a.txt").exists() && !dir.join("dst/src").exists(), "the tree moved again");

		let back = answer(move_back(vec![ (dir.join("dst/src"), dir.join("back/src")) ]));
		assert_eq!(back.map(|done| done.count), Ok(1), "the moved tree walked back");
		assert!(dir.join("dst/src/sub/a.txt").exists(), "to where it came from");
		assert!(!dir.join("back/src").exists(), "and the destination is whole again");

		// a destination that is already gone is skipped; a name standing at the source stops it
		let gone = answer(move_back(vec![ (dir.join("dst/src"), dir.join("nowhere")) ]));
		assert_eq!(gone.map(|done| done.count), Ok(0), "nothing to walk back");
		std::fs::create_dir_all(dir.join("standing")).expect("a name at the source");
		let refused = answer(move_back(vec![ (dir.join("standing"), dir.join("dst/src")) ]));
		assert!(refused.is_err(), "a name standing at the source is not overwritten");

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// The hole the sixth round of acceptance found: a force paste into the directory the source
	/// itself lives in used to remove the source and then fail — the file gone, no trash, nothing.
	/// It refuses now and touches nothing; and a force paste whose copy fails keeps the old
	/// destination standing, with no half-built temporary left behind.
	#[test]
	fn a_force_paste_onto_itself_keeps_the_file_and_a_failed_one_keeps_the_old() {
		let dir = std::env::temp_dir().join(format!("duckfm-native-paste-self-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(dir.join("dst")).expect("a destination");
		std::fs::write(dir.join("a.txt"), b"the source").expect("a source");
		std::fs::write(dir.join("dst/src.txt"), b"the old destination").expect("an old destination");

		let answer = |job: Job<Done>| loop {
			if let Some(answer) = job.poll() {
				break answer;
			}
			std::thread::sleep(std::time::Duration::from_millis(5));
		};

		// cut and copy alike: the name lands on itself, so nothing may happen at all
		for cut in [true, false] {
			let onto_itself = answer(run(vec![ dir.join("a.txt") ], dir.clone(), cut, true));
			assert!(onto_itself.is_err(), "{onto_itself:?}");
			assert_eq!(std::fs::read(dir.join("a.txt")).unwrap(), b"the source", "the source survives its own force paste");
		}

		// an unreadable source makes the copy fail: the old destination must still stand, whole
		std::fs::write(dir.join("src.txt"), b"secret").expect("a source to fail");
		std::fs::set_permissions(dir.join("src.txt"), std::os::unix::fs::PermissionsExt::from_mode(0o000)).expect("sealed");
		let failed = answer(run(vec![ dir.join("src.txt") ], dir.join("dst"), false, true));
		assert!(failed.is_err(), "{failed:?}");
		assert_eq!(std::fs::read(dir.join("dst/src.txt")).unwrap(), b"the old destination", "the old destination survives a failed force paste");
		let leftovers = std::fs::read_dir(dir.join("dst")).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".duckfm-paste-")).count();
		assert_eq!(leftovers, 0, "no temporary tree is left behind");
		std::fs::set_permissions(dir.join("src.txt"), std::os::unix::fs::PermissionsExt::from_mode(0o644)).expect("unsealed");

		let _ = std::fs::remove_dir_all(&dir);
	}
}