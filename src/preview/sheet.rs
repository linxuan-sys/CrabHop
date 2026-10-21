//! The first screenful of a spreadsheet, as columns of text.
//!
//! Moved here from the legacy window; the one change is the width
//! measure — the legacy window used ratatui's cell width, this one the list's own
//! `crate::list::cell_width`, so a CJK cell still lines up with the ASCII ones around it.

use std::io::Cursor;

use calamine::{Data, Reader as _};

use crate::list::cell_width;

pub(crate) struct Sheet;

impl Sheet {
	const MAX_ROWS: usize = 200;
	const MAX_COLS: usize = 24;
	const GAP: &'static str = "  ";

	pub(crate) fn extract(bytes: &[u8]) -> Option<String> {
		let mut book = calamine::open_workbook_auto_from_rs(Cursor::new(bytes)).ok()?;
		let name = book.sheet_names().into_iter().next()?;
		let range = book.worksheet_range(&name).ok()?;

		let rows: Vec<Vec<String>> = range
			.rows()
			.take(Self::MAX_ROWS)
			.map(|row| row.iter().take(Self::MAX_COLS).map(Self::value).collect())
			.collect();

		// pad by *columns*, so a CJK cell lines up with the ASCII ones around it
		let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
		let mut widths = vec![0usize; cols];
		for row in &rows {
			for (i, cell) in row.iter().enumerate() {
				widths[i] = widths[i].max(cell.as_str().cell_width() as usize);
			}
		}

		let mut out = format!("{name}\n\n");
		for row in &rows {
			let mut line = String::new();
			for (i, cell) in row.iter().enumerate() {
				line.push_str(cell);
				if i + 1 < row.len() {
					let pad = widths[i].saturating_sub(cell.as_str().cell_width() as usize);
					line.push_str(&" ".repeat(pad));
					line.push_str(Self::GAP);
				}
			}

			out.push_str(line.trim_end());
			out.push('\n');
		}

		let text = out.trim_end().to_owned();
		(!text.is_empty()).then_some(text)
	}

	fn value(data: &Data) -> String { data.to_string() }
}

/// The width of a string in cells: the same rule the list clips its names by, borrowed so a sheet
/// and a file name agree on what "wide" means.
trait Cells {
	fn cell_width(&self) -> u32;
}

impl Cells for str {
	fn cell_width(&self) -> u32 { self.chars().map(cell_width).sum() }
}

#[cfg(test)]
mod tests {
	use std::io::Write;

	use super::*;

	/// A minimal xlsx, written with the same zip crate the reader unpacks: one sheet, four cells.
	fn fixture() -> Vec<u8> {
		let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
		let options: zip::write::FileOptions<'_, ()> =
			zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);

		zip.start_file("[Content_Types].xml", options).unwrap();
		zip.write_all(r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/></Types>"#.as_bytes()).unwrap();

		zip.start_file("xl/workbook.xml", options).unwrap();
		zip.write_all(r#"<?xml version="1.0"?><workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="数据" sheetId="1" r:id="rId1"/></sheets></workbook>"#.as_bytes()).unwrap();

		zip.start_file("_rels/.rels", options).unwrap();
		zip.write_all(r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#.as_bytes()).unwrap();

		zip.start_file("xl/_rels/workbook.xml.rels", options).unwrap();
		zip.write_all(r#"<?xml version="1.0"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#.as_bytes()).unwrap();

		zip.start_file("xl/worksheets/sheet1.xml", options).unwrap();
		zip.write_all(r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>名称</t></is></c><c r="B1" t="inlineStr"><is><t>数量</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>鸭子</t></is></c><c r="B2"><v>42</v></c></row><row r="3"><c r="A3" t="inlineStr"><is><t>duck</t></is></c><c r="B3"><v>7.5</v></c></row></sheetData></worksheet>"#.as_bytes()).unwrap();

		zip.finish().unwrap().into_inner()
	}

	#[test]
	fn rows_become_aligned_columns() {
		let bytes = fixture();
		if let Err(e) = calamine::open_workbook_auto_from_rs(Cursor::new(&bytes)) {
			panic!("the fixture is not a workbook calamine accepts: {e:?}");
		}

		let text = Sheet::extract(&bytes).expect("a sheet should extract");

		assert!(text.starts_with("数据"), "the sheet name leads: {text}");
		assert!(text.contains('鸭'), "inline strings arrive");
		assert!(text.contains("42") && text.contains("7.5"), "numbers keep their text: {text}");

		// the second column starts at the same display column in both rows
		let starts: Vec<u32> = text
			.lines()
			.filter(|l| l.contains('鸭') || l.contains("duck"))
			.map(|l| l.split_once("  ").expect("the column gap").0.cell_width())
			.collect();
		assert_eq!(starts.len(), 2);
		assert_eq!(starts[0], starts[1], "the second column must line up: {text}");
	}

	#[test]
	fn junk_is_not_a_spreadsheet() {
		assert!(Sheet::extract(b"not a workbook").is_none());
	}
}