//! Unit tests for the PDF writer.

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use super::{pdf, TextString};
use crate::id::Ids;
use crate::testing::OnlyA;
use crate::vector::tests::sample;

/// The file's objects, checked against its cross-reference table: each
/// number's body, a stream's content inflated.
fn objects(file: &[u8]) -> Vec<(usize, String, Option<Vec<u8>>)> {
    assert!(file.starts_with(b"%PDF-1.4\n"));
    let marker = b"startxref\n";
    let start = file
        .windows(marker.len())
        .rposition(|window| window == marker)
        .expect("startxref")
        + marker.len();
    let ascii = |bytes: &[u8]| String::from_utf8(bytes.to_vec()).expect("ascii");
    let table: usize = ascii(&file[start..])
        .lines()
        .next()
        .expect("an offset")
        .parse()
        .expect("a number");
    let tail = ascii(&file[table..]);
    assert!(tail.starts_with("xref\n0 "));
    let mut lines = tail.lines().skip(1);
    let count: usize = lines
        .next()
        .expect("a count")
        .split(' ')
        .nth(1)
        .expect("count")
        .parse()
        .expect("n");
    lines.next();
    let mut found = Vec::new();
    for number in 1..count {
        let entry = lines.next().expect("an entry");
        let offset: usize = entry[..10].parse().expect("an offset");
        let head = alloc::format!("{number} 0 obj\n");
        assert!(
            file[offset..].starts_with(head.as_bytes()),
            "object {number} where its entry says"
        );
        let body_start = offset + head.len();
        let end = body_start + find(&file[body_start..], b"\nendobj\n").expect("endobj");
        let body = &file[body_start..end];
        let stream = find(body, b"\nstream\n").map(|at| {
            let dictionary = String::from_utf8_lossy(&body[..at]).into_owned();
            let length: usize = dictionary
                .split("/Length ")
                .nth(1)
                .and_then(|rest| rest.split(' ').next())
                .expect("a length")
                .parse()
                .expect("a number");
            let data = &body[at + 8..];
            assert_eq!(
                &data[length..],
                b"\nendstream",
                "the length is the stream's"
            );
            if !dictionary.contains("/FlateDecode") {
                return data[..length].to_vec();
            }
            let mut inflated = vec![0u8; 1 << 20];
            let size = tairix_compress::zlib::decompress_into(&data[..length], &mut inflated)
                .expect("inflates");
            inflated.truncate(size);
            inflated
        });
        let head_text =
            String::from_utf8_lossy(&body[..find(body, b"\nstream\n").unwrap_or(body.len())])
                .into_owned();
        found.push((number, head_text, stream));
    }
    found
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[test]
fn a_drawing_is_one_page_of_sound_pdf() {
    let mut ids = Ids::new();
    let written = pdf(&sample(&mut ids), &OnlyA).expect("written");
    assert!(!written.complete, "the b has no glyph");
    let objects = objects(&written.bytes);
    let page = objects
        .iter()
        .find(|(number, ..)| *number == super::PAGE)
        .expect("the page");
    assert!(
        page.1.contains("/MediaBox [0 0 150 75]"),
        "points, not pixels: {}",
        page.1
    );
    let info = objects
        .iter()
        .find(|(number, ..)| *number == super::INFO)
        .expect("info");
    assert!(info.1.contains(&TextString("Rivers & roads").to_string()));
    let content = objects
        .iter()
        .find(|(number, ..)| *number == super::CONTENT)
        .and_then(|(_, _, stream)| stream.clone())
        .expect("content");
    let content = String::from_utf8(content).expect("text");
    for operator in [
        "0.75 0 0 -0.75 0 75 cm",
        "re f",
        "W n",
        " d\n",
        "RG 3 w",
        " Do Q",
        " sh",
    ] {
        assert!(content.contains(operator), "{operator:?} in:\n{content}");
    }
    let forms: Vec<_> = objects
        .iter()
        .filter(|(_, head, _)| head.contains("/Subtype /Form"))
        .collect();
    assert!(
        forms
            .iter()
            .any(|(_, head, _)| head.contains("/CS /DeviceGray")),
        "a soft mask for the stops' alpha"
    );
    assert!(
        forms
            .iter()
            .any(|(_, head, _)| head.contains("/CS /DeviceRGB")),
        "a group for the opacity"
    );
    let resources = objects
        .iter()
        .find(|(number, ..)| *number == super::RESOURCES)
        .expect("resources");
    assert!(resources.1.contains("/ShadingType 3"), "a radial shading");
    assert!(
        resources.1.contains("/Encode [ 0 1 1 0"),
        "reflected periods: {}",
        resources.1
    );
    assert!(resources.1.contains("/SMask"));
}

#[test]
fn a_text_string_is_hexadecimal_nothing_can_break_out_of() {
    assert_eq!(
        TextString("é)").to_string(),
        "<FEFF00E90029>",
        "nothing escapes a hex string"
    );
}

#[test]
fn pictures_are_image_objects_each_written_once() {
    let mut ids = Ids::new();
    let (drawing, baseline) = crate::vector::tests::pictured(&mut ids);
    let written = pdf(&drawing, &OnlyA).expect("written");
    let objects = objects(&written.bytes);
    let images: Vec<_> = objects
        .iter()
        .filter(|(_, head, _)| head.contains("/Subtype /Image"))
        .collect();
    let colour: Vec<_> = images
        .iter()
        .filter(|(_, head, _)| !head.contains("/Width 2 /Height 2 /ColorSpace /DeviceGray"))
        .collect();
    assert_eq!(
        images.len(),
        5,
        "grey, the clear one and its mask, two photos"
    );
    assert_eq!(colour.len(), 4);
    let grey = images
        .iter()
        .find(|(_, head, _)| head.contains("/Width 3 /Height 2"))
        .expect("the grey picture");
    assert!(grey.1.contains("/ColorSpace /DeviceGray") && !grey.1.contains("/SMask"));
    assert_eq!(
        grey.2.as_deref(),
        Some(&[0, 40, 80, 40, 80, 120][..]),
        "its levels"
    );
    let clear = images
        .iter()
        .find(|(_, head, _)| head.contains("/Width 2 /Height 2 /ColorSpace /DeviceRGB"))
        .expect("the clear picture");
    assert!(clear.1.contains("/SMask "), "{}", clear.1);
    assert_eq!(
        clear.2.as_deref(),
        Some(&[255, 0, 0, 255, 0, 0, 255, 0, 0, 255, 0, 0][..]),
        "its clear pixels take its red"
    );
    let mask = images
        .iter()
        .find(|(_, head, _)| head.contains("/Width 2 /Height 2 /ColorSpace /DeviceGray"))
        .expect("its mask");
    assert_eq!(mask.2.as_deref(), Some(&[255, 0, 255, 0][..]));
    let photos: Vec<_> = images
        .iter()
        .filter(|(_, head, _)| head.contains("/Filter /DCTDecode"))
        .collect();
    assert_eq!(
        photos.len(),
        2,
        "a JPEG passes on as it came, progressive or not"
    );
    assert_eq!(photos[0].2.as_deref(), Some(&baseline[..]));
    let content = objects
        .iter()
        .find(|(number, ..)| *number == super::CONTENT)
        .and_then(|(_, _, stream)| stream.clone())
        .expect("content");
    let content = String::from_utf8(content).expect("text");
    assert_eq!(
        content.matches("/I1 Do").count(),
        2,
        "the picture shown twice:\n{content}"
    );
    for named in ["/I0 Do", "/I2 Do", "/I3 Do"] {
        assert!(content.contains(named), "{named}");
    }
}
