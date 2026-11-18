// Wayland drag, both directions, on the window's own connection. winit 0.30 is mute on
// wl_data_device, so this module rides the window's wl_display with a second event queue
// (smithay-clipboard's trick) and pumps it from the UI thread. Only the pipe reads and
// writes get a thread, so a frozen peer can never stall a frame — and a drag from our
// own window onto itself cannot deadlock.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::io::{AsFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle};
use wayland_client::{
	backend::{Backend, ObjectId},
	globals::{registry_queue_init, GlobalListContents},
	protocol::{
		wl_buffer::WlBuffer,
		wl_compositor::WlCompositor,
		wl_data_device::{self, WlDataDevice},
		wl_data_device_manager::{DndAction, WlDataDeviceManager},
		wl_data_offer::{self, WlDataOffer},
		wl_data_source::{self, WlDataSource},
		wl_pointer::{self, ButtonState, WlPointer},
		wl_registry::{self, WlRegistry},
		wl_seat::{self, Capability, WlSeat},
		wl_shm::{self, WlShm},
		wl_shm_pool::WlShmPool,
		wl_surface::WlSurface,
	},
	Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum,
};

/// A uri-list bigger than this is not a file drag, it's a mistake or an attack.
const MAX_LIST: u64 = 16 * 1024 * 1024;

pub(crate) struct Dnd {
	conn:    Connection,
	queue:   EventQueue<State>,
	state:   State,
	read_rx: Receiver<Vec<u8>>,
}

/// What became of a drag we started.
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
	/// The target took the files as a move: the originals are now duplicates and must go.
	Moved(Vec<PathBuf>),
	/// A copy landed, or the drag was cancelled — nothing to clean up.
	Ended,
}

/// Everything the app needs to hear about, both directions.
pub(crate) enum Event {
	DragEnded(Outcome),
	/// An external drag is over our window.
	Hovered,
	/// It left without dropping.
	Left,
	/// Files landed; cut=true means the peer negotiated a move.
	Dropped { paths: Vec<PathBuf>, cut: bool },
}

struct State {
	surface: WlSurface,
	seat:    Option<WlSeat>,
	manager: Option<WlDataDeviceManager>,
	device:  Option<WlDataDevice>,
	pointer: Option<WlPointer>,
	shm:     Option<WlShm>,
	compositor: Option<WlCompositor>,
	argb8888: bool,
	on_us:   bool,
	serial:  Option<u32>,
	drag:    Option<Drag>,
	outcome: Option<Outcome>,
	offers:   HashMap<ObjectId, OfferInfo>,
	incoming: Option<WlDataOffer>,
	reading:  Option<(WlDataOffer, DndAction)>,
	shift:    bool,
	read_tx:  Sender<Vec<u8>>,
	events:   Vec<Event>,
}

struct Drag {
	source:   WlDataSource,
	paths:    Vec<PathBuf>,
	uri_list: String,
	text:     String,
	action:   DndAction,
	icon:     Option<Icon>,
	dropped_at: Option<Instant>,
}

/// The little page that rides the pointer during an outgoing drag. The compositor
/// moves it with the cursor — that is the whole animation.
struct Icon {
	surface: WlSurface,
	buffer:  WlBuffer,
	pool:    WlShmPool,
}

struct OfferInfo {
	mimes:          Vec<String>,
	source_actions: DndAction,
	action:         DndAction,
}

impl OfferInfo {
	fn new() -> Self {
		Self { mimes: Vec::new(), source_actions: DndAction::empty(), action: DndAction::empty() }
	}
}

impl Dnd {
	/// Attach to the window's own display. None on X11 (XDND is another protocol) or
	/// when the compositor lacks data-device v3 — the app then simply has no drag.
	pub(crate) fn attach(frame: &eframe::Frame) -> Option<Dnd> {
		let (RawDisplayHandle::Wayland(display), RawWindowHandle::Wayland(window)) =
			(frame.display_handle().ok()?.as_raw(), frame.window_handle().ok()?.as_raw())
		else {
			return None;
		};

		// Safety: winit keeps this display and surface alive for the whole run, and the
		// App that owns the returned Dnd is dropped before the window goes away.
		let backend = unsafe { Backend::from_foreign_display(display.display.as_ptr().cast()) };
		let conn = Connection::from_backend(backend);
		let id = unsafe { ObjectId::from_ptr(WlSurface::interface(), window.surface.as_ptr().cast()) }.ok()?;
		let surface = WlSurface::from_id(&conn, id).ok()?;

		let (globals, mut queue) = registry_queue_init::<State>(&conn).ok()?;
		let qh = queue.handle();
		let (read_tx, read_rx) = mpsc::channel();
		let mut state = State {
			surface,
			seat:    None,
			manager: None,
			device:  None,
			pointer: None,
			shm:     None,
			compositor: None,
			argb8888: false,
			on_us:   false,
			serial:  None,
			drag:    None,
			outcome: None,
			offers:   HashMap::new(),
			incoming: None,
			reading:  None,
			shift:    false,
			read_tx,
			events: Vec::new(),
		};

		// v3 is the floor: the action / dnd_finished events that carry move semantics
		// only exist since 3.
		state.manager = Some(globals.bind(&qh, 3..=3, ()).ok()?);
		// shortcut: first seat only — a multi-seat rig drags with the first seat; upgrade if a bug report ever comes.
		state.seat = Some(globals.bind::<WlSeat, _, _>(&qh, 1..=5, ()).ok()?);
		state.shm = globals.bind(&qh, 1..=1, ()).ok(); // optional: no shm → drag without an icon
		state.compositor = globals.bind(&qh, 1..=7, ()).ok();
		queue.roundtrip(&mut state).ok()?; // capabilities arrive → pointer + device
		conn.flush().ok()?;
		Some(Dnd { conn, queue, state, read_rx })
	}

	/// Hand paths to the compositor. Must run while the initiating left button is still
	/// down — egui's drag_started() frame is exactly that moment.
	pub(crate) fn start(&mut self, paths: Vec<PathBuf>, scale: f32, move_ok: bool) -> bool {
		if paths.is_empty() || self.state.drag.is_some() {
			if debug() { eprintln!("dnd: start refused — empty={} busy={}", paths.is_empty(), self.state.drag.is_some()); }
			return false;
		}
		let (Some(serial), Some(manager), Some(device)) =
			(self.state.serial, self.state.manager.as_ref(), self.state.device.as_ref())
		else {
			if debug() { eprintln!("dnd: start refused — serial={:?} manager={} device={}", self.state.serial, self.state.manager.is_some(), self.state.device.is_some()); }
			return false;
		};

		let many = paths.len() > 1;
		let uri_list = crate::uri_clipboard::uri_list(&paths);
		let text = crate::uri_clipboard::paths_text(&paths);
		let source = manager.create_data_source(&self.queue.handle(), ());
		source.offer("text/uri-list".to_owned());
		source.offer("text/plain;charset=utf-8".to_owned());
		source.set_actions(drag_actions(move_ok));
		let icon = Icon::new(&self.state, &self.queue.handle(), (scale.round() as i32).max(1), many);
		device.start_drag(Some(&source), &self.state.surface, icon.as_ref().map(|icon| &icon.surface), serial);
		self.state.drag = Some(Drag { source, paths, uri_list, text, action: DndAction::empty(), icon, dropped_at: None });
		if debug() { eprintln!("dnd: started, serial={serial}, many={many}"); }
		self.conn.flush().is_ok()
	}

	/// Shift held mid-drag must still mean move: re-negotiate the moment it changes.
	pub(crate) fn set_shift(&mut self, shift: bool) {
		if self.state.shift == shift {
			return;
		}
		self.state.shift = shift;
		if let Some(offer) = &self.state.incoming
			&& let Some(info) = self.state.offers.get(&offer.id())
		{
			let supported = info.source_actions & allowed(&self.state);
			if !supported.is_empty() {
				offer.set_actions(supported, preferred(supported, shift));
				let _ = self.conn.flush();
			}
		}
	}

	/// Pump the queue; the app calls this every frame.
	pub(crate) fn poll(&mut self) -> Vec<Event> {
		if self.conn.protocol_error().is_some() {
			return Vec::new(); // connection is dead; stop poking it
		}
		let _ = self.queue.dispatch_pending(&mut self.state);
		let _ = self.conn.flush();
		// shortcut: a target that drops but never calls finish would wedge every later
		// drag; 3s after the drop is generous — finish follows the uri-list read, not the
		// file copy. Upgrade to a per-target quirk list if a real app needs longer.
		let timed_out = self.state.drag.as_ref()
			.is_some_and(|drag| drag.dropped_at.is_some_and(|at| at.elapsed() > Duration::from_secs(3)));
		if timed_out {
			let drag = self.state.drag.take().unwrap();
			if debug() { eprintln!("dnd: watchdog — target never finished, forcing end"); }
			drag.source.destroy();
			if let Some(icon) = drag.icon { icon.destroy(); }
			self.state.outcome = Some(Outcome::Ended);
		}
		let mut events = std::mem::take(&mut self.state.events);
		if let Some(outcome) = self.state.outcome.take() {
			events.push(Event::DragEnded(outcome));
		}
		while let Ok(bytes) = self.read_rx.try_recv() {
			let Some((offer, action)) = self.state.reading.take() else { continue };
			if !action.is_empty() {
				offer.finish();
			}
			offer.destroy();
			let paths = decode(&bytes);
			if debug() { eprintln!("dnd: inbound {} files, action = {action:?}", paths.len()); }
			if !paths.is_empty() {
				events.push(Event::Dropped { paths, cut: action.contains(DndAction::Move) });
			}
		}
		events
	}

	pub(crate) fn active(&self) -> bool {
		self.state.drag.is_some()
	}
}

/// A drag from our own window onto itself must never negotiate Move, or the source half
/// of us would delete the originals the target half just refused to duplicate.
fn allowed(state: &State) -> DndAction {
	if state.drag.is_some() { DndAction::Copy } else { DndAction::Copy | DndAction::Move }
}

/// The preferred action must stay inside what the source actually offers, or the
/// compositor kills the connection with a protocol error.
fn preferred(supported: DndAction, shift: bool) -> DndAction {
	if shift && supported.contains(DndAction::Move) {
		return DndAction::Move;
	}
	if supported.contains(DndAction::Copy) {
		return DndAction::Copy;
	}
	supported // a move-only source: move it is
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
	fn event(_: &mut State, _: &WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<State>) {}
}

impl Dispatch<WlDataDeviceManager, ()> for State {
	fn event(_: &mut State, _: &WlDataDeviceManager, _: wayland_client::protocol::wl_data_device_manager::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {}
}

impl Dispatch<WlSeat, ()> for State {
	fn event(state: &mut State, seat: &WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<State>) {
		if let wl_seat::Event::Capabilities { capabilities } = event
			&& let WEnum::Value(capabilities) = capabilities
			&& capabilities.contains(Capability::Pointer)
			&& state.pointer.is_none()
			&& let Some(manager) = &state.manager
		{
			state.pointer = Some(seat.get_pointer(qh, ()));
			state.device = Some(manager.get_data_device(seat, qh, ()));
		}
	}
}

impl Dispatch<WlPointer, ()> for State {
	fn event(state: &mut State, _: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {
		match event {
			wl_pointer::Event::Enter { surface, .. } => state.on_us = surface == state.surface,
			wl_pointer::Event::Leave { .. } => state.on_us = false,
			wl_pointer::Event::Button { serial, state: WEnum::Value(ButtonState::Pressed), button, .. } if state.on_us && button == 0x110 => {
				state.serial = Some(serial); // BTN_LEFT — the compositor checks start_drag against this press
			}
			_ => {}
		}
	}
}

impl Dispatch<WlDataDevice, ()> for State {
	fn event(state: &mut State, _: &WlDataDevice, event: wl_data_device::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {
		match event {
			wl_data_device::Event::DataOffer { id } => {
				state.offers.insert(id.id(), OfferInfo::new());
			}
			wl_data_device::Event::Enter { serial, surface, id, .. } => {
				if surface != state.surface {
					return;
				}
				let Some(offer) = id else { return };
				let Some(info) = state.offers.get(&offer.id()) else { return };
				let supported = info.source_actions & allowed(state);
				if !info.mimes.iter().any(|mime| mime == "text/uri-list") || supported.is_empty() {
					// Not a file drag (a link, an image, a prayer): refuse politely.
					offer.destroy();
					state.offers.remove(&offer.id());
					return;
				}
				offer.accept(serial, Some("text/uri-list".to_owned()));
				offer.set_actions(supported, preferred(supported, state.shift));
				state.incoming = Some(offer);
				state.events.push(Event::Hovered);
				if debug() { eprintln!("dnd: inbound enter, source actions = {:?}", info.source_actions); }
			}
			wl_data_device::Event::Motion { .. } => {
				if let Some(offer) = &state.incoming
					&& let Some(info) = state.offers.get(&offer.id())
				{
					let supported = info.source_actions & allowed(state);
					if !supported.is_empty() {
						offer.set_actions(supported, preferred(supported, state.shift));
					}
				}
			}
			wl_data_device::Event::Leave => {
				if let Some(offer) = state.incoming.take() {
					state.offers.remove(&offer.id());
					offer.destroy();
					state.events.push(Event::Left);
				}
			}
			wl_data_device::Event::Drop => {
				let Some(offer) = state.incoming.take() else { return };
				// A previous read still pending (frozen source): close it out, don't leak it.
				if let Some((old, action)) = state.reading.take() {
					if !action.is_empty() { old.finish(); }
					old.destroy();
				}
				let info = state.offers.remove(&offer.id()).unwrap_or_else(OfferInfo::new);
				let action = info.action;
				// receive() takes a pipe we create: the compositor hands the write end to
				// the source, we keep the read end. Close our write copy so EOF arrives.
				let mut fds = [0; 2];
				if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
					offer.destroy();
					return;
				}
				// Safety: pipe() just filled both slots with fresh owned descriptors.
				let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
				let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
				offer.receive("text/uri-list".to_owned(), write_fd.as_fd());
				drop(write_fd);
				let tx = state.read_tx.clone();
				std::thread::spawn(move || {
					let mut buf = Vec::new();
					let _ = std::fs::File::from(read_fd).take(MAX_LIST).read_to_end(&mut buf);
					let _ = tx.send(buf);
				});
				state.reading = Some((offer, action));
				if debug() { eprintln!("dnd: inbound drop, action = {action:?}"); }
			}
			wl_data_device::Event::Selection { id } => {
				// Clipboard traffic belongs to wl-clipboard-rs on its own connection.
				if let Some(id) = id {
					state.offers.remove(&id.id());
					id.destroy();
				}
			}
			_ => {}
		}
	}

	wayland_client::event_created_child!(State, WlDataDevice, [
		0 => (WlDataOffer, ()), // data_offer
		5 => (WlDataOffer, ()), // selection
	]);
}

impl Dispatch<WlDataOffer, ()> for State {
	fn event(state: &mut State, offer: &WlDataOffer, event: wl_data_offer::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {
		let Some(info) = state.offers.get_mut(&offer.id()) else { return };
		match event {
			wl_data_offer::Event::Offer { mime_type } => info.mimes.push(mime_type),
			wl_data_offer::Event::SourceActions { source_actions: WEnum::Value(actions) } => info.source_actions = actions,
			wl_data_offer::Event::Action { dnd_action: WEnum::Value(action) } => info.action = action,
			_ => {}
		}
	}
}

impl Dispatch<WlDataSource, ()> for State {
	fn event(state: &mut State, source: &WlDataSource, event: wl_data_source::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {
		if !state.drag.as_ref().is_some_and(|drag| drag.source == *source) {
			return;
		}
		match event {
			wl_data_source::Event::Send { mime_type, fd } => {
				let drag = state.drag.as_ref().unwrap();
				let payload = if mime_type == "text/uri-list" { drag.uri_list.clone() } else { drag.text.clone() };
				// Off-thread: a target that never reads must not freeze our frames.
				std::thread::spawn(move || write_all(fd, payload.as_bytes()));
			}
			wl_data_source::Event::Action { dnd_action: WEnum::Value(action) } => {
				if debug() { eprintln!("dnd: negotiated action = {action:?}"); }
				state.drag.as_mut().unwrap().action = action;
			}
			wl_data_source::Event::DndDropPerformed => {
				if debug() { eprintln!("dnd: drop performed, action = {:?}", state.drag.as_ref().unwrap().action); }
				state.drag.as_mut().unwrap().dropped_at = Some(Instant::now());
			}
			wl_data_source::Event::DndFinished => {
				let drag = state.drag.take().unwrap();
				if debug() { eprintln!("dnd: finished, action = {:?}", drag.action); }
				drag.source.destroy();
				if let Some(icon) = drag.icon { icon.destroy(); }
				state.outcome = Some(finish(drag.action, drag.paths));
			}
			wl_data_source::Event::Cancelled => {
				if debug() { eprintln!("dnd: cancelled"); }
				let drag = state.drag.take().unwrap();
				drag.source.destroy();
				if let Some(icon) = drag.icon { icon.destroy(); }
				state.outcome = Some(Outcome::Ended);
			}
			_ => {}
		}
	}
}

impl Dispatch<WlCompositor, ()> for State {
	fn event(_: &mut State, _: &WlCompositor, _: wayland_client::protocol::wl_compositor::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {}
}

impl Dispatch<WlShm, ()> for State {
	fn event(state: &mut State, _: &WlShm, event: wl_shm::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {
		if let wl_shm::Event::Format { format: WEnum::Value(wl_shm::Format::Argb8888) } = event {
			state.argb8888 = true;
		}
	}
}

impl Dispatch<WlShmPool, ()> for State {
	fn event(_: &mut State, _: &WlShmPool, _: wayland_client::protocol::wl_shm_pool::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {}
}

impl Dispatch<WlBuffer, ()> for State {
	fn event(_: &mut State, _: &WlBuffer, _: wayland_client::protocol::wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {}
}

impl Dispatch<WlSurface, ()> for State {
	fn event(_: &mut State, _: &WlSurface, _: wayland_client::protocol::wl_surface::Event, _: &(), _: &Connection, _: &QueueHandle<State>) {}
}

impl Icon {
	fn new(state: &State, qh: &QueueHandle<State>, scale: i32, many: bool) -> Option<Icon> {
		let shm = state.shm.as_ref()?;
		let compositor = state.compositor.as_ref()?;
		if !state.argb8888 {
			return None;
		}
		let surface = compositor.create_surface(qh, ());
		// set_buffer_scale only exists since wl_surface v3; older compositors get a 1x icon.
		let scale = if surface.version() >= 3 {
			surface.set_buffer_scale(scale);
			scale
		} else {
			1
		};
		let size = 48 * scale;
		let stride = size * 4;
		let pixels = paint(size, many);

		let fd = unsafe { libc::memfd_create(b"crabhop-dnd\0".as_ptr().cast(), libc::MFD_CLOEXEC) };
		if fd < 0 {
			surface.destroy();
			return None;
		}
		// Safety: memfd_create just returned a fresh owned descriptor.
		let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
		if file.set_len((stride * size) as u64).is_err() || file.write_all(&pixels).is_err() {
			surface.destroy();
			return None;
		}
		let pool = shm.create_pool(file.as_fd(), stride * size, qh, ());
		let buffer = pool.create_buffer(0, size, size, stride, wl_shm::Format::Argb8888, qh, ());
		drop(file);

		// First attach must sit at (0,0): the offset is relative to the *current* buffer,
		// and a fresh surface has none — a negative offset here is protocol error 3.
		// The page therefore rides just below-right of the pointer, sctk-example style.
		surface.attach(Some(&buffer), 0, 0);
		surface.damage(0, 0, size, size);
		surface.commit();
		Some(Icon { surface, buffer, pool })
	}

	fn destroy(self) {
		self.surface.destroy();
		self.buffer.destroy();
		self.pool.destroy();
	}
}

/// A little stack of pages drawn straight into BGRA pixels — no font, no asset.
/// Everything is fully opaque or fully transparent, so premultiplication is free.
fn paint(size: i32, many: bool) -> Vec<u8> {
	let s = size as usize;
	let mut px = vec![0u8; s * s * 4];
	if many {
		page(&mut px, s, 0.26, 0.04, 0.86, 0.84, [176, 180, 195, 255]);
		page(&mut px, s, 0.20, 0.07, 0.80, 0.87, [205, 209, 222, 255]);
	}
	page(&mut px, s, 0.14, 0.10, 0.74, 0.90, [230, 233, 239, 255]);
	for (i, y) in [0.48f32, 0.60, 0.72].iter().enumerate() {
		let x1 = if i == 2 { 0.54 } else { 0.64 };
		fill(&mut px, s, 0.24, *y, x1, y + 0.045, [137, 140, 160, 255]);
	}
	px
}

fn page(px: &mut [u8], s: usize, x0: f32, y0: f32, x1: f32, y1: f32, body: [u8; 4]) {
	fill(px, s, x0, y0, x1, y1, [88, 91, 112, 255]);
	let b = (s as f32 / 24.0).max(1.0) / s as f32;
	fill(px, s, x0 + b, y0 + b, x1 - b, y1 - b, body);
	// folded top-right corner: cut the tip, shade the flap
	let f = 0.16f32;
	let (cx, cy, fs) = ((x1 - f) * s as f32, y0 * s as f32, f * s as f32);
	for y in 0..fs as usize {
		for x in 0..fs as usize {
			let (gx, gy) = (cx as usize + x, cy as usize + y);
			if gx >= s || gy >= s {
				continue;
			}
			let i = (gy * s + gx) * 4;
			if (x + y) as f32 >= fs {
				px[i..i + 4].copy_from_slice(&[0, 0, 0, 0]);
			} else if y >= x {
				px[i..i + 4].copy_from_slice(&[210, 194, 190, 255]);
			}
		}
	}
}

fn fill(px: &mut [u8], s: usize, x0: f32, y0: f32, x1: f32, y1: f32, rgba: [u8; 4]) {
	let clamp = |v: f32| ((v * s as f32).round() as usize).min(s);
	let (x0, y0, x1, y1) = (clamp(x0), clamp(y0), clamp(x1), clamp(y1));
	for y in y0..y1 {
		for x in x0..x1 {
			let i = (y * s + x) * 4;
			px[i] = rgba[2];
			px[i + 1] = rgba[1];
			px[i + 2] = rgba[0];
			px[i + 3] = rgba[3];
		}
	}
}

/// The drag page as an egui image, for the in-window drag (the Wayland half paints
/// straight into shm). Opaque-or-transparent pixels, so premultiplication is identity.
pub(crate) fn icon_rgba(many: bool) -> eframe::egui::ColorImage {
	let size = 96;
	let bgra = paint(size, many);
	let mut rgba = vec![0u8; bgra.len()];
	for (src, dst) in bgra.chunks_exact(4).zip(rgba.chunks_exact_mut(4)) {
		dst[0] = src[2];
		dst[1] = src[1];
		dst[2] = src[0];
		dst[3] = src[3];
	}
	eframe::egui::ColorImage::from_rgba_unmultiplied([size as usize, size as usize], &rgba)
}

fn debug() -> bool {
	std::env::var_os("CRABHOP_DND_DEBUG").is_some()
}

/// Drag-out defaults to copy. A target that picks Move may still read the files long after
/// dnd_finished (QQ does), so originals only ever leave when the person held Shift at the
/// handoff — the same "shift means move" rule every file manager teaches.
fn drag_actions(move_ok: bool) -> DndAction {
	if move_ok { DndAction::Copy | DndAction::Move } else { DndAction::Copy }
}

/// The whole move decision: the target negotiated Move and the drop finished, so the
/// originals are duplicates now. Copy or cancel leaves them alone.
fn finish(action: DndAction, paths: Vec<PathBuf>) -> Outcome {
	if action.contains(DndAction::Move) { Outcome::Moved(paths) } else { Outcome::Ended }
}

fn write_all(fd: OwnedFd, bytes: &[u8]) {
	// EPIPE (the target walked away) is swallowed: cancelled follows.
	let _ = std::fs::File::from(fd).write_all(bytes);
}

/// file:// URIs, percent-decoded, one path per line. Comments and non-file URIs skip.
fn decode(bytes: &[u8]) -> Vec<PathBuf> {
	let text = String::from_utf8_lossy(bytes);
	let mut paths = Vec::new();
	for line in text.lines() {
		let line = line.trim_end_matches('\r');
		if line.is_empty() || line.starts_with('#') {
			continue;
		}
		let Some(rest) = line.strip_prefix("file://") else { continue };
		let rest = if rest.starts_with("localhost/") { &rest["localhost".len()..] } else { rest };
		if let Some(path) = percent_decode(rest) {
			paths.push(PathBuf::from(path));
		}
	}
	paths
}

fn percent_decode(s: &str) -> Option<String> {
	let bytes = s.as_bytes();
	let mut out = Vec::with_capacity(bytes.len());
	let mut i = 0;
	while i < bytes.len() {
		if bytes[i] == b'%' {
			let hi = hex(*bytes.get(i + 1)?)?;
			let lo = hex(*bytes.get(i + 2)?)?;
			out.push(hi << 4 | lo);
			i += 3;
		} else {
			out.push(bytes[i]);
			i += 1;
		}
	}
	String::from_utf8(out).ok()
}

fn hex(b: u8) -> Option<u8> {
	match b {
		b'0'..=b'9' => Some(b - b'0'),
		b'a'..=b'f' => Some(b - b'a' + 10),
		b'A'..=b'F' => Some(b - b'A' + 10),
		_ => None,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_drag_out_offers_move_only_when_shift_was_held() {
		assert_eq!(drag_actions(false), DndAction::Copy, "the default target can only copy — a late reader never loses the original");
		assert!(drag_actions(true).contains(DndAction::Move), "shift opens the move door");
	}

	#[test]
	fn a_finished_move_hands_back_the_originals() {
		let paths = vec![PathBuf::from("/tmp/a.txt")];
		assert_eq!(finish(DndAction::Move, paths.clone()), Outcome::Moved(paths.clone()));
		assert_eq!(finish(DndAction::Copy | DndAction::Move, paths.clone()), Outcome::Moved(paths.clone()), "the negotiated action wins, not the offer");
		assert_eq!(finish(DndAction::Copy, paths.clone()), Outcome::Ended);
		assert_eq!(finish(DndAction::empty(), paths), Outcome::Ended, "no action negotiated = nothing was taken");
	}

	#[test]
	fn the_uri_list_decodes_back_to_paths() {
		let list = b"file:///tmp/a%20b.txt\r\n# comment\r\n\r\nfile:///tmp/%E4%B8%AD%E6%96%87/note.md\r\nhttps://example.com/x\r\nfile://localhost/tmp/c\r\nfile:///tmp/bad%ZZ\r\n";
		assert_eq!(decode(list), vec![
			PathBuf::from("/tmp/a b.txt"),
			PathBuf::from("/tmp/中文/note.md"),
			PathBuf::from("/tmp/c"),
		]);
		assert!(decode(b"").is_empty());
	}

	#[test]
	fn the_drag_icon_is_opaque_where_the_page_is() {
		let px = paint(48, false);
		assert_eq!(px.len(), 48 * 48 * 4);
		let center = (24 * 48 + 24) * 4;
		assert_eq!(px[center + 3], 255, "page body must be opaque");
		assert_eq!(px[3], 0, "corner stays transparent");
		assert!(paint(48, true).iter().any(|&b| b != 0), "the stack draws too");
	}

	#[test]
	fn the_egui_icon_keeps_the_page_opaque() {
		let image = icon_rgba(false);
		assert_eq!(image.size, [96, 96]);
		assert_eq!(image.pixels[48 * 96 + 48].a(), 255, "page body opaque");
		assert_eq!(image.pixels[0].a(), 0, "corner transparent");
	}

	#[test]
	fn the_preferred_action_stays_inside_what_the_source_offers() {
		assert_eq!(preferred(DndAction::Copy | DndAction::Move, true), DndAction::Move);
		assert_eq!(preferred(DndAction::Copy | DndAction::Move, false), DndAction::Copy);
		assert_eq!(preferred(DndAction::Copy, true), DndAction::Copy, "no move offered: shift cannot invent one");
		assert_eq!(preferred(DndAction::Move, false), DndAction::Move, "move-only source: never prefer an absent copy");
	}
}