//! The window: every piece of interaction is decided here, on this thread — the tabs, the cursor,
//! the selection, the directory stack and the scrolling. The background is only ever asked for a
//! directory, and its answer may arrive whenever it likes.

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{
	self, Align2, Color32, Context, CursorIcon, FontFamily, FontId, Key, Modifiers, Painter, Pos2, Rect, Sense,
	TextEdit, Ui, Vec2, ViewportCommand, pos2, vec2,
};

use crate::book;
use crate::bulk;
use crate::chrome::{self, STATUS_HEIGHT, TAB_HEIGHT};
use crate::complete::{self, Completions};
use crate::config::{Config, SHARES};
use crate::edit::Edit;
use crate::folder::{Entry, FolderCache, Snapshot};
use crate::frecency;
use crate::fuzzy;
use crate::keys::{Action, Columns, ColumnsDo, CopyWhat, DeviceDo, Keys, LinkKind, RenameAt, Sequence, SortBy};
use crate::mount;
use crate::list;
use crate::metrics;
use crate::paste;
use crate::open;
use crate::pack;
use crate::preview::PreviewPane;
use crate::scroll::{self, Scroll};
use crate::search;
use crate::shell::{self, History};
use crate::toast::{self, Toasts, Undo};
use crate::tokens::{FONT_SIZE, FONT_SMALL, ICON_BOX, LINE_HEIGHT, ROW_HEIGHT, Tokens};
use crate::trash;

/// How long a directory may take before the skeleton admits to the wait (§1.3).
const SKELETON_AFTER: Duration = Duration::from_millis(120);

/// The size/time facts share a row in the middle column only (P12): the parent list and the
/// preview's directory listing are icon and name, so no window width can crowd them into each
/// other. Inside the middle column the ladder is `list::row_facts`'s own.
pub(crate) const PARENT_META: bool = false;
pub(crate) const CURRENT_META: bool = true;
/// The index panel's footer (P22): the keys that are only its own, and the `A` the table asked to
/// keep in sight so the add is not forgotten.
const INDEX_HINT: &str = "Enter 跳转 · Ctrl+S 保存目录为索引 · Del 删除所选 · i 再按跳输入框 · Esc 退出";
/// `M`'s panel, its keys said where a person looks for them (P21's table): the nine options, two
/// lines of them.
const DEVICE_HINT: &str = "↑↓ 选择 · Enter 挂载并跳转 · p 挂载 · m 挂载并跳转 · g 跳到挂载点 · Esc 关闭";
const DEVICE_HINT_UNMOUNT: &str = "↑↓ 选择 · Enter 卸载并弹出 · u 卸载 · U 强制卸载 · Esc 关闭";
const DEVICE_HINT_URI: &str = "R 重挂当前设备 · a 添加 URI · e 编辑 URI · r 移除 URI（远程族暂不支持） · 鼠标双击执行";
/// The line under it (P24): what the same box does when what is typed turns out to be a path —
/// the three priorities of P22, said in one breath.
/// How long the index panel's own notice stays up (P22): a typed path that is not there says so,
/// and then the footer goes back to its keys.
const NOTICE_ALIVE: Duration = Duration::from_secs(2);
/// The spot card's own width (P23): wide enough for 「可列出、可新建删除、可进入」 to stand on one
/// line, and narrow enough to stay a corner card over the preview column.
const SPOT_WIDTH: f32 = 360.0;
/// The table's own ceiling on tabs (P21): ten, and the eleventh press is answered with words.
const TAB_LIMIT: usize = 10;
/// How many change-type tasks the undo stack keeps (P33⑫): the most recent ten; the eleventh
/// pushes the oldest out.
const UNDO_DEPTH: usize = 10;
/// How long an overlay scrollbar stays awake after the last touch (§3 总规则).
const BAR_IDLE: Duration = Duration::from_millis(1200);
/// How long a chord's family stays on screen before it goes away: a key inside the chord resets
/// the clock, and nothing inside it is what closes the sequence (P18's `which` hint). Ten seconds
/// since P27 — the hint is a reference a person reads, not a flash; a continuation key, Escape, or
/// any action still takes it away at once.
const WHICH_TIMEOUT: Duration = Duration::from_secs(10);
/// The bottom hint line's own height, while the bin is the view (P19): the four keys that are
/// only the bin's, and nothing else, so the columns make room for it.
const TRASH_HINT: f32 = 24.0;

/// The chip metrics of the tab bar, from the spec's 100–220px band (§2.4).
const CHIP_MIN: f32 = 100.0;
const CHIP_MAX: f32 = 220.0;
const CHIP_GAP: f32 = 6.0;
const CHIP_PAD: f32 = 34.0;

/// One directory as the window knows it: where it is, and where the cursor was left in it.
struct View {
	path:   PathBuf,
	cursor: usize,
}

/// What a tab's view was built from: the snapshot, the directory, the switches, the ordering and
/// the filter — any change of it is a rebuild.
type ViewKey = (usize, PathBuf, bool, (SortBy, bool), Option<String>, bool);

/// A tab's own trail of directories (P13): where it has been, and where in that trail it
/// stands. Pure, so the rule can be read and tested without a window.
#[derive(Default)]
pub(crate) struct Trail {
	dirs: Vec<PathBuf>,
	at:   usize,
}

impl Trail {
	/// The tab has arrived somewhere: the trail keeps it, and anything after it is a future that
	/// no longer exists, so it is cut away.
	fn push(&mut self, dir: &Path) {
		if self.dirs.get(self.at).map(PathBuf::as_path) == Some(dir) {
			return;
		}

		self.dirs.truncate(self.at + 1);
		self.dirs.push(dir.to_owned());
		self.at = self.dirs.len() - 1;
	}

	/// `H` / `L`: the directory one step back or forward on the trail, if there is one.
	fn travel(&mut self, back: bool) -> Option<PathBuf> {
		let at = if back { self.at.checked_sub(1)? } else { self.at + 1 };
		self.dirs.get(at).cloned().inspect(|_| self.at = at)
	}

	/// How many steps the trail holds (the tests read the seat; nothing else does).
	#[cfg(test)]
	fn at(&self) -> usize { self.at }
}

/// The parent column's own mouse (P33⑧): the row a click seated — `None` means the column's own
/// reading, the stack step's cursor or the cwd's row in the filesystem parent — and the scroll
/// the wheel walks it by. Per tab, and reset when the tab walks anywhere.
#[derive(Default)]
struct ParentView {
	seat:   Option<usize>,
	scroll: Scroll,
	/// The parent these were read against: a different parent is a new column.
	path:   Option<PathBuf>,
}

/// One tab: its own stack of directories, its own ordering, its own scroll — and a name, if the
/// person gave it one.
struct Tab {
	stack:   Vec<View>,
	/// The trail `H`/`L` walk (P13).
	history: Trail,
	view:    Vec<usize>,
	view_of: Option<ViewKey>,
	scroll:  Scroll,
	title:   Option<String>,
	/// The pattern the view is filtered by, if any: per tab, the way yazi keeps it.
	filter:  Option<String>,
	/// `g f`: whether the list keeps only symbolic links (P21's table).
	symlinks: bool,
	/// The left column's pointer state (P33⑧): its seat and its scroll.
	parent:  ParentView,
	/// The name the cursor is waiting for a row to sit on: a `leave` into a directory whose
	/// listing had not arrived seats the first row now and the right one when the names land —
	/// the directory is kept beside the name, so a later navigation cannot seat the wrong listing.
	seat:    Option<(PathBuf, String)>,
	/// The bin, while this tab is its own view (P27): `g t` makes such a tab, the middle column
	/// shows it instead of the directory, and walking away (a `cd`, a `leave`) leaves it behind.
	trash:   Option<TrashView>,
}

impl Tab {
	fn at(path: PathBuf) -> Self {
		let history = Trail { dirs: vec![ path.clone() ], at: 0 };

		Self {
			stack: vec![ View { path, cursor: 0 } ],
			history,
			view: Vec::new(),
			view_of: None,
			scroll: Scroll::default(),
			title: None,
			filter: None,
			symlinks: false,
			parent: ParentView::default(),
			seat: None,
			trash: None,
		}
	}

	fn path(&self) -> &Path {
		&self.stack.last().expect("the stack is never empty").path
	}

	/// What the chip says: the name the person gave it, or the directory's own — and the bin's own
	/// word while the tab is the bin's view (P27).
	fn name(&self) -> String {
		if let Some(title) = &self.title {
			return title.clone();
		}
		if self.trash.is_some() {
			return "回收站".to_owned();
		}

		let path = self.path();
		path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
	}
}

/// A file operation in flight, as the status bar reads it: what it is doing, how far it has come
/// and how fast.
#[derive(Clone)]
struct Progress {
	title:    String,
	fraction: Option<f32>,
	rate:     Option<String>,
}

/// A device read or action in flight (P17): the intent the answer serves, and the worker's own
/// channel. The bus is never touched on this thread.
struct DeviceJob {
	do_: DeviceDo,
	rx:  std::sync::mpsc::Receiver<Answer>,
}

enum Answer {
	/// The read-only listing.
	Devices(Result<Vec<mount::Device>, String>),
	/// A mount that landed somewhere to go.
	At(Result<std::path::PathBuf, String>),
	/// An unmount (and its eject) that is done.
	Done(Result<(), String>),
}

/// What `y` / `x` are holding for a paste (P13): the targets, and whether the paste would move
/// them. The paste itself is not part of this batch.
#[derive(Clone)]
struct Yank {
	paths: Vec<PathBuf>,
	cut:   bool,
}

/// The bulk editor of §4.1: one name per line, and what that column means against the disk.
struct BulkEditor {
	edit:  Edit,
	lines: Vec<bulk::Line>,
}

/// An in-place rename of §4.3: the row's own name becomes a field, and a conflict is checked as
/// it is typed — the confirmation dialog is moved forward to the keystroke.
struct ItemRename {
	path: PathBuf,
	edit: Edit,
	/// What is wrong with the name right now, when anything is: the field turns `err` and Enter is
	/// refused until it is right.
	note: Option<&'static str>,
}

/// The bottom command line (§4.8): a vim-style line above the status bar, its history recall,
/// and the path completion the tab key opens.
struct Cmdline {
	/// `:` waits for its command; `;` is the same run seen as a quick one. The prefix's colour is
	/// the only place the two differ.
	block:  bool,
	edit:   Edit,
	comp:   Option<Completions>,
	/// Where an up-arrow stands in the history, newest = last; `None` is a line of one's own.
	recall: Option<usize>,
}

/// One command's answer (§4.8): its name, its exit code, and its text — a temporary sheet at the
/// window's bottom, put away with `Esc`.
struct Output {
	title: String,
	code:  Option<i32>,
	lines: Vec<(String, bool)>,
	from:  usize,
}

/// open-with (§4.4): the item(s) a command is handed to, the field to type it in, and the
/// history and `$PATH` behind a tab.
struct OpenWith {
	paths:  Vec<PathBuf>,
	/// What the subtitle names: one file by its own name, several by their count.
	name:   String,
	edit:   Edit,
	comp:   Option<Completions>,
	recall: Option<usize>,
}

/// What the fuzzy panel is standing over (§5.3): the source decides the text and the score's own
/// reading, and the mode decides what Enter does.
#[derive(Clone, Copy, PartialEq)]
enum FuzzyMode {
	/// `z` / `Z` / `] f`: the index panel (P14) — bookmarks, high scores and recent visits in one
	/// line, with a key badge, a delete, a rename and a save of the cwd to hand.
	Index,
	/// `M m` / `M u`: the devices UDisks2 knows, and what Enter does with the standing one.
	Devices {
		unmount: bool,
		force:   bool,
	},
	/// `s`: the root's names, filled in as the walk goes.
	FindNames,
	/// `S`: the root's matching lines, likewise.
	FindText,
}

#[derive(Clone)]
struct FuzzyItem {
	text:    String,
	/// What the right-hand column reads when the source has a score of its own — the frecency
	/// panel's own number (§5.3); the fuzzy score stands in when it does not.
	right:   Option<String>,
	payload: FuzzyPayload,
	/// The index panel's own grouping (P14): its rows come from three sources at once, and the
	/// panel says which is which where the group changes. Empty for every other panel.
	zone:    &'static str,
	/// A bookmark whose path is no longer there: still listed, and said so in the row's colour.
	broken:  bool,
}

impl FuzzyItem {
	fn plain(text: String, right: Option<String>, payload: FuzzyPayload) -> Self {
		Self { text, right, payload, zone: "", broken: false }
	}
}

#[derive(Clone)]
enum FuzzyPayload {
	/// A directory to cd into, or a file to reveal by its parent.
	Path(PathBuf),
	/// An index entry, named by the name the store knows it under.
	Index(String),
	/// A UDisks2 block, named by its object path.
	Device(String),
}

/// The index panel's standing row, when one of its zones owns it (P40): the bookmarks zone names
/// its entry, the 高频 and 最近 zones stand on a frecency path — both the store's own.
enum IndexEntry {
	/// A bookmark, by the name its store knows it under.
	Bookmark(String),
	/// A frecency row: the path, and the zone it was standing in (for the words).
	Frecency(PathBuf, &'static str),
}

/// A filter running off this thread: for a list past the spec's two thousand the matching goes
/// where it belongs, and the answer is taken only when its generation is still the current one.
struct FilterJob {
	epoch: u64,
	rx:  std::sync::mpsc::Receiver<Vec<fuzzy::Hit>>,
}

/// The fuzzy panel (§5.3): non-modal, top-anchored, filtered at each keystroke. Its source list
/// is shared behind an `Arc`, so a search can swap a fresh one in while the panel is up.
/// The two-stage focus a panel's input box has (P33⑬): the panel opens with the list holding the
/// keyboard, `f` — or a click on the box — moves it inside, and Esc steps back out of the box
/// before it closes anything. One machine, shared: the help panel (`?`) had it first, the index
/// panel (`i`) is its second user, and a later panel is one more field of this type.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(crate) struct PanelFocus {
	pub(crate) inside: bool,
}

impl PanelFocus {
	/// `f`, or a click on the box itself.
	pub(crate) fn enter(&mut self) { self.inside = true; }

	/// Esc: `true` when the box held the keyboard and now does not — the panel closes only when
	/// this answers `false`.
	pub(crate) fn escape(&mut self) -> bool {
		let held = self.inside;
		self.inside = false;

		held
	}
}

struct Fuzzy {
	mode:    FuzzyMode,
	query:   Edit,
	items:   Arc<Vec<FuzzyItem>>,
	hits:    Vec<fuzzy::Hit>,
	at:      usize,
	/// Which half of the panel has the keyboard (P33⑬): the index opens with the list, every other
	/// panel opens the way it always did — its box.
	focus:   PanelFocus,
	/// Kept across keystrokes: nucleo's tables are worth the warm.
	matcher: nucleo_matcher::Matcher,
	job:     Option<FilterJob>,
	epoch:     u64,
	/// The walk still filling the list when the panel is a search's own, when the last keystroke
	/// was, and whether results are still on their way.
	finder:  Option<search::Job>,
	since:   Instant,
	loading: bool,
	/// Whether the walk for the current query has already had its say: a finished walk must not
	/// start over — a change of query is what starts the next one.
	filled:  bool,
	/// The panel's own last word (P22): what Enter found when the typed path was not there, and
	/// when it was said — the footer says it in `err` for a moment.
	notice:  Option<(String, Instant)>,
}

/// A one-line question the window asks — `c z`'s archive name: the title, the field, and what
/// Enter does with what was typed.
struct Prompt {
	title:  String,
	edit:   Edit,
	action: PromptDo,
}

#[derive(Clone)]
enum PromptDo {
	/// Pack these into the archive the field names.
	Compress(Vec<PathBuf>),
	/// `a`: the name that was typed, made as a file when it has an extension and as a directory
	/// when it has none (P21's table).
	Create,
	/// `L`'s panel: the path the link is to point at, made in the parent column's own directory
	/// (P21's table).
	Link(LinkKind),
}

/// A dangerous confirmation (§4.5): the question, and what a yes would do. The keyboard's own
/// answers are `y` and `n`; the focus sits on the cancel side.
struct Confirm {
	text:   String,
	/// The items the question is about, up to five — the spec's own list.
	names:  Vec<String>,
	action: ConfirmDo,
}

#[derive(Clone, PartialEq)]
enum ConfirmDo {
	/// `D`: these paths, for good.
	Delete(Vec<PathBuf>),
	/// `D` inside the bin's view: these bin items, purged.
	Purge(Vec<trash::TrashItem>),
	/// Emptying the bin.
	Empty,
	/// `p` on a name already standing there: overwrite it and paste (P21's table).
	Paste {
		cut:   bool,
		force: bool,
	},
	/// `Enter` on a name the original directory still holds: back under this free name instead —
	/// nothing standing there is touched (P19).
	RestoreAs(trash::TrashItem, PathBuf),
}

/// The trash work in flight, each answer read every frame.
enum TrashJob {
	/// A delete to the bin, with the originals its toast will offer back.
	Delete(trash::Job<()>, Vec<PathBuf>),
	/// An undo: originals going back to where they were — and the words its toast says (`Enter` on a
	/// taken name says where the item actually landed, P19).
	Restore(trash::Job<()>, Vec<PathBuf>, String),
	/// The bin's own list, for the `g t` view.
	List(trash::Job<Vec<trash::TrashItem>>),
	/// A permanent delete — of paths, or of bin items — and the words its toast says when it lands.
	Purge(trash::Job<()>, String),
	/// Emptying the bin.
	Empty(trash::Job<()>),
}

/// The byte-mover an undo has set going (P33⑫): the same discipline every other mover keeps —
/// the work runs on a thread of its own, and the frame reads the answer when it lands.
enum UndoJob {
	/// A copy-paste taken back: the copies go to the bin.
	Discard(trash::Job<()>),
	/// A cut-paste taken back: the moved set walks home.
	Back(paste::Job<paste::Done>),
}

/// The bin's own view: the list, its cursor and its own scroll — a view of the window, not a
/// popup, so the columns are the view (P19).
struct TrashView {
	items:   Vec<trash::TrashItem>,
	/// Which of them are directories, read once when the list lands: the draw path touches no
	/// filesystem (P19).
	dirs:    Vec<bool>,
	cursor:  usize,
	/// Whether the list is still on its way.
	loading: bool,
	/// The view's own place in its list: the middle column's scroll stays the directory's.
	scroll:  Scroll,
}

/// What a chip's own interaction decided, applied after the loop that found it: the draws and the
/// clicks cannot both borrow the tab list.
#[derive(Default)]
struct ChipActions {
	switch:       Option<usize>,
	close:        Option<usize>,
	close_others: Option<usize>,
	rename:       Option<usize>,
	plus:         bool,
	moved:        Option<(usize, usize)>,
}

/// What a dry terminal launch would have run (P35③): the argv and its folder.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct TerminalLaunch {
	pub(crate) argv:   Vec<String>,
	pub(crate) folder: PathBuf,
}

pub(crate) struct App {
	tokens: Tokens,
	config: Config,
	shares: [f32; 3],
	cache:  FolderCache,
	keys:   Keys,

	/// The tabs, and which of them is on screen.
	tabs:   Vec<Tab>,
	active: usize,
	/// A tab title being typed in place: the buffer, the tab, and whether the field has taken the
	/// focus yet.
	rename:       Option<String>,
	rename_at:    Option<usize>,
	rename_focus: bool,
	/// The tab bar's own horizontal scroll, when the chips outgrow it.
	tabs_scroll:  f32,
	/// The tab strip's two buttons as the last frame drew them (P33⑮⑯): the \`+\` beside the last
	/// chip and the corner's close. The harness reads their places from here.
	plus_hit:     Option<Rect>,
	close_hit:    Option<Rect>,
	/// The chips' own × boxes as the last frame drew them (P33②): the harness's pointer needs a
	/// place to press, told apart by the tab's own index — a chip only grows its × while it is
	/// active or hovered, and hover settles a frame late.
	tab_close_hits: Vec<(usize, Rect)>,
	/// The chips themselves as the last frame drew them (P33②): where the pointer walks to hover
	/// one, which is what grows its ×.
	tab_chip_hits:  Vec<(usize, Rect)>,
	/// The status bar's own index button as the last frame drew it (P33⑰).
	status_index: Option<Rect>,

	selection: HashSet<PathBuf>,
	/// The cursor's own name: what a re-read of the same directory keeps it on (P18).
	cursor_name: Option<String>,
	/// What `y` / `x` are holding (P13), if anything.
	yank:      Option<Yank>,
	preview:   PreviewPane,
	tasks:     Vec<Progress>,

	/// The spot card §4.6: the cursor's own file, described over the preview column, and the
	/// half-typed `c c` chord that copies its path.
	spot:     bool,
	spot_chord: bool,

	/// The bulk editor and the in-place rename, either perhaps owning the keyboard — and the
	/// toasts, which are the only thing either says when it is done.
	bulk:        Option<BulkEditor>,
	item_rename: Option<ItemRename>,
	toasts:      Toasts,
	/// The row's own menu, when one is up (P31): the item list, the applications page, a
	/// confirmation — see `menu.rs` for what it holds.
	menu:        Option<crate::menu::Menu>,
	/// The channel an opening that failed comes back on (P30): the window hands a path to the
	/// system's launcher on a thread of its own, and a launcher that refused sends it back here.
	open_failed: (std::sync::mpsc::Sender<PathBuf>, std::sync::mpsc::Receiver<PathBuf>),
	/// What a menu action could not do (P31): a terminal that is not there, said on a toast.
	menu_note:   (std::sync::mpsc::Sender<String>, std::sync::mpsc::Receiver<String>),
	/// Whether a terminal launch is only noted, not made (P35③): the harness's own switch — a
	/// headless test must not open a window.
	terminal_dry: bool,
	/// What a dry launch would have run (P35③): the argv and its folder, newest last.
	terminal_log: (std::sync::mpsc::Sender<TerminalLaunch>, std::sync::mpsc::Receiver<TerminalLaunch>),
	/// Whether the system's own opener is only noted, not run (P37①): the harness's switch — a
	/// headless test must not spawn `xdg-open`, either.
	open_dry:    bool,
	/// The preferences a test hands the default lookup (P37④): in place of `mimeapps.list`, so a
	/// test never has to write the person's own file.
	mimeapps_for_tests:        Option<String>,
	/// The ledger paths a test's own window reads (P38④): the person's level and the system's —
	/// a file of the test's own, so the live read and the merge-write are exercised without
	/// touching the shared sandbox file the other tests read.
	mimeapps_path_for_tests:        Option<PathBuf>,
	mimeapps_system_path_for_tests: Option<PathBuf>,
	/// The frecency store a test's own window reads and writes (P40): a file of the test's own —
	/// every session writes its visits to the store, so a shared file is a race under load.
	frecency_path_for_tests:        Option<PathBuf>,
	/// What a dry default open would have handed over (P37①): the path, newest last.
	open_log:    (std::sync::mpsc::Sender<PathBuf>, std::sync::mpsc::Receiver<PathBuf>),
	/// Whether a yank offers the file-managers' clipboard flavors too (P35④ phase two): on in the
	/// window, off in the harness — a test must not take the session's own selection.
	uri_offers: bool,

	/// The command line (§4.8) and the shell's own state: the history it recalls from and the job
	/// that may be in flight.
	cmdline: Option<Cmdline>,
	history: History,
	job:     Option<shell::Job>,
	output:  Option<Output>,

	/// open-with (§4.4): the field when it is up, and the history it completes from.
	openwith:     Option<OpenWith>,
	open_history: open::History,

	/// The bookmarks and the frecency behind them (§5.1–§5.3): the store, what has been visited,
	/// the panel when it is up, the key a half-typed bookmark command waits for, the rename
	/// field, and the dangerous confirmation.
	bookmarks:   book::Bookmarks,
	frecency:    frecency::Frecency,
	visited:     Option<PathBuf>,
	fuzzy:       Option<Fuzzy>,
	confirm:     Option<Confirm>,

	/// The devices (P17): the work in flight.
	device_job: Option<DeviceJob>,


	/// The bin (§ d/D and the R b view): the work in flight and the view itself.
	trash_job:  Option<TrashJob>,
	/// `p`'s own work in flight (P21): the byte-mover never runs on this thread.
	paste_job:  Option<paste::Job<paste::Done>>,
	/// The change-type tasks that can be taken back (P33⑫): the most recent last, ten deep.
	undo:       VecDeque<Undo>,
	/// The undo's own byte-mover, when one is going (P33⑫).
	undo_job:   Option<UndoJob>,

	/// `c z`: the archive's name being typed, and the pack in flight.
	prompt: Option<Prompt>,
	pack:   Option<pack::Job>,

	/// The filter field (§P13's `f`): what is being typed, while it is up.
	filter: Option<Edit>,

	/// The switches the keymap's own runs are: whether hidden files show, how the list is ordered,
	/// and what the facts column carries.
	hidden:    bool,
	sort:      (SortBy, bool),
	/// What a `,r` press shuffles by: a new seed is a new order.
	sort_seed: u64,
	/// The fact columns the rows carry (P33⑲): the size always, and the rest a set of switches.
	columns:   Columns,
	help:      bool,
	help_from: usize,
	/// The interactive help (P21's `?`): what was typed into its box, the row it stands on, and
	/// whether the box owns the keyboard.
	/// `P21`: `L`'s own panel — three ways to link, picked by key or by click.
	link_pick: Option<usize>,
	/// `/` (P21): the box while it is open, and the pattern the list keeps colouring by once it is
	/// kept.
	find:      Option<Edit>,
	find_last: Option<String>,
	help_query: Edit,
	help_at:    usize,
	help_focus: PanelFocus,

	/// The keys of a sequence that is still open, the way yazi waits for a chord's next key.
	sequence: Option<Sequence>,
	/// When that sequence last had a key: the `which` hint's own clock, which expires it (P18).
	which_at: Option<Instant>,
	/// Where the replay's own pointer stands, put there by its `hover` step: the wheel's routing
	/// reads it, so a replay can stand over the preview column the way a hand would (P18).
	replay_at: Option<egui::Pos2>,

	/// When each overlay scrollbar (current list, preview) was last touched, for the fade.
	bars_moved: [Option<Instant>; 2],

	/// The body the columns live in, as the last frame laid it out: the wheel hit-tests here.
	body: Rect,

	/// Whether the first list has been seen at all — the metric fires once.
	first: bool,

	/// The seed a replay feeds in (`CRABHOP_KEYS`, `;`-separated), and the shots it may ask for.
	seed:      Vec<String>,
	seed_next: usize,
	seed_at:   Instant,
	shot:      Option<(String, usize)>,
	/// A reveal whose directory is still being read: it waits for the snapshot rather than failing.
	reveal:    Option<PathBuf>,
	/// Where the pointer was this frame: the hover state is drawn from it.
	/// The panel's own place as last drawn (P37③): the harness reads its height — the floor is a
	/// promise about a rect, not a formula.
	menu_panel:  Option<egui::Rect>,

	hover:     Option<egui::Pos2>,
	/// Whether the real fonts are in. They go in before the first frame now; the
	/// `CRABHOP_FONTS=late` arm of the A/B starts false and swaps them behind the first frame.
	dressed:   bool,
	/// The first frame that draws with the real faces: 1 with the fonts early, 2 when the swap
	/// happens behind the first frame. The A/B reads this through `metrics::first_dressed`.
	dress_at:  u64,
	shot_wait: Option<u32>,
	frame:     u64,
}

/// Who owns a wheel (P38③): one decision for the whole window, in one place — a standing panel
/// first (its pages walk their own window with it, P36②), then whatever stands under the pointer:
/// the tab bar, the output panel, the parent column, the preview, the help list — and the list
/// itself otherwise (the bin, while it is the view). The routing used to be a chain of ifs with
/// the menus' own guard bolted on; every new owner is now one arm here and one arm in the route.
#[derive(Clone, Copy, PartialEq)]
enum WheelOwner {
	Menu,
	Tabs,
	Output,
	Parent,
	Preview,
	Help,
	List,
}

impl App {
	pub(crate) fn new(cc: &eframe::CreationContext<'_>) -> Self {
		// The Maple faces go in before the first frame, the way they always did: the A/B (see
		// docs/dev/report.md, slice 0) measured the swap *behind* the first frame at ~35ms more to
		// the first paint and ~95ms more to the frame that actually carries the type — on this
		// stack the deferred first frame falls back to the system faces, which are the expensive
		// ones to shape, and the deferred arm pays for Maple on the second frame anyway.
		// `CRABHOP_FONTS=late` is that other arm, kept for the measurement.
		let late = std::env::var("CRABHOP_FONTS").is_ok_and(|mode| mode == "late");
		if !late {
			Tokens::install(&cc.egui_ctx);
		}

		let seed: Vec<String> = std::env::var("CRABHOP_KEYS")
			.map(|keys| keys.split(';').map(|step| step.trim().to_owned()).filter(|s| !s.is_empty()).collect())
			.unwrap_or_default();

		Self::with_context(cc.egui_ctx.clone(), seed)
	}

	/// The window with an egui context alone (P25): a `CreationContext` is what a *real* window
	/// hands over, and the headless harness has none — no window, no surface. Everything `new`
	/// takes from one is the context itself (and the fonts, which both callers install).
	pub(crate) fn with_context(ctx: egui::Context, seed: Vec<String>) -> Self {
		let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
		// the font arm is read once more here: `new` weighs it for the early install, and the
		// first dressed frame is the other half of the same measurement
		let late = std::env::var("CRABHOP_FONTS").is_ok_and(|mode| mode == "late");
		let config = Config::load();
		let shares = config.columns.shares();

		// the platform's input method is allowed in: Chinese input in the window's own fields is
		// the point of it (P18)
		ctx.send_viewport_cmd(egui::ViewportCommand::IMEAllowed(true));

		// the PDF library is bound before a document is hovered, so a missing one is a line at
		// startup rather than a surprise at the first `.pdf`
		crate::preview::warm_pdf();

		Self {
			tokens: Tokens::MOCHA,
			config,
			shares,
			cache: FolderCache::spawn(ctx.clone()),
			keys: Keys::load(),
			tabs: vec![ Tab::at(start) ],
			active: 0,
			rename: None,
			rename_at: None,
			rename_focus: false,
			tabs_scroll: 0.0,
			plus_hit: None,
			close_hit: None,
			tab_close_hits: Vec::new(),
			tab_chip_hits: Vec::new(),
			status_index: None,
			selection: HashSet::new(),
			yank: None,
			preview: PreviewPane::new(ctx.clone()),
			tasks: Vec::new(),
			spot: false,
			spot_chord: false,
			bulk: None,
			item_rename: None,
			toasts: Toasts::default(),
			menu: None,
			open_failed: std::sync::mpsc::channel(),
			menu_note: std::sync::mpsc::channel(),
			terminal_dry: false,
			terminal_log: std::sync::mpsc::channel(),
			open_dry: false,
			open_log: std::sync::mpsc::channel(),
			mimeapps_for_tests: None,
			mimeapps_path_for_tests: None,
			mimeapps_system_path_for_tests: None,
			frecency_path_for_tests: None,
			uri_offers: true,
			cmdline: None,
			history: History::load(),
			job: None,
			output: None,
			openwith: None,
			open_history: open::History::load(),
			bookmarks: book::Bookmarks::load(),
			frecency: frecency::Frecency::load(),
			visited: None,
			fuzzy: None,
			confirm: None,
			device_job: None,
			trash_job: None,
			paste_job: None,
			undo: VecDeque::new(),
			undo_job: None,
			prompt: None,
			pack: None,
			filter: None,
			hidden: true,
			sort: (SortBy::Name, false),
			sort_seed: 0,
			columns: Columns::default(),
			help: false,
			help_from: 0,
			link_pick: None,
			find: None,
			find_last: None,
			help_query: Edit::new(String::new()),
			help_at: 0,
			help_focus: PanelFocus::default(),
			sequence: None,
			which_at: None,
			replay_at: None,
			bars_moved: [ None, None ],
			body: Rect::NOTHING,
			cursor_name: None,
			first: false,
			seed,
			seed_next: 0,
			seed_at: Instant::now(),
			shot: std::env::var("CRABHOP_SHOT").ok().map(|prefix| (prefix, 1)),
			reveal: None,
			hover: None,
			menu_panel: None,
			dressed: !late,
			dress_at: if late { 2 } else { 1 },
			shot_wait: None,
			frame: 0,
		}
	}

	fn tab(&self) -> &Tab {
		&self.tabs[self.active]
	}

	fn tab_mut(&mut self) -> &mut Tab {
		&mut self.tabs[self.active]
	}

	/// The bin, while it is this tab's own view (P27).
	fn bin(&self) -> Option<&TrashView> {
		self.tab().trash.as_ref()
	}

	fn bin_mut(&mut self) -> Option<&mut TrashView> {
		self.tab_mut().trash.as_mut()
	}

	/// Whether the tab on screen is the bin's own (P27): the middle column's branch, the hint
	/// line, the body's height and the list keys all read it.
	fn in_bin(&self) -> bool {
		self.tab().trash.is_some()
	}

	fn entries(&self) -> Arc<Snapshot> {
		self.cache.get(self.tab().path())
	}

	/// The parent column's own view: the path to list and the row to light inside it. It is the
	/// directory the tab stepped out of when the stack holds one; otherwise — a tab at the root of
	/// its stack, which is every fresh tab — the cwd's filesystem parent, with the cwd's own row
	/// lit by looking it up. `None` only for a cwd with no parent at all: the filesystem root.
	fn parent_view(&self) -> Option<(PathBuf, usize)> {
		let (path, cursor) = parent_path(&self.tab().stack, self.tab().path())?;

		Some(match cursor {
			Some(cursor) => (path, cursor),
			None => {
				let snapshot = self.cache.get(&path);
				let cursor = snapshot.entries.iter().position(|e| path.join(&e.name) == self.tab().path()).unwrap_or(0);
				(path, cursor)
			}
		})
	}

	fn cursor(&self) -> usize {
		self.tab().stack.last().expect("the stack is never empty").cursor
	}

	fn selected(&self) -> Option<PathBuf> {
		let tab = self.tab();
		let snapshot = self.entries();
		let index = *tab.view.get(tab.stack.last().expect("never empty").cursor)?;

		snapshot.entries.get(index).map(|entry| tab.path().join(&entry.name))
	}

	/// The entry the cursor is on, for the status bar's own read of it.
	fn selected_entry(&self) -> Option<Entry> {
		let tab = self.tab();
		let snapshot = self.entries();
		let index = *tab.view.get(tab.stack.last().expect("never empty").cursor)?;

		snapshot.entries.get(index).cloned()
	}

	/// Rebuild the view of the active tab when the directory, the switches or the ordering move.
	fn refresh_view(&mut self) {
		let snapshot = self.cache.get(self.tab().path());
		let key = (
			Arc::as_ptr(&snapshot) as usize,
			self.tab().path().to_owned(),
			self.hidden,
			self.sort,
			self.tab().filter.clone(),
			self.tab().symlinks,
		);
		if self.tab().view_of.as_ref() == Some(&key) {
			return;
		}

		let same_dir = self.tab().view_of.as_ref().is_some_and(|key| key.1 == *self.tab().path());
		let mut view = ordered(&snapshot.entries, self.sort.0, self.sort.1, self.hidden, self.sort_seed);
		if let Some(pattern) = &self.tab().filter {
			view.retain(|index| matches(pattern, &snapshot.entries[*index].name));
		}
		// `g f`: the list keeps only what is a link, and says so in the status bar
		if self.tab().symlinks {
			view.retain(|index| snapshot.entries[*index].link);
		}
		let len = view.len();

		// a directory read again (a watcher's re-read) keeps the cursor on the same *name*: the
		// index it holds would slide onto a neighbour when a file appears above it (P18)
		if same_dir
			&& let Some(name) = &self.cursor_name
			&& let Some(at) = view.iter().position(|index| snapshot.entries[*index].name == *name)
		{
			self.tab_mut().stack.last_mut().expect("never empty").cursor = at;
		}

		// a `leave` whose listing had not landed sat on the first row: it sits down properly the
		// moment the names arrive, on the directory it came out of (P18)
		let seat = self.tab().seat.clone();
		if let Some((dir, name)) = seat
			&& *self.tab().path() == dir
			&& let Some(at) = view.iter().position(|index| snapshot.entries[*index].name == name)
		{
			let tab = self.tab_mut();
			tab.stack.last_mut().expect("never empty").cursor = at;
			tab.seat = None;
			tab.scroll.follow(at);
		}
		let tab = self.tab_mut();
		tab.view = view;
		tab.view_of = Some(key);
		tab.scroll.len = len;

		// the listing landed (or the switches moved it): the cursor's own file is what the column
		// previews now, and the sweeps read their lists from the view that just became current
		// (P27⑥ — the view is why `look` waited, and it is what `look` was waiting for)
		self.look();
	}

	/// After a switch that can shorten the list: nobody's cursor may be left past its end.
	fn clamp(&mut self) {
		let len = self.tab().view.len();
		let cursor = self.cursor().min(len.saturating_sub(1));
		self.tab_mut().stack.last_mut().expect("never empty").cursor = cursor;

		let scroll = &mut self.tab_mut().scroll;
		scroll.follow(cursor);
		self.look();
	}

	// ---- the interaction, all of it decided here ----

	fn act(&mut self, action: Action, ctx: &Context) {
		let len = self.tab().view.len();

		match action {
			// the bin's own tab takes the list's own keys (P27): the rows on screen are the bin's,
			// so the cursor, the pages, Enter and `d`/`D` mean the bin's own things — everything
			// else stays the keymap's, and navigation leaves the bin behind
			Action::Up if self.in_bin() => self.bin_move(-1),
			Action::Down if self.in_bin() => self.bin_move(1),
			Action::Top if self.in_bin() => self.bin_seat(0),
			Action::Bottom if self.in_bin() => self.bin_seat(usize::MAX),
			Action::Page { full, back } if self.in_bin() => self.bin_page(full, back),
			Action::Enter if self.in_bin() => self.trash_restore_standing(),
			Action::Remove(_) if self.in_bin() => self.trash_purge_standing(),
			Action::Up => self.set_cursor(self.cursor().saturating_sub(1)),
			Action::Down => self.set_cursor((self.cursor() + 1).min(len.saturating_sub(1))),
			Action::Top => self.set_cursor(0),
			Action::Bottom => self.set_cursor(len.saturating_sub(1)),
			Action::Parent => self.leave(),
			Action::Enter => self.open(),
			Action::Toggle => self.toggle(),
			Action::Quit => ctx.send_viewport_cmd(ViewportCommand::Close),
			Action::Hidden => {
				self.hidden = !self.hidden;
				self.refresh_view();
				self.clamp();
			}
			Action::Sort(by, reverse) => {
				self.sort = (by, reverse);
				if by == SortBy::Random {
					// a re-press shuffles again: the seed is what makes it another draw
					self.sort_seed = self.sort_seed.wrapping_add(1);
				}
				self.refresh_view();
				self.clamp();
			}
			Action::Columns(what) => match what {
				ColumnsDo::Toggle(col) => self.columns.toggle(col),
				ColumnsDo::Show(col) => self.columns.show(col),
				ColumnsDo::Reset => self.columns.reset(),
			},
			Action::Help => {
				self.help = !self.help;
				self.help_from = 0;
				self.help_at = 0;
				self.help_focus = PanelFocus::default();
				if !self.help {
					self.help_query = Edit::new(String::new());
				}
			}
			Action::TabCreate => self.tab_create(),
			Action::TabSwitch(index, relative) => self.tab_switch(index, relative),
			Action::TabSwap(step) => self.tab_swap(step),
			Action::TabRename => self.start_rename(),
			Action::TabClose => self.tab_close(ctx),
			Action::BulkCreate => self.bulk_open(),
			Action::Rename(at) => self.rename_open(at),
			Action::Open => self.open_default(),
			Action::Copy(what) => self.copy(what, ctx),
			Action::ToggleAll(on) => self.toggle_all(on),
			Action::OpenWith => self.openwith_open(),
			Action::Shell { block, command } => match command {
				Some(command) => self.shell_run(&command),
				None => {
					self.cmdline = Some(Cmdline { block, edit: Edit::new(String::new()), comp: None, recall: None });
				}
			},
			Action::Escape => self.escape(),
			// `i` (`z`/`Z` are its aliases): the one door to the index panel
			Action::Index | Action::FuzzyJump => self.fuzzy_index(),
			Action::Devices(do_) => self.devices(do_),
			Action::Search { content } => self.fuzzy_search(content),
			Action::Remove(permanent) => self.remove(permanent),
			Action::Yank { cut } => self.yank(cut, ctx),
			Action::Unyank => self.unyank(),
			// `u` (`Ctrl+Z` is its twin, handled with the global four): take the most recent
			// change-type task back (P33⑫)
			Action::Undo => self.undo_last(),
			// `T` (P35③): the cursor's door into a terminal
			Action::TerminalHere => self.terminal_here(),
			// `o` (P36④): the cursor row's own context menu — the right click's door
			Action::MenuHere => self.menu_on_cursor(),
			Action::History { back } => self.travel(back),
			Action::Follow => self.follow(),
			Action::SmartEnter => self.smart_enter(),
			Action::Cd(path) => self.cd(&path),
			Action::Link { kind } => self.link(kind),
			Action::Filter => self.filter_open(),
			Action::Find => self.find_open(),
			Action::LinkPanel => self.link_pick = Some(0),
			Action::FindNext { forward } => self.find_arrow(forward),
			Action::Symlinks => self.toggle_symlinks(),
			Action::Page { full, back } => self.page(full, back),
			Action::EnterDir => self.enter_dir(),
			Action::Create => self.create_open(),
			Action::Paste { force } => self.paste(force),
			Action::TrashView => self.trash_view_open(),
			Action::Compress => self.compress_open(),
			// `K`/`J`: the keymap's own seek into the preview column.
			Action::Seek(step) => {
				self.preview.seek(step);
			}
			// `<Tab>`: the spot card §4.6.
			Action::Spot => {
				self.spot = !self.spot;
				self.spot_chord = false;
			}
			other => eprintln!("crabhop: {other:?} lands in N1"),
		}

		ctx.request_repaint();
	}

	fn set_cursor(&mut self, cursor: usize) {
		self.tab_mut().stack.last_mut().expect("the stack is never empty").cursor = cursor;
		let scroll = &mut self.tab_mut().scroll;
		scroll.follow(cursor);
		metrics::cursor(cursor, self.tab().view.len());
		self.look();
		self.bars_moved[0] = Some(Instant::now());
	}

	/// A single click on a row seats the cursor there — yazi's own mouse semantics, and the one
	/// place the event path and the replay both come through. A position past the list is a click
	/// on the blank and decides nothing.
	fn click_row(&mut self, position: usize) {
		if position >= self.tab().view.len() {
			return;
		}

		self.set_cursor(position);
	}

	/// Whichever field owns the keyboard, in the same order `handle()` asks its owners: what
	/// Ctrl+C / Ctrl+X act on (P18).
	fn active_edit_mut(&mut self) -> Option<&mut Edit> {
		if let Some(bulk) = &mut self.bulk {
			return Some(&mut bulk.edit);
		}
		if let Some(rename) = &mut self.item_rename {
			return Some(&mut rename.edit);
		}
		if let Some(cmdline) = &mut self.cmdline {
			return Some(&mut cmdline.edit);
		}
		if let Some(openwith) = &mut self.openwith {
			return Some(&mut openwith.edit);
		}
		if let Some(fuzzy) = &mut self.fuzzy {
			return Some(&mut fuzzy.query);
		}
		if let Some(prompt) = &mut self.prompt {
			return Some(&mut prompt.edit);
		}
		if let Some(filter) = &mut self.filter {
			return Some(filter);
		}

		None
	}

	fn look(&mut self) {
		// the bin's own tab previews its own items (P27): the directory behind it was never left,
		// but the standing row is the bin's
		if self.in_bin() {
			self.look_at_trash();
			return;
		}

		self.cursor_name = self.selected().as_deref().and_then(Path::file_name).map(|name| name.to_string_lossy().into_owned());

		let snapshot = self.entries();
		let (index, cursor, dir) = {
			let tab = self.tab();
			let cursor = tab.stack.last().expect("the stack is never empty").cursor;
			(tab.view.get(cursor).copied(), cursor, tab.path().to_owned())
		};
		let entry = index.and_then(|i| snapshot.entries.get(i)).cloned();

		match entry.as_ref() {
			Some(entry) => self.preview.look(&dir.join(&entry.name), Some(entry)),
			None => self.preview.clear(),
		}

		// what a sweep will want: the first screenful when a directory opens, and the cursor's
		// neighbours on every move — images only, and warmed where no decode wants the slot first.
		// The lists are read from the view: one that is not this directory's yet (a directory just
		// entered, a listing not yet rebuilt) warms nothing — the rebuild comes back through here
		// the moment it is current, which is also what fills the idle sweep's own queue (P27⑥)
		if !self.tab().view_of.as_ref().is_some_and(|key| key.1 == dir && key.0 == Arc::as_ptr(&snapshot) as usize) {
			return;
		}

		let view = self.tab().view.clone();
		let cell = |at: usize| -> Option<(PathBuf, Option<Entry>)> {
			view.get(at).and_then(|i| snapshot.entries.get(*i)).map(|e| (dir.join(&e.name), Some(e.clone())))
		};

		let visible: Vec<(PathBuf, Option<Entry>)> = (0..view.len().min(16)).filter_map(cell).collect();
		let mut around: Vec<(PathBuf, Option<Entry>)> = Vec::new();
		for at in cursor.saturating_sub(3)..(cursor + 4).min(view.len()) {
			if at != cursor
				&& let Some(item) = cell(at)
			{
				around.push(item);
			}
		}
		// the idle sweep's own list (P27⑥'s L3): the first fifty rows, which is what a person
		// walking the list is about to hover
		let ahead: Vec<(PathBuf, Option<Entry>)> = (0..view.len().min(50)).filter_map(cell).collect();

		self.preview.warm(&dir, &visible, &around, &ahead);
	}

	fn toggle(&mut self) {
		if let Some(path) = self.selected()
			&& !self.selection.remove(&path)
		{
			self.selection.insert(path);
		}
	}

	/// `<C-a>` / `<C-r>`: every row the view shows becomes selected, or the selection is turned
	/// over — the view's own rows, not the directory's, so a filter and the hidden switch are
	/// respected.
	fn toggle_all(&mut self, on: Option<bool>) {
		let snapshot = self.entries();
		let view = self.tab().view.clone();
		let dir = self.tab().path().to_owned();
		let paths: Vec<PathBuf> = view.iter().filter_map(|i| snapshot.entries.get(*i)).map(|e| dir.join(&e.name)).collect();

		crate::app::toggled(&mut self.selection, &paths, on);
	}

	/// The `c` family: every target's own reading of the thing asked for, one per line, on the
	/// system clipboard — and a toast that says what went there.
	fn copy(&mut self, what: CopyWhat, ctx: &Context) {
		let targets = self.targets();
		if targets.is_empty() {
			return;
		}

		let lines: Vec<String> = targets
			.iter()
			.filter_map(|path| match what {
				CopyWhat::Path => Some(path.display().to_string()),
				CopyWhat::Dirname => path.parent().map(|at| at.display().to_string()),
				CopyWhat::Filename => {
					path.file_name().map(|name| name.to_string_lossy().into_owned())
				}
				CopyWhat::Stem => path.file_stem().map(|stem| stem.to_string_lossy().into_owned()),
				CopyWhat::Url => Some(crate::preview::file_uri(path)),
				CopyWhat::DirUrl => {
					let dir = if path.is_dir() { path.as_path() } else { path.parent()? };
					Some(crate::preview::file_uri(dir))
				}
			})
			.collect();

		if lines.is_empty() {
			return;
		}

		let text = lines.join("\n");
		ctx.copy_text(text);

		let words = match what {
			CopyWhat::Path => "路径",
			CopyWhat::Dirname => "目录路径",
			CopyWhat::Filename => "文件名",
			CopyWhat::Stem => "文件名（无扩展名）",
			CopyWhat::Url => "URL",
			CopyWhat::DirUrl => "目录 URL",
		};
		self.toasts.push(format!("已复制{words}（{} 项）", lines.len()), toast::Kind::Ok);
	}

	/// The plain open of `<Enter>` / `o` / yazi's own `enter`: a directory is entered, a file is
	/// handed to the system's opener (`xdg-open`) and the window waits for nothing. open-with
	/// keeps its own field, and nothing here touches it.
	fn open(&mut self) {
		// the pointer's column decides, the way the wheel does (P28's long tail): a Return with the
		// pointer standing over the archive tree's own folder folds that folder instead of opening
		// the file the cursor sits on
		if self.fold_under_pointer() {
			return;
		}

		let Some(path) = self.selected() else { return };

		if !path.is_dir() {
			// the system's own default, watched to its exit (P30): a launcher that is not there, or
			// one that says no, is said out loud on a toast instead of being swallowed
			crate::open::system(path, self.open_failed.0.clone());
			return;
		}

		self.tab_mut().stack.push(View { path, cursor: 0 });
		self.tab_mut().scroll.jump(0.0);
		self.arrived();
	}

	/// `<Enter>` (P37①, the user's re-cut): the **default opener** for whatever the cursor stands
	/// on — a file and a directory alike. A directory is handed to the default application (the
	/// file manager) rather than stepped into; the way in is `l` and `<Right>`. A `Terminal=true`
	/// default handler rides the machine's terminal chain (P37④); everything else goes to the
	/// system's own opener, watched to its exit (P30).
	fn open_default(&mut self) {
		// the pointer's column decides, the way the wheel does (P28's long tail): a Return with the
		// pointer standing over the archive tree's own folder folds that folder instead of opening
		if self.fold_under_pointer() {
			return;
		}

		let Some(path) = self.selected() else { return };
		if let Some(program) = self.default_terminal(&path) {
			// `Terminal=true` (P37④): the program runs inside the machine's own terminal
			let folder = path.parent().map(Path::to_owned).unwrap_or_else(|| self.tab().path().to_owned());
			self.spawn_in_terminal(program, folder);
			return;
		}

		if self.open_dry {
			let _ = self.open_log.0.send(path);
			return;
		}
		crate::open::system(path, self.open_failed.0.clone());
	}

	/// The person's own default handler for a path, when it is a `Terminal=true` entry (P37④):
	/// the menu's own `exec_argv` door and the machine's own directories, read fresh. `None` when
	/// nothing is preferred, or the preferred one wants no terminal.
	fn default_terminal(&self, path: &Path) -> Option<Vec<String>> {
		let name = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
		let mime = crate::mime::guess(&name, path.is_dir());
		let home = std::env::var_os("HOME").map(PathBuf::from);
		let entries = crate::desktop::scan(&crate::desktop::app_dirs(home.as_deref()));

		crate::desktop::default_terminal(&entries, &self.preferences_text(), &self.system_preferences_text(), &mime, path)
	}

	/// The tab has arrived in a new directory: the trail notes it — and a bin the tab was showing is
	/// left behind, because a directory change is what walking away means (P27). The left column's
	/// own seat and scroll go with it (P33⑧): a new parent is a new column.
	fn arrived(&mut self) {
		self.tab_mut().trash = None;
		self.tab_mut().parent = ParentView::default();
		let path = self.tab().path().to_owned();
		self.tab_mut().history.push(&path);
		metrics::cwd(&path);
		self.look();
	}

	/// Whether the pointer stands on a folder of the preview column's archive tree — and folded it
	/// (P28's long tail). The replay's own pointer counts, the way the wheel's routing counts it.
	fn fold_under_pointer(&mut self) -> bool {
		let Some(at) = self.replay_at.or(self.hover) else { return false };
		let Some(column) = chrome::columns(self.body, self.shares).preview else { return false };
		if !column.contains(at) {
			return false;
		}

		self.preview.archive_fold_at(column, at)
	}

	/// `f`: the filter field opens on the current pattern (or clears one that is on); typing
	/// re-filters the list as it goes, Enter keeps what is typed and `Esc` clears it.
	fn filter_open(&mut self) {
		if self.tab().filter.is_some() && self.filter.is_none() {
			self.tab_mut().filter = None;
			self.refresh_view();
			self.clamp();
			return;
		}

		let current = self.tab().filter.clone().unwrap_or_default();
		self.filter = Some(Edit::new(current));
	}

	/// `/`: the find box opens on the pattern the list is already carrying, so a second `/` picks
	/// the search up where it was left.
	fn find_open(&mut self) {
		self.find = Some(Edit::new(self.find_last.clone().unwrap_or_default()));
	}

	/// The pattern the list colours by: what is being typed, or what was kept (P21).
	fn find_pattern(&self) -> Option<String> {
		let live = self.find.as_ref().map(|edit| edit.text.clone());
		live.or_else(|| self.find_last.clone()).filter(|pattern| !pattern.is_empty())
	}

	/// `n` / `N`: the cursor walks to the next row whose name holds the pattern, or the one before
	/// it — and stays where it is when there is none.
	fn find_arrow(&mut self, forward: bool) {
		let Some(pattern) = self.find_pattern() else {
			self.toasts.push("先按 / 输入要查的内容", toast::Kind::Warn);
			return;
		};

		let snapshot = self.entries();
		let view = self.tab().view.clone();
		if let Some(at) = find_row(&view, &snapshot.entries, &pattern, self.cursor(), forward) {
			self.set_cursor(at);
		}
	}

	/// The pattern as typed: the tab's filter is what the view is rebuilt from, so every keystroke
	/// goes through one place.
	fn filter_apply(&mut self, pattern: Option<String>) {
		self.tab_mut().filter = pattern;
		self.refresh_view();
		self.clamp();
	}

	/// `g f` (P21's table): keep only the symbolic links in the list, and put everything back when
	/// pressed again — a switch, the way the hidden-files one is.
	fn toggle_symlinks(&mut self) {
		let on = !self.tab().symlinks;
		self.tab_mut().symlinks = on;
		self.refresh_view();
		self.clamp();
		self.toasts.push(if on { "只显示符号链接" } else { "显示全部项目" }, toast::Kind::Ok);
	}

	/// `<C-u>`/`<C-d>`/`<C-b>`/`<C-f>`/`<PageUp>`/`<PageDown>`/`<S-PageUp>`/`<S-PageDown>`: the
	/// cursor walks half the view or the whole of it, and the list follows.
	fn page(&mut self, full: bool, back: bool) {
		let rows = self.tab().scroll.rows.max(1);
		let step = if full { rows.saturating_sub(1).max(1) } else { (rows / 2).max(1) };
		let cursor = self.cursor() as isize;
		let len = self.tab().view.len();
		let next = if back { cursor - step as isize } else { cursor + step as isize };

		self.set_cursor(next.clamp(0, len.saturating_sub(1) as isize) as usize);
	}

	/// `l`: into the directory under the cursor — a file is left alone (that is what `enter` is for).
	fn enter_dir(&mut self) {
		let Some(entry) = self.selected_entry() else { return };
		if entry.dir {
			self.open();
		}
	}

	/// `a`: a name, and the name decides what it is — a file when it has an extension, a directory
	/// when it has none (P21's table).
	fn create_open(&mut self) {
		self.prompt = Some(Prompt {
			title:  "创建（带后缀=文件，不带=目录）".to_owned(),
			edit:   Edit::new(String::new()),
			action: PromptDo::Create,
		});
	}

	/// `p` / `P`: what was held put down beside the cursor. A name already standing at the
	/// destination is asked about first unless the keymap forced it (P21's table).
	fn paste(&mut self, force: bool) {
		let Some(yank) = self.yank.clone() else {
			self.toasts.push("先复制或剪切（y / x）", toast::Kind::Warn);
			return;
		};

		let dest = self.tab().path().to_owned();
		let collisions: Vec<String> = yank
			.paths
			.iter()
			.filter_map(|path| path.file_name().map(|name| dest.join(name)))
			.filter(|to| to.symlink_metadata().is_ok())
			.map(|to| frecency::display(to.as_path()))
			.collect();

		if !collisions.is_empty() && !force {
			self.confirm = Some(Confirm {
				text:   format!("已存在同名 {} 个，覆盖粘贴吗？", collisions.len()),
				names:  collisions.into_iter().take(5).collect(),
				action: ConfirmDo::Paste { cut: yank.cut, force: true },
			});
			return;
		}

		self.paste_job = Some(paste::run(yank.paths.clone(), dest, yank.cut, force));
	}

	/// `a`'s Enter: the typed name becomes a file when it carries an extension and a directory when
	/// it does not; the listing is read again and the cursor waits for the new row (P21's table).
	fn create_submit(&mut self, typed: &str) {
		let name = typed.trim();
		if name.is_empty() {
			return;
		}

		let cwd = self.tab().path().to_owned();
		let path = cwd.join(name);
		// an extension is a dot with something after it: `notes` is a directory, `notes.txt` a file
		let file = Path::new(name)
			.file_name()
			.and_then(|name| name.to_str())
			.and_then(|name| name.rsplit_once('.'))
			.is_some_and(|(stem, ext)| !stem.is_empty() && !ext.is_empty());

		let result = if file { std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map(|_| ()) } else { std::fs::create_dir_all(&path) };
		match result {
			Ok(()) => {
				self.cache.forget(&cwd);
				self.tab_mut().seat = Some((cwd, name.rsplit('/').next().unwrap_or(name).to_owned()));
				self.toasts.push(if file { format!("已创建文件 {name}") } else { format!("已创建目录 {name}") }, toast::Kind::Ok);
			}
			Err(e) => self.toasts.push(format!("创建失败：{e}"), toast::Kind::Err),
		}
	}

	/// `M`'s panel acting on a door: the standing device for mount/unmount, the cwd's own device
	/// for the remount and the jump, and a toast for the URI family (P21's second wave).
	fn device_door(&mut self, door: DeviceDoor) {
		match door {
			DeviceDoor::Mount | DeviceDoor::MountAndJump => {
				let Some(id) = self.device_standing() else {
					self.toasts.push("没有选中的设备", toast::Kind::Warn);
					return;
				};
				let jump = door == DeviceDoor::MountAndJump;
				self.device_on(id, false, false, jump);
			}
			DeviceDoor::Unmount { force } => {
				let Some(id) = self.device_standing() else {
					self.toasts.push("没有选中的设备", toast::Kind::Warn);
					return;
				};
				self.device_on(id, true, force, false);
			}
			DeviceDoor::Remount => self.devices(DeviceDo::Remount),
			DeviceDoor::Jump => self.devices(DeviceDo::Jump),
			DeviceDoor::Uri(what) => self.devices(DeviceDo::Uri(what)),
		}
	}

	/// The device the panel's cursor stands on, if the panel is up.
	fn device_standing(&self) -> Option<String> {
		let fuzzy = self.fuzzy.as_ref()?;
		let item = fuzzy.hits.get(fuzzy.at).and_then(|hit| fuzzy.items.get(hit.at))?;

		match &item.payload {
			FuzzyPayload::Device(id) => Some(id.clone()),
			_ => None,
		}
	}

	/// `M`'s panel acting on one device: mount (and go, when the key asked), or unmount and eject.
	fn device_on(&mut self, id: String, unmount: bool, force: bool, jump: bool) {
		self.fuzzy_close();
		let (tx, rx) = std::sync::mpsc::channel();
		std::thread::spawn(move || {
			let answer = if unmount {
				Answer::Done(mount::unmount(&id, force, true))
			} else {
				Answer::At(mount::mount(&id))
			};
			let _ = tx.send(answer);
		});

		let do_ = if unmount { DeviceDo::UnmountPick { force } } else if jump { DeviceDo::MountPick } else { DeviceDo::MountHere };
		self.device_job = Some(DeviceJob { do_, rx });
		self.toasts.push(
			match (unmount, jump) {
				(true, _) => "正在卸载…".to_owned(),
				(false, true) => "正在挂载并跳转…".to_owned(),
				(false, false) => "正在挂载…".to_owned(),
			},
			toast::Kind::Ok,
		);
	}

	/// The paste's answer: a toast, and the directory read again so the new names show.
	fn paste_poll(&mut self, ctx: &Context) {
		let Some(job) = self.paste_job.take() else { return };

		match job.poll() {
			None => self.paste_job = Some(job),
			Some(Ok(done)) => {
				let cut = self.yank.as_ref().is_some_and(|yank| yank.cut);
				if cut {
					self.yank = None;
				}
				self.cache.forget(self.tab().path());

				// the toast carries the undo of exactly this paste (P33⑫): the copies go to the
				// bin, or the moved set walks home — the pairs as they landed
				let word = if cut { "已移动" } else { "已复制" };
				let undo = if cut {
					Undo::Moved { pairs: done.pairs }
				} else {
					Undo::Paste { landed: done.pairs.into_iter().map(|(_, to)| to).collect() }
				};
				self.note(format!("{word} {} 项", done.count), undo);
			}
			Some(Err(e)) => self.toasts.push(format!("粘贴失败：{e}"), toast::Kind::Err),
		}

		if self.paste_job.is_some() {
			ctx.request_repaint_after(Duration::from_millis(60));
		}
	}

	/// `L`'s answer: a link into the parent column's own directory, named after what it points at
	/// — a name already standing there is refused rather than overwritten (P21's table).
	fn link_to(&mut self, kind: LinkKind, target: PathBuf) {
		let Some((dir, _)) = self.parent_view() else {
			self.toasts.push("没有可放置链接的另一列", toast::Kind::Warn);
			return;
		};

		let Some(name) = target.file_name() else { return };
		let link = dir.join(name);
		if link.symlink_metadata().is_ok() {
			self.toasts.push(format!("{} 已存在", link.display()), toast::Kind::Warn);
			return;
		}

		let result = match kind {
			LinkKind::Absolute => std::os::unix::fs::symlink(&target, &link),
			LinkKind::Relative => std::os::unix::fs::symlink(relative_target(&dir, &target), &link),
			LinkKind::Hard => std::fs::hard_link(&target, &link),
		};

		match result {
			Ok(()) => {
				self.cache.forget(&dir);
				let what = match kind {
					LinkKind::Hard => "硬链接",
					_ => "符号链接",
				};
				// one created link is one undoable change (P33⑫): the toast carries taking it away
				self.note(
					format!("已创建{what} {} → {}", link.display(), target.display()),
					Undo::Link(vec![ link ]),
				);
			}
			Err(e) => self.toasts.push(format!("链接失败：{e}"), toast::Kind::Err),
		}
	}

	/// The link family: each target gets a link beside it, named after itself, in the parent
	/// column's own directory — the other pane of this three-column world, and the place a
	/// link is usually wanted. `)` links to the absolute path, `(` to a relative one, `*` makes a
	/// hard link; a name already taken gets a toast rather than an overwrite.
	fn link(&mut self, kind: crate::keys::LinkKind) {
		use crate::keys::LinkKind;

		let Some((dir, _)) = self.parent_view() else {
			self.toasts.push("没有可放置链接的另一列", toast::Kind::Warn);
			return;
		};

		let targets = self.targets();
		if targets.is_empty() {
			return;
		}

		let mut made = 0usize;
		let mut created: Vec<PathBuf> = Vec::new();
		for path in &targets {
			let Some(name) = path.file_name() else { continue };
			let link = dir.join(name);

			if link.exists() {
				self.toasts.push(format!("{} 已存在", link.display()), toast::Kind::Warn);
				continue;
			}

			let result = match kind {
				LinkKind::Absolute => std::os::unix::fs::symlink(path, &link),
				// a relative symlink stores the target as the walk from its own directory:
				// `../paper/main.md`, not the absolute path
				LinkKind::Relative => std::os::unix::fs::symlink(relative_target(&dir, path), &link),
				LinkKind::Hard => std::fs::hard_link(path, &link),
			};

			match result {
				Ok(()) => {
					made += 1;
					created.push(link);
				}
				Err(error) => {
					self.toasts.push(format!("链接失败：{error}"), toast::Kind::Err);
					continue;
				}
			}
		}

		if made > 0 {
			self.cache.forget(&dir);
			let what = match kind {
				LinkKind::Hard => "硬链接",
				_ => "符号链接",
			};
			// the whole batch is one change (P33⑫): one undo takes every created link away
			self.note(format!("已创建 {made} 个{what}（{}）", dir.display()), Undo::Link(created));
		}
	}

	/// `g h` / `g a` / … : the well-known directories, with `~` expanded the way a shell expands it
	/// — a path that is not there gets a toast rather than a silent nothing.
	fn cd(&mut self, path: &str) {
		let Some(path) = expand_tilde(path, std::env::var_os("HOME").map(PathBuf::from)) else {
			self.toasts.push(format!("{path} 不存在"), toast::Kind::Warn);
			return;
		};

		if path.is_dir() {
			self.go(path);
		} else {
			self.toasts.push(format!("{} 不存在", path.display()), toast::Kind::Warn);
		}
	}

	/// `enter --bypass`: the plain open, and then — while the directory just entered holds a
	/// single child and that child is a directory — it keeps descending, so an `a/b/c` chain costs
	/// one key rather than three. The walk stops at the first directory with anything else in it.
	///
	/// The chain is read right here, on this thread: the point of the key is to arrive in one
	/// press, and a frame's worth of async reads per link would take that promise away. It is at
	/// most 64 bounded `read_dir`s, and only when a person asks to enter — if a chain on a slow
	/// mount ever hurts, N3 can defer it.
	fn smart_enter(&mut self) {
		let before = self.tab().path().to_owned();
		self.open();
		if self.tab().path() == before {
			return;
		}

		for _ in 0..64 {
			let dir = self.tab().path().to_owned();
			let entries = crate::folder::read_sync(&dir);

			let Some(child) = bypass_child(&dir, &entries, self.sort.0, self.sort.1, self.hidden) else {
				return;
			};

			self.tab_mut().stack.push(View { path: child, cursor: 0 });
			self.tab_mut().scroll.jump(0.0);
			self.arrived();
		}
	}

	/// `g f`: the cursor's own symlink is resolved and gone to — a directory is entered, a file is
	/// revealed where it lives. A path that is no link says so.
	fn follow(&mut self) {
		let Some(path) = self.selected() else { return };
		let Ok(target) = std::fs::read_link(&path) else {
			self.toasts.push("这不是符号链接", toast::Kind::Warn);
			return;
		};

		let parent = path.parent().unwrap_or(Path::new("/"));
		let target = followed(parent, &target);

		if target.is_dir() {
			self.go(target);
		} else if target.exists() {
			self.reveal(target);
		} else {
			self.toasts.push(format!("链接指向的 {} 不存在", target.display()), toast::Kind::Err);
		}
	}

	/// `H` / `L`: away along this tab's own trail of directories.
	fn travel(&mut self, back: bool) {
		let Some(path) = self.tab_mut().history.travel(back) else { return };

		self.tab_mut().trash = None;
		self.tab_mut().stack = vec![ View { path, cursor: 0 } ];
		self.tab_mut().scroll.jump(0.0);
		metrics::cwd(self.tab().path());
		self.look();
	}

	/// `h`: up one directory — always the directory the left column is already showing, whether
	/// that is the step the stack came down or, at the bottom of the stack, the cwd's own
	/// filesystem parent. A `g a`/`g d` jump resets the stack, so `h` has to leave from the
	/// bottom too (P18: `h` is never a no-op).
	fn leave(&mut self) {
		if self.tab().stack.len() == 1 {
			// the bottom: the filesystem parent, which the left column may well have read already
			let Some((parent, seat)) = self.parent_view() else { return };

			let left = self.tab().path().to_owned();
			let name = file_name(&left);
			let loading = self.cache.get(&parent).loading;

			// an unread listing cannot say which row the left directory is on yet: the first row
			// now, and the seat waits for the names (below)
			let waited = loading.then_some(name).flatten().map(|name| (parent.clone(), name));
			self.tab_mut().stack = vec![ View { path: parent, cursor: seat } ];
			self.tab_mut().seat = waited;
			self.tab_mut().scroll.jump(seat as f32);
			self.arrived();
			return;
		}

		let left = self.tab_mut().stack.pop().expect("just checked");
		let snapshot = self.entries();
		let parent = self.tab().stack.last().expect("the stack is never empty");

		// the cursor goes back to the directory we came out of; a listing that has not landed
		// cannot be looked through, so the name is left waiting for it
		let (cursor, seat) = match snapshot.entries.iter().position(|entry| parent.path.join(&entry.name) == left.path) {
			Some(index) => (index, None),
			None => (parent.cursor, file_name(&left.path).map(|name| (parent.path.clone(), name))),
		};

		let tab = self.tab_mut();
		tab.stack.last_mut().expect("the stack is never empty").cursor = cursor;
		tab.seat = seat;
		tab.scroll.follow(cursor);
		self.arrived();
	}

	// ---- the tabs (§2.4) ----

	fn tab_create(&mut self) {
		// the table's ceiling: ten tabs, and the eleventh press says so rather than making one
		if self.tabs.len() >= TAB_LIMIT {
			self.toasts.push(format!("最多 {TAB_LIMIT} 个标签页"), toast::Kind::Warn);
			return;
		}

		let path = self.tab().path().to_owned();
		let at = self.active + 1;

		self.tabs.insert(at, Tab::at(path));
		self.active = at;
		self.tabs_scroll = 0.0;
		self.look();
	}

	fn tab_switch(&mut self, index: isize, relative: bool) {
		self.active = switch_target(self.tabs.len(), self.active, index, relative);
		self.look();
	}

	fn tab_swap(&mut self, step: isize) {
		let target = self.active as isize + step;
		if (0..self.tabs.len() as isize).contains(&target) {
			self.tabs.swap(self.active, target as usize);
			self.active = target as usize;
		}
	}

	fn tab_close(&mut self, ctx: &Context) {
		if self.tabs.len() == 1 {
			// the last tab is the window: closing it quits, the way the keymap promises
			ctx.send_viewport_cmd(ViewportCommand::Close);
			return;
		}

		self.tabs.remove(self.active);
		self.active = self.active.min(self.tabs.len() - 1);
		self.look();
	}

	fn start_rename(&mut self) {
		self.rename = Some(self.tab().name());
		self.rename_at = Some(self.active);
		self.rename_focus = true;
	}

	fn commit_rename(&mut self) {
		if let (Some(buffer), Some(at)) = (self.rename.take(), self.rename_at.take()) {
			let name = buffer.trim();
			if let Some(tab) = self.tabs.get_mut(at) {
				tab.title = (!name.is_empty()).then(|| name.to_owned());
			}
		}
		self.rename_focus = false;
	}

	// ---- the bulk editor (§4.1) and the in-place rename (§4.3) ----

	fn bulk_open(&mut self) {
		self.bulk = Some(BulkEditor { edit: Edit::new(String::new()), lines: Vec::new() });
	}

	/// The plan is re-read whenever the column changes: the preview, the counts and the button's
	/// words all come from it, and nothing is decided twice.
	fn bulk_replan(&mut self) {
		let dir = self.tab().path().to_owned();
		if let Some(bulk) = &mut self.bulk {
			bulk.lines = bulk::parse(&bulk.edit.text, &dir);
		}
	}

	fn bulk_commit(&mut self) {
		let Some(bulk) = self.bulk.take() else { return };

		let report = bulk::commit(&bulk.lines);
		let (created, skipped) = bulk::tally(&bulk.lines);

		let dir = self.tab().path().to_owned();
		self.cache.forget(&dir);

		self.toasts.push(
			if skipped == 0 { format!("已创建 {created} 项") } else { format!("已创建 {created} 项，跳过 {skipped} 项") },
			toast::Kind::Ok,
		);

		// what could not be written goes to the status bar's own task group, one line at a time
		for (line, why) in report.failures {
			self.tasks.push(Progress { title: format!("创建失败：{line}"), fraction: None, rate: Some(why) });
		}
	}

	/// Start an in-place rename at the seat the keymap's own variant asks for (§4.3).
	fn rename_open(&mut self, at: RenameAt) {
		let Some(path) = self.selected() else { return };
		let Some(name) = path.file_name().map(|name| name.to_string_lossy().into_owned()) else { return };

		// the extension is what `before_ext` and `stem` seats are placed against; a dotfile's
		// leading dot is not one
		let dot = name.rfind('.').filter(|dot| *dot > 0);
		let text = match at {
			RenameAt::EmptyAll => String::new(),
			RenameAt::EmptyStem => dot.map(|dot| name[dot..].to_owned()).unwrap_or_default(),
			_ => name,
		};
		let cursor = match at {
			RenameAt::BeforeExt => dot.unwrap_or(text.len()),
			RenameAt::Start | RenameAt::EmptyStem => 0,
			_ => text.len(),
		};

		let mut edit = Edit::new(text);
		edit.cursor = cursor.min(edit.text.len());
		self.item_rename = Some(ItemRename { path, edit, note: None });
		self.rename_check();
	}

	/// The conflict check of §4.3, run at each keystroke: a name that is empty, has a separator, or
	/// is already taken by something else turns the field red and keeps Enter from landing.
	fn rename_check(&mut self) {
		let Some(rename) = &mut self.item_rename else { return };
		let name = rename.edit.text.as_str();

		rename.note = if name.is_empty() {
			Some("名称不能为空")
		} else if name.contains('/') {
			Some("名称不能包含 /")
		} else if name != rename.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
			&& rename.path.with_file_name(name).symlink_metadata().is_ok()
		{
			Some("已存在同名文件")
		} else {
			None
		};
	}

	fn rename_commit(&mut self) {
		let Some(rename) = self.item_rename.take() else { return };
		if rename.note.is_some() {
			// refused, and the field stays where it was to be mended
			self.item_rename = Some(rename);
			return;
		}

		let old = rename.path;
		if old.file_name().is_some_and(|name| name == rename.edit.text.as_str()) {
			return;
		}
		let new = old.with_file_name(&rename.edit.text);

		match std::fs::rename(&old, &new) {
			Ok(()) => {
				if let Some(dir) = old.parent() {
					self.cache.forget(dir);
				}
				if self.selection.remove(&old) {
					self.selection.insert(new.clone());
				}

				// a rename is a change a person may want back (P33⑫): the toast carries it
				let said = new.file_name().map_or_else(|| new.display().to_string(), |name| name.to_string_lossy().into_owned());
				self.note(format!("已重命名为 {said}"), Undo::Rename { from: old, to: new });
			}
			Err(e) => self.toasts.push(format!("重命名失败：{e}"), toast::Kind::Err),
		}
	}

	/// Double-clicking a row's name is the mouse's way into the same editor, seated like `r r`.
	fn rename_row(&mut self, at: usize) {
		self.set_cursor(at);
		self.rename_open(RenameAt::BeforeExt);
	}

	// ---- the command line (§4.8) ----

	/// Esc: put away what is open, or let the selection go when nothing is. The help closing on
	/// its own is the raw key's doing, so that stays quick even with a keymap file that cannot be
	/// read at all.
	fn escape(&mut self) {
		if self.output.take().is_some() {
			return;
		}
		if self.fuzzy.is_some() {
			self.fuzzy_close();
			return;
		}
		if self.prompt.take().is_some() {
			return;
		}
		if !self.selection.is_empty() {
			self.selection.clear();
		}

		// nothing else was open: a toast is what Esc takes away (P30's own message among them)
		if !self.toasts.is_empty() {
			self.toasts.clear();
		}
	}

	/// A command goes out: recorded in the history, run off this thread with `sh -c`, and its
	/// answer opens the output sheet when it lands.
	fn shell_run(&mut self, command: &str) {
		let cwd = self.tab().path().to_owned();
		let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

		self.job = Some(shell::spawn(command.to_owned(), cwd.clone(), cancel));
		self.history.record(command);
		self.history.save();
	}

	fn shell_poll(&mut self, ctx: &Context) {
		let Some(job) = &self.job else { return };
		let command = job.command.clone();

		match job.poll() {
			Ok(outcome) => {
				self.job = None;
				let code = outcome.code;
				self.toasts.push(
					format!("{command} → 退出码 {}", code.map_or_else(|| "中断".to_owned(), |code| code.to_string())),
					if code == Some(0) { toast::Kind::Ok } else { toast::Kind::Err },
				);
				self.output = Some(Output { title: command, code, lines: outcome.lines(), from: 0 });
			}
			Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint_after(Duration::from_millis(80)),
			Err(std::sync::mpsc::TryRecvError::Disconnected) => self.job = None,
		}
		ctx.request_repaint();
	}

	/// Tab: the candidates for the last token appear, or the standing one steps forward — either
	/// way the line is rewritten as it moves, the way a completion should read.
	fn cmdline_complete(&mut self) {
		let cwd = self.tab().path().to_owned();
		let Some(mut cmdline) = self.cmdline.take() else { return };

		if let Some(mut comp) = cmdline.comp.take() {
			comp.move_by(1);
			comp.apply(&mut cmdline.edit);
			cmdline.comp = Some(comp);
		} else {
			let items = complete::paths(&cmdline.edit.text, &cwd, 64);
			cmdline.comp = Completions::new(complete::token_start(&cmdline.edit.text), items);
			if let Some(comp) = cmdline.comp.take() {
				comp.apply(&mut cmdline.edit);
				cmdline.comp = Some(comp);
			}
		}

		self.cmdline = Some(cmdline);
	}

	/// ↑ / ↓: they walk the completions when one is up, and the history when none is — the
	/// keymap's own `[input]` recall.
	fn cmdline_recall(&mut self, step: isize) {
		let history = &self.history.lines;
		let Some(cmdline) = &mut self.cmdline else { return };

		if let Some(mut comp) = cmdline.comp.take() {
			comp.move_by(step);
			comp.apply(&mut cmdline.edit);
			cmdline.comp = Some(comp);
			return;
		}

		let next = match cmdline.recall {
			Some(at) => at as isize + step,
			None if step < 0 && !history.is_empty() => history.len() as isize - 1,
			None => return,
		};

		if next < 0 || next >= history.len() as isize {
			cmdline.recall = None;
			cmdline.edit = Edit::new(String::new());
			return;
		}

		cmdline.recall = Some(next as usize);
		cmdline.edit = Edit::new(history[next as usize].clone());
	}

	/// The candidates are re-read whenever the line moves under a list that is already up: a
	/// stale list would offer strangers.
	fn cmdline_refresh(&mut self) {
		let cwd = self.tab().path().to_owned();
		let Some(cmdline) = &mut self.cmdline else { return };
		if cmdline.comp.is_none() {
			return;
		}

		let items = complete::paths(&cmdline.edit.text, &cwd, 64);
		cmdline.comp = Completions::new(complete::token_start(&cmdline.edit.text), items);
	}

	// ---- open-with (§4.4) ----

	/// The modal takes whatever the cursor stands on, or the selection when there is one.
	fn openwith_open(&mut self) {
		let paths = self.targets();
		if paths.is_empty() {
			return;
		}

		let name = match paths.as_slice() {
			[one] => one.file_name().map_or_else(|| one.display().to_string(), |name| name.to_string_lossy().into_owned()),
			many => format!("{} 项", many.len()),
		};
		self.openwith = Some(OpenWith { paths, name, edit: Edit::new(String::new()), comp: None, recall: None });
	}

	/// Where the menu stands: the pointer's own place when there is one, the row's corner when a
	/// replay was the one asking.
	fn menu_anchor(&self, fallback: Pos2) -> Pos2 {
		// the pointer's own place when there is one; a replay's stands over it, and a keyboard
		// caller passes its own fallback with `menu_fit` below
		self.menu_fit(self.replay_at.or(self.hover).unwrap_or(fallback))
	}

	/// The clamp every menu's place takes, whoever asked: it stays inside the body.
	fn menu_fit(&self, at: Pos2) -> Pos2 {
		pos2(at.x.clamp(self.body.min.x + 8.0, self.body.max.x - 208.0), at.y.clamp(self.body.min.y + 8.0, self.body.max.y - 160.0))
	}

	/// The menu's target for a visible row: the selection when the row is inside one, else the
	/// row itself — seated first, the way a click seats. Shared by the right click, the replay
	/// and `o` (P36④), so the three doors cannot drift apart.
	fn menu_target(&mut self, position: usize) -> Option<crate::menu::Target> {
		let snapshot = self.entries();
		let dir = self.tab().path().to_owned();
		let view = self.tab().view.clone();
		let entry = view.get(position).and_then(|i| snapshot.entries.get(*i))?;

		let path = dir.join(&entry.name);
		Some(if self.selection.contains(&path) && self.selection.len() > 1 {
			crate::menu::Target::Selection(self.selection.iter().cloned().collect())
		} else {
			self.click_row(position);
			crate::menu::Target::Row(path, entry.dir)
		})
	}

	fn menu_on_row(&mut self, position: usize, at: Pos2) {
		let Some(target) = self.menu_target(position) else { return };
		self.menu_open(target, self.menu_anchor(at));
	}

	/// `o` (P36④): the cursor row's own context menu — the same panel and the same door a right
	/// click opens, for hands without a second button. It stands at the row itself (the keyboard
	/// asked; the pointer has no say), clamped inside the body like every menu.
	fn menu_on_cursor(&mut self) {
		let cursor = self.tab().stack.last().map_or(0, |view| view.cursor);
		let Some(position) = self.tab().view.iter().position(|at| *at == cursor) else { return };
		let Some(target) = self.menu_target(position) else { return };

		// the row's own place in the list: the same arithmetic the draw and the mouse use
		let list = chrome::columns(self.body, self.shares).current;
		let top = list.min.y + (position as f32 - self.tab().scroll.offset) * ROW_HEIGHT;
		self.menu_open(target, self.menu_fit(pos2(list.min.x, top)));
	}

	/// The blank's own menu: the window's directory (P31).
	fn menu_on_blank(&mut self, at: Pos2) {
		self.menu_open(crate::menu::Target::Folder(self.tab().path().to_owned()), self.menu_anchor(at));
	}

	/// Open the row's own menu (P31) on what a right click was aimed at.
	fn menu_open(&mut self, target: crate::menu::Target, anchor: Pos2) {
		let yank = self.yank.is_some();
		crate::metrics::menu(&format!("{} {} 项", target.title(), crate::menu::main_items(&target, yank).len()));
		self.menu = Some(crate::menu::Menu {
			page:      crate::menu::Page::Main,
			target,
			at:        0,
			anchor,
			apps:      Vec::new(),
			all:       Vec::new(),
			apps_at:   0,
			query:     crate::edit::Edit::new(String::new()),
			scroll:    0,
			mime:      String::new(),
			preferred: Vec::new(),
			yank,
		});
	}

	/// The menu's own keys (P31; the applications page is search-first since P35's addendum, the
	/// way the dialog it mirrors is): `j`/`k` walk — the arrows too — Enter takes, Esc backs out
	/// one page and closes the last; on the applications page the letters are the query instead,
	/// the arrows walk, and Esc clears the query before it backs out.
	fn menu_keys(&mut self, events: &[egui::Event], typed: &str, ctx: &Context) {
		let page = self.menu.as_ref().map(|menu| menu.page);
		let Some(page) = page else { return };
		// the panel's own window (P36③): what the arrows and the wheel keep in sight
		let vis = self.menu.as_ref().map_or(0, |menu| menu.visible_rows(self.body.height()));

		// the applications page's query: what the layout typed lands in it (P35), and a query
		// starts a new list at the top (P36③) — the standing row and the window both go home
		if page == crate::menu::Page::Apps
			&& !typed.is_empty()
			&& let Some(menu) = &mut self.menu
		{
			menu.query.type_text(typed);
			menu.apps_at = 0;
			menu.scroll = 0;
			ctx.request_repaint();
		}

		for event in events {
			let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };

			match key {
				Key::Escape => {
					// one page at a time back (P31): the confirm to the list, the list to the
					// items, the items away — and on the applications page a query standing is
					// cleared first (P35's addendum, the index panel's own two-stage habit)
					if page == crate::menu::Page::Apps && self.menu.as_ref().is_some_and(|menu| !menu.query.text.is_empty()) {
						if let Some(menu) = &mut self.menu {
							menu.query = crate::edit::Edit::new(String::new());
							menu.apps_at = 0;
							menu.scroll = 0;
						}
					} else if page == crate::menu::Page::Main {
						self.menu = None;
					} else if let Some(menu) = &mut self.menu {
						// Esc steps back a page: the applications list to the items, and the items
						// away (P37③: there is no page between a choice and its launch any more)
						menu.page = crate::menu::Page::Main;
						menu.at = 0;
					}
				}
				Key::Backspace if page == crate::menu::Page::Apps => {
					if let Some(menu) = &mut self.menu {
						menu.query.key(*key, *modifiers);
						menu.apps_at = 0;
						menu.scroll = 0;
					}
				}
				// the letters are the query's on the applications page: only the arrows walk
				Key::J | Key::ArrowDown if page != crate::menu::Page::Apps => {
					if let Some(menu) = &mut self.menu {
						menu.step(1);
						menu.follow(vis);
					}
				}
				Key::K | Key::ArrowUp if page != crate::menu::Page::Apps => {
					if let Some(menu) = &mut self.menu {
						menu.step(-1);
						menu.follow(vis);
					}
				}
				Key::ArrowDown | Key::ArrowUp => {
					let by = if *key == Key::ArrowDown { 1 } else { -1 };
					if let Some(menu) = &mut self.menu {
						menu.step(by);
						menu.follow(vis);
					}
				}
				// `l` is the menu's own step-into (P37②): on the 打开方式 row it opens the
				// applications list, the same door Enter takes
				Key::L if page == crate::menu::Page::Main => {
					if self.menu.as_ref().is_some_and(|menu| menu.standing_open_with()) {
						self.menu_pick(ctx);
					}
				}
				// `Ctrl+Enter` writes the preference down too (P38②): 总是 is back — the type's default is
				// set (the merge, P31's own) and then the app runs, the way the confirm page used to
				Key::Enter if page == crate::menu::Page::Apps && modifiers.ctrl => self.menu_launch(true),
				Key::Enter => self.menu_pick(ctx),
				Key::Q if !modifiers.ctrl && page != crate::menu::Page::Apps => self.menu = None,
				_ => continue,
			}

			ctx.request_repaint();
		}
	}

	/// Take the standing row of whichever page is up (P31).
	fn menu_pick(&mut self, ctx: &Context) {
		let Some(menu) = self.menu.as_ref() else { return };
		let standing = menu.at;

		match menu.page {
			crate::menu::Page::Main => match crate::menu::main_items(&menu.target, menu.yank).into_iter().nth(standing) {
				Some(crate::menu::Item::Do(crate::menu::Action::Copy, _)) => self.menu_copy(false, ctx),
				Some(crate::menu::Item::Do(crate::menu::Action::Cut, _)) => self.menu_copy(true, ctx),
				Some(crate::menu::Item::Do(crate::menu::Action::Paste, _)) => self.menu_paste(),
				Some(crate::menu::Item::Do(crate::menu::Action::OpenTerminal, _)) => self.menu_terminal(),
				Some(crate::menu::Item::Do(crate::menu::Action::OpenWith, _)) => self.menu_apps(),
				_ => {}
			},
			crate::menu::Page::Apps => self.menu_launch(false),
		}
	}

	/// The menu's own 复制/剪切 (P33⑱): the very function `y`/`x` run. The right-clicked row is the
	/// target — or the whole selection when the click landed inside one, because the dispatch seats
	/// the cursor and `targets()` reads the same two cases every other file operation does.
	fn menu_copy(&mut self, cut: bool, ctx: &Context) {
		self.menu = None;
		self.yank(cut, ctx);
	}

	/// The menu's own 粘贴 (P33⑱): the same door `p` uses, and its destination is the window's own
	/// directory — the parent of whatever row was right-clicked, never inside it. Right-clicking a
	/// folder offers a paste beside it, at its own level, which is the user's own rule.
	fn menu_paste(&mut self) {
		self.menu = None;
		self.paste(false);
	}

	/// `在终端中打开` (P31's chain): the folder's own terminal, its cwd that folder — found off
	/// this thread, and a machine with no terminal at all says so on a toast.
	/// `T` (P35③): the cursor's row in a terminal — the row has to be a folder, and it becomes the
/// terminal's own cwd. Anything else says so rather than guessing at a parent (the user's own
/// ruling: 须是文件夹).
	fn terminal_here(&mut self) {
		let Some(path) = self.selected() else { return };
		if !path.is_dir() {
			self.toasts.push("须是文件夹", toast::Kind::Warn);
			return;
		}

		self.open_terminal(path);
	}

	/// The terminal chain (P31's own, shared with the menu's `在终端中打开`): the folder's own
	/// terminal, found off this thread — `xdg-terminal-exec`, the desktop's answer, `$TERMINAL`,
	/// the four names — and a machine with no terminal at all says so on a toast. The dry switch
	/// (P35③'s harness) records what would have run instead of opening a window.
	/// The terminal chain (P31's own, shared by `T`, the menu's `在终端中打开`, and a
/// `Terminal=true` handler's launch — P35): the machine's terminal, found off this thread.
	fn terminal_chain() -> Option<Vec<String>> {
		let xdg = crate::open::ask("xdg-mime", &[ "query", "default", "x-scheme-handler/terminal" ]);
		let home = std::env::var_os("HOME").map(PathBuf::from);
		let entries = crate::desktop::scan(&crate::desktop::app_dirs(home.as_deref()));
		let path_env = std::env::var("PATH").unwrap_or_default();
		crate::desktop::terminal(&path_env, std::env::var("TERMINAL").ok().as_deref(), xdg.as_deref(), &entries)
	}

	fn open_terminal(&mut self, folder: PathBuf) {
		let note = self.menu_note.0.clone();
		let record = self.terminal_log.0.clone();
		let dry = self.terminal_dry;
		crate::metrics::menu(&format!("terminal {}", folder.display()));

		std::thread::spawn(move || {
			let Some(argv) = Self::terminal_chain() else {
				let _ = note.send("没有找到可用的终端".to_owned());
				return;
			};

			if dry {
				let _ = record.send(TerminalLaunch { argv, folder });
				return;
			}

			let Some((tool, args)) = argv.split_first() else { return };
			let spawned = std::process::Command::new(tool)
				.args(args)
				.current_dir(&folder)
				.stdin(std::process::Stdio::null())
				.stdout(std::process::Stdio::null())
				.stderr(std::process::Stdio::null())
				.spawn();

			if spawned.is_err() {
				let _ = note.send(format!("终端启动失败：{tool}"));
			}
		});
	}

	/// A `Terminal=true` handler's launch (P35's gio alignment): the program's own argv is run
	/// inside the machine's terminal — the same chain as everywhere else — with the target
	/// file's directory as the cwd. A machine with no terminal says so, the way `T` does.
	fn spawn_in_terminal(&mut self, program: Vec<String>, folder: PathBuf) {
		let note = self.menu_note.0.clone();
		let record = self.terminal_log.0.clone();
		let dry = self.terminal_dry;
		crate::metrics::menu(&format!("terminal app {}", program.first().map_or("", String::as_str)));

		std::thread::spawn(move || {
			let Some(chain) = Self::terminal_chain() else {
				let _ = note.send("没有找到可用的终端".to_owned());
				return;
			};
			let argv = crate::desktop::launch_argv(program, Some(&chain));

			if dry {
				let _ = record.send(TerminalLaunch { argv, folder });
				return;
			}

			let Some((tool, args)) = argv.split_first() else { return };
			let spawned = std::process::Command::new(tool)
				.args(args)
				.current_dir(&folder)
				.stdin(std::process::Stdio::null())
				.stdout(std::process::Stdio::null())
				.stderr(std::process::Stdio::null())
				.spawn();

			if spawned.is_err() {
				let _ = note.send(format!("终端启动失败：{tool}"));
			}
		});
	}

	/// The menu's own `在终端中打开` (P31): the same chain as `T`, on the folder the menu was
	/// aimed at.
	fn menu_terminal(&mut self) {
		let Some(menu) = self.menu.as_ref() else { return };
		let Some(folder) = menu.target.folder() else { return };

		self.menu = None;
		self.open_terminal(folder);
	}

	/// `打开方式…` (P31's own panel): what this machine has for the target's type, in the order
	/// the person's own preferences put them.
	fn menu_apps(&mut self) {
		let Some(menu) = self.menu.as_ref() else { return };
		let mime = menu.target.mime();
		let home = std::env::var_os("HOME").map(PathBuf::from);

		let entries = crate::desktop::scan(&crate::desktop::app_dirs(home.as_deref()));
		let text = crate::config::Config::mimeapps().and_then(|at| std::fs::read_to_string(at).ok()).unwrap_or_default();
		let preferred = crate::desktop::preferred_in(&text, &mime);
		// `[Added Associations]` join the candidates too, the way a desktop's own registry lets
		// them (P35's gio alignment)
		let added = crate::desktop::added_in(&text, &mime);
		// the candidates ride on the inheritance closure (P33⑥): a markdown file is answered by
		// the plain-text editors, the way a desktop manager answers it
		let types = crate::mime::closure(&mime);
		let path_env = std::env::var("PATH").unwrap_or_default();
		let handlers: Vec<crate::desktop::Entry> =
			crate::desktop::handlers(&entries, &types, &preferred, &added, &path_env).into_iter().cloned().collect();
		// every application behind the recommendations (P35's addendum): non-NoDisplay, TryExec
		// satisfied, by name — browsers and everything else, so a person picks what they like
		let all = crate::desktop::all_apps(&entries, &path_env);

		crate::metrics::menu(&format!("apps {} {} 个", mime, handlers.len()));
		if let Some(menu) = &mut self.menu {
			menu.page = crate::menu::Page::Apps;
			menu.apps = handlers;
			menu.all = all;
			menu.apps_at = 0;
			menu.query = crate::edit::Edit::new(String::new());
			menu.mime = mime;
			menu.preferred = preferred;
		}
	}

	/// A choice needs an answer: `仅一次` or `总是` (P31).
	/// `<Enter>` on the applications page (P37③, the user's own re-cut): the standing application
	/// runs at once — 仅一次, no page between. `Ctrl+Enter` (P38②) writes the preference down first
	/// — `总是`, back on its own chord — through the same merge a real window has always used;
	/// a test's preferences live in the seam and the person's own file is only ever written when
	/// no seam stands (the real window). A `Terminal=true` entry rides the machine's terminal
	/// chain (P35), and a headless test hears it through the dry record instead of a window.
	fn menu_launch(&mut self, always: bool) {
		let Some(menu) = self.menu.as_ref() else { return };
		let Some(entry) = menu.app_rows().get(menu.apps_at).map(|row| row.entry.clone()) else { return };
		let (mime, path) = (menu.mime.clone(), menu.target.first());
		self.menu = None;

		if always {
			let merged = crate::desktop::merge_defaults(&self.preferences_text(), &mime, &entry.id);
			if let Some(seam) = &mut self.mimeapps_for_tests {
				*seam = merged;
			} else if let Some(at) = self.mimeapps_path()
				&& let Err(e) = crate::desktop::write_atomic(&at, &merged)
			{
				self.toasts.push(format!("偏好写不进去：{e}"), toast::Kind::Err);
				return;
			}
			crate::metrics::menu(&format!("always {} {}", entry.id, mime));
		} else {
			crate::metrics::menu(&format!("once {} {}", entry.id, mime));
		}

		let argv = crate::desktop::exec_argv(&entry.exec, &path, &mime);
		if entry.terminal {
			// `Terminal=true` (P35): the program runs inside the machine's own terminal
			let folder = path.parent().map(Path::to_owned).unwrap_or_else(|| self.tab().path().to_owned());
			self.spawn_in_terminal(argv, folder);
		} else {
			crate::open::spawn_argv(argv, None);
		}
	}

	/// The preferences the window reads its defaults from (P38④): the test seam when a test has
/// handed one in, the person's own ledger (`~/.config/mimeapps.list`) otherwise — read live, by
/// mtime, through `config::mimeapps_read`. The merge-write side of `Ctrl+Enter` lands here too.
	fn preferences_text(&self) -> String {
		self.mimeapps_for_tests.clone().or_else(|| self.mimeapps_path().and_then(|at| crate::config::mimeapps_read(&at))).unwrap_or_default()
	}

	/// The person's level ledger path (P38④): the test's own when it named one, the spec's place
	/// otherwise — the merge-write of `Ctrl+Enter` lands on the same path.
	fn mimeapps_path(&self) -> Option<PathBuf> {
		self.mimeapps_path_for_tests.clone().or_else(crate::config::Config::mimeapps)
	}

	/// The frecency store's own path (P40): the test's own when it named one, the state dir
	/// otherwise.
	fn frecency_path(&self) -> Option<PathBuf> {
		self.frecency_path_for_tests.clone().or_else(|| crate::config::Config::state("frecency.json"))
	}

	/// The store written where `frecency_path` says: the seam's own file for a test, the state
	/// dir's for the window.
	fn frecency_save(&self) {
		if let Some(at) = self.frecency_path() {
			self.frecency.save_at(&at);
		}
	}

	/// The desk's ledger at the system level (P38④): read live like the person's own, and only
	/// ever behind it.
	fn system_preferences_text(&self) -> String {
		let at = self.mimeapps_system_path_for_tests.clone().unwrap_or_else(crate::config::mimeapps_system);

		crate::config::mimeapps_read(&at).unwrap_or_default()
	}

	/// An opening that failed comes back here (P30): the toast says which file has no program set
	/// to open it — warn-coloured, gone on its own, and Esc or a click takes it away sooner.
	fn open_poll(&mut self, ctx: &Context) {
		let mut failed: Vec<PathBuf> = Vec::new();
		while let Ok(path) = self.open_failed.1.try_recv() {
			failed.push(path);
		}

		for path in failed {
			let name = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
			crate::metrics::open_failed(&path);
			self.toasts.push(format!("没有设置打开 {name} 的程序"), toast::Kind::Warn);
			ctx.request_repaint();
		}

		// and what a menu action could not do (P31): a machine with no terminal says so
		while let Ok(note) = self.menu_note.1.try_recv() {
			crate::metrics::menu(&format!("note {note}"));
			self.toasts.push(note, toast::Kind::Warn);
			ctx.request_repaint();
		}
	}

	/// What an operation acts on: the selection when there is one, the cursor's own item
	/// otherwise — the rule every file operation of this window reads from.
	fn targets(&self) -> Vec<PathBuf> {
		if self.selection.is_empty() {
			return self.selected().into_iter().collect();
		}

		let mut paths: Vec<PathBuf> = self.selection.iter().cloned().collect();
		paths.sort();
		paths
	}

	fn openwith_complete(&mut self) {
		let cwd = self.tab().path().to_owned();
		let Some(mut open) = self.openwith.take() else { return };

		if let Some(mut comp) = open.comp.take() {
			comp.move_by(1);
			comp.apply(&mut open.edit);
			open.comp = Some(comp);
		} else {
			let items = open::candidates(&open.edit.text, &self.open_history, &cwd, 8);
			open.comp = Completions::new(complete::token_start(&open.edit.text), items);
			if let Some(comp) = open.comp.take() {
				comp.apply(&mut open.edit);
				open.comp = Some(comp);
			}
		}

		self.openwith = Some(open);
	}

	/// ↑ / ↓ recall the history by recency while no completion is up, and walk the list while one
	/// is — the same rule the command line keeps.
	fn openwith_recall(&mut self, step: isize) {
		let commands: Vec<String> = self.open_history.entries.iter().map(|entry| entry.command.clone()).collect();
		let Some(open) = &mut self.openwith else { return };

		if let Some(mut comp) = open.comp.take() {
			comp.move_by(step);
			comp.apply(&mut open.edit);
			open.comp = Some(comp);
			return;
		}

		let next = match open.recall {
			Some(at) => at as isize + step,
			None if step < 0 && !commands.is_empty() => 0,
			None => return,
		};
		if next < 0 {
			return;
		}

		if next as usize >= commands.len() {
			open.recall = None;
			open.edit = Edit::new(String::new());
			return;
		}

		open.recall = Some(next as usize);
		open.edit = Edit::new(commands[next as usize].clone());
	}

	/// A command that is known goes out with the paths and is forgotten about; one that is not is
	/// answered now, in the field, rather than by nothing at all (§4.4).
	fn openwith_submit(&mut self) {
		let Some(open) = self.openwith.take() else { return };
		let command = open.edit.text.trim().to_owned();
		if command.is_empty() {
			self.openwith = Some(open);
			return;
		}

		if !open::known(&command) {
			self.toasts.push(format!("找不到命令：{command}"), toast::Kind::Err);
			self.openwith = Some(open);
			return;
		}

		self.open_history.record(&command);
		self.open_history.save();
		open::spawn(&command, &open.paths);
		self.toasts.push(format!("已交给 {command}：{}", open.name), toast::Kind::Ok);
	}

	// ---- the bookmarks (§5.1–§5.3) and the fuzzy panel ----

	fn go(&mut self, path: PathBuf) {
		if path.is_dir() {
			self.tab_mut().stack = vec![ View { path, cursor: 0 } ];
			self.tab_mut().scroll.jump(0.0);
			self.arrived();
		} else {
			self.reveal(path);
		}
	}

	fn confirm_yes(&mut self) {
		let Some(confirm) = self.confirm.take() else { return };
		match confirm.action {
			ConfirmDo::Delete(paths) => {
				let words = format!("已永久删除 {} 项", paths.len());
				self.trash_job = Some(TrashJob::Purge(trash::remove(paths), words));
			}
			ConfirmDo::Purge(items) => {
				self.trash_job = Some(TrashJob::Purge(trash::purge(items), "已从回收站永久删除".to_owned()));
			}
			ConfirmDo::Empty => {
				self.trash_job = Some(TrashJob::Empty(trash::empty()));
			}
			ConfirmDo::Paste { cut, force } => {
				if let Some(yank) = self.yank.clone() {
					self.paste_job = Some(paste::run(yank.paths, self.tab().path().to_owned(), cut, force));
				}
			}
			ConfirmDo::RestoreAs(item, to) => {
				let words = format!("已恢复为 {}", frecency::display(&to));
				self.trash_job = Some(TrashJob::Restore(trash::restore_as(&item, to), vec![ item.original_path() ], words));
			}
		}
	}

	/// The undo link was followed: the state goes back to what it was when the toast was born.
	fn run_undo(&mut self, undo: Undo) {
		match undo {
			Undo::BookmarkSave(name) => {
				self.bookmarks.remove(&name);
				self.bookmarks.save();
				self.fuzzy_rebuild();
			}
			Undo::BookmarkDelete { name, path } => {
				self.bookmarks.put(book::Bookmark { name, path });
				self.bookmarks.save();
				self.fuzzy_rebuild();
			}
			Undo::FrecencyDelete { path, score, last } => {
				// P40: the score and the clock come back whole — the row ranks where it ranked
				self.frecency.put(frecency::Entry { path, score, last });
				self.frecency_save();
				self.fuzzy_rebuild();
			}
			Undo::Trash(paths) => {
				let words = "已从回收站恢复".to_owned();
				self.trash_job = Some(TrashJob::Restore(trash::restore(paths.clone()), paths, words));
			}
			// P33⑫'s own table: a copy-paste's undo is the copies going to the bin (the mover
			// runs on its own thread like every other), a cut-paste's is the moved set walking
			// home, a rename's is the rename back, and a link's is the link taken away
			Undo::Paste { landed } => {
				self.undo_job = Some(UndoJob::Discard(trash::delete(landed)));
			}
			Undo::Moved { pairs } => {
				self.undo_job = Some(UndoJob::Back(paste::move_back(pairs)));
			}
			Undo::Rename { from, to } => {
				match std::fs::rename(&to, &from) {
					Ok(()) => {
						if let Some(dir) = from.parent() {
							self.cache.forget(dir);
						}
						if self.selection.remove(&to) {
							self.selection.insert(from.clone());
						}
						self.toasts.push(format!("已撤回重命名：{}", frecency::display(&from)), toast::Kind::Ok);
					}
					Err(e) => self.toasts.push(format!("撤回失败：{e}"), toast::Kind::Err),
				}
			}
			Undo::Link(links) => {
				let mut gone = 0usize;
				for link in &links {
					// a link goes as a link: the thing it points at is never what is removed
					if std::fs::remove_file(link).is_ok() {
						gone += 1;
					}
				}

				if gone > 0 {
					for link in &links {
						if let Some(dir) = link.parent() {
							self.cache.forget(dir);
						}
					}
					self.toasts.push(format!("已撤回链接（{gone} 个）"), toast::Kind::Ok);
				} else {
					self.toasts.push("撤回失败：链接已经不在了", toast::Kind::Err);
				}
			}
		}
	}

	/// A change-type task that can be taken back (P33⑫): it joins the undo stack — ten deep, the
	/// oldest going first — and its own toast carries the same undo as a link. The stack and the
	/// toast hold equal copies of one action; either door takes the other's link away.
	fn note(&mut self, text: String, undo: Undo) {
		self.undo.push_back(undo.clone());
		if self.undo.len() > UNDO_DEPTH {
			self.undo.pop_front();
		}

		self.toasts.push_with_undo(text, toast::Kind::Ok, Some(undo));
	}

	/// `Ctrl+Z` / `u`: the most recent change-type task, taken back. Nothing on the stack is
	/// nothing said (P33⑫).
	fn undo_last(&mut self) {
		let Some(undo) = self.undo.pop_back() else { return };
		self.toasts.take_matching(&undo);
		self.run_undo(undo);
	}

	/// The undo's own answers, read every frame like every other mover's (P33⑫).
	fn undo_poll(&mut self, ctx: &Context) {
		let Some(job) = self.undo_job.take() else { return };

		match job {
			UndoJob::Discard(job) => match job.poll() {
				None => self.undo_job = Some(UndoJob::Discard(job)),
				Some(Ok(())) => {
					self.cache.forget(self.tab().path());
					self.toasts.push("已撤回粘贴（副本送入回收站）", toast::Kind::Ok);
				}
				Some(Err(e)) => self.toasts.push(format!("撤回失败：{e}"), toast::Kind::Err),
			},
			UndoJob::Back(job) => match job.poll() {
				None => self.undo_job = Some(UndoJob::Back(job)),
				Some(Ok(_)) => {
					self.cache.forget(self.tab().path());
					self.toasts.push("已撤回移动", toast::Kind::Ok);
				}
				Some(Err(e)) => self.toasts.push(format!("撤回失败：{e}"), toast::Kind::Err),
			},
		}

		if self.undo_job.is_some() {
			ctx.request_repaint_after(Duration::from_millis(60));
		}
	}

	// ---- the bin (§ d/D and the R b view) ----

	/// `d` goes to the system bin without a question — the spec's own rule, because a trash
	/// delete is undone by its toast. `D` is permanent, and permanent asks first (§4.5).
	/// `y` / `x`: the targets are held for a paste that will copy them — or move them, for a cut.
	/// The paths also go to the system clipboard (P35④): another program can paste them as text
	/// at once — the `text/uri-list` exchange file managers read is the next phase.
	fn yank(&mut self, cut: bool, ctx: &Context) {
		let targets = self.targets();
		if targets.is_empty() {
			return;
		}

		let count = targets.len();
		let text = targets.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join("\n");

		// the file managers' exchange first (P35④ phase two): when it takes the selection it
		// carries the text mimes itself; when it cannot — no compositor, no Wayland — the text
		// road is the one that runs. Exactly one of the two ever takes the selection.
		if !(self.uri_offers && crate::uri_clipboard::offer(&targets, cut)) {
			ctx.copy_text(text);
		}

		self.yank = Some(Yank { paths: targets, cut });
		self.toasts.push(
			if cut { format!("已剪切 {count} 项（Y/X 取消，p 粘贴）") } else { format!("已复制 {count} 项（Y/X 取消，p 粘贴）") },
			toast::Kind::Ok,
		);
	}

	/// `Y` / `X`: the held targets and the selection are let go.
	fn unyank(&mut self) {
		let held = self.yank.take().is_some();
		let selected = !self.selection.is_empty();
		self.selection.clear();

		if held || selected {
			self.toasts.push("已取消复制/剪切状态", toast::Kind::Warn);
		}
	}

	fn remove(&mut self, permanent: bool) {
		let paths = self.targets();
		if paths.is_empty() {
			return;
		}

		if permanent {
			let names: Vec<String> = paths.iter().take(5).map(|path| frecency::display(path)).collect();
			self.confirm = Some(Confirm {
				text:   format!("永久删除 {} 个项目吗？", paths.len()),
				names,
				action: ConfirmDo::Delete(paths),
			});
			return;
		}

		self.trash_job = Some(TrashJob::Delete(trash::delete(paths.clone()), paths));
	}

	/// The bin's answers, read every frame: a delete lands as a toast with its undo, a listing
	/// fills the view, and the rest say how they went.
	fn trash_poll(&mut self, ctx: &Context) {
		let Some(job) = self.trash_job.take() else { return };
		let mut keep = None;
		let mut done = false;

		match job {
			TrashJob::Delete(job, paths) => match job.poll() {
				None => keep = Some(TrashJob::Delete(job, paths)),
				Some(Ok(())) => {
					for path in &paths {
						self.selection.remove(path);
					}
					// the toast and the undo stack hold one action (P33⑫): the originals go back
					self.note(format!("已删除 {} 项 → 回收站", paths.len()), Undo::Trash(paths));
					done = true;
				}
				Some(Err(e)) => {
					self.toasts.push(format!("删除失败：{e}"), toast::Kind::Err);
					done = true;
				}
			},
			TrashJob::Restore(job, paths, words) => match job.poll() {
				None => keep = Some(TrashJob::Restore(job, paths, words)),
				Some(Ok(())) => {
					self.toasts.push(words, toast::Kind::Ok);
					done = true;
				}
				Some(Err(e)) => {
					self.toasts.push(format!("恢复失败：{e}"), toast::Kind::Err);
					done = true;
				}
			},
			TrashJob::List(job) => match job.poll() {
				None => keep = Some(TrashJob::List(job)),
				Some(Ok(items)) => {
					metrics::bin(items.len());
					// the bin is one list for every tab that shows it: whoever is waiting gets it
					for tab in &mut self.tabs {
						if let Some(view) = &mut tab.trash
							&& view.loading
						{
							view.dirs = items.iter().map(trash::is_dir).collect();
							view.items = items.clone();
							view.loading = false;
							view.cursor = view.cursor.min(view.items.len().saturating_sub(1));
						}
					}
					// the view is up: what the preview column shows is the bin's own item. No
					// refresh is asked for here — the list *is* the refresh, and asking again
					// would list the bin in a loop (P19)
					if self.in_bin() {
						self.look_at_trash();
					}
				}
				Some(Err(e)) => {
					// a list that failed says so and leaves the views standing empty: asking again
					// would be a toast every frame, and the view would read forever (P19)
					for tab in &mut self.tabs {
						if let Some(view) = &mut tab.trash {
							view.loading = false;
							view.items.clear();
							view.dirs.clear();
						}
					}
					self.toasts.push(format!("回收站读取失败：{e}"), toast::Kind::Err);
				}
			},
			TrashJob::Purge(job, words) => match job.poll() {
				None => keep = Some(TrashJob::Purge(job, words)),
				Some(Ok(())) => {
					self.toasts.push(words, toast::Kind::Ok);
					done = true;
				}
				Some(Err(e)) => {
					self.toasts.push(format!("永久删除失败：{e}"), toast::Kind::Err);
					done = true;
				}
			},
			TrashJob::Empty(job) => match job.poll() {
				None => keep = Some(TrashJob::Empty(job)),
				Some(Ok(())) => {
					self.toasts.push("已清空回收站", toast::Kind::Ok);
					done = true;
				}
				Some(Err(e)) => {
					self.toasts.push(format!("清空失败：{e}"), toast::Kind::Err);
					done = true;
				}
			},
		}

		self.trash_job = keep;

		if done {
			// whatever happened, the directory behind the window may have changed — and the bin's
			// own list is stale by definition. The refresh asks for the new one *after* the
			// finished job is dropped: a refresh overwritten by the old job's `keep` left the
			// view reading for ever (P19)
			self.cache.forget(self.tab().path());
			self.trash_refresh();
		}

		if self.trash_job.is_some() {
			ctx.request_repaint_after(Duration::from_millis(60));
		}
	}

	/// `g t`: the bin opens as a tab of its own (P27) — no modal, no swallowed keys: the keymap
	/// keeps its say inside it, the chip says 回收站, and closing it is walking away or the chip's
	/// ×. The tab stands on the directory the person left behind, so leaving the bin is a `leave`
	/// or a `cd` away.
	fn trash_view_open(&mut self) {
		// the same ceiling every new tab wears
		if self.tabs.len() >= TAB_LIMIT {
			self.toasts.push(format!("最多 {TAB_LIMIT} 个标签页"), toast::Kind::Warn);
			return;
		}

		let path = self.tab().path().to_owned();
		let at = self.active + 1;
		let mut tab = Tab::at(path);
		tab.trash = Some(TrashView { items: Vec::new(), dirs: Vec::new(), cursor: 0, loading: true, scroll: Scroll::default() });

		self.tabs.insert(at, tab);
		self.active = at;
		self.tabs_scroll = 0.0;
		self.trash_job = Some(TrashJob::List(trash::list()));
		self.preview.clear();
	}

	/// The preview column while the bin is the view: the standing item's own bytes, wherever the
	/// bin keeps them — a first-class view previews like any other (P19).
	fn look_at_trash(&mut self) {
		let Some(item) = self.bin().and_then(|view| view.items.get(view.cursor)).cloned() else {
			self.preview.clear();
			return;
		};

		match trash::in_trash(&item).and_then(|path| entry_of(&path).map(|entry| (path, entry))) {
			Some((path, entry)) => self.preview.look(&path, Some(&entry)),
			None => self.preview.clear(),
		}
	}

	/// A fresh list for a view that is up — after a restore, a purge or an emptying, what the
	/// views show is stale by definition. The bin is one global list, so every bin tab is marked
	/// waiting and one read answers them all (P27).
	fn trash_refresh(&mut self) {
		let mut wanted = false;
		for tab in &mut self.tabs {
			if let Some(view) = &mut tab.trash {
				view.loading = true;
				wanted = true;
			}
		}
		if wanted {
			self.trash_job = Some(TrashJob::List(trash::list()));
		}
	}

	fn trash_restore_standing(&mut self) {
		let Some(item) = self.bin().and_then(|view| view.items.get(view.cursor)).cloned() else {
			return;
		};

		// the way back must never be a silent overwrite: a name the original directory still
		// holds gets a question, and the answer puts the item beside it (P19)
		let original = item.original_path();
		if trash::taken(&original) {
			let free = trash::free_beside(&original);
			self.confirm = Some(Confirm {
				text:   "原位置已有同名，恢复为新的名字吗？".to_owned(),
				names:  vec![ format!("{} → {}", frecency::display(&original), frecency::display(&free)) ],
				action: ConfirmDo::RestoreAs(item, free),
			});
			return;
		}

		let paths = vec![ original ];
		let words = "已从回收站恢复".to_owned();
		self.trash_job = Some(TrashJob::Restore(trash::restore(paths.clone()), paths, words));
	}

	fn trash_purge_standing(&mut self) {
		let Some(item) = self.bin().and_then(|view| view.items.get(view.cursor)).cloned() else {
			return;
		};

		self.confirm = Some(Confirm {
			text:   "从回收站永久删除这一项吗？".to_owned(),
			names:  vec![ frecency::display(&item.original_path()) ],
			action: ConfirmDo::Purge(vec![ item ]),
		});
	}

	fn trash_empty_ask(&mut self) {
		let Some(view) = self.bin() else { return };
		if view.items.is_empty() {
			return;
		}

		self.confirm = Some(Confirm {
			text:   "清空回收站吗？".to_owned(),
			names:  vec![ format!("{} 项", view.items.len()) ],
			action: ConfirmDo::Empty,
		});
	}

	/// The link panel's own keys (P21): the three choices are its rows, `)`/`(`/`*` choose them by
	/// key, Enter takes the standing one, Escape puts the panel away — and the choice opens the
	/// path box the table asks for.
	fn link_pick_keys(&mut self, events: &[egui::Event], _typed: &str, ctx: &Context) {
		let (mut close, mut take, mut moved) = (false, false, 0isize);

		for event in events {
			let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };

			match key {
				Key::Escape => close = true,
				Key::Enter => take = true,
				Key::J | Key::ArrowDown => moved += 1,
				Key::K | Key::ArrowUp => moved -= 1,
				Key::Num0 if modifiers.shift => self.link_pick = Some(0),  // `)`
				Key::Num9 if modifiers.shift => self.link_pick = Some(1),  // `(`
				Key::Num8 if modifiers.shift => self.link_pick = Some(2),  // `*`
				_ => {}
			}
		}

		if close {
			self.link_pick = None;
		}
		if moved != 0 {
			self.link_pick = Some((self.link_pick.unwrap_or(0) as isize + moved).clamp(0, 2) as usize);
		}
		if take {
			self.link_choose();
		}
		ctx.request_repaint();
	}

	/// The choice's own step (P21): the path box opens on the cursor's own path, to be edited into
	/// whatever the link is to point at.
	fn link_choose(&mut self) {
		let Some(at) = self.link_pick.take() else { return };
		let kind = [ LinkKind::Absolute, LinkKind::Relative, LinkKind::Hard ][at.min(2)];
		let target = self.selected().map_or_else(String::new, |path| path.display().to_string());

		self.prompt = Some(Prompt {
			title: match kind {
				LinkKind::Absolute => "符号链接（绝对路径）—— 输入它指向的路径".to_owned(),
				LinkKind::Relative => "符号链接（相对路径）—— 输入它指向的路径".to_owned(),
				LinkKind::Hard => "硬链接 —— 输入它指向的路径".to_owned(),
			},
			edit: Edit::new(target),
			action: PromptDo::Link(kind),
		});
	}

	/// The find box's keyboard (P21): typing is the pattern (the list colours as it goes), Enter
	/// keeps it for `n`/`N`, and Esc clears both.
	fn find_keys(&mut self, events: &[egui::Event], typed: &str, ctx: &Context) {
		let mut close = false;
		let mut keep = None;

		for event in events {
			let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };

			match key {
				Key::Escape => {
					close = true;
					keep = Some(None);
				}
				Key::Enter => {
					close = true;
					keep = Some(self.find.as_ref().map(|edit| edit.text.clone()).filter(|text| !text.is_empty()));
				}
				_ => {
					if let Some(edit) = &mut self.find {
						edit.key(*key, *modifiers);
					}
				}
			}
		}

		if let Some(edit) = &mut self.find
			&& !typed.is_empty()
		{
			edit.type_text(typed);
		}

		if close {
			self.find = None;
			self.find_last = keep.flatten();
		}
		ctx.request_repaint();
	}

	/// The find box's strip (P21): the same line the filter uses, with the search's own words.
	fn draw_find(&mut self, ui: &mut Ui, area: Rect, advance: f32, font: &FontId, small: &FontId) {
		let Some(edit) = &self.find else { return };

		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let rect = Rect::from_min_size(pos2(area.min.x, area.max.y - STATUS_HEIGHT - 32.0), vec2(area.width(), 32.0));
		painter.rect_filled(rect, 0.0, tokens.crust);
		painter.line_segment([ rect.left_top(), rect.right_top() ], (1.0, tokens.border_strong));

		let prefix = "查找 ";
		let pw = prefix.chars().map(|c| crate::list::cell_width(c) as f32).sum::<f32>() * advance;
		painter.text(rect.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, prefix, font.clone(), tokens.accent);

		let shown = crate::list::clipped(&edit.text, ((rect.width() - pw - 32.0) / advance).max(1.0) as u32);
		let x = rect.min.x + 12.0 + pw;
		painter.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, shown, font.clone(), tokens.text);

		// the caret, one cell of a hint after the text — and the anchor the IME reads (P33⑤), which
		// must not blink with it
		let caret = x + cells(shown, advance);
		let caret_rect = Rect::from_min_size(pos2(caret, rect.center().y - 8.0), vec2(2.0, 16.0));
		if ui.ctx().input(|i| i.time).fract() < 0.5 {
			painter.rect_filled(caret_rect, 0.0, tokens.accent);
		}
		anchor_ime(ui, rect, caret_rect);

		painter.text(
			rect.right_center() + vec2(-12.0, 0.0),
			Align2::RIGHT_CENTER,
			"Enter 保留 · Esc 清除 · n/N 跳转",
			small.clone(),
			tokens.disabled,
		);
	}

	/// The help panel's keyboard (P21's `?`): `j`/`k` and the arrows walk the list, the wheel it,
	/// `f` (or a click) puts the keyboard in the box where what is typed filters the rows, and Enter
	/// runs the standing row — once the box has been left. Escape leaves the box first, then closes.
	fn help_keys(&mut self, events: &[egui::Event], typed: &str, ctx: &Context) {
		let rows = self.keys.rows();
		let (mut close, mut run, mut focus, mut moved) = (false, false, false, 0isize);

		for event in events {
			let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };

			match key {
				// Escape steps out of the box first and closes only from the list — the machine the
				// index panel shares (P33⑬)
				Key::Escape => {
					if !self.help_focus.escape() {
						close = true;
					}
				}
				Key::Enter if self.help_focus.inside => {
					self.help_focus.escape();
				}
				Key::Enter => run = true,
				Key::F if !self.help_focus.inside => focus = true,
				Key::J | Key::ArrowDown if !self.help_focus.inside => moved += 1,
				Key::K | Key::ArrowUp if !self.help_focus.inside => moved -= 1,
				_ => {
					if self.help_focus.inside {
						self.help_query.key(*key, *modifiers);
					}
				}
			}
		}

		if self.help_focus.inside && !typed.is_empty() {
			self.help_query.type_text(typed);
		}
		if focus {
			self.help_focus.enter();
		}

		if close {
			self.help = false;
			self.help_query = Edit::new(String::new());
		} else if run {
			let visible = help_rows(rows, &self.help_query.text);
			if let Some(&at) = visible.get(self.help_at) {
				let actions = self.keys.run_at(at);
				self.help = false;
				self.help_query = Edit::new(String::new());
				for action in actions.unwrap_or_default() {
					self.act(action, ctx);
				}
			}
		} else if moved != 0 {
			let visible = help_rows(rows, &self.help_query.text);
			let len = visible.len();
			if len > 0 {
				self.help_at = (self.help_at as isize + moved).clamp(0, len as isize - 1) as usize;
			}
		} else {
			// typing narrows the list: the standing row may have walked past its end
			let visible = help_rows(rows, &self.help_query.text);
			self.help_at = self.help_at.min(visible.len().saturating_sub(1));
		}

		ctx.request_repaint();
	}

	/// The bin's own list is walked by the list's own keys (P27): the middle column's rows are the
	/// bin's, so `act` sends the motion keys here while the tab is the bin's own view. The seat is
	/// the one place the bin's cursor moves — clamp, follow, preview, and the number replay reads.
	fn bin_seat(&mut self, cursor: usize) {
		let Some(view) = self.bin_mut() else { return };
		let len = view.items.len();
		if len == 0 {
			return;
		}

		view.cursor = cursor.min(len - 1);
		let cursor = view.cursor;
		view.scroll.follow(cursor);
		metrics::cursor(cursor, len);
		// the preview follows the standing item, the way it follows the cursor anywhere else
		self.look_at_trash();
		self.bars_moved[0] = Some(Instant::now());
	}

	fn bin_move(&mut self, step: isize) {
		let at = self.bin().map_or(0, |view| view.cursor);
		self.bin_seat((at as isize + step).max(0) as usize);
	}

	/// `<C-u>`/`<C-d>`/…: the same page arithmetic the list's own uses, over the bin's rows.
	fn bin_page(&mut self, full: bool, back: bool) {
		let Some(view) = self.bin() else { return };
		let rows = view.scroll.rows.max(1);
		let step = if full { rows.saturating_sub(1).max(1) } else { (rows / 2).max(1) };
		let at = view.cursor as isize;
		let next = if back { at - step as isize } else { at + step as isize };

		self.bin_seat(next.max(0) as usize);
	}

	// ---- compressing (§ the keymap's c z) ----

	/// `c z`: the archive's name is asked for, pre-filled the way every archiver does it — the
	/// directory's own name with a zip's extension.
	fn compress_open(&mut self) {
		let paths = self.targets();
		if paths.is_empty() {
			return;
		}

		let default = pack::archive_name(&self.tab().name(), "");
		self.prompt = Some(Prompt {
			title:  "压缩为（.zip / .tar / .tar.zst）".to_owned(),
			edit:   Edit::new(default),
			action: PromptDo::Compress(paths),
		});
	}

	/// Enter in the prompt: an archive that is not already there is what it takes — overwriting one
	/// is not a thing a single-line prompt should be able to do by accident.
	fn prompt_submit(&mut self) {
		let Some(prompt) = self.prompt.take() else { return };

		match prompt.action.clone() {
			PromptDo::Create => self.create_submit(&prompt.edit.text),
			PromptDo::Link(kind) => {
				let typed = prompt.edit.text.trim().to_owned();
				match expand_tilde(&typed, std::env::var_os("HOME").map(PathBuf::from)) {
					Some(target) if target.symlink_metadata().is_ok() => self.link_to(kind, target),
					_ => self.toasts.push(format!("路径不存在：{typed}"), toast::Kind::Err),
				}
			}
			PromptDo::Compress(paths) => {
				let cwd = self.tab().path().to_owned();
				let name = pack::archive_name(&self.tab().name(), &prompt.edit.text);
				let archive = cwd.join(&name);

				if archive.symlink_metadata().is_ok() {
					self.toasts.push(format!("已存在同名压缩包：{name}"), toast::Kind::Err);
					self.prompt = Some(prompt);
					return;
				}

				let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
				self.pack = Some(pack::compress(paths, archive, cwd, cancel));
			}
		}
	}

	/// The prompt's keyboard: Esc puts it away, Enter answers it, everything else edits.
	fn prompt_keys(&mut self, events: &[egui::Event], typed: &str, ctx: &Context) {
		let (mut close, mut submit) = (false, false);

		for event in events {
			let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };
			match key {
				Key::Escape => close = true,
				Key::Enter => submit = true,
				_ => {
					if let Some(prompt) = &mut self.prompt {
						prompt.edit.key(*key, *modifiers);
					}
				}
			}
		}

		if !typed.is_empty()
			&& let Some(prompt) = &mut self.prompt
		{
			prompt.edit.type_text(typed);
		}

		if close {
			self.prompt = None;
		} else if submit {
			self.prompt_submit();
		}
		ctx.request_repaint();
	}

	/// The pack's answers, read every frame: when it lands, a toast says how many files went in —
	/// and the progress group has been saying how far it was all along.
	fn pack_poll(&mut self, ctx: &Context) {
		let Some(job) = &self.pack else { return };
		match job.poll() {
			None => ctx.request_repaint_after(Duration::from_millis(80)),
			Some(Ok(written)) => {
				let name = job.name.clone();
				self.pack = None;
				self.cache.forget(self.tab().path());
				self.toasts.push(format!("已压缩 {written} 个文件 → {name}"), toast::Kind::Ok);
			}
			Some(Err(e)) => {
				self.pack = None;
				self.toasts.push(format!("压缩失败：{e}"), toast::Kind::Err);
			}
		}
	}

	// ---- the fuzzy panel (§5.3) ----

	/// The index panel's standing row, if it is an entry one of the zones owns: what `Del` acts on.
	fn index_entry(&self) -> Option<IndexEntry> {
		let fuzzy = self.fuzzy.as_ref().filter(|fuzzy| fuzzy.mode == FuzzyMode::Index)?;
		let item = fuzzy.hits.get(fuzzy.at).and_then(|hit| fuzzy.items.get(hit.at))?;

		match &item.payload {
			FuzzyPayload::Index(name) => Some(IndexEntry::Bookmark(name.clone())),
			// the index mode's only other payload is the frecency path of the 高频/最近 zones
			FuzzyPayload::Path(path) => Some(IndexEntry::Frecency(path.clone(), item.zone)),
			_ => None,
		}
	}

	/// `Del` in the index panel (P24; P40 for the other two zones): the standing entry goes — a
	/// bookmark from its store, or a 高频/最近 row from the frecency's — and the toast offers it
	/// back. A bookmark's path that is not there can only be deleted this way; a frecency row
	/// carries its score and clock into the undo, so a restore ranks where it ranked.
	fn index_delete(&mut self) {
		match self.index_entry() {
			Some(IndexEntry::Bookmark(name)) => {
				let Some(removed) = self.bookmarks.remove(&name) else { return };
				self.bookmarks.save();
				self.note(
					format!("已删除索引 {}（{}）", removed.name, frecency::display(&removed.path)),
					Undo::BookmarkDelete { name: removed.name, path: removed.path },
				);
			}
			Some(IndexEntry::Frecency(path, zone)) => {
				let Some(removed) = self.frecency.remove(&path) else { return };
				self.frecency_save();
				self.note(
					format!("已从{zone}删除 {}（分数 {:.2}）", frecency::display(&removed.path), removed.score),
					Undo::FrecencyDelete { path: removed.path, score: removed.score, last: removed.last },
				);
			}
			None => return,
		}

		self.fuzzy_rebuild();
	}

	/// `Ctrl+S` in the index panel: the directory the window stands in becomes an index — the only
	/// way to add one (P24). A path already typed in the box outranks it, a file included: what the
	/// person typed is what the person means, not its parent. A toast says what was added, and
	/// offers the undo; a path that is not there is refused the same way Enter refuses it.
	fn index_save(&mut self) {
		let (typed, cwd) = {
			let cwd = self.tab().path().to_owned();
			match &self.fuzzy {
				Some(fuzzy) if fuzzy.mode == FuzzyMode::Index => (fuzzy.query.text.clone(), cwd),
				_ => (String::new(), cwd),
			}
		};
		let path = match query_enter(&typed, &cwd, std::env::var_os("HOME").map(PathBuf::from)) {
			QueryEnter::Go(path) => path,
			QueryEnter::Missing(path) => {
				let words = format!("路径不存在：{}", frecency::display(path.as_path()));
				if let Some(fuzzy) = self.fuzzy.as_mut() {
					fuzzy.notice = Some((words, Instant::now()));
				}
				return;
			}
			QueryEnter::Selection => cwd,
		};
		let entry = self.bookmarks.add(path);

		self.bookmarks.save();
		self.toasts.push_with_undo(
			format!("已索引 {}（{}）", entry.name, frecency::display(&entry.path)),
			toast::Kind::Ok,
			Some(Undo::BookmarkSave(entry.name.clone())),
		);
		self.fuzzy_rebuild();
	}

	/// `z` / `Z` / `] f`: the index panel — bookmarks first, then the high scores, then the recent
	/// visits, all of it one fuzzy line (P14's native replacement for whoosh's own panels; the
	/// `[`/`]` family stays bound as an alias, so the muscle memory keeps working).
	fn fuzzy_index(&mut self) {
		let now = now_secs();
		let items: Vec<FuzzyItem> = index_rows(
			&self.bookmarks.entries,
			&self.frecency.ranked(now, fuzzy::CAP),
			&self.frecency.recent(fuzzy::CAP),
		)
		.into_iter()
		.map(|row| FuzzyItem { text: row.text, right: row.right, payload: row.payload, zone: row.zone, broken: row.broken })
		.collect();

		self.fuzzy_open(FuzzyMode::Index, items);
	}

	/// The device family (P17): a worker reads the bus, and the answer decides what happens next —
	/// the picker panel for `M m` / `M u`, a remount or a jump for `M R` / `G m`, and the old cwd
	/// for `` `` ``.
	fn devices(&mut self, do_: DeviceDo) {
		match do_ {
			DeviceDo::PrevCwd => {
				let before = self.tab().path().to_owned();
				self.travel(true);
				if self.tab().path() == before {
					self.toasts.push("没有上一个目录", toast::Kind::Warn);
				}
			}
			DeviceDo::Uri(what) => {
				self.toasts.push(format!("GVfs 远程 URI 的{what}暂不支持（handoff 留记）"), toast::Kind::Warn);
			}
			do_ => {
				let (tx, rx) = std::sync::mpsc::channel();
				let cwd = self.tab().path().to_owned();
				std::thread::spawn(move || {
					let answer = match do_ {
						// the listing serves the picker, and the remount/jump need it to find
						// the device the cwd stands on
						DeviceDo::MountPick | DeviceDo::UnmountPick { .. } => Answer::Devices(mount::list()),
						DeviceDo::Remount => Answer::At(remount(&cwd)),
						DeviceDo::Jump => Answer::At(jump_to(&cwd)),
						_ => Answer::Done(Ok(())),
					};
					let _ = tx.send(answer);
				});

				self.device_job = Some(DeviceJob { do_, rx });
			}
		}
	}

	/// Whatever the worker left: the picker opens, a jump goes, a toast says how it went.
	fn device_poll(&mut self, ctx: &Context) {
		let Some(job) = &self.device_job else { return };
		let devices = match job.rx.try_recv() {
			Ok(answer) => answer,
			Err(std::sync::mpsc::TryRecvError::Empty) => {
				ctx.request_repaint_after(std::time::Duration::from_millis(80));
				return;
			}
			Err(std::sync::mpsc::TryRecvError::Disconnected) => {
				self.device_job = None;
				return;
			}
		};

		let do_ = job.do_;
		self.device_job = None;

		match (devices, do_) {
			(Answer::Devices(Ok(list)), do_) if list.is_empty() => {
				self.toasts.push("没有可挂载设备", toast::Kind::Warn);
				let _ = do_;
			}
			(Answer::Devices(Ok(list)), DeviceDo::MountPick) => self.device_panel(list, false, false),
			(Answer::Devices(Ok(list)), DeviceDo::UnmountPick { force }) => self.device_panel(list, true, force),
			(Answer::Devices(Err(error)), _) => self.toasts.push(format!("设备列表读不到：{error}"), toast::Kind::Err),
			(Answer::At(Ok(at)), DeviceDo::MountHere) => {
				self.toasts.push(format!("已挂载到 {}", at.display()), toast::Kind::Ok);
			}
			(Answer::At(Ok(at)), _) => {
				self.toasts.push(format!("已挂载到 {}", at.display()), toast::Kind::Ok);
				self.go(at);
			}
			(Answer::At(Err(error)) | Answer::Done(Err(error)), _) => {
				self.toasts.push(format!("设备操作失败：{error}"), toast::Kind::Err);
			}
			(Answer::Done(Ok(())), _) => self.toasts.push("设备已卸载", toast::Kind::Ok),
			(Answer::Devices(Ok(_)), _) => {}
		}

		ctx.request_repaint();
	}

	/// The picker panel: one row a device, its badge on the right, Enter doing what the intent
	/// asked for.
	fn device_panel(&mut self, devices: Vec<mount::Device>, unmount: bool, force: bool) {
		let items: Vec<FuzzyItem> = devices
			.iter()
			.map(|device| {
				let (text, right) = device.row();
				FuzzyItem {
					text,
					right: Some(right),
					payload: FuzzyPayload::Device(device.id.clone()),
					zone: "",
					broken: false,
				}
			})
			.collect();

		self.fuzzy_open(FuzzyMode::Devices { unmount, force }, items);
	}

	/// The index panel's own rows, again — after a delete or a save changed them.
	fn index_items(&self) -> Vec<FuzzyItem> {
		let now = now_secs();

		index_rows(&self.bookmarks.entries, &self.frecency.ranked(now, fuzzy::CAP), &self.frecency.recent(fuzzy::CAP))
			.into_iter()
			.map(|row| FuzzyItem { text: row.text, right: row.right, payload: row.payload, zone: row.zone, broken: row.broken })
			.collect()
	}

		fn fuzzy_open(&mut self, mode: FuzzyMode, items: Vec<FuzzyItem>) {
		let fresh = Fuzzy {
			mode,
			query: Edit::new(String::new()),
			items: Arc::new(items),
			hits: Vec::new(),
			at: 0,
			focus: if mode == FuzzyMode::Index { PanelFocus::default() } else { PanelFocus { inside: true } },
			matcher: fuzzy::matcher(),
			job: None,
			epoch: 0,
			finder: None,
			since: Instant::now(),
			loading: false,
			filled: false,
			notice: None,
		};

		self.fuzzy = Some(fresh);
		self.fuzzy_refilter();
	}

	/// `s` / `S`: the panel opens on an empty query and waits for one — walking a whole tree off a
	/// blank line is nobody's idea of a search. The debounce below is what starts the walk.
	fn fuzzy_search(&mut self, content: bool) {
		let mode = if content { FuzzyMode::FindText } else { FuzzyMode::FindNames };
		self.fuzzy_open(mode, Vec::new());
	}

	/// The panel goes away: a walk still running is told to stop with it — the spec's `<C-s>`
	/// cancel, reached by Esc as well.
	fn fuzzy_close(&mut self) {
		if let Some(fuzzy) = self.fuzzy.take()
			&& let Some(job) = fuzzy.finder
		{
			job.cancel();
		}
	}

	/// The query moved: a walk in flight is stopped and its rows are let go — the old query's
	/// results must not stand under the new one. The next walk waits its 150ms in `fuzzy_tick`.
	fn fuzzy_requery(&mut self) {
		let Some(find) = self
			.fuzzy
			.as_ref()
			.map(|fuzzy| matches!(fuzzy.mode, FuzzyMode::FindNames | FuzzyMode::FindText))
		else {
			return;
		};
		if !find {
			self.fuzzy_refilter();
			return;
		}

		let Some(fuzzy) = &mut self.fuzzy else { return };
		if let Some(job) = fuzzy.finder.take() {
			job.cancel();
		}
		fuzzy.items = Arc::new(Vec::new());
		fuzzy.hits.clear();
		fuzzy.at = 0;
		fuzzy.loading = false;
		fuzzy.filled = false;
		fuzzy.since = Instant::now();
	}

	/// The finder's own clock, asked every frame: what a walk has sent is merged in as it lands,
	/// a query that has settled starts its walk, and a large list's filter is taken while its
	/// epoch still stands.
	fn fuzzy_tick(&mut self, ctx: &Context) {
		self.fuzzy_poll();

		// 1. what the walk has sent
		let mut merged = false;
		if let Some(fuzzy) = &mut self.fuzzy
			&& let Some(job) = fuzzy.finder.take()
		{
			let mut closed = false;
			loop {
				match job.rx.try_recv() {
					Ok(batch) => {
						Arc::make_mut(&mut fuzzy.items).extend(batch.iter().map(|hit| {
							FuzzyItem::plain(hit_text(hit), None, FuzzyPayload::Path(hit.path.clone()))
						}));
						merged = true;
					}
					Err(std::sync::mpsc::TryRecvError::Empty) => break,
					Err(std::sync::mpsc::TryRecvError::Disconnected) => {
						closed = true;
						break;
					}
				}
			}

			if closed {
				fuzzy.loading = false;
				fuzzy.filled = true;
			} else {
				fuzzy.finder = Some(job);
				ctx.request_repaint_after(Duration::from_millis(80));
			}
		}
		if merged {
			self.fuzzy_refilter();
		}

		// 2. a query that has settled starts its walk
		let root = self.tab().path().to_owned();
		let Some(mode) = self.fuzzy.as_ref().map(|fuzzy| fuzzy.mode) else { return };
		let Some(fuzzy) = &mut self.fuzzy else { return };

		let walking = matches!(mode, FuzzyMode::FindNames | FuzzyMode::FindText);
		let query = fuzzy.query.text.trim().to_owned();
		if !walking || fuzzy.finder.is_some() || fuzzy.filled || query.is_empty() {
			return;
		}

		let waited = fuzzy.since.elapsed();
		if waited < Duration::from_millis(150) {
			ctx.request_repaint_after(Duration::from_millis(150) - waited);
			return;
		}

		fuzzy.finder = Some(match mode {
			FuzzyMode::FindText => search::contents(&root, &query, search::CAP),
			_ => search::names(&root, &query, search::CAP),
		});
		fuzzy.loading = true;
		ctx.request_repaint();
	}

	/// The query moved: the hits are recomputed — on this thread up to the spec's two thousand,
	/// and on a thread of their own beyond that, so a large list never holds the frame.
	fn fuzzy_refilter(&mut self) {
		let Some(fuzzy) = &mut self.fuzzy else { return };

		if fuzzy.items.len() <= 2000 {
			let texts: Vec<&str> = fuzzy.items.iter().map(|item| item.text.as_str()).collect();
			fuzzy.hits = fuzzy::filter(&fuzzy.query.text, &texts, &mut fuzzy.matcher);
			fuzzy.at = fuzzy.at.min(fuzzy.hits.len().saturating_sub(1));
			return;
		}

		fuzzy.epoch += 1;
		let epoch = fuzzy.epoch;
		let (query, items) = (fuzzy.query.text.clone(), fuzzy.items.clone());
		let (tx, rx) = std::sync::mpsc::channel();

		std::thread::spawn(move || {
			let texts: Vec<&str> = items.iter().map(|item| item.text.as_str()).collect();
			let hits = fuzzy::filter(&query, &texts, &mut fuzzy::matcher());
			tx.send(hits).ok();
		});
		fuzzy.job = Some(FilterJob { epoch, rx });
	}

	/// A filter that came back is taken only while its generation is the current one.
	fn fuzzy_poll(&mut self) {
		let Some(fuzzy) = &mut self.fuzzy else { return };
		let Some(job) = fuzzy.job.take() else { return };

		match job.rx.try_recv() {
			Ok(hits) if job.epoch == fuzzy.epoch => {
				fuzzy.hits = hits;
				fuzzy.at = fuzzy.at.min(fuzzy.hits.len().saturating_sub(1));
			}
			Ok(_) => {}
			Err(std::sync::mpsc::TryRecvError::Empty) => fuzzy.job = Some(job),
			Err(std::sync::mpsc::TryRecvError::Disconnected) => {}
		}
	}

	/// The store moved under a panel that is up: the list is rebuilt, and the query stays.
	fn fuzzy_rebuild(&mut self) {
		let Some(mode) = self.fuzzy.as_ref().map(|fuzzy| fuzzy.mode) else { return };
		let items = match mode {
			FuzzyMode::Index => self.index_items(),
			_ => return,
		};

		if let Some(fuzzy) = &mut self.fuzzy {
			fuzzy.items = Arc::new(items);
		}
		self.fuzzy_refilter();
	}

	fn fuzzy_move(&mut self, step: isize) {
		let Some(fuzzy) = &mut self.fuzzy else { return };
		let len = fuzzy.hits.len();
		if len == 0 {
			return;
		}

		fuzzy.at = (fuzzy.at as isize + step).rem_euclid(len as isize) as usize;
	}

	/// Enter: jump, remove or rename — the panel's own question for the standing row.
	fn fuzzy_enter(&mut self) {
		// ①/② first (P22): what was typed may be a path of its own, and a path outranks the row
		if self.fuzzy.as_ref().is_some_and(|fuzzy| fuzzy.mode == FuzzyMode::Index) {
			let (typed, cwd) = {
				let fuzzy = self.fuzzy.as_ref().expect("just checked");
				(fuzzy.query.text.clone(), self.tab().path().to_owned())
			};

			match query_enter(&typed, &cwd, std::env::var_os("HOME").map(PathBuf::from)) {
				QueryEnter::Go(path) => {
					self.fuzzy_close();
					self.go(path);
					return;
				}
				QueryEnter::Missing(path) => {
					let words = format!("路径不存在：{}", frecency::display(path.as_path()));
					let fuzzy = self.fuzzy.as_mut().expect("up");
					fuzzy.notice = Some((words, Instant::now()));
					return;
				}
				QueryEnter::Selection => {}
			}
		}

		let Some(fuzzy) = &self.fuzzy else { return };
		let Some(payload) = fuzzy.hits.get(fuzzy.at).and_then(|hit| fuzzy.items.get(hit.at)).map(|item| item.payload.clone())
		else {
			return;
		};

		match (fuzzy.mode, payload) {
			(FuzzyMode::Index | FuzzyMode::FindNames | FuzzyMode::FindText, FuzzyPayload::Path(path)) => {
				self.fuzzy_close();
				self.go(path);
			}
			(FuzzyMode::Devices { unmount, force }, FuzzyPayload::Device(id)) => {
				self.device_on(id, unmount, force, false);
			}
			(FuzzyMode::Index, FuzzyPayload::Index(name)) => {
				let Some(entry) = self.bookmarks.get(&name).cloned() else {
					self.toasts.push(format!("索引 {name} 不在了"), toast::Kind::Warn);
					return;
				};
				self.fuzzy_close();
				self.go(entry.path);
			}
			_ => {}
		}
	}

	// ---- input ----

	/// The `which` hint's own clock (P18): a chord's family nobody carries on goes away, and the
	/// sequence with it — the next key is then its own again, the way yazi's key-sequence timeout
	/// has always read. A key inside the chord restarts the clock.
	fn which_expire(&mut self, ctx: &Context) {
		let Some(at) = self.which_at else { return };
		if self.sequence.is_none() {
			self.which_at = None;
			return;
		}

		if at.elapsed() >= WHICH_TIMEOUT {
			self.sequence = None;
			self.which_at = None;
			ctx.request_repaint();
			return;
		}

		ctx.request_repaint_after(WHICH_TIMEOUT - at.elapsed());
	}

	fn keys(&mut self, ctx: &Context) {
		// the rename field has the keyboard: only Esc is ours, to put the title back
		if self.rename.is_some() {
			let escaped =
				ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { key: Key::Escape, pressed: true, .. })));
			if escaped {
				self.rename = None;
				self.rename_at = None;
				self.rename_focus = false;
				ctx.request_repaint();
			}
			return;
		}

		let events = ctx.input(|i| i.events.clone());
		// what the layout typed arrives as its own events: one character per press, and nothing
		// here when a modifier is held
		let typed = typed_text(&events);

		self.handle(ctx, &events, &typed);
	}

	/// One batch of input events, whoever sent them: the window's own keyboard or a replay's
	/// synthesized steps. The text modes own the keyboard in their own order — the bulk editor,
	/// then the in-place rename — and only what none of them takes goes to the keymap.
	fn handle(&mut self, ctx: &Context, events: &[egui::Event], typed: &str) {
		// Ctrl+C / Ctrl+X / Ctrl+A inside a field: the field's own text is copied, cut, or the
		// whole of it selected — one place, whatever holds the keyboard (P18, P21's global four).
		// The real window's Ctrl+C / Ctrl+X never arrive as key events: egui-winit turns the
		// clipboard chords into `Event::Copy` / `Event::Cut` before any key is made (P35②), so
		// both spellings land in the same place. When no field holds the keyboard the list's own
		// meanings stand — those are a few lines down, and they hear the two events the same way.
		for event in events {
			let (copy, cut, select) = match event {
				egui::Event::Key { key, pressed: true, modifiers, .. } if modifiers.ctrl => match key {
					Key::C => (true, false, false),
					Key::X => (false, true, false),
					Key::A => (false, false, true),
					_ => continue,
				},
				egui::Event::Copy => (true, false, false),
				egui::Event::Cut => (false, true, false),
				_ => continue,
			};

			if let Some(edit) = self.active_edit_mut() {
				if (copy || cut) && !edit.text.is_empty() {
					ctx.copy_text(edit.text.clone());
				}
				if select {
					edit.select_all();
				}
				if cut {
					edit.text.clear();
					edit.cursor = 0;
					edit.all = false;
				}
			}
		}

		if self.bulk.is_some() {
			self.bulk_keys(events, typed, ctx);
			return;
		}
		if self.item_rename.is_some() {
			self.rename_keys(events, typed, ctx);
			return;
		}
		if self.cmdline.is_some() {
			self.cmdline_keys(events, typed, ctx);
			return;
		}
		if self.openwith.is_some() {
			self.openwith_keys(events, typed, ctx);
			return;
		}
		if self.confirm.is_some() {
			self.confirm_keys(events, typed, ctx);
			return;
		}
		if self.menu.is_some() {
			self.menu_keys(events, typed, ctx);
			return;
		}
		if self.fuzzy.is_some() {
			self.fuzzy_keys(events, typed, ctx);
			return;
		}
		if self.prompt.is_some() {
			self.prompt_keys(events, typed, ctx);
			return;
		}
		if self.filter.is_some() {
			self.filter_keys(events, typed, ctx);
			return;
		}
		if self.find.is_some() {
			self.find_keys(events, typed, ctx);
			return;
		}
		if self.link_pick.is_some() {
			self.link_pick_keys(events, typed, ctx);
			return;
		}
		if self.help {
			self.help_keys(events, typed, ctx);
			return;
		}
		if self.spot {
			self.spot_keys(events, ctx);
			return;
		}

		// the real window's clipboard chords (P35②): egui-winit answers Ctrl+C / Ctrl+X / Ctrl+V
		// with `Event::Copy` / `Event::Cut` / `Event::Paste` and never makes a key event of them —
		// so the list hears the three verbs here, the same ones the Ctrl keys name (`y`, `x`, `p`
		// under another spelling). The clipboard text a `Paste` may carry belongs to a field, and
		// none holds the keyboard on this path.
		for event in events {
			match event {
				egui::Event::Copy => self.act(Action::Yank { cut: false }, ctx),
				egui::Event::Cut => self.act(Action::Yank { cut: true }, ctx),
				egui::Event::Paste(_) => self.act(Action::Paste { force: false }, ctx),
				_ => {}
			}
		}

		for event in events {
			let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };

			if *key == Key::Escape {
				self.help = false;
				self.sequence = None;
				self.which_at = None;
				// and a toast is whatever else Esc has left to take away (P30): the note it raises,
				// among them, goes on the same key — and a selection goes with them (P33⑩: Esc 清)
				self.toasts.clear();
				self.selection.clear();
				continue;
			}

			if *key == Key::F5 {
				self.cache.forget(self.tab().path());
				ctx.request_repaint();
				continue;
			}

			// `C` while the bin's tab is up: empty it after one question (P27). The keymap has no
			// binding for the bare key — the bin is the only place it means anything.
			if *key == Key::C && !modifiers.ctrl && !modifiers.alt && self.in_bin() {
				self.trash_empty_ask();
				ctx.request_repaint();
				continue;
			}

			let text = (!typed.is_empty() && !modifiers.ctrl && !modifiers.alt).then_some(typed);
			let Some(chord) = crate::keys::chord(*key, *modifiers, text) else { continue };
			metrics::input("key");

			// the global four on the list (P21): no field took the keyboard, so Ctrl+A is every
			// row selected and Ctrl+C / Ctrl+X / Ctrl+V are the clipboard's own verbs — `y`, `x`
			// and `p` under another spelling. A field's own meanings were handled above. `Ctrl+Z`
			// joins them as `u`'s twin (P33⑫).
			if modifiers.ctrl
				&& let Some(action) = match *key {
					Key::A => Some(Action::ToggleAll(Some(true))),
					Key::C => Some(Action::Yank { cut: false }),
					Key::X => Some(Action::Yank { cut: true }),
					Key::V => Some(Action::Paste { force: false }),
					Key::Z => Some(Action::Undo),
					_ => None,
				}
			{
				self.act(action, ctx);
				continue;
			}

			if let Some(actions) = self.keys.feed(&mut self.sequence, &chord) {
				self.which_at = None;
				for action in actions {
					self.act(action, ctx);
				}
			} else if let Some(open) = &self.sequence {
				// a family is open: the hint goes up, and a key inside a longer chord resets its
				// clock rather than letting it run out under the person's hand
				metrics::which(&chord, self.keys.family(open).len());
				self.which_at = Some(Instant::now());
				ctx.request_repaint_after(WHICH_TIMEOUT);
			}
		}
	}

	/// The spot card's keyboard (the `[spot]` keymap): the card stays up while the cursor walks the
	/// list beneath it, `Tab`/`Esc` put it away, and `c c` copies the file's path.
	fn spot_keys(&mut self, events: &[egui::Event], ctx: &Context) {
		let len = self.tab().view.len();

		for event in events {
			// the card's own `Ctrl+C` puts it away — and the real window spells that as
			// `Event::Copy`, never as a key (P35②)
			if matches!(event, egui::Event::Copy) {
				self.spot = false;
				self.spot_chord = false;
				ctx.request_repaint();
				continue;
			}

			let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };
			metrics::input("key");

			match key {
				Key::Escape | Key::Tab => {
					self.spot = false;
					self.spot_chord = false;
				}
				Key::C if modifiers.ctrl => {
					self.spot = false;
					self.spot_chord = false;
				}
				Key::J | Key::ArrowDown | Key::L | Key::ArrowRight => {
					self.spot_chord = false;
					self.set_cursor((self.cursor() + 1).min(len.saturating_sub(1)));
				}
				Key::K | Key::ArrowUp | Key::H | Key::ArrowLeft => {
					self.spot_chord = false;
					self.set_cursor(self.cursor().saturating_sub(1));
				}
				Key::C => {
					if std::mem::take(&mut self.spot_chord) {
						if let Some(path) = self.selected() {
							ctx.copy_text(path.display().to_string());
							self.toasts.push("已复制路径", toast::Kind::Ok);
						}
					} else {
						self.spot_chord = true;
					}
				}
				_ => self.spot_chord = false,
			}

			ctx.request_repaint();
		}
	}

	/// The filter field's keyboard: printing keys are the pattern (which filters as it is typed),
	/// Enter keeps it, Esc and `f` clear it.
	fn filter_keys(&mut self, events: &[egui::Event], typed: &str, ctx: &Context) {
		let mut closed = false;
		let mut kept = None;

		for event in events {
			let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };

			match key {
				Key::Escape => {
					closed = true;
					kept = Some(None);
				}
				Key::Enter => {
					closed = true;
					kept = Some(self.filter.as_ref().map(|edit| edit.text.clone()).filter(|text| !text.is_empty()));
				}
				Key::F if !modifiers.ctrl && !modifiers.alt && typed.is_empty() => {
					closed = true;
					kept = Some(None);
				}
				_ => {
					if let Some(edit) = &mut self.filter {
						edit.key(*key, *modifiers);
					}
				}
			}
		}

		if !typed.is_empty()
			&& let Some(edit) = &mut self.filter
		{
			edit.type_text(typed);
		}

		let pattern = self.filter.as_ref().map(|edit| edit.text.clone()).filter(|text| !text.is_empty());
		self.filter_apply(pattern);

		if closed {
			self.filter = None;
			self.filter_apply(kept.flatten());
		}

		ctx.request_repaint();
	}

	/// The bulk editor's keyboard: every printing key is text, Enter makes a line, Ctrl+Enter
	/// submits and Esc throws the editor away.
	fn bulk_keys(&mut self, events: &[egui::Event], typed: &str, ctx: &Context) {
		let (mut close, mut commit) = (false, false);
		let mut touched = !typed.is_empty();

		for event in events {
			let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };
			touched = true;

			if *key == Key::Escape {
				close = true;
				continue;
			}
			if *key == Key::Enter && modifiers.ctrl {
				commit = true;
				continue;
			}

			if let Some(bulk) = &mut self.bulk {
				if *key == Key::Enter {
					bulk.edit.newline();
				} else {
					bulk.edit.key(*key, *modifiers);
				}
			}
		}

		if let Some(bulk) = &mut self.bulk {
			bulk.edit.type_lines(typed);
		}

		if close {
			self.bulk = None;
		} else if commit {
			self.bulk_commit();
		} else if touched {
			self.bulk_replan();
		}
		ctx.request_repaint();
	}

	/// The in-place rename's keyboard: Esc puts the name back, Enter commits when the name is
	/// legal, and everything else edits the field.
	fn rename_keys(&mut self, events: &[egui::Event], typed: &str, ctx: &Context) {
		let (mut close, mut commit) = (false, false);
		let mut touched = !typed.is_empty();

		for event in events {
			let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };
			touched = true;

			match key {
				Key::Escape => close = true,
				Key::Enter => commit = true,
				_ => {
					if let Some(rename) = &mut self.item_rename {
						rename.edit.key(*key, *modifiers);
					}
				}
			}
		}

		if let Some(rename) = &mut self.item_rename {
			rename.edit.type_text(typed);
		}

		if close {
			self.item_rename = None;
		} else if commit {
			self.rename_commit();
		} else if touched {
			self.rename_check();
		}
		ctx.request_repaint();
	}

	/// The command line's keyboard: Enter runs, Esc puts it away, Tab completes the last token,
	/// the arrows walk the completion — or the history when none is up — and everything else
	/// edits the line.
	fn cmdline_keys(&mut self, events: &[egui::Event], typed: &str, ctx: &Context) {
		let (mut close, mut run) = (false, false);
		let mut touched = !typed.is_empty();

		for event in events {
			let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };
			touched = true;

			match key {
				Key::Escape => close = true,
				Key::Enter => run = true,
				Key::Tab => self.cmdline_complete(),
				Key::ArrowUp => self.cmdline_recall(-1),
				Key::ArrowDown => self.cmdline_recall(1),
				_ => {
					if let Some(cmdline) = &mut self.cmdline {
						cmdline.recall = None;
						cmdline.comp = None;
						cmdline.edit.key(*key, *modifiers);
					}
				}
			}
		}

		if !typed.is_empty()
			&& let Some(cmdline) = &mut self.cmdline
		{
			cmdline.recall = None;
			cmdline.comp = None;
			cmdline.edit.type_text(typed);
		}

		if close {
			self.cmdline = None;
		} else if run {
			if let Some(cmdline) = self.cmdline.take() {
				self.shell_run(&cmdline.edit.text);
			}
		} else if touched {
			self.cmdline_refresh();
		}
		ctx.request_repaint();
	}

	/// open-with's keyboard: Enter hands the files over, Esc puts the field away, Tab completes,
	/// the arrows recall or walk, and everything else edits the command.
	fn openwith_keys(&mut self, events: &[egui::Event], typed: &str, ctx: &Context) {
		let (mut close, mut submit) = (false, false);

		for event in events {
			let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };

			match key {
				Key::Escape => close = true,
				Key::Enter => submit = true,
				Key::Tab => self.openwith_complete(),
				Key::ArrowUp => self.openwith_recall(-1),
				Key::ArrowDown => self.openwith_recall(1),
				_ => {
					if let Some(open) = &mut self.openwith {
						open.recall = None;
						open.comp = None;
						open.edit.key(*key, *modifiers);
					}
				}
			}
		}

		if !typed.is_empty()
			&& let Some(open) = &mut self.openwith
		{
			open.recall = None;
			open.comp = None;
			open.edit.type_text(typed);
		}

		if close {
			self.openwith = None;
		} else if submit {
			self.openwith_submit();
		}
		ctx.request_repaint();
	}

	/// The fuzzy panel's keyboard: Esc leaves, Enter asks the mode's question, the arrows walk
	/// the hits (Ctrl+p/n too, the fzf habit), and everything else is query text. Only a query
	/// that really changed restarts a walk — an arrow must not throw one away.
	fn fuzzy_keys(&mut self, events: &[egui::Event], typed: &str, ctx: &Context) {
		let (mut close, mut enter) = (false, false);
		let mut moved = 0isize;
		let mut touched = false;
		// which half held the keyboard when the call arrived (P33⑬): `f`'s own letter arrives in
		// the same call that steps into the box, and a keypress meant for the list must not type
		// just because the same call is the one that moved the keyboard
		let was_inside = self.fuzzy.as_ref().is_some_and(|fuzzy| fuzzy.focus.inside);

		for event in events {
			let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };

			let index = self.fuzzy.as_ref().is_some_and(|fuzzy| fuzzy.mode == FuzzyMode::Index);
			// which half of the panel holds the keyboard (P33⑬): the list it is, unless `f` — or a
			// click on the box — has put it inside
			let inside = self.fuzzy.as_ref().is_some_and(|fuzzy| fuzzy.focus.inside);

			if index
				&& let Some(door) = index_door(*key, modifiers.ctrl)
			{
				match door {
					IndexDoor::Save => self.index_save(),
					IndexDoor::Delete => self.index_delete(),
				}
				continue;
			}

			// `M`'s panel: the table's nine options are its own keys, not query letters (P21)
			if matches!(self.fuzzy.as_ref().map(|fuzzy| fuzzy.mode), Some(FuzzyMode::Devices { .. }))
				&& let Some(door) = device_door(*key, modifiers.shift)
			{
				self.device_door(door);
				continue;
			}

			// `i` again steps into the box, the user's own final ruling (P33⑬): the same key
			// twice — open the panel, press it once more, and the path box has the keyboard. `f`
			// stays the help panel's door, and the chord itself stays one key. Only from the list.
			if *key == Key::I && index && !inside && !modifiers.ctrl && !modifiers.alt {
				if let Some(fuzzy) = &mut self.fuzzy {
					fuzzy.focus.enter();
				}
				continue;
			}

			match (*key, modifiers.ctrl) {
				// Escape steps out of the box first and closes the panel only from the list — and
				// only the index panel keeps a list to step back to (P33⑬)
				(Key::Escape, _) => {
					let stepped_out = index && self.fuzzy.as_mut().is_some_and(|fuzzy| fuzzy.focus.escape());
					if !stepped_out {
						close = true;
					}
				}
				(Key::Enter, _) => enter = true,
				(Key::J, false) if !inside => moved += 1,
				(Key::K, false) if !inside => moved -= 1,
				(Key::Q, _) if index || matches!(self.fuzzy.as_ref().map(|fuzzy| fuzzy.mode), Some(FuzzyMode::Devices { .. })) => close = true,
				(Key::ArrowUp, _) | (Key::P, true) | (Key::K, true) => moved -= 1,
				(Key::ArrowDown, _) | (Key::N, true) | (Key::J, true) => moved += 1,
				_ => {
					// a letter on the list is navigation's nobody: it does not type (P33⑬) — the
					// box takes the keyboard before it takes keys
					if inside
						&& let Some(fuzzy) = &mut self.fuzzy
					{
						touched |= fuzzy.query.key(*key, *modifiers);
					}
				}
			}
		}

		if !typed.is_empty()
			&& was_inside
			&& let Some(fuzzy) = &mut self.fuzzy
		{
			// nothing is filtered out of what was typed (P24): `a` and `A` are search words again,
			// now that they are no longer the panel's own commands
			fuzzy.query.type_text(typed);
		}

		if close {
			self.fuzzy_close();
		} else if enter {
			self.fuzzy_enter();
		} else {
			if moved != 0 {
				self.fuzzy_move(moved);
			}
			if touched && self.fuzzy.is_some() {
				self.fuzzy_requery();
			}
		}
		ctx.request_repaint();
	}

	/// `L`'s panel (P21): three rows, the standing one marked, and the keys under it.
	fn draw_link_pick(&mut self, ui: &mut Ui, area: Rect, small: &FontId) {
		let Some(at) = self.link_pick else { return };

		let tokens = self.tokens;
		let font = FontId::new(FONT_SIZE, FontFamily::Monospace);
		let painter = ui.painter().clone();
		let panel = Rect::from_center_size(area.center(), vec2(460.0f32.min(area.width() - 32.0), 200.0));

		painter.rect_filled(area, 0.0, Color32::from_black_alpha(120));
		chrome::shadow(&painter, panel, 8.0);
		painter.rect_filled(panel, 8.0, tokens.raised);
		painter.rect_stroke(panel, 8.0, (1.0, tokens.border_strong), egui::StrokeKind::Inside);

		painter.text(pos2(panel.min.x + 20.0, panel.min.y + 24.0), Align2::LEFT_CENTER, "创建链接", font.clone(), tokens.text);

		let choices = [ (")", "符号链接（绝对路径）"), ("(", "符号链接（相对路径）"), ("*", "硬链接") ];
		let (mut seat, mut take) = (None, false);
		for (i, (key, what)) in choices.iter().enumerate() {
			let row = Rect::from_min_size(pos2(panel.min.x + 12.0, panel.min.y + 44.0 + i as f32 * 34.0), vec2(panel.width() - 24.0, 32.0));
			if i == at {
				painter.rect_filled(row, 0.0, tokens.cursor_bg);
				painter.rect_filled(Rect::from_min_size(row.min, vec2(2.0, row.height())), 0.0, tokens.accent);
			}
			painter.text(pos2(row.min.x + 12.0, row.center().y), Align2::LEFT_CENTER, *key, font.clone(), tokens.green);
			painter.text(pos2(row.min.x + 44.0, row.center().y), Align2::LEFT_CENTER, *what, font.clone(), tokens.text);

			// a click seats the choice, a double-click takes it (the same two-step the panels use)
			let response = ui.interact(row, egui::Id::unique(("link-choice", i)), Sense::click());
			if response.double_clicked() {
				seat = Some(i);
				take = true;
			} else if response.clicked() {
				seat = Some(i);
			}
		}
		if let Some(i) = seat {
			self.link_pick = Some(i);
		}
		if take
			&& let Some(i) = self.link_pick
		{
			let _ = i;
			self.link_choose();
		}

		painter.text(
			pos2(panel.min.x + 20.0, panel.max.y - 22.0),
			Align2::LEFT_CENTER,
			")/( * 选择 · Enter 确定 · Esc 关闭（路径不存在会提示）",
			small.clone(),
			tokens.disabled,
		);
	}

	/// The confirmation's keyboard (§4.5): `y` answers yes, `n`, Enter and Esc answer no — the
	/// focus sits on the cancel side, and the keyboard's own answers are quicker than a Tab.
	fn confirm_keys(&mut self, events: &[egui::Event], typed: &str, ctx: &Context) {
		let mut yes = false;
		let mut no = false;

		for event in events {
			let egui::Event::Key { key, pressed: true, .. } = event else { continue };
			match key {
				Key::Escape | Key::Enter => no = true,
				Key::Y => yes = true,
				Key::N => no = true,
				_ => {}
			}
		}

		// the layout's own text: a `y` typed straight is still a yes
		if typed.starts_with('y') {
			yes = true;
		}
		if typed.starts_with('n') {
			no = true;
		}

		if yes {
			self.confirm_yes();
		} else if no {
			self.confirm = None;
		}
		ctx.request_repaint();
	}

	fn wheel(&mut self, ctx: &Context, area: Rect, pointer: Option<egui::Pos2>) {
		// Two readings of one event: `content` is egui's own (how the *content* should move), which
		// the panels already speak; `rows` is the list's own (toward the tail), from the single
		// conversion in `scroll::wheel_rows`, which is what the list and the preview consume.
		let (mut content, mut rows) = (0.0f32, 0.0f32);

		for event in ctx.input(|i| i.events.clone()) {
			if let egui::Event::MouseWheel { unit, delta, .. } = event {
				content += match unit {
					egui::MouseWheelUnit::Line => delta.y,
					egui::MouseWheelUnit::Point => delta.y / ROW_HEIGHT,
					egui::MouseWheelUnit::Page => delta.y,
				};
				rows += scroll::wheel_rows(unit, delta.y);
			}
		}
		if content == 0.0 && rows == 0.0 {
			return;
		}

		self.wheel_route(ctx, area, pointer, content, rows);
	}

	/// Where a wheel's two readings land, decided by `wheel_owner` alone: one function, so the event
/// path and the replay cannot drift apart (P18), and the owner can be read at a glance (P38③).
	fn wheel_route(&mut self, ctx: &Context, area: Rect, pointer: Option<egui::Pos2>, content: f32, rows: f32) {
		match self.wheel_owner(area, pointer) {
			WheelOwner::Menu => {
				let vis = self.menu.as_ref().map_or(0, |menu| menu.visible_rows(self.body.height()));
				if let Some(menu) = &mut self.menu {
					menu.wheel(rows, vis);
				}
			}
			WheelOwner::Tabs => {
				// the tab bar's own horizontal scroll, when the chips outgrow it
				let overflow = self.tabs_overflow(area.width());
				self.tabs_scroll = (self.tabs_scroll - content * 24.0).clamp(0.0, overflow);
			}
			WheelOwner::Output => {
				if let Some(output) = &mut self.output {
					output.from = output.from.saturating_add_signed(-content.round() as isize);
				}
			}
			WheelOwner::Parent => {
				// the left column's own scroll (P33⑧): the pointer over it walks it, whatever the
				// keyboard's column is doing — the middle list hears nothing of this wheel
				self.tab_mut().parent.scroll.wheel(rows);
				if self.shot.is_some() {
					self.shot_wait = Some(4);
				}
			}
			WheelOwner::Preview => {
				// `Ctrl+滚轮` is §3.1's zoom gesture — a wheel back is a zoom in; a plain wheel
				// scrolls the column's own content, rows toward the tail
				let ctrl = ctx.input(|i| i.modifiers.ctrl);
				if ctrl {
					self.preview.zoom_by(rows < 0.0);
				} else if self.preview.scroll(rows) {
					self.bars_moved[1] = Some(Instant::now());
				}
			}
			WheelOwner::Help => {
				// the wheel browses the help's list the way `j`/`k` do (P21): the standing row walks
				let step = -content.round() as isize;
				let visible = help_rows(self.keys.rows(), &self.help_query.text);
				if !visible.is_empty() {
					self.help_at = (self.help_at as isize + step).clamp(0, visible.len() as isize - 1) as usize;
				}
			}
			WheelOwner::List => {
				// the bin, while it is the view, has its own place in its own list (P19/P27)
				if self.in_bin() {
					self.bin_mut().expect("in the bin's tab").scroll.wheel(rows);
				} else {
					self.tab_mut().scroll.wheel(rows);
				}
				self.bars_moved[0] = Some(Instant::now());
				if self.shot.is_some() {
					self.shot_wait = Some(4);
				}
			}
		}

		ctx.request_repaint();
	}

	/// The one owner of a wheel (P38③): the priority the user wrote down — a standing panel, then
	/// the preview under the pointer, then the list — with the window's other regions deciding
	/// between them exactly as they always did (the tab bar, the output panel, the parent column,
	/// the help list).
	fn wheel_owner(&self, area: Rect, pointer: Option<egui::Pos2>) -> WheelOwner {
		// a standing panel owns the wheel whatever the pointer is over (P36②)
		if self.menu.is_some() {
			return WheelOwner::Menu;
		}
		if pointer.is_some_and(|at| area.contains(at) && at.y < area.min.y + TAB_HEIGHT) {
			return WheelOwner::Tabs;
		}
		if let Some(output) = &self.output
			&& pointer.is_some_and(|at| output_rect(area, output.lines.len()).contains(at))
		{
			return WheelOwner::Output;
		}

		let columns = chrome::columns(self.body, self.shares);
		if pointer.zip(columns.parent).is_some_and(|(at, rect)| rect.contains(at)) {
			return WheelOwner::Parent;
		}
		if pointer.zip(columns.preview).is_some_and(|(at, rect)| rect.contains(at)) {
			return WheelOwner::Preview;
		}
		if self.help {
			return WheelOwner::Help;
		}

		WheelOwner::List
	}

	/// How far the chips outgrow the bar, in pixels: the clamp for the bar's own scroll.
	fn tabs_overflow(&self, width: f32) -> f32 {
		let advance = FONT_SIZE * 0.62;
		let chips: f32 = self
			.tabs
			.iter()
			.enumerate()
			.map(|(index, tab)| chip_width(&tab.name(), advance) + number_width(index) + CHIP_GAP)
			.sum();
		(chips + 16.0 - (width - 44.0)).max(0.0)
	}

	// ---- the replay: steps a burst of input can be driven with, for the acceptance ----

	fn replay(&mut self, ctx: &Context, area: Rect) {
		let waited = self.seed_at.elapsed();
		if self.seed_next < self.seed.len() && waited < Duration::from_millis(350) {
			// the window paints on demand: a step that is waiting for its moment has to ask back
			ctx.request_repaint_after(Duration::from_millis(350) - waited);
			return;
		}
		if self.seed_next >= self.seed.len() {
			return;
		}

		let step = self.seed[self.seed_next].clone();
		self.seed_next += 1;
		self.seed_at = Instant::now();

		let last = self.seed_next >= self.seed.len();
		if let Some(ch) = step.strip_prefix("key ") {
			if let Some((key, modifiers)) = replay_key(ch.trim()) {
				let mut events =
					vec![ egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers } ];
				match replay_text(ch.trim()) {
					Some(text) => {
						events.push(egui::Event::Text(text.to_owned()));
						self.handle(ctx, &events, text);
					}
					None => self.handle(ctx, &events, ""),
				}
			}
		} else if let Some(text) = step.strip_prefix("paste ") {
			// a real Ctrl+V: egui's own paste event, through the same handler a keyboard uses
			self.handle(ctx, &[ egui::Event::Paste(text.to_owned()) ], text);
		} else if let Some(text) = step.strip_prefix("ime ") {
			// a Chinese input method's commit
			self.handle(ctx, &[ egui::Event::Ime(egui::ImeEvent::Commit(text.to_owned())) ], text);
		} else if let Some(text) = step.strip_prefix("type ") {
			// a paste-like delivery of text: the same events a layout sends, `\n` and all
			let text = text.replace("\\n", "\n");
			self.handle(ctx, &[ egui::Event::Text(text.clone()) ], &text);
		} else if let Some(i) = step.strip_prefix("click ") {
			// a single click on the i-th *visible* row, through the same mapping the mouse path
			// uses — and a click past the rows (a blank one) goes through it too, deciding nothing
			let i: usize = i.trim().parse().unwrap_or(0);
			let first = self.tab().scroll.offset.floor() as usize;
			let len = self.tab().view.len();
			if let Some(position) = clicked_row(first, i, len) {
				self.click_row(position);
			}
			ctx.request_repaint();
		} else if let Some(rows) = step.strip_prefix("scroll ") {
			// script-level rows: positive is deeper into the list — the same view the wheel's
			// routing picks (P33③: the bin's list steps on its own, and a script has to drive
			// whichever list is actually on screen)
			let rows = rows.trim().parse().unwrap_or(3.0);
			if self.in_bin() {
				self.bin_mut().expect("in the bin's tab").scroll.wheel(rows);
			} else {
				self.tab_mut().scroll.wheel(rows);
			}
			self.bars_moved[0] = Some(Instant::now());
			self.shot_wait = Some(4);
		} else if let Some(what) = step.strip_prefix("zoom ") {
			// the zoom gestures without a pointer: `zoom in|out` are the Ctrl+wheel steps, and
			// `fit` / `full` are what the spec's own keys would call
			match what.trim() {
				"in" => {
					self.preview.zoom_by(true);
				}
				"out" => {
					self.preview.zoom_by(false);
				}
				"fit" => {
					self.preview.zoom_fit();
				}
				"full" => {
					self.preview.zoom_full();
				}
				_ => {}
			}
			self.shot_wait = Some(4);
		} else if let Some(delta) = step.strip_prefix("wheel ") {
			// a real wheel event's own arithmetic: the `delta.y` egui would deliver (positive =
			// the content moves down), through the very routing the event loop uses — with a
			// `hover` step's pointer standing over the preview column, that column is what scrolls
			let delta = delta.trim().parse().unwrap_or(0.0);
			let rows = scroll::wheel_rows(egui::MouseWheelUnit::Line, delta);
			self.wheel_route(ctx, area, self.replay_at, delta, rows);
			self.shot_wait = Some(4);
		} else if let Some(rest) = step.strip_prefix("hover ") {
			// where the replay's pointer stands, without a pointer: `hover <x> <y>` in the
			// window's own points, and `hover` alone takes it away again
			let numbers: Vec<f32> = rest.split_whitespace().filter_map(|n| n.parse().ok()).collect();
			self.replay_at = match numbers[..] {
				[ x, y ] => Some(pos2(x, y)),
				_ => None,
			};
		} else if let Some(rest) = step.strip_prefix("menu ") {
			// the right click's own dispatch (P31), without a mouse: `menu <可见行号>` opens the
			// row's menu, and a number past the rows opens the blank's own directory
			let at: usize = rest.trim().parse().unwrap_or(0);
			let first = self.tab().scroll.offset.floor() as usize;
			let len = self.tab().view.len();
			let anchor = self.replay_at.unwrap_or_else(|| self.body.min + vec2(24.0, 24.0));
			match clicked_row(first, at, len) {
				Some(position) => self.menu_on_row(position, anchor),
				None => self.menu_on_blank(anchor),
			}
			self.shot_wait = Some(4);
		} else if let Some(path) = step.strip_prefix("reveal ") {
			self.reveal(PathBuf::from(path.trim()));
		} else if step == "shot" {
			// a shot where the replay stands: the only way to catch a loading state
			self.shot_wait = Some(1);
		} else if let Some(rest) = step.strip_prefix("drag ") {
			// a separator drag, without a pointer: the same arithmetic the response path runs,
			// and the same write to the config file it ends with
			let numbers: Vec<f32> = rest.split_whitespace().filter_map(|n| n.parse().ok()).collect();
			if let [ left, right, px ] = numbers[..] {
				let columns = chrome::columns(self.body, self.shares);
				let visible = [ columns.parent.is_some(), true, columns.preview.is_some() ];
				self.shares = chrome::resized(self.shares, left as usize, right as usize, px, self.body, visible);
				self.save_columns();
			}
		}

		if last {
			self.shot_wait = Some(3);
		}
		ctx.request_repaint();
	}

	/// Put the cursor on a path, wherever it is: the directory stack is rebuilt around it.
	fn reveal(&mut self, path: PathBuf) {
		self.reveal = Some(path);
	}

	/// A reveal needs the directory's snapshot to find the file in. If it is still being read, the
	/// request waits for it — a reveal that arrives before the list does is not an error.
	fn try_reveal(&mut self, ctx: &Context) {
		let Some(path) = self.reveal.clone() else { return };
		let Some(dir) = path.parent() else {
			self.reveal = None;
			return;
		};

		let snapshot = self.cache.get(dir);
		if snapshot.loading {
			ctx.request_repaint();
			return;
		}
		self.reveal = None;

		let Some(index) = snapshot.entries.iter().position(|entry| dir.join(&entry.name) == path) else {
			return;
		};

		let tab = self.tab_mut();
		tab.stack = vec![ View { path: dir.to_owned(), cursor: index } ];
		tab.scroll.jump(0.0);
		self.arrived();
	}

	// ---- the numbers ----

	fn shots(&mut self, ctx: &Context) {
		let Some(wait) = self.shot_wait else { return };
		if wait > 0 {
			self.shot_wait = Some(wait - 1);
			ctx.request_repaint();
			return;
		}
		self.shot_wait = None;

		let Some((prefix, index)) = &mut self.shot else { return };
		let path = format!("{prefix}-{index}.png");
		*index += 1;

		// a PNG rather than the old PPM (P25): the harness's own frames and a real window's come
		// out of the same call, and one format keeps the evidence folders readable
		ctx.request_screenshot(move |image| {
			let [w, h] = image.size;
			let rgba: Vec<u8> = image.pixels.iter().flat_map(|pixel| pixel.to_srgba_unmultiplied()).collect();
			let written = image::RgbaImage::from_raw(w as u32, h as u32, rgba)
				.ok_or_else(|| "picture of the wrong size".to_owned())
				.and_then(|picture| picture.save(&path).map_err(|e| e.to_string()));

			match written {
				Ok(()) => eprintln!("crabhop: shot {path}"),
				Err(e) => eprintln!("crabhop: shot {path}: {e}"),
			}
		});
	}

	// ---- the drawing ----

	fn draw(&mut self, ui: &mut Ui, area: Rect, body: Rect) {
		let painter = ui.painter().clone();
		let font = FontId::new(FONT_SIZE, FontFamily::Monospace);
		let small = FontId::new(FONT_SMALL, FontFamily::Monospace);
		let advance = painter.layout_no_wrap("0".to_owned(), font.clone(), self.tokens.text).size().x;

		self.tab_bar(ui, area, &font, advance);
		self.draw_columns(ui, body, &font, advance);
		self.draw_spot(ui, body);
		self.draw_menu(ui, area, &font, &small);
		self.status_bar(ui, area, &small);
		self.draw_trash_hint(ui, area, &small);
		self.draw_output(ui, area, &small);
		if self.cmdline.is_some() {
			self.draw_cmdline(ui, area, advance, &font, &small);
		}
		self.draw_filter(ui, area, advance, &font, &small);
		self.draw_find(ui, area, advance, &font, &small);

		if self.help {
			self.draw_help(ui, area, advance, &font);
		}
		if self.openwith.is_some() {
			self.draw_openwith(ui, area, advance, &font, &small);
		}
		if self.fuzzy.is_some() {
			self.draw_fuzzy(ui, area, &font, &small);
		}
		if self.prompt.is_some() {
			self.draw_prompt(ui, area, advance, &font, &small);
		}
		if self.link_pick.is_some() {
			self.draw_link_pick(ui, area, &small);
		}
		if self.confirm.is_some() {
			self.draw_confirm(ui, area, &small);
		}
		if self.bulk.is_some() {
			self.draw_bulk(ui, area, &painter, advance, &font, &small);
		}
		self.draw_toasts(ui, area, &small);
		self.draw_which(ui, area, &small);
	}

	/// The tab bar (§2.4): flat chips, the active one underlined with 2px of accent, a close that
	/// only shows itself when the chip is active or hovered, and a ghost `+` at the end.
	fn tab_bar(&mut self, ui: &mut Ui, area: Rect, font: &FontId, advance: f32) {
		let tokens = self.tokens;
		let rect = Rect::from_min_size(area.min, vec2(area.width(), TAB_HEIGHT));
		let painter = ui.painter().clone();
		painter.rect_filled(rect, 0.0, tokens.base);
		painter.line_segment([ rect.left_bottom(), rect.right_bottom() ], (1.0, tokens.border));

		let strip = Rect::from_min_max(rect.min, pos2(rect.max.x - 36.0, rect.max.y));
		let chips: Vec<(usize, String, f32, String)> = self
			.tabs
			.iter()
			.enumerate()
			.map(|(index, tab)| {
				let width = chip_width(&tab.name(), advance) + number_width(index);
				(index, tab.name(), width, tab.path().display().to_string())
			})
			.collect();

		let mut actions = ChipActions::default();
		let mut field: Option<(Rect, usize)> = None;
		let mut at = strip.min.x + 8.0 - self.tabs_scroll;
		self.tab_close_hits.clear();
		self.tab_chip_hits.clear();

		for (index, name, width, path) in &chips {
			let index = *index;
			let chip = Rect::from_min_size(pos2(at, rect.min.y + 4.0), vec2(*width, TAB_HEIGHT - 8.0));
			at += width + CHIP_GAP;
			if chip.max.x < strip.min.x || chip.min.x > strip.max.x {
				continue;
			}

			let clipped = painter.with_clip_rect(strip);
			let response = ui.interact(chip, egui::Id::unique(("tab", index)), Sense::click_and_drag());
			self.tab_chip_hits.push((index, chip));
			let active = index == self.active;

			if response.hovered() && !active {
				clipped.rect_filled(chip, 6.0, tokens.cursor_bg.gamma_multiply(0.5));
			}
			if active {
				clipped.rect_filled(
					Rect::from_min_size(pos2(chip.min.x + 6.0, rect.max.y - 3.0), vec2(chip.width() - 12.0, 2.0)),
					0.0,
					tokens.accent,
				);
			}

			if self.rename_at == Some(index) {
				field = Some((Rect::from_min_size(pos2(chip.min.x + 8.0, chip.min.y + 6.0), vec2(chip.width() - 42.0, chip.height() - 12.0)), index));
			} else {
				// the chip's own number (§2.4): the badge the number keys jump by, in the small
				// secondary face — past nine it is simply the number it is
				let number = format!("{}", index + 1);
				let badge = number_width(index);
				clipped.text(
					pos2(chip.min.x + 8.0, chip.min.y + chip.height() * 0.5),
					Align2::LEFT_CENTER,
					&number,
					FontId::new(FONT_SMALL, FontFamily::Monospace),
					tokens.subtext,
				);

				let budget = ((chip.width() - 30.0 - badge) / advance).floor().max(1.0) as u32;
				clipped.text(
					pos2(chip.min.x + 8.0 + badge, chip.min.y + chip.height() * 0.5),
					Align2::LEFT_CENTER,
					crate::list::clipped(name, budget),
					font.clone(),
					if active { tokens.text } else { tokens.subtext },
				);
			}

			// the close (P33②): its box stands on every chip, and only its glyph shows for the active
			// or hovered one, 20x20 of hit area. The box may not be gated on the same frame's hover:
			// egui collapses the hover snapshot onto the very widget a click is delivered to, so a
			// click arriving on an inactive chip's × found the branch skipped — the handler was
			// gone in the one frame it was needed, and the × never answered anyone.
			let hot = Rect::from_center_size(pos2(chip.max.x - 14.0, chip.min.y + chip.height() * 0.5), vec2(20.0, 20.0));
			self.tab_close_hits.push((index, hot));
			let closing = ui.interact(hot, egui::Id::unique(("tab-close", index)), Sense::click());
			if active || response.hovered() || closing.hovered() {
				clipped.text(
					hot.center(),
					Align2::CENTER_CENTER,
					"\u{f00d}",
					FontId::new(FONT_SMALL, FontFamily::Monospace),
					if closing.hovered() { tokens.err } else { tokens.subtext },
				);
			}
			if closing.clicked() || closing.middle_clicked() {
				actions.close = Some(index);
			}

			if response.clicked() {
				actions.switch = Some(index);
			}
			if response.middle_clicked() {
				actions.close = Some(index);
			}
			if response.double_clicked() {
				actions.rename = Some(index);
			}
			if response.dragged()
				&& let Some(pointer) = ui.ctx().input(|i| i.pointer.latest_pos())
			{
				let x = chrome::unscrolled(pointer, strip, self.tabs_scroll).x;
				let mut slot = 0usize;
				let mut walk = 8.0;
				for (other, (_, _, width, _)) in chips.iter().enumerate() {
					if x >= walk && x < walk + width + CHIP_GAP {
						slot = other;
						break;
					}
					slot = other + 1;
					walk += width + CHIP_GAP;
				}
				if slot >= self.tabs.len() {
					slot = self.tabs.len() - 1;
				}
				if slot != index {
					actions.moved = Some((index, slot));
				}
			}

			response.context_menu(|ui| {
				if ui.button("重命名").clicked() {
					actions.rename = Some(index);
					ui.close();
				}
				if ui.button("关闭").clicked() {
					actions.close = Some(index);
					ui.close();
				}
				if ui.button("关闭其他").clicked() {
					actions.close_others = Some(index);
					ui.close();
				}
				if ui.button("复制路径").clicked() {
					ui.ctx().copy_text(path.clone());
					ui.close();
				}
			});
		}

		// the ghost `+` (P33⑮): it stands just past the last chip now, the browser's own habit —
		// three tabs and it sits at the third's right, one per press further right — clamped so it
		// never slides under the corner's close. Same button, same token, a new place.
		let plus = Rect::from_center_size(
			pos2((at + 14.0).clamp(strip.min.x + 14.0, strip.max.x - 14.0), rect.center().y),
			vec2(28.0, 28.0),
		);
		let response = ui.interact(plus, egui::Id::unique("tab-new"), Sense::click());
		if response.hovered() {
			painter.rect_filled(plus, 6.0, tokens.cursor_bg.gamma_multiply(0.5));
		}
		painter.text(plus.center(), Align2::CENTER_CENTER, "+", font.clone(), tokens.subtext);
		if response.clicked() {
			actions.plus = true;
		}
		self.plus_hit = Some(plus);

		// and the corner's small close (P33⑯): the window's own × where the `+` used to live —
		// niri draws no decorations, so the window carries one itself. A press is a press: the
		// close goes out at once, no question asked, and the glyph reddens under the pointer.
		let close = Rect::from_center_size(pos2(rect.max.x - 20.0, rect.center().y), vec2(28.0, 28.0));
		let response = ui.interact(close, egui::Id::unique("window-close"), Sense::click());
		if response.hovered() {
			painter.rect_filled(close, 6.0, tokens.cursor_bg.gamma_multiply(0.5));
		}
		painter.text(
			close.center(),
			Align2::CENTER_CENTER,
			"\u{f00d}",
			FontId::new(FONT_SMALL, FontFamily::Monospace),
			if response.hovered() { tokens.err } else { tokens.subtext },
		);
		if response.clicked() {
			ui.ctx().send_viewport_cmd(ViewportCommand::Close);
		}
		self.close_hit = Some(close);

		// the fade masks: the chips thin out at whichever end has more behind it
		let overflow = self.tabs_overflow(area.width());
		if self.tabs_scroll > 0.0 {
			for (strip_rect, colour) in chrome::fade_strips(strip, true, tokens.base) {
				painter.rect_filled(strip_rect, 0.0, colour);
			}
		}
		if self.tabs_scroll < overflow {
			for (strip_rect, colour) in chrome::fade_strips(strip, false, tokens.base) {
				painter.rect_filled(strip_rect, 0.0, colour);
			}
		}

		// the rename field takes the keyboard while it is there
		if let Some((rect, _)) = field
			&& let Some(buffer) = self.rename.as_mut()
		{
			let response = ui.put(rect, TextEdit::singleline(buffer).id(egui::Id::unique("tab-rename")).frame(egui::Frame::NONE).font(font.clone()));
			if self.rename_focus {
				response.request_focus();
				self.rename_focus = false;
			}
			if response.lost_focus() {
				if ui.input(|i| i.key_pressed(Key::Enter)) {
					self.commit_rename();
				} else {
					self.rename = None;
					self.rename_at = None;
				}
			}
		}

		// the decisions, one by one, in the one place that can hold them
		if let Some((from, to)) = actions.moved {
			let tab = self.tabs.remove(from);
			self.tabs.insert(to, tab);
			if self.active == from {
				self.active = to;
			} else if from < self.active && to >= self.active {
				self.active -= 1;
			} else if from > self.active && to <= self.active {
				self.active += 1;
			}
		}
		if let Some(index) = actions.rename {
			self.rename_at = Some(index);
			self.rename = Some(self.tabs.get(index).map(Tab::name).unwrap_or_default());
			self.rename_focus = true;
		}
		if let Some(index) = actions.close_others {
			self.tabs = vec![ self.tabs.remove(index) ];
			self.active = 0;
			self.look();
		}
		if let Some(index) = actions.close {
			self.active = index;
			self.tab_close(ui.ctx());
		}
		if let Some(index) = actions.switch
			&& index != self.active
		{
			self.active = index;
			self.look();
		}
		if actions.plus {
			self.tab_create();
		}
	}

	/// The three columns, their separators (draggable, double-click to reset, §2.2), the rows,
	/// the skeleton and the overlay scrollbars.
	fn draw_columns(&mut self, ui: &mut Ui, body: Rect, font: &FontId, advance: f32) {
		let mut tokens = self.tokens;
		if !ui.ctx().input(|i| i.focused) {
			// the window has lost the focus: the selection says so (§1.2's inactive state)
			tokens.sel_bg = tokens.sel_inactive;
		}
		let painter = ui.painter().clone();
		let columns = chrome::columns(body, self.shares);
		let rows_visible = (body.height() / ROW_HEIGHT).floor().max(1.0) as usize;

		painter.rect_filled(body, 0.0, tokens.mantle);
		painter.rect_filled(columns.current, 0.0, tokens.base);
		if let Some(preview) = columns.preview {
			painter.rect_filled(preview, 0.0, tokens.crust);
		}

		// the parent column: the way back, as a short list above the current one — the stack's own
		// step when there is one, and otherwise the cwd's filesystem parent, so a tab standing at
		// its first directory still has a column that says where it is (§1.2's hierarchy).
		// The pointer is as live here as on the middle column (P33⑧): the column carries a seat
		// of its own (a click moves it) and a scroll of its own (the wheel walks it) — yazi's own
		// two rights on the column beside it, held whatever the keyboard is doing.
		if let Some(parent_rect) = columns.parent
			&& let Some((path, derived)) = self.parent_view()
		{
			let snapshot = self.cache.get(&path);
			let rows_visible = (parent_rect.height() / ROW_HEIGHT).floor().max(1.0) as usize;
			let len = snapshot.entries.len();

			// a different parent is a new column: its own reading stands, and the view shows it
			let (cursor, offset) = {
				let parent = &mut self.tab_mut().parent;
				if parent.path.as_ref() != Some(&path) {
					parent.path = Some(path.clone());
					parent.seat = None;
					parent.scroll = Scroll::default();
					parent.scroll.rows = rows_visible;
					parent.scroll.len = len;
					parent.scroll.follow(derived.min(len.saturating_sub(1)));
				}
				parent.scroll.rows = rows_visible;
				parent.scroll.len = len;
				parent.scroll.clamp();

				let cursor = parent.seat.map_or(derived, |seat| seat.min(len.saturating_sub(1)));
				(cursor, parent.scroll.offset)
			};
			let first = offset.floor() as usize;
			let fraction = offset - first as f32;

			Self::rows(
				&painter,
				&tokens,
				parent_rect,
				&snapshot.entries,
				None,
				cursor,
				self.columns,
				&self.selection,
				&path,
				font,
				advance,
				fraction,
				first,
				true,
				PARENT_META,
				hover_at(self.hover, parent_rect, ROW_HEIGHT, first),
				None,
			);

			// the pointer's own way in (P33⑧/P35①): a click seats the column's cursor where it landed —
			// and on a directory the click *enters* it, the middle column becoming that directory,
			// the user's own ruling on yazi's parent click. The cwd's own row only seats (there is
			// nowhere to go), and a file only seats (a single click never launches anything).
			let mut seated = None;
			for i in 0..rows_visible + 1 {
				let Some(position) = clicked_row(first, i, len) else { break };

				let top = parent_rect.min.y + (i as f32 - fraction) * ROW_HEIGHT;
				let row = Rect::from_min_size(pos2(parent_rect.min.x, top), vec2(parent_rect.width(), ROW_HEIGHT));
				if row.max.y < parent_rect.min.y || row.min.y > parent_rect.max.y {
					continue;
				}

				if ui.interact(row, egui::Id::unique(("parent-row", position)), Sense::click()).clicked() {
					seated = Some(position);
				}
			}
			if let Some(position) = seated {
				let target = snapshot.entries.get(position).map(|entry| (path.join(&entry.name), entry.dir));
				{
					let parent = &mut self.tab_mut().parent;
					parent.seat = Some(position);
					parent.scroll.follow(position);
				}

				if let Some((target, true)) = target
					&& target != *self.tab().path()
				{
					// the `go` door itself (P35①): the same one `g a` and a followed link use
					self.go(target);
				}
			}
		}

		// the current column: only the rows that show, moved by the fraction of a row that is left —
		// or, while the bin is the view, the bin's own rows (P19)
		if self.in_bin() {
			// the bin is the view: the middle column shows what is in it, and the columns are the
			// view rather than a popup over them (P19)
			self.draw_trash_list(ui, columns.current, font, advance);
		} else {
			let snapshot = self.entries();
			let (offset, len) = (self.tab().scroll.offset, self.tab().scroll.len);
			let first = offset.floor() as usize;
			let fraction = offset - first as f32;
			let cursor = self.cursor();
			let path = self.tab().path().to_owned();
			let view = self.tab().view.clone();
			let fits = view.len() <= rows_visible;
			let list = if fits {
				Rect::from_min_size(pos2(columns.current.min.x, columns.current.min.y + 8.0), columns.current.size())
			} else {
				columns.current
			};
			// how many rows fit is the scroll state's own, not only the painter's: `follow` is what
			// keeps a click or a `j` from rewinding the list, and it can only do that knowing the view
			self.tab_mut().scroll.rows = (list.height() / ROW_HEIGHT).floor().max(1.0) as usize;

			Self::rows(
				&painter,
				&tokens,
				list,
				&snapshot.entries,
				Some(&view),
				cursor,
				self.columns,
				&self.selection,
				&path,
				font,
				advance,
				fraction,
				first,
				false,
				CURRENT_META,
				hover_at(self.hover, list, ROW_HEIGHT, first),
				self.find_pattern().as_deref(),
			);

			// the mouse's own way into the list: a click seats the cursor, a double-click opens the
			// rename field (§4.3). While a rename is in flight, the rows stand back. Only the rows
			// themselves are interactive — the blank below the last row is nobody's seat, and a click
			// there must leave the cursor where the person left it.
			if self.item_rename.is_none() {
				// the blank under the rows is the window's own directory (P31's dispatch): its own
				// interact stands first, so a row's menu wins wherever a row is
				let blank = ui.interact(list, egui::Id::unique("list-blank"), Sense::click());
				let (mut clicked, mut renamed, mut wanted) = (None, None, None);

				for i in 0..rows_visible + 1 {
					let Some(position) = clicked_row(first, i, view.len()) else { break };

					let top = list.min.y + (i as f32 - fraction) * ROW_HEIGHT;
					let row = Rect::from_min_size(pos2(list.min.x, top), vec2(list.width(), ROW_HEIGHT));
					if row.max.y < list.min.y || row.min.y > list.max.y {
						continue;
					}

					let response = ui.interact(row, egui::Id::unique(("row", position)), Sense::click());
					if response.secondary_clicked() {
						wanted = Some((position, row.min));
					} else if response.double_clicked() {
						renamed = Some(position);
					} else if response.clicked() {
						clicked = Some(position);
					}
				}

				// the dispatch: a row inside the selection acts on the whole selection; anything
				// else seats the cursor on the clicked row first — the left click's own contract
				if let Some((position, at)) = wanted {
					self.menu_on_row(position, at);
				} else if blank.secondary_clicked() {
					self.menu_on_blank(list.min);
				} else if let Some(position) = renamed {
					self.rename_row(position);
				} else if let Some(position) = clicked {
					self.click_row(position);
				}
			}

			self.draw_item_rename(ui, list, first, fraction, advance, font);

			if snapshot.loading {
				self.skeleton(&painter, columns.current, rows_visible, &snapshot);
			} else if view.is_empty() {
				painter.text(columns.current.center(), Align2::CENTER_CENTER, "（空目录）", font.clone(), tokens.subtext);
			}

			// the overlay scrollbar of the list, and the preview's own
			if !snapshot.loading
				&& len > rows_visible
				&& let Some(requested) = self.overlay_bar(ui, columns.current, 0, len as f32, rows_visible as f32, offset)
			{
				self.tab_mut().scroll.jump(requested);
				self.bars_moved[0] = Some(Instant::now());
			}
		}
		if let Some(preview) = columns.preview {
			self.preview.draw(&painter, preview, &tokens);

			// §3.1's gestures: a drag pans a zoomed picture, and the cursor says so
			let response = ui.interact(preview, egui::Id::unique("preview-gestures"), Sense::click_and_drag());
			if response.dragged() {
				self.preview.pan_by(response.drag_delta());
			}
			if self.preview.zoomed() {
				ui.ctx().set_cursor_icon(if response.dragged() { CursorIcon::Grabbing } else { CursorIcon::Grab });
			}

			// the archive tree's own folds (P28's long tail): the row under the pointer is what a
			// click answers to, and only a folder answers — a hand says so before the click lands
			if let Some(rows) = self.preview.archive_tree(preview) {
				for (row, rect, dir) in rows {
					let response = ui.interact(rect, egui::Id::unique(("archive-row", row)), Sense::click());
					if dir && response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
						self.preview.archive_fold(row);
					}
				}
			}

			// the preview's own overlay scrollbar (P38③): registered after the whole-column
			// gesture widget and the tree's rows, so its own ten points are the top-most piece of
			// the column — before this order the column's drag swallowed the thumb's, and the
			// document previews' scrollbar was dead on every kind
			if let Some((content, offset, view)) = self.preview.extent()
				&& let Some(requested) = self.overlay_bar(ui, preview, 1, content, view, offset)
			{
				self.preview.scroll_to(requested);
				self.bars_moved[1] = Some(Instant::now());
			}
		} else {
			self.preview.hide();
		}

		// the separators: 1px of structure, 7px of drag, 2px of accent when they are alive
		let visible = [ columns.parent.is_some(), true, columns.preview.is_some() ];
		for (x, left, right) in columns.seps {
			let zone = Rect::from_min_max(pos2(x - chrome::SEP_ZONE * 0.5, body.min.y), pos2(x + chrome::SEP_ZONE * 0.5, body.max.y));
			let response = ui.interact(zone, egui::Id::unique(("separator", left, right)), Sense::click_and_drag());
			let alive = response.hovered() || response.dragged();

			if alive {
				ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
			}
			painter.line_segment(
				[ pos2(x, body.min.y), pos2(x, body.max.y) ],
				if alive { (2.0, tokens.accent.gamma_multiply(0.6)) } else { (1.0, tokens.border) },
			);

			if response.dragged() {
				self.shares = chrome::resized(self.shares, left, right, response.drag_delta().x, body, visible);
			}
			if response.drag_stopped() {
				self.save_columns();
			}
			if response.double_clicked() {
				self.shares = SHARES;
				self.save_columns();
			}
		}
	}

	/// The skeleton (§1.3): icon placeholders and bars, static — no shimmer — and only after the
	/// directory has kept a person waiting.
	fn skeleton(&mut self, painter: &Painter, column: Rect, rows_visible: usize, snapshot: &Snapshot) {
		let waited = snapshot.started.is_none_or(|at| at.elapsed() >= SKELETON_AFTER);
		if !waited {
			return;
		}

		let tokens = self.tokens;
		let rows = chrome::skeleton_rows(snapshot.hint, rows_visible);
		for row in 0..rows {
			let top = column.min.y + row as f32 * ROW_HEIGHT;
			let icon =
				Rect::from_center_size(pos2(column.min.x + 8.0 + ICON_BOX * 0.5, top + ROW_HEIGHT * 0.5), Vec2::splat(ICON_BOX));
			painter.rect_filled(icon, 4.0, tokens.raised);

			let width = chrome::skeleton_bar(row) * (column.width() - ICON_BOX - 48.0);
			let bar = Rect::from_min_size(pos2(column.min.x + ICON_BOX + 20.0, top + ROW_HEIGHT * 0.5 - 5.0), vec2(width, 10.0));
			painter.rect_filled(bar, 5.0, tokens.raised);
		}
	}

	/// One overlay scrollbar (§3 总规则): 6px, asleep until it is touched, awake for 1.2s and then
	/// fading out over 200ms; draggable while it is there. A drag asks for the offset under the
	/// pointer, which the caller applies to whatever it is the scrollbar of.
	fn overlay_bar(&mut self, ui: &mut Ui, column: Rect, which: usize, content: f32, visible: f32, offset: f32) -> Option<f32> {
		let tokens = self.tokens;
		let track = Rect::from_min_max(pos2(column.max.x - 10.0, column.min.y + 4.0), pos2(column.max.x, column.max.y - 4.0));
		let bar = chrome::bar(track, content, visible, offset)?;

		let response = ui.interact(bar.thumb, egui::Id::unique(("bar", which)), Sense::click_and_drag());
		let touched = self.bars_moved[which].is_some_and(|at| at.elapsed() < BAR_IDLE) || response.hovered() || response.dragged();
		let alpha = ui.ctx().animate_bool_with_time(egui::Id::unique(("bar-alpha", which)), touched, 0.2);

		if response.dragged() {
			let pointer = ui.ctx().input(|i| i.pointer.latest_pos()).unwrap_or(bar.track.center());
			return Some(chrome::dragged_offset(&bar, pointer.y, content, visible));
		}

		if alpha > 0.01 {
			let colour = if response.dragged() {
				tokens.accent.gamma_multiply(0.7 * alpha)
			} else if response.hovered() {
				tokens.subtext.gamma_multiply(alpha)
			} else {
				tokens.disabled.gamma_multiply(0.4 * alpha)
			};
			ui.painter().rect_filled(bar.thumb, 3.0, colour);
		}

		None
	}

	/// The status bar (§2.5): the mode, the cursor's permissions, what is busy, where the cursor
	/// is, what is selected — and on the right, the view's own truth: the filter, the sort, and
	/// which way it runs.
	fn status_bar(&mut self, ui: &mut Ui, area: Rect, small: &FontId) {
		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let rect = Rect::from_min_size(pos2(area.min.x, area.max.y - STATUS_HEIGHT), vec2(area.width(), STATUS_HEIGHT));
		painter.rect_filled(rect, 0.0, tokens.base);
		painter.line_segment([ rect.left_top(), rect.right_top() ], (1.0, tokens.border));

		let mut at = rect.min.x + 12.0;
		let middle = rect.center().y;
		let cell = painter.layout_no_wrap("0".to_owned(), small.clone(), tokens.text).size().x;

		// 1. the mode badge: the one accent block that is always there
		let select = !self.selection.is_empty();
		let badge = if select { "SELECT" } else { "NORMAL" };
		let width = painter.layout_no_wrap(badge.to_owned(), small.clone(), tokens.accent_ink).size().x + 12.0;
		let badge_rect = Rect::from_center_size(pos2(at + width * 0.5, middle), vec2(width, 18.0));
		painter.rect_filled(badge_rect, 4.0, if select { tokens.warn } else { tokens.accent });
		painter.text(badge_rect.center(), Align2::CENTER_CENTER, badge, small.clone(), tokens.accent_ink);
		at = badge_rect.max.x + 12.0;

		// 2. the cursor's permissions: three colours, the way the baseline reads them
		let perm = chrome::perm(self.selected_entry().and_then(|entry| entry.mode));
		for (i, letter) in perm.into_iter().enumerate() {
			let colour = match letter {
				b'r' => tokens.ok,
				b'w' => tokens.warn,
				b'x' => tokens.busy,
				_ => tokens.disabled,
			};
			painter.text(pos2(at + i as f32 * cell, middle), Align2::LEFT_CENTER, (letter as char).to_string(), small.clone(), colour);
		}
		at += 9.0 * cell + 12.0;

		// 3. what is busy: hidden entirely when nothing is
		if let Some(busy) = self.busy() {
			let label = busy.rate.clone().unwrap_or_else(|| busy.title.clone());
			let width = painter.layout_no_wrap(label.clone(), small.clone(), tokens.busy).size().x;
			let bar = Rect::from_min_size(pos2(at, middle - 3.0), vec2(80.0, 6.0));
			painter.rect_filled(bar, 3.0, tokens.mantle);
			let fill = busy.fraction.unwrap_or(0.45).clamp(0.0, 1.0);
			painter.rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * fill, 6.0)), 3.0, tokens.busy);
			painter.text(pos2(bar.max.x + 8.0, middle), Align2::LEFT_CENTER, label, small.clone(), tokens.busy);
			at = bar.max.x + 8.0 + width + 12.0;
		}

		// 4. where the cursor is: the position said as a fraction of the whole
		let len = self.tab().view.len();
		if len > 0 {
			let current = (self.cursor() + 1).to_string();
			let total = format!("/{len}");
			let width = painter.layout_no_wrap(current.clone(), small.clone(), tokens.text).size().x;
			painter.text(pos2(at, middle), Align2::LEFT_CENTER, current, small.clone(), tokens.text);
			painter.text(pos2(at + width, middle), Align2::LEFT_CENTER, total.clone(), small.clone(), tokens.subtext);
			at += width + painter.layout_no_wrap(total, small.clone(), tokens.subtext).size().x + 12.0;
		}

		// 5. what is held: the selection (▲ warn), a cut (● err) and a yank (◆ busy) — each
		// hidden while it is empty, and each in the colour the baseline gives it
		let counts = [
			(self.selection.len(), "▲", tokens.warn),
			(self.yank.as_ref().filter(|yank| yank.cut).map_or(0, |yank| yank.paths.len()), "●", tokens.err),
			(self.yank.as_ref().filter(|yank| !yank.cut).map_or(0, |yank| yank.paths.len()), "◆", tokens.busy),
		];
		for (count, mark, colour) in counts {
			if count == 0 {
				continue;
			}

			let text = format!("{mark}{count}");
			let width = painter.layout_no_wrap(text.clone(), small.clone(), colour).size().x;
			painter.text(pos2(at, middle), Align2::LEFT_CENTER, text, small.clone(), colour);
			at += width + 12.0;
		}

		// the right group: the view's own truth, said last — the filter's prefix, the sort's own
		// word and the arrow. The linemode cell stood at its end and is gone (P33⑭): the linemode
		// is what the rows themselves show, not a word that stands in the corner
		let right = right_group(self.sort, self.tab().symlinks, self.tab().filter.as_deref());
		let start = rect.max.x - 12.0 - painter.layout_no_wrap(right.clone(), small.clone(), tokens.subtext).size().x;

		// and, left of it, the index panel's own button (P33⑰): the mouse's door to the place `i`
		// opens — Action::Index's arm calls this same function, there is no second door — and while
		// the panel is the index the button wears the accent, a second press being the way out
		let open = self.index_open();
		let name = "索引";
		let name_width = painter.layout_no_wrap(name.to_owned(), small.clone(), tokens.subtext).size().x;
		let button = Rect::from_center_size(pos2(start - 8.0 - name_width * 0.5 - 6.0, middle), vec2(name_width + 12.0, 20.0));
		let response = ui.interact(button, egui::Id::unique("status-index"), Sense::click());
		if response.hovered() {
			painter.rect_filled(button, 4.0, tokens.cursor_bg.gamma_multiply(0.5));
		}
		painter.text(
			button.center(),
			Align2::CENTER_CENTER,
			name,
			small.clone(),
			if open {
				tokens.accent
			} else if response.hovered() {
				tokens.text
			} else {
				tokens.subtext
			},
		);
		if response.clicked() {
			if open {
				self.fuzzy_close();
			} else {
				self.fuzzy_index();
			}
		}
		self.status_index = Some(button);

		painter.text(pos2(start, middle), Align2::LEFT_CENTER, right, small.clone(), tokens.subtext);

		// the one separator: between the two big groups, and only when they both have room
		if start - 12.0 > at {
			painter.line_segment([ pos2(start - 10.0, rect.min.y + 6.0), pos2(start - 10.0, rect.max.y - 6.0) ], (1.0, tokens.border));
		}
	}

	/// What the status bar's busy group shows: the operations in flight, or the one thing that is
	/// ever busy here until they land — a directory being read past its moment.
	fn busy(&self) -> Option<Progress> {
		if let Some(job) = &self.job {
			return Some(Progress {
				title:    format!("{} … 已运行 {}s", job.command, job.started.elapsed().as_secs()),
				fraction: None,
				rate:     None,
			});
		}
		if self.trash_job.is_some() {
			return Some(Progress { title: "回收站…".to_owned(), fraction: None, rate: None });
		}
		if let Some(job) = &self.pack {
			let (done, total) = job.progress();
			return Some(Progress {
				title:    format!("压缩 → {}", job.name),
				fraction: (total > 0).then(|| done as f32 / total as f32),
				rate:     Some(format!("{done}/{total}")),
			});
		}
		if !self.tasks.is_empty() {
			return self.tasks.first().cloned();
		}

		let snapshot = self.entries();
		if snapshot.loading && snapshot.started.is_some_and(|at| at.elapsed() >= SKELETON_AFTER) {
			return Some(Progress { title: "读取目录…".to_owned(), fraction: None, rate: None });
		}

		None
	}

	fn save_columns(&mut self) {
		self.config.columns.parent = self.shares[0];
		self.config.columns.current = self.shares[1];
		self.config.columns.preview = self.shares[2];

		if let Err(e) = self.config.save() {
			eprintln!("crabhop: config not written: {e}");
		}
	}

	/// The in-place rename's field (§4.3): the cursor row's name becomes a real input, its border
/// says whether the name can land, and the caret sits where the seat the key asked for put it.
	fn draw_item_rename(&self, ui: &Ui, list: Rect, first: usize, fraction: f32, advance: f32, font: &FontId) {
		let Some(rename) = &self.item_rename else { return };
		let tokens = self.tokens;
		let cursor = self.cursor();
		if cursor < first {
			return;
		}

		let top = list.min.y + (cursor - first) as f32 * ROW_HEIGHT - fraction * ROW_HEIGHT;
		let row = Rect::from_min_size(pos2(list.min.x, top), vec2(list.width(), ROW_HEIGHT));
		if row.max.y < list.min.y || row.min.y > list.max.y {
			return;
		}

		let painter = ui.painter().clone();
		let facts = self.selected_entry().map_or_else(String::new, |entry| {
			let facts = entry_facts(&entry);

			crate::list::columns_text(self.columns, &facts)
		});
		let small = FontId::new(FONT_SMALL, FontFamily::Monospace);
		let facts_width = painter.layout_no_wrap(facts.clone(), small.clone(), tokens.subtext).size().x;
		let note = rename.note.map(|note| painter.layout_no_wrap(note.to_owned(), small.clone(), tokens.err).size().x);

		let field = Rect::from_min_max(
			pos2(row.min.x + ICON_BOX + 2.0 * advance, row.min.y + 4.0),
			pos2(row.max.x - advance - facts_width.max(note.unwrap_or(0.0)) - 8.0, row.max.y - 4.0),
		);
		if field.width() < 40.0 {
			return;
		}

		let edge = if rename.note.is_some() { tokens.err } else { tokens.accent };
		painter.rect_filled(field, 4.0, tokens.crust);
		painter.rect_stroke(field, 4.0, (1.0, edge), egui::StrokeKind::Inside);

		let text = rename.edit.text.clone();
		let clipped = painter.with_clip_rect(Rect::from_min_max(field.min + vec2(6.0, 0.0), field.max - vec2(6.0, 0.0)));
		clipped.text(pos2(field.min.x + 6.0, field.center().y), Align2::LEFT_CENTER, text.clone(), font.clone(), tokens.text);

		// the caret: the buffer's own prefix, measured in cells — the font is one width
		let prefix = &text[..rename.edit.cursor.min(text.len())];
		let caret = field.min.x + 6.0 + cells(prefix, advance);
		let caret_rect = Rect::from_min_max(pos2(caret, field.center().y - 9.0), pos2(caret + 2.0, field.center().y + 9.0));
		painter.rect_filled(caret_rect, 0.0, tokens.accent);
		anchor_ime(ui, field, caret_rect);

		// and what stands in the facts' place while the field is there: why the name cannot land.
		// The facts the rows drew are covered first — two strings right-aligned in one seat would
		// read as neither.
		if let Some(note) = rename.note {
			let cover = Rect::from_min_max(pos2(row.max.x - advance - facts_width - 4.0, row.min.y), pos2(row.max.x, row.max.y));
			painter.rect_filled(cover, 0.0, tokens.base);
			painter.text(pos2(row.max.x - advance, row.center().y), Align2::RIGHT_CENTER, note, small, tokens.err);
		}
	}

	/// The bulk editor (§4.1): the column of names, the plan against the disk, and a button that
	/// says what it will do in its own words.
	fn draw_bulk(&mut self, ui: &mut Ui, area: Rect, painter: &Painter, advance: f32, font: &FontId, small: &FontId) {
		let tokens = self.tokens;
		let panel = Rect::from_center_size(
			area.center(),
			vec2(640.0f32.min(area.width() - 32.0), 480.0f32.min(area.height() - 32.0)),
		);

		// the mask first, so the world behind is what is dark; a click on it is a cancel (§4)
		let mask = ui.interact(area, egui::Id::unique("bulk-mask"), Sense::click());
		let cancel = mask.clicked() && ui.ctx().input(|i| i.pointer.interact_pos()).is_none_or(|at| !panel.contains(at));

		painter.rect_filled(area, 0.0, Color32::from_black_alpha(150));
		chrome::shadow(painter, panel, 8.0);
		painter.rect_filled(panel, 8.0, tokens.raised);
		painter.rect_stroke(panel, 8.0, (1.0, tokens.border_strong), egui::StrokeKind::Inside);

		let title = FontId::new(20.0, FontFamily::Name("maple-bold".into()));
		painter.text(pos2(panel.min.x + 24.0, panel.min.y + 24.0), Align2::LEFT_CENTER, "批量创建", title, tokens.text);

		// the window is the input and nothing else (P33⑦): the live preview column that stood at
		// its right is gone — a person writes the names, and the footer's own count is the tally
		let body = Rect::from_min_max(pos2(panel.min.x + 24.0, panel.min.y + 54.0), pos2(panel.max.x - 24.0, panel.max.y - 56.0));
		painter.text(pos2(body.min.x, body.min.y), Align2::LEFT_TOP, "输入（一行一个）", small.clone(), tokens.subtext);

		let top = body.min.y + 22.0;
		let left = Rect::from_min_max(pos2(body.min.x, top), pos2(body.max.x, body.max.y));
		painter.rect_filled(left, 6.0, tokens.crust);

		let mut commit = false;
		if let Some(bulk) = &self.bulk {
			// the left column: the gutter, the lines, and the caret kept in view
			let line = LINE_HEIGHT;
			let visible = ((left.height() - 8.0) / line).floor().max(1.0) as usize;
			let lines: Vec<&str> = bulk.edit.text.split('\n').collect();
			let (at_line, _) = bulk.edit.place();
			let from = at_line.saturating_sub(visible.saturating_sub(1));

			let clipped = painter.with_clip_rect(left);
			for (i, text) in lines.iter().enumerate().skip(from).take(visible) {
				let y = left.min.y + 4.0 + (i - from) as f32 * line;
				clipped.text(pos2(left.min.x + 44.0, y), Align2::RIGHT_TOP, format!("{}", i + 1), small.clone(), tokens.disabled);
				clipped.text(pos2(left.min.x + 56.0, y), Align2::LEFT_TOP, *text, font.clone(), tokens.text);
			}

			if at_line < from + visible {
				let start = bulk.edit.text[..bulk.edit.cursor].rfind('\n').map_or(0, |i| i + 1);
				let y = left.min.y + 4.0 + (at_line - from) as f32 * line;
				let caret = left.min.x + 56.0 + cells(&bulk.edit.text[start..bulk.edit.cursor], advance);
				let caret_rect = Rect::from_min_size(pos2(caret, y), vec2(2.0, line - 4.0));
				clipped.rect_filled(caret_rect, 0.0, tokens.accent);
				anchor_ime(ui, left, caret_rect);
			}

			// the footer: the hint clipped to the space the button leaves, and the button itself, which
			// says what will happen in its own words
			let (create, skip) = bulk::tally(&bulk.lines);
			let label = if skip == 0 { format!("创建 {create} 项") } else { format!("跳过 {skip} 个冲突，创建 {create} 项") };
			let width = painter.layout_no_wrap(label.clone(), small.clone(), tokens.accent_ink).size().x + 28.0;
			let button = Rect::from_min_size(pos2(panel.max.x - 24.0 - width, panel.max.y - 46.0), vec2(width, 30.0));

			let clipped = painter.with_clip_rect(Rect::from_min_max(
				pos2(panel.min.x + 24.0, panel.max.y - 46.0),
				pos2(button.min.x - 12.0, panel.max.y - 8.0),
			));
			clipped.text(
				pos2(panel.min.x + 24.0, panel.max.y - 31.0),
				Align2::LEFT_CENTER,
				"以 / 结尾创建目录；相对路径自动建父目录 · Ctrl+Enter 提交 · Esc 取消",
				small.clone(),
				tokens.disabled,
			);

			let enabled = create > 0;
			painter.rect_filled(button, 6.0, if enabled { tokens.accent } else { tokens.crust });
			painter.text(
				button.center(),
				Align2::CENTER_CENTER,
				label,
				small.clone(),
				if enabled { tokens.accent_ink } else { tokens.disabled },
			);

			if enabled && ui.interact(button, egui::Id::unique("bulk-create"), Sense::click()).clicked() {
				commit = true;
			}
		}

		if commit {
			self.bulk_commit();
		} else if cancel {
			self.bulk = None;
		}
		ui.ctx().request_repaint();
	}

	/// The output sheet (§4.8): what a command left, at the window's bottom above the status bar —
/// its height by its own content, at most 40% of the window.
	fn draw_output(&mut self, ui: &mut Ui, area: Rect, small: &FontId) {
		let Some(output) = &self.output else { return };
		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let rect = output_rect(area, output.lines.len());

		painter.rect_filled(rect, 0.0, tokens.crust);
		painter.line_segment([ rect.left_top(), rect.right_top() ], (1.0, tokens.border_strong));

		let code = match output.code {
			Some(0) => "退出码 0".to_owned(),
			Some(code) => format!("退出码 {code}"),
			None => "中断".to_owned(),
		};
		painter.text(
			pos2(rect.min.x + 12.0, rect.min.y + 17.0),
			Align2::LEFT_CENTER,
			format!("{}  ·  {code}", output.title),
			small.clone(),
			if output.code == Some(0) { tokens.subtext } else { tokens.err },
		);
		painter.text(
			pos2(rect.max.x - 12.0, rect.min.y + 17.0),
			Align2::RIGHT_CENTER,
			"Esc 关闭 · 滚轮翻动",
			small.clone(),
			tokens.disabled,
		);

		let line = 20.0;
		let rows = ((rect.height() - 34.0) / line).floor().max(1.0) as usize;
		let from = output.from.min(output.lines.len().saturating_sub(rows));
		let clipped = painter.with_clip_rect(rect);
		for (i, (text, err)) in output.lines.iter().skip(from).take(rows).enumerate() {
			clipped.text(
				pos2(rect.min.x + 12.0, rect.min.y + 34.0 + i as f32 * line),
				Align2::LEFT_TOP,
				crate::list::clipped(text, ((rect.width() - 24.0) / 8.0).floor() as u32),
				small.clone(),
				if *err { tokens.err } else { tokens.text },
			);
		}
	}

	/// The bottom command line (§4.8): the prefix in accent, the line at the body face, the caret
	/// where the next character lands, and the completion list above the bar.
	/// The filter field: a one-line strip above the status bar, the pattern and its caret — the
	/// same shape the command line wears, because they are the same kind of thing.
	fn draw_filter(&mut self, ui: &mut Ui, area: Rect, advance: f32, font: &FontId, small: &FontId) {
		let Some(edit) = &self.filter else { return };

		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let rect = Rect::from_min_size(pos2(area.min.x, area.max.y - STATUS_HEIGHT - 32.0), vec2(area.width(), 32.0));
		painter.rect_filled(rect, 0.0, tokens.crust);
		painter.line_segment([ rect.left_top(), rect.right_top() ], (1.0, tokens.border_strong));

		let prefix = "筛选 ";
		let pw = prefix.chars().map(|c| crate::list::cell_width(c) as f32).sum::<f32>() * advance;
		painter.text(rect.left_center() + vec2(12.0, 0.0), Align2::LEFT_CENTER, prefix, font.clone(), tokens.accent);

		let shown = crate::list::clipped(&edit.text, ((rect.width() - pw - 32.0) / advance).max(1.0) as u32);
		let x = rect.min.x + 12.0 + pw;
		painter.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, shown, font.clone(), tokens.text);

		// the caret, one cell of a hint after the text — and the IME's anchor at the field's own
		// foot, every frame, blink or no blink (P33⑤)
		let caret = x + cells(shown, advance);
		let caret_rect = Rect::from_min_size(pos2(caret, rect.center().y - 8.0), vec2(2.0, 16.0));
		if ui.ctx().input(|i| i.time).fract() < 0.5 {
			painter.rect_filled(caret_rect, 0.0, tokens.accent);
		}
		anchor_ime(ui, rect, caret_rect);

		painter.text(
			rect.right_center() + vec2(-12.0, 0.0),
			Align2::RIGHT_CENTER,
			"Enter 应用 · Esc 清除",
			small.clone(),
			tokens.disabled,
		);
	}

	fn draw_cmdline(&mut self, ui: &mut Ui, area: Rect, advance: f32, font: &FontId, small: &FontId) {
		let Some(cmdline) = &self.cmdline else { return };
		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let bar = Rect::from_min_max(pos2(area.min.x, area.max.y - STATUS_HEIGHT - 32.0), pos2(area.max.x, area.max.y - STATUS_HEIGHT));
		painter.rect_filled(bar, 0.0, tokens.crust);
		painter.line_segment([ bar.left_top(), bar.right_top() ], (1.0, tokens.border_strong));

		let prefix = if cmdline.block { ":" } else { ";" };
		painter.text(pos2(bar.min.x + 12.0, bar.center().y), Align2::LEFT_CENTER, prefix, font.clone(), tokens.accent);
		let text_at = bar.min.x + 12.0 + cells(prefix, advance) + 8.0;
		painter.text(pos2(text_at, bar.center().y), Align2::LEFT_CENTER, &cmdline.edit.text, font.clone(), tokens.text);

		let caret = text_at + cells(&cmdline.edit.text[..cmdline.edit.cursor], advance);
		let caret_rect = Rect::from_min_max(pos2(caret, bar.center().y - 9.0), pos2(caret + 2.0, bar.center().y + 9.0));
		painter.rect_filled(caret_rect, 0.0, tokens.accent);
		anchor_ime(ui, bar, caret_rect);

		// the completion: above the bar, at most eight of them, the standing one marked
		if let Some(comp) = &cmdline.comp {
			let rows = comp.items.len().min(8);
			let height = rows as f32 * 28.0;
			let rect = Rect::from_min_size(
				pos2(bar.min.x + 12.0, bar.min.y - height - 6.0),
				vec2(360.0f32.min(area.width() - 24.0), height),
			);
			painter.rect_filled(rect, 6.0, tokens.raised);
			painter.rect_stroke(rect, 6.0, (1.0, tokens.border_strong), egui::StrokeKind::Inside);

			for (i, item) in comp.items.iter().enumerate().take(rows) {
				let row = Rect::from_min_size(pos2(rect.min.x, rect.min.y + i as f32 * 28.0), vec2(rect.width(), 28.0));
				if i == comp.at {
					painter.rect_filled(row, 0.0, tokens.cursor_bg);
					painter.rect_filled(Rect::from_min_size(row.min, vec2(2.0, row.height())), 0.0, tokens.accent);
				}
				painter.text(
					pos2(row.min.x + 10.0, row.center().y),
					Align2::LEFT_CENTER,
					crate::list::clipped(item, 40),
					small.clone(),
					tokens.text,
				);
			}
		}
	}

	/// open-with (§4.4): a 480px modal with the field, and the history and `$PATH` beneath it.
	fn draw_openwith(&mut self, ui: &mut Ui, area: Rect, advance: f32, font: &FontId, small: &FontId) {
		let Some(open) = &self.openwith else { return };
		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());

		let panel = Rect::from_center_size(area.center(), vec2(480.0f32.min(area.width() - 32.0), 264.0));
		let mask = ui.interact(area, egui::Id::unique("open-mask"), Sense::click());
		let cancel = mask.clicked() && ui.ctx().input(|i| i.pointer.interact_pos()).is_none_or(|at| !panel.contains(at));

		painter.rect_filled(area, 0.0, Color32::from_black_alpha(150));
		chrome::shadow(&painter, panel, 8.0);
		painter.rect_filled(panel, 8.0, tokens.raised);
		painter.rect_stroke(panel, 8.0, (1.0, tokens.border_strong), egui::StrokeKind::Inside);

		let title = FontId::new(20.0, FontFamily::Name("maple-bold".into()));
		painter.text(pos2(panel.min.x + 24.0, panel.min.y + 26.0), Align2::LEFT_CENTER, "打开方式", title, tokens.text);
		painter.text(
			pos2(panel.min.x + 24.0, panel.min.y + 54.0),
			Align2::LEFT_CENTER,
			format!("用程序打开「{}」：", open.name),
			small.clone(),
			tokens.subtext,
		);

		let field = Rect::from_min_size(pos2(panel.min.x + 24.0, panel.min.y + 72.0), vec2(panel.width() - 48.0, 32.0));
		painter.rect_filled(field, 6.0, tokens.crust);
		painter.rect_stroke(field, 6.0, (1.0, tokens.accent), egui::StrokeKind::Inside);
		painter.text(pos2(field.min.x + 8.0, field.center().y), Align2::LEFT_CENTER, &open.edit.text, font.clone(), tokens.text);

		let caret = field.min.x + 8.0 + cells(&open.edit.text[..open.edit.cursor], advance);
		let caret_rect = Rect::from_min_max(pos2(caret, field.center().y - 9.0), pos2(caret + 2.0, field.center().y + 9.0));
		painter.rect_filled(caret_rect, 0.0, tokens.accent);
		anchor_ime(ui, field, caret_rect);

		// the completion: right below the field, at most eight, the history's own read at the right
		if let Some(comp) = &open.comp {
			let rows = comp.items.len().min(8);
			let rect = Rect::from_min_size(pos2(field.min.x, field.max.y + 6.0), vec2(field.width(), rows as f32 * 28.0));
			chrome::shadow(&painter, rect, 6.0);
			painter.rect_filled(rect, 6.0, tokens.base);
			painter.rect_stroke(rect, 6.0, (1.0, tokens.border_strong), egui::StrokeKind::Inside);

			for (i, item) in comp.items.iter().enumerate().take(rows) {
				let row = Rect::from_min_size(pos2(rect.min.x, rect.min.y + i as f32 * 28.0), vec2(rect.width(), 28.0));
				if i == comp.at {
					painter.rect_filled(row, 0.0, tokens.cursor_bg);
					painter.rect_filled(Rect::from_min_size(row.min, vec2(2.0, row.height())), 0.0, tokens.accent);
				}

				painter.text(
					pos2(row.min.x + 10.0, row.center().y),
					Align2::LEFT_CENTER,
					crate::list::clipped(item, 30),
					font.clone(),
					tokens.text,
				);
				if let Some(entry) = self.open_history.entries.iter().find(|entry| entry.command == *item) {
					painter.text(
						pos2(row.max.x - 10.0, row.center().y),
						Align2::RIGHT_CENTER,
						open::relative(entry.at, now),
						small.clone(),
						tokens.disabled,
					);
				}
			}
		}

		painter.text(
			pos2(panel.min.x + 24.0, panel.max.y - 20.0),
			Align2::LEFT_CENTER,
			"Tab 补全 · ↑↓ 选择/历史 · Enter 提交 · Esc 取消",
			small.clone(),
			tokens.disabled,
		);

		if cancel {
			self.openwith = None;
		}
		ui.ctx().request_repaint();
	}

	/// The fuzzy panel (§5.3): non-modal, anchored at the top fifth, filtered at each keystroke
	/// with the matched characters in accent. Behind it the columns stay as they are — the panel
	/// is a fast lane, not a room.
	fn draw_fuzzy(&mut self, ui: &mut Ui, area: Rect, font: &FontId, small: &FontId) {
		let Some(fuzzy) = &self.fuzzy else { return };
		// a click on the query line steps into the box (P33⑬) — applied after the drawing, once the
		// panel's own borrow has said its last
		let mut clicked_query = false;
		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let advance = painter.layout_no_wrap("0".to_owned(), font.clone(), tokens.text).size().x;

		// the user's own micro-adjustment (P33's interlude, their words: 「再调宽一点点、一丁点、后面一个字
		// 重合了」): the index panel grew 640 → 720 so a long path and the right-hand column stop
		// overlapping by a character. The find panels' 860 is untouched.
		let width: f32 = if matches!(fuzzy.mode, FuzzyMode::FindNames | FuzzyMode::FindText) { 860.0 } else { 720.0 };
		let width = width.min(area.width() - 80.0);
		let rows = fuzzy.hits.len().min(12);
		// the index panel's zone headers take a row of their own, and their height with them
		let headers = if fuzzy.mode == FuzzyMode::Index {
			(0..rows)
				.filter(|i| {
					let zone = fuzzy.hits.get(*i).and_then(|hit| fuzzy.items.get(hit.at)).map_or("", |item| item.zone);
					let before = if *i == 0 {
						""
					} else {
						fuzzy.hits.get(i - 1).and_then(|hit| fuzzy.items.get(hit.at)).map_or("", |item| item.zone)
					};
					zone != before && !zone.is_empty()
				})
				.count() as f32
		} else {
			0.0
		};
		// two panels have a second footer line to pay for (P24's index, P21's device panel)
		let extra_line = matches!(fuzzy.mode, FuzzyMode::Devices { .. });
		let height = 44.0 + rows as f32 * 32.0 + headers * 18.0 + 26.0 + if extra_line { 18.0 } else { 0.0 };
		let panel = Rect::from_min_size(pos2(area.center().x - width * 0.5, area.min.y + area.height() * 0.2), vec2(width, height));

		// the panel swallows clicks so the list behind it does not act under a live panel — and the
		// query line's own click steps into the box (P33⑬), the way the help panel's box does
		let _ = ui.interact(panel, egui::Id::unique("fuzzy-panel"), Sense::click());
		if !fuzzy.focus.inside && let Some(true) = Some(ui.interact(
			Rect::from_min_size(pos2(panel.min.x, panel.min.y), vec2(panel.width(), 44.0)),
			egui::Id::unique("fuzzy-query-box"),
			Sense::click(),
		).clicked()) {
			clicked_query = true;
		}

		chrome::shadow(&painter, panel, 8.0);
		painter.rect_filled(panel, 8.0, tokens.raised);
		painter.rect_stroke(panel, 8.0, (1.0, tokens.border_strong), egui::StrokeKind::Inside);

		// the query line: 20px, with the caret where the next character lands
		let query_at = pos2(panel.min.x + 16.0, panel.min.y + 22.0);
		painter.text(query_at, Align2::LEFT_CENTER, "\u{f002}", small.clone(), tokens.disabled);
		let text_at = query_at.x + cells("\u{f002} ", advance * 1.6);
		let query_font = FontId::new(20.0, FontFamily::Monospace);
		let caret_at = |painter: &Painter| {
			text_at
				+ fuzzy
					.query
					.text
					.get(..fuzzy.query.cursor)
					.map_or(0.0, |prefix| painter.layout_no_wrap(prefix.to_owned(), query_font.clone(), tokens.text).size().x)
		};

		// a long query scrolls the box rather than running out of it (P27's third item): the text
		// and the caret both stand `shift` to the left once the caret has passed the right edge,
		// and the box's own rectangle is what clips them
		let room = panel.max.x - 16.0;
		let shift = (caret_at(&painter) - room).max(0.0);
		let boxed = painter
			.with_clip_rect(Rect::from_min_max(pos2(query_at.x, panel.min.y), pos2(panel.max.x, panel.min.y + 44.0)));
		boxed.text(pos2(text_at - shift, query_at.y), Align2::LEFT_CENTER, &fuzzy.query.text, query_font.clone(), tokens.text);

		// the caret lives only while the box holds the keyboard, and the line under the query says
		// which half has it (P33⑬): accent inside, border on the list
		if fuzzy.focus.inside {
			let caret = caret_at(&painter) - shift;
			let caret_rect = Rect::from_min_max(pos2(caret, query_at.y - 11.0), pos2(caret + 2.0, query_at.y + 11.0));
			boxed.rect_filled(caret_rect, 0.0, tokens.accent);
			anchor_ime(ui, Rect::from_min_max(pos2(query_at.x, panel.min.y), pos2(panel.max.x, panel.min.y + 44.0)), caret_rect);
		}
		painter.line_segment(
			[ pos2(panel.min.x + 12.0, panel.min.y + 44.0), pos2(panel.max.x - 12.0, panel.min.y + 44.0) ],
			(1.0, if fuzzy.focus.inside { tokens.accent } else { tokens.border }),
		);

		// the rows: the standing one marked, the match in accent, the score at the right
		let clipped = painter.with_clip_rect(panel);
		let mut at_y = panel.min.y + 44.0;
		// the mouse's own way in (P21): a click seats the standing row, a double-click is Enter
		let (mut seat, mut run) = (None, false);
		for (i, hit) in fuzzy.hits.iter().take(rows).enumerate() {
			let Some(item) = fuzzy.items.get(hit.at) else { continue };

			// the zone's own header where the grouping changes (P14): a quiet 13px line
			let before = if i == 0 {
				""
			} else {
				fuzzy.hits.get(i - 1).and_then(|hit| fuzzy.items.get(hit.at)).map_or("", |item| item.zone)
			};
			if item.zone != before && !item.zone.is_empty() {
				clipped.text(
					pos2(panel.min.x + 16.0, at_y + 9.0),
					Align2::LEFT_CENTER,
					item.zone,
					small.clone(),
					tokens.disabled,
				);
				at_y += 18.0;
			}

			let row = Rect::from_min_size(pos2(panel.min.x, at_y), vec2(panel.width(), 32.0));
			let response = ui.interact(row, egui::Id::unique(("fuzzy-row", i)), Sense::click());
			if response.double_clicked() {
				run = true;
			} else if response.clicked() {
				seat = Some(i);
			}
			at_y += 32.0;

			if i == fuzzy.at {
				clipped.rect_filled(row, 0.0, tokens.cursor_bg);
				clipped.rect_filled(Rect::from_min_size(row.min, vec2(2.0, row.height())), 0.0, tokens.accent);
			}

			let budget = ((row.width() - 90.0) / advance).floor().max(1.0) as usize;
			let base = if item.broken { tokens.err } else { tokens.subtext };
			text_hits(&clipped, pos2(row.min.x + 12.0, row.center().y), &item.text, &hit.positions, budget, font, advance, base, tokens.accent);

			let right = item.right.clone().unwrap_or_else(|| format!("{:.2}", hit.score / 1000.0));
			clipped.text(pos2(row.max.x - 12.0, row.center().y), Align2::RIGHT_CENTER, right, small.clone(), tokens.disabled);
		}

		// the footer: what Enter will do, and where the panel already is
		let (what, hint) = match fuzzy.mode {
			FuzzyMode::Index => ("跳转", INDEX_HINT),
			FuzzyMode::Devices { unmount, .. } => {
				if unmount {
					("卸载", DEVICE_HINT_UNMOUNT)
				} else {
					("挂载", DEVICE_HINT)
				}
			}
			FuzzyMode::FindNames | FuzzyMode::FindText => ("跳转", "↑↓ 选择 · Enter 跳转 · Esc 关闭"),
		};
		let _ = what;
		let notice = fuzzy.notice.as_ref().filter(|(_, at)| at.elapsed() < NOTICE_ALIVE);
		if let Some((_, at)) = notice {
			// the message has its own clock: the panel has to paint when it runs out
			ui.ctx().request_repaint_after(NOTICE_ALIVE - at.elapsed());
		}

		let foot = if let Some((words, _)) = notice {
			words.clone()
		} else if fuzzy.loading {
			format!("{hint} · 搜索中…")
		} else if matches!(fuzzy.mode, FuzzyMode::FindNames | FuzzyMode::FindText) && fuzzy.query.text.trim().is_empty() {
			"输入以搜索 · Esc 关闭".to_owned()
		} else {
			hint.to_owned()
		};
		painter.text(
			pos2(panel.min.x + 12.0, panel.max.y - 13.0),
			Align2::LEFT_CENTER,
			foot,
			small.clone(),
			if notice.is_some() { tokens.err } else { tokens.disabled },
		);
		if extra_line {
			// the device panel lists its other keys
			painter.text(pos2(panel.min.x + 12.0, panel.max.y - 31.0), Align2::LEFT_CENTER, DEVICE_HINT_URI, small.clone(), tokens.disabled);
		}
		painter.text(
			pos2(panel.max.x - 12.0, panel.max.y - 13.0),
			Align2::RIGHT_CENTER,
			format!("{} 命中", fuzzy.hits.len()),
			small.clone(),
			tokens.disabled,
		);

		// what the mouse decided: the click seats the standing row, the double-click is Enter
		if let Some(at) = seat
			&& let Some(fuzzy) = &mut self.fuzzy
		{
			fuzzy.at = at.min(fuzzy.hits.len().saturating_sub(1));
		}
		if run {
			self.fuzzy_enter();
		}
		if clicked_query
			&& let Some(fuzzy) = &mut self.fuzzy
		{
			fuzzy.focus.enter();
		}
	}

	/// The dangerous confirmation (§4.5): `err` words, the items it is about, the focus on cancel,
	/// and y/n as the keyboard's own answers.
	fn draw_confirm(&mut self, ui: &mut Ui, area: Rect, small: &FontId) {
		let Some(confirm) = &self.confirm else { return };
		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let names = confirm.names.len().min(5);
		let panel = Rect::from_center_size(area.center(), vec2(440.0f32.min(area.width() - 32.0), 140.0 + names as f32 * 20.0));

		painter.rect_filled(area, 0.0, Color32::from_black_alpha(150));
		chrome::shadow(&painter, panel, 8.0);
		painter.rect_filled(panel, 8.0, tokens.raised);
		painter.rect_stroke(panel, 8.0, (1.0, tokens.border_strong), egui::StrokeKind::Inside);

		painter.text(pos2(panel.min.x + 20.0, panel.min.y + 30.0), Align2::LEFT_CENTER, "⚠", FontId::new(24.0, FontFamily::Monospace), tokens.err);
		painter.text(pos2(panel.min.x + 56.0, panel.min.y + 30.0), Align2::LEFT_CENTER, confirm.text.clone(), small.clone(), tokens.text);

		for (i, name) in confirm.names.iter().take(5).enumerate() {
			painter.text(
				pos2(panel.min.x + 56.0, panel.min.y + 56.0 + i as f32 * 20.0),
				Align2::LEFT_CENTER,
				crate::list::clipped(name, 40),
				small.clone(),
				tokens.subtext,
			);
		}
		if confirm.names.len() > 5 {
			painter.text(
				pos2(panel.min.x + 56.0, panel.min.y + 56.0 + names as f32 * 20.0),
				Align2::LEFT_CENTER,
				format!("…还有 {} 项", confirm.names.len() - 5),
				small.clone(),
				tokens.disabled,
			);
		}

		painter.text(
			pos2(panel.min.x + 56.0, panel.min.y + 64.0 + names as f32 * 20.0),
			Align2::LEFT_CENTER,
			"此操作不可撤销。",
			small.clone(),
			tokens.err,
		);

		let yes = painter.layout_no_wrap("确认删除".to_owned(), small.clone(), tokens.accent_ink).size().x + 28.0;
		let no = painter.layout_no_wrap("取消".to_owned(), small.clone(), tokens.text).size().x + 28.0;
		let yes_rect = Rect::from_min_size(pos2(panel.max.x - 20.0 - yes, panel.max.y - 48.0), vec2(yes, 30.0));
		let no_rect = Rect::from_min_size(pos2(yes_rect.min.x - 10.0 - no, panel.max.y - 48.0), vec2(no, 30.0));

		painter.rect_filled(no_rect, 6.0, tokens.crust);
		painter.rect_stroke(no_rect, 6.0, (1.0, tokens.accent), egui::StrokeKind::Inside);
		painter.text(no_rect.center(), Align2::CENTER_CENTER, "取消", small.clone(), tokens.text);

		painter.rect_filled(yes_rect, 6.0, tokens.err);
		painter.text(yes_rect.center(), Align2::CENTER_CENTER, "确认删除", small.clone(), tokens.accent_ink);

		painter.text(
			pos2(panel.min.x + 20.0, panel.max.y - 33.0),
			Align2::LEFT_CENTER,
			"y 确认 · n 取消",
			small.clone(),
			tokens.disabled,
		);

		// the buttons answer too: the keyboard is quicker, but the mouse is allowed
		let mut yes = false;
		let mut no = false;
		if ui.interact(yes_rect, egui::Id::unique("confirm-yes"), Sense::click()).clicked() {
			yes = true;
		}
		if ui.interact(no_rect, egui::Id::unique("confirm-no"), Sense::click()).clicked() {
			no = true;
		}

		if yes {
			self.confirm_yes();
		} else if no {
			self.confirm = None;
		}
		ui.ctx().request_repaint();
	}

	/// The bin's own view (the `R b` key): the system's trash read back — the name, where it came
	/// from, when it went — with restore, purge and empty on the keyboard.
	/// The bin as the middle column's own view (P19): the icon, the name, where it came from and when
	/// it went, with the list's cursor, wheel, click and scrolling — no popup, no dimmed backdrop.
	fn draw_trash_list(&mut self, ui: &mut Ui, column: Rect, font: &FontId, advance: f32) {
		let tokens = self.tokens;
		let small = FontId::new(FONT_SMALL, FontFamily::Monospace);
		let painter = ui.painter().clone();
		let rows_visible = (column.height() / ROW_HEIGHT).floor().max(1.0) as usize;

		let (loading, empty, len, cursor, offset) = {
			let view = self.bin_mut().expect("checked by the caller");
			let len = view.items.len();
			view.scroll.rows = rows_visible;
			view.scroll.len = len;
			view.scroll.clamp();

			(view.loading, len == 0, len, view.cursor, view.scroll.offset)
		};

		let first = offset.floor() as usize;
		let fraction = offset - first as f32;
		let list = if len <= rows_visible && len > 0 {
			Rect::from_min_size(pos2(column.min.x, column.min.y + 8.0), column.size())
		} else {
			column
		};
		let clipped = painter.with_clip_rect(list);
		let hover = hover_at(self.hover, list, ROW_HEIGHT, first);

		if loading {
			// the bin's own read: the same skeleton the directory's list wears, for the same reason
			let rows = chrome::skeleton_rows(None, rows_visible);
			for row in 0..rows {
				let top = column.min.y + row as f32 * ROW_HEIGHT;
				let icon = Rect::from_center_size(pos2(column.min.x + 8.0 + ICON_BOX * 0.5, top + ROW_HEIGHT * 0.5), Vec2::splat(ICON_BOX));
				painter.rect_filled(icon, 4.0, tokens.raised);

				let width = chrome::skeleton_bar(row) * (column.width() - ICON_BOX - 48.0);
				let bar = Rect::from_min_size(pos2(column.min.x + ICON_BOX + 20.0, top + ROW_HEIGHT * 0.5 - 5.0), vec2(width, 10.0));
				painter.rect_filled(bar, 5.0, tokens.raised);
			}
		} else if empty {
			painter.text(column.center(), Align2::CENTER_CENTER, "回收站是空的", font.clone(), tokens.subtext);
		}

		let items = self.bin().expect("up");
		for (i, item) in items.items.iter().enumerate().skip(first).take(rows_visible + 1) {
			// the row is placed by its place in the viewport, not by its place in the bin (P33③):
			// the two only agree at the top, and with the absolute index every scrolled row landed
			// `first` rows too low — the viewport's top row bare, and a deep scroll off the column
			// altogether. The click loop below has always spelled it the right way.
			let at = i - first;
			let top = list.min.y + (at as f32 - fraction) * ROW_HEIGHT;
			let row = Rect::from_min_size(pos2(list.min.x, top), vec2(list.width(), ROW_HEIGHT));
			if row.max.y < list.min.y || row.min.y > list.max.y {
				continue;
			}

			if i == cursor {
				clipped.rect_filled(row, 0.0, tokens.cursor_bg);
				clipped.rect_filled(Rect::from_min_size(row.min, vec2(2.0, row.height())), 0.0, tokens.accent);
			} else if Some(i) == hover {
				clipped.rect_filled(row, 0.0, tokens.cursor_bg.gamma_multiply(0.5));
			}

			let name = item.name.to_string_lossy();
			let (glyph, colour) = list::icon_for(&name, items.dirs.get(i).copied().unwrap_or(false), false, &tokens);
			clipped.text(
				pos2(row.min.x + advance + ICON_BOX * 0.5, row.center().y),
				Align2::CENTER_CENTER,
				glyph,
				font.clone(),
				colour,
			);
			clipped.text(
				pos2(row.min.x + advance + ICON_BOX + 8.0, row.center().y),
				Align2::LEFT_CENTER,
				crate::list::clipped(&name, 30),
				font.clone(),
				tokens.text,
			);

			// where it came from, and when it went — the two things a bin's row owes a person
			let parent = frecency::display(&item.original_parent);
			clipped.text(
				pos2(row.min.x + row.width() * 0.46, row.center().y),
				Align2::LEFT_CENTER,
				crate::list::clipped(&parent, 26),
				small.clone(),
				tokens.disabled,
			);
			let when = match item.time_deleted {
				at if at > 0 => crate::list::when(Some(std::time::UNIX_EPOCH + Duration::from_secs(at as u64))),
				_ => "-".to_owned(),
			};
			clipped.text(pos2(row.max.x - 12.0, row.center().y), Align2::RIGHT_CENTER, when, small.clone(), tokens.disabled);
		}

		// the mouse's own way in: a click seats the cursor on the row it landed on, and a click on
		// the blank decides nothing — the list's own contract, in the bin's own list
		if !loading && !empty {
			let mut clicked = None;
			for i in 0..rows_visible + 1 {
				let Some(position) = clicked_row(first, i, len) else { break };

				let top = list.min.y + (i as f32 - fraction) * ROW_HEIGHT;
				let row = Rect::from_min_size(pos2(list.min.x, top), vec2(list.width(), ROW_HEIGHT));
				if row.max.y < list.min.y || row.min.y > list.max.y {
					continue;
				}

				if ui.interact(row, egui::Id::unique(("bin-row", position)), Sense::click()).clicked() {
					clicked = Some(position);
				}
			}

			if let Some(position) = clicked {
				self.bin_seat(position);
			}

			if len > rows_visible
				&& let Some(requested) = self.overlay_bar(ui, column, 0, len as f32, rows_visible as f32, offset)
			{
				if let Some(view) = self.bin_mut() {
					view.scroll.jump(requested);
				}
				self.bars_moved[0] = Some(Instant::now());
			}
		}

		// how many are in there: the count the popup's header used to say, at the column's foot
		if !loading && !empty {
			let label = format!("{len} 项");
			let width = painter.layout_no_wrap(label.clone(), small.clone(), tokens.subtext).size().x + 16.0;
			let badge = Rect::from_min_size(pos2(column.center().x - width * 0.5, column.max.y - 34.0), vec2(width, 22.0));
			painter.rect_filled(badge, 4.0, tokens.raised);
			painter.text(badge.center(), Align2::CENTER_CENTER, label, small.clone(), tokens.subtext);
		}
	}

	/// The bottom hint line: what the keys do, and only while the bin is the tab's own view (P19/P27)
	/// — the one place where the list's keys mean the bin's own things.
	fn draw_trash_hint(&mut self, ui: &mut Ui, area: Rect, small: &FontId) {
		if !self.in_bin() {
			return;
		}

		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let rect = Rect::from_min_size(pos2(area.min.x, area.max.y - STATUS_HEIGHT - TRASH_HINT), vec2(area.width(), TRASH_HINT));
		painter.rect_filled(rect, 0.0, tokens.base);
		painter.line_segment([ rect.left_top(), rect.right_top() ], (1.0, tokens.border));

		painter.text(pos2(rect.min.x + 12.0, rect.center().y), Align2::LEFT_CENTER, "回收站", small.clone(), tokens.subtext);
		painter.text(
			pos2(rect.min.x + 12.0 + cells("回收站  ", FONT_SMALL * 0.62), rect.center().y),
			Align2::LEFT_CENTER,
			"Enter 恢复 · D 永久删除 · C 清空 · 导航即离开本页",
			small.clone(),
			tokens.subtext,
		);
	}

	/// The one-line prompt (§ `c z`): a small modal with the title, the field and its caret.
	fn draw_prompt(&mut self, ui: &mut Ui, area: Rect, advance: f32, font: &FontId, small: &FontId) {
		let Some(prompt) = &self.prompt else { return };
		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let panel = Rect::from_center_size(area.center(), vec2(480.0f32.min(area.width() - 32.0), 150.0));

		painter.rect_filled(area, 0.0, Color32::from_black_alpha(150));
		chrome::shadow(&painter, panel, 8.0);
		painter.rect_filled(panel, 8.0, tokens.raised);
		painter.rect_stroke(panel, 8.0, (1.0, tokens.border_strong), egui::StrokeKind::Inside);

		painter.text(pos2(panel.min.x + 20.0, panel.min.y + 28.0), Align2::LEFT_CENTER, prompt.title.clone(), small.clone(), tokens.subtext);

		let field = Rect::from_min_size(pos2(panel.min.x + 20.0, panel.min.y + 52.0), vec2(panel.width() - 40.0, 32.0));
		painter.rect_filled(field, 6.0, tokens.crust);
		painter.rect_stroke(field, 6.0, (1.0, tokens.accent), egui::StrokeKind::Inside);
		painter.text(pos2(field.min.x + 8.0, field.center().y), Align2::LEFT_CENTER, &prompt.edit.text, font.clone(), tokens.text);

		let caret = field.min.x + 8.0 + cells(&prompt.edit.text[..prompt.edit.cursor], advance);
		let caret_rect = Rect::from_min_max(pos2(caret, field.center().y - 9.0), pos2(caret + 2.0, field.center().y + 9.0));
		painter.rect_filled(caret_rect, 0.0, tokens.accent);
		anchor_ime(ui, field, caret_rect);

		painter.text(
			pos2(panel.min.x + 20.0, panel.max.y - 22.0),
			Align2::LEFT_CENTER,
			"Enter 开始 · Esc 取消",
			small.clone(),
			tokens.disabled,
		);
	}

	/// The toasts (§5.2): a corner note, sliding in from the right, thinning out after its stay —
	/// and an undo link when the thing that happened can be taken back.
	fn draw_toasts(&mut self, ui: &mut Ui, area: Rect, small: &FontId) {
		if self.toasts.is_empty() {
			return;
		}

		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let now = Instant::now();
		let len = self.toasts.len();
		let (width, height) = (300.0f32, 40.0f32);
		let cell = painter.layout_no_wrap("0".to_owned(), small.clone(), tokens.text).size().x;
		let (mut undo_at, mut dismiss_at) = (None, None);

		for (i, toast) in self.toasts.items().enumerate() {
			let slot = (len - 1 - i) as f32;
			let age = toast.age(now);
			let alpha = toast::alpha(age);
			let rect = Rect::from_min_size(
				pos2(area.max.x - 12.0 - width + toast::slide(age), area.max.y - STATUS_HEIGHT - 12.0 - height - slot * (height + 8.0)),
				vec2(width, height),
			);
			let colour = match toast.kind {
				toast::Kind::Ok => tokens.ok,
				toast::Kind::Warn => tokens.warn,
				toast::Kind::Err => tokens.err,
			};

			// a click on the note takes it away (P30): registered before the undo link, so the
			// link keeps the last word on its own little rect
			if ui.interact(rect, egui::Id::unique(("toast", i)), Sense::click()).clicked() {
				dismiss_at = Some(i);
			}

			painter.rect_filled(rect, 8.0, tokens.raised.gamma_multiply(alpha));
			painter.rect_stroke(rect, 8.0, (1.0, tokens.border_strong.gamma_multiply(alpha)), egui::StrokeKind::Inside);
			painter.rect_filled(Rect::from_min_size(rect.min + vec2(0.0, 5.0), vec2(3.0, height - 10.0)), 1.5, colour.gamma_multiply(alpha));

			let mut budget = ((width - 28.0) / cell).floor() as u32;
			if toast.undo.is_some() {
				let link = Rect::from_min_size(pos2(rect.max.x - 58.0, rect.center().y - 12.0), vec2(48.0, 24.0));
				painter.text(link.center(), Align2::CENTER_CENTER, "撤销", small.clone(), tokens.busy.gamma_multiply(alpha));
				if ui.interact(link, egui::Id::unique(("toast-undo", i)), Sense::click()).clicked() {
					undo_at = Some(i);
				}
				budget = budget.saturating_sub((46.0 / cell).ceil() as u32);
			}

			painter.text(
				pos2(rect.min.x + 14.0, rect.center().y),
				Align2::LEFT_CENTER,
				crate::list::clipped(&toast.text, budget),
				small.clone(),
				tokens.text.gamma_multiply(alpha),
			);
		}

		if let Some(at) = dismiss_at {
			self.toasts.dismiss(at);
		}

		if let Some(at) = undo_at
			&& let Some(undo) = self.toasts.take_undo(at)
		{
			// one action is undone once (P33⑫): the stack's copy of it goes with the link
			if let Some(position) = self.undo.iter().rposition(|held| *held == undo) {
				self.undo.remove(position);
			}
			self.run_undo(undo);
		}

		// the fade and the slide are their own reason to paint: while one is up, keep the frames
		// coming until it has gone
		self.toasts.tick(now);
		ui.ctx().request_repaint_after(Duration::from_millis(33));
	}

	/// The `which` hint (P18): the first key of a chord opened a family, and this is what is in it —
	/// the keys still to press and the keymap's own description, in a small card above the status
	/// bar. Non-modal: the list stays readable behind it, and the next key is what dismisses it.
	fn draw_which(&mut self, ui: &mut Ui, area: Rect, small: &FontId) {
		let Some(sequence) = &self.sequence else { return };
		// a modal owns the keyboard and the screen: its own hint would be noise
		if self.help || self.fuzzy.is_some() || self.confirm.is_some() || self.prompt.is_some() {
			return;
		}

		let family = self.keys.family(sequence);
		if family.is_empty() {
			return;
		}

		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let measure = |text: &str, colour: Color32| painter.layout_no_wrap(text.to_owned(), small.clone(), colour).size().x;
		let keys_w = family.iter().map(|(keys, _)| measure(keys, tokens.text)).fold(0.0, f32::max);
		let descs_w = family.iter().map(|(_, desc)| measure(desc, tokens.subtext)).fold(0.0, f32::max);

		let row_h = 20.0;
		let rows = family.len().min(((area.max.y - STATUS_HEIGHT - 12.0 - area.min.y - TAB_HEIGHT) / row_h).floor().max(1.0) as usize);
		let height = rows as f32 * row_h + 16.0;
		let width = (keys_w + descs_w + 48.0).min(area.width() - 24.0);
		let panel = Rect::from_min_size(
			pos2(area.min.x + 12.0, area.max.y - STATUS_HEIGHT - 12.0 - height),
			vec2(width.max(80.0), height),
		);

		chrome::shadow(&painter, panel, 8.0);
		painter.rect_filled(panel, 8.0, tokens.raised);
		painter.rect_stroke(panel, 8.0, (1.0, tokens.border_strong), egui::StrokeKind::Inside);

		// the keys right-aligned in one column and the descriptions left-aligned in the next, the
		// way the help overlay lays its own rows out
		let clipped = painter.with_clip_rect(panel);
		for (i, (keys, desc)) in family.iter().take(rows).enumerate() {
			let y = panel.min.y + 8.0 + i as f32 * row_h + row_h * 0.5;
			clipped.text(pos2(panel.min.x + 16.0 + keys_w, y), Align2::RIGHT_CENTER, keys.clone(), small.clone(), tokens.green);
			clipped.text(pos2(panel.min.x + 32.0 + keys_w, y), Align2::LEFT_CENTER, desc.clone(), small.clone(), tokens.subtext);
		}
	}

	/// The help overlay: the user's own bindings, read out of their own keymap, over everything.
	fn draw_help(&mut self, ui: &mut Ui, area: Rect, advance: f32, font: &FontId) {
		let tokens = self.tokens;
		let line = LINE_HEIGHT;
		let small = FontId::new(FONT_SMALL, FontFamily::Monospace);
		let rows = self.keys.rows().to_vec();
		let visible = help_rows(&rows, &self.help_query.text);

		let painter = ui.painter().clone();
		painter.rect_filled(area, 0.0, Color32::from_black_alpha(200));

		// the panel is the content's own size (P33①): the keys' widest column and the words' widest
		// column decide the width — the first version drew a slab 86% of the window for a table of
		// two columns — and the rows decide the height, capped at 70% of it
		let cells_of = |text: &str| text.chars().map(crate::list::cell_width).sum::<u32>() as usize;
		let key_cells = visible.iter().filter_map(|at| rows.get(*at)).map(|(on, ..)| cells_of(on)).max().unwrap_or(8).clamp(4, 12);
		let word_cells = visible.iter().filter_map(|at| rows.get(*at)).map(|(_, _, desc)| cells_of(desc)).max().unwrap_or(16).clamp(8, 30);
		let (width, height) = help_size(area, key_cells, word_cells, visible.len(), advance, line);
		let panel = Rect::from_center_size(area.center(), Vec2::new(width, height));
		painter.rect_filled(panel, 8.0, tokens.base);

		let header = format!("帮助 — {} 条绑定（j/k 或滚轮浏览，回车执行，Esc 关闭）", rows.len());
		painter.text(
			panel.min + Vec2::new(advance, line * 0.25),
			Align2::LEFT_TOP,
			crate::list::clipped(&header, ((panel.width() - advance * 2.0) / advance).max(8.0) as u32).to_owned(),
			font.clone(),
			tokens.accent,
		);

		// the box that filters the rows (P21): a click on it, or `f`, puts the keyboard inside —
		// under the header line, never over it, and as wide as the panel it stands in
		let field = Rect::from_min_size(panel.min + Vec2::new(advance, line * 1.45), Vec2::new(panel.width() - advance * 2.0, line * 1.2));
		painter.rect_filled(field, 4.0, tokens.crust);
		painter.rect_stroke(
			field,
			4.0,
			(1.0, if self.help_focus.inside { tokens.accent } else { tokens.border }),
			egui::StrokeKind::Inside,
		);
		painter.text(field.left_center() + Vec2::new(6.0, 0.0), Align2::LEFT_CENTER, "\u{f002}", small.clone(), tokens.disabled);
		painter.text(
			field.left_center() + Vec2::new(6.0 + advance * 2.0, 0.0),
			Align2::LEFT_CENTER,
			&self.help_query.text,
			font.clone(),
			tokens.text,
		);
		if self.help_focus.inside {
			let caret = field.min.x
				+ 6.0
				+ advance * 2.0
				+ cells(&self.help_query.text[..self.help_query.cursor], advance);
			let caret_rect = Rect::from_min_max(pos2(caret, field.center().y - 9.0), pos2(caret + 2.0, field.center().y + 9.0));
			painter.rect_filled(caret_rect, 0.0, tokens.accent);
			anchor_ime(ui, field, caret_rect);
		}
		if ui.interact(field, egui::Id::unique("help-field"), Sense::click()).clicked() {
			self.help_focus.enter();
		}

		// the rows: the standing one marked and kept in view, the mouse's click seating it
		let top = panel.min.y + line * 2.9;
		let height = ((panel.max.y - top - line * 1.6) / line).floor().max(1.0) as usize;
		if self.help_at < self.help_from {
			self.help_from = self.help_at;
		}
		if self.help_at >= self.help_from + height {
			self.help_from = self.help_at + 1 - height;
		}
		self.help_from = self.help_from.min(visible.len().saturating_sub(height));

		let mut seat = None;
		for (i, at) in visible.iter().skip(self.help_from).take(height).enumerate() {
			let Some((on, _run, desc)) = rows.get(*at) else { continue };
			let y = top + i as f32 * line;
			let row = Rect::from_min_size(pos2(panel.min.x + advance * 0.5, y), Vec2::new(panel.width() - advance, line));

			if self.help_from + i == self.help_at {
				painter.rect_filled(row, 0.0, tokens.cursor_bg);
				painter.rect_filled(Rect::from_min_size(row.min, Vec2::new(2.0, row.height())), 0.0, tokens.accent);
			}

			painter.text(pos2(row.min.x + advance * 0.5, row.center().y), Align2::LEFT_CENTER, crate::list::clipped(on, key_cells as u32).to_owned(), font.clone(), tokens.green);
			painter.text(
				pos2(row.min.x + advance * (key_cells as f32 + 1.5), row.center().y),
				Align2::LEFT_CENTER,
				crate::list::clipped(desc, word_cells as u32).to_owned(),
				font.clone(),
				tokens.subtext,
			);

			if ui.interact(row, egui::Id::unique(("help-row", *at)), Sense::click()).clicked() {
				seat = Some(self.help_from + i);
			}
		}
		if let Some(at) = seat {
			self.help_at = at.min(visible.len().saturating_sub(1));
		}

		// the footer: the table's own words for the three keys, and how many rows the query kept
		painter.text(
			pos2(panel.min.x + advance, panel.max.y - line * 1.15),
			Align2::LEFT_TOP,
			"enter 执行命令 · f 搜索 · esc 退出",
			font.clone(),
			tokens.text,
		);
		painter.text(
			pos2(panel.max.x - advance, panel.max.y - line * 1.15),
			Align2::RIGHT_TOP,
			format!("{} 命中", visible.len()),
			small.clone(),
			tokens.disabled,
		);
	}

	/// The visible rows of a column: an icon, a name, and the facts — with the cursor's own row
	/// marked. `from` is the first row drawn and `fraction` its offset, which is where the smooth
	/// scrolling comes from: this is the only place a row's position is decided.
	#[allow(clippy::too_many_arguments)]
	#[allow(clippy::too_many_arguments)]
	fn rows(
		painter: &Painter,
		tokens: &Tokens,
		column: Rect,
		entries: &[Entry],
		view: Option<&[usize]>,
		cursor: usize,
		columns: Columns,
		selection: &HashSet<PathBuf>,
		dir: &Path,
		font: &FontId,
		advance: f32,
		fraction: f32,
		from: usize,
		quiet: bool,
		// whether the size/time facts share the row (P12: only the middle column's do)
		meta: bool,
		hover: Option<usize>,
		// what `/` is looking for, if anything: the letters the name holds are drawn in the hit
		// colour (P21)
		find: Option<&str>,
	) {
		let row_h = ROW_HEIGHT;
		let rows = (column.height() / row_h).floor().max(1.0) as usize + 1;
		// a row that only half fits is cut at the column's edge (P27's second item): the list has
		// always drawn a row past its own rectangle while the scroll settles, and the whole rows
		// of the column beside it showed through the gap
		let clipped = painter.with_clip_rect(column);
		let painter = &clipped;
		// the facts read in the secondary face, the one the spec gives the metadata (§1.1)
		let small = FontId::new(FONT_SMALL, FontFamily::Monospace);
		let identity;
		let view: &[usize] = match view {
			Some(view) => view,
			None => {
				identity = (0..entries.len()).collect::<Vec<_>>();
				&identity
			}
		};

		for i in 0..rows {
			let position = from.saturating_add(i);
			let Some(&index) = view.get(position) else { break };
			let Some(entry) = entries.get(index) else { break };

			let top = column.min.y + (i as f32 - fraction) * row_h;
			let row = Rect::from_min_size(egui::pos2(column.min.x, top), Vec2::new(column.width(), row_h));
			if row.max.y < column.min.y || row.min.y > column.max.y {
				continue;
			}

			let selected = selection.contains(&dir.join(&entry.name));
			if selected {
				painter.rect_filled(row, 0.0, tokens.sel_bg);
			}
			if position == cursor {
				painter.rect_filled(row, 0.0, tokens.cursor_bg);
				if !quiet {
					// the cursor's mark: two pixels of accent down the row's own left edge
					painter.rect_filled(Rect::from_min_size(row.min, Vec2::new(2.0, row_h)), 0.0, tokens.accent);
				}
			} else if Some(position) == hover {
				// a row the pointer is over: the cursor's background at half its weight (spec §1.2)
				painter.rect_filled(row, 0.0, tokens.cursor_bg.gamma_multiply(0.5));
			}

			let (glyph, colour) = crate::list::icon(entry, tokens);
			painter.text(
				egui::pos2(column.min.x + advance + ICON_BOX * 0.5, top + row_h * 0.5),
				Align2::CENTER_CENTER,
				glyph,
				font.clone(),
				if quiet { colour.gamma_multiply(0.7) } else { colour },
			);

			// the facts, when this column shows them at all — and only as many as the width can
			// pay for (the ladder is `list::row_facts`' own)
			let facts = crate::list::row_facts(meta, columns, &entry_facts(entry), column.width(), advance);
			let facts_width = facts
				.as_ref()
				.map_or(0.0, |facts| painter.layout_no_wrap(facts.clone(), small.clone(), tokens.subtext).size().x);
			if let Some(facts) = facts {
				painter.text(
					egui::pos2(column.max.x - advance, top + row_h * 0.5),
					Align2::RIGHT_CENTER,
					facts,
					small.clone(),
					tokens.subtext,
				);
			}

			// the name's budget keeps the spec's 16px of air before the facts, when it has facts
			// to keep the air for (§1.1)
			let facts_width = if facts_width > 0.0 { facts_width + 16.0 } else { 0.0 };
			let budget =
				((column.width() - ICON_BOX - 3.0 * advance - facts_width) / advance).floor().max(1.0) as u32;
			let name = crate::list::clipped(&entry.name, budget);
			let name_colour = if entry.dir { tokens.blue } else { tokens.text };
			let name_colour = if quiet { name_colour.gamma_multiply(0.7) } else { name_colour };
			let at = egui::pos2(column.min.x + ICON_BOX + 2.0 * advance, top + row_h * 0.5);

			match find.map(|pattern| crate::app::find_hits(&entry.name, pattern)) {
				// the letters the search is about are coloured, the rest of the name keeps its own
				Some(positions) if !positions.is_empty() => {
					text_hits(painter, at, name, &positions, budget as usize, font, advance, name_colour, tokens.accent);
				}
				_ => {
					painter.text(at, Align2::LEFT_CENTER, name, font.clone(), name_colour);
				}
			}
		}
	}

/// The spot card (§4.6): the cursor's own file, described in an `Area` that floats over the
/// preview column's top-left — 320 wide, 12px off both edges, a fade-in from the left, because
/// the card belongs to the column it points at. `Tab`/`Esc` put it away and `j/k`/`h/l` walk the
/// list underneath without moving the card's anchor.
	fn draw_menu(&mut self, ui: &mut Ui, area: Rect, font: &FontId, small: &FontId) {
		let Some(menu) = self.menu.as_ref() else { return };
		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let rows = menu.rows();
		let apps_page = menu.page == crate::menu::Page::Apps;

		// the rows' own heights: a zone header is a line, an item is a row, and the applications
		// page pays a line for its query (P35's addendum — the dialog it mirrors is search-first).
		// A greyed item (P33⑱'s paste) is not a header: only the applications page draws those.
		let is_header = |live: bool| apps_page && !live;
		let row_heights: Vec<f32> = rows.iter().map(|(_, _, live)| if is_header(*live) { 18.0 } else { crate::menu::MENU_ROW_H }).collect();

		// the applications page's own window (P36③): twelve walked rows at most, fewer when the
		// window itself is short — the panel is a compact small window, and it starts where the
		// scroll stands (kept in sight by `follow`, dragged by the wheel)
		let vis = menu.visible_rows(self.body.height());
		let walked = menu.rows_walked();
		let scroll = menu.scroll.min(walked.saturating_sub(vis.max(1)));
		let end = (scroll + vis).min(walked);

		// the applications page's own width (P38①): the longest line the window shows — name and
		// its hint — or the token floor, whichever is wider, so an empty window keeps a panel
		// instead of shrinking to its query box (the user's frame: ninety points, the words
		// drawn outside). The item pages stay their own width.
		let length = |label: &str| painter.layout_no_wrap(label.to_owned(), font.clone(), tokens.text).size().x;
		let width = if apps_page {
			let mut longest = 0.0f32;
			let mut seen = 0usize;
			for (label, hint, live) in rows.iter() {
				if !is_header(*live) {
					if seen >= scroll && seen < end {
						longest = longest.max(length(label) + hint.as_ref().map_or(0.0, |hint| length(hint) + 16.0));
					}
					seen += 1;
				}
			}

			(longest + 28.0).max(crate::menu::MENU_APPS_MIN_W).min((area.width() - 16.0).max(240.0))
		} else {
			224.0
		};

		// the zone line, pinned (P36③, the choice over scrolling it away): while the window
		// stands inside a zone whose own header scrolled under, its name stays at the top
		let app_rows = apps_page.then(|| menu.app_rows());
		let pinned = app_rows.as_ref().is_some_and(|rows| rows.get(scroll).is_some_and(|row| row.zone.is_none()));

		// the panel's own height: what the window actually shows, never the whole roster
		let mut list_height = 0.0;
		let mut seen = 0usize;
		for (i, (_, _, live)) in rows.iter().enumerate() {
			if seen >= scroll && seen < end {
				list_height += row_heights[i];
			}
			if !is_header(*live) {
				seen += 1;
			}
		}
		let height = (30.0 + list_height + if apps_page { 28.0 } else { 0.0 } + if pinned { 18.0 } else { 0.0 } + 22.0)
			.max(if apps_page { crate::menu::menu_apps_floor() } else { 0.0 });

		// and it never hangs off the window (P36③): a compact window has to fit where it stands
		let anchor = pos2(
			menu.anchor.x.min(area.max.x - width - 8.0).max(area.min.x + 8.0),
			menu.anchor.y.min(area.max.y - height - 8.0).max(area.min.y + 8.0),
		);
		let panel = Rect::from_min_size(anchor, vec2(width, height));
		self.menu_panel = Some(panel);

		// no mask: a click outside is what puts it away, and the rows keep their own clicks
		let outside = ui.interact(area, egui::Id::unique("menu-outside"), Sense::click());
		if outside.clicked() && ui.ctx().input(|i| i.pointer.interact_pos()).is_none_or(|at| !panel.contains(at)) {
			self.menu = None;
		}

		chrome::shadow(&painter, panel, 8.0);
		painter.rect_filled(panel, 8.0, tokens.raised);
		painter.rect_stroke(panel, 8.0, (1.0, tokens.border_strong), egui::StrokeKind::Inside);

		// everything the panel says is clipped to it (P38①): over-long names are cut by `clipped`
		// and the clip is the backstop — not one word is ever drawn outside the rect
		let painter = painter.with_clip_rect(panel);

		let Some(menu) = self.menu.as_ref() else { return };
		painter.text(
			pos2(panel.min.x + 12.0, panel.min.y + 16.0),
			Align2::LEFT_CENTER,
			crate::list::clipped(&menu.title(), 40),
			FontId::new(13.0, FontFamily::Monospace),
			tokens.disabled,
		);

		// the applications page's query line (P35): what filters both zones, with its own caret
		let mut top = panel.min.y + 26.0;
		if apps_page {
			let field = Rect::from_min_size(pos2(panel.min.x + 6.0, top), vec2(width - 12.0, 22.0));
			painter.rect_filled(field, 4.0, tokens.crust);
			painter.rect_stroke(
				field,
				4.0,
				(1.0, if menu.query.text.is_empty() { tokens.border_strong } else { tokens.accent }),
				egui::StrokeKind::Inside,
			);
			painter.text(field.left_center() + vec2(8.0, 0.0), Align2::LEFT_CENTER, "\u{f002}", small.clone(), tokens.disabled);
			let text_at = field.min.x + 8.0 + 16.0;
			painter.text(pos2(text_at, field.center().y), Align2::LEFT_CENTER, &menu.query.text, font.clone(), tokens.text);
			let caret = text_at + cells(&menu.query.text[..menu.query.cursor.min(menu.query.text.len())], length("0"));
			painter.rect_filled(
				Rect::from_min_max(pos2(caret, field.center().y - 8.0), pos2(caret + 2.0, field.center().y + 8.0)),
				0.0,
				tokens.accent,
			);
			top += 28.0;
		}

		let standing = menu.standing();
		let mut picked = None;
		let mut at = 0usize;
		for (i, (label, hint, live)) in rows.iter().enumerate() {
			if is_header(*live) {
				// a zone header (P35): a quiet line of its own, read and not walked — drawn when
				// its zone's first row is in the window
				if at >= scroll && at < end {
					painter.text(
						pos2(panel.min.x + 12.0, top + 9.0),
						Align2::LEFT_CENTER,
						label.clone(),
						small.clone(),
						tokens.disabled,
					);
					top += row_heights[i];
				}
				continue;
			}

			if at < scroll || at >= end {
				at += 1;
				continue;
			}

			// the pinned zone (P36③): the window's first row, its own header scrolled under
			if at == scroll && pinned {
				let zone = app_rows.as_ref().and_then(|rows| rows.get(at)).map_or("", crate::menu::zone_of);
				painter.text(pos2(panel.min.x + 12.0, top + 9.0), Align2::LEFT_CENTER, zone, small.clone(), tokens.disabled);
				top += 18.0;
			}

			let row = Rect::from_min_size(pos2(panel.min.x + 6.0, top), vec2(width - 12.0, 24.0));
			let response = ui.interact(row, egui::Id::unique(("menu-row", at)), Sense::click());

			// the highlight is the standing row's alone (P36①): the pointer's hover no longer
			// moves it — that quiet re-seat was the bug the user hit (walk to 打开方式, Enter,
			// and the hovered row had taken the standing away). A click seats its row and acts;
			// Enter always takes the row that is lit
			if at == standing {
				painter.rect_filled(row, 4.0, tokens.cursor_bg);
				painter.rect_filled(Rect::from_min_size(row.min, vec2(2.0, row.height())), 0.0, tokens.accent);
			}

			let budget = (((width - 12.0) - 28.0 - hint.as_ref().map_or(0.0, |hint| length(hint) + 16.0)) / length("0")).floor().max(1.0) as u32;
			painter.text(
				pos2(row.min.x + 10.0, row.center().y),
				Align2::LEFT_CENTER,
				crate::list::clipped(label, budget),
				font.clone(),
				if *live { tokens.text } else { tokens.disabled },
			);
			if let Some(hint) = hint {
				painter.text(pos2(row.max.x - 8.0, row.center().y), Align2::RIGHT_CENTER, hint.clone(), small.clone(), tokens.disabled);
			}
			if *live && response.clicked() {
				picked = Some(at);
			}

			top += row_heights[i];
			at += 1;
		}

		if rows.is_empty() {
			painter.text(panel.center(), Align2::CENTER_CENTER, "没有匹配的应用", small.clone(), tokens.disabled);
		}

		painter.text(
			pos2(panel.min.x + 12.0, panel.max.y - 12.0),
			Align2::LEFT_CENTER,
			if apps_page { "输入过滤 · ↑↓/滚轮 走行 · Enter 打开 · Ctrl+Enter 总是 · Esc 清除/返回" } else { "Enter 选择 · Esc 返回/关闭" },
			FontId::new(11.0, FontFamily::Monospace),
			tokens.disabled,
		);

		// a click sets the standing row first, then acts on it — Enter takes the row already lit,
		// so the two are one door (P36①), the click's seat just its way in
		if let Some(at) = picked
			&& let Some(menu) = &mut self.menu
		{
			menu.seat(at);
		}
		if picked.is_some() {
			self.menu_pick(ui.ctx());
		}
	}

	/// The cursor's own file, described in an `Area` that floats over the preview column's top-left
	/// — 320 wide, 12px off both edges — for `Tab`: see §4.6.
	fn draw_spot(&mut self, ui: &mut Ui, body: Rect) {
		if !self.spot {
			return;
		}

		let columns = chrome::columns(body, self.shares);
		let (at, slide) = match columns.preview {
			Some(preview) => (preview, true),
			None => (columns.current, false),
		};
		let tokens = self.tokens;

		let Some(path) = self.selected() else { return };
		let entry = self.selected_entry();
		let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
		let (glyph, colour) = list::icon_for(&name, entry.as_ref().is_some_and(|e| e.dir), entry.as_ref().and_then(|e| e.mode).is_some_and(|mode| mode & 0o111 != 0), &tokens);

		// every attribute, in Chinese a beginner reads (P23): the type (a link with its target),
		// the size with its exact bytes, a picture's own section — format, pixels, the ratio the
		// column shows it at, the colour depth (P28② revised) — the owner and the group by name,
		// the three permission classes as phrases, the link count, the three times whole, the path
		let shown_width = self.preview.shown_physical(&path).map(|shown| shown[0]).or_else(|| self.preview.fit_width_pixels());
		let rows = match crate::spot::rows(&path, shown_width) {
			Some(rows) => rows,
			None => vec![ ("类型".to_owned(), "读不到这个条目（可能刚被移走）".to_owned()) ],
		};

		// 320 wide, 12px off the column's edges (§4.6) — and never pushed off the window: a
		// preview column can be narrower than the card
		let width = SPOT_WIDTH;
		let x = if slide {
			(at.min.x + 12.0).min(body.max.x - width - 12.0).max(body.min.x + 12.0)
		} else {
			(at.max.x - 12.0 - width).max(body.min.x + 12.0)
		};
		let pos = pos2(x, at.min.y + 12.0);

		egui::Area::new(egui::Id::unique("crabhop-spot"))
			.order(egui::Order::Foreground)
			.fixed_pos(pos)
			.fade_in(true)
			.show(ui.ctx(), |ui| {
				egui::Frame::NONE
					.fill(tokens.raised)
					.stroke((1.0, tokens.border_strong))
					.corner_radius(8.0)
					.inner_margin(12.0)
					.show(ui, |ui| {
						ui.set_width(SPOT_WIDTH - 24.0);
						ui.spacing_mut().item_spacing = vec2(6.0, 6.0);

						ui.horizontal(|ui| {
							ui.label(egui::RichText::new(glyph).size(16.0).color(colour));
							ui.label(egui::RichText::new(list::clipped(&name, 28)).size(14.0).color(tokens.text));
						});
						ui.separator();

						egui::Grid::new("crabhop-spot-grid").num_columns(2).spacing(vec2(8.0, 5.0)).show(ui, |ui| {
							for (key, value) in rows {
								ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
									ui.label(egui::RichText::new(key).size(13.0).color(tokens.disabled).monospace());
								});
								ui.label(egui::RichText::new(value).size(13.0).color(tokens.text).monospace());
								ui.end_row();
							}
						});

						ui.separator();
						ui.label(
							egui::RichText::new("Tab/Esc 关闭 · j/k 移动 · y 复制")
								.size(13.0)
								.color(tokens.disabled)
								.monospace(),
						);
					});
			});
	}
}

/// A chip's width: its name's cells at the body font, in the spec's 100–220px band.
fn chip_width(name: &str, advance: f32) -> f32 {
	let cells = name.chars().map(crate::list::cell_width).sum::<u32>();
	(cells as f32 * advance + CHIP_PAD).clamp(CHIP_MIN, CHIP_MAX)
}

/// The room a chip's own number badge takes: the digits in the small face, plus the gap before
/// the name. Past nine the badge simply grows a digit (§2.4's number keys).
fn number_width(index: usize) -> f32 {
	let digits = (index + 1).to_string().len() as f32;

	digits * FONT_SMALL * 0.62 + 6.0
}

/// A string's width in a monospace face, counted in cells — a wide glyph takes two. The carets of
/// the window's own fields are placed with this.
fn cells(text: &str, advance: f32) -> f32 {
	text.chars().map(|c| crate::list::cell_width(c) as f32 * advance).sum()
}

/// The IME's anchor (P33⑤): every field in this window is drawn by hand, and egui sets the
/// platform's cursor area only for its own `TextEdit` — without this the Chinese input method's
/// candidate window had no place to stand. Called where the field's box and the caret's column
/// are both already known; it must not blink with the caret, so it is set every frame the field
/// holds the keyboard. The platform reads the field's box and the caret's own thin rectangle.
fn anchor_ime(ui: &Ui, field: Rect, caret: Rect) {
	ui.output_mut(|out| {
		out.ime = Some(egui::output::IMEOutput {
			purpose: egui::IMEPurpose::Normal,
			rect: field,
			cursor_rect: caret,
			should_interrupt_composition: false,
		});
	});
}

/// Where the output sheet sits: the window's bottom above the status bar, its height by its own
/// content, at most 40% of the window (§4.8).
fn output_rect(area: Rect, lines: usize) -> Rect {
	let height = (34.0 + lines as f32 * 20.0).min(area.height() * 0.4);
	Rect::from_min_max(pos2(area.min.x, area.max.y - STATUS_HEIGHT - height), pos2(area.max.x, area.max.y - STATUS_HEIGHT))
}

/// One line with its matched characters in the hit colour: drawn as runs, so a long path costs a
/// handful of draws rather than one per character. `budget` is how many cells fit — the same
/// count the list's own clipping pays in.
#[allow(clippy::too_many_arguments)]
fn text_hits(
	painter: &Painter,
	at: egui::Pos2,
	text: &str,
	positions: &[usize],
	budget: usize,
	font: &FontId,
	advance: f32,
	base: Color32,
	hit: Color32,
) {
	let (mut run, mut lit, mut x, mut used) = (String::new(), false, at.x, 0usize);

	for (i, c) in text.chars().enumerate() {
		let width = crate::list::cell_width(c) as usize;
		if used + width > budget {
			break;
		}
		used += width;

		let on = positions.contains(&i);
		if on != lit && !run.is_empty() {
			painter.text(pos2(x, at.y), Align2::LEFT_CENTER, run.clone(), font.clone(), if lit { hit } else { base });
			x += cells(&run, advance);
			run.clear();
		}
		lit = on;
		run.push(c);
	}

	if !run.is_empty() {
		painter.text(pos2(x, at.y), Align2::LEFT_CENTER, run, font.clone(), if lit { hit } else { base });
	}
}


/// The wall clock as unix seconds: what the frecency stamps its visits with.
fn now_secs() -> u64 {
	std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// A search hit's own row: the path, and for a content hit its line and text — the string the
/// panel matches against and shows.
fn hit_text(hit: &search::Hit) -> String {
	let path = frecency::display(&hit.path);
	match (hit.line, &hit.text) {
		(Some(line), Some(text)) => format!("{path}:{line}  {text}"),
		_ => path,
	}
}

/// Which tab a `tab_switch` lands on: absolute indexes clamp, relative steps wrap — the way a
/// ring of tabs reads.
pub(crate) fn switch_target(len: usize, active: usize, index: isize, relative: bool) -> usize {
	if len == 0 {
		return 0;
	}

	if relative {
		(active as isize + index).rem_euclid(len as isize) as usize
	} else {
		index.clamp(0, len as isize - 1) as usize
	}
}

/// The keys a replay may type, spelled the way a person would: `j`, `G`, `~`, `Enter`, `C-Enter`.
/// The single characters go through egui's own name table, so the whole alphabet is here.
fn replay_key(key: &str) -> Option<(Key, Modifiers)> {
	let (modifiers, name) = match key.strip_prefix("C-") {
		Some(rest) => (Modifiers::CTRL, rest),
		None if replay_shifted(key) => (Modifiers::SHIFT, key),
		None => (Modifiers::NONE, key),
	};

	let key = match name {
		"Enter" => Key::Enter,
		"Esc" => Key::Escape,
		"Space" => Key::Space,
		"Backspace" => Key::Backspace,
		"Tab" => Key::Tab,
		"Up" => Key::ArrowUp,
		"Down" => Key::ArrowDown,
		"Left" => Key::ArrowLeft,
		"Right" => Key::ArrowRight,
		";" => Key::Semicolon,
		"," => Key::Comma,
		"." => Key::Period,
		"/" => Key::Slash,
		// the help's own key: `?` is what the layout types for shift+slash (P21)
		"?" => Key::Slash,
		"[" => Key::OpenBracket,
		"]" => Key::CloseBracket,
		"`" => Key::Backtick,
		// the shifted digit rows the link family and the closer brackets sit on
		")" => Key::Num0,
		"(" => Key::Num9,
		"*" => Key::Num8,
		"}" => Key::CloseBracket,
		"{" => Key::OpenBracket,
		_ => Key::from_name(name)?,
	};

	Some((key, modifiers))
}

/// What the layout types for a replay's key: a one-character name types itself, and the tokens —
/// and anything held with Ctrl — type nothing.
fn replay_text(key: &str) -> Option<&str> {
	if key.starts_with("C-") {
		return None;
	}

	let mut characters = key.chars();
	let (Some(one), None) = (characters.next(), characters.next()) else { return None };
	(!one.is_control()).then(|| &key[..one.len_utf8()])
}

/// Whether a replay's step is typed with shift held — upper-case letters and the shifted
/// punctuation, the way a layout would send them.
fn replay_shifted(key: &str) -> bool {
	key.chars().all(|c| c.is_ascii_uppercase())
		|| matches!(key, "~" | "_" | "+" | "{" | "}" | "|" | ":" | "<" | ">" | "?")
}

/// One row of the index panel before it becomes a `FuzzyItem` (P14): which zone it belongs to,
/// what it reads as, what the right column adds, and whether a bookmark's path is gone.
struct IndexRow {
	pub zone:   &'static str,
	pub text:   String,
	pub right:  Option<String>,
	pub payload: FuzzyPayload,
	pub broken: bool,
}

/// The three zones of the index panel, in the order they read: the bookmarks (key badge and all,
/// a broken path still listed), then the frecency's high scores, then its most recent visits —
/// a path already said once is not said again, and each zone is capped so the panel stays a
/// glance.
fn index_rows(
	books: &[crate::book::Bookmark],
	ranked: &[(PathBuf, f64)],
	recent: &[(PathBuf, u64)],
) -> Vec<IndexRow> {
	const ZONE: usize = 12;
	let mut rows: Vec<IndexRow> = Vec::new();
	let mut seen: HashSet<PathBuf> = HashSet::new();

	for bookmark in books {
		seen.insert(bookmark.path.clone());
		rows.push(IndexRow {
			zone:    "索引",
			// the path says the name already: the row is the path and nothing else
			text:    frecency::display(&bookmark.path),
			right:   None,
			payload: FuzzyPayload::Index(bookmark.name.clone()),
			broken:  !bookmark.path.exists(),
		});
	}

	for (path, score) in ranked.iter().take(ZONE) {
		if !seen.insert(path.clone()) {
			continue;
		}
		rows.push(IndexRow {
			zone:    "高频",
			text:    frecency::display(path),
			right:   Some(format!("{score:.2}")),
			payload: FuzzyPayload::Path(path.clone()),
			broken:  false,
		});
	}

	for (path, _) in recent.iter().take(ZONE) {
		if !seen.insert(path.clone()) {
			continue;
		}
		rows.push(IndexRow {
			zone:    "最近",
			text:    frecency::display(path),
			right:   None,
			payload: FuzzyPayload::Path(path.clone()),
			broken:  false,
		});
	}

	rows
}

/// `M R`: the device the cwd stands on is unmounted (forced — a busy filesystem must not keep a
/// person from their own remount) and mounted again. Runs on the worker.
fn remount(cwd: &std::path::Path) -> Result<std::path::PathBuf, String> {
	let devices = mount::list()?;
	let Some(device) = mount::device_of(&devices, cwd) else {
		return Err(format!("{} 不在任何设备上", cwd.display()));
	};
	if device.mounts.is_empty() {
		return Err(format!("{} 没有挂载点", device.label));
	}

	mount::unmount(&device.id, true, false)?;
	mount::mount(&device.id)
}

/// `G m`: where the cwd's own device is mounted (the first of its mount points).
fn jump_to(cwd: &std::path::Path) -> Result<std::path::PathBuf, String> {
	let devices = mount::list()?;
	let Some(device) = mount::device_of(&devices, cwd) else {
		return Err(format!("{} 不在任何设备上", cwd.display()));
	};

	device.mounts.first().cloned().ok_or_else(|| format!("{} 没有挂载点", device.label))
}

/// Whether a name answers a filter's pattern: case-insensitive substring, and an empty pattern
/// answers everything. Pure, so the rule can be read and tested.
pub(crate) fn matches(pattern: &str, name: &str) -> bool {
	pattern.is_empty() || name.to_lowercase().contains(&pattern.to_lowercase())
}

/// The walk from one directory to a path — the target a relative symlink stores (P13's `(`):
/// the common prefix drops away, a `..` a level remains, and the rest is appended. Pure, so the
/// arithmetic can be tested.
pub(crate) fn relative_target(from: &Path, to: &Path) -> PathBuf {
	let from: Vec<_> = from.components().collect();
	let to: Vec<_> = to.components().collect();
	let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();

	let mut out = PathBuf::new();
	for _ in common..from.len() {
		out.push("..");
	}
	for component in &to[common..] {
		out.push(component.as_os_str());
	}

	out
}

/// A `cd` run's path with its `~` expanded: `~` is the home itself, `~/x` is `home/x`, and a
/// home that cannot be named leaves the path alone. Pure, so the rule can be tested.
pub(crate) fn expand_tilde(path: &str, home: Option<PathBuf>) -> Option<PathBuf> {
	match path {
		"~" => home,
		_ => match path.strip_prefix("~/") {
			Some(rest) => home.map(|home| home.join(rest)),
			None => Some(PathBuf::from(path)),
		},
	}
}

/// What `M`'s panel answers besides its own browsing (P21's second wave): the nine options the
/// table lists as panel keys. Enter is the panel's own (mount, or unmount in the other mode), and
/// these are the rest of them. Pure, so the routing can be read and tested without a window.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum DeviceDoor {
	/// `p`: mount the standing device, and stay.
	Mount,
	/// `m`: mount it and go to it.
	MountAndJump,
	/// `R`: the device the cwd stands on, unmounted and mounted again.
	Remount,
	/// `u` / `U`: unmount it (and eject when it can), asking for force when the key said so.
	Unmount {
		force: bool,
	},
	/// `g`: to the mount point of the device the cwd stands on (automounting it when it can).
	Jump,
	/// `a` / `e` / `r`: the GVfs remote-URI family — not this window's (a toast, in the deviation
	/// table).
	Uri(&'static str),
}

pub(crate) fn device_door(key: Key, shift: bool) -> Option<DeviceDoor> {
	Some(match (key, shift) {
		(Key::P, false) => DeviceDoor::Mount,
		(Key::M, false) => DeviceDoor::MountAndJump,
		(Key::R, true) => DeviceDoor::Remount,
		(Key::R, false) => DeviceDoor::Uri("移除"),
		(Key::U, false) => DeviceDoor::Unmount { force: false },
		(Key::U, true) => DeviceDoor::Unmount { force: true },
		(Key::G, false) => DeviceDoor::Jump,
		(Key::A, false) => DeviceDoor::Uri("添加"),
		(Key::E, false) => DeviceDoor::Uri("编辑"),
		_ => return None,
	})
}

/// The character positions where a name holds what was typed (P21's `/`): every occurrence, folded
/// the way ASCII folds, so the list colours exactly the letters the search is about. Pure.
pub(crate) fn find_hits(text: &str, needle: &str) -> Vec<usize> {
	let needle: Vec<char> = needle.chars().collect();
	let chars: Vec<char> = text.chars().collect();
	if needle.is_empty() || needle.len() > chars.len() {
		return Vec::new();
	}

	let mut hits = Vec::new();
	for start in 0..=chars.len() - needle.len() {
		if needle.iter().enumerate().all(|(i, n)| chars[start + i].eq_ignore_ascii_case(n)) {
			hits.extend(start..start + needle.len());
		}
	}

	hits
}

/// The view position of the next name that holds the pattern, walking from `from` (P21's `n`/`N`).
/// Pure, so the rule can be read and tested without a window.
pub(crate) fn find_row(view: &[usize], entries: &[Entry], pattern: &str, from: usize, forward: bool) -> Option<usize> {
	let holds = |at: usize| {
		view.get(at)
			.and_then(|index| entries.get(*index))
			.is_some_and(|entry| !find_hits(&entry.name, pattern).is_empty() || entry.name.to_lowercase().contains(&pattern.to_lowercase()))
	};

	if forward {
		((from + 1)..view.len()).find(|at| holds(*at))
	} else {
		(0..from).rev().find(|at| holds(*at))
	}
}

/// The keymap rows the help panel shows, and where the standing one is: the query keeps the rows
/// whose key, run or words hold what was typed (P21's `?` — 智能过滤). Pure, so the panel's own
/// filtering can be read without a window.
/// The help panel's own size (P33①): the two columns decide the width, the rows decide the height,
/// and the height never passes seventy percent of the window — the panel is the content's box, not
/// a slab of the screen. Pure, so the arithmetic can be read without a window.
pub(crate) fn help_size(area: Rect, key_cells: usize, word_cells: usize, visible: usize, advance: f32, line: f32) -> (f32, f32) {
	let width = (advance * (key_cells + word_cells) as f32 + advance * 4.0)
		.clamp(advance * 24.0, (area.width() * 0.9).max(advance * 24.0));
	let room = ((area.height() * 0.7 - line * 4.5).max(line * 4.0) / line).floor();
	let rows = (visible as f32).clamp(4.0, room);
	let height = line * 4.5 + rows * line;

	(width, height)
}

/// The rows a help query keeps (P21; P33⑨): the haystack is what the panel *shows* — the key's own
/// spelling and the Chinese words. The `run` text is not on screen, so it is not searched: a query
/// that could only match a run would leave a list of rows none of which says why.
/// What the status bar's right group says (P33⑭): the filter's own prefix, the sort's word and
/// the arrow — and nothing else. A linemode cell stood at its end once and was removed by the
/// user's ruling; the linemode is what the rows show, not a word in the corner. Pure, so the
/// words can be read without a window.
pub(crate) fn right_group(sort: (SortBy, bool), symlinks: bool, filter: Option<&str>) -> String {
	let label = match sort.0 {
		SortBy::Name => "名称",
		SortBy::Size => "大小",
		SortBy::Mtime => "修改时间",
		SortBy::Ext => "扩展名",
		SortBy::Natural => "自然",
		SortBy::Btime => "创建时间",
		SortBy::Random => "随机",
	};
	let arrow = if sort.1 { "↓" } else { "↑" };
	let prefix = if symlinks {
		"符号链接  ".to_owned()
	} else {
		filter.map_or(String::new(), |pattern| format!("筛选:{pattern}  "))
	};

	format!("{prefix}{label}{arrow}")
}

pub(crate) fn help_rows(rows: &[(String, String, String)], query: &str) -> Vec<usize> {
	// a chord is said without its space: `gh` finds `g h` the way the fingers type it
	let needle: String = query.chars().filter(|ch| !ch.is_whitespace()).collect::<String>().to_lowercase();

	rows.iter()
		.enumerate()
		.filter(|(_, (on, _run, desc))| {
			needle.is_empty()
				|| format!("{on}{desc}")
					.chars()
					.filter(|ch| !ch.is_whitespace())
					.collect::<String>()
					.to_lowercase()
					.contains(&needle)
		})
		.map(|(at, _)| at)
		.collect()
}

/// The keys the index panel answers besides its own typing (P24): `Del` deletes the standing
/// index and `Ctrl+S` saves the directory the window stands in — and nothing else. `F2` and
/// `a`/`A` were doors here once; the table took them away, and this is where that is written down.
/// Pure, so the routing can be read and tested without a window.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum IndexDoor {
	Save,
	Delete,
}

pub(crate) fn index_door(key: Key, ctrl: bool) -> Option<IndexDoor> {
	match (key, ctrl) {
		(Key::S, true) => Some(IndexDoor::Save),
		(Key::Delete, _) => Some(IndexDoor::Delete),
		_ => None,
	}
}

/// What the index panel's Enter does with what was typed (P22), in the table's own order: a path
/// that is there is gone to, a path that is not there says so, and anything else is the panel's
/// own selection. Pure, so the three priorities can be read without a window.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum QueryEnter {
	/// ① a directory (or a file, which `go` reveals) that is there.
	Go(PathBuf),
	/// ② it reads like a path and is not there.
	Missing(PathBuf),
	/// ③ not a path at all.
	Selection,
}

/// The rule itself: `~` is expanded, a relative path is resolved against the cwd, `.`/`..` fold
/// without touching the filesystem, and only what *looks* like a path (`/`, `~`, or a leading `.`)
/// is treated as one — a search word stays a search word.
pub(crate) fn query_enter(query: &str, cwd: &Path, home: Option<PathBuf>) -> QueryEnter {
	let typed = query.trim();
	if typed.is_empty() || !looks_like_path(typed) {
		return QueryEnter::Selection;
	}

	// `~` with no home to expand against is not a path this window can resolve: the row decides
	let Some(expanded) = expand_tilde(typed, home) else {
		return QueryEnter::Selection;
	};

	let path = if expanded.is_absolute() { expanded } else { cwd.join(expanded) };
	let path = folded(&path);

	if path.symlink_metadata().is_ok() { QueryEnter::Go(path) } else { QueryEnter::Missing(path) }
}

/// Whether what was typed reads as a path at all: an absolute one, a home one, or one that starts
/// where the person stands — anything else is a word to search for.
fn looks_like_path(typed: &str) -> bool {
	typed.contains('/') || typed.starts_with('~') || typed.starts_with('.')
}

/// `.` and `..` folded away without asking the filesystem: a typed `..` means the parent
/// directory, never a directory literally named `..`.
fn folded(path: &Path) -> PathBuf {
	use std::path::Component;

	let mut out = PathBuf::new();
	for part in path.components() {
		match part {
			Component::CurDir => {}
			Component::ParentDir => {
				if !out.pop() {
					out.push(part.as_os_str());
				}
			}
			other => out.push(other.as_os_str()),
		}
	}

	out
}

/// `file2` before `file10` (P13's natural sort): runs of digits compare as the numbers they
/// spell, everything else character by character with its case folded, and a name that has run
/// out comes first. Pure, so the rule can be read and tested.
pub(crate) fn natural(a: &str, b: &str) -> std::cmp::Ordering {
	use std::cmp::Ordering;

	fn number(rest: &mut std::iter::Peekable<std::str::Chars>) -> u128 {
		let mut value: u128 = 0;
		while let Some(digit) = rest.peek().filter(|c| c.is_ascii_digit()) {
			value = value.saturating_mul(10).saturating_add(digit.to_digit(10).unwrap_or(0) as u128);
			rest.next();
		}

		value
	}

	let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
	loop {
		match (a.peek(), b.peek()) {
			(None, None) => return Ordering::Equal,
			(None, Some(_)) => return Ordering::Less,
			(Some(_), None) => return Ordering::Greater,
			(Some(left), Some(right)) => {
				let (left_digit, right_digit) = (left.is_ascii_digit(), right.is_ascii_digit());

				let order = match (left_digit, right_digit) {
					(true, true) => number(&mut a).cmp(&number(&mut b)),
					(false, false) => {
						let (left, right) = (a.next().unwrap(), b.next().unwrap());
						left.to_lowercase().cmp(right.to_lowercase())
					}
					// a digit outranks a letter, the way a byte does
					(true, false) => Ordering::Less,
					(false, true) => Ordering::Greater,
				};

				if order != Ordering::Equal {
					return order;
				}
			}
		}
	}
}

/// A name's shuffle key under `,r`: arbitrary, but the same name under the same seed always
/// lands in the same place — a scroll must not reorder the list under the hand (a re-press picks
/// a new seed instead). FNV-1a over the name and the seed.
pub(crate) fn shuffle_key(name: &str, seed: u64) -> u64 {
	let mut hash: u64 = 0xcbf2_9ce4_8422_2325;

	for byte in seed.to_le_bytes().iter().chain(name.as_bytes()) {
		hash ^= *byte as u64;
		hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
	}

	hash
}

/// The one child a bypass would descend into: the directory must show exactly one row (the
/// hidden switch and the ordering decide what "one" means) and that row must be a directory —
/// P13's smart enter. Pure, so the rule can be read and tested.
pub(crate) fn bypass_child(
	dir: &Path,
	entries: &[Entry],
	by: SortBy,
	reverse: bool,
	hidden: bool,
) -> Option<PathBuf> {
	let view = ordered(entries, by, reverse, hidden, 0);
	let [only] = view[..] else { return None };
	let entry = entries.get(only)?;

	entry.dir.then(|| dir.join(&entry.name))
}

/// What a symlink's own target means when it is read: an absolute target is itself, a relative
/// one is relative to the link's directory (P13's `g f`). Pure, so the rule can be tested.
pub(crate) fn followed(parent: &Path, target: &Path) -> PathBuf {
	if target.is_absolute() {
		target.to_owned()
	} else {
		parent.join(target)
	}
}

/// What a batch of events typed, as far as the window's fields are concerned: the layout's own
/// text, a paste (Ctrl+V, or a middle-click's `Event::Paste`) and an input method's commit — the
/// three ways characters reach a field (P18). Pure, so the rule can be tested without a window.
pub(crate) fn typed_text(events: &[egui::Event]) -> String {
	events
		.iter()
		.filter_map(|event| match event {
			egui::Event::Text(text) => Some(text.as_str()),
			egui::Event::Paste(text) => Some(text.as_str()),
			egui::Event::Ime(egui::ImeEvent::Commit(text)) => Some(text.as_str()),
			_ => None,
		})
		.collect()
}

/// `<C-a>` / `<C-r>`'s own rule, over a set of rows: `Some(true)` selects them all, `Some(false)`
/// clears them, and `None` turns each one over — pure, so the rule can be read and tested.
pub(crate) fn toggled(selection: &mut HashSet<PathBuf>, paths: &[PathBuf], on: Option<bool>) {
	match on {
		Some(true) => selection.extend(paths.iter().cloned()),
		Some(false) => {
			for path in paths {
				selection.remove(path);
			}
		}
		None => {
			for path in paths {
				if !selection.remove(path) {
					selection.insert(path.clone());
				}
			}
		}
	};
}

/// The parent column's own path, and the cursor if the stack already knows it: the directory a tab
/// stepped out of (`Some(cursor)`), or the cwd's filesystem parent (`None` — the row has to be
/// found in the listing). Pure, so the rule can be read and tested without a window.
fn parent_path(stack: &[View], cwd: &Path) -> Option<(PathBuf, Option<usize>)> {
	if let Some(above) = stack.len().checked_sub(2) {
		return Some((stack[above].path.clone(), Some(stack[above].cursor)));
	}

	Some((cwd.parent()?.to_owned(), None))
}

/// The facts a row may wear, spelled once per row (P21): the size, the two times, the permission
/// bits the status bar already spells, and the owner's name behind its id.
fn entry_facts(entry: &Entry) -> crate::list::Facts {
	crate::list::Facts {
		size:  crate::list::size(entry.size),
		when:  crate::list::when(entry.modified),
		btime: crate::list::when(entry.created),
		perms: String::from_utf8_lossy(&chrome::perm(entry.mode)).into_owned(),
		owner: crate::spot::owner(entry.uid),
	}
}

/// The name a directory is listed under: what a `leave` seats the cursor on when it comes back up.
fn file_name(path: &Path) -> Option<String> {
	path.file_name().map(|name| name.to_string_lossy().into_owned())
}

/// The metadata a preview needs for a path the window did not list: the bin's own copy of an item
/// is exactly that (P19), and the preview asks for something to size its decode by.
fn entry_of(path: &Path) -> Option<Entry> {
	use std::os::unix::fs::PermissionsExt;

	let meta = std::fs::symlink_metadata(path).ok()?;

	Some(Entry {
		name:     path.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
		size:     meta.len(),
		modified: meta.modified().ok(),
		dir:      meta.is_dir(),
		mode:     Some(meta.permissions().mode()),
		link:     meta.file_type().is_symlink(),
		created:  meta.created().ok(),
		uid:      {
			use std::os::unix::fs::MetadataExt;
			meta.uid()
		},
	})
}

/// Which entries show, in which order: the switches and the ordering, in one place — so the rule
/// can be read, and tested, without a window.
pub(crate) fn ordered(entries: &[Entry], by: SortBy, reverse: bool, hidden: bool, seed: u64) -> Vec<usize> {
	let mut view: Vec<usize> = entries
		.iter()
		.enumerate()
		.filter(|(_, entry)| hidden || !entry.name.starts_with('.'))
		.map(|(index, _)| index)
		.collect();

	view.sort_by(|a, b| {
		let (a, b) = (&entries[*a], &entries[*b]);
		let order = match by {
			SortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
			SortBy::Size => a.size.cmp(&b.size),
			SortBy::Mtime => a.modified.cmp(&b.modified),
			SortBy::Ext => {
				let (a, b) = (a.name.rsplit('.').next().unwrap_or(""), b.name.rsplit('.').next().unwrap_or(""));
				a.to_lowercase().cmp(&b.to_lowercase()).then_with(|| a.cmp(b))
			}
			SortBy::Natural => natural(&a.name, &b.name),
			SortBy::Btime => a.created.cmp(&b.created),
			SortBy::Random => shuffle_key(&a.name, seed).cmp(&shuffle_key(&b.name, seed)),
		};

		// directories first, the way a file manager has always done it, then the chosen order
		b.dir.cmp(&a.dir).then_with(|| if reverse { order.reverse() } else { order })
	});

	view
}

/// The view position a click on the drawn window's `i`-th row decided, if any: a row hit seats
/// the cursor there, and anything past the list is the blank below the rows, which decides
/// nothing. Pure, so the rule can be read (and tested) without a window.
pub(crate) fn clicked_row(first: usize, i: usize, len: usize) -> Option<usize> {
	let position = first + i;

	(position < len).then_some(position)
}

/// Which row the pointer is over, as a position in the view — the rule the hover state is drawn
/// from, in one place so it can be read (and tested) without a window.
pub(crate) fn hover_at(pointer: Option<egui::Pos2>, column: Rect, row_h: f32, from: usize) -> Option<usize> {
	let pointer = pointer?;
	if !column.contains(pointer) {
		return None;
	}

	let row = ((pointer.y - column.min.y) / row_h).floor();
	(row >= 0.0).then_some(from.saturating_add(row as usize))
}

impl eframe::App for App {
	fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
		self.frame(ui);
	}
}

impl App {
	/// One frame of the window, from a `Ui` and nothing else (P25): this is what eframe's `ui`
	/// calls, and what the headless harness calls — the same frame either way.
	pub(crate) fn frame(&mut self, ui: &mut Ui) {
		let ctx = ui.ctx().clone();
		let area = ui.max_rect();
		let dt = ctx.input(|i| i.stable_dt).clamp(0.001, 0.1);
		self.frame += 1;
		metrics::frame_edge(self.frame, true);

		let body = Rect::from_min_max(
			pos2(area.min.x, area.min.y + TAB_HEIGHT),
			pos2(area.max.x, area.max.y - STATUS_HEIGHT - if self.in_bin() { TRASH_HINT } else { 0.0 }),
		);
		self.body = body;

		// 1. the input, decided now, on this thread: a chord's family that has waited long enough
		// goes away before the key in this frame is read
		self.which_expire(&ctx);
		self.keys(&ctx);

		// 2. whatever a command in flight has left, read now so its sheet lands in this frame
		self.shell_poll(&ctx);
		self.open_poll(&ctx);
		self.trash_poll(&ctx);
		self.paste_poll(&ctx);
		self.undo_poll(&ctx);
		self.device_poll(&ctx);
		self.pack_poll(&ctx);
		self.fuzzy_tick(&ctx);
		self.preview.poll();

		// every directory the window stands in is a visit, recorded once — the frecency list
		let here = self.tab().path().to_owned();
		if self.visited.as_deref() != Some(here.as_path()) {
			self.visited = Some(here.clone());
			self.frecency.visit(&here, now_secs());
			self.frecency_save();
		}

		// 3. the wheel, and the smooth scroll it feeds
		let pointer = ctx.input(|i| i.pointer.latest_pos());
		self.wheel(&ctx, area, pointer);
		self.hover = pointer;

		// the view that is up is the one that steps (P33③): the bin carries a scroll of its own,
		// and a wheel'd target that nothing ever approached was a bin that never moved — and the
		// left column steps beside the middle one (P33⑧)
		let mut moved = if self.in_bin() {
			self.bin_mut().is_some_and(|view| view.scroll.step(dt))
		} else {
			self.tab_mut().scroll.step(dt)
		};
		if self.tab_mut().parent.scroll.step(dt) {
			moved = true;
		}
		if moved {
			ctx.request_repaint();
		}

		// 3. the picture, over the rows the switches decided — and the directories on show are
		// watched, so another program's writes appear (P18)
		self.refresh_view();
		self.cache.watch(self.tab().path());
		if let Some((parent, _)) = self.parent_view() {
			self.cache.watch(&parent);
		}
		self.draw(ui, area, body);

		// 4. the numbers, the replay and the shots
		let snapshot = self.entries();
		if !snapshot.loading && !snapshot.entries.is_empty() && !self.first {
			self.first = true;
			metrics::first_list();
			self.shot_wait = Some(2);
		}
		if snapshot.loading {
			// wake up when the skeleton's own moment arrives
			if let Some(started) = snapshot.started
				&& started.elapsed() < SKELETON_AFTER
			{
				ctx.request_repaint_after(SKELETON_AFTER - started.elapsed());
			}
		}

		self.try_reveal(&ctx);
		self.replay(&ctx, area);
		self.shots(&ctx);
		if self.tab().scroll.moving {
			self.bars_moved[0] = Some(Instant::now());
			metrics::scroll(self.tab().scroll.offset, self.tab().scroll.target);
		}

		metrics::frame(dt);

		if self.frame == 1 {
			metrics::first_paint();
		}
		if self.frame == self.dress_at {
			metrics::first_dressed();
		}
		if !self.dressed {
			// the first frame is drawn: the fonts and styles can be built now, behind it
			self.dressed = true;
			Tokens::install(&ctx);
			ctx.request_repaint();
		}

		metrics::frame_edge(self.frame, false);
	}

	/// The body the last frame painted in, and the column shares it was laid out with: the harness
	/// measures a frame's bands without guessing (P25).
	/// Where the cursor stands, for a test that watches a replay step move it (P25).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn cursor_for_tests(&self) -> usize {
		self.cursor()
	}

	/// How many rows are selected: the harness's own reading of the global Ctrl+A (P21).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn selected_count(&self) -> usize {
		self.selection.len()
	}

	/// Whether the find box is open, for the harness's own reading of `/` (P26).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn find_open_for_tests(&self) -> bool {
		self.find.is_some()
	}

	/// What the filter field holds, for the harness's own reading of a field's Ctrl+A (P21).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn filter_text(&self) -> Option<String> {
		self.filter.as_ref().map(|edit| edit.text.clone())
	}

	/// What a fuzzy panel's box holds, if one stands open: the harness's own reading of what
	/// Ctrl+S in the index panel is about to save.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn fuzzy_query_for_tests(&self) -> Option<String> {
		self.fuzzy.as_ref().map(|fuzzy| fuzzy.query.text.clone())
	}

	/// Which half of a fuzzy panel holds the keyboard (P33⑬), None when none stands: the harness's
	/// own reading of the index panel's two-stage focus.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn fuzzy_focus_for_tests(&self) -> Option<bool> {
		self.fuzzy.as_ref().map(|fuzzy| fuzzy.focus.inside)
	}

	/// The fuzzy panel's own rows as they stand (P40's own reading): `(zone, text)` in the walked
	/// order — what the index panel shows, zone by zone, for the tests that watch a row come and go.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn fuzzy_rows_for_tests(&self) -> Vec<(&'static str, String)> {
		let Some(fuzzy) = &self.fuzzy else { return Vec::new() };
		fuzzy.hits.iter().filter_map(|hit| fuzzy.items.get(hit.at)).map(|item| (item.zone, item.text.clone())).collect()
	}

	/// The sort the window runs and whether it is reversed (P33⑭'s guard: the status bar's words
	/// changed, never the family's behaviour — the harness reads the state back).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn sort_for_tests(&self) -> (SortBy, bool) {
		self.sort
	}

	/// The fact columns the rows carry (P33⑲'s own guard: the model changed, and the harness
	/// reads what the keys made of it).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn columns_for_tests(&self) -> Columns {
		self.columns
	}

	/// Whether a chord family is still open (P33④'s own guard: `t` opens one, and the assertion
	/// that Esc closed it reads this).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn saw_sequence_for_tests(&self) -> bool {
		self.sequence.is_some()
	}

	/// Whether the index panel is the one standing (P33⑰): what the status bar's button wears its
	/// accent for, and what the tests read after a click.
	pub(crate) fn index_open(&self) -> bool {
		self.fuzzy.as_ref().is_some_and(|fuzzy| fuzzy.mode == FuzzyMode::Index)
	}

	/// Where the last frame put the strip's `+` — beside the last chip (P33⑮) — and the corner's
	/// close (P33⑯). The harness reads the places from here.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn plus_hit_for_tests(&self) -> Option<egui::Rect> {
		self.plus_hit
	}
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn close_hit_for_tests(&self) -> Option<egui::Rect> {
		self.close_hit
	}

	/// Where the last frame put a chip's own × (P33②): the mouse's place to press.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn tab_close_hit_for_tests(&self, tab: usize) -> Option<egui::Rect> {
		self.tab_close_hits.iter().find(|(index, _)| *index == tab).map(|(_, rect)| *rect)
	}

	/// Where the last frame put a chip itself (P33②): where a test's pointer walks to hover it.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn tab_chip_hit_for_tests(&self, tab: usize) -> Option<egui::Rect> {
		self.tab_chip_hits.iter().find(|(index, _)| *index == tab).map(|(_, rect)| *rect)
	}

	/// Where the last frame put the status bar's index button (P33⑰).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn status_index_hit_for_tests(&self) -> Option<egui::Rect> {
		self.status_index
	}

	/// What is held for a paste, if anything (P33⑱'s tests read the menu's own work back).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn yank_paths_for_tests(&self) -> Vec<PathBuf> {
		self.yank.as_ref().map(|yank| yank.paths.clone()).unwrap_or_default()
	}

	/// Whether what is held is a cut (P33⑪): `Ctrl+X`'s own flag, beside the paths.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn yank_cut_for_tests(&self) -> bool {
		self.yank.as_ref().is_some_and(|yank| yank.cut)
	}

	/// Whether the replay's own steps have all been delivered (P33⑫'s harness waits): the last
	/// step lands at the end of its frame, and the tests stop waiting there instead of burning
	/// frames — a toast's own 3s run does not survive a fixed frame count on a slow machine.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn replay_done_for_tests(&self) -> bool {
		self.seed_next >= self.seed.len()
	}

	/// The undo stack's own labels, oldest first (P33⑫'s harness reading): the depth cap and the
	/// newest action, read without popping.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn undo_labels_for_tests(&self) -> Vec<&'static str> {
		self.undo.iter().map(Undo::label).collect()
	}

	/// Push an undo of the test's own through the same door a change goes through (P33⑫): the
	/// stack's depth cap is what is under test.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn note_for_tests(&mut self, undo: Undo) {
		self.note(String::new(), undo);
	}

	/// Whether the newest toast still offers its undo (P33⑫'s button).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn toast_undo_for_tests(&self) -> bool {
		self.toasts.newest_has_undo()
	}

	/// The applications page's own rows as it stands (P35's addendum): `(zone, id)` — the zone
	/// header attached to the first row of each, the walked order the tests need.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn menu_app_rows_for_tests(&self) -> Vec<(Option<&'static str>, String)> {
		self.menu
			.as_ref()
			.map(|menu| menu.app_rows().into_iter().map(|row| (row.zone, row.entry.id)).collect())
			.unwrap_or_default()
	}

	/// Where the applications page's window starts (P36②③), for the tests: the first walked row
	/// it shows — what the wheel moves and the arrows keep in sight.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn menu_scroll_for_tests(&self) -> Option<usize> {
		self.menu.as_ref().map(|menu| menu.scroll)
	}

	/// Which text modes stand, for the matrix that closes each of them (P35②): the in-place rename
	/// field.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn item_rename_for_tests(&self) -> bool {
		self.item_rename.is_some()
	}

	/// Which text modes stand, for the matrix that closes each of them (P35②): the bulk editor,
	/// the filter strip, the find strip and the help panel.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn text_modes_for_tests(&self) -> (bool, bool, bool, bool) {
		(self.bulk.is_some(), self.filter.is_some(), self.find.is_some(), self.help)
	}

	/// Make terminal launches dry (P35③): what `open_terminal` would have run is recorded, and
	/// nothing is spawned — a headless test must not open a window.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn set_terminal_dry_for_tests(&mut self) {
		self.terminal_dry = true;
	}

	/// The newest dry terminal launch (P35③): the argv and its folder, if one has arrived.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn terminal_spawn_for_tests(&mut self) -> Option<TerminalLaunch> {
		self.terminal_log.1.try_recv().ok()
	}

	/// P37①'s own dry switch: a default open records what it would have handed the system's
	/// opener instead of spawning `xdg-open` from a headless test.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn set_open_dry_for_tests(&mut self) {
		self.open_dry = true;
	}

	/// The preferences the default lookup reads, for the tests (P37④): `mimeapps.list`'s stand-in,
	/// so the assertion needs no file anywhere — the person's own is never touched.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn set_mimeapps_for_tests(&mut self, text: String) {
		self.mimeapps_for_tests = Some(text);
	}

	/// The person's level ledger a test's window reads (P38④): a path of its own, exercised live
	/// (mtime and all) and written by the merge, without touching any shared file.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn set_mimeapps_path_for_tests(&mut self, at: PathBuf) {
		self.mimeapps_path_for_tests = Some(at);
	}

	/// The system level's ledger path a test's window reads (P38④): the same, one level down.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn set_mimeapps_system_path_for_tests(&mut self, at: PathBuf) {
		self.mimeapps_system_path_for_tests = Some(at);
	}

	/// P40's own seam: the frecency store the window reads from and writes to becomes the test's
	/// file — loaded now, from there on every visit and delete lands there.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn set_frecency_path_for_tests(&mut self, at: PathBuf) {
		self.frecency = frecency::Frecency::load_at(&at);
		self.frecency_path_for_tests = Some(at);
	}

	/// The preferences the window holds for a test (P38②): what `Ctrl+Enter`'s merge wrote, read
	/// back without a file anywhere.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn mimeapps_text_for_tests(&self) -> Option<String> {
		self.mimeapps_for_tests.clone()
	}

	/// The applications panel's own rect as last drawn (P37③/P38①): `(x, y, w, h)` — the floors'
	/// promise, read from the rect rather than the formula.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn menu_panel_rect_for_tests(&self) -> Option<(f32, f32, f32, f32)> {
		self.menu_panel.map(|panel| (panel.min.x, panel.min.y, panel.width(), panel.height()))
	}

	/// The path a dry default open handed over (P37①) — the harness's own reading.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn open_spawn_for_tests(&mut self) -> Option<PathBuf> {
		self.open_log.1.try_recv().ok()
	}

	/// Turn a yank's clipboard offers off (P35④): the harness's own switch — the text road then
	/// carries the assertions, and no test ever takes the session's real selection.
	pub(crate) fn set_uri_offers_for_tests(&mut self, on: bool) {
		self.uri_offers = on;
	}

	/// The middle column's scroll offset in rows, for the harness's own reading of where the rows
	/// stand after a replay's `scroll`.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn scroll_offset_for_tests(&self) -> usize {
		self.tab().scroll.offset.round() as usize
	}

	/// The left column's own seat and scroll (P33⑧): `(seat, offset, rows, len)` — what a click
	/// and a wheel did over the parent column.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn parent_mouse_for_tests(&self) -> (Option<usize>, f32, usize, usize) {
		let parent = &self.tab().parent;
		(parent.seat, parent.scroll.offset, parent.scroll.rows, parent.scroll.len)
	}

	/// The directory the window stands in.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn cwd_for_tests(&self) -> PathBuf {
		self.tab().path().to_owned()
	}

	/// Hold a selection, for the tests that need one before a right click. `Space` is the factory
/// door now (P33⑩), and this is the door for a test that wants the state without the steps.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn hold_selection_for_tests(&mut self, paths: Vec<PathBuf>) {
		self.selection = paths.into_iter().collect();
	}

	/// The selection, sorted, for the harness's own reading of `Space` and Escape (P33⑩).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn selection_for_tests(&self) -> Vec<PathBuf> {
		let mut paths: Vec<PathBuf> = self.selection.iter().cloned().collect();
		paths.sort();
		paths
	}

	/// The index's own entries' paths, in the order they were saved.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn bookmarked_paths(&self) -> Vec<PathBuf> {
		self.bookmarks.entries.iter().map(|entry| entry.path.clone()).collect()
	}

	/// How many rows the view holds: the test side's own reading of "the listing has landed" (P25's
	/// flake fix — a replay step must never be fed into a list that is still being read).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn listed(&self) -> usize {
		self.tab().view.len()
	}

	/// Whether the tab on screen is the bin's own (P27), for the harness's own reading of `g t`.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn in_bin_for_tests(&self) -> bool {
		self.in_bin()
	}

	/// How many tabs are open, for the harness's own reading of a new one (P27).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn tabs_for_tests(&self) -> usize {
		self.tabs.len()
	}

	/// What a chip says, for the harness's own reading of the bin's title (P27).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn tab_name_for_tests(&self, at: usize) -> String {
		self.tabs[at].name()
	}

	/// Where the bin's own cursor stands (P27), for a replay step that walks it.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn bin_cursor_for_tests(&self) -> Option<usize> {
		self.bin().map(|view| view.cursor)
	}

	/// A bin list of the harness's own (P33③): what is under test is the scroll's accounting, and
	/// the real bin is not a thing a test may fill. The tab opens the way `g t` opens it; this
	/// hands the list over whole and drops any read still in flight with it.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn bin_fill_for_tests(&mut self, count: usize) {
		self.trash_job = None;
		let Some(view) = self.bin_mut() else { return };

		view.items = (0..count)
			.map(|n| trash::TrashItem {
				id:              PathBuf::from(format!("/tmp/crabhop-bin/info/item-{n:03}.trashinfo")).into_os_string(),
				name:            format!("item-{n:03}.txt").into(),
				original_parent: PathBuf::from("/tmp/crabhop-bin/src"),
				time_deleted:    1_700_000_000 + n as i64,
			})
			.collect();
		view.dirs = vec![ false; count ];
		view.loading = false;
	}

	/// The bin's scroll as the last frame left it (P33③'s accounting): the offset it draws from,
	/// the target it is walking to, and the row counts the clamp is made of — `(offset, target,
	/// rows, len)`.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn bin_scroll_for_tests(&self) -> Option<(f32, f32, usize, usize)> {
		self.bin().map(|view| (view.scroll.offset, view.scroll.target, view.scroll.rows, view.scroll.len))
	}

	/// Put a replay in the app's hands *now*, timer restarted (P25's flake fix): a test waits for
	/// the listing and only then hands the keys over, so `key j` cannot land on an empty list.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn rehearse(&mut self, steps: Vec<String>) {
		self.seed = steps;
		self.seed_next = 0;
		self.seed_at = Instant::now();
	}
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn layout(&self) -> (Rect, [f32; 3]) {
		(self.body, self.shares)
	}

	/// Where the preview's own scroll stands, when a picture is on screen (P27①): the harness's
	/// reading for the scroll-and-clip test.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn preview_offset_for_tests(&self) -> Option<f32> {
		self.preview.image_offset_for_tests()
	}

	/// A textual preview's own scroll (P37⑤): `(content, offset, view)` — `None` when there is
	/// nothing to walk. The harness's reading for the document scroll regression.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn preview_extent_for_tests(&self) -> Option<(f32, f32, f32)> {
		self.preview.extent()
	}

	/// The menu's own page and row count (P31), for the harness's walk of its pages.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn menu_state_for_tests(&self) -> Option<(&'static str, usize)> {
		self.menu.as_ref().map(|menu| {
			let page = match menu.page {
				crate::menu::Page::Main => "main",
				crate::menu::Page::Apps => "apps",
			};
			(page, menu.rows().len())
		})
	}

	/// The newest toast's own words, for the harness's test of P30's failure visibility.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn toast_text_for_tests(&self) -> Option<String> {
		self.toasts.items().last().map(|toast| toast.text.clone())
	}

	/// The failure a refusing launcher would send, for the harness's own test of P30's failure.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn open_failed_for_tests(&self, path: PathBuf) {
		let _ = self.open_failed.0.send(path);
	}

	/// The label the picture's badge shows (P28①), for the harness's own reading of a zoom.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn preview_badge_for_tests(&self) -> Option<String> {
		self.preview.badge_for_tests().map(str::to_owned)
	}

	/// How many rows the archive tree shows (P28's long tail): the harness's own reading for the
	/// fold test — `None` while the preview is not an archive.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn preview_archive_rows_for_tests(&self) -> Option<usize> {
		self.preview.archive_rows_for_tests()
	}

	/// The archive tree's rows as the pointer sees them (P28's long tail): where each stands, and
	/// whether it is a folder — the harness puts its pointer on one of them.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn preview_archive_tree_for_tests(&self) -> Option<Vec<(usize, egui::Rect, bool)>> {
		let column = chrome::columns(self.body, self.shares).preview?;
		self.preview.archive_tree(column)
	}

}

#[cfg(test)]
mod order_tests {
	use super::*;
	use std::time::{Duration, UNIX_EPOCH};

	fn entry(name: &str, size: u64, dir: bool, secs: u64) -> Entry {
		Entry {
			name: name.to_owned(),
			size,
			modified: Some(UNIX_EPOCH + Duration::from_secs(secs)),
			dir,
			mode: Some(0o644),
			link: false,
			created: None,
			uid: 0,
		}
	}

	/// `,b` / `,B` (P21): the creation time orders the list where the filesystem carries one, and a
	/// file without a creation time sorts before the ones that have it.
	#[test]
	fn the_creation_time_can_order_the_list() {
		let mut older = entry("older.txt", 1, false, 0);
		older.created = Some(UNIX_EPOCH + Duration::from_secs(1_000));
		let mut newer = entry("newer.txt", 1, false, 0);
		newer.created = Some(UNIX_EPOCH + Duration::from_secs(2_000));
		let unknown = entry("unknown.txt", 1, false, 0);

		let view = ordered(&[ newer.clone(), older.clone(), unknown.clone() ], SortBy::Btime, false, true, 0);
		let names: Vec<&str> = view.iter().map(|i| [ &newer, &older, &unknown ][*i].name.as_str()).collect();
		assert_eq!(names, vec![ "unknown.txt", "older.txt", "newer.txt" ], "oldest first, no time at the head");

		let view = ordered(&[ newer, older, unknown ], SortBy::Btime, true, true, 0);
		let names: Vec<String> = view.iter().map(|i| [ "newer.txt", "older.txt", "unknown.txt" ][*i].to_owned()).collect();
		assert_eq!(names, vec![ "newer.txt".to_owned(), "older.txt".to_owned(), "unknown.txt".to_owned() ], "and turned over: newest first");
	}

	/// The view is the rule the switches express: hidden files by their name, directories first, and
	/// the ordering the keymap asked for.
	#[test]
	fn the_view_hides_and_orders_the_way_the_switches_say() {
		let entries = vec![
			entry("b.txt", 30, false, 300),
			entry(".hidden", 10, false, 100),
			entry("a-dir", 0, true, 200),
			entry("a.txt", 20, false, 400),
		];
		let names = |view: &[usize]| view.iter().map(|i| entries[*i].name.clone()).collect::<Vec<_>>();

		assert_eq!(
			names(&ordered(&entries, SortBy::Name, false, false, 0)),
			vec![ "a-dir", "a.txt", "b.txt" ],
			"hidden out, directories first, then names"
		);
		assert_eq!(
			names(&ordered(&entries, SortBy::Name, false, true, 0)),
			vec![ "a-dir", ".hidden", "a.txt", "b.txt" ],
			"and the switch brings the dotfile back"
		);
		assert_eq!(
			names(&ordered(&entries, SortBy::Size, false, true, 0)),
			vec![ "a-dir", ".hidden", "a.txt", "b.txt" ],
			"by size among the files, directories still first"
		);
		assert_eq!(
			names(&ordered(&entries, SortBy::Mtime, true, true, 0)),
			vec![ "a-dir", "a.txt", "b.txt", ".hidden" ],
			"and reversed"
		);
		assert_eq!(ordered(&entries, SortBy::Name, false, false, 0).len(), 3, "one hidden entry is out");
	}
}

#[cfg(test)]
mod index_tests {
	use super::*;

	/// The index panel's three zones: bookmarks first (a broken one listed and flagged), then the
	/// high scores, then the recent visits — and a path already said once is not said twice.
	#[test]
	fn the_index_gathers_three_zones_without_repeating_itself() {
		let books = vec![
			crate::book::Bookmark { name: "tmp".to_owned(), path: PathBuf::from("/tmp") },
			crate::book::Bookmark { name: "gone".to_owned(), path: PathBuf::from("/tmp/crabhop-p14-never/gone") },
		];
		let ranked = vec![
			(PathBuf::from("/home/x/dev"), 9.5),
			(PathBuf::from("/home/x"), 8.0),
			(PathBuf::from("/etc"), 1.0),
		];
		let recent = vec![
			(PathBuf::from("/home/x"), 100),
			(PathBuf::from("/home/x/dev"), 90),
			(PathBuf::from("/var/log"), 80),
		];

		let rows = index_rows(&books, &ranked, &recent);
		let zones: Vec<&str> = rows.iter().map(|row| row.zone).collect();
		assert_eq!(
			zones,
			vec![ "索引", "索引", "高频", "高频", "高频", "最近" ],
			"three zones in order, and the recent one keeps only what the others had not said"
		);

		assert!(matches!(&rows[0].payload, FuzzyPayload::Index(name) if name == "tmp"), "entries carry their name");
		assert_eq!(rows[0].text, "/tmp", "the row is the path and nothing else: {:?}", rows[0].text);
		assert!(!rows[0].broken, "a path that is there is not flagged");
		assert!(rows[1].broken, "and one that is not there is");
		assert!(rows[5].text.contains("log"), "the recent zone keeps what the others had not said: {:?}", rows[5].text);
		assert!(zones.iter().all(|zone| !zone.is_empty()), "every row knows its zone");
	}
}

#[cfg(test)]
mod filter_tests {
	use super::*;

	/// The pattern is a case-insensitive substring, and an empty one answers everything.
	#[test]
	fn a_filter_is_a_case_insensitive_substring() {
		assert!(matches("re", "Report-Final.pdf"));
		assert!(matches("REPORT", "report-final.pdf"));
		assert!(matches("鸭", "鸭子账本.xlsx"));
		assert!(!matches("zzz", "report-final.pdf"));
		assert!(matches("", "anything at all"), "no pattern, nothing filtered");
	}
}

#[cfg(test)]
mod link_tests {
	use super::*;

	/// The relative symlink's own arithmetic: the common prefix drops away, levels become `..`,
	/// and the walk lands exactly on the target.
	#[test]
	fn a_relative_target_walks_from_the_links_own_directory() {
		assert_eq!(relative_target(Path::new("/a/x"), Path::new("/a/b/c")), PathBuf::from("../b/c"));
		assert_eq!(relative_target(Path::new("/a/b"), Path::new("/a/b/c")), PathBuf::from("c"));
		assert_eq!(relative_target(Path::new("/a/b/c/d"), Path::new("/a/b")), PathBuf::from("../.."));
		assert_eq!(relative_target(Path::new("/a/b"), Path::new("/a/b")), PathBuf::from(""));

		// and the walk is its own inverse check: from the link's directory it lands back on `to`
		let joint = Path::new("/a/b").join(relative_target(Path::new("/a/b"), Path::new("/a/c/d")));
		assert_eq!(joint, PathBuf::from("/a/b/../c/d"));
	}
}

#[cfg(test)]
mod sort_tests {
	use super::*;

	fn entry(name: &str) -> Entry {
		Entry { name: name.to_owned(), size: 0, modified: None, dir: false, mode: None, link: false , created: None, uid: 0 }
	}

	fn names(view: &[usize], entries: &[Entry]) -> Vec<String> {
		view.iter().map(|i| entries[*i].name.clone()).collect()
	}

	/// The natural sort reads numbers inside names as numbers: `file2` before `file10`, `a1b2`
	/// before `a1b10` — the order a person's eye reads, not the byte order's.
	#[test]
	fn the_natural_order_counts_the_way_a_person_counts() {
		assert!(natural("file2", "file10") == std::cmp::Ordering::Less);
		assert!(natural("a1b2", "a1b10") == std::cmp::Ordering::Less);
		assert!(natural("FileB", "filea") == std::cmp::Ordering::Greater, "case folds");
		assert!(natural("file", "file2") == std::cmp::Ordering::Less, "a name that ran out comes first");
		assert!(natural("file2", "file2") == std::cmp::Ordering::Equal);

		let entries = vec![ entry("file10"), entry("file2"), entry("File1") ];
		assert_eq!(
			names(&ordered(&entries, SortBy::Natural, false, true, 0), &entries),
			vec![ "File1", "file2", "file10" ]
		);
		assert_eq!(
			names(&ordered(&entries, SortBy::Natural, true, true, 0), &entries),
			vec![ "file10", "file2", "File1" ],
			"and `,N` turns it over"
		);
	}

	/// The random sort is arbitrary but not fickle: one seed, one order — however often the list
	/// is rebuilt — and another seed is another draw.
	#[test]
	fn the_random_order_is_stable_for_a_seed_and_a_permutation_always() {
		let entries: Vec<Entry> = [ "a", "b", "c", "d", "e", "f", "g", "h" ].iter().map(|n| entry(n)).collect();

		let first = ordered(&entries, SortBy::Random, false, true, 7);
		let again = ordered(&entries, SortBy::Random, false, true, 7);
		assert_eq!(first, again, "the same seed is the same order");

		let mut sorted = names(&first, &entries);
		sorted.sort();
		assert_eq!(sorted, vec![ "a", "b", "c", "d", "e", "f", "g", "h" ], "and a permutation of the whole");

		assert_ne!(shuffle_key("a", 7), shuffle_key("a", 8), "a new press is a new key");
	}
}

#[cfg(test)]
#[cfg(test)]
mod query_tests {
	use super::*;

	/// The index panel's Enter reads what was typed first (P22), in the table's own order: a path
	/// that is there wins over the row, a path that is not there says so, and anything else is the
	/// panel's own jump. `~` expands, a relative path resolves against the cwd, and `.`/`..` fold
	/// without the filesystem being asked.
	#[test]
	fn the_query_reads_as_a_path_before_it_reads_as_a_search() {
		let dir = std::env::temp_dir().join(format!("crabhop-query-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(dir.join("sub/inner")).expect("a tree");
		std::fs::write(dir.join("a.txt"), b"x").expect("a file");

		let cwd = dir.clone();
		let home = || Some(dir.clone());

		// ① a path that exists — absolute, home-relative, cwd-relative, with `..` folded
		assert_eq!(
			query_enter(dir.join("sub").to_str().expect("utf-8"), &cwd, home()),
			QueryEnter::Go(dir.join("sub"))
		);
		assert_eq!(query_enter("~/sub", &cwd, home()), QueryEnter::Go(dir.join("sub")), "`~` expands to the home");
		assert_eq!(
			query_enter("sub/inner", &cwd, home()),
			QueryEnter::Go(dir.join("sub/inner")),
			"a relative path resolves against the cwd"
		);
		assert_eq!(
			query_enter("sub/inner/..", &cwd, home()),
			QueryEnter::Go(dir.join("sub")),
			"`..` folds without asking the filesystem"
		);
		assert_eq!(query_enter("./a.txt", &cwd, home()), QueryEnter::Go(dir.join("a.txt")), "a file is gone to as well: `go` reveals it");

		// ② it reads like a path and is not there
		assert_eq!(query_enter("/nowhere/at/all", &cwd, home()), QueryEnter::Missing(PathBuf::from("/nowhere/at/all")));
		assert_eq!(query_enter("./missing", &cwd, home()), QueryEnter::Missing(cwd.join("missing")));
		assert_eq!(query_enter("~/nowhere", &cwd, home()), QueryEnter::Missing(dir.join("nowhere")));
		assert_eq!(
			query_enter("sub/nowhere", &cwd, home()),
			QueryEnter::Missing(cwd.join("sub/nowhere")),
			"a name that is not there under a directory that is"
		);

		// ③ not a path at all: the row decides
		assert_eq!(query_enter("paper", &cwd, home()), QueryEnter::Selection);
		assert_eq!(query_enter("sub", &cwd, home()), QueryEnter::Selection, "a bare name is a search word, not a path");
		assert_eq!(query_enter("", &cwd, home()), QueryEnter::Selection);
		assert_eq!(query_enter("   ", &cwd, home()), QueryEnter::Selection);
		assert_eq!(query_enter("~", &cwd, None), QueryEnter::Selection, "a bare `~` with no home is not a path this window knows");
		assert_eq!(
			query_enter("~user/x", &cwd, None),
			QueryEnter::Missing(cwd.join("~user/x")),
			"another user's home is a path we cannot resolve — and it is not there"
		);

		// and the words are trimmed before they are read
		assert_eq!(query_enter("  ~/sub  ", &cwd, home()), QueryEnter::Go(dir.join("sub")));

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// The index panel's keys are the table's own (P24): `Del` deletes and `Ctrl+S` saves the
	/// directory the window stands in — `F2`, `a` and `A` have no door here any more.
	#[test]
	fn the_index_panels_doors_are_delete_and_save_only() {
		assert_eq!(index_door(Key::Delete, false), Some(IndexDoor::Delete), "Del deletes");
		assert_eq!(index_door(Key::S, true), Some(IndexDoor::Save), "Ctrl+S saves the cwd");
		assert_eq!(index_door(Key::S, false), None, "a bare s types into the query");
		assert_eq!(index_door(Key::F2, false), None, "F2's rename is gone");
		assert_eq!(index_door(Key::A, false), None, "`a` is a search word now");
		assert_eq!(index_door(Key::A, true), None, "and so is `A`");
		assert_eq!(index_door(Key::Enter, false), None, "Enter is the panel's own, not a door here");
	}

	/// `/`'s own two rules (P21): the letters a name holds are found case-folded and all of them,
	/// and the arrows walk the view to the next name with them.
	#[test]
	fn the_find_finds_letters_and_the_row_that_holds_them() {
		assert_eq!(find_hits("Readme.md", "me"), vec![ 4, 5 ], "the letters' own positions");
		assert_eq!(find_hits("readme.md", "ME"), vec![ 4, 5 ], "and the case folds");
		assert_eq!(find_hits("aaa", "a"), vec![ 0, 1, 2 ], "every occurrence is coloured");
		assert_eq!(find_hits("中文名字", "名字"), vec![ 2, 3 ], "wide letters count as one each");
		assert!(find_hits("aaa", "b").is_empty());
		assert!(find_hits("aaa", "").is_empty(), "an empty search colours nothing");

		let entries: Vec<Entry> = [ "alpha.txt", "beta.md", "gamma.txt", "delta.txt" ]
			.iter()
			.map(|name| Entry {
				name:     (*name).to_owned(),
				size:     0,
				modified: None,
				dir:      false,
				mode:     None,
				link:     false,
				created:  None,
				uid:      0,
			})
			.collect();
		let view = vec![ 0, 1, 2, 3 ];

		assert_eq!(find_row(&view, &entries, "txt", 0, true), Some(2), "the next one with the letters");
		assert_eq!(find_row(&view, &entries, "txt", 2, true), Some(3));
		assert_eq!(find_row(&view, &entries, "txt", 3, true), None, "and nothing past the last");
		assert_eq!(find_row(&view, &entries, "txt", 3, false), Some(2), "backwards it is the one before");
		assert_eq!(find_row(&view, &entries, "md", 0, true), Some(1), "anywhere in the name counts");
		assert_eq!(find_row(&view, &entries, "nothing", 0, true), None);
	}

	/// The focus machine both panels share (P33⑬): the list has the keyboard when a panel opens,
	/// `f` (the help panel's door) or a click on the box moves it inside, and the first Esc steps
	/// back out — the second is the panel's own to close.
	/// The status bar's right group (P33⑭): the filter's prefix, the sort's word — mtime in
	/// Chinese — and the arrow, and never a linemode word any more.
	#[test]
	fn the_status_bar_says_the_sort_and_never_the_linemode() {
		let mtime = right_group((SortBy::Mtime, false), false, None);
		assert_eq!(mtime, "修改时间↑", "mtime is spoken in Chinese: {mtime}");
		assert!(!mtime.contains("mtime") && !mtime.contains("行模式"), "and the linemode cell is gone: {mtime}");
		assert_eq!(right_group((SortBy::Size, true), false, Some("*.rs")), "筛选:*.rs  大小↓", "descending says so with its arrow");
		assert_eq!(right_group((SortBy::Natural, false), true, Some("*")), "符号链接  自然↑", "a symlink listing says so first");
		assert_eq!(right_group((SortBy::Btime, false), false, None), "创建时间↑", "and btime keeps its own name");
	}

	#[test]
	fn a_panel_focus_steps_in_on_f_and_out_on_the_first_escape() {
		// the machine both panels share (P33⑬): the list has the keyboard when the panel opens,
		// `f` (or a click on the box) moves it inside, and the first Esc steps back out — the
		// second is the panel's own to close
		let mut focus = PanelFocus::default();
		assert!(!focus.inside, "a panel opens on its list");
		focus.enter();
		assert!(focus.inside, "f and the box's click put the keyboard in the box");
		assert!(focus.escape(), "the first Esc leaves the box");
		assert!(!focus.inside, "and the list has it again");
		assert!(!focus.escape(), "the second Esc says the panel itself may close");
	}

	/// The help panel's rows are the keymap's own, filtered by what was typed (P21's `?`): an empty
	/// query keeps them all, a word keeps the rows whose key or words hold it, and nonsense keeps
	/// none. Its `#[test]` had gone missing (the attribute above was the focus machine's own): it
	/// runs again.
	#[test]
	fn the_help_filters_its_own_rows() {
		let rows = vec![
			("h".to_owned(), "leave".to_owned(), "返回上级目录".to_owned()),
			("g t".to_owned(), "plugin trash".to_owned(), "回收站".to_owned()),
			("m p".to_owned(), "linemode permissions".to_owned(), "行模式：显示权限".to_owned()),
			(", b".to_owned(), "sort btime --reverse=no".to_owned(), "按创建时间正序".to_owned()),
		];

		// the panel's own size (P33①): the columns say the width, the rows the height, and the
		// height never passes seventy percent of the window
		let area = Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(1200.0, 800.0));
		let (narrow, short) = help_size(area, 4, 8, 3, 9.0, 24.0);
		let (wide, tall) = help_size(area, 12, 30, 200, 9.0, 24.0);
		assert!(wide > narrow, "the columns decide the width: {narrow} → {wide}");
		assert!((short - 24.0 * 8.5).abs() < 0.01, "a handful of rows is a short panel, floored at four rows: {short}");
		assert!(tall <= 800.0 * 0.7 + 0.01, "and the height stops at seventy percent: {tall}");
		assert!(tall > short, "more rows, a taller panel: {short} → {tall}");

		assert_eq!(help_rows(&rows, ""), vec![ 0, 1, 2, 3 ], "nothing typed: the whole table");
		assert_eq!(help_rows(&rows, "  "), vec![ 0, 1, 2, 3 ], "and spaces are nothing");
		assert_eq!(help_rows(&rows, "回收站"), vec![ 1 ], "the words a person reads are searched too");
		assert_eq!(help_rows(&rows, "M P"), vec![ 2 ], "letters ignore their case");
		assert!(help_rows(&rows, "plugin trash").is_empty(), "a run that is not on screen is not searched");
		assert!(help_rows(&rows, "linemode").is_empty(), "and neither is half of one");
		assert_eq!(help_rows(&rows, "m p"), vec![ 2 ], "and the key itself counts");
		assert_eq!(help_rows(&rows, "mp"), vec![ 2 ], "a chord typed without its space finds its row");
		assert!(help_rows(&rows, "没有这一条").is_empty(), "nonsense keeps none");
	}

	/// `M`'s panel answers the nine options the table lists (P21): `p` mounts, `m` mounts and goes,
	/// `R` remounts the cwd's device, `u`/`U` unmount (force on the shifted one), `g` jumps, and
	/// `a`/`e`/`r` are the URI family — while a letter that is not one of them is left to typing.
	#[test]
	fn the_device_panels_doors_read_their_keys() {
		assert_eq!(device_door(Key::P, false), Some(DeviceDoor::Mount));
		assert_eq!(device_door(Key::M, false), Some(DeviceDoor::MountAndJump));
		assert_eq!(device_door(Key::R, true), Some(DeviceDoor::Remount));
		assert_eq!(device_door(Key::R, false), Some(DeviceDoor::Uri("移除")));
		assert_eq!(device_door(Key::U, false), Some(DeviceDoor::Unmount { force: false }));
		assert_eq!(device_door(Key::U, true), Some(DeviceDoor::Unmount { force: true }));
		assert_eq!(device_door(Key::G, false), Some(DeviceDoor::Jump));
		assert_eq!(device_door(Key::A, false), Some(DeviceDoor::Uri("添加")));
		assert_eq!(device_door(Key::E, false), Some(DeviceDoor::Uri("编辑")));
		assert_eq!(device_door(Key::J, false), None, "a letter that is not one of them is typing");
		assert_eq!(device_door(Key::P, true), None, "and the shifted one is not the option");
	}

	/// The footer's words are the terminal ones (P24): the four keys, and the box's own path rule
	/// under them — the `A 添加该目录索引` wording is gone.
	#[test]
	fn the_index_footers_words_are_the_final_ones() {
		assert_eq!(INDEX_HINT, "Enter 跳转 · Ctrl+S 保存目录为索引 · Del 删除所选 · i 再按跳输入框 · Esc 退出");
		assert!(!INDEX_HINT.contains("A 添加"), "the old add wording is replaced");
		assert!(!INDEX_HINT.contains("F2"), "and the rename is not advertised");
	}
}

#[cfg(test)]
mod cd_tests {
	use super::*;

	/// `~` and `~/x` expand against the home; a plain path is itself; and a missing home is not a
	/// path at all.
	#[test]
	fn the_tilde_expands_the_way_a_shell_expands_it() {
		let home = || Some(PathBuf::from("/home/x"));

		assert_eq!(expand_tilde("~", home()), Some(PathBuf::from("/home/x")));
		assert_eq!(expand_tilde("~/Desktop/", home()), Some(PathBuf::from("/home/x/Desktop/")));
		assert_eq!(expand_tilde("/etc", home()), Some(PathBuf::from("/etc")));
		assert_eq!(expand_tilde("~/a", None), None, "no home, no path");
	}
}

#[cfg(test)]
mod bypass_tests {
	use super::*;

	fn entry(name: &str, dir: bool) -> Entry {
		Entry { name: name.to_owned(), size: 0, modified: None, dir, mode: None, link: false , created: None, uid: 0 }
	}

	/// The bypass walks a single-child chain and nothing else: a second row stops it, a file
	/// stops it, and the hidden switch decides what "a single child" means.
	#[test]
	fn the_bypass_descends_only_a_single_child_chain() {
		let dir = Path::new("/a");
		let one_dir = vec![ entry("b", true) ];
		assert_eq!(bypass_child(dir, &one_dir, SortBy::Name, false, false), Some(PathBuf::from("/a/b")));

		let one_file = vec![ entry("b.txt", false) ];
		assert_eq!(bypass_child(dir, &one_file, SortBy::Name, false, false), None, "a file is the end of the walk");

		let two = vec![ entry("b", true), entry("c", true) ];
		assert_eq!(bypass_child(dir, &two, SortBy::Name, false, false), None, "two children: the walk stops");

		let hidden_only = vec![ entry(".b", true) ];
		assert_eq!(bypass_child(dir, &hidden_only, SortBy::Name, false, false), None, "hidden rows are not on show");
		assert_eq!(
			bypass_child(dir, &hidden_only, SortBy::Name, false, true),
			Some(PathBuf::from("/a/.b")),
			"but the switch brings them back"
		);
	}
}

#[cfg(test)]
mod follow_tests {
	use super::*;

	/// A symlink's target is read the way the kernel reads it: an absolute one stands alone, a
	/// relative one is relative to the link's own directory.
	#[test]
	fn a_symlinks_target_is_resolved_against_its_own_directory() {
		assert_eq!(followed(Path::new("/a/b"), Path::new("/etc/hosts")), PathBuf::from("/etc/hosts"));
		assert_eq!(followed(Path::new("/a/b"), Path::new("../c")), PathBuf::from("/a/b/../c"));
		assert_eq!(followed(Path::new("/"), Path::new("mnt")), PathBuf::from("/mnt"));
	}
}

#[cfg(test)]
mod history_tests {
	use super::*;

	/// The trail `H`/`L` walk: arrivals are kept, a new arrival after going back cuts the future
	/// away, and travel only moves where there is somewhere to go.
	#[test]
	fn the_trail_keeps_arrivals_and_cuts_the_future() {
		let mut history = Trail::default();
		for dir in [ "/a", "/b", "/c" ] {
			history.push(Path::new(dir));
		}
		assert_eq!(history.at(), 2);

		assert_eq!(history.travel(true).as_deref(), Some(Path::new("/b")));
		assert_eq!(history.travel(true).as_deref(), Some(Path::new("/a")));
		assert_eq!(history.travel(true), None, "the trail's start is the end of the walk");
		assert_eq!(history.travel(false).as_deref(), Some(Path::new("/b")), "and forward walks it again");

		// a new arrival from the middle: /c is a future that no longer exists
		history.push(Path::new("/d"));
		assert_eq!(history.at(), 2);
		assert_eq!(history.travel(false), None, "nothing after /d");
		assert_eq!(history.travel(true).as_deref(), Some(Path::new("/b")));

		// arriving where it already stands is not a new step
		history.push(Path::new("/b"));
		assert_eq!(history.travel(true).as_deref(), Some(Path::new("/a")));
	}
}

#[cfg(test)]
mod selection_tests {
	use super::*;

	/// `<C-a>` selects every row on show, `<C-r>` turns the selection over, and `--state=off`
	/// clears — over the view's own rows, so nothing outside the list is touched.
	#[test]
	fn select_all_invert_and_clear_act_on_the_rows_they_are_given() {
		let rows: Vec<PathBuf> = [ "/a", "/b", "/c" ].iter().map(PathBuf::from).collect();
		let mut selection = HashSet::new();

		toggled(&mut selection, &rows, Some(true));
		assert_eq!(selection.len(), 3, "all of them");
		assert!(selection.contains(Path::new("/b")));

		toggled(&mut selection, &rows, None);
		assert!(selection.is_empty(), "and the turn takes them all back");

		selection.insert(PathBuf::from("/a"));
		toggled(&mut selection, &rows, None);
		assert_eq!(selection.len(), 2, "a turn over a partial selection fills the rest");
		assert!(!selection.contains(Path::new("/a")));

		toggled(&mut selection, &rows, Some(false));
		assert!(selection.is_empty(), "and the off-state clears");
	}
}

#[cfg(test)]
mod typed_tests {
	use super::*;

	/// The three ways characters reach a field (P18): the layout's own text, a paste, and an
	/// input method's commit. A keystroke that typed nothing contributes nothing.
	#[test]
	fn typing_pasting_and_an_input_method_all_count_as_text() {
		let events = vec![
			egui::Event::Text("测".to_owned()),
			egui::Event::Key {
				key:             Key::A,
				physical_key:    None,
				pressed:         true,
				repeat:          false,
				modifiers:       Modifiers::NONE,
			},
		];
		assert_eq!(typed_text(&events), "测", "the layout's text counts, a bare key does not");

		let pasted = vec![ egui::Event::Paste("粘贴的词".to_owned()) ];
		assert_eq!(typed_text(&pasted), "粘贴的词", "a paste is text");

		let ime = vec![ egui::Event::Ime(egui::ImeEvent::Commit("中文过滤".to_owned())) ];
		assert_eq!(typed_text(&ime), "中文过滤", "and so is an input method's commit");

		assert_eq!(typed_text(&[]), "");
	}
}

#[cfg(test)]
mod facts_tests {
	use super::*;

	/// The P12 rule, pinned at the row's own content: only the middle column's rows carry the
	/// size and time — the parent column and the preview's listing ask for none, so a narrow
	/// window has nothing to squeeze together.
	#[test]
	fn only_the_middle_column_shows_the_facts() {
		let advance = FONT_SIZE * 0.62;
		let when = "10-19 09:03".to_owned();
		let facts = |when: &str| list::Facts {
			size:  "1.2K".to_owned(),
			when:  when.to_owned(),
			btime: "10-19 09:03".to_owned(),
			perms: "rwxr-xr-x".to_owned(),
			owner: "LinXuan".to_owned(),
		};

		assert_eq!(
			list::row_facts(PARENT_META, Columns::default(), &facts(&when), 640.0, advance),
			None,
			"the parent column: icon and name alone, however wide it is"
		);
		assert_eq!(
			list::row_facts(PARENT_META, Columns { time: None, perms: true, owner: true }, &facts(&when), 2_000.0, advance),
			None,
			"and no set of columns can bring them back"
		);

		assert_eq!(
			list::row_facts(CURRENT_META, Columns::default(), &facts(&when), 640.0, advance).as_deref(),
			Some("1.2K  10-19 09:03"),
			"the middle column keeps the pair"
		);
	}
}

#[cfg(test)]
mod click_tests {
	use super::*;

	/// The click's own rule: a hit row is a view position, the blank below the rows is nothing —
	/// and it is a *view* position even when the list is scrolled.
	#[test]
	fn a_click_seats_the_row_it_hit_and_only_that_row() {
		assert_eq!(clicked_row(0, 0, 60), Some(0));
		assert_eq!(clicked_row(0, 6, 60), Some(6), "the k-th drawn row is the k-th view position");
		assert_eq!(clicked_row(4, 2, 60), Some(6), "and a scrolled list counts from its first row");
		assert_eq!(clicked_row(0, 60, 60), None, "past the last row: the blank decides nothing");
		assert_eq!(clicked_row(0, 999, 60), None);
		assert_eq!(clicked_row(0, 0, 0), None, "an empty list has nothing to seat");
	}
}

#[cfg(test)]
mod parent_tests {
	use super::*;

	fn view(path: &str) -> View { View { path: PathBuf::from(path), cursor: 3 } }

	/// The parent column always has a path: the stack's own step when there is one, the cwd's
	/// filesystem parent otherwise — `None` only for the root, which has no parent to list.
	#[test]
	fn the_parent_column_finds_a_parent_under_any_cwd() {
		let stack = [ view("/home/x"), view("/home/x/Downloads") ];
		assert_eq!(
			parent_path(&stack, Path::new("/home/x/Downloads")),
			Some((PathBuf::from("/home/x"), Some(3))),
			"a step the tab took: the stack's own cursor"
		);
		assert_eq!(
			parent_path(&stack[..1], Path::new("/home/x")),
			Some((PathBuf::from("/home"), None)),
			"a tab at its first directory still lists where it is"
		);
		assert_eq!(parent_path(&stack[..1], Path::new("/")), None, "the root has no parent");

		let deep = [ view("/a"), view("/a/b"), view("/a/b/c") ];
		assert_eq!(
			parent_path(&deep, Path::new("/a/b/c")),
			Some((PathBuf::from("/a/b"), Some(3))),
			"deeper stacks count back one step"
		);
	}

	/// A `g a` / `g d` jump resets the stack to a single view, and `h` still has a directory to
	/// leave to: the parent column's own, which is the cwd's filesystem parent (P18 — `h` is never
	/// a no-op). The name is what the cursor seats itself on: the directory it came out of.
	#[test]
	fn a_jump_that_resets_the_stack_still_leaves_somewhere_to_go() {
		let jumped = [ view("/home/x/Desktop") ];
		assert_eq!(
			parent_path(&jumped, Path::new("/home/x/Desktop")),
			Some((PathBuf::from("/home/x"), None)),
			"one deep after `g a`, and the filesystem parent is the way up"
		);

		assert_eq!(file_name(Path::new("/home/x/Desktop")).as_deref(), Some("Desktop"), "the row to wait for");
		assert_eq!(file_name(Path::new("/")), None, "the root has no name to sit on");
	}
}

#[cfg(test)]
mod hover_tests {
	use super::*;

	/// The pointer's row is the row under it, and nothing else: above the column, below it or beside
	/// it are not rows, and the row that is found is a position in the view.
	#[test]
	fn the_hover_row_is_the_row_under_the_pointer() {
		let column = Rect::from_min_size(egui::pos2(100.0, 10.0), Vec2::new(300.0, 320.0));

		assert_eq!(hover_at(Some(egui::pos2(120.0, 10.0)), column, 32.0, 0), Some(0), "the first row's very top");
		assert_eq!(hover_at(Some(egui::pos2(120.0, 41.9)), column, 32.0, 0), Some(0), "and its very bottom");
		assert_eq!(hover_at(Some(egui::pos2(120.0, 42.0)), column, 32.0, 0), Some(1), "the next one starts there");
		assert_eq!(hover_at(Some(egui::pos2(120.0, 74.0)), column, 32.0, 7), Some(9), "and it is a view position");
		assert_eq!(hover_at(Some(egui::pos2(120.0, 9.0)), column, 32.0, 0), None, "above the column is nowhere");
		assert_eq!(hover_at(Some(egui::pos2(99.0, 40.0)), column, 32.0, 0), None, "and so is beside it");
		assert_eq!(hover_at(None, column, 32.0, 0), None, "no pointer, no hover");
	}
}

#[cfg(test)]
mod tab_tests {
	use super::*;

	/// Switching tabs: a number clamps to the tabs that exist, a relative step wraps around them.
	#[test]
	fn a_switch_clamps_an_index_and_wraps_a_step() {
		assert_eq!(switch_target(5, 2, 0, false), 0, "1 goes to the first tab");
		assert_eq!(switch_target(5, 2, 8, false), 4, "9 clamps to the last");
		assert_eq!(switch_target(5, 0, -1, true), 4, "the previous tab wraps");
		assert_eq!(switch_target(5, 4, 1, true), 0, "and so does the next");
		assert_eq!(switch_target(1, 0, 1, true), 0, "one tab is still one tab");
	}

	/// The chip's number badge: it takes its own room, and past nine it is the number it is —
	/// a tenth tab's badge is a digit wider, not a clamped "9".
	#[test]
	fn a_chips_number_takes_room_and_grows_past_nine() {
		assert!(number_width(0) > 0.0, "the badge takes room");
		assert_eq!(number_width(0), number_width(8), "one digit, one width");
		assert!(number_width(9) > number_width(8), "the tenth tab's number is two digits");
		assert!(number_width(99) > number_width(9), "and the hundredth, three");
	}

	/// A tab says its directory's name until the person gives it one of their own.
	#[test]
	fn a_tab_is_named_by_its_directory_or_by_hand() {
		let mut tab = Tab::at(PathBuf::from("/home/someone/Downloads"));
		assert_eq!(tab.name(), "Downloads");
		tab.title = Some("下好的东西".to_owned());
		assert_eq!(tab.name(), "下好的东西");
	}

	/// The bin's own tab says 回收站 — and a name the person typed still wins (P27).
	#[test]
	fn a_bin_tab_is_named_by_the_bin() {
		let mut tab = Tab::at(PathBuf::from("/home/someone/Downloads"));
		tab.trash = Some(TrashView { items: Vec::new(), dirs: Vec::new(), cursor: 0, loading: true, scroll: Scroll::default() });
		assert_eq!(tab.name(), "回收站", "the chip is the bin's own word");
		tab.title = Some("垃圾堆".to_owned());
		assert_eq!(tab.name(), "垃圾堆", "a name by hand is still the person's");
	}
}