//! The JPEG encoder: the Annex K tables it ships, the quality scaling, the
//! frame it chooses, and files read back through the crate's own decoder.

use alloc::vec;
use alloc::vec::Vec;

use super::{
    scaled_table, AC_CHROMA, AC_LUMA, CHROMA_QUANT, CODES, DC_CHROMA, DC_LUMA, LUMA_QUANT,
};
use crate::huffman::{Canonical, MAX_CODE_BITS};
use crate::{
    decode_as, encode_jpeg, open_native, DecodeLimits, EncodeError, ImageFormat, IndexDepth,
    JpegOptions, NativeDocument, Picture, PictureKind, PictureSource, Rgba8, Unkept,
};

fn limits() -> DecodeLimits {
    DecodeLimits::new(4096, 4096, 4096 * 4096, 64 << 20)
}

fn options(quality: u8) -> JpegOptions {
    JpegOptions::new(quality, [255, 255, 255]).expect("a valid quality")
}

fn rgba(width: u32, height: u32, mut pixel: impl FnMut(u32, u32) -> Rgba8) -> Picture {
    let mut bytes = Vec::new();
    for y in 0..height {
        for x in 0..width {
            bytes.extend_from_slice(&pixel(x, y));
        }
    }
    Picture::rgba(width, height, bytes).expect("valid")
}

fn decoded(jpeg: &[u8]) -> Vec<u8> {
    decode_as(ImageFormat::Jpeg, jpeg, &limits())
        .expect("the file decodes")
        .into_pixels()
}

/// Mean squared error over the colour channels. Against 8-bit samples an
/// error of 10 is about 38 dB of signal to noise, 26 about 34 dB, 65 about
/// 30 dB and 260 about 24 dB.
fn mse(a: &[u8], b: &[u8]) -> u64 {
    let (mut sum, mut count) = (0u64, 0u64);
    for (pa, pb) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        for channel in 0..3 {
            let d = u64::from(pa[channel].abs_diff(pb[channel]));
            sum += d * d;
            count += 1;
        }
    }
    sum / count.max(1)
}

/// The frame header's component count and first component's sampling byte.
fn frame(jpeg: &[u8]) -> (u8, u8) {
    let at = jpeg
        .windows(2)
        .position(|window| window == [0xFF, 0xC0])
        .expect("a baseline frame header");
    (jpeg[at + 9], jpeg[at + 11])
}

#[test]
fn every_table_codes_exactly_the_symbols_a_baseline_scan_needs() {
    let dc: Vec<u8> = (0..=11).collect();
    let mut ac = vec![0x00, 0xF0];
    for run in 0..16u8 {
        for size in 1..=10u8 {
            ac.push(run << 4 | size);
        }
    }
    ac.sort_unstable();
    for (spec, expected) in [
        (&DC_LUMA, &dc),
        (&DC_CHROMA, &dc),
        (&AC_LUMA, &ac),
        (&AC_CHROMA, &ac),
    ] {
        let total: usize = spec.counts.iter().map(|&count| usize::from(count)).sum();
        assert_eq!(total, spec.symbols.len());
        let mut symbols = spec.symbols.to_vec();
        symbols.sort_unstable();
        assert_eq!(&symbols, expected);
    }
}

#[test]
fn every_assigned_code_decodes_back_to_its_symbol() {
    let specs = [&DC_LUMA, &AC_LUMA, &DC_CHROMA, &AC_CHROMA];
    let codes = [CODES[0][0], CODES[0][1], CODES[1][0], CODES[1][1]];
    for (spec, assigned) in specs.into_iter().zip(codes) {
        let counts: [u32; MAX_CODE_BITS] = spec.counts.map(u32::from);
        let table = Canonical::build(&counts).expect("a valid code");
        for (order, &symbol) in spec.symbols.iter().enumerate() {
            let (code, len) = (
                assigned.code[usize::from(symbol)],
                assigned.len[usize::from(symbol)],
            );
            let mut walk = Canonical::walk();
            let mut found = None;
            for bit in (0..len).rev() {
                found = walk.push(&table, u32::from(code >> bit & 1));
            }
            assert_eq!(found, Some(u32::try_from(order).expect("small")));
        }
    }
}

#[test]
fn quality_fifty_is_the_annex_tables_unscaled() {
    assert_eq!(scaled_table(&LUMA_QUANT, 50), LUMA_QUANT);
    assert_eq!(scaled_table(&CHROMA_QUANT, 50), CHROMA_QUANT);
}

#[test]
fn quality_is_clamped_to_what_a_baseline_table_holds() {
    assert!(scaled_table(&LUMA_QUANT, 100).iter().all(|&q| q == 1));
    assert!(scaled_table(&CHROMA_QUANT, 1).iter().all(|&q| q == 255));
    // The reference encoder's scaling: quality 75 halves the tables.
    assert_eq!(scaled_table(&LUMA_QUANT, 75)[0], 8);
}

#[test]
fn a_quality_outside_one_to_a_hundred_is_refused() {
    assert_eq!(
        JpegOptions::new(0, [0; 3]).err(),
        Some(EncodeError::InvalidQuality)
    );
    assert_eq!(
        JpegOptions::new(101, [0; 3]).err(),
        Some(EncodeError::InvalidQuality)
    );
    assert_eq!(options(1).quality(), 1);
}

#[test]
fn a_flat_colour_comes_back_the_same_colour() {
    let picture = rgba(24, 16, |_, _| [200, 60, 30, 255]);
    let jpeg = encode_jpeg(&picture, options(90)).expect("encodes");
    for pixel in decoded(&jpeg).as_chunks::<4>().0 {
        for (channel, expected) in pixel.iter().zip([200u8, 60, 30]) {
            assert!(channel.abs_diff(expected) <= 2, "{pixel:?}");
        }
    }
}

#[test]
fn a_smooth_picture_survives_high_quality_closely() {
    let picture = rgba(64, 48, |x, y| {
        [
            u8::try_from(x * 4).expect("byte"),
            u8::try_from(y * 5).expect("byte"),
            u8::try_from((x + y) * 2).expect("byte"),
            255,
        ]
    });
    let original = picture.to_rgba().expect("memory");
    let fine = encode_jpeg(&picture, options(95)).expect("encodes");
    let coarse = encode_jpeg(&picture, options(20)).expect("encodes");
    let fine_error = mse(&original, &decoded(&fine));
    let coarse_error = mse(&original, &decoded(&coarse));
    assert!(fine_error < 10, "quality 95 erred by {fine_error}");
    assert!(coarse_error < 260, "quality 20 erred by {coarse_error}");
    assert!(fine.len() > coarse.len(), "a finer quality costs bytes");
}

#[test]
fn high_quality_keeps_full_resolution_colour_and_lower_halves_it() {
    let picture = rgba(16, 16, |x, _| {
        [u8::try_from(x * 16).expect("byte"), 0, 128, 255]
    });
    let full = encode_jpeg(&picture, options(90)).expect("encodes");
    let halved = encode_jpeg(&picture, options(89)).expect("encodes");
    assert_eq!(frame(&full), (3, 0x11));
    assert_eq!(frame(&halved), (3, 0x22));
    decoded(&full);
    decoded(&halved);
}

#[test]
fn a_grey_picture_is_written_with_one_component() {
    let picture = rgba(10, 10, |x, y| {
        let level = u8::try_from(x * 20 + y).expect("byte");
        [level, level, level, 255]
    });
    let jpeg = encode_jpeg(&picture, options(85)).expect("encodes");
    assert_eq!(frame(&jpeg).0, 1);
    let original = picture.to_rgba().expect("memory");
    assert!(mse(&original, &decoded(&jpeg)) < 26);
}

#[test]
fn transparency_is_composited_over_the_background() {
    let picture = rgba(8, 8, |_, _| [0, 0, 255, 0]);
    let red = JpegOptions::new(90, [255, 0, 0]).expect("valid");
    let jpeg = encode_jpeg(&picture, red).expect("encodes");
    for pixel in decoded(&jpeg).as_chunks::<4>().0 {
        assert!(pixel[0] > 250 && pixel[1] < 5 && pixel[2] < 5, "{pixel:?}");
    }
}

#[test]
fn an_odd_sized_picture_is_padded_by_its_own_edge() {
    for (width, height) in [(1, 1), (13, 7), (17, 33), (8, 9)] {
        let picture = rgba(width, height, |x, y| {
            [
                u8::try_from(x * 9 % 256).expect("byte"),
                u8::try_from(y * 7 % 256).expect("byte"),
                90,
                255,
            ]
        });
        let jpeg = encode_jpeg(&picture, options(92)).expect("encodes");
        let back = decode_as(ImageFormat::Jpeg, &jpeg, &limits()).expect("decodes");
        assert_eq!((back.width(), back.height()), (width, height));
    }
}

#[test]
fn noise_exercises_the_marker_stuffing_and_still_decodes() {
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let picture = rgba(40, 40, |_, _| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let [a, b, c, ..] = state.to_le_bytes();
        [a, b, c, 255]
    });
    let jpeg = encode_jpeg(&picture, options(100)).expect("encodes");
    decoded(&jpeg);
}

#[test]
fn an_indexed_picture_is_written_as_the_colours_it_shows() {
    let picture = Picture::indexed(
        16,
        8,
        IndexDepth::One,
        vec![[255, 255, 0, 255], [0, 0, 128, 255]],
        (0..128).map(|i| u8::from(i % 16 >= 8)).collect(),
        None,
    )
    .expect("valid");
    let jpeg = encode_jpeg(&picture, options(95)).expect("encodes");
    let original = picture.to_rgba().expect("memory");
    assert!(mse(&original, &decoded(&jpeg)) < 65);
}

#[test]
fn a_side_past_what_a_frame_header_can_state_is_refused() {
    struct Wide;
    impl PictureSource for Wide {
        fn width(&self) -> u32 {
            65_536
        }
        fn height(&self) -> u32 {
            1
        }
        fn kind(&self) -> PictureKind<'_> {
            PictureKind::Rgba
        }
        fn read_row(&self, _: u32, _: &mut [u8], _: &mut [u8]) {}
    }
    assert_eq!(encode_jpeg(&Wide, options(90)), Err(EncodeError::TooLarge));
}

fn unkept(jpeg: &[u8]) -> Unkept {
    match open_native(ImageFormat::Jpeg, jpeg, &limits()).expect("the file decodes") {
        NativeDocument::Picture { unkept, .. } => unkept,
        NativeDocument::Sprites(_) => panic!("a JPEG is one picture"),
    }
}

/// `jpeg` with `segment` (marker, then payload) after its start of image.
fn with_segment(jpeg: &[u8], marker: u8, payload: &[u8]) -> Vec<u8> {
    let length = u16::try_from(payload.len() + 2).expect("a segment's length");
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, marker]);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(payload);
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// A JPEG holds more than its picture in any segment the encoder would not
/// write back: metadata, a comment, or a JFIF header stating a density.
/// Framing that only says how the picture is coded is not more.
#[test]
fn what_a_jpeg_holds_beside_its_picture_is_said_when_it_opens() {
    let shade = |at: u32| u8::try_from(at * 8).expect("small");
    let jpeg = encode_jpeg(
        &rgba(8, 8, |x, y| [shade(x), shade(y), 9, 255]),
        options(80),
    )
    .expect("encodes");
    assert_eq!(unkept(&jpeg), Unkept::default(), "its own output");
    let beside = Unkept {
        precision: false,
        extras: true,
    };
    assert_eq!(unkept(&with_segment(&jpeg, 0xFE, b"a note")), beside);
    assert_eq!(unkept(&with_segment(&jpeg, 0xE1, b"Exif\0\0")), beside);
    assert_eq!(unkept(&with_segment(&jpeg, 0xE2, b"ICC_PROFILE\0")), beside);
    // The encoder's own header, a 14-byte JFIF segment, given way to another.
    let bare: Vec<u8> = [&jpeg[..2], &jpeg[2 + 4 + 14..]].concat();
    let dpi = *b"JFIF\0\x01\x02\x01\x01\x2C\x01\x2C\x00\x00";
    assert_eq!(unkept(&with_segment(&bare, 0xE0, &dpi)), beside);
    let later = *b"JFIF\0\x01\x02\x00\x00\x01\x00\x01\x00\x00";
    assert_eq!(
        unkept(&with_segment(&bare, 0xE0, &later)),
        Unkept::default(),
        "another version of the same header"
    );
    let adobe = *b"Adobe\0\x64\0\0\0\0\x01";
    assert_eq!(
        unkept(&with_segment(&jpeg, 0xEE, &adobe)),
        Unkept::default()
    );
}
