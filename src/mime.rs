//! The machine's own MIME database (P33⑥): shared-mime-info's compiled files under `mime/` —
//! the globs, the subclasses and the aliases — read the way GIO reads them, so that the window's
//! open-with candidates stand where a desktop manager's stand. A desktop's own managers answer
//! `text/markdown` with the `text/plain` editors, because markdown is a kind of plain text; the
//! same is true here.
//!
//! Pure where it can be: each file's text comes in as a string and the answers come back — the
//! tests hand in texts of their own. `Db::load` is the only reader of the machine's files, and it
//! only ever looks.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// One glob of the database, as `globs2` writes it: `weight:type:glob[:cs]`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Glob {
	pub(crate) weight:  u32,
	pub(crate) pattern: String,
	pub(crate) mime:    String,
	/// A case-sensitive pattern (`cs`): most globs fold case, a few do not.
	pub(crate) cs:      bool,
}

/// The machine's database: the globs in their own order (highest weight first, as the file is
/// written), the parents `subclasses` names, and the alias table.
#[derive(Debug, Default)]
pub(crate) struct Db {
	pub(crate) globs:   Vec<Glob>,
	pub(crate) parents: HashMap<String, Vec<String>>,
	pub(crate) aliases: HashMap<String, String>,
}

impl Db {
	/// Load from the data directories, merged in their own order the way GIO merges them: a machine
/// often keeps a small database of its own beside the system's (a custom `~/.local/share/mime`
/// with four globs and an empty `subclasses` was measured while this was written), and the
/// system's own files must still answer. `globs2` beats the legacy `globs` within a directory;
/// on a tie of weight and specificity, the earlier directory wins, as a person's own file should.
pub(crate) fn load(dirs: &[PathBuf]) -> Db {
	let read = |dir: &PathBuf, name: &str| std::fs::read_to_string(dir.join(name)).ok();

	let mut globs = Vec::new();
	for dir in dirs {
		if let Some(text) = read(dir, "globs2") {
			globs.extend(parse_globs2(&text));
		} else if let Some(text) = read(dir, "globs") {
			globs.extend(parse_globs(&text));
		}
	}

	let mut parents: HashMap<String, Vec<String>> = HashMap::new();
	let mut aliases: HashMap<String, String> = HashMap::new();
	for dir in dirs {
		if let Some(text) = read(dir, "subclasses") {
			for (child, list) in parse_subclasses(&text) {
				parents.entry(child).or_default().extend(list);
			}
		}
		if let Some(text) = read(dir, "aliases") {
			for (alias, canonical) in parse_aliases(&text) {
				aliases.entry(alias).or_insert(canonical);
			}
		}
	}

	Db { globs, parents, aliases }
}

	/// The type a name carries: the longest dot-suffix with a matching glob wins — `*.tar.gz`
	/// beats `*.gz` the way a desktop manager reads it — and with no suffix at all (a `README`,
	/// a `Makefile`) the whole name is matched. A directory is a directory (§ the same rule the
	/// window has always had: managers answer `inode/directory` without consulting a glob).
	pub(crate) fn guess(&self, name: &str, dir: bool) -> String {
		if dir {
			return "inode/directory".to_owned();
		}

		for (at, _) in name.char_indices().filter(|(_, c)| *c == '.') {
			if let Some(found) = self.best(name, Some(&name[at..])) {
				return self.canonical(found);
			}
		}

		match self.best(name, None) {
			Some(found) => self.canonical(found),
			None => "application/octet-stream".to_owned(),
		}
	}

	/// The best glob matching `name`, within one suffix when one is given: the heaviest wins,
	/// then the longer pattern (the more specific spelling), then a case-preserving match over a
	/// case-folded one, then the file's own order. This is the order the alignment against GIO
	/// was measured in — one glob per answer, and the tie-breaks are the ones it kept.
	fn best(&self, name: &str, suffix: Option<&str>) -> Option<&Glob> {
		let mut best: Option<(&Glob, bool)> = None;

		for glob in &self.globs {
			if let Some(suffix) = suffix
				&& !ends_with(&glob.pattern, suffix, glob.cs)
			{
				continue;
			}
			if !glob_match(&glob.pattern, name, glob.cs) {
				continue;
			}

			// whether the pattern also matched without folding: the more exact reading wins a tie
			let exact = glob.cs || glob_match(&glob.pattern, name, true);
			let better = match best {
				None => true,
				Some((previous, previous_exact)) => {
					(glob.weight, glob.pattern.len(), exact, !glob.cs)
						> (previous.weight, previous.pattern.len(), previous_exact, !previous.cs)
				}
			};
			if better {
				best = Some((glob, exact));
			}
		}

		best.map(|(glob, _)| glob)
	}

	/// A type and every type it is a kind of (`subclasses`, read transitively), the type itself
	/// first — what a desktop's own `g_app_info_get_all_for_type` is made of. Aliases resolve as
	/// the walk goes.
	pub(crate) fn closure(&self, mime: &str) -> Vec<String> {
		let mut seen = Vec::new();
		let mut stack = vec![ self.resolve(mime) ];

		while let Some(next) = stack.pop() {
			if seen.contains(&next) {
				continue;
			}
			for parent in self.parents.get(&next).into_iter().flatten() {
				stack.push(self.resolve(parent));
			}
			seen.push(next);
		}

		seen
	}

	/// An alias's own name, or the type itself.
	fn resolve(&self, mime: &str) -> String {
		self.aliases.get(mime).cloned().unwrap_or_else(|| mime.to_owned())
	}

	fn canonical(&self, glob: &Glob) -> String {
		self.resolve(&glob.mime)
	}
}

/// Whether a pattern ends with a suffix under the pattern's own case rule.
fn ends_with(pattern: &str, suffix: &str, cs: bool) -> bool {
	if cs {
		pattern.ends_with(suffix)
	} else {
		pattern.to_lowercase().ends_with(&suffix.to_lowercase())
	}
}

/// A glob against a name: `*`, `?` and `[...]` classes (with `!` negation and `a-z` ranges) —
/// what the glob files hold and what fnmatch reads. Case folds first when the glob is not
/// case-sensitive.
pub(crate) fn glob_match(pattern: &str, name: &str, cs: bool) -> bool {
	let (pattern, name) = if cs {
		(pattern.to_owned(), name.to_owned())
	} else {
		(pattern.to_lowercase(), name.to_lowercase())
	};

	matched(&pattern.chars().collect::<Vec<_>>(), &name.chars().collect::<Vec<_>>())
}

fn matched(pattern: &[char], name: &[char]) -> bool {
	let Some(&head) = pattern.first() else {
		return name.is_empty();
	};

	match head {
		'*' => (0..=name.len()).any(|skip| matched(&pattern[1..], &name[skip..])),
		'?' => !name.is_empty() && matched(&pattern[1..], &name[1..]),
		'[' => {
			let Some(end) = pattern.iter().position(|c| *c == ']').filter(|end| *end > 1) else {
				return name.first() == Some(&'[') && matched(&pattern[1..], &name[1..]);
			};
			match name.first() {
				Some(c) if in_class(&pattern[1..end], *c) => matched(&pattern[end + 1..], &name[1..]),
				_ => false,
			}
		}
		_ => name.first() == Some(&head) && matched(&pattern[1..], &name[1..]),
	}
}

/// A character class's own reading: `[abc]`, `[!abc]`, `[a-z]` and the two mixed.
fn in_class(class: &[char], c: char) -> bool {
	let (negated, class) = match class.first() {
		Some('!') => (true, &class[1..]),
		_ => (false, class),
	};

	let mut inside = false;
	let mut at = 0;
	while at < class.len() {
		if at + 2 < class.len() && class[at + 1] == '-' {
			if (class[at]..=class[at + 2]).contains(&c) {
				inside = true;
			}
			at += 3;
		} else {
			if class[at] == c {
				inside = true;
			}
			at += 1;
		}
	}

	inside != negated
}

/// The `globs2` file: `weight:type:flag:glob`, with the flag field written only when there is
/// one (`cs`) — and the older spelling an always-present flags field, `:cs:` or `::`, read too.
pub(crate) fn parse_globs2(text: &str) -> Vec<Glob> {
	let mut globs = Vec::new();

	for line in text.lines() {
		let line = line.trim();
		if line.is_empty() || line.starts_with('#') {
			continue;
		}

		let fields: Vec<&str> = line.split(':').collect();
		if fields.len() < 3 {
			continue;
		}
		let Ok(weight) = fields[0].parse::<u32>() else { continue };
		let mime = fields[1].to_owned();
		let tail = &fields[2..];

		// the older form keeps a flag field even when empty (`weight:type::glob` or
		// `weight:type:cs:glob`); this machine's form drops it and trails the glob
		// (`weight:type:glob`, `weight:type:glob:cs`)
		let (pattern, cs) = if tail[0] == "cs" || tail[0].is_empty() {
			(tail[1..].join(":"), tail[0] == "cs")
		} else if tail.len() >= 2 && tail[tail.len() - 1] == "cs" {
			(tail[..tail.len() - 1].join(":"), true)
		} else {
			(tail.join(":"), false)
		};

		if pattern.is_empty() || pattern == "__NOGLOBS__" {
			continue;
		}

		globs.push(Glob { weight, pattern, mime, cs });
	}

	globs
}

/// The legacy `globs` file: `type:glob[:cs]`, one weight for all.
pub(crate) fn parse_globs(text: &str) -> Vec<Glob> {
	let mut globs = Vec::new();

	for line in text.lines() {
		let line = line.trim();
		if line.is_empty() || line.starts_with('#') {
			continue;
		}

		let fields: Vec<&str> = line.split(':').collect();
		if fields.len() < 2 {
			continue;
		}
		let mime = fields[0].to_owned();
		let tail = &fields[1..];
		let (pattern, cs) = match tail.last() {
			Some(&"cs") if tail.len() >= 2 => (tail[..tail.len() - 1].join(":"), true),
			_ => (tail.join(":"), false),
		};
		if pattern.is_empty() || pattern == "__NOGLOBS__" {
			continue;
		}

		globs.push(Glob { weight: 50, pattern, mime, cs });
	}

	globs
}

/// The `subclasses` file: `child parent`, a child may name more than one parent.
pub(crate) fn parse_subclasses(text: &str) -> HashMap<String, Vec<String>> {
	let mut parents: HashMap<String, Vec<String>> = HashMap::new();

	for line in text.lines() {
		let mut fields = line.split_whitespace();
		if let (Some(child), Some(parent)) = (fields.next(), fields.next()) {
			parents.entry(child.to_owned()).or_default().push(parent.to_owned());
		}
	}

	parents
}

/// The `aliases` file: `alias canonical`.
pub(crate) fn parse_aliases(text: &str) -> HashMap<String, String> {
	let mut aliases = HashMap::new();

	for line in text.lines() {
		let mut fields = line.split_whitespace();
		if let (Some(alias), Some(canonical)) = (fields.next(), fields.next()) {
			aliases.insert(alias.to_owned(), canonical.to_owned());
		}
	}

	aliases
}

/// Where a desktop keeps its MIME data: the same data directories the entries come from.
pub(crate) fn data_dirs(home: Option<&Path>) -> Vec<PathBuf> {
	let mut dirs: Vec<PathBuf> = std::env::var_os("XDG_DATA_HOME")
		.map(PathBuf::from)
		.into_iter()
		.collect();
	if dirs.is_empty()
		&& let Some(home) = home
	{
		dirs.push(home.join(".local/share"));
	}
	dirs.push(PathBuf::from("/usr/local/share"));
	dirs.push(PathBuf::from("/usr/share"));

	dirs.into_iter().map(|dir| dir.join("mime")).collect()
}

/// The machine's own database, read once. Empty when the machine has none (the caller falls
/// back to the window's own small table).
pub(crate) fn machine() -> &'static Db {
	static DB: OnceLock<Db> = OnceLock::new();

	DB.get_or_init(|| {
		let home = std::env::var_os("HOME").map(PathBuf::from);
		Db::load(&data_dirs(home.as_deref()))
	})
}

/// The type a name carries, from the machine's database when it has one — and the window's own
/// small table for anything the database does not know.
pub(crate) fn guess(name: &str, dir: bool) -> String {
	let db = machine();
	if db.globs.is_empty() {
		return crate::desktop::mime_of(name, dir);
	}

	let guessed = db.guess(name, dir);
	if guessed == "application/octet-stream" {
		crate::desktop::mime_of(name, dir)
	} else {
		guessed
	}
}

/// A type and every type it is a kind of: what the open-with candidates are matched against.
pub(crate) fn closure(mime: &str) -> Vec<String> {
	machine().closure(mime)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The globs file as this machine writes it (the flag field only when it is there), the
	/// older spelling with an always-present field, and the entries that carry no glob at all.
	#[test]
	fn the_globs_file_is_read_both_ways() {
		let text = "\
# a comment
50:text/markdown:*.md
50:text/x-genie:*.gs:cs
80:text/html::*.html
0:application/x-modrinth-modpack+zip:__NOGLOBS__
";
		let globs = parse_globs2(text);
		assert_eq!(globs.len(), 3, "{globs:?}");
		assert_eq!(globs[0], Glob { weight: 50, pattern: "*.md".to_owned(), mime: "text/markdown".to_owned(), cs: false });
		assert_eq!(globs[1], Glob { weight: 50, pattern: "*.gs".to_owned(), mime: "text/x-genie".to_owned(), cs: true });
		assert_eq!(globs[2], Glob { weight: 80, pattern: "*.html".to_owned(), mime: "text/html".to_owned(), cs: false });

		// the legacy file: one weight, the same shape of answer
		let legacy = parse_globs("text/html:*.html\ntext/x-genie:*.gs:cs\n");
		assert_eq!(legacy.len(), 2);
		assert!(legacy[0].cs == false && legacy[1].cs);
	}

	/// The matcher: stars, questions, classes, negation — and case folding only when the glob
	/// asks for it.
	#[test]
	fn a_glob_matches_the_way_fnmatch_does() {
		assert!(glob_match("*.md", "notes.MD", false));
		assert!(!glob_match("*.gs", "x.GS", true), "a cs glob folds nothing");
		assert!(glob_match("*.gs", "x.gs", true));
		assert!(glob_match("README*", "README", false), "a star may match nothing");
		assert!(glob_match("*.t?t", "a.txt", false));
		assert!(glob_match("*.[1-9]", "man.5", false));
		assert!(!glob_match("*.[1-9]", "man.[1-9]", false), "a class is a class, not brackets");
		assert!(glob_match("*.[!1-9]", "man.x", false));
		assert!(glob_match("*.[!1-9]", "man.0", false));
		assert!(!glob_match("*.[!1-9]", "man.5", false));
		assert!(glob_match("sconstruct", "SConstruct", false));
		assert!(!glob_match("sconstruct", "SConstruct", true));
	}

	/// The guess reads the suffixes longest first — `*.tar.gz` beats `*.gz` — reads a literal
	/// name for a name with no dot, and falls back to the whole-name match for anything else.
	#[test]
	fn the_longest_suffix_answers_first() {
		let db = Db {
			globs:   vec![
				Glob { weight: 50, pattern: "*.gz".to_owned(), mime: "application/gzip".to_owned(), cs: false },
				Glob { weight: 50, pattern: "*.tar.gz".to_owned(), mime: "application/x-compressed-tar".to_owned(), cs: false },
				Glob { weight: 50, pattern: "*.md".to_owned(), mime: "text/markdown".to_owned(), cs: false },
				Glob { weight: 50, pattern: "readme*".to_owned(), mime: "text/x-readme".to_owned(), cs: false },
			],
			parents: parse_subclasses("text/markdown text/plain\n"),
			aliases: Default::default(),
		};

		assert_eq!(db.guess("a.tar.gz", false), "application/x-compressed-tar", "the longer suffix wins");
		assert_eq!(db.guess("a.gz", false), "application/gzip");
		assert_eq!(db.guess("notes.MD", false), "text/markdown", "case folds");
		assert_eq!(db.guess("README", false), "text/x-readme", "a name with no dot is matched whole");
		assert_eq!(db.guess("mystery.zzz", false), "application/octet-stream");
		assert_eq!(db.guess("Pictures", true), "inode/directory", "a directory is a directory");
	}

	/// The inheritance closure: a type and every type it is a kind of, the type itself first —
	/// what the open-with candidates are matched against.
	#[test]
	fn the_closure_walks_every_parent() {
		let db = Db {
			globs:   Vec::new(),
			parents: parse_subclasses("\
text/markdown text/plain
text/x-tex text/plain
text/x-latex text/x-tex
"),
			aliases: parse_aliases("application/x-old text/markdown\n"),
		};

		assert_eq!(db.closure("text/plain"), vec![ "text/plain" ]);
		assert_eq!(db.closure("text/x-latex"), vec![ "text/x-latex", "text/x-tex", "text/plain" ], "the walk goes all the way up");
		assert_eq!(db.closure("application/x-old"), vec![ "text/markdown", "text/plain" ], "an alias resolves, and its parents still walk");
	}

	/// The machine's own database usually stands there, and the names the report's table is
	/// about are the ones it must answer (P33⑥; the table itself is in the report).
	#[test]
	fn the_machines_own_database_answers_the_common_names() {
		let db = machine();
		if db.globs.is_empty() {
			return; // a machine without shared-mime-info keeps the window's own table
		}

		assert_eq!(db.guess("notes.txt", false), "text/plain");
		assert_eq!(db.guess("README.md", false), "text/markdown");
		assert_eq!(db.guess("a.tar.gz", false), "application/x-compressed-tar", "the manager's own answer, not the window's old table");
		assert_eq!(db.guess("Pictures", true), "inode/directory");
		assert_eq!(db.guess("photo.PNG", false), "image/png");

		// and the closure the candidates ride on: markdown is a kind of plain text
		assert!(db.closure("text/markdown").contains(&"text/plain".to_owned()));
	}
}