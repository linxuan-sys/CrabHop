//! The window's colours and type, written for this crate alone: Catppuccin Mocha, the same palette
//! the legacy window used, laid out as an egui factory rather than as a rebinding of someone else's.

use std::sync::Arc;

use eframe::egui::{Color32, Context, FontData, FontDefinitions, FontFamily, FontId, TextStyle, Visuals};

/// The type baseline the spec keeps: 16px on a 24px line.
pub(crate) const FONT_SIZE: f32 = 16.0;
pub(crate) const LINE_HEIGHT: f32 = 24.0;

/// The secondary type the chrome reads in: 13px, for the status bar and the small print (§0).
pub(crate) const FONT_SMALL: f32 = 13.0;

/// The row's own height in the comfortable density: the 24px line plus 4px either side, so rows
/// are told apart by their air rather than by a rule (spec §1.1). The compact density is 26 and
/// belongs to the settings window of N3.
pub(crate) const ROW_HEIGHT: f32 = 32.0;
#[allow(dead_code)]
pub(crate) const ROW_HEIGHT_TIGHT: f32 = 26.0;

/// The icon box: a 20px square with a 16px glyph centred in it.
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
	/// The selection of a column that does not have the focus: the desktop has to answer "who has
	/// it" (spec §1.2).
	pub sel_inactive: Color32,
	/// The state colours the permission bits and the task rows are read with.
	pub ok:   Color32,
	pub warn: Color32,
	pub err:  Color32,
	pub busy: Color32,
	pub border:    Color32,
	/// The stronger rule the overlays are outlined with — a 1px line with more presence than the
	/// separators' (§4 container).
	pub border_strong: Color32,
	pub blue:      Color32,
	pub teal:      Color32,
	pub green:     Color32,
	/// The baseline's mauve: the media files' own colour in the filetype rules (their value is the
	/// accent's, and the two roles are kept apart on purpose).
	pub mauve:     Color32,
	pub yellow:    Color32,
	pub red:       Color32,
	/// The palette's pink: no file wears it any more (P34's icon table brought yazi's own colours
	/// for the fonts and the special names), and the Latte batch's appearance page may want it.
	#[allow(dead_code)]
	pub pink:      Color32,
	/// bg-raised (surface0): the skeleton's placeholders and the tab hover's substrate (§1.3).
	pub raised:    Color32,
	/// fg-disabled (overlay0): the quiet half of the scrollbar, the dashes of a permission, the
	/// text that is there to be ignored.
	pub disabled:  Color32,
	/// The text on an accent fill — crust, the darkest of the palette (§2.5's badge).
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

	/// Fonts and styling in one place: the crate's own copy of the same two faces the legacy window
	/// embedded — read from where they already are, not moved.
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

	/// A dark, quiet window: the list is the picture, everything around it recedes.
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
