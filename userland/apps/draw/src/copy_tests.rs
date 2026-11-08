//! Copies of a drawing: formats by name, pictures at the drawing's
//! resolution, and what each format could not keep.

use alloc::sync::Arc;
use alloc::vec;

use tairix_abi::fs::Filetype;
use tairix_colour::Rgba;
use tairix_drawing::shape::RectShape;
use tairix_drawing::{Children, Drawing, Grid, Ids, Kind, Node, Page, Paint, Shape, Style, Unit};
use tairix_image::DensityUnit;

use super::{copy, raster, CopyFormat, MOST_SIDE};
use crate::testing::NoGlyphs;

/// A page `width` by `height` pixels holding a rectangle over its left half,
/// filled `fill`.
fn page_of(width: f64, height: f64, fill: Rgba) -> Drawing {
    let mut ids = Ids::new();
    let rect = Node::new(
        ids.issue(),
        Kind::Shape {
            shape: Shape::Rect(RectShape {
                x: 0.0,
                y: 0.0,
                width: width / 2.0,
                height,
                rx: 0.0,
                ry: 0.0,
            }),
            style: Arc::new(Style {
                fill: Paint::Colour(fill),
                ..Style::default()
            }),
        },
    )
    .expect("a rectangle");
    let layer = Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::from_nodes(vec![Arc::new(rect)]).expect("held"),
            clip: None,
        },
    )
    .expect("a layer");
    Drawing::new(
        Page {
            width,
            height,
            unit: Unit::Px,
            background: None,
        },
        Children::from_nodes(vec![Arc::new(layer)]).expect("held"),
        tairix_drawing::SharedSlice::new(),
        Grid::default(),
    )
    .expect("a drawing")
}

#[test]
fn a_copys_format_is_the_type_the_desktop_takes_its_name_to_be() {
    for (name, format) in [
        ("a.SVG", CopyFormat::Svg),
        ("b.pdf", CopyFormat::Pdf),
        ("c.eps", CopyFormat::Eps),
        ("d.ps", CopyFormat::Eps),
        ("e.png", CopyFormat::Png),
        ("f.jpg", CopyFormat::Jpeg),
        ("g.JPEG", CopyFormat::Jpeg),
        ("h.tif", CopyFormat::Tiff),
        ("i.tiff", CopyFormat::Tiff),
        ("j,aff", CopyFormat::Draw),
        ("k,aad", CopyFormat::Svg),
    ] {
        assert_eq!(CopyFormat::of(name, None), Some(format), "{name}");
    }
    assert_eq!(CopyFormat::of("picture.gif", None), None);
    assert_eq!(CopyFormat::of("no-extension", None), None);
    let draw = Filetype::new(0xAFF);
    assert_eq!(
        CopyFormat::of("Plan", draw),
        Some(CopyFormat::Draw),
        "a RISC OS file known by the filetype its volume records"
    );
    assert_eq!(
        CopyFormat::of("Plan.pdf", draw),
        Some(CopyFormat::Pdf),
        "its name first"
    );
}

#[test]
fn a_draw_file_copy_says_what_it_could_not_keep() {
    let opaque = copy(
        CopyFormat::Draw,
        &page_of(40.0, 20.0, Rgba::new(10, 160, 90, 255)),
        &NoGlyphs,
    )
    .expect("copied");
    assert!(opaque.bytes.starts_with(b"Draw"));
    assert_eq!(
        opaque.note.as_deref(),
        Some("A Draw file holds the drawing's page size only as near as it can"),
        "a page no A-series paper is"
    );
    let veiled = copy(
        CopyFormat::Draw,
        &page_of(40.0, 20.0, Rgba::new(10, 160, 90, 128)),
        &NoGlyphs,
    )
    .expect("copied");
    assert_eq!(
        veiled.note.as_deref(),
        Some("A Draw file holds the drawing's transparency and page size only as near as it can")
    );
}

#[test]
fn a_picture_is_made_at_the_drawings_resolution_and_says_so() {
    let red = Rgba::new(220, 20, 30, 255);
    let drawing = page_of(100.0, 50.0, red)
        .with_resolution(192)
        .expect("192 dpi");
    let (picture, resolution) = raster(&drawing, &NoGlyphs).expect("rendered");
    assert_eq!(resolution, 192);
    assert_eq!(
        (picture.width(), picture.height()),
        (200, 100),
        "twice the pixels"
    );
    let density = picture.density().expect("a density");
    assert_eq!(density.unit(), DensityUnit::Inch);
    assert_eq!(density.across(), (192, 1));
    let tairix_image::Pixels::Rgba(rgba) = picture.pixels() else {
        panic!("rgba pixels");
    };
    let pixel = |x: usize, y: usize| {
        let at = (y * 200 + x) * 4;
        [rgba[at], rgba[at + 1], rgba[at + 2], rgba[at + 3]]
    };
    assert_eq!(pixel(10, 50), [220, 20, 30, 255], "the rectangle");
    assert_eq!(pixel(190, 50)[3], 0, "clear where nothing is drawn");
}

#[test]
fn a_picture_too_large_is_made_as_fine_as_fits_and_the_copy_says_so() {
    let drawing = page_of(20_000.0, 40.0, Rgba::new(0, 0, 0, 255));
    let (picture, resolution) = raster(&drawing, &NoGlyphs).expect("rendered");
    assert!(
        f64::from(picture.width()) <= MOST_SIDE,
        "{}",
        picture.width()
    );
    assert!(resolution < tairix_drawing::PIXELS_PER_INCH);
    let copied = copy(CopyFormat::Png, &drawing, &NoGlyphs).expect("copied");
    let note = copied.note.expect("a note");
    assert!(note.contains(&alloc::format!("{resolution} dpi")), "{note}");
}

#[test]
fn each_format_is_written_as_itself() {
    let drawing = page_of(40.0, 20.0, Rgba::new(10, 160, 90, 255));
    let written = |format| copy(format, &drawing, &NoGlyphs).expect("copied");
    assert!(written(CopyFormat::Png)
        .bytes
        .starts_with(b"\x89PNG\r\n\x1a\n"));
    assert!(written(CopyFormat::Jpeg)
        .bytes
        .starts_with(&[0xFF, 0xD8, 0xFF]));
    let tiff = written(CopyFormat::Tiff).bytes;
    assert!(tiff.starts_with(b"II*\0") || tiff.starts_with(b"MM\0*"));
    assert!(written(CopyFormat::Pdf).bytes.starts_with(b"%PDF-"));
    assert!(written(CopyFormat::Svg).bytes.starts_with(b"<?xml"));
    let eps = written(CopyFormat::Eps);
    assert!(eps.bytes.starts_with(b"%!PS-Adobe-3.0 EPSF-3.0"));
    assert!(eps.note.is_none(), "nothing translucent to make opaque");
    let veiled = page_of(40.0, 20.0, Rgba::new(10, 160, 90, 128));
    let flattened = copy(CopyFormat::Eps, &veiled, &NoGlyphs).expect("copied");
    assert!(flattened.note.is_some_and(|note| note.contains("opaque")));
}

#[test]
fn a_copy_refused_says_why_in_words() {
    for (error, says) in [
        (tairix_drawing::DrawingError::OutOfMemory, "memory"),
        (tairix_drawing::DrawingError::TooComplex, "too complex"),
        (tairix_drawing::DrawingError::Invalid, "value"),
    ] {
        let said = super::refused(error);
        assert!(said.contains(says) && !said.contains("Error"), "{said}");
    }
}
