
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime};

use tokio::sync::mpsc;

const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);

enum Signal {
	Watch(PathBuf),
}

#[derive(Clone)]
pub(crate) struct Entry {
	pub name:     String,
	pub size:     u64,
	pub modified: Option<SystemTime>,
	pub dir:      bool,
	pub mode:     Option<u32>,
	pub link:     bool,
	pub created:  Option<SystemTime>,
	pub uid:      u32,
}

#[derive(Clone, Default)]
pub(crate) struct Snapshot {
	pub entries: Vec<Entry>,
	pub loading: bool,
	pub started: Option<Instant>,
	pub hint:    Option<usize>,
}

pub(crate) struct FolderCache {
	map: Arc<RwLock<HashMap<PathBuf, Arc<Snapshot>>>>,
	counts: Arc<RwLock<HashMap<PathBuf, usize>>>,
	tx:  mpsc::UnboundedSender<PathBuf>,
	watch: Arc<std::sync::Mutex<std::collections::HashSet<PathBuf>>>,
	signal: std::sync::mpsc::Sender<Signal>,
}

impl FolderCache {
	pub(crate) fn spawn(ctx: eframe::egui::Context) -> Self {
		let map = Arc::new(RwLock::new(HashMap::new()));
		let counts = Arc::new(RwLock::new(HashMap::new()));
		let (tx, mut rx) = mpsc::unbounded_channel::<PathBuf>();
		let (signal, signals) = std::sync::mpsc::channel::<Signal>();

		spawn_watcher(signals, map.clone(), counts.clone(), ctx.clone());

		let worker_map = map.clone();
		let worker_counts = counts.clone();
		std::thread::spawn(move || {
			let rt = tokio::runtime::Builder::new_multi_thread()
				.worker_threads(2)
				.enable_all()
				.build()
				.expect("a runtime for the folder worker");

			rt.block_on(async move {
				while let Some(path) = rx.recv().await {
					let snapshot = Arc::new(read(&path).await);
					if let Ok(mut cache) = worker_map.write() {
						cache.insert(path.clone(), snapshot.clone());
					}
					if let Ok(mut known) = worker_counts.write() {
						known.insert(path, snapshot.entries.len());
					}
					ctx.request_repaint();
				}
			});
		});

		Self {
			map,
			counts,
			tx,
			watch: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
			signal,
		}
	}

	#[cfg(test)]
	pub(crate) fn watching(&self, path: &Path) -> bool {
		self.watch.lock().map(|known| known.contains(path)).unwrap_or(false)
	}

	pub(crate) fn watch(&self, path: &Path) {
		let fresh = self
			.watch
			.lock()
			.map(|mut known| known.insert(path.to_owned()))
			.unwrap_or(false);
		if !fresh {
			return;
		}

		self.signal.send(Signal::Watch(path.to_owned())).ok();
	}

	pub(crate) fn get(&self, path: &Path) -> Arc<Snapshot> {
		if let Ok(map) = self.map.read()
			&& let Some(snapshot) = map.get(path)
		{
			return snapshot.clone();
		}

		let hint = self.counts.read().ok().and_then(|known| known.get(path).copied());
		let loading = Arc::new(Snapshot { entries: Vec::new(), loading: true, started: Some(Instant::now()), hint });

		if let Ok(mut map) = self.map.write() {
			map.entry(path.to_owned()).or_insert_with(|| loading.clone());
		}
		self.tx.send(path.to_owned()).ok();

		loading
	}

	pub(crate) fn forget(&self, path: &Path) {
		if let Ok(mut map) = self.map.write() {
			map.remove(path);
		}
	}
}

async fn read(path: &Path) -> Snapshot {
	let path = path.to_owned();
	let entries = tokio::task::spawn_blocking(move || read_sync(&path)).await.unwrap_or_default();

	Snapshot { entries, loading: false, started: None, hint: None }
}

fn spawn_watcher(
	signals: std::sync::mpsc::Receiver<Signal>,
	map: Arc<RwLock<HashMap<PathBuf, Arc<Snapshot>>>>,
	counts: Arc<RwLock<HashMap<PathBuf, usize>>>,
	ctx: eframe::egui::Context,
) {
	let (changed, changes) = std::sync::mpsc::channel::<PathBuf>();

	let repaint = ctx.clone();
	std::thread::spawn(move || {
		let handler = move |event: Result<notify::Event, notify::Error>| {
			let Ok(event) = event else { return };
			let Some(path) = event.paths.first() else { return };
			let directory = if path.is_dir() { path.clone() } else { path.parent().map(Path::to_owned).unwrap_or_default() };

			changed.send(directory).ok();
			repaint.request_repaint();
		};

		use notify::Watcher as _;

		let Ok(mut watcher) = notify::recommended_watcher(handler) else { return };
		for signal in signals {
			let Signal::Watch(directory) = signal;
			let _ = watcher.watch(&directory, notify::RecursiveMode::NonRecursive);
		}
	});

	let repaint = ctx.clone();
	std::thread::spawn(move || {
		while let Ok(first) = changes.recv() {
			let mut directories = std::collections::HashSet::new();
			directories.insert(first);

			let closes = Instant::now() + DEBOUNCE;
			while let Ok(directory) = changes.recv_timeout(closes.saturating_duration_since(Instant::now())) {
				directories.insert(directory);
			}

			for directory in directories {
				let entries = read_sync(&directory);
				let snapshot = Arc::new(Snapshot { entries, loading: false, started: None, hint: None });
				if let Ok(mut known) = counts.write() {
					known.insert(directory.clone(), snapshot.entries.len());
				}
				if let Ok(mut cache) = map.write() {
					cache.insert(directory, snapshot);
				}
			}

			repaint.request_repaint();
		}
	});
}

pub(crate) fn read_sync(path: &Path) -> Vec<Entry> {
	let mut entries = Vec::new();

	let Ok(dir) = std::fs::read_dir(path) else {
		return entries;
	};

	for item in dir.flatten() {
		let meta = std::fs::symlink_metadata(item.path()).ok();

		entries.push(Entry {
			name:     item.file_name().to_string_lossy().into_owned(),
			size:     meta.as_ref().map_or(0, |m| m.len()),
			modified: meta.as_ref().and_then(|m| m.modified().ok()),
			dir:      meta.as_ref().is_some_and(|m| m.is_dir()),
			mode:     mode_of(meta.as_ref()),
			link:     meta.as_ref().is_some_and(|m| m.file_type().is_symlink()),
			created:  meta.as_ref().and_then(|m| m.created().ok()),
			uid:      uid_of(meta.as_ref()),
		});
	}

	entries.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
	entries
}

fn uid_of(meta: Option<&std::fs::Metadata>) -> u32 {
	#[cfg(unix)]
	{
		use std::os::unix::fs::MetadataExt;
		return meta.map_or(0, |m| m.uid());
	}

	#[allow(unreachable_code)]
	0
}

fn mode_of(meta: Option<&std::fs::Metadata>) -> Option<u32> {
	#[cfg(unix)]
	{
		use std::os::unix::fs::MetadataExt;
		meta.map(|m| m.mode())
	}
	#[cfg(not(unix))]
	{
		let _ = meta;
		None
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_file_made_by_another_program_arrives_by_itself() {
		let dir = std::env::temp_dir().join(format!("crabhop-watch-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");

		let cache = FolderCache::spawn(eframe::egui::Context::default());
		let first = cache.get(&dir);
		assert!(first.loading, "the first look is a loading one");
		cache.watch(&dir);
		assert!(cache.watching(&dir), "and the directory is watched");

		let settled = |cache: &FolderCache| !cache.get(&dir).loading;
		for _ in 0..100 {
			if settled(&cache) {
				break;
			}
			std::thread::sleep(std::time::Duration::from_millis(20));
		}
		assert!(settled(&cache), "the first read lands");
		assert!(cache.get(&dir).entries.is_empty(), "an empty directory");

		std::fs::write(dir.join("made-elsewhere.txt"), b"hello").expect("a file");

		let mut seen = false;
		for _ in 0..250 {
			if cache.get(&dir).entries.iter().any(|entry| entry.name == "made-elsewhere.txt") {
				seen = true;
				break;
			}
			std::thread::sleep(std::time::Duration::from_millis(20));
		}
		assert!(seen, "the new file arrived without anyone asking");

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn a_directory_reads_off_the_thread_and_sorts_directories_first() {
		let dir = std::env::temp_dir().join(format!("crabhop-test-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(dir.join("beta")).expect("a directory");
		std::fs::write(dir.join("alpha.txt"), b"hello").expect("a file");
		std::fs::write(dir.join("Zeta.txt"), b"hello").expect("a file");

		let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("a runtime");
		let snapshot = rt.block_on(read(&dir));

		assert!(!snapshot.loading);
		let names: Vec<&str> = snapshot.entries.iter().map(|e| e.name.as_str()).collect();
		assert_eq!(names, vec![ "beta", "alpha.txt", "Zeta.txt" ], "directories first, then names");
		assert!(snapshot.entries[0].dir);
		assert_eq!(snapshot.entries[1].size, 5);
		assert!(snapshot.entries[1].mode.is_some(), "and the status bar will have bits to read");

		let _ = std::fs::remove_dir_all(&dir);
	}
}
