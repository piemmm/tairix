//! What a picture file is made of, as a person reads it: each record of its
//! container in file order, at the offset and length it occupies, with what it
//! declares — dimensions, colour model, density, palette, frames, colour
//! profile, text — and every EXIF tag by name with its value.
//!
//! The file is untrusted, so the report is built in a parser sandbox, never in
//! the process that draws it, within the bounds every report is held to
//! (`tairix_util::report`). A record that will not read ends the walk with
//! where and why, rather than a guess at what follows.

use tairix_util::report::{Builder, Report};

use crate::{bmp, gif, ico, jpeg, ora, png, sprite, tiff, webp, ImageFormat};

/// The structure report of `bytes`, read as `format`.
#[must_use]
pub fn inspect(bytes: &[u8], format: ImageFormat) -> Report {
    let mut report = Builder::default();
    match format {
        ImageFormat::Png => png::inspect::report(bytes, &mut report),
        ImageFormat::Jpeg => jpeg::inspect::report(bytes, &mut report),
        ImageFormat::Gif => gif::inspect::report(bytes, &mut report),
        ImageFormat::Bmp => bmp::inspect::report(bytes, &mut report),
        ImageFormat::Ico => ico::inspect::report(bytes, &mut report),
        ImageFormat::Sprite => sprite::inspect::report(bytes, &mut report),
        ImageFormat::Tiff => tiff::inspect::report(bytes, &mut report),
        ImageFormat::Webp => webp::inspect::report(bytes, &mut report),
        ImageFormat::OpenRaster => ora::inspect::report(bytes, &mut report),
    }
    report.finish()
}

/// `pixels` per metre as the dots per inch it amounts to, to the nearest.
pub(crate) fn dpi_of_ppm(pixels: u32) -> u64 {
    // An inch is 0.0254 m, so a metre's pixels × 127 / 5000 is an inch's.
    (u64::from(pixels) * 127 + 2500) / 5000
}

#[cfg(test)]
#[path = "inspect_tests.rs"]
mod tests;
