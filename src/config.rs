//! The window's own little files: what it remembers between runs (the column shares), and the
//! paths it keeps them in. Written for this crate alone — nothing here is the legacy window's.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The columns' shares, which the drags rewrite and a double-click puts back.
pub(crate) const SHARES: [f32; 3] = [ 1.0, 4.0, 3.0 ];

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Config {
	pub columns: Columns,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Columns {
	/// The parent, current and preview columns' shares — whatever the last drag made them, and
	/// 1:4:3 (the spec's ratio) whenever the file has nothing to say.
	pub parent:  f32,
	pub current: f32,
	pub preview: f32,
}

impl Default for Config {
	fn default() -> Self {
		Self { columns: Columns { parent: SHARES[0], current: SHARES[1], preview: SHARES[2] } }
	}
}

impl Default for Columns {
	fn default() -> Self {
		Self { parent: SHARES[0], current: SHARES[1], preview: SHARES[2] }
	}
}

impl Columns {
	pub(crate) fn shares(&self) -> [f32; 3] {
		let shares = [ self.parent, self.current, self.preview ];
		// a file a person edited by hand is not allowed to break the window
		if shares.iter().any(|share| !share.is_finite() || *share <= 0.0) {
			return SHARES;
		}

		shares
	}
}

impl Config {
	/// The config file, under `$XDG_CONFIG_HOME/crabhop` (or `~/.config/crabhop`), the way the
	/// XDG spec says it belongs there.
	pub(crate) fn path() -> Option<PathBuf> {
		if let Some(dir) = CONFIG_DIR.get() {
			return Some(dir.join("config.toml"));
		}

		let base = std::env::var_os("XDG_CONFIG_HOME")
			.map(PathBuf::from)
			.or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;

		Some(base.join("crabhop").join("config.toml"))
	}

	/// The desktop's own preferences file, `mimeapps.list`, beside the window's config (P31's
	/// `总是` writes it): the spec's own place, and never the window's to own.
	pub(crate) fn mimeapps() -> Option<PathBuf> {
		let base = match CONFIG_DIR.get() {
			Some(dir) => dir.parent()?.to_owned(),
			None => std::env::var_os("XDG_CONFIG_HOME")
				.map(PathBuf::from)
				.or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?,
		};

		Some(base.join("mimeapps.list"))
	}

/// The state file a feature keeps between runs, under `$XDG_DATA_HOME/crabhop` (or
	/// `~/.local/share/crabhop`) — the place the XDG spec says state belongs. A test may claim a
	/// directory of its own instead, so a test that saves never touches what the person saved.
	pub(crate) fn state(name: &str) -> Option<PathBuf> {
		let base = match STATE_DIR.get() {
			Some(dir) => dir.clone(),
			None => std::env::var_os("XDG_DATA_HOME")
				.map(PathBuf::from)
				.or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share")))?,
		};

		Some(base.join("crabhop").join(name))
	}

	/// A cache directory a feature keeps rebuilt-at-will data under, `$XDG_CACHE_HOME/crabhop` (or
	/// `~/.cache/crabhop`) — the place the XDG spec says caches belong. Nothing here is a document:
	/// every entry may be lost without a word (P27⑥'s preview buckets).
	pub(crate) fn cache(name: &str) -> Option<PathBuf> {
		let base = std::env::var_os("XDG_CACHE_HOME")
			.map(PathBuf::from)
			.or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;

		Some(base.join("crabhop").join(name))
	}

	/// Make a state directory, 0700 the first time there is something to keep in it.
	pub(crate) fn ensure(dir: &std::path::Path) -> std::io::Result<()> {
		if dir.exists() {
			return Ok(());
		}

		let mut builder = std::fs::DirBuilder::new();
		builder.recursive(true);
		#[cfg(unix)]
		{
			use std::os::unix::fs::DirBuilderExt;
			builder.mode(0o700);
		}
		builder.create(dir)
	}

	/// The file if it is there and readable, and the defaults otherwise — a missing file is not a
	/// problem to report, it is the first run.
	pub(crate) fn load() -> Self {
		Self::path()
			.and_then(|path| std::fs::read_to_string(path).ok())
			.and_then(|text| toml::from_str(&text).ok())
			.unwrap_or_default()
	}

	/// Write the file back, creating its directory 0700 the first time — the state has to land
	/// where the next run will look for it.
	pub(crate) fn save(&self) -> std::io::Result<()> {
		let Some(path) = Self::path() else {
			return Ok(());
		};
		let Some(dir) = path.parent() else {
			return Ok(());
		};

		Self::ensure(dir)?;

		let text = toml::to_string_pretty(self).unwrap_or_default();
		std::fs::write(path, text)
	}
}

/// The desk's shared ledger at the system level (P38④): `/usr/share/applications/mimeapps.list`
/// — consulted only when the person's own file does not answer for the type. The window reads the
/// spec's own place; a test hands its app its own path instead (`App::set_mimeapps_system_path_for_tests`).
pub(crate) fn mimeapps_system() -> PathBuf {
	PathBuf::from("/usr/share/applications/mimeapps.list")
}

/// A ledger's text, read live (P38④): cached against the file's mtime, so a hand edit or another
/// manager's write is seen on the very next read — no restart — and the disk is only touched when
/// the mtime has moved. `None` for a file that is not there (and absent files are remembered too,
/// so a ledger that appears is noticed the same way).
pub(crate) fn mimeapps_read(at: &std::path::Path) -> Option<String> {
	use std::sync::Mutex;

	type Cached = (Option<std::time::SystemTime>, Option<String>);
	static CACHE: std::sync::OnceLock<Mutex<std::collections::HashMap<PathBuf, Cached>>> = std::sync::OnceLock::new();

	let mtime = std::fs::metadata(at).ok().and_then(|meta| meta.modified().ok());
	let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
	let mut cache = cache.lock().ok()?;
	if let Some((cached, text)) = cache.get(at)
		&& *cached == mtime
	{
		return text.clone();
	}

	let text = std::fs::read_to_string(at).ok();
	cache.insert(at.to_owned(), (mtime, text.clone()));

	text
}

static STATE_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// The tests' own state dir: once claimed, every state file lives under it for the rest of the
/// process — thread-safe, unlike the environment it replaces.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn state_dir_for_tests(dir: PathBuf) {
	let _ = STATE_DIR.set(dir);
}

static CONFIG_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// The tests' own config directory: once claimed, the window's little file lives under it for the
/// rest of the process. A session then starts on the default shares — whatever a person dragged is
/// their window's own business, not a test's fixture — and a drag inside a test writes there rather
/// than into the person's own file.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn config_dir_for_tests(dir: PathBuf) {
	let _ = CONFIG_DIR.set(dir);
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The shares are the spec's ratio by default, and a file someone broke by hand falls back to
	/// the same ratio rather than dividing the window by zero.
	#[test]
	fn shares_default_and_never_break() {
		assert_eq!(Config::default().columns.shares(), SHARES);

		let broken = Columns { parent: 0.0, current: f32::NAN, preview: -3.0 };
		assert_eq!(broken.shares(), SHARES, "a hand-edited file cannot break the window");
	}

	/// What lands in the file is what a next run reads back: the round trip the persistence
	/// stands on.
	#[test]
	fn the_file_round_trips() {
		let config = Config { columns: Columns { parent: 1.5, current: 3.25, preview: 3.25 } };
		let text = toml::to_string_pretty(&config).expect("writable");
		let back: Config = toml::from_str(&text).expect("readable");

		assert_eq!(back.columns.shares(), [ 1.5, 3.25, 3.25 ]);
	}

	/// A file that says nothing about a field keeps the default for it.
	#[test]
	fn a_partial_file_keeps_the_defaults_it_does_not_say() {
		let config: Config = toml::from_str("[columns]\nparent = 2.0\n").expect("readable");
		assert_eq!(config.columns.shares(), [ 2.0, 4.0, 3.0 ]);
	}
}