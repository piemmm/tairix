//! The BMP encoder: every depth it writes read back through the crate's own
//! decoder and natively, the rows' padding at every width, and the density
//! it states.

use alloc::vec::Vec;

use crate::encode_fixture::{self, rgba, shown, Noise};
use crate::{
    decode_as, encode_bmp, DecodeLimits, Density, DensityUnit, ImageFormat, IndexDepth, Picture,
    Pixels, Rgba8, Unkept,
};

fn limits() -> DecodeLimits {
    DecodeLimits::new(4096, 4096, 4096 * 4096, 0)
}

/// Encode `picture`, check the file shows it, and answer the file and its
/// native reading.
fn round_trip(picture: &Picture) -> (Vec<u8>, Picture, Unkept) {
    let bmp = encode_bmp(picture).expect("the picture encodes");
    let decoded = decode_as(ImageFormat::Bmp, &bmp, &limits()).expect("the file decodes");
    assert_eq!(
        shown(decoded.pixels()),
        shown(&picture.to_rgba().expect("memory")),
        "the file shows a different picture"
    );
    let (native, unkept, _) = encode_fixture::native(ImageFormat::Bmp, &bmp, &limits());
    assert_eq!(
        unkept,
        Unkept::default(),
        "its own output holds nothing more"
    );
    (bmp, native, unkept)
}

/// The bit count the file's header declares.
fn bits(bmp: &[u8]) -> u16 {
    u16::from_le_bytes([bmp[28], bmp[29]])
}

fn colours(count: usize) -> Vec<Rgba8> {
    (0..count)
        .map(|at| {
            let at = u8::try_from(at).expect("at most 256");
            [at.wrapping_mul(3), at, 255 - at, 255]
        })
        .collect()
}

#[test]
fn a_palette_picture_reads_back_as_its_indices_and_palette_at_every_width() {
    for (depth, written) in [
        (IndexDepth::One, 1),
        (IndexDepth::Two, 4),
        (IndexDepth::Four, 4),
        (IndexDepth::Eight, 8),
    ] {
        for width in 1..=9 {
            let count = depth.colours();
            let indices = (0..width * 3)
                .map(|at| u8::try_from(at % count).expect("an index"))
                .collect();
            let picture = Picture::indexed(
                u32::try_from(width).expect("small"),
                3,
                depth,
                colours(count),
                indices,
                None,
            )
            .expect("valid");
            let (bmp, native, _) = round_trip(&picture);
            assert_eq!(bits(&bmp), written, "{depth:?}");
            let (
                Pixels::Indexed {
                    indices: read,
                    palette,
                    ..
                },
                Pixels::Indexed {
                    indices: held,
                    palette: own,
                    ..
                },
            ) = (native.pixels(), picture.pixels())
            else {
                panic!("a palette picture reads back as one");
            };
            assert_eq!(read, held, "{depth:?} at {width} across");
            assert_eq!(palette, own);
        }
    }
}

#[test]
fn opaque_colour_is_written_at_twenty_four_bits() {
    let mut noise = Noise(0x9E37_79B9_7F4A_7C15);
    for width in 1..=5 {
        let picture = rgba(width, 4, |_, _| {
            [noise.next(), noise.next(), noise.next(), 255]
        });
        let (bmp, native, _) = round_trip(&picture);
        assert_eq!(bits(&bmp), 24);
        assert_eq!(native.pixels(), picture.pixels());
    }
}

#[test]
fn transparency_is_written_at_thirty_two_bits_with_an_alpha_mask() {
    let picture = rgba(3, 3, |x, y| {
        [10, 20, 30, u8::try_from(x * 100 + y).expect("small")]
    });
    let (bmp, native, _) = round_trip(&picture);
    assert_eq!(bits(&bmp), 32);
    assert_eq!(native.pixels(), picture.pixels());
}

#[test]
fn a_translucent_palette_picture_is_written_as_colour() {
    let mut palette = colours(2);
    palette[1][3] = 128;
    let picture =
        Picture::indexed(2, 1, IndexDepth::One, palette, alloc::vec![0, 1], None).expect("valid");
    let (bmp, _, _) = round_trip(&picture);
    assert_eq!(bits(&bmp), 32);
    let masked = Picture::indexed(
        2,
        1,
        IndexDepth::One,
        colours(2),
        alloc::vec![0, 1],
        Some(alloc::vec![255, 0]),
    )
    .expect("valid");
    let (bmp, _, _) = round_trip(&masked);
    assert_eq!(bits(&bmp), 32);
}

#[test]
fn a_bmp_states_its_density_per_metre() {
    let read = |density| {
        let picture = rgba(2, 2, |_, _| [1, 2, 3, 255]).with_density(density);
        round_trip(&picture).1.density()
    };
    let metre = Density::whole(2835, 3780, DensityUnit::Metre);
    assert_eq!(read(metre), metre);
    assert_eq!(
        read(Density::whole(72, 96, DensityUnit::Inch)),
        metre,
        "rounded per metre"
    );
    assert_eq!(
        read(Density::whole(1, 2, DensityUnit::Aspect)),
        None,
        "a bare shape it cannot state"
    );
    assert_eq!(read(None), None);
}
