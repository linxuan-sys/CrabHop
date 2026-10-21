
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::Config;

const HALF_LIFE: f64 = 14.0 * 24.0 * 3600.0;
const CAP: usize = 500;

#[derive(Clone, Deserialize, Serialize)]
pub(crate) struct Entry {
	pub path:  PathBuf,
	pub score: f64,
	pub last:  u64,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(default)]
pub(crate) struct Frecency {
	pub entries: Vec<Entry>,
}

impl Frecency {
	pub(crate) fn load() -> Self {
		match Config::state("frecency.json") {
			Some(path) => Self::load_at(&path),
			None => Self::default(),
		}
	}

	pub(crate) fn load_at(at: &Path) -> Self {
		std::fs::read_to_string(at).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
	}

	pub(crate) fn save_at(&self, at: &Path) {
		if let Some(dir) = at.parent()
			&& Config::ensure(dir).is_err()
		{
			return;
		}

		if let Ok(text) = serde_json::to_string_pretty(self) {
			let _ = std::fs::write(at, text);
		}
	}

	pub(crate) fn visit(&mut self, path: &Path, now: u64) {
		match self.entries.iter_mut().find(|entry| entry.path == path) {
			Some(entry) => {
				entry.score = decay(entry.score, now.saturating_sub(entry.last)) + 1.0;
				entry.last = now;
			}
			None => self.entries.push(Entry { path: path.to_owned(), score: 1.0, last: now }),
		}

		if self.entries.len() > CAP {
			self.demote(now);
		}
	}

	pub(crate) fn remove(&mut self, path: &Path) -> Option<Entry> {
		let at = self.entries.iter().position(|entry| entry.path == path)?;

		Some(self.entries.remove(at))
	}

	pub(crate) fn put(&mut self, entry: Entry) {
		self.entries.retain(|other| other.path != entry.path);
		self.entries.push(entry);
	}

	pub(crate) fn ranked(&self, now: u64, cap: usize) -> Vec<(PathBuf, f64)> {
		let mut ranked: Vec<(PathBuf, f64)> = self
			.entries
			.iter()
			.map(|entry| (entry.path.clone(), decay(entry.score, now.saturating_sub(entry.last))))
			.collect();

		ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
		ranked.truncate(cap);
		ranked
	}

	pub(crate) fn recent(&self, cap: usize) -> Vec<(PathBuf, u64)> {
		let mut by_time: Vec<(PathBuf, u64)> = self.entries.iter().map(|entry| (entry.path.clone(), entry.last)).collect();

		by_time.sort_by_key(|(_, last)| std::cmp::Reverse(*last));
		by_time.truncate(cap);
		by_time
	}

	fn demote(&mut self, now: u64) {
		self.entries
			.sort_by(|a, b| decay(a.score, now.saturating_sub(a.last)).total_cmp(&decay(b.score, now.saturating_sub(b.last))));
		self.entries.drain(..self.entries.len() - CAP);
	}
}

pub(crate) fn decay(score: f64, age: u64) -> f64 {
	score * 0.5f64.powf(age as f64 / HALF_LIFE)
}

pub(crate) fn display(path: &Path) -> String {
	let home = std::env::var_os("HOME").map(PathBuf::from);
	if let Some(home) = home
		&& let Ok(rest) = path.strip_prefix(&home)
	{
		return if rest.as_os_str().is_empty() { "~".to_owned() } else { format!("~/{}", rest.display()) };
	}

	path.display().to_string()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_decay_halves_at_a_fortnight() {
		assert_eq!(decay(4.0, 0), 4.0);
		assert!((decay(4.0, HALF_LIFE as u64) - 2.0).abs() < 1e-9);
		assert!((decay(4.0, (HALF_LIFE * 2.0) as u64) - 1.0).abs() < 1e-9);
	}

	#[test]
	fn an_entry_goes_and_comes_back_whole() {
		let mut store = Frecency::default();
		store.visit(Path::new("/tmp/a"), 1000);
		store.visit(Path::new("/tmp/b"), 1000);

		let removed = store.remove(Path::new("/tmp/a")).expect("the entry was there");
		assert_eq!(removed.score, 1.0);
		assert_eq!(store.entries.len(), 1, "it is gone from the store");
		assert!(store.remove(Path::new("/tmp/a")).is_none(), "and it does not go twice");

		store.put(Entry { score: 9.5, last: 500, ..removed });
		let back = store.entries.iter().find(|entry| entry.path == Path::new("/tmp/a")).expect("it is back");
		assert_eq!((back.score, back.last), (9.5, 500), "the score and the clock are the ones put back");
		assert_eq!(store.entries.len(), 2, "no copy stands beside it");
	}

	#[test]
	fn visits_accumulate_and_the_ranking_decays_toward_today() {
		let mut frecency = Frecency::default();
		frecency.visit(Path::new("/tmp/old"), 0);
		frecency.visit(Path::new("/tmp/old"), 100);
		frecency.visit(Path::new("/tmp/new"), 100);

		let ranked = frecency.ranked(100, 10);
		assert_eq!(ranked[0].0, PathBuf::from("/tmp/old"), "two visits beat one");
		assert!((ranked[0].1 - 2.0).abs() < 1e-3, "the second visit kept nearly all of the first: {}", ranked[0].1);

		let ranked = frecency.ranked(100 + (HALF_LIFE * 20.0) as u64, 10);
		assert!(ranked[0].1 < 0.01 && ranked[1].1 < 0.01);
		assert_eq!(frecency.entries.len(), 2, "one directory, one record");
	}

	#[test]
	fn the_store_round_trips_and_paths_read_as_tildes() {
		let mut frecency = Frecency::default();
		frecency.visit(Path::new("/tmp/a"), 7);

		let text = serde_json::to_string(&frecency).expect("written");
		let back: Frecency = serde_json::from_str(&text).expect("read");
		assert_eq!(back.entries.len(), 1);
		assert_eq!(back.entries[0].path, PathBuf::from("/tmp/a"));

		let home = std::env::var_os("HOME").map(PathBuf::from).expect("tests run with a home");
		assert_eq!(display(&home.join("dev")), "~/dev");
		assert_eq!(display(Path::new("/etc")), "/etc");
	}
}
