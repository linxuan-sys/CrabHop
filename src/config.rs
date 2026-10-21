
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub(crate) const SHARES: [f32; 3] = [ 1.0, 4.0, 3.0 ];

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Config {
	pub columns: Columns,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Columns {
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
		if shares.iter().any(|share| !share.is_finite() || *share <= 0.0) {
			return SHARES;
		}

		shares
	}
}

impl Config {
	pub(crate) fn dir() -> Option<PathBuf> {
		if let Some(dir) = CONFIG_DIR.get() {
			return Some(dir.clone());
		}

		let base = std::env::var_os("XDG_CONFIG_HOME")
			.map(PathBuf::from)
			.or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;

		Some(base.join("crabhop"))
	}

	pub(crate) fn path() -> Option<PathBuf> {
		Some(Self::dir()?.join("config.toml"))
	}

	pub(crate) fn mimeapps() -> Option<PathBuf> {
		let base = match CONFIG_DIR.get() {
			Some(dir) => dir.parent()?.to_owned(),
			None => std::env::var_os("XDG_CONFIG_HOME")
				.map(PathBuf::from)
				.or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?,
		};

		Some(base.join("mimeapps.list"))
	}

	pub(crate) fn state(name: &str) -> Option<PathBuf> {
		let base = match STATE_DIR.get() {
			Some(dir) => dir.clone(),
			None => std::env::var_os("XDG_DATA_HOME")
				.map(PathBuf::from)
				.or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share")))?,
		};

		Some(base.join("crabhop").join(name))
	}

	pub(crate) fn cache(name: &str) -> Option<PathBuf> {
		let base = std::env::var_os("XDG_CACHE_HOME")
			.map(PathBuf::from)
			.or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))?;

		Some(base.join("crabhop").join(name))
	}

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

	pub(crate) fn load() -> Self {
		Self::path()
			.and_then(|path| std::fs::read_to_string(path).ok())
			.and_then(|text| toml::from_str(&text).ok())
			.unwrap_or_default()
	}

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

pub(crate) fn mimeapps_system() -> PathBuf {
	PathBuf::from("/usr/share/applications/mimeapps.list")
}

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

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn state_dir_for_tests(dir: PathBuf) {
	let _ = STATE_DIR.set(dir);
}

static CONFIG_DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn config_dir_for_tests(dir: PathBuf) {
	let _ = CONFIG_DIR.set(dir);
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn shares_default_and_never_break() {
		assert_eq!(Config::default().columns.shares(), SHARES);

		let broken = Columns { parent: 0.0, current: f32::NAN, preview: -3.0 };
		assert_eq!(broken.shares(), SHARES, "a hand-edited file cannot break the window");
	}

	#[test]
	fn the_file_round_trips() {
		let config = Config { columns: Columns { parent: 1.5, current: 3.25, preview: 3.25 } };
		let text = toml::to_string_pretty(&config).expect("writable");
		let back: Config = toml::from_str(&text).expect("readable");

		assert_eq!(back.columns.shares(), [ 1.5, 3.25, 3.25 ]);
	}

	#[test]
	fn a_partial_file_keeps_the_defaults_it_does_not_say() {
		let config: Config = toml::from_str("[columns]\nparent = 2.0\n").expect("readable");
		assert_eq!(config.columns.shares(), [ 2.0, 4.0, 3.0 ]);
	}
}
