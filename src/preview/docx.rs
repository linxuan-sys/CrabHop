//! `word/document.xml` as plain text: what a preview needs, and nothing else.
//!
//! Moved here from the legacy window whole: the extraction was
//! verified there, and a preview's job is structure, not typographic fidelity (§3.6).

use std::io::{BufReader, Cursor};

use quick_xml::{Reader, events::Event};

pub(crate) struct Docx;

impl Docx {
	/// Where the preview stops: this many paragraphs, or this many bytes, whichever comes first.
	/// The byte cap is the memory bound (one preview is held at a time), and it is generous on
	/// purpose — a book chapter should not lose its second half to a cap.
	const MAX_PARAGRAPHS: usize = 1200;
	const MAX_BYTES: usize = 256 * 1024;

	pub(crate) fn extract(bytes: &[u8]) -> Option<String> {
		let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
		let document = zip.by_name("word/document.xml").ok()?;

		let mut reader = Reader::from_reader(BufReader::new(document));
		reader.config_mut().trim_text(false);

		let mut out = String::new();
		let mut paragraph = String::new();
		let mut state = State::default();
		let mut buf = Vec::new();

		loop {
			let done = match reader.read_event_into(&mut buf) {
				Ok(Event::Start(e)) => {
					state.open(e.local_name().into_inner());
					false
				}
				Ok(Event::Empty(e)) => {
					// an empty element has no end tag to close it, but it is not the end of the part
					state.open(e.local_name().into_inner());
					false
				}
				Ok(Event::Text(t)) if state.text => {
					paragraph.push_str(&t.xml10_content());
					false
				}
				Ok(Event::End(e)) => state.close(e.local_name().into_inner(), &mut paragraph, &mut out),
				Ok(Event::Eof) => true,
				// a truncated or malformed part still previews as far as it got
				Err(_) => true,
				_ => false,
			};

			buf.clear();
			if done || state.paragraphs >= Self::MAX_PARAGRAPHS || out.len() >= Self::MAX_BYTES {
				break;
			}
		}

		let text = out.trim_end().to_owned();
		(!text.is_empty()).then_some(text)
	}
}

/// The little bit of document structure the extraction cares about.
#[derive(Default)]
struct State {
	/// Inside a `w:t`, so text is worth collecting.
	text:       bool,
	/// Inside a `w:instrText` or `w:delText`, whose content is not worth showing.
	skipped:    usize,
	/// Inside a `w:tc`, where paragraphs are cells of one line rather than lines of their own.
	cell:       usize,
	/// The paragraph carries `w:numPr`, so it is a list item.
	bullet:     bool,
	paragraphs: usize,
}

impl State {
	fn open(&mut self, name: &str) {
		match name {
			"t" => self.text = true,
			"instrText" | "delText" => self.skipped += 1,
			"numPr" => self.bullet = true,
			"tc" => self.cell += 1,
			_ => {}
		}
	}

	/// Returns true when there is nothing left to read in this part.
	fn close(&mut self, name: &str, paragraph: &mut String, out: &mut String) -> bool {
		match name {
			"t" => self.text = false,
			"instrText" | "delText" => self.skipped = self.skipped.saturating_sub(1),
			"p" => {
				let text = paragraph.trim();
				if !text.is_empty() {
					if self.bullet {
						out.push_str("• ");
					}
					out.push_str(text);
					// a cell joins its paragraphs into one line; the table row ends the line
					out.push(if self.cell > 0 { ' ' } else { '\n' });
				}

				paragraph.clear();
				self.bullet = false;
				self.paragraphs += 1;
			}
			// a cell separator the preview can actually draw: a tab is a control character, and it
			// reaches neither the width maths nor the painter
			"tc" => out.push(' '),
			"tr" => out.push('\n'),
			_ => {}
		}

		false
	}
}

#[cfg(test)]
mod tests {
	use std::io::Write;

	use super::*;

	/// A minimal `docx` built with the same crate that reads it: two paragraphs (one a list item), a
	/// table, a field instruction and a deletion that must not show up.
	fn fixture(document: &str) -> Vec<u8> {
		let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
		let options: zip::write::FileOptions<'_, ()> =
			zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

		zip.start_file("[Content_Types].xml", options).unwrap();
		zip.write_all(
			br#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
		)
		.unwrap();
		zip.start_file("word/document.xml", options).unwrap();
		zip.write_all(document.as_bytes()).unwrap();

		zip.finish().unwrap().into_inner()
	}

	#[test]
	fn paragraphs_tables_lists_and_skipped_runs() {
		let document = r#"<?xml version="1.0"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
	<w:p><w:r><w:t>第一段</w:t></w:r></w:p>
	<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/></w:numPr></w:pPr><w:r><w:t>列表项</w:t></w:r></w:p>
	<w:p><w:r><w:instrText>PAGE</w:instrText><w:t>可见</w:t><w:delText>删掉的</w:delText></w:r></w:p>
	<w:tbl><w:tr><w:tc><w:p><w:r><w:t>甲</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>乙</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
</w:body></w:document>"#;

		let text = Docx::extract(&fixture(document)).expect("a document part should extract");

		assert!(text.contains("第一段"));
		assert!(text.contains("• 列表项"), "list items keep a bullet: {text}");
		assert!(text.contains("可见"), "visible runs survive");
		assert!(!text.contains("PAGE"), "field instructions are dropped: {text}");
		assert!(!text.contains("删掉的"), "deleted text is dropped: {text}");

		let table = text.lines().find(|l| l.contains('甲')).expect("the table row");
		assert_eq!(
			table.split_whitespace().collect::<Vec<_>>(),
			vec!["甲", "乙"],
			"cells sit on one line: {table:?}"
		);
		assert!(!text.contains('\t'), "no control character may reach the painter: {text:?}");
	}

	#[test]
	fn stops_at_the_paragraph_limit() {
		let mut document = String::from(r#"<w:document xmlns:w="x"><w:body>"#);
		for i in 0..1500 {
			document.push_str(&format!("<w:p><w:r><w:t>p{i}</w:t></w:r></w:p>"));
		}
		document.push_str("</w:body></w:document>");

		let text = Docx::extract(&fixture(&document)).expect("a long document still extracts");
		let paragraphs = text.lines().count();
		assert!(paragraphs <= Docx::MAX_PARAGRAPHS, "{paragraphs} paragraphs is too many");
		assert!(paragraphs > 1000, "and it should not stop early: {paragraphs}");
	}

	#[test]
	fn a_zip_without_a_document_part_is_not_a_docx() {
		let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
		zip.start_file::<_, ()>("hello.txt", zip::write::FileOptions::default()).unwrap();
		zip.write_all(b"not a document").unwrap();
		let bytes = zip.finish().unwrap().into_inner();

		assert!(Docx::extract(&bytes).is_none());
		assert!(Docx::extract(b"definitely not a zip").is_none());
	}
}