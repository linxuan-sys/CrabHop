//! The preview column: what the cursor is on, decoded away from this thread and painted as soon
//! as it is ready.
//!
//! The Zed rules the task names, all of them here: a hover starts nothing until it has sat still
//! for a debounce; a decode that has not answered within [`LOADING_DELAY`] shows a quiet
//! `载入中…`; whatever was on the screen is kept (the pending/displayed double buffer — a cursor
//! sweeping the list never sees a blank column) until an answer replaces it; textures are held in
//! an LRU that releases the handles it evicts; and the cache key is `(path, mtime, size bucket)`.
//!
//! The providers themselves are moved from the legacy window: `img` (decode + resample),
//! `thumb` (the freedesktop cache), `docx`, `sheet`, `pdf` and `code`. `md` is new (§3.2), and
//! `rows` is the common display-row model all of the textual ones are laid out into.

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

/// How long a decode may take before the column says it is waiting. Zed's own rule: a task that
/// answers within this is never "loading" to the eye, and a flash of a spinner costs more than the
/// wait it describes.
pub(crate) const LOADING_DELAY: Duration = Duration::from_millis(200);

/// How long the cursor must sit on a file before its picture may replace what is on screen: the
/// decode itself starts the moment the cursor lands (its answer is the newest or it is dropped),
/// and this throttle is the one thing that still waits — a sweep past a file must not push a
/// texture the sweep will replace a frame later (P27⑥, the old 80ms decode debounce's heir).
const UPLOAD_THROTTLE: Duration = Duration::from_millis(40);

/// One row of text, in pixels: the whole column scrolls on the 24px rhythm, so the wheel, the
/// seek keys and the markdown step all agree.
const LINE_PX: f32 = 24.0;

/// The content inset, and the strip kept clear of the overlay scrollbar (§3 总规则).
const PAD: f32 = 16.0;
const PAD_RIGHT: f32 = 10.0;

/// The rectangle a preview draws inside its column: the one place the inset lives, so what is
/// painted and what a click hits can never drift apart (P28's long tail hit-tests with it).
pub(crate) fn content_of(column: Rect) -> Rect {
	Rect::from_min_max(
		pos2(column.min.x + PAD, column.min.y + PAD),
		pos2(column.max.x - PAD_RIGHT, column.max.y - PAD),
	)
}

/// How much of a text file a worker reads: past a megabyte the preview is the first 2000 lines
/// anyway (§3.3), and a read that never ends is worse than a truncated one.
const HEAD: usize = 1024 * 1024;

/// How much of a document (docx/xlsx) may be read, so a hostile zip cannot ask for all the memory.
const DOC_CAP: usize = 64 * 1024 * 1024;

/// The extensions that are always text; anything else is read before it is judged, and a binary
/// guess lands on the placeholder.
const TEXT: &[&str] = &[
	"txt", "log", "ini", "conf", "cfg", "toml", "yaml", "yml", "json", "jsonc", "xml", "html", "htm", "css", "scss",
	"js", "jsx", "ts", "tsx", "rs", "py", "rb", "go", "java", "c", "h", "cpp", "hpp", "cc", "cs", "lua", "sh", "bash",
	"zsh", "fish", "sql", "csv", "tsv", "diff", "patch", "gitignore", "lock", "env", "service", "desktop", "nix", "vim",
	"el", "clj", "kt", "swift", "php", "pl", "r", "jl", "ex", "exs", "erl", "hs", "ml", "scala", "dart", "zig", "v",
	"asm", "s", "wat", "proto", "graphql", "tf", "hcl", "dockerfile", "makefile", "cmake", "gradle",
];

/// What a worker is asked to decode or extract. Warm jobs are asked separately: they answer
/// nothing, because a warm cache is not a picture on screen.
struct Look {
	generation: u64,
	path:       PathBuf,
	kind:       Kind,
	len:        u64,
	mtime:      Option<SystemTime>,
	target:     [u32; 2],
	/// The PDF page the column wants.
	page:       usize,
}

/// What a worker made of a file.
enum Payload {
	Image {
		size:   [u32; 2],
		source: [u32; 2],
		rgba:   Vec<u8>,
		/// A GIF's own frames and their delays, the first one being `rgba` (P26's first item):
		/// empty for every still picture.
		frames: Vec<(Vec<u8>, u32)>,
	},
	/// A video whose frame no tool could make (P29): the container's own facts, as the rows the
	/// column draws in place of a picture — and never an error.
	Media(Vec<(String, String)>),
	/// A directory's next level, as the same `Entry` rows the middle column draws.
	Dir(Vec<Entry>),
	/// An archive's own entries, in the archive's order.
	Archive(archive::Listing),
	Pdf { page: usize, total: usize, size: [u32; 2], rgba: Vec<u8> },
	Text {
		text:      String,
		truncated: bool,
		/// The document's own pictures (P26's second item): the `src` as it was written, the
		/// texture's own key, the pixels' size and the RGBA. Empty for every kind whose text has no
		/// pictures in it.
		images:    Vec<(String, String, [u32; 2], Vec<u8>)>,
	},
	/// A spreadsheet's used range as a table (P41): the pane paints its grid, not its text.
	Table(sheet::Table),
	/// A named failure: too big to decode, an encrypted PDF — the column says so.
	Note(String),
	/// Nothing could be made of it: the placeholder stands, with a reason under the name when the
	/// worker has one to give (an encrypted archive, a broken one).
	Nothing,
	Degraded(String),
}

struct Answer {
	generation: u64,
	key:        String,
	payload:    Payload,
}

/// The preview's own classification of the hovered file. The extension decides — yazi's mime
/// resolution is a separate asynchronous pass and may not have landed on the hovered file yet.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Kind {
	Image,
	Markdown,
	Code,
	Docx,
	Sheet,
	Pdf,
	/// A directory: the column previews its own listing (§3.7's placeholder does not apply).
	Dir,
	/// An archive: the column previews the tree of entries inside it, without opening it.
	Archive,
	/// A video: the column previews its first frame where a tool can make one (P29), and the
	/// container's own facts where none can.
	Video,
	/// A format with no decoder, or a file with no preview worth making.
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
			// anything else is read before it is judged: text previews, and a binary guess falls
			// back to the placeholder when the worker meets a NUL
			_ => Self::Code,
		}
	}
}

/// What the column is showing. Textual answers live in [`PreviewPane::raw`] instead, because
/// their rows are laid out again whenever the width moves.
enum Show {
	Empty,
	/// A quiet line of text (a named failure the worker sent).
	Note(String),
	/// The §3.7 placeholder: the type's big icon, the name, and what can be done — plus, when a
	/// provider failed in a way worth naming, the reason under it.
	Placeholder(Option<String>),
	Image { key: String, source: [u32; 2] },
	/// A video's own card (P29): what the container says it holds, drawn where a frame would have
	/// been. A `None` column is the column before anything was read.
	Media { facts: Vec<(String, String)> },
	Pdf { key: String, size: [u32; 2], page: usize, total: usize },
	/// A hovered directory's listing: the rows, the true entry count, and whether the rows stop
	/// short of it.
	Dir { entries: Vec<Entry>, doc: Option<Doc>, truncated: bool },
	/// A hovered archive's entry tree, built the same way.
	Archive { items: Vec<archive::Item>, doc: Option<Doc>, total: Option<usize>, truncated: bool },
	/// A spreadsheet's own table (P41): the used range with its grid's dimensions.
	Table { table: sheet::Table },
}

/// A textual answer, kept so a width change can lay it out again.
struct Raw {
	path:      PathBuf,
	kind:      Kind,
	text:      String,
	truncated: bool,
	/// The pictures the document asked for and the worker could answer (P26's second item): the
	/// `src` as written, the texture's own key, and the pixels' size. The pixels themselves went to
	/// the textures when the answer landed.
	images:    Vec<(String, String, [u32; 2])>,
}

/// A playing animation: the texture it owns, the frames, the one on screen, and when the next is
/// due. The frames are already the column's size — the worker resampled them — so a tick is one
/// texture upload and nothing else (P26's first item).
struct Anim {
	key:    String,
	size:   [u32; 2],
	frames: Vec<(Vec<u8>, u32)>,
	at:     usize,
	due:    Instant,
}

/// Whether the window is asked to keep animations still: an environment answer for now, and the
/// place the settings window's own switch will land (N3's motion audit).
fn reduced_motion() -> bool {
	static STILL: LazyLock<bool> = LazyLock::new(|| std::env::var_os("CRABHOP_REDUCED_MOTION").is_some());

	*STILL
}

/// A hover that is waiting for a decode slot or the column's own size. The job starts the frame
/// the cursor lands — P27⑥'s speculative decode — and only its answer waits.
struct Queued {
	generation: u64,
	path:       PathBuf,
	kind:       Kind,
	len:        u64,
	mtime:      Option<SystemTime>,
	at:         Instant,
}

/// The idle sweep (P27⑥'s L3): after a directory has sat still for half a second, the pictures it
/// is about to show are decoded into the bucket in the background — a few at a time on the
/// preloader's own slot — and a visit that ends cancels whatever is left of the queue.
struct Sweep {
	/// The visit's own number: a walker whose visit is over writes nothing more.
	stamp: u64,
	/// What the sweep will predecode, in the listing's own order.
	queue: Vec<(PathBuf, u64, Option<SystemTime>)>,
	/// When the cursor last moved: the sweep waits for a quiet half-second.
	since: Instant,
}

/// The per-file syntax state for the code preview: the walk continues from where it stopped, and
/// only the lines the view has asked for are ever highlighted.
struct Hl {
	key:    String,
	syntax: code::Syntax,
	/// The next row the walk has to look at: a source line's first row, and the continuations
	/// after it are skipped over without a call (one `next` per source line, always).
	row:    usize,
}

pub(crate) struct PreviewPane {
	ctx:   Context,
	tx:    Sender<Answer>,
	rx:    Receiver<Answer>,
	slots: Arc<Semaphore>,
	/// The preloader's own slot (P27⑥): warm jobs and the idle sweep share it, so they never hold
	/// a decode slot a hover is waiting for. A hover's task always runs first.
	preload: Arc<Semaphore>,

	/// The file the cursor is on, and its identity — two thirds of the cache key.
	path:  Option<PathBuf>,
	kind:  Kind,
	len:   u64,
	mtime: Option<SystemTime>,

	/// The request's number: decodes run side by side, and an answer from before the latest
	/// request has lost a race this thread must not resurrect.
	generation: u64,
	queued:     Option<Queued>,
	/// When the decode in flight began, for [`LOADING_DELAY`].
	pending: Option<Instant>,

	show: Show,
	raw:  Option<Raw>,

	/// The texture cache: the handles, and the LRU that decides what to release.
	textures: HashMap<String, TextureHandle>,
	lru:      img::Lru,

	/// The laid-out rows of the textual preview, and what (width, content) they were laid for.
	laid:    Option<Doc>,
	laid_of: Option<String>,
	notice:  Option<&'static str>,
	hl:      Option<Hl>,

	/// Where the column is scrolled to, in pixels; the view's height; and the pixel target the
	/// providers are rendered for — all known from the last draw.
	offset: f32,
	view:   f32,
	target: [u32; 2],

	/// The image gestures (§3.1): the zoom on top of fit, and the pan a zoomed picture takes.
	zoom: f32,
	pan:  Vec2,
	/// When the pointer last touched the pane: the badge lives for 1.5s after.
	touched: Option<Instant>,
	/// The badge label a picture last showed (P28①): printed once per change, so a replay can
	/// assert a zoom by the reading the badge itself carries.
	badge:   Option<String>,
	/// A GIF's own clock (P26's first item): its frames, which one is up, and when the next is due.
	/// `None` for every still picture.
	anim:    Option<Anim>,

	/// The images already warmed, so a sweep does not ask twice.
	warmed:     HashSet<String>,
	warmed_dir: Option<PathBuf>,

	/// When the cursor last landed, for the upload throttle (P27⑥).
	last_look: Option<Instant>,
	/// An answer that has landed but may not be uploaded yet: the throttle's own waiting room.
	held:      Option<Answer>,
	/// The idle sweep (P27⑥'s L3), and the visit it belongs to.
	sweep:     Option<Sweep>,
	/// The visits' own counter: a walker from a visit that is over stops writing. Shared with the
	/// threads, because a directory change happens on this one.
	visit:     Arc<std::sync::atomic::AtomicU64>,
	/// The archive tree's shut folders, by their path inside the archive (P28's long tail): the
	/// listing arrives with its first level open and the rest of them here.
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

	/// A playing animation's own tick (P26's first item): the frame that is due goes to the texture
	/// and the next one is scheduled — and a pane nobody can see is not drawn, so it does not play.
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

	/// The cursor has moved: take the file, and let the double buffer keep showing what it was
	/// showing until the answer for this one lands.
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
		// the throttle's clock, and the waiting room: an answer for the file just left is not
		// this file's answer (P27⑥)
		self.last_look = Some(Instant::now());
		self.held = None;

		match self.kind {
			// a directory, or a format no decoder is built for: known at once, shown at once
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

	/// The preview column is not on screen: nothing is decoded for it — and a sweep it had going is
	/// called off, because nobody is looking at what it would fill (P27⑥).
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

	/// Warm the caches for what the cursor is about to meet (P27⑥, three layers): the window's own
	/// bucket first and the shared thumbnail cache beside it — the hovered file's neighbours on
	/// every move (L2), and the first fifty pictures of a freshly entered directory once it has
	/// sat still for half a second (L3). Cache writes only, on the preloader's own slot, and never
	/// at the cost of a hover's own decode.
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
			// a new directory is a new visit: whatever the last sweep had left is called off, and
			// this one's idle clock starts now (P27⑥)
			self.visit.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
			let stamp = self.visit.load(std::sync::atomic::Ordering::Relaxed);
			self.sweep = Some(Sweep { stamp, queue: Vec::new(), since: Instant::now() });
		}
		if let Some(sweep) = &mut self.sweep {
			// a cursor that moves is a person still deciding: the sweep waits for quiet — and a
			// listing that landed after the visit began fills the queue it could not fill then
			// (the first view of a directory is often still empty)
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

	/// One predecode, on the preloader's own slot: a job that cannot have the slot now waits a
	/// tick and tries again — a hover's decode is never made to queue behind it (P27⑥).
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

	/// The idle sweep's own clock (P27⑥'s L3): half a second after the cursor last moved, the
	/// queue goes to one walker thread that decodes each picture into the bucket — on the
	/// preloader's slot, one file at a time, and nothing at all once the visit is over.
	fn sweep_tick(&mut self) {
		let Some(sweep) = &self.sweep else { return };
		// an empty queue is a listing that has not landed: the sweep waits for its names rather
		// than spending its one shot on nothing (P27⑥)
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
				// one file at a time on the one slot: warm jobs get their turns between files
				let Some(_permit) = preload_slot(slot.clone()) else { return };
				if visit.load(std::sync::atomic::Ordering::Relaxed) != sweep.stamp {
					return;
				}

				run_warm(&path, len, mtime, target);
			}

			// the sweep is what fills the bucket by the handful: it is also the moment to make
			// sure the bucket did not quietly eat the disk
			img::bucket_prune();
		});
	}

	/// Read whatever the workers have left, and put it on the screen if it is still the latest.
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

	/// Start the hovered file's job the moment the cursor lands — P27⑥'s speculative decode: the
	/// answer of an older look is dropped by its generation, so there is nothing to protect by
	/// waiting. What does wait is the upload (see `held`), and the column's own size, without
	/// which no target can be named.
	fn advance(&mut self) {
		if self.target[0] == 0 {
			return;
		}

		self.release_held();

		let Some(mut queued) = self.queued.take() else {
			if let Some(at) = self.pending {
				// the note appears LOADING_DELAY after the decode began: make sure a frame happens then
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
			// a PDF is *rendered* for its own target — zoom and all — while a picture is uploaded
			// whole and scaled by the sampler, so only the page's render moves with the zoom
			// (P27⑥)
			match queued.kind {
				Kind::Pdf => img::request_target(self.target, self.zoom),
				// a picture is reduced for the exact rectangle it will be drawn in (P32); a video's
				// frame stands at the column's own width and the sampler scales it from there
				Kind::Image => self.image_target(),
				Kind::Video => [ self.target[0].max(1), 0 ],
				_ => self.target,
			},
			queued.generation,
		);
		let page = self.pdf_page();

		let Ok(slot) = self.slots.clone().try_acquire_owned() else {
			// every decode slot is busy: the hover keeps its place in the queue and waits a tick
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

	/// The throttle's own moment (P27⑥): an answer that landed while the cursor was still moving
	/// goes up once the hand has settled — [`UPLOAD_THROTTLE`] — or when a frame catches it late.
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

	/// A texture lands on the GPU, the old display is replaced, and the previous texture's handle
	/// is released the moment the LRU says so — no orphans are left to age on the GPU.
	fn accept(&mut self, answer: Answer) {
		// the upload throttle (P27⑥): a picture that answers while the cursor is still moving
		// waits its 40ms in the hold — the double buffer keeps the screen calm, and a texture the
		// hand is one key away from replacing is never pushed
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

	/// The answer goes up (or becomes the screen's own): the one place the display swaps.
	fn place(&mut self, answer: Answer) {
		match answer.payload {
			Payload::Image { size, source, rgba, frames } => {
				self.upload(&answer.key, size, &rgba);
				self.pending = None;
				self.raw = None;
				self.show = Show::Image { key: answer.key.clone(), source };
				// a picture that has just changed shows its scale for a moment, the way a zoom does
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
			Payload::Table(table) => {
				self.pending = None;
				self.raw = None;
				self.show = Show::Table { table };
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
			// dropping the handle is the release: egui frees the texture with it
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

	// ---- the wheel, the seek keys and the image gestures ----

	/// A wheel over the column: a line of scroll a line of text — and the offset the wheel moved
	/// it to, printed for a replay that has to assert it (P18).
	pub(crate) fn scroll(&mut self, lines: f32) -> bool {
		if self.content() <= self.view {
			return false;
		}

		let before = self.offset;
		self.offset = (self.offset + lines * LINE_PX).max(0.0);
		crate::metrics::preview(before, self.offset);
		true
	}

	/// The picture's own height at the column's width: what the extent and the scroll measure a
	/// fit-inside picture by (P28①) — the fit's height times the zoom, which is the only way a
	/// picture grows past the column now.
	pub(crate) fn image_height(&self) -> Option<f32> {
		match &self.show {
			Show::Image { source, .. } if self.raw.is_none() => Some(self.shown_size(*source).y * self.zoom),
			_ => None,
		}
	}

	/// The size a picture is shown at with no zoom (P27's first item — the final ruling): the column's
	/// own width, the file's own shape, and a picture taller than the column scrolls through it
	/// rather than being squashed into it. In points; zero before the column has told the pane how
	/// much room there is.
	fn shown_size(&self, source: [u32; 2]) -> Vec2 {
		if self.target[0] == 0 || self.target[1] == 0 {
			return Vec2::ZERO;
		}

		let ppp = self.ctx.pixels_per_point().max(f32::MIN_POSITIVE);
		let [ width, height ] = img::fit_width((source[0], source[1]), self.target[0] as f32 / ppp);

		vec2(width, height)
	}

	/// The target a picture is decoded and keyed for (P32's 1:1 model): the width the column will
	/// draw it at — the content width times the zoom — with the height left to the file's own shape
	/// (a zero). A zoom therefore moves the key and asks for a new reduction; a zoom back finds the
	/// old one in the bucket, and the texture that lands is exactly the rectangle on screen.
	fn image_target(&self) -> [u32; 2] {
		[ ((self.target[0] as f32 * self.zoom).round().max(1.0) as u32).max(1), 0 ]
	}

	/// The physical width the column would show a fit-width file's own pixels at (P29): the content
	/// width itself — the video's ratio row reads it, as the picture's own reads `shown_physical`.
	pub(crate) fn fit_width_pixels(&self) -> Option<f32> {
		(self.target[0] > 0).then(|| self.target[0] as f32)
	}

	/// The size the column is showing `path`'s picture at, physical pixels, zoom and all (P28②
	/// revised): the spot card's ratio row reads it — `None` when the column is not showing that
	/// file, or is not showing a picture at all.
	pub(crate) fn shown_physical(&self, path: &Path) -> Option<[f32; 2]> {
		let Show::Image { source, .. } = &self.show else { return None };
		if self.raw.is_some() || self.path.as_deref() != Some(path) {
			return None;
		}

		let ppp = self.ctx.pixels_per_point().max(f32::MIN_POSITIVE);
		let shown = self.shown_size(*source) * self.zoom;

		Some([ shown.x * ppp, shown.y * ppp ])
	}

	/// The overlay scrollbar was dragged: the offset it asks for.
	pub(crate) fn scroll_to(&mut self, offset: f32) { self.offset = offset.max(0.0); }

	/// `K`/`J`, the keymap's own seek: five lines of text (the spec's 120px step), one page of
	/// PDF, nothing for an image.
	pub(crate) fn seek(&mut self, step: i32) -> bool {
		match self.kind {
			Kind::Pdf if matches!(self.show, Show::Pdf { .. }) => {
				let page = self.pdf_page();
				let next = (page as i32 + step.signum()).max(0) as usize;
				if next == page {
					return true;
				}

				// the flip is a fresh render; the double buffer keeps the old page up meanwhile
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
			Kind::Code | Kind::Markdown | Kind::Docx | Kind::Sheet | Kind::Dir | Kind::Archive => {
				// the keymap's own count of lines, at the document's line height: `seek 5` is the
				// spec's five lines (120px), not five times them (P18's correction)
				let before = self.offset;
				self.offset = (self.offset + step as f32 * LINE_PX).max(0.0);
				crate::metrics::preview(before, self.offset);
				true
			}
			_ => false,
		}
	}

	/// `Ctrl+滚轮` on an image: 1.25× a step, inside the spec's 25%–800% band.
	pub(crate) fn zoom_by(&mut self, up: bool) -> bool {
		if !matches!(self.show, Show::Image { .. } | Show::Pdf { .. }) {
			return false;
		}

		self.zoom = if up { self.zoom * 1.25 } else { self.zoom / 1.25 }.clamp(0.25, 8.0);
		self.touched = Some(Instant::now());
		self.ctx.request_repaint();
		true
	}

	/// `zoom fit`: the whole picture at the column's own scale.
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

	/// `zoom full`: one picture pixel a screen pixel — the file's own resolution, however large
	/// that makes it (the pan is how the rest is reached). The fit is the basis the multiplier
	/// rides on (P28①), so the ratio is the file's own pixels against what the fit shows.
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

	/// A drag on a zoomed picture pans it — the page of a zoomed PDF included. A picture at the fit
/// has nothing to drag: it is whole inside its zone (P28①), and a picture past it only is past
/// it when zoomed.
	pub(crate) fn pan_by(&mut self, delta: Vec2) {
		if self.zoomed() {
			self.pan += delta;
			self.touched = Some(Instant::now());
			return;
		}

		// a picture taller than the column is a page to drag through (P27's first item): a drag
		// that cannot be a pan is a scroll
		if matches!(self.show, Show::Image { .. }) && self.content() > self.view {
			self.offset = (self.offset - delta.y).clamp(0.0, (self.content() - self.view).max(0.0));
			self.touched = Some(Instant::now());
			self.ctx.request_repaint();
		}
	}

	pub(crate) fn zoomed(&self) -> bool {
		matches!(self.show, Show::Image { .. } | Show::Pdf { .. }) && self.zoom > 1.0
	}

	/// Whether a picture is on screen, and where its own scroll stands (P27①): the harness's own
	/// reading for the spill test.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn image_offset_for_tests(&self) -> Option<f32> {
		matches!(self.show, Show::Image { .. }).then_some(self.offset)
	}

	/// The badge label the last drawn picture showed (P28①): the harness's own reading of what a
	/// zoom did to the display ratio — the very string the badge carries, and the log's own line.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn badge_for_tests(&self) -> Option<&str> {
		self.badge.as_deref()
	}

	/// How many rows the archive tree shows right now, folded or not (P28's long tail): the tests'
	/// own reading, `None` when the column is not showing an archive.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn archive_rows_for_tests(&self) -> Option<usize> {
		let Show::Archive { items, .. } = &self.show else { return None };
		Some(archive::visible(items, &self.collapsed).len())
	}

	/// The column's scrollable extent, for the overlay scrollbar: the content's height in pixels,
	/// the offset, and the view's own height. `None` when there is nothing to scroll.
	pub(crate) fn extent(&self) -> Option<(f32, f32, f32)> {
		let content = self.content();
		(content > self.view + 0.5).then_some((content, self.offset, self.view))
	}

	fn content(&self) -> f32 {
		match (&self.show, &self.raw) {
			// a raw text outranks whatever was on screen before it: the pane's own answer is what
			// the wheel measures — with the old order a listing loaded before the file kept
			// `content` at the listing's own height while the file's rows were being drawn
			(_, Some(_)) => self.laid.as_ref().map_or(PAD, |doc| doc.total + PAD),
			// the sheet's grid measures itself (P41): its rows' own heights
			(Show::Table { table }, None) => table.height() + PAD,
			(Show::Dir { doc: Some(doc), .. } | Show::Archive { doc: Some(doc), .. }, _) => doc.total + PAD,
			// the picture's own height at the column's width (P27's first item): a tall picture is
			// a page to walk, so this is what the wheel and the scrollbar measure against
			(Show::Image { .. }, None) => self.image_height().map_or(PAD, |tall| tall + PAD),
			(Show::Pdf { size, .. }, None) => {
				let width = self.target[0].max(1) as f32;
				width * size[1] as f32 / size[0].max(1) as f32 + PAD
			}
			_ => 0.0,
		}
	}

	// ---- the picture ----

	/// Paint the column: the content, the fades, the badge, and the loading note when the decode
	/// has taken longer than the eye forgives.
	pub(crate) fn draw(&mut self, painter: &Painter, rect: Rect, tokens: &Tokens) {
		// the column keeps its own edges (P27②'s rule for the list, and the answer to P27①'s
		// spill: a scrolled picture's top used to reach over the tab bar, because the painter it
		// borrowed knew only the window's own rectangle — chrome included)
		let clipped = painter.with_clip_rect(rect);
		let painter = &clipped;

		let content = content_of(rect);
		self.view = content.height().max(1.0);
		let ppp = self.ctx.pixels_per_point();
		self.target = [
			(content.width() * ppp).round().max(1.0) as u32,
			(content.height() * ppp).round().max(1.0) as u32,
		];

		// the target may have moved into another bucket: the texture that fits the old one is not
		// the one the column wants, so the same file is asked for again
		self.retarget();

		let pointer = self.ctx.input(|i| i.pointer.latest_pos());
		if pointer.is_some_and(|at| rect.contains(at)) {
			self.touched = Some(Instant::now());
		}

		// the animation's own clock, before the frame it may have just uploaded is drawn
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
			(Show::Table { .. }, None) => self.draw_sheet(painter, content, tokens),
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

		// the fades: the content says it continues rather than a scrollbar shouting it
		self.fades(painter, rect, tokens);

		// `载入中…` once the decode has outlived the delay, over whatever the double buffer holds —
		// except for a directory: its wait is drawn as the list's own skeleton rows
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

	/// A file whose column changed shape while it was being decoded is asked for again — the cache
	/// key's size bucket is what makes the question answerable, and a jitter never asks twice.
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
			// a picture's key carries the width it will be drawn at, which the zoom moves (P32); a
			// video's frame stands at the column's own width and the sampler scales it from there
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

		// fit-width, the way a picture viewer shows one (P27's first item — the final ruling): the
		// column's width is the picture's width, the shape is kept, and a picture taller than the
		// column scrolls through it — never squashed into it. `zoom` rides on top of that, the pan
		// is what a zoomed picture is dragged by, and the offset what any of them walks.
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

		// the zoom badge: the percentage is of the picture's own pixels, and the mode shows itself
		// the multiplier §3.1's badge carries: what the eye gets, in *physical* pixels, against the
		// file's own pixels — so a picture shown at its native size reads 100% on any display
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

	/// The badge's label has changed (P28①): the reading a replay asserts a zoom by — printed once
	/// per change, paired with the file, so `zoom in` leaves 12% ⤢ → 15% ×1.25 in the log.
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

		// fit-width (§3.4) — times the zoom a person asked for, which is also what the render was
		// asked for: a zoomed page is wider than the column, and the pan reaches the rest
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

		// the page badge: `3 / 12`, bottom centre, alive on hover and 1.5s after
		self.badge(
			painter,
			pos2(column.center().x, column.max.y - PAD),
			Align2::CENTER_BOTTOM,
			&format!("{} / {total}", page + 1),
			tokens,
		);
	}

	/// The sheet's own grid (P41): the used range painted as a table — the file's column widths and
	/// row heights, each cell's text clipped where it stands, an empty cell marching through all
	/// the same, the first row bolded under a line of its own, and every merged span drawn from
	/// its anchor with no line inside it.
	fn draw_sheet(&mut self, painter: &Painter, content: Rect, tokens: &Tokens) {
		let max = match &self.show {
			Show::Table { table } => (table.height() + PAD - content.height()).max(0.0),
			_ => return,
		};
		self.offset = self.offset.clamp(0.0, max);
		let Show::Table { table } = &self.show else { return };

		let clip = painter.with_clip_rect(content);
		let painter = &clip;
		let font = FontId::new(13.0, FontFamily::Monospace);
		let bold = FontId::new(13.0, FontFamily::Name("maple-bold".into()));
		let line = tokens.border;
		let anchor = tokens.border_strong;

		let cols = table.cols.len();
		let rows = table.rows.len();
		if cols == 0 || rows == 0 {
			return;
		}

		// every column's left edge and the range's right; every row's top and its bottom
		let mut xs = Vec::with_capacity(cols + 1);
		let mut x = content.min.x;
		for w in &table.cols {
			xs.push(x);
			x += w;
		}
		xs.push(x);

		let mut ys = Vec::with_capacity(rows + 1);
		let mut y = content.min.y - self.offset;
		for h in &table.rows {
			ys.push(y);
			y += h;
		}
		ys.push(y);

		// the first row's own band: the header's shading, under the grid's own lines
		let band = Rect::from_min_max(pos2(content.min.x, ys[0]), pos2(content.max.x, ys[1]));
		if band.max.y > content.min.y && band.min.y < content.max.y {
			painter.rect_filled(band, 0.0, tokens.raised);
		}

		// the grid itself: every boundary the window shows, and none inside a merged span — a
		// span's border closes across it, its body carries no line of its own (P41's ④)
		for r in 0..rows {
			if ys[r + 1] < content.min.y || ys[r] > content.max.y {
				continue;
			}
			for (b, x) in xs.iter().enumerate() {
				let interior = table.merges.iter().any(|&(r1, c1, r2, c2)| c1 < b && b <= c2 && r1 <= r && r <= r2);
				if interior {
					continue;
				}
				painter.rect_filled(Rect::from_min_max(pos2(*x, ys[r]), pos2(*x + 1.0, ys[r + 1])), 0.0, line);
			}
		}
		for (b, y) in ys.iter().enumerate() {
			if *y < content.min.y - 1.0 || *y > content.max.y + 1.0 {
				continue;
			}
			for c in 0..cols {
				let interior = table.merges.iter().any(|&(r1, c1, r2, c2)| r1 < b && b <= r2 && c1 <= c && c <= c2);
				if interior {
					continue;
				}
				painter.rect_filled(Rect::from_min_max(pos2(xs[c], *y), pos2(xs[c + 1], *y + 1.0)), 0.0, line);
			}
		}

		// the cells: the anchors hold their text (a span's own across its width), the bodies say
		// nothing, and an empty cell is skipped while the grid marches on
		let advance = 13.0 * 0.62;
		for r in 0..rows {
			if ys[r + 1] < content.min.y || ys[r] > content.max.y {
				continue;
			}
			for c in 0..cols {
				let Some(text) = table.text(r, c) else { continue };
				let end = match table.span(r, c) {
					Some((_, _, _, c2)) => xs[(c2 + 1).min(cols)],
					None => xs[c + 1],
				};
				let budget = (((end - xs[c] - 8.0) / advance).floor().max(1.0)) as u32;
				painter.text(
					pos2(xs[c] + 4.0, (ys[r] + ys[r + 1]) * 0.5),
					Align2::LEFT_CENTER,
					crate::list::clipped(text, budget),
					if r == 0 { bold.clone() } else { font.clone() },
					tokens.text,
				);
			}
		}

		// the header's own line under the first row: the anchor the eye reads, drawn last so the
		// grid cannot cover it
		let at = ys[1].min(content.max.y);
		if at > content.min.y {
			painter.rect_filled(Rect::from_min_max(pos2(content.min.x, at - 1.0), pos2(content.max.x, at)), 0.0, anchor);
		}
	}

	/// A listing — a hovered directory's next level, or an archive's entry tree: the rows are
	/// built once (they do not wrap), drawn through the same display-row painter as every other
	/// text preview, and named at the bottom with the badge that says how much of it there is.
	fn draw_listing(&mut self, painter: &Painter, content: Rect, tokens: &Tokens) {
		// the rows are built once: a listing does not wrap, so nothing about it depends on the
		// column's width
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

		// the ↑↓ margins, the same the text previews keep
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

	/// The rows of whichever listing is up.
	fn listing_doc(&self) -> Option<&Doc> {
		match &self.show {
			Show::Dir { doc, .. } | Show::Archive { doc, .. } => doc.as_ref(),
			_ => None,
		}
	}

	/// How many entries that listing holds.
	fn listing_count(&self) -> usize {
		match &self.show {
			Show::Dir { entries, .. } => entries.len(),
			Show::Archive { items, .. } => items.len(),
			_ => 0,
		}
	}

	/// The archive tree's rows in column coordinates: where each stands and whether it is a folder
	/// a click may fold (P28's long tail). `None` when the column is not showing an archive, and an
	/// empty list while the rows have not been laid out yet.
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

	/// The fold under a point of the column (P28's long tail): the row the point stands on folds
	/// its folder, and only a folder answers — a file row leaves the gesture to the window. True
	/// when the point was taken.
	pub(crate) fn archive_fold_at(&mut self, column: Rect, at: Pos2) -> bool {
		let Some(rows) = self.archive_tree(column) else { return false };
		let Some((row, _, dir)) = rows.into_iter().find(|(_, rect, _)| rect.contains(at)) else { return false };

		dir && self.archive_fold(row)
	}

	/// Fold or unfold the folder a tree row stands for; false when that row is not a folder. The
	/// laid-out rows are dropped, so the next draw shows the tree the way it now stands.
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

	/// A directory the worker has not answered for, past the loading delay: the same static
	/// skeleton rows the middle column shows — placeholders and bars, nothing that claims to know
	/// what is in there yet.
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

	/// The zoom/page badge: 13px on `bg-raised@80%`, rounded 4, alive while pointed at and for
	/// 1.5s after (§3.1).
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

	/// The top 4px and bottom 12px of a column whose content is cut off: thinning slivers of
	/// `bg-sunken`, the painter's stand-in for a gradient (§3 总规则 / §3.8).
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

	/// A video's card (P29): the column has no frame — no tool on this machine, or a tool that
	/// failed — so it says what the container holds instead. The placeholder's own shape (the type
	/// icon, the name, the "按 Enter 打开" line) with the facts between them, and **no err colour
	/// anywhere**: a lacking tool is not a failure of the file.
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
		// whether it is executable is part of what the filetype rules colour (P26's third item)
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

		// a provider that failed in a way worth naming says so under the name (§3.4's own pattern)
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

	/// A textual preview: make sure the rows are laid out for the width on screen, keep the scroll
	/// inside them, and paint the rows that show.
	fn draw_text(&mut self, painter: &Painter, content: Rect, tokens: &Tokens) {
		let raw = self.raw.as_ref().expect("checked by the caller");
		let width = content.width();
		let key = format!("{}|{}", raw.path.display(), (width / 16.0).round() as u32);
		if self.laid.is_none() || self.laid_of.as_deref() != Some(key.as_str()) {
			// the layout wants `src` → size; the keys are the pane's own business
			let sizes: HashMap<String, [u32; 2]> =
				raw.images.iter().map(|(src, _, size)| (src.clone(), *size)).collect();
			let (doc, notice) = lay(raw.kind, &raw.text, width, tokens, raw.truncated, &sizes);
			self.laid = Some(doc);
			self.notice = notice;
			self.laid_of = Some(key);
		}

		// the code preview's syntax is filled only for the rows the view has reached
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
						// a wrapped line's highlight is computed on the whole source line (its
						// pieces put back together), then sliced back onto each piece — the walk
						// stays exactly one call per source line
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

		// a code line wraps now (P16), so there is nothing left to clip or fade at the right edge
		rows::draw(painter, content, doc, self.offset, tokens, None, !code);

		// the document's own pictures (P26's second item): their bands were reserved by the layout,
		// and the textures are the pane's, so they are painted here — half the column wide,
		// contained in the band, centred
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

		// the ↑↓ margins a text preview keeps (§3.3/§3.5)
		let font = FontId::new(13.0, FontFamily::Monospace);
		if self.offset > 0.5 {
			painter.text(pos2(content.max.x, content.min.y), Align2::RIGHT_TOP, "↑", font.clone(), tokens.subtext);
		}
		if self.offset < max - 0.5 {
			painter.text(pos2(content.max.x, content.max.y), Align2::RIGHT_BOTTOM, "↓", font, tokens.subtext);
		}

		// the truncation bar: a file past the spec's ceiling says so at the column's bottom
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

/// Bind the PDF library before anything asks for it: a missing one is a line at startup rather
/// than a surprise at the first hovered document.
pub(crate) fn warm_pdf() { pdf::warm(); }

/// What a picture's own head says about it (P23's spot card, reworked by P28②'s revision): the
/// pixels, the format and — where the header states it — the colour depth, read without decoding
/// a pixel, so the card answers the moment `Tab` is pressed. `None` for anything whose head does
/// not read as a picture.
pub(crate) fn picture_facts(path: &Path) -> Option<(img::Report, [u32; 2])> {
	use std::io::Read;

	// headers live at the front: 64KiB covers every format this window recognises, and a picture
	// nobody is going to look at is not read whole for a card's own lines
	let mut head = Vec::with_capacity(64 * 1024);
	std::fs::File::open(path).ok()?.take(64 * 1024).read_to_end(&mut head).ok()?;
	let (width, height) = img::dimensions(&head)?;

	// the format and the colour depth come from the same head (P28② revised) — no decode, so the
	// card answers the moment it is asked
	Some((img::header_report(&head), [ width, height ]))
}

/// A path spelled as the URI the copy-URL verbs need, from the freedesktop module that owns the
/// spelling.
pub(crate) fn file_uri(path: &Path) -> String {
	thumb::file_uri(&path.to_string_lossy())
}

/// A colour at an alpha, drawn over the pane by the fades.
fn faded(colour: Color32, alpha: u8) -> Color32 {
	Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), alpha)
}

/// The picture's own rows for the spot card (P28② revised): the format, the file's own pixels,
/// the ratio it is shown at on screen and its colour depth — the info block that stood under the
/// picture in the column is gone, and these are its words, moved to where `Tab` asks for them.
/// Chinese labels in the card's own vocabulary (P23). `shown` is the picture's on-screen size in
/// physical pixels — `None` when the column is not showing this file, and the ratio row steps out
/// with the rest; every row whose datum is not there steps out the same way, and no row takes
/// space for a datum nobody has.
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

/// Lay a textual answer out: code and text keep their lines and their numbers, docx and a sheet
/// are wrapped to the column, markdown goes through its own layout. `notice` is the truncation
/// bar the caller paints.
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

			// a code line wraps at the column's own text width (P16): the gutter takes what it
			// takes, and what is left is measured in cells — the same CJK-aware wrap the documents
			// use, so a Latin word moves whole and a CJK line breaks anywhere
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
						// one number a source line: it rides the first piece only, and the
						// continuations hang under the first piece's own text (indent 0), never
						// under the number
						number: (p == 0).then_some(i + 1),
						size: 16.0,
						height: 24.0,
						colour: tokens.text,
						// the diff's band only covers the line's first piece
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

/// A directory listing as display rows: the same row anatomy the middle column draws — a 20px
/// icon slot, the name, and the size and time right-aligned in the secondary face — as rows the
/// pane can virtualize like any other text.
/// Past this many entries a preview's listing stops: the badge still says the true count.
const DIR_CAP: usize = 20_000;

/// How many archive entries the tree shows: the archive reader stops at the same number.
const ARCHIVE_CAP: usize = archive::CAP;

/// An archive's entry tree as display rows: the list's anatomy (lead glyph slot, name, size fact),
/// indented a level at a time, folded by the pane's own set of shut folders, with the remainder
/// said in a last row when the cap cut it short (P28's long tail for the fold).
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

	// icon and name only (P12): the size and time live in the middle column, and a preview's
	// listing that repeated them would crowd the names in any column narrower than a full window
	Row {
		text: entry.name.clone(),
		height: ROW_HEIGHT,
		// the list's own anatomy: the name starts past the icon slot (spec §1.1)
		indent: ICON_BOX + 2.0 * rows::advance(16.0),
		size:   16.0,
		colour: if entry.dir { tokens.blue } else { tokens.text },
		lead:   Some((glyph.to_owned(), colour)),
		..Default::default()
	}
}

/// A source line's first row and the pieces after it, and how many rows that is: a wrapped code
/// line lives as consecutive rows, the first one numbered.
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

/// The runs that fall inside one piece of a wrapped line: a slice of the whole line's highlight,
/// with the ranges rebased to the piece's own text.
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

/// How many digits the highest line number takes — what the gutter's width follows.
fn digits_of(count: usize) -> usize { count.max(1).to_string().len() }

/// The cells a code preview line may fill: the column's own width, less the gutter the numbers
/// take. Pure, so the wrap width can be read and tested without a window.
fn code_columns(width: f32, digits: usize) -> u32 {
	let gutter = rows::gutter(digits, rows::advance(13.0)).border;

	(((width - gutter) / rows::advance(16.0)).floor() as u32).max(4)
}

/// One line of a document, as however many lines the column needs: a CJK glyph may be broken
/// after any of them, Latin only at the spaces between its words, and a word longer than a whole
/// line is cut where it stands. The legacy window's wrap, kept because it was verified there.
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

/// The worker: read the file **once**, decode or extract it away from the window's thread, and
/// send each answer as it exists. The window is asked back even when an answer is empty, because
/// its own poll is what ends the `载入中…`.
fn run(ctx: &Context, job: Look, tx: Sender<Answer>) {
	let Look { generation, path, kind, len, mtime, target, page } = job;
	let started = Instant::now();
	let name = path.to_str().unwrap_or_default().to_owned();
	let key = img::key(&name, mtime, target);
	// where the answer came from (P27⑥): the bucket, the shared thumbnail, a text cache or the
	// file itself — the acceptance pairs it with the file and the milliseconds in one line
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

			// a GIF is an animation: its frames come back together, coarsely reduced to the panel,
			// and the first of them is the picture that goes up now (P26's first item, P27⑥'s
			// exception — original-size frames would be hundreds of megabytes)
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

			// the answer's own road is told for the acceptance: the window's bucket, the shared
			// thumbnail, or the file itself (P27⑥)
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

			// the bucket first (P27⑥): a page rendered once is shown again without pdfium
			if let Some((size, total, rgba)) = img::pdf_bucket_load(&name, mtime, target, page) {
				road = Some("bucket");
				let _ = tx.send(Answer {
					generation,
					key: format!("{key}#p{page}"),
					payload: Payload::Pdf { page, total, size, rgba },
				});
			} else {
				match pdf::render(&bytes, target, page) {
					// a rendered page is a picture of *this* page: the key carries the page number
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
			// the window's own text cache first (P27⑥): a file read once is not read again —
			// its pictures are still walked, because they are cheap and the rows want them
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

			// a binary file is not a text preview, whatever its extension promised
			if bytes.iter().take(8192).any(|byte| *byte == 0) {
				send(Payload::Nothing);
			} else {
				let text = String::from_utf8_lossy(&bytes).into_owned();
				// a document's own pictures are read here, off the window's thread (P26's second
				// item): at most a handful, at most a few megabytes, and never a fetch
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
				// a sheet is a table, not a text (P41): the pane draws the grid and its cells
				_ => match sheet::Sheet::read(&bytes) {
					Some(table) => send(Payload::Table(table)),
					None => send(Payload::Nothing),
				},
			}
		}
		Kind::Video => {
			// the frame first (P29): the tools the machine happens to have, the bucket always
			match video::frame(&path, mtime, target) {
				Some((size, source, rgba)) => {
					send(Payload::Image { size, source, rgba, frames: Vec::new() });
				}
				None => {
					// no tool, or a tool that failed: the facts the machine can read anyway, as the
					// card the column shows in place of a picture — never an error
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
			// a real archive family, and no decoder here reads it (P28's long tail): the reason
			// names the format, so the placeholder says what this window cannot do
			Err(archive::Fail::Unsupported(format)) => {
				send(Payload::Degraded(format!("{format} 压缩包暂不支持预览（无纯 Rust 解码器）")))
			}
		},
		Kind::Placeholder => send(Payload::Nothing),
	}

	answered(kind, started, road, &name);
	ctx.request_repaint();
}

/// The one answer line (P27⑥): the kind, the milliseconds, where the answer came from and whose
/// file it was — the acceptance counts the roads per row and takes its medians from this.
fn answered(kind: Kind, started: Instant, road: Option<&str>, name: &str) {
	if !crate::metrics::enabled() {
		return;
	}

	let road = road.map_or(String::new(), |road| format!(" from {road}"));
	let file = Path::new(name).file_name().map_or_else(|| name.to_owned(), |file| file.to_string_lossy().into_owned());
	eprintln!("Timing: preview {kind:?} answered in {:.0}ms{road} {file}", started.elapsed().as_secs_f64() * 1000.0);
}

/// How large an inline picture may be before the preview lets it go: the row is drawn at half a
/// column, so anything past this is only ever thrown away by the resample.
const INLINE_MAX: u32 = 1024;

/// How much of one inline picture the worker will read: past this it is not a picture a row is
/// for, and the row says it could not be read.
const INLINE_BYTES: u64 = 16 * 1024 * 1024;

/// The pictures a document asks for, decoded off the window's thread (P26's second item): the
/// freedesktop thumbnail when the shared cache already has one, the file itself otherwise, each
/// resampled to [`INLINE_MAX`] and keyed by its own absolute path so the pane can paint it. A
/// picture that is not there, or cannot be read, is simply left out — the layout turns the missing
/// key into a line of words.
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

/// One inline picture: the shared thumbnail if it is fresh, else the file's own decode — either
/// way resampled to what a row at half a column can show.
fn inline_picture(path: &Path, len: u64, mtime: Option<SystemTime>) -> Option<([u32; 2], Vec<u8>)> {
	let decode = |bytes: &[u8]| -> Option<([u32; 2], Vec<u8>)> {
		let picture = image::load_from_memory(bytes).ok()?.to_rgba8();
		let size = [ picture.width(), picture.height() ];
		let to = img::fit((size[0], size[1]), [ INLINE_MAX, INLINE_MAX ]);
		let rgba = img::resample(picture.as_raw(), size, [ to.0, to.1 ])?;

		Some(([ to.0, to.1 ], rgba))
	};

	// the freedesktop cache first: a document shown twice pays for its pictures once
	if let Some(png) = thumb::Thumb::of(&path.to_string_lossy(), mtime, len).load()
		&& let Some(answer) = decode(&png)
	{
		return Some(answer);
	}

	let bytes = std::fs::read(path).ok()?;
	decode(&bytes)
}

/// How long a directory must sit still before the sweep starts (P27⑥'s L3): a person still
/// walking the list is not a person waiting for it to be decoded.
const SWEEP_IDLE: Duration = Duration::from_millis(500);

/// The pictures among a set of rows, ready for a predecode: images only, with what a cache entry
/// is judged by (P27⑥).
fn image_jobs(rows: &[(PathBuf, Option<Entry>)]) -> Vec<(PathBuf, u64, Option<SystemTime>)> {
	rows.iter()
		.filter(|(path, entry)| Kind::of(path, entry.as_ref().is_some_and(|e| e.dir)) == Kind::Image)
		.map(|(path, entry)| {
			let (len, mtime) = entry.as_ref().map_or((0, None), |e| (e.size, e.modified));
			(path.clone(), len, mtime)
		})
		.collect()
}

/// Take the preloader's one slot, waiting a tick at a time: a predecode is never in that much of
/// a hurry, and the wait is what keeps a preload off a hover's decode (P27⑥).
fn preload_slot(slot: Arc<Semaphore>) -> Option<tokio::sync::OwnedSemaphorePermit> {
	for _ in 0..600 {
		if let Ok(permit) = slot.clone().try_acquire_owned() {
			return Some(permit);
		}
		std::thread::sleep(Duration::from_millis(25));
	}

	None
}

/// The window's own text cache (P27⑥): `~/.cache/crabhop/preview-text`, keyed by path and
/// modification time — the words of a file cost their read once, not once a session. No
/// modification time is nothing to judge an entry by, so nothing is read or written then.
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

/// A prefetch: decode the file into the window's own bucket and the shared thumbnail cache, and
/// nothing else. No answer is ever sent — a warm cache is not a picture on screen (P27⑥).
fn run_warm(path: &Path, len: u64, mtime: Option<SystemTime>, target: [u32; 2]) {
	let Ok(bytes) = std::fs::read(path) else { return };

	let _ = img::warm(path.to_str().unwrap_or_default(), mtime, len, &bytes, target);
}

/// The head of a file, and whether that was all of it. One byte past the cap is read, which is
/// what tells a file that ends there from one that continues.
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

	/// What a hovered file is, by its extension: the decoder's own list, the document types, and
	/// everything else read before it is judged.
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
		// named as archives even where no decoder here reads them: the placeholder says why (P28)
		assert_eq!(Kind::of(Path::new("/a.7z"), false), Kind::Archive, "7z is named, then refused with a reason");
		assert_eq!(Kind::of(Path::new("/a.rar"), false), Kind::Archive);
		assert_eq!(Kind::of(Path::new("/a.bin"), false), Kind::Code, "no reader: it is read before it is judged");
	}

	/// The legacy wrap, kept whole: a wrapped line fits the column and loses nothing.
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

	/// The textual layout of each provider: a code file keeps one row a line with numbers, a docx
	/// wraps, markdown goes through its own ladder, and a truncation says so.
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

		// a diff is the one special case: plus and minus lines carry the state colours
		let (doc, _) = lay(Kind::Code, "diff --git a/x b/x\n--- a/x\n+++ b/x\n+added\n-removed", 400.0, &tokens, false, &HashMap::new());
		assert!(doc.rows[3].bg.is_some(), "the + line has a band");
		assert!(doc.rows[4].bg.is_some(), "and so does the - line");
	}

	/// A document whose text wraps into pieces is reachable end to end: every piece is a row in the
	/// column's own extent, so the wheel — and `K`/`J` — can walk a docx to its last row instead of
	/// stopping on the first screen (P18).
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

	/// A picture is shown fit-width (P27's first item — the final ruling): the column's own width,
	/// the file's own shape, and a picture taller than the column is a page the wheel walks — never
	/// squashed into it.
	#[test]
	fn a_picture_is_shown_at_the_columns_width_and_tall_ones_scroll() {
		// a 400 × 900 column and a 200 × 1600 picture: the shown width is the column's 400, and the
		// height follows the file's shape — 3200, well past the column
		let mut pane = PreviewPane { show: image("k", [ 200, 1600 ]), target: [ 400, 900 ], view: 900.0, ..PreviewPane::default() };

		let height = pane.image_height().expect("a picture's own height");
		assert!((height - 3200.0).abs() < 0.01, "the column's width, the file's shape: {height}");
		assert!(pane.content() > pane.view, "a tall picture is a page, not a fit");
		assert!(pane.scroll(3.0), "and the wheel walks it");

		// a picture the column's width leaves shorter than the column: nothing to walk
		pane.show = image("k", [ 800, 200 ]);
		let height = pane.image_height().expect("height");
		assert!((height - 100.0).abs() < 0.01, "800 wide at the column's 400 gives 100 tall: {height}");
		assert!(pane.content() <= pane.view && !pane.scroll(3.0), "there is nothing for the wheel to walk");

		// a small picture is shown at the column's width too (the fit-width size is the size a
		// person sees), and its shape still decides the height
		pane.show = image("k", [ 100, 60 ]);
		let height = pane.image_height().expect("height");
		assert!((height - 240.0).abs() < 0.01, "400 wide from a 100 × 60 file: {height}");
	}

	/// A zoom rides on the fit and may pass it (P28①): a step is 1.25× of the fit, `zoom full`
	/// lands on one file pixel a screen pixel, and `zoom fit` is the fit again.
	#[test]
	fn a_zoom_rides_on_top_of_the_fit_and_can_pass_it() {
		let mut pane = PreviewPane { show: image("k", [ 200, 800 ]), target: [ 400, 900 ], view: 720.0, ..PreviewPane::default() };

		let fit = pane.image_height().expect("the fit");
		assert!(pane.zoom_by(true));
		assert!((pane.image_height().expect("zoomed") - fit * 1.25).abs() < 0.01, "a step multiplies the fit");
		assert!(pane.zoomed(), "and the column knows a picture may now be dragged");

		// `zoom full`: the shown physical width is the file's own, on any display
		assert!(pane.zoom_full());
		let ppp = pane.ctx.pixels_per_point();
		let shown = pane.shown_size([ 200, 800 ]);
		assert!(((shown.x * pane.zoom * ppp) - 200.0).abs() < 0.01, "100% is one file pixel a screen pixel");

		assert!(pane.zoom_fit());
		assert!((pane.image_height().expect("back to the fit") - fit).abs() < 0.01);
		assert!(!pane.zoomed());
	}

	/// The wheel moves the column a line at a time, and says so when the document fits: the
	/// extent, not the first screen, is what it measures against (P18).
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

		// a document that fits the column has nothing to scroll
		pane.view = pane.content() + 1.0;
		assert!(!pane.scroll(3.0), "no extent, no wheel");
		assert_eq!(pane.offset, 0.0);
	}

	/// `K`/`J` walk the document five lines at a time — the spec's own 120px step, not five times
	/// it (P18's correction) — and an image has nothing to seek.
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

	/// A pane that has just looked at a file shows nothing of it yet — the double buffer holds the
	/// screen — and the queued job is not started before its debounce is over and the column has
	/// told it how much room there is.
	#[test]
	fn a_hover_is_queued_and_not_shown_before_its_answer() {
		let mut pane = PreviewPane::default();
		pane.look(Path::new("/tmp/crabhop-n2-never/a.png"), None);

		assert!(pane.queued.is_some(), "the job waits for its debounce");
		assert!(matches!(pane.show, Show::Empty), "and nothing has replaced the screen yet");

		pane.advance();
		assert!(pane.queued.is_some(), "with no column drawn, nothing is started");
		assert!(pane.pending.is_none());

		// a directory is read on a worker: it queues too, and the screen keeps what it had
		pane.look(Path::new("/tmp/crabhop-n2-never/dir"), Some(&dir_entry()));
		assert_eq!(pane.kind, Kind::Dir);
		assert!(pane.queued.is_some(), "a listing is not read on this thread");
		assert!(matches!(pane.show, Show::Empty), "and nothing has replaced the screen yet");

		// a format with no decoder is known at once
		pane.look(Path::new("/tmp/crabhop-n2-never/a.avif"), None);
		assert!(matches!(pane.show, Show::Placeholder(_)));
		assert!(pane.queued.is_none());
	}

	/// The fold from the column's own point (P28's long tail): the point decides which row answers,
	/// a folder folds and a file does not, and the rows the tree shows follow the fold.
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

		// the column's own geometry: content starts PAD in, the rows stand 24px apart
		let column = Rect::from_min_max(pos2(0.0, 100.0), pos2(400.0, 500.0));
		let rows = pane.archive_tree(column).expect("an archive tree with its rows laid out");
		assert_eq!(rows.len(), 4);
		assert!(rows[1].2, "row one is the shut folder");
		assert!(!rows[3].2, "row three is a file");

		// a file row does not answer, and the tree does not move
		assert!(!pane.archive_fold_at(column, pos2(100.0, 100.0 + PAD + 2.0 * 24.0 + 12.0)), "a file is not a fold");
		assert_eq!(pane.archive_rows_for_tests(), Some(4));

		// the folder's own row does: its file comes out, and a second fold puts it back
		assert!(pane.archive_fold_at(column, pos2(100.0, 100.0 + PAD + 24.0 + 12.0)));
		assert_eq!(pane.archive_rows_for_tests(), Some(5), "deep is open now");
		assert!(pane.archive_fold(1), "row one of the open tree is deep again");
		assert_eq!(pane.archive_rows_for_tests(), Some(4));
	}

	/// An archive's tree: the rows are the archive's own entries, and a listing the cap cut short
	/// says so in a last row — with the true count when the format knew it.
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

		// and a tar, whose total nobody knows until it has all gone by
		let (doc, truncated) = archive_doc(&long, None, &open, &tokens);
		assert!(truncated);
		assert!(doc.rows[ARCHIVE_CAP].text.contains("还有更多条目"), "{:?}", doc.rows[ARCHIVE_CAP].text);
	}

	/// A long code line wraps instead of being cut (P16): nothing is lost, every piece fits the
	/// column, only the source line's first row carries its number, and the continuations start
	/// where the first piece's text does — under the text, never under the number.
	#[test]
	fn a_long_code_line_wraps_under_its_own_text() {
		let tokens = Tokens::MOCHA;
		let line = r#"let x = "这是一个很长的中文与 English 混排的赋值表达式，加上一段足够长的尾巴";"#;
		let width = 400.0;

		let (doc, _) = lay(Kind::Code, line, width, &tokens, false, &HashMap::new());
		assert!(doc.rows.len() > 1, "a line this long cannot fit one row");

		// no truncation: the pieces put back together are the source line, character for character
		let put_back: String = doc.rows.iter().map(|row| row.text.as_str()).collect();
		assert_eq!(put_back, line, "not a character may be lost to the wrap");

		// the gutter numbers the source line once, on its first row
		assert_eq!(doc.rows[0].number, Some(1));
		assert!(doc.rows[1..].iter().all(|row| row.number.is_none()), "continuations carry no number");

		// the hanging indent: every piece starts at the same text edge (indent 0 = after the
		// gutter's hairline), so the continuations hang under the first piece's text
		assert!(doc.rows.iter().all(|row| row.indent == 0.0), "the text edge is the same for all pieces");

		// and every piece fits the cells the column allows beside the gutter
		let cols = code_columns(width, digits_of(1));
		for row in &doc.rows {
			let cells: u32 = row.text.chars().map(list::cell_width).sum();
			assert!(cells <= cols, "{:?} is wider than the column's {cols} cells", row.text);
		}
	}

	/// The wrap width follows the gutter: a longer file's digits take a little of the column, and
	/// the pieces shrink to pay for it — while a narrow column still gets a workable minimum.
	#[test]
	fn the_code_wrap_width_pays_for_its_gutter() {
		let one = code_columns(400.0, 1);
		let four = code_columns(400.0, 4);
		assert!(four < one, "four digits take cells from the line: {four} vs {one}");
		assert!(one - four <= 8, "and no more than the digits themselves cost");

		assert!(code_columns(40.0, 1) >= 4, "a sliver of a column still wraps at a workable width");
	}

	/// A hovered archive goes to a worker like a directory does: nothing is read on this thread.
	#[test]
	fn a_hovered_archive_queues_its_listing() {
		let mut pane = PreviewPane::default();
		pane.look(Path::new("/tmp/crabhop-p11-never/a.zip"), None);

		assert_eq!(pane.kind, Kind::Archive);
		assert!(pane.queued.is_some(), "the listing is read on a worker");
		assert!(matches!(pane.show, Show::Empty), "and nothing has replaced the screen yet");
	}

	/// A directory's listing wears the middle column's own row anatomy: the icon slot, the blue a
	/// directory's name keeps, and the facts right-aligned in the secondary face.
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

	/// The pane's own cache key: the same file, time and shape reuse a texture; a different shape
	/// bucket asks again.
	#[test]
	fn the_texture_key_is_path_time_and_bucket() {
		let at = SystemTime::UNIX_EPOCH + Duration::from_secs(9);
		let a = img::key("/x.png", Some(at), [ 300, 200 ]);
		let b = img::key("/x.png", Some(at), [ 310, 250 ]);
		assert_eq!(a, b, "one bucket, one texture");
		assert_ne!(a, img::key("/x.png", Some(at), [ 700, 200 ]));
	}

	/// A picture on screen, for the layout tests: the key is nobody's texture.
	fn image(key: &str, source: [u32; 2]) -> Show {
		Show::Image { key: key.to_owned(), source }
	}

	/// The spot card's image section (P28② revised): Chinese labels, the header's format, the
	/// file's own pixels, the ratio the column is showing it at, the colour depth — and a row whose
	/// datum is not there steps out rather than taking a line for nothing.
	#[test]
	fn the_cards_image_section_says_what_a_picture_is() {
		let full = img::Report { format: Some("PNG".to_owned()), colour: Some("8 位 RGBA".to_owned()) };

		let rows: HashMap<_, _> = image_rows(&full, [ 3840, 2160 ], Some(456.0)).into_iter().collect();
		assert_eq!(rows.get("格式").map(String::as_str), Some("PNG"));
		assert_eq!(rows.get("尺寸").map(String::as_str), Some("3840 × 2160 像素"));
		assert_eq!(rows.get("显示比例").map(String::as_str), Some("12%"), "456 of 3840");
		assert_eq!(rows.get("色深").map(String::as_str), Some("8 位 RGBA"));
		assert_eq!(rows.len(), 4, "four words and no more — the card's own rows carry size and time");

		// the column is showing nothing of this file: the ratio row steps out
		let rows: HashMap<_, _> = image_rows(&full, [ 3840, 2160 ], None).into_iter().collect();
		assert!(!rows.contains_key("显示比例"), "{rows:?}");

		// a header that read nothing: its rows step out with it
		let bare = img::Report { format: None, colour: None };
		let rows: HashMap<_, _> = image_rows(&bare, [ 100, 50 ], Some(100.0)).into_iter().collect();
		assert!(!rows.contains_key("格式") && !rows.contains_key("色深"), "{rows:?}");
		assert_eq!(rows.get("显示比例").map(String::as_str), Some("100%"), "a small picture shows its own size");
		assert_eq!(rows.get("尺寸").map(String::as_str), Some("100 × 50 像素"), "the pixels are the header's own");
	}

	fn dir_entry() -> Entry {
		Entry { name: "dir".to_owned(), size: 0, modified: None, dir: true, mode: None, link: false , created: None, uid: 0 }
	}

	/// The idle sweep (P27⑥'s L3): a directory visit starts one, the empty first view of a
	/// directory is filled the moment the listing lands, another directory is another visit, and
	/// nothing runs without a column to fill.
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

		// the first view of a directory is often still empty: the sweep is born without a queue
		pane.warm(&dir, &[], &[], &[]);
		let stamp = pane.sweep.as_ref().expect("the visit made a sweep").stamp;
		assert!(pane.sweep.as_ref().expect("a sweep").queue.is_empty(), "nothing to sweep before the names land");

		// the listing lands: the queue fills under the same visit
		pane.warm(&dir, &pictures, &[], &pictures);
		let sweep = pane.sweep.as_ref().expect("still the same sweep");
		assert_eq!(sweep.queue.len(), 3, "the pictures the view holds");
		assert_eq!(sweep.stamp, stamp, "and it is the same visit");

		// a new directory is a new visit: the old stamp is left behind
		let other = PathBuf::from("/tmp/crabhop-n2-never/other");
		pane.warm(&other, &[], &[], &[]);
		assert_ne!(pane.sweep.as_ref().expect("a sweep").stamp, stamp, "a new visit, a new stamp");

		// a pane that has not been drawn does not sweep, and neither does a queue with no names in it
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

		// a queue with names in it, and quiet past its half-second, is handed over
		pane.warm(&queueless, &pictures, &[], &pictures);
		pane.sweep.as_mut().expect("a sweep").since = Instant::now() - Duration::from_millis(900);
		pane.sweep_tick();
		assert!(pane.sweep.is_none(), "a quiet half-second past, the queue is handed over");
	}
}