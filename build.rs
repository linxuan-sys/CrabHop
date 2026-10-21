//! PDFium, linked in. Chromium's libpdfium references its own C++ runtime, and the fetched tarball
//! ships it: libc++.a and libc++abi.a sit next to libpdfium.a. The three are grouped, because a
//! static archive only resolves the symbols it has already seen by the time it is scanned, and the
//! order the other link arguments arrive in is not ours to promise.
//!
//! The library lives in exactly one place: `libs/pdfium/lib`, fetched by `scripts/fetch-pdfium.sh` (the pinned sha256 is that script's).
//! `.cargo/config.toml` points `PDFIUM_STATIC_LIB_PATH` there.

fn main() {
	println!("cargo:rerun-if-env-changed=PDFIUM_STATIC_LIB_PATH");

	let Ok(dir) = std::env::var("PDFIUM_STATIC_LIB_PATH") else {
		println!(
			"cargo:warning=PDFIUM_STATIC_LIB_PATH is unset; PDF pages will not render (check .cargo/config.toml)"
		);
		return;
	};

	if !std::path::Path::new(&dir).join("libpdfium.a").exists() {
		println!("cargo:warning=libpdfium.a is not in {dir}; run scripts/fetch-pdfium.sh");
	}

	println!("cargo:rustc-link-search=native={dir}");
	println!("cargo:rustc-link-lib=static=pdfium");
	println!("cargo:rustc-link-lib=static=c++");
	println!("cargo:rustc-link-lib=static=c++abi");

	// and again as one group, so the linker keeps all three in sight at once
	println!("cargo:rustc-link-arg=-Wl,--start-group");
	println!("cargo:rustc-link-arg=-lpdfium");
	println!("cargo:rustc-link-arg=-lc++");
	println!("cargo:rustc-link-arg=-lc++abi");
	println!("cargo:rustc-link-arg=-Wl,--end-group");
}