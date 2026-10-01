//! The sprite encoder: a canonical file read natively and written back is
//! the same file, byte for byte, at every depth, mode word and mask form; a
//! sprite the decoder cannot read goes back exactly as it came; and every
//! picture a mode cannot state is refused.

use alloc::vec;
use alloc::vec::Vec;

use crate::sprite::tests::{
    area, limits, one, ro35, ro5, Sprite as Fixture, FLAG_ALPHA, FLAG_RGB_ORDER, MODE_16, MODE_2,
    MODE_256, MODE_4,
};
use crate::{
    desktop_palette, encode_sprite_area, EncodeError, IndexDepth, Picture, Pixels, Rgba8,
    SpriteAreaReader, SpriteEntry, SpriteInput, SpriteMode, SpriteName, SpritePalette,
};

/// Every sprite of `bytes`, read natively.
fn entries(bytes: &[u8]) -> Vec<SpriteEntry> {
    let mut reader = SpriteAreaReader::open(bytes, &limits()).expect("the area opens");
    (0..reader.count())
        .map(|index| {
            reader
                .sprite(index)
                .expect("no machine refusal")
                .expect("present")
        })
        .collect()
}

/// `entries` as the encoder takes them.
fn inputs(entries: &[SpriteEntry]) -> Vec<SpriteInput<'_>> {
    entries
        .iter()
        .map(|entry| match entry {
            SpriteEntry::Picture(sprite) => SpriteInput::Picture {
                name: sprite.name,
                mode: sprite.mode,
                palette: &sprite.palette,
                masked: sprite.masked,
                source: &sprite.picture,
            },
            SpriteEntry::Opaque(kept) => SpriteInput::Opaque(&kept.bytes),
        })
        .collect()
}

/// Read `bytes` natively and write it back.
fn rewrite(bytes: &[u8]) -> Vec<u8> {
    let read = entries(bytes);
    encode_sprite_area(&inputs(&read)).expect("the area encodes")
}

/// The one sprite of `bytes`, which must read back as pixels.
fn only_picture(bytes: &[u8]) -> crate::Sprite {
    let mut read = entries(bytes);
    assert_eq!(read.len(), 1, "one sprite");
    match read.remove(0) {
        SpriteEntry::Picture(sprite) => sprite,
        SpriteEntry::Opaque(kept) => panic!("kept as bytes: {:?}", kept.reason),
    }
}

/// Check a canonical file survives a native read and a write unchanged.
fn same_after_rewrite(bytes: &[u8]) {
    assert_eq!(rewrite(bytes), bytes, "the rewritten area differs");
}

/// `count` distinct values `bits` wide, with `unused` top bits left clear.
fn values(count: u32, bits: u32, unused: u32) -> Vec<u32> {
    let mask = if bits - unused >= 32 {
        u32::MAX
    } else {
        (1u32 << (bits - unused)) - 1
    };
    (0..count)
        .map(|i| i.wrapping_mul(0x9E37_79B9).rotate_left(i % 7) & mask)
        .collect()
}

fn desktop(depth: IndexDepth) -> Vec<Rgba8> {
    desktop_palette(depth)
        .iter()
        .map(|&[r, g, b]| [r, g, b, 255])
        .collect()
}

#[test]
fn a_known_sprite_is_written_exactly_as_risc_os_lays_it_out() {
    let picture = Picture::indexed(
        3,
        1,
        IndexDepth::Four,
        desktop(IndexDepth::Four),
        vec![1, 2, 3],
        None,
    )
    .expect("valid");
    let bytes = encode_sprite_area(&[SpriteInput::Picture {
        name: SpriteName::new("icon").expect("valid"),
        mode: SpriteMode::from_value(12).expect("mode 12"),
        palette: &SpritePalette::Implied,
        masked: false,
        source: &picture,
    }])
    .expect("encodes");
    let expected: Vec<u8> = [
        // The area: one sprite, the first at 16, the free space at 64.
        &[1, 0, 0, 0, 16, 0, 0, 0, 64, 0, 0, 0][..],
        // Its control block: 48 bytes long, named, one word by one row.
        &[48, 0, 0, 0],
        b"icon\0\0\0\0\0\0\0\0",
        &[0, 0, 0, 0, 0, 0, 0, 0],
        // First bit 0, last bit 11, image and mask both at 44, mode 12.
        &[
            0, 0, 0, 0, 11, 0, 0, 0, 44, 0, 0, 0, 44, 0, 0, 0, 12, 0, 0, 0,
        ],
        // Three nibbles, least significant pixel first.
        &[0x21, 0x03, 0, 0],
    ]
    .concat();
    assert_eq!(bytes, expected);
}

#[test]
fn every_numbered_depth_is_written_back_byte_for_byte() {
    for (mode, bits) in [(MODE_2, 1), (MODE_4, 2), (MODE_16, 4), (MODE_256, 8)] {
        let pixels = values(12, bits, 0);
        same_after_rewrite(&one(Fixture::new(mode, bits, 6, 2).pixels(&pixels)));
        same_after_rewrite(&one(Fixture::new(mode, bits, 6, 2)
            .pixels(&pixels)
            .mask_image(&[
                true, false, true, true, false, true, false, false, true, true, true, false,
            ])));
        let count = 1usize << bits;
        let colours: Vec<[u8; 3]> = (0..count)
            .map(|i| {
                let i = u8::try_from(i).expect("at most 255");
                [i, i.wrapping_mul(3), i.wrapping_mul(7)]
            })
            .collect();
        same_after_rewrite(&one(Fixture::new(mode, bits, 6, 2)
            .palette(&colours)
            .pixels(&pixels)));
    }
}

#[test]
fn a_short_vidc_palette_is_kept_as_the_file_held_it() {
    let pixels = values(8, 8, 0);
    for entries in [16, 64] {
        let colours: Vec<[u8; 3]> = (0..entries)
            .map(|i: u8| [i.wrapping_mul(16), 0x20, 0xF0 - i])
            .collect();
        same_after_rewrite(&one(Fixture::new(MODE_256, 8, 4, 2)
            .palette(&colours)
            .pixels(&pixels)));
    }
}

/// A mode word, with or without a wide mask.
type Word = fn(bool) -> u32;

/// Check a canonical file of one sprite reads back as pixels, not kept bytes
/// a rewrite would trivially reproduce, and survives the rewrite unchanged.
fn picture_after_rewrite(bytes: &[u8]) {
    let _ = only_picture(bytes);
    same_after_rewrite(bytes);
}

#[test]
fn every_mode_word_depth_is_written_back_byte_for_byte() {
    let opaque = [true, false, true, true, false, false, true, true];
    let alpha = [0, 255, 17, 128, 200, 1, 99, 255];
    // Type 16 has no RISC OS 3.5 word: five bits of type reach bit 31.
    let words: [(Word, u32, u32); 9] = [
        (|wide| ro35(1, wide), 1, 0),
        (|wide| ro35(2, wide), 2, 0),
        (|wide| ro35(3, wide), 4, 0),
        (|wide| ro35(4, wide), 8, 0),
        (|wide| ro35(5, wide), 16, 1),
        (|wide| ro35(6, wide), 32, 8),
        (|wide| ro35(8, wide), 24, 0),
        (|wide| ro35(10, wide), 16, 0),
        (|wide| ro5(16, 0, wide), 16, 4),
    ];
    for (word, bits, unused) in words {
        let pixels = values(8, bits, unused);
        picture_after_rewrite(&one(Fixture::new(word(false), bits, 4, 2).pixels(&pixels)));
        picture_after_rewrite(&one(Fixture::new(word(false), bits, 4, 2)
            .pixels(&pixels)
            .mask_bits(&opaque)));
        picture_after_rewrite(&one(Fixture::new(word(true), bits, 4, 2)
            .pixels(&pixels)
            .mask_alpha(&alpha)));
    }
}

#[test]
fn a_risc_os_five_word_keeps_its_channel_order_and_alpha() {
    for (sprite_type, bits) in [(5, 16), (6, 32), (16, 16)] {
        let pixels = values(8, bits, 0);
        same_after_rewrite(&one(Fixture::new(
            ro5(sprite_type, FLAG_ALPHA, false),
            bits,
            4,
            2,
        )
        .pixels(&pixels)));
        same_after_rewrite(&one(Fixture::new(
            ro5(sprite_type, FLAG_ALPHA | FLAG_RGB_ORDER, false),
            bits,
            4,
            2,
        )
        .pixels(&pixels)));
    }
    let unused = values(8, 32, 8);
    same_after_rewrite(&one(
        Fixture::new(ro5(6, FLAG_RGB_ORDER, false), 32, 4, 2).pixels(&unused)
    ));
}

#[test]
fn an_area_holding_a_sprite_that_cannot_be_read_goes_back_unchanged() {
    let bytes = area(&[
        Fixture::new(MODE_16, 4, 4, 1).pixels(&[1, 2, 3, 4]),
        Fixture::new(ro35(9, false), 32, 2, 1).pixels(&[0x1234_5678, 0x9ABC_DEF0]),
        Fixture::new(7, 4, 2, 1),
        Fixture::new(MODE_2, 1, 8, 1).pixels(&[1, 0, 1, 1, 0, 0, 1, 0]),
    ]);
    let read = entries(&bytes);
    assert!(matches!(read[1], SpriteEntry::Opaque(_)));
    assert!(matches!(read[2], SpriteEntry::Opaque(_)));
    same_after_rewrite(&bytes);
}

#[test]
fn a_kept_sprite_is_written_back_exactly() {
    // Declared a word short of its own image, so it cannot be read — and any
    // padding appended to it would make it readable.
    let short = one(Fixture::new(MODE_16, 4, 8, 1).pixels(&[1; 8]).length(44));
    let [SpriteEntry::Opaque(kept)] = &entries(&short)[..] else {
        panic!("a sprite short of its own image is kept as bytes");
    };
    assert_eq!(kept.bytes.len(), 44);
    let bytes = encode_sprite_area(&[SpriteInput::Opaque(&kept.bytes)]).expect("encodes");
    assert_eq!(&bytes[12..], kept.bytes.as_slice());
    let [SpriteEntry::Opaque(again)] = &entries(&bytes)[..] else {
        panic!("still kept as bytes after being written back");
    };
    assert_eq!(again.reason, kept.reason);
}

#[test]
fn a_kept_sprite_off_a_word_is_refused_rather_than_misplacing_those_after_it() {
    let short = one(Fixture::new(MODE_16, 4, 8, 1).pixels(&[1; 8]).length(46));
    let [SpriteEntry::Opaque(kept)] = &entries(&short)[..] else {
        panic!("a sprite short of its own image is kept as bytes");
    };
    assert_eq!(kept.bytes.len(), 46);
    assert_eq!(
        encode_sprite_area(&[SpriteInput::Opaque(&kept.bytes)]),
        Err(EncodeError::SpriteOpaqueMalformed)
    );
}

#[test]
fn a_kept_sprites_length_word_is_restated_as_the_bytes_it_heads() {
    let mut kept = one(Fixture::new(ro35(9, false), 32, 1, 1))[12..].to_vec();
    kept[0] = 0xFF;
    let bytes = encode_sprite_area(&[SpriteInput::Opaque(&kept)]).expect("encodes");
    let written = &bytes[12..];
    assert_eq!(
        u32::from_le_bytes([written[0], written[1], written[2], written[3]]) as usize,
        kept.len()
    );
    assert_eq!(&written[4..], &kept[4..]);
}

#[test]
fn a_new_paletted_sprite_with_its_own_colours_reads_back_as_written() {
    let palette: Vec<Rgba8> = (0..16u8).map(|i| [i * 16, 255 - i * 16, i, 255]).collect();
    let indices: Vec<u8> = (0..12).map(|i| i % 16).collect();
    let mask: Vec<u8> = (0..12).map(|i| if i % 3 == 0 { 0 } else { 255 }).collect();
    let picture = Picture::indexed(
        4,
        3,
        IndexDepth::Four,
        palette.clone(),
        indices.clone(),
        Some(mask.clone()),
    )
    .expect("valid");
    let mode = SpriteMode::indexed(IndexDepth::Four, (1, 1), false);
    assert!(mode.is_numbered());
    let bytes = encode_sprite_area(&[SpriteInput::Picture {
        name: SpriteName::new("new").expect("valid"),
        mode,
        palette: &SpritePalette::Full,
        masked: true,
        source: &picture,
    }])
    .expect("encodes");
    let sprite = only_picture(&bytes);
    assert_eq!(sprite.mode, mode);
    assert!(matches!(sprite.palette, SpritePalette::Stored(_)));
    assert_eq!(
        sprite.picture.into_pixels(),
        Pixels::Indexed {
            depth: IndexDepth::Four,
            palette,
            indices,
            mask: Some(mask)
        }
    );
}

#[test]
fn a_new_truecolour_sprite_keeps_partial_transparency_in_a_wide_mask() {
    let rgba: Vec<u8> = (0..16u8)
        .flat_map(|i| [i * 15, 100, 255 - i * 15, i * 17])
        .collect();
    let picture = Picture::rgba(4, 4, rgba.clone()).expect("valid");
    let bytes = encode_sprite_area(&[SpriteInput::Picture {
        name: SpriteName::new("photo").expect("valid"),
        mode: SpriteMode::truecolour((1, 1), true),
        palette: &SpritePalette::Implied,
        masked: true,
        source: &picture,
    }])
    .expect("encodes");
    assert_eq!(
        only_picture(&bytes).picture.into_pixels(),
        Pixels::Rgba(rgba)
    );
}

/// Encode one picture sprite, for the refusals.
fn encode_one(
    mode: u32,
    palette: &SpritePalette,
    masked: bool,
    picture: &Picture,
) -> Result<Vec<u8>, EncodeError> {
    encode_sprite_area(&[SpriteInput::Picture {
        name: SpriteName::new("refused").expect("valid"),
        mode: SpriteMode::from_value(mode).expect("a mode"),
        palette,
        masked,
        source: picture,
    }])
}

fn sixteen(mask: Option<Vec<u8>>) -> Picture {
    Picture::indexed(
        2,
        1,
        IndexDepth::Four,
        desktop(IndexDepth::Four),
        vec![0, 1],
        mask,
    )
    .expect("valid")
}

#[test]
fn a_picture_of_the_wrong_kind_for_its_mode_is_refused() {
    let rgba = Picture::rgba(1, 1, vec![1, 2, 3, 255]).expect("valid");
    assert_eq!(
        encode_one(MODE_16, &SpritePalette::Implied, false, &rgba),
        Err(EncodeError::SpriteLayoutMismatch)
    );
    assert_eq!(
        encode_one(MODE_4, &SpritePalette::Implied, false, &sixteen(None)),
        Err(EncodeError::SpriteLayoutMismatch),
        "a sixteen-colour picture under a four-colour mode"
    );
    assert_eq!(
        encode_one(MODE_16, &SpritePalette::Implied, true, &sixteen(None)),
        Err(EncodeError::SpriteLayoutMismatch),
        "a mask asked for that the picture does not carry"
    );
    assert_eq!(
        encode_one(ro35(6, false), &SpritePalette::Full, false, &rgba),
        Err(EncodeError::SpriteLayoutMismatch),
        "a direct colour sprite has no palette to write"
    );
}

#[test]
fn a_palette_that_is_not_the_pixels_colours_is_refused() {
    let mut palette = desktop(IndexDepth::Four);
    palette[3] = [1, 2, 3, 255];
    let recoloured =
        Picture::indexed(2, 1, IndexDepth::Four, palette, vec![0, 1], None).expect("valid");
    assert_eq!(
        encode_one(MODE_16, &SpritePalette::Implied, false, &recoloured),
        Err(EncodeError::SpritePaletteMismatch)
    );
    assert_eq!(
        encode_one(
            MODE_16,
            &SpritePalette::Stored(vec![0; 16 * 8]),
            false,
            &recoloured
        ),
        Err(EncodeError::SpritePaletteMismatch)
    );
    assert_eq!(
        encode_one(
            MODE_16,
            &SpritePalette::Stored(vec![0; 13]),
            false,
            &recoloured
        ),
        Err(EncodeError::SpritePaletteMismatch),
        "a stored palette that is not whole entries"
    );
    assert!(encode_one(MODE_16, &SpritePalette::Full, false, &recoloured).is_ok());
}

#[test]
fn a_translucent_palette_entry_is_refused() {
    let mut palette = desktop(IndexDepth::Four);
    palette[0][3] = 10;
    let picture =
        Picture::indexed(2, 1, IndexDepth::Four, palette, vec![0, 1], None).expect("valid");
    assert_eq!(
        encode_one(MODE_16, &SpritePalette::Full, false, &picture),
        Err(EncodeError::SpritePaletteAlpha)
    );
}

#[test]
fn a_partial_mask_under_a_binary_mask_form_is_refused() {
    let partial = sixteen(Some(vec![255, 100]));
    assert_eq!(
        encode_one(MODE_16, &SpritePalette::Implied, true, &partial),
        Err(EncodeError::SpriteMaskNotBinary),
        "a numbered mode's mask is on or off"
    );
    assert_eq!(
        encode_one(ro35(3, false), &SpritePalette::Implied, true, &partial),
        Err(EncodeError::SpriteMaskNotBinary),
        "a one-bit mask is on or off"
    );
    assert!(encode_one(ro35(3, true), &SpritePalette::Implied, true, &partial).is_ok());
    let translucent = Picture::rgba(1, 1, vec![9, 9, 9, 64]).expect("valid");
    assert_eq!(
        encode_one(ro35(6, false), &SpritePalette::Implied, true, &translucent),
        Err(EncodeError::SpriteMaskNotBinary)
    );
}

#[test]
fn transparency_with_nowhere_to_go_is_refused() {
    let translucent = Picture::rgba(1, 1, vec![9, 9, 9, 64]).expect("valid");
    assert_eq!(
        encode_one(ro35(6, false), &SpritePalette::Implied, false, &translucent),
        Err(EncodeError::SpriteAlphaUnrepresentable)
    );
    assert!(encode_one(
        ro5(6, FLAG_ALPHA, false),
        &SpritePalette::Implied,
        false,
        &translucent
    )
    .is_ok());
}

#[test]
fn a_sprite_whose_length_overflows_is_refused_rather_than_panicking() {
    /// A truecolour picture too large for any length word, never read.
    struct Huge;
    impl crate::PictureSource for Huge {
        fn width(&self) -> u32 {
            (1 << 30) - 1
        }
        fn height(&self) -> u32 {
            u32::MAX
        }
        fn kind(&self) -> crate::PictureKind<'_> {
            crate::PictureKind::Rgba
        }
        fn read_row(&self, _: u32, _: &mut [u8], _: &mut [u8]) {}
    }
    assert_eq!(
        encode_sprite_area(&[SpriteInput::Picture {
            name: SpriteName::new("huge").expect("valid"),
            mode: SpriteMode::truecolour((1, 1), true),
            palette: &SpritePalette::Implied,
            masked: true,
            source: &Huge,
        }]),
        Err(EncodeError::TooLarge)
    );
}

#[test]
fn an_empty_area_and_a_truncated_kept_sprite_are_refused() {
    assert_eq!(encode_sprite_area(&[]), Err(EncodeError::SpriteAreaEmpty));
    assert_eq!(
        encode_sprite_area(&[SpriteInput::Opaque(&[0; 43])]),
        Err(EncodeError::SpriteOpaqueMalformed)
    );
}
