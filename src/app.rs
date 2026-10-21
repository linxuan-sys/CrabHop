
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

const SKELETON_AFTER: Duration = Duration::from_millis(120);

pub(crate) const PARENT_META: bool = false;
pub(crate) const CURRENT_META: bool = true;
const INDEX_HINT: &str = "Enter 跳转 · Ctrl+S 保存目录为索引 · Del 删除所选 · i 再按跳输入框 · Esc 退出";
const DEVICE_HINT: &str = "↑↓ 选择 · Enter 挂载并跳转 · p 挂载 · m 挂载并跳转 · g 跳到挂载点 · Esc 关闭";
const DEVICE_HINT_UNMOUNT: &str = "↑↓ 选择 · Enter 卸载并弹出 · u 卸载 · U 强制卸载 · Esc 关闭";
const DEVICE_HINT_URI: &str = "R 重挂当前设备 · a 添加 URI · e 编辑 URI · r 移除 URI（远程族暂不支持） · 鼠标双击执行";
const NOTICE_ALIVE: Duration = Duration::from_secs(2);
const SPOT_WIDTH: f32 = 360.0;
const TAB_LIMIT: usize = 10;
const UNDO_DEPTH: usize = 10;
const BAR_IDLE: Duration = Duration::from_millis(1200);
const WHICH_TIMEOUT: Duration = Duration::from_secs(10);
const TRASH_HINT: f32 = 24.0;

const CHIP_MIN: f32 = 100.0;
const CHIP_MAX: f32 = 220.0;
const CHIP_GAP: f32 = 6.0;
const CHIP_PAD: f32 = 34.0;

struct View {
	path:   PathBuf,
	cursor: usize,
}

type ViewKey = (usize, PathBuf, bool, (SortBy, bool), Option<String>, bool);

#[derive(Default)]
pub(crate) struct Trail {
	dirs: Vec<PathBuf>,
	at:   usize,
}

impl Trail {
	fn push(&mut self, dir: &Path) {
		if self.dirs.get(self.at).map(PathBuf::as_path) == Some(dir) {
			return;
		}

		self.dirs.truncate(self.at + 1);
		self.dirs.push(dir.to_owned());
		self.at = self.dirs.len() - 1;
	}

	fn travel(&mut self, back: bool) -> Option<PathBuf> {
		let at = if back { self.at.checked_sub(1)? } else { self.at + 1 };
		self.dirs.get(at).cloned().inspect(|_| self.at = at)
	}

	#[cfg(test)]
	fn at(&self) -> usize { self.at }
}

#[derive(Default)]
struct ParentView {
	seat:   Option<usize>,
	scroll: Scroll,
	path:   Option<PathBuf>,
}

struct Tab {
	stack:   Vec<View>,
	history: Trail,
	view:    Vec<usize>,
	view_of: Option<ViewKey>,
	scroll:  Scroll,
	title:   Option<String>,
	filter:  Option<String>,
	symlinks: bool,
	parent:  ParentView,
	seat:    Option<(PathBuf, String)>,
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

#[derive(Clone)]
struct Progress {
	title:    String,
	fraction: Option<f32>,
	rate:     Option<String>,
}

struct DeviceJob {
	do_: DeviceDo,
	rx:  std::sync::mpsc::Receiver<Answer>,
}

enum Answer {
	Devices(Result<Vec<mount::Device>, String>),
	At(Result<std::path::PathBuf, String>),
	Done(Result<(), String>),
}

#[derive(Clone)]
struct Yank {
	paths: Vec<PathBuf>,
	cut:   bool,
}

struct BulkEditor {
	edit:  Edit,
	lines: Vec<bulk::Line>,
}

struct ItemRename {
	path: PathBuf,
	edit: Edit,
	note: Option<&'static str>,
}

struct Cmdline {
	block:  bool,
	edit:   Edit,
	comp:   Option<Completions>,
	recall: Option<usize>,
}

struct Output {
	title: String,
	code:  Option<i32>,
	lines: Vec<(String, bool)>,
	from:  usize,
}

struct OpenWith {
	paths:  Vec<PathBuf>,
	name:   String,
	edit:   Edit,
	comp:   Option<Completions>,
	recall: Option<usize>,
}

#[derive(Clone, Copy, PartialEq)]
enum FuzzyMode {
	Index,
	Devices {
		unmount: bool,
		force:   bool,
	},
	FindNames,
	FindText,
}

#[derive(Clone)]
struct FuzzyItem {
	text:    String,
	right:   Option<String>,
	payload: FuzzyPayload,
	zone:    &'static str,
	broken:  bool,
}

impl FuzzyItem {
	fn plain(text: String, right: Option<String>, payload: FuzzyPayload) -> Self {
		Self { text, right, payload, zone: "", broken: false }
	}
}

#[derive(Clone)]
enum FuzzyPayload {
	Path(PathBuf),
	Index(String),
	Device(String),
}

enum IndexEntry {
	Bookmark(String),
	Frecency(PathBuf, &'static str),
}

struct FilterJob {
	epoch: u64,
	rx:  std::sync::mpsc::Receiver<Vec<fuzzy::Hit>>,
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub(crate) struct PanelFocus {
	pub(crate) inside: bool,
}

impl PanelFocus {
	pub(crate) fn enter(&mut self) { self.inside = true; }

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
	focus:   PanelFocus,
	matcher: nucleo_matcher::Matcher,
	job:     Option<FilterJob>,
	epoch:     u64,
	finder:  Option<search::Job>,
	since:   Instant,
	loading: bool,
	filled:  bool,
	notice:  Option<(String, Instant)>,
}

struct Prompt {
	title:  String,
	edit:   Edit,
	action: PromptDo,
}

#[derive(Clone)]
enum PromptDo {
	Compress(Vec<PathBuf>),
	Create,
	Link(LinkKind),
}

struct Confirm {
	text:   String,
	names:  Vec<String>,
	action: ConfirmDo,
}

#[derive(Clone, PartialEq)]
enum ConfirmDo {
	Delete(Vec<PathBuf>),
	Purge(Vec<trash::TrashItem>),
	Empty,
	Paste {
		cut:   bool,
		force: bool,
	},
	RestoreAs(trash::TrashItem, PathBuf),
}

enum TrashJob {
	Delete(trash::Job<()>, Vec<PathBuf>),
	Restore(trash::Job<()>, Vec<PathBuf>, String),
	List(trash::Job<Vec<trash::TrashItem>>),
	Purge(trash::Job<()>, String),
	Empty(trash::Job<()>),
}

enum UndoJob {
	Discard(trash::Job<()>),
	Back(paste::Job<paste::Done>),
}

struct TrashView {
	items:   Vec<trash::TrashItem>,
	dirs:    Vec<bool>,
	cursor:  usize,
	loading: bool,
	scroll:  Scroll,
}

#[derive(Default)]
struct ChipActions {
	switch:       Option<usize>,
	close:        Option<usize>,
	close_others: Option<usize>,
	rename:       Option<usize>,
	plus:         bool,
	moved:        Option<(usize, usize)>,
}

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

	tabs:   Vec<Tab>,
	active: usize,
	rename:       Option<String>,
	rename_at:    Option<usize>,
	rename_focus: bool,
	tabs_scroll:  f32,
	plus_hit:     Option<Rect>,
	close_hit:    Option<Rect>,
	tab_close_hits: Vec<(usize, Rect)>,
	tab_chip_hits:  Vec<(usize, Rect)>,
	status_index: Option<Rect>,

	selection: HashSet<PathBuf>,
	cursor_name: Option<String>,
	yank:      Option<Yank>,
	preview:   PreviewPane,
	tasks:     Vec<Progress>,

	spot:     bool,
	spot_chord: bool,

	bulk:        Option<BulkEditor>,
	item_rename: Option<ItemRename>,
	toasts:      Toasts,
	menu:        Option<crate::menu::Menu>,
	open_failed: (std::sync::mpsc::Sender<PathBuf>, std::sync::mpsc::Receiver<PathBuf>),
	menu_note:   (std::sync::mpsc::Sender<String>, std::sync::mpsc::Receiver<String>),
	terminal_dry: bool,
	terminal_log: (std::sync::mpsc::Sender<TerminalLaunch>, std::sync::mpsc::Receiver<TerminalLaunch>),
	open_dry:    bool,
	mimeapps_for_tests:        Option<String>,
	mimeapps_path_for_tests:        Option<PathBuf>,
	mimeapps_system_path_for_tests: Option<PathBuf>,
	frecency_path_for_tests:        Option<PathBuf>,
	open_log:    (std::sync::mpsc::Sender<PathBuf>, std::sync::mpsc::Receiver<PathBuf>),
	uri_offers: bool,

	cmdline: Option<Cmdline>,
	history: History,
	job:     Option<shell::Job>,
	output:  Option<Output>,

	openwith:     Option<OpenWith>,
	open_history: open::History,

	bookmarks:   book::Bookmarks,
	frecency:    frecency::Frecency,
	visited:     Option<PathBuf>,
	fuzzy:       Option<Fuzzy>,
	confirm:     Option<Confirm>,

	device_job: Option<DeviceJob>,

	trash_job:  Option<TrashJob>,
	paste_job:  Option<paste::Job<paste::Done>>,
	undo:       VecDeque<Undo>,
	undo_job:   Option<UndoJob>,

	prompt: Option<Prompt>,
	pack:   Option<pack::Job>,

	filter: Option<Edit>,

	hidden:    bool,
	sort:      (SortBy, bool),
	sort_seed: u64,
	columns:   Columns,
	help:      bool,
	help_from: usize,
	link_pick: Option<usize>,
	find:      Option<Edit>,
	find_last: Option<String>,
	help_query: Edit,
	help_at:    usize,
	help_focus: PanelFocus,

	sequence: Option<Sequence>,
	which_at: Option<Instant>,
	replay_at: Option<egui::Pos2>,

	bars_moved: [Option<Instant>; 2],

	body: Rect,

	first: bool,

	seed:      Vec<String>,
	seed_next: usize,
	seed_at:   Instant,
	shot:      Option<(String, usize)>,
	reveal:    Option<PathBuf>,
	menu_panel:  Option<egui::Rect>,
	toast_widths: Vec<(f32, f32)>,

	hover:     Option<egui::Pos2>,
	dressed:   bool,
	dress_at:  u64,
	shot_wait: Option<u32>,
	frame:     u64,
}

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
		let tokens = Tokens::theme();
		let late = std::env::var("CRABHOP_FONTS").is_ok_and(|mode| mode == "late");
		if !late {
			Tokens::install_with(&cc.egui_ctx, &tokens);
		}

		let seed: Vec<String> = std::env::var("CRABHOP_KEYS")
			.map(|keys| keys.split(';').map(|step| step.trim().to_owned()).filter(|s| !s.is_empty()).collect())
			.unwrap_or_default();

		let mut app = Self::with_context(cc.egui_ctx.clone(), seed);
		app.tokens = tokens;
		app
	}

	pub(crate) fn with_context(ctx: egui::Context, seed: Vec<String>) -> Self {
		let start = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
		let late = std::env::var("CRABHOP_FONTS").is_ok_and(|mode| mode == "late");
		let config = Config::load();
		let shares = config.columns.shares();

		ctx.send_viewport_cmd(egui::ViewportCommand::IMEAllowed(true));

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
			toast_widths: Vec::new(),
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

	fn bin(&self) -> Option<&TrashView> {
		self.tab().trash.as_ref()
	}

	fn bin_mut(&mut self) -> Option<&mut TrashView> {
		self.tab_mut().trash.as_mut()
	}

	fn in_bin(&self) -> bool {
		self.tab().trash.is_some()
	}

	fn entries(&self) -> Arc<Snapshot> {
		self.cache.get(self.tab().path())
	}

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

	fn selected_entry(&self) -> Option<Entry> {
		let tab = self.tab();
		let snapshot = self.entries();
		let index = *tab.view.get(tab.stack.last().expect("never empty").cursor)?;

		snapshot.entries.get(index).cloned()
	}

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
		if self.tab().symlinks {
			view.retain(|index| snapshot.entries[*index].link);
		}
		let len = view.len();

		if same_dir
			&& let Some(name) = &self.cursor_name
			&& let Some(at) = view.iter().position(|index| snapshot.entries[*index].name == *name)
		{
			self.tab_mut().stack.last_mut().expect("never empty").cursor = at;
		}

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

		self.look();
	}

	fn clamp(&mut self) {
		let len = self.tab().view.len();
		let cursor = self.cursor().min(len.saturating_sub(1));
		self.tab_mut().stack.last_mut().expect("never empty").cursor = cursor;

		let scroll = &mut self.tab_mut().scroll;
		scroll.follow(cursor);
		self.look();
	}

	fn act(&mut self, action: Action, ctx: &Context) {
		let len = self.tab().view.len();

		match action {
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
			Action::Index | Action::FuzzyJump => self.fuzzy_index(),
			Action::Devices(do_) => self.devices(do_),
			Action::Search { content } => self.fuzzy_search(content),
			Action::Remove(permanent) => self.remove(permanent),
			Action::Yank { cut } => self.yank(cut, ctx),
			Action::Unyank => self.unyank(),
			Action::Undo => self.undo_last(),
			Action::TerminalHere => self.terminal_here(),
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
			Action::Seek(step) => {
				self.preview.seek(step);
			}
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

	fn click_row(&mut self, position: usize) {
		if position >= self.tab().view.len() {
			return;
		}

		self.set_cursor(position);
	}

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

	fn toggle_all(&mut self, on: Option<bool>) {
		let snapshot = self.entries();
		let view = self.tab().view.clone();
		let dir = self.tab().path().to_owned();
		let paths: Vec<PathBuf> = view.iter().filter_map(|i| snapshot.entries.get(*i)).map(|e| dir.join(&e.name)).collect();

		crate::app::toggled(&mut self.selection, &paths, on);
	}

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

	fn open(&mut self) {
		if self.fold_under_pointer() {
			return;
		}

		let Some(path) = self.selected() else { return };

		if !path.is_dir() {
			crate::open::system(path, self.open_failed.0.clone());
			return;
		}

		self.tab_mut().stack.push(View { path, cursor: 0 });
		self.tab_mut().scroll.jump(0.0);
		self.arrived();
	}

	fn open_default(&mut self) {
		if self.fold_under_pointer() {
			return;
		}

		let Some(path) = self.selected() else { return };
		if let Some(program) = self.default_terminal(&path) {
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

	fn default_terminal(&self, path: &Path) -> Option<Vec<String>> {
		let name = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());
		let mime = crate::mime::guess(&name, path.is_dir());
		let home = std::env::var_os("HOME").map(PathBuf::from);
		let entries = crate::desktop::scan(&crate::desktop::app_dirs(home.as_deref()));

		crate::desktop::default_terminal(&entries, &self.preferences_text(), &self.system_preferences_text(), &mime, path)
	}

	fn arrived(&mut self) {
		self.tab_mut().trash = None;
		self.tab_mut().parent = ParentView::default();
		let path = self.tab().path().to_owned();
		self.tab_mut().history.push(&path);
		metrics::cwd(&path);
		self.look();
	}

	fn fold_under_pointer(&mut self) -> bool {
		let Some(at) = self.replay_at.or(self.hover) else { return false };
		let Some(column) = chrome::columns(self.body, self.shares).preview else { return false };
		if !column.contains(at) {
			return false;
		}

		self.preview.archive_fold_at(column, at)
	}

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

	fn find_open(&mut self) {
		self.find = Some(Edit::new(self.find_last.clone().unwrap_or_default()));
	}

	fn find_pattern(&self) -> Option<String> {
		let live = self.find.as_ref().map(|edit| edit.text.clone());
		live.or_else(|| self.find_last.clone()).filter(|pattern| !pattern.is_empty())
	}

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

	fn filter_apply(&mut self, pattern: Option<String>) {
		self.tab_mut().filter = pattern;
		self.refresh_view();
		self.clamp();
	}

	fn toggle_symlinks(&mut self) {
		let on = !self.tab().symlinks;
		self.tab_mut().symlinks = on;
		self.refresh_view();
		self.clamp();
		self.toasts.push(if on { "只显示符号链接" } else { "显示全部项目" }, toast::Kind::Ok);
	}

	fn page(&mut self, full: bool, back: bool) {
		let rows = self.tab().scroll.rows.max(1);
		let step = if full { rows.saturating_sub(1).max(1) } else { (rows / 2).max(1) };
		let cursor = self.cursor() as isize;
		let len = self.tab().view.len();
		let next = if back { cursor - step as isize } else { cursor + step as isize };

		self.set_cursor(next.clamp(0, len.saturating_sub(1) as isize) as usize);
	}

	fn enter_dir(&mut self) {
		let Some(entry) = self.selected_entry() else { return };
		if entry.dir {
			self.open();
		}
	}

	fn create_open(&mut self) {
		self.prompt = Some(Prompt {
			title:  "创建（带后缀=文件，不带=目录）".to_owned(),
			edit:   Edit::new(String::new()),
			action: PromptDo::Create,
		});
	}

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

	fn create_submit(&mut self, typed: &str) {
		let name = typed.trim();
		if name.is_empty() {
			return;
		}

		let cwd = self.tab().path().to_owned();
		let path = cwd.join(name);
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

	fn device_standing(&self) -> Option<String> {
		let fuzzy = self.fuzzy.as_ref()?;
		let item = fuzzy.hits.get(fuzzy.at).and_then(|hit| fuzzy.items.get(hit.at))?;

		match &item.payload {
			FuzzyPayload::Device(id) => Some(id.clone()),
			_ => None,
		}
	}

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
				self.note(
					format!("已创建{what} {} → {}", link.display(), target.display()),
					Undo::Link(vec![ link ]),
				);
			}
			Err(e) => self.toasts.push(format!("链接失败：{e}"), toast::Kind::Err),
		}
	}

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
			self.note(format!("已创建 {made} 个{what}（{}）", dir.display()), Undo::Link(created));
		}
	}

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

	fn travel(&mut self, back: bool) {
		let Some(path) = self.tab_mut().history.travel(back) else { return };

		self.tab_mut().trash = None;
		self.tab_mut().stack = vec![ View { path, cursor: 0 } ];
		self.tab_mut().scroll.jump(0.0);
		metrics::cwd(self.tab().path());
		self.look();
	}

	fn leave(&mut self) {
		if self.tab().stack.len() == 1 {
			let Some((parent, seat)) = self.parent_view() else { return };

			let left = self.tab().path().to_owned();
			let name = file_name(&left);
			let loading = self.cache.get(&parent).loading;

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

	fn tab_create(&mut self) {
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

	fn bulk_open(&mut self) {
		self.bulk = Some(BulkEditor { edit: Edit::new(String::new()), lines: Vec::new() });
	}

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

		for (line, why) in report.failures {
			self.tasks.push(Progress { title: format!("创建失败：{line}"), fraction: None, rate: Some(why) });
		}
	}

	fn rename_open(&mut self, at: RenameAt) {
		let Some(path) = self.selected() else { return };
		let Some(name) = path.file_name().map(|name| name.to_string_lossy().into_owned()) else { return };

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

				let said = new.file_name().map_or_else(|| new.display().to_string(), |name| name.to_string_lossy().into_owned());
				self.note(format!("已重命名为 {said}"), Undo::Rename { from: old, to: new });
			}
			Err(e) => self.toasts.push(format!("重命名失败：{e}"), toast::Kind::Err),
		}
	}

	fn rename_row(&mut self, at: usize) {
		self.set_cursor(at);
		self.rename_open(RenameAt::BeforeExt);
	}

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

		if !self.toasts.is_empty() {
			self.toasts.clear();
		}
	}

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

	fn cmdline_refresh(&mut self) {
		let cwd = self.tab().path().to_owned();
		let Some(cmdline) = &mut self.cmdline else { return };
		if cmdline.comp.is_none() {
			return;
		}

		let items = complete::paths(&cmdline.edit.text, &cwd, 64);
		cmdline.comp = Completions::new(complete::token_start(&cmdline.edit.text), items);
	}

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

	fn menu_anchor(&self, fallback: Pos2) -> Pos2 {
		self.menu_fit(self.replay_at.or(self.hover).unwrap_or(fallback))
	}

	fn menu_fit(&self, at: Pos2) -> Pos2 {
		pos2(at.x.clamp(self.body.min.x + 8.0, self.body.max.x - 208.0), at.y.clamp(self.body.min.y + 8.0, self.body.max.y - 160.0))
	}

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

	fn menu_on_cursor(&mut self) {
		let cursor = self.tab().stack.last().map_or(0, |view| view.cursor);
		let Some(position) = self.tab().view.iter().position(|at| *at == cursor) else { return };
		let Some(target) = self.menu_target(position) else { return };

		let list = chrome::columns(self.body, self.shares).current;
		let top = list.min.y + (position as f32 - self.tab().scroll.offset) * ROW_HEIGHT;
		self.menu_open(target, self.menu_fit(pos2(list.min.x, top)));
	}

	fn menu_on_blank(&mut self, at: Pos2) {
		self.menu_open(crate::menu::Target::Folder(self.tab().path().to_owned()), self.menu_anchor(at));
	}

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

	fn menu_keys(&mut self, events: &[egui::Event], typed: &str, ctx: &Context) {
		let page = self.menu.as_ref().map(|menu| menu.page);
		let Some(page) = page else { return };
		let vis = self.menu.as_ref().map_or(0, |menu| menu.visible_rows(self.body.height()));

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
					if page == crate::menu::Page::Apps && self.menu.as_ref().is_some_and(|menu| !menu.query.text.is_empty()) {
						if let Some(menu) = &mut self.menu {
							menu.query = crate::edit::Edit::new(String::new());
							menu.apps_at = 0;
							menu.scroll = 0;
						}
					} else if page == crate::menu::Page::Main {
						self.menu = None;
					} else if let Some(menu) = &mut self.menu {
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
				Key::L if page == crate::menu::Page::Main => {
					if self.menu.as_ref().is_some_and(|menu| menu.standing_open_with()) {
						self.menu_pick(ctx);
					}
				}
				Key::Enter if page == crate::menu::Page::Apps && modifiers.ctrl => self.menu_launch(true),
				Key::Enter => self.menu_pick(ctx),
				Key::Q if !modifiers.ctrl && page != crate::menu::Page::Apps => self.menu = None,
				_ => continue,
			}

			ctx.request_repaint();
		}
	}

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

	fn menu_copy(&mut self, cut: bool, ctx: &Context) {
		self.menu = None;
		self.yank(cut, ctx);
	}

	fn menu_paste(&mut self) {
		self.menu = None;
		self.paste(false);
	}

	fn terminal_here(&mut self) {
		let Some(path) = self.selected() else { return };
		if !path.is_dir() {
			self.toasts.push("须是文件夹", toast::Kind::Warn);
			return;
		}

		self.open_terminal(path);
	}

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

	fn menu_terminal(&mut self) {
		let Some(menu) = self.menu.as_ref() else { return };
		let Some(folder) = menu.target.folder() else { return };

		self.menu = None;
		self.open_terminal(folder);
	}

	fn menu_apps(&mut self) {
		let Some(menu) = self.menu.as_ref() else { return };
		let mime = menu.target.mime();
		let home = std::env::var_os("HOME").map(PathBuf::from);

		let entries = crate::desktop::scan(&crate::desktop::app_dirs(home.as_deref()));
		let text = crate::config::Config::mimeapps().and_then(|at| std::fs::read_to_string(at).ok()).unwrap_or_default();
		let preferred = crate::desktop::preferred_in(&text, &mime);
		let added = crate::desktop::added_in(&text, &mime);
		let types = crate::mime::closure(&mime);
		let path_env = std::env::var("PATH").unwrap_or_default();
		let handlers: Vec<crate::desktop::Entry> =
			crate::desktop::handlers(&entries, &types, &preferred, &added, &path_env).into_iter().cloned().collect();
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
			let folder = path.parent().map(Path::to_owned).unwrap_or_else(|| self.tab().path().to_owned());
			self.spawn_in_terminal(argv, folder);
		} else {
			crate::open::spawn_argv(argv, None);
		}
	}

	fn preferences_text(&self) -> String {
		self.mimeapps_for_tests.clone().or_else(|| self.mimeapps_path().and_then(|at| crate::config::mimeapps_read(&at))).unwrap_or_default()
	}

	fn mimeapps_path(&self) -> Option<PathBuf> {
		self.mimeapps_path_for_tests.clone().or_else(crate::config::Config::mimeapps)
	}

	fn frecency_path(&self) -> Option<PathBuf> {
		self.frecency_path_for_tests.clone().or_else(|| crate::config::Config::state("frecency.json"))
	}

	fn frecency_save(&self) {
		if let Some(at) = self.frecency_path() {
			self.frecency.save_at(&at);
		}
	}

	fn system_preferences_text(&self) -> String {
		let at = self.mimeapps_system_path_for_tests.clone().unwrap_or_else(crate::config::mimeapps_system);

		crate::config::mimeapps_read(&at).unwrap_or_default()
	}

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

		while let Ok(note) = self.menu_note.1.try_recv() {
			crate::metrics::menu(&format!("note {note}"));
			self.toasts.push(note, toast::Kind::Warn);
			ctx.request_repaint();
		}
	}

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
				self.frecency.put(frecency::Entry { path, score, last });
				self.frecency_save();
				self.fuzzy_rebuild();
			}
			Undo::Trash(paths) => {
				let words = "已从回收站恢复".to_owned();
				self.trash_job = Some(TrashJob::Restore(trash::restore(paths.clone()), paths, words));
			}
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

	fn note(&mut self, text: String, undo: Undo) {
		self.undo.push_back(undo.clone());
		if self.undo.len() > UNDO_DEPTH {
			self.undo.pop_front();
		}

		self.toasts.push_with_undo(text, toast::Kind::Ok, Some(undo));
	}

	fn undo_last(&mut self) {
		let Some(undo) = self.undo.pop_back() else { return };
		self.toasts.take_matching(&undo);
		self.run_undo(undo);
	}

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

	fn yank(&mut self, cut: bool, ctx: &Context) {
		let targets = self.targets();
		if targets.is_empty() {
			return;
		}

		let count = targets.len();
		let text = targets.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join("\n");

		if !(self.uri_offers && crate::uri_clipboard::offer(&targets, cut)) {
			ctx.copy_text(text);
		}

		self.yank = Some(Yank { paths: targets, cut });
		self.toasts.push(
			if cut { format!("已剪切 {count} 项（Y/X 取消，p 粘贴）") } else { format!("已复制 {count} 项（Y/X 取消，p 粘贴）") },
			toast::Kind::Ok,
		);
	}

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
					if self.in_bin() {
						self.look_at_trash();
					}
				}
				Some(Err(e)) => {
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
			self.cache.forget(self.tab().path());
			self.trash_refresh();
		}

		if self.trash_job.is_some() {
			ctx.request_repaint_after(Duration::from_millis(60));
		}
	}

	fn trash_view_open(&mut self) {
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

	fn help_keys(&mut self, events: &[egui::Event], typed: &str, ctx: &Context) {
		let rows = self.keys.rows();
		let (mut close, mut run, mut focus, mut moved) = (false, false, false, 0isize);

		for event in events {
			let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };

			match key {
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
			let visible = help_rows(rows, &self.help_query.text);
			self.help_at = self.help_at.min(visible.len().saturating_sub(1));
		}

		ctx.request_repaint();
	}

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
		self.look_at_trash();
		self.bars_moved[0] = Some(Instant::now());
	}

	fn bin_move(&mut self, step: isize) {
		let at = self.bin().map_or(0, |view| view.cursor);
		self.bin_seat((at as isize + step).max(0) as usize);
	}

	fn bin_page(&mut self, full: bool, back: bool) {
		let Some(view) = self.bin() else { return };
		let rows = view.scroll.rows.max(1);
		let step = if full { rows.saturating_sub(1).max(1) } else { (rows / 2).max(1) };
		let at = view.cursor as isize;
		let next = if back { at - step as isize } else { at + step as isize };

		self.bin_seat(next.max(0) as usize);
	}

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

	fn index_entry(&self) -> Option<IndexEntry> {
		let fuzzy = self.fuzzy.as_ref().filter(|fuzzy| fuzzy.mode == FuzzyMode::Index)?;
		let item = fuzzy.hits.get(fuzzy.at).and_then(|hit| fuzzy.items.get(hit.at))?;

		match &item.payload {
			FuzzyPayload::Index(name) => Some(IndexEntry::Bookmark(name.clone())),
			FuzzyPayload::Path(path) => Some(IndexEntry::Frecency(path.clone(), item.zone)),
			_ => None,
		}
	}

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

	fn fuzzy_search(&mut self, content: bool) {
		let mode = if content { FuzzyMode::FindText } else { FuzzyMode::FindNames };
		self.fuzzy_open(mode, Vec::new());
	}

	fn fuzzy_close(&mut self) {
		if let Some(fuzzy) = self.fuzzy.take()
			&& let Some(job) = fuzzy.finder
		{
			job.cancel();
		}
	}

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

	fn fuzzy_tick(&mut self, ctx: &Context) {
		self.fuzzy_poll();

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

	fn fuzzy_enter(&mut self) {
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
		let typed = typed_text(&events);

		self.handle(ctx, &events, &typed);
	}

	fn handle(&mut self, ctx: &Context, events: &[egui::Event], typed: &str) {
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
				self.toasts.clear();
				self.selection.clear();
				continue;
			}

			if *key == Key::F5 {
				self.cache.forget(self.tab().path());
				ctx.request_repaint();
				continue;
			}

			if *key == Key::C && !modifiers.ctrl && !modifiers.alt && self.in_bin() {
				self.trash_empty_ask();
				ctx.request_repaint();
				continue;
			}

			let text = (!typed.is_empty() && !modifiers.ctrl && !modifiers.alt).then_some(typed);
			let Some(chord) = crate::keys::chord(*key, *modifiers, text) else { continue };
			metrics::input("key");

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
				metrics::which(&chord, self.keys.family(open).len());
				self.which_at = Some(Instant::now());
				ctx.request_repaint_after(WHICH_TIMEOUT);
			}
		}
	}

	fn spot_keys(&mut self, events: &[egui::Event], ctx: &Context) {
		let len = self.tab().view.len();

		for event in events {
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

	fn fuzzy_keys(&mut self, events: &[egui::Event], typed: &str, ctx: &Context) {
		let (mut close, mut enter) = (false, false);
		let mut moved = 0isize;
		let mut touched = false;
		let was_inside = self.fuzzy.as_ref().is_some_and(|fuzzy| fuzzy.focus.inside);

		for event in events {
			let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { continue };

			let index = self.fuzzy.as_ref().is_some_and(|fuzzy| fuzzy.mode == FuzzyMode::Index);
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

			if matches!(self.fuzzy.as_ref().map(|fuzzy| fuzzy.mode), Some(FuzzyMode::Devices { .. }))
				&& let Some(door) = device_door(*key, modifiers.shift)
			{
				self.device_door(door);
				continue;
			}

			if *key == Key::I && index && !inside && !modifiers.ctrl && !modifiers.alt {
				if let Some(fuzzy) = &mut self.fuzzy {
					fuzzy.focus.enter();
				}
				continue;
			}

			match (*key, modifiers.ctrl) {
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

	fn wheel_route(&mut self, ctx: &Context, area: Rect, pointer: Option<egui::Pos2>, content: f32, rows: f32) {
		match self.wheel_owner(area, pointer) {
			WheelOwner::Menu => {
				let vis = self.menu.as_ref().map_or(0, |menu| menu.visible_rows(self.body.height()));
				if let Some(menu) = &mut self.menu {
					menu.wheel(rows, vis);
				}
			}
			WheelOwner::Tabs => {
				let overflow = self.tabs_overflow(area.width());
				self.tabs_scroll = (self.tabs_scroll - content * 24.0).clamp(0.0, overflow);
			}
			WheelOwner::Output => {
				if let Some(output) = &mut self.output {
					output.from = output.from.saturating_add_signed(-content.round() as isize);
				}
			}
			WheelOwner::Parent => {
				self.tab_mut().parent.scroll.wheel(rows);
				if self.shot.is_some() {
					self.shot_wait = Some(4);
				}
			}
			WheelOwner::Preview => {
				let ctrl = ctx.input(|i| i.modifiers.ctrl);
				if ctrl {
					self.preview.zoom_by(rows < 0.0);
				} else if self.preview.scroll(rows) {
					self.bars_moved[1] = Some(Instant::now());
				}
			}
			WheelOwner::Help => {
				let step = -content.round() as isize;
				let visible = help_rows(self.keys.rows(), &self.help_query.text);
				if !visible.is_empty() {
					self.help_at = (self.help_at as isize + step).clamp(0, visible.len() as isize - 1) as usize;
				}
			}
			WheelOwner::List => {
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

	fn wheel_owner(&self, area: Rect, pointer: Option<egui::Pos2>) -> WheelOwner {
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

	fn replay(&mut self, ctx: &Context, area: Rect) {
		let waited = self.seed_at.elapsed();
		if self.seed_next < self.seed.len() && waited < Duration::from_millis(350) {
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
			self.handle(ctx, &[ egui::Event::Paste(text.to_owned()) ], text);
		} else if let Some(text) = step.strip_prefix("ime ") {
			self.handle(ctx, &[ egui::Event::Ime(egui::ImeEvent::Commit(text.to_owned())) ], text);
		} else if let Some(text) = step.strip_prefix("type ") {
			let text = text.replace("\\n", "\n");
			self.handle(ctx, &[ egui::Event::Text(text.clone()) ], &text);
		} else if let Some(i) = step.strip_prefix("click ") {
			let i: usize = i.trim().parse().unwrap_or(0);
			let first = self.tab().scroll.offset.floor() as usize;
			let len = self.tab().view.len();
			if let Some(position) = clicked_row(first, i, len) {
				self.click_row(position);
			}
			ctx.request_repaint();
		} else if let Some(rows) = step.strip_prefix("scroll ") {
			let rows = rows.trim().parse().unwrap_or(3.0);
			if self.in_bin() {
				self.bin_mut().expect("in the bin's tab").scroll.wheel(rows);
			} else {
				self.tab_mut().scroll.wheel(rows);
			}
			self.bars_moved[0] = Some(Instant::now());
			self.shot_wait = Some(4);
		} else if let Some(what) = step.strip_prefix("zoom ") {
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
			let delta = delta.trim().parse().unwrap_or(0.0);
			let rows = scroll::wheel_rows(egui::MouseWheelUnit::Line, delta);
			self.wheel_route(ctx, area, self.replay_at, delta, rows);
			self.shot_wait = Some(4);
		} else if let Some(rest) = step.strip_prefix("hover ") {
			let numbers: Vec<f32> = rest.split_whitespace().filter_map(|n| n.parse().ok()).collect();
			self.replay_at = match numbers[..] {
				[ x, y ] => Some(pos2(x, y)),
				_ => None,
			};
		} else if let Some(rest) = step.strip_prefix("menu ") {
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
			self.shot_wait = Some(1);
		} else if let Some(rest) = step.strip_prefix("drag ") {
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

	fn reveal(&mut self, path: PathBuf) {
		self.reveal = Some(path);
	}

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

	fn draw_columns(&mut self, ui: &mut Ui, body: Rect, font: &FontId, advance: f32) {
		let mut tokens = self.tokens;
		if !ui.ctx().input(|i| i.focused) {
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

		if let Some(parent_rect) = columns.parent
			&& let Some((path, derived)) = self.parent_view()
		{
			let snapshot = self.cache.get(&path);
			let rows_visible = (parent_rect.height() / ROW_HEIGHT).floor().max(1.0) as usize;
			let len = snapshot.entries.len();

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
					self.go(target);
				}
			}
		}

		if self.in_bin() {
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

			if self.item_rename.is_none() {
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

			let response = ui.interact(preview, egui::Id::unique("preview-gestures"), Sense::click_and_drag());
			if response.dragged() {
				self.preview.pan_by(response.drag_delta());
			}
			if self.preview.zoomed() {
				ui.ctx().set_cursor_icon(if response.dragged() { CursorIcon::Grabbing } else { CursorIcon::Grab });
			}

			if let Some(rows) = self.preview.archive_tree(preview) {
				for (row, rect, dir) in rows {
					let response = ui.interact(rect, egui::Id::unique(("archive-row", row)), Sense::click());
					if dir && response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
						self.preview.archive_fold(row);
					}
				}
			}

			if let Some((content, offset, view)) = self.preview.extent()
				&& let Some(requested) = self.overlay_bar(ui, preview, 1, content, view, offset)
			{
				self.preview.scroll_to(requested);
				self.bars_moved[1] = Some(Instant::now());
			}
		} else {
			self.preview.hide();
		}

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

	fn status_bar(&mut self, ui: &mut Ui, area: Rect, small: &FontId) {
		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let rect = Rect::from_min_size(pos2(area.min.x, area.max.y - STATUS_HEIGHT), vec2(area.width(), STATUS_HEIGHT));
		painter.rect_filled(rect, 0.0, tokens.base);
		painter.line_segment([ rect.left_top(), rect.right_top() ], (1.0, tokens.border));

		let mut at = rect.min.x + 12.0;
		let middle = rect.center().y;
		let cell = painter.layout_no_wrap("0".to_owned(), small.clone(), tokens.text).size().x;

		let select = !self.selection.is_empty();
		let badge = if select { "SELECT" } else { "NORMAL" };
		let width = painter.layout_no_wrap(badge.to_owned(), small.clone(), tokens.accent_ink).size().x + 12.0;
		let badge_rect = Rect::from_center_size(pos2(at + width * 0.5, middle), vec2(width, 18.0));
		painter.rect_filled(badge_rect, 4.0, if select { tokens.warn } else { tokens.accent });
		painter.text(badge_rect.center(), Align2::CENTER_CENTER, badge, small.clone(), tokens.accent_ink);
		at = badge_rect.max.x + 12.0;

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

		let len = self.tab().view.len();
		if len > 0 {
			let current = (self.cursor() + 1).to_string();
			let total = format!("/{len}");
			let width = painter.layout_no_wrap(current.clone(), small.clone(), tokens.text).size().x;
			painter.text(pos2(at, middle), Align2::LEFT_CENTER, current, small.clone(), tokens.text);
			painter.text(pos2(at + width, middle), Align2::LEFT_CENTER, total.clone(), small.clone(), tokens.subtext);
			at += width + painter.layout_no_wrap(total, small.clone(), tokens.subtext).size().x + 12.0;
		}

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

		let right = right_group(self.sort, self.tab().symlinks, self.tab().filter.as_deref());
		let start = rect.max.x - 12.0 - painter.layout_no_wrap(right.clone(), small.clone(), tokens.subtext).size().x;

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

		if start - 12.0 > at {
			painter.line_segment([ pos2(start - 10.0, rect.min.y + 6.0), pos2(start - 10.0, rect.max.y - 6.0) ], (1.0, tokens.border));
		}
	}

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

		let prefix = &text[..rename.edit.cursor.min(text.len())];
		let caret = field.min.x + 6.0 + cells(prefix, advance);
		let caret_rect = Rect::from_min_max(pos2(caret, field.center().y - 9.0), pos2(caret + 2.0, field.center().y + 9.0));
		painter.rect_filled(caret_rect, 0.0, tokens.accent);
		anchor_ime(ui, field, caret_rect);

		if let Some(note) = rename.note {
			let cover = Rect::from_min_max(pos2(row.max.x - advance - facts_width - 4.0, row.min.y), pos2(row.max.x, row.max.y));
			painter.rect_filled(cover, 0.0, tokens.base);
			painter.text(pos2(row.max.x - advance, row.center().y), Align2::RIGHT_CENTER, note, small, tokens.err);
		}
	}

	fn draw_bulk(&mut self, ui: &mut Ui, area: Rect, painter: &Painter, advance: f32, font: &FontId, small: &FontId) {
		let tokens = self.tokens;
		let panel = Rect::from_center_size(
			area.center(),
			vec2(640.0f32.min(area.width() - 32.0), 480.0f32.min(area.height() - 32.0)),
		);

		let mask = ui.interact(area, egui::Id::unique("bulk-mask"), Sense::click());
		let cancel = mask.clicked() && ui.ctx().input(|i| i.pointer.interact_pos()).is_none_or(|at| !panel.contains(at));

		painter.rect_filled(area, 0.0, Color32::from_black_alpha(150));
		chrome::shadow(painter, panel, 8.0);
		painter.rect_filled(panel, 8.0, tokens.raised);
		painter.rect_stroke(panel, 8.0, (1.0, tokens.border_strong), egui::StrokeKind::Inside);

		let title = FontId::new(20.0, FontFamily::Name("maple-bold".into()));
		painter.text(pos2(panel.min.x + 24.0, panel.min.y + 24.0), Align2::LEFT_CENTER, "批量创建", title, tokens.text);

		let body = Rect::from_min_max(pos2(panel.min.x + 24.0, panel.min.y + 54.0), pos2(panel.max.x - 24.0, panel.max.y - 56.0));
		painter.text(pos2(body.min.x, body.min.y), Align2::LEFT_TOP, "输入（一行一个）", small.clone(), tokens.subtext);

		let top = body.min.y + 22.0;
		let left = Rect::from_min_max(pos2(body.min.x, top), pos2(body.max.x, body.max.y));
		painter.rect_filled(left, 6.0, tokens.crust);

		let mut commit = false;
		if let Some(bulk) = &self.bulk {
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

	fn draw_fuzzy(&mut self, ui: &mut Ui, area: Rect, font: &FontId, small: &FontId) {
		let Some(fuzzy) = &self.fuzzy else { return };
		let mut clicked_query = false;
		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let advance = painter.layout_no_wrap("0".to_owned(), font.clone(), tokens.text).size().x;

		let width: f32 = if matches!(fuzzy.mode, FuzzyMode::FindNames | FuzzyMode::FindText) { 860.0 } else { 720.0 };
		let width = width.min(area.width() - 80.0);
		let rows = fuzzy.hits.len().min(12);
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
		let extra_line = matches!(fuzzy.mode, FuzzyMode::Devices { .. });
		let height = 44.0 + rows as f32 * 32.0 + headers * 18.0 + 26.0 + if extra_line { 18.0 } else { 0.0 };
		let panel = Rect::from_min_size(pos2(area.center().x - width * 0.5, area.min.y + area.height() * 0.2), vec2(width, height));

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

		let room = panel.max.x - 16.0;
		let shift = (caret_at(&painter) - room).max(0.0);
		let boxed = painter
			.with_clip_rect(Rect::from_min_max(pos2(query_at.x, panel.min.y), pos2(panel.max.x, panel.min.y + 44.0)));
		boxed.text(pos2(text_at - shift, query_at.y), Align2::LEFT_CENTER, &fuzzy.query.text, query_font.clone(), tokens.text);

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

		let clipped = painter.with_clip_rect(panel);
		let mut at_y = panel.min.y + 44.0;
		let (mut seat, mut run) = (None, false);
		for (i, hit) in fuzzy.hits.iter().take(rows).enumerate() {
			let Some(item) = fuzzy.items.get(hit.at) else { continue };

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
			painter.text(pos2(panel.min.x + 12.0, panel.max.y - 31.0), Align2::LEFT_CENTER, DEVICE_HINT_URI, small.clone(), tokens.disabled);
		}
		painter.text(
			pos2(panel.max.x - 12.0, panel.max.y - 13.0),
			Align2::RIGHT_CENTER,
			format!("{} 命中", fuzzy.hits.len()),
			small.clone(),
			tokens.disabled,
		);

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

		if !loading && !empty {
			let label = format!("{len} 项");
			let width = painter.layout_no_wrap(label.clone(), small.clone(), tokens.subtext).size().x + 16.0;
			let badge = Rect::from_min_size(pos2(column.center().x - width * 0.5, column.max.y - 34.0), vec2(width, 22.0));
			painter.rect_filled(badge, 4.0, tokens.raised);
			painter.text(badge.center(), Align2::CENTER_CENTER, label, small.clone(), tokens.subtext);
		}
	}

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

	fn draw_toasts(&mut self, ui: &mut Ui, area: Rect, small: &FontId) {
		if self.toasts.is_empty() {
			return;
		}

		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let now = Instant::now();
		let len = self.toasts.len();
		let height = 40.0f32;
		let max_w = (area.width() - 24.0).max(160.0);
		let cell = painter.layout_no_wrap("0".to_owned(), small.clone(), tokens.text).size().x;
		let (mut undo_at, mut dismiss_at) = (None, None);
		let mut widths: Vec<(f32, f32)> = Vec::with_capacity(len);

		for (i, toast) in self.toasts.items().enumerate() {
			let text_w = painter.layout_no_wrap(toast.text.clone(), small.clone(), tokens.text).size().x;
			let want = text_w + 28.0 + if toast.undo.is_some() { 50.0 } else { 0.0 };
			let width = want.clamp(160.0, max_w);
			widths.push((width, text_w));

			let slot = (len - 1 - i) as f32;
			let age = toast.age(now);
			let alpha = toast::alpha(age, toast.stay());
			let rect = Rect::from_min_size(
				pos2(area.max.x - 12.0 - width + toast::slide(age), area.max.y - STATUS_HEIGHT - 12.0 - height - slot * (height + 8.0)),
				vec2(width, height),
			);
			let colour = match toast.kind {
				toast::Kind::Ok => tokens.ok,
				toast::Kind::Warn => tokens.warn,
				toast::Kind::Err => tokens.err,
			};

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

		self.toast_widths = widths;

		if let Some(at) = dismiss_at {
			self.toasts.dismiss(at);
		}

		if let Some(at) = undo_at
			&& let Some(undo) = self.toasts.take_undo(at)
		{
			if let Some(position) = self.undo.iter().rposition(|held| *held == undo) {
				self.undo.remove(position);
			}
			self.run_undo(undo);
		}

		self.toasts.tick(now);
		ui.ctx().request_repaint_after(Duration::from_millis(33));
	}

	fn draw_which(&mut self, ui: &mut Ui, area: Rect, small: &FontId) {
		let Some(sequence) = &self.sequence else { return };
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

		let clipped = painter.with_clip_rect(panel);
		for (i, (keys, desc)) in family.iter().take(rows).enumerate() {
			let y = panel.min.y + 8.0 + i as f32 * row_h + row_h * 0.5;
			clipped.text(pos2(panel.min.x + 16.0 + keys_w, y), Align2::RIGHT_CENTER, keys.clone(), small.clone(), tokens.green);
			clipped.text(pos2(panel.min.x + 32.0 + keys_w, y), Align2::LEFT_CENTER, desc.clone(), small.clone(), tokens.subtext);
		}
	}

	fn draw_help(&mut self, ui: &mut Ui, area: Rect, advance: f32, font: &FontId) {
		let tokens = self.tokens;
		let line = LINE_HEIGHT;
		let small = FontId::new(FONT_SMALL, FontFamily::Monospace);
		let rows = self.keys.rows().to_vec();
		let visible = help_rows(&rows, &self.help_query.text);

		let painter = ui.painter().clone();
		painter.rect_filled(area, 0.0, Color32::from_black_alpha(200));

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
		meta: bool,
		hover: Option<usize>,
		find: Option<&str>,
	) {
		let row_h = ROW_HEIGHT;
		let rows = (column.height() / row_h).floor().max(1.0) as usize + 1;
		let clipped = painter.with_clip_rect(column);
		let painter = &clipped;
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
					painter.rect_filled(Rect::from_min_size(row.min, Vec2::new(2.0, row_h)), 0.0, tokens.accent);
				}
			} else if Some(position) == hover {
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

			let facts_width = if facts_width > 0.0 { facts_width + 16.0 } else { 0.0 };
			let budget =
				((column.width() - ICON_BOX - 3.0 * advance - facts_width) / advance).floor().max(1.0) as u32;
			let name = crate::list::clipped(&entry.name, budget);
			let name_colour = if entry.dir { tokens.blue } else { tokens.text };
			let name_colour = if quiet { name_colour.gamma_multiply(0.7) } else { name_colour };
			let at = egui::pos2(column.min.x + ICON_BOX + 2.0 * advance, top + row_h * 0.5);

			match find.map(|pattern| crate::app::find_hits(&entry.name, pattern)) {
				Some(positions) if !positions.is_empty() => {
					text_hits(painter, at, name, &positions, budget as usize, font, advance, name_colour, tokens.accent);
				}
				_ => {
					painter.text(at, Align2::LEFT_CENTER, name, font.clone(), name_colour);
				}
			}
		}
	}

	fn draw_menu(&mut self, ui: &mut Ui, area: Rect, font: &FontId, small: &FontId) {
		let Some(menu) = self.menu.as_ref() else { return };
		let tokens = self.tokens;
		let painter = ui.painter().clone();
		let rows = menu.rows();
		let apps_page = menu.page == crate::menu::Page::Apps;

		let is_header = |live: bool| apps_page && !live;
		let row_heights: Vec<f32> = rows.iter().map(|(_, _, live)| if is_header(*live) { 18.0 } else { crate::menu::MENU_ROW_H }).collect();

		let vis = menu.visible_rows(self.body.height());
		let walked = menu.rows_walked();
		let scroll = menu.scroll.min(walked.saturating_sub(vis.max(1)));
		let end = (scroll + vis).min(walked);

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

		let app_rows = apps_page.then(|| menu.app_rows());
		let pinned = app_rows.as_ref().is_some_and(|rows| rows.get(scroll).is_some_and(|row| row.zone.is_none()));

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

		let anchor = pos2(
			menu.anchor.x.min(area.max.x - width - 8.0).max(area.min.x + 8.0),
			menu.anchor.y.min(area.max.y - height - 8.0).max(area.min.y + 8.0),
		);
		let panel = Rect::from_min_size(anchor, vec2(width, height));
		self.menu_panel = Some(panel);

		let outside = ui.interact(area, egui::Id::unique("menu-outside"), Sense::click());
		if outside.clicked() && ui.ctx().input(|i| i.pointer.interact_pos()).is_none_or(|at| !panel.contains(at)) {
			self.menu = None;
		}

		chrome::shadow(&painter, panel, 8.0);
		painter.rect_filled(panel, 8.0, tokens.raised);
		painter.rect_stroke(panel, 8.0, (1.0, tokens.border_strong), egui::StrokeKind::Inside);

		let painter = painter.with_clip_rect(panel);

		let Some(menu) = self.menu.as_ref() else { return };
		painter.text(
			pos2(panel.min.x + 12.0, panel.min.y + 16.0),
			Align2::LEFT_CENTER,
			crate::list::clipped(&menu.title(), 40),
			FontId::new(13.0, FontFamily::Monospace),
			tokens.disabled,
		);

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

			if at == scroll && pinned {
				let zone = app_rows.as_ref().and_then(|rows| rows.get(at)).map_or("", crate::menu::zone_of);
				painter.text(pos2(panel.min.x + 12.0, top + 9.0), Align2::LEFT_CENTER, zone, small.clone(), tokens.disabled);
				top += 18.0;
			}

			let row = Rect::from_min_size(pos2(panel.min.x + 6.0, top), vec2(width - 12.0, 24.0));
			let response = ui.interact(row, egui::Id::unique(("menu-row", at)), Sense::click());

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

		if let Some(at) = picked
			&& let Some(menu) = &mut self.menu
		{
			menu.seat(at);
		}
		if picked.is_some() {
			self.menu_pick(ui.ctx());
		}
	}

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

		let shown_width = self.preview.shown_physical(&path).map(|shown| shown[0]).or_else(|| self.preview.fit_width_pixels());
		let rows = match crate::spot::rows(&path, shown_width) {
			Some(rows) => rows,
			None => vec![ ("类型".to_owned(), "读不到这个条目（可能刚被移走）".to_owned()) ],
		};

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

fn chip_width(name: &str, advance: f32) -> f32 {
	let cells = name.chars().map(crate::list::cell_width).sum::<u32>();
	(cells as f32 * advance + CHIP_PAD).clamp(CHIP_MIN, CHIP_MAX)
}

fn number_width(index: usize) -> f32 {
	let digits = (index + 1).to_string().len() as f32;

	digits * FONT_SMALL * 0.62 + 6.0
}

fn cells(text: &str, advance: f32) -> f32 {
	text.chars().map(|c| crate::list::cell_width(c) as f32 * advance).sum()
}

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

fn output_rect(area: Rect, lines: usize) -> Rect {
	let height = (34.0 + lines as f32 * 20.0).min(area.height() * 0.4);
	Rect::from_min_max(pos2(area.min.x, area.max.y - STATUS_HEIGHT - height), pos2(area.max.x, area.max.y - STATUS_HEIGHT))
}

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

fn now_secs() -> u64 {
	std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn hit_text(hit: &search::Hit) -> String {
	let path = frecency::display(&hit.path);
	match (hit.line, &hit.text) {
		(Some(line), Some(text)) => format!("{path}:{line}  {text}"),
		_ => path,
	}
}

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
		"?" => Key::Slash,
		"[" => Key::OpenBracket,
		"]" => Key::CloseBracket,
		"`" => Key::Backtick,
		")" => Key::Num0,
		"(" => Key::Num9,
		"*" => Key::Num8,
		"}" => Key::CloseBracket,
		"{" => Key::OpenBracket,
		_ => Key::from_name(name)?,
	};

	Some((key, modifiers))
}

fn replay_text(key: &str) -> Option<&str> {
	if key.starts_with("C-") {
		return None;
	}

	let mut characters = key.chars();
	let (Some(one), None) = (characters.next(), characters.next()) else { return None };
	(!one.is_control()).then(|| &key[..one.len_utf8()])
}

fn replay_shifted(key: &str) -> bool {
	key.chars().all(|c| c.is_ascii_uppercase())
		|| matches!(key, "~" | "_" | "+" | "{" | "}" | "|" | ":" | "<" | ">" | "?")
}

struct IndexRow {
	pub zone:   &'static str,
	pub text:   String,
	pub right:  Option<String>,
	pub payload: FuzzyPayload,
	pub broken: bool,
}

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

fn jump_to(cwd: &std::path::Path) -> Result<std::path::PathBuf, String> {
	let devices = mount::list()?;
	let Some(device) = mount::device_of(&devices, cwd) else {
		return Err(format!("{} 不在任何设备上", cwd.display()));
	};

	device.mounts.first().cloned().ok_or_else(|| format!("{} 没有挂载点", device.label))
}

pub(crate) fn matches(pattern: &str, name: &str) -> bool {
	pattern.is_empty() || name.to_lowercase().contains(&pattern.to_lowercase())
}

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

pub(crate) fn expand_tilde(path: &str, home: Option<PathBuf>) -> Option<PathBuf> {
	match path {
		"~" => home,
		_ => match path.strip_prefix("~/") {
			Some(rest) => home.map(|home| home.join(rest)),
			None => Some(PathBuf::from(path)),
		},
	}
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum DeviceDoor {
	Mount,
	MountAndJump,
	Remount,
	Unmount {
		force: bool,
	},
	Jump,
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

pub(crate) fn help_size(area: Rect, key_cells: usize, word_cells: usize, visible: usize, advance: f32, line: f32) -> (f32, f32) {
	let width = (advance * (key_cells + word_cells) as f32 + advance * 4.0)
		.clamp(advance * 24.0, (area.width() * 0.9).max(advance * 24.0));
	let room = ((area.height() * 0.7 - line * 4.5).max(line * 4.0) / line).floor();
	let rows = (visible as f32).clamp(4.0, room);
	let height = line * 4.5 + rows * line;

	(width, height)
}

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

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum QueryEnter {
	Go(PathBuf),
	Missing(PathBuf),
	Selection,
}

pub(crate) fn query_enter(query: &str, cwd: &Path, home: Option<PathBuf>) -> QueryEnter {
	let typed = query.trim();
	if typed.is_empty() || !looks_like_path(typed) {
		return QueryEnter::Selection;
	}

	let Some(expanded) = expand_tilde(typed, home) else {
		return QueryEnter::Selection;
	};

	let path = if expanded.is_absolute() { expanded } else { cwd.join(expanded) };
	let path = folded(&path);

	if path.symlink_metadata().is_ok() { QueryEnter::Go(path) } else { QueryEnter::Missing(path) }
}

fn looks_like_path(typed: &str) -> bool {
	typed.contains('/') || typed.starts_with('~') || typed.starts_with('.')
}

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

pub(crate) fn shuffle_key(name: &str, seed: u64) -> u64 {
	let mut hash: u64 = 0xcbf2_9ce4_8422_2325;

	for byte in seed.to_le_bytes().iter().chain(name.as_bytes()) {
		hash ^= *byte as u64;
		hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
	}

	hash
}

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

pub(crate) fn followed(parent: &Path, target: &Path) -> PathBuf {
	if target.is_absolute() {
		target.to_owned()
	} else {
		parent.join(target)
	}
}

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

fn parent_path(stack: &[View], cwd: &Path) -> Option<(PathBuf, Option<usize>)> {
	if let Some(above) = stack.len().checked_sub(2) {
		return Some((stack[above].path.clone(), Some(stack[above].cursor)));
	}

	Some((cwd.parent()?.to_owned(), None))
}

fn entry_facts(entry: &Entry) -> crate::list::Facts {
	crate::list::Facts {
		size:  crate::list::size(entry.size),
		when:  crate::list::when(entry.modified),
		btime: crate::list::when(entry.created),
		perms: String::from_utf8_lossy(&chrome::perm(entry.mode)).into_owned(),
		owner: crate::spot::owner(entry.uid),
	}
}

fn file_name(path: &Path) -> Option<String> {
	path.file_name().map(|name| name.to_string_lossy().into_owned())
}

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

		b.dir.cmp(&a.dir).then_with(|| if reverse { order.reverse() } else { order })
	});

	view
}

pub(crate) fn clicked_row(first: usize, i: usize, len: usize) -> Option<usize> {
	let position = first + i;

	(position < len).then_some(position)
}

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

		self.which_expire(&ctx);
		self.keys(&ctx);

		self.shell_poll(&ctx);
		self.open_poll(&ctx);
		self.trash_poll(&ctx);
		self.paste_poll(&ctx);
		self.undo_poll(&ctx);
		self.device_poll(&ctx);
		self.pack_poll(&ctx);
		self.fuzzy_tick(&ctx);
		self.preview.poll();

		let here = self.tab().path().to_owned();
		if self.visited.as_deref() != Some(here.as_path()) {
			self.visited = Some(here.clone());
			self.frecency.visit(&here, now_secs());
			self.frecency_save();
		}

		let pointer = ctx.input(|i| i.pointer.latest_pos());
		self.wheel(&ctx, area, pointer);
		self.hover = pointer;

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

		self.refresh_view();
		self.cache.watch(self.tab().path());
		if let Some((parent, _)) = self.parent_view() {
			self.cache.watch(&parent);
		}
		self.draw(ui, area, body);

		let snapshot = self.entries();
		if !snapshot.loading && !snapshot.entries.is_empty() && !self.first {
			self.first = true;
			metrics::first_list();
			self.shot_wait = Some(2);
		}
		if snapshot.loading
			&& let Some(started) = snapshot.started
			&& started.elapsed() < SKELETON_AFTER
		{
			ctx.request_repaint_after(SKELETON_AFTER - started.elapsed());
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
			self.dressed = true;
			Tokens::install_with(&ctx, &self.tokens);
			ctx.request_repaint();
		}

		metrics::frame_edge(self.frame, false);
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn cursor_for_tests(&self) -> usize {
		self.cursor()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn set_tokens_for_tests(&mut self, tokens: Tokens) {
		self.tokens = tokens;
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn selected_count(&self) -> usize {
		self.selection.len()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn find_open_for_tests(&self) -> bool {
		self.find.is_some()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn filter_text(&self) -> Option<String> {
		self.filter.as_ref().map(|edit| edit.text.clone())
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn fuzzy_query_for_tests(&self) -> Option<String> {
		self.fuzzy.as_ref().map(|fuzzy| fuzzy.query.text.clone())
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn fuzzy_focus_for_tests(&self) -> Option<bool> {
		self.fuzzy.as_ref().map(|fuzzy| fuzzy.focus.inside)
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn fuzzy_rows_for_tests(&self) -> Vec<(&'static str, String)> {
		let Some(fuzzy) = &self.fuzzy else { return Vec::new() };
		fuzzy.hits.iter().filter_map(|hit| fuzzy.items.get(hit.at)).map(|item| (item.zone, item.text.clone())).collect()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn sort_for_tests(&self) -> (SortBy, bool) {
		self.sort
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn columns_for_tests(&self) -> Columns {
		self.columns
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn saw_sequence_for_tests(&self) -> bool {
		self.sequence.is_some()
	}

	pub(crate) fn index_open(&self) -> bool {
		self.fuzzy.as_ref().is_some_and(|fuzzy| fuzzy.mode == FuzzyMode::Index)
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn plus_hit_for_tests(&self) -> Option<egui::Rect> {
		self.plus_hit
	}
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn close_hit_for_tests(&self) -> Option<egui::Rect> {
		self.close_hit
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn tab_close_hit_for_tests(&self, tab: usize) -> Option<egui::Rect> {
		self.tab_close_hits.iter().find(|(index, _)| *index == tab).map(|(_, rect)| *rect)
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn tab_chip_hit_for_tests(&self, tab: usize) -> Option<egui::Rect> {
		self.tab_chip_hits.iter().find(|(index, _)| *index == tab).map(|(_, rect)| *rect)
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn status_index_hit_for_tests(&self) -> Option<egui::Rect> {
		self.status_index
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn yank_paths_for_tests(&self) -> Vec<PathBuf> {
		self.yank.as_ref().map(|yank| yank.paths.clone()).unwrap_or_default()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn yank_cut_for_tests(&self) -> bool {
		self.yank.as_ref().is_some_and(|yank| yank.cut)
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn replay_done_for_tests(&self) -> bool {
		self.seed_next >= self.seed.len()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn undo_labels_for_tests(&self) -> Vec<&'static str> {
		self.undo.iter().map(Undo::label).collect()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn note_for_tests(&mut self, undo: Undo) {
		self.note(String::new(), undo);
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn toast_undo_for_tests(&self) -> bool {
		self.toasts.newest_has_undo()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn menu_app_rows_for_tests(&self) -> Vec<(Option<&'static str>, String)> {
		self.menu
			.as_ref()
			.map(|menu| menu.app_rows().into_iter().map(|row| (row.zone, row.entry.id)).collect())
			.unwrap_or_default()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn menu_scroll_for_tests(&self) -> Option<usize> {
		self.menu.as_ref().map(|menu| menu.scroll)
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn item_rename_for_tests(&self) -> bool {
		self.item_rename.is_some()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn text_modes_for_tests(&self) -> (bool, bool, bool, bool) {
		(self.bulk.is_some(), self.filter.is_some(), self.find.is_some(), self.help)
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn set_terminal_dry_for_tests(&mut self) {
		self.terminal_dry = true;
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn terminal_spawn_for_tests(&mut self) -> Option<TerminalLaunch> {
		self.terminal_log.1.try_recv().ok()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn set_open_dry_for_tests(&mut self) {
		self.open_dry = true;
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn set_mimeapps_for_tests(&mut self, text: String) {
		self.mimeapps_for_tests = Some(text);
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn set_mimeapps_path_for_tests(&mut self, at: PathBuf) {
		self.mimeapps_path_for_tests = Some(at);
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn set_mimeapps_system_path_for_tests(&mut self, at: PathBuf) {
		self.mimeapps_system_path_for_tests = Some(at);
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn set_frecency_path_for_tests(&mut self, at: PathBuf) {
		self.frecency = frecency::Frecency::load_at(&at);
		self.frecency_path_for_tests = Some(at);
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn mimeapps_text_for_tests(&self) -> Option<String> {
		self.mimeapps_for_tests.clone()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn menu_panel_rect_for_tests(&self) -> Option<(f32, f32, f32, f32)> {
		self.menu_panel.map(|panel| (panel.min.x, panel.min.y, panel.width(), panel.height()))
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn open_spawn_for_tests(&mut self) -> Option<PathBuf> {
		self.open_log.1.try_recv().ok()
	}

	pub(crate) fn set_uri_offers_for_tests(&mut self, on: bool) {
		self.uri_offers = on;
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn scroll_offset_for_tests(&self) -> usize {
		self.tab().scroll.offset.round() as usize
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn parent_mouse_for_tests(&self) -> (Option<usize>, f32, usize, usize) {
		let parent = &self.tab().parent;
		(parent.seat, parent.scroll.offset, parent.scroll.rows, parent.scroll.len)
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn cwd_for_tests(&self) -> PathBuf {
		self.tab().path().to_owned()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn hold_selection_for_tests(&mut self, paths: Vec<PathBuf>) {
		self.selection = paths.into_iter().collect();
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn selection_for_tests(&self) -> Vec<PathBuf> {
		let mut paths: Vec<PathBuf> = self.selection.iter().cloned().collect();
		paths.sort();
		paths
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn bookmarked_paths(&self) -> Vec<PathBuf> {
		self.bookmarks.entries.iter().map(|entry| entry.path.clone()).collect()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn listed(&self) -> usize {
		self.tab().view.len()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn in_bin_for_tests(&self) -> bool {
		self.in_bin()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn tabs_for_tests(&self) -> usize {
		self.tabs.len()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn tab_name_for_tests(&self, at: usize) -> String {
		self.tabs[at].name()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn bin_cursor_for_tests(&self) -> Option<usize> {
		self.bin().map(|view| view.cursor)
	}

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

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn bin_scroll_for_tests(&self) -> Option<(f32, f32, usize, usize)> {
		self.bin().map(|view| (view.scroll.offset, view.scroll.target, view.scroll.rows, view.scroll.len))
	}

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

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn preview_offset_for_tests(&self) -> Option<f32> {
		self.preview.image_offset_for_tests()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn preview_extent_for_tests(&self) -> Option<(f32, f32, f32)> {
		self.preview.extent()
	}

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

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn toast_text_for_tests(&self) -> Option<String> {
		self.toasts.items().last().map(|toast| toast.text.clone())
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn toast_widths_for_tests(&self) -> Vec<(f32, f32)> {
		self.toast_widths.clone()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn open_failed_for_tests(&self, path: PathBuf) {
		let _ = self.open_failed.0.send(path);
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn preview_badge_for_tests(&self) -> Option<String> {
		self.preview.badge_for_tests().map(str::to_owned)
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn preview_archive_rows_for_tests(&self) -> Option<usize> {
		self.preview.archive_rows_for_tests()
	}

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

	#[test]
	fn a_relative_target_walks_from_the_links_own_directory() {
		assert_eq!(relative_target(Path::new("/a/x"), Path::new("/a/b/c")), PathBuf::from("../b/c"));
		assert_eq!(relative_target(Path::new("/a/b"), Path::new("/a/b/c")), PathBuf::from("c"));
		assert_eq!(relative_target(Path::new("/a/b/c/d"), Path::new("/a/b")), PathBuf::from("../.."));
		assert_eq!(relative_target(Path::new("/a/b"), Path::new("/a/b")), PathBuf::from(""));

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

	#[test]
	fn the_query_reads_as_a_path_before_it_reads_as_a_search() {
		let dir = std::env::temp_dir().join(format!("crabhop-query-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(dir.join("sub/inner")).expect("a tree");
		std::fs::write(dir.join("a.txt"), b"x").expect("a file");

		let cwd = dir.clone();
		let home = || Some(dir.clone());

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

		assert_eq!(query_enter("/nowhere/at/all", &cwd, home()), QueryEnter::Missing(PathBuf::from("/nowhere/at/all")));
		assert_eq!(query_enter("./missing", &cwd, home()), QueryEnter::Missing(cwd.join("missing")));
		assert_eq!(query_enter("~/nowhere", &cwd, home()), QueryEnter::Missing(dir.join("nowhere")));
		assert_eq!(
			query_enter("sub/nowhere", &cwd, home()),
			QueryEnter::Missing(cwd.join("sub/nowhere")),
			"a name that is not there under a directory that is"
		);

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

		assert_eq!(query_enter("  ~/sub  ", &cwd, home()), QueryEnter::Go(dir.join("sub")));

		let _ = std::fs::remove_dir_all(&dir);
	}

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
		let mut focus = PanelFocus::default();
		assert!(!focus.inside, "a panel opens on its list");
		focus.enter();
		assert!(focus.inside, "f and the box's click put the keyboard in the box");
		assert!(focus.escape(), "the first Esc leaves the box");
		assert!(!focus.inside, "and the list has it again");
		assert!(!focus.escape(), "the second Esc says the panel itself may close");
	}

	#[test]
	fn the_help_filters_its_own_rows() {
		let rows = vec![
			("h".to_owned(), "leave".to_owned(), "返回上级目录".to_owned()),
			("g t".to_owned(), "plugin trash".to_owned(), "回收站".to_owned()),
			("m p".to_owned(), "linemode permissions".to_owned(), "行模式：显示权限".to_owned()),
			(", b".to_owned(), "sort btime --reverse=no".to_owned(), "按创建时间正序".to_owned()),
		];

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

		history.push(Path::new("/d"));
		assert_eq!(history.at(), 2);
		assert_eq!(history.travel(false), None, "nothing after /d");
		assert_eq!(history.travel(true).as_deref(), Some(Path::new("/b")));

		history.push(Path::new("/b"));
		assert_eq!(history.travel(true).as_deref(), Some(Path::new("/a")));
	}
}

#[cfg(test)]
mod selection_tests {
	use super::*;

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

	#[test]
	fn a_switch_clamps_an_index_and_wraps_a_step() {
		assert_eq!(switch_target(5, 2, 0, false), 0, "1 goes to the first tab");
		assert_eq!(switch_target(5, 2, 8, false), 4, "9 clamps to the last");
		assert_eq!(switch_target(5, 0, -1, true), 4, "the previous tab wraps");
		assert_eq!(switch_target(5, 4, 1, true), 0, "and so does the next");
		assert_eq!(switch_target(1, 0, 1, true), 0, "one tab is still one tab");
	}

	#[test]
	fn a_chips_number_takes_room_and_grows_past_nine() {
		assert!(number_width(0) > 0.0, "the badge takes room");
		assert_eq!(number_width(0), number_width(8), "one digit, one width");
		assert!(number_width(9) > number_width(8), "the tenth tab's number is two digits");
		assert!(number_width(99) > number_width(9), "and the hundredth, three");
	}

	#[test]
	fn a_tab_is_named_by_its_directory_or_by_hand() {
		let mut tab = Tab::at(PathBuf::from("/home/someone/Downloads"));
		assert_eq!(tab.name(), "Downloads");
		tab.title = Some("下好的东西".to_owned());
		assert_eq!(tab.name(), "下好的东西");
	}

	#[test]
	fn a_bin_tab_is_named_by_the_bin() {
		let mut tab = Tab::at(PathBuf::from("/home/someone/Downloads"));
		tab.trash = Some(TrashView { items: Vec::new(), dirs: Vec::new(), cursor: 0, loading: true, scroll: Scroll::default() });
		assert_eq!(tab.name(), "回收站", "the chip is the bin's own word");
		tab.title = Some("垃圾堆".to_owned());
		assert_eq!(tab.name(), "垃圾堆", "a name by hand is still the person's");
	}
}
