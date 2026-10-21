
use std::path::PathBuf;

use eframe::egui::{Key, Modifiers};
use serde::Deserialize;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum SortBy {
	Name,
	Size,
	Mtime,
	Ext,
	Natural,
	Btime,
	Random,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Columns {
	pub(crate) time:  Option<TimeCol>,
	pub(crate) perms: bool,
	pub(crate) owner: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum TimeCol {
	Mtime,
	Btime,
}

impl Default for Columns {
	fn default() -> Self {
		Columns { time: Some(TimeCol::Mtime), perms: false, owner: false }
	}
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Col {
	Mtime,
	Btime,
	Permissions,
	Owner,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ColumnsDo {
	Toggle(Col),
	Show(Col),
	Reset,
}

impl Columns {
	pub(crate) fn toggle(&mut self, col: Col) {
		match col {
			Col::Mtime => self.time = if self.time == Some(TimeCol::Mtime) { None } else { Some(TimeCol::Mtime) },
			Col::Btime => self.time = if self.time == Some(TimeCol::Btime) { None } else { Some(TimeCol::Btime) },
			Col::Permissions => self.perms = !self.perms,
			Col::Owner => self.owner = !self.owner,
		}
	}

	pub(crate) fn show(&mut self, col: Col) {
		match col {
			Col::Mtime => self.time = Some(TimeCol::Mtime),
			Col::Btime => self.time = Some(TimeCol::Btime),
			Col::Permissions => self.perms = true,
			Col::Owner => self.owner = true,
		}
	}

	pub(crate) fn reset(&mut self) {
		*self = Columns::default();
	}
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum RenameAt {
	BeforeExt,
	Start,
	End,
	EmptyAll,
	EmptyStem,
}

#[derive(Clone, PartialEq, Debug)]
pub(crate) enum Action {
	Up,
	Down,
	Top,
	Bottom,
	Parent,
	Enter,
	Toggle,
	Visual,
	Yank {
		cut: bool,
	},
	Unyank,
	Undo,
	TerminalHere,
	MenuHere,
	History {
		back: bool,
	},
	Follow,
	SmartEnter,
	Cd(String),
	Filter,
	Find,
	FindNext {
		forward: bool,
	},
	Link {
		kind: LinkKind,
	},
	LinkPanel,
	Paste {
		force: bool,
	},
	Remove(bool),
	Create,
	BulkCreate,
	Rename(RenameAt),
	Open,
	OpenWith,
	Index,
	Devices(DeviceDo),
	FuzzyJump,
	TrashView,
	Compress,
	Search {
		content: bool,
	},
	Shell {
		block:   bool,
		command: Option<String>,
	},
	Escape,
	Quit,
	Hidden,
	Sort(SortBy, bool),
	Columns(ColumnsDo),
	Help,
	TabCreate,
	TabSwitch(isize, bool),
	TabSwap(isize),
	TabRename,
	TabClose,
	Seek(i32),
	Spot,
	Page {
		full: bool,
		back: bool,
	},
	Symlinks,
	EnterDir,
	Copy(CopyWhat),
	ToggleAll(Option<bool>),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum DeviceDo {
	MountPick,
	MountHere,
	UnmountPick {
		force: bool,
	},
	Remount,
	Jump,
	PrevCwd,
	Uri(&'static str),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum LinkKind {
	Absolute,
	Relative,
	Hard,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum CopyWhat {
	Path,
	Dirname,
	Filename,
	Stem,
	Url,
	DirUrl,
}

#[derive(Clone)]
pub(crate) struct Chord {
	pub on:      Vec<String>,
	pub actions: Vec<Action>,
}

#[derive(Default)]
pub(crate) struct Sequence {
	cands: Vec<usize>,
	times: usize,
}

pub(crate) type Family = (String, String);

#[derive(Deserialize)]
struct Keymap {
	#[serde(default)]
	keys: Vec<Binding>,
}

#[derive(Deserialize)]
struct Binding {
	on:   On,
	run:  Run,
	#[serde(default)]
	desc: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Run {
	One(String),
	Many(Vec<String>),
}

impl Run {
	fn steps(&self) -> Vec<&str> {
		match self {
			Self::One(one) => vec![ one.as_str() ],
			Self::Many(many) => many.iter().map(String::as_str).collect(),
		}
	}
}

#[derive(Deserialize)]
#[serde(untagged)]
enum On {
	One(String),
	Many(Vec<String>),
}

impl On {
	fn chords(&self) -> Vec<String> {
		match self {
			Self::One(one) => vec![ one.clone() ],
			Self::Many(many) => many.clone(),
		}
	}
}

pub(crate) const FACTORY: &str = include_str!("../keymap-default.toml");

pub(crate) struct Keys {
	chords: Vec<Chord>,
	descs:  Vec<String>,
	rows:   Vec<(String, String, String)>,
	runs:   Vec<Vec<Action>>,
	done:   usize,
	total:  usize,
}

impl Keys {
	pub(crate) fn load() -> Self {
		let path = std::env::var_os("HOME")
			.map(|home| PathBuf::from(home).join(".config").join("crabhop").join("keymap.toml"));

		let Some(text) = path.as_ref().and_then(|path| std::fs::read_to_string(path).ok()) else {
			return Self::parse(FACTORY);
		};

		let keys = Self::parse(&text);
		if !keys.chords.is_empty() {
			return keys;
		}

		eprintln!("keymap: {} has no binding this window knows; the factory's own stands", path.as_ref().map_or_else(String::new, |path| path.display().to_string()));
		Self::parse(FACTORY)
	}

	fn parse(text: &str) -> Self {
		let mut chords = Vec::new();
		let mut descs = Vec::new();
		let mut rows = Vec::new();
		let mut runs = Vec::new();
		let (mut done, mut total) = (0, 0);

		let keymap = match toml::from_str::<Keymap>(text) {
			Ok(keymap) => keymap,
			Err(e) => {
				eprintln!("keymap: unreadable ({e}), and no binding is acted on");
				return Self { chords, descs, rows, runs, done, total };
			}
		};

		for binding in keymap.keys {
			let steps = binding.run.steps();
			let actions: Vec<Action> = steps.iter().filter_map(|run| action_of(run)).collect();
			let on = binding.on.chords();

			total += steps.len();
			done += steps.iter().filter(|run| action_of(run).is_some()).count();
			rows.push((on.join(" "), steps.join("; "), binding.desc.clone()));
			runs.push(actions.clone());

			if actions.is_empty() {
				continue;
			}
			chords.push(Chord { on, actions });
			descs.push(binding.desc.clone());
		}

		Self { chords, descs, rows, runs, done, total }
	}

	pub(crate) fn rows(&self) -> &[(String, String, String)] {
		&self.rows
	}

	pub(crate) fn run_at(&self, at: usize) -> Option<Vec<Action>> {
		self.runs.get(at).filter(|actions| !actions.is_empty()).cloned()
	}

	pub(crate) fn family(&self, sequence: &Sequence) -> Vec<Family> {
		sequence
			.cands
			.iter()
			.filter_map(|at| {
				let chord = self.chords.get(*at)?;
				Some((chord.on.get(sequence.times..)?.join(" "), self.descs.get(*at)?.clone()))
			})
			.collect()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn done(&self) -> (usize, usize) {
		(self.done, self.total)
	}

	pub(crate) fn feed(&self, sequence: &mut Option<Sequence>, key: &str) -> Option<Vec<Action>> {
		if let Some(mut open) = sequence.take() {
			open.cands
				.retain(|at| self.chords[*at].on.len() > open.times && self.chords[*at].on[open.times] == key);
			open.times += 1;

			if open.cands.is_empty() {
				return None;
			}
			if open.cands.len() == 1 {
				return Some(self.chords[open.cands[0]].actions.clone());
			}
			if let Some(&at) = open.cands.iter().find(|at| self.chords[**at].on.len() == open.times) {
				return Some(self.chords[at].actions.clone());
			}

			*sequence = Some(open);
			return None;
		}

		let chord = self.chords.iter().find(|chord| chord.on.first().is_some_and(|on| on == key))?;
		if chord.on.len() == 1 {
			return Some(chord.actions.clone());
		}

		*sequence = Some(Sequence {
			cands: self
				.chords
				.iter()
				.enumerate()
				.filter(|(_, shorter)| shorter.on.len() > 1 && shorter.on[0] == *key)
				.map(|(at, _)| at)
				.collect(),
			times: 1,
		});
		None
	}
}

pub(crate) fn chord(key: Key, modifiers: Modifiers, text: Option<&str>) -> Option<String> {
	if let Some(name) = named(key) {
		if modifiers.ctrl || modifiers.alt || modifiers.shift {
			let mut prefix = String::new();
			if modifiers.ctrl {
				prefix.push_str("C-");
			}
			if modifiers.alt {
				prefix.push_str("A-");
			}
			if modifiers.shift {
				prefix.push_str("S-");
			}

			return Some(format!("<{prefix}{}>", name.trim_matches(['<', '>'])));
		}

		return Some(name.to_owned());
	}

	if modifiers.ctrl || modifiers.alt {
		let base = bare(key)?;
		return Some(match (modifiers.ctrl, modifiers.alt) {
			(true, true) => format!("<C-A-{base}>"),
			(true, false) => format!("<C-{base}>"),
			_ => format!("<A-{base}>"),
		});
	}

	if let Some(text) = text {
		let mut characters = text.chars();
		if let (Some(one), None) = (characters.next(), characters.next())
			&& !one.is_control()
		{
			return Some(one.to_string());
		}
	}

	let base = bare(key)?;
	if modifiers.shift {
		if base.as_bytes().first().is_some_and(u8::is_ascii_lowercase) {
			return Some(base.to_uppercase());
		}
		return Some(shifted(base).unwrap_or(base).to_owned());
	}

	Some(base.to_owned())
}

fn named(key: Key) -> Option<&'static str> {
	Some(match key {
		Key::Enter => "<Enter>",
		Key::Space => "<Space>",
		Key::Escape => "<Esc>",
		Key::Tab => "<Tab>",
		Key::Backspace => "<Backspace>",
		Key::Delete => "<Delete>",
		Key::Insert => "<Insert>",
		Key::Home => "<Home>",
		Key::End => "<End>",
		Key::PageUp => "<PageUp>",
		Key::PageDown => "<PageDown>",
		Key::ArrowUp => "<Up>",
		Key::ArrowDown => "<Down>",
		Key::ArrowLeft => "<Left>",
		Key::ArrowRight => "<Right>",
		Key::F1 => "<F1>",
		Key::F2 => "<F2>",
		Key::F3 => "<F3>",
		Key::F4 => "<F4>",
		Key::F5 => "<F5>",
		Key::F6 => "<F6>",
		Key::F7 => "<F7>",
		Key::F8 => "<F8>",
		Key::F9 => "<F9>",
		Key::F10 => "<F10>",
		Key::F11 => "<F11>",
		Key::F12 => "<F12>",
		_ => return None,
	})
}

fn bare(key: Key) -> Option<&'static str> {
	Some(match key {
		Key::A => "a",
		Key::B => "b",
		Key::C => "c",
		Key::D => "d",
		Key::E => "e",
		Key::F => "f",
		Key::G => "g",
		Key::H => "h",
		Key::I => "i",
		Key::J => "j",
		Key::K => "k",
		Key::L => "l",
		Key::M => "m",
		Key::N => "n",
		Key::O => "o",
		Key::P => "p",
		Key::Q => "q",
		Key::R => "r",
		Key::S => "s",
		Key::T => "t",
		Key::U => "u",
		Key::V => "v",
		Key::W => "w",
		Key::X => "x",
		Key::Y => "y",
		Key::Z => "z",
		Key::Num0 => "0",
		Key::Num1 => "1",
		Key::Num2 => "2",
		Key::Num3 => "3",
		Key::Num4 => "4",
		Key::Num5 => "5",
		Key::Num6 => "6",
		Key::Num7 => "7",
		Key::Num8 => "8",
		Key::Num9 => "9",
		Key::Minus => "-",
		Key::Equals => "=",
		Key::Period => ".",
		Key::Comma => ",",
		Key::Semicolon => ";",
		Key::Quote => "'",
		Key::Slash => "/",
		Key::Backslash => "\\",
		Key::OpenBracket => "[",
		Key::CloseBracket => "]",
		Key::Backtick => "`",
		_ => return None,
	})
}

fn shifted(base: &str) -> Option<&'static str> {
	Some(match base {
		"`" => "~",
		"-" => "_",
		"=" => "+",
		"[" => "{",
		"]" => "}",
		"\\" => "|",
		";" => ":",
		"'" => "\"",
		"," => "<",
		"." => ">",
		"/" => "?",
		"1" => "!",
		"2" => "@",
		"3" => "#",
		"4" => "$",
		"5" => "%",
		"6" => "^",
		"7" => "&",
		"8" => "*",
		"9" => "(",
		"0" => ")",
		_ => return None,
	})
}

#[cfg(test)]
pub(crate) fn inventory(text: &str) -> Vec<(String, usize, bool)> {
	let Ok(keymap) = toml::from_str::<Keymap>(text) else { return Vec::new() };

	let mut seen: std::collections::BTreeMap<String, (usize, bool)> = Default::default();
	for binding in keymap.keys {
		for run in binding.run.steps() {
			let Some(name) = run.split_whitespace().next() else { continue };
			let entry = seen.entry(name.to_owned()).or_default();
			entry.0 += 1;
			entry.1 |= action_of(run).is_some();
		}
	}

	seen.into_iter().map(|(name, (count, done))| (name, count, done)).collect()
}

fn shell_command(run: &str) -> Option<String> {
	let mut rest = run.trim_start_matches("shell").to_owned();
	for flag in [ "--block", "--interactive" ] {
		rest = rest.replace(flag, "");
	}

	let command = rest.trim().trim_matches('\'').trim();
	(!command.is_empty()).then(|| command.to_owned())
}

fn gvfs_action(run: &str) -> Option<Action> {
	let what = run.split_whitespace().nth(3)?;

	Some(Action::Devices(match what {
		"select-then-mount" => DeviceDo::MountPick,
		"select-then-unmount" => DeviceDo::UnmountPick { force: run.contains("--force") },
		"remount-current-cwd-device" => DeviceDo::Remount,
		"jump-to-device" => DeviceDo::Jump,
		"jump-back-prev-cwd" => DeviceDo::PrevCwd,
		"add-mount" => DeviceDo::Uri("添加"),
		"edit-mount" => DeviceDo::Uri("编辑"),
		"remove-mount" => DeviceDo::Uri("移除"),
		_ => return None,
	}))
}

fn action_of(run: &str) -> Option<Action> {
	let mut words = run.split_whitespace();
	let name = words.next()?;
	let argument = words.next();

	Some(match name {
		"arrow" => match argument.unwrap_or("next") {
			"next" | "down" => Action::Down,
			"prev" | "up" => Action::Up,
			"first" | "top" => Action::Top,
			"last" | "bot" | "bottom" => Action::Bottom,
			n => match n.parse::<i32>() {
				Ok(n) if n > 1 => Action::Bottom,
				Ok(n) if n < -1 => Action::Top,
				Ok(n) if n < 0 => Action::Up,
				_ => Action::Down,
			},
		},
		"leave" => Action::Parent,
		"enter" => match argument {
			Some("--bypass") => Action::SmartEnter,
			Some("--dir-only") => Action::EnterDir,
			_ => Action::Enter,
		},
		"toggle" => Action::Toggle,
		"visual_mode" => Action::Visual,
		"yank" => Action::Yank { cut: run.contains("--cut") },
		"cut" => Action::Yank { cut: true },
		"unyank" => Action::Unyank,
		"undo" => Action::Undo,
		"terminal" => Action::TerminalHere,
		"menu" => Action::MenuHere,
		"back" => Action::History { back: true },
		"forward" => Action::History { back: false },
		"follow" => Action::Follow,
		"filter" => Action::Filter,
		"find" => Action::Find,
		"find_arrow" => Action::FindNext { forward: !run.contains("--previous") },
		"index" => Action::Index,
		"link" if argument.is_none() => Action::LinkPanel,
		"link" => Action::Link {
			kind: if run.contains("--relative") { LinkKind::Relative } else { LinkKind::Absolute },
		},
		"hardlink" => Action::Link { kind: LinkKind::Hard },
		"cd" => match argument {
			Some("--interactive") => Action::FuzzyJump,
			Some(path) => Action::Cd(path.to_owned()),
			None => Action::Cd("~".to_owned()),
		},
		"paste" => Action::Paste { force: run.contains("--force") },
		"half-up" => Action::Page { full: false, back: true },
		"half-down" => Action::Page { full: false, back: false },
		"page-up" => Action::Page { full: true, back: true },
		"page-down" => Action::Page { full: true, back: false },
		"symlinks" => Action::Symlinks,
		"remove" => Action::Remove(run.contains("--permanently")),
		"create" => Action::Create,
		"bulk_create" => Action::BulkCreate,
		"open" => match argument {
			Some("--interactive") => Action::OpenWith,
			_ => Action::Open,
		},
		"plugin" => match argument {
			Some("open-with-cmd") => Action::OpenWith,
			Some("fzf") | Some("zoxide") => Action::FuzzyJump,
			Some("trash") => Action::TrashView,
			Some("ouch") => Action::Compress,
			Some("mount") => Action::Devices(DeviceDo::MountPick),
			Some("gvfs") => gvfs_action(run)?,
			_ => return None,
		},
		"shell" => Action::Shell { block: run.contains("--block"), command: shell_command(run) },
		"escape" => Action::Escape,
		"search" => Action::Search { content: run.contains("--via=rg") },
		"rename" => {
			let at = if run.contains("--empty=stem") {
				RenameAt::EmptyStem
			} else if run.contains("--empty=all") {
				RenameAt::EmptyAll
			} else if run.contains("--cursor=start") {
				RenameAt::Start
			} else if run.contains("--cursor=end") {
				RenameAt::End
			} else {
				RenameAt::BeforeExt
			};

			Action::Rename(at)
		}
		"quit" => Action::Quit,
		"tab_create" => Action::TabCreate,
		"tab_switch" => Action::TabSwitch(
			argument.and_then(|n| n.parse().ok()).unwrap_or(0),
			run.contains("--relative"),
		),
		"tab_swap" => Action::TabSwap(argument.and_then(|n| n.parse().ok()).unwrap_or(1)),
		"tab_rename" => Action::TabRename,
		"close" => Action::TabClose,
		"tab_close" => Action::TabClose,
		"toggle_all" => Action::ToggleAll(match argument {
			Some("--state=on") => Some(true),
			Some("--state=off") => Some(false),
			_ => None,
		}),
		"copy" => Action::Copy(match run.split_whitespace().last() {
			Some("dirname") => CopyWhat::Dirname,
			Some("filename") => CopyWhat::Filename,
			Some("name_without_ext") => CopyWhat::Stem,
			Some("url") => CopyWhat::Url,
			Some("dirurl") => CopyWhat::DirUrl,
			_ => CopyWhat::Path,
		}),
		"seek" => Action::Seek(argument.and_then(|n| n.parse().ok()).unwrap_or(5)),
		"spot" => Action::Spot,
		"hidden" => match argument {
			Some("toggle") => Action::Hidden,
			_ => return None,
		},
		"sort" => {
			let by = match argument {
				Some("size") => SortBy::Size,
				Some("mtime") => SortBy::Mtime,
				Some("btime") => SortBy::Btime,
				Some("extension") | Some("ext") => SortBy::Ext,
			Some("natural") => SortBy::Natural,
			Some("random") => SortBy::Random,
				Some("alphabetical") | Some("name") => SortBy::Name,
				_ => return None,
			};
			let reverse = run.contains("--reverse=yes");

			Action::Sort(by, reverse)
		}
		"column" => Action::Columns(match argument {
			Some("mtime") => ColumnsDo::Toggle(Col::Mtime),
			Some("btime") => ColumnsDo::Toggle(Col::Btime),
			Some("permissions") | Some("perm") => ColumnsDo::Toggle(Col::Permissions),
			Some("owner") => ColumnsDo::Toggle(Col::Owner),
			_ => return None,
		}),
		"show" => Action::Columns(match argument {
			Some("mtime") => ColumnsDo::Show(Col::Mtime),
			Some("btime") => ColumnsDo::Show(Col::Btime),
			Some("permissions") | Some("perm") => ColumnsDo::Show(Col::Permissions),
			Some("owner") => ColumnsDo::Show(Col::Owner),
			_ => return None,
		}),
		"reset" => {
			if argument != Some("columns") {
				return None;
			}
			Action::Columns(ColumnsDo::Reset)
		}
		"linemode" => Action::Columns(match argument {
			Some("mtime") => ColumnsDo::Toggle(Col::Mtime),
			Some("btime") => ColumnsDo::Toggle(Col::Btime),
			Some("permissions") | Some("perm") => ColumnsDo::Toggle(Col::Permissions),
			Some("owner") => ColumnsDo::Toggle(Col::Owner),
			_ => ColumnsDo::Reset,
		}),
		"help" => Action::Help,
		_ => return None,
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_factory_keymap_has_no_dead_binding() {
		let keys = Keys::parse(FACTORY);

		for (on, run, desc) in keys.rows() {
			for dead in [ "update_config", "whoosh", "plugin bypass", "suspend", "tasks:show" ] {
				assert!(!run.contains(dead), "{on} still asks for {dead}");
			}
			assert!(!desc.is_empty(), "{on} has no words for the help overlay");
		}

		let (done, total) = keys.done();
		assert_eq!(done, total, "every run in the factory file has an action: {done}/{total}");
		assert!(done > 80, "and the table is the real one: {done} runs");
	}

	#[test]
	fn the_factory_keymap_speaks_for_itself() {
		let keys = Keys::parse(FACTORY);

		let mut open = None;
		assert_eq!(keys.feed(&mut open, "j"), Some(vec![ Action::Down ]), "j moves down");
		assert_eq!(keys.feed(&mut open, "k"), Some(vec![ Action::Up ]), "and k up");
		assert!(keys.feed(&mut open, "g").is_none(), "g opens a sequence");
		assert!(open.is_some(), "and it is open");
		assert_eq!(keys.feed(&mut open, "g"), Some(vec![ Action::Top ]), "gg is the top");
		assert!(open.is_none(), "and the sequence is closed again");
		assert!(keys.feed(&mut open, "h").is_some(), "h is a key this list uses");
		assert!(keys.feed(&mut open, "l").is_some(), "and so is l");
		assert!(keys.feed(&mut open, "m").is_none(), "m opens the line/settings family (P21's table)");
		assert!(keys.feed(&mut open, ".").is_some(), "and m . is the hidden-files switch");
	}

	#[test]
	fn the_first_binding_in_the_files_own_order_decides() {
		let single_first = r#"
			keys = [
				{ on = "t", run = "tab_create", desc = "" },
				{ on = [ "t", "r" ], run = "tab_rename", desc = "" },
			]
		"#;
		let keys = Keys::parse(single_first);
		let mut open = None;
		assert_eq!(keys.feed(&mut open, "t"), Some(vec![ Action::TabCreate ]), "t acts at once");
		assert!(open.is_none(), "and nothing is left waiting");

		let chord_first = r#"
			keys = [
				{ on = [ "t", "r" ], run = "tab_rename", desc = "" },
				{ on = "t", run = "tab_create", desc = "" },
			]
		"#;
		let keys = Keys::parse(chord_first);
		let mut open = None;
		assert!(keys.feed(&mut open, "t").is_none(), "t opens the sequence instead");
		assert_eq!(keys.feed(&mut open, "r"), Some(vec![ Action::TabRename ]), "and t r renames");
		assert!(open.is_none());
	}

	#[test]
	fn a_sequence_completes_at_the_shortest_binding_that_fits() {
		let text = r#"
			keys = [
				{ on = [ "g", "h" ], run = "help", desc = "" },
				{ on = [ "g", "h", "x" ], run = "tab_create", desc = "" },
				{ on = [ "g", "a" ], run = "quit", desc = "" },
			]
		"#;
		let keys = Keys::parse(text);
		let mut open = None;
		assert!(keys.feed(&mut open, "g").is_none(), "g waits");
		assert_eq!(keys.feed(&mut open, "h"), Some(vec![ Action::Help ]), "the two-key head of the three lands");
		assert!(open.is_none(), "and the sequence is over");

		let mut open = None;
		assert!(keys.feed(&mut open, "g").is_none());
		assert_eq!(keys.feed(&mut open, "a"), Some(vec![ Action::Quit ]), "a lone candidate is executed");

		let mut open = None;
		assert!(keys.feed(&mut open, "g").is_none());
		assert!(keys.feed(&mut open, "z").is_none(), "a key nothing continues closes the sequence");
		assert!(open.is_none());
	}

	#[test]
	fn a_keystroke_is_named_the_way_the_keymap_spells_it() {
		assert_eq!(chord(Key::J, Modifiers::NONE, Some("j")), Some("j".to_owned()));
		assert_eq!(chord(Key::J, Modifiers::SHIFT, Some("J")), Some("J".to_owned()));
		assert_eq!(chord(Key::Minus, Modifiers::SHIFT, Some("_")), Some("_".to_owned()), "what was typed");
		assert_eq!(chord(Key::Minus, Modifiers::SHIFT, None), Some("_".to_owned()), "and without a layout");
		assert_eq!(chord(Key::Backtick, Modifiers::SHIFT, None), Some("~".to_owned()));
		assert_eq!(chord(Key::C, Modifiers::CTRL, Some("c")), Some("<C-c>".to_owned()));
		assert_eq!(chord(Key::K, Modifiers::ALT, None), Some("<A-k>".to_owned()));
		assert_eq!(chord(Key::Enter, Modifiers::NONE, Some("\r")), Some("<Enter>".to_owned()));
		assert_eq!(chord(Key::Enter, Modifiers::SHIFT, None), Some("<S-Enter>".to_owned()), "a held shift is in the name");
		assert_eq!(chord(Key::PageUp, Modifiers::SHIFT, None), Some("<S-PageUp>".to_owned()));
		assert_eq!(chord(Key::Space, Modifiers::NONE, Some(" ")), Some("<Space>".to_owned()));
		assert_eq!(chord(Key::F5, Modifiers::NONE, None), Some("<F5>".to_owned()));
	}

	#[test]
	fn an_unknown_run_is_ignored_and_the_keymap_still_loads() {
		let text = r#"
			keys = [
				{ on = [ "z" ], run = "plugin shell --some-argument", desc = "not ours" },
				{ on = [ "j" ], run = "arrow 1", desc = "down" },
			]
		"#;

		let keys = Keys::parse(text);
		let mut open = None;
		assert_eq!(keys.feed(&mut open, "z"), None, "no action, and no stub either");
		assert_eq!(keys.feed(&mut open, "j"), Some(vec![ Action::Down ]), "the map around it still parsed");
	}

	#[test]
	fn the_slash_opens_the_find_box_in_the_factory_table() {
		let keys = Keys::parse(FACTORY);
		let mut open = None;

		assert_eq!(keys.feed(&mut open, "/"), Some(vec![ Action::Find ]), "`/` opens the find box");
		assert_eq!(keys.feed(&mut open, "n"), Some(vec![ Action::FindNext { forward: true } ]));
		assert_eq!(keys.feed(&mut open, "N"), Some(vec![ Action::FindNext { forward: false } ]));
	}

	#[test]
	fn the_link_panel_has_the_bare_verb_and_the_kinds_keep_their_spellings() {
		assert_eq!(action_of("link"), Some(Action::LinkPanel));
		assert_eq!(action_of("link --absolute"), Some(Action::Link { kind: LinkKind::Absolute }));
		assert_eq!(action_of("link --relative"), Some(Action::Link { kind: LinkKind::Relative }));
		assert_eq!(action_of("hardlink"), Some(Action::Link { kind: LinkKind::Hard }));
	}

	#[test]
	fn the_find_family_reads_its_spellings() {
		assert_eq!(action_of("find --smart"), Some(Action::Find));
		assert_eq!(action_of("find_arrow"), Some(Action::FindNext { forward: true }));
		assert_eq!(action_of("find_arrow --previous"), Some(Action::FindNext { forward: false }));
	}

	#[test]
	fn the_new_linemodes_and_the_btime_sort_read_their_names() {
		assert_eq!(action_of("column permissions"), Some(Action::Columns(ColumnsDo::Toggle(Col::Permissions))));
		assert_eq!(action_of("column owner"), Some(Action::Columns(ColumnsDo::Toggle(Col::Owner))));
		assert_eq!(action_of("column mtime"), Some(Action::Columns(ColumnsDo::Toggle(Col::Mtime))));
		assert_eq!(action_of("column btime"), Some(Action::Columns(ColumnsDo::Toggle(Col::Btime))));
		assert_eq!(action_of("show mtime"), Some(Action::Columns(ColumnsDo::Show(Col::Mtime))));
		assert_eq!(action_of("reset columns"), Some(Action::Columns(ColumnsDo::Reset)));
		assert_eq!(action_of("column size"), None, "the size column has no switch of its own (P33⑲④)");
		assert_eq!(action_of("reset cable"), None);
		assert_eq!(action_of("toggle"), Some(Action::Toggle), "`toggle` stays the selection's own word");
		assert_eq!(action_of("linemode permissions"), Some(Action::Columns(ColumnsDo::Toggle(Col::Permissions))));
		assert_eq!(action_of("linemode btime"), Some(Action::Columns(ColumnsDo::Toggle(Col::Btime))));
		assert_eq!(action_of("linemode owner"), Some(Action::Columns(ColumnsDo::Toggle(Col::Owner))));
		assert_eq!(action_of("linemode none"), Some(Action::Columns(ColumnsDo::Reset)), "the old word means the default set now");
		assert_eq!(action_of("linemode size"), Some(Action::Columns(ColumnsDo::Reset)), "and so does the size-only one");
		assert_eq!(action_of("sort btime --reverse=no"), Some(Action::Sort(SortBy::Btime, false)));
		assert_eq!(action_of("sort btime --reverse=yes"), Some(Action::Sort(SortBy::Btime, true)));
	}

	#[test]
	fn the_movement_and_switch_verbs_read_their_spellings() {
		assert_eq!(action_of("half-up"), Some(Action::Page { full: false, back: true }));
		assert_eq!(action_of("half-down"), Some(Action::Page { full: false, back: false }));
		assert_eq!(action_of("page-up"), Some(Action::Page { full: true, back: true }));
		assert_eq!(action_of("page-down"), Some(Action::Page { full: true, back: false }));
		assert_eq!(action_of("symlinks"), Some(Action::Symlinks));
		assert_eq!(action_of("enter --dir-only"), Some(Action::EnterDir));
		assert_eq!(action_of("enter"), Some(Action::Enter), "the plain enter still opens");
		assert_eq!(action_of("paste"), Some(Action::Paste { force: false }));
		assert_eq!(action_of("paste --force"), Some(Action::Paste { force: true }));
	}

	#[test]
	fn the_family_a_key_opens_reads_the_rest_of_each_chord_and_its_description() {
		let text = r#"
			keys = [
				{ on = [ "g", "h" ], run = "cd ~", desc = "主目录" },
				{ on = [ "g", "a" ], run = "cd ~/Desktop/", desc = "桌面" },
				{ on = [ "g", "t" ], run = "plugin trash", desc = "回收站" },
				{ on = "j", run = "arrow 1", desc = "down" },
			]
		"#;

		let keys = Keys::parse(text);
		let mut open = None;
		assert!(keys.feed(&mut open, "g").is_none(), "g opens the family");
		assert_eq!(
			keys.family(open.as_ref().expect("the sequence is open")),
			vec![
				("h".to_owned(), "主目录".to_owned()),
				("a".to_owned(), "桌面".to_owned()),
				("t".to_owned(), "回收站".to_owned()),
			],
			"the keys still to press, and what the keymap says they do"
		);

		assert_eq!(keys.feed(&mut open, "a"), Some(vec![ Action::Cd("~/Desktop/".to_owned()) ]));
		assert!(open.is_none());
	}

	#[test]
	fn a_crabhop_keyfile_reads_its_own_shape() {
		let text = r#"
			[[keys]]
			on   = "j"
			run  = "arrow 1"
			desc = "向下"

			[[keys]]
			on   = [ "g", "h" ]
			run  = "cd ~"
			desc = "主目录"

			[[keys]]
			on   = [ ",", "m" ]
			run  = [ "sort mtime --reverse=no", "linemode mtime" ]
			desc = "按修改时间排序(正序)"
		"#;

		let keys = Keys::parse(text);
		let mut open = None;
		assert_eq!(keys.feed(&mut open, "j"), Some(vec![ Action::Down ]));
		assert!(keys.feed(&mut open, "g").is_none(), "a chord opens");
		assert_eq!(keys.feed(&mut open, "h"), Some(vec![ Action::Cd("~".to_owned()) ]));

		let rows = keys.rows();
		assert_eq!(rows.len(), 3, "every binding is in the help overlay's table");
		assert_eq!(rows[0], ("j".to_owned(), "arrow 1".to_owned(), "向下".to_owned()));
		assert_eq!(rows[2].1, "sort mtime --reverse=no; linemode mtime", "a binding's several runs read as one row");
	}

	#[test]
	fn every_factory_binding_can_be_reached_by_its_own_keys() {
		let keys = Keys::parse(FACTORY);

		for chord in &keys.chords {
			let mut open = None;
			let mut answered = None;
			for key in &chord.on {
				answered = keys.feed(&mut open, key);
				if answered.is_some() {
					break;
				}
			}

			assert_eq!(answered, Some(chord.actions.clone()), "unreachable in the factory file: {}", chord.on.join(" "));
		}
	}

	#[test]
	fn the_inventory_lists_what_the_keymap_asks_for() {
		let inventory = inventory(FACTORY);

		assert!(!inventory.is_empty());
		let done: Vec<&str> = inventory.iter().filter(|(_, _, done)| *done).map(|(name, _, _)| name.as_str()).collect();
		assert!(done.contains(&"arrow"), "movement is done: {done:?}");
		assert!(done.contains(&"hidden"), "the switch is done");
		assert!(done.contains(&"sort"), "and the sorting");
		assert!(done.contains(&"help"), "and the help");
		assert!(done.contains(&"tab_create"), "and the tabs");

		for (name, count, acted) in &inventory {
			assert!(*count > 0, "{name} appears");
			assert!(*acted, "{name} is a verb this window does not act on");
		}
		println!("inventory: {inventory:?}");
	}

	#[test]
	fn the_rename_familys_arguments_read_as_their_seats() {
		let reads = [
			("rename -- --cursor=before_ext", RenameAt::BeforeExt),
			("rename --cursor=before_ext", RenameAt::BeforeExt),
			("rename -- --cursor=start", RenameAt::Start),
			("rename -- --cursor=end", RenameAt::End),
			("rename -- --empty=all", RenameAt::EmptyAll),
			("rename -- --empty=stem --cursor=start", RenameAt::EmptyStem),
		];
		for (run, at) in reads {
			assert_eq!(action_of(run), Some(Action::Rename(at)), "{run}");
		}

		assert_eq!(action_of("bulk_create"), Some(Action::BulkCreate), "and the bulk editor is its own action");
	}

	#[test]
	fn the_shell_runs_are_read_with_their_flags_and_their_commands() {
		assert_eq!(action_of("shell --interactive"), Some(Action::Shell { block: false, command: None }));
		assert_eq!(action_of("shell --block --interactive"), Some(Action::Shell { block: true, command: None }));
		assert_eq!(
			action_of("shell --block 'sh $HOME/.config/yazi/scripts/update_config.sh'"),
			Some(Action::Shell { block: true, command: Some("sh $HOME/.config/yazi/scripts/update_config.sh".to_owned()) }),
			"the quoted command survives as written"
		);
		assert_eq!(action_of("escape --search"), Some(Action::Escape), "and escaping is one action");
	}

	#[test]
	fn open_reads_its_spellings() {
		assert_eq!(action_of("open --interactive"), Some(Action::OpenWith));
		assert_eq!(action_of("plugin open-with-cmd --block"), Some(Action::OpenWith));
		assert_eq!(action_of("open"), Some(Action::Open), "plain open acts");
	}

	#[test]
	fn the_new_sorts_read_their_names() {
		assert_eq!(action_of("sort natural --reverse=no"), Some(Action::Sort(SortBy::Natural, false)));
		assert_eq!(action_of("sort natural --reverse=yes"), Some(Action::Sort(SortBy::Natural, true)));
		assert_eq!(action_of("sort random --reverse=no"), Some(Action::Sort(SortBy::Random, false)));
	}

	#[test]
	fn the_device_family_reads_its_runs() {
		assert_eq!(action_of("plugin gvfs -- select-then-mount --jump"), Some(Action::Devices(DeviceDo::MountPick)));
		assert_eq!(action_of("plugin gvfs -- select-then-unmount --eject"), Some(Action::Devices(DeviceDo::UnmountPick { force: false })));
		assert_eq!(action_of("plugin gvfs -- select-then-unmount --eject --force"), Some(Action::Devices(DeviceDo::UnmountPick { force: true })));
		assert_eq!(action_of("plugin gvfs -- remount-current-cwd-device"), Some(Action::Devices(DeviceDo::Remount)));
		assert_eq!(action_of("plugin gvfs -- jump-to-device --automount"), Some(Action::Devices(DeviceDo::Jump)));
		assert_eq!(action_of("plugin gvfs -- jump-back-prev-cwd"), Some(Action::Devices(DeviceDo::PrevCwd)));
		assert_eq!(action_of("plugin gvfs -- add-mount"), Some(Action::Devices(DeviceDo::Uri("添加"))));
		assert_eq!(action_of("plugin mount"), Some(Action::Devices(DeviceDo::MountPick)), "the old name still opens it");
	}

	#[test]
	fn the_cd_family_reads_its_paths() {
		assert_eq!(action_of("cd ~"), Some(Action::Cd("~".to_owned())));
		assert_eq!(action_of("cd ~/Desktop/"), Some(Action::Cd("~/Desktop/".to_owned())));
		assert_eq!(action_of("cd --interactive"), Some(Action::FuzzyJump), "the interactive one is the jump panel");
		assert_eq!(action_of("plugin trash"), Some(Action::TrashView), "and `g t` is the bin's view");
		assert_eq!(action_of("plugin recycle-bin"), None, "the old `R b` alias is gone (P19)");
	}

	#[test]
	fn the_copy_family_reads_its_six_spellings() {
		assert_eq!(action_of("copy -- path"), Some(Action::Copy(CopyWhat::Path)));
		assert_eq!(action_of("copy -- dirname"), Some(Action::Copy(CopyWhat::Dirname)));
		assert_eq!(action_of("copy -- filename"), Some(Action::Copy(CopyWhat::Filename)));
		assert_eq!(action_of("copy -- name_without_ext"), Some(Action::Copy(CopyWhat::Stem)));
		assert_eq!(action_of("copy url"), Some(Action::Copy(CopyWhat::Url)));
		assert_eq!(action_of("copy dirurl"), Some(Action::Copy(CopyWhat::DirUrl)));
	}

	#[test]
	fn the_whoosh_family_has_no_way_in() {
		for run in [ "plugin whoosh jump_by_key", "plugin whoosh save", "plugin whoosh jump_key_k" ] {
			assert_eq!(action_of(run), None, "{run}");
		}
		assert_eq!(action_of("index"), Some(Action::Index), "and `i` opens the index");
		assert_eq!(action_of("plugin fzf"), Some(Action::FuzzyJump), "and `z` still reaches the same panel");
		assert_eq!(action_of("plugin zoxide"), Some(Action::FuzzyJump));
	}

	#[test]
	fn the_search_runs_read_their_via() {
		assert_eq!(action_of("search --via=fd"), Some(Action::Search { content: false }));
		assert_eq!(action_of("search --via=rg"), Some(Action::Search { content: true }));
	}

	#[test]
	fn the_removals_read_their_flags() {
		assert_eq!(action_of("remove"), Some(Action::Remove(false)));
		assert_eq!(action_of("remove --permanently"), Some(Action::Remove(true)));
		assert_eq!(action_of("plugin recycle-bin"), None, "the bin has one door now (P19)");
		assert_eq!(action_of("plugin trash"), Some(Action::TrashView), "and it is `g t`");
		assert_eq!(action_of("plugin ouch"), Some(Action::Compress));
	}

	#[test]
	fn a_binding_with_several_runs_acts_on_all_of_them() {
		let keys = Keys::parse(FACTORY);

		let actions: Vec<Action> = [ "sort mtime --reverse=no", "show mtime" ]
			.iter()
			.filter_map(|run| action_of(run))
			.collect();
		assert_eq!(
			actions,
			vec![ Action::Sort(SortBy::Mtime, false), Action::Columns(ColumnsDo::Show(Col::Mtime)) ],
			"both of a binding's runs act"
		);

		let legacy: Vec<Action> = [ "sort mtime --reverse=no", "linemode mtime" ]
			.iter()
			.filter_map(|run| action_of(run))
			.collect();
		assert_eq!(
			legacy,
			vec![ Action::Sort(SortBy::Mtime, false), Action::Columns(ColumnsDo::Toggle(Col::Mtime)) ],
			"and the old word is the toggle"
		);

		let mut open = None;
		assert!(keys.feed(&mut open, ",").is_none(), "the `,` family is a sequence, not a single key");
		assert!(open.is_some());
	}
}
