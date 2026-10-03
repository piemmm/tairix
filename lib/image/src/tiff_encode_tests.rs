//! The TIFF encoder: every kind of page under every compression read back
//! through the crate's own decoder and natively, documents of several pages,
//! strips past one, the coder restarting on each, and the density stated.

use alloc::vec;
use alloc::vec::Vec;

use crate::encode_fixture::{rgba, shown, Noise};
use crate::{
    decode_as, encode_tiff, open_native, DecodeLimits, Density, DensityUnit, EncodeError,
    ImageFormat, IndexDepth, NativeDocument, Picture, PictureSource, Pixels, Rgba8,
    TiffCompression, TiffOptions, Unkept, Written,
};

fn limits() -> DecodeLimits {
    DecodeLimits::new(4096, 4096, 4096 * 4096, 0)
}

fn options(compression: TiffCompression) -> TiffOptions {
    TiffOptions { compression }
}

/// The pages `tiff` opens as, and what it held beside them and how it was
/// written.
fn pages(tiff: &[u8]) -> (Vec<Picture>, Unkept, Written) {
    let NativeDocument::Pages {
        mut pages,
        unkept,
        written,
    } = open_native(ImageFormat::Tiff, tiff, &limits()).expect("the file opens")
    else {
        panic!("a TIFF opens as its pages");
    };
    let read = (0..pages.count())
        .map(|index| {
            pages
                .page(index)
                .expect("the page decodes")
                .expect("a page")
        })
        .collect();
    (read, unkept, written)
}

/// Encode `picture` alone, check the file shows it and opens as exactly it,
/// and answer the file.
fn round_trip(picture: &Picture, compression: TiffCompression) -> Vec<u8> {
    let tiff = encode_tiff(&[picture], options(compression)).expect("the picture encodes");
    let decoded = decode_as(ImageFormat::Tiff, &tiff, &limits()).expect("the file decodes");
    assert_eq!(
        shown(decoded.pixels()),
        shown(&picture.to_rgba().expect("memory")),
        "{compression:?}: the file shows a different picture"
    );
    let (read, unkept, written) = pages(&tiff);
    assert_eq!(unkept, Unkept::default(), "{compression:?}: its own output");
    assert_eq!(written, Written::Tiff(options(compression)));
    assert_eq!(read.len(), 1);
    assert_same(&read[0], picture, compression);
    tiff
}

/// Whether `read` holds what `written` did: a palette picture's indices and
/// palette, or colour as it looks.
fn assert_same(read: &Picture, written: &Picture, compression: TiffCompression) {
    match (read.pixels(), written.pixels()) {
        (
            Pixels::Indexed {
                indices, palette, ..
            },
            Pixels::Indexed {
                indices: own,
                palette: held,
                mask: None,
                ..
            },
        ) if held.iter().all(|entry| entry[3] == u8::MAX) => {
            assert_eq!(indices, own, "{compression:?}");
            assert_eq!(&palette[..held.len()], held.as_slice(), "{compression:?}");
        }
        _ => assert_eq!(
            shown(&read.to_rgba().expect("memory")),
            shown(&written.to_rgba().expect("memory")),
            "{compression:?}"
        ),
    }
}

fn colours(count: usize) -> Vec<Rgba8> {
    (0..count)
        .map(|at| {
            let at = u8::try_from(at).expect("at most 256");
            [at, 255 - at, at / 2, 255]
        })
        .collect()
}

fn indexed(width: u32, height: u32, depth: IndexDepth) -> Picture {
    let count = depth.colours();
    let indices = (0..width * height)
        .map(|at| u8::try_from((at as usize * 7) % count).expect("an index"))
        .collect();
    Picture::indexed(width, height, depth, colours(count), indices, None).expect("valid")
}

/// The `SamplesPerPixel` and `Photometric` the file's one directory states.
fn layout(tiff: &[u8]) -> (u32, u32) {
    let at = u32::from_le_bytes([tiff[4], tiff[5], tiff[6], tiff[7]]) as usize;
    let count = usize::from(u16::from_le_bytes([tiff[at], tiff[at + 1]]));
    let mut samples = 1;
    let mut photometric = u32::MAX;
    for entry in 0..count {
        let field = at + 2 + entry * 12;
        let tag = u16::from_le_bytes([tiff[field], tiff[field + 1]]);
        let value = u32::from(u16::from_le_bytes([tiff[field + 8], tiff[field + 9]]));
        match tag {
            262 => photometric = value,
            277 => samples = value,
            _ => {}
        }
    }
    (samples, photometric)
}

#[test]
fn every_kind_of_page_reads_back_under_every_compression() {
    let mut noise = Noise(0x0123_4567_89AB_CDEF);
    let mut kinds: Vec<(Picture, (u32, u32))> = IndexDepth::ALL
        .into_iter()
        .map(|depth| (indexed(11, 5, depth), (1, 3)))
        .collect();
    kinds.push((
        rgba(9, 4, |x, _| {
            let level = u8::try_from(x * 20).expect("small");
            [level, level, level, 255]
        }),
        (1, 1),
    ));
    kinds.push((
        rgba(9, 4, |x, y| {
            [7, 7, 7, u8::try_from(x * 10 + y).expect("small")]
        }),
        (2, 1),
    ));
    kinds.push((
        rgba(9, 4, |_, _| [noise.next(), noise.next(), noise.next(), 255]),
        (3, 2),
    ));
    kinds.push((
        rgba(9, 4, |_, _| {
            [noise.next(), noise.next(), noise.next(), noise.next()]
        }),
        (4, 2),
    ));
    for compression in TiffCompression::ALL {
        for (picture, expected) in &kinds {
            let tiff = round_trip(picture, compression);
            assert_eq!(layout(&tiff), *expected, "{compression:?}");
        }
    }
}

#[test]
fn a_translucent_palette_picture_is_written_as_colour() {
    let mut palette = colours(4);
    palette[3][3] = 64;
    let picture =
        Picture::indexed(4, 1, IndexDepth::Two, palette, vec![0, 1, 2, 3], None).expect("valid");
    let tiff = round_trip(&picture, TiffCompression::Lzw);
    assert_eq!(layout(&tiff).0, 4);
}

#[test]
fn strips_past_the_first_restart_their_coder_and_read_back() {
    let mut noise = Noise(0xDEAD_BEEF_F00D_CAFE);
    // Each strip codes well past nine-bit codes, so a strip whose clear were
    // written at its predecessor's width would not read back.
    let photo = rgba(300, 300, |x, y| {
        let level = u8::try_from((x + y) % 256).expect("a byte");
        [level, noise.next() / 8, level, 255]
    });
    for compression in TiffCompression::ALL {
        round_trip(&photo, compression);
    }
}

#[test]
fn pack_bits_runs_and_literals_meet_at_every_boundary() {
    let row = |x: u32| -> Rgba8 {
        let level = match x {
            0..=129 => 5,
            130..=135 => u8::try_from(x).expect("small"),
            136..=138 => 9,
            _ => u8::try_from(x % 3).expect("small"),
        };
        [level, level, level, 255]
    };
    round_trip(&rgba(400, 2, |x, _| row(x)), TiffCompression::PackBits);
}

#[test]
fn a_document_of_several_pages_reads_back_page_for_page() {
    let first = indexed(5, 3, IndexDepth::Four);
    let second = rgba(4, 6, |x, y| {
        [
            u8::try_from(x * 9).expect("small"),
            u8::try_from(y).expect("small"),
            1,
            255,
        ]
    });
    let third = indexed(2, 2, IndexDepth::One);
    let sources: [&dyn PictureSource; 3] = [&first, &second, &third];
    let tiff = encode_tiff(&sources, options(TiffCompression::Deflate)).expect("encodes");
    let (read, unkept, _) = pages(&tiff);
    assert_eq!(
        unkept,
        Unkept::default(),
        "page numbering is not held beside the pages"
    );
    assert_eq!(read.len(), 3);
    for (read, written) in read.iter().zip([&first, &second, &third]) {
        assert_same(read, written, TiffCompression::Deflate);
    }
}

#[test]
fn no_pages_is_not_a_tiff() {
    assert_eq!(
        encode_tiff(&[], TiffOptions::default()),
        Err(EncodeError::NoPages)
    );
}

#[test]
fn a_tiff_states_its_density_exactly() {
    let read = |density| {
        let picture = rgba(2, 2, |_, _| [9, 8, 7, 255]).with_density(density);
        pages(&round_trip(&picture, TiffCompression::None)).0[0].density()
    };
    let inch = Density::new((600, 1), (1200, 2), DensityUnit::Inch);
    assert_eq!(read(inch), inch);
    let centimetre = Density::whole(118, 236, DensityUnit::Centimetre);
    assert_eq!(read(centimetre), centimetre);
    let metre = Density::whole(2835, 3780, DensityUnit::Metre).expect("valid");
    assert_eq!(
        read(Some(metre)),
        metre.exact_in(DensityUnit::Centimetre),
        "restated exactly per centimetre"
    );
    let shape = Density::whole(1, 3, DensityUnit::Aspect);
    assert_eq!(read(shape), shape);
    assert_eq!(read(None), None);
}
