//! Images: samples in each space and depth, decode arrays, stencils, masks,
//! JPEG, and inline images.

#![allow(
    clippy::float_cmp,
    reason = "corners and opacities are compared exactly: each is the exact product of small whole numbers"
)]

use alloc::format;
use alloc::vec::Vec;

use tairix_image::{encode_jpeg, JpegOptions};

use crate::display::{DisplayList, Item, Lost};
use crate::document::Document;
use crate::fixture::{Builder, OPTIONS};

/// The display list of a one-page document painting `content` with
/// `resources`, and `objects` written as streams from 5 on.
fn page(content: &[u8], resources: &str, objects: &[(u32, &str, &[u8])]) -> DisplayList {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(
            3,
            &format!(
                "<</Type /Page /MediaBox [0 0 200 100] /Contents 4 0 R /Resources {resources}>>"
            ),
        )
        .stream(4, "", content);
    for &(num, entries, data) in objects {
        pdf.stream(num, entries, data);
    }
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    document
        .page(0)
        .expect("a page")
        .display_list()
        .expect("a display list")
}

/// A one-page document painting `content` with image `Im0` written as a
/// stream of `entries` over `data`, and `extra` objects from 6 on.
fn painted(content: &str, entries: &str, data: &[u8], extra: &[(u32, &str, &[u8])]) -> DisplayList {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 200 100] /Contents 4 0 R /Resources <</XObject <</Im0 5 0 R>>>>>>")
        .stream(4, "", content.as_bytes())
        .stream(5, &format!("/Type /XObject /Subtype /Image {entries}"), data);
    for &(num, entries, data) in extra {
        pdf.stream(num, entries, data);
    }
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    document
        .page(0)
        .expect("a page")
        .display_list()
        .expect("a display list")
}

/// The pixels of the one picture `list` paints.
fn pixels(list: &DisplayList) -> Vec<[u8; 4]> {
    let [Item::Image { picture, .. }] = list.items.as_slice() else {
        panic!("one picture: {:?} {:?}", list.items.len(), list.lost);
    };
    picture.pixels.as_chunks::<4>().0.to_vec()
}

#[test]
fn rgb_and_grey_samples_become_opaque_pixels() {
    let rgb = painted(
        "/Im0 Do",
        "/Width 2 /Height 1 /ColorSpace /DeviceRGB /BitsPerComponent 8",
        &[255, 0, 0, 0, 0, 255],
        &[],
    );
    assert_eq!(pixels(&rgb), [[255, 0, 0, 255], [0, 0, 255, 255]]);
    let grey = painted(
        "/Im0 Do",
        "/Width 3 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 2",
        &[0b0001_1011],
        &[],
    );
    assert_eq!(
        pixels(&grey),
        [[0, 0, 0, 255], [85, 85, 85, 255], [170, 170, 170, 255]]
    );
    let deep = painted(
        "/Im0 Do",
        "/Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 16",
        &[0x80, 0x00],
        &[],
    );
    assert_eq!(pixels(&deep), [[128, 128, 128, 255]]);
}

#[test]
fn rows_are_padded_to_whole_bytes() {
    let list = painted(
        "/Im0 Do",
        "/Width 3 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 1",
        &[0b1010_0000, 0b0100_0000],
        &[],
    );
    let white = [255, 255, 255, 255];
    let black = [0, 0, 0, 255];
    assert_eq!(pixels(&list), [white, black, white, black, white, black]);
}

#[test]
fn a_decode_array_inverts_samples() {
    let list = painted(
        "/Im0 Do",
        "/Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Decode [1 0]",
        &[0, 255],
        &[],
    );
    assert_eq!(pixels(&list), [[255, 255, 255, 255], [0, 0, 0, 255]]);
}

#[test]
fn indexed_and_cmyk_samples_go_through_their_spaces() {
    let indexed = painted("/Im0 Do", "/Width 2 /Height 1 /ColorSpace [/Indexed /DeviceRGB 1 <00ff00 ff00ff>] /BitsPerComponent 1", &[0b0100_0000], &[]);
    assert_eq!(pixels(&indexed), [[0, 255, 0, 255], [255, 0, 255, 255]]);
    let cmyk = painted(
        "/Im0 Do",
        "/Width 1 /Height 1 /ColorSpace /DeviceCMYK /BitsPerComponent 8",
        &[0, 255, 255, 0],
        &[],
    );
    assert_eq!(pixels(&cmyk), [[255, 0, 0, 255]]);
}

#[test]
fn a_stencil_paints_the_fill_colour_where_its_samples_are_zero() {
    let list = painted(
        "0 0 1 rg /Im0 Do",
        "/Width 2 /Height 1 /ImageMask true",
        &[0b0100_0000],
        &[],
    );
    assert_eq!(pixels(&list), [[0, 0, 255, 255], [0, 0, 255, 0]]);
    let inverted = painted(
        "/Im0 Do",
        "/Width 2 /Height 1 /ImageMask true /Decode [1 0]",
        &[0b0100_0000],
        &[],
    );
    assert_eq!(pixels(&inverted), [[0, 0, 0, 0], [0, 0, 0, 255]]);
}

#[test]
fn a_stencil_inked_with_nothing_paints_nothing() {
    let list = page(
        b"/CS0 cs 1 scn /Im0 Do",
        "<</ColorSpace <</CS0 [/Separation /None /DeviceGray <</FunctionType 2 /Domain [0 1] /N 1>>]>> /XObject <</Im0 5 0 R>>>>",
        &[(5, "/Type /XObject /Subtype /Image /Width 2 /Height 1 /ImageMask true", &[0b0100_0000])],
    );
    assert!(list.items.is_empty());
    assert!(!list.lost.contains(Lost::Images));
}

#[test]
fn a_soft_mask_is_the_alpha_and_a_matte_is_undone() {
    let list = painted(
        "/Im0 Do",
        "/Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /SMask 6 0 R",
        &[200, 100],
        &[(6, "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8", &[128])],
    );
    assert_eq!(pixels(&list), [[200, 200, 200, 128], [100, 100, 100, 128]]);
    let matted = painted(
        "/Im0 Do",
        "/Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /SMask 6 0 R",
        &[128],
        &[(6, "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Matte [0]", &[128])],
    );
    assert_eq!(pixels(&matted), [[255, 255, 255, 128]]);
}

#[test]
fn colour_key_and_stencil_masks_clear_pixels() {
    let keyed = painted(
        "/Im0 Do",
        "/Width 3 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Mask [10 20]",
        &[5, 15, 25],
        &[],
    );
    assert_eq!(
        pixels(&keyed)
            .iter()
            .map(|pixel| pixel[3])
            .collect::<Vec<_>>(),
        [255, 0, 255]
    );
    let stencilled = painted(
        "/Im0 Do",
        "/Width 2 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Mask 6 0 R",
        &[50, 60],
        &[(
            6,
            "/Type /XObject /Subtype /Image /Width 2 /Height 1 /ImageMask true",
            &[0b0100_0000],
        )],
    );
    assert_eq!(
        pixels(&stencilled)
            .iter()
            .map(|pixel| pixel[3])
            .collect::<Vec<_>>(),
        [255, 0]
    );
}

#[test]
fn an_image_fills_the_unit_square_its_top_row_at_the_top() {
    let list = painted(
        "q 100 0 0 50 10 20 cm /Im0 Do Q",
        "/Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
        &[0],
        &[],
    );
    let Some(Item::Image {
        transform, opacity, ..
    }) = list.items.first()
    else {
        panic!("a picture");
    };
    let top_left = transform.map(tairix_bezier::Point::new(0.0, 0.0));
    let bottom_right = transform.map(tairix_bezier::Point::new(1.0, 1.0));
    // Unit square (0, 1) in user space is (10, 70), y down from a 100 high page: (10, 30).
    assert_eq!((top_left.x, top_left.y), (10.0, 30.0));
    assert_eq!((bottom_right.x, bottom_right.y), (110.0, 80.0));
    assert_eq!(*opacity, 1.0);
}

/// A JPEG of `width` by `height` filled with `rgb`.
fn jpeg(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    let pixels: Vec<u8> = (0..width * height)
        .flat_map(|_| [rgb[0], rgb[1], rgb[2], 255])
        .collect();
    let picture = tairix_image::Picture::rgba(width, height, pixels).expect("a picture");
    encode_jpeg(&picture, JpegOptions::new(95, [255; 3]).expect("a quality")).expect("a JPEG")
}

#[test]
fn a_jpeg_is_decoded_and_kept_as_it_came() {
    let bytes = jpeg(16, 8, [200, 30, 40]);
    let list = painted(
        "/Im0 Do",
        "/Width 16 /Height 8 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode",
        &bytes,
        &[],
    );
    let [Item::Image { picture, .. }] = list.items.as_slice() else {
        panic!("one picture");
    };
    assert_eq!(picture.jpeg.as_deref(), Some(bytes.as_slice()));
    let [r, g, b, a] = [
        picture.pixels[0],
        picture.pixels[1],
        picture.pixels[2],
        picture.pixels[3],
    ];
    assert!(
        r.abs_diff(200) < 6 && g.abs_diff(30) < 6 && b.abs_diff(40) < 6 && a == 255,
        "{:?}",
        [r, g, b, a]
    );
}

#[test]
fn a_jpeg_whose_frame_disagrees_with_its_dictionary_is_lost() {
    let bytes = jpeg(16, 8, [0, 0, 0]);
    let list = painted(
        "/Im0 Do",
        "/Width 8 /Height 8 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode",
        &bytes,
        &[],
    );
    assert!(list.lost.contains(Lost::Images));
    assert!(list.items.is_empty());
}

#[test]
fn codecs_not_yet_read_are_lost() {
    for filter in ["JPXDecode", "JBIG2Decode", "CCITTFaxDecode"] {
        let entries = format!(
            "/Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /{filter}"
        );
        let list = painted("/Im0 Do", &entries, b"x", &[]);
        assert!(list.lost.contains(Lost::Images), "{filter}");
    }
}

#[test]
fn an_inline_image_is_read_and_the_content_after_it_goes_on() {
    let content = b"BI /W 2 /H 1 /CS /RGB /BPC 8 ID \xff\x00\x00EI\x00\xff EI 0 0 1 1 re f";
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 200 100] /Contents 4 0 R>>")
        .stream(4, "", content);
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let list = document
        .page(0)
        .expect("a page")
        .display_list()
        .expect("a display list");
    let [Item::Image { picture, .. }, Item::Fill { .. }] = list.items.as_slice() else {
        panic!("a picture and a fill: {:?}", list.items);
    };
    assert_eq!(picture.pixels, [255, 0, 0, 255, 0x45, 0x49, 0x00, 255]);
}

#[test]
fn a_filtered_inline_image_ends_at_an_ei_followed_by_content() {
    let content = b"BI /W 1 /H 1 /CS /G /BPC 8 /F /AHx ID 7f> EI 0 0 1 1 re f";
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 200 100] /Contents 4 0 R>>")
        .stream(4, "", content);
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let list = document
        .page(0)
        .expect("a page")
        .display_list()
        .expect("a display list");
    let [Item::Image { picture, .. }, Item::Fill { .. }] = list.items.as_slice() else {
        panic!("a picture and a fill: {:?}", list.items);
    };
    assert_eq!(picture.pixels, [127, 127, 127, 255]);
}

#[test]
fn an_image_too_large_for_the_budget_is_too_complex() {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 200 100] /Contents 4 0 R /Resources <</XObject <</Im0 5 0 R>>>>>>")
        .stream(4, "", b"/Im0 Do")
        .stream(5, "/Type /XObject /Subtype /Image /Width 60000 /Height 60000 /ImageMask true", b"");
    let file = pdf.tabled(1, "");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let page = document.page(0).expect("a page");
    assert_eq!(page.display_list(), Err(crate::PdfError::TooComplex));
}

#[test]
fn an_image_objects_colour_space_names_no_resource() {
    let list = page(
        b"/Im0 Do",
        "<</XObject <</Im0 5 0 R>> /ColorSpace <</CS0 /DeviceRGB>>>>",
        &[(
            5,
            "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /CS0 /BitsPerComponent 8",
            &[255, 0, 0],
        )],
    );
    assert!(list.items.is_empty(), "{:?}", list.items);
    assert!(list.lost.contains(Lost::Images));
}

#[test]
fn an_unfiltered_inline_image_is_as_long_as_its_named_spaces_samples() {
    let list = page(
        b"BI /W 2 /H 1 /CS /CS0 /BPC 8 ID \xff\x00\x00EI\x00\xff EI 0 0 1 1 re f",
        "<</ColorSpace <</CS0 /DeviceRGB>>>>",
        &[],
    );
    let [Item::Image { picture, .. }, Item::Fill { .. }] = list.items.as_slice() else {
        panic!("a picture and a fill: {:?} {:?}", list.items, list.lost);
    };
    assert_eq!(picture.pixels, [255, 0, 0, 255, 0x45, 0x49, 0x00, 255]);
    assert!(list.lost.is_empty(), "{:?}", list.lost);
}

#[test]
fn an_inline_image_of_no_readable_size_ends_at_its_ei() {
    let list = page(
        b"BI /W 0 /H 1 /CS /G /BPC 8 ID 5 5 1 1 re f EI 0 0 1 1 re f",
        "<<>>",
        &[],
    );
    let [Item::Fill { .. }] = list.items.as_slice() else {
        panic!("one fill: {:?}", list.items);
    };
    assert!(list.lost.contains(Lost::Images));
}

/// The display list of `file`'s page, opened with a budget of `budget`.
fn within(file: &[u8], budget: u64) -> Result<DisplayList, crate::PdfError> {
    let options = crate::Options {
        password: None,
        budget,
    };
    let document = Document::open(crate::fixture::kept(file), &options)?;
    document.page(0)?.display_list()
}

/// A one-page document painting `content` with image `Im0`, as `painted`
/// writes it, the file answered rather than read.
fn image_file(content: &str, entries: &str, data: &[u8], extra: &[(u32, &str, &[u8])]) -> Vec<u8> {
    let mut pdf = Builder::new();
    pdf.object(1, "<</Type /Catalog /Pages 2 0 R>>")
        .object(2, "<</Type /Pages /Kids [3 0 R]>>")
        .object(3, "<</Type /Page /MediaBox [0 0 200 100] /Contents 4 0 R /Resources <</XObject <</Im0 5 0 R>>>>>>")
        .stream(4, "", content.as_bytes())
        .stream(5, &format!("/Type /XObject /Subtype /Image {entries}"), data);
    for &(num, entries, data) in extra {
        pdf.stream(num, entries, data);
    }
    pdf.tabled(1, "")
}

#[test]
fn an_explicit_stencil_mask_is_charged_to_the_budget() {
    let file = image_file(
        "/Im0 Do",
        "/Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Mask 6 0 R",
        &[128],
        &[(6, "/Type /XObject /Subtype /Image /ImageMask true /Width 65536 /Height 65536 /BitsPerComponent 1", b"")],
    );
    assert_eq!(
        within(&file, 1 << 20).err(),
        Some(crate::PdfError::TooComplex)
    );
}

#[test]
fn a_codec_not_read_is_refused_before_its_pixels_are_spent() {
    let file = image_file(
        "/Im0 Do",
        "/Width 4000 /Height 4000 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /JPXDecode",
        b"x",
        &[],
    );
    let list = within(&file, 1 << 20).expect("a display list");
    assert!(list.items.is_empty());
    assert!(list.lost.contains(Lost::Images));
}

#[test]
fn an_image_that_cannot_be_read_is_read_once_however_often_painted() {
    let file = image_file(
        "/Im0 Do /Im0 Do /Im0 Do",
        "/Width 0 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /FlateDecode",
        &crate::fixture::flate(&[0u8; 1000]),
        &[],
    );
    let list = within(&file, 2500).expect("a display list");
    assert!(list.lost.contains(Lost::Images));
}

#[test]
fn a_stencil_painted_again_in_its_colour_is_one_picture() {
    let list = painted(
        "1 0 0 rg /Im0 Do /Im0 Do 0 0 1 rg /Im0 Do",
        "/Width 1 /Height 1 /ImageMask true /BitsPerComponent 1",
        &[0],
        &[],
    );
    let [Item::Image { picture: a, .. }, Item::Image { picture: b, .. }, Item::Image { picture: c, .. }] =
        list.items.as_slice()
    else {
        panic!("three pictures: {:?}", list.items.len());
    };
    assert!(alloc::sync::Arc::ptr_eq(a, b));
    assert!(!alloc::sync::Arc::ptr_eq(a, c));
    assert_eq!(&c.pixels[..4], &[0, 0, 255, 255]);
}

#[test]
fn an_inline_image_in_a_form_painted_again_is_one_picture() {
    let list = page(
        b"/F0 Do /F0 Do",
        "<</XObject <</F0 5 0 R>>>>",
        &[(
            5,
            "/Type /XObject /Subtype /Form",
            b"BI /W 1 /H 1 /CS /G /BPC 8 ID \xff EI",
        )],
    );
    let [Item::Image { picture: a, .. }, Item::Image { picture: b, .. }] = list.items.as_slice()
    else {
        panic!("two pictures: {:?}", list.items.len());
    };
    assert!(alloc::sync::Arc::ptr_eq(a, b));
}

#[test]
fn samples_few_bits_wide_go_through_a_table_in_their_order() {
    let mut data = alloc::vec![0u8; 64 * 64 * 3 / 2];
    data[..5].copy_from_slice(&[0xf0, 0x00, 0xf0, 0x00, 0xf0]);
    let list = painted(
        "/Im0 Do",
        "/Width 64 /Height 64 /ColorSpace /DeviceRGB /BitsPerComponent 4",
        &data,
        &[],
    );
    assert_eq!(
        &pixels(&list)[..4],
        &[
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            [0, 0, 255, 255],
            [0, 0, 0, 255]
        ]
    );
}

#[test]
fn samples_a_byte_or_two_wide_go_through_a_table_by_their_bytes() {
    let mut data = alloc::vec![0u8; 16 * 16];
    data[..4].copy_from_slice(&[0, 64, 128, 255]);
    let list = painted(
        "/Im0 Do",
        "/Width 16 /Height 16 /ColorSpace /DeviceGray /BitsPerComponent 8 /Decode [1 0]",
        &data,
        &[],
    );
    assert_eq!(
        &pixels(&list)[..4],
        &[
            [255, 255, 255, 255],
            [191, 191, 191, 255],
            [127, 127, 127, 255],
            [0, 0, 0, 255]
        ]
    );
    let mut wide = alloc::vec![0u8; 256 * 256 * 2];
    wide[..6].copy_from_slice(&[0xff, 0xff, 0x80, 0x00, 0x00, 0x00]);
    let list = painted(
        "/Im0 Do",
        "/Width 256 /Height 256 /ColorSpace /DeviceGray /BitsPerComponent 16",
        &wide,
        &[],
    );
    assert_eq!(
        &pixels(&list)[..3],
        &[[255, 255, 255, 255], [128, 128, 128, 255], [0, 0, 0, 255]]
    );
}

#[test]
fn a_negative_width_is_damage_not_too_complex() {
    let list = page(
        b"BI /W -1 /H 1 /BPC 8 /CS /G ID x EI 0 0 1 1 re f",
        "<<>>",
        &[],
    );
    let [Item::Fill { .. }] = list.items.as_slice() else {
        panic!("the fill after the image: {:?}", list.items);
    };
    assert!(list.lost.contains(Lost::Images));
}

#[test]
fn a_jpegs_decoded_pixels_are_charged_with_the_pictures() {
    let bytes = jpeg(8, 8, [10, 20, 30]);
    let content = "/Im0 Do";
    let file = image_file(
        content,
        "/Width 8 /Height 8 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode",
        &bytes,
        &[],
    );
    let pixels = 8 * 8 * 4;
    let needed = u64::try_from(content.len() + bytes.len() + 2 * pixels).expect("small");
    assert!(within(&file, needed).is_ok());
    assert_eq!(
        within(&file, needed - 1).err(),
        Some(crate::PdfError::TooComplex)
    );
}

#[test]
fn a_jpegs_decode_array_reads_the_samples_it_decodes_to() {
    let grey = jpeg(8, 8, [200, 200, 200]);
    let list = painted(
        "/Im0 Do",
        "/Width 8 /Height 8 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /DCTDecode /Decode [1 0]",
        &grey,
        &[],
    );
    let [Item::Image { picture, .. }] = list.items.as_slice() else {
        panic!("one picture: {:?}", list.lost);
    };
    assert!(
        picture.jpeg.is_none(),
        "its pixels are no longer the JPEG's own"
    );
    assert!(
        picture.pixels[0].abs_diff(55) < 6,
        "{:?}",
        &picture.pixels[..4]
    );
    let masked = painted(
        "/Im0 Do",
        "/Width 8 /Height 8 /ColorSpace /DeviceGray /BitsPerComponent 8 /SMask 6 0 R",
        &[0; 64],
        &[(
            6,
            "/Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /DCTDecode /Decode [1 0]",
            grey.as_slice(),
        )],
    );
    let alpha = pixels(&masked)[0][3];
    assert!(alpha.abs_diff(55) < 6, "{alpha}");
}

#[test]
fn a_jpegs_colour_key_clears_the_samples_it_decodes_to() {
    let bytes = jpeg(16, 8, [200, 30, 40]);
    let list = painted(
        "/Im0 Do",
        "/Width 16 /Height 8 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode /Mask [190 210 20 40 30 50]",
        &bytes,
        &[],
    );
    let alphas: Vec<u8> = pixels(&list).iter().map(|pixel| pixel[3]).collect();
    assert!(alphas.iter().all(|&alpha| alpha == 0), "{alphas:?}");
}

#[test]
fn a_jpeg_that_cannot_fill_its_picture_is_refused_not_a_panic() {
    let file = crate::fixture::one_page(b"", "<<>>");
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let bytes = jpeg(16, 8, [10, 20, 30]);
    let mut short = alloc::vec![0u8; 16 * 8 * 4 - 4];
    assert_eq!(
        super::dct(
            &document,
            &bytes,
            (16, 8),
            &crate::colour::Space::Rgb,
            &mut short
        ),
        Err(crate::PdfError::Unsupported)
    );
}

#[test]
fn a_tiny_image_runs_its_tint_transform_no_more_often_than_it_has_pixels() {
    // A table of every colour eight bits spell would run it 256 times.
    let file = image_file(
        "/Im0 Do",
        "/Width 2 /Height 1 /ColorSpace [/Separation /A /DeviceGray 6 0 R] /BitsPerComponent 8",
        &[0, 255],
        &[(
            6,
            "/FunctionType 4 /Domain [0 1] /Range [0 1]",
            b"{ 1 exch sub }",
        )],
    );
    let document = Document::open(crate::fixture::kept(&file), &OPTIONS).expect("a document");
    let list = document
        .page(0)
        .expect("a page")
        .display_list()
        .expect("a display list");
    assert_eq!(pixels(&list), [[255, 255, 255, 255], [0, 0, 0, 255]]);
    let steps = crate::limits::MAX_EVALUATION_STEPS - document.work().left();
    assert!(steps < 64, "{steps} steps for two pixels");
}

#[test]
fn a_tint_transform_runs_once_for_each_colour_an_image_holds() {
    // Each run is charged its program's whole length, which it all but skips:
    // run a pixel at a time, two colours alternating would spend the
    // document's steps.
    let program = format!("{{ pop pop false {{ {} }} if }}", "dup pop ".repeat(32_500));
    let data: Vec<u8> = (0..130 * 130)
        .flat_map(|at| if at % 2 == 0 { [0; 3] } else { [255; 3] })
        .collect();
    let list = painted(
        "/Im0 Do",
        "/Width 130 /Height 130 /ColorSpace [/DeviceN [/A /B /C] /DeviceGray 6 0 R] /BitsPerComponent 8",
        &data,
        &[(
            6,
            "/FunctionType 4 /Domain [0 1 0 1 0 1] /Range [0 1]",
            program.as_bytes(),
        )],
    );
    assert_eq!(&pixels(&list)[..2], &[[0, 0, 0, 255], [255, 255, 255, 255]]);
}

#[test]
fn a_stencils_samples_are_read_once_in_every_colour_it_is_painted_in() {
    let content = "1 0 0 rg /Im0 Do 0 0 1 rg /Im0 Do";
    let file = image_file(
        content,
        "/Width 8 /Height 8 /ImageMask true",
        &[0x0f; 8],
        &[],
    );
    // The page's content and the stencil's bytes, its samples once, and a
    // picture in each of its two colours.
    let needed = u64::try_from(content.len() + 8 + 8 * 8 + 2 * 8 * 8 * 4).expect("small");
    assert!(within(&file, needed).is_ok());
    assert_eq!(
        within(&file, needed - 1).err(),
        Some(crate::PdfError::TooComplex)
    );
}

#[test]
fn an_inverted_stencil_short_of_its_samples_paints_nowhere_past_them() {
    let list = painted(
        "/Im0 Do",
        "/Width 2 /Height 2 /ImageMask true /Decode [1 0]",
        &[0b1000_0000],
        &[],
    );
    let alphas: Vec<u8> = pixels(&list).iter().map(|pixel| pixel[3]).collect();
    assert_eq!(alphas, [255, 0, 0, 0]);
}
