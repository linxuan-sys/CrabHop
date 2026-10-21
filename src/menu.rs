
use std::path::PathBuf;

use eframe::egui;

use crate::desktop::Entry;
use crate::edit::Edit;

#[derive(Clone, PartialEq)]
pub(crate) enum Target {
	Row(PathBuf, bool),
	Selection(Vec<PathBuf>),
	Folder(PathBuf),
}

impl Target {
	pub(crate) fn paths(&self) -> Vec<PathBuf> {
		match self {
			Target::Row(path, _) => vec![ path.clone() ],
			Target::Selection(paths) => paths.clone(),
			Target::Folder(path) => vec![ path.clone() ],
		}
	}

	pub(crate) fn folder(&self) -> Option<PathBuf> {
		match self {
			Target::Row(path, true) => Some(path.clone()),
			Target::Folder(path) => Some(path.clone()),
			_ => None,
		}
	}

	pub(crate) fn first(&self) -> PathBuf {
		self.paths().into_iter().next().unwrap_or_else(|| PathBuf::from("/"))
	}

	pub(crate) fn title(&self) -> String {
		match self {
			Target::Selection(paths) if paths.len() > 1 => format!("{} 项", paths.len()),
			_ => {
				let path = self.first();
				path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned())
			}
		}
	}

	pub(crate) fn dir(&self) -> bool {
		match self {
			Target::Row(_, dir) => *dir,
			Target::Folder(_) => true,
			Target::Selection(paths) => paths.first().is_some_and(|path| path.is_dir()),
		}
	}

	pub(crate) fn mime(&self) -> String {
		let path = self.first();
		let name = path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned());

		crate::mime::guess(&name, self.dir())
	}
}

pub(crate) enum Item {
	Do(Action, &'static str),
	Off(&'static str),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Action {
	Copy,
	Cut,
	Paste,
	OpenTerminal,
	OpenWith,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Page {
	Main,
	Apps,
}

pub(crate) fn main_items(target: &Target, yank: bool) -> Vec<Item> {
	let mut items = vec![
		Item::Do(Action::Copy, "复制"),
		Item::Do(Action::Cut, "剪切"),
		if yank { Item::Do(Action::Paste, "粘贴") } else { Item::Off("粘贴") },
	];

	if target.folder().is_some() {
		items.push(Item::Do(Action::OpenTerminal, "在终端中打开"));
	}
	items.push(Item::Do(Action::OpenWith, "打开方式…"));

	items
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AppRow {
	pub(crate) zone:        Option<&'static str>,
	pub(crate) entry:       Entry,
	pub(crate) recommended: bool,
}

pub(crate) const ZONES: [&str; 2] = [ "推荐", "全部应用" ];

pub(crate) const MENU_APPS_ROWS: usize = 12;

pub(crate) const MENU_APPS_MIN_ROWS: usize = 6;

pub(crate) const MENU_APPS_MIN_W: f32 = 520.0;

pub(crate) const MENU_ROW_H: f32 = 26.0;
const MENU_APPS_CHROME: f32 = 30.0 + 28.0 + 22.0;

pub(crate) const fn menu_apps_floor() -> f32 {
	MENU_APPS_CHROME + MENU_APPS_MIN_ROWS as f32 * MENU_ROW_H
}

pub(crate) fn zone_of(row: &AppRow) -> &'static str {
	if row.recommended { ZONES[0] } else { ZONES[1] }
}

impl Menu {
	pub(crate) fn standing_open_with(&self) -> bool {
		matches!(main_items(&self.target, self.yank).into_iter().nth(self.at), Some(Item::Do(Action::OpenWith, _)))
	}

	pub(crate) fn visible_rows(&self, window_h: f32) -> usize {
		if self.page != Page::Apps {
			return self.rows_walked();
		}

		let fit = ((window_h - 96.0) / 26.0).floor() - 3.0;
		fit.clamp(1.0, MENU_APPS_ROWS as f32) as usize
	}

	pub(crate) fn follow(&mut self, vis: usize) {
		let vis = vis.max(1);
		let at = self.standing();
		if at < self.scroll {
			self.scroll = at;
		} else if at >= self.scroll + vis {
			self.scroll = at + 1 - vis;
		}

		self.scroll = self.scroll.min(self.rows_walked().saturating_sub(vis));
	}

	pub(crate) fn wheel(&mut self, rows: f32, vis: usize) {
		let walked = self.rows_walked();
		if walked == 0 {
			return;
		}

		let at = (self.standing() as isize + rows.round() as isize).clamp(0, walked as isize - 1) as usize;
		self.seat(at);
		self.follow(vis);
	}
}

impl Menu {
	pub(crate) fn app_rows(&self) -> Vec<AppRow> {
		let needle = self.query.text.trim().to_lowercase();
		let hit = |entry: &Entry| {
			needle.is_empty()
				|| entry.name.to_lowercase().contains(&needle)
				|| entry.id.to_lowercase().contains(&needle)
		};

		let mut rows: Vec<AppRow> = Vec::new();
		for entry in self.apps.iter().filter(|entry| hit(entry)) {
			rows.push(AppRow { zone: rows.is_empty().then_some(ZONES[0]), entry: entry.clone(), recommended: true });
		}
		let mut zone_open = false;
		for entry in self.all.iter().filter(|entry| hit(entry)) {
			rows.push(AppRow { zone: (!zone_open).then_some(ZONES[1]), entry: entry.clone(), recommended: false });
			zone_open = true;
		}

		rows
	}
}

impl Menu {
	pub(crate) fn rows(&self) -> Vec<(String, Option<String>, bool)> {
		match self.page {
			Page::Main => main_items(&self.target, self.yank)
				.into_iter()
				.map(|item| match item {
					Item::Do(_, label) => (label.to_owned(), None, true),
					Item::Off(label) => (label.to_owned(), None, false),
				})
				.collect(),
			Page::Apps => {
				let mut out = Vec::new();
				for row in self.app_rows() {
					if let Some(zone) = row.zone {
						out.push((zone.to_owned(), None, false));
					}
					let hint = row.recommended.then(|| {
						if self.preferred.first().is_some_and(|id| *id == row.entry.id) { "默认".to_owned() } else { "—".to_owned() }
					});
					out.push((row.entry.name.clone(), hint, true));
				}
				out
			}
		}
	}

	pub(crate) fn rows_walked(&self) -> usize {
		if self.page == Page::Apps { self.app_rows().len() } else { self.rows().len() }
	}

	pub(crate) fn standing(&self) -> usize {
		if self.page == Page::Apps { self.apps_at } else { self.at }
	}

	pub(crate) fn seat(&mut self, at: usize) {
		if at >= self.rows_walked() {
			return;
		}

		if self.page == Page::Apps {
			self.apps_at = at;
		} else {
			self.at = at;
		}
	}

	pub(crate) fn title(&self) -> String {
		match &self.page {
			Page::Apps => format!("{} · {}", self.target.title(), self.mime),
			_ => self.target.title(),
		}
	}
}

pub(crate) struct Menu {
	pub(crate) page:    Page,
	pub(crate) target:  Target,
	pub(crate) at:      usize,
	pub(crate) anchor:  egui::Pos2,
	pub(crate) apps:      Vec<Entry>,
	pub(crate) all:       Vec<Entry>,
	pub(crate) apps_at:   usize,
	pub(crate) query:     Edit,
	pub(crate) scroll:    usize,
	pub(crate) mime:      String,
	pub(crate) preferred: Vec<String>,
	pub(crate) yank:    bool,
}

impl Menu {
	pub(crate) fn step(&mut self, by: isize) {
		let rows = self.rows_walked() as isize;
		if rows == 0 {
			return;
		}

		let at = self.standing() as isize;
		let stepped = (at + by).clamp(0, rows - 1) as usize;
		self.seat(stepped);
	}

}

#[cfg(test)]
mod tests {
	use super::*;
	use std::path::Path;

	#[test]
	fn the_folder_item_is_for_folders() {
		let folder = Target::Row(PathBuf::from("/tmp/Pictures"), true);
		let file = Target::Row(PathBuf::from("/tmp/a.png"), false);
		let blank = Target::Folder(PathBuf::from("/tmp"));
		let selection = Target::Selection(vec![ PathBuf::from("/tmp/a.png"), PathBuf::from("/tmp/b.png") ]);

		let words = |target: &Target| -> Vec<&'static str> {
			main_items(target, true)
				.into_iter()
				.map(|item| match item {
					Item::Do(_, label) | Item::Off(label) => label,
				})
				.collect()
		};

		assert_eq!(words(&folder), vec![ "复制", "剪切", "粘贴", "在终端中打开", "打开方式…" ]);
		assert_eq!(words(&blank), vec![ "复制", "剪切", "粘贴", "在终端中打开", "打开方式…" ], "the blank is its own directory");
		assert_eq!(words(&file), vec![ "复制", "剪切", "粘贴", "打开方式…" ], "a file has no folder to open a terminal in");
		assert_eq!(words(&selection), vec![ "复制", "剪切", "粘贴", "打开方式…" ], "two files are not one folder");

		assert_eq!(folder.folder().as_deref(), Some(Path::new("/tmp/Pictures")));
		assert_eq!(blank.folder().as_deref(), Some(Path::new("/tmp")));
		assert!(file.folder().is_none() && selection.folder().is_none());

		assert_eq!(file.mime(), "image/png", "the first path's own type");
		assert_eq!(folder.mime(), "inode/directory");
		assert_eq!(selection.title(), "2 项");
		assert_eq!(file.title(), "a.png");
	}

	#[test]
	fn the_standing_row_stays_inside_its_page() {
		let mut menu = Menu {
			page:      Page::Apps,
			target:    Target::Row(PathBuf::from("/tmp/Pictures"), true),
			at:        0,
			anchor:    egui::Pos2::ZERO,
			apps:      Vec::new(),
			all:       Vec::new(),
			apps_at:   0,
			query:     Edit::new(String::new()),
			scroll:    0,
			mime:      "image/png".to_owned(),
			preferred: Vec::new(),
			yank:      false,
		};

		menu.step(1);
		assert_eq!(menu.apps_at, 0, "an empty page has nowhere to stand");
		assert_eq!(menu.at, 0, "and the main page's row is not moved by a page that is not up");

		menu.page = Page::Main;
		menu.step(1);
		assert_eq!(menu.at, 1, "the main page's own row moves");
		menu.step(9);
		assert_eq!(menu.at, 4, "and stops at its last item — the fifth, the folder's own roster (P33⑱)");
		menu.step(-9);
		assert_eq!(menu.at, 0);
	}

	#[test]
	fn the_roster_is_five_and_paste_knows_when_it_cannot() {
		let label = |item: &Item| match item {
			Item::Do(_, label) | Item::Off(label) => *label,
		};
		let live = |item: &Item| matches!(item, Item::Do(..));

		let file = Target::Row(PathBuf::from("/tmp/note.txt"), false);
		let held: Vec<&str> = main_items(&file, true).iter().map(label).collect();
		assert_eq!(held, vec![ "复制", "剪切", "粘贴", "打开方式…" ], "a file gets the family and the apps");
		assert!(main_items(&file, true).iter().all(live), "and with a yank every one of them is live");

		let empty = main_items(&file, false);
		assert!(!live(&empty[2]), "with nothing held, paste is shown and cannot be taken");
		assert_eq!(label(&empty[2]), "粘贴", "the words stay where the mouse looks for them");

		let folder = Target::Row(PathBuf::from("/tmp/Pictures"), true);
		let names: Vec<&str> = main_items(&folder, true).iter().map(label).collect();
		assert_eq!(names, vec![ "复制", "剪切", "粘贴", "在终端中打开", "打开方式…" ], "a folder gets its terminal too");

		let blank = Target::Folder(PathBuf::from("/tmp"));
		assert!(main_items(&blank, false).iter().any(|item| label(item) == "在终端中打开"), "and so does the blank's own directory");
	}

	#[test]
	fn the_applications_page_holds_two_zones_and_one_query() {
		let entry = |id: &str, name: &str, mimes: &[&str]| Entry {
			id:     id.to_owned(),
			name:   name.to_owned(),
			exec:   "run %f".to_owned(),
			mimes:  mimes.iter().map(|mime| (*mime).to_owned()).collect(),
			nodisplay: false,
			terminal:  false,
			try_exec:  None,
		};
		let make = || Menu {
			page:      Page::Apps,
			target:    Target::Row(PathBuf::from("/tmp/notes.txt"), false),
			at:        0,
			anchor:    egui::Pos2::ZERO,
			apps:      vec![
				entry("libreoffice-writer.desktop", "LibreOffice Writer", &[ "text/plain" ]),
				entry("nvim.desktop", "Neovim", &[ "text/plain" ]),
			],
			all:       vec![
				entry("chromium.desktop", "Chromium", &[ "text/html" ]),
				entry("libreoffice-writer.desktop", "LibreOffice Writer", &[ "text/plain" ]),
				entry("nvim.desktop", "Neovim", &[ "text/plain" ]),
				entry("vlc.desktop", "VLC", &[ "video/mp4" ]),
			],
			apps_at:   0,
			query:     Edit::new(String::new()),
			scroll:    0,
			mime:      "text/plain".to_owned(),
			preferred: Vec::new(),
			yank:      false,
		};

		let rows = make().app_rows();
		assert_eq!(rows[0].zone, Some("推荐"), "the recommended zone opens the page");
		assert!(rows[0].recommended && rows[1].recommended, "its rows stand together");
		let full = rows.iter().position(|row| !row.recommended).expect("the full zone follows");
		assert_eq!(rows[full].zone, Some("全部应用"), "with its own header");
		assert!(rows[full + 1..].iter().all(|row| row.zone.is_none()), "and one header each");
		assert!(!rows[..full].iter().any(|row| !row.recommended), "no full-zone row sneaks up front");

		let ids: Vec<&str> = rows[full..].iter().map(|row| row.entry.id.as_str()).collect();
		assert!(ids.contains(&"nvim.desktop") && ids.contains(&"chromium.desktop") && ids.contains(&"libreoffice-writer.desktop"), "{ids:?}");
		assert!(ids.contains(&"vlc.desktop"), "a program for entirely other types is there too");
		let names: Vec<String> = rows[full..].iter().map(|row| row.entry.name.to_lowercase()).collect();
		let mut sorted = names.clone();
		sorted.sort();
		assert_eq!(names, sorted, "by name inside the zone");

		let drawn = make().rows();
		assert_eq!(drawn[0], ("推荐".to_owned(), None, false), "the header is its own line");
		assert_eq!(drawn[1].0, "LibreOffice Writer", "the first candidate follows it");
		assert!(drawn.iter().any(|row| row.0 == "全部应用" && !row.2), "and the full zone's header too");

		let mut filtered = make();
		filtered.query = Edit::new("chrom".to_owned());
		let rows = filtered.app_rows();
		assert_eq!(rows.len(), 1, "{rows:?}");
		assert_eq!(rows[0].entry.id, "chromium.desktop");
		assert_eq!(rows[0].zone, Some("全部应用"), "the browser comes from the full zone");

		let mut filtered = make();
		filtered.query = Edit::new("nvim".to_owned());
		let rows = filtered.app_rows();
		assert_eq!(rows.iter().map(|row| row.entry.id.as_str()).collect::<Vec<_>>(), vec![ "nvim.desktop", "nvim.desktop" ], "both zones answer");
		assert_eq!(rows[0].zone, Some("推荐"));
		assert_eq!(rows[1].zone, Some("全部应用"));
	}

	#[test]
	fn the_applications_window_caps_and_walks_with_the_standing_row() {
		let entry = |n: usize| Entry {
			id:     format!("app{n:02}.desktop"),
			name:   format!("App {n:02}"),
			exec:   "run %f".to_owned(),
			mimes:  vec![ "text/plain".to_owned() ],
			nodisplay: false,
			terminal:  false,
			try_exec:  None,
		};
		let mut menu = Menu {
			page:      Page::Apps,
			target:    Target::Row(PathBuf::from("/tmp/notes.txt"), false),
			at:        0,
			anchor:    egui::Pos2::ZERO,
			apps:      Vec::new(),
			all:       (0..30).map(entry).collect(),
			apps_at:   0,
			query:     Edit::new(String::new()),
			scroll:    0,
			mime:      "text/plain".to_owned(),
			preferred: Vec::new(),
			yank:      false,
		};

		assert_eq!(menu.visible_rows(900.0), MENU_APPS_ROWS, "a tall window shows the cap");
		assert_eq!(menu.visible_rows(800.0), MENU_APPS_ROWS);
		assert_eq!(menu.visible_rows(200.0), 1, "a short window narrows the window");
		assert!(menu.visible_rows(120.0) >= 1, "and never to nothing");
		menu.page = Page::Main;
		assert_eq!(menu.visible_rows(200.0), menu.rows_walked(), "the short pages show everything they have");
		menu.page = Page::Apps;

		let vis = menu.visible_rows(900.0);
		assert_eq!(vis, 12);
		menu.wheel(20.0, vis);
		assert_eq!(menu.standing(), 20, "the wheel walks the standing row");
		assert_eq!(menu.scroll, 20 + 1 - vis, "and the window follows it");
		menu.wheel(-100.0, vis);
		assert_eq!(menu.standing(), 0, "a wheel to the top walks back");
		assert_eq!(menu.scroll, 0);
		menu.wheel(100.0, vis);
		assert_eq!(menu.standing(), 29, "and to the last row");
		assert_eq!(menu.scroll, 30 - vis, "with the window clamped to the last page");

		menu.seat(0);
		menu.follow(vis);
		assert_eq!(menu.scroll, 0, "the top row's window starts at the top");
		for _ in 0..13 {
			menu.step(1);
			menu.follow(vis);
		}
		assert_eq!(menu.standing(), 13);
		assert_eq!(menu.scroll, 13 + 1 - vis, "the window walked down with the highlight");

		menu.query = Edit::new("app1".to_owned());
		menu.apps_at = 0;
		menu.scroll = 0;
		let rows = menu.app_rows();
		assert_eq!(rows.len(), 10, "app10..app19 answer");
		assert_eq!(menu.visible_rows(900.0), MENU_APPS_ROWS, "and the window still caps at the same twelve");
	}
}
