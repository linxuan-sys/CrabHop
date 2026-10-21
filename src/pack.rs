
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Format {
	Zip,
	Tar,
	TarZstd,
}

pub(crate) fn format_of(name: &str) -> Format {
	let lower = name.to_lowercase();
	if lower.ends_with(".tar.zst") || lower.ends_with(".tzst") {
		Format::TarZstd
	} else if lower.ends_with(".tar") {
		Format::Tar
	} else {
		Format::Zip
	}
}

pub(crate) fn archive_name(directory: &str, typed: &str) -> String {
	let typed = typed.trim();
	if typed.is_empty() {
		return format!("{directory}.zip");
	}
	if format_of(typed) == Format::Zip && !typed.to_lowercase().ends_with(".zip") {
		return format!("{typed}.zip");
	}

	typed.to_owned()
}

pub(crate) struct Job {
	pub name: String,
	done:     Arc<AtomicUsize>,
	total:    Arc<AtomicUsize>,
	rx:       Receiver<Result<usize, String>>,
}

impl Job {
	pub(crate) fn progress(&self) -> (usize, usize) {
		(self.done.load(Ordering::Relaxed), self.total.load(Ordering::Relaxed))
	}

	pub(crate) fn poll(&self) -> Option<Result<usize, String>> {
		match self.rx.try_recv() {
			Ok(result) => Some(result),
			Err(std::sync::mpsc::TryRecvError::Empty) => None,
			Err(std::sync::mpsc::TryRecvError::Disconnected) => Some(Err("压缩意外中断".to_owned())),
		}
	}
}

fn entry_name(path: &Path, cwd: &Path) -> String {
	path.strip_prefix(cwd).unwrap_or(path).to_string_lossy().into_owned()
}

fn walk(root: &Path, files: &mut Vec<PathBuf>, dirs: &mut Vec<PathBuf>) {
	match std::fs::symlink_metadata(root) {
		Ok(meta) if meta.is_dir() => {
			dirs.push(root.to_owned());
			let Ok(entries) = std::fs::read_dir(root) else { return };
			let mut children: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
			children.sort();

			for child in children {
				walk(&child, files, dirs);
			}
		}
		Ok(_) => files.push(root.to_owned()),
		Err(_) => {}
	}
}

pub(crate) fn compress(paths: Vec<PathBuf>, archive: PathBuf, cwd: PathBuf, cancel: Arc<AtomicBool>) -> Job {
	let name = archive.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
	let format = format_of(&name);
	let (done, total) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0)));
	let (task_done, task_total) = (done.clone(), total.clone());
	let (tx, rx) = std::sync::mpsc::channel();

	std::thread::spawn(move || {
		let mut files = Vec::new();
		let mut dirs = Vec::new();
		for path in &paths {
			walk(path, &mut files, &mut dirs);
		}
		task_total.store(files.len(), Ordering::Relaxed);

		let result = if cancel.load(Ordering::Relaxed) {
			Err("已取消".to_owned())
		} else {
			match format {
				Format::Zip => pack_zip(&paths, &files, &archive, &cwd, &task_done, &cancel),
				Format::Tar => pack_tar(&paths, &archive, &cwd, &task_done, false, &cancel),
				Format::TarZstd => pack_tar(&paths, &archive, &cwd, &task_done, true, &cancel),
			}
		};

		if result.is_err() && cancel.load(Ordering::Relaxed) {
			let _ = std::fs::remove_file(&archive);
		}

		tx.send(result).ok();
	});

	Job { name, done, total, rx }
}

fn pack_zip(
	paths: &[PathBuf],
	files: &[PathBuf],
	archive: &Path,
	cwd: &Path,
	done: &AtomicUsize,
	cancel: &AtomicBool,
) -> Result<usize, String> {
	use zip::write::SimpleFileOptions;

	let file = std::fs::File::create(archive).map_err(|e| e.to_string())?;
	let mut zip = zip::ZipWriter::new(std::io::BufWriter::new(file));
	let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
	let mut written = 0usize;

	for path in paths {
		let mut empty = Vec::new();
		collect_dirs(path, &mut empty);
		for dir in empty {
			let name = format!("{}/", entry_name(&dir, cwd));
			zip.add_directory(name, options).map_err(|e| e.to_string())?;
		}
	}

	for path in files {
		if cancel.load(Ordering::Relaxed) {
			return Err("已取消".to_owned());
		}

		let name = entry_name(path, cwd);
		let mut source = std::fs::File::open(path).map_err(|e| format!("{name}: {e}"))?;
		zip.start_file(name, options).map_err(|e| e.to_string())?;
		std::io::copy(&mut source, &mut zip).map_err(|e| e.to_string())?;

		written += 1;
		done.store(written, Ordering::Relaxed);
	}

	zip.finish().map_err(|e| e.to_string())?;
	Ok(written)
}

fn collect_dirs(root: &Path, out: &mut Vec<PathBuf>) {
	let Ok(meta) = std::fs::symlink_metadata(root) else { return };
	if !meta.is_dir() {
		return;
	}

	out.push(root.to_owned());
	let Ok(entries) = std::fs::read_dir(root) else { return };
	for child in entries.flatten().map(|entry| entry.path()) {
		collect_dirs(&child, out);
	}
}

fn pack_tar(
	paths: &[PathBuf],
	archive: &Path,
	cwd: &Path,
	done: &AtomicUsize,
	zstd: bool,
	cancel: &AtomicBool,
) -> Result<usize, String> {
	use std::io::BufWriter;

	let file = std::fs::File::create(archive).map_err(|e| e.to_string())?;
	if zstd {
		let encoder = zstd::stream::write::Encoder::new(BufWriter::new(file), 3).map_err(|e| e.to_string())?;
		let written = tar_into(encoder, paths, cwd, done, cancel)?;
		return Ok(written);
	}

	tar_into(BufWriter::new(file), paths, cwd, done, cancel)
}

fn tar_into<W: Write>(
	writer: W,
	paths: &[PathBuf],
	cwd: &Path,
	done: &AtomicUsize,
	cancel: &AtomicBool,
) -> Result<usize, String> {
	let mut builder = tar::Builder::new(writer);
	let mut written = 0usize;

	for path in paths {
		if cancel.load(Ordering::Relaxed) {
			return Err("已取消".to_owned());
		}

		let name = entry_name(path, cwd);
		let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{name}: {e}"))?;

		if meta.is_dir() {
			builder.append_dir_all(&name, path).map_err(|e| e.to_string())?;
			let mut files = Vec::new();
			walk(path, &mut files, &mut Vec::new());
			written += files.len();
			done.store(written, Ordering::Relaxed);
		} else {
			builder.append_path_with_name(path, &name).map_err(|e| e.to_string())?;
			written += 1;
			done.store(written, Ordering::Relaxed);
		}
	}

	let inner = builder.into_inner().map_err(|e| e.to_string())?;
	let mut inner = inner;
	inner.flush().map_err(|e| e.to_string())?;
	Ok(written)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_cancelled_pack_writes_nothing_and_leaves_no_archive() {
		let dir = std::env::temp_dir().join(format!("crabhop-pack-cancel-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(dir.join("tree")).expect("a tree");
		std::fs::write(dir.join("tree/one.txt"), b"one").expect("a file");
		let archive = dir.join("out.zip");

		let cancel = Arc::new(AtomicBool::new(true));
		let job = compress(vec![ dir.join("tree") ], archive.clone(), dir.clone(), cancel);

		let answer = loop {
			match job.poll() {
				Some(answer) => break answer,
				None => std::thread::sleep(std::time::Duration::from_millis(20)),
			}
		};
		assert_eq!(answer, Err("已取消".to_owned()));
		assert!(!archive.exists(), "and the half archive is gone");

		let _ = std::fs::remove_dir_all(&dir);
	}

	fn scratch(name: &str) -> PathBuf {
		let dir = std::env::temp_dir().join(format!("crabhop-pack-{name}-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(dir.join("tree/inner")).expect("a tree");
		std::fs::write(dir.join("tree/a.txt"), b"hello").expect("a file");
		std::fs::write(dir.join("tree/inner/b.txt"), b"world").expect("a file");
		dir
	}

	fn wait(job: &Job) -> Result<usize, String> {
		loop {
			if let Some(answer) = job.poll() {
				return answer;
			}
			std::thread::sleep(std::time::Duration::from_millis(5));
		}
	}

	#[test]
	fn the_archive_name_reads_its_extension() {
		assert_eq!(format_of("photos.zip"), Format::Zip);
		assert_eq!(format_of("photos.tar"), Format::Tar);
		assert_eq!(format_of("photos.tar.zst"), Format::TarZstd);
		assert_eq!(format_of("photos.tzst"), Format::TarZstd);
		assert_eq!(format_of("photos"), Format::Zip, "no extension at all");

		assert_eq!(archive_name("work", ""), "work.zip");
		assert_eq!(archive_name("work", "backup"), "backup.zip");
		assert_eq!(archive_name("work", "backup.tar"), "backup.tar");
		assert_eq!(archive_name("work", "  backup.zip "), "backup.zip");
	}

	#[test]
	fn a_zip_packs_the_tree_and_counts_it() {
		let dir = scratch("zip");
		let archive = dir.join("tree.zip");
		let job = compress(vec![ dir.join("tree") ], archive.clone(), dir.clone(), Arc::new(AtomicBool::new(false)));
		let written = wait(&job).expect("packed");
		assert_eq!(written, 2, "two files under the tree");
		assert_eq!(job.progress(), (2, 2), "and the progress knows the whole story");

		let file = std::fs::File::open(&archive).expect("the archive");
		let mut zip = zip::ZipArchive::new(file).expect("a zip");
		let names: Vec<String> = (0..zip.len()).map(|at| zip.by_index(at).expect("an entry").name().to_owned()).collect();
		assert!(names.iter().any(|name| name == "tree/a.txt"), "{names:?}");
		assert!(names.iter().any(|name| name == "tree/inner/b.txt"), "{names:?}");

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn a_tar_and_a_tar_zst_both_pack() {
		let dir = scratch("tar");
		let archive = dir.join("tree.tar");
		let job = compress(vec![ dir.join("tree") ], archive.clone(), dir.clone(), Arc::new(AtomicBool::new(false)));
		assert_eq!(wait(&job).expect("packed"), 2);

		let file = std::fs::File::open(&archive).expect("the archive");
		let mut tar = tar::Archive::new(file);
		let names: Vec<String> = tar
			.entries()
			.expect("entries")
			.flatten()
			.map(|entry| entry.path().expect("a path").to_string_lossy().into_owned())
			.collect();
		assert!(names.iter().any(|name| name == "tree/a.txt"), "{names:?}");

		let archive = dir.join("tree.tar.zst");
		let job = compress(vec![ dir.join("tree") ], archive.clone(), dir.clone(), Arc::new(AtomicBool::new(false)));
		assert_eq!(wait(&job).expect("packed"), 2);

		let file = std::fs::File::open(&archive).expect("the archive");
		let decoder = zstd::stream::read::Decoder::new(file).expect("a zstd stream");
		let mut tar = tar::Archive::new(decoder);
		assert!(tar.entries().expect("entries").flatten().count() >= 2, "the zstd tar reads back");

		let _ = std::fs::remove_dir_all(&dir);
	}
}
