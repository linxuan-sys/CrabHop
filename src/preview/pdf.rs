
use std::sync::OnceLock;

use pdfium_render::prelude::*;

use crate::preview::img;

const MAPLE_REGULAR: &[u8] = include_bytes!("../../assets/fonts/MapleMonoNormal-NF-CN-Regular.ttf");
const MAPLE_BOLD: &[u8] = include_bytes!("../../assets/fonts/MapleMonoNormal-NF-CN-Bold.ttf");

pub(crate) const RENDER_DPI: f32 = 150.0;

pub(crate) const NOTE_FAILED: &str = "PDF 已加密或损坏";

struct MapleFonts;

impl PdfiumCustomFontProvider for MapleFonts {
	fn provide(
		&mut self,
		request: PdfiumCustomFontProviderRequest,
	) -> Option<PdfiumCustomFontProviderResponse> {
		let bold = matches!(
			request.weight,
			PdfFontWeight::Weight600 | PdfFontWeight::Weight700Bold | PdfFontWeight::Weight800 | PdfFontWeight::Weight900
		);

		Some(PdfiumCustomFontProviderResponse {
			id: if bold { 2 } else { 1 },
			font_face: request.font_face,
			character_set: request.character_set,
			data: if bold { MAPLE_BOLD.to_vec() } else { MAPLE_REGULAR.to_vec() },
		})
	}
}

static PDFIUM: OnceLock<Option<Pdfium>> = OnceLock::new();

fn library() -> Option<&'static Pdfium> {
	PDFIUM
		.get_or_init(|| match Pdfium::bind_to_statically_linked_library() {
			Ok(bindings) => {
				let mut pdfium = Pdfium::new(bindings);
				pdfium.set_custom_font_provider(Box::new(MapleFonts));

				eprintln!("crabhop: pdfium is bound");
				Some(pdfium)
			}
			Err(e) => {
				eprintln!("crabhop: pdfium is not available, PDFs fall back to text: {e}");
				None
			}
		})
		.as_ref()
}

pub(crate) fn warm() {
	library();
}

pub(crate) struct Render {
	pub(crate) page:  usize,
	pub(crate) total: usize,
	pub(crate) size:  [u32; 2],
	pub(crate) rgba:  Vec<u8>,
}

pub(crate) fn render(bytes: &[u8], target: [u32; 2], page: usize) -> Option<Render> {
	let pdfium = library()?;
	let doc = pdfium.load_pdf_from_byte_slice(bytes, None).ok()?;
	let total = doc.pages().len() as usize;
	let page_handle = doc.pages().get(page as PdfPageIndex).ok()?;

	let (pw, ph) = (page_handle.width().value.max(1.0), page_handle.height().value.max(1.0));
	let ratio = ph / pw;

	let dpi_scale = RENDER_DPI / 72.0;
	let bitmap = page_handle
		.render_with_config(&PdfRenderConfig::new().scale_page_by_factor(dpi_scale))
		.ok()?;
	if bitmap.width() <= 0 || bitmap.height() <= 0 {
		return None;
	}

	let mut size = [ bitmap.width() as u32, bitmap.height() as u32 ];
	let mut rgba = bitmap.as_rgba_bytes();

	if size[0] > target[0].max(1) {
		let width = target[0].max(1);
		let height = ((width as f32 * ratio).round() as u32).max(1);
		rgba = img::resample(&rgba, size, [ width, height ])?;
		size = [ width, height ];
	}

	Some(Render { page, total, size, rgba })
}

pub(crate) struct Pdf;

impl Pdf {
	pub(crate) fn extract(bytes: &[u8]) -> Option<String> {
		let pages = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(bytes))
			.ok()?
			.ok()?;

		let text = pages.first()?.trim().to_owned();
		(!text.is_empty() && !text.chars().all(|c| c.is_control() || c.is_whitespace())).then_some(text)
	}
}

#[cfg(test)]
mod tests {
	use lopdf::{Document, Object, Stream, dictionary};

	use super::*;

	fn fixture(lines: &[(&str, i64)]) -> Vec<u8> {
		let mut doc = Document::with_version("1.5");

		let font = doc.add_object(dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" });
		let kids: Vec<Object> = lines
			.iter()
			.map(|(text, y)| {
				let content = format!("BT /F1 24 Tf 72 {y} Td ({text}) Tj ET");
				let stream = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
				doc.add_object(dictionary! {
					"Type" => "Page",
					"MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
					"Contents" => stream,
					"Resources" => dictionary! { "Font" => dictionary! { "F1" => font } },
				})
				.into()
			})
			.collect();

		let pages = doc.add_object(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => lines.len() as i64 });
		let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
		doc.trailer.set("Root", Object::Reference(catalog));

		let mut out = Vec::new();
		doc.save_to(&mut out).expect("lopdf writes what it reads");
		out
	}

	#[test]
	fn a_page_renders_at_the_columns_width_and_the_page_counts() {
		let bytes = fixture(&[("First page", 720), ("Second page", 600)]);

		let page = render(&bytes, [ 320, 600 ], 0).expect("the fixture renders");
		assert_eq!(page.page, 0);
		assert_eq!(page.total, 2, "both pages are counted for the badge");
		assert_eq!(page.rgba.len(), page.size[0] as usize * page.size[1] as usize * 4, "four bytes a pixel");
		assert_eq!(page.size[0], 320, "fit-width");

		let ratio = page.size[0] as f32 / page.size[1] as f32;
		assert!((ratio - 612.0 / 792.0).abs() < 0.01, "the page keeps its shape: {:?}", page.size);

		assert!(render(&bytes, [ 320, 600 ], 5).is_none());
	}

	#[test]
	fn the_first_page_comes_out_as_text_and_later_pages_do_not() {
		let bytes = fixture(&[("First page", 720), ("Second page", 600)]);

		let text = Pdf::extract(&bytes).expect("a text page should extract");
		assert!(text.contains("First page"), "{text}");
		assert!(!text.contains("Second page"), "page two is not part of the preview: {text}");
	}

	#[test]
	fn junk_is_not_a_document() {
		assert!(render(b"definitely not a pdf", [ 320, 240 ], 0).is_none());
		assert!(Pdf::extract(b"not a pdf at all").is_none());
	}
}
