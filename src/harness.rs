//! The headless harness (P25): the window without a window. A replay or a screenshot is asked for
//! by `CRABHOP_KEYS` / `CRABHOP_SHOT` as it always was — but instead of a real window, this builds
//! its own egui context, hands it a synthetic `RawInput`, runs `App::frame` on it, tessellates on
//! the CPU and paints into an offscreen texture with `egui_wgpu::Renderer`. `CRABHOP_SHOT`'s frames
//! come out as PNGs through the app's own `request_screenshot` callbacks, completed from the
//! read-back.
//!
//! Nothing here touches the user's session: no window, no surface, no compositor. RADV makes the
//! device without one (the spike proved it), and `CRABHOP_HEADLESS=0` is the way back to a real
//! window when a hand needs to see it.
//!
//! The sequence is the one the project's own acceptance already used: `cargo run` in a directory
//! with `CRABHOP_KEYS="key j;key k;shot"` and `CRABHOP_SHOT=<prefix>` — the same steps, the same
//! `Timing:` readings, the same `<prefix>-N.png` frames, and no window anywhere.

use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};

use crate::app::App;
use crate::tokens::{Tokens, ROW_HEIGHT};

/// Whether the headless path is the one to take: a replay is asked for, unless the caller says
/// otherwise. A real window is then only for a person's own eyes (`CRABHOP_HEADLESS=0`).
pub(crate) fn wanted() -> bool {
	match std::env::var("CRABHOP_HEADLESS") {
		Ok(asked) => asked != "0",
		Err(_) => std::env::var_os("CRABHOP_KEYS").is_some(),
	}
}

/// The frame's own size, in points: `CRABHOP_WINDOW` when it is set, the same default a window gets.
fn size() -> [f32; 2] {
	let size = crate::inner_size();

	[ size[0], size[1] ]
}

/// Run the replay to its end, headless, and return how many frames were painted.
pub(crate) fn run() -> usize {
	let (device, queue) = device();
	let format = wgpu::TextureFormat::Rgba8Unorm;

	let ctx = egui::Context::default();
	Tokens::install(&ctx);
	let mut session = Session::new(ctx, device, queue, format, size(), seed());

	let steps = session.steps();
	// one replay step every 350ms — the app's own cadence — plus the tail the shots need
	let frames = 40 + steps * 24;
	for _ in 0..frames {
		session.paint();
		std::thread::sleep(Duration::from_millis(16));
	}

	session.frames
}

/// A headless window: the device, the context, the app and the offscreen target. Kept in one place
/// so the tests can drive it frame by frame.
pub(crate) struct Session {
	pub(crate) ctx:   egui::Context,
	pub(crate) app:   App,
	device:           wgpu::Device,
	queue:            wgpu::Queue,
	renderer:         egui_wgpu::Renderer,
	target:           wgpu::Texture,
	view:             wgpu::TextureView,
	readback:         wgpu::Buffer,
	padded:           u32,
	size:             [u32; 2],
	/// The device's own texture ceiling, put into every frame's input the way eframe puts the
	/// window's: egui's default of 2048 would refuse a tall picture's upload that the real window
	/// accepts (P28's parity fix — the harness must be able to show what the window shows).
	texture_side:     usize,
	pub(crate) frames: usize,
	started:          Instant,
	/// A click the next frame should receive (P33⑮⑯'s harness reading of the mouse): the press and
	/// the release egui needs, in one frame, at the point a test asks for.
	click:            Option<egui::Pos2>,
	/// A pointer move the next frame should receive (P33②): a chip's × only exists once the chip
	/// is hovered, and hover settles a frame late — the move frame stands before the press, the
	/// way a hand's pointer does.
	moved:            Option<egui::Pos2>,
	/// Whether any frame has asked the window to close (P33⑯), read back by the test that presses
	/// the corner's ×.
	closed:           bool,
	/// What the last frame handed the platform for the IME (P33⑤): the field's box and the
	/// caret's column, as the candidate window's place — the tests read the very value eframe
	/// would pass on to winit.
	ime:              Option<egui::output::IMEOutput>,
	/// Raw events the next frame should receive (P35②), verbatim: what the window's own
	/// integration hands over — `Event::Copy`/`Cut`/`Paste` for the clipboard chords, which a
	/// real window never spells as key events.
	injected:         Vec<egui::Event>,
	/// The last text a frame put on the clipboard (`OutputCommand::CopyText`), for the tests
	/// that follow a field's own Ctrl+C (P35②).
	copied:           Option<String>,
}

impl Session {
	pub(crate) fn new(
		ctx: egui::Context,
		device: wgpu::Device,
		queue: wgpu::Queue,
		format: wgpu::TextureFormat,
		size: [f32; 2],
		seed: Vec<String>,
	) -> Self {
		let [width, height] = [ size[0].round() as u32, size[1].round() as u32 ];
		let target = device.create_texture(&wgpu::TextureDescriptor {
			label: Some("harness-target"),
			size: wgpu::Extent3d { width, height, depth_or_array_layers: 1 },
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format,
			usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
			view_formats: &[],
		});
		let view = target.create_view(&wgpu::TextureViewDescriptor::default());

		// row reads come back padded to 256 bytes (the same arithmetic every picture book uses)
		let padded = (width * 4).div_ceil(256) * 256;
		let readback = device.create_buffer(&wgpu::BufferDescriptor {
			label: Some("harness-readback"),
			size: (padded * height) as u64,
			usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
			mapped_at_creation: false,
		});

		ctx.set_pixels_per_point(1.0);
		// the harness never touches the session's own clipboard (P35④): the yank's offers are off,
		// and the text road carries every assertion
		let mut app = App::with_context(ctx.clone(), seed);
		app.set_uri_offers_for_tests(false);

		let texture_side = device.limits().max_texture_dimension_2d as usize;

		Self {
			renderer: egui_wgpu::Renderer::new(&device, format, egui_wgpu::RendererOptions::default()),
			ctx,
			app,
			device,
			queue,
			target,
			view,
			readback,
			padded,
			size: [ width, height ],
			texture_side,
			frames: 0,
			click: None,
			moved: None,
			closed: false,
			ime: None,
			injected: Vec::new(),
			copied: None,
			started: Instant::now(),
		}
	}

	fn steps(&self) -> usize {
		std::env::var("CRABHOP_KEYS").map(|keys| keys.split(';').filter(|s| !s.is_empty()).count()).unwrap_or(0)
	}

	/// One frame: the app's own ui, then the picture, then whatever `request_screenshot` asked for.
	/// A click a test asked for (P33⑮⑯): the pointer's move, its press and its release, in one
	/// frame — the same events a real mouse sends, in the same order.
	fn input_events(&mut self) -> Vec<egui::Event> {
		let mut events = std::mem::take(&mut self.injected);

		if let Some(at) = self.click.take() {
			self.moved = None;
			events.extend([
				egui::Event::PointerMoved(at),
				egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
				egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() },
			]);
		} else if let Some(at) = self.moved.take() {
			events.push(egui::Event::PointerMoved(at));
		}

		events
	}

	/// Whether a frame's output asked the window to close (P33⑯) — read while the output is still
	/// whole, before its shapes are moved into the tessellator.
	fn asked_to_close(out: &egui::FullOutput) -> bool {
		out.viewport_output
			.values()
			.flat_map(|viewport| viewport.commands.iter())
			.any(|command| matches!(command, egui::ViewportCommand::Close))
	}

	pub(crate) fn paint(&mut self) {
		let raw = egui::RawInput {
			screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(self.size[0] as f32, self.size[1] as f32))),
			time: Some(self.started.elapsed().as_secs_f64()),
			max_texture_side: Some(self.texture_side),
			events: self.input_events(),
			..Default::default()
		};

		let mut out = self.ctx.run_ui(raw, |ui| self.app.frame(ui));
		self.closed |= Self::asked_to_close(&out);
		self.ime = out.platform_output.ime;
		for command in &out.platform_output.commands {
			if let egui::OutputCommand::CopyText(text) = command {
				self.copied = Some(text.clone());
			}
		}
		let primitives = self.ctx.tessellate(out.shapes, out.pixels_per_point);

		// a frame that asked for a screenshot: the callbacks are answered from this very read-back
		let asked: Vec<egui::ScreenshotCallback> = out
			.viewport_output
			.values()
			.flat_map(|viewport| viewport.commands.iter())
			.filter_map(|command| match command {
				egui::ViewportCommand::Screenshot(callback) => Some(callback.clone()),
				_ => None,
			})
			.collect();

		let image = self.render(&primitives);
		for callback in asked {
			callback.complete(Arc::new(image.clone()));
		}

		// the texture deltas are the app's (and epaint's guard insists they are handled)
		for (id, deltas) in &out.textures_delta.set {
			for delta in deltas {
				self.renderer.update_texture(&self.device, &self.queue, *id, delta);
			}
		}
		out.textures_delta.clear();

		self.frames += 1;
		crate::metrics::frame(1.0 / 60.0);
	}

	/// Paint the primitives into the offscreen texture and read the pixels back.
	fn render(&mut self, primitives: &[egui::ClippedPrimitive]) -> egui::ColorImage {
		let screen = egui_wgpu::ScreenDescriptor { size_in_pixels: self.size, pixels_per_point: 1.0 };
		let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("harness") });

		let user_buffers = self.renderer.update_buffers(&self.device, &self.queue, &mut encoder, primitives, &screen);
		self.queue.submit(user_buffers);

		{
			let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("harness"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: &self.view,
					depth_slice: None,
					resolve_target: None,
					ops: wgpu::Operations {
						load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
						store: wgpu::StoreOp::Store,
					},
				})],
				depth_stencil_attachment: None,
				timestamp_writes: None,
				occlusion_query_set: None,
				multiview_mask: None,
			});
			// the lifetime is forgotten the way eframe forgets it: the pass never sees the encoder
			self.renderer.render(&mut pass.forget_lifetime(), primitives, &screen);
		}

		encoder.copy_texture_to_buffer(
			wgpu::TexelCopyTextureInfo {
				texture: &self.target,
				mip_level: 0,
				origin: wgpu::Origin3d::ZERO,
				aspect: wgpu::TextureAspect::All,
			},
			wgpu::TexelCopyBufferInfo {
				buffer: &self.readback,
				layout: wgpu::TexelCopyBufferLayout {
					offset: 0,
					bytes_per_row: Some(self.padded),
					rows_per_image: Some(self.size[1]),
				},
			},
			wgpu::Extent3d { width: self.size[0], height: self.size[1], depth_or_array_layers: 1 },
		);
		self.queue.submit(Some(encoder.finish()));

		let slice = self.readback.slice(..);
		let (tx, rx) = std::sync::mpsc::channel();
		slice.map_async(wgpu::MapMode::Read, move |result| {
			let _ = tx.send(result);
		});
		self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).expect("poll");
		rx.recv().expect("the map's own answer").expect("map failed");

		let data = slice.get_mapped_range().expect("mapped range");
		let mut pixels = Vec::with_capacity((self.size[0] * self.size[1]) as usize);
		for y in 0..self.size[1] {
			for x in 0..self.size[0] {
				let i = (y * self.padded + x * 4) as usize;
				pixels.push(egui::Color32::from_rgba_unmultiplied(data[i], data[i + 1], data[i + 2], data[i + 3]));
			}
		}
		drop(data);
		self.readback.unmap();

		egui::ColorImage::new([ self.size[0] as usize, self.size[1] as usize ], pixels)
	}

	/// Where the cursor stands, for a test that watches a replay step move it (P25).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn cursor(&self) -> usize {
		self.app.cursor_for_tests()
	}

	/// Paint frames until the listing holds at least `want` rows, and say whether it got there
	/// (P25's flake fix): the replay's own cadence is not something a test may lean on.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn wait_for_list(&mut self, want: usize, frames: usize) -> bool {
		for _ in 0..frames {
			if self.app.listed() >= want {
				return true;
			}
			std::thread::sleep(Duration::from_millis(20));
			self.shoot();
		}

		self.app.listed() >= want
	}

	/// One painted frame as a picture, for the tests to read: paint, then take the pixels.
	/// Queue a click for the next frame (P33⑮⑯): the point in the window a test's pointer presses.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn click_at(&mut self, at: egui::Pos2) {
		self.click = Some(at);
	}

	/// Queue a bare pointer move for the next frame (P33②): what a real hand does before a press —
	/// and what a chip needs before its × exists for the mouse to hit.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn move_at(&mut self, at: egui::Pos2) {
		self.moved = Some(at);
	}

	/// Whether any frame so far has asked the window to close (P33⑯).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn close_requested(&self) -> bool {
		self.closed
	}

	/// What the last frame handed the platform for the IME (P33⑤).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn ime_for_tests(&self) -> Option<egui::output::IMEOutput> {
		self.ime
	}

	/// Hand the next frame the very events a real window's integration would (P35②): the
	/// clipboard chords arrive as `Event::Copy` / `Event::Cut` / `Event::Paste`, never as keys.
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn inject(&mut self, events: Vec<egui::Event>) {
		self.injected.extend(events);
	}

	/// The last text a frame put on the clipboard (P35②'s field-side assertion).
	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn copied_for_tests(&self) -> Option<String> {
		self.copied.clone()
	}

	#[cfg_attr(not(test), allow(dead_code))]
	pub(crate) fn shoot(&mut self) -> egui::ColorImage {
		let raw = egui::RawInput {
			screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(self.size[0] as f32, self.size[1] as f32))),
			time: Some(self.started.elapsed().as_secs_f64()),
			max_texture_side: Some(self.texture_side),
			events: self.input_events(),
			..Default::default()
		};

		let mut out = self.ctx.run_ui(raw, |ui| self.app.frame(ui));
		self.closed |= Self::asked_to_close(&out);
		self.ime = out.platform_output.ime;
		for command in &out.platform_output.commands {
			if let egui::OutputCommand::CopyText(text) = command {
				self.copied = Some(text.clone());
			}
		}
		let primitives = self.ctx.tessellate(out.shapes, out.pixels_per_point);
		let image = self.render(&primitives);

		for (id, deltas) in &out.textures_delta.set {
			for delta in deltas {
				self.renderer.update_texture(&self.device, &self.queue, *id, delta);
			}
		}
		out.textures_delta.clear();
		self.frames += 1;

		image
	}
}

/// The replay steps, from the environment — the same corpus every acceptance run has used.
fn seed() -> Vec<String> {
	std::env::var("CRABHOP_KEYS")
		.map(|keys| keys.split(';').map(|step| step.trim().to_owned()).filter(|step| !step.is_empty()).collect())
		.unwrap_or_default()
}

/// A device without a surface: RADV answers, and nothing needs a window or a compositor.
pub(crate) fn device() -> (wgpu::Device, wgpu::Queue) {
	let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
		backends: wgpu::Backends::all(),
		flags: wgpu::InstanceFlags::default(),
		memory_budget_thresholds: Default::default(),
		backend_options: Default::default(),
		display: None,
	});

	let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
		compatible_surface: None,
		..Default::default()
	}))
	.expect("no adapter without a surface");

	if crate::metrics::enabled() {
		let info = adapter.get_info();
		eprintln!("Timing: harness adapter {} / {} / {:?}", info.name, info.driver, info.backend);
	}

	block_on(adapter.request_device(&wgpu::DeviceDescriptor {
		label: Some("crabhop-harness"),
		required_features: wgpu::Features::empty(),
		required_limits: wgpu::Limits::default(),
		..Default::default()
	}))
	.expect("no device")
}

/// The async calls, driven on this thread: the runtime is the crate's own dependency already.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
	tokio::runtime::Builder::new_current_thread()
		.enable_all()
		.build()
		.expect("a runtime for the device")
		.block_on(future)
}

/// How tall the middle column's rows are, for a test that reads a cursor's move out of pixels.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) const fn row_height() -> f32 { ROW_HEIGHT }

#[cfg(test)]
mod tests {
	use crate::chrome::{self, STATUS_HEIGHT};

	use super::*;

	/// One headless window for a test: a device, the fonts, the app and a size that shows all
	/// three columns.
	fn session(seed: Vec<String>) -> Session {
		// the window's own config is nobody's fixture: every session runs on the default shares and
		// writes whatever a drag makes into a directory of its own (the person's own file stays out
		// of the way of the tests, and the tests stay out of the way of it)
		crate::config::config_dir_for_tests(
			std::env::temp_dir().join(format!("crabhop-config-{}", std::process::id())).join("crabhop"),
		);

		let (device, queue) = device();
		let ctx = egui::Context::default();
		Tokens::install(&ctx);

		Session::new(ctx, device, queue, wgpu::TextureFormat::Rgba8Unorm, [ 1200.0, 800.0 ], seed)
	}

	/// How many pixels of a band are anything but the window's flat background.
	fn lit(image: &egui::ColorImage, rect: egui::Rect) -> usize {
		let mut count = 0;
		for y in (rect.min.y.max(0.0) as usize)..(rect.max.y.min(image.size[1] as f32) as usize) {
			for x in (rect.min.x.max(0.0) as usize)..(rect.max.x.min(image.size[0] as f32) as usize) {
				let p = image.pixels[y * image.size[0] + x];
				if p.r() > 40 || p.g() > 40 || p.b() > 60 {
					count += 1;
				}
			}
		}
		count
	}

	/// The top of the row band the cursor stands on, read one pixel in from the middle column's
	/// left edge — past the two-pixel accent mark, which is not the only accent in the frame.
	fn cursor_row(image: &egui::ColorImage, x: usize) -> Option<usize> {
		let cursor_bg = Tokens::MOCHA.cursor_bg;
		(40..image.size[1] - 40).find(|y| image.pixels[y * image.size[0] + x] == cursor_bg)
	}

	/// The global four land where the table says (P21): with the list holding the keyboard Ctrl+A
	/// marks every row on show; inside a field Ctrl+A is the text's own select-all, so what is
	/// typed next starts the field over.
	#[test]
	fn the_global_ctrl_a_is_the_list_or_the_field_that_has_the_keyboard() {
		// the listing first, and only then the key: a replay's own cadence is not something a test
		// may lean on (the flake the P21 gate caught)
		let mut list = session(Vec::new());
		assert!(list.wait_for_list(2, 200), "the listing has to land");
		list.app.rehearse(vec![ "key C-a".to_owned() ]);
		for _ in 0..80 {
			std::thread::sleep(Duration::from_millis(16));
			list.shoot();
			if list.app.selected_count() > 0 {
				break;
			}
		}

		assert!(list.app.listed() > 1, "the test needs a listing to select");
		assert_eq!(list.app.selected_count(), list.app.listed(), "every row on show is selected");

		// the field's own four steps, waited out one by one rather than by a frame count
		let mut field = session(Vec::new());
		field.app.rehearse(vec![
			"key f".to_owned(),
			"type ab".to_owned(),
			"key C-a".to_owned(),
			"type c".to_owned(),
		]);
		for _ in 0..160 {
			std::thread::sleep(Duration::from_millis(16));
			field.shoot();
			if field.app.filter_text().as_deref() == Some("c") {
				break;
			}
		}

		assert_eq!(
			field.app.filter_text().as_deref(),
			Some("c"),
			"the field's own select-all was replaced by what was typed next"
		);
	}

	/// A picture's display ratio moves with a zoom (P27①'s rule stands): the fit reads the
	/// column's width against the file's own — 133% for a 320px file in this column — and a step
	/// puts the same ratio times 1.25 on the badge, the very string the panel shows a person and
	/// the log carries a replay.
	#[test]
	fn a_zoom_moves_the_display_ratio_the_badge_shows() {
		let at = std::env::temp_dir().join(format!("crabhop-zoom-{}.png", std::process::id()));
		image::RgbaImage::from_pixel(320, 240, image::Rgba([ 40, 160, 60, 255 ]))
			.save(&at)
			.expect("the fixture encodes");

		let mut session = session(vec![ format!("reveal {}", at.display()), "hover 900 400".to_owned() ]);

		// the fit's own label has to be up before the zoom means anything
		let mut fit: Option<String> = None;
		for _ in 0..240 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
			if let Some(label) = session.app.preview_badge_for_tests() {
				fit = Some(label);
				break;
			}
		}
		let fit = fit.expect("the picture has to be on screen");
		// the ratio the fit must read: the column's own content width against the file's 320, from
		// the layout the session actually got — never from a number typed into the test
		let (body, shares) = session.app.layout();
		let content = crate::preview::content_of(chrome::columns(body, shares).preview.expect("three columns"));
		let percent = |scale: f32| ((content.width() * scale / 320.0 * 100.0).round().max(1.0)) as u32;
		assert_eq!(fit, format!("{}% ⤢", percent(1.0)), "the fit is the column's width against the file's");

		session.app.rehearse(vec![ "hover 900 400".to_owned(), "zoom in".to_owned() ]);
		let mut zoomed: Option<String> = None;
		for _ in 0..240 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
			if let Some(label) = session.app.preview_badge_for_tests() {
				if label.contains('×') {
					zoomed = Some(label);
					break;
				}
			}
		}
		let zoomed = zoomed.expect("the zoom has to land");
		assert_eq!(zoomed, format!("{}% ×1.25", percent(1.25)), "a step multiplies the ratio the badge carries");

		let _ = std::fs::remove_file(&at);
	}

	/// `g t` opens the bin as a tab of its own (P27): the chip says 回收站, the keymap keeps its say
	/// inside it — Escape no longer closes it — and a `leave` walks away from it, the tab staying.
	#[test]
	fn the_bin_is_a_tab_the_keymap_still_works_in() {
		let mut session = session(Vec::new());
		assert!(session.wait_for_list(2, 200), "the listing has to land");

		session.app.rehearse(vec![ "key g".to_owned(), "key t".to_owned() ]);
		for _ in 0..120 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
			if session.app.in_bin_for_tests() {
				break;
			}
		}

		assert!(session.app.in_bin_for_tests(), "`g t` opens the bin");
		assert_eq!(session.app.tabs_for_tests(), 2, "as a tab of its own");
		assert_eq!(session.app.tab_name_for_tests(1), "回收站", "the chip is the bin's own word");
		assert!(session.app.bin_cursor_for_tests().is_some(), "and the preview column knows what stands");

		// Escape no longer means "close" (P27): the tab is still the bin's own
		session.app.rehearse(vec![ "key Esc".to_owned() ]);
		for _ in 0..40 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
		}
		assert!(session.app.in_bin_for_tests(), "Escape is not the bin's own anymore");

		// a `leave` walks away: the tab stays where it was, the bin is behind it
		session.app.rehearse(vec![ "key h".to_owned() ]);
		for _ in 0..120 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
			if !session.app.in_bin_for_tests() {
				break;
			}
		}

		assert!(!session.app.in_bin_for_tests(), "`h` leaves the bin behind");
		assert_eq!(session.app.tabs_for_tests(), 2, "and the tab it lived in is still there");
	}

	/// P33②: with the bin standing as a tab of its own, another chip's × still closes that chip.
	/// The mouse path is walked the way a hand walks it — the pointer moves onto the chip, hover
	/// settles a frame later (that is when the × exists at all), and then the press lands. The
	/// boxes come from the frame that drew them; the state is what the test reads back.
	#[test]
	fn the_other_chips_x_still_closes_while_the_bin_stands() {
		let mut s = session(Vec::new());
		assert!(s.wait_for_list(2, 200), "the listing has to land");

		s.app.rehearse(vec![ "key g".to_owned(), "key t".to_owned() ]);
		for _ in 0..120 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if s.app.in_bin_for_tests() {
				break;
			}
		}
		assert!(s.app.in_bin_for_tests(), "`g t` opened the bin tab");
		assert_eq!(s.app.tabs_for_tests(), 2, "two chips stand");

		// the pointer walks onto the first chip (the directory's own — the bin is the active one)
		let chip = s.app.tab_chip_hit_for_tests(0).expect("the frame drew the first chip");
		s.move_at(egui::pos2(chip.max.x - 8.0, chip.center().y));
		s.shoot();
		s.shoot();
		let close = s.app.tab_close_hit_for_tests(0).expect("the hovered chip grew its ×");

		s.click_at(close.center());
		for _ in 0..8 {
			s.shoot();
		}

		assert_eq!(s.app.tabs_for_tests(), 1, "the × closed the first chip");
		assert!(s.app.in_bin_for_tests(), "and the bin's tab is the one left standing");
	}

	/// The same × answers on a chip the window is not standing on, with no bin in sight (P33②): the
	/// handler used to sit behind the frame's own hover, which collapses onto the very widget the
	/// click is delivered to — an inactive chip's × could never read its own click.
	#[test]
	fn an_inactive_chips_x_closes_it_without_a_bin_too() {
		let mut s = session(Vec::new());
		s.shoot();

		s.app.rehearse(vec![ "key t".to_owned(), "key t".to_owned() ]);
		for _ in 0..80 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if s.app.tabs_for_tests() >= 2 {
				break;
			}
		}
		assert_eq!(s.app.tabs_for_tests(), 2, "t t made the second tab");

		// the pointer walks onto the first chip, hover settles, and the press lands on its ×
		let chip = s.app.tab_chip_hit_for_tests(0).expect("the frame drew the first chip");
		s.move_at(egui::pos2(chip.max.x - 8.0, chip.center().y));
		s.shoot();
		s.shoot();
		let close = s.app.tab_close_hit_for_tests(0).expect("the hovered chip grew its ×");

		s.click_at(close.center());
		for _ in 0..8 {
			s.shoot();
		}

		assert_eq!(s.app.tabs_for_tests(), 1, "the inactive chip's × closed it");
	}

	/// P33⑤: while one of the window's own fields holds the keyboard, every frame anchors the
	/// platform's IME cursor area at that field — its box and the caret's column — where egui
	/// only knows its own `TextEdit`. The filter strip proves the plain field, and the index
	/// panel both halves of its machine: the list holds the keyboard and nothing anchors, the
	/// box takes it and the anchor appears.
	#[test]
	fn a_focused_field_anchors_the_ime_cursor_area() {
		let mut s = session(Vec::new());
		s.shoot();
		assert!(s.ime_for_tests().is_none(), "no field holds the keyboard at rest");

		// `f` opens the filter strip at the window's foot: the anchor lands at its own box
		s.app.rehearse(vec![ "key f".to_owned() ]);
		let mut anchored = None;
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if let Some(ime) = s.ime_for_tests() {
				anchored = Some(ime);
				break;
			}
		}
		let ime = anchored.expect("the filter strip takes the keyboard and anchors the IME");
		let body = s.app.layout().0;
		assert!(ime.rect.min.y > body.max.y - 40.0, "the anchor stands at the strip's own box: {:?}", ime.rect);
		assert!(ime.rect.min.x >= 0.0 && ime.rect.width() > 100.0, "the box is the field's, not a point: {:?}", ime.rect);
		assert!(
			ime.cursor_rect.min.x >= ime.rect.min.x && ime.cursor_rect.max.x <= ime.rect.max.x + 2.0,
			"and the caret is its own thin column inside the box: {:?} / {:?}",
			ime.cursor_rect,
			ime.rect
		);

		// Escape puts the strip away, and the anchor goes with it
		s.app.rehearse(vec![ "key Esc".to_owned() ]);
		for _ in 0..40 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if s.ime_for_tests().is_none() {
				break;
			}
		}
		assert!(s.ime_for_tests().is_none(), "no field, no anchor");

		// the index opens on the list: the list holds the keyboard, nothing anchors
		s.app.rehearse(vec![ "key i".to_owned() ]);
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if s.app.index_open() {
				break;
			}
		}
		assert!(s.app.index_open(), "i opened the index");
		for _ in 0..4 {
			s.shoot();
		}
		assert!(s.ime_for_tests().is_none(), "with the list holding the keyboard, no anchor stands");

		// a second `i` steps into the box: the anchor appears at the query line
		s.app.rehearse(vec![ "key i".to_owned() ]);
		let mut inside = None;
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if let Some(ime) = s.ime_for_tests() {
				inside = Some(ime);
				break;
			}
		}
		assert_eq!(s.app.fuzzy_focus_for_tests(), Some(true), "the box holds the keyboard");
		let ime = inside.expect("and the IME has its anchor at the query line");
		assert!(ime.rect.min.y < body.max.y, "the anchor is the panel's query line, above the strip: {:?}", ime.rect);
	}

	/// P33③: the bin's rows are placed by their place in the viewport, not by their place in the
	/// bin — the two only agree at the top, and a scrolled bin used to draw every row `first`
	/// rows too low: the viewport's top row went bare at once, and a deep scroll left the column
	/// empty altogether. The wheel walks the list to the last page and back, and every row band
	/// that should hold a row holds ink — at the top, at the bottom, and home again.
	#[test]
	fn the_bins_rows_keep_their_places_bottom_and_back() {
		let mut s = session(Vec::new());
		assert!(s.wait_for_list(2, 200), "the listing has to land");

		s.app.rehearse(vec![ "key g".to_owned(), "key t".to_owned() ]);
		for _ in 0..120 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if s.app.in_bin_for_tests() {
				break;
			}
		}
		assert!(s.app.in_bin_for_tests(), "`g t` opened the bin tab");

		// a bin of the test's own (P33③'s seam): sixty rows against the twenty-two the column shows
		s.app.bin_fill_for_tests(60);
		s.shoot();
		s.shoot();

		// every band that should hold a row holds ink: the icon slot and the name start inside it.
		// The column's own count: what the last frame made of the viewport
		let bands_with_ink = |s: &mut Session| -> (usize, usize) {
			let image = s.shoot();
			let (body, shares) = s.app.layout();
			let current = chrome::columns(body, shares).current;
			let (_, _, rows, _) = s.app.bin_scroll_for_tests().expect("the bin's scroll");

			let mut lit = 0;
			for i in 0..rows {
				let y0 = current.min.y + i as f32 * ROW_HEIGHT;
				let lit_row = ((y0 as usize)..((y0 + ROW_HEIGHT) as usize).min(image.size[1])).any(|y| {
					((current.min.x as usize)..(current.min.x as usize + 140).min(image.size[0])).any(|x| {
						let p = image.pixels[y * image.size[0] + x];
						p.r() as u32 + p.g() as u32 + p.b() as u32 > 300
					})
				});
				if lit_row {
					lit += 1;
				}
			}
			(lit, rows)
		};

		let wait_settled = |s: &mut Session, want: f32| {
			for _ in 0..240 {
				if let Some((offset, _, _, _)) = s.app.bin_scroll_for_tests()
					&& (offset - want).abs() < 0.001
				{
					return;
				}
				std::thread::sleep(Duration::from_millis(16));
				s.shoot();
			}
		};

		// the top: every band is a row
		let (lit, rows) = bands_with_ink(&mut s);
		assert_eq!(lit, rows, "at the top, every band holds a row");

		// the wheel walks to the last page — the offset follows its target now, and the deep
		// view still shows rows: this is where the whole column used to be blank
		s.app.rehearse(vec![ "wheel -30".to_owned(), "wheel -30".to_owned(), "wheel -30".to_owned() ]);
		wait_settled(&mut s, (60 - rows) as f32);
		let (offset, target, _, len) = s.app.bin_scroll_for_tests().expect("the bin's scroll");
		assert!((offset - (60 - rows) as f32).abs() < 0.001, "the offset walked to the last page: {offset}");
		assert_eq!((target, len), ((60 - rows) as f32, 60), "and the last page is len - rows");
		let (lit, rows) = bands_with_ink(&mut s);
		assert_eq!(lit, rows, "at the bottom, every band still holds a row");

		// and home again: the same count of rows, from the same place
		s.app.rehearse(vec![ "wheel 30".to_owned(), "wheel 30".to_owned(), "wheel 30".to_owned() ]);
		wait_settled(&mut s, 0.0);
		let (offset, _, rows_after, len) = s.app.bin_scroll_for_tests().expect("the bin's scroll");
		assert_eq!((offset, len), (0.0, 60), "the wheel walks back to the head");
		assert_eq!(rows_after, rows, "and the viewport's own count is not lost");
		let (lit, rows) = bands_with_ink(&mut s);
		assert_eq!(lit, rows, "and every band holds a row again");
	}

	/// P33⑧: the left column answers the pointer like the middle one does — a click seats its own
/// cursor, the wheel walks its own scroll — and the keyboard's column (the middle one) is not
/// what decides any of it: the pointer's own column is. Both are read back as state, and the
/// highlight's move is read off the pixels.
#[test]
fn the_parent_column_answers_the_mouse() {
	// a sandbox: forty entries and the directory the test stands in
	let root = std::env::temp_dir().join(format!("crabhop-parent-{}", std::process::id()));
	let _ = std::fs::remove_dir_all(&root);
	std::fs::create_dir_all(root.join("inside")).expect("a sandbox");
	for n in 0..40 {
		std::fs::write(root.join(format!("file-{n:02}.txt")), b"x").expect("a file");
	}
	std::fs::write(root.join("inside/file-00.txt"), b"x").expect("a file to stand on");

	let mut s = session(vec![ format!("reveal {}", root.join("inside/file-00.txt").display()) ]);
	for _ in 0..120 {
		std::thread::sleep(Duration::from_millis(16));
		s.shoot();
		if s.app.cwd_for_tests() == root.join("inside") {
			break;
		}
	}
	assert_eq!(s.app.cwd_for_tests(), root.join("inside"), "the reveal stands inside the sandbox: {:?}", s.app.cwd_for_tests());

	// the column has to have drawn its own reading: the sandbox's listing
	let (mut rows, mut len) = (0usize, 0usize);
	for _ in 0..120 {
		let (_, _, drawn_rows, listed) = s.app.parent_mouse_for_tests();
		if listed == 41 {
			(rows, len) = (drawn_rows, listed);
			break;
		}
		std::thread::sleep(Duration::from_millis(16));
		s.shoot();
	}
	assert_eq!(len, 41, "the parent column holds the sandbox: forty files and `inside`");
	assert!(len > rows, "and the listing is longer than the column: {len} > {rows}");

	// the pointer over the left column: the wheel walks the left column's own scroll — settled
	// to the last page — and the middle column's list never hears it
	let (body, shares) = s.app.layout();
	let parent = chrome::columns(body, shares).parent.expect("all three columns at this width");
	s.app.rehearse(vec![ format!("hover {} {}", parent.center().x, parent.min.y + 60.0), "wheel -60".to_owned() ]);
	let mut offset = 0.0;
	for _ in 0..240 {
		std::thread::sleep(Duration::from_millis(16));
		s.shoot();
		offset = s.app.parent_mouse_for_tests().1;
		if (offset - (len - rows) as f32).abs() < 0.001 {
			break;
		}
	}
	assert!((offset - (len - rows) as f32).abs() < 0.001, "the left column's scroll settled at the last page: {offset}");
	assert_eq!(s.app.scroll_offset_for_tests(), 0, "the middle column's list did not move");

	// and back to the head: the column's own reading — the cwd's row — stands highlighted there
	s.app.rehearse(vec![ "wheel 60".to_owned(), "wheel 60".to_owned() ]);
	for _ in 0..240 {
		std::thread::sleep(Duration::from_millis(16));
		s.shoot();
		if s.app.parent_mouse_for_tests().1 < 0.001 {
			break;
		}
	}
	assert!(s.app.parent_mouse_for_tests().1 < 0.001, "the wheel walks back to the head too");

	// a click seats the left column's own cursor on the row it landed on — the middle column's
	// cursor and scroll are still its own
	let before_middle = s.app.cursor_for_tests();
	let image = s.shoot();
	let cursor_x = parent.min.x as usize + 6;
	let highlight_at = cursor_row(&image, cursor_x).expect("the left column drew its cursor row");

	s.move_at(egui::pos2(parent.center().x, parent.min.y + 40.0));
	s.shoot();
	s.click_at(egui::pos2(parent.center().x, parent.min.y + 40.0));
	for _ in 0..8 {
		s.shoot();
	}

	let (seat, _, _, _) = s.app.parent_mouse_for_tests();
	assert_eq!(seat, Some(1), "the click seated the second row of the view");
	assert_eq!(s.app.cursor_for_tests(), before_middle, "the middle column's cursor never moved");
	assert_eq!(s.app.scroll_offset_for_tests(), 0, "and neither did its scroll");

	let image = s.shoot();
	let moved = cursor_row(&image, cursor_x).expect("the left column still draws its cursor row");
	assert!(
		(moved - highlight_at) as f32 >= row_height() * 0.8,
		"the highlight walked down a row: {highlight_at} → {moved}"
	);

	let _ = std::fs::remove_dir_all(&root);
}

/// P33⑩: `Space` toggles the row the cursor stands on in and out of the selection; `j`/`k` walk
/// without touching what is selected; the status bar wears the SELECT badge while anything
/// stands selected, and Escape clears it. The selection is read off the state and the badge off
/// the pixels (its fill is the `warn` block, where NORMAL's is `accent`).
#[test]
fn space_toggles_the_selection_and_escape_clears_it() {
	let mut s = session(Vec::new());
	assert!(s.wait_for_list(2, 200), "the listing has to land");

	// the badge's own block, in the status bar: `warn` before anything, `accent` after
	let warn_in_badge = |s: &mut Session| -> usize {
		let image = s.shoot();
		let mut count = 0;
		for y in 774..798 {
			for x in 12..80 {
				if image.pixels[y * image.size[0] + x] == Tokens::MOCHA.warn {
					count += 1;
				}
			}
		}
		count
	};
	let walk = |s: &mut Session, keys: &[&str]| {
		s.app.rehearse(keys.iter().map(|key| (*key).to_owned()).collect());
		for _ in 0..30 * keys.len() {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
		}
	};

	assert_eq!(s.app.selection_for_tests().len(), 0, "nothing is selected at rest");
	assert_eq!(warn_in_badge(&mut s), 0, "and the badge is NORMAL's own");

	// ① Space seats the cursor's row in; a second Space takes it out again
	walk(&mut s, &[ "key Space" ]);
	assert_eq!(s.app.selection_for_tests().len(), 1, "Space selected the cursor's row");
	assert!(warn_in_badge(&mut s) > 0, "and the badge turned SELECT");

	walk(&mut s, &[ "key Space" ]);
	assert_eq!(s.app.selection_for_tests().len(), 0, "Space again took it out");

	// ② three rows in a row, walking with `j` in between — and `k` back loses nothing
	walk(&mut s, &[ "key Space", "key j", "key Space", "key j", "key Space" ]);
	assert_eq!(s.cursor(), 2, "the cursor walked with the selections");
	assert_eq!(s.app.selection_for_tests().len(), 3, "three rows stand selected");
	walk(&mut s, &[ "key k", "key k" ]);
	assert_eq!(s.cursor(), 0, "the cursor walked back up");
	assert_eq!(s.app.selection_for_tests().len(), 3, "and the selection was not lost on the way");

	// ③ Escape clears it, and the badge goes back to NORMAL
	walk(&mut s, &[ "key Esc" ]);
	assert_eq!(s.app.selection_for_tests().len(), 0, "Escape cleared the selection");
	assert_eq!(warn_in_badge(&mut s), 0, "and the badge is NORMAL's own again");
}

/// P33⑪: the global Ctrl names — no new bindings — are selection-aware: with anything selected
/// they act on the whole set, with nothing selected on the cursor's row. `Ctrl+C` / `Ctrl+X`
/// fill the yank (copy or cut), `Ctrl+V` pastes the held set into the window's own directory,
/// and a held cut moves the bytes rather than copying them.
#[test]
fn the_global_ctrl_names_act_on_the_selection_or_the_cursor_row() {
	let root = std::env::temp_dir().join(format!("crabhop-ctrl-{}", std::process::id()));
	let _ = std::fs::remove_dir_all(&root);
	std::fs::create_dir_all(root.join("dst")).expect("a sandbox");
	std::fs::write(root.join("a.txt"), b"a").expect("a file");
	std::fs::write(root.join("b.txt"), b"b").expect("a file");
	std::fs::write(root.join("dst/seed.txt"), b"s").expect("a file to stand on");

	let walk = |s: &mut Session, keys: &[&str]| {
		s.app.rehearse(keys.iter().map(|key| (*key).to_owned()).collect());
		for _ in 0..30 * keys.len() {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
		}
	};

	let mut s = session(vec![ format!("reveal {}", root.join("a.txt").display()) ]);
	for _ in 0..120 {
		std::thread::sleep(Duration::from_millis(16));
		s.shoot();
		if s.app.cwd_for_tests() == root {
			break;
		}
	}
	assert_eq!(s.app.cwd_for_tests(), root, "the window stands in the sandbox");

	// nothing selected: `Ctrl+C` takes the cursor's own row
	walk(&mut s, &[ "key C-c" ]);
	assert_eq!(s.app.yank_paths_for_tests(), vec![ root.join("a.txt") ], "the cursor's row alone");
	assert!(!s.app.yank_cut_for_tests(), "a copy, not a cut");

	// two rows selected: `Ctrl+X` takes the whole set, as a cut
	walk(&mut s, &[ "key Space", "key j", "key Space" ]);
	assert_eq!(s.app.selection_for_tests(), vec![ root.join("a.txt"), root.join("b.txt") ], "two rows stand selected");
	walk(&mut s, &[ "key C-x" ]);
	assert_eq!(
		s.app.yank_paths_for_tests(),
		vec![ root.join("a.txt"), root.join("b.txt") ],
		"the whole selection is held, the cursor's row included"
	);
	assert!(s.app.yank_cut_for_tests(), "and it is a cut");

	// and `Ctrl+V` in another directory pastes the held set: a cut moves the bytes
	walk(&mut s, &[ "key Esc" ]);
	walk(&mut s, &[ &format!("reveal {}", root.join("dst/seed.txt").display()) ]);
	for _ in 0..120 {
		std::thread::sleep(Duration::from_millis(16));
		s.shoot();
		if s.app.cwd_for_tests() == root.join("dst") {
			break;
		}
	}
	assert_eq!(s.app.cwd_for_tests(), root.join("dst"), "the window stands in the destination");
	walk(&mut s, &[ "key C-v" ]);
	for _ in 0..240 {
		std::thread::sleep(Duration::from_millis(16));
		s.shoot();
		if root.join("dst/a.txt").exists() && root.join("dst/b.txt").exists() {
			break;
		}
	}
	assert!(root.join("dst/a.txt").exists() && root.join("dst/b.txt").exists(), "the held set landed");
	assert!(!root.join("a.txt").exists() && !root.join("b.txt").exists(), "a cut takes the originals with it");

	let _ = std::fs::remove_dir_all(&root);
}

/// P33⑫: the change-type tasks leave an undo behind — the same action on a ten-deep stack and
/// on the toast's own button (`u` and `Ctrl+Z` take the stack's newest) — and the semantics
/// table holds: a link is taken away, a rename goes back, a cut-paste walks home. Nothing on
/// the stack is nothing said.
#[test]
fn the_undo_takes_the_change_type_tasks_back() {
	let root = std::env::temp_dir().join(format!("crabhop-undo-{}", std::process::id()));
	let _ = std::fs::remove_dir_all(&root);
	std::fs::create_dir_all(root.join("sub")).expect("a sandbox");
	std::fs::write(root.join("sub/a.txt"), b"a").expect("a file");

	let walk = |s: &mut Session, keys: &[&str]| {
		s.app.rehearse(keys.iter().map(|key| (*key).to_owned()).collect());
		for _ in 0..3000 {
			if s.app.replay_done_for_tests() {
				break;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		for _ in 0..2 {
			s.shoot();
		}
	};
	fn settle(s: &mut Session, check: impl Fn() -> bool) -> bool {
		for _ in 0..600 {
			if check() {
				return true;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		check()
	}
	let stands_at = |s: &mut Session, want: &std::path::Path| -> bool {
		for _ in 0..600 {
			if s.app.cwd_for_tests() == want {
				return true;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		false
	};

	let mut s = session(vec![ format!("reveal {}", root.join("sub/a.txt").display()) ]);
	assert!(stands_at(&mut s, &root.join("sub")), "the window stands in the sandbox's subdirectory");
	assert!(s.app.undo_labels_for_tests().is_empty(), "and the stack is empty at rest");

	// ① a link: `L` opens the panel, `)` chooses the absolute one, Enter takes it — the prompt
	// comes prefilled with the target, and its own Enter makes the link
	walk(&mut s, &[ "key L", "key )", "key Enter", "key Enter" ]);
	assert!(settle(&mut s, || root.join("a.txt").symlink_metadata().is_ok()), "the link is there");
	assert!(root.join("a.txt").symlink_metadata().expect("it").file_type().is_symlink(), "a link, not a copy");
	assert_eq!(std::fs::read_link(root.join("a.txt")).expect("it reads"), root.join("sub/a.txt"), "pointing at the selected file");
	assert_eq!(s.app.undo_labels_for_tests(), vec![ "link" ], "the link joined the stack");
	assert!(s.app.toast_undo_for_tests(), "and its toast carries the button: {:?}", s.app.toast_text_for_tests());

	walk(&mut s, &[ "key u" ]);
	assert!(settle(&mut s, || !root.join("a.txt").symlink_metadata().is_ok()), "the undo took the link away");
	assert!(root.join("sub/a.txt").exists(), "and the file it pointed at is untouched");
	assert!(s.app.undo_labels_for_tests().is_empty(), "the stack is empty again");

	// nothing to take back is nothing said
	let before = s.app.toast_text_for_tests();
	walk(&mut s, &[ "key C-z" ]);
	assert!(s.app.undo_labels_for_tests().is_empty(), "the stack stays empty");
	assert_eq!(s.app.toast_text_for_tests(), before, "and nothing new is said");

	// ② a rename: `r` opens the field, Ctrl+A its text, the new name is typed and Enter lands it
	walk(&mut s, &[ "key r", "key C-a", "type b.txt", "key Enter" ]);
	assert!(settle(&mut s, || root.join("sub/b.txt").exists() && !root.join("sub/a.txt").exists()), "the rename landed");
	assert_eq!(s.app.undo_labels_for_tests(), vec![ "rename" ], "the rename joined the stack");
	assert!(s.app.toast_undo_for_tests(), "with its button");

	walk(&mut s, &[ "key C-z" ]);
	assert!(settle(&mut s, || root.join("sub/a.txt").exists() && !root.join("sub/b.txt").exists()), "Ctrl+Z renamed it back");
	assert!(s.app.undo_labels_for_tests().is_empty(), "the stack is empty again");

	// ③ a cut-paste: the selection is held, the cut taken, `h` walks up, and the paste moves it
	// — the undo walks the moved set home (搬回)
	walk(&mut s, &[ "key Space", "key C-x" ]);
	assert_eq!(s.app.selection_for_tests(), vec![ root.join("sub/a.txt") ], "the row is selected for the cut");
	walk(&mut s, &[ "key h" ]);
	assert!(stands_at(&mut s, &root), "the window walked up to the sandbox");
	walk(&mut s, &[ "key C-v" ]);
	assert!(settle(&mut s, || root.join("a.txt").exists() && !root.join("sub/a.txt").exists()), "the cut moved the file");
	assert_eq!(s.app.undo_labels_for_tests(), vec![ "moved" ], "the move joined the stack");

	walk(&mut s, &[ "key u" ]);
	assert!(settle(&mut s, || root.join("sub/a.txt").exists() && !root.join("a.txt").exists()), "the undo walked it home");
	assert!(s.app.undo_labels_for_tests().is_empty(), "and the stack is empty again");

	// ④ the stack is ten deep: the eleventh change pushes the oldest out
	s.app.note_for_tests(crate::toast::Undo::BookmarkSave("old".to_owned()));
	for _ in 0..10 {
		s.app.note_for_tests(crate::toast::Undo::Link(Vec::new()));
	}
	let labels = s.app.undo_labels_for_tests();
	assert_eq!(labels.len(), 10, "ten changes and no more");
	assert_eq!(labels.first(), Some(&"link"), "the oldest went first");

	let _ = std::fs::remove_dir_all(&root);
}

/// P35①: a click on the parent column's directory row *enters* that directory — the middle
/// column becomes its content, the user's own ruling (yazi's parent click navigates). The cwd's
/// own row only seats (there is nowhere to go), a file row only seats, and the wheel keeps its
/// own column (P33⑧'s test holds that half).
#[test]
fn a_click_on_the_parent_columns_directory_enters_it() {
	// sandbox: root/{inside, sibling}/ and a loose file, standing in root/inside
	let root = std::env::temp_dir().join(format!("crabhop-parent-go-{}", std::process::id()));
	let _ = std::fs::remove_dir_all(&root);
	std::fs::create_dir_all(root.join("inside")).expect("a sandbox");
	std::fs::create_dir_all(root.join("sibling")).expect("a sibling");
	std::fs::write(root.join("inside/seat.txt"), b"s").expect("a file");
	std::fs::write(root.join("sibling/mark.txt"), b"m").expect("a file");
	std::fs::write(root.join("loose.txt"), b"l").expect("a loose file");

	let mut s = session(vec![ format!("reveal {}", root.join("inside/seat.txt").display()) ]);
	let stands_at = |s: &mut Session, want: &std::path::Path| -> bool {
		for _ in 0..600 {
			if s.app.cwd_for_tests() == want {
				return true;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		false
	};
	assert!(stands_at(&mut s, &root.join("inside")), "the window stands in the sandbox's inside");
	let mut parent = None;
	for _ in 0..600 {
		let (_, _, _, len) = s.app.parent_mouse_for_tests();
		if len == 3 {
			let (body, shares) = s.app.layout();
			parent = chrome::columns(body, shares).parent;
			break;
		}
		std::thread::sleep(Duration::from_millis(4));
		s.shoot();
	}
	let parent = parent.expect("the parent column drew the sandbox: inside, sibling, loose");

	// the second row is `sibling`: the click enters it — `go`'s own door
	s.move_at(egui::pos2(parent.center().x, parent.min.y + 40.0));
	s.shoot();
	s.click_at(egui::pos2(parent.center().x, parent.min.y + 40.0));
	assert!(stands_at(&mut s, &root.join("sibling")), "the click entered the sibling directory");
	let mut listed = 0;
	for _ in 0..600 {
		listed = s.app.listed();
		if listed == 1 {
			break;
		}
		std::thread::sleep(Duration::from_millis(4));
		s.shoot();
	}
	assert_eq!(listed, 1, "and the middle column shows the sibling's own content");

	// the cwd's own row (now `sibling`, still the second row) only seats
	s.move_at(egui::pos2(parent.center().x, parent.min.y + 40.0));
	s.shoot();
	s.click_at(egui::pos2(parent.center().x, parent.min.y + 40.0));
	for _ in 0..8 {
		s.shoot();
	}
	assert_eq!(s.app.cwd_for_tests(), root.join("sibling"), "the cwd's own row goes nowhere");

	// and a file row only seats too: `loose.txt` is the third row
	s.move_at(egui::pos2(parent.center().x, parent.min.y + 70.0));
	s.shoot();
	s.click_at(egui::pos2(parent.center().x, parent.min.y + 70.0));
	for _ in 0..8 {
		s.shoot();
	}
	assert_eq!(s.app.cwd_for_tests(), root.join("sibling"), "a file row is not entered");
	assert_eq!(s.app.parent_mouse_for_tests().0, Some(2), "the click still seated the column's cursor");

	let _ = std::fs::remove_dir_all(&root);
}

/// P35②: the user's own finding — Ctrl+C/X/Z/A answered nothing in the real window. The root
/// cause is the window's own plumbing: egui-winit answers the clipboard chords with
/// `Event::Copy` / `Event::Cut` / `Event::Paste` and never makes a key event of them, so a list
/// that only reads keys is deaf to them (a replay synthesizes keys, which is why this stayed
/// green headless). The keys' own spelling still works beside them. The panel matrix walks
/// every panel the factory opens, closes it with Escape, and proves the real clipboard event
/// still lands on the list — which is also the focus-cleanup dipstick.
#[test]
fn the_clipboard_chords_are_heard_in_the_windows_own_events() {
	// a sandbox with room under the cursor: the matrix walks it a row per panel, and a listing
	// of five would saturate `j` and make the walk a false failure
	let root = std::env::temp_dir().join(format!("crabhop-chords-{}", std::process::id()));
	let _ = std::fs::remove_dir_all(&root);
	std::fs::create_dir_all(&root).expect("a sandbox");
	for n in 0..12 {
		std::fs::write(root.join(format!("file-{n:02}.txt")), b"x").expect("a file");
	}

	let mut s = session(vec![ format!("reveal {}", root.join("file-00.txt").display()) ]);
	for _ in 0..600 {
		if s.app.listed() >= 12 {
			break;
		}
		std::thread::sleep(Duration::from_millis(4));
		s.shoot();
	}
	assert!(s.app.listed() >= 12, "the sandbox listing has to land: {} rows", s.app.listed());

	// the real window's spelling, on the list: Paste with nothing held reaches the paste door
	s.inject(vec![ egui::Event::Paste("剪贴板的话".to_owned()) ]);
	s.shoot();
	assert_eq!(
		s.app.toast_text_for_tests().as_deref(),
		Some("先复制或剪切（y / x）"),
		"a real Ctrl+V reaches the paste's own door"
	);

	s.inject(vec![ egui::Event::Copy ]);
	s.shoot();
	assert_eq!(s.app.yank_paths_for_tests().len(), 1, "a real Ctrl+C yanks the cursor's row");
	assert!(!s.app.yank_cut_for_tests());

	s.inject(vec![ egui::Event::Cut ]);
	s.shoot();
	assert_eq!(s.app.yank_paths_for_tests().len(), 1, "a real Ctrl+X holds the same row");
	assert!(s.app.yank_cut_for_tests(), "as a cut");

	// Ctrl+A is not one of the intercepted clipboard chords — it still arrives as a key event —
	// and the list answers it with the whole view selected
	s.inject(vec![ egui::Event::Key {
		key: egui::Key::A,
		physical_key: None,
		pressed: true,
		repeat: false,
		modifiers: egui::Modifiers::CTRL,
	} ]);
	s.shoot();
	assert_eq!(s.app.selection_for_tests().len(), s.app.listed(), "a real Ctrl+A selects the whole view");
	s.inject(vec![ egui::Event::Key {
		key: egui::Key::Escape,
		physical_key: None,
		pressed: true,
		repeat: false,
		modifiers: egui::Modifiers::NONE,
	} ]);
	s.shoot();
	assert!(s.app.selection_for_tests().is_empty(), "and Escape lets it go again");

	// the panel matrix: every panel the factory opens, closed with Escape, the list's own door
	// proven by a cleared yank and the real Copy that follows
	for (name, open) in [
		("index", "key i"),
		("help", "key ?"),
		("find", "key /"),
		("filter", "key f"),
		("bulk", "key A"),
		("rename", "key r"),
	] {
		s.app.rehearse(vec![ open.to_owned() ]);
		for _ in 0..3000 {
			if s.app.replay_done_for_tests() {
				break;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		s.app.rehearse(vec![ "key Esc".to_owned() ]);
		for _ in 0..3000 {
			if s.app.replay_done_for_tests() {
				break;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		for _ in 0..3 {
			s.shoot();
		}
		let (bulk, filter, find, help) = s.app.text_modes_for_tests();
		assert!(
			!bulk && !filter && !find && !help && !s.app.index_open() && !s.app.item_rename_for_tests(),
			"{name}: Escape closed it — bulk={bulk} filter={filter} find={find} help={help} index={} rename={}",
			s.app.index_open(),
			s.app.item_rename_for_tests(),
		);

		// the keyboard must be the list's again: `j` (the keymap's own arrow) walks the middle
		// cursor — a panel that failed to close would eat it — and the real Copy that follows
		// must hold the row the cursor now stands on
		let before = s.app.yank_paths_for_tests();
		let cursor = s.app.cursor_for_tests();
		s.inject(vec![ egui::Event::Key {
			key: egui::Key::J,
			physical_key: None,
			pressed: true,
			repeat: false,
			modifiers: egui::Modifiers::NONE,
		} ]);
		s.shoot();
		assert_eq!(s.app.cursor_for_tests(), cursor + 1, "{name}: the keyboard is the list's again");

		s.inject(vec![ egui::Event::Copy ]);
		s.shoot();
		let after = s.app.yank_paths_for_tests();
		assert_eq!(after.len(), 1, "{name}: the real Ctrl+C lands after Escape");
		assert_ne!(after, before, "{name}: and it holds the row the cursor now stands on");
	}

	// the field's own door, the real spelling: the filter takes the keyboard and its text is
	// what Ctrl+C / Ctrl+X act on
	s.app.rehearse(vec![ "key f".to_owned(), "type 甲乙".to_owned() ]);
	for _ in 0..3000 {
		if s.app.replay_done_for_tests() {
			break;
		}
		std::thread::sleep(Duration::from_millis(4));
		s.shoot();
	}
	s.inject(vec![ egui::Event::Copy ]);
	s.shoot();
	assert_eq!(s.copied_for_tests().as_deref(), Some("甲乙"), "a real Ctrl+C in the field copies its text");

	s.inject(vec![ egui::Event::Cut ]);
	s.shoot();
	s.inject(vec![ egui::Event::Text("x".to_owned()) ]);
	s.shoot();
	s.inject(vec![ egui::Event::Copy ]);
	s.shoot();
	assert_eq!(s.copied_for_tests().as_deref(), Some("x"), "a real Ctrl+X emptied the field");
}

/// P35③: `T` opens the cursor's row in a terminal — a folder becomes the terminal's own cwd
/// (P31's chain), and anything else says what it wants rather than guessing at a parent. The
/// dry switch keeps the harness from opening a window: what would have run is recorded.
#[test]
fn t_opens_the_cursor_row_in_a_terminal() {
	// sandbox: a folder and a file beside it
	let root = std::env::temp_dir().join(format!("crabhop-terminal-here-{}", std::process::id()));
	let _ = std::fs::remove_dir_all(&root);
	std::fs::create_dir_all(root.join("folder")).expect("a sandbox");
	std::fs::write(root.join("note.txt"), b"n").expect("a file");

	let walk = |s: &mut Session, keys: &[&str]| {
		s.app.rehearse(keys.iter().map(|key| (*key).to_owned()).collect());
		for _ in 0..3000 {
			if s.app.replay_done_for_tests() {
				break;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		for _ in 0..2 {
			s.shoot();
		}
	};

	// the cursor stands on the folder: `T` records the chain's own answer, its cwd that folder
	let mut s = session(vec![ format!("reveal {}", root.join("folder").display()) ]);
	for _ in 0..600 {
		if s.app.cwd_for_tests() == root {
			break;
		}
		std::thread::sleep(Duration::from_millis(4));
		s.shoot();
	}
	assert_eq!(s.app.cwd_for_tests(), root, "the window stands beside the folder");
	s.app.set_terminal_dry_for_tests();

	walk(&mut s, &[ "key T" ]);
	let mut launch = None;
	for _ in 0..600 {
		if let Some(found) = s.app.terminal_spawn_for_tests() {
			launch = Some(found);
			break;
		}
		std::thread::sleep(Duration::from_millis(4));
		s.shoot();
	}
	let launch = launch.expect("the terminal chain answered with something to run");
	assert!(!launch.argv.is_empty(), "the chain named a command: {:?}", launch.argv);
	assert_eq!(launch.folder, root.join("folder"), "and its cwd is the folder the cursor stood on");

	// a file row only says what it wants — and nothing is launched
	walk(&mut s, &[ "key j", "key T" ]);
	assert_eq!(s.app.toast_text_for_tests().as_deref(), Some("须是文件夹"), "a file row is refused with words");
	assert!(s.app.terminal_spawn_for_tests().is_none(), "and nothing was launched for it");
	assert_eq!(s.app.cwd_for_tests(), root, "the window never moved");

	let _ = std::fs::remove_dir_all(&root);
}

/// P35④: `y` / `x` reach the system clipboard as well — the targets' paths as text, so another
/// program can paste them at once (the `text/uri-list` exchange the file managers read is the
/// next phase). The `c` family has always gone through the same door; this is the yank's own.
#[test]
fn a_yank_also_reaches_the_system_clipboard() {
	let root = std::env::temp_dir().join(format!("crabhop-yank-clip-{}", std::process::id()));
	let _ = std::fs::remove_dir_all(&root);
	std::fs::create_dir_all(&root).expect("a sandbox");
	std::fs::write(root.join("a.txt"), b"a").expect("a file");
	std::fs::write(root.join("b.txt"), b"b").expect("a file");

	let walk = |s: &mut Session, keys: &[&str]| {
		s.app.rehearse(keys.iter().map(|key| (*key).to_owned()).collect());
		for _ in 0..3000 {
			if s.app.replay_done_for_tests() {
				break;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		for _ in 0..2 {
			s.shoot();
		}
	};

	let mut s = session(vec![ format!("reveal {}", root.join("a.txt").display()) ]);
	for _ in 0..600 {
		if s.app.cwd_for_tests() == root {
			break;
		}
		std::thread::sleep(Duration::from_millis(4));
		s.shoot();
	}
	assert_eq!(s.app.cwd_for_tests(), root, "the reveal has to stand in the sandbox");
	for _ in 0..600 {
		if s.app.listed() >= 2 {
			break;
		}
		std::thread::sleep(Duration::from_millis(4));
		s.shoot();
	}
	assert!(s.app.listed() >= 2, "the sandbox listing has to land");

	// the `c` family's own door (audited, not changed): a filename as typed
	walk(&mut s, &[ "key c", "key f" ]);
	assert_eq!(s.copied_for_tests().as_deref(), Some("a.txt"), "`c f` copies the filename");

	// a plain `y`: the cursor's row lands on the clipboard as its path
	walk(&mut s, &[ "key y" ]);
	assert_eq!(
		s.copied_for_tests().as_deref(),
		Some(root.join("a.txt").display().to_string().as_str()),
		"`y` puts the cursor's path on the clipboard"
	);

	// a selection, cut: every target's path, one per line — the same order the yank holds
	walk(&mut s, &[ "key Space", "key j", "key Space", "key x" ]);
	let want = format!("{}\n{}", root.join("a.txt").display(), root.join("b.txt").display());
	assert_eq!(s.copied_for_tests().as_deref(), Some(want.as_str()), "`x` puts the whole selection there");
	assert!(s.app.yank_cut_for_tests(), "and the yank itself still holds it as a cut");

	let _ = std::fs::remove_dir_all(&root);
}

/// P35's final bug and its addendum, end to end: `text/plain`'s candidates carry nvim (its
/// user-local `NoDisplay=true` mask used to erase it); the panel shows two zones — the
/// recommended candidates first, every application behind — and one query filters them alike;
/// and choosing a full-zone application runs once without writing a preference unless asked.
/// The test walks the real menu: the row's menu, 打开方式…, the query, and the rows found by
/// their own ids.
#[test]
fn the_open_with_list_carries_nvim_and_wraps_a_terminal_entry() {
	let root = std::env::temp_dir().join(format!("crabhop-openwith-{}", std::process::id()));
	let _ = std::fs::remove_dir_all(&root);
	std::fs::create_dir_all(&root).expect("a sandbox");
	std::fs::write(root.join("note.txt"), b"n").expect("a text file");

	let walk = |s: &mut Session, keys: &[&str]| {
		s.app.rehearse(keys.iter().map(|key| (*key).to_owned()).collect());
		for _ in 0..3000 {
			if s.app.replay_done_for_tests() {
				break;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		for _ in 0..2 {
			s.shoot();
		}
	};

	let mut s = session(vec![ format!("reveal {}", root.join("note.txt").display()) ]);
	for _ in 0..600 {
		if s.app.cwd_for_tests() == root {
			break;
		}
		std::thread::sleep(Duration::from_millis(4));
		s.shoot();
	}
	assert_eq!(s.app.cwd_for_tests(), root, "the window stands on the sandbox");
	s.app.set_terminal_dry_for_tests();

	// the row's menu, then 打开方式… — the fourth row of a file's own roster
	walk(&mut s, &[ "menu 0", "key j", "key j", "key j", "key Enter" ]);

	// the two zones (P35's addendum): the recommended candidates open the page, and every
	// application follows — nvim, the writer, and a browser nobody recommends for text/plain
	let rows = s.app.menu_app_rows_for_tests();
	assert_eq!(rows.first().map(|(zone, _)| *zone), Some(Some("推荐")), "{rows:?}");
	let full = rows.iter().position(|(zone, _)| *zone == Some("全部应用")).expect("the full zone follows: {rows:?} in {rows:?}");
	assert!(rows[..full].iter().any(|(_, id)| id == "nvim.desktop"), "nvim is recommended: {rows:?}");
	let all_ids: Vec<&str> = rows[full..].iter().map(|(_, id)| id.as_str()).collect();
	assert!(all_ids.contains(&"nvim.desktop") && all_ids.contains(&"libreoffice-writer.desktop") && all_ids.contains(&"chromium.desktop"), "{all_ids:?}");

	// one query over both zones: `chromium` is found from the full zone — whatever else a machine
	// has whose *name* says chrom stands beside it, and nothing else does
	walk(&mut s, &[ "type chrom" ]);
	let filtered = s.app.menu_app_rows_for_tests();
	assert!(filtered.iter().any(|(zone, id)| id == "chromium.desktop" && *zone == Some("全部应用")), "{filtered:?}");
	assert!(filtered.iter().all(|(_, id)| id.to_lowercase().contains("chrom")), "only the matches stand: {filtered:?}");
	assert!(!filtered.iter().any(|(_, id)| id == "nvim.desktop"), "and a text/plain-only app is filtered out");

	// Escape clears the query before it backs out; the page is whole again
	walk(&mut s, &[ "key Escape" ]);
	assert_eq!(s.app.menu_app_rows_for_tests().len(), rows.len(), "the query is let go first");
	assert!(s.app.menu_app_rows_for_tests().iter().any(|(_, id)| id == "nvim.desktop"), "and the page still stands");

	// stand on the full zone's own nvim — the query brings it down to two rows, and the second is
	// the full zone's — take it: P37③'s own re-cut runs it at once (仅一次), and the mimeapps
	// file is not touched
	walk(&mut s, &[ "type nvim" ]);
	let rows = s.app.menu_app_rows_for_tests();
	assert_eq!(rows.len(), 2, "both zones answer for nvim: {rows:?}");
	assert_eq!(rows[1].0, Some("全部应用"), "the second is the full zone's own");
	let before = crate::config::Config::mimeapps().and_then(|at| std::fs::read_to_string(at).ok());
	s.app.rehearse(vec![ "key Down".to_owned(), "key Enter".to_owned() ]);
	for _ in 0..3000 {
		if s.app.replay_done_for_tests() {
			break;
		}
		std::thread::sleep(Duration::from_millis(4));
		s.shoot();
	}

	let mut launch = None;
	for _ in 0..600 {
		if let Some(found) = s.app.terminal_spawn_for_tests() {
			launch = Some(found);
			break;
		}
		std::thread::sleep(Duration::from_millis(4));
		s.shoot();
	}
	let launch = launch.expect("the terminal chain answered");
	assert!(!launch.argv.is_empty(), "a terminal command stands first: {:?}", launch.argv);
	assert!(launch.argv.iter().any(|part| part == "nvim"), "the program follows it: {:?}", launch.argv);
	assert_eq!(launch.argv.last().map(String::as_str), Some(root.join("note.txt").display().to_string().as_str()), "and the file is the command's argument");
	assert_eq!(launch.folder, root, "with the file's own directory as the cwd");
	// 仅一次 and the write path retired together (P37③): the pick writes no preference at all
	assert_eq!(
		crate::config::Config::mimeapps().and_then(|at| std::fs::read_to_string(at).ok()),
		before,
		"仅一次 writes no preference"
	);

	let _ = std::fs::remove_dir_all(&root);
}

	/// P36①: Enter on a menu row goes the way a click goes — that was the user's own complaint
	/// (「Standing 在打开方式行按 Enter 无反应，必须鼠标点」). The bug was the pointer's quiet
	/// re-seat: every frame, whatever row the pointer lay over took the standing row over, so a
	/// walk to 打开方式 was rewritten under the person's eyes and Enter took the pointer's row —
	/// which, on a greyed or out-of-range row, acted on nothing at all. The test keeps the real
	/// pointer lying over the menu's first row for the whole walk.
	#[test]
	fn the_open_with_row_opens_on_enter_with_the_pointer_lying_over_another_row() {
		let root = std::env::temp_dir().join(format!("crabhop-p36a-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(&root).expect("a sandbox");
		std::fs::write(root.join("note.txt"), b"n").expect("a text file");

		let mut s = session(vec![ format!("reveal {}", root.join("note.txt").display()) ]);
		for _ in 0..600 {
			if s.app.cwd_for_tests() == root {
				break;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		assert_eq!(s.app.cwd_for_tests(), root, "the window stands on the sandbox");

		// a real pointer where the row's menu will open, then the menu's own replay step
		let (body, shares) = s.app.layout();
		let list = chrome::columns(body, shares).current;
		let at = egui::pos2(list.min.x + 80.0, list.min.y + 40.0);
		s.move_at(at);
		s.shoot();
		s.app.rehearse(vec![ "menu 0".to_owned() ]);
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if s.app.menu_state_for_tests().is_some() {
				break;
			}
		}
		assert!(s.app.menu_state_for_tests().is_some_and(|(page, _)| page == "main"), "the row's menu is up: {:?}", s.app.menu_state_for_tests());

		// the pointer settles over the menu's first row and stays there for the whole walk
		s.move_at(egui::pos2(at.x + 40.0, at.y + 26.0 + 12.0));
		s.shoot();
		s.shoot();

		// walk to 打开方式 (a file's fourth row) and press Enter
		s.app.rehearse(vec![ "key j".to_owned(), "key j".to_owned(), "key j".to_owned(), "key Enter".to_owned() ]);
		for _ in 0..120 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if s.app.menu_state_for_tests().is_some_and(|(page, _)| page == "apps") {
				break;
			}
		}
		assert_eq!(s.app.menu_state_for_tests().map(|(page, _)| page), Some("apps"), "Enter opened the applications page");

		let _ = std::fs::remove_dir_all(&root);
	}

	/// P36②: while the panel stands, the wheel is the panel's — its pages walk their own window
	/// with it (P36③), and the file list behind does not move a row. The control runs first: with
	/// nobody over it, the very same wheel does move the list.
	#[test]
	fn the_wheel_walks_the_panel_and_never_the_list_behind() {
		let root = std::env::temp_dir().join(format!("crabhop-p36b-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(&root).expect("a sandbox");
		for i in 0..60 {
			std::fs::write(root.join(format!("f{i:02}.txt")), b"x").expect("a file of the sandbox");
		}

		let mut s = session(vec![ format!("reveal {}", root.join("f00.txt").display()) ]);
		assert!(s.wait_for_list(60, 400), "the listing lands");
		assert_eq!(s.app.cwd_for_tests(), root, "the window stands on the sandbox");
		assert_eq!(s.app.scroll_offset_for_tests(), 0);

		// the control: with no panel up, the wheel walks the list
		s.app.rehearse(vec![ "wheel -6".to_owned() ]);
		for _ in 0..120 {
			if s.app.replay_done_for_tests() && s.app.scroll_offset_for_tests() > 0 {
				break;
			}
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
		}
		assert!(s.app.scroll_offset_for_tests() > 0, "the list hears the wheel when no panel stands: {}", s.app.scroll_offset_for_tests());

		// the applications page, up the way the E2E opens it
		s.app.rehearse(vec![ "menu 0".to_owned(), "key j".to_owned(), "key j".to_owned(), "key j".to_owned(), "key Enter".to_owned() ]);
		for _ in 0..120 {
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
			if s.app.menu_state_for_tests().is_some_and(|(page, _)| page == "apps") {
				break;
			}
		}
		assert!(s.app.menu_state_for_tests().is_some_and(|(page, _)| page == "apps"), "the applications page stands");

		// the wheel now belongs to the panel: its window walks (the cap is twelve rows against a
		// machine's whole roster), and the list behind is where it was
		let list_before = s.app.scroll_offset_for_tests();
		let menu_before = s.app.menu_scroll_for_tests();
		s.app.rehearse(vec![ "wheel -14".to_owned() ]);
		for _ in 0..120 {
			if s.app.replay_done_for_tests() {
				break;
			}
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
		}
		s.shoot();
		s.shoot();
		assert_eq!(s.app.scroll_offset_for_tests(), list_before, "the list behind did not move");
		assert!(
			s.app.menu_scroll_for_tests().is_some_and(|at| Some(at) > menu_before),
			"the panel's own window walked: {:?} from {menu_before:?}",
			s.app.menu_scroll_for_tests()
		);

		// and the rule holds on the item page too: Esc back to the menu, a wheel, the list still
		s.app.rehearse(vec![ "key Esc".to_owned(), "wheel -6".to_owned() ]);
		for _ in 0..120 {
			if s.app.replay_done_for_tests() {
				break;
			}
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
		}
		s.shoot();
		s.shoot();
		assert!(s.app.menu_state_for_tests().is_some_and(|(page, _)| page == "main"), "the item page is up again");
		assert_eq!(s.app.menu_scroll_for_tests(), Some(0), "a short page has nowhere to scroll and stays home");
		assert_eq!(s.app.scroll_offset_for_tests(), list_before, "and the list behind still did not move");

		let _ = std::fs::remove_dir_all(&root);
	}

	/// P36④: `o` opens the cursor row's context menu — the user's own key, for hands without a
	/// second button. It is the right click's own door: the same panel, the same target, the same
	/// state. The factory table carries the row (the help list and `which` read it from there).
	/// P37⑤: the right column's document previews (docx, sheet, text) walk again — the seek keys
	/// and the wheel both. The bisect the user handed down pointed at P36②'s wheel routing and at
	/// the document path; the evidence here says neither: with no panel standing the wheel walked
	/// the preview all along (the guard only fires while the menu is), and what was missing were
	/// the seek keys — `J`/`K` lost their rows from the factory table in P21's wave one and never
	/// got them back, so the keyboard could not move a document at all. Restored beside the wheel,
	/// with this reel as the guard: a docx (a real zip, the extractor's own shape) scrolls by the
	/// key, then by the wheel, and the second screen's pixel band changes with the offset.
	#[test]
	fn the_seek_keys_and_the_wheel_walk_a_document_preview() {
		use std::io::Write as _;
		let root = std::env::temp_dir().join(format!("crabhop-p37e-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(&root).expect("a sandbox");

		// a docx of the test's own: a zip with word/document.xml, the extractor's own shape
		{
			let file = std::fs::File::create(root.join("long.docx")).expect("a docx file");
			let mut zip = zip::ZipWriter::new(file);
			let options: zip::write::FileOptions<'_, ()> =
				zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
			zip.start_file("word/document.xml", options).unwrap();
			let mut xml = String::from("<w:document xmlns:w=\"x\"><w:body>");
			for i in 0..400 {
				xml.push_str(&format!("<w:p><w:r><w:t>line {i:03}</w:t></w:r></w:p>"));
			}
			xml.push_str("</w:body></w:document>");
			zip.write_all(xml.as_bytes()).unwrap();
			zip.finish().unwrap();
		}

		let mut s = session(vec![ format!("reveal {}", root.join("long.docx").display()) ]);
		for _ in 0..600 {
			if s.app.cwd_for_tests() == root {
				break;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		assert_eq!(s.app.cwd_for_tests(), root, "the window stands on the sandbox");
		for _ in 0..240 {
			if s.app.preview_extent_for_tests().is_some() {
				break;
			}
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
		}
		let (content, offset, view) = s.app.preview_extent_for_tests().expect("the document is up and scrollable");
		assert!(content > view + 100.0, "the document outruns its column: {content} against {view}");
		assert_eq!(offset, 0.0, "at the first screen");
		let (body, shares) = s.app.layout();
		let preview = chrome::columns(body, shares).preview.expect("a preview column");
		let band = egui::Rect::from_min_max(
			egui::pos2(preview.min.x + 20.0, preview.min.y + 300.0),
			egui::pos2(preview.max.x - 20.0, preview.min.y + 600.0),
		);
		let ink_before = lit(&s.shoot(), band);

		// `J`: the seek key walks five lines of text
		s.app.rehearse(vec![ "key J".to_owned() ]);
		for _ in 0..60 {
			if s.app.replay_done_for_tests() {
				break;
			}
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
		}
		s.shoot();
		let by_key = s.app.preview_extent_for_tests().expect("still scrollable").1;
		assert!(by_key >= 100.0, "`J` walked five lines of text: {by_key}");

		// the wheel over the preview column walks it further, and the second screen moves with it
		s.app.rehearse(vec![ format!("hover {} {}", preview.center().x, preview.center().y), "wheel -5".to_owned() ]);
		for _ in 0..60 {
			if s.app.replay_done_for_tests() {
				break;
			}
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
		}
		s.shoot();
		s.shoot();
		let by_wheel = s.app.preview_extent_for_tests().expect("still scrollable").1;
		assert!(by_wheel > by_key, "the wheel walked it further: {by_wheel} after {by_key}");
		assert_ne!(lit(&s.shoot(), band), ink_before, "and the second screen's pixels moved with the offset");

		let _ = std::fs::remove_dir_all(&root);
	}

	#[test]
	fn the_o_key_opens_the_cursor_rows_menu() {
		let root = std::env::temp_dir().join(format!("crabhop-p36o-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(&root).expect("a sandbox");
		std::fs::write(root.join("note.txt"), b"n").expect("a text file");

		let mut s = session(vec![ format!("reveal {}", root.join("note.txt").display()) ]);
		for _ in 0..600 {
			if s.app.cwd_for_tests() == root {
				break;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		assert_eq!(s.app.cwd_for_tests(), root, "the window stands on the sandbox");

		// the cursor's own row by the key: a file's roster, four rows
		s.app.rehearse(vec![ "key o".to_owned() ]);
		for _ in 0..120 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if s.app.menu_state_for_tests().is_some() {
				break;
			}
		}
		let by_key = s.app.menu_state_for_tests();
		assert_eq!(by_key, Some(("main", 4)), "`o` opened the cursor row's menu");

		// Esc, then the right click on the very same row: one menu, two doors
		s.app.rehearse(vec![ "key Esc".to_owned() ]);
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if s.app.menu_state_for_tests().is_none() {
				break;
			}
		}
		assert!(s.app.menu_state_for_tests().is_none(), "Esc put it away");
		s.app.rehearse(vec![ "menu 0".to_owned() ]);
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if s.app.menu_state_for_tests().is_some() {
				break;
			}
		}
		assert_eq!(s.app.menu_state_for_tests(), by_key, "the right click opens the same state");

		let _ = std::fs::remove_dir_all(&root);
	}

/// P37①, the user's own re-cut of the list's `Enter`: the default opener takes the row — files
	/// and directories alike (a directory goes to its default application, the file manager, not
	/// into the window) — and the way in is `l` and `<Right>`, which still change the cwd. The
	/// harness's dry switch records what the opener would have been handed, so nothing spawns.
	#[test]
	fn enter_opens_the_default_and_l_enters_the_directory() {
		let root = std::env::temp_dir().join(format!("crabhop-p37a-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(root.join("sub")).expect("a sandbox");
		std::fs::write(root.join("note.txt"), b"n").expect("a text file");
		std::fs::write(root.join("sub").join("deep.txt"), b"d").expect("a file inside");

		let mut s = session(vec![ format!("reveal {}", root.join("sub").display()) ]);
		for _ in 0..600 {
			if s.app.cwd_for_tests() == root {
				break;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		assert_eq!(s.app.cwd_for_tests(), root, "the window stands on the sandbox, seated on the directory");
		s.app.set_open_dry_for_tests();

		// Enter on the directory row: the default opener's own path — and the window stays put
		s.app.rehearse(vec![ "key Enter".to_owned() ]);
		let mut opened = None;
		for _ in 0..120 {
			if let Some(path) = s.app.open_spawn_for_tests() {
				opened = Some(path);
				break;
			}
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
		}
		assert_eq!(opened, Some(root.join("sub")), "Enter handed the directory to the default opener");
		assert_eq!(s.app.cwd_for_tests(), root, "and did not step into it");

		// `l`: the way in
		s.app.rehearse(vec![ "key l".to_owned() ]);
		for _ in 0..120 {
			if s.app.cwd_for_tests() == root.join("sub") {
				break;
			}
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
		}
		assert_eq!(s.app.cwd_for_tests(), root.join("sub"), "`l` entered the directory");

		// back out, seat on the directory again, then `<Right>`: the smart enter walks in too
		s.app.rehearse(vec![ "key h".to_owned() ]);
		for _ in 0..120 {
			if s.app.cwd_for_tests() == root {
				break;
			}
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
		}
		assert_eq!(s.app.cwd_for_tests(), root, "h walked back");
		s.app.rehearse(vec![ format!("reveal {}", root.join("sub").display()) ]);
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
		}
		s.app.rehearse(vec![ "key Right".to_owned() ]);
		for _ in 0..120 {
			if s.app.cwd_for_tests() == root.join("sub") {
				break;
			}
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
		}
		assert_eq!(s.app.cwd_for_tests(), root.join("sub"), "Right entered the directory too");

		let _ = std::fs::remove_dir_all(&root);
	}

	/// P37②: the 打开方式 row answers to `l` too — the menu's own step-into gesture takes the
	/// applications list, exactly the door Enter opens. The test walks both ways to the same row
	/// and compares the states.
	#[test]
	fn the_open_with_row_opens_on_l_as_well() {
		let root = std::env::temp_dir().join(format!("crabhop-p37l-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(&root).expect("a sandbox");
		std::fs::write(root.join("note.txt"), b"n").expect("a text file");

		let mut s = session(vec![ format!("reveal {}", root.join("note.txt").display()) ]);
		for _ in 0..600 {
			if s.app.cwd_for_tests() == root {
				break;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		assert_eq!(s.app.cwd_for_tests(), root, "the window stands on the sandbox");

		let wait_page = |s: &mut Session, page: &str| -> Option<(&'static str, usize)> {
			for _ in 0..120 {
				std::thread::sleep(Duration::from_millis(8));
				s.shoot();
				if s.app.menu_state_for_tests().is_some_and(|(up, _)| up == page) {
					break;
				}
			}
			s.app.menu_state_for_tests()
		};

		// `l` on the 打开方式 row (a file's fourth)
		s.app.rehearse(vec![ "menu 0".to_owned(), "key j".to_owned(), "key j".to_owned(), "key j".to_owned(), "key l".to_owned() ]);
		let by_l = wait_page(&mut s, "apps");
		assert!(by_l.is_some_and(|(page, _)| page == "apps"), "`l` opened the applications page: {by_l:?}");

		// Esc back to the items, then Enter on the same row: one page, two doors
		s.app.rehearse(vec![ "key Esc".to_owned() ]);
		let back = wait_page(&mut s, "main");
		assert_eq!(back, Some(("main", 4)), "Esc stepped back to the items");
		s.app.rehearse(vec![ "key j".to_owned(), "key j".to_owned(), "key j".to_owned(), "key Enter".to_owned() ]);
		let by_enter = wait_page(&mut s, "apps");
		assert_eq!(by_enter, by_l, "Enter and `l` open the same state");

		let _ = std::fs::remove_dir_all(&root);
	}

	/// P37③ + P38①, the user's own re-cut and their frame: the applications panel is a compact
	/// window with floors on both axes — a query that filters to nothing leaves the rect at the
	/// height floor and the width floor, with the empty state standing in the kept room — and
	/// whatever the panel says is clipped to it: nothing is drawn outside the rect.
	#[test]
	fn the_applications_panel_keeps_its_floors_and_draws_inside_them() {
		let root = std::env::temp_dir().join(format!("crabhop-p37f-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(&root).expect("a sandbox");
		std::fs::write(root.join("note.txt"), b"n").expect("a text file");

		let mut s = session(vec![ format!("reveal {}", root.join("note.txt").display()) ]);
		for _ in 0..600 {
			if s.app.cwd_for_tests() == root {
				break;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		assert_eq!(s.app.cwd_for_tests(), root, "the window stands on the sandbox");

		// the applications page, the way the E2E opens it
		s.app.rehearse(vec![ "menu 0".to_owned(), "key j".to_owned(), "key j".to_owned(), "key j".to_owned(), "key Enter".to_owned() ]);
		for _ in 0..120 {
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
			if s.app.menu_state_for_tests().is_some_and(|(page, _)| page == "apps") {
				break;
			}
		}
		// the panel's rect is written by the draw, one step behind the state: one settling frame
		s.shoot();
		assert!(s.app.menu_state_for_tests().is_some_and(|(page, _)| page == "apps"), "the applications page stands");
		let full = s.app.menu_panel_rect_for_tests().expect("the panel was drawn").3;
		assert!(full >= crate::menu::menu_apps_floor(), "a full window is at least the floor: {full}");

		// a needle nothing answers: the rows go, the height stays
		s.app.rehearse(vec![ "type zzzzzzz".to_owned() ]);
		for _ in 0..120 {
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
			if s.app.menu_app_rows_for_tests().is_empty() {
				break;
			}
		}
		s.shoot();
		assert!(s.app.menu_app_rows_for_tests().is_empty(), "no application answers zzzzzzz");
		s.shoot();
		let (px, py, pw, ph) = s.app.menu_panel_rect_for_tests().expect("the panel was drawn");
		assert_eq!(ph, crate::menu::menu_apps_floor(), "the height floor holds while the empty state shows");
		assert_eq!(pw, crate::menu::MENU_APPS_MIN_W, "and so does the width floor: {pw}");

		// nothing is painted outside the panel: a strip just beyond its right edge is the list's
		// own pixels with the panel up — exactly what it is with the panel down
		let pixels_of = |image: &egui::ColorImage, rect: egui::Rect| -> Vec<egui::Color32> {
			let mut out = Vec::new();
			for y in (rect.min.y as usize)..(rect.max.y as usize) {
				for x in (rect.min.x as usize)..(rect.max.x as usize) {
					out.push(image.pixels[y * image.size[0] + x]);
				}
			}
			out
		};
		// clear of the edge by more than the shadow reaches (18px and a nudge): the shadow is the
		// panel's own décor, not a word (P38①)
		let strip = egui::Rect::from_min_max(
			egui::pos2(px + pw + 26.0, py + 6.0),
			egui::pos2(px + pw + 60.0, py + ph - 6.0),
		);
		let open_pixels = pixels_of(&s.shoot(), strip);

		// Esc clears the query, Esc steps back, Esc closes: the same strip, the panel gone
		s.app.rehearse(vec![ "key Esc".to_owned(), "key Esc".to_owned(), "key Esc".to_owned() ]);
		for _ in 0..120 {
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
			if s.app.menu_state_for_tests().is_none() {
				break;
			}
		}
		assert!(s.app.menu_state_for_tests().is_none(), "the menu is closed again");
		let closed_pixels = pixels_of(&s.shoot(), strip);
		let differing = open_pixels.iter().zip(&closed_pixels).filter(|(a, b)| a != b).count();
		assert!(open_pixels == closed_pixels, "the panel's words stay inside it: {differing} pixels differ");

		let _ = std::fs::remove_dir_all(&root);
	}

	/// P37④, the user's own report («总是 nvim 后 Enter 打不开»): the default-open path wraps a
	/// `Terminal=true` default handler in the machine's terminal chain, the way the menu's own
	/// launch does — and everything else stays with `xdg-open`. The preference is handed in
	/// through the test seam (the person's own `mimeapps.list` is never touched), the entry is the
	/// machine's real nvim, and the dry switch hears the chain so no window opens.
	#[test]
	fn the_default_open_wraps_a_terminal_handler_in_the_chain() {
		let root = std::env::temp_dir().join(format!("crabhop-p37t-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(&root).expect("a sandbox");
		std::fs::write(root.join("note.dfmx"), b"x").expect("a file of an unregistered type");

		let mut s = session(vec![ format!("reveal {}", root.join("note.dfmx").display()) ]);
		for _ in 0..600 {
			if s.app.cwd_for_tests() == root {
				break;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		assert_eq!(s.app.cwd_for_tests(), root, "the window stands on the sandbox");

		// the preferences the lookup reads, handed in (the person's own file is never touched):
		// nvim — a `Terminal=true` entry — is this unregistered type's default
		let mime = crate::mime::guess("note.dfmx", false);
		s.app.set_mimeapps_for_tests(format!("[Default Applications]\n{mime}=nvim.desktop;\n"));
		s.app.set_terminal_dry_for_tests();
		s.app.set_open_dry_for_tests();

		s.app.rehearse(vec![ "key Enter".to_owned() ]);
		let mut launch = None;
		for _ in 0..120 {
			if let Some(found) = s.app.terminal_spawn_for_tests() {
				launch = Some(found);
				break;
			}
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
		}
		let launch = launch.unwrap_or_else(|| panic!("the terminal chain answered for {mime}: {:?}", s.app.open_spawn_for_tests()));
		assert!(!launch.argv.is_empty(), "a terminal command stands first: {:?}", launch.argv);
		assert!(launch.argv.iter().any(|part| part == "nvim"), "the program follows it: {:?}", launch.argv);
		assert_eq!(launch.argv.last().map(String::as_str), Some(root.join("note.dfmx").display().to_string().as_str()), "and the file is the command's argument");
		assert_eq!(launch.folder, root, "with the file's own directory as the cwd");
		assert!(s.app.open_spawn_for_tests().is_none(), "the system opener was not used");

		let _ = std::fs::remove_dir_all(&root);
	}

	/// P38②, the user's own ruling («总是 复活»): Enter on an applications row runs it once —
/// `Ctrl+Enter` writes the type's default first (the merge, through the test seam — no file
/// anywhere) and then runs it, and the footer says so. The assertions: Enter leaves the
/// preferences byte-identical, Ctrl+Enter gains the entry's own id.
	#[test]
	fn ctrl_enter_writes_the_default_and_enter_leaves_it_alone() {
		let root = std::env::temp_dir().join(format!("crabhop-p38b-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(&root).expect("a sandbox");
		std::fs::write(root.join("note.txt"), b"n").expect("a text file");

		let mut s = session(vec![ format!("reveal {}", root.join("note.txt").display()) ]);
		for _ in 0..600 {
			if s.app.cwd_for_tests() == root {
				break;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		assert_eq!(s.app.cwd_for_tests(), root, "the window stands on the sandbox");
		s.app.set_terminal_dry_for_tests();

		// the preferences the merge reads and writes: the seam, holding one other type already
		let seed = "[Added Associations]\nimage/png=writer.desktop;\n".to_owned();
		s.app.set_mimeapps_for_tests(seed.clone());

		// the walk into the applications, onto nvim (a terminal entry, so the launch is dry)
		let onto_nvim = |s: &mut Session| {
			s.app.rehearse(vec![
				"menu 0".to_owned(),
				"key j".to_owned(),
				"key j".to_owned(),
				"key j".to_owned(),
				"key Enter".to_owned(),
				"type nvim".to_owned(),
				"key Down".to_owned(),
			]);
			for _ in 0..600 {
				if s.app.replay_done_for_tests() {
					break;
				}
				std::thread::sleep(Duration::from_millis(4));
				s.shoot();
			}
			for _ in 0..4 {
				s.shoot();
			}
		};

		// Enter: the app runs at once, and the preferences are not touched
		onto_nvim(&mut s);
		s.app.rehearse(vec![ "key Enter".to_owned() ]);
		let mut once = None;
		for _ in 0..600 {
			if s.app.replay_done_for_tests()
				&& let Some(found) = s.app.terminal_spawn_for_tests()
			{
				once = Some(found);
				break;
			}
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
		}
		assert!(once.is_some(), "the app ran");
		assert_eq!(s.app.mimeapps_text_for_tests(), Some(seed.clone()), "Enter writes nothing");

		// Ctrl+Enter: the default is written down (the merge gains nvim) and the app runs
		onto_nvim(&mut s);
		s.app.rehearse(vec![ "key C-Enter".to_owned() ]);
		let mut always = None;
		for _ in 0..600 {
			if s.app.replay_done_for_tests()
				&& let Some(found) = s.app.terminal_spawn_for_tests()
			{
				always = Some(found);
				break;
			}
			std::thread::sleep(Duration::from_millis(8));
			s.shoot();
		}
		let text = s.app.mimeapps_text_for_tests().expect("the seam still stands");
		assert!(text.contains("text/plain=nvim.desktop"), "the default was written: {text:?}");
		assert!(text.contains("image/png=writer.desktop;"), "and the other type was left where it was");
		assert!(always.is_some(), "and the app ran");

		let _ = std::fs::remove_dir_all(&root);
	}

	/// P38③: the scroll contract — six preview kinds × three ways; every pane that can scroll must
/// actually scroll. The wheel, the seek keys, and the overlay scrollbar dragged by a real
/// pointer. The picture's keys cell is its documented exception: `seek` answers nothing for an
/// image (the picture walks by wheel and by hand), and that is asserted too. One test, eighteen
/// cells — the gate the routing and extent changes must pass from now on.
	#[test]
	fn the_scroll_contract_matrix_holds_for_every_kind_and_way() {
		use std::io::Write as _;
		let root = std::env::temp_dir().join(format!("crabhop-p38c-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(&root).expect("a sandbox");

		let put_zip = |name: &str, files: Vec<(&str, String)>| {
			let file = std::fs::File::create(root.join(name)).expect("a fixture file");
			let mut zip = zip::ZipWriter::new(file);
			let options: zip::write::FileOptions<'_, ()> =
				zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
			for (path, body) in files {
				zip.start_file(path, options).unwrap();
				zip.write_all(body.as_bytes()).unwrap();
			}
			zip.finish().unwrap();
		};

		// docx: the extractor's own shape
		let mut docx = String::from("<w:document xmlns:w=\"x\"><w:body>");
		for i in 0..400 {
			docx.push_str(&format!("<w:p><w:r><w:t>line {i:03}</w:t></w:r></w:p>"));
		}
		docx.push_str("</w:body></w:document>");
		put_zip("long.docx", vec![ ("word/document.xml", docx) ]);

		// xlsx: calamine's shape — content types, workbook, its rels, one inline-string sheet
		let mut sheet = String::from(
			"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>",
		);
		for i in 0..200 {
			sheet.push_str(&format!("<row r=\"{}\"><c r=\"A{}\" t=\"inlineStr\"><is><t>row {i:03}</t></is></c></row>", i + 1, i + 1));
		}
		sheet.push_str("</sheetData></worksheet>");
		put_zip("long.xlsx", vec![
			(
				"[Content_Types].xml",
				"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
<Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>\
</Types>"
					.to_owned(),
			),
			(
				"_rels/.rels",
				"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>\
</Relationships>"
					.to_owned(),
			),
			(
				"xl/workbook.xml",
				"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\">\
<sheets><sheet name=\"Sheet1\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>"
					.to_owned(),
			),
			(
				"xl/_rels/workbook.xml.rels",
				"<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/>\
</Relationships>"
					.to_owned(),
			),
			("xl/worksheets/sheet1.xml", sheet),
		]);

		// markdown and code: plain lines, long enough to outrun the column
		let lines: String = (0..400).map(|i| format!("# line {i:03} of the long markdown file\n")).collect();
		std::fs::write(root.join("long.md"), lines.as_bytes()).expect("a markdown file");
		let code: String = (0..400).map(|i| format!("let value_{i:03} = {i};\n")).collect();
		std::fs::write(root.join("long.rs"), code.as_bytes()).expect("a code file");

		// archive: enough entries that the tree outruns the column
		let file = std::fs::File::create(root.join("many.zip")).expect("an archive");
		{
			let mut zip = zip::ZipWriter::new(file);
			let options: zip::write::FileOptions<'_, ()> =
				zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);
			for i in 0..80 {
				zip.start_file(format!("entry-{i:03}.txt"), options).unwrap();
				zip.write_all(b"x").unwrap();
			}
			zip.finish().unwrap();
		}

		// picture: taller than the column at 1:1 (the fit keeps it whole, so the zoom comes first)
		image::RgbaImage::from_pixel(120, 900, image::Rgba([ 200, 40, 40, 255 ]))
			.save(root.join("tall.png"))
			.expect("a picture");

		let cases = [
			("docx", "long.docx", false),
			("xlsx", "long.xlsx", false),
			("md", "long.md", false),
			("code", "long.rs", false),
			("archive", "many.zip", false),
			("image", "tall.png", true),
		];

		for (kind, name, picture) in cases {
			let mut s = session(vec![ format!("reveal {}", root.join(name).display()) ]);
			for _ in 0..600 {
				if s.app.cwd_for_tests() == root {
					break;
				}
				std::thread::sleep(Duration::from_millis(4));
				s.shoot();
			}
			assert_eq!(s.app.cwd_for_tests(), root, "{kind}: the window stands on the sandbox");

			if picture {
				// the picture lands first, then the zoom that makes it a page to walk
				for _ in 0..300 {
					if s.app.preview_offset_for_tests().is_some() {
						break;
					}
					std::thread::sleep(Duration::from_millis(16));
					s.shoot();
				}
				s.app.rehearse(vec![ "zoom full".to_owned() ]);
				for _ in 0..60 {
					if s.app.replay_done_for_tests() {
						break;
					}
					std::thread::sleep(Duration::from_millis(8));
					s.shoot();
				}
				s.shoot();
				s.shoot();
			}

			// the pane has to be up and scrollable before any way means anything
			for _ in 0..300 {
				if s.app.preview_extent_for_tests().is_some() {
					break;
				}
				std::thread::sleep(Duration::from_millis(16));
				s.shoot();
			}
			let start = s.app.preview_extent_for_tests().unwrap_or_else(|| panic!("{kind}: nothing to scroll")).1;

			let (body, shares) = s.app.layout();
			let preview = chrome::columns(body, shares).preview.expect("a preview column");

			// way one: the wheel over the column
			s.app.rehearse(vec![ format!("hover {} {}", preview.center().x, preview.center().y), "wheel -5".to_owned() ]);
			for _ in 0..60 {
				if s.app.replay_done_for_tests() {
					break;
				}
				std::thread::sleep(Duration::from_millis(8));
				s.shoot();
			}
			s.shoot();
			s.shoot();
			let by_wheel = s.app.preview_extent_for_tests().expect("still scrollable").1;
			assert!(by_wheel > start + 1.0, "{kind} × wheel moves the offset: {by_wheel} from {start}");

			// way two: the seek keys (`J`; the picture's own contract is the documented no-op)
			let before_keys = by_wheel;
			s.app.rehearse(vec![ "key J".to_owned() ]);
			for _ in 0..60 {
				if s.app.replay_done_for_tests() {
					break;
				}
				std::thread::sleep(Duration::from_millis(8));
				s.shoot();
			}
			s.shoot();
			let by_keys = s.app.preview_extent_for_tests().expect("still scrollable").1;
			if picture {
				assert!((by_keys - before_keys).abs() < 0.5, "{kind} × keys is the documented no-op: {by_keys} from {before_keys}");
			} else {
				assert!(by_keys > before_keys + 1.0, "{kind} × keys moves the offset: {by_keys} from {before_keys}");
			}

			// way three: the overlay scrollbar, dragged by a real pointer
			let before_bar = s.app.preview_extent_for_tests().expect("still scrollable").1;
			let (content, offset, view) = s.app.preview_extent_for_tests().expect("still scrollable");
			let track = egui::Rect::from_min_max(
				egui::pos2(preview.max.x - 10.0, preview.min.y + 4.0),
				egui::pos2(preview.max.x, preview.max.y - 4.0),
			);
			let bar = chrome::bar(track, content, view, offset).expect("the bar is up while content outruns the column");
			let from = bar.thumb.center();
			let to = egui::pos2(from.x, (from.y + 60.0).min(track.max.y));
			s.inject(vec![
				egui::Event::PointerMoved(from),
				egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
			]);
			s.shoot();
			s.shoot();
			s.inject(vec![ egui::Event::PointerMoved(to) ]);
			s.shoot();
			s.shoot();
			s.inject(vec![ egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() } ]);
			s.shoot();
			s.shoot();
			let by_bar = s.app.preview_extent_for_tests().expect("still scrollable").1;
			assert!((by_bar - before_bar).abs() > 1.0, "{kind} × scrollbar drag moves the offset: {by_bar} from {before_bar}");
		}

		let _ = std::fs::remove_dir_all(&root);
	}

	/// P38④, the user's ruling (mimeapps.list = the desk's shared ledger, read live, written whole):
/// the default-open resolution takes the person's own file first and the system's behind it, and
/// a change to the ledger — mtime and all, the way another manager writes it — is seen on the
/// very next open, no restart. `Ctrl+Enter`'s merge lands on the same file, whole and civil.
	#[test]
	fn the_mimeapps_ledger_is_read_live_and_the_system_stands_behind_it() {
		let root = std::env::temp_dir().join(format!("crabhop-p38l-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(&root).expect("a sandbox");
		std::fs::write(root.join("note.txt"), b"n").expect("a text file");

		let mut s = session(vec![ format!("reveal {}", root.join("note.txt").display()) ]);
		for _ in 0..600 {
			if s.app.cwd_for_tests() == root {
				break;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		assert_eq!(s.app.cwd_for_tests(), root, "the window stands on the sandbox");

		// the ledgers are the test's own files (P38④'s path seams): the person's level and the
		// system's — never a shared path another test reads, and outside the sandbox's own
		// listing, so the row the walk opens is always the note itself (a directory here would
		// sort first and the walk would open its folder menu instead)
		let ledgers = std::env::temp_dir().join(format!("crabhop-p38l-ledgers-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&ledgers);
		std::fs::create_dir_all(&ledgers).expect("the ledgers' own directory");
		let user = ledgers.join("mimeapps.list");
		let system = ledgers.join("system-mimeapps.list");
		s.app.set_mimeapps_path_for_tests(user.clone());
		s.app.set_mimeapps_system_path_for_tests(system.clone());
		s.app.set_terminal_dry_for_tests();
		s.app.set_open_dry_for_tests();

		// a ledger write moves the mtime, the way another manager's own write does
		let write = |at: &std::path::Path, text: &str, ahead: u64| {
			std::fs::write(at, text).expect("the ledger write");
			let file = std::fs::File::options().write(true).open(at).expect("the ledger");
			file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(ahead)).expect("the mtime moves");
		};
		let enter = |s: &mut Session| {
			s.app.rehearse(vec![ "key Enter".to_owned() ]);
			for _ in 0..300 {
				if s.app.replay_done_for_tests() {
					break;
				}
				std::thread::sleep(Duration::from_millis(8));
				s.shoot();
			}
			for _ in 0..4 {
				s.shoot();
			}
		};
		let terminal = |s: &mut Session| -> Option<crate::app::TerminalLaunch> {
			let mut found = None;
			for _ in 0..600 {
				if s.app.replay_done_for_tests()
					&& let Some(record) = s.app.terminal_spawn_for_tests()
				{
					found = Some(record);
					break;
				}
				std::thread::sleep(Duration::from_millis(8));
				s.shoot();
			}
			found
		};
		let opened = |s: &mut Session| -> Option<std::path::PathBuf> {
			for _ in 0..60 {
				if let Some(path) = s.app.open_spawn_for_tests() {
					return Some(path);
				}
				std::thread::sleep(Duration::from_millis(8));
				s.shoot();
			}
			None
		};

		// the person's ledger names nvim: the terminal chain answers
		write(&user, "[Default Applications]\ntext/plain=nvim.desktop;\n", 1);
		enter(&mut s);
		assert!(terminal(&mut s).is_some(), "the person's own ledger pointed at nvim");

		// another manager rewrites the ledger under the window's feet: chromium answers now —
		// the live read sees it on the very next open (a cache that missed the mtime would
		// still be serving nvim here)
		write(&user, "[Default Applications]\ntext/plain=chromium.desktop;\n", 2);
		enter(&mut s);
		assert!(opened(&mut s).is_some(), "the rewrite was seen without a restart");

		// the person's ledger drops the type: the system's own stands behind it
		write(&user, "[Added Associations]\nimage/png=writer.desktop;\n", 3);
		write(&system, "[Default Applications]\ntext/plain=nvim.desktop;\n", 1);
		enter(&mut s);
		assert!(terminal(&mut s).is_some(), "the system's ledger answered behind the person's");

		// and when both speak, the person's own line wins
		write(&user, "[Default Applications]\ntext/plain=chromium.desktop;\n", 4);
		enter(&mut s);
		assert!(opened(&mut s).is_some(), "the person's line beat the system's");

		// `Ctrl+Enter`'s merge writes the same ledger whole: the type gains its line and every
		// unrelated line stays (a real file, written atomically)
		write(&user, "[Added Associations]\nimage/png=writer.desktop;\ntext/x-other=other.desktop;\n", 5);
		s.app.rehearse(vec![
			"menu 0".to_owned(),
			"key j".to_owned(),
			"key j".to_owned(),
			"key j".to_owned(),
			"key Enter".to_owned(),
			"type nvim".to_owned(),
			"key Down".to_owned(),
			"key C-Enter".to_owned(),
		]);
		for _ in 0..600 {
			if s.app.replay_done_for_tests() {
				break;
			}
			std::thread::sleep(Duration::from_millis(4));
			s.shoot();
		}
		for _ in 0..4 {
			s.shoot();
		}
		let merged = std::fs::read_to_string(&user).expect("the ledger is there");
		assert!(merged.contains("text/plain=nvim.desktop"), "the merge wrote the type: {merged:?}");
		assert!(merged.contains("image/png=writer.desktop;"), "and kept the unrelated line");
		assert!(merged.contains("text/x-other=other.desktop;"), "every one of them");

		let _ = std::fs::remove_dir_all(&root);
		let _ = std::fs::remove_dir_all(&ledgers);
	}

	/// A picture's own scroll stops at the column's top (P27①'s spill, caught by eye on the
	/// evidence frames): with a zoomed picture scrolled up, the tab bar's band holds none of it —
	/// the column's own clip keeps the chrome clean. A picture at the fit is whole inside its
	/// zone and has nothing to scroll (P28①), so the zoom is what makes the wheel mean anything.
	#[test]
	fn a_scrolled_picture_never_reaches_the_tab_bar() {
		// a tall, unmistakably red picture of the test's own
		let at = std::env::temp_dir().join(format!("crabhop-tall-{}.png", std::process::id()));
		image::RgbaImage::from_pixel(120, 900, image::Rgba([ 250, 20, 20, 255 ]))
			.save(&at)
			.expect("the fixture encodes");

		let mut session = session(vec![
			format!("reveal {}", at.display()),
			"hover 900 400".to_owned(),
		]);

		// the picture has to land before any gesture means anything: a zoom fed to an empty column
		// is no zoom at all, and the replay's own cadence is not something a test may lean on
		let mut on_screen = false;
		for _ in 0..240 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
			if session.app.preview_offset_for_tests().is_some() {
				on_screen = true;
				break;
			}
		}
		assert!(on_screen, "the picture has to be on screen");

		// the fit is whole inside its zone (P28①), so the zoom is what makes the picture taller
		// than the column — and the wheel is what walks it
		session.app.rehearse(vec![
			"hover 900 400".to_owned(),
			"zoom in".to_owned(),
			"zoom in".to_owned(),
			"zoom in".to_owned(),
			"wheel -10".to_owned(),
			"wheel -10".to_owned(),
		]);

		// and the wheel has to move it: its own offset is the app's word
		let mut offset = 0.0;
		for _ in 0..240 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
			if let Some(moved) = session.app.preview_offset_for_tests() {
				offset = moved;
				if moved > 0.0 {
					break;
				}
			}
		}

		assert!(offset > 0.0, "the wheel moved the picture's own scroll: {offset}");
		let image = session.shoot();

		let count = |x0: f32, y0: f32, x1: f32, y1: f32| -> usize {
			let mut n = 0;
			for y in (y0.max(0.0) as usize)..(y1.min(image.size[1] as f32) as usize) {
				for x in (x0.max(0.0) as usize)..(x1.min(image.size[0] as f32) as usize) {
					let p = image.pixels[y * image.size[0] + x];
					if p.r() > 150 && p.g() < 80 && p.b() < 120 {
						n += 1;
					}
				}
			}
			n
		};

		let (body, shares) = session.app.layout();
		let columns = chrome::columns(body, shares);
		let preview = columns.preview.expect("all three columns at this size");

		// the picture is on screen — its own column, under the tab bar, is where it shows
		assert!(
			count(preview.min.x, body.min.y + 8.0, preview.max.x, body.min.y + 300.0) > 0,
			"the scrolled picture is up"
		);
		// and the tab bar's band above the body holds none of it
		assert_eq!(count(0.0, 0.0, image.size[0] as f32, chrome::TAB_HEIGHT), 0, "the chrome stays clean");

		let _ = std::fs::remove_file(&at);
	}

	/// `/` opens its box and the list colours the letters the search holds (P21): the strip's own
	/// prefix is accent, and the middle column's list carries more accent with a search live than
	/// without one — where the *letters* are, and only there.
	#[test]
	fn the_slash_paints_its_strip_and_colours_its_matches() {
		let accent = Tokens::MOCHA.accent;
		let count = |image: &egui::ColorImage, x0: f32, y0: f32, x1: f32, y1: f32| -> usize {
			let mut n = 0;
			for y in (y0.max(0.0) as usize)..(y1.min(image.size[1] as f32) as usize) {
				for x in (x0.max(0.0) as usize)..(x1.min(image.size[0] as f32) as usize) {
					if image.pixels[y * image.size[0] + x] == accent {
						n += 1;
					}
				}
			}
			n
		};
		// the strip sits between the body and the status bar; the list's own band is the middle
		// column's upper half (where the rows with a match are)
		let bands = |session: &Session| {
			let (body, shares) = session.app.layout();
			let columns = chrome::columns(body, shares);

			(
				egui::Rect::from_min_max(egui::pos2(0.0, body.max.y + 4.0), egui::pos2(400.0, body.max.y + 24.0)),
				egui::Rect::from_min_max(egui::pos2(columns.current.min.x, body.min.y), egui::pos2(columns.current.max.x, body.min.y + 220.0)),
			)
		};

		let mut plain = session(Vec::new());
		for _ in 0..20 {
			std::thread::sleep(Duration::from_millis(20));
		}
		let plain_image = plain.shoot();
		let (plain_strip, plain_list) = bands(&plain);

		let mut found = session(Vec::new());
		found.app.rehearse(vec![ "key /".to_owned() ]);
		for _ in 0..80 {
			std::thread::sleep(Duration::from_millis(16));
			found.shoot();
			if found.app.find_open_for_tests() {
				break;
			}
		}
		assert!(found.app.find_open_for_tests(), "the box opened");

		// the query goes in next, and the frame that draws its colours is read
		found.app.rehearse(vec![ "type toml".to_owned() ]);
		for _ in 0..40 {
			std::thread::sleep(Duration::from_millis(16));
			found.shoot();
		}
		let found_image = found.shoot();
		let (found_strip, found_list) = bands(&found);

		let strip = |image: &egui::ColorImage, rect: egui::Rect| count(image, rect.min.x, rect.min.y, rect.max.x, rect.max.y);
		assert!(
			strip(&found_image, found_strip) > strip(&plain_image, plain_strip),
			"the find strip is painted: {} → {}",
			strip(&plain_image, plain_strip),
			strip(&found_image, found_strip)
		);
		assert!(
			strip(&found_image, found_list) > strip(&plain_image, plain_list),
			"and the list colours the letters it holds: {} → {}",
			strip(&plain_image, plain_list),
			strip(&found_image, found_list)
		);
	}

	/// The harness paints a frame with the three columns and the status bar in it: the picture is
	/// not empty, and each of the window's own bands has content of its own (P25).
	#[test]
	fn a_headless_frame_holds_the_columns_and_the_status_bar() {
		let mut session = session(Vec::new());

		// let the list land: the first frames read the directory off a worker
		let mut image = session.shoot();
		for _ in 0..30 {
			std::thread::sleep(Duration::from_millis(20));
			image = session.shoot();
		}

		assert_eq!(image.size, [ 1200, 800 ], "the picture is the frame's own size");
		assert!(lit(&image, egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1200.0, 800.0))) > 1000, "the frame is not blank");

		// the bands the app itself laid out: the three columns and the status bar
		let (body, shares) = session.app.layout();
		let columns = chrome::columns(body, shares);
		let middle = egui::Rect::from_min_max(egui::pos2(columns.current.min.x, body.min.y), egui::pos2(columns.current.max.x, body.max.y));

		if let Some(left) = columns.parent {
			assert!(lit(&image, left) > 20, "the parent column has rows: {}", lit(&image, left));
		}
		assert!(lit(&image, middle) > 50, "the middle column has its listing: {}", lit(&image, middle));
		if let Some(right) = columns.preview {
			assert!(lit(&image, right) > 50, "the preview column has something: {}", lit(&image, right));
		}

		let status = egui::Rect::from_min_max(egui::pos2(0.0, 800.0 - STATUS_HEIGHT), egui::pos2(1200.0, 800.0));
		assert!(lit(&image, status) > 20, "the status bar is drawn: {}", lit(&image, status));
	}

	/// `j` moves the cursor (P25): one session stands on the first row, and a session that is
	/// handed `key j` — *after* its listing has landed — stands on the second. The app's cursor says
	/// so, and the accent mark down the middle column's left edge stands a row lower in the picture.
	///
	/// The listing is waited for before any key is fed: the replay's timer and the directory's
	/// worker race, and a `key j` that lands first moves nothing (the orchestrator caught exactly
	/// that on their machine — the green run at hand-off was this machine's luck).
	#[test]
	fn the_cursor_row_moves_with_a_replay_step() {
		let mut first = session(Vec::new());
		assert!(first.wait_for_list(2, 200), "the listing has to land before the test means anything ({} rows)", first.app.listed());
		let first_image = first.shoot();
		assert_eq!(first.cursor(), 0, "where a session with no replay stands");

		// the second session waits for the same listing, and only then is `j` put in its hands
		let mut second = session(Vec::new());
		assert!(second.wait_for_list(2, 200), "the second session's listing");
		second.app.rehearse(vec![ "key j".to_owned() ]);

		// the replay runs at the *end* of a frame: paint until the cursor has moved, then paint the
		// frame that draws it (this one is where the picture and the state agree)
		for _ in 0..50 {
			if second.cursor() == 1 {
				break;
			}
			std::thread::sleep(Duration::from_millis(16));
			second.shoot();
		}
		assert_eq!(second.cursor(), 1, "`key j` walks to the second row of {} rows", second.app.listed());
		let second_image = second.shoot();

		let (before, after) = (
			cursor_row(&first_image, band_x(&first)).expect("the first session's cursor band"),
			cursor_row(&second_image, band_x(&second)).expect("the second session's cursor band"),
		);
		assert!(after > before, "the cursor's row band stands lower: {before} → {after}");
		assert!(
			(after - before) as f32 >= row_height() * 0.8,
			"and it walked a row, not a pixel: {before} → {after} (a row is {})",
			row_height()
		);
	}

	/// One pixel inside the middle column's own left edge (the cursor's band is read there).
	fn band_x(session: &Session) -> usize {
		let (body, shares) = session.app.layout();
		let columns = chrome::columns(body, shares);

		(columns.current.min.x + 6.0) as usize
	}

	/// The column holds the picture and nothing else (P28②'s revision): the info block that used
	/// to stand under it is gone — it moved to the spot card — so the band below a short picture
	/// is bare, where six lines of words used to be.
	#[test]
	fn no_words_stand_under_the_picture_in_the_column() {
		let at = std::env::temp_dir().join(format!("crabhop-p28-notext-{}.png", std::process::id()));
		image::RgbaImage::from_pixel(200, 120, image::Rgba([ 250, 20, 20, 255 ]))
			.save(&at)
			.expect("the fixture encodes");

		let mut session = session(vec![ format!("reveal {}", at.display()) ]);

		// the picture has to be up first, and its own foot is where the red pixels stop
		let mut up = None;
		for _ in 0..240 {
			std::thread::sleep(Duration::from_millis(16));
			let image = session.shoot();
			let (body, shares) = session.app.layout();
			let preview = chrome::columns(body, shares).preview.expect("all three columns at this size");

			let row_lit = |y: usize| {
				(preview.min.x as usize..preview.max.x as usize).any(|x| {
					let p = image.pixels[y * image.size[0] + x];
					p.r() > 150 && p.g() < 80 && p.b() < 80
				})
			};
			let rows: Vec<usize> = (preview.min.y as usize..preview.max.y as usize).filter(|y| row_lit(*y)).collect();
			if let Some(foot) = rows.last() {
				up = Some((preview, *foot, image.size[0]));
				break;
			}
		}
		let (preview, foot, stride) = up.expect("the picture has to be on screen");

		// the badge lives 1.5s after a picture lands: let it die before the band is read
		std::thread::sleep(Duration::from_millis(1700));
		let image = session.shoot();

		let crust = Tokens::MOCHA.crust;
		let mut lit = 0;
		for y in (foot + 4)..(preview.max.y as usize - 16) {
			for x in (preview.min.x as usize + 2)..(preview.max.x as usize - 2) {
				if image.pixels[y * stride + x] != crust {
					lit += 1;
				}
			}
		}
		assert_eq!(lit, 0, "nothing stands under the picture: {lit} pixels");

		let _ = std::fs::remove_file(&at);
	}

	/// The row's own menu (P31), walked headless through its pages: a file's one item, a folder's
	/// two, the applications that handle a directory's type, the two answers of the confirm — and
	/// Esc backing out one page at a time.
	#[test]
	fn the_rows_menu_walks_its_pages() {
		let dir = std::env::temp_dir().join(format!("crabhop-menu-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(dir.join("sub")).expect("a directory");
		let at = dir.join("a.png");
		image::RgbaImage::from_pixel(40, 30, image::Rgba([ 200, 30, 30, 255 ]))
			.save(&at)
			.expect("the fixture encodes");

		let mut session = session(vec![ format!("reveal {}", at.display()) ]);

		// the fixture's own directory has two rows: the landing is when both show
		let mut landed = false;
		for _ in 0..240 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
			if session.app.listed() == 2 {
				landed = true;
				break;
			}
		}
		assert!(landed, "the fixture's own directory has to land");

		let step = |session: &mut Session, want: Option<(&str, usize)>| -> Option<(&'static str, usize)> {
			for _ in 0..60 {
				std::thread::sleep(Duration::from_millis(16));
				session.shoot();
				let state = session.app.menu_state_for_tests();
				if state == want {
					return state;
				}
			}
			session.app.menu_state_for_tests()
		};

		// the file's own row (directories sort first, so it is the second row): the file family and
		// the applications — four rows (P33⑱'s roster) — and Esc puts the menu away
		session.app.rehearse(vec![ "menu 1".to_owned() ]);
		assert_eq!(step(&mut session, Some(("main", 4))), Some(("main", 4)), "a file's menu is four rows");
		session.app.rehearse(vec![ "key Esc".to_owned() ]);
		assert_eq!(step(&mut session, None), None);

		// the folder's own row: a terminal and the applications
		session.app.rehearse(vec![ "reveal ".to_owned() + dir.join("sub").to_str().unwrap() ]);
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
		}
		session.app.rehearse(vec![ "menu 0".to_owned() ]);
		assert_eq!(step(&mut session, Some(("main", 5))), Some(("main", 5)), "a folder's menu has five");

		// `j` walks down to the applications — past the family and the terminal — and Enter takes it
		session.app.rehearse(vec![ "key j".to_owned(), "key j".to_owned(), "key j".to_owned(), "key j".to_owned(), "key Enter".to_owned() ]);
		let apps = {
			let mut state = None;
			for _ in 0..60 {
				std::thread::sleep(Duration::from_millis(16));
				session.shoot();
				state = session.app.menu_state_for_tests();
				if state.is_some_and(|(page, _)| page == "apps") {
					break;
				}
			}
			state
		};
		assert!(
			apps.is_some_and(|(page, rows)| page == "apps" && rows >= 1),
			"the directory's applications page: {apps:?} (this machine has none?"
		);

		// (P37③: Enter on the applications list launches the standing row at once — there is no
		// confirm page to walk to any more. The launch's own door is asserted in the open-with E2E,
		// whose pick is a `Terminal=true` entry and so hears the dry record instead of a window.)

		// Esc backs out a page at a time, and closes the last one
		session.app.rehearse(vec![ "key Esc".to_owned() ]);
		assert_eq!(step(&mut session, Some(("main", 5))), Some(("main", 5)), "Esc steps back to the items");
		session.app.rehearse(vec![ "key Esc".to_owned() ]);
		assert_eq!(step(&mut session, None), None, "and the last Esc puts it away");

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// An opening that failed is said out loud (P30): the same message a refusing launcher sends
	/// raises the warn toast that names the file, and the frame carries its words.
	#[test]
	fn a_failed_opening_raises_the_toast() {
		let mut session = session(Vec::new());
		assert!(session.wait_for_list(2, 200), "the listing has to land first");

		session.app.open_failed_for_tests(std::path::PathBuf::from("/tmp/crabhop-n2-never/a.zzz"));

		let mut said = None;
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
			said = session.app.toast_text_for_tests();
			if said.is_some() {
				break;
			}
		}
		assert_eq!(said.as_deref(), Some("没有设置打开 a.zzz 的程序"));

		// and the toast is on the frame: its warn-coloured face stands in the corner it lives in
		let tokens = Tokens::MOCHA;
		let image = session.shoot();
		let (body, shares) = session.app.layout();
		let _ = (body, shares);
		let warn = tokens.warn;
		let said_on_screen = image
			.pixels
			.iter()
			.filter(|pixel| {
				(pixel.r() as i32 - warn.r() as i32).abs() < 30
					&& (pixel.g() as i32 - warn.g() as i32).abs() < 30
					&& (pixel.b() as i32 - warn.b() as i32).abs() < 40
			})
			.count();
		assert!(said_on_screen > 0, "the toast's own colour is on the frame: {said_on_screen} pixels");
	}

	/// The archive tree folds under the pointer (P28's long tail): a Return with the pointer on a
	/// folder's row opens it — the folder's own file comes out — where a Return anywhere else is
	/// the window's own `open`, which the pane's own test covers for the file rows.
	#[test]
	fn an_archive_folder_opens_under_the_pointer() {
		use std::io::Write;

		let dir = std::env::temp_dir().join(format!("crabhop-p28-fold-harness-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");

		let zip = dir.join("tree.zip");
		let file = std::fs::File::create(&zip).expect("a zip file");
		{
			let mut zip = zip::ZipWriter::new(file);
			let options: zip::write::FileOptions<'_, ()> =
				zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Stored);

			zip.add_directory("docs/", options).unwrap();
			zip.add_directory("docs/deep/", options).unwrap();
			zip.start_file("docs/a.txt", options).unwrap();
			zip.write_all(b"hello").unwrap();
			zip.start_file("docs/deep/b.bin", options).unwrap();
			zip.write_all(&[ 0u8; 12 ]).unwrap();
			zip.start_file("top.md", options).unwrap();
			zip.write_all(b"# hi").unwrap();
			zip.finish().unwrap();
		}

		let mut session = session(vec![ format!("reveal {}", zip.display()) ]);

		// the tree has to land before a gesture means anything
		let mut rows = None;
		for _ in 0..240 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
			if let Some(count) = session.app.preview_archive_rows_for_tests() {
				rows = Some(count);
				break;
			}
		}
		assert_eq!(rows, Some(4), "the first level opens, deep stays shut");

		// the pointer goes to the shut folder's own row — read from the tree, not from arithmetic
		let tree = session.app.preview_archive_tree_for_tests().expect("the rows are laid out");
		let (_, rect, _) = *tree.iter().find(|(at, _, dir)| *at == 1 && *dir).expect("the folder's row");
		let at = rect.center();

		session.app.rehearse(vec![ format!("hover {} {}", at.x, at.y), "key Enter".to_owned() ]);
		let mut opened = None;
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
			opened = session.app.preview_archive_rows_for_tests();
			if opened == Some(5) {
				break;
			}
		}
		assert_eq!(opened, Some(5), "the folder under the pointer opened");

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// Ctrl+S in the index panel saves what the box holds when what it holds is a path — a file
	/// included, and not the file's parent; a bare search word still saves the directory the
	/// window stands in. The save writes state, so the test points the state dir at a directory
	/// of its own for as long as it runs.
	#[test]
	fn the_index_saves_what_the_box_holds_and_not_its_parent() {
		let tmp = std::env::temp_dir().join(format!("crabhop-index-{}", std::process::id()));
		crate::config::state_dir_for_tests(tmp.clone());

		let open = |session: &mut Session| {
			session.app.rehearse(vec![ "key i".to_owned() ]);
			for _ in 0..40 {
				std::thread::sleep(Duration::from_millis(16));
				session.shoot();
				if session.app.fuzzy_query_for_tests().is_some() {
					return;
				}
			}
		};
		let settle = |session: &mut Session| {
			for _ in 0..80 {
				std::thread::sleep(Duration::from_millis(16));
				session.shoot();
				if !session.app.bookmarked_paths().is_empty() {
					return;
				}
			}
		};

		let mut held = session(Vec::new());
		open(&mut held);
		assert!(held.app.fuzzy_query_for_tests().is_some(), "the index panel opened");
		let dir = std::env::temp_dir().join(format!("crabhop-index-files-{}", std::process::id()));
		std::fs::create_dir_all(&dir).expect("a dir of the test's own");
		std::fs::write(dir.join("note.txt"), b"x").expect("a file of the test's own");
		// the box takes the keyboard only after a second i (P33⑬): the typing goes to the list first
		held.app.rehearse(vec![ "key i".to_owned(), format!("type {}", dir.join("note.txt").display()), "key C-s".to_owned() ]);
		settle(&mut held);
		let saved = held.app.bookmarked_paths();
		assert_eq!(saved.len(), 1, "one index was saved: {saved:?}");
		assert!(saved[0].is_file(), "the box held a file, so the file is the index, not its parent: {:?}", saved[0]);
		assert_eq!(saved[0].file_name().and_then(|name| name.to_str()), Some("note.txt"));

		let mut word = session(Vec::new());
		open(&mut word);
		word.app.rehearse(vec![ "key i".to_owned(), "type note".to_owned(), "key C-s".to_owned() ]);
		for _ in 0..80 {
			std::thread::sleep(Duration::from_millis(16));
			word.shoot();
			if word.app.bookmarked_paths().iter().any(|path| path.is_dir()) {
				break;
			}
		}
		let saved = word.app.bookmarked_paths();
		let last = saved.last().expect("the word's save lands last");
		assert!(last.is_dir(), "a search word saves the directory the window stands in: {last:?}");

		let _ = std::fs::remove_dir_all(&dir);
		let _ = std::fs::remove_dir_all(&tmp);
	}

	/// The index panel's focus machine (P33⑬): the same two-stage one the help panel has — the list
	/// first, `i` again into the box (the user's final ruling: press `i` twice and you are typing),
	/// Esc back out, a second Esc to close. The replay's steps stand 350ms apart, so each wait below
	/// is longer than one step.
	#[test]
	fn the_index_opens_on_its_list_and_f_steps_into_the_box() {
		let wait = |s: &mut Session, frames: usize| {
			for _ in 0..frames {
				std::thread::sleep(Duration::from_millis(16));
				s.shoot();
			}
		};

		let mut s = session(Vec::new());
		s.app.rehearse(vec![ "key i".to_owned() ]);
		for _ in 0..40 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if s.app.fuzzy_query_for_tests().is_some() {
				break;
			}
		}
		assert_eq!(s.app.fuzzy_focus_for_tests(), Some(false), "the panel opens with the list holding the keyboard");

		// a word typed on the list types nothing (P33⑬): one step, then time for it to have run
		s.app.rehearse(vec![ "type 回收".to_owned() ]);
		wait(&mut s, 40);
		assert_eq!(s.app.fuzzy_query_for_tests().as_deref(), Some(""), "the list does not feed the box");
		assert_eq!(s.app.fuzzy_focus_for_tests(), Some(false), "and the list still has the keyboard");

		// a second `i` steps in; the same word now filters
		s.app.rehearse(vec![ "key i".to_owned(), "type 回收".to_owned() ]);
		wait(&mut s, 80);
		assert_eq!(s.app.fuzzy_focus_for_tests(), Some(true), "the second i puts the keyboard in the box");
		assert_eq!(s.app.fuzzy_query_for_tests().as_deref(), Some("回收"), "and the box holds what was typed");

		// Esc steps out of the box; the panel stands; a second Esc closes it
		s.app.rehearse(vec![ "key Escape".to_owned() ]);
		wait(&mut s, 40);
		assert_eq!(s.app.fuzzy_focus_for_tests(), Some(false), "the first Esc steps back to the list");
		assert_eq!(s.app.fuzzy_query_for_tests().as_deref(), Some("回收"), "the box keeps its words");
		s.app.rehearse(vec![ "key Escape".to_owned() ]);
		wait(&mut s, 40);
		assert_eq!(s.app.fuzzy_query_for_tests(), None, "the second Esc closes the panel");
	}

	/// The column and sort families still act (P33⑭'s guard, now under P33⑲'s model): the chords
	/// answer, and what they answer with is the column set. `m p` adds a column without touching
	/// the others, `m o` stacks on top of it, `m b` replaces the time, `m n` resets, and `,m` makes
	/// the time stand without ever toggling it away.
	#[test]
	fn the_column_and_sort_families_still_act() {
		use crate::keys::{Col, Columns, ColumnsDo, SortBy, TimeCol};

		let wait = |s: &mut Session, frames: usize| {
			for _ in 0..frames {
				std::thread::sleep(Duration::from_millis(16));
				s.shoot();
			}
		};

		let mut s = session(Vec::new());
		assert_eq!(s.app.columns_for_tests(), Columns::default(), "the window opens on the default set: size and modification time");
		assert_eq!(s.app.sort_for_tests(), (SortBy::Name, false), "and on the name, ascending");

		// ② the toggles stack: m p adds the permissions, m O the owner (P36④'s case-shift), and
		// neither touches the rest
		s.app.rehearse(vec![ "key m".to_owned(), "key p".to_owned() ]);
		wait(&mut s, 60);
		assert_eq!(
			s.app.columns_for_tests(),
			Columns { time: Some(TimeCol::Mtime), perms: true, owner: false },
			"m p added the permissions column"
		);
		s.app.rehearse(vec![ "key m".to_owned(), "key O".to_owned() ]);
		wait(&mut s, 60);
		assert_eq!(
			s.app.columns_for_tests(),
			Columns { time: Some(TimeCol::Mtime), perms: true, owner: true },
			"m O stacked the owner on top (P36④'s case-shift from m o): three columns stand together"
		);

		// ③ the time pair replaces, never piles: m b takes the modification time's place
		s.app.rehearse(vec![ "key m".to_owned(), "key b".to_owned() ]);
		wait(&mut s, 60);
		assert_eq!(
			s.app.columns_for_tests(),
			Columns { time: Some(TimeCol::Btime), perms: true, owner: true },
			"m b replaced the modification time and left the switches alone"
		);
		s.app.rehearse(vec![ "key m".to_owned(), "key b".to_owned() ]);
		wait(&mut s, 60);
		assert_eq!(
			s.app.columns_for_tests(),
			Columns { time: None, perms: true, owner: true },
			"and a second m b closes the time column"
		);

		// ⑤ m n is the one-key reset back to the default set
		s.app.rehearse(vec![ "key m".to_owned(), "key n".to_owned() ]);
		wait(&mut s, 60);
		assert_eq!(s.app.columns_for_tests(), Columns::default(), "m n reset the columns");

		// ⑦ the sort family's verb is show: ,m makes the time stand (already the default here) and
		// ,M reverses the sort without touching the columns
		s.app.rehearse(vec![ "key ,".to_owned(), "key m".to_owned() ]);
		wait(&mut s, 60);
		assert_eq!(s.app.sort_for_tests(), (SortBy::Mtime, false), ",m still sorts by modification time");
		assert_eq!(s.app.columns_for_tests(), Columns::default(), "and its show mtime is idempotent");
		s.app.rehearse(vec![ "key ,".to_owned(), "key M".to_owned() ]);
		wait(&mut s, 60);
		assert_eq!(s.app.sort_for_tests(), (SortBy::Mtime, true), ",M still reverses it");

		// the old verb names parse to the same actions — the keys' own test holds that (a keymap
		// from before the column model still loads); here only that the imports all answer
		let _ = (Col::Owner, ColumnsDo::Reset);
	}

	/// P33⑮: the `+` stands beside the last chip and steps right with every tab — the browser's own
	/// habit, measured (the frames are kept separately, in p33/).
	#[test]
	fn the_plus_follows_the_last_chip() {
		let mut s = session(Vec::new());
		s.shoot();
		let one = s.app.plus_hit_for_tests().expect("the strip drew its plus");
		let close = s.app.close_hit_for_tests().expect("and the corner drew its close");

		// two creations, which are `t t` now (P33④): each is a chord of its own
		s.app.rehearse(vec![ "key t".to_owned(), "key t".to_owned(), "key t".to_owned(), "key t".to_owned() ]);
		for _ in 0..80 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if s.app.tabs_for_tests() >= 3 {
				break;
			}
		}
		assert_eq!(s.app.tabs_for_tests(), 3, "two presses, three tabs");
		let three = s.app.plus_hit_for_tests().expect("the plus is still there");
		let stepped = three.min.x - one.min.x;
		assert!(stepped > 100.0, "the plus stepped right by two chips' worth: {stepped}px");
		assert!(three.center().x < close.center().x, "and it stays clear of the corner's close");
		assert_eq!(one.center().y, three.center().y, "the strip's own line, never a new one");
	}

	/// P33⑯: a press on the corner's × asks the window to close — the harness catches the very
	/// command the frame sent, no question and no confirmation in between.
	#[test]
	fn the_corner_close_asks_the_window_to_close() {
		let mut s = session(Vec::new());
		s.shoot();
		let close = s.app.close_hit_for_tests().expect("the strip drew its close");
		assert!(!s.close_requested(), "nothing has asked yet");

		s.click_at(close.center());
		s.shoot();
		assert!(s.close_requested(), "the press sent the close command");
	}

	/// P33⑰: the status bar's own 索引 button opens the panel the `i` key opens — the same door,
	/// Action::Index's arm — wears its accent while the panel stands, and a second press is the way
	/// out. The key's own door is checked beside it, so the two are known to be one.
	#[test]
	fn the_status_bar_opens_the_index_and_says_so() {
		let mut s = session(Vec::new());
		s.shoot();
		let button = s.app.status_index_hit_for_tests().expect("the status bar drew its index button");
		assert!(!s.app.index_open(), "no panel yet");

		s.click_at(button.center());
		for _ in 0..8 {
			s.shoot();
		}
		assert!(s.app.index_open(), "the click opened the index panel");
		assert!(s.app.fuzzy_query_for_tests().is_some(), "and a fuzzy panel is what stands");

		s.click_at(button.center());
		for _ in 0..8 {
			s.shoot();
		}
		assert!(!s.app.index_open() && s.app.fuzzy_query_for_tests().is_none(), "the second click closed it");

		// the key's own door is the same one: `i` opens it, and Esc closes it — the list holding
		// the keyboard, so the first Esc is the panel's own
		s.app.rehearse(vec![ "key i".to_owned() ]);
		for _ in 0..40 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if s.app.index_open() {
				break;
			}
		}
		assert!(s.app.index_open(), "i opens the same panel");
		s.app.rehearse(vec![ "key Escape".to_owned() ]);
		for _ in 0..40 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if !s.app.index_open() {
				break;
			}
		}
		assert!(!s.app.index_open(), "and Esc closes it");
	}

	/// P33⑱, the user's own rule measured: right-clicking a folder offers paste, and its landing
	/// point is the window's own directory — the folder's parent, beside it — never inside the
	/// folder that was right-clicked. The file comes from one level up, so the two are told apart.
	#[test]
	fn the_menu_pastes_beside_the_folder_it_was_opened_on() {
		let dir = std::env::temp_dir().join(format!("crabhop-menu-paste-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(dir.join("dst").join("sub")).expect("a sandbox of the test's own");
		std::fs::write(dir.join("src.txt"), b"x").expect("a file of the test's own");

		let wait = |session: &mut Session, frames: usize| {
			for _ in 0..frames {
				std::thread::sleep(Duration::from_millis(16));
				session.shoot();
			}
		};

		let mut session = session(Vec::new());
		session.app.rehearse(vec![ format!("reveal {}", dir.join("src.txt").display()) ]);
		wait(&mut session, 40);
		assert_eq!(session.app.cwd_for_tests(), dir, "the reveal stood in the file's own directory");
		session.app.rehearse(vec![ "key y".to_owned() ]);
		for _ in 0..40 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
			if !session.app.yank_paths_for_tests().is_empty() {
				break;
			}
		}
		assert_eq!(session.app.yank_paths_for_tests(), vec![ dir.join("src.txt") ], "y held the file");

		session.app.rehearse(vec![ format!("reveal {}", dir.join("dst").display()), "key l".to_owned() ]);
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
			if session.app.cwd_for_tests() == dir.join("dst") {
				break;
			}
		}
		assert_eq!(session.app.cwd_for_tests(), dir.join("dst"), "the window stands in dst");

		// the folder's own row, right-clicked: the paste stands three rows down (copy, cut, paste)
		session.app.rehearse(vec![ "menu 0".to_owned(), "key j".to_owned(), "key j".to_owned(), "key Enter".to_owned() ]);
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
			if dir.join("dst").join("src.txt").exists() {
				break;
			}
		}
		assert!(dir.join("dst").join("src.txt").is_file(), "the paste landed in the window's own directory");
		assert!(
			!dir.join("dst").join("sub").join("src.txt").exists(),
			"and never inside the folder the menu was opened on"
		);

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// P33⑱: a right click inside a selection copies the whole selection — the menu's copy calls
	/// the same `yank` the `y` key does, whose targets are the selection when one is held.
	#[test]
	fn the_menu_copies_the_selection_when_one_is_held() {
		let dir = std::env::temp_dir().join(format!("crabhop-menu-copy-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(dir.join("two")).expect("a sandbox of the test's own");
		std::fs::write(dir.join("two").join("a.txt"), b"x").expect("a file of the test's own");
		std::fs::write(dir.join("two").join("b.txt"), b"x").expect("another");

		let mut session = session(Vec::new());
		session.app.rehearse(vec![ format!("reveal {}", dir.join("two").join("a.txt").display()) ]);
		for _ in 0..40 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
		}
		assert_eq!(session.app.cwd_for_tests(), dir.join("two"), "the reveal stood in the pair's directory");

		// two rows held (no factory key toggles a selection — the person writes their own), then a
		// right click on the first of them: the menu's own copy is its first row
		let (a, b) = (dir.join("two").join("a.txt"), dir.join("two").join("b.txt"));
		session.app.hold_selection_for_tests(vec![ a.clone(), b.clone() ]);
		session.app.rehearse(vec![ "menu 0".to_owned(), "key Enter".to_owned() ]);
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(16));
			session.shoot();
			if session.app.yank_paths_for_tests().len() == 2 {
				break;
			}
		}

		let mut want = vec![ a, b ];
		want.sort();
		assert_eq!(session.app.yank_paths_for_tests(), want, "the menu's copy took the whole selection");

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// P33⑲'s own pixel word: the columns are drawn, not merely held. Opening the permissions and
	/// the owner puts their glyphs into the fact band, and the picture says so — the frames the
	/// replay keeps (p33/columns-*.png) are this assertion's archive.
	#[test]
	fn the_fact_band_widens_as_the_columns_open() {
		use crate::keys::{Columns, TimeCol};

		let base = Tokens::MOCHA.base;
		let ink = |image: &egui::ColorImage, rect: egui::Rect| -> usize {
			let mut n = 0;
			for y in (rect.min.y.max(0.0) as usize)..(rect.max.y.min(image.size[1] as f32) as usize) {
				for x in (rect.min.x.max(0.0) as usize)..(rect.max.x.min(image.size[0] as f32) as usize) {
					if image.pixels[y * image.size[0] + x] != base {
						n += 1;
					}
				}
			}
			n
		};

		let mut s = session(Vec::new());
		for _ in 0..30 {
			std::thread::sleep(Duration::from_millis(20));
			s.shoot();
		}
		let before = s.shoot();
		let (body, shares) = s.app.layout();
		let current = chrome::columns(body, shares).current;

		s.app.rehearse(vec![ "key m".to_owned(), "key p".to_owned(), "key m".to_owned(), "key O".to_owned() ]);
		for _ in 0..120 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
			if s.app.columns_for_tests() == (Columns { time: Some(TimeCol::Mtime), perms: true, owner: true }) {
				break;
			}
		}
		assert_eq!(
			s.app.columns_for_tests(),
			Columns { time: Some(TimeCol::Mtime), perms: true, owner: true },
			"three columns stand"
		);
		let after = s.shoot();

		let (before_ink, after_ink) = (ink(&before, current), ink(&after, current));
		assert!(
			after_ink > before_ink,
			"the fact band carries more ink with the columns open: {before_ink} → {after_ink}"
		);
	}

	/// P33④'s own guard, the user's named danger: once `t` has opened its chord, the second key is
	/// the family's alone. `q` closes the tab and never reaches the global quit; `Esc` cancels the
	/// chord so a later `q` is global again; and a bare `q` still quits.
	#[test]
	fn the_t_chord_owns_its_second_key() {
		let wait = |s: &mut Session, frames: usize| {
			for _ in 0..frames {
				std::thread::sleep(Duration::from_millis(16));
				s.shoot();
			}
		};

		// a bare q, with no chord under it, is the global quit
		let mut plain = session(Vec::new());
		plain.shoot();
		assert!(!plain.close_requested(), "nothing has asked yet");
		plain.app.rehearse(vec![ "key q".to_owned() ]);
		wait(&mut plain, 60);
		assert!(plain.close_requested(), "a bare q quits");

		// t t makes a tab, t q closes it — and that q is the family's own, not the quit
		let mut chord = session(Vec::new());
		chord.shoot();
		let tabs = chord.app.tabs_for_tests();
		chord.app.rehearse(vec![ "key t".to_owned(), "key t".to_owned() ]);
		wait(&mut chord, 60);
		assert_eq!(chord.app.tabs_for_tests(), tabs + 1, "t t opened a tab");
		chord.app.rehearse(vec![ "key t".to_owned(), "key q".to_owned() ]);
		wait(&mut chord, 60);
		assert_eq!(chord.app.tabs_for_tests(), tabs, "t q closed the tab");
		assert!(!chord.close_requested(), "and the q inside the chord never reached the global quit");

		// Esc cancels the chord without acting: the next key is global again, and that q quits
		let mut cancelled = session(Vec::new());
		cancelled.shoot();
		cancelled.app.rehearse(vec![ "key t".to_owned(), "key Escape".to_owned(), "key q".to_owned() ]);
		wait(&mut cancelled, 100);
		assert!(!cancelled.app.saw_sequence_for_tests(), "the chord is gone");
		assert!(cancelled.close_requested(), "after Esc cancelled the chord, q is the global quit again");
	}

	/// P34's own rendering guard, from the orchestrator's eyes on the frame: in the middle column a
	/// row's whole name must be drawn, character by character. The sandbox's names are all the
	/// same length and every extension is a different kind, so each row of the column can be
	/// measured against egui's own layout its own name — and a missing letter is exactly one cell
	/// without ink.
	#[test]
	fn the_middle_column_draws_the_whole_name_beside_a_table_icon() {
		use crate::tokens::{FONT_SIZE, ICON_BOX};

		// twenty-eight names, seven characters each, one extension apiece — the table's own kinds
		// and one it does not know (the fallback), every one of them the same width
		let exts = [
			"png", "jpg", "gif", "svg", "webp", "mp3", "flac", "ogg", "mp4", "mkv", "mov", "zip", "tar", "gz", "7z", "pdf",
			"doc", "docx", "xls", "csv", "md", "txt", "json", "toml", "yaml", "rs", "py", "zzz",
		];
		let names: Vec<String> = exts.iter().enumerate().map(|(i, ext)| format!("a{i:02}.{ext}")).collect();

		let dir = std::env::temp_dir().join(format!("crabhop-icon-name-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a sandbox of the test's own");
		for name in &names {
			std::fs::write(dir.join(name), b"x").expect("a file of the test's own");
		}

		let mut s = session(Vec::new());
		s.app.rehearse(vec![ format!("reveal {}", dir.join(&names[0]).display()) ]);
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
		}
		assert_eq!(s.app.cwd_for_tests(), dir, "the reveal stood in the sandbox");

		let font = egui::FontId::new(FONT_SIZE, egui::FontFamily::Monospace);
		let advance = s.ctx.fonts_mut(|fonts| fonts.layout_no_wrap("0".to_owned(), font.clone(), egui::Color32::WHITE).size().x);

		// each round reads the rows from where the list actually stands: the replay's `scroll`
		// clamps at the column's own last page, so the top row is the offset, not the number asked
		let inspect = |s: &mut Session, want: &[String]| {
			let first = s.app.scroll_offset_for_tests();
			let image = s.shoot();
			let (body, shares) = s.app.layout();
			let current = chrome::columns(body, shares).current;
			let name_start = current.min.x + ICON_BOX + 2.0 * advance;

			let mut missing: Vec<String> = Vec::new();
			for (i, name) in want.iter().enumerate() {
				let top = current.min.y + i as f32 * ROW_HEIGHT;
				if top + ROW_HEIGHT > current.max.y {
					break;
				}
				// each character of the name owns one cell: ink must stand in every one of them
				for (at, ch) in name.chars().enumerate() {
					let from = name_start + at as f32 * advance + 1.0;
					let to = name_start + (at + 1) as f32 * advance - 1.0;
					let mut lit = 0usize;
					for x in (from as usize)..(to as usize).min(image.size[0]) {
						for y in (top + 4.0) as usize..(top + ROW_HEIGHT - 2.0) as usize {
							let p = image.pixels[y * image.size[0] + x];
							if p.r() as u32 + p.g() as u32 + p.b() as u32 > 620 {
								lit += 1;
							}
						}
					}
					if lit == 0 {
						missing.push(format!("row {} ({name}): the {at}th character '{ch}' has no ink", first + i));
					}
				}
			}
			assert!(missing.is_empty(), "characters with no ink in the middle column's names:\n{}", missing.join("\n"));
		};

		inspect(&mut s, &names);

		// and the same again with the list scrolled, which is where the eyes caught it: the four
		// rows the scroll brings in are the four to read
		s.app.rehearse(vec![ "scroll 24".to_owned() ]);
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
		}
		let offset = s.app.scroll_offset_for_tests();
		let visible: Vec<String> = names[offset..].to_vec();
		inspect(&mut s, &visible);

		// and the same reading with the names the eyes had: long ones — ten to thirteen characters,
		// the lengths where a missing letter is likeliest — with the same extensions, scrolled by
		// the six rows the evidence frame's own replay asked for
		let long: Vec<String> = [
			"settings.yaml", "engine.cpp", "bundle.zip", "script.sh", "dockerfile", "cargo.toml", "makefile", "license",
			"index.js", "init.lua", "main.go", "main.rs", "mod.ts", "notes.md", "page.html", "paper.pdf", "photo.png",
			"readme.txt", "sheet.xlsx", "song.mp3", "data.json", "config.toml", "font.ttf", "clip.mp4", "app.py", "lib.c",
			"mystery.zzz", "a.long.markdown",
		]
		.iter()
		.map(|name| (*name).to_owned())
		.collect();
		// the column's own order is the names ascending — the table below is read in that order, so
		// the list must be in it too
		let mut long: Vec<String> = long;
		long.sort();
		let dir2 = std::env::temp_dir().join(format!("crabhop-icon-long-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir2);
		std::fs::create_dir_all(&dir2).expect("a second sandbox of the test's own");
		for name in &long {
			std::fs::write(dir2.join(name), b"x").expect("a file of the test's own");
		}

		let mut s = session(Vec::new());
		s.app.rehearse(vec![ format!("reveal {}", dir2.join("main.rs").display()) ]);
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
		}
		s.app.rehearse(vec![ "scroll 6".to_owned() ]);
		for _ in 0..60 {
			std::thread::sleep(Duration::from_millis(16));
			s.shoot();
		}
		let offset = s.app.scroll_offset_for_tests();
		let visible: Vec<String> = long[offset..].to_vec();
		inspect(&mut s, &visible);

		let _ = std::fs::remove_dir_all(&dir);
		let _ = std::fs::remove_dir_all(&dir2);
	}
}
