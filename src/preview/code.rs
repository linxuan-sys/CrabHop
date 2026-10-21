//! Syntax highlighting for a preview's certainty: syntect, a Catppuccin Mocha theme in the same
//! palette the window's tokens use (§3.3), and only the lines a scroll actually shows.
//!
//! The backend is `default-fancy` — pure Rust. The workspace's own syntect is built with
//! `regex-onig`, an Oniguruma C library this project bans; `crabhop/Cargo.toml` says so in
//! writing and this crate takes syntect directly instead of through the workspace.
//!
//! Highlighting walks a file's lines in order, because a rich syntax cannot be understood line by
//! line in isolation; it stops at the last line drawn, and a scroll that only asks for more of the
//! same file continues from where it stopped. Nothing ahead of the viewport is ever highlighted.

use std::sync::LazyLock;

use syntect::{easy::HighlightLines, highlighting::Theme, parsing::SyntaxSet};

/// The spec's own ceilings (§3.3): a file past this is shown as its first 2000 lines, and a line
/// past 200 columns is clipped rather than wrapped.
pub(crate) const TRUNCATE_BYTES: u64 = 1024 * 1024;
pub(crate) const TRUNCATE_LINES: usize = 2000;

/// Nothing walks further than this, however far a scroll goes: past it the file is shown plain,
/// and the memory a highlight state holds stays bounded.
pub(crate) const MAX_HIGHLIGHTED: usize = 20_000;

/// The Catppuccin Mocha palette, written as the syntax theme itself (§3.3: the code must not be
/// "Mocha UI, some other theme's code"). A compact map of the scopes a preview meets, in the same
/// hex values the tokens are.
const MOCHA: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>name</key><string>Catppuccin Mocha</string><key>settings</key><array>
<dict><key>settings</key><dict><key>foreground</key><string>#CDD6F4</string><key>background</key><string>#1E1E2E</string></dict></dict>
<dict><key>scope</key><string>comment</string><key>settings</key><dict><key>foreground</key><string>#6C7086</string><key>fontStyle</key><string>italic</string></dict></dict>
<dict><key>scope</key><string>string</string><key>settings</key><dict><key>foreground</key><string>#A6E3A1</string></dict></dict>
<dict><key>scope</key><string>constant, constant.numeric</string><key>settings</key><dict><key>foreground</key><string>#FAB387</string></dict></dict>
<dict><key>scope</key><string>entity.name.type, entity.name.class, entity.name.struct, support.type, support.class</string><key>settings</key><dict><key>foreground</key><string>#F9E2AF</string></dict></dict>
<dict><key>scope</key><string>entity.name.function, support.function, meta.function-call</string><key>settings</key><dict><key>foreground</key><string>#89B4FA</string></dict></dict>
<dict><key>scope</key><string>keyword.operator</string><key>settings</key><dict><key>foreground</key><string>#89DCEB</string></dict></dict>
<dict><key>scope</key><string>keyword, storage, storage.modifier, storage.type</string><key>settings</key><dict><key>foreground</key><string>#CBA6F7</string></dict></dict>
<dict><key>scope</key><string>entity.name.tag</string><key>settings</key><dict><key>foreground</key><string>#CBA6F7</string></dict></dict>
<dict><key>scope</key><string>entity.other.attribute-name</string><key>settings</key><dict><key>foreground</key><string>#F9E2AF</string></dict></dict>
<dict><key>scope</key><string>variable.parameter</string><key>settings</key><dict><key>foreground</key><string>#EBA0AC</string></dict></dict>
<dict><key>scope</key><string>variable, meta.object-literal.key</string><key>settings</key><dict><key>foreground</key><string>#CDD6F4</string></dict></dict>
<dict><key>scope</key><string>constant.language</string><key>settings</key><dict><key>foreground</key><string>#CBA6F7</string></dict></dict>
<dict><key>scope</key><string>punctuation, meta.brace</string><key>settings</key><dict><key>foreground</key><string>#9399B2</string></dict></dict>
</array></dict></plist>
"##;

/// The syntax set and the theme, built once: loading them is tens of milliseconds, and a preview
/// asks for them on every hover.
static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME: LazyLock<Theme> = LazyLock::new(|| {
	let mut reader = std::io::Cursor::new(MOCHA);

	syntect::highlighting::ThemeSet::load_from_reader(&mut reader)
		.unwrap_or_else(|e| panic!("the embedded theme is not a theme: {e}"))
});

/// The syntax an extension is highlighted with, or plain text: an unknown extension is not an
/// error, it is a text file.
fn syntax(ext: &str) -> &'static syntect::parsing::SyntaxReference {
	SYNTAXES
		.find_syntax_by_extension(ext)
		.unwrap_or_else(|| SYNTAXES.find_syntax_plain_text())
}

/// The per-file highlighting state: the parse state between lines, and how far its walk has gone.
/// It lives in the pane for the file on screen and is thrown away when the file changes.
pub(crate) struct Syntax {
	highlighter: HighlightLines<'static>,
	upto:        usize,
}

impl Syntax {
	pub(crate) fn new(ext: &str) -> Self {
		Self { highlighter: HighlightLines::new(syntax(ext), &THEME), upto: 0 }
	}

	/// The highlighting of a fenced code block, named the way fences name languages (`rust`,
	/// `python`, `toml`): an unknown name is plain text, not an error.
	pub(crate) fn for_fence(lang: &str) -> Self {
		let syntax = SYNTAXES
			.find_syntax_by_token(lang)
			.unwrap_or_else(|| SYNTAXES.find_syntax_plain_text());

		Self { highlighter: HighlightLines::new(syntax, &THEME), upto: 0 }
	}

	/// How many lines have been highlighted so far (the tests read the walk's own count).
	#[cfg(test)]
	pub(crate) fn upto(&self) -> usize { self.upto }

	/// Highlight the next line of the walk and hand back its runs, as byte ranges with a colour.
	/// Called in order, once per line — a scroll back up re-reads what the caller already kept.
	pub(crate) fn next(&mut self, line: &str) -> Vec<(std::ops::Range<usize>, eframe::egui::Color32)> {
		use eframe::egui::Color32;

		self.upto += 1;
		let Ok(chunks) = self.highlighter.highlight_line(line, &SYNTAXES) else {
			return Vec::new();
		};

		let mut runs: Vec<(std::ops::Range<usize>, Color32)> = Vec::new();
		let mut at = 0;

		for (style, text) in chunks {
			let colour =
				Color32::from_rgb(style.foreground.r, style.foreground.g, style.foreground.b);
			let end = at + text.len();

			if let Some(last) = runs.last_mut()
				&& last.1 == colour
			{
				last.0.end = end;
			} else {
				runs.push((at..end, colour));
			}

			at = end;
		}

		runs
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The theme is the one it says it is: loading is the test, and a `fn` in Rust has to come out
	/// with a keyword's own colour — mauve, not white on some other theme.
	#[test]
	fn the_embedded_theme_loads_and_a_keyword_gets_its_colour() {
		let mut syntax = Syntax::new("rs");
		let runs = syntax.next("fn main() {}");

		let keyword = runs.first().expect("a keyword run");
		assert_eq!(keyword.0, 0..2, "the `fn` is the first run");
		assert_eq!(keyword.1, eframe::egui::Color32::from_rgb(0xcb, 0xa6, 0xf7), "mauve, the token's own");
		assert_eq!(syntax.upto(), 1, "and the walk has moved on");
	}

	/// An unknown extension is a text file, not a crash: plain text highlights without failing.
	#[test]
	fn an_unknown_extension_is_plain_text() {
		let mut syntax = Syntax::new("nonexistent-ext");

		assert_eq!(syntax.next("just words"), syntax.next("just words"), "plain text has no runs of its own");
		assert_eq!(syntax.upto(), 2);
	}
}