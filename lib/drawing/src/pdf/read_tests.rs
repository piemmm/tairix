//! Reading PDFs into drawings: Draw's own files back, page choice, refusal.

#![allow(
    clippy::float_cmp,
    reason = "a dash offset written as a whole number is read back as that number"
)]

use alloc::sync::Arc;
use alloc::vec::Vec;

use tairix_collections::SharedStr;

use super::{pages, read, Request};
use crate::drawing::Unit;
use crate::id::Ids;
use crate::node::{Kind, Node};
use crate::pdf::{pdf, recognised};
use crate::style::Paint;
use crate::testing::OnlyA;
use crate::unkept::Lost;
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

#[test]
fn draws_own_pdf_reads_back_at_its_size() {
    let file = written_sample();
    assert!(recognised(&file));
    let read = read(&file, &REQUEST, &mut Ids::new()).expect("a drawing");
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
    let read = read(&file, &REQUEST, &mut Ids::new()).expect("a drawing");
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
    let read = read(&file, &REQUEST, &mut Ids::new()).expect("a drawing");
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
fn a_one_page_pdf_lists_one_page() {
    let file = written_sample();
    let (count, listed) = pages(&file, &REQUEST, 8).expect("its pages");
    assert_eq!((count, listed.len()), (1, 1));
    assert!(
        (listed[0].0 - 150.0).abs() < 1e-6 && (listed[0].1 - 75.0).abs() < 1e-6,
        "{listed:?}"
    );
}

#[test]
fn a_listing_counts_every_page_and_sizes_as_many_as_asked() {
    let file = b"%PDF-1.7\n1 0 obj <</Type /Catalog /Pages 2 0 R>> endobj\n\
        2 0 obj <</Type /Pages /Kids [3 0 R 4 0 R 5 0 R]>> endobj\n\
        3 0 obj <</Type /Page /MediaBox [0 0 10 20]>> endobj\n\
        4 0 obj <</Type /Page /MediaBox [0 0 30 40]>> endobj\n\
        5 0 obj <</Type /Page /MediaBox [0 0 50 60]>> endobj\n";
    assert_eq!(
        pages(file, &REQUEST, 2),
        Ok((3, alloc::vec![(10.0, 20.0), (30.0, 40.0)]))
    );
    assert_eq!(pages(file, &REQUEST, 0), Ok((3, Vec::new())));
}

#[test]
fn a_page_it_lacks_or_bytes_that_are_no_pdf_are_refused() {
    let file = written_sample();
    let past = Request { page: 1, ..REQUEST };
    assert_eq!(
        read(&file, &past, &mut Ids::new()).err(),
        Some(ReadError::NoSuchPage)
    );
    assert_eq!(
        read(b"<svg/>", &REQUEST, &mut Ids::new()).err(),
        Some(ReadError::Unrecognised)
    );
    assert_eq!(
        read(b"%PDF-1.4 garbage", &REQUEST, &mut Ids::new()).err(),
        Some(ReadError::Malformed)
    );
}

#[test]
fn the_crate_reads_a_pdf_by_its_content() {
    let file = written_sample();
    let read = crate::read(&file, &REQUEST, &mut Ids::new()).expect("a drawing");
    assert_eq!(read.drawing.page().unit, Unit::Pt);
}

#[test]
fn markup_whose_text_holds_the_header_is_not_a_pdf() {
    let svg = b"\xef\xbb\xbf  <svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"><text>%PDF-1.7</text></svg>";
    assert!(!recognised(svg));
    assert!(recognised(b"junk\n%PDF-1.7\n"));
    assert!(crate::read(svg, &REQUEST, &mut Ids::new()).is_ok());
}

#[test]
fn a_page_of_several_records_the_others_as_lost() {
    let file = b"%PDF-1.7\n1 0 obj <</Type /Catalog /Pages 2 0 R>> endobj\n\
        2 0 obj <</Type /Pages /Kids [3 0 R 4 0 R]>> endobj\n\
        3 0 obj <</Type /Page /MediaBox [0 0 10 20]>> endobj\n\
        4 0 obj <</Type /Page /MediaBox [0 0 30 40]>> endobj\n";
    let read = read(file, &REQUEST, &mut Ids::new()).expect("a drawing");
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
    let read = read(&file, &REQUEST, &mut Ids::new()).expect("a drawing");
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
    let read = read(file, &REQUEST, &mut Ids::new()).expect("a drawing");
    assert!(
        read.unkept.contains(Lost::DocumentParts),
        "{:?}",
        read.unkept
    );
    assert!(read.unkept.contains(Lost::Annotations), "{:?}", read.unkept);
    assert_eq!(read.drawing.info().title.as_deref(), Some("A title"));
}
