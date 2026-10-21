
use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

pub(crate) fn rows(path: &Path, shown_width: Option<f32>) -> Option<Vec<(String, String)>> {
	use std::os::unix::fs::MetadataExt;

	let meta = std::fs::symlink_metadata(path).ok()?;
	let dir = meta.is_dir();
	let mode = mode_of(&meta);

	let mut rows = vec![
		("类型".to_owned(), kind_words(path, &meta)),
		("大小".to_owned(), size_words(meta.len(), dir)),
	];

	if let Some((report, source)) = crate::preview::picture_facts(path) {
		rows.extend(crate::preview::image_rows(&report, source, shown_width));
	}

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

pub(crate) fn size_words(bytes: u64, dir: bool) -> String {
	if dir {
		return format!("目录（本身 {} B）", bytes);
	}

	format!("{}（精确 {} 字节）", crate::list::size(bytes), bytes)
}

pub(crate) fn owner(uid: u32) -> String {
	passwd().get(&uid).cloned().unwrap_or_else(|| format!("#{uid}"))
}

pub(crate) fn group(gid: u32) -> String {
	group_file().get(&gid).cloned().unwrap_or_else(|| format!("#{gid}"))
}

fn passwd() -> &'static HashMap<u32, String> {
	static USERS: LazyLock<HashMap<u32, String>> = LazyLock::new(|| parse("/etc/passwd", ":", 2, 0));

	&USERS
}

fn group_file() -> &'static HashMap<u32, String> {
	static GROUPS: LazyLock<HashMap<u32, String>> = LazyLock::new(|| parse("/etc/group", ":", 2, 0));

	&GROUPS
}

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

		for value in file.values().chain(sub.values()) {
			assert!(!value.contains("rwx"), "{value}");
		}

		let _ = std::fs::remove_dir_all(&dir);
	}

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

		let pic = rows("pic.png", Some(32.0));
		assert_eq!(pic.get("格式").map(String::as_str), Some("PNG"), "the header names it");
		assert_eq!(pic.get("尺寸").map(String::as_str), Some("64 × 40 像素"));
		assert_eq!(pic.get("色深").map(String::as_str), Some("8 位 RGBA"), "the PNG header states it");
		assert_eq!(pic.get("显示比例").map(String::as_str), Some("50%"), "32 of 64");

		let idle = rows("pic.png", None);
		assert!(!idle.contains_key("显示比例"), "{idle:?}");
		assert!(idle.contains_key("格式") && idle.contains_key("尺寸"), "and the rest stand");

		let note = rows("note.txt", Some(32.0));
		for word in [ "格式", "尺寸", "色深", "显示比例" ] {
			assert!(!note.contains_key(word), "{word} stood for a text file");
		}

		let _ = std::fs::remove_dir_all(&dir);
	}

	#[test]
	fn an_owner_and_a_group_are_named_when_the_system_knows_them() {
		assert_eq!(owner(0), "root", "/etc/passwd names uid 0");
		assert!(owner(4_000_000).starts_with('#'), "an id nobody owns stays a number: {}", owner(4_000_000));
		assert_eq!(group(0), "root", "/etc/group names gid 0");
		assert!(group(4_000_000).starts_with('#'), "and so does a group");
	}
}
