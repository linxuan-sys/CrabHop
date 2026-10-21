//! The spot card's own words (P23): what a file is, said so a person who has never seen a
//! permission bit can read it. The card's rows are built here, away from the drawing, so the
//! mapping — the three classes of permission as phrases, a link's own target, the owner and the
//! group by name — can be read and tested without a window.
//!
//! No `rwx` spelling reaches this card: every class is a Chinese phrase ("可读、可写、可执行",
//! "可列出、可新建删除、可进入", "只读", "无权限").

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

/// Every row the card shows, in the order it shows them. `None` for a path nothing can be read
/// from at all. `shown_width` is the physical width the preview column shows this file's own
/// pixels at — the picture's own ratio when it is the picture on screen (P28②), the column's
/// fit-width for a video (P29) — and the ratio rows step out when the column has not said.
pub(crate) fn rows(path: &Path, shown_width: Option<f32>) -> Option<Vec<(String, String)>> {
	use std::os::unix::fs::MetadataExt;

	let meta = std::fs::symlink_metadata(path).ok()?;
	let dir = meta.is_dir();
	let mode = mode_of(&meta);

	let mut rows = vec![
		("类型".to_owned(), kind_words(path, &meta)),
		("大小".to_owned(), size_words(meta.len(), dir)),
	];

	// a picture's own section (P28② revised): its header read once, no decode — the format, the
	// pixels, the ratio the column is showing it at, the colour depth; the info block that stood
	// under the picture in the column moved here, and a row the header cannot answer is simply
	// not there (format and colour depth are the header's own words where it has them)
	if let Some((report, source)) = crate::preview::picture_facts(path) {
		rows.extend(crate::preview::image_rows(&report, source, shown_width));
	}

	// a video's own section (P29): the container, the codec, the pixels, the clock, the rate —
	// ffprobe's answer where the machine has it, the container's header where it does not
	let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
	if crate::preview::video::is_video(name)
		&& let Some(media) = crate::preview::video::media(path, meta.modified().ok())
	{
		rows.extend(media.rows(shown_width));
	}

	let owner = owner(meta.uid());
	let group = group(meta.gid());
	rows.push(("所有者".to_owned(), owner));
	rows.push(("所属组".to_owned(), group));

	let words = perm_words(mode, dir);
	rows.push(("所有者权限".to_owned(), words[0].clone()));
	rows.push(("组权限".to_owned(), words[1].clone()));
	rows.push(("其他权限".to_owned(), words[2].clone()));

	rows.push(("链接数".to_owned(), meta.nlink().to_string()));
	rows.push(("创建时间".to_owned(), crate::list::moment(meta.created().ok())));
	rows.push(("修改时间".to_owned(), crate::list::moment(meta.modified().ok())));
	rows.push(("访问时间".to_owned(), crate::list::moment(meta.accessed().ok())));
	rows.push(("完整路径".to_owned(), path.display().to_string()));

	Some(rows)
}

/// What the thing is, in the words the table asks for: a file, a directory, a symbolic link with
/// the path it points at, a socket, a device — and the odd one out said plainly. Pure enough to
/// test: it reads the filesystem, and nothing else.
pub(crate) fn kind_words(path: &Path, meta: &std::fs::Metadata) -> String {
	use std::os::unix::fs::FileTypeExt;

	let kind = meta.file_type();
	if kind.is_symlink() {
		let parent = path.parent().unwrap_or(Path::new("/"));
		return match std::fs::read_link(path) {
			Ok(target) => {
				let target = crate::app::followed(parent, &target);
				let tail = if target.symlink_metadata().is_ok() { "" } else { "（目标不存在）" };
				format!("符号链接 → {}{tail}", target.display())
			}
			Err(e) => format!("符号链接（目标读不到：{e}）"),
		};
	}
	if kind.is_dir() {
		return "目录".to_owned();
	}
	if kind.is_file() {
		return "文件".to_owned();
	}
	if kind.is_socket() {
		return "套接字".to_owned();
	}
	if kind.is_char_device() {
		return "字符设备".to_owned();
	}
	if kind.is_block_device() {
		return "块设备".to_owned();
	}
	if kind.is_fifo() {
		return "命名管道".to_owned();
	}

	"其他".to_owned()
}

/// The three classes of permission, as phrases: `[owner, group, other]`. A file reads 可读 /
/// 可写 / 可执行, a directory 可列出 / 可新建删除 / 可进入 — the same bit meaning as a person
/// means it. Nothing at all is 无权限, and read-and-nothing-else is 只读.
pub(crate) fn perm_words(mode: u32, dir: bool) -> [String; 3] {
	let bits = mode & 0o777;
	[ class(bits >> 6, dir), class(bits >> 3, dir), class(bits, dir) ]
}

fn class(bits: u32, dir: bool) -> String {
	let (read, write, run) = (bits & 4 != 0, bits & 2 != 0, bits & 1 != 0);

	if !(read || write || run) {
		return "无权限".to_owned();
	}
	if read && !write && !run {
		return "只读".to_owned();
	}

	let mut words: Vec<&str> = Vec::new();
	if read {
		words.push(if dir { "可列出" } else { "可读" });
	}
	if write {
		words.push(if dir { "可新建删除" } else { "可写" });
	}
	if run {
		words.push(if dir { "可进入" } else { "可执行" });
	}

	words.join("、")
}

/// A size a person reads, with the exact byte count beside it: a directory's own entry size is
/// not what is inside it, and the card says so rather than printing a number that means nothing.
pub(crate) fn size_words(bytes: u64, dir: bool) -> String {
	if dir {
		return format!("目录（本身 {} B）", bytes);
	}

	format!("{}（精确 {} 字节）", crate::list::size(bytes), bytes)
}

/// A user by the name the system knows, the number only when there is no name for it.
pub(crate) fn owner(uid: u32) -> String {
	passwd().get(&uid).cloned().unwrap_or_else(|| format!("#{uid}"))
}

/// A group by the same rule.
pub(crate) fn group(gid: u32) -> String {
	group_file().get(&gid).cloned().unwrap_or_else(|| format!("#{gid}"))
}

/// `/etc/passwd` read once for the whole window: uid → user name.
fn passwd() -> &'static HashMap<u32, String> {
	static USERS: LazyLock<HashMap<u32, String>> = LazyLock::new(|| parse("/etc/passwd", ":", 2, 0));

	&USERS
}

/// `/etc/group` read once: gid → group name.
fn group_file() -> &'static HashMap<u32, String> {
	static GROUPS: LazyLock<HashMap<u32, String>> = LazyLock::new(|| parse("/etc/group", ":", 2, 0));

	&GROUPS
}

/// One of those two files: `name:x:id:…`, the name first and the id third.
fn parse(file: &str, separator: &str, id_at: usize, name_at: usize) -> HashMap<u32, String> {
	let Ok(text) = std::fs::read_to_string(file) else {
		return HashMap::new();
	};

	text.lines()
		.filter_map(|line| {
			let parts: Vec<&str> = line.split(separator).collect();
			let id = parts.get(id_at)?.parse().ok()?;
			let name = parts.get(name_at)?.trim();
			(!name.is_empty()).then(|| (id, name.to_owned()))
		})
		.collect()
}

/// The unix mode bits, on a platform that has them.
fn mode_of(meta: &std::fs::Metadata) -> u32 {
	#[cfg(unix)]
	{
		use std::os::unix::fs::MetadataExt;
		return meta.mode();
	}

	#[allow(unreachable_code)]
	0
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The eight permission combinations, per class, for a file and for a directory (P23): every
	/// class is a Chinese phrase, and no `rwx` spelling appears anywhere in the mapping.
	#[test]
	fn the_eight_combinations_read_as_chinese_phrases() {
		let file = |bits: u32| perm_words(bits << 6 | bits << 3 | bits, false);
		let dir = |bits: u32| perm_words(bits << 6 | bits << 3 | bits, true);

		for (bits, file_words, dir_words) in [
			(0o0, "无权限", "无权限"),
			(0o1, "可执行", "可进入"),
			(0o2, "可写", "可新建删除"),
			(0o3, "可写、可执行", "可新建删除、可进入"),
			(0o4, "只读", "只读"),
			(0o5, "可读、可执行", "可列出、可进入"),
			(0o6, "可读、可写", "可列出、可新建删除"),
			(0o7, "可读、可写、可执行", "可列出、可新建删除、可进入"),
		] {
			assert_eq!(file(bits), [ file_words.to_owned(), file_words.to_owned(), file_words.to_owned() ], "{bits:o} as a file");
			assert_eq!(dir(bits), [ dir_words.to_owned(), dir_words.to_owned(), dir_words.to_owned() ], "{bits:o} as a directory");
		}

		// the classes are read apart: `0644` is the common one
		assert_eq!(
			perm_words(0o644, false),
			[ "可读、可写".to_owned(), "只读".to_owned(), "只读".to_owned() ],
			"a file's owner writes, everyone else only reads"
		);
		assert_eq!(
			perm_words(0o755, true),
			[ "可列出、可新建删除、可进入".to_owned(), "可列出、可进入".to_owned(), "可列出、可进入".to_owned() ],
			"a directory the world may walk and read but only the owner may change"
		);

		// and the words never spell a bit
		for bits in 0..=0o777 {
			for dir in [ false, true ] {
				for words in perm_words(bits, dir) {
					for forbidden in [ "r", "w", "x" ] {
						assert!(!words.contains(forbidden), "{words} spells {forbidden}");
					}
				}
			}
		}
	}

	/// The card's rows say a symlink with the path it points at, a directory as a directory, and a
	/// file as a file — and the permissions in them are the words, not the bits.
	#[test]
	fn the_rows_say_what_a_thing_is_in_words() {
		let dir = std::env::temp_dir().join(format!("crabhop-spot-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(dir.join("sub")).expect("a directory");
		std::fs::write(dir.join("note.txt"), b"hello").expect("a file");
		std::os::unix::fs::symlink(dir.join("note.txt"), dir.join("link.txt")).expect("a link");
		std::os::unix::fs::symlink(dir.join("nowhere"), dir.join("broken.txt")).expect("a dangling link");

		let rows = |name: &str| -> HashMap<String, String> {
			rows(&dir.join(name), None).expect("rows").into_iter().collect()
		};

		let file = rows("note.txt");
		assert_eq!(file.get("类型").map(String::as_str), Some("文件"));
		assert!(file.get("大小").is_some_and(|size| size.contains("精确 5 字节")), "{:?}", file.get("大小"));
		assert!(file.get("所有者").is_some_and(|owner| !owner.is_empty()), "the owner has a name");
		assert!(file.contains_key("完整路径"), "the whole path is there");
		assert!(file.contains_key("链接数"), "and the link count");

		let link = rows("link.txt");
		assert_eq!(link.get("类型").map(String::as_str), Some(format!("符号链接 → {}", dir.join("note.txt").display()).as_str()));

		let broken = rows("broken.txt");
		assert!(
			broken.get("类型").is_some_and(|kind| kind.ends_with("（目标不存在）")),
			"{:?}",
			broken.get("类型")
		);

		let sub = rows("sub");
		assert_eq!(sub.get("类型").map(String::as_str), Some("目录"));
		assert!(sub.get("大小").is_some_and(|size| size.starts_with("目录")), "{:?}", sub.get("大小"));

		// nothing the card says spells a permission as its bits
		for value in file.values().chain(sub.values()) {
			assert!(!value.contains("rwx"), "{value}");
		}

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// The card's image section (P28② revised): a picture gets its four words — from the header,
	/// no decode — and a file that is not a picture gets none of them; the ratio row is there only
	/// when the column is showing that very file.
	#[test]
	fn a_picture_gets_its_own_section_and_a_file_gets_none() {
		let dir = std::env::temp_dir().join(format!("crabhop-spot-pic-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");
		std::fs::write(dir.join("note.txt"), b"hello").expect("a file");
		image::RgbaImage::from_pixel(64, 40, image::Rgba([ 9, 9, 9, 255 ]))
			.save(dir.join("pic.png"))
			.expect("the fixture encodes");

		let rows = |name: &str, shown: Option<f32>| -> HashMap<String, String> {
			rows(&dir.join(name), shown).expect("rows").into_iter().collect()
		};

		// the picture, with the column showing it at half its width
		let pic = rows("pic.png", Some(32.0));
		assert_eq!(pic.get("格式").map(String::as_str), Some("PNG"), "the header names it");
		assert_eq!(pic.get("尺寸").map(String::as_str), Some("64 × 40 像素"));
		assert_eq!(pic.get("色深").map(String::as_str), Some("8 位 RGBA"), "the PNG header states it");
		assert_eq!(pic.get("显示比例").map(String::as_str), Some("50%"), "32 of 64");

		// nothing is showing it: the ratio row is not there, the picture's other words are
		let idle = rows("pic.png", None);
		assert!(!idle.contains_key("显示比例"), "{idle:?}");
		assert!(idle.contains_key("格式") && idle.contains_key("尺寸"), "and the rest stand");

		// a text file: no image section at all — not one of its four words
		let note = rows("note.txt", Some(32.0));
		for word in [ "格式", "尺寸", "色深", "显示比例" ] {
			assert!(!note.contains_key(word), "{word} stood for a text file");
		}

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// The names behind the numbers: `root` is 0 on any machine this runs on, and an id with no
	/// name is said as the number rather than as an empty word.
	#[test]
	fn an_owner_and_a_group_are_named_when_the_system_knows_them() {
		assert_eq!(owner(0), "root", "/etc/passwd names uid 0");
		assert!(owner(4_000_000).starts_with('#'), "an id nobody owns stays a number: {}", owner(4_000_000));
		assert_eq!(group(0), "root", "/etc/group names gid 0");
		assert!(group(4_000_000).starts_with('#'), "and so does a group");
	}
}