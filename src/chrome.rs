//! The window's chrome, kept pure: where the three columns are, what a separator drag does to
//! their shares, the skeleton's deterministic rows, the overlay scrollbar's thumb, and the
//! permission string the status bar colours. No painter here — this is the arithmetic the
//! spec's §1.3/§2.1/§2.2/§2.5 are, so it can be read and tested without a window.

use eframe::egui::{Painter, Pos2, Rect, Vec2, pos2, vec2};

/// The chart of heights the spec lays down: a 36px tab bar, a 28px status bar between which the
/// three columns live (§2.1).
pub(crate) const TAB_HEIGHT: f32 = 36.0;
pub(crate) const STATUS_HEIGHT: f32 = 28.0;

/// The narrow-window ladder: the parent column goes first, then the preview (§2.1).
pub(crate) const HIDE_PARENT: f32 = 760.0;
pub(crate) const HIDE_PREVIEW: f32 = 560.0;

/// No column may be dragged into nothing, and the current column — the one a person operates —
/// gets the larger floor.
pub(crate) const MIN_COLUMN: f32 = 120.0;
pub(crate) const MIN_CURRENT: f32 = 240.0;

/// The separator's invisible hot zone: 3px either side of the 1px line (§2.2).
pub(crate) const SEP_ZONE: f32 = 7.0;

/// The overlay scrollbar: 6px wide, at least 24px long, its right edge 2px inside the column
/// (§3 总规则).
pub(crate) const SCROLL_W: f32 = 6.0;
pub(crate) const SCROLL_MIN: f32 = 24.0;
pub(crate) const SCROLL_INSET: f32 = 2.0;

/// One column of the three: a rectangle, and the divider it shares with its right neighbour.
pub(crate) struct Columns {
	pub parent:  Option<Rect>,
	pub current: Rect,
	pub preview: Option<Rect>,
	/// The draggable boundaries: the x of the 1px line, and the two columns it sits between —
	/// what a drag rewrites.
	pub seps:    Vec<(f32, usize, usize)>,
}

/// The three columns of a body rect: the spec's 1:4:3 among whichever of them fit, and the
/// boundaries a drag can move.
pub(crate) fn columns(body: Rect, shares: [f32; 3]) -> Columns {
	let parent = body.width() >= HIDE_PARENT;
	let preview = body.width() >= HIDE_PREVIEW;

	let visible = [ parent, true, preview ];
	let sum: f32 = (0..3).filter(|i| visible[*i]).map(|i| shares[i]).sum();
	if sum <= 0.0 {
		return Columns { parent: None, current: body, preview: None, seps: Vec::new() };
	}

	let mut at = body.min.x;
	let mut widths = [ 0.0f32; 3 ];
	for i in 0..3 {
		if visible[i] {
			widths[i] = shares[i] / sum * body.width();
		}
	}

	let parent_rect = parent.then(|| Rect::from_min_size(pos2(at, body.min.y), vec2(widths[0], body.height())));
	at += if parent { widths[0] } else { 0.0 };
	let current_rect = Rect::from_min_size(pos2(at, body.min.y), vec2(widths[1], body.height()));
	at += widths[1];
	let preview_rect = preview.then(|| Rect::from_min_size(pos2(at, body.min.y), vec2(widths[2], body.height())));

	let mut seps = Vec::new();
	if parent {
		seps.push((current_rect.min.x, 0, 1));
	}
	if preview {
		seps.push((current_rect.max.x, 1, 2));
	}

	Columns { parent: parent_rect, current: current_rect, preview: preview_rect, seps }
}

/// A drag of the boundary between two columns, in shares: the two sides give and take the pixels,
/// and both are clamped so neither can be dragged into nothing. `visible` is which of the three
/// columns the window is showing, and `body` the width the shares divide.
pub(crate) fn resized(
	shares: [f32; 3],
	left: usize,
	right: usize,
	delta: f32,
	body: Rect,
	visible: [bool; 3],
) -> [f32; 3] {
	let sum: f32 = (0..3).filter(|i| visible[*i]).map(|i| shares[i]).sum();
	if sum <= 0.0 || body.width() <= 0.0 {
		return shares;
	}

	let left_px = if visible[left] { shares[left] / sum * body.width() } else { 0.0 };
	let right_px = if visible[right] { shares[right] / sum * body.width() } else { 0.0 };
	let floor = |column: usize| if column == 1 { MIN_CURRENT } else { MIN_COLUMN };

	// what the drag is allowed to be, both sides considered — and when the window is so narrow
	// that both floors cannot hold at once, no drag can satisfy them: the shares stay put instead
	// of a clamp whose bounds have crossed (which would panic)
	let (low, high) = (floor(left) - left_px, right_px - floor(right));
	let delta = if low <= high { delta.clamp(low, high) } else { 0.0 };

	let mut next = shares;
	next[left] = (left_px + delta) / body.width() * sum;
	next[right] = (right_px - delta) / body.width() * sum;
	next
}

/// The skeleton's row count: what the directory held last time, or a plain dozen — and never more
/// than the column can show (§1.3).
pub(crate) fn skeleton_rows(hint: Option<usize>, visible: usize) -> usize {
	hint.unwrap_or(12).clamp(1, visible.max(1))
}

/// The skeleton's bar width at a row, 40%–80% of the column. A fixed scramble of the row number:
/// scattered, but the same every frame — the skeleton is static, and honest about it (§1.3).
pub(crate) fn skeleton_bar(row: usize) -> f32 {
	0.40 + ((row as u32).wrapping_mul(2_654_435_761) >> 10) as f32 % 41.0 / 100.0
}

/// The overlay scrollbar's thumb on its track, or nothing when the content fits.
pub(crate) struct Bar {
	pub track:  Rect,
	pub thumb:  Rect,
	pub height: f32,
	pub travel: f32,
}

/// Where the thumb sits for a scroll offset, in rows.
pub(crate) fn bar(track: Rect, content: f32, visible: f32, offset: f32) -> Option<Bar> {
	if content <= visible || content <= 0.0 || track.height() <= 0.0 {
		return None;
	}

	let height = (track.height() * visible / content).clamp(SCROLL_MIN.min(track.height()), track.height());
	let travel = track.height() - height;
	let at = travel * (offset / (content - visible)).clamp(0.0, 1.0);
	let thumb = Rect::from_min_size(
		pos2(track.max.x - SCROLL_INSET - SCROLL_W, track.min.y + at),
		vec2(SCROLL_W, height),
	);

	Some(Bar { track, thumb, height, travel })
}

/// The offset a drag of the thumb asks for, from the pointer's y.
pub(crate) fn dragged_offset(bar: &Bar, pointer_y: f32, content: f32, visible: f32) -> f32 {
	if bar.travel <= 0.0 {
		return 0.0;
	}

	let at = (pointer_y - bar.track.min.y - bar.height * 0.5).clamp(0.0, bar.travel);
	at / bar.travel * (content - visible)
}

/// `rwxr-xr--`, the way a person reads a permission — and nine dashes when the platform has no
/// bits to show. The status bar colours each character; this decides which ones are there.
pub(crate) fn perm(mode: Option<u32>) -> [u8; 9] {
	const DASH: u8 = b'-';
	let Some(mode) = mode else {
		return [ DASH; 9 ];
	};

	let mut out = [ DASH; 9 ];
	let flags = [
		(0o400, b'r'),
		(0o200, b'w'),
		(0o100, b'x'),
		(0o040, b'r'),
		(0o020, b'w'),
		(0o010, b'x'),
		(0o004, b'r'),
		(0o002, b'w'),
		(0o001, b'x'),
	];
	for (i, (bit, letter)) in flags.into_iter().enumerate() {
		if mode & bit != 0 {
			out[i] = letter;
		}
	}

	out
}

/// One fade mask for the edges of an overflowing tab bar: `strips` slivers of a colour that
/// thins out, drawn from the `edge` inwards (§2.4).
pub(crate) fn fade_strips(bar: Rect, left: bool, colour: eframe::egui::Color32) -> Vec<(Rect, eframe::egui::Color32)> {
	const WIDTH: f32 = 12.0;
	const STRIPS: usize = 4;

	let from = if left { bar.min.x } else { bar.max.x - WIDTH };
	(0..STRIPS)
		.map(|i| {
			let alpha = (255.0 * (1.0 - (i as f32 + 0.5) / STRIPS as f32)) as u8;
			let x = from + i as f32 * WIDTH / STRIPS as f32;
			(
				Rect::from_min_size(pos2(x, bar.min.y), vec2(WIDTH / STRIPS as f32 + 0.5, bar.height())),
				eframe::egui::Color32::from_rgba_unmultiplied(colour.r(), colour.g(), colour.b(), alpha),
			)
		})
		.collect()
}

/// Where a pointer sits in the bar's own scrolled space: the tab drag and the wheel both ask.
pub(crate) fn unscrolled(at: Pos2, screen: Rect, scroll: f32) -> Vec2 {
	vec2(at.x - screen.min.x + scroll, at.y - screen.min.y)
}

/// The soft shadow an overlay carries (§4's container): egui's painter has no blur of its own, so
/// three dark rings stand in for the spec's `0 8px 24px @35%`.
pub(crate) fn shadow(painter: &Painter, rect: Rect, radius: f32) {
	for (grow, alpha) in [ (18.0f32, 16u8), (11.0, 26), (5.0, 38) ] {
		painter.rect_filled(
			rect.translate(vec2(0.0, 4.0)).expand(grow),
			radius + grow * 0.5,
			eframe::egui::Color32::from_black_alpha(alpha),
		);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn body(width: f32) -> Rect {
		Rect::from_min_size(pos2(100.0, 40.0), vec2(width, 700.0))
	}

	/// 1:4:3, hiding the parent first and the preview second — and with each hidden the rest of
	/// the width is still divided by the same rule (§2.1).
	#[test]
	fn the_three_columns_are_the_specs_ratio_until_the_window_narrows() {
		let wide = columns(body(1280.0), [ 1.0, 4.0, 3.0 ]);
		assert!((wide.current.width() - 640.0).abs() < 0.5);
		assert!((wide.parent.as_ref().unwrap().width() - 160.0).abs() < 0.5);
		assert!((wide.preview.as_ref().unwrap().width() - 480.0).abs() < 0.5);
		assert_eq!(wide.seps.len(), 2, "two boundaries, two drags");

		let narrow = columns(body(700.0), [ 1.0, 4.0, 3.0 ]);
		assert!(narrow.parent.is_none(), "the parent goes first");
		assert!((narrow.current.width() - 400.0).abs() < 0.5, "4/7 of it");
		assert!((narrow.preview.as_ref().unwrap().width() - 300.0).abs() < 0.5);
		assert_eq!(narrow.seps.len(), 1);

		let tiny = columns(body(500.0), [ 1.0, 4.0, 3.0 ]);
		assert!(tiny.parent.is_none() && tiny.preview.is_none(), "then the preview");
		assert_eq!(tiny.seps.len(), 0);
	}

	/// A drag moves the boundary and clamps at both floors: what the two sides can give and take
	/// is what the pointer can do.
	#[test]
	fn a_drag_moves_the_boundary_and_clamps_both_columns() {
		let shares = [ 1.0, 4.0, 3.0 ];
		let body = body(1280.0);

		let moved = resized(shares, 0, 1, 40.0, body, [ true; 3 ]);
		assert!(moved[0] > shares[0] && moved[1] < shares[1], "the boundary moved right");

		let stopped = resized(shares, 0, 1, 10_000.0, body, [ true; 3 ]);
		let sum = 8.0;
		let left_px = stopped[0] / sum * 1280.0;
		let right_px = stopped[1] / sum * 1280.0;
		assert!(left_px >= MIN_COLUMN - 0.5, "the parent is not dragged into nothing: {left_px}");
		assert!(right_px >= MIN_CURRENT - 0.5, "nor is the current column: {right_px}");

		let stopped = resized(shares, 1, 2, 10_000.0, body, [ true; 3 ]);
		let right_px = stopped[2] / sum * 1280.0;
		assert!(right_px >= MIN_COLUMN - 0.5, "and the preview keeps its floor too: {right_px}");
	}

	/// A window narrower than the columns' floors: both floors cannot hold at once, so a drag —
	/// even a nudge — leaves the shares exactly where they were instead of panicking on a clamp
	/// whose bounds have crossed.
	#[test]
	fn a_drag_too_narrow_for_the_floors_keeps_the_shares() {
		let shares = [ 1.0, 4.0, 3.0 ];
		let body = body(240.0);

		assert_eq!(resized(shares, 0, 1, 0.0, body, [ true; 3 ]), shares, "a nudge moves nothing");
		assert_eq!(resized(shares, 0, 1, 500.0, body, [ true; 3 ]), shares, "nor does a shove");
		assert_eq!(resized(shares, 1, 2, -500.0, body, [ true; 3 ]), shares);
	}

	/// The skeleton's rows are a hint, then a dozen, and its bars stay between 40% and 80% and
	/// never move — no shimmer (§1.3).
	#[test]
	fn the_skeleton_is_static_and_between_its_bounds() {
		assert_eq!(skeleton_rows(Some(5), 20), 5);
		assert_eq!(skeleton_rows(None, 20), 12);
		assert_eq!(skeleton_rows(None, 4), 4, "never more than the column shows");
		assert_eq!(skeleton_rows(Some(30), 12), 12);

		let widths: Vec<f32> = (0..12).map(skeleton_bar).collect();
		assert!(widths.iter().all(|w| (0.40..=0.80).contains(w)), "{widths:?}");
		assert_eq!(widths, (0..12).map(skeleton_bar).collect::<Vec<_>>(), "the same every frame");
	}

	/// The thumb is proportional, sits inside the track, and a drag on it asks for the offset
	/// under the pointer.
	#[test]
	fn the_scrollbar_thumb_is_proportional_and_draggable() {
		let track = Rect::from_min_size(pos2(600.0, 40.0), vec2(SCROLL_W, 600.0));
		assert!(bar(track, 10.0, 20.0, 0.0).is_none(), "content that fits has no thumb");

		let scroll = bar(track, 100.0, 20.0, 0.0).expect("a thumb");
		assert!((scroll.height - 120.0).abs() < 0.5, "a fifth of the content: 600 * 20/100");
		assert!(scroll.thumb.max.x <= track.max.x - SCROLL_INSET, "and 2px inside the edge");

		let half = bar_offset(&scroll, 100.0, 20.0, 0.6);
		assert!((half - 48.0).abs() < 0.5, "60% down the track is 48 of the 80 rows left: {half}");

		let long = bar(track, 10_000.0, 5.0, 0.0).expect("a tiny thumb");
		assert!(long.height >= SCROLL_MIN, "which is still grabbable");
	}

	fn bar_offset(bar: &Bar, content: f32, visible: f32, at: f32) -> f32 {
		dragged_offset(bar, bar.track.min.y + at * bar.travel + bar.height * 0.5, content, visible)
	}

	/// The nine characters are the bits, in the order a person reads them.
	#[test]
	fn permissions_spell_the_bits_or_nothing() {
		assert_eq!(perm(Some(0o754)), *b"rwxr-xr--");
		assert_eq!(perm(Some(0o000)), *b"---------");
		assert_eq!(perm(None), *b"---------");
	}
}