//! The image pipeline, moved here from the legacy window and held to the Zed rules the task names:
//! a cache key of `(path, mtime, size bucket)`, one or two answers per file (a fast resample, then
//! the fine one), and a freedesktop thumbnail read before any decode.
//!
//! Nothing here touches a window: `decode_into` hands each answer to a callback, and the pane is
//! what uploads it.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::preview::thumb::Thumb;

/// The answer a picture's decode can give.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Outcome {
	/// It is on its way to the screen, in one or two stages.
	Shipped,
	/// Too many pixels to be a preview's business: named, not decoded.
	TooBig,
	/// Nothing could be made of the bytes (and the shared cache was told so).
	Unreadable,
}

/// The width a clip is held to, in device pixels: a cheap way to keep a jitter of a resize from
/// asking for a fresh decode. Two columns within this of each other share one texture.
const BUCKET: u32 = 64;

/// How many decodes may run at once, and the number of slots the pane hands out. Blocking threads
/// that cannot be taken back, so the cap keeps a burst of hovers from queueing behind each other;
/// the floor of 2 keeps a two-core laptop able to decode anything at all.
pub(crate) fn slots() -> usize {
	std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(2, 4)
}

/// The bucket a target falls in, `(width, height)`.
pub(crate) fn bucket(target: [u32; 2]) -> (u32, u32) {
	(target[0].div_ceil(BUCKET) * BUCKET, target[1].div_ceil(BUCKET) * BUCKET)
}

/// The pixels a request is rendered for: the column's own target, times the zoom a person asked
/// for. A zoomed PDF is therefore re-rendered rather than stretched, and the cache key's size
/// bucket moves with it — which is what asks for the new render at all. Bounded so a deep zoom
/// cannot ask pdfium for a poster.
pub(crate) fn request_target(target: [u32; 2], zoom: f32) -> [u32; 2] {
	const MAX: u32 = 6000;

	let scaled = |side: u32| ((side as f32 * zoom).round() as u32).clamp(1, MAX);

	[ scaled(target[0]), scaled(target[1]) ]
}

/// The cache key the task names: the path, the modification time, and the **width** bucket (P32's
/// 1:1 model). The height is never part of it — each file's own shape decides that — so two
/// pictures of different shapes share a key while the column is showing them at the same width,
/// and the width is exactly what the zoom moves: a zoom asks for a new reduction, a zoom back
/// finds the old one.
pub(crate) fn key(path: &str, mtime: Option<SystemTime>, target: [u32; 2]) -> String {
	let mtime = mtime
		.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
		.map_or(0, |d| d.as_millis());
	let (w, _) = bucket(target);

	format!("{path}|{mtime}|{w}")
}

/// How much memory the decoded textures may be counted against before the least recently used ones
/// are dropped (§3.8: images ≤512MB; a settings switch for it is N3).
pub(crate) const TEXTURE_CAP: usize = 512 * 1024 * 1024;

/// The raster formats this window previews: exactly what the workspace's `image` features can
/// decode. AVIF is not among them (no decoder is built), and anything unknown stays a text preview.
pub(crate) const FORMATS: &[&str] = &[
	"png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff", "qoi", "tga", "dds", "hdr", "exr", "ff", "pnm",
	"pbm", "pgm", "ppm",
];

pub(crate) fn recognises(ext: &str) -> bool {
	FORMATS.iter().any(|known| known.eq_ignore_ascii_case(ext))
}

/// How much memory a decode may be expected to take before a preview refuses it: four bytes a
/// pixel, and more while it works. A large picture is not refused for being large — it is decoded
/// like any other — but one whose pixels alone would take half a gigabyte is past what a preview
/// column is for, and its header is refused before a pixel is touched.
const MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;

/// What the column says when a picture is too large to decode.
pub(crate) const NOTE_TOO_BIG: &str = "图片过大，已跳过预览";

/// Whether a picture is past that much memory, decided from its header alone.
pub(crate) fn too_big(width: u32, height: u32) -> bool {
	width as u64 * height as u64 * 4 > MAX_DECODE_BYTES
}

/// Fit `size` into `target` without distorting it: the preview column keeps the aspect ratio, and
/// a small image is never blown up past its own pixels.
pub(crate) fn fit(size: (u32, u32), target: [u32; 2]) -> (u32, u32) {
	let (w, h) = size;
	let (tw, th) = (target[0].max(1), target[1].max(1));
	if w == 0 || h == 0 {
		return (0, 0);
	}

	let scale = f64::min(tw as f64 / w as f64, th as f64 / h as f64);
	if scale >= 1.0 {
		return (w, h);
	}

	((w as f64 * scale).round().max(1.0) as u32, (h as f64 * scale).round().max(1.0) as u32)
}

/// The display size of a fit-width picture (P27's first item — the sizing rule the final ruling
/// kept): the column's own width, and the file's own shape with it. Never squashed; a picture
/// taller than the column is taller than the column, and its own scroll walks it. Points in,
/// points out, pure, so the rule can be read and tested without a window.
pub(crate) fn fit_width(size: (u32, u32), width: f32) -> [f32; 2] {
	let (w, h) = (size.0.max(1) as f32, size.1.max(1) as f32);

	[ width, width * h / w ]
}

/// Contain into the column, both ways: the picture is scaled to touch whichever edge its shape
/// reaches, up as well as down, and centred by the caller. A preview that leaves a third of the
/// column empty around a small picture says less than one that fills it (§3.1's contain).
pub(crate) fn contain(size: (u32, u32), target: [u32; 2]) -> (u32, u32) {
	let (w, h) = size;
	let (tw, th) = (target[0].max(1), target[1].max(1));
	if w == 0 || h == 0 {
		return (0, 0);
	}

	let scale = f64::min(tw as f64 / w as f64, th as f64 / h as f64);

	((w as f64 * scale).round().max(1.0) as u32, (h as f64 * scale).round().max(1.0) as u32)
}

/// How many frames of an animation a preview keeps: past this the first two hundred and forty are
/// what plays — a minute of animation at any sane frame rate — which keeps one preview's memory in
/// the tens of megabytes rather than the hundreds.
pub(crate) const FRAME_CAP: usize = 240;

/// How short a frame's own delay may be before the window stops believing it: a zero-delay GIF is
/// a repaint storm, and twenty milliseconds is already faster than the eye reads.
const FRAME_FLOOR_MS: u32 = 20;

/// What an animation's decode hands back: the size it was drawn at, the source picture's own size,
/// and the frames themselves with the delay each stands for.
pub(crate) type Animation = ([u32; 2], [u32; 2], Vec<(Vec<u8>, u32)>);

/// An animation's frames, resampled to the column, with the delay each is to stand for — the first
/// frame first, so a still picture is the same list with one entry. Only the image crate's own
/// `AnimationDecoder` is asked (P26's first item), and only GIFs animate here.
pub(crate) fn decode_frames(target: [u32; 2], bytes: &[u8]) -> Option<Animation> {
	use image::AnimationDecoder;

	let gif = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).ok()?;
	let mut frames = Vec::new();
	let mut drawn = [ 0u32, 0u32 ];

	// the target is the pane's own, and it carries the same 1:1 rule as a still picture's (P32): a
	// height of zero means the frame's own shape decides it, and every frame is reduced to the
	// rectangle it will be drawn in — the coarse filter, because a hundred frames of Lanczos is
	// nobody's preview
	for frame in gif.into_frames().take(FRAME_CAP) {
		let Ok(frame) = frame else { break };
		let delay = frame_ms(frame.delay());
		let buffer = frame.into_buffer();
		let size = [ buffer.width(), buffer.height() ];
		let to = upload_size(size, target);
		let rgba = resample_fast(buffer.as_raw(), size, to)?;
		drawn = to;
		frames.push((rgba, delay));
	}

	if frames.is_empty() {
		return None;
	}

	// the file's own dimensions when its header still answers, the drawn ones otherwise: the badge
	// measures the screen against the file, not against this resample
	let source = dimensions(bytes).map_or(drawn, |(w, h)| [ w, h ]);

	Some((drawn, source, frames))
}

/// A GIF frame's delay in milliseconds, floored so a zero-delay file cannot spin the window.
fn frame_ms(delay: image::Delay) -> u32 {
	let (numerator, denominator) = delay.numer_denom_ms();
	if denominator == 0 {
		return FRAME_FLOOR_MS;
	}

	(numerator / denominator).max(FRAME_FLOOR_MS)
}

/// A picture's own dimensions, from its header and without decoding a pixel.
pub(crate) fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
	image::ImageReader::new(std::io::Cursor::new(bytes))
		.with_guessed_format()
		.ok()?
		.into_dimensions()
		.ok()
}

/// What a picture's own header says about it (P28②, revised): the format the decoder recognises
/// from the header alone, and the colour type where the format's header states it — no decode, so
/// a spot card can ask at once. A field that is `None` is left out of the card's section.
#[derive(Clone)]
pub(crate) struct Report {
	pub format: Option<String>,
	pub colour: Option<String>,
}

/// The report a picture's own header allows (P28② revised): the format and — where the format's
/// header states it — the colour depth, read by the `image` crate's own decoders without decoding
/// a pixel. What the header does not say stays `None`, and the card leaves that row out.
pub(crate) fn header_report(bytes: &[u8]) -> Report {
	Report { format: format_of(bytes), colour: header_colour(bytes).map(colour_words) }
}

/// The colour type a picture's header states, without decoding it: a decoder reads its own header
/// and answers the colour type the format carries. Formats whose header says nothing (or whose
/// decoder is not compiled in) answer `None` — an honest nothing rather than a guess.
fn header_colour(bytes: &[u8]) -> Option<image::ColorType> {
	use image::ImageDecoder;
	use image::codecs::{gif::GifDecoder, jpeg::JpegDecoder, png::PngDecoder, webp::WebPDecoder};

	let format = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.format()?;
	let cursor = || std::io::Cursor::new(bytes);

	match format {
		image::ImageFormat::Png => Some(PngDecoder::new(cursor()).ok()?.color_type()),
		image::ImageFormat::Jpeg => Some(JpegDecoder::new(cursor()).ok()?.color_type()),
		image::ImageFormat::Gif => Some(GifDecoder::new(cursor()).ok()?.color_type()),
		image::ImageFormat::WebP => Some(WebPDecoder::new(cursor()).ok()?.color_type()),
		_ => None,
	}
}

/// The format the decoder recognises in the bytes themselves, under the name people use (`PNG`,
/// `JPG`): the header read, not a decode.
fn format_of(bytes: &[u8]) -> Option<String> {
	let format = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.format()?;

	Some(format.extensions_str().first().map_or_else(|| format!("{format:?}"), |ext| ext.to_ascii_uppercase()))
}

/// A colour type in words a person reads (P28②) — the file's own depth and channels, not the RGBA
/// every preview is converted to.
pub(crate) fn colour_words(colour: image::ColorType) -> String {
	use image::ColorType;

	match colour {
		ColorType::L8 => "8 位灰度".to_owned(),
		ColorType::La8 => "8 位灰度 + 透明度".to_owned(),
		ColorType::Rgb8 => "8 位 RGB".to_owned(),
		ColorType::Rgba8 => "8 位 RGBA".to_owned(),
		ColorType::L16 => "16 位灰度".to_owned(),
		ColorType::La16 => "16 位灰度 + 透明度".to_owned(),
		ColorType::Rgb16 => "16 位 RGB".to_owned(),
		ColorType::Rgba16 => "16 位 RGBA".to_owned(),
		ColorType::Rgb32F => "32 位浮点 RGB".to_owned(),
		ColorType::Rgba32F => "32 位浮点 RGBA".to_owned(),
		// the enum may grow: a type nobody named yet is still said, if not in words
		_ => format!("{colour:?}"),
	}
}

/// The pixels a picture is uploaded at (P32's 1:1 model): **the exact rectangle the column will
/// draw it in** — the width it was handed, which is the column's own width times the zoom, and the
/// file's own shape for the height (a target height of zero means "the shape decides"). A picture
/// that already fits inside that rectangle keeps its own pixels, so nothing is ever blown up here.
/// The sampler then draws the texture exactly as it is: one screen pixel, one texture pixel.
pub(crate) fn upload_size(source: [u32; 2], target: [u32; 2]) -> [u32; 2] {
	if source[0] == 0 || source[1] == 0 {
		return [ 1, 1 ];
	}

	let width = target[0].max(1) as f64;
	let height = if target[1] == 0 {
		(width * source[1] as f64 / source[0] as f64).round().max(1.0)
	} else {
		target[1].max(1) as f64
	};

	if source[0] as f64 <= width && source[1] as f64 <= height {
		return source;
	}

	[ width.round().max(1.0) as u32, height.round().max(1.0) as u32 ]
}

/// The number of pixels past which a reduction is made in two stages (P32): a photograph this
/// large is halved with the cheap filter first, so the fine one only ever meets a picture a
/// column can still look at. One Lanczos pass over forty megapixels would spend seconds for a
/// screen five hundred pixels wide.
const TWO_STAGE_PIXELS: u64 = 16 * 1024 * 1024;

/// The coarse first stage of such a reduction, when there is one to make.
fn coarse_stage(source: [u32; 2]) -> Option<[u32; 2]> {
	(source[0] as u64 * source[1] as u64 > TWO_STAGE_PIXELS)
		.then(|| [ (source[0] / 2).max(1), (source[1] / 2).max(1) ])
}

/// The window's own bucket of ready-to-upload pixels (P27⑥'s L0): `~/.cache/duckfm/preview-img`,
/// keyed by path, modification time and the size bucket — the same key the pane's textures use,
/// so the second visit shows the picture without decoding anything. Raw RGBA in a small header,
/// because a PNG round trip is the very cost this bucket exists to remove.
pub(crate) fn bucket_path(path: &str, mtime: Option<SystemTime>, target: [u32; 2]) -> Option<std::path::PathBuf> {
	let key = key(path, mtime, target);
	let dir = crate::config::Config::cache("preview-img")?;

	Some(dir.join(format!("{}.rgba", crate::preview::thumb::md5_hex(key.as_bytes()))))
}

/// Whether the bucket already holds this file at this size — a stat, not a read.
pub(crate) fn bucket_has(path: &str, mtime: Option<SystemTime>, target: [u32; 2]) -> bool {
	bucket_path(path, mtime, target).is_some_and(|at| at.is_file())
}

/// The pixels the bucket holds, if it holds them: the uploaded size, the file's own size and the
/// RGBA. `None` for an absent or malformed entry — a broken cache is a miss, never an error.
pub(crate) fn bucket_load(path: &str, mtime: Option<SystemTime>, target: [u32; 2]) -> Option<([u32; 2], [u32; 2], Vec<u8>)> {
	bucket_read(&bucket_path(path, mtime, target)?)
}

/// The bucket entry the path names: a small header, then the RGBA (P27⑥).
fn bucket_read(at: &std::path::Path) -> Option<([u32; 2], [u32; 2], Vec<u8>)> {
	let bytes = std::fs::read(at).ok()?;

	if bytes.len() < 24 || &bytes[..8] != BUCKET_MAGIC {
		return None;
	}

	let read = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
	let (size, source) = ([ read(8), read(12) ], [ read(16), read(20) ]);
	let rgba = &bytes[24..];
	if rgba.len() != size[0] as usize * size[1] as usize * 4 {
		return None;
	}

	Some((size, source, rgba.to_vec()))
}

/// A name no other writer is using at this instant: the same process writes buckets from a hover
/// and from a sweep at once, and two writers sharing one temp name would cut each other's file.
pub(super) fn unique_temp(at: &std::path::Path, dir: &std::path::Path) -> std::path::PathBuf {
	let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
	let stem = at.file_stem().map_or_else(|| "entry".into(), |stem| stem.to_string_lossy());

	dir.join(format!(".{stem}.tmp{}-{nanos}", std::process::id()))
}

/// The pixels' own file header: a magic the reader can trust and four sizes. The trailing version
/// number is the product's own (P28③): the bucket key carries no filter and no shown size, so the
/// version is what refuses the products of the old coarse bilinear reduction — a stale entry is
/// a miss and is decoded again, not shown as if it were clear.
const BUCKET_MAGIC: &[u8; 8] = b"DFPIX\x00\x00\x03";

/// Put the pixels where the next visit will find them, owner-only and written whole (P27⑥'s L0).
/// No modification time means nothing to judge the entry by, so nothing is written — the same rule
/// the shared thumbnail cache keeps.
pub(crate) fn bucket_store(path: &str, mtime: Option<SystemTime>, target: [u32; 2], size: [u32; 2], source: [u32; 2], rgba: &[u8]) {
	if mtime.is_none() {
		return;
	}
	let Some(at) = bucket_path(path, mtime, target) else { return };
	let Some(dir) = at.parent() else { return };
	if crate::config::Config::ensure(dir).is_err() {
		return;
	}

	bucket_write(&at, size, source, rgba);
}

fn bucket_write(at: &std::path::Path, size: [u32; 2], source: [u32; 2], rgba: &[u8]) {
	if size[0] == 0 || size[1] == 0 || rgba.len() != size[0] as usize * size[1] as usize * 4 {
		return;
	}

	let mut file = Vec::with_capacity(24 + rgba.len());
	file.extend_from_slice(BUCKET_MAGIC);
	for side in [ size[0], size[1], source[0], source[1] ] {
		file.extend_from_slice(&side.to_le_bytes());
	}
	file.extend_from_slice(rgba);

	// whole or not at all: a reader must never find half an entry
	let Some(dir) = at.parent() else { return };
	let tmp = unique_temp(at, dir);
	if std::fs::write(&tmp, &file).is_ok() {
		use std::os::unix::fs::PermissionsExt;
		let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
		let _ = std::fs::rename(&tmp, at);
	}
}

/// How much of the window's own bucket may stand before the oldest entries are taken out: the
/// sweep that fills it must not eat the disk quietly. 256MiB of pictures is a directory's worth
/// at preview sizes, and a hover's fresh entry is never the oldest.
const BUCKET_BUDGET: u64 = 256 * 1024 * 1024;

/// Take the oldest entries out of the bucket until it is under its budget (P27⑥). Best effort:
/// a directory that cannot be read is a cache that is not pruned, not an error.
pub(crate) fn bucket_prune() {
	let Some(dir) = crate::config::Config::cache("preview-img") else { return };
	let Ok(entries) = std::fs::read_dir(&dir) else { return };

	let mut files: Vec<(std::time::SystemTime, u64, std::path::PathBuf)> = entries
		.flatten()
		.filter_map(|entry| {
			let meta = entry.metadata().ok()?;
			meta.is_file().then(|| (meta.modified().unwrap_or(std::time::UNIX_EPOCH), meta.len(), entry.path()))
		})
		.collect();

	let mut total: u64 = files.iter().map(|(_, len, _)| len).sum();
	if total <= BUCKET_BUDGET {
		return;
	}

	files.sort_by_key(|(at, _, _)| *at);
	for (_, len, path) in files {
		if total <= BUCKET_BUDGET {
			break;
		}
		if std::fs::remove_file(&path).is_ok() {
			total = total.saturating_sub(len);
		}
	}
}

/// How much of a PDF render the bucket keeps: a page beyond this is shown once and not kept —
/// the cache is for the pages people come back to, not for a poster nobody re-reads.
const PDF_BUCKET_PIXELS: u64 = 4 * 1024 * 1024;

/// Where a PDF page's render is kept: the same bucket directory, under a key that carries the
/// page and the size bucket (P27⑥: the first page's render survives the session).
pub(crate) fn pdf_bucket_path(path: &str, mtime: Option<SystemTime>, target: [u32; 2], page: usize) -> Option<std::path::PathBuf> {
	let key = format!("{}#p{}", key(path, mtime, target), page);
	let dir = crate::config::Config::cache("preview-img")?;

	Some(dir.join(format!("{}.pdf.rgba", crate::preview::thumb::md5_hex(key.as_bytes()))))
}

/// A cached page render: its own size, the page count it was rendered beside, and the pixels.
pub(crate) fn pdf_bucket_load(
	path: &str,
	mtime: Option<SystemTime>,
	target: [u32; 2],
	page: usize,
) -> Option<([u32; 2], usize, Vec<u8>)> {
	let at = pdf_bucket_path(path, mtime, target, page)?;
	let bytes = std::fs::read(at).ok()?;

	const MAGIC: &[u8; 8] = b"DFPDF\x00\x00\x01";
	if bytes.len() < 24 || &bytes[..8] != MAGIC {
		return None;
	}

	let read = |at: usize| u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
	let size = [ read(8), read(12) ];
	let total = read(16) as usize;
	let rgba = &bytes[24..];
	if rgba.len() != size[0] as usize * size[1] as usize * 4 {
		return None;
	}

	Some((size, total, rgba.to_vec()))
}

/// Keep a page's render for the next visit. Only renders whose pixels stay inside the budget are
/// kept, and only when there is a modification time to judge the entry by.
pub(crate) fn pdf_bucket_store(
	path: &str,
	mtime: Option<SystemTime>,
	target: [u32; 2],
	page: usize,
	render: ([u32; 2], usize, &[u8]),
) {
	let (size, total, rgba) = render;
	if mtime.is_none() || size[0] as u64 * size[1] as u64 > PDF_BUCKET_PIXELS {
		return;
	}
	let Some(at) = pdf_bucket_path(path, mtime, target, page) else { return };
	let Some(dir) = at.parent() else { return };
	if crate::config::Config::ensure(dir).is_err() {
		return;
	}

	let mut file = Vec::with_capacity(24 + rgba.len());
	file.extend_from_slice(b"DFPDF\x00\x00\x01");
	for side in [ size[0], size[1], total as u32, page as u32 ] {
		file.extend_from_slice(&side.to_le_bytes());
	}
	file.extend_from_slice(rgba);

	let tmp = unique_temp(&at, dir);
	if std::fs::write(&tmp, &file).is_ok() {
		use std::os::unix::fs::PermissionsExt;
		let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
		let _ = std::fs::rename(&tmp, &at);
	}
}

/// Where a picture's answer came from: the window's own bucket, the shared thumbnail cache, or
/// the file's own decode — the acceptance counts what the sweeps filled (P27⑥).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Road {
	Bucket,
	Thumbnail,
	Decode,
}

/// The picture of a file, in the order it should reach the screen (P27⑥): the window's own
/// bucket first — pixels that only need uploading — then the shared thumbnail cache, and only
/// then the file's own decode. `ship` is called with the answer as it exists, and with the road
/// it took.
pub(crate) fn decode_into(
	path: &str,
	mtime: Option<SystemTime>,
	size: u64,
	bytes: &[u8],
	target: [u32; 2],
	mut ship: impl FnMut(Road, [u32; 2], [u32; 2], Vec<u8>),
) -> Outcome {
	// the window's own bucket: the pixels are the ones this column wants, so they go up as they
	// are — the fastest answer there is, and the one the sweep fills
	if let Some((size, source, rgba)) = bucket_load(path, mtime, target) {
		ship(Road::Bucket, size, source, rgba);
		return Outcome::Shipped;
	}

	let thumb = Thumb::of(path, mtime, size);

	if let Some(png) = thumb.load()
		&& let Ok(image) = image::load_from_memory(&png)
	{
		// a cached thumbnail is small and already shaped for a column: it goes up as it is — a first
		// quick look, *not* the answer — and the *file's* own dimensions are what the badge measures
		// against, the header answering that without decoding a pixel
		let size = [ image.width(), image.height() ];
		let source = dimensions(bytes).map_or(size, |(w, h)| [ w, h ]);

		// a cache entry whose shape is not the file's is a photograph of another day (a stale
		// hash, a crop): it is dropped rather than stretched to the file's own proportions (P27's
		// first item — a picture is never squashed)
		let same_shape = |a: [u32; 2], b: [u32; 2]| {
			let (a, b) = (a[0] as f64 * b[1] as f64, b[0] as f64 * a[1] as f64);
			(a - b).abs() <= (a.max(b) * 0.02).max(2.0)
		};
		if same_shape(size, source) {
			// the proxy goes up first (P32's own correction): a thumbnail is a quick look, and the
			// decode below is what makes the picture *sharp* — shipping it and stopping here left
			// every file whose cache had a thumbnail of it soft forever
			ship(Road::Thumbnail, size, source, image.to_rgba8().into_raw());
		}
	}

	// the header is asked before the decoder is: a picture too large to be a preview is named
	// rather than decoded
	if let Some((width, height)) = dimensions(bytes)
		&& too_big(width, height)
	{
		return Outcome::TooBig;
	}

	let Some(image) = image::load_from_memory(bytes).ok() else {
		thumb.failed();
		return Outcome::Unreadable;
	};
	let image = image.to_rgba8();

	let (size, rgba) = pixels_of(&image, target);
	if size != [ image.width(), image.height() ] {
		upload_note(path, [ image.width(), image.height() ], size, target);
	}

	// the products, written after the answer so nothing waits on the disk: the bucket first —
	// it is what the next hover reads — and the shared thumbnail for the user's other windows
	ship(Road::Decode, size, [ image.width(), image.height() ], rgba.clone());

	bucket_store(path, mtime, target, size, [ image.width(), image.height() ], &rgba);
	thumb.store([ image.width(), image.height() ], image.as_raw());
	Outcome::Shipped
}

/// The pixels a still picture travels as: the file's own RGBA, reduced to the upload size when it
/// outgrows twice what the column shows — the one resample the preview path still does, and it is
/// the fine filter's now (P28③): a coarse one-step bilinear is what "some pictures are not clear"
/// looked like.
fn pixels_of(image: &image::RgbaImage, target: [u32; 2]) -> ([u32; 2], Vec<u8>) {
	let source = [ image.width(), image.height() ];
	let size = upload_size(source, target);

	if size == source {
		return (size, image.as_raw().clone());
	}

	// past the ceiling the reduction is made in two stages (P32): the cheap filter halves the
	// photograph first, and the fine one meets what is left
	let (bytes, from) = match coarse_stage(source) {
		Some(half) => match resample_fast(image.as_raw(), source, half) {
			Some(rgba) => (rgba, half),
			None => (image.as_raw().clone(), source),
		},
		None => (image.as_raw().clone(), source),
	};

	match resample(&bytes, from, size) {
		Some(rgba) => (size, rgba),
		// a reduction that could not be made is no reason to lose the picture
		None => (source, image.as_raw().clone()),
	}
}

/// A picture that was reduced before upload says so — the numbers the acceptance reads VRAM by
/// (P28③). The name only, to keep the line short.
fn upload_note(path: &str, from: [u32; 2], to: [u32; 2], target: [u32; 2]) {
	let name = std::path::Path::new(path).file_name().map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned());

	crate::metrics::upload(&name, from, to, target);
}

/// A coarse resample to an exact size — the GIF frames' own filter, and only theirs (P28③: a
/// still picture is reduced with the fine one). A frame is resampled once and played back from
/// the texture, so the cheap filter is the right one for an animation that must not stutter.
pub(crate) fn resample_fast(rgba: &[u8], from: [u32; 2], to: [u32; 2]) -> Option<Vec<u8>> {
	use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer, images::Image};

	if from[0] == 0 || from[1] == 0 || to[0] == 0 || to[1] == 0 {
		return None;
	}
	if from == to {
		return Some(rgba.to_vec());
	}

	let src = Image::from_vec_u8(from[0], from[1], rgba.to_vec(), PixelType::U8x4).ok()?;
	let mut dst = Image::new(to[0], to[1], PixelType::U8x4);

	Resizer::new()
		.resize(&src, &mut dst, &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Bilinear)))
		.ok()?;

	Some(dst.into_vec())
}

/// Decode a file's bytes into the cache products the sweep keeps — the preview-size RGBA and the
/// shared thumbnail — and nothing else. A hover has not asked for this file yet, so nothing is
/// ever sent to the window: a prefetch is a warm cache, not a picture on screen (P27⑥'s L2/L3).
pub(crate) fn warm(path: &str, mtime: Option<SystemTime>, size: u64, bytes: &[u8], target: [u32; 2]) -> bool {
	// already in the window's own bucket: the whole job
	if bucket_has(path, mtime, target) {
		return true;
	}

	let Some(image) = image::load_from_memory(bytes).ok() else {
		Thumb::of(path, mtime, size).failed();
		return false;
	};
	let image = image.to_rgba8();
	let source = [ image.width(), image.height() ];

	let (upload, rgba) = pixels_of(&image, target);
	if upload != source {
		upload_note(path, source, upload, target);
	}
	bucket_store(path, mtime, target, upload, source, &rgba);
	Thumb::of(path, mtime, size).store(source, image.as_raw());
	true
}

/// Resample an RGBA picture to an exact size, whatever the aspect: the PDF page's own scale maths
/// decides the shape, and this only carries it out without distorting a pixel twice.
pub(crate) fn resample(rgba: &[u8], from: [u32; 2], to: [u32; 2]) -> Option<Vec<u8>> {
	use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer, images::Image};

	if from[0] == 0 || from[1] == 0 || to[0] == 0 || to[1] == 0 {
		return None;
	}
	if from == to {
		return Some(rgba.to_vec());
	}

	let src = Image::from_vec_u8(from[0], from[1], rgba.to_vec(), PixelType::U8x4).ok()?;
	let mut dst = Image::new(to[0], to[1], PixelType::U8x4);

	Resizer::new()
		.resize(&src, &mut dst, &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3)))
		.ok()?;

	Some(dst.into_vec())
}

/// Least-recently-used accounting for the uploaded textures: they live on the GPU, and their
/// source size is what the cap counts. Moved here from the legacy window's GUI.
pub(crate) struct Lru {
	cap:   usize,
	used:  usize,
	order: Vec<String>,
	sizes: std::collections::HashMap<String, usize>,
}

impl Lru {
	pub(crate) fn new(cap: usize) -> Self {
		Self { cap, used: 0, order: Vec::new(), sizes: std::collections::HashMap::new() }
	}

	/// Record a use of `key`, dropping the least recently used keys that no longer fit. Returns the
	/// keys the caller should forget — and the caller releases those texture handles.
	pub(crate) fn touch(&mut self, key: &str, bytes: usize) -> Vec<String> {
		if let Some(old) = self.sizes.remove(key) {
			self.used -= old;
			self.order.retain(|known| known != key);
		}

		self.sizes.insert(key.to_owned(), bytes);
		self.used += bytes;
		self.order.push(key.to_owned());

		let mut evicted = Vec::new();
		while self.used > self.cap {
			let Some(oldest) = self.order.first().cloned() else { break };
			self.order.remove(0);
			if let Some(size) = self.sizes.remove(&oldest) {
				self.used -= size;
				evicted.push(oldest);
			}
		}

		evicted
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// An animation decodes through the image crate's own frame reader (P26's first item): every
	/// frame of a hand-made GIF comes back resampled to the column, in order, with its delay.
	#[test]
	fn a_gif_comes_back_as_its_frames_with_their_delays() {
		use image::{Frame, Rgba, RgbaImage};

		let colours = [ Rgba([ 255, 0, 0, 255 ]), Rgba([ 0, 255, 0, 255 ]), Rgba([ 0, 0, 255, 255 ]) ];
		let mut bytes: Vec<u8> = Vec::new();
		{
			let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
			for colour in colours {
				let mut picture = RgbaImage::new(40, 20);
				for pixel in picture.pixels_mut() {
					*pixel = colour;
				}
				encoder
					.encode_frame(Frame::from_parts(picture, 0, 0, image::Delay::from_numer_denom_ms(120, 1)))
					.expect("a frame");
			}
		}
		let bytes = bytes;

		let (drawn, source, frames) = decode_frames([ 800, 600 ], &bytes).expect("three frames");
		assert_eq!(frames.len(), 3, "every frame of the file");
		assert_eq!(drawn, [ 40, 20 ], "a small picture is not blown up by `fit`");
		assert_eq!(source, [ 40, 20 ], "and the file's own header says the same");
		assert!(frames.iter().all(|(_, delay)| *delay == 120), "the delays come through: {frames:?}");

		// a width-only target — what the pane hands over (P32): every frame is reduced to the
		// rectangle it will be drawn in, its own shape deciding the height
		let (drawn, _, frames) = decode_frames([ 20, 0 ], &bytes).expect("frames with a width-only target");
		assert_eq!(drawn, [ 20, 10 ], "the display rectangle: the width given, the shape in charge of the height");
		assert!(frames.iter().all(|(rgba, _)| rgba.len() == 20 * 10 * 4), "and the pixels agree");

		// a frame smaller than the rectangle keeps its own pixels, like a still one does
		let (drawn, _, _) = decode_frames([ 800, 0 ], &bytes).expect("a big width");
		assert_eq!(drawn, [ 40, 20 ], "never blown up here — the sampler scales a small one");

		// the resample is the column's: ask for a smaller one and every frame follows
		let (drawn, _, frames) = decode_frames([ 20, 10 ], &bytes).expect("frames again");
		assert_eq!(drawn, [ 20, 10 ], "the frames are the column's size");
		assert!(frames.iter().all(|(rgba, _)| rgba.len() == 20 * 10 * 4), "and so is every buffer");

		// bytes that are not an animation at all
		assert!(decode_frames([ 800, 600 ], b"not a gif at all").is_none());
	}

	/// A zero-delay frame is floored (P26): a file whose delays are nothing would ask the window to
	/// repaint as fast as it can, and a frame nobody can see is not a frame.
	#[test]
	fn a_frames_delay_has_a_floor() {
		assert_eq!(frame_ms(image::Delay::from_numer_denom_ms(0, 1)), 20);
		assert_eq!(frame_ms(image::Delay::from_numer_denom_ms(120, 1)), 120);
		assert_eq!(frame_ms(image::Delay::from_numer_denom_ms(1, 1)), 20, "one millisecond is past believing");
	}

	/// A zoom scales the render target — that is the whole point of re-rendering a PDF — and the
	/// bound keeps a deep zoom from asking for a poster.
	#[test]
	fn a_zoom_scales_the_render_target() {
		assert_eq!(request_target([ 800, 600 ], 1.0), [ 800, 600 ], "fit asks for the column");
		assert_eq!(request_target([ 800, 600 ], 2.0), [ 1600, 1200 ]);
		assert_eq!(request_target([ 800, 600 ], 0.5), [ 400, 300 ], "and a smaller zoom asks for less");
		assert_eq!(request_target([ 5000, 5000 ], 8.0), [ 6000, 6000 ], "bounded");
	}

	/// The cache key is the task's own rule: the same file at the same shape is the same key, a
	/// newer file or another shape is not, and a jitter of a resize stays inside one bucket.
	#[test]
	fn the_key_is_path_mtime_and_the_width_bucket() {
		let at = |secs| UNIX_EPOCH + std::time::Duration::from_secs(secs);
		let a = key("/x.png", Some(at(100)), [ 300, 0 ]);
		let b = key("/x.png", Some(at(100)), [ 310, 0 ]);
		assert_eq!(a, b, "within one bucket is one texture");
		assert_eq!(a, key("/x.png", Some(at(100)), [ 300, 9000 ]), "the height is nobody's business: the shape decides it");
		assert_ne!(a, key("/x.png", Some(at(101)), [ 300, 0 ]), "a newer file is another key");
		assert_ne!(a, key("/y.png", Some(at(100)), [ 300, 0 ]), "and so is another path");
		assert_ne!(a, key("/x.png", Some(at(100)), [ 640, 0 ]), "and another width: what the zoom moves");
	}

	/// The pixels a picture travels as (P32's 1:1 model): **the rectangle it will be drawn in** —
	/// the width the pane hands over (the column's own width times the zoom), the file's own shape
	/// for the height — so the texture and the screen rectangle are one and the same. A picture
	/// that already fits inside that rectangle keeps its own pixels, and the two-stage guard halves
	/// a photograph before the fine filter meets it.
	#[test]
	fn a_picture_is_uploaded_at_the_size_it_is_shown() {
		// the width says where the picture stands; a zero height asks the shape to say the rest
		assert_eq!(upload_size([ 2000, 6000 ], [ 454, 0 ]), [ 454, 1362 ], "the column's width, the file's shape");
		assert_eq!(upload_size([ 2000, 6000 ], [ 568, 0 ]), [ 568, 1704 ], "zoom widens the rectangle, the shape keeps the ratio");
		assert_eq!(upload_size([ 3840, 2160 ], [ 454, 0 ]), [ 454, 255 ]);

		// a picture that fits inside its rectangle travels as it is (never blown up here)
		assert_eq!(upload_size([ 100, 50 ], [ 400, 0 ]), [ 100, 50 ], "a small one keeps its own pixels");
		assert_eq!(upload_size([ 600, 400 ], [ 800, 0 ]), [ 600, 400 ]);
		assert_eq!(upload_size([ 10, 10 ], [ 300, 200 ]), [ 10, 10 ], "an explicit height changes nothing for a small one");

		// and nothing at all is nothing
		assert_eq!(upload_size([ 0, 10 ], [ 300, 0 ]), [ 1, 1 ]);

		// the two-stage guard's own trigger: past the ceiling, the coarse filter has a job
		assert!(coarse_stage([ 4000, 3000 ]).is_none(), "twelve megapixels is one stage");
		assert_eq!(coarse_stage([ 8000, 8000 ]), Some([ 4000, 4000 ]), "sixty-four megapixels is two");
		assert_eq!(coarse_stage([ 20000, 20000 ]), Some([ 10000, 10000 ]));
	}

	/// The reduction is the fine filter's (P28③): on a fine checkerboard the Lanczos reduction
	/// keeps far more contrast than the coarse bilinear one — exactly the blur the sharpness fix
	/// was for.
	#[test]
	fn the_reduction_keeps_more_contrast_than_the_coarse_filter() {
		let (w, h) = (128u32, 128u32);
		let mut rgba = Vec::with_capacity((w * h * 4) as usize);
		for y in 0..h {
			for x in 0..w {
				let value = if (x / 4 + y / 4) % 2 == 0 { 20u8 } else { 235u8 };
				rgba.extend_from_slice(&[ value, value, value, 255 ]);
			}
		}

		let to = [ 64u32, 64u32 ];
		let fine = resample(&rgba, [ w, h ], to).expect("the fine filter");
		let coarse = resample_fast(&rgba, [ w, h ], to).expect("the coarse one");

		let spread = |rgba: &[u8]| {
			let samples: Vec<u8> = rgba.chunks(4).map(|pixel| pixel[0]).collect();
			samples.iter().max().unwrap() - samples.iter().min().unwrap()
		};

		assert!(
			spread(&fine) > spread(&coarse),
			"lanczos keeps more contrast than bilinear: {} vs {}",
			spread(&fine),
			spread(&coarse)
		);
	}

	/// A picture reaches the column in one answer, at the upload size, with the road it came by:
	/// a fresh decode says so, the source's own dimensions travel with it for the badge, and the
	/// decoder's report travels with it for the info block (P28②). The pixels are the file's own
	/// whenever they fit twice what the column shows, and a Lanczos reduction to that when they
	/// do not (P28③ — the fast/fine pair is gone; the sampler scales what lands).
	#[test]
	fn a_picture_arrives_whole_and_names_the_road_it_took() {
		let (w, h) = (600u32, 400u32);
		let mut bytes = Vec::new();
		image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([ (x % 256) as u8, (y % 256) as u8, 90, 255 ]))
			.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
			.expect("the fixture encodes");

		let mut answers = Vec::new();
		// no mtime, so no cache is read or written: the test stays in memory
		let shipped = decode_into(
			"/tmp/duckfm-native-n2-never/never.png",
			None,
			bytes.len() as u64,
			&bytes,
			[ 300, 200 ],
			|road, size, source, rgba| {
				assert_eq!(road, Road::Decode, "nothing was cached, so the file itself was read");
				assert_eq!(size, [ 300, 200 ], "the rectangle it will be drawn in: the column's width, the file's shape");
				assert_eq!(rgba.len(), size[0] as usize * size[1] as usize * 4, "four bytes a pixel");
				assert_eq!(source, [ w, h ]);
				answers.push(road);
			},
		);

		assert_eq!(shipped, Outcome::Shipped, "the fixture is a picture");
		assert_eq!(answers, vec![ Road::Decode ], "one answer, and its road");
	}

	/// The window's own bucket (P27⑥'s L0): a written entry comes back whole at the same header, a
/// malformed or absent one is a miss, and the size the writer told it is the size the reader
/// trusts — the test's own file under /tmp, so the real cache is never touched.
	#[test]
	fn a_bucket_entry_survives_the_round_trip() {
		let at = std::env::temp_dir().join(format!("duckfm-native-bucket-{}.rgba", std::process::id()));
		let _ = std::fs::remove_file(&at);

		assert!(bucket_read(&at).is_none(), "nothing stands there yet");

		let rgba = vec![ 7u8; 2 * 2 * 4 ];
		bucket_write(&at, [ 2, 2 ], [ 4, 4 ], &rgba);
		let (size, source, back) = bucket_read(&at).expect("the entry is there");
		assert_eq!((size, source), ([ 2, 2 ], [ 4, 4 ]), "both sizes survive");
		assert_eq!(back, rgba, "and so do the pixels");

		// a file that is not a bucket, and a bucket whose pixels do not match its header
		std::fs::write(&at, b"not a bucket at all").expect("a file");
		assert!(bucket_read(&at).is_none(), "junk is a miss");

		let mut file = Vec::new();
		file.extend_from_slice(BUCKET_MAGIC);
		for side in [ 9u32, 9, 9, 9 ] {
			file.extend_from_slice(&side.to_le_bytes());
		}
		file.extend_from_slice(&[ 0u8; 16 ]);
		std::fs::write(&at, &file).expect("a file");
		assert!(bucket_read(&at).is_none(), "a header the pixels do not fill is a miss");

		// an entry of the old product version is a miss (P28③): the bucket key carries no filter,
		// so the version is what refuses the products of the coarse reduction
		let mut old = Vec::new();
		old.extend_from_slice(b"DFPIX\x00\x00\x01");
		for side in [ 2u32, 2, 4, 4 ] {
			old.extend_from_slice(&side.to_le_bytes());
		}
		old.extend_from_slice(&[ 7u8; 16 ]);
		std::fs::write(&at, &old).expect("a file");
		assert!(bucket_read(&at).is_none(), "an entry from before the fine filter is not trusted");

		let _ = std::fs::remove_file(&at);

		// and a lookup with no mtime never writes: the key it would form names no file
		assert!(!bucket_has("/tmp/duckfm-native-never/none.png", None, [ 300, 200 ]), "no mtime, no entry");
	}

	/// A prefetch writes a cache entry and nothing else: with a modification time to judge it by, a
	/// picture is warmed; without one there is nothing a thumbnail could be judged against, and the
	/// cache is left alone. The path is a made-up one that no cache can hold, so nothing real is
	/// written by this test.
	#[test]
	fn a_prefetch_is_a_cache_write_and_no_picture() {
		let (w, h) = (256u32, 192u32);
		let mut bytes = Vec::new();
		image::RgbaImage::from_pixel(w, h, image::Rgba([ 1, 2, 3, 255 ]))
			.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
			.expect("the fixture encodes");

		assert!(warm("/tmp/duckfm-native-n2-never/warm.png", None, bytes.len() as u64, &bytes, [ 256, 256 ]));
		assert!(!warm("/tmp/duckfm-native-n2-never/warm.png", None, 4, b"nope", [ 256, 256 ]), "junk is not warmed");
	}

	/// The fit-width rule (P27's first item — the final ruling): the picture's display size is the
	/// column's own width and the file's own shape — a tall one is taller than the column and walks
	/// through it, never squashed; a zero-sized file is not a division by zero.
	#[test]
	fn a_picture_is_as_wide_as_the_column_and_as_tall_as_it_is() {
		// a tall picture: the column's width, its own height following the shape
		assert_eq!(fit_width((200, 800), 400.0), [ 400.0, 1600.0 ]);
		// a wide one: the same width, and no vertical walk at all
		assert_eq!(fit_width((800, 200), 400.0), [ 400.0, 100.0 ]);
		// a small one is blown up to the column's width all the same — the fit-width size is the
		// size a person sees (and the upload cap is twice it, so the texture keeps up)
		assert_eq!(fit_width((100, 50), 400.0), [ 400.0, 200.0 ]);
		// nothing is nobody's shape: the guard keeps the division honest (a zero side counts as 1)
		assert_eq!(fit_width((0, 10), 400.0), [ 400.0, 4000.0 ]);
		assert_eq!(fit_width((10, 0), 400.0), [ 400.0, 40.0 ]);
	}

	/// The header's own report (P28② revised): the format and the colour type both come from the
	/// head bytes — no decode — in words a person reads rather than an enum's name, and junk names
	/// nothing at all.
	#[test]
	fn a_header_report_names_the_format_and_the_colour_type() {
		let (w, h) = (8u32, 6u32);
		let mut png = Vec::new();
		image::RgbaImage::from_pixel(w, h, image::Rgba([ 1, 2, 3, 255 ]))
			.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
			.expect("the fixture encodes");
		let mut jpg = Vec::new();
		image::RgbImage::from_pixel(w, h, image::Rgb([ 1, 2, 3 ]))
			.write_to(&mut std::io::Cursor::new(&mut jpg), image::ImageFormat::Jpeg)
			.expect("the fixture encodes");

		let header = header_report(&png);
		assert_eq!(header.format.as_deref(), Some("PNG"), "the header alone names the format");
		assert_eq!(header.colour.as_deref(), Some("8 位 RGBA"), "and the PNG header states its colour type");

		let jpeg = header_report(&jpg);
		assert_eq!(jpeg.format.as_deref(), Some("JPG"));
		assert!(jpeg.colour.is_some(), "a JPEG header states its own components too");
		assert!(header_report(b"not a picture at all").format.is_none(), "junk names nothing");

		// every colour type reads in words, and none of them is a bare enum name
		for colour in [
			image::ColorType::L8,
			image::ColorType::La8,
			image::ColorType::Rgb8,
			image::ColorType::Rgba8,
			image::ColorType::L16,
			image::ColorType::La16,
			image::ColorType::Rgb16,
			image::ColorType::Rgba16,
			image::ColorType::Rgb32F,
			image::ColorType::Rgba32F,
		] {
			let words = colour_words(colour);
			assert!(words.contains('位'), "{words} says no depth");
			assert!(words.contains("灰度") || words.contains("RGB"), "{words} names no channels");
		}
	}

	/// The sizes are the spec's own: the memory is what is refused, not the size; a decode is never
	/// blown up past the file's pixels, while the *display* contain fills the column either way.
	#[test]
	fn only_a_decode_that_would_not_fit_is_refused_and_fitting_keeps_the_ratio() {
		assert!(!too_big(4000, 3000), "a twelve-megapixel photograph is fine");
		assert!(!too_big(8000, 8000), "and so is sixty-four megapixels");
		assert!(too_big(20_000, 20_000), "but a gigabyte and a half is not a preview");

		assert_eq!(fit((200, 100), [ 100, 100 ]), (100, 50));
		assert_eq!(fit((100, 200), [ 100, 100 ]), (50, 100));
		assert_eq!(fit((10, 10), [ 100, 100 ]), (10, 10), "a small image is never scaled up");
		assert_eq!(fit((0, 10), [ 100, 100 ]), (0, 0));

		assert_eq!(contain((200, 100), [ 100, 100 ]), (100, 50), "down to the column");
		assert_eq!(contain((128, 128), [ 640, 480 ]), (480, 480), "and up to it, touching an edge");
		assert_eq!(contain((100, 400), [ 320, 800 ]), (200, 800), "the shorter side sets the scale");
		assert_eq!(contain((10, 10), [ 100, 50 ]), (50, 50), "a small picture fills the column too");
		assert_eq!(contain((0, 10), [ 100, 100 ]), (0, 0));
	}

	#[test]
	fn junk_is_not_a_picture() {
		let mut shipped = false;
		let outcome = decode_into("/tmp/duckfm-native-n2-never/junk.png", None, 4, b"nope", [ 100, 100 ], | _, _, _, _| {
			shipped = true
		});

		assert_eq!(outcome, Outcome::Unreadable, "junk is not a picture");
		assert!(!shipped, "and nothing is shipped for it");
	}

	/// The cache drops the least recently used image, and re-touching a key replaces its size
	/// instead of counting it twice.
	#[test]
	fn the_cache_drops_the_least_recently_used_image() {
		let mut lru = Lru::new(100);

		assert!(lru.touch("a", 60).is_empty());
		assert_eq!(lru.touch("b", 60), vec![ "a".to_owned() ], "60 + 60 no longer fits into 100");
		assert_eq!(lru.touch("a", 60), vec![ "b".to_owned() ], "using a again makes b the old one");
		assert!(lru.touch("c", 10).is_empty(), "the eviction already happened above");
	}
}
