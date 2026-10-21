
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub(crate) const STAY: Duration = Duration::from_secs(8);
pub(crate) const STAY_UNDO: Duration = Duration::from_secs(10);
pub(crate) const FADE: Duration = Duration::from_millis(200);
const SLIDE: Duration = Duration::from_millis(150);
const MAX: usize = 3;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Kind {
	Ok,
	Warn,
	Err,
}

#[derive(Clone, PartialEq)]
pub(crate) enum Undo {
	BookmarkSave(String),
	BookmarkDelete {
		name: String,
		path: PathBuf,
	},
	Trash(Vec<PathBuf>),
	Paste {
		landed: Vec<PathBuf>,
	},
	Moved {
		pairs: Vec<(PathBuf, PathBuf)>,
	},
	Rename {
		from: PathBuf,
		to:   PathBuf,
	},
	Link(Vec<PathBuf>),
	FrecencyDelete {
		path:  PathBuf,
		score: f64,
		last:  u64,
	},
}

impl Undo {
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn label(&self) -> &'static str {
		match self {
			Undo::BookmarkSave(_) => "bookmark-save",
			Undo::BookmarkDelete { .. } => "bookmark-delete",
			Undo::Trash(_) => "trash",
			Undo::Paste { .. } => "paste",
			Undo::Moved { .. } => "moved",
			Undo::Rename { .. } => "rename",
			Undo::Link(_) => "link",
			Undo::FrecencyDelete { .. } => "frecency-delete",
		}
	}
}

pub(crate) struct Toast {
	pub(crate) text: String,
	pub(crate) kind: Kind,
	pub(crate) undo: Option<Undo>,
	born:            Instant,
}

#[derive(Default)]
pub(crate) struct Toasts {
	items: VecDeque<Toast>,
}

impl Toasts {
	pub(crate) fn push(&mut self, text: impl Into<String>, kind: Kind) {
		self.push_with_undo(text, kind, None);
	}

	pub(crate) fn push_with_undo(&mut self, text: impl Into<String>, kind: Kind, undo: Option<Undo>) {
		if self.items.len() >= MAX {
			self.items.pop_front();
		}

		self.items.push_back(Toast { text: text.into(), kind, undo, born: Instant::now() });
	}

	pub(crate) fn is_empty(&self) -> bool {
		self.items.is_empty()
	}

	pub(crate) fn items(&self) -> impl Iterator<Item = &Toast> {
		self.items.iter()
	}

	pub(crate) fn len(&self) -> usize {
		self.items.len()
	}

	pub(crate) fn tick(&mut self, now: Instant) -> bool {
		self.items.retain(|toast| now.duration_since(toast.born) < toast.stay() + FADE);
		!self.items.is_empty()
	}

	pub(crate) fn take_undo(&mut self, at: usize) -> Option<Undo> {
		self.items.get_mut(at)?.undo.take()
	}

	pub(crate) fn take_matching(&mut self, undo: &Undo) {
		for toast in &mut self.items {
			if toast.undo.as_ref() == Some(undo) {
				toast.undo = None;
			}
		}
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn newest_has_undo(&self) -> bool {
		self.items.back().is_some_and(|toast| toast.undo.is_some())
	}

	pub(crate) fn dismiss(&mut self, at: usize) {
		if at < self.items.len() {
			self.items.remove(at);
		}
	}

	pub(crate) fn clear(&mut self) { self.items.clear(); }
}

impl Toast {
	pub(crate) fn age(&self, now: Instant) -> Duration {
		now.duration_since(self.born)
	}

	pub(crate) fn stay(&self) -> Duration {
		if self.undo.is_some() { STAY_UNDO } else { STAY }
	}
}

pub(crate) fn slide(age: Duration) -> f32 {
	if age >= SLIDE {
		return 0.0;
	}

	let t = age.as_secs_f32() / SLIDE.as_secs_f32();
	24.0 * (1.0 - t) * (1.0 - t)
}

pub(crate) fn alpha(age: Duration, stay: Duration) -> f32 {
	if age <= stay {
		return 1.0;
	}

	(1.0 - (age - stay).as_secs_f32() / FADE.as_secs_f32()).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_toast_slides_in_then_fades_out() {
		assert!((slide(Duration::ZERO) - 24.0).abs() < 0.01, "it enters from the right");
		assert!(slide(Duration::from_millis(150)) == 0.0, "and is seated after 150ms");
		assert!(slide(Duration::from_millis(75)) < 24.0 && slide(Duration::from_millis(75)) > 0.0);

		assert_eq!(alpha(STAY, STAY), 1.0, "whole through its stay");
		assert!(alpha(STAY + FADE / 2, STAY) < 0.6, "halfway gone at half the fade");
		assert_eq!(alpha(STAY + FADE, STAY), 0.0);

		assert_eq!(STAY, Duration::from_secs(8), "a note stands eight seconds");
		assert_eq!(STAY_UNDO, Duration::from_secs(10), "and an undo's ten — the user's own cut");
		assert_eq!(alpha(Duration::from_secs(9), STAY), 0.0, "a plain note is gone by the ninth");
		assert_eq!(alpha(Duration::from_secs(9), STAY_UNDO), 1.0, "an undo's still whole there");

		let mut toasts = Toasts::default();
		toasts.push("plain", Kind::Ok);
		toasts.push_with_undo("undo", Kind::Ok, Some(Undo::BookmarkSave("k".to_owned())));
		let born = Instant::now();
		assert!(toasts.tick(born + Duration::from_secs(9)), "at the ninth the undo's still there");
		assert_eq!(toasts.len(), 1, "and the plain one is not");
		assert!(!toasts.tick(born + STAY_UNDO + FADE), "by the tenth and a fade, none");
	}

	#[test]
	fn a_toast_can_be_dismissed() {
		let mut toasts = Toasts::default();
		toasts.push("one", Kind::Warn);
		toasts.push("two", Kind::Warn);
		toasts.push("three", Kind::Warn);

		toasts.dismiss(1);
		let said: Vec<&str> = toasts.items().map(|toast| toast.text.as_str()).collect();
		assert_eq!(said, vec![ "one", "three" ], "the seat is gone, the rest keeps its order");

		toasts.dismiss(9);
		assert_eq!(toasts.len(), 2, "a seat past the end changes nothing");

		toasts.clear();
		assert!(toasts.is_empty(), "and `Esc` takes them all");
	}

	#[test]
	fn the_stack_holds_three_and_pushes_the_oldest_out() {
		let mut toasts = Toasts::default();
		toasts.push("一", Kind::Ok);
		toasts.push("二", Kind::Ok);
		toasts.push("三", Kind::Ok);
		toasts.push("四", Kind::Err);

		let texts: Vec<&str> = toasts.items().map(|toast| toast.text.as_str()).collect();
		assert_eq!(texts, vec![ "二", "三", "四" ]);
		assert_eq!(toasts.len(), 3);
	}

	#[test]
	fn an_undo_comes_out_only_once() {
		let mut toasts = Toasts::default();
		toasts.push_with_undo("已加入书签", Kind::Ok, Some(Undo::BookmarkSave("k".to_owned())));

		assert!(toasts.take_undo(0).is_some());
		assert!(toasts.take_undo(0).is_none(), "the link does not survive its own click");
		assert!(toasts.take_undo(9).is_none(), "and a seat that is not there holds nothing");
	}

	#[test]
	fn one_action_loses_all_of_its_links_at_once() {
		let action = Undo::Paste { landed: vec![ PathBuf::from("/tmp/a.txt") ] };
		let mut toasts = Toasts::default();
		toasts.push_with_undo("已复制 1 项", Kind::Ok, Some(action.clone()));
		toasts.push_with_undo("已复制 1 项", Kind::Ok, Some(action.clone()));
		toasts.push_with_undo("已移动 1 项", Kind::Ok, Some(Undo::Paste { landed: vec![ PathBuf::from("/tmp/b.txt") ] }));

		toasts.take_matching(&action);
		assert!(!toasts.items().next().expect("one").undo.is_some(), "the first link is gone");
		assert!(!toasts.items().nth(1).expect("two").undo.is_some(), "and the second, the same action");
		assert!(toasts.items().nth(2).expect("three").undo.is_some(), "another action keeps its own");

		assert!(toasts.newest_has_undo(), "the newest note is the one that says so");
	}
}
