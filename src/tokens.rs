
use std::sync::{Arc, OnceLock};

use eframe::egui::{Color32, Context, FontData, FontDefinitions, FontFamily, FontId, TextStyle, Visuals};

use crate::config::Config;

pub(crate) const FONT_SIZE: f32 = 16.0;
pub(crate) const LINE_HEIGHT: f32 = 24.0;

pub(crate) const FONT_SMALL: f32 = 13.0;

pub(crate) const ROW_HEIGHT: f32 = 32.0;
#[allow(dead_code)]
pub(crate) const ROW_HEIGHT_TIGHT: f32 = 26.0;

pub(crate) const SHEET_COL_MIN_W: f32 = 48.0;
pub(crate) const SHEET_COL_MAX_W: f32 = 320.0;

pub(crate) const ICON_BOX: f32 = 20.0;

static THEME: OnceLock<Tokens> = OnceLock::new();

#[derive(Clone, Copy, PartialEq)]
pub(crate) struct Tokens {
	pub base:      Color32,
	pub mantle:    Color32,
	pub crust:     Color32,
	pub text:      Color32,
	pub subtext:   Color32,
	pub accent:    Color32,
	pub cursor_bg: Color32,
	pub sel_bg:    Color32,
	pub sel_inactive: Color32,
	pub ok:   Color32,
	pub warn: Color32,
	pub err:  Color32,
	pub busy: Color32,
	pub border:    Color32,
	pub border_strong: Color32,
	pub blue:      Color32,
	pub teal:      Color32,
	pub green:     Color32,
	pub mauve:     Color32,
	pub yellow:    Color32,
	pub red:       Color32,
	#[allow(dead_code)]
	pub pink:      Color32,
	pub raised:    Color32,
	pub disabled:  Color32,
	pub accent_ink: Color32,
}

impl Tokens {
	pub(crate) const MOCHA: Self = Self {
		base:      Color32::from_rgb(0x1e, 0x1e, 0x2e),
		mantle:    Color32::from_rgb(0x18, 0x18, 0x25),
		crust:     Color32::from_rgb(0x11, 0x11, 0x1b),
		text:      Color32::from_rgb(0xcd, 0xd6, 0xf4),
		subtext:   Color32::from_rgb(0xa6, 0xad, 0xc8),
		accent:    Color32::from_rgb(0xcb, 0xa6, 0xf7),
		cursor_bg: Color32::from_rgb(0x31, 0x32, 0x44),
		sel_bg:    Color32::from_rgb(0x45, 0x47, 0x5a),
		sel_inactive: Color32::from_rgb(0x24, 0x25, 0x35),
		ok:   Color32::from_rgb(0xa6, 0xe3, 0xa1),
		warn: Color32::from_rgb(0xf9, 0xe2, 0xaf),
		err:  Color32::from_rgb(0xf3, 0x8b, 0xa8),
		busy: Color32::from_rgb(0x89, 0xb4, 0xfa),
		border:    Color32::from_rgb(0x45, 0x47, 0x5a),
		border_strong: Color32::from_rgb(0x58, 0x5b, 0x70),
		blue:      Color32::from_rgb(0x89, 0xb4, 0xfa),
		teal:      Color32::from_rgb(0x94, 0xe2, 0xd5),
		green:     Color32::from_rgb(0xa6, 0xe3, 0xa1),
		mauve:     Color32::from_rgb(0xcb, 0xa6, 0xf7),
		yellow:    Color32::from_rgb(0xf9, 0xe2, 0xaf),
		red:       Color32::from_rgb(0xf3, 0x8b, 0xa8),
		pink:      Color32::from_rgb(0xf5, 0xc2, 0xe7),
		raised:    Color32::from_rgb(0x31, 0x32, 0x44),
		disabled:  Color32::from_rgb(0x6c, 0x70, 0x86),
		accent_ink: Color32::from_rgb(0x11, 0x11, 0x1b),
	};

	pub(crate) fn theme() -> Self {
		*THEME.get_or_init(|| {
			let Some(path) = Config::dir().map(|dir| dir.join("theme.toml")) else {
				return Self::MOCHA;
			};
			let Ok(text) = std::fs::read_to_string(&path) else {
				return Self::MOCHA;
			};

			let (tokens, notes) = Self::parse(&text);
			for note in notes {
				eprintln!("crabhop: {}: {note}", path.display());
			}

			tokens
		})
	}

	pub(crate) fn parse(text: &str) -> (Self, Vec<String>) {
		let mut tokens = Self::MOCHA;
		let mut notes = Vec::new();

		let value: toml::Value = match toml::from_str(text) {
			Ok(value) => value,
			Err(e) => {
				notes.push(format!("not TOML ({e}); the factory colours stand"));
				return (tokens, notes);
			}
		};
		let Some(colors) = value.get("colors") else {
			return (tokens, notes);
		};
		let Some(table) = colors.as_table() else {
			notes.push("[colors] is not a table; the factory colours stand".to_owned());
			return (tokens, notes);
		};

		for (name, value) in table {
			let Some(slot) = tokens.slot(name) else {
				notes.push(format!("no colour called \"{name}\"; ignored"));
				continue;
			};
			let Some(text) = value.as_str() else {
				notes.push(format!("colors.{name} is not a string; ignored"));
				continue;
			};
			match Self::hex(text) {
				Some(colour) => *slot = colour,
				None => notes.push(format!("colors.{name} = \"{text}\" is not #rrggbb; ignored")),
			}
		}

		(tokens, notes)
	}

	fn slot(&mut self, name: &str) -> Option<&mut Color32> {
		Some(match name {
			"base" => &mut self.base,
			"mantle" => &mut self.mantle,
			"crust" => &mut self.crust,
			"text" => &mut self.text,
			"subtext" => &mut self.subtext,
			"accent" => &mut self.accent,
			"cursor_bg" => &mut self.cursor_bg,
			"sel_bg" => &mut self.sel_bg,
			"sel_inactive" => &mut self.sel_inactive,
			"ok" => &mut self.ok,
			"warn" => &mut self.warn,
			"err" => &mut self.err,
			"busy" => &mut self.busy,
			"border" => &mut self.border,
			"border_strong" => &mut self.border_strong,
			"blue" => &mut self.blue,
			"teal" => &mut self.teal,
			"green" => &mut self.green,
			"mauve" => &mut self.mauve,
			"yellow" => &mut self.yellow,
			"red" => &mut self.red,
			"pink" => &mut self.pink,
			"raised" => &mut self.raised,
			"disabled" => &mut self.disabled,
			"accent_ink" => &mut self.accent_ink,
			_ => return None,
		})
	}

	fn hex(text: &str) -> Option<Color32> {
		let digits = text.strip_prefix('#')?;
		if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
			return None;
		}
		let at = |index: usize| u8::from_str_radix(&digits[index..index + 2], 16).ok();

		Some(Color32::from_rgb(at(0)?, at(2)?, at(4)?))
	}

	pub(crate) fn install(ctx: &Context) {
		Self::install_with(ctx, &Self::MOCHA);
	}

	pub(crate) fn install_with(ctx: &Context, tokens: &Tokens) {
		let mut fonts = FontDefinitions::default();
		fonts.font_data.insert(
			"maple".to_owned(),
			Arc::new(FontData::from_static(include_bytes!("../assets/fonts/MapleMonoNormal-NF-CN-Regular.ttf"))),
		);
		fonts.font_data.insert(
			"maple-bold".to_owned(),
			Arc::new(FontData::from_static(include_bytes!("../assets/fonts/MapleMonoNormal-NF-CN-Bold.ttf"))),
		);

		let mut mono = vec![ "maple".to_owned() ];
		mono.extend(fonts.families.get(&FontFamily::Monospace).cloned().unwrap_or_default());
		fonts.families.insert(FontFamily::Monospace, mono.clone());

		let mut bold = vec![ "maple-bold".to_owned() ];
		bold.extend(mono);
		fonts.families.insert(FontFamily::Name("maple-bold".into()), bold);

		ctx.set_fonts(fonts);

		ctx.all_styles_mut(|style| {
			style.text_styles.insert(TextStyle::Body, FontId::new(FONT_SIZE, FontFamily::Monospace));
			style.text_styles.insert(TextStyle::Monospace, FontId::new(FONT_SIZE, FontFamily::Monospace));
			style.visuals = tokens.visuals();
		});
	}

	pub(crate) fn visuals(&self) -> Visuals {
		let mut visuals = Visuals::dark();

		visuals.panel_fill = self.mantle;
		visuals.window_fill = self.base;
		visuals.extreme_bg_color = self.crust;
		visuals.faint_bg_color = self.base;
		visuals.override_text_color = Some(self.text);
		visuals.widgets.noninteractive.bg_fill = self.base;
		visuals.widgets.noninteractive.fg_stroke.color = self.subtext;
		visuals.selection.bg_fill = self.sel_bg;

		visuals
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_example_is_the_whole_factory_set() {
		let text = include_str!("../packaging/theme.toml.example");
		let value: toml::Value = toml::from_str(text).expect("the example is TOML");
		assert_eq!(value.get("colors").and_then(|colors| colors.as_table()).map(|table| table.len()), Some(25));

		let (tokens, notes) = Tokens::parse(text);
		assert!(notes.is_empty(), "{notes:?}");
		assert!(tokens == Tokens::MOCHA, "the example is the factory set");
	}

	#[test]
	fn a_partial_file_keeps_the_colours_it_does_not_say() {
		let (tokens, notes) = Tokens::parse("[colors]\nbase = \"#000000\"\naccent = \"#FFFFFF\"\n");
		assert!(notes.is_empty(), "{notes:?}");
		assert_eq!(tokens.base, Color32::BLACK);
		assert_eq!(tokens.accent, Color32::WHITE);
		assert_eq!(tokens.mantle, Tokens::MOCHA.mantle);
	}

	#[test]
	fn a_bad_value_is_noted_and_ignored() {
		let (tokens, notes) = Tokens::parse("[colors]\nbase = \"#12345\"\ntext = \"purple\"\nwarn = 7\n");
		assert!(tokens == Tokens::MOCHA, "nothing half-applied");
		assert_eq!(notes.len(), 3, "{notes:?}");
	}

	#[test]
	fn an_unknown_colour_is_noted_and_ignored() {
		let (tokens, notes) = Tokens::parse("[colors]\nglow = \"#ffffff\"\n");
		assert!(tokens == Tokens::MOCHA);
		assert_eq!(notes.len(), 1);
		assert!(notes[0].contains("glow"), "{notes:?}");
	}

	#[test]
	fn broken_toml_keeps_the_factory_colours() {
		let (tokens, notes) = Tokens::parse("[colors\nbase = ");
		assert!(tokens == Tokens::MOCHA);
		assert_eq!(notes.len(), 1);
	}
}
