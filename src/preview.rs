
mod archive;
mod code;
mod docx;
mod img;
mod md;
mod pdf;
mod rows;
mod sheet;
pub(crate) mod video;
mod thumb;

use std::{
	collections::{HashMap, HashSet},
	path::{Path, PathBuf},
	sync::{
		Arc, LazyLock,
		mpsc::{Receiver, Sender, channel},
	},
	time::{Duration, Instant, SystemTime},
};

use eframe::egui::{
	Align2, Color32, ColorImage, Context, FontFamily, FontId, Id, Painter, Pos2, Rect, TextureHandle, TextureOptions,
	Vec2, pos2, vec2,
};
use tokio::sync::Semaphore;

use crate::chrome;
use crate::folder::Entry;
use crate::list;
use crate::preview::rows::{Doc, Row, Run};
use crate::tokens::{ICON_BOX, ROW_HEIGHT, Tokens};

pub(crate) const LOADING_DELAY: Duration = Duration::from_millis(200);

const UPLOAD_THROTTLE: Duration = Duration::from_millis(40);

const LINE_PX: f32 = 24.0;

const PAD: f32 = 16.0;
const PAD_RIGHT: f32 = 10.0;

pub(crate) fn content_of(column: Rect) -> Rect {
	Rect::from_min_max(
		pos2(column.min.x + PAD, column.min.y + PAD),
		pos2(column.max.x - PAD_RIGHT, column.max.y - PAD),
	)
}

const HEAD: usize = 1024 * 1024;

const DOC_CAP: usize = 64 * 1024 * 1024;

const TEXT: &[&str] = &[
	"txt", "log", "ini", "conf", "cfg", "toml", "yaml", "yml", "json", "jsonc", "xml", "html", "htm", "css", "scss",
	"js", "jsx", "ts", "tsx", "rs", "py", "rb", "go", "java", "c", "h", "cpp", "hpp", "cc", "cs", "lua", "sh", "bash",
	"zsh", "fish", "sql", "csv", "tsv", "diff", "patch", "gitignore", "lock", "env", "service", "desktop", "nix", "vim",
	"el", "clj", "kt", "swift", "php", "pl", "r", "jl", "ex", "exs", "erl", "hs", "ml", "scala", "dart", "zig", "v",
	"asm", "s", "wat", "proto", "graphql", "tf", "hcl", "dockerfile", "makefile", "cmake", "gradle",
];

struct Look {
	generation: u64,
	path:       PathBuf,
	kind:       Kind,
	len:        u64,
	mtime:      Option<SystemTime>,
	target:     [u32; 2],
	page:       usize,
}

enum Payload {
	Image {
		size:   [u32; 2],
		source: [u32; 2],
		rgba:   Vec<u8>,
		frames: Vec<(Vec<u8>, u32)>,
	},
	Media(Vec<(String, String)>),
	Dir(Vec<Entry>),
	Archive(archive::Listing),
	Pdf { page: usize, total: usize, size: [u32; 2], rgba: Vec<u8> },
	Text {
		text:      String,
		truncated: bool,
		images:    Vec<(String, String, [u32; 2], Vec<u8>)>,
	},
	Sheet(sheet::Head),
	Note(String),
	Nothing,
	Degraded(String),
}

struct Answer {
	generation: u64,
	key:        String,
	payload:    Payload,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Kind {
	Image,
	Markdown,
	Code,
	Docx,
	Sheet,
	Pdf,
	Dir,
	Archive,
	Video,
	Placeholder,
}

impl Kind {
	pub(crate) fn of(path: &Path, dir: bool) -> Self {
		if dir {
			return Self::Dir;
		}

		let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
		if archive::is_known(name) {
			return Self::Archive;
		}

		let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();

		match ext.as_str() {
			e if img::recognises(e) => Self::Image,
			"md" | "markdown" => Self::Markdown,
			"docx" => Self::Docx,
			"xlsx" | "xls" | "xlsm" | "xlsb" | "ods" => Self::Sheet,
			"pdf" => Self::Pdf,
			e if video::is_video(&format!("x.{e}")) => Self::Video,
			"avif" | "heic" | "heif" => Self::Placeholder,
			e if TEXT.contains(&e) => Self::Code,
			_ => Self::Code,
		}
	}
}

enum Show {
	Empty,
	Note(String),
	Placeholder(Option<String>),
	Image { key: String, source: [u32; 2] },
	Media { facts: Vec<(String, String)> },
	Pdf { key: String, size: [u32; 2], page: usize, total: usize },
	Dir { entries: Vec<Entry>, doc: Option<Doc>, truncated: bool },
	Archive { items: Vec<archive::Item>, doc: Option<Doc>, total: Option<usize>, truncated: bool },
	Sheet { head: sheet::Head },
}

struct Raw {
	path:      PathBuf,
	kind:      Kind,
	text:      String,
	truncated: bool,
	images:    Vec<(String, String, [u32; 2])>,
}

struct Anim {
	key:    String,
	size:   [u32; 2],
	frames: Vec<(Vec<u8>, u32)>,
	at:     usize,
	due:    Instant,
}

fn reduced_motion() -> bool {
	static STILL: LazyLock<bool> = LazyLock::new(|| std::env::var_os("CRABHOP_REDUCED_MOTION").is_some());

	*STILL
}

struct Queued {
	generation: u64,
	path:       PathBuf,
	kind:       Kind,
	len:        u64,
	mtime:      Option<SystemTime>,
	at:         Instant,
}

struct Sweep {
	stamp: u64,
	queue: Vec<(PathBuf, u64, Option<SystemTime>)>,
	since: Instant,
}

struct Hl {
	key:    String,
	syntax: code::Syntax,
	row:    usize,
}

pub(crate) struct PreviewPane {
	ctx:   Context,
	tx:    Sender<Answer>,
	rx:    Receiver<Answer>,
	slots: Arc<Semaphore>,
	preload: Arc<Semaphore>,

	path:  Option<PathBuf>,
	kind:  Kind,
	len:   u64,
	mtime: Option<SystemTime>,

	generation: u64,
	queued:     Option<Queued>,
	pending: Option<Instant>,

	show: Show,
	raw:  Option<Raw>,

	textures: HashMap<String, TextureHandle>,
	lru:      img::Lru,

	laid:    Option<Doc>,
	laid_of: Option<String>,
	notice:  Option<&'static str>,
	hl:      Option<Hl>,

	offset: f32,
	view:   f32,
	target: [u32; 2],

	zoom: f32,
	pan:  Vec2,
	touched: Option<Instant>,
	badge:   Option<String>,
	anim:    Option<Anim>,

	warmed:     HashSet<String>,
	warmed_dir: Option<PathBuf>,

	last_look: Option<Instant>,
	held:      Option<Answer>,
	sweep:     Option<Sweep>,
	visit:     Arc<std::sync::atomic::AtomicU64>,
	collapsed: HashSet<String>,
}

impl Default for PreviewPane {
	fn default() -> Self { Self::new(Context::default()) }
}

impl PreviewPane {
	pub(crate) fn new(ctx: Context) -> Self {
		let (tx, rx) = channel();
		let slots = Arc::new(Semaphore::new(img::slots()));
		let preload = Arc::new(Semaphore::new(1));

		Self {
			ctx,
			tx,
			rx,
			slots,
			preload,
			path: None,
			kind: Kind::Placeholder,
			len: 0,
			mtime: None,
			generation: 0,
			queued: None,
			pending: None,
			show: Show::Empty,
			raw: None,
			textures: HashMap::new(),
			lru: img::Lru::new(img::TEXTURE_CAP),
			laid: None,
			laid_of: None,
			notice: None,
			hl: None,
			offset: 0.0,
			view: 1.0,
			target: [ 0, 0 ],
			zoom: 1.0,
			pan: Vec2::ZERO,
			touched: None,
			badge: None,
			anim: None,
			warmed: HashSet::new(),
			warmed_dir: None,
			last_look: None,
			held: None,
			sweep: None,
			visit: Arc::new(std::sync::atomic::AtomicU64::new(0)),
			collapsed: HashSet::new(),
		}
	}

	fn tick_animation(&mut self) {
		let Some(anim) = &self.anim else { return };
		let now = Instant::now();
		if anim.due > now {
			self.ctx.request_repaint_after(anim.due - now);
			return;
		}

		let (key, size, rgba, delay) = {
			let anim = self.anim.as_ref().expect("checked above");
			(anim.key.clone(), anim.size, anim.frames[anim.at].0.clone(), anim.frames[anim.at].1)
		};
		self.upload(&key, size, &rgba);

		if let Some(anim) = &mut self.anim {
			anim.at = (anim.at + 1) % anim.frames.len();
			anim.due = now + Duration::from_millis(delay as u64);
		}
		self.ctx.request_repaint_after(Duration::from_millis(delay as u64));
	}

	pub(crate) fn look(&mut self, path: &Path, entry: Option<&Entry>) {
		let (len, mtime, dir) = entry.map_or((0, None, false), |e| (e.size, e.modified, e.dir));
		if self.path.as_deref() == Some(path) && self.len == len && self.mtime == mtime {
			return;
		}

		self.path = Some(path.to_owned());
		self.len = len;
		self.mtime = mtime;
		self.kind = Kind::of(path, dir);
		self.offset = 0.0;
		self.zoom = 1.0;
		self.pan = Vec2::ZERO;
		self.badge = None;
		self.hl = None;
		self.pending = None;
		self.anim = None;
		self.generation += 1;
		self.last_look = Some(Instant::now());
		self.held = None;

		match self.kind {
			Kind::Placeholder => {
				self.queued = None;
				self.raw = None;
				self.show = Show::Placeholder(None);
			}
			kind => {
				self.queued = Some(Queued {
					generation: self.generation,
					path:       path.to_owned(),
					kind,
					len,
					mtime,
					at: Instant::now(),
				});
			}
		}
	}

	pub(crate) fn hide(&mut self) {
		self.target = [ 0, 0 ];
		self.visit.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		self.sweep = None;
	}

	pub(crate) fn clear(&mut self) {
		self.path = None;
		self.raw = None;
		self.show = Show::Empty;
		self.queued = None;
		self.pending = None;
		self.offset = 0.0;
		self.badge = None;
		self.anim = None;
		self.generation += 1;
		self.visit.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		self.sweep = None;
		self.held = None;
		self.collapsed.clear();
	}

	pub(crate) fn warm(
		&mut self,
		dir: &Path,
		visible: &[(PathBuf, Option<Entry>)],
		around: &[(PathBuf, Option<Entry>)],
		ahead: &[(PathBuf, Option<Entry>)],
	) {
		let first = self.warmed_dir.as_deref() != Some(dir);
		if first {
			self.warmed_dir = Some(dir.to_owned());
			self.visit.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
			let stamp = self.visit.load(std::sync::atomic::Ordering::Relaxed);
			self.sweep = Some(Sweep { stamp, queue: Vec::new(), since: Instant::now() });
		}
		if let Some(sweep) = &mut self.sweep {
			sweep.since = Instant::now();
			if sweep.queue.is_empty() {
				sweep.queue = image_jobs(ahead);
			}
		}

		let mut screenful: Vec<(PathBuf, Option<Entry>)> = visible.to_vec();
		if first {
			screenful.extend_from_slice(around);
		}
		let jobs = image_jobs(if first { &screenful } else { around });

		for (path, len, mtime) in jobs {
			let key = format!(
				"{}|{}",
				path.display(),
				mtime.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis())
			);
			if !self.warmed.insert(key) {
				continue;
			}
			if self.warmed.len() > 8192 {
				self.warmed.clear();
			}

			self.preload_job(path, len, mtime);
		}
	}

	fn preload_job(&self, path: PathBuf, len: u64, mtime: Option<SystemTime>) {
		let target = self.image_target();
		if self.target[0] == 0 {
			return;
		}

		let (slot, visit) = (self.preload.clone(), self.visit.clone());
		let stamp = visit.load(std::sync::atomic::Ordering::Relaxed);

		std::thread::spawn(move || {
			let Some(_permit) = preload_slot(slot.clone()) else { return };
			if visit.load(std::sync::atomic::Ordering::Relaxed) != stamp {
				return;
			}

			run_warm(&path, len, mtime, target);
		});
	}

	fn sweep_tick(&mut self) {
		let Some(sweep) = &self.sweep else { return };
		if sweep.queue.is_empty() || self.target[0] == 0 || sweep.since.elapsed() < SWEEP_IDLE {
			return;
		}

		let sweep = self.sweep.take().expect("checked above");
		let (slot, target, visit) = (self.preload.clone(), self.image_target(), self.visit.clone());
		crate::metrics::sweep(sweep.queue.len());

		std::thread::spawn(move || {
			for (path, len, mtime) in sweep.queue {
				if visit.load(std::sync::atomic::Ordering::Relaxed) != sweep.stamp {
					return;
				}
				let Some(_permit) = preload_slot(slot.clone()) else { return };
				if visit.load(std::sync::atomic::Ordering::Relaxed) != sweep.stamp {
					return;
				}

				run_warm(&path, len, mtime, target);
			}

			img::bucket_prune();
		});
	}

	pub(crate) fn poll(&mut self) {
		while let Ok(answer) = self.rx.try_recv() {
			if answer.generation != self.generation {
				continue;
			}
			self.accept(answer);
		}

		self.advance();
		self.sweep_tick();
	}

	fn advance(&mut self) {
		if self.target[0] == 0 {
			return;
		}

		self.release_held();

		let Some(mut queued) = self.queued.take() else {
			if let Some(at) = self.pending {
				let waited = at.elapsed();
				if waited < LOADING_DELAY {
					self.ctx.request_repaint_after(LOADING_DELAY - waited);
				}
			}
			return;
		};

		let (path, kind, len, mtime, target, generation) = (
			queued.path.clone(),
			queued.kind,
			queued.len,
			queued.mtime,
			match queued.kind {
				Kind::Pdf => img::request_target(self.target, self.zoom),
				Kind::Image => self.image_target(),
				Kind::Video => [ self.target[0].max(1), 0 ],
				_ => self.target,
			},
			queued.generation,
		);
		let page = self.pdf_page();

		let Ok(slot) = self.slots.clone().try_acquire_owned() else {
			queued.at = Instant::now();
			self.queued = Some(queued);
			self.ctx.request_repaint_after(Duration::from_millis(50));
			return;
		};

		let tx = self.tx.clone();
		let ctx = self.ctx.clone();
		self.pending = Some(Instant::now());

		std::thread::spawn(move || {
			let _slot = slot;
			run(&ctx, Look { generation, path, kind, len, mtime, target, page }, tx);
		});
	}

	fn release_held(&mut self) {
		if self.held.is_none() {
			return;
		}

		let waited = self.last_look.map_or(UPLOAD_THROTTLE, |at| at.elapsed());
		if waited < UPLOAD_THROTTLE {
			self.ctx.request_repaint_after(UPLOAD_THROTTLE - waited);
			return;
		}

		let held = self.held.take().expect("checked above");
		self.place(held);
	}

	fn accept(&mut self, answer: Answer) {
		if matches!(answer.payload, Payload::Image { .. } | Payload::Pdf { .. }) {
			self.pending = None;
			let waited = self.last_look.map_or(UPLOAD_THROTTLE, |at| at.elapsed());
			if waited < UPLOAD_THROTTLE {
				self.held = Some(answer);
				self.ctx.request_repaint_after(UPLOAD_THROTTLE - waited);
				return;
			}

			self.place(answer);
			return;
		}

		self.place(answer);
	}

	fn place(&mut self, answer: Answer) {
		match answer.payload {
			Payload::Image { size, source, rgba, frames } => {
				self.upload(&answer.key, size, &rgba);
				self.pending = None;
				self.raw = None;
				self.show = Show::Image { key: answer.key.clone(), source };
				self.touched = Some(Instant::now());
				self.anim = (!reduced_motion() && frames.len() > 1).then(|| Anim {
					key: answer.key,
					size,
					due: Instant::now() + Duration::from_millis(frames[0].1 as u64),
					at: 1 % frames.len(),
					frames,
				});
			}
			Payload::Pdf { page, total, size, rgba } => {
				self.upload(&answer.key, size, &rgba);
				self.pending = None;
				self.raw = None;
				self.show = Show::Pdf { key: answer.key, size, page, total };
				self.touched = Some(Instant::now());
			}
			Payload::Archive(listing) => {
				self.pending = None;
				self.raw = None;
				self.collapsed = archive::default_collapsed(&listing.items);
				self.show = Show::Archive { items: listing.items, doc: None, total: listing.total, truncated: listing.truncated };
				self.touched = Some(Instant::now());
			}
			Payload::Media(facts) => {
				self.pending = None;
				self.raw = None;
				self.show = Show::Media { facts };
				self.touched = Some(Instant::now());
			}
			Payload::Dir(entries) => {
				self.pending = None;
				self.raw = None;
				self.show = Show::Dir { doc: None, truncated: false, entries };
				self.touched = Some(Instant::now());
			}
			Payload::Text { text, truncated, images } => {
				self.pending = None;
				for (_, key, size, rgba) in &images {
					self.upload(key, *size, rgba);
				}

				self.raw = Some(Raw {
					path: self.path.clone().unwrap_or_default(),
					kind: self.kind,
					text,
					truncated,
					images: images.iter().map(|(src, key, size, _)| (src.clone(), key.clone(), *size)).collect(),
				});
				self.laid = None;
				self.laid_of = None;
			}
			Payload::Sheet(head) => {
				self.pending = None;
				self.raw = None;
				self.show = Show::Sheet { head };
				self.touched = Some(Instant::now());
			}
			Payload::Note(text) => {
				self.pending = None;
				self.raw = None;
				self.show = Show::Note(text);
			}
			Payload::Nothing => {
				self.pending = None;
				self.raw = None;
				self.show = Show::Placeholder(None);
			}
			Payload::Degraded(reason) => {
				self.pending = None;
				self.raw = None;
				self.show = Show::Placeholder(Some(reason));
			}
		}

		self.ctx.request_repaint();
	}

	fn upload(&mut self, key: &str, size: [u32; 2], rgba: &[u8]) {
		let image = ColorImage::from_rgba_unmultiplied([ size[0] as usize, size[1] as usize ], rgba);

		for evicted in self.lru.touch(key, rgba.len()) {
			self.textures.remove(&evicted);
		}

		match self.textures.get_mut(key) {
			Some(texture) => texture.set(image, TextureOptions::LINEAR),
			None => {
				let texture = self.ctx.load_texture(key.to_owned(), image, TextureOptions::LINEAR);
				self.textures.insert(key.to_owned(), texture);
			}
		}
	}

	fn pdf_page(&self) -> usize {
		match &self.show {
			Show::Pdf { page, .. } if self.path.is_some() => *page,
			_ => 0,
		}
	}

	pub(crate) fn scroll(&mut self, lines: f32) -> bool {
		if self.content() <= self.view {
			return false;
		}

		let before = self.offset;
		self.offset = (self.offset + lines * LINE_PX).max(0.0);
		crate::metrics::preview(before, self.offset);
		true
	}

	pub(crate) fn image_height(&self) -> Option<f32> {
		match &self.show {
			Show::Image { source, .. } if self.raw.is_none() => Some(self.shown_size(*source).y * self.zoom),
			_ => None,
		}
	}

	fn shown_size(&self, source: [u32; 2]) -> Vec2 {
		if self.target[0] == 0 || self.target[1] == 0 {
			return Vec2::ZERO;
		}

		let ppp = self.ctx.pixels_per_point().max(f32::MIN_POSITIVE);
		let [ width, height ] = img::fit_width((source[0], source[1]), self.target[0] as f32 / ppp);

		vec2(width, height)
	}

	fn image_target(&self) -> [u32; 2] {
		[ ((self.target[0] as f32 * self.zoom).round().max(1.0) as u32).max(1), 0 ]
	}

	pub(crate) fn fit_width_pixels(&self) -> Option<f32> {
		(self.target[0] > 0).then(|| self.target[0] as f32)
	}

	pub(crate) fn shown_physical(&self, path: &Path) -> Option<[f32; 2]> {
		let Show::Image { source, .. } = &self.show else { return None };
		if self.raw.is_some() || self.path.as_deref() != Some(path) {
			return None;
		}

		let ppp = self.ctx.pixels_per_point().max(f32::MIN_POSITIVE);
		let shown = self.shown_size(*source) * self.zoom;

		Some([ shown.x * ppp, shown.y * ppp ])
	}

	pub(crate) fn scroll_to(&mut self, offset: f32) { self.offset = offset.max(0.0); }

	pub(crate) fn seek(&mut self, step: i32) -> bool {
		match self.kind {
			Kind::Pdf if matches!(self.show, Show::Pdf { .. }) => {
				let page = self.pdf_page();
				let next = (page as i32 + step.signum()).max(0) as usize;
				if next == page {
					return true;
				}

				self.generation += 1;
				self.pending = None;
				self.queued = Some(Queued {
					generation: self.generation,
					path:       self.path.clone().unwrap_or_default(),
					kind:       Kind::Pdf,
					len:        self.len,
					mtime:      self.mtime,
					at:         Instant::now(),
				});
				true
			}
		Kind::Code | Kind::Markdown | Kind::Docx | Kind::Dir | Kind::Archive => {
				let before = self.offset;
				self.offset = (self.offset + step as f32 * LINE_PX).max(0.0);
				crate::metrics::preview(before, self.offset);
				true
			}
			_ => false,
		}
	}

	pub(crate) fn zoom_by(&mut self, up: bool) -> bool {
		if !matches!(self.show, Show::Image { .. } | Show::Pdf { .. }) {
			return false;
		}

		self.zoom = if up { self.zoom * 1.25 } else { self.zoom / 1.25 }.clamp(0.25, 8.0);
		self.touched = Some(Instant::now());
		self.ctx.request_repaint();
		true
	}

	pub(crate) fn zoom_fit(&mut self) -> bool {
		if !matches!(self.show, Show::Image { .. } | Show::Pdf { .. }) {
			return false;
		}

		self.zoom = 1.0;
		self.pan = Vec2::ZERO;
		self.touched = Some(Instant::now());
		self.ctx.request_repaint();
		true
	}

	pub(crate) fn zoom_full(&mut self) -> bool {
		let source = match &self.show {
			Show::Image { source, .. } => *source,
			Show::Pdf { .. } => return self.zoom_by(true),
			_ => return false,
		};

		let ppp = self.ctx.pixels_per_point();
		let shown = self.shown_size(source);
		if shown.x <= 0.0 {
			return false;
		}

		self.zoom = (source[0] as f32 / (shown.x * ppp).max(1.0)).clamp(0.25, 8.0);
		self.pan = Vec2::ZERO;
		self.touched = Some(Instant::now());
		self.ctx.request_repaint();
		true
	}

	pub(crate) fn pan_by(&mut self, delta: Vec2) {
		if self.zoomed() {
			self.pan += delta;
			self.touched = Some(Instant::now());
			return;
		}

		if matches!(self.show, Show::Image { .. }) && self.content() > self.view {
			self.offset = (self.offset - delta.y).clamp(0.0, (self.content() - self.view).max(0.0));
			self.touched = Some(Instant::now());
			self.ctx.request_repaint();
		}
	}

	pub(crate) fn zoomed(&self) -> bool {
		matches!(self.show, Show::Image { .. } | Show::Pdf { .. }) && self.zoom > 1.0
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn image_offset_for_tests(&self) -> Option<f32> {
		matches!(self.show, Show::Image { .. }).then_some(self.offset)
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn badge_for_tests(&self) -> Option<&str> {
		self.badge.as_deref()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn archive_rows_for_tests(&self) -> Option<usize> {
		let Show::Archive { items, .. } = &self.show else { return None };
		Some(archive::visible(items, &self.collapsed).len())
	}

	pub(crate) fn extent(&self) -> Option<(f32, f32, f32)> {
		let content = self.content();
		(content > self.view + 0.5).then_some((content, self.offset, self.view))
	}

	fn content(&self) -> f32 {
		match (&self.show, &self.raw) {
			(_, Some(_)) => self.laid.as_ref().map_or(PAD, |doc| doc.total + PAD),
			(Show::Sheet { .. }, None) => sheet::HEAD_HEIGHT + PAD,
			(Show::Dir { doc: Some(doc), .. } | Show::Archive { doc: Some(doc), .. }, _) => doc.total + PAD,
			(Show::Image { .. }, None) => self.image_height().map_or(PAD, |tall| tall + PAD),
			(Show::Pdf { size, .. }, None) => {
				let width = self.target[0].max(1) as f32;
				width * size[1] as f32 / size[0].max(1) as f32 + PAD
			}
			_ => 0.0,
		}
	}

	pub(crate) fn draw(&mut self, painter: &Painter, rect: Rect, tokens: &Tokens) {
		let clipped = painter.with_clip_rect(rect);
		let painter = &clipped;

		let content = content_of(rect);
		self.view = content.height().max(1.0);
		let ppp = self.ctx.pixels_per_point();
		self.target = [
			(content.width() * ppp).round().max(1.0) as u32,
			(content.height() * ppp).round().max(1.0) as u32,
		];

		self.retarget();

		let pointer = self.ctx.input(|i| i.pointer.latest_pos());
		if pointer.is_some_and(|at| rect.contains(at)) {
			self.touched = Some(Instant::now());
		}

		if matches!(self.show, Show::Image { .. }) && self.raw.is_none() {
			self.tick_animation();
		}

		match (&self.show, &self.raw) {
			(_, Some(_)) => self.draw_text(painter, content, tokens),
			(Show::Image { key, source, .. }, None) => {
				let (key, source) = (key.clone(), *source);
				self.draw_image(painter, content, &key, source, tokens);
			}
			(Show::Pdf { key, size, page, total }, None) => {
				let (key, size, page, total) = (key.clone(), *size, *page, *total);
				self.draw_pdf(painter, content, &key, size, page, total, rect, tokens);
			}
			(Show::Dir { .. } | Show::Archive { .. }, None) => self.draw_listing(painter, content, tokens),
			(Show::Sheet { .. }, None) => self.draw_sheet_head(painter, content, tokens),
			(Show::Media { facts }, None) => {
				let facts = facts.clone();
				self.draw_media(painter, content, &facts, tokens);
			}
			(Show::Placeholder(reason), None) => {
				let reason = reason.clone();
				Self::draw_placeholder(painter, content, self.path.as_deref(), reason.as_deref(), tokens);
			}
			(Show::Note(note), None) => {
				painter.text(
					content.min + vec2(0.0, 8.0),
					Align2::LEFT_TOP,
					note.clone(),
					FontId::new(16.0, FontFamily::Monospace),
					tokens.subtext,
				);
			}
			(Show::Empty, None) => {}
		}

		self.fades(painter, rect, tokens);

		if let Some(at) = self.pending
			&& at.elapsed() >= LOADING_DELAY
			&& !(matches!(self.kind, Kind::Dir | Kind::Archive)
				&& !matches!(self.show, Show::Dir { .. } | Show::Archive { .. }))
		{
			painter.text(
				pos2(content.min.x, content.max.y - 4.0),
				Align2::LEFT_BOTTOM,
				"载入中…",
				FontId::new(13.0, FontFamily::Monospace),
				tokens.disabled,
			);
		}
		self.dir_skeleton(painter, content, tokens);

		self.advance();
	}

	fn retarget(&mut self) {
		if !matches!(self.kind, Kind::Image | Kind::Pdf | Kind::Video) || self.queued.is_some() || self.pending.is_some() {
			return;
		}
		let Some(path) = self.path.clone() else { return };

		let current = match &self.show {
			Show::Image { key, .. } | Show::Pdf { key, .. } => key.clone(),
			_ => return,
		};

		let mut wanted = match self.kind {
			Kind::Image => img::key(path.to_str().unwrap_or_default(), self.mtime, self.image_target()),
			Kind::Video => img::key(path.to_str().unwrap_or_default(), self.mtime, [ self.target[0].max(1), 0 ]),
			_ => img::key(path.to_str().unwrap_or_default(), self.mtime, img::request_target(self.target, self.zoom)),
		};
		if self.kind == Kind::Pdf {
			wanted = format!("{wanted}#p{}", self.pdf_page());
		}
		if wanted == current {
			return;
		}

		self.generation += 1;
		self.queued = Some(Queued {
			generation: self.generation,
			path,
			kind: self.kind,
			len: self.len,
			mtime: self.mtime,
			at: Instant::now(),
		});
	}

	fn draw_image(
		&mut self,
		painter: &Painter,
		content: Rect,
		key: &str,
		source: [u32; 2],
		tokens: &Tokens,
	) {
		let Some(texture) = self.textures.get(key).map(|texture| texture.id()) else { return };

		let ppp = self.ctx.pixels_per_point();
		let shown = self.shown_size(source) * self.zoom;

		self.offset = self.offset.clamp(0.0, (shown.y - content.height()).max(0.0));
		let at = pos2(content.center().x - shown.x * 0.5 + self.pan.x, content.min.y + self.pan.y - self.offset);

		painter.image(
			texture,
			Rect::from_min_size(at, shown),
			Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
			Color32::WHITE,
		);

		let shown_phys = shown.x * ppp;
		let percent = (shown_phys / source[0].max(1) as f32 * 100.0).round().max(1.0) as u32;
		let label = if (self.zoom - 1.0).abs() < 0.001 {
			format!("{percent}% ⤢")
		} else {
			format!("{percent}% ×{:.2}", self.zoom)
		};
		self.note_badge(&label);
		self.badge(painter, content.right_bottom() - vec2(2.0, 2.0), Align2::RIGHT_BOTTOM, &label, tokens);
	}

	fn note_badge(&mut self, label: &str) {
		if self.badge.as_deref() == Some(label) {
			return;
		}
		self.badge = Some(label.to_owned());

		let file = self
			.path
			.as_deref()
			.map(|path| path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned()))
			.unwrap_or_default();
		crate::metrics::zoom(label, &file);
	}

	#[allow(clippy::too_many_arguments)]
	fn draw_pdf(
		&mut self,
		painter: &Painter,
		content: Rect,
		key: &str,
		size: [u32; 2],
		page: usize,
		total: usize,
		column: Rect,
		tokens: &Tokens,
	) {
		let Some(texture) = self.textures.get(key) else { return };

		let width = content.width() * self.zoom;
		let height = width * size[1] as f32 / size[0].max(1) as f32;
		let x = content.min.x - (width - content.width()) * 0.5 + self.pan.x;
		let page_rect = Rect::from_min_size(pos2(x, content.min.y - self.offset), vec2(width, height));

		painter.rect_filled(page_rect.expand(1.0), 0.0, tokens.border);
		painter.image(
			texture.id(),
			page_rect,
			Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
			Color32::WHITE,
		);

		self.badge(
			painter,
			pos2(column.center().x, column.max.y - PAD),
			Align2::CENTER_BOTTOM,
			&format!("{} / {total}", page + 1),
			tokens,
		);
	}

	fn draw_sheet_head(&mut self, painter: &Painter, content: Rect, tokens: &Tokens) {
		let Show::Sheet { head } = &self.show else { return };

		let clip = painter.with_clip_rect(content);
		let painter = &clip;
		let font = FontId::new(13.0, FontFamily::Monospace);
		let bold = FontId::new(13.0, FontFamily::Name("maple-bold".into()));
		let line = tokens.border;

		painter.text(
			pos2(content.min.x + 4.0, content.min.y + 12.0),
			Align2::LEFT_CENTER,
			crate::list::clipped(&head.name, 40),
			font.clone(),
			tokens.subtext,
		);

		let band = Rect::from_min_size(pos2(content.min.x, content.min.y + 26.0), vec2(content.width(), 28.0));
		painter.rect_filled(band, 0.0, tokens.raised);
		let mut x = band.min.x;
		for (i, w) in head.cols.iter().enumerate() {
			if i > 0 {
				painter.rect_filled(Rect::from_min_max(pos2(x, band.min.y + 4.0), pos2(x + 1.0, band.max.y - 4.0)), 0.0, line);
			}
			if let Some(label) = head.labels.get(i).filter(|label| !label.is_empty()) {
				let budget = (((*w - 10.0) / (13.0 * 0.62)).floor().max(1.0)) as u32;
				painter.text(pos2(x + 5.0, band.center().y), Align2::LEFT_CENTER, crate::list::clipped(label, budget), bold.clone(), tokens.text);
			}
			x += w;
		}
		painter.rect_filled(Rect::from_min_max(pos2(band.min.x, band.max.y - 1.0), pos2(band.max.x, band.max.y)), 0.0, tokens.border_strong);

		let summary = format!("{} 行 × {} 列 · 完整内容请用表格应用打开", head.rows, head.wide);
		painter.text(
			pos2(content.min.x + 4.0, band.max.y + 16.0),
			Align2::LEFT_CENTER,
			crate::list::clipped(&summary, 60),
			font.clone(),
			tokens.disabled,
		);
	}

	fn draw_listing(&mut self, painter: &Painter, content: Rect, tokens: &Tokens) {
		match &mut self.show {
			Show::Dir { entries, doc, truncated, .. } if doc.is_none() => {
				let (built, cut) = dir_doc(entries, tokens);
				*doc = Some(built);
				*truncated = cut;
			}
			Show::Archive { items, doc, total, truncated } if doc.is_none() => {
				let (built, cut) = archive_doc(items, *total, &self.collapsed, tokens);
				*doc = Some(built);
				*truncated = cut;
			}
			_ => {}
		}

		let (total, empty, label) = match &self.show {
			Show::Dir { doc: Some(doc), truncated, entries, .. } => {
				let count = entries.len();
				let label = if *truncated { format!("{count} 项（只列前 {DIR_CAP}）") } else { format!("{count} 项") };
				(doc.total, "（空目录）".to_owned(), label)
			}
			Show::Archive { doc: Some(doc), truncated, items, total } => {
				let count = items.len();
				let label = match total {
					Some(count) if *truncated => format!("{count} 个条目（只列前 {ARCHIVE_CAP}）"),
					Some(count) => format!("{count} 个条目"),
					None if *truncated => format!("{count}+ 个条目"),
					None => format!("{count} 个条目"),
				};
				(doc.total, "（空压缩包）".to_owned(), label)
			}
			_ => return,
		};

		let max = (total - content.height()).max(0.0);
		self.offset = self.offset.clamp(0.0, max);

		if let Some(doc) = self.listing_doc() {
			rows::draw(painter, content, doc, self.offset, tokens, None, false);
		}

		let font = FontId::new(13.0, FontFamily::Monospace);
		if self.offset > 0.5 {
			painter.text(pos2(content.max.x, content.min.y), Align2::RIGHT_TOP, "↑", font.clone(), tokens.subtext);
		}
		if self.offset < max - 0.5 {
			painter.text(pos2(content.max.x, content.max.y), Align2::RIGHT_BOTTOM, "↓", font, tokens.subtext);
		}

		if self.listing_count() == 0 {
			painter.text(content.center(), Align2::CENTER_CENTER, empty, FontId::new(16.0, FontFamily::Monospace), tokens.subtext);
		} else {
			self.badge(painter, pos2(content.center().x, content.max.y + PAD), Align2::CENTER_BOTTOM, &label, tokens);
		}
	}

	fn listing_doc(&self) -> Option<&Doc> {
		match &self.show {
			Show::Dir { doc, .. } | Show::Archive { doc, .. } => doc.as_ref(),
			_ => None,
		}
	}

	fn listing_count(&self) -> usize {
		match &self.show {
			Show::Dir { entries, .. } => entries.len(),
			Show::Archive { items, .. } => items.len(),
			_ => 0,
		}
	}

	pub(crate) fn archive_tree(&self, column: Rect) -> Option<Vec<(usize, Rect, bool)>> {
		let Show::Archive { items, doc: Some(doc), .. } = &self.show else { return None };
		let content = content_of(column);
		let visible = archive::visible(items, &self.collapsed);
		let mut rows = Vec::new();

		let mut at = doc.first_at(self.offset);
		while at < visible.len() {
			let y = content.min.y + doc.starts[at] - self.offset;
			if y > content.max.y {
				break;
			}
			if y + doc.rows[at].height >= content.min.y {
				let rect = Rect::from_min_max(
					pos2(column.min.x, y.max(column.min.y)),
					pos2(column.max.x, (y + doc.rows[at].height).min(column.max.y)),
				);
				rows.push((at, rect, items[visible[at]].dir));
			}
			at += 1;
		}

		Some(rows)
	}

	pub(crate) fn archive_fold_at(&mut self, column: Rect, at: Pos2) -> bool {
		let Some(rows) = self.archive_tree(column) else { return false };
		let Some((row, _, dir)) = rows.into_iter().find(|(_, rect, _)| rect.contains(at)) else { return false };

		dir && self.archive_fold(row)
	}

	pub(crate) fn archive_fold(&mut self, row: usize) -> bool {
		let Show::Archive { items, doc, .. } = &mut self.show else { return false };
		let Some(&at) = archive::visible(items, &self.collapsed).get(row) else { return false };
		if !items[at].dir {
			return false;
		}

		let path = items[at].path.clone();
		let folded = !self.collapsed.remove(&path);
		if folded {
			self.collapsed.insert(path.clone());
		}
		*doc = None;

		let file = self.path.as_deref().and_then(|path| path.file_name());
		crate::metrics::archive(file.map_or("", |name| name.to_str().unwrap_or_default()), &path, folded);
		true
	}

	fn dir_skeleton(&self, painter: &Painter, content: Rect, tokens: &Tokens) {
		if !matches!(self.kind, Kind::Dir | Kind::Archive) || matches!(self.show, Show::Dir { .. } | Show::Archive { .. }) {
			return;
		}
		let Some(at) = self.pending else { return };
		if at.elapsed() < LOADING_DELAY {
			return;
		}

		let rows_visible = (content.height() / ROW_HEIGHT).floor().max(1.0) as usize;
		let rows = chrome::skeleton_rows(None, rows_visible);
		for row in 0..rows {
			let top = content.min.y + row as f32 * ROW_HEIGHT;
			let icon = Rect::from_center_size(
				pos2(content.min.x + ICON_BOX * 0.5, top + ROW_HEIGHT * 0.5),
				Vec2::splat(ICON_BOX),
			);
			painter.rect_filled(icon, 4.0, tokens.raised);

			let width = chrome::skeleton_bar(row) * (content.width() - ICON_BOX - 48.0);
			let bar = Rect::from_min_size(pos2(content.min.x + ICON_BOX + 20.0, top + ROW_HEIGHT * 0.5 - 5.0), vec2(width, 10.0));
			painter.rect_filled(bar, 5.0, tokens.raised);
		}
	}

	fn badge(&self, painter: &Painter, anchor: Pos2, align: Align2, label: &str, tokens: &Tokens) {
		let alive = self.touched.is_some_and(|at| at.elapsed() < Duration::from_millis(1500));
		let alpha = self.ctx.animate_bool_with_time(Id::unique("crabhop-preview-badge"), alive, 0.2);
		if alpha <= 0.01 {
			return;
		}

		let font = FontId::new(13.0, FontFamily::Monospace);
		let galley = painter.layout_no_wrap(label.to_owned(), font.clone(), tokens.text);
		let size = galley.size() + vec2(16.0, 10.0);
		let chip = match align {
			Align2::RIGHT_BOTTOM => Rect::from_min_size(anchor - vec2(size.x, size.y), size),
			Align2::CENTER_BOTTOM => Rect::from_min_size(pos2(anchor.x - size.x * 0.5, anchor.y - size.y), size),
			_ => Rect::from_min_size(anchor, size),
		};

		painter.rect_filled(chip, 4.0, tokens.raised.gamma_multiply(0.8 * alpha));
		painter.text(chip.center(), Align2::CENTER_CENTER, label, font, tokens.text.gamma_multiply(alpha));
	}

	fn fades(&self, painter: &Painter, rect: Rect, tokens: &Tokens) {
		let top = self.offset > 0.5;
		let bottom = self.content() > self.view + self.offset + 0.5;

		if top {
			for strip in 0..2 {
				let alpha = (255.0 * (1.0 - (strip as f32 + 0.5) / 2.0)) as u8;
				painter.rect_filled(
					Rect::from_min_size(pos2(rect.min.x, rect.min.y + strip as f32 * 2.0), vec2(rect.width(), 2.5)),
					0.0,
					faded(tokens.crust, alpha),
				);
			}
		}

		if bottom {
			for strip in 0..4 {
				let alpha = (255.0 * ((strip as f32 + 0.5) / 4.0)) as u8;
				painter.rect_filled(
					Rect::from_min_size(pos2(rect.min.x, rect.max.y - 12.0 + strip as f32 * 3.0), vec2(rect.width(), 3.5)),
					0.0,
					faded(tokens.crust, alpha),
				);
			}
		}
	}

	fn draw_media(&self, painter: &Painter, content: Rect, facts: &[(String, String)], tokens: &Tokens) {
		let Some(path) = self.path.as_deref() else { return };
		let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
		let (glyph, colour) = list::icon_for(&name, false, false, tokens);
		let middle = content.center().x;
		let top = content.center().y - 24.0 - facts.len() as f32 * 10.0;

		painter.text(pos2(middle, top - 60.0), Align2::CENTER_CENTER, glyph, FontId::new(48.0, FontFamily::Monospace), colour);
		painter.text(
			pos2(middle, top - 16.0),
			Align2::CENTER_CENTER,
			list::clipped(&name, 30).to_owned(),
			FontId::new(16.0, FontFamily::Monospace),
			tokens.text,
		);

		let font = FontId::new(13.0, FontFamily::Monospace);
		for (i, (key, value)) in facts.iter().enumerate() {
			let y = top + i as f32 * 20.0;
			painter.text(pos2(middle - 6.0, y), Align2::RIGHT_TOP, key.clone(), font.clone(), tokens.disabled);
			painter.text(pos2(middle + 6.0, y), Align2::LEFT_TOP, value.clone(), font.clone(), tokens.subtext);
		}

		painter.text(
			pos2(middle, top + facts.len() as f32 * 20.0 + 18.0),
			Align2::CENTER_CENTER,
			"按 Enter 打开",
			FontId::new(13.0, FontFamily::Monospace),
			tokens.disabled,
		);
	}

	fn draw_placeholder(painter: &Painter, rect: Rect, path: Option<&Path>, reason: Option<&str>, tokens: &Tokens) {
		let Some(path) = path else { return };
		let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
		let exec = std::fs::symlink_metadata(path).is_ok_and(|meta| {
			use std::os::unix::fs::PermissionsExt;
			meta.permissions().mode() & 0o111 != 0
		});
		let (glyph, colour) = list::icon_for(&name, false, exec, tokens);
		let middle = rect.center().x;

		painter.text(
			pos2(middle, rect.center().y - 40.0),
			Align2::CENTER_CENTER,
			glyph,
			FontId::new(48.0, FontFamily::Monospace),
			colour,
		);
		painter.text(
			pos2(middle, rect.center().y + 16.0),
			Align2::CENTER_CENTER,
			list::clipped(&name, 30).to_owned(),
			FontId::new(16.0, FontFamily::Monospace),
			tokens.text,
		);
		painter.text(
			pos2(middle, rect.center().y + 40.0),
			Align2::CENTER_CENTER,
			"按 Enter 打开",
			FontId::new(13.0, FontFamily::Monospace),
			tokens.disabled,
		);

		if let Some(reason) = reason {
			painter.text(
				pos2(middle, rect.center().y + 62.0),
				Align2::CENTER_CENTER,
				reason,
				FontId::new(13.0, FontFamily::Monospace),
				tokens.err,
			);
		}
	}

	fn draw_text(&mut self, painter: &Painter, content: Rect, tokens: &Tokens) {
		let raw = self.raw.as_ref().expect("checked by the caller");
		let width = content.width();
		let key = format!("{}|{}", raw.path.display(), (width / 16.0).round() as u32);
		if self.laid.is_none() || self.laid_of.as_deref() != Some(key.as_str()) {
			let sizes: HashMap<String, [u32; 2]> =
				raw.images.iter().map(|(src, _, size)| (src.clone(), *size)).collect();
			let (doc, notice) = lay(raw.kind, &raw.text, width, tokens, raw.truncated, &sizes);
			self.laid = Some(doc);
			self.notice = notice;
			self.laid_of = Some(key);
		}

		if raw.kind == Kind::Code {
			let file_key = format!(
				"{}|{}",
				raw.path.display(),
				self.mtime.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis())
			);
			if self.hl.as_ref().is_none_or(|hl| hl.key != file_key) {
				let ext = raw.path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_owned();
				self.hl = Some(Hl { key: file_key, syntax: code::Syntax::new(&ext), row: 0 });
			}

			let end = self
				.laid
				.as_ref()
				.map_or(0, |doc| doc.first_at(self.offset + content.height() + LINE_PX) + 2);
			if let (Some(doc), Some(hl)) = (&mut self.laid, &mut self.hl) {
				let mut row = hl.row;
				while row < end.min(doc.rows.len()).min(code::MAX_HIGHLIGHTED) {
					if doc.rows[row].number.is_some() {
						let (full, len) = full_line(doc, row);
						let runs: Vec<(std::ops::Range<usize>, Color32)> = hl.syntax.next(&full);
						let mut at = 0;

						for piece in 0..len {
							let width = doc.rows[row + piece].text.len();
							doc.rows[row + piece].runs = slice_runs(&runs, at, width);
							at += width;
						}
						row += len;
					} else {
						row += 1;
					}
				}
				hl.row = row;
			}
		}

		let Some(doc) = &self.laid else { return };
		let max = (doc.total - content.height()).max(0.0);
		self.offset = self.offset.clamp(0.0, max);
		let code = raw.kind == Kind::Code;

		rows::draw(painter, content, doc, self.offset, tokens, None, !code);

		if !raw.images.is_empty() {
			let ppp = self.ctx.pixels_per_point();
			for (i, row) in doc.rows.iter().enumerate() {
				let Some(src) = &row.image else { continue };
				let y = content.min.y + doc.starts[i] - self.offset;
				if y > content.max.y {
					break;
				}
				if y + row.height < content.min.y {
					continue;
				}

				let Some((_, key, _)) = raw.images.iter().find(|(written, _, _)| written == src) else { continue };
				let Some(texture) = self.textures.get(key) else { continue };
				let pixels = texture.size_vec2() / ppp;
				let box_ = (content.width() * 0.5, (row.height - 12.0).max(1.0));
				let to = img::contain((pixels.x.max(1.0) as u32, pixels.y.max(1.0) as u32), [ box_.0 as u32, box_.1 as u32 ]);
				let rect = Rect::from_center_size(pos2(content.min.x + box_.0 * 0.5, y + row.height * 0.5), vec2(to.0 as f32, to.1 as f32));
				painter.image(texture.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
			}
		}

		let font = FontId::new(13.0, FontFamily::Monospace);
		if self.offset > 0.5 {
			painter.text(pos2(content.max.x, content.min.y), Align2::RIGHT_TOP, "↑", font.clone(), tokens.subtext);
		}
		if self.offset < max - 0.5 {
			painter.text(pos2(content.max.x, content.max.y), Align2::RIGHT_BOTTOM, "↓", font, tokens.subtext);
		}

		if let Some(notice) = self.notice {
			let bar = Rect::from_min_size(
				pos2(content.min.x - PAD, content.max.y + 6.0),
				vec2(content.width() + PAD + PAD_RIGHT, 24.0),
			);
			painter.rect_filled(bar, 0.0, tokens.raised);
			painter.text(
				bar.left_center() + vec2(12.0, 0.0),
				Align2::LEFT_CENTER,
				notice,
				FontId::new(13.0, FontFamily::Monospace),
				tokens.subtext,
			);
		}
	}
}

pub(crate) fn warm_pdf() { pdf::warm(); }

pub(crate) fn picture_facts(path: &Path) -> Option<(img::Report, [u32; 2])> {
	use std::io::Read;

	let mut head = Vec::with_capacity(64 * 1024);
	std::fs::File::open(path).ok()?.take(64 * 1024).read_to_end(&mut head).ok()?;
	let (width, height) = img::dimensions(&head)?;

	Some((img::header_report(&head), [ width, height ]))
}

pub(crate) fn file_uri(path: &Path) -> String {
	thumb::file_uri(&path.to_string_lossy())
}

fn faded(colour: Color32, alpha: u8) -> Color32 {
	Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), alpha)
}

pub(crate) fn image_rows(report: &img::Report, source: [u32; 2], shown_width: Option<f32>) -> Vec<(String, String)> {
	let mut rows = Vec::new();

	if let Some(format) = &report.format {
		rows.push(("格式".to_owned(), format.clone()));
	}
	rows.push(("尺寸".to_owned(), format!("{} × {} 像素", source[0], source[1])));

	if let Some(shown_width) = shown_width {
		let percent = (shown_width / source[0].max(1) as f32 * 100.0).round().max(1.0) as u32;
		rows.push(("显示比例".to_owned(), format!("{percent}%")));
	}
	if let Some(colour) = &report.colour {
		rows.push(("色深".to_owned(), colour.clone()));
	}

	rows
}

fn lay(kind: Kind, text: &str, width: f32, tokens: &Tokens, truncated: bool, images: &HashMap<String, [u32; 2]>) -> (Doc, Option<&'static str>) {
	const TRUNCATED: &str = "文件较大，仅预览前 2000 行（Enter 打开全文）";
	let notice = (truncated && matches!(kind, Kind::Code | Kind::Markdown)).then_some(TRUNCATED);

	match kind {
		Kind::Markdown => (md::layout(text, width, tokens, images), notice),
		Kind::Code => {
			let diff = text.lines().next().is_some_and(|line| line.starts_with("diff --git") || line.starts_with("--- "))
				&& text.contains("+++");
			let ok = tokens.ok.gamma_multiply(0.12);
			let err = tokens.err.gamma_multiply(0.12);

			let lines: Vec<&str> = text.lines().take(code::TRUNCATE_LINES).collect();
			let cols = code_columns(width, digits_of(lines.len()));

			let mut rows = Vec::new();
			for (i, line) in lines.iter().enumerate() {
				let pieces = wrap_plain(line, cols);
				let last = pieces.len().saturating_sub(1);
				let bg = if diff {
					line.starts_with('+').then_some(ok).or_else(|| line.starts_with('-').then_some(err))
				} else {
					None
				};

				for (p, piece) in pieces.into_iter().enumerate() {
					rows.push(Row {
						text: piece,
						number: (p == 0).then_some(i + 1),
						size: 16.0,
						height: 24.0,
						colour: tokens.text,
						bg: (p == 0 || p == last).then_some(bg).flatten(),
						..Default::default()
					});
				}
			}

			(Doc::new(rows), notice)
		}
		Kind::Docx | Kind::Sheet => {
			let cols = ((width / (16.0 * 0.62)).floor() as u32).max(8);
			let rows = text
				.lines()
				.flat_map(|line| wrap_plain(line, cols))
				.map(|line| Row::new(line, 16.0, tokens.text))
				.collect();

			(Doc::new(rows), None)
		}
		_ => (Doc::new(Vec::new()), None),
	}
}

const DIR_CAP: usize = 20_000;

const ARCHIVE_CAP: usize = archive::CAP;

fn archive_doc(items: &[archive::Item], total: Option<usize>, collapsed: &HashSet<String>, tokens: &Tokens) -> (Doc, bool) {
	let truncated = total.is_some_and(|count| count > items.len()) || items.len() >= ARCHIVE_CAP;
	let mut rows = archive::tree(items, collapsed, tokens);

	if truncated {
		let note = match total {
			Some(count) => format!("…共 {count} 个条目（只列前 {ARCHIVE_CAP}）"),
			None => format!("…还有更多条目（只列前 {ARCHIVE_CAP}）"),
		};
		rows.push(Row {
			text: note,
			size: 13.0,
			height: 24.0,
			indent: 8.0,
			colour: tokens.disabled,
			..Default::default()
		});
	}

	(Doc::new(rows), truncated)
}

fn dir_doc(entries: &[Entry], tokens: &Tokens) -> (Doc, bool) {
	let truncated = entries.len() > DIR_CAP;
	let mut rows: Vec<Row> = entries.iter().take(DIR_CAP).map(|entry| dir_row(entry, tokens)).collect();

	if truncated {
		rows.push(Row {
			text: format!("…共 {} 项，此处只列前 {DIR_CAP} 项", entries.len()),
			size: 13.0,
			height: 24.0,
			colour: tokens.disabled,
			..Default::default()
		});
	}

	(Doc::new(rows), truncated)
}

fn dir_row(entry: &Entry, tokens: &Tokens) -> Row {
	let (glyph, colour) = list::icon_for(&entry.name, entry.dir, entry.mode.is_some_and(|mode| mode & 0o111 != 0), tokens);

	Row {
		text: entry.name.clone(),
		height: ROW_HEIGHT,
		indent: ICON_BOX + 2.0 * rows::advance(16.0),
		size:   16.0,
		colour: if entry.dir { tokens.blue } else { tokens.text },
		lead:   Some((glyph.to_owned(), colour)),
		..Default::default()
	}
}

fn full_line(doc: &Doc, first: usize) -> (String, usize) {
	let mut full = doc.rows[first].text.clone();
	let mut pieces = 1usize;

	while let Some(next) = doc.rows.get(first + pieces)
		&& next.number.is_none()
	{
		full.push_str(&next.text);
		pieces += 1;
	}

	(full, pieces)
}

fn slice_runs(runs: &[(std::ops::Range<usize>, Color32)], offset: usize, len: usize) -> Vec<Run> {
	let window = offset..offset + len;

	runs.iter()
		.filter_map(|(range, colour)| {
			let start = range.start.max(window.start);
			let end = range.end.min(window.end);
			(start < end).then(|| Run {
				range:  (start - offset)..(end - offset),
				colour: *colour,
				size:   16.0,
				bold:   false,
				bg:     None,
			})
		})
		.collect()
}

fn digits_of(count: usize) -> usize { count.max(1).to_string().len() }

fn code_columns(width: f32, digits: usize) -> u32 {
	let gutter = rows::gutter(digits, rows::advance(13.0)).border;

	(((width - gutter) / rows::advance(16.0)).floor() as u32).max(4)
}

fn wrap_plain(line: &str, cols: u32) -> Vec<String> {
	fn cells(c: char) -> u32 { list::cell_width(c) }
	fn width(line: &str) -> u32 { line.chars().map(cells).sum() }

	let cols = cols.max(1);
	if width(line) <= cols {
		return vec![ line.to_owned() ];
	}

	let mut out = Vec::new();
	let (mut start, mut w, mut space) = (0, 0, None);

	for (i, c) in line.char_indices() {
		let cw = cells(c);

		if w + cw > cols && i > start {
			let cut = match space {
				Some(s) => s + 1,
				None => i,
			};

			out.push(line[start..cut].to_owned());
			start = cut;
			w = line[start..i].chars().map(cells).sum();
			space = None;
		}

		if c == ' ' {
			space = Some(i);
		}
		w += cw;
	}

	out.push(line[start..].to_owned());
	out
}

fn run(ctx: &Context, job: Look, tx: Sender<Answer>) {
	let Look { generation, path, kind, len, mtime, target, page } = job;
	let started = Instant::now();
	let name = path.to_str().unwrap_or_default().to_owned();
	let key = img::key(&name, mtime, target);
	let mut road: Option<&'static str> = None;
	let send = |payload: Payload| {
		let _ = tx.send(Answer { generation, key: key.clone(), payload });
	};

	match kind {
		Kind::Image => {
			let Ok(bytes) = std::fs::read(&path) else {
				send(Payload::Nothing);
				ctx.request_repaint();
				return;
			};

			let header = img::dimensions(&bytes);
			if name.ends_with(".gif")
				&& header.is_none_or(|(w, h)| !img::too_big(w, h)) // the header is asked first
				&& let Some((size, source, frames)) = img::decode_frames(target, &bytes)
			{
				let rgba = frames[0].0.clone();
				send(Payload::Image { size, source, rgba, frames });
				answered(kind, started, None, &name);
				ctx.request_repaint();
				return;
			}

			match img::decode_into(&name, mtime, len, &bytes, target, |from, size, source, rgba| {
				road = Some(match from {
					img::Road::Bucket => "bucket",
					img::Road::Thumbnail => "thumbnail",
					img::Road::Decode => "decode",
				});
				send(Payload::Image { size, source, rgba, frames: Vec::new() });
			}) {
				img::Outcome::TooBig => send(Payload::Note(img::NOTE_TOO_BIG.to_owned())),
				img::Outcome::Unreadable => send(Payload::Nothing),
				img::Outcome::Shipped => {}
			}
		}
		Kind::Pdf => {
			let Ok(bytes) = std::fs::read(&path) else {
				send(Payload::Nothing);
				ctx.request_repaint();
				return;
			};

			if let Some((size, total, rgba)) = img::pdf_bucket_load(&name, mtime, target, page) {
				road = Some("bucket");
				let _ = tx.send(Answer {
					generation,
					key: format!("{key}#p{page}"),
					payload: Payload::Pdf { page, total, size, rgba },
				});
			} else {
				match pdf::render(&bytes, target, page) {
					Some(render) => {
						img::pdf_bucket_store(&name, mtime, target, render.page, (render.size, render.total, &render.rgba));
						let _ = tx.send(Answer {
							generation,
							key: format!("{key}#p{}", render.page),
							payload: Payload::Pdf {
								page:  render.page,
								total: render.total,
								size:  render.size,
								rgba:  render.rgba,
							},
						});
					}
					None => match pdf::Pdf::extract(&bytes) {
						Some(text) => send(Payload::Text { text, truncated: false, images: Vec::new() }),
						None => send(Payload::Note(pdf::NOTE_FAILED.to_owned())),
					},
				}
			}
		}
		Kind::Markdown | Kind::Code => {
			if let Some(text) = text_cache_load(&name, mtime) {
				let images = if kind == Kind::Markdown { inline_images(&path, &text) } else { Vec::new() };
				send(Payload::Text { text, truncated: len > code::TRUNCATE_BYTES, images });
				answered(kind, started, Some("text"), &name);
				ctx.request_repaint();
				return;
			}

			let Some((bytes, _)) = head(&path, HEAD) else {
				send(Payload::Nothing);
				ctx.request_repaint();
				return;
			};

			if bytes.iter().take(8192).any(|byte| *byte == 0) {
				send(Payload::Nothing);
			} else {
				let text = String::from_utf8_lossy(&bytes).into_owned();
				let images = if kind == Kind::Markdown { inline_images(&path, &text) } else { Vec::new() };
				text_cache_store(&name, mtime, &text);
				send(Payload::Text { text, truncated: len > code::TRUNCATE_BYTES, images });
			}
		}
		Kind::Docx | Kind::Sheet => {
			if let Some(text) = text_cache_load(&name, mtime) {
				send(Payload::Text { text, truncated: false, images: Vec::new() });
				answered(kind, started, Some("text"), &name);
				ctx.request_repaint();
				return;
			}

			let Some((bytes, _)) = head(&path, DOC_CAP) else {
				send(Payload::Nothing);
				ctx.request_repaint();
				return;
			};

			match kind {
				Kind::Docx => match docx::Docx::extract(&bytes) {
					Some(text) => {
						text_cache_store(&name, mtime, &text);
						send(Payload::Text { text, truncated: false, images: Vec::new() });
					}
					None => send(Payload::Nothing),
				},
				_ => match sheet::Sheet::read(&bytes) {
					Some(head) => send(Payload::Sheet(head)),
					None => send(Payload::Nothing),
				},
			}
		}
		Kind::Video => {
			match video::frame(&path, mtime, target) {
				Some((size, source, rgba)) => {
					send(Payload::Image { size, source, rgba, frames: Vec::new() });
				}
				None => {
					let facts = video::media(&path, mtime).map_or_else(Vec::new, |media| media.rows(Some(target[0] as f32)));
					crate::metrics::video(&name, if facts.is_empty() { "none" } else { "card" });
					send(Payload::Media(facts));
				}
			}
		}
		Kind::Dir => send(Payload::Dir(crate::folder::read_sync(&path))),
		Kind::Archive => match archive::read(&path) {
			Ok(listing) => send(Payload::Archive(listing)),
			Err(archive::Fail::Encrypted) => send(Payload::Degraded("压缩包已加密，无法预览内容".to_owned())),
			Err(archive::Fail::Unreadable) => send(Payload::Degraded("压缩包损坏，无法读取内容".to_owned())),
			Err(archive::Fail::Unsupported(format)) => {
				send(Payload::Degraded(format!("{format} 压缩包暂不支持预览（无纯 Rust 解码器）")))
			}
		},
		Kind::Placeholder => send(Payload::Nothing),
	}

	answered(kind, started, road, &name);
	ctx.request_repaint();
}

fn answered(kind: Kind, started: Instant, road: Option<&str>, name: &str) {
	if !crate::metrics::enabled() {
		return;
	}

	let road = road.map_or(String::new(), |road| format!(" from {road}"));
	let file = Path::new(name).file_name().map_or_else(|| name.to_owned(), |file| file.to_string_lossy().into_owned());
	eprintln!("Timing: preview {kind:?} answered in {:.0}ms{road} {file}", started.elapsed().as_secs_f64() * 1000.0);
}

const INLINE_MAX: u32 = 1024;

const INLINE_BYTES: u64 = 16 * 1024 * 1024;

fn inline_images(doc: &Path, text: &str) -> Vec<(String, String, [u32; 2], Vec<u8>)> {
	let parent = doc.parent().unwrap_or(Path::new("/"));
	let mut done = Vec::new();

	for src in md::images(text) {
		if md::is_remote(&src) {
			continue;
		}

		let path = if Path::new(&src).is_absolute() { PathBuf::from(&src) } else { parent.join(&src) };
		let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
		if meta.len() > INLINE_BYTES {
			continue;
		}

		let key = format!("md:{}", path.display());
		let Some((size, rgba)) = inline_picture(&path, meta.len(), meta.modified().ok()) else { continue };
		done.push((src, key, size, rgba));
	}

	done
}

fn inline_picture(path: &Path, len: u64, mtime: Option<SystemTime>) -> Option<([u32; 2], Vec<u8>)> {
	let decode = |bytes: &[u8]| -> Option<([u32; 2], Vec<u8>)> {
		let picture = image::load_from_memory(bytes).ok()?.to_rgba8();
		let size = [ picture.width(), picture.height() ];
		let to = img::fit((size[0], size[1]), [ INLINE_MAX, INLINE_MAX ]);
		let rgba = img::resample(picture.as_raw(), size, [ to.0, to.1 ])?;

		Some(([ to.0, to.1 ], rgba))
	};

	if let Some(png) = thumb::Thumb::of(&path.to_string_lossy(), mtime, len).load()
		&& let Some(answer) = decode(&png)
	{
		return Some(answer);
	}

	let bytes = std::fs::read(path).ok()?;
	decode(&bytes)
}

const SWEEP_IDLE: Duration = Duration::from_millis(500);

fn image_jobs(rows: &[(PathBuf, Option<Entry>)]) -> Vec<(PathBuf, u64, Option<SystemTime>)> {
	rows.iter()
		.filter(|(path, entry)| Kind::of(path, entry.as_ref().is_some_and(|e| e.dir)) == Kind::Image)
		.map(|(path, entry)| {
			let (len, mtime) = entry.as_ref().map_or((0, None), |e| (e.size, e.modified));
			(path.clone(), len, mtime)
		})
		.collect()
}

fn preload_slot(slot: Arc<Semaphore>) -> Option<tokio::sync::OwnedSemaphorePermit> {
	for _ in 0..600 {
		if let Ok(permit) = slot.clone().try_acquire_owned() {
			return Some(permit);
		}
		std::thread::sleep(Duration::from_millis(25));
	}

	None
}

fn text_cache_at(path: &str, mtime: Option<SystemTime>) -> Option<PathBuf> {
	let mtime = mtime?;
	let millis = mtime.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_millis();
	let dir = crate::config::Config::cache("preview-text")?;

	Some(dir.join(format!("{}.txt", thumb::md5_hex(format!("{path}|{millis}").as_bytes()))))
}

fn text_cache_load(path: &str, mtime: Option<SystemTime>) -> Option<String> {
	let at = text_cache_at(path, mtime)?;
	std::fs::read(at).ok().map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

fn text_cache_store(path: &str, mtime: Option<SystemTime>, text: &str) {
	let Some(at) = text_cache_at(path, mtime) else { return };
	let Some(dir) = at.parent() else { return };
	if crate::config::Config::ensure(dir).is_err() {
		return;
	}

	let tmp = img::unique_temp(&at, dir);
	if std::fs::write(&tmp, text.as_bytes()).is_ok() {
		use std::os::unix::fs::PermissionsExt;
		let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
		let _ = std::fs::rename(&tmp, &at);
	}
}

fn run_warm(path: &Path, len: u64, mtime: Option<SystemTime>, target: [u32; 2]) {
	let Ok(bytes) = std::fs::read(path) else { return };

	let _ = img::warm(path.to_str().unwrap_or_default(), mtime, len, &bytes, target);
}

fn head(path: &Path, cap: usize) -> Option<(Vec<u8>, bool)> {
	use std::io::Read;

	let file = std::fs::File::open(path).ok()?;
	let mut bytes = Vec::new();
	let read = file.take(cap as u64 + 1).read_to_end(&mut bytes).ok()?;
	let complete = read <= cap;
	bytes.truncate(cap);

	Some((bytes, complete))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_file_is_classified_by_what_it_is() {
		assert_eq!(Kind::of(Path::new("/a.png"), false), Kind::Image);
		assert_eq!(Kind::of(Path::new("/a.PDF"), false), Kind::Pdf);
		assert_eq!(Kind::of(Path::new("/a.docx"), false), Kind::Docx);
		assert_eq!(Kind::of(Path::new("/a.xlsx"), false), Kind::Sheet);
		assert_eq!(Kind::of(Path::new("/a.md"), false), Kind::Markdown);
		assert_eq!(Kind::of(Path::new("/a.rs"), false), Kind::Code);
		assert_eq!(Kind::of(Path::new("/a.avif"), false), Kind::Placeholder, "no decoder is built for it");
		assert_eq!(Kind::of(Path::new("/a-dir"), true), Kind::Dir, "a directory lists itself");
		assert_eq!(Kind::of(Path::new("/a.zip"), false), Kind::Archive);
		assert_eq!(Kind::of(Path::new("/a.tar.zst"), false), Kind::Archive);
		assert_eq!(Kind::of(Path::new("/a.7z"), false), Kind::Archive, "7z is named, then refused with a reason");
		assert_eq!(Kind::of(Path::new("/a.rar"), false), Kind::Archive);
		assert_eq!(Kind::of(Path::new("/a.bin"), false), Kind::Code, "no reader: it is read before it is judged");
	}

	#[test]
	fn a_wrapped_line_fits_the_column_and_loses_nothing() {
		let line = "你好 crabhop！这是 docx 预览测试，第二段：中文与 English 混排 A 单元格A";
		let cols = 12;

		let pieces = wrap_plain(line, cols);
		assert!(pieces.len() > 1, "a line this long cannot fit one row");
		for piece in &pieces {
			assert!(piece.chars().map(list::cell_width).sum::<u32>() <= cols, "{piece:?} is wider than the column");
		}
		assert_eq!(pieces.concat(), line, "not a character may be lost to the wrap");
	}

	#[test]
	fn latin_breaks_at_the_spaces_and_a_long_word_is_cut_where_it_stands() {
		assert_eq!(wrap_plain("one two three four five", 9), vec![ "one two ", "three ", "four five" ]);
		assert_eq!(wrap_plain("supercalifragilistic", 8), vec![ "supercal", "ifragili", "stic" ]);
	}

	#[test]
	fn each_textual_kind_lays_out_its_own_way() {
		let tokens = Tokens::MOCHA;

		let (doc, notice) = lay(Kind::Code, "fn main() {}\nlet x = 1;", 400.0, &tokens, false, &HashMap::new());
		assert_eq!(doc.rows.len(), 2);
		assert_eq!(doc.rows[0].number, Some(1));
		assert_eq!(doc.rows[1].number, Some(2));
		assert!(doc.gutter, "code carries its line numbers");
		assert!(notice.is_none());

		let (doc, notice) = lay(Kind::Code, "a\nb", 400.0, &tokens, true, &HashMap::new());
		assert!(notice.is_some(), "a truncated file says so");
		assert_eq!(doc.rows.len(), 2, "and still lays its lines out");

		let (doc, _) = lay(Kind::Docx, "一句话被折到几个行盒子里", 80.0, &tokens, false, &HashMap::new());
		assert!(doc.rows.len() > 1, "the docx text wraps to the column");
		assert!(!doc.gutter);

		let (doc, _) = lay(Kind::Markdown, "# 标题\n\n正文", 400.0, &tokens, false, &HashMap::new());
		assert_eq!(doc.rows[0].size, 22.0, "the markdown ladder is used");

		let (doc, _) = lay(Kind::Code, "diff --git a/x b/x\n--- a/x\n+++ b/x\n+added\n-removed", 400.0, &tokens, false, &HashMap::new());
		assert!(doc.rows[3].bg.is_some(), "the + line has a band");
		assert!(doc.rows[4].bg.is_some(), "and so does the - line");
	}

	#[test]
	fn a_wrapped_documents_pieces_are_all_inside_the_scrollable_extent() {
		let tokens = Tokens::MOCHA;
		let text = "第一段：一封很长的文档，中文与 English 混排，用来把行折成几个 pieces。\n".repeat(40);

		let (doc, _) = lay(Kind::Docx, &text, 200.0, &tokens, false, &HashMap::new());
		assert!(doc.rows.len() > 40, "the text wraps into more rows than it has lines: {}", doc.rows.len());
		assert_eq!(doc.total, doc.rows.len() as f32 * LINE_PX, "every piece is a line box of its own");
		assert!(doc.total > 800.0, "and the extent covers the whole document: {}", doc.total);

		let last = doc.rows.len() - 1;
		assert_eq!(doc.starts[last] + doc.rows[last].height, doc.total, "the extent ends with the last piece");
	}

	#[test]
	fn a_picture_is_shown_at_the_columns_width_and_tall_ones_scroll() {
		let mut pane = PreviewPane { show: image("k", [ 200, 1600 ]), target: [ 400, 900 ], view: 900.0, ..PreviewPane::default() };

		let height = pane.image_height().expect("a picture's own height");
		assert!((height - 3200.0).abs() < 0.01, "the column's width, the file's shape: {height}");
		assert!(pane.content() > pane.view, "a tall picture is a page, not a fit");
		assert!(pane.scroll(3.0), "and the wheel walks it");

		pane.show = image("k", [ 800, 200 ]);
		let height = pane.image_height().expect("height");
		assert!((height - 100.0).abs() < 0.01, "800 wide at the column's 400 gives 100 tall: {height}");
		assert!(pane.content() <= pane.view && !pane.scroll(3.0), "there is nothing for the wheel to walk");

		pane.show = image("k", [ 100, 60 ]);
		let height = pane.image_height().expect("height");
		assert!((height - 240.0).abs() < 0.01, "400 wide from a 100 × 60 file: {height}");
	}

	#[test]
	fn a_zoom_rides_on_top_of_the_fit_and_can_pass_it() {
		let mut pane = PreviewPane { show: image("k", [ 200, 800 ]), target: [ 400, 900 ], view: 720.0, ..PreviewPane::default() };

		let fit = pane.image_height().expect("the fit");
		assert!(pane.zoom_by(true));
		assert!((pane.image_height().expect("zoomed") - fit * 1.25).abs() < 0.01, "a step multiplies the fit");
		assert!(pane.zoomed(), "and the column knows a picture may now be dragged");

		assert!(pane.zoom_full());
		let ppp = pane.ctx.pixels_per_point();
		let shown = pane.shown_size([ 200, 800 ]);
		assert!(((shown.x * pane.zoom * ppp) - 200.0).abs() < 0.01, "100% is one file pixel a screen pixel");

		assert!(pane.zoom_fit());
		assert!((pane.image_height().expect("back to the fit") - fit).abs() < 0.01);
		assert!(!pane.zoomed());
	}

	#[test]
	fn the_wheel_walks_a_line_at_a_time_and_stops_when_there_is_nothing_to_walk() {
		let tokens = Tokens::MOCHA;
		let raw = Raw {
			path: PathBuf::from("/tmp/x.docx"),
			kind: Kind::Docx,
			text: "a\n".repeat(100),
			truncated: false,
			images: Vec::new(),
		};
		let mut pane = PreviewPane { kind: Kind::Docx, raw: Some(raw), view: 240.0, ..PreviewPane::default() };
		pane.laid = Some(lay(Kind::Docx, &"a\n".repeat(100), 200.0, &tokens, false, &HashMap::new()).0);

		assert!(pane.scroll(3.0), "three lines deeper");
		assert_eq!(pane.offset, 3.0 * LINE_PX);
		assert!(pane.scroll(-1.0));
		assert_eq!(pane.offset, 2.0 * LINE_PX, "and back up");
		assert!(pane.scroll(-9.0));
		assert_eq!(pane.offset, 0.0, "the head is where a backward wheel stops");

		pane.view = pane.content() + 1.0;
		assert!(!pane.scroll(3.0), "no extent, no wheel");
		assert_eq!(pane.offset, 0.0);
	}

	#[test]
	fn a_seek_walks_the_specs_five_lines_and_no_further() {
		let raw = Raw { path: PathBuf::from("/tmp/x.docx"), kind: Kind::Docx, text: "a\n".repeat(200), truncated: false, images: Vec::new() };
		let mut pane = PreviewPane { kind: Kind::Docx, raw: Some(raw), view: 240.0, ..PreviewPane::default() };

		assert!(pane.seek(5));
		assert_eq!(pane.offset, 5.0 * LINE_PX, "five lines, not five times five");
		assert!(pane.seek(5));
		assert_eq!(pane.offset, 10.0 * LINE_PX);
		assert!(pane.seek(-5));
		assert_eq!(pane.offset, 5.0 * LINE_PX, "and back up the same five");
		assert!(pane.seek(-5) && pane.seek(-5));
		assert_eq!(pane.offset, 0.0, "the head is where a backward seek stops");

		pane.kind = Kind::Image;
		assert!(!pane.seek(5), "a picture has no lines to walk");
	}

	#[test]
	fn a_hover_is_queued_and_not_shown_before_its_answer() {
		let mut pane = PreviewPane::default();
		pane.look(Path::new("/tmp/crabhop-n2-never/a.png"), None);

		assert!(pane.queued.is_some(), "the job waits for its debounce");
		assert!(matches!(pane.show, Show::Empty), "and nothing has replaced the screen yet");

		pane.advance();
		assert!(pane.queued.is_some(), "with no column drawn, nothing is started");
		assert!(pane.pending.is_none());

		pane.look(Path::new("/tmp/crabhop-n2-never/dir"), Some(&dir_entry()));
		assert_eq!(pane.kind, Kind::Dir);
		assert!(pane.queued.is_some(), "a listing is not read on this thread");
		assert!(matches!(pane.show, Show::Empty), "and nothing has replaced the screen yet");

		pane.look(Path::new("/tmp/crabhop-n2-never/a.avif"), None);
		assert!(matches!(pane.show, Show::Placeholder(_)));
		assert!(pane.queued.is_none());
	}

	#[test]
	fn a_point_on_the_tree_folds_the_folder_it_stands_on() {
		let tokens = Tokens::MOCHA;
		let item = |name: &str, path: &str, depth: usize, dir: bool| archive::Item {
			name:  name.to_owned(),
			path:  path.to_owned(),
			depth,
			dir,
			size:  Some(4),
		};
		let items: Vec<archive::Item> = vec![
			item("docs", "docs", 1, true),
			item("deep", "docs/deep", 2, true),
			item("b.bin", "docs/deep/b.bin", 3, false),
			item("a.txt", "docs/a.txt", 2, false),
			item("top.md", "top.md", 1, false),
		];

		let mut pane = PreviewPane {
			path: Some(PathBuf::from("/tmp/crabhop-n2-never/tree.zip")),
			collapsed: archive::default_collapsed(&items),
			..PreviewPane::default()
		};
		let (doc, _) = archive_doc(&items, Some(items.len()), &pane.collapsed, &tokens);
		pane.show = Show::Archive { items, doc: Some(doc), total: Some(5), truncated: false };

		assert_eq!(pane.archive_rows_for_tests(), Some(4), "the first level opens, deep stays shut");

		let column = Rect::from_min_max(pos2(0.0, 100.0), pos2(400.0, 500.0));
		let rows = pane.archive_tree(column).expect("an archive tree with its rows laid out");
		assert_eq!(rows.len(), 4);
		assert!(rows[1].2, "row one is the shut folder");
		assert!(!rows[3].2, "row three is a file");

		assert!(!pane.archive_fold_at(column, pos2(100.0, 100.0 + PAD + 2.0 * 24.0 + 12.0)), "a file is not a fold");
		assert_eq!(pane.archive_rows_for_tests(), Some(4));

		assert!(pane.archive_fold_at(column, pos2(100.0, 100.0 + PAD + 24.0 + 12.0)));
		assert_eq!(pane.archive_rows_for_tests(), Some(5), "deep is open now");
		assert!(pane.archive_fold(1), "row one of the open tree is deep again");
		assert_eq!(pane.archive_rows_for_tests(), Some(4));
	}

	#[test]
	fn an_archive_tree_says_what_it_cut_short() {
		let tokens = Tokens::MOCHA;
		let item = |i: usize| archive::Item {
			name:  format!("f{i}.txt"),
			path:  format!("f{i}.txt"),
			depth: 1,
			dir:   false,
			size:  Some(i as u64),
		};
		let open = HashSet::new();

		let small: Vec<archive::Item> = (0..3).map(item).collect();
		let (doc, truncated) = archive_doc(&small, Some(3), &open, &tokens);
		assert!(!truncated);
		assert_eq!(doc.rows.len(), 3, "three entries, three rows and no note");

		let long: Vec<archive::Item> = (0..ARCHIVE_CAP).map(item).collect();
		let (doc, truncated) = archive_doc(&long, Some(ARCHIVE_CAP + 500), &open, &tokens);
		assert!(truncated, "the cap cut it short");
		assert_eq!(doc.rows.len(), ARCHIVE_CAP + 1, "the entries, then the note");
		assert!(doc.rows[ARCHIVE_CAP].text.contains("共 2500 个条目"), "{:?}", doc.rows[ARCHIVE_CAP].text);

		let (doc, truncated) = archive_doc(&long, None, &open, &tokens);
		assert!(truncated);
		assert!(doc.rows[ARCHIVE_CAP].text.contains("还有更多条目"), "{:?}", doc.rows[ARCHIVE_CAP].text);
	}

	#[test]
	fn a_long_code_line_wraps_under_its_own_text() {
		let tokens = Tokens::MOCHA;
		let line = r#"let x = "这是一个很长的中文与 English 混排的赋值表达式，加上一段足够长的尾巴";"#;
		let width = 400.0;

		let (doc, _) = lay(Kind::Code, line, width, &tokens, false, &HashMap::new());
		assert!(doc.rows.len() > 1, "a line this long cannot fit one row");

		let put_back: String = doc.rows.iter().map(|row| row.text.as_str()).collect();
		assert_eq!(put_back, line, "not a character may be lost to the wrap");

		assert_eq!(doc.rows[0].number, Some(1));
		assert!(doc.rows[1..].iter().all(|row| row.number.is_none()), "continuations carry no number");

		assert!(doc.rows.iter().all(|row| row.indent == 0.0), "the text edge is the same for all pieces");

		let cols = code_columns(width, digits_of(1));
		for row in &doc.rows {
			let cells: u32 = row.text.chars().map(list::cell_width).sum();
			assert!(cells <= cols, "{:?} is wider than the column's {cols} cells", row.text);
		}
	}

	#[test]
	fn the_code_wrap_width_pays_for_its_gutter() {
		let one = code_columns(400.0, 1);
		let four = code_columns(400.0, 4);
		assert!(four < one, "four digits take cells from the line: {four} vs {one}");
		assert!(one - four <= 8, "and no more than the digits themselves cost");

		assert!(code_columns(40.0, 1) >= 4, "a sliver of a column still wraps at a workable width");
	}

	#[test]
	fn a_hovered_archive_queues_its_listing() {
		let mut pane = PreviewPane::default();
		pane.look(Path::new("/tmp/crabhop-p11-never/a.zip"), None);

		assert_eq!(pane.kind, Kind::Archive);
		assert!(pane.queued.is_some(), "the listing is read on a worker");
		assert!(matches!(pane.show, Show::Empty), "and nothing has replaced the screen yet");
	}

	#[test]
	fn a_directory_listing_wears_the_lists_rows() {
		let tokens = Tokens::MOCHA;
		let entries = vec![
			Entry { name: "sub".to_owned(), size: 4096, modified: None, dir: true, mode: None, link: false , created: None, uid: 0 },
			Entry { name: "note.txt".to_owned(), size: 5, modified: None, dir: false, mode: None, link: false , created: None, uid: 0 },
		];

		let (doc, truncated) = dir_doc(&entries, &tokens);
		assert!(!truncated);
		assert_eq!(doc.rows.len(), 2);
		assert_eq!(doc.rows[0].height, ROW_HEIGHT, "a listing row is a list row");
		assert!(doc.rows[0].lead.is_some(), "the icon slot is there");
		assert_eq!(doc.rows[0].colour, tokens.blue, "directories keep the list's blue");
		assert!(doc.rows[1].lead.is_some() && doc.rows[1].colour == tokens.text);
		assert!(doc.rows[0].right.is_none() && doc.rows[1].right.is_none(), "and no facts crowd the names (P12)");
	}

	#[test]
	fn the_texture_key_is_path_time_and_bucket() {
		let at = SystemTime::UNIX_EPOCH + Duration::from_secs(9);
		let a = img::key("/x.png", Some(at), [ 300, 200 ]);
		let b = img::key("/x.png", Some(at), [ 310, 250 ]);
		assert_eq!(a, b, "one bucket, one texture");
		assert_ne!(a, img::key("/x.png", Some(at), [ 700, 200 ]));
	}

	fn image(key: &str, source: [u32; 2]) -> Show {
		Show::Image { key: key.to_owned(), source }
	}

	#[test]
	fn the_cards_image_section_says_what_a_picture_is() {
		let full = img::Report { format: Some("PNG".to_owned()), colour: Some("8 位 RGBA".to_owned()) };

		let rows: HashMap<_, _> = image_rows(&full, [ 3840, 2160 ], Some(456.0)).into_iter().collect();
		assert_eq!(rows.get("格式").map(String::as_str), Some("PNG"));
		assert_eq!(rows.get("尺寸").map(String::as_str), Some("3840 × 2160 像素"));
		assert_eq!(rows.get("显示比例").map(String::as_str), Some("12%"), "456 of 3840");
		assert_eq!(rows.get("色深").map(String::as_str), Some("8 位 RGBA"));
		assert_eq!(rows.len(), 4, "four words and no more — the card's own rows carry size and time");

		let rows: HashMap<_, _> = image_rows(&full, [ 3840, 2160 ], None).into_iter().collect();
		assert!(!rows.contains_key("显示比例"), "{rows:?}");

		let bare = img::Report { format: None, colour: None };
		let rows: HashMap<_, _> = image_rows(&bare, [ 100, 50 ], Some(100.0)).into_iter().collect();
		assert!(!rows.contains_key("格式") && !rows.contains_key("色深"), "{rows:?}");
		assert_eq!(rows.get("显示比例").map(String::as_str), Some("100%"), "a small picture shows its own size");
		assert_eq!(rows.get("尺寸").map(String::as_str), Some("100 × 50 像素"), "the pixels are the header's own");
	}

	fn dir_entry() -> Entry {
		Entry { name: "dir".to_owned(), size: 0, modified: None, dir: true, mode: None, link: false , created: None, uid: 0 }
	}

	#[test]
	fn the_idle_sweep_waits_for_the_listing_and_for_quiet() {
		let mut pane = PreviewPane { target: [ 400, 300 ], ..PreviewPane::default() };

		let dir = PathBuf::from("/tmp/crabhop-n2-never/dir");
		let pictures: Vec<(PathBuf, Option<Entry>)> = (0..3)
			.map(|i| {
				let name = format!("p{i}.png");
				(dir.join(&name), Some(Entry { name, dir: false, ..dir_entry() }))
			})
			.collect();

		pane.warm(&dir, &[], &[], &[]);
		let stamp = pane.sweep.as_ref().expect("the visit made a sweep").stamp;
		assert!(pane.sweep.as_ref().expect("a sweep").queue.is_empty(), "nothing to sweep before the names land");

		pane.warm(&dir, &pictures, &[], &pictures);
		let sweep = pane.sweep.as_ref().expect("still the same sweep");
		assert_eq!(sweep.queue.len(), 3, "the pictures the view holds");
		assert_eq!(sweep.stamp, stamp, "and it is the same visit");

		let other = PathBuf::from("/tmp/crabhop-n2-never/other");
		pane.warm(&other, &[], &[], &[]);
		assert_ne!(pane.sweep.as_ref().expect("a sweep").stamp, stamp, "a new visit, a new stamp");

		pane.target = [ 0, 0 ];
		pane.warm(&other, &pictures, &[], &pictures);
		pane.sweep.as_mut().expect("a sweep").since = Instant::now() - Duration::from_millis(900);
		pane.sweep_tick();
		assert!(pane.sweep.is_some(), "no column, no sweep to run");

		let queueless = PathBuf::from("/tmp/crabhop-n2-never/empty");
		pane.target = [ 400, 300 ];
		pane.warm(&queueless, &[], &[], &[]);
		pane.sweep.as_mut().expect("a sweep").since = Instant::now() - Duration::from_millis(900);
		pane.sweep_tick();
		assert!(pane.sweep.is_some(), "a listing that has not landed is not a queue");

		pane.warm(&queueless, &pictures, &[], &pictures);
		pane.sweep.as_mut().expect("a sweep").since = Instant::now() - Duration::from_millis(900);
		pane.sweep_tick();
		assert!(pane.sweep.is_none(), "a quiet half-second past, the queue is handed over");
	}
}
