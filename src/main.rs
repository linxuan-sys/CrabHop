//! duckfm-native: the window is the application. No engine behind it, no grid to mirror — the
//! list, the cursor, the selection and the scrolling are all this thread's own state.

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

	// the harness is the default whenever a replay or a shot is asked for (P25): no window at all,
	// and `DUCKFM_HEADLESS=0` brings the real one back for a person's own eyes
	if harness::wanted() {
		let frames = harness::run();
		eprintln!("duckfm-native: headless harness painted {frames} frames");
		return Ok(());
	}

	eframe::run_native(
		"duckfm-native",
		eframe::NativeOptions {
			viewport: eframe::egui::ViewportBuilder::default()
				.with_title("duckfm-native")
				.with_inner_size(inner_size()),
			..Default::default()
		},
		Box::new(|cc| Ok(Box::new(app::App::new(cc)))),
	)
}

/// The window's own size, unless `DUCKFM_WINDOW=WxH` asks for another: the acceptance's narrow
/// shots need a width the three columns can still be read at, and asking the window beats asking
/// the window manager (which the red lines leave alone).
pub(crate) fn inner_size() -> [f32; 2] {
	let Some((w, h)) = std::env::var("DUCKFM_WINDOW").ok().and_then(|size| {
		let (w, h) = size.split_once('x')?;
		Some((w.trim().parse::<f32>().ok()?, h.trim().parse::<f32>().ok()?))
	}) else {
		return [ 1280.0, 800.0 ];
	};

	[ w.clamp(320.0, 4096.0), h.clamp(240.0, 4096.0) ]
}
