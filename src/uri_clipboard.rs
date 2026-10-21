
use std::path::{Path, PathBuf};

fn uri(path: &Path) -> String {
	crate::preview::file_uri(path)
}

pub(crate) fn uri_list(paths: &[PathBuf]) -> String {
	let mut out = String::new();
	for path in paths {
		out.push_str(&uri(path));
		out.push_str("\r\n");
	}

	out
}

pub(crate) fn gnome_copied_files(paths: &[PathBuf], cut: bool) -> String {
	let mut out = if cut { "cut\n".to_owned() } else { "copy\n".to_owned() };
	for path in paths {
		out.push_str(&uri(path));
		out.push('\n');
	}

	out
}

pub(crate) fn kde_cut(cut: bool) -> String {
	if cut { "1".to_owned() } else { "0".to_owned() }
}

pub(crate) fn paths_text(paths: &[PathBuf]) -> String {
	paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join("\n")
}

fn source(mime: &str, data: Vec<u8>) -> wl_clipboard_rs::copy::MimeSource {
	wl_clipboard_rs::copy::MimeSource {
		source:    wl_clipboard_rs::copy::Source::Bytes(data.into_boxed_slice()),
		mime_type: wl_clipboard_rs::copy::MimeType::Specific(mime.to_owned()),
	}
}

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

	let mut options = Options::new();
	options.omit_additional_text_mime_types(true);
	options.trim_newline(false);
	options.copy_multi(sources).is_ok()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn the_uri_list_is_one_encoded_line_per_path() {
		let paths = vec![ PathBuf::from("/tmp/a b.txt"), PathBuf::from("/tmp/中文/note.md") ];

		assert_eq!(uri_list(&paths), "file:///tmp/a%20b.txt\r\nfile:///tmp/%E4%B8%AD%E6%96%87/note.md\r\n");
		assert_eq!(uri_list(&[]), "", "nothing held is an empty list");
	}

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
