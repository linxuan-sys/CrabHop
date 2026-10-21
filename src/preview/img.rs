
use std::time::{SystemTime, UNIX_EPOCH};

use crate::preview::thumb::Thumb;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Outcome {
	Shipped,
	TooBig,
	Unreadable,
}

const BUCKET: u32 = 64;

pub(crate) fn slots() -> usize {
	std::thread::available_parallelism().map_or(2, |n| n.get()).clamp(2, 4)
}

pub(crate) fn bucket(target: [u32; 2]) -> (u32, u32) {
	(target[0].div_ceil(BUCKET) * BUCKET, target[1].div_ceil(BUCKET) * BUCKET)
}

pub(crate) fn request_target(target: [u32; 2], zoom: f32) -> [u32; 2] {
	const MAX: u32 = 6000;

	let scaled = |side: u32| ((side as f32 * zoom).round() as u32).clamp(1, MAX);

	[ scaled(target[0]), scaled(target[1]) ]
}

pub(crate) fn key(path: &str, mtime: Option<SystemTime>, target: [u32; 2]) -> String {
	let mtime = mtime
		.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
		.map_or(0, |d| d.as_millis());
	let (w, _) = bucket(target);

	format!("{path}|{mtime}|{w}")
}

pub(crate) const TEXTURE_CAP: usize = 512 * 1024 * 1024;

pub(crate) const FORMATS: &[&str] = &[
	"png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tif", "tiff", "qoi", "tga", "dds", "hdr", "exr", "ff", "pnm",
	"pbm", "pgm", "ppm",
];

pub(crate) fn recognises(ext: &str) -> bool {
	FORMATS.iter().any(|known| known.eq_ignore_ascii_case(ext))
}

const MAX_DECODE_BYTES: u64 = 512 * 1024 * 1024;

pub(crate) const NOTE_TOO_BIG: &str = "图片过大，已跳过预览";

pub(crate) fn too_big(width: u32, height: u32) -> bool {
	width as u64 * height as u64 * 4 > MAX_DECODE_BYTES
}

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

pub(crate) fn fit_width(size: (u32, u32), width: f32) -> [f32; 2] {
	let (w, h) = (size.0.max(1) as f32, size.1.max(1) as f32);

	[ width, width * h / w ]
}

pub(crate) fn contain(size: (u32, u32), target: [u32; 2]) -> (u32, u32) {
	let (w, h) = size;
	let (tw, th) = (target[0].max(1), target[1].max(1));
	if w == 0 || h == 0 {
		return (0, 0);
	}

	let scale = f64::min(tw as f64 / w as f64, th as f64 / h as f64);

	((w as f64 * scale).round().max(1.0) as u32, (h as f64 * scale).round().max(1.0) as u32)
}

pub(crate) const FRAME_CAP: usize = 240;

const FRAME_FLOOR_MS: u32 = 20;

pub(crate) type Animation = ([u32; 2], [u32; 2], Vec<(Vec<u8>, u32)>);

pub(crate) fn decode_frames(target: [u32; 2], bytes: &[u8]) -> Option<Animation> {
	use image::AnimationDecoder;

	let gif = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes)).ok()?;
	let mut frames = Vec::new();
	let mut drawn = [ 0u32, 0u32 ];

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

	let source = dimensions(bytes).map_or(drawn, |(w, h)| [ w, h ]);

	Some((drawn, source, frames))
}

fn frame_ms(delay: image::Delay) -> u32 {
	let (numerator, denominator) = delay.numer_denom_ms();
	if denominator == 0 {
		return FRAME_FLOOR_MS;
	}

	(numerator / denominator).max(FRAME_FLOOR_MS)
}

pub(crate) fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
	image::ImageReader::new(std::io::Cursor::new(bytes))
		.with_guessed_format()
		.ok()?
		.into_dimensions()
		.ok()
}

#[derive(Clone)]
pub(crate) struct Report {
	pub format: Option<String>,
	pub colour: Option<String>,
}

pub(crate) fn header_report(bytes: &[u8]) -> Report {
	Report { format: format_of(bytes), colour: header_colour(bytes).map(colour_words) }
}

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

fn format_of(bytes: &[u8]) -> Option<String> {
	let format = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.format()?;

	Some(format.extensions_str().first().map_or_else(|| format!("{format:?}"), |ext| ext.to_ascii_uppercase()))
}

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
		_ => format!("{colour:?}"),
	}
}

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

const TWO_STAGE_PIXELS: u64 = 16 * 1024 * 1024;

fn coarse_stage(source: [u32; 2]) -> Option<[u32; 2]> {
	(source[0] as u64 * source[1] as u64 > TWO_STAGE_PIXELS)
		.then(|| [ (source[0] / 2).max(1), (source[1] / 2).max(1) ])
}

pub(crate) fn bucket_path(path: &str, mtime: Option<SystemTime>, target: [u32; 2]) -> Option<std::path::PathBuf> {
	let key = key(path, mtime, target);
	let dir = crate::config::Config::cache("preview-img")?;

	Some(dir.join(format!("{}.rgba", crate::preview::thumb::md5_hex(key.as_bytes()))))
}

pub(crate) fn bucket_has(path: &str, mtime: Option<SystemTime>, target: [u32; 2]) -> bool {
	bucket_path(path, mtime, target).is_some_and(|at| at.is_file())
}

pub(crate) fn bucket_load(path: &str, mtime: Option<SystemTime>, target: [u32; 2]) -> Option<([u32; 2], [u32; 2], Vec<u8>)> {
	bucket_read(&bucket_path(path, mtime, target)?)
}

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

pub(super) fn unique_temp(at: &std::path::Path, dir: &std::path::Path) -> std::path::PathBuf {
	let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
	let stem = at.file_stem().map_or_else(|| "entry".into(), |stem| stem.to_string_lossy());

	dir.join(format!(".{stem}.tmp{}-{nanos}", std::process::id()))
}

const BUCKET_MAGIC: &[u8; 8] = b"DFPIX\x00\x00\x03";

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

	let Some(dir) = at.parent() else { return };
	let tmp = unique_temp(at, dir);
	if std::fs::write(&tmp, &file).is_ok() {
		use std::os::unix::fs::PermissionsExt;
		let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
		let _ = std::fs::rename(&tmp, at);
	}
}

const BUCKET_BUDGET: u64 = 256 * 1024 * 1024;

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

const PDF_BUCKET_PIXELS: u64 = 4 * 1024 * 1024;

pub(crate) fn pdf_bucket_path(path: &str, mtime: Option<SystemTime>, target: [u32; 2], page: usize) -> Option<std::path::PathBuf> {
	let key = format!("{}#p{}", key(path, mtime, target), page);
	let dir = crate::config::Config::cache("preview-img")?;

	Some(dir.join(format!("{}.pdf.rgba", crate::preview::thumb::md5_hex(key.as_bytes()))))
}

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

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Road {
	Bucket,
	Thumbnail,
	Decode,
}

pub(crate) fn decode_into(
	path: &str,
	mtime: Option<SystemTime>,
	size: u64,
	bytes: &[u8],
	target: [u32; 2],
	mut ship: impl FnMut(Road, [u32; 2], [u32; 2], Vec<u8>),
) -> Outcome {
	if let Some((size, source, rgba)) = bucket_load(path, mtime, target) {
		ship(Road::Bucket, size, source, rgba);
		return Outcome::Shipped;
	}

	let thumb = Thumb::of(path, mtime, size);

	if let Some(png) = thumb.load()
		&& let Ok(image) = image::load_from_memory(&png)
	{
		let size = [ image.width(), image.height() ];
		let source = dimensions(bytes).map_or(size, |(w, h)| [ w, h ]);

		let same_shape = |a: [u32; 2], b: [u32; 2]| {
			let (a, b) = (a[0] as f64 * b[1] as f64, b[0] as f64 * a[1] as f64);
			(a - b).abs() <= (a.max(b) * 0.02).max(2.0)
		};
		if same_shape(size, source) {
			ship(Road::Thumbnail, size, source, image.to_rgba8().into_raw());
		}
	}

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

	ship(Road::Decode, size, [ image.width(), image.height() ], rgba.clone());

	bucket_store(path, mtime, target, size, [ image.width(), image.height() ], &rgba);
	thumb.store([ image.width(), image.height() ], image.as_raw());
	Outcome::Shipped
}

fn pixels_of(image: &image::RgbaImage, target: [u32; 2]) -> ([u32; 2], Vec<u8>) {
	let source = [ image.width(), image.height() ];
	let size = upload_size(source, target);

	if size == source {
		return (size, image.as_raw().clone());
	}

	let (bytes, from) = match coarse_stage(source) {
		Some(half) => match resample_fast(image.as_raw(), source, half) {
			Some(rgba) => (rgba, half),
			None => (image.as_raw().clone(), source),
		},
		None => (image.as_raw().clone(), source),
	};

	match resample(&bytes, from, size) {
		Some(rgba) => (size, rgba),
		None => (source, image.as_raw().clone()),
	}
}

fn upload_note(path: &str, from: [u32; 2], to: [u32; 2], target: [u32; 2]) {
	let name = std::path::Path::new(path).file_name().map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned());

	crate::metrics::upload(&name, from, to, target);
}

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

pub(crate) fn warm(path: &str, mtime: Option<SystemTime>, size: u64, bytes: &[u8], target: [u32; 2]) -> bool {
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

		let (drawn, _, frames) = decode_frames([ 20, 0 ], &bytes).expect("frames with a width-only target");
		assert_eq!(drawn, [ 20, 10 ], "the display rectangle: the width given, the shape in charge of the height");
		assert!(frames.iter().all(|(rgba, _)| rgba.len() == 20 * 10 * 4), "and the pixels agree");

		let (drawn, _, _) = decode_frames([ 800, 0 ], &bytes).expect("a big width");
		assert_eq!(drawn, [ 40, 20 ], "never blown up here — the sampler scales a small one");

		let (drawn, _, frames) = decode_frames([ 20, 10 ], &bytes).expect("frames again");
		assert_eq!(drawn, [ 20, 10 ], "the frames are the column's size");
		assert!(frames.iter().all(|(rgba, _)| rgba.len() == 20 * 10 * 4), "and so is every buffer");

		assert!(decode_frames([ 800, 600 ], b"not a gif at all").is_none());
	}

	#[test]
	fn a_frames_delay_has_a_floor() {
		assert_eq!(frame_ms(image::Delay::from_numer_denom_ms(0, 1)), 20);
		assert_eq!(frame_ms(image::Delay::from_numer_denom_ms(120, 1)), 120);
		assert_eq!(frame_ms(image::Delay::from_numer_denom_ms(1, 1)), 20, "one millisecond is past believing");
	}

	#[test]
	fn a_zoom_scales_the_render_target() {
		assert_eq!(request_target([ 800, 600 ], 1.0), [ 800, 600 ], "fit asks for the column");
		assert_eq!(request_target([ 800, 600 ], 2.0), [ 1600, 1200 ]);
		assert_eq!(request_target([ 800, 600 ], 0.5), [ 400, 300 ], "and a smaller zoom asks for less");
		assert_eq!(request_target([ 5000, 5000 ], 8.0), [ 6000, 6000 ], "bounded");
	}

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

	#[test]
	fn a_picture_is_uploaded_at_the_size_it_is_shown() {
		assert_eq!(upload_size([ 2000, 6000 ], [ 454, 0 ]), [ 454, 1362 ], "the column's width, the file's shape");
		assert_eq!(upload_size([ 2000, 6000 ], [ 568, 0 ]), [ 568, 1704 ], "zoom widens the rectangle, the shape keeps the ratio");
		assert_eq!(upload_size([ 3840, 2160 ], [ 454, 0 ]), [ 454, 255 ]);

		assert_eq!(upload_size([ 100, 50 ], [ 400, 0 ]), [ 100, 50 ], "a small one keeps its own pixels");
		assert_eq!(upload_size([ 600, 400 ], [ 800, 0 ]), [ 600, 400 ]);
		assert_eq!(upload_size([ 10, 10 ], [ 300, 200 ]), [ 10, 10 ], "an explicit height changes nothing for a small one");

		assert_eq!(upload_size([ 0, 10 ], [ 300, 0 ]), [ 1, 1 ]);

		assert!(coarse_stage([ 4000, 3000 ]).is_none(), "twelve megapixels is one stage");
		assert_eq!(coarse_stage([ 8000, 8000 ]), Some([ 4000, 4000 ]), "sixty-four megapixels is two");
		assert_eq!(coarse_stage([ 20000, 20000 ]), Some([ 10000, 10000 ]));
	}

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

	#[test]
	fn a_picture_arrives_whole_and_names_the_road_it_took() {
		let (w, h) = (600u32, 400u32);
		let mut bytes = Vec::new();
		image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([ (x % 256) as u8, (y % 256) as u8, 90, 255 ]))
			.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
			.expect("the fixture encodes");

		let mut answers = Vec::new();
		let shipped = decode_into(
			"/tmp/crabhop-n2-never/never.png",
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

	#[test]
	fn a_bucket_entry_survives_the_round_trip() {
		let at = std::env::temp_dir().join(format!("crabhop-bucket-{}.rgba", std::process::id()));
		let _ = std::fs::remove_file(&at);

		assert!(bucket_read(&at).is_none(), "nothing stands there yet");

		let rgba = vec![ 7u8; 2 * 2 * 4 ];
		bucket_write(&at, [ 2, 2 ], [ 4, 4 ], &rgba);
		let (size, source, back) = bucket_read(&at).expect("the entry is there");
		assert_eq!((size, source), ([ 2, 2 ], [ 4, 4 ]), "both sizes survive");
		assert_eq!(back, rgba, "and so do the pixels");

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

		let mut old = Vec::new();
		old.extend_from_slice(b"DFPIX\x00\x00\x01");
		for side in [ 2u32, 2, 4, 4 ] {
			old.extend_from_slice(&side.to_le_bytes());
		}
		old.extend_from_slice(&[ 7u8; 16 ]);
		std::fs::write(&at, &old).expect("a file");
		assert!(bucket_read(&at).is_none(), "an entry from before the fine filter is not trusted");

		let _ = std::fs::remove_file(&at);

		assert!(!bucket_has("/tmp/crabhop-never/none.png", None, [ 300, 200 ]), "no mtime, no entry");
	}

	#[test]
	fn a_prefetch_is_a_cache_write_and_no_picture() {
		let (w, h) = (256u32, 192u32);
		let mut bytes = Vec::new();
		image::RgbaImage::from_pixel(w, h, image::Rgba([ 1, 2, 3, 255 ]))
			.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
			.expect("the fixture encodes");

		assert!(warm("/tmp/crabhop-n2-never/warm.png", None, bytes.len() as u64, &bytes, [ 256, 256 ]));
		assert!(!warm("/tmp/crabhop-n2-never/warm.png", None, 4, b"nope", [ 256, 256 ]), "junk is not warmed");
	}

	#[test]
	fn a_picture_is_as_wide_as_the_column_and_as_tall_as_it_is() {
		assert_eq!(fit_width((200, 800), 400.0), [ 400.0, 1600.0 ]);
		assert_eq!(fit_width((800, 200), 400.0), [ 400.0, 100.0 ]);
		assert_eq!(fit_width((100, 50), 400.0), [ 400.0, 200.0 ]);
		assert_eq!(fit_width((0, 10), 400.0), [ 400.0, 4000.0 ]);
		assert_eq!(fit_width((10, 0), 400.0), [ 400.0, 40.0 ]);
	}

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
		let outcome = decode_into("/tmp/crabhop-n2-never/junk.png", None, 4, b"nope", [ 100, 100 ], | _, _, _, _| {
			shipped = true
		});

		assert_eq!(outcome, Outcome::Unreadable, "junk is not a picture");
		assert!(!shipped, "and nothing is shipped for it");
	}

	#[test]
	fn the_cache_drops_the_least_recently_used_image() {
		let mut lru = Lru::new(100);

		assert!(lru.touch("a", 60).is_empty());
		assert_eq!(lru.touch("b", 60), vec![ "a".to_owned() ], "60 + 60 no longer fits into 100");
		assert_eq!(lru.touch("a", 60), vec![ "b".to_owned() ], "using a again makes b the old one");
		assert!(lru.touch("c", 10).is_empty(), "the eviction already happened above");
	}
}
