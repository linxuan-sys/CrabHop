//! Markdown, laid out by hand (§3.8's own route): pulldown-cmark turns the source into events,
//! and the events become the window's own display rows — headings at the spec's size ladder, code
//! blocks in `bg-raised` with syntax highlighting borrowed from §3.3, lists with an accent dot,
//! links in the busy colour. The tokens are the window's, so "Mocha UI, Mocha code" holds.

use std::collections::HashMap;

use eframe::egui::Color32;
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::preview::code::Syntax;
use crate::preview::rows::{Doc, Row, Run, advance};
use crate::tokens::Tokens;

/// The body size: the ladder above it is §3.2's 22/19/16.
const BODY: f32 = 16.0;

/// Lays the source out for a content width, in pixels.
pub(crate) fn layout(source: &str, width: f32, tokens: &Tokens, images: &HashMap<String, [u32; 2]>) -> Doc {
	let mut build = Build {
		rows:    Vec::new(),
		tokens,
		width:   width.max(64.0),
		faces:   Vec::new(),
		quote:   0,
		lists:   Vec::new(),
		pending: 0.0,
		code:    None,
		table:   None,
		images,
		image:   None,
	};
	build.run(source);
	Doc::new(build.rows)
}

/// The local pictures a document asks for, in the order it asks (P26's second item): the `src` of
/// every `![alt](src)`, with the remote and the embedded ones left out — a preview does not fetch
/// anything. Pure, so the rule can be read and tested.
pub(crate) fn images(source: &str) -> Vec<String> {
	let mut wanted = Vec::new();
	let mut rest = source;

	while let Some(at) = rest.find("![") {
		rest = &rest[at + 2..];
		let Some(close) = rest.find(']') else { break };
		let after = &rest[close + 1..];
		let Some(open) = after.strip_prefix('(') else { continue };
		let Some(end) = open.find(')') else { break };
		let src = open[..end].trim().trim_matches(['<', '>']).to_owned();

		if !src.is_empty() && !is_remote(&src) && !wanted.contains(&src) {
			wanted.push(src);
		}
		rest = &open[end + 1..];
	}

	wanted
}

/// Whether a picture lives somewhere a preview will not go: the network, or inside the document.
pub(crate) fn is_remote(src: &str) -> bool {
	let src = src.trim_start();

	src.starts_with("http://") || src.starts_with("https://") || src.starts_with("data:") || src.starts_with("//")
}

/// One inline stretch with the face it is read in.
#[derive(Clone, Copy, PartialEq)]
enum Face {
	Plain,
	Strong,
	Em,
	Code,
	Link,
	Strike,
	/// A list's unordered dot, and an ordered list's number.
	Bullet,
	Marker,
}

/// The layout in progress: the rows made so far, and the state the events run through.
struct Build<'a> {
	rows:    Vec<Row>,
	/// The pictures the document's own file answered for (P26's second item): `src` → its pixels.
	images:  &'a HashMap<String, [u32; 2]>,
	/// The picture the reader is inside right now, with the alt text gathered so far.
	image:   Option<(String, String)>,
	tokens:  &'a Tokens,
	width:   f32,
	faces:   Vec<Face>,
	quote:   usize,
	lists:   Vec<Option<u64>>,
	/// The space the next block wants above it, in pixels.
	pending: f32,
	/// Inside a code block: its language, and the text so far.
	code:    Option<(String, String)>,
	/// Inside a table: the rows of cells so far.
	table:   Option<Vec<Vec<String>>>,
}

/// A stretch of text with its face: the wrapper's unit, before it becomes a Row's runs.
#[derive(Clone)]
struct Span {
	text: String,
	face: Face,
}

impl Build<'_> {
	fn run(&mut self, source: &str) {
		let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
		let mut words: Vec<Span> = Vec::new();

		for event in Parser::new_ext(source, options) {
			match event {
				Event::Start(tag) => self.start(tag, &mut words),
				Event::End(tag) => self.end(tag, &mut words),
				Event::Text(text) => match &mut self.code {
					Some((_, buf)) => buf.push_str(&text),
					// inside an image the text is its alt: it is kept beside the src rather than
					// becoming a paragraph of its own (P26's second item)
					None if self.image.is_some() => {
						let (_, alt) = self.image.as_mut().expect("checked just now");
						alt.push_str(&text);
					}
					None => words.push(Span { text: text.into_string(), face: self.face() }),
				},
				Event::Code(text) => words.push(Span { text: text.into_string(), face: Face::Code }),
				Event::SoftBreak => words.push(Span { text: " ".to_owned(), face: self.face() }),
				Event::HardBreak => words.push(Span { text: "\n".to_owned(), face: self.face() }),
				Event::Rule => {
					self.flush(&mut words);
					let before = self.take_pending(16.0);
					self.rows.push(Row {
						height: 1.0,
						before,
						rule: Some(self.tokens.border),
						..Default::default()
					});
					self.pending = 16.0;
				}
				Event::TaskListMarker(done) => {
					words.push(Span { text: if done { "[x] " } else { "[ ] " }.to_owned(), face: Face::Plain });
				}
				// html in a preview is not interpreted: it neither shows nor breaks the layout
				Event::Html(_) | Event::InlineHtml(_) => {}
				_ => {}
			}
		}
	}

	/// An image's own row (P26's second item): a band as tall as the picture stands when contained
	/// at half the column, with the picture's key in it for the pane to paint. One whose file the
	/// preview could not read — or one kept on the network — is a line of words instead, and says
	/// which of the two it is.
	fn image_row(&mut self, words: &mut Vec<Span>) {
		let Some((src, alt)) = self.image.take() else { return };
		words.clear();

		let alt = alt.trim().to_owned();
		let note = |what: &str, colour: Color32, image: Option<String>, height: f32| Row {
			text: if alt.is_empty() { format!("{what}{src}") } else { format!("{what}{src}（{alt}）") },
			size: 13.0,
			colour,
			height,
			before: 8.0,
			image,
			..Default::default()
		};

		// half the column, the picture's own shape, and a ceiling so one tall picture cannot take
		// the whole pane
		const HALF: f32 = 0.5;
		const CEILING: f32 = 480.0;

		let row = match self.images.get(&src) {
			Some([ width, height ]) if *width > 0 && *height > 0 => {
				let drawn = self.width * HALF;
				let tall = (drawn * *height as f32 / *width as f32).min(CEILING);
				note("", self.tokens.disabled, Some(src.clone()), tall + 12.0)
			}
			_ if is_remote(&src) => note("（远程图片不加载）", self.tokens.disabled, None, 24.0),
			_ => note("（图片读不到）", self.tokens.err, None, 24.0),
		};

		self.rows.push(row);
	}

	/// The face the next plain text is read in: the innermost styling that is open.
	fn face(&self) -> Face { self.faces.last().copied().unwrap_or(Face::Plain) }

	fn start(&mut self, tag: Tag, words: &mut Vec<Span>) {
		match tag {
			Tag::Heading { .. } => self.flush(words),
			Tag::Strong => self.faces.push(Face::Strong),
			Tag::Emphasis => self.faces.push(Face::Em),
			Tag::Strikethrough => self.faces.push(Face::Strike),
			Tag::Link { .. } => self.faces.push(Face::Link),
			Tag::Image { dest_url, .. } => {
				self.faces.push(Face::Link);
				self.image = Some((dest_url.to_string(), String::new()));
			}
			Tag::BlockQuote(_) => {
				self.flush(words);
				self.quote += 1;
			}
			Tag::CodeBlock(kind) => {
				self.flush(words);
				let lang = match kind {
					CodeBlockKind::Fenced(lang) => lang.into_string(),
					CodeBlockKind::Indented => String::new(),
				};
				self.code = Some((lang, String::new()));
			}
			Tag::List(first) => {
				self.flush(words);
				self.lists.push(first);
			}
			Tag::Item => {
				self.flush(words);
				let marker = match self.lists.last_mut() {
					Some(Some(next)) => {
						let marker = format!("{next}. ");
						*next += 1;
						marker
					}
					_ => "• ".to_owned(),
				};
				let face = if self.lists.last().is_some_and(Option::is_some) { Face::Marker } else { Face::Bullet };
				words.push(Span { text: marker, face });
				self.pending = 4.0;
			}
			Tag::Table(..) => {
				self.flush(words);
				self.table = Some(Vec::new());
			}
			Tag::TableHead | Tag::TableRow => {
				if let Some(rows) = &mut self.table {
					rows.push(Vec::new());
				}
			}
			Tag::TableCell => words.clear(),
			_ => {}
		}
	}

	fn end(&mut self, tag: TagEnd, words: &mut Vec<Span>) {
		match tag {
			TagEnd::Paragraph => self.flush(words),
			TagEnd::Heading(level) => self.heading(level, words),
			TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough | TagEnd::Link => {
				self.faces.pop();
			}
			// a picture ends its own line: the alt it gathered becomes the band's caption, and the
			// band is the picture's to stand in (P26's second item)
			TagEnd::Image => {
				self.faces.pop();
				self.image_row(words);
			}
			TagEnd::BlockQuote(_) => {
				self.flush(words);
				self.quote = self.quote.saturating_sub(1);
			}
			TagEnd::CodeBlock => self.code_block(),
			TagEnd::List(_) => {
				self.flush(words);
				self.lists.pop();
				self.pending = 8.0;
			}
			TagEnd::Item => self.flush(words),
			TagEnd::Table => self.table(),
			TagEnd::TableHead | TagEnd::TableRow => {}
			TagEnd::TableCell => {
				if let Some(rows) = &mut self.table
					&& let Some(row) = rows.last_mut()
				{
					let text = std::mem::take(words).iter().map(|span| span.text.as_str()).collect::<String>();
					row.push(text.trim().to_owned());
				}
			}
			_ => {}
		}
	}

	/// A heading: the ladder's size and margins, H1's own rule, and never wrapped past the column.
	fn heading(&mut self, level: HeadingLevel, words: &mut Vec<Span>) {
		let spans = std::mem::take(words);
		let (size, before, after, rule) = match level {
			HeadingLevel::H1 => (22.0, 20.0, 12.0, Some(self.tokens.border)),
			HeadingLevel::H2 => (19.0, 16.0, 8.0, None),
			_ => (16.0, 12.0, 6.0, None),
		};

		let wrapped = self.wrap(&spans, size, 0.0);
		for (i, line) in wrapped.iter().enumerate() {
			let mut row = self.row_of(line, size, self.tokens.text, true, 0.0, None);
			row.height = size + 10.0;
			if i == 0 {
				row.before = self.take_pending(before);
				row.rule = rule;
			}
			self.rows.push(row);
		}
		self.pending = after;
	}

	/// A finished code block: one row a line, in the raised band, with the fence's own syntax.
	fn code_block(&mut self) {
		let Some((lang, text)) = self.code.take() else { return };
		let mut lines: Vec<&str> = text.split('\n').collect();
		while lines.last().is_some_and(|line| line.is_empty()) {
			lines.pop();
		}
		let last = lines.len().saturating_sub(1);
		let mut syntax = Syntax::for_fence(&lang);

		for (i, line) in lines.iter().enumerate() {
			let runs = syntax
				.next(line)
				.into_iter()
				.map(|(range, colour)| Run { range, colour, size: 14.0, bold: false, bg: None })
				.collect();

			self.rows.push(Row {
				text:     (*line).to_owned(),
				height:   20.0,
				before:   if i == 0 { 12.0 } else { 0.0 },
				indent:   12.0,
				size:     14.0,
				colour:   self.tokens.text,
				runs,
				bg:       Some(self.tokens.raised),
				bg_round: i == 0 || i == last,
				..Default::default()
			});
		}
		self.pending = 12.0;
	}

	/// A paragraph's (or item's, or heading's) words become its rows, wrapped to the column.
	fn flush(&mut self, words: &mut Vec<Span>) {
		if words.is_empty() {
			return;
		}

		let spans = std::mem::take(words);
		let quote = self.quote > 0;
		let indent = 24.0 * self.lists.len() as f32 + if quote { 16.0 } else { 0.0 };
		let colour = if quote { self.tokens.subtext } else { self.tokens.text };
		let bar = quote.then_some(self.tokens.border_strong);

		let wrapped = self.wrap(&spans, BODY, indent);
		for (i, line) in wrapped.iter().enumerate() {
			let mut row = self.row_of(line, BODY, colour, false, indent, bar);
			if i == 0 {
				row.before = self.take_pending(if quote { 8.0 } else { 12.0 });
			}
			self.rows.push(row);
		}
		self.pending = if quote { 8.0 } else { 12.0 };
	}

	/// A finished table becomes rows with cells, a header band and the spec's full grid.
	fn table(&mut self) {
		let Some(cells) = self.table.take() else { return };
		// the parser announces the head with a row of its own: the rows that carry no cell are
		// not rows a table should draw
		let cells: Vec<Vec<String>> = cells.into_iter().filter(|row| !row.is_empty()).collect();
		if cells.is_empty() {
			return;
		}

		let cols = cells.iter().map(Vec::len).max().unwrap_or(0);
		if cols == 0 {
			return;
		}

		// each column's width follows its widest cell, between the spec's 60 and 320 pixels
		let mut widths = vec![60.0f32; cols];
		for row in &cells {
			for (i, cell) in row.iter().enumerate() {
				let px =
					cell.chars().map(crate::list::cell_width).sum::<u32>() as f32 * advance(14.0) + 16.0;
				widths[i] = widths[i].max(px.min(320.0));
			}
		}
		// and the pane's own width caps the whole of it
		let total: f32 = widths.iter().sum();
		if total > self.width {
			let scale = self.width / total;
			for width in &mut widths {
				*width *= scale;
			}
		}

		let first = self.take_pending(12.0);
		for (i, row) in cells.iter().enumerate() {
			let head = i == 0;
			self.rows.push(Row {
				cells:    row.clone(),
				col_w:    widths.first().copied().unwrap_or(60.0),
				height:   28.0,
				before:   if i == 0 { first } else { 0.0 },
				size:     if head { 13.0 } else { 14.0 },
				bold:     head,
				colour:   if head { self.tokens.subtext } else { self.tokens.text },
				bg:       head.then_some(self.tokens.raised),
				rule:     Some(self.tokens.border),
				rule_top: head,
				..Default::default()
			});
		}
		self.pending = 12.0;
	}

	/// A row from one wrapped line of spans.
	fn row_of(
		&self,
		line: &[Span],
		size: f32,
		colour: Color32,
		bold: bool,
		indent: f32,
		bar: Option<Color32>,
	) -> Row {
		let mut row = Row { size, height: 24.0, colour, bold, indent, bar, ..Default::default() };

		for span in line {
			let start = row.text.len();
			row.text.push_str(&span.text);
			let end = row.text.len();

			let (run_colour, run_bold, run_bg, run_size) = match span.face {
				Face::Plain => (colour, bold, None, size),
				Face::Strong => (self.tokens.text, true, None, size),
				Face::Em => (colour, bold, None, size),
				Face::Code => (self.tokens.text, false, Some(self.tokens.raised), 14.0),
				Face::Link => (self.tokens.busy, bold, None, size),
				Face::Strike => (self.tokens.disabled, bold, None, size),
				Face::Bullet => (self.tokens.accent.gamma_multiply(0.7), bold, None, size),
				Face::Marker => (self.tokens.subtext, bold, None, size),
			};

			row.runs.push(Run { range: start..end, colour: run_colour, size: run_size, bold: run_bold, bg: run_bg });
		}

		row
	}

	/// Wrap a block's spans into lines: a CJK glyph may break after any of them, Latin only at the
	/// spaces between its words, a word longer than a line is cut where it stands, and a hard break
	/// ends the line where it stands. The same rule the legacy window's wrap used, over spans.
	fn wrap(&self, spans: &[Span], size: f32, indent: f32) -> Vec<Vec<Span>> {
		let width = (self.width - indent).max(64.0);
		let chars: Vec<(char, Face)> = spans.iter().flat_map(|span| span.text.chars().map(|c| (c, span.face))).collect();

		let px = |c: char, face: Face| {
			let size = if face == Face::Code { 14.0 } else { size };
			crate::list::cell_width(c) as f32 * advance(size)
		};

		let mut out: Vec<Vec<Span>> = Vec::new();
		let mut line: Vec<(char, Face)> = Vec::new();
		let (mut used, mut space) = (0.0f32, None::<usize>);

		for (c, face) in chars {
			if c == '\n' {
				if !line.is_empty() {
					out.push(line_of(&std::mem::take(&mut line)));
				}
				used = 0.0;
				space = None;
				continue;
			}

			let width_of = px(c, face);
			if used + width_of > width && !line.is_empty() {
				let cut = space.unwrap_or(line.len());
				let tail = line.split_off(cut);
				out.push(line_of(&line));
				line = tail;
				used = line.iter().map(|(c, face)| px(*c, *face)).sum();
				space = None;
			}

			line.push((c, face));
			used += width_of;
			if c == ' ' {
				space = Some(line.len());
			}
		}

		if !line.is_empty() {
			out.push(line_of(&line));
		}

		out
	}

	/// The space above a block, consumed once: the previous block's margin and this one's meet
	/// the way adjacent CSS margins do — the larger wins, and nothing is counted twice (§3.2's
	/// 20/12, 16/8, 12/6 are read that way).
	fn take_pending(&mut self, fallback: f32) -> f32 {
		let space = std::mem::take(&mut self.pending);
		space.max(fallback)
	}
}

/// One wrapped line of chars back into runs: consecutive chars of one face merge.
fn line_of(line: &[(char, Face)]) -> Vec<Span> {
	let mut out: Vec<Span> = Vec::new();

	for (c, face) in line {
		match out.last_mut() {
			Some(last) if last.face == *face => last.text.push(*c),
			_ => out.push(Span { text: c.to_string(), face: *face }),
		}
	}

	out
}

#[cfg(test)]
mod tests {
	use super::*;

	fn tokens() -> Tokens { Tokens::MOCHA }

	/// The heading ladder is the spec's: H1 is the biggest, it carries the only rule, and the
	/// margins of the ladder are the 20/12, 16/8, 12/6 the table lays down.
	#[test]
	fn headings_follow_the_specs_ladder() {
		let doc = layout("# 一\n\n## 二\n\n### 三\n\ntext", 400.0, &tokens(), &HashMap::new());

		let sizes: Vec<f32> = doc.rows.iter().filter(|row| !row.text.is_empty()).map(|row| row.size).collect();
		assert_eq!(sizes, vec![ 22.0, 19.0, 16.0, 16.0 ], "the ladder, then the body");
		assert!(doc.rows[0].rule.is_some(), "H1 is the only one with a line");
		assert_eq!(doc.rows[0].before, 20.0);
		assert_eq!(doc.rows[1].before, 16.0);
		assert_eq!(doc.rows[2].before, 12.0);
	}

	/// A list keeps its bullet in the accent colour, indents 24px a level, and an ordered one
	/// numbers its items.
	#[test]
	fn lists_bullet_and_number() {
		let doc = layout("- one\n- two\n\n1. first\n2. second", 400.0, &tokens(), &HashMap::new());

		let bullets: Vec<&Row> = doc.rows.iter().filter(|row| row.text.starts_with("• ")).collect();
		assert_eq!(bullets.len(), 2, "two items, two bullets");
		assert_ne!(bullets[0].runs[0].colour, tokens().text, "the dot is not body text");
		assert!(bullets[0].text.ends_with("one"), "the marker leads its own item: {:?}", bullets[0].text);

		let numbered: Vec<&Row> = doc.rows.iter().filter(|row| row.text.starts_with("1. ") || row.text.starts_with("2. ")).collect();
		assert_eq!(numbered.len(), 2);
		assert_eq!(numbered[0].text, "1. first");
		assert_eq!(numbered[1].text, "2. second");
	}

	/// A fenced block wears the raised band and the code size, and its words get syntax colours.
	#[test]
	fn a_fenced_block_is_a_band_of_code() {
		let doc = layout("```rust\nfn main() {}\n```", 400.0, &tokens(), &HashMap::new());
		let row = doc.rows.iter().find(|row| row.text.contains("fn main")).expect("the code line");

		assert_eq!(row.size, 14.0);
		assert_eq!(row.bg, Some(tokens().raised));
		assert!(row.bg_round, "the block's first row rounds its corners");
		assert!(!row.runs.is_empty(), "the syntax has something to say");
		assert_eq!(row.runs[0].colour, Color32::from_rgb(0xcb, 0xa6, 0xf7), "the keyword is mauve");
	}

	/// Links are busy, code in a paragraph is a chip, and a table is cells with a header band — the
	/// spec's own readings of each.
	#[test]
	fn links_code_chips_and_tables() {
		let doc = layout("see [docs](https://x) and `code`\n\n| a | b |\n|---|---|\n| 1 | 2 |", 400.0, &tokens(), &HashMap::new());

		let paragraph = doc.rows.iter().find(|row| row.text.contains("docs")).expect("the paragraph");
		let link = paragraph.runs.iter().find(|run| run.colour == tokens().busy).expect("the link run");
		assert_eq!(&paragraph.text[link.range.clone()], "docs");
		let chip = paragraph.runs.iter().find(|run| run.bg.is_some()).expect("the chip");
		assert_eq!(&paragraph.text[chip.range.clone()], "code");

		let table: Vec<&Row> = doc.rows.iter().filter(|row| !row.cells.is_empty()).collect();
		assert_eq!(table.len(), 2, "a header and one body row");
		assert_eq!(table[0].cells, vec![ "a", "b" ]);
		assert_eq!(table[1].cells, vec![ "1", "2" ]);
		assert_eq!(table[0].bg, Some(tokens().raised), "the header wears the band");
		assert!(table[0].rule.is_some() && table[0].rule_top, "a full grid, borders on every row");
	}


	#[test]
	fn paragraphs_wrap_within_the_column() {
		let doc = layout("one two three four five six seven eight nine ten", 120.0, &tokens(), &HashMap::new());
		for row in &doc.rows {
			let px: f32 =
				row.text.chars().map(|c| crate::list::cell_width(c) as f32).sum::<f32>() * advance(row.size);
			assert!(px <= 120.0 + 1.0, "{:?} is wider than the column", row.text);
		}
		let all: String = doc.rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>().join(" ");
		for word in [ "one", "eight", "ten" ] {
			assert!(all.contains(word), "{word} survived the wrap: {all:?}");
		}

		let doc = layout("你好 duckfm！这是第二段：中文与 English 混排 A", 100.0, &tokens(), &HashMap::new());
		let joined: String = doc.rows.iter().map(|row| row.text.as_str()).collect::<Vec<_>>().join("");
		assert!(joined.contains("这是第二段"), "no character is lost to the wrap: {joined:?}");
	}

	/// A quote greys its text and hangs the spec's bar; a rule is a line with air around it.
	#[test]
	fn quotes_and_rules() {
		let doc = layout("> quoted words\n\n---\n\nplain", 400.0, &tokens(), &HashMap::new());

		let quote = doc.rows.iter().find(|row| row.text.contains("quoted")).expect("the quote");
		assert_eq!(quote.colour, tokens().subtext);
		assert_eq!(quote.bar, Some(tokens().border_strong));

		let rule = doc.rows.iter().find(|row| row.height == 1.0).expect("the rule");
		assert_eq!(rule.rule, Some(tokens().border));

		// and a row's height is what the layout asked: a rule is one line, not a text line
		assert!(doc.total > 0.0);
	}
	/// The pictures a document asks for (P26's second item): extracted in order, deduplicated, and
	/// with the remote and the embedded ones left out — a preview fetches nothing.
	#[test]
	fn the_pictures_a_document_asks_for_are_the_local_ones() {
		let source = "# 标题\n\n![一个图](pics/a.png)\n\n![remote](https://x.example/b.png)\n\n![数据](data:image/png;base64,AAAA)\n\n![again](pics/a.png)\n";
		assert_eq!(images(source), vec![ "pics/a.png".to_owned() ], "the local one, once");

		assert!(is_remote("https://x/a.png"));
		assert!(is_remote("http://x/a.png"));
		assert!(is_remote("//x/a.png"));
		assert!(is_remote("data:image/png;base64,AA"));
		assert!(!is_remote("pics/a.png"));
		assert!(!is_remote("/tmp/a.png"));
	}

	/// An image's own row (P26's second item): when the picture is known its band stands as tall as
	/// the picture drawn at half the column, and the row carries what the pane looks up; a remote
	/// one and one the preview could not read are lines of words instead, and say which they are.
	#[test]
	fn an_image_gets_a_band_and_a_missing_one_gets_words() {
		let tokens = Tokens::MOCHA;
		let mut pictures = HashMap::new();
		pictures.insert("pics/a.png".to_owned(), [ 200u32, 100 ]);

		let source = "![宽图](pics/a.png)\n\n![远端](https://x.example/b.png)\n\n![丢了](pics/gone.png)\n";
		let doc = layout(source, 400.0, &tokens, &pictures);

		let band = &doc.rows[0];
		assert!(band.image.is_some(), "the known picture's row is a band");
		assert_eq!(band.image.as_deref(), Some("pics/a.png"), "and it carries what the pane looks up");
		// half of 400 is 200 wide, and 200 × 100/200 = 100 tall, plus the band's own air
		assert!((band.height - 112.0).abs() < 0.01, "the band is the picture's shape: {}", band.height);
		assert!(band.text.contains("宽图"), "with the alt kept beside it: {}", band.text);

		let remote = doc.rows.iter().find(|row| row.text.contains("远端")).expect("the remote line");
		assert!(remote.image.is_none(), "a remote picture is words, not a band");
		assert!(remote.text.contains("远程图片不加载"), "{}", remote.text);

		let gone = doc.rows.iter().find(|row| row.text.contains("丢了")).expect("the missing line");
		assert!(gone.text.contains("图片读不到"), "{}", gone.text);
		assert_eq!(gone.colour, tokens.err, "and it is said in the error colour");
	}

}