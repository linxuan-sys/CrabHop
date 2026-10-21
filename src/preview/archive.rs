
use std::{
	collections::HashSet,
	io::{BufReader, Read},
	path::Path,
};

use crate::preview::rows::{Row, advance};
use crate::tokens::{ICON_BOX, Tokens};
use crate::list;

pub(crate) const CAP: usize = 2000;

const INDENT: f32 = 16.0;

const BASE: f32 = 8.0;

pub(crate) struct Item {
	pub name:  String,
	pub path:  String,
	pub depth: usize,
	pub dir:   bool,
	pub size:  Option<u64>,
}

pub(crate) struct Listing {
	pub items:     Vec<Item>,
	pub total:     Option<usize>,
	pub truncated: bool,
}

#[derive(Debug)]
pub(crate) enum Fail {
	Encrypted,
	Unreadable,
	Unsupported(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
enum Family {
	Zip,
	Tar,
	TarGz,
	TarBz2,
	TarXz,
	TarZst,
	Gz,
	Bz2,
	Xz,
}

fn family(name: &str) -> Option<Family> {
	let lower = name.to_ascii_lowercase();
	let ends = |suffix: &str| lower.ends_with(suffix);

	if ends(".zip") || ends(".jar") || ends(".cbz") || ends(".apk") {
		Some(Family::Zip)
	} else if ends(".tar.gz") || ends(".tgz") {
		Some(Family::TarGz)
	} else if ends(".tar.bz2") || ends(".tbz") || ends(".tbz2") || ends(".tb2") {
		Some(Family::TarBz2)
	} else if ends(".tar.xz") || ends(".txz") {
		Some(Family::TarXz)
	} else if ends(".tar.zst") || ends(".tar.zstd") || ends(".tzst") {
		Some(Family::TarZst)
	} else if ends(".tar") {
		Some(Family::Tar)
	} else if ends(".gz") {
		Some(Family::Gz)
	} else if ends(".bz2") {
		Some(Family::Bz2)
	} else if ends(".xz") {
		Some(Family::Xz)
	} else {
		None
	}
}

fn unsupported(name: &str) -> Option<&'static str> {
	let lower = name.to_ascii_lowercase();

	if lower.ends_with(".7z") {
		Some("7z")
	} else if lower.ends_with(".rar") {
		Some("rar")
	} else {
		None
	}
}

pub(crate) fn is_archive(name: &str) -> bool { family(name).is_some() }

pub(crate) fn is_known(name: &str) -> bool { is_archive(name) || unsupported(name).is_some() }

pub(crate) fn read(path: &Path) -> Result<Listing, Fail> {
	let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
	if let Some(format) = unsupported(name) {
		return Err(Fail::Unsupported(format));
	}

	let Some(family) = family(name) else {
		return Err(Fail::Unreadable);
	};

	let file = std::fs::File::open(path).map_err(|_| Fail::Unreadable)?;

	match family {
		Family::Zip => read_zip(file),
		Family::Tar => read_tar(file),
		Family::TarGz => read_tar(flate2::read::GzDecoder::new(file)),
		Family::TarBz2 => read_tar(bzip2_rs::DecoderReader::new(file)),
		Family::TarXz => read_tar_xz(file),
		Family::TarZst => {
			let decoder = zstd::stream::read::Decoder::new(file).map_err(|_| Fail::Unreadable)?;
			read_tar(decoder)
		}
		Family::Gz => {
			let (inner, size) = gz_facts(path, name);
			Ok(single_stream(inner, size))
		}
		Family::Bz2 => Ok(single_stream(strip_suffix(name, ".bz2"), None)),
		Family::Xz => Ok(single_stream(strip_suffix(name, ".xz"), None)),
	}
}

const XZ_BYTES: u64 = 64 * 1024 * 1024;

fn read_tar_xz(mut file: std::fs::File) -> Result<Listing, Fail> {
	let (mut tar, mut at) = (Vec::new(), 0);
	{
		let mut capped = Cap { at: &mut at, out: &mut tar };
		let _ = lzma_rs::xz_decompress(&mut std::io::BufReader::new(&mut file), &mut capped);
	}

	let mut listing = read_tar(std::io::Cursor::new(tar))?;
	listing.truncated |= at >= XZ_BYTES;
	Ok(listing)
}

struct Cap<'a> {
	at:  &'a mut u64,
	out: &'a mut Vec<u8>,
}

impl std::io::Write for Cap<'_> {
	fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
		if *self.at >= XZ_BYTES {
			return Err(std::io::Error::other("the listing's own cap"));
		}

		let take = bytes.len().min((XZ_BYTES - *self.at) as usize);
		self.out.extend_from_slice(&bytes[..take]);
		*self.at += take as u64;
		Ok(take)
	}

	fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

fn single_stream(name: String, size: Option<u64>) -> Listing {
	let name = name.trim_start_matches("./").trim_end_matches('/').to_owned();

	Listing {
		items:     vec![ Item { path: name.clone(), name, depth: 1, dir: false, size } ],
		total:     Some(1),
		truncated: false,
	}
}

fn strip_suffix(name: &str, suffix: &str) -> String {
	name.strip_suffix(suffix).unwrap_or(name).rsplit('/').next().unwrap_or(name).to_owned()
}

fn gz_facts(path: &Path, file_name: &str) -> (String, Option<u64>) {
	use std::io::{Read, Seek, SeekFrom};

	let named = std::fs::File::open(path)
		.ok()
		.and_then(|file| flate2::read::GzDecoder::new(file).header().and_then(|header| header.filename()).map(<[u8]>::to_vec))
		.filter(|raw| !raw.is_empty())
		.and_then(|raw| String::from_utf8(raw).ok())
		.map(|name| name.trim_start_matches("./").to_owned());

	let size = std::fs::File::open(path).ok().and_then(|mut file| {
		let len = file.metadata().ok()?.len();
		if len < 4 {
			return None;
		}

		file.seek(SeekFrom::End(-4)).ok()?;
		let mut tail = [ 0u8; 4 ];
		file.read_exact(&mut tail).ok()?;
		Some(u32::from_le_bytes(tail) as u64)
	});

	(named.unwrap_or_else(|| strip_suffix(file_name, ".gz")), size)
}

fn read_zip(file: std::fs::File) -> Result<Listing, Fail> {
	let mut zip = zip::ZipArchive::new(BufReader::new(file)).map_err(|_| Fail::Unreadable)?;
	let total = zip.len();
	let take = total.min(CAP);
	let mut items = Vec::with_capacity(take);

	for i in 0..take {
		let entry = zip.by_index_raw(i).map_err(|_| Fail::Unreadable)?;
		if entry.encrypted() {
			return Err(Fail::Encrypted);
		}

		let dir = entry.is_dir();
		push(&mut items, entry.name(), dir, (!dir).then(|| entry.size()));
	}

	Ok(Listing { items, total: Some(total), truncated: total > take })
}

fn read_tar<R: Read>(reader: R) -> Result<Listing, Fail> {
	let mut archive = tar::Archive::new(reader);
	let entries = archive.entries().map_err(|_| Fail::Unreadable)?;
	let mut items = Vec::new();
	let mut truncated = false;

	for entry in entries {
		let entry = entry.map_err(|_| Fail::Unreadable)?;
		if items.len() >= CAP {
			truncated = true;
			break;
		}

		let dir = entry.header().entry_type().is_dir();
		let size = (!dir).then(|| entry.header().size().unwrap_or(0));
		let path = entry.path().map_err(|_| Fail::Unreadable)?.into_owned();

		push(&mut items, &path.to_string_lossy(), dir, size);
	}

	Ok(Listing { items, total: None, truncated })
}

fn push(items: &mut Vec<Item>, path: &str, dir: bool, size: Option<u64>) {
	let path = path.trim_start_matches("./").trim_end_matches('/');
	if path.is_empty() {
		return;
	}

	let depth = path.matches('/').count() + 1;
	let name = path.rsplit('/').next().unwrap_or(path).to_owned();

	items.push(Item { name, path: path.to_owned(), depth, dir, size });
}

fn hidden(path: &str, collapsed: &HashSet<String>) -> bool {
	path.match_indices('/').any(|(at, _)| collapsed.contains(&path[..at]))
}

pub(crate) fn visible(items: &[Item], collapsed: &HashSet<String>) -> Vec<usize> {
	items.iter().enumerate().filter(|(_, item)| !hidden(&item.path, collapsed)).map(|(at, _)| at).collect()
}

pub(crate) fn default_collapsed(items: &[Item]) -> HashSet<String> {
	items.iter().filter(|item| item.dir && item.depth >= 2).map(|item| item.path.clone()).collect()
}

pub(crate) fn tree(items: &[Item], collapsed: &HashSet<String>, tokens: &Tokens) -> Vec<Row> {
	const SIZE: f32 = 14.0;

	visible(items, collapsed)
		.into_iter()
		.map(|at| {
			let item = &items[at];
			let (glyph, colour) = if item.dir {
				(if collapsed.contains(&item.path) { "▸" } else { "▾" }, tokens.blue)
			} else {
				list::icon_for(&item.name, false, false, tokens)
			};
			let indent = BASE + item.depth.saturating_sub(1) as f32 * INDENT + advance(SIZE) + ICON_BOX * 0.5;

			Row {
				text: item.name.clone(),
				height: 24.0,
				indent,
				size: SIZE,
				colour: if item.dir { tokens.blue } else { tokens.text },
				lead: Some((glyph.to_owned(), colour)),
				right: (!item.dir).then(|| item.size.map_or_else(|| "—".to_owned(), list::size)),
				..Default::default()
			}
		})
		.collect()
}

#[cfg(test)]
mod tests {
	use std::io::Write;

	use super::*;

	fn zip_fixture(dir: &Path) -> std::path::PathBuf {
		let path = dir.join("tree.zip");
		let file = std::fs::File::create(&path).expect("a zip file");
		let mut zip = zip::ZipWriter::new(file);
		let options: zip::write::FileOptions<'_, ()> =
			zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);

		zip.add_directory("docs/", options).unwrap();
		zip.add_directory("docs/deep/", options).unwrap();
		zip.start_file("docs/a.txt", options).unwrap();
		zip.write_all(b"hello").unwrap();
		zip.start_file("docs/deep/b.bin", options).unwrap();
		zip.write_all(&[0u8; 12]).unwrap();
		zip.start_file("top.md", options).unwrap();
		zip.write_all(b"# hi").unwrap();
		zip.finish().unwrap();
		path
	}

	fn tar_zst_fixture(dir: &Path) -> std::path::PathBuf {
		let path = dir.join("tree.tar.zst");
		let file = std::fs::File::create(&path).expect("a tar.zst file");
		let encoder = zstd::stream::write::Encoder::new(file, 3).expect("a zstd encoder");
		let mut tar = tar::Builder::new(encoder);

		let mut header = tar::Header::new_gnu();
		header.set_size(5);
		header.set_mode(0o644);
		header.set_cksum();
		tar.append_data(&mut header, "docs/a.txt", &b"hello"[..]).unwrap();
		tar.append_dir("docs/deep", ".").unwrap();

		let mut header = tar::Header::new_gnu();
		header.set_size(3);
		header.set_mode(0o644);
		header.set_cksum();
		tar.append_data(&mut header, "docs/deep/b.txt", &b"abc"[..]).unwrap();

		let encoder = tar.into_inner().unwrap();
		encoder.finish().unwrap();
		path
	}

	#[test]
	fn a_zip_lists_its_tree_with_depths_and_sizes() {
		let dir = std::env::temp_dir().join(format!("crabhop-p11-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");

		let listing = read(&zip_fixture(&dir)).expect("the fixture lists");
		let names: Vec<(&str, usize, bool, Option<u64>)> =
			listing.items.iter().map(|i| (i.name.as_str(), i.depth, i.dir, i.size)).collect();

		assert_eq!(names, vec![
			("docs", 1, true, None),
			("deep", 2, true, None),
			("a.txt", 2, false, Some(5)),
			("b.bin", 3, false, Some(12)),
			("top.md", 1, false, Some(4)),
		]);
		assert_eq!(listing.total, Some(5), "the central directory knows the count");
		assert!(!listing.truncated);
		assert_eq!(listing.items[3].path, "docs/deep/b.bin", "a fold names the whole path");

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn a_tar_zst_lists_through_its_decoder() {
		let dir = std::env::temp_dir().join(format!("crabhop-p11-zst-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");

		let listing = read(&tar_zst_fixture(&dir)).expect("the fixture lists");
		let names: Vec<(&str, usize, bool, Option<u64>)> =
			listing.items.iter().map(|i| (i.name.as_str(), i.depth, i.dir, i.size)).collect();

		assert_eq!(names, vec![ ("a.txt", 2, false, Some(5)), ("deep", 2, true, None), ("b.txt", 3, false, Some(3)) ]);
		assert_eq!(listing.total, None, "a tar does not say its count up front");
		assert!(!listing.truncated);

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn a_tar_bz2_lists_through_its_decoder() {
		const TREE_TAR_BZ2: &[u8] = &[
			66, 90, 104, 57, 49, 65, 89, 38, 83, 89, 70, 20, 99, 66, 0, 0, 190, 255, 132, 193, 128, 2, 32, 64,
			1, 255, 128, 0, 4, 0, 105, 126, 101, 158, 64, 0, 4, 0, 72, 48, 0, 184, 12, 50, 52, 211, 76, 140,
			38, 70, 9, 160, 48, 97, 145, 166, 154, 100, 97, 50, 48, 77, 1, 129, 20, 80, 155, 68, 154, 109, 70, 135,
			169, 180, 201, 7, 169, 233, 169, 221, 223, 82, 254, 56, 69, 141, 135, 228, 10, 231, 65, 1, 246, 183, 165, 6,
			9, 232, 242, 153, 173, 193, 181, 68, 150, 2, 95, 214, 93, 52, 238, 88, 80, 109, 16, 237, 158, 151, 33, 88,
			229, 44, 142, 81, 16, 243, 106, 106, 168, 98, 140, 56, 213, 207, 129, 232, 246, 78, 81, 75, 107, 26, 49, 91,
			55, 22, 5, 237, 247, 54, 13, 129, 213, 196, 100, 114, 241, 176, 230, 231, 241, 142, 252, 53, 143, 177, 215, 107,
			71, 97, 153, 120, 187, 155, 187, 30, 7, 119, 3, 213, 16, 255, 23, 114, 69, 56, 80, 144, 70, 20, 99, 66,
		];

		let dir = std::env::temp_dir().join(format!("crabhop-bz2-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");

		let at = dir.join("tree.tar.bz2");
		std::fs::write(&at, TREE_TAR_BZ2).expect("the fixture lands");
		let listing = read(&at).expect("the fixture lists");

		let names: Vec<(&str, usize, bool, Option<u64>)> =
			listing.items.iter().map(|i| (i.name.as_str(), i.depth, i.dir, i.size)).collect();
		assert_eq!(names, vec![ ("a.txt", 1, false, Some(5)), ("d", 1, true, None), ("b.txt", 2, false, Some(3)) ]);
		assert_eq!(listing.total, None, "a tar does not say its count up front");

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn a_tar_xz_lists_through_its_decoder() {
		let dir = std::env::temp_dir().join(format!("crabhop-xz-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");

		let mut tar = Vec::new();
		{
			let mut builder = tar::Builder::new(&mut tar);
			let mut header = tar::Header::new_gnu();
			header.set_size(5);
			header.set_mode(0o644);
			header.set_cksum();
			builder.append_data(&mut header, "docs/a.txt", &b"hello"[..]).unwrap();
			builder.append_dir("deep", ".").unwrap();

			let mut header = tar::Header::new_gnu();
			header.set_size(3);
			header.set_mode(0o644);
			header.set_cksum();
			builder.append_data(&mut header, "deep/b.txt", &b"abc"[..]).unwrap();
			builder.into_inner().unwrap();
		}

		let mut xz = Vec::new();
		lzma_rs::xz_compress(&mut std::io::BufReader::new(&tar[..]), &mut xz).expect("the fixture compresses");

		let at = dir.join("tree.tar.xz");
		std::fs::write(&at, xz).expect("the fixture lands");
		let listing = read(&at).expect("the fixture lists");

		let names: Vec<(&str, usize, bool, Option<u64>)> =
			listing.items.iter().map(|i| (i.name.as_str(), i.depth, i.dir, i.size)).collect();
		assert_eq!(names, vec![ ("a.txt", 2, false, Some(5)), ("deep", 1, true, None), ("b.txt", 2, false, Some(3)) ]);
		assert!(!listing.truncated);

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn a_single_stream_is_one_row() {
		let dir = std::env::temp_dir().join(format!("crabhop-single-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");

		let at = dir.join("note.txt.gz");
		{
			let file = std::fs::File::create(&at).expect("a file");
			let mut encoder = flate2::GzBuilder::new().filename("note.txt").write(file, flate2::Compression::fast());
			use std::io::Write;
			encoder.write_all(b"hello gzip").unwrap();
			encoder.finish().unwrap();
		}

		let listing = read(&at).expect("the gzip lists");
		assert_eq!(listing.total, Some(1), "one stream, one entry, and the format knows it");
		assert_eq!(listing.items.len(), 1);
		assert_eq!(listing.items[0].name, "note.txt", "the header's own name");
		assert_eq!(listing.items[0].size, Some(10), "the trailer's own count");

		let bare = dir.join("data.bin.gz");
		{
			let file = std::fs::File::create(&bare).expect("a file");
			let mut encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
			use std::io::Write;
			encoder.write_all(b"four").unwrap();
			encoder.finish().unwrap();
		}
		let listing = read(&bare).expect("the anonymous gzip lists");
		assert_eq!(listing.items[0].name, "data.bin");
		assert_eq!(listing.items[0].size, Some(4));

		for (name, inner) in [ ("note.txt.bz2", "note.txt"), ("note.txt.xz", "note.txt") ] {
			let at = dir.join(name);
			std::fs::write(&at, b"junk the listing never reads").expect("a file");
			let listing = read(&at).expect("the name alone is enough for one row");
			assert_eq!(listing.items[0].name, inner, "{name}");
			assert_eq!(listing.items[0].size, None, "{name} states no size");
		}

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn the_tree_indents_a_level_at_a_time() {
		let tokens = Tokens::MOCHA;
		let dir = std::env::temp_dir().join(format!("crabhop-p11-tree-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");

		let listing = read(&zip_fixture(&dir)).expect("the fixture lists");
		let rows = tree(&listing.items, &HashSet::new(), &tokens);

		assert_eq!(rows.len(), 5, "nothing is shut until something shuts it");
		for (row, item) in rows.iter().zip(&listing.items) {
			assert!(row.lead.is_some(), "every entry wears a lead glyph");
			assert_eq!(row.size, 14.0, "the tree's own face");
			if item.dir {
				assert!(row.right.is_none(), "a directory's size says nothing");
				assert_eq!(row.colour, tokens.blue);
			} else {
				assert!(row.right.is_some());
			}
		}

		let lead_x = |row: &Row| row.indent - advance(row.size) - ICON_BOX * 0.5;
		assert!((lead_x(&rows[1]) - lead_x(&rows[0]) - INDENT).abs() < 0.01, "level two is one step in");
		assert!((lead_x(&rows[3]) - lead_x(&rows[0]) - 2.0 * INDENT).abs() < 0.01, "level three, two steps");
		assert!((lead_x(&rows[4]) - lead_x(&rows[0])).abs() < 0.01, "and a top-level file sits at the base");

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn the_first_level_opens_and_the_rest_stay_shut() {
		let tokens = Tokens::MOCHA;
		let dir = std::env::temp_dir().join(format!("crabhop-p28-fold-unit-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");

		let listing = read(&zip_fixture(&dir)).expect("the fixture lists");
		let mut collapsed = default_collapsed(&listing.items);
		assert_eq!(collapsed, HashSet::from([ "docs/deep".to_owned() ]), "only the second level shuts");

		let names = |collapsed: &HashSet<String>| -> Vec<String> {
			tree(&listing.items, collapsed, &tokens).into_iter().map(|row| row.text).collect()
		};
		assert_eq!(names(&collapsed), vec![ "docs", "deep", "a.txt", "top.md" ], "deep's own file is hidden");

		let leads: Vec<String> =
			tree(&listing.items, &collapsed, &tokens).into_iter().filter_map(|row| row.lead.map(|(g, _)| g)).collect();
		assert_eq!(leads, vec![ "▾", "▸", "\u{f0219}", "\u{f48a}" ]);

		assert!(collapsed.remove("docs/deep"));
		assert_eq!(names(&collapsed), vec![ "docs", "deep", "a.txt", "b.bin", "top.md" ]);

		collapsed.insert("docs".to_owned());
		assert_eq!(names(&collapsed), vec![ "docs", "top.md" ], "a shut parent hides its whole subtree");

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn the_formats_are_told_by_their_names() {
		for name in [
			"a.zip", "B.ZIP", "a.jar", "a.cbz", "a.tar", "a.tar.gz", "a.tgz", "x.tar.zst", "x.tzst",
			"a.tar.bz2", "a.tbz", "a.tbz2", "a.tb2", "a.tar.xz", "a.txz", "a.gz", "a.bz2", "a.xz",
		] {
			assert!(is_archive(name), "{name}");
			assert!(is_known(name), "{name}");
		}
		for name in [ "a.7z", "A.7Z", "a.rar", "a.zst", "a.zstd", "a.txt", "archive" ] {
			assert!(!is_archive(name), "{name}");
		}
		assert!(is_known("a.7z") && is_known("a.rar"), "the two the preview still names");
		assert!(!is_known("a.txt"), "a text file is nobody's archive");

		assert_eq!(family("x.tar.bz2"), Some(Family::TarBz2));
		assert_eq!(family("x.tbz"), Some(Family::TarBz2));
		assert_eq!(family("x.tar.xz"), Some(Family::TarXz));
		assert_eq!(family("x.bz2"), Some(Family::Bz2));
		assert_eq!(family("x.xz"), Some(Family::Xz));
		assert_eq!(family("x.gz"), Some(Family::Gz));

		let missing = std::path::PathBuf::from("/tmp/crabhop-n2-never/shut.7z");
		match read(&missing) {
			Err(Fail::Unsupported(format)) => assert_eq!(format, "7z"),
			other => panic!("a 7z is named, not read: {:?}", other.err()),
		}
	}

	#[test]
	fn junk_is_not_a_listing() {
		let dir = std::env::temp_dir().join(format!("crabhop-p11-junk-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");

		let junk = dir.join("broken.zip");
		std::fs::write(&junk, b"this is not a zip at all").expect("a file");
		assert!(matches!(read(&junk), Err(Fail::Unreadable)));

		let _ = std::fs::remove_dir_all(&dir);
	}
}
