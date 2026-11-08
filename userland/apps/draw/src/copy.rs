//! Copies of a drawing in other formats: the vector formats written whole,
//! and pictures rendered through the one renderer at the drawing's copy
//! resolution, each recording the resolution it was made at.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::fs::Filetype;
use tairix_bezier::{Affine, Point};
use tairix_browse::media::{media_for_document, MediaType};
use tairix_drawing::render::{artwork, Afresh, Mode, View as Square};
use tairix_drawing::{Drawing, DrawingError, Glyphs, PAPER_WHITE, PIXELS_PER_INCH};
use tairix_image::{
    encode_jpeg, encode_png, encode_tiff, Density, DensityUnit, JpegOptions, Picture, TiffOptions,
};
use tairix_raster::{unpremultiply_span, Color, Surface};
use tairix_util::mathf::{floor, fmax, round_i32};

use crate::listed;
use crate::tiles::TILE;

/// The longest side a picture copied from a drawing has, in pixels: a
/// containment bound on the memory a copy takes, four bytes a pixel.
pub const MOST_SIDE: f64 = 8192.0;

/// The format a copy is written in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CopyFormat {
    /// SVG.
    Svg,
    /// PDF.
    Pdf,
    /// Encapsulated PostScript.
    Eps,
    /// PNG.
    Png,
    /// JPEG, over white paper where nothing is drawn.
    Jpeg,
    /// TIFF.
    Tiff,
    /// A RISC OS Draw file.
    Draw,
}

impl CopyFormat {
    /// The format a file called `name`, whose volume records `recorded` as
    /// its RISC OS filetype, is written in, if Draw writes one: the type the
    /// desktop's media registry takes the file to be, by its extension, by
    /// the filetype after a comma, or by the one recorded.
    #[must_use]
    pub fn of(name: &str, recorded: Option<Filetype>) -> Option<Self> {
        Some(match media_for_document(name, recorded)? {
            MediaType::ImageSvg => Self::Svg,
            MediaType::Pdf => Self::Pdf,
            MediaType::PostScript => Self::Eps,
            MediaType::ImagePng => Self::Png,
            MediaType::ImageJpeg => Self::Jpeg,
            MediaType::ImageTiff => Self::Tiff,
            MediaType::RiscOsDraw => Self::Draw,
            _ => return None,
        })
    }
}

/// A copy written: its bytes, and what it could not keep that the user is
/// told.
#[derive(Clone, Debug)]
pub struct Copied {
    /// The file.
    pub bytes: Vec<u8>,
    /// What it could not keep, as the message saying it was saved reads on.
    pub note: Option<String>,
}

/// `drawing` written as `format`, its text set from `glyphs`.
///
/// # Errors
///
/// Why it could not be written, as the window says it.
pub fn copy(format: CopyFormat, drawing: &Drawing, glyphs: &dyn Glyphs) -> Result<Copied, String> {
    let written = |bytes, note| Ok(Copied { bytes, note });
    let failed = refused;
    match format {
        CopyFormat::Svg => written(
            tairix_drawing::svg::write(drawing)
                .map_err(failed)?
                .into_bytes(),
            None,
        ),
        CopyFormat::Pdf => written(
            tairix_drawing::pdf::pdf(drawing, glyphs)
                .map_err(failed)?
                .bytes,
            None,
        ),
        CopyFormat::Eps => {
            let eps = tairix_drawing::eps::eps(drawing, glyphs).map_err(failed)?;
            let note = eps
                .flattened
                .then(|| String::from("its transparency made opaque, as PostScript paints it"));
            written(eps.bytes, note)
        }
        CopyFormat::Draw => {
            let draw = tairix_drawing::riscos::write(drawing, glyphs).map_err(failed)?;
            let note = (!draw.dropped.is_empty()).then(|| {
                alloc::format!(
                    "its {} drawn as near as a Draw file holds them",
                    listed(draw.dropped.iter())
                )
            });
            written(draw.bytes, note)
        }
        CopyFormat::Png | CopyFormat::Jpeg | CopyFormat::Tiff => {
            let (picture, resolution) = raster(drawing, glyphs)?;
            let encoded = match format {
                CopyFormat::Png => encode_png(&picture),
                CopyFormat::Jpeg => JpegOptions::new(
                    JpegOptions::DEFAULT_QUALITY,
                    PAPER_WHITE.without_alpha().to_array(),
                )
                .and_then(|options| encode_jpeg(&picture, options)),
                _ => encode_tiff(&[&picture], TiffOptions::default()),
            };
            let bytes = encoded.map_err(|err| alloc::format!("it could not be encoded ({err})"))?;
            let note = (resolution < drawing.resolution())
                .then(|| alloc::format!("at {resolution} dpi, as large as a picture is copied"));
            written(bytes, note)
        }
    }
}

/// Why a copy could not be made of a drawing that refused `error`, as the
/// window says it.
fn refused(error: DrawingError) -> String {
    String::from(match error {
        DrawingError::OutOfMemory => "there is not enough memory to make it",
        DrawingError::TooComplex => "the drawing is too complex for it",
        DrawingError::Invalid => "the drawing holds a value it cannot",
    })
}

/// The page of `drawing` rendered at its copy resolution — scaled down,
/// where that would make a side longer than [`MOST_SIDE`], to fit — and the
/// resolution it was made at, which the picture records.
///
/// # Errors
///
/// Why it could not be rendered, as the window says it.
pub fn raster(drawing: &Drawing, glyphs: &dyn Glyphs) -> Result<(Picture, u32), String> {
    let size = drawing.page().size();
    let inch = f64::from(PIXELS_PER_INCH);
    // The finest whole resolution within the bound, so a picture made at it
    // says what it was made at.
    let finest = floor(MOST_SIDE / fmax(fmax(size.x, size.y), 1.0) * inch);
    let asked = drawing.resolution();
    let resolution = if f64::from(asked) <= finest {
        asked
    } else {
        u32::try_from(round_i32(finest)).unwrap_or(1).max(1)
    };
    let scale = f64::from(resolution) / inch;
    let pixels_of = |length: f64| u32::try_from(round_i32(fmax(length * scale, 1.0))).unwrap_or(1);
    let (width, height) = (pixels_of(size.x), pixels_of(size.y));
    let short = || String::from("there is not enough memory to render the page");
    let bytes = usize::try_from(u64::from(width) * u64::from(height) * 4)
        .map_err(|_| String::from("the page is too large"))?;
    let mut rgba = tairix_util::fallible::zeroed(bytes).ok_or_else(short)?;
    let mut tile = Surface::new(TILE, TILE).ok_or_else(short)?;
    let pitch = usize::try_from(TILE).unwrap_or(0);
    let mut row = 0;
    while row < height {
        let mut column = 0;
        while column < width {
            tile.fill(Color::rgba(0, 0, 0, 0));
            let square = Square {
                to_device: Affine::scale(scale, scale),
                origin: Point::new(f64::from(column), f64::from(row)),
                side: f64::from(TILE),
                design: TILE * 256,
                mode: Mode::Painted,
                ground: true,
            };
            let art = artwork(drawing, &square, glyphs, &Afresh).map_err(refused)?;
            if !tile.draw_artwork(&art.nodes, square.design) {
                return Err(short());
            }
            let across = usize::try_from(TILE.min(width - column)).unwrap_or(0);
            let down = usize::try_from(TILE.min(height - row)).unwrap_or(0);
            for (y, line) in tile.pixels().chunks_exact(pitch).take(down).enumerate() {
                let first = u64::from(row) + u64::try_from(y).unwrap_or(0);
                let start = usize::try_from((first * u64::from(width) + u64::from(column)) * 4)
                    .unwrap_or(usize::MAX);
                let Some(out) = rgba.get_mut(start..start.saturating_add(across * 4)) else {
                    continue;
                };
                unpremultiply_span(line, out.as_chunks_mut::<4>().0);
            }
            column += TILE;
        }
        row += TILE;
    }
    let picture = Picture::rgba(width, height, rgba)
        .map_err(|err| alloc::format!("{err}"))?
        .with_density(Density::whole(resolution, resolution, DensityUnit::Inch));
    Ok((picture, resolution))
}

#[cfg(test)]
#[path = "copy_tests.rs"]
mod tests;
