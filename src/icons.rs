
use eframe::egui::Color32;

mod table;

pub(crate) fn lookup(name: &str, dir: bool) -> Option<(&'static str, Color32)> {
	if dir {
		return table::DIRS
			.iter()
			.find(|(what, ..)| what.eq_ignore_ascii_case(name))
			.map(|(.., glyph, fg)| (*glyph, colour(*fg)));
	}

	if let Some((.., glyph, fg)) = table::FILES.iter().find(|(what, ..)| what.eq_ignore_ascii_case(name)) {
		return Some((glyph, colour(*fg)));
	}

	let ext = name.rsplit_once('.').map(|(_, ext)| ext.to_lowercase())?;
	table::EXTS.iter().find(|(what, ..)| *what == ext).map(|(.., glyph, fg)| (*glyph, colour(*fg)))
}

pub(crate) fn colour(fg: u32) -> Color32 {
	Color32::from_rgb(((fg >> 16) & 0xff) as u8, ((fg >> 8) & 0xff) as u8, (fg & 0xff) as u8)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_lookup_reads_the_table_in_order() {
		let glyph = |name: &str, dir: bool| lookup(name, dir).map(|(glyph, _)| glyph);

		assert_eq!(glyph(".gitignore", false), Some("\u{e702}"), "a name the table knows by heart");
		assert_eq!(glyph("Package.json", false), glyph("package.json", false), "and its case does not matter");
		assert_eq!(glyph("license", false), Some("\u{e60a}"), "the lowercase name is the table's own spelling");

		assert_eq!(glyph("Cargo.toml", false), glyph("other.toml", false), "the extension answers for both");
		assert_eq!(glyph("main.rs", false), Some("\u{e68b}"), "Rust's own face");
		assert_eq!(glyph("图.PNG", false), glyph("图.png", false), "and the extension is case-folded");

		assert_eq!(glyph(".config", true), Some("\u{e5fc}"), "the directories have their own rules");
		assert_eq!(glyph("Downloads", true), Some("\u{f498}"));

		assert_eq!(glyph("mystery.zzz", false), None);
		assert_eq!(glyph("no-extension", false), None);
		assert_eq!(glyph("", false), None);
	}

	#[test]
	fn every_glyph_is_a_private_use_character() {
		let mut seen = 0usize;
		for (name, glyph, _) in table::DIRS.iter().chain(table::FILES.iter()).chain(table::EXTS.iter()) {
			let chars: Vec<char> = glyph.chars().collect();
			assert_eq!(chars.len(), 1, "{name}'s glyph is one character: {glyph:?}");
			let at = chars[0] as u32;
			let pua = (0xE000..=0xF8FF).contains(&at) || (0xF0000..=0xFFFFD).contains(&at) || (0x100000..=0x10FFFD).contains(&at);
			assert!(pua, "{name}'s glyph {at:#X} is a private use character");
			seen += 1;
		}

		assert!(seen >= 700, "yazi's preset carries the whole table (P34①): {seen} rules");
	}

	#[test]
	fn the_colours_are_yazis_own_hex() {
		assert_eq!(colour(0xdea584), Color32::from_rgb(0xde, 0xa5, 0x84), "Rust's own tan");
		assert_eq!(lookup("main.rs", false).map(|(_, colour)| colour), Some(colour(0xdea584)));
	}
}
