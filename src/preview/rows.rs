
use std::ops::Range;

use eframe::egui::{Align2, Color32, FontFamily, FontId, Painter, Rect, pos2, vec2};

use crate::list::clipped;
use crate::tokens::{ICON_BOX, Tokens};

#[derive(Clone)]
pub(crate) struct Run {
	pub range:  Range<usize>,
	pub colour: Color32,
	pub size:   f32,
	pub bold:   bool,
	pub bg:     Option<Color32>,
}

#[derive(Clone)]
pub(crate) struct Row {
	pub text:    String,
	pub height:  f32,
	pub before:  f32,
	pub indent:  f32,
	pub size:    f32,
	pub colour:  Color32,
	pub bold:    bool,
	pub runs:    Vec<Run>,
	pub bg:      Option<Color32>,
	pub bg_round: bool,
	pub bar:     Option<Color32>,
	pub rule:    Option<Color32>,
	pub rule_top: bool,
	pub number:  Option<usize>,
	pub cells:   Vec<String>,
	pub col_w:   f32,
	pub lead:    Option<(String, Color32)>,
	pub right:   Option<String>,
	pub image:   Option<String>,
}

impl Default for Row {
	fn default() -> Self {
		Self {
			text:     String::new(),
			height:   24.0,
			before:   0.0,
			indent:   0.0,
			size:     16.0,
			colour:   Color32::PLACEHOLDER,
			bold:     false,
			runs:     Vec::new(),
			bg:       None,
			bg_round: false,
			bar:      None,
			rule:     None,
			rule_top: false,
			number:   None,
			cells:    Vec::new(),
			col_w:    0.0,
			lead:     None,
			right:    None,
			image:    None,
		}
	}
}

impl Row {
	pub(crate) fn new(text: impl Into<String>, size: f32, colour: Color32) -> Self {
		Self { text: text.into(), size, colour, height: size + 8.0, ..Default::default() }
	}
}

pub(crate) struct Doc {
	pub rows:   Vec<Row>,
	pub starts: Vec<f32>,
	pub total:  f32,
	pub gutter: bool,
	pub digits: usize,
}

impl Doc {
	pub(crate) fn new(rows: Vec<Row>) -> Self {
		let mut starts = Vec::with_capacity(rows.len());
		let mut at = 0.0;

		for row in &rows {
			at += row.before;
			starts.push(at);
			at += row.height.max(1.0);
		}

		let gutter = rows.iter().any(|row| row.number.is_some());
		let digits = rows.iter().filter_map(|row| row.number).max().map_or(1, |last| last.to_string().len());
		Self { rows, starts, total: at, gutter, digits }
	}

	pub(crate) fn first_at(&self, offset: f32) -> usize {
		self.starts.partition_point(|start| *start < offset)
	}
}

pub(crate) fn advance(size: f32) -> f32 { size * 0.62 }

const GUTTER_PAD: f32 = 6.0;

pub(crate) struct Gutter {
	pub number_left:  f32,
	#[cfg_attr(not(test), allow(dead_code))]
	pub number_right: f32,
	pub border:       f32,
}

pub(crate) fn gutter(digits: usize, advance: f32) -> Gutter {
	let width = digits.max(1) as f32 * advance;
	let number_left = GUTTER_PAD;

	Gutter { number_left, number_right: number_left + width, border: number_left + width + GUTTER_PAD }
}

fn font(size: f32, bold: bool) -> FontId {
	if bold {
		FontId::new(size, FontFamily::Name("maple-bold".into()))
	} else {
		FontId::new(size, FontFamily::Monospace)
	}
}

pub(crate) fn draw(
	painter: &Painter,
	rect: Rect,
	doc: &Doc,
	offset: f32,
	tokens: &Tokens,
	max_columns: Option<usize>,
	fade: bool,
) -> bool {
	let hairline = gutter(doc.digits, advance(13.0));
	let left = if doc.gutter { rect.min.x + hairline.border } else { rect.min.x };
	let right_fade = rect.max.x - 6.0;
	let mut cut_right = false;

	if doc.gutter {
		let x = rect.min.x + hairline.border;
		painter.line_segment([ pos2(x, rect.min.y), pos2(x, rect.max.y) ], (1.0, tokens.border));
	}

	let mut i = doc.first_at(offset).saturating_sub(1);

	while i < doc.rows.len() {
		let row = &doc.rows[i];
		let y = rect.min.y + doc.starts[i] - offset;
		if y > rect.max.y {
			break;
		}
		i += 1;
		if y + row.height < rect.min.y {
			continue;
		}

		if let Some(bg) = row.bg {
			let band = Rect::from_min_size(pos2(rect.min.x, y), vec2(rect.width(), row.height));
			let radius = if row.bg_round { 6.0 } else { 0.0 };
			painter.rect_filled(band, radius, bg);
		}
		if let Some(bar) = row.bar {
			let x = rect.min.x + (row.indent - 12.0).max(0.0);
			painter.rect_filled(Rect::from_min_size(pos2(x, y + 2.0), vec2(3.0, row.height - 4.0)), 1.5, bar);
		}
		if row.rule_top {
			painter.line_segment([ pos2(rect.min.x, y), pos2(rect.max.x, y) ], (1.0, row.rule.unwrap_or(tokens.border)));
		}
		if let Some(rule) = row.rule {
			painter.line_segment([ pos2(rect.min.x, y + row.height), pos2(rect.max.x, y + row.height) ], (1.0, rule));
		}

		if let Some((glyph, colour)) = &row.lead {
			let lead = rect.min.x + row.indent - advance(row.size) - ICON_BOX * 0.5;
			painter.text(
				pos2(lead, y + row.height * 0.5),
				Align2::CENTER_CENTER,
				glyph.clone(),
				font(row.size, false),
				*colour,
			);
		}
		if let Some(right) = &row.right {
			painter.text(
				pos2(rect.max.x - advance(13.0), y + row.height * 0.5),
				Align2::RIGHT_CENTER,
				right.clone(),
				font(13.0, false),
				tokens.subtext,
			);
		}

		let x = left + row.indent;
		if let Some(number) = row.number {
			painter.text(
				pos2(rect.min.x + hairline.number_left, y + row.height * 0.5),
				Align2::LEFT_CENTER,
				number.to_string(),
				FontId::new(13.0, FontFamily::Monospace),
				tokens.disabled,
			);
		}

		if row.image.is_some() {
			continue;
		}

		if !row.cells.is_empty() {
			let mut cx = x;
			for (c, cell) in row.cells.iter().enumerate() {
				let budget = ((row.col_w - 16.0) / advance(row.size)).floor().max(1.0) as u32;
				let shown = clipped(cell, budget);
				painter.text(pos2(cx + 8.0, y + row.height * 0.5), Align2::LEFT_CENTER, shown, font(row.size, row.bold), row.colour);

				cx += row.col_w;
				if c < row.cells.len() {
					painter.line_segment([ pos2(cx, y), pos2(cx, y + row.height) ], (1.0, tokens.border));
				}
			}
			continue;
		}

		let facts = row.right.as_ref().map_or(0.0, |right| {
			painter.layout_no_wrap(right.clone(), font(13.0, false), tokens.subtext).size().x + 12.0
		});
		let budget = ((right_fade - facts - x) / advance(row.size)).floor().max(1.0) as u32;
		let budget = max_columns.map_or(budget, |max| budget.min(max as u32));
		let shown = clipped(&row.text, budget);
		if shown.len() < row.text.len() {
			cut_right = true;
		}

		if row.runs.is_empty() {
			painter.text(pos2(x, y + row.height * 0.5), Align2::LEFT_CENTER, shown, font(row.size, row.bold), row.colour);
			continue;
		}

		let mut rx = x;
		for run in &row.runs {
			if run.range.start >= shown.len() {
				break;
			}
			let end = run.range.end.min(shown.len());
			let Some(text) = row.text.get(run.range.start..end) else { continue };
			if text.is_empty() {
				continue;
			}

			let width = text.chars().map(crate::list::cell_width).sum::<u32>() as f32 * advance(run.size);
			if let Some(bg) = run.bg {
				painter.rect_filled(
					Rect::from_min_size(pos2(rx - 3.0, y + row.height * 0.5 - run.size * 0.6), vec2(width + 6.0, run.size * 1.2)),
					3.0,
					bg,
				);
			}
			painter.text(pos2(rx, y + row.height * 0.5), Align2::LEFT_CENTER, text, font(run.size, run.bold), run.colour);
			rx += width;
		}
	}

	if cut_right && fade {
		for strip in 0..4 {
			let alpha = (255.0 * (1.0 - (strip as f32 + 0.5) / 4.0)) as u8;
			let x = rect.max.x - 24.0 + strip as f32 * 6.0;
			let colour = Color32::from_rgba_unmultiplied(tokens.crust.r(), tokens.crust.g(), tokens.crust.b(), alpha);
			painter.rect_filled(Rect::from_min_size(pos2(x, rect.min.y), vec2(6.5, rect.height())), 0.0, colour);
		}
	}

	cut_right
}

#[cfg(test)]
mod tests {
	use super::*;
	use eframe::egui::Color32;

	#[test]
	fn the_gutter_sandwiches_its_digits_between_equal_gaps() {
		let advance = advance(13.0);

		for digits in 1..=4 {
			let g = gutter(digits, advance);
			let left_gap = g.number_left;
			let right_gap = g.border - g.number_right;

			assert!((left_gap - GUTTER_PAD).abs() < 0.01, "the left gap is the pad");
			assert!((right_gap - left_gap).abs() < 0.01, "{digits} digits: gaps equal ({left_gap} vs {right_gap})");
			assert!(
				(g.number_right - g.number_left - digits as f32 * advance).abs() < 0.01,
				"and the slot is exactly the digits"
			);
		}

		let one = gutter(1, advance);
		assert!(one.border < 32.0, "a one-digit gutter stays a sliver: {}", one.border);
		let four = gutter(4, advance);
		assert!(four.border > one.border && four.border < 64.0, "four digits widen it, still short of the old column");
		assert_eq!(gutter(0, advance).border, one.border, "a lone number is one digit's gutter");
	}

	#[test]
	fn a_document_knows_its_widest_number() {
		let row = |n: usize| {
			let mut row = Row::new("code", 16.0, Color32::WHITE);
			row.number = Some(n);
			row
		};

		assert_eq!(Doc::new(vec![ row(1), row(9) ]).digits, 1);
		assert_eq!(Doc::new(vec![ row(1), row(12) ]).digits, 2);
		assert_eq!(Doc::new(vec![ row(1), row(120) ]).digits, 3);
	}

	#[test]
	fn starts_are_the_prefix_sums_and_a_scroll_finds_its_first_row() {
		let mut a = Row::new("a", 16.0, Color32::WHITE);
		a.height = 24.0;
		let mut b = Row::new("b", 16.0, Color32::WHITE);
		b.height = 24.0;
		b.before = 12.0;
		let c = Row::new("c", 16.0, Color32::WHITE);

		let doc = Doc::new(vec![ a, b, c ]);
		assert_eq!(doc.starts, vec![ 0.0, 36.0, 60.0 ], "the second row's margin is part of its start");
		assert_eq!(doc.total, 60.0 + 24.0);

		assert_eq!(doc.first_at(0.0), 0);
		assert_eq!(doc.first_at(36.0), 1);
		assert_eq!(doc.first_at(37.0), 2, "past the second row's start it is the third that begins");
	}

	#[test]
	fn a_row_is_clipped_the_way_the_list_clips_its_names() {
		assert_eq!(clipped("abcdef", 3), "abc");
		assert_eq!(clipped("陶海水", 4), "陶海");
		assert_eq!(clipped("a陶b", 2), "a");
	}
}
