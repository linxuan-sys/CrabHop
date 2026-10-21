//! The fuzzy match behind every panel (§5.3): nucleo-matcher, kept warm between keystrokes, and
//! the hits read back with the character positions the accent highlight needs.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// How many rows a panel keeps: the rest are never drawn, and 200 is already a wall of text.
pub(crate) const CAP: usize = 200;

#[derive(Clone, Debug)]
pub(crate) struct Hit {
	/// Where the hit stands in the list it was matched against.
	pub at:        usize,
	pub score:     f64,
	/// The character positions of the match, for the accent.
	pub positions: Vec<usize>,
}

/// A matcher for a panel to keep across keystrokes: nucleo's tables are worth keeping warm — the
/// spec's 「matcher 跨按键增量复用」.
pub(crate) fn matcher() -> Matcher {
	Matcher::new(Config::DEFAULT)
}

/// Match a query against a list: scored, best first, capped. An empty query keeps the list's own
/// order — a panel shows what there is before anything has been typed.
pub(crate) fn filter(query: &str, texts: &[&str], matcher: &mut Matcher) -> Vec<Hit> {
	if query.is_empty() {
		return (0..texts.len().min(CAP)).map(|at| Hit { at, score: 0.0, positions: Vec::new() }).collect();
	}

	let pattern = Pattern::parse(query, CaseMatching::Smart, Normalization::Smart);
	let mut hits = Vec::new();
	let mut scratch = Vec::new();
	let mut positions = Vec::new();

	for (at, text) in texts.iter().enumerate() {
		positions.clear();
		let haystack = Utf32Str::new(text, &mut scratch);
		if let Some(score) = pattern.indices(haystack, matcher, &mut positions) {
			hits.push(Hit { at, score: f64::from(score), positions: positions.iter().map(|at| *at as usize).collect() });
		}
	}

	hits.sort_by(|a, b| b.score.total_cmp(&a.score));
	hits.truncate(CAP);
	hits
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A subsequence matches, its positions come back for the highlight, and a query that is not
	/// there at all yields nothing.
	#[test]
	fn a_query_matches_as_a_subsequence_and_says_where() {
		let mut matcher = matcher();
		let texts = [ "linux-kernel", "links", "abc" ];

		let hits = filter("lnx", &texts, &mut matcher);
		assert_eq!(hits.len(), 1, "only l…n…x word matches: {hits:?}");
		let hit = &hits[0];
		assert_eq!(texts[hit.at], "linux-kernel");
		assert!(!hit.positions.is_empty(), "and the match has positions to highlight");

		// the positions are in the word's own text, in order
		let chars: Vec<char> = texts[hit.at].chars().collect();
		let matched: String = hit.positions.iter().filter_map(|at| chars.get(*at)).collect();
		assert_eq!(matched.to_lowercase(), "lnx");

		let hits = filter("lnks", &texts, &mut matcher);
		assert_eq!(hits.len(), 1, "and l…n…k…s is the other one");
		assert_eq!(texts[hits[0].at], "links");

		assert!(filter("zzz", &texts, &mut matcher).is_empty());
	}

	/// An empty query is the list itself, and a stronger match stands first.
	#[test]
	fn an_empty_query_lists_and_a_better_match_leads() {
		let mut matcher = matcher();
		let texts = [ "aaa", "links", "linux" ];

		let hits = filter("", &texts, &mut matcher);
		assert_eq!(hits.iter().map(|hit| hit.at).collect::<Vec<_>>(), vec![ 0, 1, 2 ], "the list's own order");

		let hits = filter("lin", &texts, &mut matcher);
		assert_eq!(hits.len(), 2);
		assert!(hits[0].score >= hits[1].score, "best first");
	}
}