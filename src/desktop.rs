
use std::path::{Path, PathBuf};

pub(crate) fn mime_of(name: &str, dir: bool) -> String {
	if dir {
		return "inode/directory".to_owned();
	}

	let ext = name.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();

	match ext.as_str() {
		"png" => "image/png",
		"jpg" | "jpeg" => "image/jpeg",
		"gif" => "image/gif",
		"webp" => "image/webp",
		"svg" => "image/svg+xml",
		"avif" => "image/avif",
		"bmp" => "image/bmp",
		"tif" | "tiff" => "image/tiff",
		"mp4" | "m4v" => "video/mp4",
		"mkv" => "video/x-matroska",
		"webm" => "video/webm",
		"mov" => "video/quicktime",
		"avi" => "video/x-msvideo",
		"mp3" => "audio/mpeg",
		"flac" => "audio/flac",
		"wav" => "audio/x-wav",
		"ogg" | "oga" => "audio/ogg",
		"opus" => "audio/opus",
		"m4a" => "audio/mp4",
		"pdf" => "application/pdf",
		"zip" => "application/zip",
		"tar" => "application/x-tar",
		"gz" | "tgz" => "application/gzip",
		"bz2" => "application/x-bzip2",
		"xz" => "application/x-xz",
		"7z" => "application/x-7z-compressed",
		"rar" => "application/vnd.rar",
		"txt" | "log" => "text/plain",
		"md" | "markdown" => "text/markdown",
		"html" | "htm" => "text/html",
		"csv" => "text/csv",
		"json" => "application/json",
		"toml" => "application/toml",
		"yaml" | "yml" => "application/yaml",
		_ => "application/octet-stream",
	}
	.to_owned()
}

pub(crate) fn app_dirs(home: Option<&Path>) -> Vec<PathBuf> {
	let mut dirs = vec![ PathBuf::from("/usr/share/applications"), PathBuf::from("/usr/local/share/applications") ];
	if let Some(home) = home {
		dirs.push(home.join(".local/share/applications"));
	}

	dirs
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Entry {
	pub id:    String,
	pub name:  String,
	pub exec:  String,
	pub mimes: Vec<String>,
	pub nodisplay: bool,
	pub terminal: bool,
	pub try_exec: Option<String>,
}

pub(crate) fn parse_entry(text: &str, id: &str) -> Option<Entry> {
	let mut section = String::new();
	let (mut name, mut exec, mut mimes, mut nodisplay, mut hidden, mut kind) = (None, None, Vec::new(), false, false, None);
	let (mut terminal, mut try_exec) = (false, None);

	for line in text.lines() {
		let line = line.trim();
		if line.starts_with('[') && line.ends_with(']') {
			section = line[1..line.len() - 1].to_owned();
			continue;
		}
		if section != "Desktop Entry" || line.starts_with('#') {
			continue;
		}

		let Some((key, value)) = line.split_once('=') else { continue };
		match key.trim() {
			"Type" => kind = Some(value.trim().to_owned()),
			"Name" => {
				if name.is_none() {
					name = Some(value.trim().to_owned());
				}
			}
			"Exec" => exec = Some(value.trim().to_owned()),
			"MimeType" => {
				mimes = value.split(';').map(str::trim).filter(|mime| !mime.is_empty()).map(str::to_owned).collect();
			}
			"NoDisplay" if value.trim() == "true" => nodisplay = true,
			"Hidden" if value.trim() == "true" => hidden = true,
			"Terminal" if value.trim() == "true" => terminal = true,
			"TryExec" => try_exec = Some(value.trim().to_owned()),
			_ => {}
		}
	}

	if hidden || kind.as_deref() != Some("Application") {
		return None;
	}

	Some(Entry { id: id.to_owned(), name: name?, exec: exec?, mimes, nodisplay, terminal, try_exec })
}

pub(crate) fn scan(dirs: &[PathBuf]) -> Vec<Entry> {
	let mut entries: Vec<Entry> = Vec::new();

	for dir in dirs {
		let Ok(read) = std::fs::read_dir(dir) else { continue };
		for file in read.flatten() {
			let path = file.path();
			let Some(name) = path.file_name().and_then(|name| name.to_str()) else { continue };
			if !name.ends_with(".desktop") {
				continue;
			}

			let Ok(text) = std::fs::read_to_string(&path) else { continue };
			if let Some(entry) = parse_entry(&text, name) {
				if let Some(at) = entries.iter().position(|other| other.id == entry.id) {
					if !(entry.nodisplay && !entries[at].nodisplay) {
						entries[at] = entry;
					}
				} else {
					entries.push(entry);
				}
			}
		}
	}

	entries.sort_by(|a, b| a.id.cmp(&b.id));
	entries
}

pub(crate) fn preferred_in(text: &str, mime: &str) -> Vec<String> {
	section_line(text, "Default Applications", mime)
}

pub(crate) fn added_in(text: &str, mime: &str) -> Vec<String> {
	section_line(text, "Added Associations", mime)
}

fn section_line(text: &str, wanted: &str, mime: &str) -> Vec<String> {
	let mut section = String::new();

	for line in text.lines() {
		let line = line.trim();
		if line.starts_with('[') && line.ends_with(']') {
			section = line[1..line.len() - 1].to_owned();
			continue;
		}
		if section != wanted {
			continue;
		}

		if let Some((key, value)) = line.split_once('=')
			&& key.trim() == mime
		{
			return value.split(';').map(str::trim).filter(|id| !id.is_empty()).map(str::to_owned).collect();
		}
	}

	Vec::new()
}

pub(crate) fn handlers<'a>(
	entries: &'a [Entry],
	types: &[String],
	preferred: &[String],
	added: &[String],
	path_env: &str,
) -> Vec<&'a Entry> {
	let mut found: Vec<&Entry> = entries
		.iter()
		.filter(|entry| try_exec_ok(entry, path_env))
		.filter(|entry| {
			added.contains(&entry.id) || entry.mimes.iter().any(|mime| types.iter().any(|kind| kind == mime))
		})
		.collect();
	found.sort_by_key(|entry| {
		let rank = preferred.iter().position(|id| *id == entry.id).unwrap_or(preferred.len());
		(rank, entry.name.to_lowercase())
	});

	found
}

pub(crate) fn all_apps(entries: &[Entry], path_env: &str) -> Vec<Entry> {
	let mut all: Vec<Entry> = entries.iter().filter(|entry| try_exec_ok(entry, path_env)).cloned().collect();
	all.sort_by_key(|entry| entry.name.to_lowercase());

	all
}

pub(crate) fn try_exec_ok(entry: &Entry, path_env: &str) -> bool {
	let Some(program) = &entry.try_exec else { return true };

	if program.contains('/') {
		return is_executable(Path::new(program));
	}

	which_in(path_env, program).is_some()
}

pub(crate) fn launch_argv(program: Vec<String>, terminal: Option<&[String]>) -> Vec<String> {
	match terminal {
		Some(chain) => chain.iter().cloned().chain(program).collect(),
		None => program,
	}
}

fn words(exec: &str) -> Vec<String> {
	let mut out = Vec::new();
	let (mut word, mut quoted) = (String::new(), false);

	for c in exec.chars() {
		match c {
			'"' => quoted = !quoted,
			'\\' if quoted => {}
			' ' | '\t' if !quoted => {
				if !word.is_empty() {
					out.push(std::mem::take(&mut word));
				}
			}
			_ => word.push(c),
		}
	}

	if !word.is_empty() {
		out.push(word);
	}

	out
}

pub(crate) fn exec_argv(exec: &str, path: &Path, _mime: &str) -> Vec<String> {
	let path = path.to_string_lossy();
	let mut argv = Vec::new();
	let mut placed = false;

	for word in words(exec) {
		if !word.contains('%') {
			argv.push(word);
			continue;
		}

		let mut built = String::new();
		let mut chars = word.chars();
		while let Some(c) = chars.next() {
			if c != '%' {
				built.push(c);
				continue;
			}

			match chars.next() {
				Some('f' | 'u' | 'F' | 'U') => {
					built.push_str(&path);
					placed = true;
				}
				Some('%') => built.push('%'),
				Some(_) => {}
				None => built.push('%'),
			}
		}

		if !built.is_empty() {
			argv.push(built);
		}
	}

	if !placed {
		argv.push(path.to_string());
	}

	argv
}

pub(crate) fn command_argv(exec: &str) -> Vec<String> {
	words(exec)
		.into_iter()
		.map(|word| {
			let mut built = String::new();
			let mut chars = word.chars();
			while let Some(c) = chars.next() {
				if c != '%' {
					built.push(c);
					continue;
				}

				match chars.next() {
					Some('%') => built.push('%'),
					Some(_) => {}
					None => built.push('%'),
				}
			}
			built
		})
		.filter(|word| !word.is_empty())
		.collect()
}

pub(crate) fn default_terminal(entries: &[Entry], user: &str, system: &str, mime: &str, path: &Path) -> Option<Vec<String>> {
	let id = preferred_in(user, mime)
		.into_iter()
		.next()
		.or_else(|| preferred_in(system, mime).into_iter().next())?;

	entries.iter().find(|entry| entry.id == id).filter(|entry| entry.terminal).map(|entry| exec_argv(&entry.exec, path, mime))
}

pub(crate) fn merge_defaults(text: &str, mime: &str, id: &str) -> String {
	let line = format!("{mime}={id};");
	let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
	let mut section: Option<(usize, usize)> = None;
	let mut inside = false;

	for (at, raw) in lines.iter().enumerate() {
		let trimmed = raw.trim();
		if trimmed.starts_with('[') && trimmed.ends_with(']') {
			let name = &trimmed[1..trimmed.len() - 1];
			inside = name == "Default Applications" && section.is_none();
			if inside {
				section = Some((at, at));
			}
			continue;
		}

		if inside
			&& let Some((_, end)) = section.as_mut()
			&& trimmed.split_once('=').is_some_and(|(key, _)| key.trim() == mime)
		{
			*end = at;
		}
	}

	match section {
		Some((header, last)) if last > header => {
			lines[last] = line;
		}
		Some((header, _)) => lines.insert(header + 1, line),
		None => {
			if !lines.is_empty() && !lines.last().is_some_and(|line| line.is_empty()) {
				lines.push(String::new());
			}
			lines.push("[Default Applications]".to_owned());
			lines.push(line);
		}
	}

	let mut text = lines.join("\n");
	text.push('\n');
	text
}

pub(crate) fn write_atomic(at: &Path, text: &str) -> std::io::Result<()> {
	use std::os::unix::fs::PermissionsExt;

	let Some(dir) = at.parent() else {
		return Err(std::io::Error::other("no directory to write into"));
	};
	crate::config::Config::ensure(dir)?;

	let mode = std::fs::metadata(at).ok().map(|meta| meta.permissions().mode() & 0o777);
	let name = at.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned());
	let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
	std::fs::write(&temp, text)?;
	std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(mode.unwrap_or(0o600)))?;
	std::fs::rename(&temp, at)
}

pub(crate) fn which_in(path_env: &str, name: &str) -> Option<PathBuf> {
	path_env.split(':').filter(|dir| !dir.is_empty()).map(|dir| Path::new(dir).join(name)).find(|at| is_executable(at))
}

fn is_executable(at: &Path) -> bool {
	use std::os::unix::fs::PermissionsExt;

	std::fs::metadata(at).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

pub(crate) fn terminal(
	path_env: &str,
	env_terminal: Option<&str>,
	xdg_default: Option<&str>,
	entries: &[Entry],
) -> Option<Vec<String>> {
	if which_in(path_env, "xdg-terminal-exec").is_some() {
		return Some(vec![ "xdg-terminal-exec".to_owned() ]);
	}

	if let Some(id) = xdg_default
		&& let Some(entry) = entries.iter().find(|entry| entry.id == id)
	{
		let argv = command_argv(&entry.exec);
		if !argv.is_empty() {
			return Some(argv);
		}
	}

	if let Some(terminal) = env_terminal {
		let argv: Vec<String> = terminal.split_whitespace().map(str::to_owned).collect();
		if !argv.is_empty() {
			return Some(argv);
		}
	}

	for name in [ "kitty", "alacritty", "foot", "wezterm" ] {
		if which_in(path_env, name).is_some() {
			return Some(vec![ name.to_owned() ]);
		}
	}

	None
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_mime_table_names_the_common_types() {
		assert_eq!(mime_of("a.png", false), "image/png");
		assert_eq!(mime_of("A.JPG", false), "image/jpeg");
		assert_eq!(mime_of("clip.mp4", false), "video/mp4");
		assert_eq!(mime_of("notes.txt", false), "text/plain");
		assert_eq!(mime_of("a.zzz", false), "application/octet-stream", "not a type this window names");
		assert_eq!(mime_of("no-extension", false), "application/octet-stream");
		assert_eq!(mime_of("Pictures", true), "inode/directory", "a directory is a directory");
		assert_eq!(mime_of("weird.png", true), "inode/directory", "whatever it is called");
	}

	#[test]
	fn an_entry_is_parsed_from_its_own_section() {
		let text = "\
[Desktop Entry]
Type=Application
Name=Feh
GenericName=Image viewer
Exec=feh --start-at %f
MimeType=image/png;image/jpeg;
NoDisplay=false
Terminal=false

[Desktop Action new-window]
Name=New Window
Exec=feh
";
		let entry = parse_entry(text, "feh.desktop").expect("an application");
		assert_eq!(entry.name, "Feh");
		assert_eq!(entry.exec, "feh --start-at %f");
		assert_eq!(entry.mimes, vec![ "image/png", "image/jpeg" ]);
		assert!(!entry.nodisplay);

		let hidden = "\
[Desktop Entry]
Type=Application
Name=Kitten
Exec=kitten
MimeType=image/png;
NoDisplay=true
";
		assert!(parse_entry(hidden, "kitten.desktop").expect("an application").nodisplay);

		let gone = "[Desktop Entry]\nType=Application\nName=X\nExec=x\nHidden=true\n";
		assert!(parse_entry(gone, "x.desktop").is_none(), "a hidden entry is not there");
		let link = "[Desktop Entry]\nType=Link\nName=X\nExec=x\n";
		assert!(parse_entry(link, "x.desktop").is_none(), "and neither is a link");
		let bare = "[Desktop Entry]\nName=No command\n";
		assert!(parse_entry(bare, "x.desktop").is_none(), "an entry with no command offers nothing");
	}

	#[test]
	fn the_handlers_follow_the_persons_own_order() {
		let entry = |id: &str, name: &str, mimes: &[&str], nodisplay: bool| Entry {
			id:    id.to_owned(),
			name:  name.to_owned(),
			exec:  "run %f".to_owned(),
			mimes: mimes.iter().map(|mime| (*mime).to_owned()).collect(),
			nodisplay,
			terminal: false,
			try_exec: None,
		};
		let entries = vec![
			entry("a.desktop", "Alpha", &[ "image/png" ], false),
			entry("b.desktop", "Beta", &[ "image/png", "image/jpeg" ], false),
			entry("c.desktop", "Gamma", &[ "image/png" ], true),
			entry("d.desktop", "Delta", &[ "text/plain" ], false),
		];
		let exact = |mime: &str| vec![ mime.to_owned() ];

		let found = handlers(&entries, &exact("image/png"), &[ "b.desktop".to_owned() ], &[], "");
		let names: Vec<&str> = found.iter().map(|entry| entry.name.as_str()).collect();
		assert_eq!(names, vec![ "Beta", "Alpha", "Gamma" ], "the preferred entry first, then by name — the NoDisplay one included (P39)");

		let found = handlers(&entries, &exact("image/png"), &[], &[], "");
		assert_eq!(found.first().map(|entry| entry.name.as_str()), Some("Alpha"), "no preference: by name");

		assert!(handlers(&entries, &exact("video/mp4"), &[], &[], "").is_empty(), "nothing claims that type");

		let types = vec![ "text/markdown".to_owned(), "text/plain".to_owned() ];
		let names: Vec<&str> = handlers(&entries, &types, &[], &[], "").iter().map(|entry| entry.name.as_str()).collect();
		assert_eq!(names, vec![ "Delta" ], "the parent's own editors answer for the child");

		let found = handlers(&entries, &exact("video/mp4"), &[], &[ "a.desktop".to_owned() ], "");
		let names: Vec<&str> = found.iter().map(|entry| entry.name.as_str()).collect();
		assert_eq!(names, vec![ "Alpha" ], "an added association is a candidate");

		let mut ghost = entries.clone();
		ghost.push(Entry {
			id:       "e.desktop".to_owned(),
			name:     "Epsilon".to_owned(),
			exec:     "epsilon %f".to_owned(),
			mimes:    vec![ "image/png".to_owned() ],
			nodisplay: false,
			terminal: false,
			try_exec: Some("not-a-program-anywhere".to_owned()),
		});
		let names: Vec<&str> = handlers(&ghost, &exact("image/png"), &[], &[], "").iter().map(|entry| entry.name.as_str()).collect();
		assert!(!names.contains(&"Epsilon"), "a TryExec that is not there hides the entry");
	}

	#[test]
	fn a_command_line_stands_in_for_the_file() {
		let path = Path::new("/tmp/a picture.png");
		let argv = |exec: &str| exec_argv(exec, path, "image/png");

		assert_eq!(argv("feh %f"), vec![ "feh", "/tmp/a picture.png" ]);
		assert_eq!(argv("xdg-open %u"), vec![ "xdg-open", "/tmp/a picture.png" ]);
		assert_eq!(argv("app --file=%f"), vec![ "app", "--file=/tmp/a picture.png" ]);
		assert_eq!(argv("app %F"), vec![ "app", "/tmp/a picture.png" ]);
		assert_eq!(argv("app %U"), vec![ "app", "/tmp/a picture.png" ]);
		assert_eq!(argv("app %i %c"), vec![ "app", "/tmp/a picture.png" ], "the codes that name nothing are dropped, and the path appended");
		assert_eq!(argv("app %f %i"), vec![ "app", "/tmp/a picture.png" ], "the icon code is dropped where it stands");
		assert_eq!(argv("app \"two words\" %f"), vec![ "app", "two words", "/tmp/a picture.png" ], "a quoted argument stays one");
		assert_eq!(argv("app %"), vec![ "app", "%", "/tmp/a picture.png" ], "a lone percent is nobody's code, and stands as it was written");
	}

	#[test]
	fn the_default_terminal_answers_only_for_a_terminal_entry() {
		let entry = |id: &str, terminal: bool| Entry {
			id:     id.to_owned(),
			name:   id.to_owned(),
			exec:   "run %f".to_owned(),
			mimes:  vec![ "text/plain".to_owned() ],
			nodisplay: false,
			terminal,
			try_exec:  None,
		};
		let entries = vec![ entry("nvim.desktop", true), entry("writer.desktop", false) ];
		let path = std::path::Path::new("/tmp/notes.txt");
		let nvim = "[Default Applications]\ntext/plain=nvim.desktop;\n";
		let writer = "[Default Applications]\ntext/plain=writer.desktop;\n";

		assert_eq!(
			default_terminal(&entries, nvim, "", "text/plain", path),
			Some(vec![ "run".to_owned(), "/tmp/notes.txt".to_owned() ]),
			"the terminal entry answers with its own command, %f placed"
		);

		assert_eq!(
			default_terminal(&entries, "", nvim, "text/plain", path),
			Some(vec![ "run".to_owned(), "/tmp/notes.txt".to_owned() ]),
			"and the system's own ledger stands behind the person's"
		);

		assert_eq!(
			default_terminal(&entries, writer, nvim, "text/plain", path),
			None,
			"the person's own line wins over the system's — and a plain entry is xdg-open's business"
		);

		assert_eq!(default_terminal(&entries, writer, "", "text/plain", path), None, "a plain entry answers nothing");
		assert_eq!(
			default_terminal(&entries, "[Default Applications]\nimage/png=nvim.desktop;\n", "", "text/plain", path),
			None,
			"and another type's default is not this one's"
		);
	}

	#[test]
	fn the_defaults_merge_keeps_everything_else() {
		let text = "\
# mine
[Added Associations]
image/png=a.desktop;

[Default Applications]
image/png=old.desktop;
text/plain=editor.desktop;

[Removed Associations]
image/png=c.desktop;
";
		let merged = merge_defaults(text, "image/png", "new.desktop");
		assert!(merged.contains("# mine"), "the comment stays");
		assert!(merged.contains("[Added Associations]\nimage/png=a.desktop;"), "another section stays whole");
		assert!(merged.contains("[Default Applications]\nimage/png=new.desktop;\ntext/plain=editor.desktop;"), "{merged}");
		assert!(merged.contains("[Removed Associations]\nimage/png=c.desktop;"), "and so does the last one");
		assert_eq!(merged.matches("image/png=new.desktop").count(), 1);

		let merged = merge_defaults(text, "video/mp4", "player.desktop");
		assert!(merged.contains("[Default Applications]\nvideo/mp4=player.desktop;\nimage/png=old.desktop;"), "{merged}");

		let merged = merge_defaults("[Added Associations]\nimage/png=a.desktop;\n", "image/png", "new.desktop");
		assert!(merged.ends_with("[Default Applications]\nimage/png=new.desktop;\n"), "{merged}");

		let merged = merge_defaults("", "image/png", "new.desktop");
		assert_eq!(merged, "[Default Applications]\nimage/png=new.desktop;\n", "an empty file gets the section");
	}

	#[test]
	fn the_terminal_chain_takes_the_first_it_finds() {
		let dir = std::env::temp_dir().join(format!("crabhop-terminal-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");
		let put = |name: &str| {
			let at = dir.join(name);
			std::fs::write(&at, b"#!/bin/sh\n").expect("a script");
			{
				use std::os::unix::fs::PermissionsExt;
				std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).expect("executable");
			}
		};
		put("xdg-terminal-exec");
		put("kitty");
		put("foot");

		let path = dir.to_str().unwrap().to_owned();
		let entries = vec![ Entry {
			id:    "kgx.desktop".to_owned(),
			name:  "Console".to_owned(),
			exec:  "kgx -- %U".to_owned(),
			mimes: vec![ "x-scheme-handler/terminal".to_owned() ],
			nodisplay: false,
			terminal: false,
			try_exec: None,
		} ];

		assert_eq!(terminal(&path, Some("wezterm"), Some("kgx.desktop"), &entries), Some(vec![ "xdg-terminal-exec".to_owned() ]));

		let without = std::env::temp_dir().join(format!("crabhop-terminal-empty-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&without);
		std::fs::create_dir_all(&without).expect("a directory");
		let bare_path = without.to_str().unwrap().to_owned();
		assert_eq!(terminal(&bare_path, Some("wezterm"), Some("kgx.desktop"), &entries), Some(vec![ "kgx".to_owned(), "--".to_owned() ]));

		assert_eq!(terminal(&bare_path, Some("alacritty --class float"), None, &entries), Some(vec![ "alacritty".to_owned(), "--class".to_owned(), "float".to_owned() ]));

		assert_eq!(terminal(&path, None, None, &[]), Some(vec![ "xdg-terminal-exec".to_owned() ]));
		let kitty_only = std::env::temp_dir().join(format!("crabhop-terminal-kitty-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&kitty_only);
		std::fs::create_dir_all(&kitty_only).expect("a directory");
		let at = kitty_only.join("kitty");
		std::fs::write(&at, b"#!/bin/sh\n").expect("a script");
		{
			use std::os::unix::fs::PermissionsExt;
			std::fs::set_permissions(&at, std::fs::Permissions::from_mode(0o755)).expect("executable");
		}
		assert_eq!(terminal(kitty_only.to_str().unwrap(), None, None, &[]), Some(vec![ "kitty".to_owned() ]));

		assert!(terminal(&bare_path, None, None, &entries).is_none(), "no terminal this chain can name");

		let _ = std::fs::remove_dir_all(&dir);
		let _ = std::fs::remove_dir_all(&without);
		let _ = std::fs::remove_dir_all(&kitty_only);
	}

	#[test]
	fn the_scan_reads_what_the_directories_hold() {
		let root = std::env::temp_dir().join(format!("crabhop-scan-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(root.join("system")).expect("a directory");
		std::fs::create_dir_all(root.join("local")).expect("a directory");
		std::fs::write(root.join("system/a.desktop"), "[Desktop Entry]\nType=Application\nName=System A\nExec=sa %f\n").unwrap();
		std::fs::write(root.join("local/a.desktop"), "[Desktop Entry]\nType=Application\nName=Local A\nExec=la %f\n").unwrap();
		std::fs::write(root.join("system/b.desktop"), "[Desktop Entry]\nType=Application\nName=Bee\nExec=b %f\n").unwrap();
		std::fs::write(root.join("system/junk.txt"), "not an entry").unwrap();

		let entries = scan(&[ root.join("system"), root.join("local") ]);
		let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
		assert_eq!(names, vec![ "Local A", "Bee" ], "the local entry wins, and junk is not read: {names:?}");

		let _ = std::fs::remove_dir_all(&root);
	}

	#[test]
	fn a_hiding_local_copy_does_not_mask_the_installed_entry() {
		let root = std::env::temp_dir().join(format!("crabhop-shadow-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(root.join("system")).expect("a directory");
		std::fs::create_dir_all(root.join("local")).expect("a directory");

		let visible = "[Desktop Entry]\nType=Application\nName=Neovim\nExec=nvim %F\nTerminal=true\nTryExec=nvim\nMimeType=text/plain;text/x-csrc;\n";
		let hidden = format!("{visible}NoDisplay=true\n");
		std::fs::write(root.join("system/nvim.desktop"), visible).unwrap();
		std::fs::write(root.join("local/nvim.desktop"), &hidden).unwrap();

		let entries = scan(&[ root.join("system"), root.join("local") ]);
		let nvim = entries.iter().find(|entry| entry.id == "nvim.desktop").expect("the app stands, once");
		assert!(!nvim.nodisplay, "the visible copy stands over the hiding one");
		assert!(nvim.terminal, "and it keeps its own Terminal=true");
		assert_eq!(nvim.try_exec.as_deref(), Some("nvim"), "and its TryExec");

		let types = vec![ "text/plain".to_owned() ];
		let listed: Vec<&str> =
			handlers(&entries, &types, &[], &[], &std::env::var("PATH").unwrap_or_default()).iter().map(|e| e.id.as_str()).collect();
		assert_eq!(listed, vec![ "nvim.desktop" ], "and it is a candidate for text/plain");

		let renamed = "[Desktop Entry]\nType=Application\nName=Mine Nvim\nExec=nvim %F\nMimeType=text/plain;\n";
		std::fs::write(root.join("local/nvim.desktop"), renamed).unwrap();
		let entries = scan(&[ root.join("system"), root.join("local") ]);
		let nvim = entries.iter().find(|entry| entry.id == "nvim.desktop").expect("one nvim");
		assert_eq!(nvim.name, "Mine Nvim", "the local copy wins when it shows itself");

		let _ = std::fs::remove_dir_all(&root);
	}

	#[test]
	fn the_installed_nvim_parses_and_is_a_candidate() {
		let Ok(text) = std::fs::read_to_string("/usr/share/applications/nvim.desktop") else { return };

		let entry = parse_entry(&text, "nvim.desktop").expect("the installed entry parses");
		assert_eq!(entry.name, "Neovim");
		assert!(entry.mimes.iter().any(|mime| mime == "text/plain"), "its long MimeType line is whole");
		assert!(entry.terminal, "Terminal=true is read");
		assert_eq!(entry.try_exec.as_deref(), Some("nvim"));
		assert!(!entry.nodisplay, "the installed copy shows itself");

		let types = vec![ "text/plain".to_owned() ];
		let entries = [ entry ];
		let path_env = std::env::var("PATH").unwrap_or_default();
		let listed: Vec<&str> = handlers(&entries, &types, &[], &[], &path_env).iter().map(|e| e.id.as_str()).collect();
		assert_eq!(listed, vec![ "nvim.desktop" ], "nvim is offered for text/plain");
	}

	#[test]
	fn the_added_associations_and_the_terminal_wrap_are_read() {
		let text = "\
[Added Associations]
text/plain=nvim.desktop;editor.desktop;

[Default Applications]
text/plain=editor.desktop;
";
		assert_eq!(added_in(text, "text/plain"), vec![ "nvim.desktop", "editor.desktop" ]);
		assert_eq!(added_in(text, "text/html"), Vec::<String>::new(), "another type is not this one's");
		assert_eq!(preferred_in(text, "text/plain"), vec![ "editor.desktop" ], "and the sections stay apart");

		let program = vec![ "nvim".to_owned(), "/tmp/a.txt".to_owned() ];
		assert_eq!(
			launch_argv(program.clone(), Some(&[ "xdg-terminal-exec".to_owned() ])),
			vec![ "xdg-terminal-exec", "nvim", "/tmp/a.txt" ],
			"the terminal comes first, the program after it"
		);
		assert_eq!(launch_argv(program.clone(), None), program, "with no chain the program stands alone");

		let entry = |id: &str, name: &str, nodisplay: bool, try_exec: Option<&str>| Entry {
			id:     id.to_owned(),
			name:   name.to_owned(),
			exec:   "run %f".to_owned(),
			mimes:  vec![ "text/plain".to_owned() ],
			nodisplay,
			terminal:  false,
			try_exec:  try_exec.map(str::to_owned),
		};
		let entries = vec![
			entry("vlc.desktop", "VLC", false, None),
			entry("chromium.desktop", "Chromium", false, None),
			entry("hidden.desktop", "Hidden App", true, None),
			entry("ghost.desktop", "Ghost", false, Some("not-a-program-anywhere")),
			entry("nvim.desktop", "Neovim", false, None),
		];
		let all = all_apps(&entries, &std::env::var("PATH").unwrap_or_default());
		let ids: Vec<&str> = all.iter().map(|entry| entry.id.as_str()).collect();
		assert_eq!(
			ids,
			vec![ "chromium.desktop", "hidden.desktop", "nvim.desktop", "vlc.desktop" ],
			"by name: the NoDisplay one stands, and only an absent program is left out"
		);
	}

	#[test]
	fn the_candidates_keep_a_nodisplay_entry_and_drop_a_hidden_one() {
		let path = std::env::var("PATH").unwrap_or_default();
		let exact = |mime: &str| vec![ mime.to_owned() ];

		let satty = "\
[Desktop Entry]
Type=Application
Name=Satty
Exec=satty --filename %f
NoDisplay=true
Terminal=false
MimeType=image/png;image/jpeg;
TryExec=sh
";
		let entries = vec![ parse_entry(satty, "satty.desktop").expect("an application") ];
		let found = handlers(&entries, &exact("image/png"), &[], &[], &path);
		assert_eq!(found.len(), 1, "the NoDisplay entry is a candidate (P39)");
		assert_eq!(found[0].id, "satty.desktop");
		let all = all_apps(&entries, &path);
		assert_eq!(all.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(), vec![ "satty.desktop" ], "and it stands in the full zone too");

		assert!(
			parse_entry("[Desktop Entry]\nType=Application\nName=Hidden\nExec=x\nHidden=true\nMimeType=image/png;\n", "hidden.desktop").is_none(),
			"a hidden entry is no entry at all — hence absent from both zones"
		);

		let roster = [
			("imv.desktop", "imv"),
			("imv-dir.desktop", "imv (directory)"),
			("chromium.desktop", "Chromium"),
			("io.gitlab.theevilskeleton.Upscaler.desktop", "Upscaler"),
		];
		let entries: Vec<Entry> = roster
			.iter()
			.map(|(id, name)| Entry {
				id:     (*id).to_owned(),
				name:   (*name).to_owned(),
				exec:   "run %f".to_owned(),
				mimes:  vec![ "image/png".to_owned() ],
				nodisplay: false,
				terminal:  false,
				try_exec:  None,
			})
			.collect();
		let all = all_apps(&entries, &path);
		for (id, _) in roster {
			assert!(all.iter().any(|entry| entry.id == id), "{id} stands in the full zone");
		}

		if let Ok(text) = std::fs::read_to_string("/usr/share/applications/satty.desktop")
			&& let Some(entry) = parse_entry(&text, "satty.desktop")
			&& try_exec_ok(&entry, &path)
		{
			assert!(entry.nodisplay, "the machine's satty wears NoDisplay=true, as the evidence had it");
			assert_eq!(handlers(&[ entry ], &exact("image/png"), &[], &[], &path).len(), 1, "and it is a candidate");
		}
	}
}
