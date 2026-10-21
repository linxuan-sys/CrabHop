//! The file managers' own clipboard exchange (P35④ phase two): a Wayland selection offering
//! `text/uri-list`, GNOME's `x-special/gnome-copied-files` and KDE's cut marker, beside the
//! plain-text mimes the window's paths have carried since phase one — so Nautilus and Thunar
//! can paste the window's targets as files. Pure Rust, and no external binary anywhere:
//! `wl-clipboard-rs` speaks `wayland-client` on a thread of its own. A session without a
//! compositor simply loses this arm and the caller falls back to the text road — exactly one
//! of the two ever takes the selection, so they can never fight over it.
//!
//! Reading other programs' `text/uri-list` back (their files pasted into this window) is the
//! other half of the exchange, and stays on the community list for now.

use std::path::{Path, PathBuf};

/// One line of every payload: `file://` + the path, percent-encoded — the same spelling the
/// `c C` URL copy has always used.
fn uri(path: &Path) -> String {
	crate::preview::file_uri(path)
}

/// The RFC 2483 payload: one URI to a line, CRLF-terminated.
pub(crate) fn uri_list(paths: &[PathBuf]) -> String {
	let mut out = String::new();
	for path in paths {
		out.push_str(&uri(path));
		out.push_str("\r\n");
	}

	out
}

/// GNOME's own flavor: the operation on the first line (`copy` / `cut`), then the URIs.
pub(crate) fn gnome_copied_files(paths: &[PathBuf], cut: bool) -> String {
	let mut out = if cut { "cut\n".to_owned() } else { "copy\n".to_owned() };
	for path in paths {
		out.push_str(&uri(path));
		out.push('\n');
	}

	out
}

/// KDE's cut flag: `1` is a cut, anything else a copy.
pub(crate) fn kde_cut(cut: bool) -> String {
	if cut { "1".to_owned() } else { "0".to_owned() }
}

/// The paths as the plain text every text program reads, one per line — phase one's payload.
pub(crate) fn paths_text(paths: &[PathBuf]) -> String {
	paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join("\n")
}

/// One `MimeSource` of a whole-slice payload.
fn source(mime: &str, data: Vec<u8>) -> wl_clipboard_rs::copy::MimeSource {
	wl_clipboard_rs::copy::MimeSource {
		source:    wl_clipboard_rs::copy::Source::Bytes(data.into_boxed_slice()),
		mime_type: wl_clipboard_rs::copy::MimeType::Specific(mime.to_owned()),
	}
}

/// Offer the targets to the session's clipboard in every flavor at once.
///
/// `false` when the clipboard could not be reached — no compositor, a session without Wayland,
/// a broken socket — and the caller takes the text road instead (`ctx.copy_text`). The copy is
/// prepared on the crate's own thread; a preparation error comes back here synchronously, and
/// the serving then lives until another program takes the selection over.
pub(crate) fn offer(paths: &[PathBuf], cut: bool) -> bool {
	use wl_clipboard_rs::copy::{Options, MimeSource};

	let text = paths_text(paths);
	let sources: Vec<MimeSource> = vec![
		source("text/uri-list", uri_list(paths).into_bytes()),
		source("x-special/gnome-copied-files", gnome_copied_files(paths, cut).into_bytes()),
		source("application/x-kde-cutselection", kde_cut(cut).into_bytes()),
		source("text/plain;charset=utf-8", text.clone().into_bytes()),
		source("text/plain", text.clone().into_bytes()),
		source("STRING", text.clone().into_bytes()),
		source("UTF8_STRING", text.clone().into_bytes()),
		source("TEXT", text.into_bytes()),
	];

	// the additional text types are spelled out above — without this, `text/uri-list` (a text
	// mime) would be the one expanded into them, and a text program would paste URIs; and the
	// payloads stand whole, newline endings included
	let mut options = Options::new();
	options.omit_additional_text_mime_types(true);
	options.trim_newline(false);
	options.copy_multi(sources).is_ok()
}

#[cfg(test)]
mod tests {
	use super::*;

	/// The URI list: one CRLF-terminated line per path, percent-encoded the way `c C` spells it —
	/// spaces, Chinese letters and the rest of the unsafe set all go through `%XX`.
	#[test]
	fn the_uri_list_is_one_encoded_line_per_path() {
		let paths = vec![ PathBuf::from("/tmp/a b.txt"), PathBuf::from("/tmp/中文/note.md") ];

		assert_eq!(uri_list(&paths), "file:///tmp/a%20b.txt\r\nfile:///tmp/%E4%B8%AD%E6%96%87/note.md\r\n");
		assert_eq!(uri_list(&[]), "", "nothing held is an empty list");
	}

	/// GNOME's flavor carries the operation first; KDE's is a one-byte flag. Both say the same
	/// thing about a cut as the window's own yank does.
	#[test]
	fn the_flavors_carry_the_operation() {
		let paths = vec![ PathBuf::from("/tmp/a.txt") ];

		assert_eq!(gnome_copied_files(&paths, false), "copy\nfile:///tmp/a.txt\n");
		assert_eq!(gnome_copied_files(&paths, true), "cut\nfile:///tmp/a.txt\n");
		assert_eq!(kde_cut(false), "0");
		assert_eq!(kde_cut(true), "1");
		assert_eq!(paths_text(&paths), "/tmp/a.txt", "and the text road is phase one's payload");
	}
}