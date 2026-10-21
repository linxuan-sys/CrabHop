//! duckfm's own keymap, read as data: `[[keys]]` binds a chord to a `run` verb, and the verbs this
//! window has an action for become those actions. The file is the window's own
//! (`keymap-default.toml`, embedded, overridden by `$HOME/.config/duckfm/keymap.toml`) — yazi's
//! keymap is no longer read (P20).
//!
//! A keystroke is resolved the way yazi's own router resolves it: the bindings keep the order the
//! user wrote them in, and the *first* binding that starts with the key decides — one key long and
//! it acts immediately, longer and it opens a sequence whose candidates every longer binding that
//! starts the same way.

use std::path::PathBuf;

use eframe::egui::{Key, Modifiers};
use serde::Deserialize;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum SortBy {
	Name,
	Size,
	Mtime,
	Ext,
	/// `,n` / `,N`: numbers inside names compare as numbers (`file2` before `file10`).
	Natural,
	/// `,b` / `,B`: the creation time, where the filesystem carries one (P21's table).
	Btime,
	/// `,r`: a shuffle — arbitrary but stable for a given press, so a scroll does not reorder the
	/// world under the person's hand.
	Random,
}

/// The fact columns a row carries (P33⑲, the user's own model): the old single choice is gone —
/// the columns are a set. The size always stands (the default set's member, with no key to turn
/// it off), the time is either modification or creation and never both (so the two are never
/// confused), and permissions and owner are each a switch of their own.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Columns {
	/// The time column: none, modification, or creation.
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
	/// The default set (P33⑲①): the size and the modification time.
	fn default() -> Self {
		Columns { time: Some(TimeCol::Mtime), perms: false, owner: false }
	}
}

/// One column a verb names (P33⑲②).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Col {
	Mtime,
	Btime,
	Permissions,
	Owner,
}

/// What a column verb asks for (P33⑲⑦): a toggle, a make-sure-it-stands, or the reset.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum ColumnsDo {
	Toggle(Col),
	Show(Col),
	Reset,
}

impl Columns {
	/// A press adds its column, a press again takes it away, and the two time columns replace
	/// each other rather than piling up (P33⑲②③) — the user's own model.
	pub(crate) fn toggle(&mut self, col: Col) {
		match col {
			Col::Mtime => self.time = if self.time == Some(TimeCol::Mtime) { None } else { Some(TimeCol::Mtime) },
			Col::Btime => self.time = if self.time == Some(TimeCol::Btime) { None } else { Some(TimeCol::Btime) },
			Col::Permissions => self.perms = !self.perms,
			Col::Owner => self.owner = !self.owner,
		}
	}

	/// The sort family's own verb (P33⑲⑦): make the column stand — `,m` sorts by modification
	/// time and shows it, and pressing it again must not take the column away.
	pub(crate) fn show(&mut self, col: Col) {
		match col {
			Col::Mtime => self.time = Some(TimeCol::Mtime),
			Col::Btime => self.time = Some(TimeCol::Btime),
			Col::Permissions => self.perms = true,
			Col::Owner => self.owner = true,
		}
	}

	/// `m n` (P33⑲⑤): one key back to the default set.
	pub(crate) fn reset(&mut self) {
		*self = Columns::default();
	}
}

/// Where the cursor of an in-place rename starts, and what it starts with — the `r` family's own
/// argument (§4.3).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum RenameAt {
	/// `r r`: the cursor before the extension, so the stem is what gets rewritten.
	BeforeExt,
	/// `r i`: the very front.
	Start,
	/// `r a`: the very end.
	End,
	/// `r f` / `r c`: nothing at all — the whole name is to be typed.
	EmptyAll,
	/// `c m`: the extension kept, the stem emptied.
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
	/// `y`: hold the targets for a paste. `x` (`yank --cut`) holds them for a move.
	Yank {
		cut: bool,
	},
	/// `Y` / `X`: let the held targets (and the selection) go.
	Unyank,
	/// `u` / `Ctrl+Z` (P33⑫): take the most recent change-type task back — silent when the
	/// stack is empty.
	Undo,
	/// `T` (P35③): the cursor's row in a terminal — a folder becomes the terminal's cwd, and
	/// anything else says 「须是文件夹」.
	TerminalHere,
	/// `o` (P36④): the cursor row's own context menu — the same panel and the same door a right
	/// click opens, for hands without a second button.
	MenuHere,
	/// `H` / `L` / `<S-Left>` / `<S-Right>`: walk this tab's own trail of directories.
	History {
		back: bool,
	},
	/// `g f`: where the cursor's own symlink points.
	Follow,
	/// `enter --bypass` (`<Right>` in the factory keymap): enter, and keep descending while the
	/// way down is a single-child chain.
	SmartEnter,
	/// The `g` family: a jump to one of the well-known directories.
	Cd(String),
	/// `f`: filter the rows on show by a pattern — again, or `Esc`, clears it.
	Filter,
	/// `/`: find what is typed in the list, colouring the letters it holds (P21).
	Find,
	/// `n` / `N`: the cursor walks to the next match, or the one before it.
	FindNext {
		forward: bool,
	},
	/// `)` / `(`: a symbolic link of the targets, into the parent column's own directory —
	/// absolute or relative. `*` makes a hard link instead.
	Link {
		kind: LinkKind,
	},
	/// `L`: the three-way panel that asks which kind of link, and for the path it points at (P21).
	LinkPanel,
	/// `p` / `P`: what the yank state holds, put beside the cursor. A name already standing there
	/// is asked about first, unless the keymap said `--force`.
	Paste {
		force: bool,
	},
	/// `d` to the system bin, `D` for good.
	Remove(bool),
	Create,
	/// `A`: the editor of §4.1, one name per line.
	BulkCreate,
	Rename(RenameAt),
	/// `<Enter>` / `o` / `o o`: open the cursor's own item — a directory is entered, a file goes
	/// to the system's opener (yazi's plain `open`, minus the plugin-configurable opener rules).
	Open,
	/// `O` / `o c` / `<S-Enter>`: open-with (§4.4).
	OpenWith,
	/// `i`: the index panel (P14's re-decision) — the one door to the index, frecency and the
	/// recent directories.
	Index,
	/// The `M` / `G m` / `` `` `` family (P17): the devices UDisks2 knows, handled natively.
	Devices(DeviceDo),
	/// `z` / `Z`: the frecency panel.
	FuzzyJump,
	/// `R b`: the bin's own view.
	TrashView,
	/// `c z`: pack the selection into an archive, in process.
	Compress,
	/// `s` / `S`: the in-process search (`--via=rg` is the contents one).
	Search {
		content: bool,
	},
	/// `;` / `:`: the bottom command line of §4.8, and `U`'s `shell --block '<command>'` running
	/// straight away.
	Shell {
		block:   bool,
		command: Option<String>,
	},
	/// `<Esc>` / `<C-s>`: close whatever is open, or clear the selection.
	Escape,
	Quit,
	/// The native replacements for what the keymap asks of plugins: the switches first.
	Hidden,
	Sort(SortBy, bool),
	Columns(ColumnsDo),
	Help,
	/// The tabs (§2.4): created on the current directory, switched by index or relatively, swapped
	/// with their neighbours, renamed in place, and closed.
	TabCreate,
	TabSwitch(isize, bool),
	TabSwap(isize),
	TabRename,
	TabClose,
	/// `K`/`J` (and `<C-Up>`/`<C-Down>`): the preview's own seek, signed the way the keymap
	/// signs it — five lines of text, one page of a PDF.
	Seek(i32),
	/// `<Tab>`: the spot card (§4.6), and `<Tab>` again to put it away.
	Spot,
	/// `<C-u>`/`<C-d>`/`<C-b>`/`<C-f>`/`<PageUp>`/…: the cursor walks half a view or a whole one.
	Page {
		full: bool,
		back: bool,
	},
	/// `g f`: the list keeps only what is a symbolic link, and everything when pressed again.
	Symlinks,
	/// `l`: into the subdirectory under the cursor — a file is not entered (that is `enter`).
	EnterDir,
	/// The `c` family: put what the keymap asked for on the system clipboard (P13).
	Copy(CopyWhat),
	/// `<C-a>` / `<C-r>`: select every row on show, or turn the selection over.
	ToggleAll(Option<bool>),
}

/// What a device binding asks for (P17).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum DeviceDo {
	/// `M m`: the panel opens on the devices; Enter mounts one and goes to it.
	MountPick,
	/// `p` in the panel: mount the standing device and stay where the window is (P21).
	MountHere,
	/// `M u` / `M U`: the panel opens on the devices; Enter unmounts (and ejects when it can).
	UnmountPick {
		force: bool,
	},
	/// `M R`: the device the cwd stands on is unmounted and mounted again.
	Remount,
	/// `G m`: to the mount point of the device the cwd stands on.
	Jump,
	/// `` `` ``: back to the directory before this one.
	PrevCwd,
	/// `M a` / `M e` / `M r`: the GVfs remote-URI family — not this batch's (a toast, recorded).
	Uri(&'static str),
}

/// The link family's own kinds.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum LinkKind {
	/// `)`: a symlink whose target is the absolute path.
	Absolute,
	/// `(`: a symlink whose target is relative to where the link lands.
	Relative,
	/// `*`: a hard link.
	Hard,
}

/// What the copy verbs put on the clipboard.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum CopyWhat {
	/// `c c`: the absolute path.
	Path,
	/// `c d`: the directory that holds it.
	Dirname,
	/// `c f`: the file's own name.
	Filename,
	/// `c n`: the name without its extension.
	Stem,
	/// `c C`: the `file://` URL.
	Url,
	/// `c D`: the URL of the directory that holds it.
	DirUrl,
}

/// One binding as written: the chord it is on (one key or several), and the actions of the runs
/// this window understands. A binding none of whose runs is understood never becomes a chord.
#[derive(Clone)]
pub(crate) struct Chord {
	pub on:      Vec<String>,
	pub actions: Vec<Action>,
}

/// A keystroke sequence that is still open: yazi's own rule for a key that is both a binding and
/// the start of longer ones — the candidates the next key will be filtered against.
#[derive(Default)]
pub(crate) struct Sequence {
	cands: Vec<usize>,
	times: usize,
}

/// One row of the `which` hint: the keys still to press, and what the binding does.
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

/// `run` is one command spelled as a string, or several as an array — every one of them this
/// window has an action for becomes an action, in order.
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

/// `on` is a single key spelled as a string, or a chord spelled as an array of them — the keymap
/// writes both, and both are the same thing with a length.
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

/// The factory table, embedded: what a window without a user file binds, and what the help and
/// `which` overlays read when there is no file of the user's own (P20).
pub(crate) const FACTORY: &str = include_str!("../keymap-default.toml");

pub(crate) struct Keys {
	/// The bindings in the user's own order, which is the order the matching runs in.
	chords: Vec<Chord>,
	/// What each of those bindings says it does, in step with `chords` — the `which` hint's
	/// descriptions (P18).
	descs:  Vec<String>,
	/// The bindings as written, for the help overlay to read out.
	rows:   Vec<(String, String, String)>,
	/// What each of those rows *does*, in step with `rows`: the interactive help runs the standing
	/// row's actions (P21's `?`). Empty for a row this window has no action for.
	runs:   Vec<Vec<Action>>,
	/// The runs the keymap asks for, and how many of them have an action here.
	#[cfg_attr(not(test), allow(dead_code))]
	done:   usize,
	#[cfg_attr(not(test), allow(dead_code))]
	total:  usize,
}

impl Keys {
	/// The user's own keymap when they have one, the factory copy embedded in the binary otherwise.
	/// A file that parses but holds no binding this window knows is not a keymap: the factory's own
	/// stands, with a line on stderr — an empty table would leave the window without keys.
	pub(crate) fn load() -> Self {
		let path = std::env::var_os("HOME")
			.map(|home| PathBuf::from(home).join(".config").join("duckfm").join("keymap.toml"));

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

	/// The bindings as written: what the help overlay shows, straight from the user's own file.
	pub(crate) fn rows(&self) -> &[(String, String, String)] {
		&self.rows
	}

	/// What the `at`-th row of that table does, if this window has an action for it (P21's `?`:
	/// the help runs the standing row).
	pub(crate) fn run_at(&self, at: usize) -> Option<Vec<Action>> {
		self.runs.get(at).filter(|actions| !actions.is_empty()).cloned()
	}

	/// The family a first key opened: what each open candidate still asks for, and the binding's
	/// own description — the `which` hint's rows, driven by the keymap itself (P18).
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

	/// How many of the keymap's runs this window acts on, out of how many it asks for.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn done(&self) -> (usize, usize) {
		(self.done, self.total)
	}

	/// A key arrives: what the keymap says to do, if it is a whole binding — and the sequence that
	/// is left open, if the keymap is waiting for another key.
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

		// the first binding in the keymap's own order decides
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

/// The name of a key the way a keymap writes it: `<Enter>`, `<C-c>`, `<A-k>`, `_`, `G` — the
/// character the layout would type when there is one, and the key's own token otherwise.
pub(crate) fn chord(key: Key, modifiers: Modifiers, text: Option<&str>) -> Option<String> {
	if let Some(name) = named(key) {
		// a named key still carries what is held: the keymap spells `<S-Enter>` and `<S-PageUp>`
		// as their own bindings, and a shifted key must not answer to the bare one
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

	// what the layout typed: "_" is shift+minus, "G" is shift+g — one character, and never a
	// control one
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

/// The keys a keymap spells as tokens, and which never carry a character.
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

/// The unshifted name of a key, for the tokens and the tie-breaking above.
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

/// What a shifted punctuation key spells, for the keys a replay types without a layout.
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

/// Every `run` name the keymap asks for, how often, and where this crate stands with it: the
/// feature spec's own inventory, read out of the file that is the spec. The tests and the report
/// read this table; the window itself counts through `Keys::done`.
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

/// The command a `shell` run carries after its flags, when it carries one: `;` and `:` carry
/// none and open the command line, while `U`'s update script is a whole command with its own
/// spaces that runs straight away.
fn shell_command(run: &str) -> Option<String> {
	let mut rest = run.trim_start_matches("shell").to_owned();
	for flag in [ "--block", "--interactive" ] {
		rest = rest.replace(flag, "");
	}

	let command = rest.trim().trim_matches('\'').trim();
	(!command.is_empty()).then(|| command.to_owned())
}

/// The `plugin whoosh` names read as the bookmark commands they ask for — the plugin system is
/// gone, its jobs are not.
/// The `plugin gvfs -- <what>` runs the keymap spells, read as the device commands they mean.
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

/// The `run` strings this window understands. A name it does not know is not an error: the
/// keymap is the user's, and the rest is waiting for later milestones.
fn action_of(run: &str) -> Option<Action> {
	let mut words = run.split_whitespace();
	let name = words.next()?;
	let argument = words.next();

	Some(match name {
		// the argument is a number in yazi's own keymap and a name in this one: both are read
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
		// `u`'s own verb (P33⑫): the most recent change-type task, taken back
		"undo" => Action::Undo,
		// `T`'s own verb (P35③): the cursor's row in a terminal
		"terminal" => Action::TerminalHere,
		// `o`'s own verb (P36④): the cursor row's own context menu
		"menu" => Action::MenuHere,
		"back" => Action::History { back: true },
		"forward" => Action::History { back: false },
		"follow" => Action::Follow,
		"filter" => Action::Filter,
		"find" => Action::Find,
		"find_arrow" => Action::FindNext { forward: !run.contains("--previous") },
		"index" => Action::Index,
		// the panel is the one door now (P21's table); the old spellings still choose a kind, and
		// the panel's own keys use them
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
			// `g t` is the bin's only way in (P19: the old `R b` alias is gone)
			Some("trash") => Action::TrashView,
			Some("ouch") => Action::Compress,
			// `M p` is the device panel's old name for the same thing
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
		// the tab family's own spelling of the same (P33④: `t q` reads better than `t close`)
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
		// the column verbs (P33⑲⑦): `column` adds or takes away, `show` makes sure, `reset` is the
		// default set. `toggle` itself is the selection's own old word and stays with it. The old
		// `linemode` word parses to the same three — a person's keymap written before the column
		// model still loads — and its `none`/`size` mean the default set now.
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

	/// The factory file carries no run this window does not act on: `U`'s update-config, the
	/// whoosh family, `plugin bypass`, `find`, `suspend` and `tasks:show` were all dropped when the
	/// table moved into duckfm's own file (P20) — a row nothing answers is a lie in the help
	/// overlay.
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

	/// The factory keymap is what a user without one gets, and what it binds has to be readable
	/// here: the movement keys, the enter and the toggle are the ones this window acts on.
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

	/// The keymap's own order rules a keystroke: the first binding that starts with the key
	/// decides. A one-key binding acts at once — even when longer bindings start with the same
	/// key later in the file — and only a binding written before them opens a sequence.
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

	/// A sequence completes at the shortest binding that fits: a candidate that is that long is
	/// executed even when longer ones share its keys, a lone candidate left standing is executed,
	/// and a key nothing continues simply closes the sequence.
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

	/// The name a keystroke is looked up under: the character the layout types, the control
	/// tokens, and the shifted spellings a replay relies on.
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

	/// A binding this window has no action for is ignored, not fatal — and a chord among with one
	/// is still found after it.
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

	/// The factory table's own `/`, `n` and `N` (P21): the find family is bound where the table
	/// says, without a window in the way.
	#[test]
	fn the_slash_opens_the_find_box_in_the_factory_table() {
		let keys = Keys::parse(FACTORY);
		let mut open = None;

		assert_eq!(keys.feed(&mut open, "/"), Some(vec![ Action::Find ]), "`/` opens the find box");
		assert_eq!(keys.feed(&mut open, "n"), Some(vec![ Action::FindNext { forward: true } ]));
		assert_eq!(keys.feed(&mut open, "N"), Some(vec![ Action::FindNext { forward: false } ]));
	}

	/// `L` is the panel now (P21's table): a bare `link` opens it, and the two spellings that name
	/// a kind still do what they say (the panel's own keys go through `Action::Link`).
	#[test]
	fn the_link_panel_has_the_bare_verb_and_the_kinds_keep_their_spellings() {
		assert_eq!(action_of("link"), Some(Action::LinkPanel));
		assert_eq!(action_of("link --absolute"), Some(Action::Link { kind: LinkKind::Absolute }));
		assert_eq!(action_of("link --relative"), Some(Action::Link { kind: LinkKind::Relative }));
		assert_eq!(action_of("hardlink"), Some(Action::Link { kind: LinkKind::Hard }));
	}

	/// P21's second wave: `/` and the two arrows that walk its matches.
	#[test]
	fn the_find_family_reads_its_spellings() {
		assert_eq!(action_of("find --smart"), Some(Action::Find));
		assert_eq!(action_of("find_arrow"), Some(Action::FindNext { forward: true }));
		assert_eq!(action_of("find_arrow --previous"), Some(Action::FindNext { forward: false }));
	}

	/// P21's second wave: the three linemodes the table adds (`m p`/`m b`/`m o`) and the
	/// creation-time sort (`,b`/`,B`).
	#[test]
	fn the_new_linemodes_and_the_btime_sort_read_their_names() {
		// the column verbs (P33⑲⑦): the factory's own words, and the legacy `linemode` word read
		// as the same three — a keymap from before the column model still loads
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

	/// The movement family's verbs (P21's table): half a view, a whole one, and the symlink-only
	/// switch the `g f` row means now.
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

	/// The `which` hint's own data (P18): the family a first key opened, each open candidate's
	/// remaining keys and the binding's own description — straight from the keymap, nothing added.
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

		// one key inside the family closes it, so there is nothing left to hint
		assert_eq!(keys.feed(&mut open, "a"), Some(vec![ Action::Cd("~/Desktop/".to_owned()) ]));
		assert!(open.is_none());
	}

	/// The file's own shape (P20): `[[keys]]`, `on` a string or an array, `run` a verb or several,
	/// and `desc` the words the help overlay and the `which` hint read out.
	#[test]
	fn a_duckfm_keyfile_reads_its_own_shape() {
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

	/// Nothing in the factory file is shadowed: pressing a binding's own keys reaches it. The rule
	/// is yazi's first match in the file's own order, so a single key written above a longer chord
	/// that starts with it swallows that chord — the two the preset had (`t t`, `t r`, both behind
	/// the single `t`) are not in this file (P20).
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

	/// The feature spec is the keymap, so this is its inventory: every `run` name it asks for, and
	/// whether this crate acts on it yet. The report reads this table.
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

	/// The rename family's arguments decide where the cursor starts (§4.3): the variants the keymap
	/// writes read back as their seats, and `--empty` empties before anything is typed.
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

	/// The shell family: `;` and `:` open the command line (the colon's is the blocking one), and
	/// a `shell --block '<command>'` binding carries its command through as written.
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

	/// open is reached the ways the keymap spells it: the interactive one opens the open-with
	/// field, and a plain `open` — `<Enter>` / `o` / `o o` — is the system's own open.
	#[test]
	fn open_reads_its_spellings() {
		assert_eq!(action_of("open --interactive"), Some(Action::OpenWith));
		assert_eq!(action_of("plugin open-with-cmd --block"), Some(Action::OpenWith));
		assert_eq!(action_of("open"), Some(Action::Open), "plain open acts");
	}

	/// The two sorts this batch adds read as their keymap names.
	#[test]
	fn the_new_sorts_read_their_names() {
		assert_eq!(action_of("sort natural --reverse=no"), Some(Action::Sort(SortBy::Natural, false)));
		assert_eq!(action_of("sort natural --reverse=yes"), Some(Action::Sort(SortBy::Natural, true)));
		assert_eq!(action_of("sort random --reverse=no"), Some(Action::Sort(SortBy::Random, false)));
	}

	/// The device family (P17) reads the gvfs runs and the old `plugin mount` name.
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

	/// The `g` family's cd runs read as paths (with `~` to expand), and its trash view is the
	/// plugin's own name for it.
	#[test]
	fn the_cd_family_reads_its_paths() {
		assert_eq!(action_of("cd ~"), Some(Action::Cd("~".to_owned())));
		assert_eq!(action_of("cd ~/Desktop/"), Some(Action::Cd("~/Desktop/".to_owned())));
		assert_eq!(action_of("cd --interactive"), Some(Action::FuzzyJump), "the interactive one is the jump panel");
		assert_eq!(action_of("plugin trash"), Some(Action::TrashView), "and `g t` is the bin's view");
		assert_eq!(action_of("plugin recycle-bin"), None, "the old `R b` alias is gone (P19)");
	}

	/// The `c` family reads as the six things it can copy, spelled the way the keymap spells them.
	#[test]
	fn the_copy_family_reads_its_six_spellings() {
		assert_eq!(action_of("copy -- path"), Some(Action::Copy(CopyWhat::Path)));
		assert_eq!(action_of("copy -- dirname"), Some(Action::Copy(CopyWhat::Dirname)));
		assert_eq!(action_of("copy -- filename"), Some(Action::Copy(CopyWhat::Filename)));
		assert_eq!(action_of("copy -- name_without_ext"), Some(Action::Copy(CopyWhat::Stem)));
		assert_eq!(action_of("copy url"), Some(Action::Copy(CopyWhat::Url)));
		assert_eq!(action_of("copy dirurl"), Some(Action::Copy(CopyWhat::DirUrl)));
	}

	/// The whoosh family is gone from the factory keymap (P14's re-decision): no run it spelled
	/// reaches an action here, and `i` is the index panel's door.
	#[test]
	fn the_whoosh_family_has_no_way_in() {
		let preset = include_str!("../../yazi-config/preset/keymap-default.toml");
		assert!(!preset.contains("whoosh"), "the presets says nothing of whoosh");

		for run in [ "plugin whoosh jump_by_key", "plugin whoosh save", "plugin whoosh jump_key_k" ] {
			assert_eq!(action_of(run), None, "{run}");
		}
		assert_eq!(action_of("index"), Some(Action::Index), "and `i` opens the index");
		assert_eq!(action_of("plugin fzf"), Some(Action::FuzzyJump), "and `z` still reaches the same panel");
		assert_eq!(action_of("plugin zoxide"), Some(Action::FuzzyJump));
	}

	/// The two searches: `s` names, `S` contents — the keymap's `--via` says which.
	#[test]
	fn the_search_runs_read_their_via() {
		assert_eq!(action_of("search --via=fd"), Some(Action::Search { content: false }));
		assert_eq!(action_of("search --via=rg"), Some(Action::Search { content: true }));
	}

	/// The removals tell their two apart, and the bin's view has its own name.
	#[test]
	fn the_removals_read_their_flags() {
		assert_eq!(action_of("remove"), Some(Action::Remove(false)));
		assert_eq!(action_of("remove --permanently"), Some(Action::Remove(true)));
		assert_eq!(action_of("plugin recycle-bin"), None, "the bin has one door now (P19)");
		assert_eq!(action_of("plugin trash"), Some(Action::TrashView), "and it is `g t`");
		assert_eq!(action_of("plugin ouch"), Some(Action::Compress));
	}

	/// The multi-command bindings the factory has (`sort` and its column verb together) act on
	/// both of their commands, in order — and the legacy pairing (`linemode mtime`) parses to the
	/// same two, its old word now the toggle (P33⑲⑦).
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