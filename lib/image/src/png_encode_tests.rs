//! The PNG encoder: the colour type each kind of picture is written as, the
//! mask a palette cannot hold, and every file read back through the crate's
//! own decoder, pixel for pixel.

use alloc::vec;
use alloc::vec::Vec;

use crate::png_fixture::{build_png, chunk};
use crate::{
    decode_as, encode_png, open_native, DecodeLimits, ImageFormat, IndexDepth, NativeDocument,
    Picture, Pixels, Rgba8, Unkept,
};

fn limits() -> DecodeLimits {
    DecodeLimits::new(4096, 4096, 4096 * 4096, 0)
}

/// A small deterministic generator, so a noise picture is the same every run.
struct Noise(u64);

impl Noise {
    fn next(&mut self) -> u8 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        u8::try_from(self.0 >> 56).expect("the top byte")
    }
}

/// The IHDR's bit depth and colour type.
fn header(png: &[u8]) -> (u8, u8) {
    (png[24], png[25])
}

/// Whether the file carries a chunk of `kind`.
fn has_chunk(png: &[u8], kind: [u8; 4]) -> bool {
    png.windows(4).any(|window| window == kind)
}

fn native(png: &[u8]) -> Picture {
    match open_native(ImageFormat::Png, png, &limits()).expect("the file decodes") {
        NativeDocument::Picture { picture, .. } => picture,
        NativeDocument::Sprites(_) => panic!("a PNG is one picture"),
    }
}

/// `rgba` as it looks: a fully transparent pixel shows nothing, whatever
/// colour it was left holding.
fn shown(rgba: &[u8]) -> Vec<u8> {
    rgba.as_chunks::<4>()
        .0
        .iter()
        .flat_map(|&pixel| if pixel[3] == 0 { [0; 4] } else { pixel })
        .collect()
}

/// Encode `picture`, decode the file, and check it looks exactly the same.
fn round_trip(picture: &Picture) -> Vec<u8> {
    let png = encode_png(picture).expect("the picture encodes");
    let decoded = decode_as(ImageFormat::Png, &png, &limits()).expect("the file decodes");
    assert_eq!(
        shown(decoded.pixels()),
        shown(&picture.to_rgba().expect("memory")),
        "the file shows a different picture"
    );
    png
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

#[test]
fn a_palette_picture_keeps_its_palette_and_every_index() {
    let palette = vec![[10, 20, 30, 255], [40, 50, 60, 255], [70, 80, 90, 128]];
    let indices = vec![0, 1, 2, 2, 1, 0];
    let picture = Picture::indexed(
        3,
        2,
        IndexDepth::Eight,
        palette.clone(),
        indices.clone(),
        None,
    )
    .expect("valid");
    let png = round_trip(&picture);
    assert_eq!(header(&png), (2, 3), "three colours need only two bits");
    assert!(has_chunk(&png, *b"tRNS"));
    let Pixels::Indexed {
        palette: read,
        indices: read_indices,
        ..
    } = native(&png).into_pixels()
    else {
        panic!("a palette file reopens as indices");
    };
    assert_eq!(read, palette);
    assert_eq!(read_indices, indices);
}

#[test]
fn an_opaque_palette_writes_no_transparency_chunk() {
    let picture = Picture::indexed(
        2,
        1,
        IndexDepth::One,
        vec![[0, 0, 0, 255], [255, 255, 255, 255]],
        vec![1, 0],
        None,
    )
    .expect("valid");
    let png = round_trip(&picture);
    assert_eq!(header(&png), (1, 3));
    assert!(!has_chunk(&png, *b"tRNS"));
}

#[test]
fn a_binary_mask_becomes_one_new_transparent_entry() {
    let palette = vec![[255, 0, 0, 255], [0, 255, 0, 255]];
    let picture = Picture::indexed(
        3,
        1,
        IndexDepth::One,
        palette,
        vec![0, 1, 1],
        Some(vec![255, 0, 255]),
    )
    .expect("valid");
    let png = round_trip(&picture);
    assert_eq!(header(&png), (2, 3), "the new entry needs a second bit");
    let Pixels::Indexed {
        palette, indices, ..
    } = native(&png).into_pixels()
    else {
        panic!("indices");
    };
    assert_eq!(palette.len(), 3);
    assert_eq!(palette[2], [0, 0, 0, 0]);
    assert_eq!(indices, [0, 2, 1]);
}

#[test]
fn a_full_palette_reuses_an_entry_no_visible_pixel_selects() {
    let palette: Vec<Rgba8> = (0..=255u8).map(|i| [i, i, i, 255]).collect();
    let mut indices: Vec<u8> = (0..=255u8).collect();
    let mut mask = vec![255u8; 256];
    // Entry 7 shows only under the mask, so it is the one to give up.
    indices[200] = 7;
    indices[7] = 8;
    mask[200] = 0;
    let picture =
        Picture::indexed(256, 1, IndexDepth::Eight, palette, indices, Some(mask)).expect("valid");
    let png = round_trip(&picture);
    assert_eq!(header(&png), (8, 3));
}

#[test]
fn a_full_palette_every_entry_of_which_shows_falls_back_to_rgba() {
    let palette: Vec<Rgba8> = (0..=255u8).map(|i| [i, 0, 0, 255]).collect();
    let mut indices: Vec<u8> = (0..=255u8).collect();
    indices.push(0);
    let mut mask = vec![255u8; 257];
    mask[256] = 0;
    let picture =
        Picture::indexed(257, 1, IndexDepth::Eight, palette, indices, Some(mask)).expect("valid");
    let png = round_trip(&picture);
    assert_eq!(header(&png).1, 6);
}

#[test]
fn a_partial_mask_is_written_as_rgba() {
    let picture = Picture::indexed(
        2,
        1,
        IndexDepth::One,
        vec![[9, 9, 9, 255], [200, 100, 50, 255]],
        vec![0, 1],
        Some(vec![255, 100]),
    )
    .expect("valid");
    let png = round_trip(&picture);
    assert_eq!(header(&png), (8, 6));
}

#[test]
fn an_opaque_colour_picture_drops_its_alpha_channel() {
    let picture = rgba(5, 3, |x, y| {
        [
            u8::try_from(x * 40).expect("small"),
            100,
            u8::try_from(y * 70).expect("small"),
            255,
        ]
    });
    let png = round_trip(&picture);
    assert_eq!(header(&png), (8, 2));
}

#[test]
fn a_translucent_colour_picture_keeps_rgba() {
    let picture = rgba(4, 4, |x, y| {
        [200, 10, 30, u8::try_from(x * 60 + y).expect("small")]
    });
    let png = round_trip(&picture);
    assert_eq!(header(&png), (8, 6));
    // A truecolour picture never reopens as indices, however few colours.
    assert!(matches!(native(&png).pixels(), Pixels::Rgba(_)));
}

#[test]
fn grey_is_written_at_the_shallowest_depth_its_levels_are_exact_at() {
    let cases: [(&[u8], u8); 4] = [
        (&[0, 255], 1),
        (&[0, 85, 170, 255], 2),
        (&[0, 17, 34, 238, 255], 4),
        (&[0, 1, 255], 8),
    ];
    for (levels, bits) in cases {
        let width = u32::try_from(levels.len()).expect("small");
        let picture = rgba(width, 2, |x, _| {
            let level = levels[x as usize];
            [level, level, level, 255]
        });
        let png = round_trip(&picture);
        assert_eq!(header(&png), (bits, 0), "levels {levels:?}");
    }
}

#[test]
fn translucent_grey_is_written_as_grey_with_alpha() {
    let picture = rgba(3, 1, |x, _| {
        [50, 50, 50, u8::try_from(x * 100).expect("small")]
    });
    let png = round_trip(&picture);
    assert_eq!(header(&png), (8, 4));
}

#[test]
fn a_smooth_picture_is_filtered_and_still_exact() {
    let picture = rgba(97, 61, |x, y| {
        let r = u8::try_from((x * 3 + y) % 256).expect("byte");
        let g = u8::try_from((x + y * 5) % 256).expect("byte");
        [r, g, r ^ g, 255]
    });
    let png = round_trip(&picture);
    // A gradient compresses far below its raw size once filtered.
    assert!(png.len() < 97 * 61 * 3 / 2, "{} bytes", png.len());
}

#[test]
fn a_large_picture_is_split_across_several_data_chunks() {
    let mut noise = Noise(0x9E37_79B9_7F4A_7C15);
    let picture = rgba(200, 200, |_, _| {
        [
            noise_byte(&mut noise),
            noise_byte(&mut noise),
            noise_byte(&mut noise),
            noise_byte(&mut noise),
        ]
    });
    let png = round_trip(&picture);
    let chunks = png.windows(4).filter(|window| *window == b"IDAT").count();
    assert!(chunks > 1, "{chunks} IDAT chunks");
}

fn noise_byte(noise: &mut Noise) -> u8 {
    noise.next()
}

#[test]
fn the_same_picture_always_encodes_to_the_same_bytes() {
    let picture = rgba(16, 16, |x, y| {
        [
            u8::try_from(x * 16).expect("byte"),
            u8::try_from(y * 16).expect("byte"),
            7,
            255,
        ]
    });
    assert_eq!(encode_png(&picture), encode_png(&picture));
}

fn unkept(png: &[u8]) -> Unkept {
    match open_native(ImageFormat::Png, png, &limits()).expect("the file decodes") {
        NativeDocument::Picture { unkept, .. } => unkept,
        NativeDocument::Sprites(_) => panic!("a PNG is one picture"),
    }
}

/// A PNG holds more than its picture in samples finer than eight bits and
/// in any chunk beside the ones the picture is read from, and says so.
#[test]
fn what_a_png_holds_beside_its_picture_is_said_when_it_opens() {
    let plain = build_png(1, 1, 8, 2, 0, None, None, &[0, 1, 2, 3]);
    assert_eq!(unkept(&plain), Unkept::default());
    let picture = Picture::rgba(2, 1, vec![1, 2, 3, 255, 4, 5, 6, 128]).expect("valid");
    assert_eq!(
        unkept(&encode_png(&picture).expect("encodes")),
        Unkept::default()
    );
    let wide = build_png(1, 1, 16, 2, 0, None, None, &[0, 1, 1, 2, 2, 3, 3]);
    assert_eq!(
        unkept(&wide),
        Unkept {
            precision: true,
            extras: false
        }
    );
    // After the signature and the header chunk.
    let (head, tail) = plain.split_at(8 + 25);
    let mut texted = head.to_vec();
    texted.extend(chunk(*b"tEXt", b"Title\0Sunset"));
    texted.extend_from_slice(tail);
    assert_eq!(
        unkept(&texted),
        Unkept {
            precision: false,
            extras: true
        }
    );
}
