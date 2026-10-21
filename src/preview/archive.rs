//! What is inside an archive, without opening it: the entry list of a zip, tar, tar.gz, tar.bz2,
//! tar.xz or tar.zst, and of the single-stream gz/bz2/xz files beside them — read in process, no
//! subprocess and no extraction, and every decoder of it pure Rust (flate2, bzip2-rs, lzma-rs,
//! zstd), so the binary carries its own readers.
//!
//! The rows the preview draws are the archive's own entries in the archive's own order, indented by
//! the depth of their path (§3.7's entry list), and folded a directory at a time (P28's long tail:
//! the first level opens, deeper ones stay shut until they are clicked). A zip knows how many
//! entries it holds from its central directory; a tar only says so as it is scanned, so a listing
//! that stopped at the cap reports what it read instead of a total. A single-stream file is one
//! row — the name its header carries, or its own name less the suffix — and a size only where the
//! format states one cheaply (gzip's trailer does; bz2 and xz do not). 7z and rar are named as
//! archives too, and say why they stay shut — no pure-Rust decoder here reads them.

use std::{
	collections::HashSet,
	io::{BufReader, Read},
	path::Path,
};

use crate::preview::rows::{Row, advance};
use crate::tokens::{ICON_BOX, Tokens};
use crate::list;

/// How many entries are read and shown: past it the listing stops and says so. Bounds both the
/// scan (a tar's headers are all it reads, but a tar.zst still has to be decompressed through) and
/// the rows in memory.
pub(crate) const CAP: usize = 2000;

/// How far one level of the path indents a row, at the tree's own 14px face.
const INDENT: f32 = 16.0;

/// Where the tree's first level starts, inside the column's own inset.
const BASE: f32 = 8.0;

/// One entry as the tree shows it.
pub(crate) struct Item {
	/// The leaf name, a directory's without its trailing slash.
	pub name:  String,
	/// The whole path inside the archive, `./` trimmed and no trailing slash — what a fold names.
	pub path:  String,
	/// How deep the path sits: 1 for a top-level entry.
	pub depth: usize,
	pub dir:   bool,
	/// The entry's size, where the archive states one without a full walk (a bz2/xz single stream
	/// does not; its row keeps the shape and says nothing rather than a made-up number).
	pub size:  Option<u64>,
}

/// What a read produced.
pub(crate) struct Listing {
	pub items:     Vec<Item>,
	/// The archive's own count of entries, when the format states it up front (zip).
	pub total:     Option<usize>,
	pub truncated: bool,
}

/// Why an archive could not be listed.
#[derive(Debug)]
pub(crate) enum Fail {
	/// The entries are there but unreadable without a password.
	Encrypted,
	/// The structure is not what the format promises.
	Unreadable,
	/// A real archive family no decoder in this window reads (7z, rar): the name of it.
	Unsupported(&'static str),
}

/// The archive families this window can list, told by the file's own name — the tar family first,
/// so `.tar.bz2` is a tar of bz2 and not a bz2 named tar.
#[derive(Debug, PartialEq, Eq)]
enum Family {
	Zip,
	Tar,
	TarGz,
	TarBz2,
	TarXz,
	TarZst,
	/// A single compressed stream: one row, the name the format carries (or the file's own less the
	/// suffix), and a size only where the format states one for free.
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

/// The archives whose names promise an entry list but whose format no decoder here opens.
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

/// Whether a file's name promises an archive this window can list.
pub(crate) fn is_archive(name: &str) -> bool { family(name).is_some() }

/// Whether the name promises an archive at all — the listable ones and the ones that will answer
/// with [`Fail::Unsupported`]: the preview names the type either way (P28's long tail).
pub(crate) fn is_known(name: &str) -> bool { is_archive(name) || unsupported(name).is_some() }

/// The entries of the archive at `path`, in the archive's own order.
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

/// How many decompressed bytes a listing will walk through for a tar.xz: the entry cap's own work
/// stops long before this for any ordinary archive, and a stream past it is cut short with a word
/// said rather than decompressed whole into memory.
const XZ_BYTES: u64 = 64 * 1024 * 1024;

/// The tar inside an xz, decompressed through lzma-rs — the one decoder here that hands back a
/// writer's stream rather than a reader's — up to [`XZ_BYTES`], past which the listing is marked
/// cut short.
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

/// The writer that stops the decompressor once the byte cap is reached: its own error is the end
/// of the walk, not a failure to report.
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

/// A single compressed stream as one row: the leaf name inside (the header's own words where the
/// format carries them, the file's own less the suffix otherwise) and the size where free.
fn single_stream(name: String, size: Option<u64>) -> Listing {
	let name = name.trim_start_matches("./").trim_end_matches('/').to_owned();

	Listing {
		items:     vec![ Item { path: name.clone(), name, depth: 1, dir: false, size } ],
		total:     Some(1),
		truncated: false,
	}
}

/// A name less its suffix, as a leaf.
fn strip_suffix(name: &str, suffix: &str) -> String {
	name.strip_suffix(suffix).unwrap_or(name).rsplit('/').next().unwrap_or(name).to_owned()
}

/// A gzip's own two facts, without decompressing a byte: the inner name its header carries (its own
/// file name less `.gz` when the header is anonymous) and the size its trailer states — the last
/// four bytes, little endian, the format's own field.
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
		// the metadata only: no entry is decrypted or decompressed for a listing
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

	// a tar does not say how many entries it holds until they have all gone by, and the listing
	// stops at the cap: the count of what was read is what an honest badge can carry
	Ok(Listing { items, total: None, truncated })
}

/// One entry's leaf name and path depth, from the path the archive stored: `./` prefixes are the
/// tar writer's habit and carry no depth of their own.
fn push(items: &mut Vec<Item>, path: &str, dir: bool, size: Option<u64>) {
	let path = path.trim_start_matches("./").trim_end_matches('/');
	if path.is_empty() {
		return;
	}

	let depth = path.matches('/').count() + 1;
	let name = path.rsplit('/').next().unwrap_or(path).to_owned();

	items.push(Item { name, path: path.to_owned(), depth, dir, size });
}

/// The row a fold hides an entry behind: any ancestor of its path is a directory that is shut.
fn hidden(path: &str, collapsed: &HashSet<String>) -> bool {
	path.match_indices('/').any(|(at, _)| collapsed.contains(&path[..at]))
}

/// The entries a tree shows with these directories shut, as indices into `items` — the rows the
/// preview draws and the rows a click can name, one list so the two can never disagree.
pub(crate) fn visible(items: &[Item], collapsed: &HashSet<String>) -> Vec<usize> {
	items.iter().enumerate().filter(|(_, item)| !hidden(&item.path, collapsed)).map(|(at, _)| at).collect()
}

/// The folders a fresh listing arrives with shut: the first level opens, the ones inside it do not
/// (P28's long tail — what a person sees first is the shape of the archive, not its leaves).
pub(crate) fn default_collapsed(items: &[Item]) -> HashSet<String> {
	items.iter().filter(|item| item.dir && item.depth >= 2).map(|item| item.path.clone()).collect()
}

/// The tree rows: the list's own anatomy — a lead glyph in its 20px slot, the name, the size fact —
/// with the lead and the name both stepping in by one level of the path. A directory's lead is the
/// fold itself (`▾` open, `▸` shut, in the directory's own blue); a file keeps its type's icon.
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

	/// A zip with a nested tree and a directory entry of its own, written by the same crate the
	/// reader uses so the test carries no fixture file.
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

	/// A tar.zst, written by the packer's own crates.
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

	/// The zip's listing: leaf names, the depth of each path, directories marked, sizes real, and
	/// the total stated from the central directory.
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

	/// A tar.zst lists the same way, through the decoder — and its total is not known up front.
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

	/// A tar.bz2 lists through its decoder (the final ruling's third item): the fixture is a real
	/// two-entry tar.bz2, made once with `bzip2 -9 < tree.tar` and carried as its own bytes — the
	/// decoder here is decode-only, so no fixture is written at test time.
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

	/// A tar.xz lists through its decoder too, built at test time by the same crate that reads it.
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

	/// The single-stream files are one row each (the final ruling's third item): a gzip reads its
	/// inner name and its trailer's size for free; bz2 and xz state neither name nor size, so the
	/// row is the file's own name and the size column says nothing rather than a made-up number.
	#[test]
	fn a_single_stream_is_one_row() {
		let dir = std::env::temp_dir().join(format!("crabhop-single-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");

		// a gzip that carries its own inner name, written by flate2's own builder
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

		// an anonymous gzip: the file's own name less the suffix
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

		// bz2 and xz: one row, the file's own name, and no size claim
		for (name, inner) in [ ("note.txt.bz2", "note.txt"), ("note.txt.xz", "note.txt") ] {
			let at = dir.join(name);
			std::fs::write(&at, b"junk the listing never reads").expect("a file");
			let listing = read(&at).expect("the name alone is enough for one row");
			assert_eq!(listing.items[0].name, inner, "{name}");
			assert_eq!(listing.items[0].size, None, "{name} states no size");
		}

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// The tree rows: both the lead and the name step in with the depth, the lead stays in its own
	/// slot ahead of the name, and a file carries its size while a directory carries none.
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

		// the lead slot steps with the depth: one INDENT per level, never two rows in one place
		let lead_x = |row: &Row| row.indent - advance(row.size) - ICON_BOX * 0.5;
		assert!((lead_x(&rows[1]) - lead_x(&rows[0]) - INDENT).abs() < 0.01, "level two is one step in");
		assert!((lead_x(&rows[3]) - lead_x(&rows[0]) - 2.0 * INDENT).abs() < 0.01, "level three, two steps");
		assert!((lead_x(&rows[4]) - lead_x(&rows[0])).abs() < 0.01, "and a top-level file sits at the base");

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// The fold (P28's long tail): a fresh listing opens its first level and shuts the folders
	/// inside it, a shut folder hides its whole subtree, and the marker says which way it stands.
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

		// the markers: `docs` is open, `deep` is shut
		let leads: Vec<String> =
			tree(&listing.items, &collapsed, &tokens).into_iter().filter_map(|row| row.lead.map(|(g, _)| g)).collect();
		// the entries wear the table's own faces now (P34): `a.txt` the plain sheet, `top.md` markdown's
		assert_eq!(leads, vec![ "▾", "▸", "\u{f0219}", "\u{f48a}" ]);

		// opening `deep` shows its file, and the marker turns over
		assert!(collapsed.remove("docs/deep"));
		assert_eq!(names(&collapsed), vec![ "docs", "deep", "a.txt", "b.bin", "top.md" ]);

		// and shutting `docs` hides everything under it at once
		collapsed.insert("docs".to_owned());
		assert_eq!(names(&collapsed), vec![ "docs", "top.md" ], "a shut parent hides its whole subtree");

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// The archives this window names: every family it can list — the single streams and the tars
	/// of gz/bz2/xz/zst among them — and the ones it knows by name and has no decoder for, which
	/// answer with the format's own name rather than a broken read.
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

		// the familes are told apart by their names: a tar of bz2 is a tar, not a bz2 named tar
		assert_eq!(family("x.tar.bz2"), Some(Family::TarBz2));
		assert_eq!(family("x.tbz"), Some(Family::TarBz2));
		assert_eq!(family("x.tar.xz"), Some(Family::TarXz));
		assert_eq!(family("x.bz2"), Some(Family::Bz2));
		assert_eq!(family("x.xz"), Some(Family::Xz));
		assert_eq!(family("x.gz"), Some(Family::Gz));

		// the file need not exist for the name alone to answer — the read says why it stays shut
		let missing = std::path::PathBuf::from("/tmp/crabhop-n2-never/shut.7z");
		match read(&missing) {
			Err(Fail::Unsupported(format)) => assert_eq!(format, "7z"),
			other => panic!("a 7z is named, not read: {:?}", other.err()),
		}
	}

	/// Junk is not an archive, whatever its name promised.
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