//! Where the file names come from: a directory is read once, off this thread, and kept as a
//! snapshot the list can draw at any moment. The window never waits for it — a loading snapshot is
//! a valid thing to draw.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Instant, SystemTime};

use tokio::sync::mpsc;

/// How long the watcher lets a storm of writes settle before the directory is read again (P18).
const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(200);

/// What the watcher thread is told: a directory to keep an eye on.
enum Signal {
	Watch(PathBuf),
}

#[derive(Clone)]
pub(crate) struct Entry {
	pub name:     String,
	pub size:     u64,
	pub modified: Option<SystemTime>,
	pub dir:      bool,
	/// The unix permission bits, when the platform has them: the status bar reads them.
	pub mode:     Option<u32>,
	/// Whether the entry itself is a symbolic link — `g f` keeps only these (P21's table).
	pub link:     bool,
	/// When the file was created, where the filesystem can say (P21's `m b`).
	pub created:  Option<SystemTime>,
	/// The owner's user id — the `m o` linemode reads the name behind it (P21).
	pub uid:      u32,
}

#[derive(Clone, Default)]
pub(crate) struct Snapshot {
	pub entries: Vec<Entry>,
	pub loading: bool,
	/// When the read went out, so the skeleton can wait its 120ms before it shows.
	pub started: Option<Instant>,
	/// How many entries this directory had last time — the skeleton's own row count.
	pub hint:    Option<usize>,
}

pub(crate) struct FolderCache {
	map: Arc<RwLock<HashMap<PathBuf, Arc<Snapshot>>>>,
	/// What each directory held the last time it was read: remembered for the skeleton only.
	counts: Arc<RwLock<HashMap<PathBuf, usize>>>,
	tx:  mpsc::UnboundedSender<PathBuf>,
	/// The watcher's own line (P18): a directory to watch, asked for once.
	watch: Arc<std::sync::Mutex<std::collections::HashSet<PathBuf>>>,
	/// The signal line into the watcher thread.
	signal: std::sync::mpsc::Sender<Signal>,
}

impl FolderCache {
	/// Start the one worker that reads directories: a small runtime of its own, and a channel it
	/// listens on. When a snapshot lands, the window is asked to paint again.
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

	/// Whether a directory is being watched yet — the tests read it; the window does not.
	#[cfg(test)]
	pub(crate) fn watching(&self, path: &Path) -> bool {
		self.watch.lock().map(|known| known.contains(path)).unwrap_or(false)
	}

	/// Ask the watcher to keep an eye on a directory — once; every later ask is free. The window
	/// asks for the directory it shows and the parent column's own (P18).
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

	/// The snapshot for a directory, asking the worker for it the first time it is seen. The first
	/// answer is a loading one: the list draws it and the content arrives when it arrives.
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

	/// F5: forget it, so the next look asks the worker again.
	pub(crate) fn forget(&self, path: &Path) {
		if let Ok(mut map) = self.map.write() {
			map.remove(path);
		}
	}
}

async fn read(path: &Path) -> Snapshot {
	// one listing implementation for the whole window: the preview column reads directories too,
	// and its workers are plain threads — so the read itself is synchronous, and the async door
	// simply opens it on a blocking thread
	let path = path.to_owned();
	let entries = tokio::task::spawn_blocking(move || read_sync(&path)).await.unwrap_or_default();

	Snapshot { entries, loading: false, started: None, hint: None }
}

/// The watcher thread and its debounce (P18): notify hands every write in a watched directory
/// over, the storm is allowed to settle for [`DEBOUNCE`], and then the directory is read again
/// and swapped into the cache — the list rebuilds itself, and a file another program made
/// appears without a restart.
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
			// the cache keys on directories: a file's event names it by its parent
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

			// let the storm settle: anything inside the window joins the same re-read
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

/// A directory read, synchronously: the entries a list draws, directories first, then by name —
/// the order every file manager has ever used. Shared by the folder worker (through `read`) and by
/// the preview column's directory listing.
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

/// The owner's user id, on the platforms that have them.
fn uid_of(meta: Option<&std::fs::Metadata>) -> u32 {
	#[cfg(unix)]
	{
		use std::os::unix::fs::MetadataExt;
		return meta.map_or(0, |m| m.uid());
	}

	#[allow(unreachable_code)]
	0
}

/// The unix permission bits, on the platforms that have them.
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

	/// The watcher (P18): a file another program makes in a watched directory shows up in the
	/// snapshot without anyone asking for a re-read.
	#[test]
	fn a_file_made_by_another_program_arrives_by_itself() {
		let dir = std::env::temp_dir().join(format!("duckfm-native-watch-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");

		let cache = FolderCache::spawn(eframe::egui::Context::default());
		let first = cache.get(&dir);
		assert!(first.loading, "the first look is a loading one");
		cache.watch(&dir);
		assert!(cache.watching(&dir), "and the directory is watched");

		// wait for the first read to land, then let another program write
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

		// the watcher's own debounce is 200ms; the patience is five seconds because the suite runs
		// this beside a dozen other tests (the GPU ones among them) and a loaded machine can push
		// the debounce's own thread past a second — the assertion stays strict, the wait is longer
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

	/// A snapshot is drawn, not waited for: the first look at a directory is a loading one, and the
	/// worker's answer replaces it.
	#[test]
	fn a_directory_reads_off_the_thread_and_sorts_directories_first() {
		let dir = std::env::temp_dir().join(format!("duckfm-native-test-{}", std::process::id()));
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
