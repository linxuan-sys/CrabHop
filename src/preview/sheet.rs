//! A spreadsheet's used range, as a table the pane can draw (P41): the file's own column widths
//! and row heights (clamped by the tokens), the dense rectangle of cells with its holes, and the
//! merged spans — each anchored at its top-left. The pane paints lines from this; nothing here
//! formats text.
//!
//! The range is what calamine answers for the sheet: its smallest rectangle is exactly the
//! non-empty cells' own span, which is what "used" means here. Widths, heights and merges come
//! from the sheet's own XML (calamine's xlsx reader covers the merges; the dimensions are read
//! beside it, with the workbook's own relationship resolving the sheet to its part).

use std::io::{Cursor, Read as _};

use calamine::{Data, Reader as _};
use quick_xml::{Reader, events::Event};
use quick_xml::events::BytesStart;

use crate::tokens::{SHEET_COL_MAX_W, SHEET_COL_MIN_W, SHEET_ROW_MAX_H, SHEET_ROW_MIN_H};

pub(crate) struct Sheet;

/// A file's width unit — one "character" of the sheet's own font — as points on screen, and its
/// height unit: a row's `ht` is in points already, and the screen's are a third longer.
const CHAR_W: f32 = 7.0;
const CHAR_PAD: f32 = 5.0;
const POINT_PX: f32 = 4.0 / 3.0;
/// What a column without a width entry stands at: the sheet default, Excel's own 8.43 characters.
const DEFAULT_W: f32 = 8.43 * CHAR_W + CHAR_PAD;
/// And a row's: 15 points, the sheet's own default height.
const DEFAULT_H: f32 = 15.0 * POINT_PX;

/// The used range as a table: everything the pane's grid needs, nothing of its own.
pub(crate) struct Table {
	/// One width per column, in points on screen — the file's own, clamped by the tokens.
	pub(crate) cols:   Vec<f32>,
	/// One height per row, same rule.
	pub(crate) rows:   Vec<f32>,
	/// The cells, row-major over the range's own rectangle; an empty cell is an empty string and
	/// the grid marches through it all the same.
	pub(crate) cells:  Vec<Vec<String>>,
	/// The merged spans as `(r1, c1, r2, c2)`, inclusive and relative to the range: the anchor
	/// holds the text, the pane keeps the lines outside and no line inside.
	pub(crate) merges: Vec<(usize, usize, usize, usize)>,
}

impl Table {
	/// The grid's own height at its column: what the pane scrolls against.
	pub(crate) fn height(&self) -> f32 {
		self.rows.iter().sum()
	}

	/// The merged span a cell stands in, if any — the anchor's own included.
	pub(crate) fn span(&self, r: usize, c: usize) -> Option<(usize, usize, usize, usize)> {
		self.merges.iter().copied().find(|&(r1, c1, r2, c2)| (r1..=r2).contains(&r) && (c1..=c2).contains(&c))
	}

	/// The text the pane draws for the cell: the anchor's when the cell is one, nothing when the
	/// cell is only a span's body — an inner cell says nothing of its own.
	pub(crate) fn text(&self, r: usize, c: usize) -> Option<&str> {
		match self.span(r, c) {
			Some((r1, c1, ..)) if (r, c) != (r1, c1) => None,
			_ => self.cells.get(r).and_then(|row| row.get(c)).map(String::as_str).filter(|text| !text.is_empty()),
		}
	}
}

impl Sheet {
	const MAX_ROWS: usize = 200;
	const MAX_COLS: usize = 24;

	/// The sheet's used range, read whole (P41): `None` when the file is not a workbook or its
	/// first sheet is empty.
	pub(crate) fn read(bytes: &[u8]) -> Option<Table> {
		let mut book = calamine::open_workbook_auto_from_rs(Cursor::new(bytes)).ok()?;
		let name = book.sheet_names().into_iter().next()?;
		let range = book.worksheet_range(&name).ok()?;
		if range.is_empty() {
			return None;
		}

		let start = range.start().map_or((0usize, 0usize), |(row, col)| (row as usize, col as usize));
		let rows = range.height().min(Self::MAX_ROWS);
		let cols = range.width().min(Self::MAX_COLS);

		let cells: Vec<Vec<String>> = range
			.rows()
			.take(rows)
			.map(|row| row.iter().take(cols).map(cell_text).collect())
			.collect();

		// the sheet's own XML for the dimensions: the workbook names the sheet's part, its rels
		// resolve it, and the part carries the column widths and row heights
		let dims = Self::dimensions(bytes, &name).unwrap_or_default();
		// the file's own indices are 1-based; the range's start is 0-based
		let cols_w: Vec<f32> = (0..cols)
			.map(|i| clamp_col(dims.cols.get(&(start.1 + i + 1)).copied().unwrap_or(DEFAULT_W)))
			.collect();
		let rows_h: Vec<f32> = (0..rows)
			.map(|i| clamp_row(dims.rows.get(&(start.0 + i + 1)).copied().unwrap_or(DEFAULT_H)))
			.collect();

		// the merges calamine reads for xlsx, shifted into the range and clipped to it (another
		// container's own merges are nobody's business here: the empty list stands)
		let found = match &mut book {
			calamine::Sheets::Xlsx(xlsx) => xlsx.merge_cells_by_sheet_name(&name).unwrap_or_default(),
			_ => Vec::new(),
		};
		let merges: Vec<(usize, usize, usize, usize)> = found
			.into_iter()
			.filter_map(|dim| {
				let (r1, c1) = (dim.start.0 as usize, dim.start.1 as usize);
				let (r2, c2) = (dim.end.0 as usize, dim.end.1 as usize);
				if r2 < start.0 || c2 < start.1 || r1 >= start.0 + rows || c1 >= start.1 + cols {
					return None;
				}
				Some((
					r1.saturating_sub(start.0),
					c1.saturating_sub(start.1),
					(r2.saturating_sub(start.0)).min(rows - 1),
					(c2.saturating_sub(start.1)).min(cols - 1),
				))
			})
			.collect();

		Some(Table { cols: cols_w, rows: rows_h, cells, merges })
	}

	/// The sheet's own dimensions, read from its XML: column widths by their 1-based index and
	/// row heights by theirs, in the file's units. `None` when the part cannot be found — the
	/// caller then falls back to the defaults.
	fn dimensions(bytes: &[u8], sheet: &str) -> Option<Dims> {
		let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
		let part = sheet_target(&mut zip, sheet)?;
		let mut part = zip.by_name(&part).ok()?;
		let mut xml = Vec::new();
		part.read_to_end(&mut xml).ok()?;

		let mut dims = Dims::default();
		let mut reader = Reader::from_reader(std::io::BufReader::new(std::io::Cursor::new(&xml[..])));
		let mut buf = Vec::new();
		loop {
			match reader.read_event_into(&mut buf) {
				Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
					let attrs = attrs_of(&e);
					match e.local_name().into_inner() {
						"col" => {
							let get = |name: &str| attrs.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone());
							if let (Some(min), Some(max), Some(width)) =
								(get("min"), get("max"), get("width"))
								&& let (Ok(min), Ok(max), Ok(width)) =
									(min.parse::<usize>(), max.parse::<usize>(), width.parse::<f32>())
							{
								for at in min..=max {
									dims.cols.insert(at, width * CHAR_W + CHAR_PAD);
								}
							}
						}
						"row" => {
							let get = |name: &str| attrs.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone());
							if let (Some(at), Some(ht)) = (get("r"), get("ht"))
								&& let (Ok(at), Ok(ht)) = (at.parse::<usize>(), ht.parse::<f32>())
							{
								dims.rows.insert(at, ht * POINT_PX);
							}
						}
						_ => {}
					}
				}
				Ok(Event::Eof) => break,
				Err(_) => break,
				_ => {}
			}
			buf.clear();
		}

		Some(dims)
	}
}

/// The width a column stands at, clamped by the tokens (P41's own min/max).
fn clamp_col(w: f32) -> f32 {
	w.clamp(SHEET_COL_MIN_W, SHEET_COL_MAX_W)
}

/// The height a row stands at, the same rule.
fn clamp_row(h: f32) -> f32 {
	h.clamp(SHEET_ROW_MIN_H, SHEET_ROW_MAX_H)
}

#[derive(Default)]
struct Dims {
	/// By the file's own 1-based column index, in points on screen.
	cols: std::collections::HashMap<usize, f32>,
	/// And the rows', by theirs — the height as read, still in points.
	rows: std::collections::HashMap<usize, f32>,
}

/// Where a sheet's own part lives: the workbook's `sheet` element gives its relationship id and
/// the workbook's rels give the path — `xl/worksheets/sheet1.xml` by the usual convention.
fn sheet_target(zip: &mut zip::ZipArchive<Cursor<&[u8]>>, sheet: &str) -> Option<String> {
	let mut workbook = Vec::new();
	zip.by_name("xl/workbook.xml").ok()?.read_to_end(&mut workbook).ok()?;

	let mut rel = None;
	let mut reader = Reader::from_reader(std::io::BufReader::new(std::io::Cursor::new(&workbook[..])));
	let mut buf = Vec::new();
	loop {
		match reader.read_event_into(&mut buf) {
			Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.local_name().into_inner() == "sheet" => {
				let attrs = attrs_of(&e);
				let get = |name: &str| attrs.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone());
				if get("name").as_deref() == Some(sheet)
					&& let Some(id) = get("r:id")
				{
					rel = Some(id);
					break;
				}
			}
			Ok(Event::Eof) => break,
			Err(_) => break,
			_ => {}
		}
		buf.clear();
	}
	let rel = rel?;

	let mut rels = Vec::new();
	zip.by_name("xl/_rels/workbook.xml.rels").ok()?.read_to_end(&mut rels).ok()?;
	let mut target = None;
	let mut reader = Reader::from_reader(std::io::BufReader::new(std::io::Cursor::new(&rels[..])));
	let mut buf = Vec::new();
	loop {
		match reader.read_event_into(&mut buf) {
			Ok(Event::Start(e)) | Ok(Event::Empty(e)) if e.local_name().into_inner() == "Relationship" => {
				let attrs = attrs_of(&e);
				let get = |name: &str| attrs.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone());
				if get("Id").as_deref() == Some(rel.as_str())
					&& let Some(found) = get("Target")
				{
					target = Some(found);
					break;
				}
			}
			Ok(Event::Eof) => break,
			Err(_) => break,
			_ => {}
		}
		buf.clear();
	}
	let target = target?;

	Some(if let Some(rest) = target.strip_prefix('/') { rest.to_owned() } else { format!("xl/{target}") })
}

/// One element's attributes as owned pairs — the three readers below share the one parse.
fn attrs_of(e: &BytesStart<'_>) -> Vec<(String, String)> {
	e.attributes()
		.flatten()
		.filter_map(|attr| {
			let key = attr.key.as_ref().to_owned();
			let value = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok()?.into_owned();
			Some((key, value))
		})
		.collect()
}

/// A cell's text as the pane reads it: numbers keep the shape the sheet wrote them in, an empty
/// cell says nothing.
fn cell_text(data: &Data) -> String {
	match data {
		Data::Empty => String::new(),
		other => other.to_string(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	use std::io::Write as _;

	/// A minimal xlsx, written with the same zip crate the reader unpacks (P41's own fixture): one
	/// sheet with the file's own dimensions, a sparse rectangle, and a merged span.
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

		// the sheet's own shape: A wide, B the default, C narrow; row 1 tall; A1:B1 merged; a gap
		// at B3 so the grid has a hole to march through
		zip.start_file("xl/worksheets/sheet1.xml", options).unwrap();
		zip.write_all(r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cols><col min="1" max="1" width="18"/><col min="3" max="3" width="4"/></cols><sheetData><row r="1" ht="30" customHeight="1"><c r="A1" t="inlineStr"><is><t>名称</t></is></c><c r="B1"><v>1</v></c><c r="C1" t="inlineStr"><is><t>注</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>鸭子</t></is></c><c r="B2"><v>42</v></c><c r="C2"><v>7.5</v></c></row><row r="3"><c r="A3" t="inlineStr"><is><t>duck</t></is></c><c r="C3" t="inlineStr"><is><t>x</t></is></c></row></sheetData><mergeCells count="1"><mergeCell ref="A1:B1"/></mergeCells></worksheet>"#.as_bytes()).unwrap();

		zip.finish().unwrap().into_inner()
	}

	/// The used range arrives whole (P41): the grid's own dimensions from the file, the cells in
	/// their own columns, the hole inside the range, and the merge anchored at its top-left.
	#[test]
	fn the_used_range_arrives_as_a_table() {
		let bytes = fixture();
		let table = Sheet::read(&bytes).expect("a sheet should read");

		assert_eq!(table.cells.len(), 3, "three rows");
		assert_eq!(table.cells[0].len(), 3, "three columns");
		assert_eq!(table.cols.len(), 3);
		assert_eq!(table.rows.len(), 3);

		// the dimensions are the file's own, clamped: A is wide, B default, C floored by the token
		assert!(table.cols[0] > table.cols[1], "A is wider than B: {:?}", table.cols);
		assert!(table.cols[2] < table.cols[1], "C is narrower than B: {:?}", table.cols);
		assert_eq!(table.cols[2], SHEET_COL_MIN_W, "and the token floor holds: {:?}", table.cols);
		assert!(table.rows[0] > table.rows[1], "the first row is the tall one: {:?}", table.rows);

		// the hole: B3 is empty and still stands in the grid's rectangle
		assert_eq!(table.cells[2][1], "");
		assert_eq!(table.text(2, 1), None, "an empty cell says nothing");

		// the merge: the text lives on A1, the body says nothing of its own
		assert_eq!(table.merges, vec![ (0, 0, 0, 1) ]);
		assert_eq!(table.text(0, 0), Some("名称"));
		assert_eq!(table.text(0, 1), None);
		assert_eq!(table.span(0, 1), Some((0, 0, 0, 1)), "the body knows its span");
		assert_eq!(table.span(1, 1), None, "and a free cell has none");

		assert!(table.height() > 0.0, "the grid has its own height");
	}
}