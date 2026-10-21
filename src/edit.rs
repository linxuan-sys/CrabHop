//! The one editing model the window's inputs share: a buffer and a cursor, moved by keys and fed by
//! the layout's own text. The bulk editor, the command line, the open-with field and the fuzzy
//! query are all this — so what a backspace means is decided once, and can be read and tested
//! without a window.

use eframe::egui::{Key, Modifiers};

pub(crate) struct Edit {
	/// The text so far.
	pub(crate) text:   String,
	/// A byte index into it, always on a character boundary.
	pub(crate) cursor: usize,
	/// Every character is selected (Ctrl+A, P21): the next thing typed replaces the whole text —
	/// a field's own select-all, kept as one flag rather than a range to walk beside the cursor.
	pub(crate) all:    bool,
}

impl Edit {
	pub(crate) fn new(text: String) -> Self {
		let cursor = text.len();
		Self { text, cursor, all: false }
	}

	/// Ctrl+A (P21): everything is selected, so the next typed character starts over — and
	/// backspace takes it all away. An empty field has nothing to select.
	pub(crate) fn select_all(&mut self) {
		self.all = !self.text.is_empty();
	}

	/// What the layout typed: inserted at the cursor, and the cursor follows. Control characters
	/// are dropped — a text event carries `\r` for Enter, and the key itself is what decides that.
	pub(crate) fn type_text(&mut self, typed: &str) {
		self.insert_filtered(typed, false);
	}

	/// The bulk editor's own typing: there a newline is text, because a whole list pasted in is a
	/// real thing a person does.
	pub(crate) fn type_lines(&mut self, typed: &str) {
		self.insert_filtered(typed, true);
	}

	fn insert_filtered(&mut self, typed: &str, newlines: bool) {
		let mut clean = String::with_capacity(typed.len());
		clean.extend(typed.chars().filter(|c| !c.is_control() || (newlines && *c == '\n')));
		if clean.is_empty() {
			return;
		}

		// a select-all is replaced by what is typed (P21)
		if self.all {
			self.text.clear();
			self.cursor = 0;
			self.all = false;
		}

		self.text.insert_str(self.cursor, &clean);
		self.cursor += clean.len();
	}

	/// The bulk editor's Enter: a new line at the cursor.
	pub(crate) fn newline(&mut self) {
		self.text.insert(self.cursor, '\n');
		self.cursor += 1;
	}

	pub(crate) fn backspace(&mut self) -> bool {
		// with everything selected, one backspace takes everything (P21)
		if self.all {
			self.all = false;
			let taken = !self.text.is_empty();
			self.text.clear();
			self.cursor = 0;
			return taken;
		}

		let Some(previous) = self.text[..self.cursor].char_indices().next_back().map(|(at, _)| at) else {
			return false;
		};

		self.text.remove(previous);
		self.cursor = previous;
		true
	}

	pub(crate) fn delete(&mut self) -> bool {
		if self.cursor >= self.text.len() {
			return false;
		}

		self.text.remove(self.cursor);
		true
	}

	pub(crate) fn left(&mut self) -> bool {
		let Some(previous) = self.text[..self.cursor].char_indices().next_back().map(|(at, _)| at) else {
			return false;
		};

		self.cursor = previous;
		true
	}

	pub(crate) fn right(&mut self) -> bool {
		let Some(next) = self.text[self.cursor..].chars().next() else { return false };

		self.cursor += next.len_utf8();
		true
	}

	/// The line the cursor is on: its byte range, the newline itself not included.
	fn line_bounds(&self, at: usize) -> (usize, usize) {
		let start = self.text[..at].rfind('\n').map_or(0, |i| i + 1);
		let end = self.text[at..].find('\n').map_or(self.text.len(), |i| at + i);
		(start, end)
	}

	pub(crate) fn home(&mut self) -> bool {
		let (start, _) = self.line_bounds(self.cursor);
		let moved = start != self.cursor;
		self.cursor = start;
		moved
	}

	pub(crate) fn end(&mut self) -> bool {
		let (_, end) = self.line_bounds(self.cursor);
		let moved = end != self.cursor;
		self.cursor = end;
		moved
	}

	/// How many characters the cursor sits past a line's start — the column a vertical move keeps.
	fn column(&self, start: usize) -> usize {
		self.text[start..self.cursor].chars().count()
	}

	/// Where a column falls on a line: the column if it fits, the line's own end otherwise.
	fn at_column(&self, start: usize, end: usize, column: usize) -> usize {
		for (taken, (at, _)) in self.text[start..end].char_indices().enumerate() {
			if taken == column {
				return start + at;
			}
		}

		end
	}

	pub(crate) fn up(&mut self) -> bool {
		let (start, _) = self.line_bounds(self.cursor);
		if start == 0 {
			return false;
		}

		let previous_end = start - 1;
		let previous_start = self.text[..previous_end].rfind('\n').map_or(0, |i| i + 1);
		let column = self.column(start);
		self.cursor = self.at_column(previous_start, previous_end, column);
		true
	}

	pub(crate) fn down(&mut self) -> bool {
		let (_, end) = self.line_bounds(self.cursor);
		if end == self.text.len() {
			return false;
		}

		let next_start = end + 1;
		let next_end = self.text[next_start..].find('\n').map_or(self.text.len(), |i| next_start + i);
		// after the line the cursor is on, the column is the cursor's own — recompute from the
		// line it was on so the jump keeps its place
		let (start, _) = self.line_bounds(self.cursor);
		let column = self.column(start);
		self.cursor = self.at_column(next_start, next_end, column);
		true
	}

	/// Ctrl+u: everything from the cursor back to the start of its line.
	pub(crate) fn kill_start(&mut self) -> bool {
		let (start, _) = self.line_bounds(self.cursor);
		if start == self.cursor {
			return false;
		}

		self.text.replace_range(start..self.cursor, "");
		self.cursor = start;
		true
	}

	/// Ctrl+k: everything from the cursor to the end of its line.
	pub(crate) fn kill_end(&mut self) -> bool {
		let (_, end) = self.line_bounds(self.cursor);
		if end == self.cursor {
			return false;
		}

		self.text.replace_range(self.cursor..end, "");
		true
	}

	/// Ctrl+w: the word behind the cursor, whitespace and all — the way a shell deletes it.
	pub(crate) fn kill_word(&mut self) -> bool {
		let head = &self.text[..self.cursor];
		let trimmed = head.trim_end_matches(|c: char| c.is_whitespace());

		let at = match trimmed.rfind(|c: char| c.is_whitespace()) {
			Some(space) => head[..=space].len(),
			None => 0,
		};

		if at == self.cursor {
			return false;
		}

		self.text.replace_range(at..self.cursor, "");
		self.cursor = at;
		true
	}

	/// The editing keys every field shares — the arrows, the deletions and the kills. Enter is not
	/// here: a single-line field has its own use for it, and the bulk editor makes its own newline.
	pub(crate) fn key(&mut self, key: Key, modifiers: Modifiers) -> bool {
		if modifiers.ctrl {
			// the select-all itself is the caller's (P21): it is not a reason to drop the selection
			// it just made
			if key == Key::A {
				return false;
			}

			// anything else gives the selection up: a cursor that walks is not a cursor that holds
			// everything
			self.all = false;
			return match key {
				Key::U => self.kill_start(),
				Key::K => self.kill_end(),
				Key::W => self.kill_word(),
				Key::H => self.backspace(),
				_ => false,
			};
		}

		// anything that walks gives the select-all up (P21)
		self.all = false;
		match key {
			Key::Backspace => self.backspace(),
			Key::Delete => self.delete(),
			Key::ArrowLeft => self.left(),
			Key::ArrowRight => self.right(),
			Key::ArrowUp => self.up(),
			Key::ArrowDown => self.down(),
			Key::Home => self.home(),
			Key::End => self.end(),
			_ => false,
		}
	}

	/// The line and column a caret is drawn at: the line index, and the character column in it.
	pub(crate) fn place(&self) -> (usize, usize) {
		let (start, _) = self.line_bounds(self.cursor);
		(self.text[..start].matches('\n').count(), self.column(start))
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Ctrl+A's own contract (P21): everything is selected, the next thing typed replaces it, one
	/// backspace takes it all away, and anything that moves the cursor gives the selection up.
	#[test]
	fn select_all_is_replaced_by_what_is_typed_next() {
		let mut edit = Edit::new("hello".to_owned());
		assert!(!edit.all);

		edit.select_all();
		assert!(edit.all, "the whole text is selected");
		edit.type_text("world");
		assert_eq!(edit.text, "world", "and what is typed replaces it");
		assert_eq!(edit.cursor, 5, "with the cursor after the new text");
		assert!(!edit.all);

		edit.select_all();
		edit.type_text("a");
		assert_eq!(edit.text, "a");

		edit.select_all();
		assert!(edit.backspace(), "a backspace takes the whole selection");
		assert_eq!(edit.text, "");
		assert!(!edit.all);

		let mut empty = Edit::new(String::new());
		empty.select_all();
		assert!(!empty.all, "an empty field has nothing to select");

		let mut edit = Edit::new("abc".to_owned());
		edit.select_all();
		edit.key(Key::ArrowLeft, Modifiers::NONE);
		assert!(!edit.all, "a cursor that walks is not a cursor that holds everything");
	}

	fn typed(text: &str) -> Edit {
		let mut edit = Edit::new(String::new());
		edit.type_text(text);
		edit
	}

	/// Typing lands at the cursor, control characters drop out, and a wide glyph is moved over
	/// whole — the three rules every field in this window shares.
	#[test]
	fn typing_follows_the_cursor_and_never_cuts_a_character() {
		let mut edit = typed("ab");
		edit.type_text("陶");
		assert_eq!(edit.text, "ab陶");
		assert_eq!(edit.cursor, 5);

		edit.left();
		assert_eq!(edit.cursor, 2, "the wide glyph was moved over whole");
		edit.type_text("X\rY");
		assert_eq!(edit.text, "abXY陶", "the carriage return is not text");
		assert_eq!(edit.cursor, 4);

		// the bulk editor takes whole lists: there a newline is text
		let mut lines = Edit::new(String::new());
		lines.type_lines("one\r\ntwo");
		assert_eq!(lines.text, "one\ntwo");
	}

	/// Backspace and delete take one character, wide or not, and stop at the ends.
	#[test]
	fn deleting_takes_one_character_at_a_time() {
		let mut edit = Edit::new("a陶b".to_owned());
		assert!(edit.backspace());
		assert_eq!(edit.text, "a陶");
		assert!(edit.backspace());
		assert_eq!(edit.text, "a", "the wide glyph went in one press");
		assert!(edit.backspace());
		assert_eq!(edit.text, "");
		assert!(!edit.backspace(), "and the buffer's own start is the end of it");

		let mut edit = Edit::new("ab".to_owned());
		edit.cursor = 0;
		assert!(edit.delete());
		assert_eq!((edit.text.as_str(), edit.cursor), ("b", 0));
		assert!(edit.delete());
		assert_eq!(edit.text, "");
		assert!(!edit.delete(), "and the buffer's own end is the end of it");
	}

	/// Home and end are the cursor's own line's, not the buffer's.
	#[test]
	fn home_and_end_know_which_line_the_cursor_is_on() {
		let mut edit = Edit::new("one\ntwo\nthree".to_owned());
		edit.cursor = 6; // inside "two"
		assert!(edit.home());
		assert_eq!(edit.cursor, 4);
		assert!(edit.end());
		assert_eq!(edit.cursor, 7);
		assert!(!edit.end(), "already there");
	}

	/// A vertical move keeps the column, and a shorter line takes it to its own end.
	#[test]
	fn moving_up_and_down_keeps_the_column() {
		let mut edit = Edit::new("longest\ntiny\nmedium".to_owned());
		edit.cursor = 18; // inside `medium`, column 5
		assert!(edit.up(), "from `medium` up to `tiny`");
		assert_eq!(edit.place(), (1, 4), "the shorter line clamps to its end");
		assert!(edit.up());
		assert_eq!(edit.place(), (0, 4), "and the column holds where it fits");
		assert!(!edit.up(), "the top is the top");
		assert!(edit.down());
		assert_eq!(edit.place(), (1, 4));
		assert!(edit.down());
		assert_eq!(edit.place(), (2, 4), "and down keeps it too");
	}

	/// The three kills: to the line's start, to its end, and the word behind the cursor.
	#[test]
	fn the_kills_cut_where_the_shell_says() {
		let mut edit = Edit::new("one two three".to_owned());
		assert!(edit.kill_word());
		assert_eq!(edit.text, "one two ", "the word and the air before the cursor");
		assert!(edit.kill_word());
		assert_eq!(edit.text, "one ");

		let mut edit = Edit::new("one two".to_owned());
		edit.cursor = 3;
		assert!(edit.kill_end());
		assert_eq!(edit.text, "one", "nothing after the cursor");
		assert!(!edit.kill_end());

		let mut edit = Edit::new("keep cut".to_owned());
		edit.cursor = 5;
		assert!(edit.kill_start());
		assert_eq!(edit.text, "cut", "Ctrl+u goes back to the line's own start");
	}

	/// The caret's own place: which line, and how many characters in.
	#[test]
	fn the_caret_reports_its_line_and_column() {
		let mut edit = Edit::new("ab\n陶cd".to_owned());
		assert_eq!(edit.place(), (1, 3), "wide glyphs count as one character");
		edit.cursor = 1;
		assert_eq!(edit.place(), (0, 1));
	}

	/// The shared keys move and cut the way the keymap's own `[input]` section says: arrows,
	/// deletions at both ends, and the three Ctrl kills.
	#[test]
	fn the_keys_move_and_cut_the_way_the_keymaps_input_section_says() {
		let mut edit = Edit::new("one two".to_owned());
		assert!(edit.key(Key::ArrowLeft, Modifiers::NONE));
		assert_eq!(edit.cursor, 6);
		assert!(edit.key(Key::Backspace, Modifiers::NONE));
		assert_eq!(edit.text, "one to");
		assert!(edit.key(Key::Home, Modifiers::NONE));
		assert!(edit.key(Key::Delete, Modifiers::NONE));
		assert_eq!(edit.text, "ne to");
		assert!(edit.key(Key::End, Modifiers::NONE));
		assert!(edit.key(Key::U, Modifiers::CTRL));
		assert_eq!(edit.text, "", "Ctrl+u took the line");
		assert!(!edit.key(Key::U, Modifiers::CTRL), "and there is nothing left to take");

		let mut edit = Edit::new("keep".to_owned());
		assert!(!edit.key(Key::Enter, Modifiers::NONE), "Enter belongs to the field, not the buffer");
	}
}