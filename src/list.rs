//! The rows: an icon, a name, and the facts about it — and how much of a name fits into a column
//! without ever cutting a wide glyph in half.

use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::Color32;

use crate::folder::Entry;
use crate::keys::{Columns, TimeCol};
use crate::tokens::{FONT_SMALL, ICON_BOX, Tokens};

/// The width of a character in cells: a full-width glyph takes two, everything else one. This is
/// the rule the clipping below stands on — a name is cut between cells, never inside one.
pub(crate) fn cell_width(c: char) -> u32 {
	let c = c as u32;

	match c {
		0x1100..=0x115F | 0x2E80..=0x303E | 0x3041..=0x33FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF
		| 0xA000..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60
		| 0xFFE0..=0xFFE6 | 0x1F300..=0x1FAFF => 2,
		_ => 1,
	}
}

/// As much of a name as `cells` cells hold. The slice always ends on a character boundary, and a
/// two-cell glyph is taken whole or left out.
pub(crate) fn clipped(name: &str, cells: u32) -> &str {
	let mut used = 0;

	for (i, c) in name.char_indices() {
		let width = cell_width(c);
		if used + width > cells {
			return &name[..i];
		}
		used += width;
	}

	name
}

/// A file's icon and the colour of it. A small table on purpose: the kinds a person can tell apart
/// at a glance, not every extension the internet has.
pub(crate) fn icon(entry: &Entry, tokens: &Tokens) -> (&'static str, Color32) {
	icon_for(&entry.name, entry.dir, entry.mode.is_some_and(|mode| mode & 0o111 != 0), tokens)
}

/// The same table, for a caller that has a name and no `Entry` — the preview's placeholder, which
/// asks whether the path is executable and passes the answer in.
pub(crate) fn icon_for(name: &str, dir: bool, exec: bool, tokens: &Tokens) -> (&'static str, Color32) {
	// P34: yazi's own table first — the name, then the extension; a directory's own name before
	// the generic folder. What the table does not name keeps the rules below (and the generic face)
	if let Some((glyph, colour)) = crate::icons::lookup(name, dir) {
		return (glyph, colour);
	}
	if dir {
		return ("\u{f115}", tokens.blue);
	}

	let ext = name.rsplit('.').next().unwrap_or_default().to_lowercase();

	// the filetype rules the design's §3.7 names — images, media, archives, documents, executables
	// — with the baseline's own Catppuccin mapping behind them (`duckfm/src/gui/tokens.rs`, the
	// place the design defers to). Anything the rules do not name keeps the plain file's face.
	let colour = match ext.as_str() {
		"png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "tif" | "tiff" | "avif" | "heic" | "ico" | "jxl" => {
			Some(tokens.yellow)
		}
		"mp3" | "flac" | "wav" | "ogg" | "oga" | "opus" | "m4a" | "aac" | "wma" | "mp4" | "mkv" | "webm" | "mov" | "avi"
		| "m4v" | "mpg" | "mpeg" | "3gp" => Some(tokens.mauve),
		"zip" | "jar" | "cbz" | "apk" | "gz" | "tgz" | "xz" | "txz" | "tar" | "7z" | "rar" | "zst" | "tzst" | "bz2"
		| "bzip2" | "lzma" | "cpio" => Some(tokens.red),
		"pdf" | "doc" | "docx" | "rtf" | "odt" | "xls" | "xlsx" | "ods" | "csv" => Some(tokens.teal),
		_ if exec => Some(tokens.ok),
		_ => None,
	};

	let glyph = match ext.as_str() {
		"png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" | "tif" | "tiff" | "avif" | "heic" | "ico" | "jxl" => "\u{f1c5}",
		"mp3" | "flac" | "wav" | "ogg" | "oga" | "opus" | "m4a" | "aac" | "wma" => "\u{f1c7}",
		"mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "mpg" | "mpeg" | "3gp" => "\u{f1c8}",
		"zip" | "jar" | "cbz" | "apk" | "gz" | "tgz" | "xz" | "txz" | "tar" | "7z" | "rar" | "zst" | "tzst" | "bz2"
		| "bzip2" | "lzma" | "cpio" => "\u{f410}",
		"pdf" | "doc" | "docx" | "rtf" | "odt" => "\u{f1c2}",
		"xls" | "xlsx" | "ods" | "csv" => "\u{f1c3}",
		_ => "\u{f15b}",
	};

	(glyph, colour.unwrap_or(tokens.subtext))
}

/// How many cells of the name are always kept, whatever facts want to share the row: below it
/// the facts are what crowds the name out, and a file list that cannot show its file names is
/// not a file list (P12's own floor).
pub(crate) const NAME_FLOOR: u32 = 10;

/// The facts column a row shows, in its own pixels — or nothing at all. Only the middle column
/// asks for them (P12: the parent list and the preview's directory listing are icon and name);
/// inside it, a narrow column drops the time first and then the size, so the name keeps
/// [`NAME_FLOOR`] cells either way. `advance` is the row's own character advance.
/// The facts a row may wear, already spelled: the middle column asks for one of them by its
/// linemode (P21: `m s`/`m m`/`m b`/`m p`/`m o` and the pair of defaults).
pub(crate) struct Facts {
	pub size:  String,
	pub when:  String,
	pub btime: String,
	pub perms: String,
	pub owner: String,
}

/// The fact columns in their one fixed order (P33⑲⑥): the size first (it always stands), then the
/// time, then the permissions, then the owner — only the ones that are open, and never two
/// columns of one kind.
pub(crate) fn columns_text(columns: Columns, facts: &Facts) -> String {
	let mut parts = vec![ facts.size.clone() ];
	if let Some(time) = columns.time {
		parts.push(match time {
			TimeCol::Mtime => facts.when.clone(),
			TimeCol::Btime => facts.btime.clone(),
		});
	}
	if columns.perms {
		parts.push(facts.perms.clone());
	}
	if columns.owner {
		parts.push(facts.owner.clone());
	}

	parts.join("  ")
}

pub(crate) fn row_facts(show: bool, columns: Columns, facts: &Facts, width: f32, advance: f32) -> Option<String> {
	if !show {
		return None;
	}

	// the secondary face's own advance, the same 0.62 the rest of the window measures with
	let small = FONT_SMALL * 0.62;
	let room = NAME_FLOOR as f32 * advance;
	let fits = |facts: &str| {
		let facts_width: f32 = facts.chars().map(|c| cell_width(c) as f32).sum::<f32>() * small;

		width - ICON_BOX - 3.0 * advance - facts_width - 16.0 >= room
	};

	let mut parts = vec![ facts.size.clone() ];
	if let Some(time) = columns.time {
		parts.push(match time {
			TimeCol::Mtime => facts.when.clone(),
			TimeCol::Btime => facts.btime.clone(),
		});
	}
	if columns.perms {
		parts.push(facts.perms.clone());
	}
	if columns.owner {
		parts.push(facts.owner.clone());
	}

	// out of room: the columns thin from the right — the owner goes first, then the permissions,
	// then the time — and the size is the last one standing
	while !parts.is_empty() {
		let whole = parts.join("  ");
		if fits(&whole) {
			return Some(whole);
		}
		parts.pop();
	}

	None
}

/// `1.2K`, `3.4M` — a size a person reads rather than counts.
pub(crate) fn size(bytes: u64) -> String {
	match bytes {
		b if b >= 1 << 30 => format!("{:.1}G", b as f64 / (1u64 << 30) as f64),
		b if b >= 1 << 20 => format!("{:.1}M", b as f64 / (1u64 << 20) as f64),
		b if b >= 1 << 10 => format!("{:.1}K", b as f64 / (1u64 << 10) as f64),
		b => format!("{b}B"),
	}
}

/// `MM-DD HH:MM`, from the clock the file carries. The civil date is computed here rather than
/// taken from another crate: it is twelve lines of arithmetic anybody can check.
pub(crate) fn when(modified: Option<SystemTime>) -> String {
	let Some((_, month, day, hour, minute, _)) = civil(modified) else {
		return "-".to_owned();
	};

	format!("{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// A time said whole (P23's spot card): the year, the date and the seconds, because a card has the
/// room and a person reading it wants the actual moment. `—` when the time is not there at all.
pub(crate) fn moment(at: Option<SystemTime>) -> String {
	let Some((year, month, day, hour, minute, second)) = civil(at) else {
		return "—".to_owned();
	};

	format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

/// A timestamp as a civil date and time: days since 1970-01-01 through Howard Hinnant's
/// civil-from-days, which is twelve lines of arithmetic anybody can check.
fn civil(at: Option<SystemTime>) -> Option<(i64, i64, i64, u64, u64, u64)> {
	let secs = at.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs())?;

	let days = (secs / 86_400) as i64;
	let (hour, minute, second) = ((secs % 86_400) / 3_600, (secs % 3_600) / 60, secs % 60);

	let z = days + 719_468;
	let era = z.div_euclid(146_097);
	let doe = z.rem_euclid(146_097);
	let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
	let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
	let mp = (5 * doy + 2) / 153;
	let day = doy - (153 * mp + 2) / 5 + 1;
	let month = if mp < 10 { mp + 3 } else { mp - 9 };
	let year = yoe + era * 400 + if month <= 2 { 1 } else { 0 };

	Some((year, month, day, hour, minute, second))
}

#[cfg(test)]
mod tests {
	use std::time::Duration;

	use super::*;
	use crate::keys::Col;

	/// The icon colours, after P34's re-source: yazi's own table answers first, and the filetype
	/// rules stand behind it for what the table does not name — an executable green, a directory
	/// blue, and anything the rules do not name keeps the plain file's face.
	#[test]
	fn the_type_colours_follow_the_filetype_rules() {
		let tokens = Tokens::MOCHA;
		let colour = |name: &str, dir: bool, exec: bool| icon_for(name, dir, exec, &tokens).1;
		let glyph = |name: &str, dir: bool, exec: bool| icon_for(name, dir, exec, &tokens).0;

		assert_eq!(colour("歌.mp3", false, false), crate::icons::colour(0x00afff), "audio: yazi's own blue");
		assert_eq!(colour("片子.mp4", false, false), crate::icons::colour(0xfd971f), "video: yazi's own orange");
		assert_eq!(colour("图.png", false, false), crate::icons::colour(0xa074c4), "images: yazi's own violet");
		assert_eq!(colour("图.avif", false, false), colour("图.png", false, false), "the picture family shares it");
		assert_eq!(colour("档.zip", false, false), crate::icons::colour(0xeca517), "archives: yazi's own amber");
		assert_eq!(colour("档.tar.zst", false, false), colour("档.zip", false, false), "and the archive family too");
		assert_eq!(colour("文.pdf", false, false), crate::icons::colour(0xb30b00), "documents: yazi's own red");
		assert_eq!(colour("文.docx", false, false), crate::icons::colour(0x185abd));
		assert_eq!(colour("本.rs", false, false), crate::icons::colour(0xdea584), "code, yazi's own tan (P34)");
		assert_eq!(colour("谜.zzz", false, false), tokens.subtext, "what the table does not know is plain");
		assert_eq!(colour(".gitignore", false, false), crate::icons::colour(0xf54d27), "a name's own colour");
		assert_eq!(colour("字体.ttf", false, false), crate::icons::colour(0xececec), "and a font's");

		// the table answers before the exec rule: a `.sh` wears the shell's own face even when it
		// runs — and a name the table does not know keeps the rule, an executable green
		assert_eq!(colour("脚本.sh", false, true), crate::icons::colour(0x4d5a5e), "the table first");
		assert_eq!(colour("bin/thing", false, true), tokens.ok, "no extension, so the rule: executable green");
		assert_eq!(colour("图.png", false, true), crate::icons::colour(0xa074c4), "a picture keeps the picture's face");
		assert_eq!(colour("目录", true, true), tokens.blue, "a directory is a directory");
		assert_eq!(colour("下载", true, false), tokens.blue, "and one the table does not name keeps the folder's face");

		// and the glyphs are the table's own faces (P34): the media families keep theirs, and the
		// code's is the language's now — a Rust file wears the Rust glyph, not a generic sheet
		assert_eq!(glyph("歌.mp3", false, false), "\u{f001}", "audio: the table's own notes");
		assert_eq!(glyph("片子.mp4", false, false), "\u{e69f}", "video: the table's own film");
		assert_eq!(glyph("本.rs", false, false), "\u{e68b}", "code: the language's own face, yazi's own table");
		assert_eq!(glyph("本.py", false, false), "\u{e606}", "and another language, another face");
		assert_eq!(glyph("Cargo.toml", false, false), glyph("别的.toml", false, false), "the extension answers for both");
		assert_eq!(glyph(".gitignore", false, false), "\u{e702}", "a name yazi knows by heart");
		assert_eq!(glyph("谜.zzz", false, false), "\u{f15b}", "and what the table does not know keeps the generic face");
	}






	/// A name is clipped between cells: a wide glyph is taken whole or not at all, which is the one
	/// thing a list of mixed Chinese and ASCII names cannot get wrong.
	#[test]
	fn a_name_is_clipped_between_cells_never_inside_one() {
		assert_eq!(cell_width('a'), 1);
		assert_eq!(cell_width('陶'), 2);

		assert_eq!(clipped("abcdef", 3), "abc");
		assert_eq!(clipped("陶海水", 4), "陶海", "two cells each, so four cells is two of them");
		assert_eq!(clipped("陶海水", 5), "陶海", "and the fifth cell is not half a glyph");
		assert_eq!(clipped("a陶b", 3), "a陶", "a narrow one first, then the wide one still fits");
		assert_eq!(clipped("a陶b", 2), "a", "but not when it would be cut");
		assert_eq!(clipped("abc", 99), "abc", "a name that fits is left alone");
	}

	#[test]
	fn sizes_read_the_way_people_read_them() {
		assert_eq!(size(12), "12B");
		assert_eq!(size(2_048), "2.0K");
		assert_eq!(size(5 * 1_048_576), "5.0M");
		assert_eq!(size(3 * 1_073_741_824), "3.0G");
	}

	/// The facts column's own ladder (P12): only the middle column asks for facts at all; a wide
	/// one shows the size and the time, a narrower one drops the time first, and a column too
	/// narrow for either keeps its names and shows nothing. The floor is the name's own room.
	#[test]
	fn the_facts_ladder_gives_the_name_its_room() {
		let advance = 16.0 * 0.62;
		let facts = |size: &str, when: &str| Facts {
			size:  size.to_owned(),
			when:  when.to_owned(),
			btime: "10-19 09:03".to_owned(),
			perms: "rwxr-xr-x".to_owned(),
			owner: "LinXuan".to_owned(),
		};

		let set = |time: Option<TimeCol>, perms: bool, owner: bool| Columns { time, perms, owner };

		// the left and right columns: no facts, whatever the width
		assert_eq!(row_facts(false, Columns::default(), &facts("1.2K", "10-19 09:03"), 640.0, advance), None);
		assert_eq!(row_facts(false, set(None, true, true), &facts("1.2K", "10-19 09:03"), 2000.0, advance), None);

		// the middle column at a normal width: the default set, size and modification time
		assert_eq!(
			row_facts(true, Columns::default(), &facts("1.2K", "10-19 09:03"), 640.0, advance).as_deref(),
			Some("1.2K  10-19 09:03")
		);
		// narrower: the columns thin from the right, the size is the last one standing
		assert_eq!(row_facts(true, Columns::default(), &facts("1.2K", "10-19 09:03"), 260.0, advance).as_deref(), Some("1.2K"));
		// too narrow for even that: no facts at all
		assert_eq!(row_facts(true, Columns::default(), &facts("1.2K", "10-19 09:03"), 170.0, advance), None);

		// the fixed order (P33⑲⑥): size, time, permissions, owner — only the open ones
		assert_eq!(
			row_facts(true, set(Some(TimeCol::Btime), true, true), &facts("1.2K", "10-19 09:03"), 640.0, advance).as_deref(),
			Some("1.2K  10-19 09:03  rwxr-xr-x  LinXuan"),
			"the creation time where the modification time stands, and the two switches behind it"
		);
		assert_eq!(
			row_facts(true, set(None, true, false), &facts("1.2K", "10-19 09:03"), 640.0, advance).as_deref(),
			Some("1.2K  rwxr-xr-x"),
			"with no time column the permissions come straight after the size"
		);
		assert_eq!(
			row_facts(true, set(Some(TimeCol::Mtime), false, true), &facts("1.2K", "10-19 09:03"), 640.0, advance).as_deref(),
			Some("1.2K  10-19 09:03  LinXuan"),
			"and the owner keeps its own place with the permissions away"
		);

		// the set itself (P33⑲②③⑤): toggles that add and take away, the exclusive time pair, the
		// one-key reset
		let mut columns = Columns::default();
		assert_eq!(columns, set(Some(TimeCol::Mtime), false, false), "the default set is size and modification time");
		columns.toggle(Col::Owner);
		assert_eq!(columns, set(Some(TimeCol::Mtime), false, true), "m o adds the owner and touches nothing else");
		columns.toggle(Col::Permissions);
		assert_eq!(columns, set(Some(TimeCol::Mtime), true, true), "m p adds the permissions — three columns now");
		columns.toggle(Col::Permissions);
		assert_eq!(columns, set(Some(TimeCol::Mtime), false, true), "and m p again takes only its own away");
		columns.toggle(Col::Btime);
		assert_eq!(columns, set(Some(TimeCol::Btime), false, true), "m b replaces the modification time, never joins it");
		columns.toggle(Col::Mtime);
		assert_eq!(columns, set(Some(TimeCol::Mtime), false, true), "and m m replaces it right back");
		columns.toggle(Col::Mtime);
		assert_eq!(columns, set(None, false, true), "a second m m closes the time column");
		columns.show(Col::Mtime);
		assert_eq!(columns, set(Some(TimeCol::Mtime), false, true), "show makes sure, idempotent (the sort family's own verb)");
		columns.show(Col::Mtime);
		assert_eq!(columns, set(Some(TimeCol::Mtime), false, true), "and again does not take it away");
		columns.reset();
		assert_eq!(columns, Columns::default(), "m n is the one-key reset");
	}

	/// The full form the spot card reads (P23): the year is there, the seconds are there, and a
	/// time the file does not carry is a dash rather than a lie.
	#[test]
	fn a_moment_is_said_whole() {
		// the clock the app reads is UTC, the same one `when` has always shown (TZ handling is a
		// recorded gap): 1_700_000_000s is 2023-11-14 22:13:20Z
		let at = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
		assert_eq!(moment(Some(at)), "2023-11-14 22:13:20");
		assert_eq!(moment(None), "—");
		assert_eq!(when(Some(at)), "11-14 22:13", "the short form the list uses is unchanged");
	}

	#[test]
	fn a_time_without_a_clock_says_so() {
		assert_eq!(when(None), "-");
	}
}
