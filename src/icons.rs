//! The file icons (P34, re-sourced by the user's own ruling): **yazi's preset icon table**,
//! adapted — the table itself is `icons/table.rs`, generated from yazi's `theme-dark.toml`'s
//! `[icon]` section and carrying its own provenance comment. yazi's careful coordination is the
//! reason it was chosen over a table of our own; the licence is MIT (Copyright (c) 2023 - sxyazi)
//! and the repository's NOTICE file carries the attribution the licence asks for.
//!
//! What this module adds is only the reading order and the plumbing: the exact file name first (a
//! `Cargo.toml` is a Cargo file before it is a TOML one), then the extension, then — for a
//! directory — the directory's own name, and the caller's generic face when none of them match.
//! Latte's own table (`theme-light.toml`) waits for the appearance page's batch.

use eframe::egui::Color32;

mod table;

/// A name's own face (P34①'s priority): the exact name, then the extension — and for a directory
/// its own name first, before the folder's generic glyph the caller keeps.
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

/// yazi's own hex, read as a colour: the table's coordination is exactly what was wanted, so its
/// colours are kept as they are — the window's own Catppuccin tokens are not mixed in.
pub(crate) fn colour(fg: u32) -> Color32 {
	Color32::from_rgb(((fg >> 16) & 0xff) as u8, ((fg >> 8) & 0xff) as u8, (fg & 0xff) as u8)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The reading order (P34①): the exact name beats the extension, the extension answers when the
	/// name does not, a directory's own name beats its generic face, and nothing at all is said
	/// about a name the table does not know (the caller's fallback).
	#[test]
	fn the_lookup_reads_the_table_in_order() {
		let glyph = |name: &str, dir: bool| lookup(name, dir).map(|(glyph, _)| glyph);

		// the exact name first: yazi's own file rules
		assert_eq!(glyph(".gitignore", false), Some("\u{e702}"), "a name the table knows by heart");
		assert_eq!(glyph("Package.json", false), glyph("package.json", false), "and its case does not matter");
		assert_eq!(glyph("license", false), Some("\u{e60a}"), "the lowercase name is the table's own spelling");

		// the extension, when the name is not in the files list — and `Cargo.toml` is that case:
		// yazi's table names no Cargo.toml, so it wears the toml face like any other toml
		assert_eq!(glyph("Cargo.toml", false), glyph("other.toml", false), "the extension answers for both");
		assert_eq!(glyph("main.rs", false), Some("\u{e68b}"), "Rust's own face");
		assert_eq!(glyph("图.PNG", false), glyph("图.png", false), "and the extension is case-folded");

		// a directory's own name, before the generic folder the caller keeps
		assert_eq!(glyph(".config", true), Some("\u{e5fc}"), "the directories have their own rules");
		assert_eq!(glyph("Downloads", true), Some("\u{f498}"));

		// and a name the table does not know is the caller's fallback, not a guess
		assert_eq!(glyph("mystery.zzz", false), None);
		assert_eq!(glyph("no-extension", false), None);
		assert_eq!(glyph("", false), None);
	}

	/// Every glyph of every rule is one character of the private use area (P34④) — the ranges the
	/// embedded Nerd Font carries, BMP half and the two supplementary planes.
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

	/// The colour is yazi's own hex, read straight (P34②): the coordination is the point, and no
	/// Catppuccin token stands in for it.
	#[test]
	fn the_colours_are_yazis_own_hex() {
		assert_eq!(colour(0xdea584), Color32::from_rgb(0xde, 0xa5, 0x84), "Rust's own tan");
		assert_eq!(lookup("main.rs", false).map(|(_, colour)| colour), Some(colour(0xdea584)));
	}
}
