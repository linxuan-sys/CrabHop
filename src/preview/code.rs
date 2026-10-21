
use std::sync::LazyLock;

use syntect::{easy::HighlightLines, highlighting::Theme, parsing::SyntaxSet};

pub(crate) const TRUNCATE_BYTES: u64 = 1024 * 1024;
pub(crate) const TRUNCATE_LINES: usize = 2000;

pub(crate) const MAX_HIGHLIGHTED: usize = 20_000;

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

static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(SyntaxSet::load_defaults_newlines);
static THEME: LazyLock<Theme> = LazyLock::new(|| {
	let mut reader = std::io::Cursor::new(MOCHA);

	syntect::highlighting::ThemeSet::load_from_reader(&mut reader)
		.unwrap_or_else(|e| panic!("the embedded theme is not a theme: {e}"))
});

fn syntax(ext: &str) -> &'static syntect::parsing::SyntaxReference {
	SYNTAXES
		.find_syntax_by_extension(ext)
		.unwrap_or_else(|| SYNTAXES.find_syntax_plain_text())
}

pub(crate) struct Syntax {
	highlighter: HighlightLines<'static>,
	upto:        usize,
}

impl Syntax {
	pub(crate) fn new(ext: &str) -> Self {
		Self { highlighter: HighlightLines::new(syntax(ext), &THEME), upto: 0 }
	}

	pub(crate) fn for_fence(lang: &str) -> Self {
		let syntax = SYNTAXES
			.find_syntax_by_token(lang)
			.unwrap_or_else(|| SYNTAXES.find_syntax_plain_text());

		Self { highlighter: HighlightLines::new(syntax, &THEME), upto: 0 }
	}

	#[cfg(test)]
	pub(crate) fn upto(&self) -> usize { self.upto }

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

	#[test]
	fn the_embedded_theme_loads_and_a_keyword_gets_its_colour() {
		let mut syntax = Syntax::new("rs");
		let runs = syntax.next("fn main() {}");

		let keyword = runs.first().expect("a keyword run");
		assert_eq!(keyword.0, 0..2, "the `fn` is the first run");
		assert_eq!(keyword.1, eframe::egui::Color32::from_rgb(0xcb, 0xa6, 0xf7), "mauve, the token's own");
		assert_eq!(syntax.upto(), 1, "and the walk has moved on");
	}

	#[test]
	fn an_unknown_extension_is_plain_text() {
		let mut syntax = Syntax::new("nonexistent-ext");

		assert_eq!(syntax.next("just words"), syntax.next("just words"), "plain text has no runs of its own");
		assert_eq!(syntax.upto(), 2);
	}
}
