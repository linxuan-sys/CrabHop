
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

static ENABLED: LazyLock<bool> = LazyLock::new(|| std::env::var_os("CRABHOP_TIMING").is_some());
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

pub(crate) fn input(what: &'static str) {
	if let Ok(mut pending) = PENDING.lock() {
		*pending = Some((Instant::now(), what));
	}
}

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

pub(crate) fn frame_edge(frame: u64, begin: bool) {
	if !enabled() || frame > 3 {
		return;
	}

	let edge = if begin { "begin" } else { "end" };
	eprintln!("Timing: main→frame {frame} {edge} {:.1}ms", START.elapsed().as_secs_f64() * 1000.0);
}

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

pub(crate) fn cursor(at: usize, len: usize) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: cursor {at}/{len}");
}

pub(crate) fn cwd(path: &std::path::Path) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: cwd {}", path.display());
}

pub(crate) fn which(key: &str, rows: usize) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: which {key} → {rows} 项");
}

pub(crate) fn preview(before: f32, after: f32) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: preview offset {before:.1} → {after:.1}");
}

pub(crate) fn zoom(label: &str, file: &str) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: preview zoom {label} {file}");
}

pub(crate) fn upload(file: &str, from: [u32; 2], to: [u32; 2], target: [u32; 2]) {
	if !enabled() {
		return;
	}

	eprintln!(
		"Timing: preview upload {file} {}x{} → {}x{}（列 {}x{}）",
		from[0], from[1], to[0], to[1], target[0], target[1]
	);
}

pub(crate) fn bin(items: usize) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: bin {items} 项");
}

pub(crate) fn sweep(pictures: usize) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: preload sweep {pictures} 张");
}

pub(crate) fn menu(words: &str) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: menu {words}");
}

pub(crate) fn open_failed(path: &std::path::Path) {
	if !enabled() {
		return;
	}

	let file = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
	eprintln!("Timing: open {file} failed");
}

pub(crate) fn video(file: &str, road: &str) {
	if !enabled() {
		return;
	}

	let file = std::path::Path::new(file).file_name().map_or_else(|| file.to_owned(), |name| name.to_string_lossy().into_owned());
	eprintln!("Timing: video {file} {road}");
}

pub(crate) fn archive(file: &str, node: &str, folded: bool) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: archive {file} {node} {}", if folded { "folded" } else { "unfolded" });
}

pub(crate) fn scroll(offset: f32, target: f32) {
	if !enabled() {
		return;
	}

	eprintln!("Timing: scroll offset {offset:.3} → {target:.3}");
}
