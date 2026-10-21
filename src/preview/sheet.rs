
use std::io::{Cursor, Read as _};

use calamine::{Data, Reader as _, Range};
use quick_xml::{Reader, events::Event};
use quick_xml::events::BytesStart;

use crate::tokens::{SHEET_COL_MAX_W, SHEET_COL_MIN_W};

pub(crate) struct Sheet;

const CHAR_W: f32 = 7.0;
const CHAR_PAD: f32 = 5.0;
const DEFAULT_W: f32 = 8.43 * CHAR_W + CHAR_PAD;

pub(crate) const HEAD_HEIGHT: f32 = 96.0;

pub(crate) struct Head {
	pub(crate) name:   String,
	pub(crate) labels: Vec<String>,
	pub(crate) cols:   Vec<f32>,
	pub(crate) rows:   usize,
	pub(crate) wide:   usize,
}

impl Sheet {
	const MAX_COLS: usize = 24;

	pub(crate) fn read(bytes: &[u8]) -> Option<Head> {
		let mut book = calamine::open_workbook_auto_from_rs(Cursor::new(bytes)).ok()?;
		let name = book.sheet_names().into_iter().next()?;
		let range = book.worksheet_range(&name).ok()?;
		if range.is_empty() {
			return None;
		}

		let wide = range.width();
		let cols = wide.min(Self::MAX_COLS);
		let labels = first_row(&range, cols);

		let start = range.start().map_or((0usize, 0usize), |(row, col)| (row as usize, col as usize));
		let widths = Self::widths(bytes, &name).unwrap_or_default();
		let cols_w: Vec<f32> = (0..cols).map(|i| clamp_col(widths.get(&(start.1 + i + 1)).copied().unwrap_or(DEFAULT_W))).collect();

		Some(Head { name, labels, cols: cols_w, rows: range.height(), wide })
	}

	fn widths(bytes: &[u8], sheet: &str) -> Option<std::collections::HashMap<usize, f32>> {
		let mut zip = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;
		let part = sheet_target(&mut zip, sheet)?;
		let mut part = zip.by_name(&part).ok()?;
		let mut xml = Vec::new();
		part.read_to_end(&mut xml).ok()?;

		let mut widths = std::collections::HashMap::new();
		let mut reader = Reader::from_reader(std::io::BufReader::new(std::io::Cursor::new(&xml[..])));
		let mut buf = Vec::new();
		loop {
			match reader.read_event_into(&mut buf) {
				Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
					if e.local_name().into_inner() == "col" && let Some((min, max, width)) = col_span(&e) {
						for at in min..=max {
							widths.insert(at, width * CHAR_W + CHAR_PAD);
						}
					}
				}
				Ok(Event::Eof) => break,
				Err(_) => break,
				_ => {}
			}
			buf.clear();
		}

		Some(widths)
	}
}

fn first_row(range: &Range<Data>, cols: usize) -> Vec<String> {
	range.rows().next().map(|row| row.iter().take(cols).map(cell_text).collect()).unwrap_or_default()
}

fn col_span(e: &BytesStart<'_>) -> Option<(usize, usize, f32)> {
	let attrs = attrs_of(e);
	let get = |name: &str| attrs.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone());
	let min = get("min")?.parse::<usize>().ok()?;
	let max = get("max")?.parse::<usize>().ok()?;
	let width = get("width")?.parse::<f32>().ok()?;

	Some((min, max, width))
}

fn clamp_col(w: f32) -> f32 {
	w.clamp(SHEET_COL_MIN_W, SHEET_COL_MAX_W)
}

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

	#[test]
	fn the_first_row_becomes_the_labels() {
		let range = Range::from_sparse(vec![
			calamine::Cell::new((2, 1), Data::String("名称".to_owned())),
			calamine::Cell::new((2, 3), Data::String("注".to_owned())),
			calamine::Cell::new((3, 1), Data::String("鸭子".to_owned())),
		]);

		assert_eq!(first_row(&range, 4), vec![ "名称", "", "注" ], "the first row, its blank kept in place");
		assert_eq!(first_row(&range, 2), vec![ "名称", "" ], "and the cap takes the rest away");
	}

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
		zip.write_all(r#"<?xml version="1.0"?><worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><cols><col min="1" max="1" width="18"/><col min="3" max="3" width="4"/></cols><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>名称</t></is></c><c r="B1"><v>1</v></c><c r="C1" t="inlineStr"><is><t>注</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>鸭子</t></is></c><c r="B2"><v>42</v></c></row><row r="3"><c r="A3" t="inlineStr"><is><t>duck</t></is></c><c r="B3"><v>7.5</v></c></row></sheetData></worksheet>"#.as_bytes()).unwrap();

		zip.finish().unwrap().into_inner()
	}

	#[test]
	fn the_head_carries_the_name_the_labels_and_the_shape() {
		let head = Sheet::read(&fixture()).expect("a sheet should read");

		assert_eq!(head.name, "数据");
		assert_eq!(head.labels, vec![ "名称", "1", "注" ], "the first row is the labels");
		assert_eq!(head.rows, 3, "the range's own rows");
		assert_eq!(head.wide, 3, "and its own columns");
		assert_eq!(head.cols.len(), 3);
		assert!(head.cols[0] > head.cols[1], "A is wider than B: {:?}", head.cols);
		assert_eq!(head.cols[2], SHEET_COL_MIN_W, "and the token floor holds: {:?}", head.cols);
	}
}
