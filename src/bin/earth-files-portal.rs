// SPDX-License-Identifier: GPL-3.0-only

//! The file chooser backend for `xdg-desktop-portal`.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _ = jxl_oxide::integration::register_image_decoding_hook();
    earth_files::portal::main()
}
