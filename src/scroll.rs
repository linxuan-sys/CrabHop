
use eframe::egui::MouseWheelUnit;

use crate::tokens::ROW_HEIGHT;

const TAU: f32 = 0.090;

pub(crate) fn wheel_rows(unit: MouseWheelUnit, delta_y: f32) -> f32 {
	let lines = match unit {
		MouseWheelUnit::Line | MouseWheelUnit::Page => delta_y,
		MouseWheelUnit::Point => delta_y / ROW_HEIGHT,
	};

	-lines
}

#[derive(Default)]
pub(crate) struct Scroll {
	pub(crate) offset: f32,
	pub(crate) target: f32,
	pub(crate) len:  usize,
	pub(crate) rows: usize,
	pub(crate) moving: bool,
}

impl Scroll {
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

	pub(crate) fn wheel(&mut self, rows: f32) {
		self.target += rows;
	}

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

pub(crate) fn approach(current: f32, target: f32, dt: f32) -> f32 {
	current + (target - current) * (1.0 - (-dt / TAU).exp())
}

#[cfg(test)]
mod tests {
	use super::*;

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

		let (a, b) = (approach(40.0, 0.0, 0.016), approach(40.0, 0.0, 0.016));
		assert!((approach(a, 0.0, 0.016) - approach(40.0, 0.0, 0.032)).abs() < 0.01);
		assert!(b > 0.0);
	}

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

	#[test]
	fn a_wheel_that_moves_the_content_down_walks_back_toward_the_head() {
		let mut scroll = Scroll { offset: 20.0, target: 20.0, len: 100, rows: 10, moving: false };

		scroll.wheel(wheel_rows(MouseWheelUnit::Line, 3.0));
		assert_eq!(scroll.target, 17.0, "content down is three rows up the list");

		scroll.wheel(wheel_rows(MouseWheelUnit::Line, -2.5));
		assert_eq!(scroll.target, 19.5, "and content up walks down into the list");

		assert_eq!(wheel_rows(MouseWheelUnit::Point, ROW_HEIGHT * 2.0), -2.0, "points become rows");
		assert_eq!(wheel_rows(MouseWheelUnit::Page, 1.0), -1.0, "and a page is a page");

		let mut scroll = Scroll { offset: 20.0, target: 20.0, len: 100, rows: 10, moving: true };
		scroll.wheel(wheel_rows(MouseWheelUnit::Line, 3.0));
		for _ in 0..30 {
			scroll.step(1.0 / 60.0);
		}
		assert!(scroll.offset < 18.0, "the pixels followed: {}", scroll.offset);
	}

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
