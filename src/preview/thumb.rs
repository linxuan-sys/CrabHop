
use std::{
	path::PathBuf,
	sync::LazyLock,
	time::{SystemTime, UNIX_EPOCH},
};

const BOXES: [(u32, &str); 2] = [ (256, "large"), (128, "normal") ];

pub(crate) struct Thumb {
	uri:   String,
	hash:  String,
	mtime: u64,
	size:  u64,
	mime:  &'static str,
}

impl Thumb {
	pub(crate) fn of(path: &str, mtime: Option<SystemTime>, size: u64) -> Self {
		let uri = file_uri(path);

		Self {
			hash: md5_hex(uri.as_bytes()),
			mtime: mtime
				.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
				.map_or(0, |d| d.as_secs()),
			uri,
			size,
			mime: mime_of(path),
		}
	}

	pub(crate) fn load(&self) -> Option<Vec<u8>> {
		for (_, dir) in BOXES {
			let Ok(png) = std::fs::read(cache_dir(dir).join(format!("{}.png", self.hash))) else {
				continue;
			};

			let Ok(dimensions) = dimensions(&png) else { continue };
			let (width, height) = (dimensions.0, dimensions.1);
			if width == 0 || height == 0 || width > 1024 || height > 1024 {
				continue;
			}

			if !self.accepts(&texts(&png)) {
				continue;
			}

			return Some(png);
		}

		None
	}

	fn accepts(&self, texts: &[(String, String)]) -> bool {
		let value = |key: &str| texts.iter().find(|(k, _)| k == key).map(|(_, v)| v.trim());

		if value("Thumb::URI") != Some(self.uri.as_str()) {
			return false;
		}
		if value("Thumb::MTime") != Some(self.mtime.to_string().as_str()) {
			return false;
		}
		if let Some(size) = value("Thumb::Size")
			&& size != self.size.to_string()
		{
			return false;
		}

		true
	}

	pub(crate) fn failed(&self) {
		if self.mtime == 0 {
			return;
		}

		let png = image::RgbaImage::from_pixel(1, 1, image::Rgba([ 0, 0, 0, 0 ]));
		if let Some(bytes) = encode(&png, &[ ("Software", "crabhop") ]) {
			write_atomically(&cache_dir("fail").join(FILE).join(format!("{}.png", self.hash)), &bytes);
		}
	}

	pub(crate) fn store(&self, size: [u32; 2], rgba: &[u8]) {
		if self.mtime == 0 || size[0] < 128 || size[1] < 128 {
			return;
		}
		let Some(image) = image::RgbaImage::from_raw(size[0], size[1], rgba.to_vec()) else { return };

		let mtime = self.mtime.to_string();
		let size = self.size.to_string();
		let texts = [
			("Thumb::URI", self.uri.as_str()),
			("Thumb::MTime", mtime.as_str()),
			("Thumb::Size", size.as_str()),
			("Thumb::Mimetype", self.mime),
			("Software", "crabhop"),
		];

		for (box_side, dir) in BOXES {
			let Some(fitted) = shrink(&image, box_side) else { continue };

			if let Some(bytes) = encode(&fitted, &texts) {
				write_atomically(&cache_dir(dir).join(format!("{}.png", self.hash)), &bytes);
			}
		}
	}
}

fn shrink(image: &image::RgbaImage, box_side: u32) -> Option<image::RgbaImage> {
	use fast_image_resize::{FilterType, PixelType, ResizeAlg, ResizeOptions, Resizer, images::Image};

	let (w, h) = crate::preview::img::fit((image.width(), image.height()), [ box_side, box_side ]);
	if w == 0 || h == 0 {
		return None;
	}
	if (w, h) == (image.width(), image.height()) {
		return Some(image.clone());
	}

	let src = Image::from_vec_u8(image.width(), image.height(), image.as_raw().clone(), PixelType::U8x4).ok()?;
	let mut dst = Image::new(w, h, PixelType::U8x4);

	Resizer::new()
		.resize(&src, &mut dst, &ResizeOptions::new().resize_alg(ResizeAlg::Convolution(FilterType::Lanczos3)))
		.ok()?;

	image::RgbaImage::from_raw(w, h, dst.into_vec())
}

const FILE: &str = concat!("crabhop-", env!("CARGO_PKG_VERSION"));

fn encode(image: &image::RgbaImage, texts: &[(&str, &str)]) -> Option<Vec<u8>> {
	let mut png = Vec::new();
	{
		let mut encoder = png::Encoder::new(std::io::Cursor::new(&mut png), image.width(), image.height());
		encoder.set_color(png::ColorType::Rgba);
		encoder.set_depth(png::BitDepth::Eight);

		for (key, value) in texts {
			encoder.add_text_chunk((*key).to_owned(), (*value).to_owned()).ok()?;
		}

		let mut writer = encoder.write_header().ok()?;
		writer.write_image_data(image.as_raw()).ok()?;
		writer.finish().ok()?;
	}

	Some(png)
}

fn texts(png: &[u8]) -> Vec<(String, String)> {
	let Ok(reader) = png::Decoder::new(std::io::Cursor::new(png)).read_info() else {
		return Vec::new();
	};

	reader
		.info()
		.uncompressed_latin1_text
		.iter()
		.map(|chunk| (chunk.keyword.clone(), chunk.text.clone()))
		.collect()
}

fn dimensions(png: &[u8]) -> Result<(u32, u32), ()> {
	let reader = png::Decoder::new(std::io::Cursor::new(png)).read_info().map_err(|_| ())?;
	Ok((reader.info().width, reader.info().height))
}

pub(crate) fn file_uri(path: &str) -> String {
	let mut out = String::from("file://");

	for byte in path.bytes() {
		let safe = byte.is_ascii_alphanumeric()
			|| matches!(
				byte,
				b'/' | b'-' | b'.' | b'_' | b'~' | b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'=' | b':' | b'@'
			);

		if safe {
			out.push(byte as char);
		} else {
			out.push_str(&format!("%{byte:02X}"));
		}
	}

	out
}

fn mime_of(path: &str) -> &'static str {
	match path.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
		Some("png") => "image/png",
		Some("jpg" | "jpeg") => "image/jpeg",
		Some("gif") => "image/gif",
		Some("webp") => "image/webp",
		Some("bmp") => "image/bmp",
		Some("tif" | "tiff") => "image/tiff",
		Some("ico") => "image/x-icon",
		_ => "application/octet-stream",
	}
}

fn cache_dir(name: &str) -> PathBuf {
	let base = std::env::var_os("XDG_CACHE_HOME")
		.map(PathBuf::from)
		.or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
		.unwrap_or_else(|| PathBuf::from(".cache"));

	base.join("thumbnails").join(name)
}

fn write_atomically(path: &PathBuf, bytes: &[u8]) {
	let Some(dir) = path.parent() else { return };
	if std::fs::create_dir_all(dir).is_err() {
		return;
	}
	if let Ok(meta) = std::fs::metadata(dir)
		&& !meta.permissions().readonly()
	{
		let _ = std::fs::set_permissions(dir, std::os::unix::fs::PermissionsExt::from_mode(0o700));
	}

	let tmp = dir.join(format!(
		".{}.tmp{}",
		path.file_stem().unwrap_or_default().to_string_lossy(),
		std::process::id()
	));

	if std::fs::write(&tmp, bytes).is_err() {
		return;
	}
	let _ = std::fs::set_permissions(&tmp, std::os::unix::fs::PermissionsExt::from_mode(0o600));
	let _ = std::fs::rename(&tmp, path);
}

pub(crate) fn md5_hex(input: &[u8]) -> String {
	const SHIFT: [u32; 64] = [
		7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, //
		5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, //
		4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, //
		6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
	];
	static K: LazyLock<[u32; 64]> =
		LazyLock::new(|| std::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32));

	let mut data = input.to_vec();
	let bits = (input.len() as u64).wrapping_mul(8);
	data.push(0x80);
	while data.len() % 64 != 56 {
		data.push(0);
	}
	data.extend_from_slice(&bits.to_le_bytes());

	let (mut a0, mut b0, mut c0, mut d0) =
		(0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);

	for chunk in data.as_chunks::<64>().0 {
		let m: [u32; 16] = std::array::from_fn(|i| u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap()));
		let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);

		for i in 0..64 {
			let (f, g) = match i / 16 {
				0 => ((b & c) | (!b & d), i),
				1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
				2 => (b ^ c ^ d, (3 * i + 5) % 16),
				_ => (c ^ (b | !d), (7 * i) % 16),
			};

			let tmp = d;
			d = c;
			c = b;
			b = b.wrapping_add(
				a.wrapping_add(f).wrapping_add(K[i]).wrapping_add(m[g]).rotate_left(SHIFT[i]),
			);
			a = tmp;
		}

		a0 = a0.wrapping_add(a);
		b0 = b0.wrapping_add(b);
		c0 = c0.wrapping_add(c);
		d0 = d0.wrapping_add(d);
	}

	let mut out = String::with_capacity(32);
	for word in [ a0, b0, c0, d0 ] {
		for byte in word.to_le_bytes() {
			out.push_str(&format!("{byte:02x}"));
		}
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn md5_matches_the_rfc() {
		assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
		assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
		assert_eq!(md5_hex(b"The quick brown fox jumps over the lazy dog"), "9e107d9d372bb6826bd81d3542a419d6");
		assert_eq!(md5_hex(b"message digest"), "f96b697d7cb7938d525a2f31aaf161d0");
	}

	#[test]
	fn a_file_uri_escapes_what_a_uri_cannot_carry() {
		assert_eq!(file_uri("/home/user/plain.jpg"), "file:///home/user/plain.jpg");
		assert_eq!(file_uri("/home/user/a b.png"), "file:///home/user/a%20b.png");
		assert_eq!(file_uri("/home/陶.png"), "file:///home/%E9%99%B6.png");
		assert_eq!(file_uri("/home/q?x#y.png"), "file:///home/q%3Fx%23y.png");
	}

	#[test]
	fn a_thumbnail_is_only_used_when_it_still_describes_the_file() {
		let mut thumb = Thumb::of("/home/user/x.png", None, 100);
		thumb.mtime = 7;

		let texts = |uri: &str, mtime: &str, size: &str| {
			vec![
				("Thumb::URI".to_owned(), uri.to_owned()),
				("Thumb::MTime".to_owned(), mtime.to_owned()),
				("Thumb::Size".to_owned(), size.to_owned()),
			]
		};

		assert!(thumb.accepts(&texts(&thumb.uri, "7", "100")), "the same file: it passes");
		assert!(!thumb.accepts(&texts(&thumb.uri, "8", "100")), "a newer file makes it old");
		assert!(!thumb.accepts(&texts(&thumb.uri, "7", "101")), "and so does another size");
		assert!(!thumb.accepts(&texts("file:///other.png", "7", "100")), "and so does another file");
		assert!(!thumb.accepts(&texts("", "7", "100")), "a thumbnail without a URI is not one");
		assert!(thumb.accepts(&texts(&thumb.uri, "7 ", "100 ")), "padding around the numbers is no difference");
	}

	#[test]
	fn the_metadata_round_trips_through_the_png() {
		let image = fixture();
		let written = [
			("Thumb::URI", "file:///home/user/x.png"),
			("Thumb::MTime", "1700000000"),
			("Thumb::Size", "12345"),
			("Thumb::Mimetype", "image/png"),
			("Software", "crabhop"),
		];

		let encoded = encode(&image, &written).expect("the thumbnail encodes");
		let read = texts(&encoded);

		for (key, value) in written {
			assert_eq!(
				read.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()),
				Some(value),
				"{key} comes back"
			);
		}
		assert_eq!(dimensions(&encoded), Ok((4, 4)), "and it is still a 4x4 picture");
	}

	#[test]
	fn a_bad_cache_file_is_not_believed() {
		let png = encode(&fixture(), &[]).expect("a thumbnail");

		assert!(dimensions(b"not a png at all").is_err());
		assert!(dimensions(&png[..20]).is_err(), "a truncated header is not a header");
		assert_eq!(texts(b"not a png"), Vec::new(), "and junk carries no metadata");
	}

	fn fixture() -> image::RgbaImage {
		image::RgbaImage::from_pixel(4, 4, image::Rgba([ 12, 34, 56, 255 ]))
	}
}
