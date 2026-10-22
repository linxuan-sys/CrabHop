
mod app;
mod book;
mod bulk;
mod chrome;
mod complete;
mod config;
mod desktop;
mod edit;
mod folder;
mod frecency;
mod fuzzy;
mod icons;
mod harness;
mod keys;
mod list;
mod menu;
mod metrics;
mod mime;
mod mount;
mod open;
mod pack;
mod paste;
mod preview;
mod scroll;
mod search;
mod shell;
mod spot;
mod toast;
mod tokens;
mod trash;
mod uri_clipboard;

fn main() -> eframe::Result {
	metrics::start();

	if harness::wanted() {
		let frames = harness::run();
		eprintln!("crabhop: headless harness painted {frames} frames");
		return Ok(());
	}

	desktop::register();

	let mut viewport = eframe::egui::ViewportBuilder::default().with_title("CrabHop").with_inner_size(inner_size());
	if let Some(icon) = window_icon() {
		viewport = viewport.with_icon(icon);
	}

	eframe::run_native(
		"crabhop",
		eframe::NativeOptions { viewport, ..Default::default() },
		Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
	)
}

pub(crate) fn window_icon() -> Option<eframe::egui::IconData> {
	let bytes = include_bytes!("../assets/icons/hicolor/256x256/apps/crabhop.png");
	let image = image::load_from_memory(bytes).ok()?.to_rgba8();
	let (width, height) = image.dimensions();

	Some(eframe::egui::IconData { rgba: image.into_raw(), width, height })
}

pub(crate) fn inner_size() -> [f32; 2] {
	let Some((w, h)) = std::env::var("CRABHOP_WINDOW").ok().and_then(|size| {
		let (w, h) = size.split_once('x')?;
		Some((w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?))
	}) else {
		return [ 1280.0, 800.0 ];
	};

	[ w.clamp(320.0, 4096.0), h.clamp(240.0, 4096.0) ]
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_window_icon_decodes() {
		let icon = window_icon().expect("the embedded icon decodes");
		assert_eq!((icon.width, icon.height), (256, 256), "the hicolor 256, the one the packaging installs");
		assert_eq!(icon.rgba.len(), 256 * 256 * 4, "RGBA, whole");
	}
}
