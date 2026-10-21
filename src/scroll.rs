//! The list's scrolling, owned entirely by this thread: a pixel offset that approaches its target
//! with an exponential decay. Nothing here talks to a background worker — that is the point.

use eframe::egui::MouseWheelUnit;

use crate::tokens::ROW_HEIGHT;

/// How long the approach takes to close its distance: an exponential with this time constant,
/// which reads the same at any frame rate and never overshoots.
const TAU: f32 = 0.090;

/// A wheel event's rows, signed the way the list reads them. egui reports how the *content* should
/// move — a positive `delta.y` moves the content down, revealing what is above it — while a
/// positive offset is deeper into the list, so the two conventions are one negation apart. A
/// wheel pushed "up" therefore walks back toward the head of the list, the way every file manager
/// and every terminal has done it since scrollbars were invented.
pub(crate) fn wheel_rows(unit: MouseWheelUnit, delta_y: f32) -> f32 {
	let lines = match unit {
		MouseWheelUnit::Line | MouseWheelUnit::Page => delta_y,
		MouseWheelUnit::Point => delta_y / ROW_HEIGHT,
	};

	-lines
}

#[derive(Default)]
pub(crate) struct Scroll {
	/// Where the list is, in rows — the fraction of a row is where the pixels come from.
	pub(crate) offset: f32,
	/// Where it is going.
	pub(crate) target: f32,
	/// How many rows there are and how many fit: the clamps.
	pub(crate) len:  usize,
	pub(crate) rows: usize,
	pub(crate) moving: bool,
}

impl Scroll {
	/// One frame of the approach, and whether anything moved. The clamps are looked at here rather
	/// than when the wheel arrives: a directory that is still being read has no length yet, and a
	/// wheel that lands during that must not be thrown away.
	pub(crate) fn step(&mut self, dt: f32) -> bool {
		self.clamp();
		let before = self.offset;
		self.offset = approach(self.offset, self.target, dt);
		self.moving = (self.offset - self.target).abs() > 0.001;

		if !self.moving {
			self.offset = self.target;
		}

		(self.offset - before).abs() > f32::EPSILON || self.moving
	}

	/// A wheel: the target moves, the picture follows.
	pub(crate) fn wheel(&mut self, rows: f32) {
		self.target += rows;
	}

	/// A key: no queue, no settling — the list is where it was asked to be, this frame.
	pub(crate) fn jump(&mut self, rows: f32) {
		self.target = rows;
		self.offset = rows;
		self.clamp();
		self.moving = false;
	}

	pub(crate) fn clamp(&mut self) {
		let max = self.len.saturating_sub(self.rows) as f32;
		self.target = self.target.clamp(0.0, max);
		self.offset = self.offset.clamp(0.0, max);
	}

	/// How far the cursor may be moved before the list has to follow it: the whole point of a
	/// keyboard scroll is that it is decided now.
	pub(crate) fn follow(&mut self, cursor: usize) {
		let cursor = cursor as f32;
		let (top, bottom) = (self.offset, self.offset + self.rows.saturating_sub(1) as f32);

		if cursor < top {
			self.jump(cursor);
		} else if cursor > bottom {
			self.jump(cursor - self.rows.saturating_sub(1) as f32);
		}
	}
}

/// The exponential approach itself.
pub(crate) fn approach(current: f32, target: f32, dt: f32) -> f32 {
	current + (target - current) * (1.0 - (-dt / TAU).exp())
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The approach converges monotonically and never passes its target — the jitter of the old
	/// architecture was two owners of one offset, and this one has a single owner and one direction.
	#[test]
	fn the_approach_is_monotone_and_arrives() {
		let (mut at, target) = (40.0f32, 0.0f32);
		let mut last = at;

		for _ in 0..60 {
			at = approach(at, target, 1.0 / 60.0);
			assert!(at <= last, "towards the target only: {at} after {last}");
			assert!(at >= target, "and never past it: {at}");
			last = at;
		}
		assert!(at < 0.5, "and it arrives: {at}");

		// frame-rate independent: two 16ms frames and one 32ms frame land together
		let (a, b) = (approach(40.0, 0.0, 0.016), approach(40.0, 0.0, 0.016));
		assert!((approach(a, 0.0, 0.016) - approach(40.0, 0.0, 0.032)).abs() < 0.01);
		assert!(b > 0.0);
	}

	/// Three frames of an inertia, read the way the acceptance reads them: each closer to the target
	/// than the last, with nothing turning back — the shape a screenshot sequence has to show.
	#[test]
	fn three_frames_of_inertia_converge() {
		let mut scroll = Scroll { offset: 12.0, target: 0.0, len: 100, rows: 20, moving: true };
		let mut seen = vec![scroll.offset];

		for _ in 0..3 {
			scroll.step(1.0 / 60.0);
			seen.push(scroll.offset);
		}

		assert!(seen[0] > seen[1] && seen[1] > seen[2] && seen[2] > seen[3], "monotone: {seen:?}");
		assert!(scroll.moving, "and still moving");
	}

	/// The wheel is a direction, not a number: what the event asked for is the content moving, and
	/// the offset has to move the other way. The unit test spells the whole contract out.
	#[test]
	fn a_wheel_that_moves_the_content_down_walks_back_toward_the_head() {
		let mut scroll = Scroll { offset: 20.0, target: 20.0, len: 100, rows: 10, moving: false };

		scroll.wheel(wheel_rows(MouseWheelUnit::Line, 3.0));
		assert_eq!(scroll.target, 17.0, "content down is three rows up the list");

		scroll.wheel(wheel_rows(MouseWheelUnit::Line, -2.5));
		assert_eq!(scroll.target, 19.5, "and content up walks down into the list");

		assert_eq!(wheel_rows(MouseWheelUnit::Point, ROW_HEIGHT * 2.0), -2.0, "points become rows");
		assert_eq!(wheel_rows(MouseWheelUnit::Page, 1.0), -1.0, "and a page is a page");

		// the settled offset follows the target the same way: away from the tail
		let mut scroll = Scroll { offset: 20.0, target: 20.0, len: 100, rows: 10, moving: true };
		scroll.wheel(wheel_rows(MouseWheelUnit::Line, 3.0));
		for _ in 0..30 {
			scroll.step(1.0 / 60.0);
		}
		assert!(scroll.offset < 18.0, "the pixels followed: {}", scroll.offset);
	}

	/// The viewport the click bug was missing: once `rows` is the view's own count, seating the
	/// cursor on a row *inside* the viewport moves nothing — the list stays where the person had
	/// it — a row past the bottom scrolls just far enough to show it, and a row above the top
	/// walks the view up to it.
	#[test]
	fn following_the_cursor_only_scrolls_when_it_leaves_the_viewport() {
		let mut scroll = Scroll { offset: 40.0, target: 40.0, len: 100, rows: 20, moving: false };

		scroll.follow(45);
		assert_eq!((scroll.offset, scroll.target), (40.0, 40.0), "inside the view: nothing moves");

		scroll.follow(59);
		assert_eq!((scroll.offset, scroll.target), (40.0, 40.0), "the last visible row is still inside");

		scroll.follow(60);
		assert_eq!(scroll.offset, 41.0, "one past it: one row of scroll — not a jump to the top");

		scroll.follow(12);
		assert_eq!(scroll.offset, 12.0, "above the viewport: the view walks up to it");

		// and the clamp cannot scroll past the end once the viewport has a size
		let mut scroll = Scroll { offset: 0.0, target: 500.0, len: 100, rows: 20, moving: true };
		scroll.clamp();
		assert_eq!(scroll.target, 80.0, "the last page starts at len - rows");
	}

	#[test]
	fn a_keyboard_jump_does_not_queue_behind_anything() {
		let mut scroll = Scroll { offset: 40.0, target: 0.0, len: 100, rows: 20, moving: true };

		scroll.jump(7.0);
		assert_eq!((scroll.offset, scroll.target), (7.0, 7.0), "the wheel's momentum is gone, not queued");
		assert!(!scroll.moving);
	}
}
