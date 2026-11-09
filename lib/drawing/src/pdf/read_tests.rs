//! Reading PDFs into drawings: Draw's own files back, page choice, refusal.

#![allow(
    clippy::float_cmp,
    reason = "a dash offset written as a whole number is read back as that number"
)]

use alloc::format;
use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_bezier::Affine;
use tairix_collections::SharedStr;
use tairix_colour::Rgba;
use tairix_pdf::Picture;

use super::{open, read, Builder, Request};
use crate::drawing::Unit;
use crate::id::Ids;
use crate::limits::MAX_TOTAL_IMAGE_PIXELS;
use crate::node::{Kind, Node};
use crate::pdf::{pdf, recognised};
use crate::style::Paint;
use crate::testing::OnlyA;
use crate::unkept::{Lost, Unkept};
use crate::vector::tests::sample;
use crate::ReadError;

const REQUEST: Request<'static> = Request {
    page: 0,
    password: None,
    budget: 1 << 26,
};

/// Every node under `node`, depth first, `node` included.
fn all(node: &Arc<Node>, into: &mut Vec<Arc<Node>>) {
    into.push(Arc::clone(node));
    if let Kind::Group { children, clip } = node.kind() {
        if let Some(clip) = clip {
            into.push(Arc::clone(clip));
        }
        for child in children.iter() {
            all(child, into);
        }
    }
}

fn written_sample() -> Vec<u8> {
    let drawing = sample(&mut Ids::new());
    pdf(&drawing, &OnlyA).expect("a PDF").bytes
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).expect("hex digits"))
        .collect()
}

/// A one-page PDF, its page's content `content` as written, encrypted under
/// the security handler `encrypt` where one is given.
fn one_page(content: &[u8], encrypt: Option<&str>) -> Vec<u8> {
    const ID: &str = "fedcba9876543210fedcba9876543210";
    let mut file = Vec::new();
    file.extend_from_slice(
        b"%PDF-1.7\n1 0 obj <</Type /Catalog /Pages 2 0 R>> endobj\n\
        2 0 obj <</Type /Pages /Kids [3 0 R]>> endobj\n\
        3 0 obj <</Type /Page /MediaBox [0 0 100 100] /Contents 4 0 R>> endobj\n",
    );
    file.extend_from_slice(format!("4 0 obj <</Length {}>> stream\n", content.len()).as_bytes());
    file.extend_from_slice(content);
    file.extend_from_slice(b"\nendstream endobj\n");
    let trailer = match encrypt {
        Some(encrypt) => format!(
            "9 0 obj {encrypt} endobj\n\
            trailer <</Root 1 0 R /Encrypt 9 0 R /ID [<{ID}> <{ID}>]>>\n"
        ),
        None => format!("trailer <</Root 1 0 R /ID [<{ID}> <{ID}>]>>\n"),
    };
    file.extend_from_slice(trailer.as_bytes());
    file
}

/// Every shape `read` holds, depth first.
fn shapes(read: &crate::Read) -> Vec<Arc<Node>> {
    let mut nodes = Vec::new();
    for layer in read.drawing.layers().iter() {
        all(layer, &mut nodes);
    }
    nodes.retain(|node| matches!(node.kind(), Kind::Shape { .. }));
    nodes
}

#[test]
fn draws_own_pdf_reads_back_at_its_size() {
    let file = written_sample();
    assert!(recognised(&file));
    let read = read(file, &REQUEST, &mut Ids::new()).expect("a drawing");
    let page = read.drawing.page();
    assert_eq!(page.unit, Unit::Pt);
    let size = page.size();
    assert!(
        (size.x - 200.0).abs() < 1e-6 && (size.y - 100.0).abs() < 1e-6,
        "{size:?}"
    );
    let [layer] = read.drawing.layers().iter().collect::<Vec<_>>()[..] else {
        panic!("one layer");
    };
    let mut nodes = Vec::new();
    all(layer, &mut nodes);
    let shapes = nodes
        .iter()
        .filter(|node| matches!(node.kind(), Kind::Shape { .. }))
        .count();
    assert!(shapes >= 5, "{shapes} shapes");
    let translucent = nodes
        .iter()
        .any(|node| node.opacity() < 1.0 && matches!(node.kind(), Kind::Group { .. }));
    assert!(
        translucent,
        "the half-opaque group comes back as a group at its opacity"
    );
    let clipped = nodes
        .iter()
        .any(|node| matches!(node.kind(), Kind::Group { clip: Some(_), .. }));
    assert!(clipped, "the clipped group comes back clipped");
    assert!(
        !read.unkept.contains(Lost::Masks),
        "{:?}",
        read.unkept.iter().collect::<Vec<_>>()
    );
    assert!(!read.unkept.contains(Lost::Damage));
}

#[test]
fn a_stroked_and_filled_shape_reads_back_as_one_shape_with_its_pen() {
    let file = written_sample();
    let read = read(file, &REQUEST, &mut Ids::new()).expect("a drawing");
    let mut nodes = Vec::new();
    for layer in read.drawing.layers().iter() {
        all(layer, &mut nodes);
    }
    let pens: Vec<_> = nodes
        .iter()
        .filter_map(|node| match node.kind() {
            Kind::Shape { style, .. }
                if matches!(style.fill, Paint::Colour(_)) && style.stroke.paints() =>
            {
                Some(style.stroke.clone())
            }
            _ => None,
        })
        .collect();
    assert!(!pens.is_empty(), "a shape filled and stroked");
    let pen = &pens[0];
    assert_eq!(pen.dashes.as_slice(), &[4.0, 2.0]);
    assert_eq!(pen.dash_offset, 1.0);
}

#[test]
fn a_translucent_gradient_keeps_its_alpha() {
    let file = written_sample();
    let read = read(file, &REQUEST, &mut Ids::new()).expect("a drawing");
    let mut nodes = Vec::new();
    for layer in read.drawing.layers().iter() {
        all(layer, &mut nodes);
    }
    let alphas: Vec<u8> = nodes
        .iter()
        .filter_map(|node| match node.kind() {
            Kind::Shape { style, .. } => match &style.fill {
                Paint::Gradient(gradient) => Some(
                    gradient
                        .stops
                        .iter()
                        .map(|stop| stop.colour.a)
                        .min()
                        .unwrap_or(255),
                ),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert!(
        alphas.iter().any(|&alpha| alpha > 100 && alpha < 160),
        "{alphas:?}"
    );
}

#[test]
fn a_one_page_pdf_opens_to_one_page_at_its_size() {
    let file = written_sample();
    let opened = open(file, None, REQUEST.budget).expect("an opened PDF");
    assert_eq!(opened.page_count(), 1);
    let (width, height) = opened.page_size(0).expect("its size");
    assert!(
        (width - 150.0).abs() < 1e-6 && (height - 75.0).abs() < 1e-6,
        "{width} by {height}"
    );
    assert_eq!(opened.page_size(1), Err(ReadError::NoSuchPage));
}

#[test]
fn one_opening_counts_sizes_and_reads_every_page_asked_for() {
    let file = b"%PDF-1.7\n1 0 obj <</Type /Catalog /Pages 2 0 R>> endobj\n\
        2 0 obj <</Type /Pages /Kids [3 0 R 4 0 R 5 0 R]>> endobj\n\
        3 0 obj <</Type /Page /MediaBox [0 0 10 20]>> endobj\n\
        4 0 obj <</Type /Page /MediaBox [0 0 30 40]>> endobj\n\
        5 0 obj <</Type /Page /MediaBox [0 0 50 60]>> endobj\n";
    let opened = open(file.to_vec(), None, REQUEST.budget).expect("an opened PDF");
    assert_eq!(opened.page_count(), 3);
    assert_eq!(opened.page_size(2), Ok((50.0, 60.0)));
    assert_eq!(opened.page_size(0), Ok((10.0, 20.0)));
    for (page, width) in [(1, 30.0), (2, 50.0), (1, 30.0)] {
        let read = opened.read(page, &mut Ids::new()).expect("the page");
        assert_eq!(read.drawing.page().width, width);
        assert!(read.unkept.contains(Lost::OtherPages));
    }
    assert_eq!(
        opened.read(3, &mut Ids::new()).err(),
        Some(ReadError::NoSuchPage)
    );
}

#[test]
fn a_page_it_lacks_or_bytes_that_are_no_pdf_are_refused() {
    let file = written_sample();
    let past = Request { page: 1, ..REQUEST };
    assert_eq!(
        read(file, &past, &mut Ids::new()).err(),
        Some(ReadError::NoSuchPage)
    );
    assert_eq!(
        read(b"<svg/>".to_vec(), &REQUEST, &mut Ids::new()).err(),
        Some(ReadError::Unrecognised)
    );
    assert_eq!(
        read(b"%PDF-1.4 garbage".to_vec(), &REQUEST, &mut Ids::new()).err(),
        Some(ReadError::Malformed)
    );
}

#[test]
fn a_pdf_protected_by_its_owner_alone_reads_with_its_protection_lost() {
    // The handler's entries and the ciphertext come from an implementation
    // of ISO 32000 written apart from this one: the owner's password is
    // `keeper`, the user's empty.
    let file = one_page(
        &hex("b05d06f65e96032b13bdd210bc1a305019726f8f328a62fea9"),
        Some(
            "<</Filter /Standard /V 2 /R 3 /Length 128 /P -3904 \
            /O <e30980c9c379a50d357bcdbd282faf4b53590c65074dc985630572673ca059d5> \
            /U <5dc34cea5f19dbb5db2af945bce8dcfc00000000000000000000000000000000>>>",
        ),
    );
    let protected = read(file, &REQUEST, &mut Ids::new()).expect("a drawing");
    assert!(
        protected.unkept.contains(Lost::Protection),
        "{:?}",
        protected.unkept
    );
    let blue = Rgba::new(0, 0, 255, 255);
    let painted = shapes(&protected).iter().any(|node| {
        matches!(node.kind(), Kind::Shape { style, .. }
            if matches!(style.fill, Paint::Colour(colour) if colour == blue))
    });
    assert!(painted, "the page's square comes back decrypted");
    let clear = one_page(b"0 0 1 rg 10 10 50 50 re f", None);
    let clear = read(clear, &REQUEST, &mut Ids::new()).expect("a drawing");
    assert!(clear.unkept.is_empty(), "{:?}", clear.unkept);
    assert_eq!(shapes(&clear).len(), 1);
}

#[test]
fn a_pdf_refused_for_its_password_is_handed_back_to_be_opened_with_another() {
    // As above, the user's password being `open sesame`.
    let file = one_page(
        &hex("8be74153d5e3cb6e781d0afd213f38c5cd5c8d1353e15212f5"),
        Some(
            "<</Filter /Standard /V 2 /R 3 /Length 128 /P -3904 \
            /O <a4c6abf9ad7f4a3f3016e6c3689bf00d08fd4db7d76ba1fab608d3b776f3867f> \
            /U <92df1671434db6e95fd54175bc3e0cb500000000000000000000000000000000>>>",
        ),
    );
    let refused = open(file.clone(), None, REQUEST.budget)
        .err()
        .expect("a password wanted");
    assert_eq!(refused.error, ReadError::NeedsPassword);
    assert_eq!(refused.file, file);
    let refused = open(refused.file, Some(b"guess"), REQUEST.budget)
        .err()
        .expect("the wrong password");
    assert_eq!(refused.error, ReadError::WrongPassword);
    let opened = open(refused.file, Some(b"open sesame"), REQUEST.budget).expect("opened");
    let read = opened.read(0, &mut Ids::new()).expect("its page");
    assert!(read.unkept.contains(Lost::Protection));
    let markup = open(b"<svg/>".to_vec(), None, REQUEST.budget)
        .err()
        .expect("no PDF");
    assert_eq!(
        (markup.error, markup.file),
        (ReadError::Unrecognised, b"<svg/>".to_vec())
    );
}

#[test]
fn a_pdf_protected_by_certificate_is_unsupported_not_damaged() {
    let file = one_page(b"0 0 1 rg", Some("<</Filter /Adobe.PubSec /V 4 /R 4>>"));
    assert_eq!(
        read(file, &REQUEST, &mut Ids::new()).err(),
        Some(ReadError::Unsupported)
    );
}

#[test]
fn nothing_beneath_a_clip_the_drawing_cannot_hold_is_kept_unclipped() {
    // The clip is scaled past what a drawing's transform may be; the fill
    // beneath it is scaled back to the page's size.
    let file = one_page(
        b"q 5e6 0 0 1 0 0 cm 0 0 1 1 re W n 1e-7 0 0 1 0 0 cm 0 0 200 100 re f Q \
        0 0 10 10 re f",
        None,
    );
    let read = read(file, &REQUEST, &mut Ids::new()).expect("a drawing");
    assert!(read.unkept.contains(Lost::Damage), "{:?}", read.unkept);
    let kept = shapes(&read);
    assert_eq!(kept.len(), 1, "only the fill after the clip ends: {kept:?}");
    let bounds = kept[0].bounds();
    assert!(bounds.max.x < 20.0, "{bounds:?}");
}

#[test]
fn a_picture_past_what_a_surface_or_a_drawing_holds_is_lost_not_the_page() {
    let mut ids = Ids::new();
    let mut builder = Builder::new(&mut ids, Unkept::NONE).expect("room");
    // Its pixels are never read: its size alone refuses it.
    let huge = Arc::new(Picture {
        width: 8193,
        height: 8193,
        pixels: Vec::new(),
        jpeg: None,
    });
    builder
        .image(&huge, Affine::IDENTITY, 1.0)
        .expect("the page goes on");
    assert!(builder.unkept.contains(Lost::Images));
    let small = Arc::new(Picture {
        width: 2,
        height: 2,
        pixels: alloc::vec![255; 16],
        jpeg: None,
    });
    builder.unkept = Unkept::NONE;
    builder.pixels = MAX_TOTAL_IMAGE_PIXELS - 3;
    builder
        .image(&small, Affine::IDENTITY, 1.0)
        .expect("the page goes on");
    assert!(
        builder.unkept.contains(Lost::Images),
        "past what a drawing's pictures hold together"
    );
    builder.unkept = Unkept::NONE;
    builder.pixels = MAX_TOTAL_IMAGE_PIXELS - 4;
    builder
        .image(&small, Affine::IDENTITY, 1.0)
        .expect("a picture that fits");
    assert!(builder.unkept.is_empty(), "{:?}", builder.unkept);
    assert_eq!(builder.pixels, MAX_TOTAL_IMAGE_PIXELS);
    let (layer, _) = builder.finish().expect("the layer");
    let Kind::Group { children, .. } = layer.kind() else {
        panic!("a layer");
    };
    assert_eq!(children.len(), 1, "the one picture that fits");
}

#[test]
fn the_crate_reads_a_pdf_by_its_content() {
    let file = written_sample();
    let read = crate::read(file, &REQUEST, &mut Ids::new()).expect("a drawing");
    assert_eq!(read.drawing.page().unit, Unit::Pt);
}

#[test]
fn markup_whose_text_holds_the_header_is_not_a_pdf() {
    let svg = b"\xef\xbb\xbf  <svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"><text>%PDF-1.7</text></svg>";
    assert!(!recognised(svg));
    assert!(recognised(b"junk\n%PDF-1.7\n"));
    assert!(crate::read(svg.to_vec(), &REQUEST, &mut Ids::new()).is_ok());
}

#[test]
fn a_page_of_several_records_the_others_as_lost() {
    let file = b"%PDF-1.7\n1 0 obj <</Type /Catalog /Pages 2 0 R>> endobj\n\
        2 0 obj <</Type /Pages /Kids [3 0 R 4 0 R]>> endobj\n\
        3 0 obj <</Type /Page /MediaBox [0 0 10 20]>> endobj\n\
        4 0 obj <</Type /Page /MediaBox [0 0 30 40]>> endobj\n";
    let read = read(file.to_vec(), &REQUEST, &mut Ids::new()).expect("a drawing");
    assert!(read.unkept.contains(Lost::OtherPages), "{:?}", read.unkept);
}

#[test]
fn a_drawings_title_and_description_come_back_from_its_pdf() {
    let mut ids = Ids::new();
    let info = crate::drawing::Info {
        title: Some(SharedStr::try_from_str("Garden plan").expect("room")),
        description: Some(SharedStr::try_from_str("Beds, paths and the pond").expect("room")),
    };
    let drawing = sample(&mut ids).with_info(info.clone()).expect("an info");
    let file = pdf(&drawing, &OnlyA).expect("a PDF").bytes;
    let read = read(file, &REQUEST, &mut Ids::new()).expect("a drawing");
    assert_eq!(read.drawing.info(), &info);
    assert!(read.unkept.is_empty(), "{:?}", read.unkept);
}

#[test]
fn what_a_document_holds_beyond_its_page_is_recorded_as_lost() {
    let file = b"%PDF-1.7\n1 0 obj <</Type /Catalog /Pages 2 0 R /Outlines 5 0 R>> endobj\n\
        2 0 obj <</Type /Pages /Kids [3 0 R]>> endobj\n\
        3 0 obj <</Type /Page /MediaBox [0 0 10 20]>> endobj\n\
        4 0 obj <</Author (Ann) /Title (A title)>> endobj\n\
        5 0 obj <</Type /Outlines>> endobj\n\
        trailer <</Root 1 0 R /Info 4 0 R>>\n";
    let read = read(file.to_vec(), &REQUEST, &mut Ids::new()).expect("a drawing");
    assert!(
        read.unkept.contains(Lost::DocumentParts),
        "{:?}",
        read.unkept
    );
    assert!(read.unkept.contains(Lost::Annotations), "{:?}", read.unkept);
    assert_eq!(read.drawing.info().title.as_deref(), Some("A title"));
}
