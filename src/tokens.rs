
use std::sync::Arc;

use eframe::egui::{Color32, Context, FontData, FontDefinitions, FontFamily, FontId, TextStyle, Visuals};

pub(crate) const FONT_SIZE: f32 = 16.0;
pub(crate) const LINE_HEIGHT: f32 = 24.0;

pub(crate) const FONT_SMALL: f32 = 13.0;

pub(crate) const ROW_HEIGHT: f32 = 32.0;
#[allow(dead_code)]
pub(crate) const ROW_HEIGHT_TIGHT: f32 = 26.0;

pub(crate) const SHEET_COL_MIN_W: f32 = 48.0;
pub(crate) const SHEET_COL_MAX_W: f32 = 320.0;

pub(crate) const ICON_BOX: f32 = 20.0;

#[derive(Clone, Copy)]
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

	pub(crate) fn install(ctx: &Context) {
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
			style.visuals = Self::MOCHA.visuals();
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
