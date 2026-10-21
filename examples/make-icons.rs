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
//!
//! The corners are rounded into transparency before the shrink, at the source's own resolution:
//! a launcher draws the icon as a square stamp, and a hard corner reads as a picture pasted onto
//! the panel rather than an app's own face (P44). The radius is the fraction a desktop icon
//! convention uses — a fifth of the side, the squircle-ish softer end of the range.

use std::path::Path;

use image::{Rgba, RgbaImage};

/// How round the corner is, as a fraction of the side. 0.2 is the soft-but-still-square end;
/// 0.25 is a full rounded square; above 0.3 it starts reading as a circle.
const RADIUS: f32 = 0.2;

fn main() {
	let source = Path::new("assets/icons/crabhop.png");
	let image = image::open(source).expect("the original icon is there and decodes").to_rgba8();
	println!("source: {} ({}×{})", source.display(), image.width(), image.height());

	let rounded = round_corners(&image, RADIUS);
	rounded.save(source).expect("the rounded original writes");

	for size in [ 512u32, 256, 128 ] {
		let scaled = image::imageops::resize(&rounded, size, size, image::imageops::FilterType::Lanczos3);
		let at = Path::new("assets/icons/hicolor").join(format!("{size}x{size}")).join("apps/crabhop.png");
		if let Some(dir) = at.parent() {
			std::fs::create_dir_all(dir).expect("the hicolor directory");
		}
		scaled.save(&at).expect("the scaled icon writes");
		println!("wrote {} ({size}×{size})", at.display());
	}
}

/// Clear every pixel outside a rounded square: the pixel's distance to the nearest corner
/// centre decides whether it is inside the arc, with one pixel of feather so the edge is not a
/// staircase. Pixels are premultiplied-agnostic here: the source is opaque, so clearing alpha is
/// the whole of it.
fn round_corners(image: &RgbaImage, fraction: f32) -> RgbaImage {
	let (w, h) = image.dimensions();
	let radius = (fraction * w.min(h) as f32).max(1.0);
	let mut out = image.clone();

	for y in 0..h {
		for x in 0..w {
			let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
			// find the corner this pixel would be near, if any
			let cx = if fx < radius {
				radius
			} else if fx > w as f32 - radius {
				w as f32 - radius
			} else {
				fx
			};
			let cy = if fy < radius {
				radius
			} else if fy > h as f32 - radius {
				h as f32 - radius
			} else {
				fy
			};
			let distance = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
			// inside: distance <= radius; one pixel of feather between radius-1 and radius
			if distance >= radius {
				out.put_pixel(x, y, Rgba([ 0, 0, 0, 0 ]));
			} else if distance > radius - 1.0 {
				let alpha = (radius - distance).clamp(0.0, 1.0);
				let p = out.get_pixel(x, y);
				out.put_pixel(x, y, Rgba([ p[0], p[1], p[2], (p[3] as f32 * alpha) as u8 ]));
			}
		}
	}

	out
}
