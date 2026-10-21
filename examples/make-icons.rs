//! The hicolor tree's own images, made from the one original (P43①):
//!
//!     cargo run --release --example make-icons
//!
//! Pure Rust: the `image` crate is the project's own dependency, so a packaging build needs
//! nothing but cargo — no ImageMagick, no external converter. The products are committed; this
//! tool is how they were made, and how they are made again when the original changes.
//!
//! In:  `assets/icons/crabhop.png` (the source, any square size).
//! Out: `assets/icons/hicolor/{512x512,256x256,128x128}/apps/crabhop.png`.

use std::path::Path;

fn main() {
	let source = Path::new("assets/icons/crabhop.png");
	let image = image::open(source).expect("the original icon is there and decodes");
	println!("source: {} ({}×{})", source.display(), image.width(), image.height());

	for size in [ 512u32, 256, 128 ] {
		let scaled = image.resize_exact(size, size, image::imageops::FilterType::Lanczos3);
		let at = Path::new("assets/icons/hicolor").join(format!("{size}x{size}")).join("apps/crabhop.png");
		if let Some(dir) = at.parent() {
			std::fs::create_dir_all(dir).expect("the hicolor directory");
		}
		scaled.save(&at).expect("the scaled icon writes");
		println!("wrote {} ({size}×{size})", at.display());
	}
}