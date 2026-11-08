//! Page geometry: inherited attributes, boxes, rotation and the user unit.

use alloc::format;

use tairix_bezier::Point;

use crate::document::{Document, Options};
use crate::fixture::Builder;
use crate::PdfError;

const OPTIONS: Options<'static> = Options {
    password: None,
    budget: 1 << 20,
};

/// A document whose page tree root states `node` and whose one page states
/// `page`.
fn page_file(node: &str, page: &str) -> alloc::vec::Vec<u8> {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, &format!("<</Type /Pages /Kids [3 0 R] {node}>>"))
        .object(3, &format!("<</Type /Page /Parent 2 0 R {page}>>"))
        .object(4, "[0 0 300 400]")
        .object(5, "<</Font <<>>>>");
    pdf.tabled(1, "")
}

fn size(node: &str, page: &str) -> Result<(f64, f64), PdfError> {
    let file = page_file(node, page);
    let document = Document::open(&file, &OPTIONS)?;
    Ok(document.page(0)?.size())
}

#[test]
fn a_page_inherits_its_box_and_rotation_from_the_tree() {
    assert_eq!(size("/MediaBox [0 0 300 400]", ""), Ok((300.0, 400.0)));
    assert_eq!(size("/MediaBox 4 0 R /Rotate 90", ""), Ok((400.0, 300.0)));
    assert_eq!(
        size("/MediaBox [0 0 300 400] /Rotate 90", "/Rotate 0"),
        Ok((300.0, 400.0))
    );
    assert_eq!(
        size("/MediaBox [0 0 1 1]", "/MediaBox [10 20 110 70]"),
        Ok((100.0, 50.0))
    );
}

#[test]
fn a_page_inherits_its_resources() {
    let file = page_file("/Resources 5 0 R", "");
    let document = Document::open(&file, &OPTIONS).expect("a document");
    let page = document.page(0).expect("a page");
    let resources = page.resources().expect("its resources");
    assert!(resources.get(b"Font").is_some());
}

#[test]
fn a_page_with_no_usable_media_box_is_letter() {
    for media in [
        "",
        "/MediaBox [0 0 0 100]",
        "/MediaBox [0 0 1e9 100]",
        "/MediaBox (x)",
        "/MediaBox [1 2 3]",
    ] {
        assert_eq!(size("", media), Ok((612.0, 792.0)), "{media}");
    }
}

#[test]
fn the_crop_box_is_held_within_the_media_box() {
    assert_eq!(
        size("/MediaBox [0 0 300 400]", "/CropBox [100 100 200 150]"),
        Ok((100.0, 50.0))
    );
    assert_eq!(
        size("/MediaBox [0 0 300 400]", "/CropBox [-50 -50 100 100]"),
        Ok((100.0, 100.0))
    );
    assert_eq!(
        size("/MediaBox [0 0 300 400]", "/CropBox [500 500 600 600]"),
        Ok((300.0, 400.0))
    );
    assert_eq!(
        size("/MediaBox [0 0 300 400]", "/CropBox [200 150 100 100]"),
        Ok((100.0, 50.0))
    );
}

#[test]
fn a_rotation_is_read_in_quarter_turns() {
    let media = "/MediaBox [0 0 300 400]";
    for (rotate, turned) in [
        (90, true),
        (-90, true),
        (180, false),
        (450, true),
        (-360, false),
        (45, false),
    ] {
        let expected = if turned {
            (400.0, 300.0)
        } else {
            (300.0, 400.0)
        };
        assert_eq!(
            size(media, &format!("/Rotate {rotate}")),
            Ok(expected),
            "{rotate}"
        );
    }
}

#[test]
fn the_user_unit_scales_the_page() {
    let media = "/MediaBox [0 0 300 400]";
    assert_eq!(size(media, "/UserUnit 2"), Ok((600.0, 800.0)));
    for bad in ["0", "-1", "1e9", "(x)"] {
        assert_eq!(
            size(media, &format!("/UserUnit {bad}")),
            Ok((300.0, 400.0)),
            "{bad}"
        );
    }
}

#[test]
fn the_transform_puts_the_crop_box_top_left_at_the_origin_for_every_turn() {
    let corners = [(10.0, 20.0), (110.0, 20.0), (110.0, 70.0), (10.0, 70.0)];
    // Where each corner of the crop box lands, y down, for each quarter turn.
    let expected = [
        [(0.0, 50.0), (100.0, 50.0), (100.0, 0.0), (0.0, 0.0)],
        [(0.0, 0.0), (0.0, 100.0), (50.0, 100.0), (50.0, 0.0)],
        [(100.0, 0.0), (0.0, 0.0), (0.0, 50.0), (100.0, 50.0)],
        [(50.0, 100.0), (50.0, 0.0), (0.0, 0.0), (0.0, 100.0)],
    ];
    for (turns, expected) in expected.iter().enumerate() {
        let file = page_file(
            "",
            &format!("/MediaBox [10 20 110 70] /Rotate {}", turns * 90),
        );
        let document = Document::open(&file, &OPTIONS).expect("a document");
        let transform = document.page(0).expect("a page").transform();
        for (&(x, y), &(ex, ey)) in corners.iter().zip(expected) {
            let mapped = transform.map(Point::new(x, y));
            assert!(
                (mapped.x - ex).abs() < 1e-9 && (mapped.y - ey).abs() < 1e-9,
                "turn {turns}: ({x}, {y}) mapped to {mapped:?}, not ({ex}, {ey})"
            );
        }
    }
}
