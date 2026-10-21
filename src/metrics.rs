//! The numbers the acceptance asks for, taken where they happen. Off unless `DUCKFM_TIMING` is
//! set; the state that is measured is kept either way, so measuring hardly changes the measurement.

use std::sync::{LazyLock, Mutex};
use std::time::Instant;

static ENABLED: LazyLock<bool> = LazyLock::new(|| std::env::var_os("DUCKFM_TIMING").is_some());
static START: LazyLock<Instant> = LazyLock::new(Instant::now);
static PENDING: LazyLock<Mutex<Option<(Instant, &'static str)>>> = LazyLock::new(|| Mutex::new(None));
static FIRST: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));
static PAINT: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));
static DRESSED: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

pub(crate) fn start() {
	LazyLock::force(&START);
}

pub(crate) fn enabled() -> bool {
	*ENABLED
}

/// A person has just done something; the frame it produces is measured against this.
pub(crate) fn input(what: &'static str) {
	if let Ok(mut pending) = PENDING.lock() {
		*pending = Some((Instant::now(), what));
	}
}

/// The frame is being handed to the GPU: what a person actually waited for is now known.
pub(crate) fn frame(dt: f32) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: frame {:.1}ms", dt * 1000.0);

	let Ok(mut pending) = PENDING.lock() else { return };
	if let Some((at, what)) = pending.take() {
		eprintln!("Timing: {what}→frame {:.1}ms", at.elapsed().as_secs_f64() * 1000.0);
	}
}

/// The begin or end of a frame's own ui, in wall time. With the fonts deferred the first frame is
/// not the one that carries them, and the number that matters for the A/B is when *that* frame
/// reaches the glass — which is the begin of the one after it. The first three frames are enough,
/// and it costs nothing when the measurement is off.
pub(crate) fn frame_edge(frame: u64, begin: bool) {
	if !enabled() || frame > 3 {
		return;
	}

	let edge = if begin { "begin" } else { "end" };
	eprintln!("Timing: main→frame {frame} {edge} {:.1}ms", START.elapsed().as_secs_f64() * 1000.0);
}

/// The first frame drawn with the real faces: with the fonts deferred that is the frame after the
/// swap, with them early the very first frame — the other half of the A/B.
pub(crate) fn first_dressed() {
	if !enabled() {
		return;
	}

	let Ok(mut first) = DRESSED.lock() else { return };
	if *first {
		return;
	}
	*first = true;

	eprintln!("Timing: main→first dressed {:.1}ms", START.elapsed().as_secs_f64() * 1000.0);
}

/// The very first frame — the skeleton, before any font atlas is built or any directory is read.
/// Zed's own bar (120-350ms) is measured at this point, so this is the number to hold.
pub(crate) fn first_paint() {
	if !enabled() {
		return;
	}

	let Ok(mut first) = PAINT.lock() else { return };
	if *first {
		return;
	}
	*first = true;

	eprintln!("Timing: main→first paint {:.1}ms", START.elapsed().as_secs_f64() * 1000.0);
}

/// The first frame that has a list in it, which is where the cold start ends.
pub(crate) fn first_list() {
	if !enabled() {
		return;
	}

	let Ok(mut first) = FIRST.lock() else { return };
	if *first {
		return;
	}
	*first = true;

	eprintln!("Timing: main→first list {:.1}ms", START.elapsed().as_secs_f64() * 1000.0);
}

/// The cursor has been seated — by a click, a key or a jump: the number a replay reads back to
/// assert where a click left it.
pub(crate) fn cursor(at: usize, len: usize) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: cursor {at}/{len}");
}

/// The directory a tab has arrived in: what a replay reads back to assert that a jump — `g a`,
/// `h`, a trail step — landed where the keymap promised (P18).
pub(crate) fn cwd(path: &std::path::Path) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: cwd {}", path.display());
}

/// A chord's family has just opened: the key, and how many continuations the keymap offers — the
/// replay's own reading of what the `which` hint had to show (P18).
pub(crate) fn which(key: &str, rows: usize) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: which {key} → {rows} 项");
}

/// The preview column's own offset, before and after a wheel or a seek step — the reading a
/// replay asserts when it has to show that the column under the pointer is the one that moved
/// (P18).
pub(crate) fn preview(before: f32, after: f32) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: preview offset {before:.1} → {after:.1}");
}

/// A picture's badge label has changed (P28①): the reading a replay asserts a zoom by, paired
/// with the file whose picture moved.
pub(crate) fn zoom(label: &str, file: &str) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: preview zoom {label} {file}");
}

/// A picture was reduced before upload (P28③): the pixels of the file, of the upload, and the
/// column the reduction was sized against — the numbers the acceptance reads VRAM by.
pub(crate) fn upload(file: &str, from: [u32; 2], to: [u32; 2], target: [u32; 2]) {
	if !enabled() {
		return;
	}

	eprintln!(
		"Timing: preview upload {file} {}x{} → {}x{}（列 {}x{}）",
		from[0], from[1], to[0], to[1], target[0], target[1]
	);
}

/// The bin's own list has landed, and how many items it holds: the reading that shows the view
/// refreshed itself after a restore instead of standing on「读取中」 (P19).
pub(crate) fn bin(items: usize) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: bin {items} 项");
}

/// The idle sweep has begun, and how many pictures it took on (P27⑥'s L3).
pub(crate) fn sweep(pictures: usize) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: preload sweep {pictures} 张");
}

/// A menu's own goings-on (P31): what was opened on what, which page held how much, and which
/// answer was taken — one line, so a replay can assert each.
pub(crate) fn menu(words: &str) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: menu {words}");
}

/// An opening that failed (P30): the file a launcher could not be found for, said out loud.
pub(crate) fn open_failed(path: &std::path::Path) {
	if !enabled() {
		return;
	}

	let file = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
	eprintln!("Timing: open {file} failed");
}

/// A video's frame or its facts (P29): the file and the road it came by — `bucket` (the window's
/// own cache), `extract` (a tool made it), `header` (read in process) or `none` (nothing said).
pub(crate) fn video(file: &str, road: &str) {
	if !enabled() {
		return;
	}

	let file = std::path::Path::new(file).file_name().map_or_else(|| file.to_owned(), |name| name.to_string_lossy().into_owned());
	eprintln!("Timing: video {file} {road}");
}

/// An archive tree's folder was folded or unfolded (P28's long tail): the archive's own file name,
/// the node's path inside it, and which way it now stands.
pub(crate) fn archive(file: &str, node: &str, folded: bool) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: archive {file} {node} {}", if folded { "folded" } else { "unfolded" });
}

/// A frame worth looking at during an inertia: the offset of the scroll, for the monotonicity the
/// acceptance reads out of three of them.
pub(crate) fn scroll(offset: f32, target: f32) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: scroll offset {offset:.3} → {target:.3}");
}
