
use eframe::egui::Color32;

mod standard;
mod table;

// The user's standard layer wins; yazi's preset table fills the long tail;
// the standard conds catch whatever no name rule claimed.
pub(crate) fn lookup(name: &str, dir: bool, exec: bool, link: bool, orphan: bool) -> Option<(&'static str, Color32)> {
	let hidden = name.starts_with('.');

	let hit = if dir {
		standard::DIRS
			.iter()
			.chain(table::DIRS.iter())
			.find(|(what, ..)| what.eq_ignore_ascii_case(name))
			.map(|(.., glyph, fg)| (*glyph, *fg))
			.or_else(|| standard::cond(dir, hidden, orphan, link, exec))
	} else {
		standard::FILES
			.iter()
			.chain(table::FILES.iter())
			.find(|(what, ..)| what.eq_ignore_ascii_case(name))
			.map(|(.., glyph, fg)| (*glyph, *fg))
			.or_else(|| {
				let ext = name.rsplit_once('.').map(|(_, ext)| ext.to_lowercase())?;
				standard::EXTS
					.iter()
					.chain(table::EXTS.iter())
					.find(|(what, ..)| *what == ext)
					.map(|(.., glyph, fg)| (*glyph, *fg))
			})
			.or_else(|| standard::cond(dir, hidden, orphan, link, exec))
	};

	hit.map(|(glyph, fg)| (glyph, colour(fg)))
}

pub(crate) fn colour(fg: u32) -> Color32 {
	Color32::from_rgb(((fg >> 16) & 0xff) as u8, ((fg >> 8) & 0xff) as u8, (fg & 0xff) as u8)
}

#[cfg(test)]
mod tests {
	use super::*;

	fn glyph(name: &str, dir: bool) -> Option<&'static str> {
		lookup(name, dir, false, false, false).map(|(glyph, _)| glyph)
	}

	fn paint(name: &str, dir: bool) -> Option<Color32> {
		lookup(name, dir, false, false, false).map(|(_, colour)| colour)
	}

	#[test]
	fn the_lookup_reads_the_tables_in_order() {
		assert_eq!(glyph(".gitignore", false), Some("\u{e702}"), "a name the table knows by heart");
		assert_eq!(glyph("Package.json", false), glyph("package.json", false), "and its case does not matter");
		assert_eq!(glyph("license", false), Some("\u{e60a}"), "the lowercase name is the table's own spelling");

		assert_eq!(glyph("Cargo.toml", false), Some("\u{e615}"), "the standard layer answers for the extension");
		assert_eq!(glyph("main.rs", false), Some("\u{e68b}"), "Rust's own face");
		assert_eq!(glyph("图.PNG", false), glyph("图.png", false), "and the extension is case-folded");

		assert_eq!(glyph(".config", true), Some("\u{e5fc}"), "the directories have their own rules");
		assert_eq!(glyph("Downloads", true), Some("\u{f01da}"), "the standard's own downloads folder");

		assert_eq!(glyph("mystery.zzz", false), None);
		assert_eq!(glyph("no-extension", false), None);
		assert_eq!(glyph("", false), None);
	}

	#[test]
	fn every_glyph_is_a_private_use_character() {
		let mut seen = 0usize;
		let all = standard::DIRS
			.iter()
			.chain(standard::FILES.iter())
			.chain(standard::EXTS.iter())
			.chain(table::DIRS.iter())
			.chain(table::FILES.iter())
			.chain(table::EXTS.iter());
		for (name, glyph, _) in all {
			let chars: Vec<char> = glyph.chars().collect();
			assert_eq!(chars.len(), 1, "{name}'s glyph is one character: {glyph:?}");
			let at = chars[0] as u32;
			let pua = (0xE000..=0xF8FF).contains(&at) || (0xF0000..=0xFFFFD).contains(&at) || (0x100000..=0x10FFFD).contains(&at);
			assert!(pua, "{name}'s glyph {at:#X} is a private use character");
			seen += 1;
		}

		assert!(seen >= 700, "the two tables carry the whole set: {seen} rules");
	}

	#[test]
	fn the_standard_layer_sits_on_yazis_table() {
		assert_eq!(paint("main.rs", false), Some(colour(0x99ccfa)), "the standard's blue answers for Rust");
		assert_eq!(paint("字体.ttf", false), Some(colour(0xececec)), "and yazi's own table still fills the gaps");
		assert_eq!(colour(0xdea584), Color32::from_rgb(0xde, 0xa5, 0x84), "the hex becomes the colour");
	}

	#[test]
	fn the_standard_conds_catch_what_the_names_miss() {
		let look = |name: &str, dir: bool, exec: bool, link: bool, orphan: bool| lookup(name, dir, exec, link, orphan);

		assert_eq!(look("thing", false, false, true, true).map(|(g, _)| g), Some("\u{f030c}"), "a broken link wears the orphan's face");
		assert_eq!(look("thing", false, false, true, false).map(|(g, _)| g), Some("\u{f0337}"), "a link that stands wears the link's face");
		assert_eq!(look("thing", false, true, false, false).map(|(g, _)| g), Some("\u{f01a7}"), "an executable wears the exec's face");
		assert_eq!(look(".thing", false, false, false, false).map(|(g, _)| g), Some("\u{f0219}"), "and a hidden file the hidden face");
		assert_eq!(look(".prog", false, true, false, false).map(|(_, c)| c), Some(colour(0xb8c8da)), "the exec face stands ahead of the hidden one");

		assert_eq!(look("whatever", true, false, false, false).map(|(_, c)| c), Some(colour(0x99ccfa)), "every directory gets the folder's blue");
		assert_eq!(look(".whatever", true, false, false, false).map(|(_, c)| c), Some(colour(0xfafdff)), "and a hidden one its own shade");

		assert_eq!(look(".notes.txt", false, false, false, false).map(|(_, c)| c), Some(colour(0xe0e2e8)), "a known extension still outranks the hidden cond");
	}
}
