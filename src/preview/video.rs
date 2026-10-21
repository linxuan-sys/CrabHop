//! What a video is (P29): the tools a machine happens to have are used when they are there —
//! `ffmpegthumbnailer` first, `ffmpeg` as the fallback, `ffprobe` for the facts — and the window
//! never asks anyone to install one. Nothing found, or a tool that fails: the container's own
//! header is read in process (mp4, mkv/webm) and the column says what the file is rather than
//! showing a picture of it. A frame that was made lands in the window's own bucket under the image
//! road's own key, so the second hover spawns nothing at all.
//!
//! This is an **opportunistic enhancement on trial** (P29's own ruling): everything degrades to the
//! pure probe when the tools are not there, and the whole layer can be taken back out in one piece.
//!
//! Every tool runs as a child of this process with its own process group, a three-second leash and
//! its writes pointed at the window's cache — killed at the leash's end, and only ever that group.

use std::{
	collections::HashMap,
	path::{Path, PathBuf},
	process::{Command, Stdio},
	sync::{LazyLock, Mutex},
	time::{Duration, Instant, SystemTime},
};

use crate::preview::img;

/// How long a tool may run before it is killed: a preview is not worth a stalled frame.
const TOOL_TIMEOUT: Duration = Duration::from_secs(3);

/// How wide a frame is asked for: twice a column at any sane size, and far less than a 4K film
/// would cost to decode for nothing.
const FRAME_WIDTH: u32 = 1280;

/// The video extensions this window describes (P29): the containers a person's media files use.
const EXTENSIONS: &[&str] = &[
	"mp4", "m4v", "mkv", "webm", "mov", "avi", "mpg", "mpeg", "3gp", "wmv", "flv", "ts", "m2ts",
];

/// Whether a name promises a video.
pub(crate) fn is_video(name: &str) -> bool {
	let ext = name.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();

	EXTENSIONS.contains(&ext.as_str())
}

/// The tools this machine happens to have (P29): looked for once per process, and never installed.
#[derive(Clone, Default)]
pub(crate) struct Tools {
	pub thumbnailer: Option<PathBuf>,
	pub ffmpeg:      Option<PathBuf>,
	pub ffprobe:     Option<PathBuf>,
}

impl Tools {
	/// The tool a frame would come from, in the order P29 names them.
	pub(crate) fn frame_tool(&self) -> Option<&Path> { self.thumbnailer.as_deref().or(self.ffmpeg.as_deref()) }
}

static TOOLS: LazyLock<Tools> = LazyLock::new(|| tools_in(&std::env::var("PATH").unwrap_or_default()));

/// The tools of this process.
pub(crate) fn tools() -> &'static Tools { &TOOLS }

/// The tools found under a PATH — a pure function of the string, so the tests can hand it an empty
/// one and see the degraded road without touching the process environment.
pub(crate) fn tools_in(path_env: &str) -> Tools {
	let find = |name: &str| {
		path_env
			.split(':')
			.filter(|dir| !dir.is_empty())
			.map(|dir| Path::new(dir).join(name))
			.find(|at| is_executable(at))
	};

	Tools { thumbnailer: find("ffmpegthumbnailer"), ffmpeg: find("ffmpeg"), ffprobe: find("ffprobe") }
}

fn is_executable(at: &Path) -> bool {
	use std::os::unix::fs::PermissionsExt;

	std::fs::metadata(at).is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Run a tool under the leash (P29's discipline): its own process group, stdout into memory (a
/// pipe, never a terminal), stderr dropped, stdin closed, killed at the timeout — its group first,
/// then itself, and never anything but what this call started. `None` when it did not finish or did
/// not run.
fn run(tool: &Path, args: &[&str]) -> Option<Vec<u8>> {
	use std::os::unix::process::CommandExt;

	let mut child = Command::new(tool)
		.args(args)
		.stdin(Stdio::null())
		.stdout(Stdio::piped())
		.stderr(Stdio::null())
		.process_group(0)
		.spawn()
		.ok()?;

	// the reader runs beside the wait: a tool that fills the pipe must be drained while it works
	let mut stdout = child.stdout.take()?;
	let reader = std::thread::spawn(move || {
		use std::io::Read;

		let mut bytes = Vec::new();
		let _ = stdout.read_to_end(&mut bytes);
		bytes
	});

	let pid = child.id() as i32;
	let started = Instant::now();
	let mut finished = false;
	while started.elapsed() < TOOL_TIMEOUT {
		match child.try_wait() {
			Ok(Some(status)) => {
				finished = status.success();
				break;
			}
			Ok(None) => std::thread::sleep(Duration::from_millis(20)),
			Err(_) => break,
		}
	}

	if !finished {
		// the leash: the group this call started, then the child — reaped right after, so the
		// number is never left to be reused
		unsafe {
			libc::killpg(pid, libc::SIGKILL);
			libc::kill(pid, libc::SIGKILL);
		}
		let _ = child.wait();
	}

	let bytes = reader.join().ok()?;
	finished.then_some(bytes)
}

/// The frame of a video, ready to upload (P29): the window's own bucket first — the second hover of
/// the same file at the same column is instant — and the tools only when there is nothing in it.
/// `None` when no tool is there or the frame could not be made.
pub(crate) fn frame(path: &Path, mtime: Option<SystemTime>, target: [u32; 2]) -> Option<([u32; 2], [u32; 2], Vec<u8>)> {
	let name = path.to_str().unwrap_or_default();
	if let Some(hit) = img::bucket_load(name, mtime, target) {
		crate::metrics::video(name, "bucket");
		return Some(hit);
	}

	let tool = tools().frame_tool()?.to_owned();
	let png = extract(&tool, path)?;

	let image = image::load_from_memory(&png).ok()?.to_rgba8();
	let source = [ image.width(), image.height() ];
	let size = img::upload_size(source, target);
	let rgba = if size == source {
		image.into_raw()
	} else {
		img::resample(image.as_raw(), source, size)?
	};

	img::bucket_store(name, mtime, target, size, source, &rgba);
	crate::metrics::video(name, "extract");
	Some((size, source, rgba))
}

/// One frame through whichever tool is there, into a file inside the window's own cache and back
/// out as its bytes: the tool never writes anywhere that is not ours.
fn extract(tool: &Path, path: &Path) -> Option<Vec<u8>> {
	let dir = crate::config::Config::cache("preview-img")?;
	crate::config::Config::ensure(&dir).ok()?;

	let stamp = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).ok()?.as_nanos();
	let out = dir.join(format!(".frame-{}-{stamp}.png", std::process::id()));
	let (input, frame) = (path.to_str()?, out.to_str()?);
	let width = FRAME_WIDTH.to_string();

	let name = tool.file_name().and_then(|n| n.to_str()).unwrap_or_default();
	let args: Vec<&str> = if name.starts_with("ffmpegthumbnailer") {
		// the thumbnailer takes a size for its longest side and a moment to stand on (the very
		// first frame, `0`)
		vec![ "-i", input, "-o", frame, "-s", &width, "-t", "0" ]
	} else {
		// ffmpeg proper: the first frame, scaled so a 4K film is not decoded whole for a column
		vec![
			"-ss", "0", "-frames:v", "1", "-i", input, "-vf", "scale='min(1280,iw)':-2", "-nostdin", "-loglevel", "error",
			"-y", frame,
		]
	};

	let made = run(tool, &args);
	let bytes = made.and_then(|_| std::fs::read(&out).ok());
	let _ = std::fs::remove_file(&out);

	bytes.filter(|bytes| !bytes.is_empty())
}

/// What a video is, in the window's own words: the facts' own answers, and a `None` for what was
/// not said — the card leaves those rows out rather than printing a made-up number.
#[derive(Clone, Default, PartialEq)]
pub(crate) struct Media {
	pub codec:    Option<String>,
	pub size:     Option<[u32; 2]>,
	pub duration: Option<f64>,
	pub fps:      Option<f64>,
}

impl Media {
	/// The card's rows — Chinese a beginner reads, the card's own shape — with the facts that are
	/// not there left out. `shown_width` is the physical width the preview column would show this
	/// file's own pixels at (the fit-width rule): the ratio row is there only when the column has
	/// said how wide it is.
	pub(crate) fn rows(&self, shown_width: Option<f32>) -> Vec<(String, String)> {
		let mut rows = Vec::new();

		if let Some(codec) = &self.codec {
			rows.push(("编码".to_owned(), codec.clone()));
		}
		if let Some([ width, height ]) = self.size {
			rows.push(("分辨率".to_owned(), format!("{width} × {height}")));
		}
		if let (Some([ width, .. ]), Some(shown)) = (self.size, shown_width) {
			let percent = (shown / width.max(1) as f32 * 100.0).round().max(1.0) as u32;
			rows.push(("显示比例".to_owned(), format!("{percent}%")));
		}
		if let Some(seconds) = self.duration {
			rows.push(("时长".to_owned(), duration_words(seconds)));
		}
		if let Some(fps) = self.fps {
			rows.push(("帧率".to_owned(), fps_words(fps)));
		}

		rows
	}
}

/// A duration in words a person reads: hours, minutes and seconds, the empty ones left out.
pub(crate) fn duration_words(seconds: f64) -> String {
	let seconds = seconds.max(0.0).round() as u64;
	let (hours, minutes, rest) = (seconds / 3600, seconds % 3600 / 60, seconds % 60);

	if hours > 0 {
		format!("{hours} 小时 {minutes} 分 {rest} 秒")
	} else if minutes > 0 {
		format!("{minutes} 分 {rest} 秒")
	} else {
		format!("{rest} 秒")
	}
}

/// A frame rate in words a person reads: whole rates whole, the rest to two decimals.
pub(crate) fn fps_words(fps: f64) -> String {
	let rounded = (fps * 100.0).round() / 100.0;

	if (rounded - rounded.round()).abs() < 0.005 {
		format!("{:.0} fps", rounded)
	} else {
		format!("{rounded:.2} fps")
	}
}

/// The codec behind a header's own name, in the words people use: the sample entry's fourcc
/// (`avc1`), the matroska id (`V_MPEG4/ISO/AVC`) and ffmpeg's own names all land on one word.
fn codec_words(name: &str) -> String {
	let lower = name.to_ascii_lowercase();
	let id = lower.trim_start_matches("v_").trim_start_matches("a_");

	let words = match id {
		"avc1" | "avc3" | "h264" => "H.264",
		"hev1" | "hvc1" | "h265" | "hevc" => "H.265 / HEVC",
		"vp8" | "vp09" => "VP8",
		"vp9" => "VP9",
		"av1" | "av01" => "AV1",
		"mjpeg" => "Motion JPEG",
		"mpeg2video" | "mpgv" => "MPEG-2",
		other if other.contains("iso/avc") => "H.264",
		other if other.contains("iso/hevc") => "H.265 / HEVC",
		other if other.contains("iso/mpeg4") || other == "mp4v" || other == "mpeg4" => "MPEG-4",
		_ => return name.to_owned(),
	};

	words.to_owned()
}

static MEDIA: LazyLock<Mutex<Cache>> = LazyLock::new(|| Mutex::new(Cache::new()));

/// One file's facts, keyed by its path and modification time.
type Cache = HashMap<(PathBuf, u128), Option<Media>>;

/// What a video is (P29): its own header, read once per file — the spot card asks on every `Tab`,
/// and a header that has been read is not read again.
pub(crate) fn media(path: &Path, mtime: Option<SystemTime>) -> Option<Media> {
	let key = (
		path.to_owned(),
		mtime.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis()),
	);
	if let Some(cached) = MEDIA.lock().ok().and_then(|map| map.get(&key).cloned()) {
		return cached;
	}

	// the tools' own answer first (P29), the header's when there is no ffprobe — and the header
	// again when ffprobe says nothing usable
	let probed = tools()
		.ffprobe
		.as_deref()
		.and_then(|tool| ffprobe_media(tool, path))
		.or_else(|| {
			crate::metrics::video(path.to_str().unwrap_or_default(), "header");
			header_media(path)
		});

	if let Ok(mut map) = MEDIA.lock() {
		if map.len() > 512 {
			map.clear();
		}
		map.insert(key, probed.clone());
	}

	probed
}

/// ffprobe's own answer (P29): the video stream's codec, pixels and frame rate, and the format's
/// clock — its JSON, parsed purely.
fn ffprobe_media(tool: &Path, path: &Path) -> Option<Media> {
	let input = path.to_str()?;
	let args = [
		"-v", "error", "-select_streams", "v:0", "-show_entries",
		"stream=codec_name,width,height,avg_frame_rate,r_frame_rate:format=duration,format_name", "-of", "json", input,
	];

	let media = parse_ffprobe(&run(tool, &args)?);
	if media.is_some() {
		crate::metrics::video(input, "ffprobe");
	}

	media
}

/// The same answer, parsed purely: ffprobe's JSON with the stream and the format in it. The frame
/// rate is ffprobe's own ratio spelling (`30000/1001`).
pub(crate) fn parse_ffprobe(bytes: &[u8]) -> Option<Media> {
	let json: serde_json::Value = serde_json::from_slice(bytes).ok()?;
	let stream = json.get("streams").and_then(|streams| streams.get(0));
	let format = json.get("format");

	let number = |value: Option<&serde_json::Value>| -> Option<f64> {
		value.and_then(|value| value.as_str().map_or_else(|| value.as_f64(), |text| text.parse().ok()))
	};
	let size = match (number(stream.and_then(|s| s.get("width"))), number(stream.and_then(|s| s.get("height")))) {
		(Some(width), Some(height)) => Some([ width as u32, height as u32 ]),
		_ => None,
	};
	let fps = stream
		.and_then(|stream| stream.get("avg_frame_rate").or_else(|| stream.get("r_frame_rate")))
		.and_then(|rate| rate.as_str())
		.and_then(ratio_words);

	let media = Media {
		codec: stream.and_then(|stream| stream.get("codec_name")).and_then(|name| name.as_str()).map(codec_words),
		size,
		duration: number(format.and_then(|format| format.get("duration"))),
		fps,
	};

	(media != Media::default()).then_some(media)
}

/// A ratio's own spelling (`30000/1001`) as a number.
fn ratio_words(rate: &str) -> Option<f64> {
	let (top, bottom) = rate.split_once('/')?;
	let (top, bottom) = (top.parse::<f64>().ok()?, bottom.parse::<f64>().ok()?);

	(bottom > 0.0 && top > 0.0).then(|| top / bottom)
}

/// The head of a file, read once: the box tree of an mp4 lives at the front — or at the very back,
/// for a file whose `moov` was written last — and an EBML header is at the front by construction.
fn header_media(path: &Path) -> Option<Media> {
	const HEAD: u64 = 4 * 1024 * 1024;

	let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default().to_ascii_lowercase();
	let ext = name.rsplit('.').next().unwrap_or_default().to_owned();

	let mut file = std::fs::File::open(path).ok()?;
	let len = file.metadata().ok()?.len();
	let head = read_some(&mut file, 0, len.min(HEAD))?;

	match ext.as_str() {
		"mp4" | "m4v" | "mov" | "3gp" => {
			if let Some(media) = parse_mp4(&head) {
				return Some(media);
			}
			// a moov box written at the end (a plain ffmpeg default): the tail carries it
			if len > HEAD {
				let tail = read_some(&mut file, len - HEAD, HEAD)?;
				return parse_mp4_at(&tail, find_box(&tail, b"moov")?);
			}
			None
		}
		"mkv" | "webm" => parse_ebml(&head),
		// the containers with no reader here: the extension says video, the header says nothing
		_ => None,
	}
}

fn read_some(file: &mut std::fs::File, at: u64, take: u64) -> Option<Vec<u8>> {
	use std::io::{Read, Seek, SeekFrom};

	file.seek(SeekFrom::Start(at)).ok()?;
	let mut bytes = Vec::with_capacity(take as usize);
	file.take(take).read_to_end(&mut bytes).ok()?;
	Some(bytes)
}

/// Where a box of that name begins in a chunk: the four bytes of its size sit just before it.
fn find_box(bytes: &[u8], kind: &[u8; 4]) -> Option<usize> {
	bytes.windows(4).position(|window| window == kind).and_then(|at| at.checked_sub(4))
}

/// An mp4's own facts (P29), from its head bytes: the video track's codec and pixels, the movie's
/// clock, and the frame rate its sample table implies — no decode and no tool.
pub(crate) fn parse_mp4(bytes: &[u8]) -> Option<Media> { parse_mp4_at(bytes, 0) }

fn parse_mp4_at(bytes: &[u8], at: usize) -> Option<Media> {
	let mut movie = Movie::default();
	walk_mp4(&bytes[at..], &mut movie);

	let track = movie.video_track()?;
	let duration = movie.duration;

	Some(Media {
		codec:    track.codec.clone(),
		size:     track.size,
		duration,
		fps:      track.fps(),
	})
}

/// The movie's own clock and the tracks inside it, kept apart while the boxes are walked.
#[derive(Default)]
struct Movie {
	duration: Option<f64>,
	tracks:   Vec<Track>,
}

impl Movie {
	/// The first track that says it is video — or, for a header that says nothing about any of
	/// them, the first track with a picture's own facts. A track that says it is audio is never
	/// the answer: this card describes the video.
	fn video_track(&self) -> Option<&Track> {
		self.tracks
			.iter()
			.find(|track| track.handler == Some(*b"vide"))
			.or_else(|| self.tracks.iter().find(|track| track.handler.is_none() && (track.size.is_some() || track.codec.is_some())))
	}
}

#[derive(Default)]
struct Track {
	handler:   Option<[u8; 4]>,
	codec:     Option<String>,
	size:      Option<[u32; 2]>,
	timescale: Option<u32>,
	ticks:     u64,
	samples:   u64,
}

impl Track {
	/// The frame rate the track's own clock and sample table imply (P29): the media timescale over
	/// the average sample duration.
	fn fps(&self) -> Option<f64> {
		let timescale = self.timescale? as f64;
		if self.samples == 0 || self.ticks == 0 {
			return None;
		}

		let average = self.ticks as f64 / self.samples as f64;
		(average > 0.0 && timescale > 0.0).then(|| timescale / average)
	}
}

/// The box walk: every box's payload is looked at by its name, the containers this window cares
/// about are walked into, and `trak` opens a track of its own so an audio track's clock never
/// lands on the video's.
fn walk_mp4(bytes: &[u8], movie: &mut Movie) {
	let mut at = 0usize;
	while at + 8 <= bytes.len() {
		let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as u64;
		let kind: [u8; 4] = bytes[at + 4..at + 8].try_into().unwrap();
		let (header, size) = match size {
			0 => (8usize, (bytes.len() - at) as u64),
			1 if at + 16 <= bytes.len() => (16, u64::from_be_bytes(bytes[at + 8..at + 16].try_into().unwrap())),
			_ => (8usize, size),
		};
		if size < header as u64 {
			break;
		}

		let body = at + header;
		let end = (at as u64 + size).min(bytes.len() as u64) as usize;
		let Some(payload) = bytes.get(body..end) else { break };

		match &kind {
			b"mvhd" => {
				if movie.duration.is_none() {
					movie.duration = mvhd_seconds(payload);
				}
			}
			b"trak" => {
				let mut track = Track::default();
				walk_track(payload, &mut track);
				movie.tracks.push(track);
			}
			b"moov" => walk_mp4(payload, movie),
			_ => {}
		}

		if end <= at {
			break;
		}
		at = end;
	}
}

/// One track's own boxes.
fn walk_track(bytes: &[u8], track: &mut Track) {
	let mut at = 0usize;
	while at + 8 <= bytes.len() {
		let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as u64;
		let kind: [u8; 4] = bytes[at + 4..at + 8].try_into().unwrap();
		let (header, size) = match size {
			0 => (8usize, (bytes.len() - at) as u64),
			1 if at + 16 <= bytes.len() => (16, u64::from_be_bytes(bytes[at + 8..at + 16].try_into().unwrap())),
			_ => (8usize, size),
		};
		if size < header as u64 {
			break;
		}

		let body = at + header;
		let end = (at as u64 + size).min(bytes.len() as u64) as usize;
		let Some(payload) = bytes.get(body..end) else { break };

		match &kind {
			b"mdia" | b"minf" | b"stbl" => walk_track(payload, track),
			b"hdlr" if payload.len() >= 12 => track.handler = payload[8..12].try_into().ok(),
			b"mdhd" => {
				if track.timescale.is_none() {
					track.timescale = mdhd_timescale(payload);
				}
			}
			b"stts" => {
				let (ticks, samples) = stts_totals(payload);
				track.ticks += ticks;
				track.samples += samples;
			}
			b"stsd" if payload.len() > 16 => {
				let entry = &payload[8..];
				if let Some(codec) = entry.get(4..8).map(String::from_utf8_lossy) {
					track.codec = Some(codec_words(&codec));
				}
				// an `avc1`-style sample entry carries width and height as two 16-bit numbers
				if track.size.is_none() && entry.len() >= 28 {
					let width = u16::from_be_bytes([ entry[24], entry[25] ]);
					let height = u16::from_be_bytes([ entry[26], entry[27] ]);
					if width > 0 && height > 0 {
						track.size = Some([ width as u32, height as u32 ]);
					}
				}
			}
			b"tkhd" if track.size.is_none() && payload.len() >= 84 => {
				// the track header's own width and height, 16.16 fixed point at its end
				let base = if payload[0] == 1 { 96 } else { 84 };
				if payload.len() >= base {
					let width = u32::from_be_bytes(payload[base - 8..base - 4].try_into().unwrap()) as f64 / 65536.0;
					let height = u32::from_be_bytes(payload[base - 4..base].try_into().unwrap()) as f64 / 65536.0;
					if width >= 1.0 && height >= 1.0 {
						track.size = Some([ width as u32, height as u32 ]);
					}
				}
			}
			_ => {}
		}

		if end <= at {
			break;
		}
		at = end;
	}
}

/// The `moov` header's own clock: version, timescale and duration.
fn mvhd_seconds(payload: &[u8]) -> Option<f64> {
	if payload.len() < 20 {
		return None;
	}

	let (timescale_at, duration_at) = if payload[0] == 1 { (20, 24) } else { (12, 16) };
	let timescale = u32::from_be_bytes(payload.get(timescale_at..timescale_at + 4)?.try_into().unwrap()) as f64;
	let duration = if payload[0] == 1 {
		u64::from_be_bytes(payload.get(duration_at..duration_at + 8)?.try_into().unwrap()) as f64
	} else {
		u32::from_be_bytes(payload.get(duration_at..duration_at + 4)?.try_into().unwrap()) as f64
	};

	(timescale > 0.0 && duration > 0.0).then(|| duration / timescale)
}

/// The media header's own timescale: the ticks a second of this track is measured in.
fn mdhd_timescale(payload: &[u8]) -> Option<u32> {
	let at = if payload.first() == Some(&1) { 20 } else { 12 };

	Some(u32::from_be_bytes(payload.get(at..at + 4)?.try_into().unwrap()))
}

/// The sample table's own totals: how many ticks the samples span, and how many samples there are
/// — the average of the two is one frame's duration.
fn stts_totals(payload: &[u8]) -> (u64, u64) {
	let Some(count) = payload.get(4..8).map(|bytes| u32::from_be_bytes(bytes.try_into().unwrap())) else { return (0, 0) };

	let mut ticks = 0u64;
	let mut samples = 0u64;
	for entry in 0..count as usize {
		let at = 8 + entry * 8;
		let (Some(count), Some(delta)) = (
			payload.get(at..at + 4).map(|bytes| u32::from_be_bytes(bytes.try_into().unwrap())),
			payload.get(at + 4..at + 8).map(|bytes| u32::from_be_bytes(bytes.try_into().unwrap())),
		) else {
			break;
		};

		ticks += count as u64 * delta as u64;
		samples += count as u64;
	}

	(ticks, samples)
}

/// A matroska/webm's own facts (P29): the segment's clock and the first video track — the codec,
/// the pixels, the frame rate its own `DefaultDuration` states.
pub(crate) fn parse_ebml(bytes: &[u8]) -> Option<Media> {
	// the EBML header is an element of its own: the segment begins where it ends
	let (_, id_len) = ebml_id(bytes)?;
	let (size, size_len) = vint(&bytes[id_len..])?;
	let at = (id_len + size_len) as u64 + size;
	let rest = bytes.get(at as usize..)?;

	let mut segment = Segment::default();
	walk_ebml(rest, &mut segment);

	let track = segment.video_track()?;
	let duration = match (segment.duration_ticks, segment.scale) {
		(Some(ticks), Some(scale)) => Some(ticks * scale as f64 / 1_000_000_000.0),
		_ => None,
	};

	Some(Media { codec: track.codec.clone(), size: track.size, duration, fps: track.fps() })
}

/// The segment's own clock and tracks.
#[derive(Default)]
struct Segment {
	/// Nanoseconds a tick holds.
	scale:          Option<u64>,
	duration_ticks: Option<f64>,
	tracks:         Vec<EbmlTrack>,
}

impl Segment {
	fn video_track(&self) -> Option<&EbmlTrack> {
		self.tracks
			.iter()
			.find(|track| track.kind == Some(1))
			.or_else(|| self.tracks.iter().find(|track| track.kind.is_none() && (track.size.is_some() || track.codec.is_some())))
	}
}

#[derive(Default)]
struct EbmlTrack {
	/// 1 is video, 2 audio.
	kind:     Option<u64>,
	codec:    Option<String>,
	size:     Option<[u32; 2]>,
	/// Nanoseconds a frame lasts, as the track states it.
	frame_ns: Option<u64>,
}

impl EbmlTrack {
	fn fps(&self) -> Option<f64> {
		let ns = self.frame_ns?;
		(ns > 0).then(|| 1_000_000_000.0 / ns as f64)
	}
}

/// The bytes a VINT holds: the length marker is the first set bit, and the value's bytes follow.
fn vint(bytes: &[u8]) -> Option<(u64, usize)> {
	let first = *bytes.first()?;
	let width = first.leading_zeros() as usize + 1;
	if width > 8 || bytes.len() < width {
		return None;
	}

	let mut value = (first & (0xff >> width)) as u64;
	for byte in &bytes[1..width] {
		value = value << 8 | *byte as u64;
	}

	Some((value, width))
}

/// An element id: the marker is **part of** the number (0x2AD7B1 is written `2A D7 B1`), so ids
/// compare as the spec writes them — where [`vint`] masks the marker off a size.
fn ebml_id(bytes: &[u8]) -> Option<(u64, usize)> {
	let first = *bytes.first()?;
	let width = first.leading_zeros() as usize + 1;
	if width > 4 || bytes.len() < width {
		return None;
	}

	let mut value = first as u64;
	for byte in &bytes[1..width] {
		value = value << 8 | *byte as u64;
	}

	Some((value, width))
}

/// The EBML walk: the segment's clock, the info's duration and the tracks' own facts, with each
/// `TrackEntry` kept apart for the same reason the mp4 walk keeps its tracks apart.
fn walk_ebml(bytes: &[u8], segment: &mut Segment) {
	const SEGMENT: u64 = 0x1853_8067;
	const INFO: u64 = 0x1549_A966;
	const TRACKS: u64 = 0x1654_AE6B;
	const TRACK_ENTRY: u64 = 0xAE;

	let mut at = 0usize;
	while at < bytes.len() {
		let Some((id, id_len)) = ebml_id(&bytes[at..]) else { break };
		let Some((size, size_len)) = vint(&bytes[at + id_len..]) else { break };
		let body = at + id_len + size_len;
		// an unknown-size element (a streaming webm) runs to the end of what was read
		let unknown = size == u64::MAX - (1 << (7 * size_len));
		let end = if unknown { bytes.len() } else { (body as u64 + size).min(bytes.len() as u64) as usize };
		let Some(payload) = bytes.get(body..end) else { break };

		match id {
			SEGMENT | INFO | TRACKS => walk_ebml(payload, segment),
			TRACK_ENTRY => {
				let mut track = EbmlTrack::default();
				walk_ebml_track(payload, &mut track);
				segment.tracks.push(track);
			}
			// the timecode scale, in nanoseconds a tick
			0x2AD7B1 if segment.scale.is_none() => {
				let scale = payload.iter().fold(0u64, |value, byte| value << 8 | *byte as u64);
				if scale > 0 {
					segment.scale = Some(scale);
				}
			}
			0x4489 if segment.duration_ticks.is_none() => {
				let ticks = float_bytes(payload);
				if ticks > 0.0 {
					segment.duration_ticks = Some(ticks);
				}
			}
			_ => {}
		}

		if end <= at {
			break;
		}
		at = end;
	}
}

/// One track entry's own elements.
fn walk_ebml_track(bytes: &[u8], track: &mut EbmlTrack) {
	let mut at = 0usize;
	while at < bytes.len() {
		let Some((id, id_len)) = ebml_id(&bytes[at..]) else { break };
		let Some((size, size_len)) = vint(&bytes[at + id_len..]) else { break };
		let body = at + id_len + size_len;
		let end = (body as u64 + size).min(bytes.len() as u64) as usize;
		let Some(payload) = bytes.get(body..end) else { break };

		match id {
			0x83 => track.kind = Some(payload.iter().fold(0u64, |value, byte| value << 8 | *byte as u64)),
			0x86 => track.codec = Some(codec_words(&String::from_utf8_lossy(payload))),
			0xB0 | 0xBA => {
				let value = payload.iter().fold(0u64, |value, byte| value << 8 | *byte as u64) as u32;
				if value > 0 {
					let size = track.size.unwrap_or([ 0, 0 ]);
					track.size = Some(if id == 0xB0 { [ value, size[1] ] } else { [ size[0], value ] });
				}
			}
			// the nanoseconds one frame lasts
			0x23E383 => track.frame_ns = Some(payload.iter().fold(0u64, |value, byte| value << 8 | *byte as u64)),
			_ => {}
		}

		if end <= at {
			break;
		}
		at = end;
	}
}

/// A float element's own bytes, whichever of the two widths it came in.
fn float_bytes(payload: &[u8]) -> f64 {
	match payload.len() {
		8 => f64::from_bits(payload.iter().fold(0u64, |value, byte| value << 8 | *byte as u64)),
		4 => f32::from_bits(payload.iter().fold(0u32, |value, byte| value << 8 | *byte as u32)) as f64,
		_ => 0.0,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The tools are found under a PATH, once (P29's first item): an empty PATH is the machine with
	/// nothing installed — the degraded road's own trigger — and only an executable counts.
	#[test]
	fn the_tools_are_the_ones_the_path_has() {
		let empty = tools_in("");
		assert!(empty.frame_tool().is_none() && empty.ffprobe.is_none(), "nothing on an empty PATH");

		let dir = std::env::temp_dir().join(format!("duckfm-native-tools-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");
		let fake = dir.join("ffmpegthumbnailer");
		std::fs::write(&fake, b"#!/bin/sh\nexit 0\n").expect("a script");
		{
			use std::os::unix::fs::PermissionsExt;
			std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).expect("executable");
		}
		std::fs::write(dir.join("ffmpeg"), b"nope").expect("a file without the bit set");

		let found = tools_in(dir.to_str().unwrap());
		assert_eq!(found.thumbnailer.as_deref(), Some(fake.as_path()), "the executable is found");
		assert!(found.ffmpeg.is_none(), "a file without the bit set is not a tool");
		assert!(found.frame_tool().is_some(), "and the thumbnailer is the first choice");

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// The leash (P29's own discipline): a tool that does not come back is killed at three seconds,
	/// its own group first — and the call says so by answering nothing.
	#[test]
	fn a_tool_that_will_not_come_back_is_killed() {
		let dir = std::env::temp_dir().join(format!("duckfm-native-leash-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&dir);
		std::fs::create_dir_all(&dir).expect("a directory");
		let script = dir.join("slow-tool");
		std::fs::write(&script, b"#!/bin/sh\nsleep 30\n").expect("a script");
		{
			use std::os::unix::fs::PermissionsExt;
			std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("executable");
		}

		let started = Instant::now();
		let answer = run(&script, &[]);
		let waited = started.elapsed();

		assert!(answer.is_none(), "a tool past its leash answers nothing");
		assert!(waited >= TOOL_TIMEOUT && waited < TOOL_TIMEOUT + Duration::from_secs(2), "its three seconds and no more: {waited:?}");

		// a tool that does come back is heard
		let quick = dir.join("quick-tool");
		std::fs::write(&quick, b"#!/bin/sh\nprintf hello\n").expect("a script");
		{
			use std::os::unix::fs::PermissionsExt;
			std::fs::set_permissions(&quick, std::fs::Permissions::from_mode(0o755)).expect("executable");
		}
		assert_eq!(run(&quick, &[]).as_deref(), Some(&b"hello"[..]), "its words come back whole");

		let _ = std::fs::remove_dir_all(&dir);
	}

	/// ffprobe's own answer (P29), parsed into the window's shape: the codec, the pixels, the clock
	/// and the frame rate its ratio spelling carries — and an empty answer is no answer at all.
	#[test]
	fn ffprobe_json_becomes_the_cards_rows() {
		let json = br#"{
			"streams": [ { "codec_name": "h264", "width": 1920, "height": 1080, "avg_frame_rate": "30000/1001" } ],
			"format": { "format_name": "mov,mp4,m4a,3gp,3g2,mj2", "duration": "83.520000" }
		}"#;

		let media = parse_ffprobe(json).expect("an answer");
		assert_eq!(media.codec.as_deref(), Some("H.264"));
		assert_eq!(media.size, Some([ 1920, 1080 ]));
		assert_eq!(media.duration, Some(83.52));
		assert!((media.fps.unwrap() - 29.97).abs() < 0.01, "30000/1001 is 29.97: {:?}", media.fps);

		let rows: std::collections::HashMap<_, _> = media.rows(Some(454.0)).into_iter().collect();
		assert_eq!(rows.get("分辨率").map(String::as_str), Some("1920 × 1080"));
		assert_eq!(rows.get("显示比例").map(String::as_str), Some("24%"), "454 of 1920");
		assert_eq!(rows.get("帧率").map(String::as_str), Some("29.97 fps"));

		assert!(parse_ffprobe(br#"{"streams": [], "format": {}}"#).is_none(), "an empty answer is no answer");
		assert!(parse_ffprobe(b"not json at all").is_none());
	}

	/// **Both roads carry the three fields the ruling names** (P29's own item): the size, the ratio
	/// and the frame rate are there whether the facts came from ffprobe or from the header alone.
	#[test]
	fn both_roads_carry_size_ratio_and_fps() {
		let from_probe = Media {
			codec:    Some("H.264".to_owned()),
			size:     Some([ 1920, 1080 ]),
			duration: Some(83.52),
			fps:      Some(29.97),
		};
		let from_header = parse_mp4(&fixture_mp4()).expect("the header's own answer");

		for media in [ &from_probe, &from_header ] {
			let rows: std::collections::HashMap<_, _> = media.rows(Some(454.0)).into_iter().collect();
			assert!(rows.contains_key("分辨率"), "a size: {rows:?}");
			assert!(rows.contains_key("显示比例"), "a ratio: {rows:?}");
			assert!(rows.contains_key("帧率"), "a frame rate: {rows:?}");
		}
	}

	/// The facts read in words a person says: the durations and the frame rates.
	#[test]
	fn the_facts_read_in_words() {
		assert_eq!(duration_words(9.4), "9 秒");
		assert_eq!(duration_words(63.0), "1 分 3 秒");
		assert_eq!(duration_words(3723.0), "1 小时 2 分 3 秒");
		assert_eq!(duration_words(-4.0), "0 秒", "a broken clock is not a negative duration");

		assert_eq!(fps_words(30.0), "30 fps");
		assert_eq!(fps_words(29.970_03), "29.97 fps");
		assert_eq!(fps_words(23.976_02), "23.98 fps");
		assert_eq!(fps_words(60.0), "60 fps");
	}

	/// An mp4's own header, read without a tool: the video track's codec, pixels and clock, the
	/// movie's duration, and the frame rate the sample table implies. The fixture is written box by
	/// box, the way the format lays them out, and an audio track's own clock must never be mistaken
	/// for the video's.
	#[test]
	fn an_mp4_header_says_what_it_holds() {
		let bytes = fixture_mp4();
		let media = parse_mp4(&bytes).expect("the fixture parses");

		assert_eq!(media.codec.as_deref(), Some("H.264"), "the video track's sample entry");
		assert_eq!(media.size, Some([ 640, 360 ]));
		assert_eq!(media.duration, Some(2.0), "the mvhd clock: 60000 / 30000");
		assert_eq!(media.fps, Some(30.0), "the track's 9000 ticks over 300 samples, at timescale 900");

		// junk is not an mp4, and a header with no video track in it is no answer either
		assert!(parse_mp4(b"this is not a video at all").is_none());
		assert!(parse_mp4(&[ 0, 0, 0, 8, b'f', b'r', b'e', b'e' ]).is_none());

		// an audio track first, the video track second: the audio's own rate is not the answer
		let reversed = fixture_mp4_with_audio_first();
		let media = parse_mp4(&reversed).expect("the reversed fixture parses");
		assert_eq!(media.codec.as_deref(), Some("H.264"), "the video track's codec, not the audio's");
		assert_eq!(media.fps, Some(30.0), "and the video track's own rate");
	}

	/// A matroska/webm's own header: the codec, the pixels, the duration the segment's clock states
	/// and the frame rate `DefaultDuration` gives.
	#[test]
	fn a_mkv_header_says_what_it_holds() {
		let bytes = fixture_mkv();
		let media = parse_ebml(&bytes).expect("the fixture parses");

		assert_eq!(media.codec.as_deref(), Some("H.264"), "the V_MPEG4/ISO/AVC codec id");
		assert_eq!(media.size, Some([ 1280, 720 ]));
		assert_eq!(media.duration, Some(4.0), "4000 ticks of a millisecond scale");
		assert_eq!(media.fps, Some(25.0), "40 000 000 ns a frame");

		assert!(parse_ebml(b"junk at all").is_none(), "junk is not an ebml header");
		assert!(parse_ebml(&fixture_mkv_audio_only()).is_none(), "an audio-only file has no video facts");
	}

	/// The card's media rows: the facts that are there, the ratio only where the column said how
	/// wide it is, and nothing printed empty.
	#[test]
	fn the_media_rows_are_the_facts_that_are_there() {
		let full = Media {
			codec:    Some("H.264".to_owned()),
			size:     Some([ 1920, 1080 ]),
			duration: Some(83.52),
			fps:      Some(29.97),
		};

		let rows: std::collections::HashMap<_, _> = full.rows(Some(454.0)).into_iter().collect();
		assert_eq!(rows.get("编码").map(String::as_str), Some("H.264"));
		assert_eq!(rows.get("分辨率").map(String::as_str), Some("1920 × 1080"));
		assert_eq!(rows.get("显示比例").map(String::as_str), Some("24%"), "454 of 1920");
		assert_eq!(rows.get("时长").map(String::as_str), Some("1 分 24 秒"));
		assert_eq!(rows.get("帧率").map(String::as_str), Some("29.97 fps"));

		// the column has not said how wide it is: the ratio row steps out with the rest
		let rows: std::collections::HashMap<_, _> = full.rows(None).into_iter().collect();
		assert!(!rows.contains_key("显示比例"), "{rows:?}");
		assert!(rows.contains_key("编码") && rows.contains_key("帧率"), "and the rest stand");

		// a header that said little: those rows are not there
		let bare = Media { codec: Some("AV1".to_owned()), ..Media::default() };
		let rows: std::collections::HashMap<_, _> = bare.rows(Some(100.0)).into_iter().collect();
		assert_eq!(rows.len(), 1, "{rows:?}");
		assert_eq!(rows.get("编码").map(String::as_str), Some("AV1"));

		// the names this window calls videos, and the text files it does not
		assert!(is_video("a.mp4") && is_video("B.MKV") && is_video("x.webm") && is_video("film.mov"));
		assert!(!is_video("a.txt") && !is_video("archive") && !is_video("a.png"));
	}

	/// A tiny mp4's boxes: ftyp, moov (the header, one video track whose own clock and sample table
	/// are the fps's), and stsd for the codec and the pixels.
	fn fixture_mp4() -> Vec<u8> {
		let ftyp = box_of(b"ftyp", b"isom\0\0\0\0isom");
		let moov = box_of(b"moov", &[ mvhd(60000, 30000), trak(None, 900, 9000, 300) ].concat());

		[ ftyp, moov ].concat()
	}

	/// The same fixture with an audio track ahead of the video one: a per-track walk must tell them
	/// apart by the handler, and answer from the video one.
	fn fixture_mp4_with_audio_first() -> Vec<u8> {
		let ftyp = box_of(b"ftyp", b"isom\0\0\0\0isom");
		let audio = trak(Some(*b"soun"), 48000, 48000, 1);
		let video = trak(None, 900, 9000, 300);
		let moov = box_of(b"moov", &[ mvhd(60000, 30000), audio, video ].concat());

		[ ftyp, moov ].concat()
	}

	fn mvhd(duration: u32, timescale: u32) -> Vec<u8> {
		let mut payload = vec![ 0u8; 4 ];
		payload.extend_from_slice(&[ 0u8; 4 ]);
		payload.extend_from_slice(&[ 0u8; 4 ]);
		payload.extend_from_slice(&timescale.to_be_bytes());
		payload.extend_from_slice(&duration.to_be_bytes());
		payload.extend_from_slice(&[ 0u8; 84 ]);

		box_of(b"mvhd", &payload)
	}

	/// One track: the media header's own clock, the handler, the sample table's totals and the
	/// sample entry's codec and pixels.
	fn trak(handler: Option<[u8; 4]>, timescale: u32, ticks: u32, samples: u32) -> Vec<u8> {
		let mut mdhd = vec![ 0u8; 4 ];
		mdhd.extend_from_slice(&[ 0u8; 4 ]);
		mdhd.extend_from_slice(&[ 0u8; 4 ]);
		mdhd.extend_from_slice(&timescale.to_be_bytes());
		mdhd.extend_from_slice(&ticks.to_be_bytes());
		mdhd.extend_from_slice(&[ 0u8; 4 ]);
		let mdhd = box_of(b"mdhd", &mdhd);

		let mut hdlr = vec![ 0u8; 8 ];
		hdlr.extend_from_slice(&handler.unwrap_or(*b"vide"));
		hdlr.extend_from_slice(&[ 0u8; 12 ]);
		let hdlr = box_of(b"hdlr", &hdlr);

		let mut stts = vec![ 0u8; 4 ];
		stts.extend_from_slice(&1u32.to_be_bytes());
		stts.extend_from_slice(&samples.to_be_bytes());
		stts.extend_from_slice(&(ticks / samples.max(1)).to_be_bytes());
		let stts = box_of(b"stts", &stts);

		// stsd: version+flags, one entry whose first four bytes are its own name (avc1)
		let mut entry = vec![ 0u8; 4 ];
		entry.extend_from_slice(b"avc1");
		entry.extend_from_slice(&[ 0u8; 16 ]);
		entry.extend_from_slice(&640u16.to_be_bytes());
		entry.extend_from_slice(&360u16.to_be_bytes());
		entry.extend_from_slice(&[ 0u8; 4 ]);
		let mut stsd = vec![ 0u8; 4 ];
		stsd.extend_from_slice(&1u32.to_be_bytes());
		stsd.extend_from_slice(&entry);
		let stsd = box_of(b"stsd", &stsd);

		let stbl = box_of(b"stbl", &[ stts, stsd ].concat());
		let minf = box_of(b"minf", &stbl);
		let mdia = box_of(b"mdia", &[ mdhd, hdlr, minf ].concat());

		box_of(b"trak", &mdia)
	}

	/// A tiny mkv/webm: the EBML header, a segment whose info states the millisecond scale and a
	/// duration of 4000 ticks, and one video track with its codec, pixels and frame duration.
	fn fixture_mkv() -> Vec<u8> {
		let ebml = element(0x1A45_DFA3, &element_uint(0x4286, 1));
		let segment = element(
			0x1853_8067,
			&[
				element(0x1549_A966, &[ element_uint(0x2AD7B1, 1_000_000), element_float(0x4489, 4000.0) ].concat()),
				element(
					0x1654_AE6B,
					&element(
						0xAE,
						&[
							element_uint(0x83, 1),
							element_string(0x86, b"V_MPEG4/ISO/AVC"),
							element_uint(0xB0, 1280),
							element_uint(0xBA, 720),
							element_uint(0x23E383, 40_000_000),
						]
						.concat(),
					),
				),
			]
			.concat(),
		);

		[ ebml, segment ].concat()
	}

	/// The same shape with an audio-only track: nothing for a media card to say.
	fn fixture_mkv_audio_only() -> Vec<u8> {
		let ebml = element(0x1A45_DFA3, &element_uint(0x4286, 1));
		let segment = element(
			0x1853_8067,
			&element(0x1654_AE6B, &element(0xAE, &[ element_uint(0x83, 2), element_string(0x86, b"A_OPUS") ].concat())),
		);

		[ ebml, segment ].concat()
	}

	fn box_of(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
		let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
		out.extend_from_slice(kind);
		out.extend_from_slice(payload);
		out
	}

	fn element(id: u64, payload: &[u8]) -> Vec<u8> {
		let mut out = id_bytes(id);
		out.extend_from_slice(&vint_bytes(payload.len() as u64));
		out.extend_from_slice(payload);
		out
	}

	fn element_uint(id: u64, value: u64) -> Vec<u8> {
		let bytes: Vec<u8> = value.to_be_bytes().into_iter().skip_while(|byte| *byte == 0).collect();
		element(id, &if bytes.is_empty() { vec![ 0 ] } else { bytes })
	}

	fn element_string(id: u64, text: &[u8]) -> Vec<u8> { element(id, text) }

	fn element_float(id: u64, value: f64) -> Vec<u8> { element(id, &value.to_bits().to_be_bytes()) }

	/// An id's own bytes: the spec's id already carries its marker, so the bytes are the number's
	/// own big-endian ones (0x2AD7B1 is `2A D7 B1`).
	fn id_bytes(id: u64) -> Vec<u8> {
		let mut bytes = Vec::new();
		let mut value = id;
		while value > 0 {
			bytes.push((value & 0xff) as u8);
			value >>= 8;
		}
		bytes.reverse();
		bytes
	}

	/// A size's own bytes, in the shortest width that holds them.
	fn vint_bytes(value: u64) -> Vec<u8> {
		let mut width = 1;
		while width < 8 && value >= (1u64 << (7 * width)) - 1 {
			width += 1;
		}

		let mut bytes = Vec::new();
		let mut rest = value;
		for _ in 0..width {
			bytes.push((rest & 0xff) as u8);
			rest >>= 8;
		}
		bytes.reverse();
		bytes[0] |= 1 << (8 - width) as u8;
		bytes
	}
}