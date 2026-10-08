//! RISC OS sprite decoder tests: every mode word form, every depth, the
//! palette arrangements, both mask forms, the wastage, and every refusal.
//!
//! Every input is built here from writers that mirror the format's own
//! layout — the crate ships no fixtures. [`Sprite`] carries one control
//! block and its payload, so a test states only the field it is about, and
//! [`area`] lays a set of them out as a file.

use alloc::vec;
use alloc::vec::Vec;

use crate::{
    decode, decode_as, decode_fitted_as, desktop_palette, open_native, probe_as, sniff,
    DecodeError, DecodeLimits, FitBox, ImageFormat, IndexDepth, NativeDocument, Pixels,
    RasterImage, Sequence, SequenceKind, SpriteAreaReader, SpriteEntry, SpriteLayout, SpriteMode,
    SpriteName, SpritePalette,
};

/// Limits generous enough for every fixture here.
pub(crate) fn limits() -> DecodeLimits {
    DecodeLimits::new(256, 256, 256 * 256, 0)
}

/// A numbered mode at each depth the table holds.
pub(crate) const MODE_2: u32 = 0;
pub(crate) const MODE_4: u32 = 1;
pub(crate) const MODE_16: u32 = 12;
pub(crate) const MODE_256: u32 = 15;

/// A RISC OS 3.5 sprite mode word: a type over two 90 DPI fields.
pub(crate) fn ro35(sprite_type: u32, wide_mask: bool) -> u32 {
    (u32::from(wide_mask) << 31) | (sprite_type << 27) | (90 << 14) | (90 << 1) | 1
}

/// A RISC OS 5 sprite mode word: the fixed pattern, a seven-bit type, and
/// mode-flags bits 8 to 15.
pub(crate) fn ro5(sprite_type: u32, flags: u32, wide_mask: bool) -> u32 {
    (u32::from(wide_mask) << 31) | (0xF << 27) | (sprite_type << 20) | (flags << 8) | 1
}

/// Mode-flags bits, as the RISC OS 5 word's own eight-bit field spells them.
pub(crate) const FLAG_RGB_ORDER: u32 = 1 << 6;
pub(crate) const FLAG_ALPHA: u32 = 1 << 7;
const FLAG_FAMILY_MISC: u32 = 1 << 4;

/// Words one row of `width` pixels occupies at `bits`, after `left` wasted
/// bits.
fn row_words(bits: u32, width: u32, left: u32) -> u32 {
    (left + width * bits).div_ceil(32)
}

fn row_stride(bits: u32, width: u32, left: u32) -> usize {
    row_words(bits, width, left) as usize * 4
}

/// Lay `values` out `bits` at a time, least significant pixel leftmost.
fn pack_rows(bits: u32, width: u32, height: u32, left: u32, values: &[u32]) -> Vec<u8> {
    let stride = row_stride(bits, width, left);
    let mut rows = vec![0u8; stride * height as usize];
    for y in 0..height {
        for x in 0..width {
            let value = values[(y * width + x) as usize] & (u32::MAX >> (32 - bits));
            let mut bit = left + x * bits;
            let mut rest = value;
            let mut wrote = 0;
            while wrote < bits {
                let take = (8 - bit % 8).min(bits - wrote);
                rows[y as usize * stride + (bit / 8) as usize] |=
                    u8::try_from((rest & (u32::MAX >> (32 - take))) << (bit % 8) & 0xFF)
                        .expect("a byte's worth of bits");
                rest >>= take;
                bit += take;
                wrote += take;
            }
        }
    }
    rows
}

/// One sprite: its control block's fields, and the payload after them.
pub(crate) struct Sprite {
    mode: u32,
    bits: u32,
    width: u32,
    height: u32,
    left: u32,
    palette: Vec<u8>,
    image: Vec<u8>,
    mask: Option<Vec<u8>>,
    /// Overrides of the fields a malformed-input test is about.
    width_words: Option<u32>,
    last_bit: Option<u32>,
    length: Option<u32>,
}

impl Sprite {
    /// A sprite of flat zero pixels at `bits` deep.
    pub(crate) fn new(mode: u32, bits: u32, width: u32, height: u32) -> Self {
        Self {
            mode,
            bits,
            width,
            height,
            left: 0,
            palette: Vec::new(),
            image: vec![0u8; row_stride(bits, width, 0) * height as usize],
            mask: None,
            width_words: None,
            last_bit: None,
            length: None,
        }
    }

    fn left(mut self, left: u32) -> Self {
        self.left = left;
        self.image = vec![0u8; row_stride(self.bits, self.width, left) * self.height as usize];
        self
    }

    /// Attach a palette in the format's own `&BBGGRR00` entry pairs.
    pub(crate) fn palette(mut self, entries: &[[u8; 3]]) -> Self {
        self.palette = entries
            .iter()
            .flat_map(|&[r, g, b]| [0, r, g, b, 0, r, g, b])
            .collect();
        self
    }

    /// Attach a palette of `bytes` raw bytes, for a length test.
    fn raw_palette(mut self, bytes: Vec<u8>) -> Self {
        self.palette = bytes;
        self
    }

    pub(crate) fn pixels(mut self, values: &[u32]) -> Self {
        self.image = pack_rows(self.bits, self.width, self.height, self.left, values);
        self
    }

    /// A new-format mask: one bit per pixel, from bit zero of its own rows.
    pub(crate) fn mask_bits(mut self, opaque: &[bool]) -> Self {
        let values: Vec<u32> = opaque.iter().map(|&on| u32::from(on)).collect();
        self.mask = Some(pack_rows(1, self.width, self.height, 0, &values));
        self
    }

    /// A wide mask: eight bits of alpha per pixel.
    pub(crate) fn mask_alpha(mut self, alpha: &[u8]) -> Self {
        let values: Vec<u32> = alpha.iter().map(|&a| u32::from(a)).collect();
        self.mask = Some(pack_rows(8, self.width, self.height, 0, &values));
        self
    }

    /// An old-format mask: the image's own depth and row layout, every bit
    /// of a pixel set or clear together.
    pub(crate) fn mask_image(mut self, opaque: &[bool]) -> Self {
        let solid = u32::MAX >> (32 - self.bits);
        let values: Vec<u32> = opaque
            .iter()
            .map(|&on| if on { solid } else { 0 })
            .collect();
        self.mask = Some(pack_rows(
            self.bits,
            self.width,
            self.height,
            self.left,
            &values,
        ));
        self
    }

    fn raw_image(mut self, bytes: Vec<u8>) -> Self {
        self.image = bytes;
        self
    }

    fn raw_mask(mut self, bytes: Vec<u8>) -> Self {
        self.mask = Some(bytes);
        self
    }

    fn width_words(mut self, words: u32) -> Self {
        self.width_words = Some(words);
        self
    }

    fn last_bit(mut self, last: u32) -> Self {
        self.last_bit = Some(last);
        self
    }

    pub(crate) fn length(mut self, length: u32) -> Self {
        self.length = Some(length);
        self
    }

    /// This sprite's control block and payload.
    pub(crate) fn bytes(&self) -> Vec<u8> {
        let len = |bytes: &[u8]| u32::try_from(bytes.len()).expect("a small fixture");
        let image_at = 44 + len(&self.palette);
        let mask_at = image_at + len(&self.image);
        let total = mask_at + self.mask.as_deref().map_or(0, len);
        let mut out = Vec::new();
        out.extend_from_slice(&self.length.unwrap_or(total).to_le_bytes());
        out.extend_from_slice(b"fixture\0\0\0\0\0");
        let words = self
            .width_words
            .unwrap_or_else(|| row_words(self.bits, self.width, self.left));
        out.extend_from_slice(&(words - 1).to_le_bytes());
        out.extend_from_slice(&(self.height - 1).to_le_bytes());
        out.extend_from_slice(&self.left.to_le_bytes());
        out.extend_from_slice(
            &self
                .last_bit
                .unwrap_or((self.left + self.width * self.bits - 1) % 32)
                .to_le_bytes(),
        );
        out.extend_from_slice(&image_at.to_le_bytes());
        out.extend_from_slice(
            &if self.mask.is_some() {
                mask_at
            } else {
                image_at
            }
            .to_le_bytes(),
        );
        out.extend_from_slice(&self.mode.to_le_bytes());
        out.extend_from_slice(&self.palette);
        out.extend_from_slice(&self.image);
        if let Some(mask) = &self.mask {
            out.extend_from_slice(mask);
        }
        out
    }
}

/// Lay a set of sprites out as a sprite area file.
pub(crate) fn area(sprites: &[Sprite]) -> Vec<u8> {
    let bodies: Vec<Vec<u8>> = sprites.iter().map(Sprite::bytes).collect();
    let total: usize = 12 + bodies.iter().map(Vec::len).sum::<usize>();
    let mut out = Vec::new();
    out.extend_from_slice(
        &u32::try_from(sprites.len())
            .expect("a small count")
            .to_le_bytes(),
    );
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&(u32::try_from(total).expect("a small file") + 4).to_le_bytes());
    for body in bodies {
        out.extend_from_slice(&body);
    }
    out
}

pub(crate) fn one(sprite: Sprite) -> Vec<u8> {
    area(&[sprite])
}

/// Decode a one-sprite file and hand back its pixels as RGBA quads.
fn rgba(bytes: &[u8]) -> Vec<[u8; 4]> {
    quads(&decode_as(ImageFormat::Sprite, bytes, &limits()).expect("the fixture decodes"))
}

fn quads(image: &RasterImage) -> Vec<[u8; 4]> {
    image.pixels().as_chunks::<4>().0.to_vec()
}

fn refusal(bytes: &[u8]) -> DecodeError {
    decode_as(ImageFormat::Sprite, bytes, &limits()).expect_err("the fixture is refused")
}

const BLACK: [u8; 4] = [0, 0, 0, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];
const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];

/// The desktop's sixteen standard colours in Wimp colour order: what a
/// palette-less sprite's pixels mean, written out here rather than read back
/// from the decoder.
const WIMP: [[u8; 4]; 16] = [
    [0xFF, 0xFF, 0xFF, 255],
    [0xDD, 0xDD, 0xDD, 255],
    [0xBB, 0xBB, 0xBB, 255],
    [0x99, 0x99, 0x99, 255],
    [0x77, 0x77, 0x77, 255],
    [0x55, 0x55, 0x55, 255],
    [0x33, 0x33, 0x33, 255],
    [0x00, 0x00, 0x00, 255],
    [0x00, 0x44, 0x99, 255],
    [0xEE, 0xEE, 0x00, 255],
    [0x00, 0xCC, 0x00, 255],
    [0xDD, 0x00, 0x00, 255],
    [0xEE, 0xEE, 0xBB, 255],
    [0x55, 0x88, 0x00, 255],
    [0xFF, 0xBB, 0x00, 255],
    [0x00, 0xBB, 0xFF, 255],
];

// ---------------------------------------------------------------- the area

#[test]
fn a_sprite_area_is_never_recognised_from_its_content() {
    // The first word is a sprite count, so there is no signature to find and
    // nothing may be guessed from one.
    let bytes = one(Sprite::new(MODE_2, 1, 8, 1));
    assert_eq!(sniff(&bytes), None);
    assert_eq!(
        decode(&bytes, &limits()),
        Err(DecodeError::UnknownFormat),
        "a sniffing decode must not reach a format it cannot recognise"
    );
    assert_eq!(
        Sequence::open(&bytes, &limits()).err(),
        Some(DecodeError::UnknownFormat)
    );
    assert!(decode_as(ImageFormat::Sprite, &bytes, &limits()).is_ok());
}

#[test]
fn an_empty_input_is_refused() {
    assert_eq!(refusal(&[]), DecodeError::SpriteTruncated);
}

#[test]
fn an_area_declaring_no_sprites_is_refused() {
    let mut bytes = one(Sprite::new(MODE_2, 1, 8, 1));
    bytes[0..4].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(refusal(&bytes), DecodeError::SpriteNoSprites);
}

#[test]
fn a_first_sprite_offset_inside_the_header_is_refused() {
    let mut bytes = one(Sprite::new(MODE_2, 1, 8, 1));
    bytes[4..8].copy_from_slice(&12u32.to_le_bytes());
    assert_eq!(refusal(&bytes), DecodeError::SpriteBadArea);
}

#[test]
fn an_offset_below_the_files_own_bias_is_refused() {
    let mut bytes = one(Sprite::new(MODE_2, 1, 8, 1));
    bytes[4..8].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(refusal(&bytes), DecodeError::SpriteBadArea);
}

#[test]
fn a_free_word_offset_past_the_input_is_refused() {
    let mut bytes = one(Sprite::new(MODE_2, 1, 8, 1));
    let past = u32::try_from(bytes.len()).expect("a small file") + 64;
    bytes[8..12].copy_from_slice(&past.to_le_bytes());
    assert_eq!(refusal(&bytes), DecodeError::SpriteBadArea);
}

#[test]
fn sprites_starting_after_the_area_ends_are_refused() {
    let mut bytes = one(Sprite::new(MODE_2, 1, 8, 1));
    bytes[8..12].copy_from_slice(&16u32.to_le_bytes());
    assert_eq!(refusal(&bytes), DecodeError::SpriteBadArea);
}

#[test]
fn bytes_after_the_area_end_are_ignored() {
    let mut bytes = one(Sprite::new(MODE_2, 1, 8, 1).pixels(&[1, 0, 1, 0, 1, 0, 1, 0]));
    bytes.extend_from_slice(b"trailing junk the area does not claim");
    assert_eq!(rgba(&bytes)[0], WIMP[7]);
}

#[test]
fn a_control_block_running_past_the_input_is_refused() {
    let mut bytes = one(Sprite::new(MODE_2, 1, 8, 1));
    bytes.truncate(30);
    // The area header still describes a whole file, so the truncation is
    // only found when the control block is read.
    assert!(matches!(
        refusal(&bytes),
        DecodeError::SpriteBadArea | DecodeError::SpriteTruncated
    ));
}

#[test]
fn a_sprite_shorter_than_a_control_block_is_refused() {
    let bytes = one(Sprite::new(MODE_2, 1, 8, 1).length(43));
    assert_eq!(refusal(&bytes), DecodeError::SpriteBadArea);
}

#[test]
fn a_sprite_reaching_past_the_area_end_is_refused() {
    let bytes = one(Sprite::new(MODE_2, 1, 8, 1).length(4096));
    assert_eq!(refusal(&bytes), DecodeError::SpriteBadArea);
}

#[test]
fn a_declared_count_the_chain_cannot_satisfy_is_refused() {
    let mut bytes = one(Sprite::new(MODE_2, 1, 8, 1));
    bytes[0..4].copy_from_slice(&8u32.to_le_bytes());
    assert_eq!(refusal(&bytes), DecodeError::SpriteBadArea);
}

// ----------------------------------------------------------- numbered modes

#[test]
fn a_two_and_a_four_colour_sprite_are_shown_as_the_wimp_plots_them() {
    // Two colours are Wimp colours 0 and 7; four are 0, 2, 4 and 7.
    assert_eq!(
        rgba(&one(Sprite::new(MODE_2, 1, 2, 1).pixels(&[0, 1]))),
        [WIMP[0], WIMP[7]]
    );
    assert_eq!(
        rgba(&one(Sprite::new(MODE_4, 2, 4, 1).pixels(&[0, 1, 2, 3]))),
        [WIMP[0], WIMP[2], WIMP[4], WIMP[7]]
    );
}

#[test]
fn a_sixteen_colour_sprite_is_the_wimp_palette_not_a_pc_one() {
    let indices: Vec<u32> = (0..16).collect();
    let decoded = rgba(&one(Sprite::new(MODE_16, 4, 16, 1).pixels(&indices)));
    assert_eq!(decoded, WIMP);
    // Neither the EGA order nor the BBC mode-entry colours.
    assert_ne!(decoded[1], [0x00, 0x00, 0xAA, 255]);
    assert_ne!(decoded[1], RED);
}

#[test]
fn the_desktop_palette_is_the_one_every_depth_decodes_against() {
    for depth in IndexDepth::ALL {
        let colours = desktop_palette(depth);
        assert_eq!(colours.len(), depth.colours());
        let mode = [MODE_2, MODE_4, MODE_16, MODE_256][depth.bits().trailing_zeros() as usize];
        let count = u32::try_from(depth.colours()).expect("at most 256");
        let indices: Vec<u32> = (0..count).collect();
        let decoded = rgba(&one(
            Sprite::new(mode, depth.bits(), count, 1).pixels(&indices)
        ));
        for (pixel, &[r, g, b]) in decoded.iter().zip(colours) {
            assert_eq!(*pixel, [r, g, b, 255]);
        }
    }
}

#[test]
fn a_teletext_mode_is_refused() {
    assert_eq!(
        refusal(&one(Sprite::new(7, 4, 4, 1))),
        DecodeError::SpriteUnknownMode
    );
}

#[test]
fn an_extension_mode_number_is_refused() {
    assert_eq!(
        refusal(&one(Sprite::new(54, 4, 4, 1))),
        DecodeError::SpriteUnknownMode
    );
}

#[test]
fn a_mode_number_with_the_shadow_bit_is_refused() {
    // Numbers 128 to 255 are a shadow-bank spelling, never a sprite's mode.
    assert_eq!(
        refusal(&one(Sprite::new(128 + 15, 8, 4, 1))),
        DecodeError::SpriteUnknownMode
    );
}

// --------------------------------------------------------- sprite mode words

#[test]
fn a_mode_selector_pointer_is_refused() {
    // Bit 0 clear above 255 is a pointer to a mode selector block, which no
    // file can hold.
    assert_eq!(
        refusal(&one(Sprite::new(0x0001_0000, 8, 4, 1))),
        DecodeError::SpriteInvalidModeWord
    );
}

#[test]
fn a_thirty_five_word_with_a_zero_dpi_field_is_refused() {
    let no_vertical = (4 << 27) | (90 << 1) | 1;
    let no_horizontal = (4 << 27) | (90 << 14) | 1;
    assert_eq!(
        refusal(&one(Sprite::new(no_vertical, 8, 4, 1))),
        DecodeError::SpriteInvalidModeWord
    );
    assert_eq!(
        refusal(&one(Sprite::new(no_horizontal, 8, 4, 1))),
        DecodeError::SpriteInvalidModeWord
    );
}

#[test]
fn the_indexed_sprite_types_carry_their_depths() {
    for (sprite_type, bits) in [(1, 1), (2, 2), (3, 4), (4, 8)] {
        let sprite = Sprite::new(ro35(sprite_type, false), bits, 2, 1).pixels(&[0, 1]);
        let decoded = rgba(&one(sprite));
        assert_eq!(
            decoded.len(),
            2,
            "type {sprite_type} decoded the wrong width"
        );
        // Index 0 is the desktop's white below eight bits, and the screen
        // byte's own black at eight.
        let expected = if bits == 8 { BLACK } else { WIMP[0] };
        assert_eq!(decoded[0], expected, "type {sprite_type} index 0");
    }
}

#[test]
fn the_colour_space_sprite_types_are_refused_by_name() {
    for sprite_type in [7, 9] {
        assert_eq!(
            refusal(&one(Sprite::new(ro35(sprite_type, false), 32, 2, 1))),
            DecodeError::SpriteUnsupportedType,
            "type {sprite_type}"
        );
    }
    for sprite_type in [17, 18] {
        assert_eq!(
            refusal(&one(Sprite::new(ro5(sprite_type, 0, false), 32, 2, 1))),
            DecodeError::SpriteUnsupportedType,
            "type {sprite_type}"
        );
    }
}

#[test]
fn the_reserved_sprite_types_are_refused() {
    for sprite_type in [0, 11, 12, 13, 14] {
        assert_eq!(
            refusal(&one(Sprite::new(ro35(sprite_type, false), 32, 2, 1))),
            DecodeError::SpriteUnsupportedType,
            "type {sprite_type}"
        );
    }
    assert_eq!(
        refusal(&one(Sprite::new(ro5(127, 0, false), 32, 2, 1))),
        DecodeError::SpriteUnsupportedType
    );
    // Type 15 has no 3.5 spelling: bits 27-30 all set is what marks a RISC
    // OS 5 word, so such a word is read as one and refused for its pattern.
    assert_eq!(
        refusal(&one(Sprite::new(ro35(15, false), 32, 2, 1))),
        DecodeError::SpriteInvalidModeWord
    );
}

#[test]
fn a_five_word_with_a_broken_fixed_pattern_is_refused() {
    // Bits 16-19 must be zero; a value there is what tells a RISC OS 5 word
    // from a 3.5 one whose type happens to be 15.
    let broken = ro5(6, 0, false) | (1 << 17);
    assert_eq!(
        refusal(&one(Sprite::new(broken, 32, 2, 1))),
        DecodeError::SpriteInvalidModeWord
    );
    assert_eq!(
        refusal(&one(Sprite::new(ro5(6, 0, false) | (1 << 2), 32, 2, 1))),
        DecodeError::SpriteInvalidModeWord
    );
}

#[test]
fn a_five_word_selects_the_four_bit_per_channel_type() {
    let value = 0xF00F;
    let decoded = rgba(&one(
        Sprite::new(ro5(16, FLAG_ALPHA, false), 16, 1, 1).pixels(&[value])
    ));
    assert_eq!(decoded, [[255, 0, 0, 255]]);
}

#[test]
fn a_non_rgb_colour_family_is_refused() {
    assert_eq!(
        refusal(&one(Sprite::new(ro5(6, FLAG_FAMILY_MISC, false), 32, 1, 1))),
        DecodeError::SpriteUnsupportedType
    );
}

#[test]
fn the_order_flag_swaps_red_and_blue() {
    // A 32-bit pixel lands in memory lowest field first, so the default TBGR
    // order reads red from the first byte and TRGB reads blue from it.
    let pixel = 0x0000_00FF;
    assert_eq!(
        rgba(&one(
            Sprite::new(ro5(6, 0, false), 32, 1, 1).pixels(&[pixel])
        )),
        [RED]
    );
    assert_eq!(
        rgba(&one(
            Sprite::new(ro5(6, FLAG_RGB_ORDER, false), 32, 1, 1).pixels(&[pixel])
        )),
        [[0, 0, 255, 255]]
    );
}

#[test]
fn the_alpha_flag_reads_the_top_channel_as_alpha() {
    let half = 0x8000_0000 | 0x0000_00FF;
    assert_eq!(
        rgba(&one(Sprite::new(ro5(6, 0, false), 32, 1, 1).pixels(&[half]))),
        [RED],
        "an unused top byte leaves the pixel opaque"
    );
    assert_eq!(
        rgba(&one(
            Sprite::new(ro5(6, FLAG_ALPHA, false), 32, 1, 1).pixels(&[half])
        )),
        [[255, 0, 0, 0x80]]
    );
}

#[test]
fn asking_for_alpha_a_format_has_no_room_for_is_refused() {
    for sprite_type in [8, 10] {
        assert_eq!(
            refusal(&one(Sprite::new(
                ro5(sprite_type, FLAG_ALPHA, false),
                if sprite_type == 8 { 24 } else { 16 },
                1,
                1
            ))),
            DecodeError::SpriteInvalidModeWord,
            "type {sprite_type}"
        );
    }
}

// ------------------------------------------------------------ packed depths

#[test]
fn a_sixteen_bit_pixel_is_five_five_five() {
    let decoded = rgba(&one(
        Sprite::new(ro35(5, false), 16, 3, 1).pixels(&[0x001F, 0x03E0, 0x7C00])
    ));
    assert_eq!(decoded, [RED, GREEN, [0, 0, 255, 255]]);
}

#[test]
fn the_five_six_five_type_widens_its_six_bit_green() {
    let decoded = rgba(&one(
        Sprite::new(ro35(10, false), 16, 3, 1).pixels(&[0x001F, 0x07E0, 0xF800])
    ));
    assert_eq!(decoded, [RED, GREEN, [0, 0, 255, 255]]);
}

#[test]
fn a_twenty_four_bit_pixel_is_three_whole_bytes() {
    let decoded = rgba(&one(
        Sprite::new(ro35(8, false), 24, 2, 1).pixels(&[0x0000_00FF, 0x00FF_8040])
    ));
    assert_eq!(decoded, [RED, [0x40, 0x80, 0xFF, 255]]);
}

#[test]
fn a_thirty_two_bit_pixels_top_byte_is_unused_without_the_alpha_flag() {
    let decoded = rgba(&one(
        Sprite::new(ro35(6, false), 32, 2, 1).pixels(&[0xFF00_00FF, 0x0000_FF00])
    ));
    assert_eq!(decoded, [RED, GREEN]);
}

#[test]
fn indexed_pixels_run_least_significant_first() {
    // The leftmost pixel of a row is the low bits of its first word, which
    // is the opposite of every other format here.
    let bytes = one(Sprite::new(MODE_16, 4, 2, 1).pixels(&[15, 0]));
    let decoded = decode_as(ImageFormat::Sprite, &bytes, &limits()).expect("decodes");
    assert_eq!(quads(&decoded), [WIMP[15], WIMP[0]]);
}

// ------------------------------------------------------------------ palettes

#[test]
fn an_attached_palette_supersedes_the_default() {
    let sprite = Sprite::new(MODE_4, 2, 4, 1)
        .palette(&[[1, 2, 3], [4, 5, 6], [7, 8, 9], [10, 11, 12]])
        .pixels(&[0, 1, 2, 3]);
    assert_eq!(
        rgba(&one(sprite)),
        [
            [1, 2, 3, 255],
            [4, 5, 6, 255],
            [7, 8, 9, 255],
            [10, 11, 12, 255]
        ]
    );
}

#[test]
fn a_full_256_entry_palette_is_read_straight_through() {
    let entries: Vec<[u8; 3]> = (0..256)
        .map(|i| [u8::try_from(i).expect("in range"), 0, 0])
        .collect();
    let sprite = Sprite::new(MODE_256, 8, 3, 1)
        .palette(&entries)
        .pixels(&[0, 128, 255]);
    assert_eq!(
        rgba(&one(sprite)),
        [[0, 0, 0, 255], [128, 0, 0, 255], [255, 0, 0, 255]]
    );
}

#[test]
fn a_sixteen_entry_palette_at_eight_bits_is_the_vidc_arrangement() {
    // The low four bits select the entry; the top four override red bit 3,
    // green bits 2 and 3, and blue bit 3.
    let mut entries = [[0u8, 0, 0]; 16];
    entries[1] = [0x00, 0x00, 0x00];
    let sprite = Sprite::new(MODE_256, 8, 3, 1)
        .palette(&entries)
        .pixels(&[0x01, 0x11, 0x81]);
    let decoded = rgba(&one(sprite));
    assert_eq!(decoded[0], [0, 0, 0, 255], "no supremacy bits set");
    assert_eq!(decoded[1], [0x88, 0, 0, 255], "red bit 3 from pixel bit 4");
    assert_eq!(decoded[2], [0, 0, 0x88, 255], "blue bit 3 from pixel bit 7");
}

#[test]
fn a_sixty_four_entry_palette_uses_its_last_sixteen() {
    // VIDC holds sixteen registers, so a longer-but-still-short palette
    // hands it the last sixteen entries.
    let mut entries = [[0u8, 0, 0]; 64];
    entries[48] = [0x70, 0x30, 0x70];
    let sprite = Sprite::new(MODE_256, 8, 1, 1)
        .palette(&entries)
        .pixels(&[0x00]);
    assert_eq!(rgba(&one(sprite)), [[0x77, 0x33, 0x77, 255]]);
}

#[test]
fn the_eight_bit_default_is_the_screen_bytes_own_arrangement() {
    let sprite = Sprite::new(MODE_256, 8, 5, 1).pixels(&[0x00, 0xFF, 0x03, 0x60, 0x10]);
    assert_eq!(
        rgba(&one(sprite)),
        [
            BLACK,
            WHITE,
            [0x33, 0x33, 0x33, 255],
            [0x00, 0xCC, 0x00, 255],
            [0x88, 0x00, 0x00, 255],
        ]
    );
}

#[test]
fn a_palette_that_is_not_whole_entries_is_ignored() {
    // The default colours stand rather than a part-read entry being invented.
    let sprite = Sprite::new(MODE_4, 2, 2, 1)
        .raw_palette(vec![9; 12])
        .pixels(&[0, 1]);
    assert_eq!(rgba(&one(sprite)), [WIMP[0], WIMP[2]]);
}

#[test]
fn a_deep_sprites_palette_does_not_colour_its_pixels() {
    let sprite = Sprite::new(ro35(6, false), 32, 1, 1)
        .palette(&[[9, 9, 9]; 16])
        .pixels(&[0x0000_00FF]);
    assert_eq!(rgba(&one(sprite)), [RED]);
}

// ------------------------------------------------------------------ wastage

#[test]
fn left_wastage_skips_the_bits_before_the_first_pixel() {
    // Four wasted bits at 4bpp is one pixel's worth, so the row's own first
    // pixel is the second field in the word.
    let sprite = Sprite::new(MODE_16, 4, 2, 1).left(4).pixels(&[15, 2]);
    assert_eq!(rgba(&one(sprite)), [WIMP[15], WIMP[2]]);
}

#[test]
fn left_wastage_is_refused_on_a_new_format_sprite() {
    let sprite = Sprite::new(ro35(3, false), 4, 2, 1).left(4);
    assert_eq!(refusal(&one(sprite)), DecodeError::SpriteInvalidWastage);
}

#[test]
fn a_used_bit_field_past_a_word_is_refused() {
    assert_eq!(
        refusal(&one(Sprite::new(MODE_16, 4, 2, 1).last_bit(32))),
        DecodeError::SpriteInvalidWastage
    );
    let mut bytes = one(Sprite::new(MODE_16, 4, 2, 1));
    bytes[12 + 24..12 + 28].copy_from_slice(&40u32.to_le_bytes());
    assert_eq!(refusal(&bytes), DecodeError::SpriteInvalidWastage);
}

#[test]
fn wastage_off_a_pixel_boundary_is_refused() {
    let mut bytes = one(Sprite::new(MODE_16, 4, 2, 1));
    bytes[12 + 24..12 + 28].copy_from_slice(&2u32.to_le_bytes());
    assert_eq!(refusal(&bytes), DecodeError::SpriteInvalidWastage);
}

#[test]
fn a_row_holding_no_whole_pixels_is_refused() {
    // One word wide with the first pixel starting after the last used bit.
    let sprite = Sprite::new(MODE_16, 4, 2, 1).left(12).last_bit(11);
    assert_eq!(refusal(&one(sprite)), DecodeError::SpriteInvalidWastage);
}

#[test]
fn a_row_that_is_not_a_whole_number_of_pixels_is_refused() {
    // Seven bits used at four bits a pixel names no whole number of them.
    let sprite = Sprite::new(MODE_16, 4, 2, 1).width_words(1).last_bit(6);
    assert_eq!(refusal(&one(sprite)), DecodeError::SpriteInvalidWastage);
}

#[test]
fn an_image_shorter_than_its_declared_rows_is_refused() {
    let sprite = Sprite::new(MODE_16, 4, 4, 4).raw_image(vec![0; 4]);
    assert_eq!(refusal(&one(sprite)), DecodeError::SpriteTruncated);
}

// -------------------------------------------------------------------- masks

#[test]
fn a_sprite_with_no_mask_is_wholly_opaque() {
    let decoded = rgba(&one(Sprite::new(MODE_16, 4, 2, 1).pixels(&[15, 0])));
    assert!(decoded.iter().all(|pixel| pixel[3] == 255));
}

#[test]
fn an_old_format_mask_is_read_at_the_images_own_depth() {
    let sprite = Sprite::new(MODE_16, 4, 4, 1)
        .pixels(&[15, 15, 15, 15])
        .mask_image(&[true, false, true, false]);
    let decoded = rgba(&one(sprite));
    assert_eq!(
        decoded.iter().map(|pixel| pixel[3]).collect::<Vec<_>>(),
        [255, 0, 255, 0]
    );
}

#[test]
fn an_old_format_mask_honours_the_images_wastage() {
    let sprite = Sprite::new(MODE_16, 4, 2, 1)
        .left(8)
        .pixels(&[15, 15])
        .mask_image(&[false, true]);
    let decoded = rgba(&one(sprite));
    assert_eq!(
        decoded.iter().map(|pixel| pixel[3]).collect::<Vec<_>>(),
        [0, 255]
    );
}

#[test]
fn any_set_bit_of_an_old_format_mask_pixel_means_solid() {
    // The format asks for every bit of a mask pixel to agree; one that does
    // not is still a plotted pixel rather than a refusal.
    let sprite = Sprite::new(MODE_16, 4, 2, 1)
        .pixels(&[15, 15])
        .raw_mask(vec![0x01, 0x00, 0x00, 0x00]);
    let decoded = rgba(&one(sprite));
    assert_eq!(
        decoded.iter().map(|pixel| pixel[3]).collect::<Vec<_>>(),
        [255, 0]
    );
}

#[test]
fn a_new_format_mask_is_one_bit_per_pixel() {
    let sprite = Sprite::new(ro35(3, false), 4, 4, 1)
        .pixels(&[15, 15, 15, 15])
        .mask_bits(&[true, false, false, true]);
    let decoded = rgba(&one(sprite));
    assert_eq!(
        decoded.iter().map(|pixel| pixel[3]).collect::<Vec<_>>(),
        [255, 0, 0, 255]
    );
}

#[test]
fn a_wide_mask_is_eight_bits_of_alpha() {
    let sprite = Sprite::new(ro35(3, true), 4, 3, 1)
        .pixels(&[15, 15, 15])
        .mask_alpha(&[0, 0x40, 0xFF]);
    let decoded = rgba(&one(sprite));
    assert_eq!(
        decoded.iter().map(|pixel| pixel[3]).collect::<Vec<_>>(),
        [0, 0x40, 0xFF]
    );
}

#[test]
fn a_mask_supersedes_a_pixels_own_alpha() {
    let sprite = Sprite::new(ro5(6, FLAG_ALPHA, true), 32, 2, 1)
        .pixels(&[0xFF00_00FF, 0xFF00_00FF])
        .mask_alpha(&[0x20, 0xFF]);
    let decoded = rgba(&one(sprite));
    assert_eq!(
        decoded.iter().map(|pixel| pixel[3]).collect::<Vec<_>>(),
        [0x20, 0xFF]
    );
}

#[test]
fn a_truncated_mask_is_refused() {
    let sprite = Sprite::new(ro35(3, false), 4, 4, 2)
        .pixels(&[15; 8])
        .raw_mask(vec![0xFF]);
    assert_eq!(refusal(&one(sprite)), DecodeError::SpriteTruncated);
}

// ------------------------------------------------------------- the sequence

#[test]
fn an_area_of_several_sprites_is_a_page_container() {
    let bytes = area(&[
        Sprite::new(MODE_2, 1, 2, 1).pixels(&[0, 1]),
        Sprite::new(MODE_16, 4, 4, 2).pixels(&[15; 8]),
        Sprite::new(MODE_4, 2, 1, 1).pixels(&[1]),
    ]);
    let mut sequence =
        Sequence::open_as(ImageFormat::Sprite, &bytes, &limits()).expect("the area opens");
    let info = sequence.info();
    assert_eq!(info.format(), ImageFormat::Sprite);
    assert_eq!(info.count(), 3);
    assert_eq!(info.kind(), SequenceKind::Pages);
    // A page container's geometry is its largest page's.
    assert_eq!((info.width(), info.height()), (4, 2));

    let mut seen = Vec::new();
    while let Some(frame) = sequence.next_frame().expect("each page decodes") {
        seen.push((
            frame.index(),
            frame.width(),
            frame.height(),
            frame.delay_ns(),
        ));
    }
    assert_eq!(seen, [(0, 2, 1, 0), (1, 4, 2, 0), (2, 1, 1, 0)]);
}

#[test]
fn a_page_is_addressed_directly_and_the_walk_restarts() {
    let bytes = area(&[
        Sprite::new(MODE_2, 1, 2, 1).pixels(&[0, 1]),
        Sprite::new(MODE_16, 4, 4, 1).pixels(&[15, 0, 15, 0]),
    ]);
    let mut sequence =
        Sequence::open_as(ImageFormat::Sprite, &bytes, &limits()).expect("the area opens");
    let page = sequence
        .page(1)
        .expect("the page decodes")
        .expect("present");
    assert_eq!((page.index(), page.width()), (1, 4));
    // Addressing a page backwards re-walks the chain from its start.
    let first = sequence
        .page(0)
        .expect("the page decodes")
        .expect("present");
    assert_eq!(first.pixels().as_chunks::<4>().0, [WIMP[0], WIMP[7]]);
    assert!(sequence.page(2).expect("no such page").is_none());

    sequence.rewind();
    assert_eq!(
        sequence
            .next_frame()
            .expect("decodes")
            .expect("present")
            .index(),
        0
    );
}

#[test]
fn one_unsupported_sprite_does_not_refuse_the_others() {
    let bytes = area(&[
        Sprite::new(MODE_16, 4, 4, 1).pixels(&[15, 0, 15, 0]),
        Sprite::new(ro35(7, false), 32, 2, 1),
    ]);
    let mut sequence =
        Sequence::open_as(ImageFormat::Sprite, &bytes, &limits()).expect("the area opens");
    assert_eq!(sequence.info().count(), 2);
    assert!(sequence.next_frame().expect("the first decodes").is_some());
    assert_eq!(
        sequence.next_frame().expect_err("the second is refused"),
        DecodeError::SpriteUnsupportedType
    );
    // A refusal is remembered until the walk restarts.
    assert_eq!(
        sequence.next_frame().expect_err("still refused"),
        DecodeError::SpriteUnsupportedType
    );
    sequence.rewind();
    assert!(sequence.next_frame().expect("the first decodes").is_some());
}

#[test]
fn an_area_whose_every_sprite_is_unsupported_names_a_reason() {
    let bytes = area(&[
        Sprite::new(ro35(7, false), 32, 2, 1),
        Sprite::new(7, 4, 2, 1),
    ]);
    assert_eq!(refusal(&bytes), DecodeError::SpriteUnsupportedType);
}

#[test]
fn a_probe_and_a_decode_both_answer_the_largest_sprite() {
    let bytes = area(&[
        Sprite::new(MODE_2, 1, 2, 1).pixels(&[0, 1]),
        Sprite::new(MODE_16, 4, 4, 3).pixels(&[15; 12]),
        Sprite::new(MODE_4, 2, 1, 1).pixels(&[1]),
    ]);
    let info = probe_as(ImageFormat::Sprite, &bytes).expect("the header probes");
    assert_eq!(info.format(), ImageFormat::Sprite);
    assert_eq!((info.width(), info.height()), (4, 3));
    let decoded = decode_as(ImageFormat::Sprite, &bytes, &limits()).expect("decodes");
    assert_eq!((decoded.width(), decoded.height()), (4, 3));
}

#[test]
fn a_probe_allocates_nothing_from_a_geometry_it_reports() {
    // Probing applies no limits of its own, so a caller learns the size it
    // would have to afford before anything is reserved for it.
    let bytes = one(Sprite::new(MODE_16, 4, 8, 4).pixels(&[0; 32]));
    let info = probe_as(ImageFormat::Sprite, &bytes).expect("probes");
    assert_eq!((info.width(), info.height()), (8, 4));
    let tight = DecodeLimits::new(4, 4, 16, 0);
    assert_eq!(
        decode_as(ImageFormat::Sprite, &bytes, &tight),
        Err(DecodeError::WidthExceedsLimit)
    );
}

#[test]
fn a_page_is_weighed_against_the_limits_when_it_is_asked_for() {
    // A page container weighs nothing when it opens, so a small page out of
    // a file whose largest the caller could never afford still decodes.
    let bytes = area(&[
        Sprite::new(MODE_16, 4, 2, 1).pixels(&[15, 0]),
        Sprite::new(MODE_16, 4, 64, 4).pixels(&[0; 256]),
    ]);
    let tight = DecodeLimits::new(8, 8, 64, 0);
    let mut sequence =
        Sequence::open_as(ImageFormat::Sprite, &bytes, &tight).expect("the area still opens");
    assert!(sequence.page(0).expect("the small page decodes").is_some());
    assert_eq!(
        sequence.page(1).expect_err("the large page is refused"),
        DecodeError::WidthExceedsLimit
    );
}

#[test]
fn naming_the_wrong_format_is_refused_rather_than_misread() {
    let bytes = one(Sprite::new(MODE_2, 1, 8, 1));
    assert_eq!(
        decode_as(ImageFormat::Png, &bytes, &limits()),
        Err(DecodeError::BadSignature)
    );
    assert_eq!(
        probe_as(ImageFormat::Gif, &bytes).err(),
        Some(DecodeError::GifBadSignature)
    );
}

// ------------------------------------------------------------ the sprite model

/// The one sprite of `bytes`, read natively.
fn native(bytes: &[u8]) -> SpriteEntry {
    let mut reader = SpriteAreaReader::open(bytes, &limits()).expect("the area opens");
    reader
        .sprite(0)
        .expect("no machine refusal")
        .expect("one sprite")
}

fn picture(entry: SpriteEntry) -> crate::Sprite {
    match entry {
        SpriteEntry::Picture(sprite) => sprite,
        SpriteEntry::Opaque(kept) => panic!("kept as bytes: {:?}", kept.reason),
    }
}

#[test]
fn a_name_ends_at_its_first_control_character() {
    let sprite = picture(native(&one(Sprite::new(MODE_2, 1, 8, 1))));
    assert_eq!(sprite.name.as_bytes(), b"fixture");
    assert_eq!(alloc::format!("{}", sprite.name), "fixture");
}

#[test]
fn a_new_name_is_lower_cased_and_held_to_the_control_blocks_field() {
    assert_eq!(
        SpriteName::new("MyIcon").expect("valid").as_bytes(),
        b"myicon"
    );
    assert!(SpriteName::new("").is_none());
    assert!(SpriteName::new("thirteenchars").is_none());
    assert!(SpriteName::new("has space").is_none());
    assert!(SpriteName::new("caf\u{e9}").is_none());
    assert!(SpriteName::new("twelve_chars").is_some());
}

#[test]
fn names_match_as_risc_os_matches_them_regardless_of_case() {
    let lower = SpriteName::new("icon").expect("valid");
    let read = picture(native(&one(Sprite::new(MODE_2, 1, 8, 1)))).name;
    assert!(!lower.matches(&read));
    assert!(lower.matches(&SpriteName::new("ICON").expect("valid")));
}

#[test]
fn a_numbered_modes_pixel_shape_comes_from_its_eigen_factors() {
    let shape = |mode| SpriteMode::from_value(mode).expect("a mode").pixel_aspect();
    assert_eq!(shape(12), (1, 2), "mode 12 pixels are twice as tall");
    assert_eq!(shape(27), (1, 1));
    assert_eq!(shape(49), (2, 1), "mode 49 pixels are twice as wide");
    assert_eq!(shape(2), (2, 1), "mode 2's eight-unit pixels over four");
    assert_eq!(SpriteMode::from_value(12).expect("a mode").eig(), (1, 2));
    // Mode 22's 768 by 288 pixels cover 768 by 576 OS units.
    let large = SpriteMode::from_value(22).expect("a mode");
    assert_eq!(large.eig(), (0, 1));
    assert_eq!(
        large.with_alpha_mask(true).eig(),
        (0, 1),
        "a mask keeps the shape"
    );
}

/// A mode made for a new sprite is exactly the mode its word reads back as,
/// so what is written is what a reader sees.
#[test]
fn a_new_sprites_mode_reads_back_as_itself() {
    let depths = [
        IndexDepth::One,
        IndexDepth::Two,
        IndexDepth::Four,
        IndexDepth::Eight,
    ];
    for eig in [
        (0, 0),
        (0, 1),
        (1, 1),
        (1, 2),
        (2, 1),
        (3, 2),
        (4, 4),
        (9, 9),
    ] {
        for alpha in [false, true] {
            for depth in depths {
                let mode = SpriteMode::indexed(depth, eig, alpha);
                let read = SpriteMode::from_value(mode.value());
                assert_eq!(read, Some(mode), "{depth:?} {eig:?} {alpha}");
            }
            let mode = SpriteMode::truecolour(eig, alpha);
            let read = SpriteMode::from_value(mode.value());
            assert_eq!(read, Some(mode), "truecolour {eig:?} {alpha}");
        }
    }
}

#[test]
fn a_thirty_five_words_pixel_shape_comes_from_its_resolutions() {
    let word = (3 << 27) | (45 << 14) | (90 << 1) | 1;
    let mode = SpriteMode::from_value(word).expect("a mode word");
    assert_eq!(mode.eig(), (1, 2));
    assert_eq!(mode.pixel_aspect(), (1, 2));
    assert!(!mode.is_numbered());
}

#[test]
fn a_five_words_eigen_factors_are_its_own_fields() {
    let word = ro5(6, 0, false) | (2 << 4) | (1 << 6);
    let mode = SpriteMode::from_value(word).expect("a mode word");
    assert_eq!(mode.eig(), (2, 1));
    assert_eq!(
        mode.layout(),
        SpriteLayout::Direct {
            bits_per_pixel: 32,
            alpha_channel: false
        }
    );
}

#[test]
fn a_new_paletted_sprite_takes_a_numbered_mode_every_risc_os_reads() {
    let mode = SpriteMode::indexed(IndexDepth::Four, (1, 2), false);
    assert_eq!(mode.value(), 12);
    let square = SpriteMode::indexed(IndexDepth::Eight, (1, 1), false);
    assert!(square.is_numbered());
    assert_eq!(square.eig(), (1, 1));
    assert_eq!(square.layout(), SpriteLayout::Indexed(IndexDepth::Eight));
    // An alpha mask needs a mode word: a numbered mode's mask is binary.
    let alpha = SpriteMode::indexed(IndexDepth::Four, (1, 1), true);
    assert!(!alpha.is_numbered());
    assert!(alpha.alpha_mask());
    assert_eq!(alpha.layout(), SpriteLayout::Indexed(IndexDepth::Four));
}

#[test]
fn the_least_a_sprite_stores_is_its_control_block_and_its_word_aligned_rows() {
    let one_bit = SpriteLayout::Indexed(IndexDepth::One);
    // Thirty-three one-bit pixels need two words a row.
    assert_eq!(one_bit.least_stored_bytes(33, 3), Some(44 + 3 * 8));
    assert_eq!(one_bit.least_stored_bytes(32, 1), Some(44 + 4));
    let direct = SpriteLayout::Direct {
        bits_per_pixel: 24,
        alpha_channel: false,
    };
    assert_eq!(direct.bits_per_pixel(), 24);
    // Three twenty-four-bit pixels are seventy-two bits: three words.
    assert_eq!(direct.least_stored_bytes(3, 2), Some(44 + 2 * 12));
    assert_eq!(
        SpriteLayout::Indexed(IndexDepth::Eight).least_stored_bytes(u32::MAX, u32::MAX),
        Some(44 + u64::from(u32::MAX).div_ceil(4) * 4 * u64::from(u32::MAX)),
    );
    let widest = SpriteLayout::Direct {
        bits_per_pixel: 32,
        alpha_channel: true,
    };
    assert_eq!(
        widest.least_stored_bytes(u32::MAX, u32::MAX),
        None,
        "overflows"
    );
}

#[test]
fn a_new_truecolour_sprite_is_a_thirty_two_bit_mode_word() {
    let mode = SpriteMode::truecolour((1, 1), false);
    assert_eq!(mode.value(), ro35(6, false));
    assert!(SpriteMode::truecolour((1, 1), true).alpha_mask());
}

#[test]
fn a_value_naming_no_readable_layout_is_not_a_mode() {
    assert!(SpriteMode::from_value(7).is_none());
    assert!(SpriteMode::from_value(ro35(9, false)).is_none());
    assert!(SpriteMode::from_value(0x100).is_none());
}

#[test]
fn a_paletted_sprite_reads_as_its_indices_palette_and_mask() {
    let sprite = picture(native(&one(Sprite::new(MODE_16, 4, 4, 1)
        .pixels(&[0, 7, 11, 15])
        .mask_image(&[true, false, true, true]))));
    assert_eq!(sprite.mode.value(), MODE_16);
    assert_eq!(sprite.palette, SpritePalette::Implied);
    assert!(sprite.masked);
    let Pixels::Indexed {
        depth,
        palette,
        indices,
        mask,
    } = sprite.picture.pixels()
    else {
        panic!("a paletted sprite reads as indices");
    };
    assert_eq!(*depth, IndexDepth::Four);
    assert_eq!(palette.as_slice(), &WIMP);
    assert_eq!(indices.as_slice(), &[0, 7, 11, 15]);
    assert_eq!(mask.as_deref(), Some(&[255, 0, 255, 255][..]));
}

#[test]
fn a_stored_palette_is_kept_exactly_as_the_file_held_it() {
    let entries = [[1, 2, 3], [4, 5, 6], [7, 8, 9], [10, 11, 12]];
    let fixture = Sprite::new(MODE_4, 2, 4, 1)
        .palette(&entries)
        .pixels(&[3, 2, 1, 0]);
    let raw = fixture.palette.clone();
    let sprite = picture(native(&one(fixture)));
    assert_eq!(sprite.palette, SpritePalette::Stored(raw));
    let Pixels::Indexed { palette, .. } = sprite.picture.pixels() else {
        panic!("indices");
    };
    assert_eq!(palette[3], [10, 11, 12, 255]);
}

#[test]
fn a_direct_sprites_mask_is_folded_into_its_alpha() {
    let sprite = picture(native(&one(Sprite::new(ro35(6, false), 32, 2, 1)
        .pixels(&[0x0000_00FF, 0x0000_FF00])
        .mask_bits(&[true, false]))));
    assert!(sprite.masked);
    let Pixels::Rgba(rgba) = sprite.picture.pixels() else {
        panic!("a direct sprite reads as RGBA");
    };
    assert_eq!(rgba.as_slice(), &[255, 0, 0, 255, 0, 255, 0, 0]);
}

#[test]
fn a_sprite_that_cannot_be_read_is_kept_as_its_exact_bytes() {
    let jpeg = Sprite::new(ro35(9, false), 32, 2, 1);
    let bytes = area(&[Sprite::new(MODE_2, 1, 8, 1), jpeg]);
    let mut reader = SpriteAreaReader::open(&bytes[..], &limits()).expect("the chain is sound");
    assert_eq!(reader.count(), 2);
    assert!(matches!(
        reader.sprite(0).expect("no machine refusal"),
        Some(SpriteEntry::Picture(_))
    ));
    let Some(SpriteEntry::Opaque(kept)) = reader.sprite(1).expect("no machine refusal") else {
        panic!("the JPEG sprite is kept");
    };
    assert_eq!(kept.reason, DecodeError::SpriteUnsupportedType);
    let second = &bytes[16 - 4 + Sprite::new(MODE_2, 1, 8, 1).bytes().len()..];
    assert_eq!(kept.bytes.as_slice(), second);
    assert_eq!(kept.name.as_bytes(), b"fixture");
    assert!(reader.sprite(2).expect("past the end").is_none());
}

#[test]
fn a_sprite_too_large_to_open_is_kept_rather_than_refusing_the_area() {
    let bytes = one(Sprite::new(MODE_16, 4, 64, 4).pixels(&[0; 256]));
    let tight = DecodeLimits::new(8, 8, 64, 0);
    let mut reader = SpriteAreaReader::open(&bytes[..], &tight).expect("the chain is sound");
    let Some(SpriteEntry::Opaque(kept)) = reader.sprite(0).expect("no machine refusal") else {
        panic!("kept");
    };
    assert_eq!(kept.reason, DecodeError::WidthExceedsLimit);
}

#[test]
fn an_area_none_of_whose_sprites_can_be_read_still_opens_natively() {
    let bytes = area(&[
        Sprite::new(7, 4, 2, 1),
        Sprite::new(ro35(7, false), 32, 2, 1),
    ]);
    let mut reader = SpriteAreaReader::open(&bytes[..], &limits()).expect("the chain is sound");
    for index in 0..2 {
        assert!(matches!(
            reader.sprite(index).expect("no machine refusal"),
            Some(SpriteEntry::Opaque(_))
        ));
    }
}

#[test]
fn a_broken_chain_refuses_the_native_walk_too() {
    let bytes = one(Sprite::new(MODE_2, 1, 8, 1).length(4096));
    assert_eq!(
        SpriteAreaReader::open(&bytes[..], &limits()).err(),
        Some(DecodeError::SpriteBadArea)
    );
}

#[test]
fn opening_an_area_natively_answers_its_reader() {
    let bytes = one(Sprite::new(MODE_2, 1, 8, 1));
    let Ok(NativeDocument::Sprites(reader)) =
        open_native(ImageFormat::Sprite, &bytes[..], &limits())
    else {
        panic!("a sprite area opens as its sprites");
    };
    assert_eq!(reader.count(), 1);
}

#[test]
fn a_native_sprite_flattens_to_exactly_what_a_decode_answers() {
    let bytes = one(Sprite::new(MODE_256, 8, 3, 2)
        .pixels(&[0x00, 0x11, 0xFF, 0x80, 0x03, 0x60])
        .mask_image(&[true, true, false, true, false, true]));
    let flat = picture(native(&bytes)).picture.to_rgba().expect("memory");
    let decoded = decode_as(ImageFormat::Sprite, &bytes, &limits()).expect("decodes");
    assert_eq!(flat, decoded.pixels());
}

#[test]
fn a_mode_takes_an_alpha_mask_keeping_its_layout_and_shape() {
    let numbered = SpriteMode::from_value(MODE_16).expect("mode 12");
    let widened = numbered.with_alpha_mask(true);
    assert!(widened.alpha_mask() && !widened.is_numbered());
    assert_eq!(widened.layout(), numbered.layout());
    assert_eq!(widened.eig(), numbered.eig());
    assert_eq!(numbered.with_alpha_mask(false), numbered);
    let word = SpriteMode::from_value(ro35(6, false)).expect("a word");
    let alpha = word.with_alpha_mask(true);
    assert_eq!(alpha.value(), ro35(6, true));
    assert_eq!(alpha.with_alpha_mask(false), word);
}

#[test]
fn a_name_read_back_keeps_its_case_but_never_a_control_character() {
    assert_eq!(
        SpriteName::from_bytes(b"MixedCase")
            .expect("valid")
            .as_bytes(),
        b"MixedCase"
    );
    assert!(SpriteName::from_bytes(b"").is_some());
    assert!(SpriteName::from_bytes(b"has space").is_none());
    assert!(SpriteName::from_bytes(b"thirteen_long").is_none());
    assert!(SpriteName::from_bytes(b"tab\there").is_none());
}

/// A mode restated at another pixel shape keeps its layout and mask, and its
/// word reads back as that shape, whichever form the word takes.
#[test]
fn a_mode_restated_at_another_shape_keeps_its_layout() {
    let sixteen = SpriteMode::from_value((5 << 27) | (45 << 14) | (90 << 1) | 1).expect("a word");
    let five = SpriteMode::from_value(ro5(6, 0, false) | (1 << 4) | (2 << 6)).expect("a word");
    for mode in [sixteen, five, SpriteMode::from_value(12).expect("a mode")] {
        let turned = mode.with_eig((mode.eig().1, mode.eig().0));
        assert_eq!(turned.eig(), (mode.eig().1, mode.eig().0), "{mode:?}");
        assert_eq!(turned.layout(), mode.layout());
        assert_eq!(turned.alpha_mask(), mode.alpha_mask());
        assert_eq!(
            SpriteMode::from_value(turned.value()),
            Some(turned),
            "{mode:?}"
        );
    }
}

/// A fitted decode streams the largest sprite's rows into exactly its whole
/// decode reduced, and admits only what a decode admits.
#[test]
fn a_fitted_decode_is_the_whole_decode_reduced() {
    let values: Vec<u32> = (0..9 * 7).map(|at| at % 16).collect();
    let bytes = area(&[
        Sprite::new(MODE_16, 4, 2, 2),
        Sprite::new(MODE_16, 4, 9, 7).pixels(&values),
    ]);
    let whole = decode_as(ImageFormat::Sprite, &bytes, &limits()).expect("decodes");
    let fitted = decode_fitted_as(ImageFormat::Sprite, &bytes, &limits(), FitBox::new(3, 3))
        .expect("decodes");
    assert_eq!((fitted.width(), fitted.height()), (4, 3));
    let source = tairix_raster::Rgba8Image::new(9, 7, whole.pixels()).expect("image");
    let reduced = tairix_raster::resample(&source, source.whole(), 4, 3).expect("resamples");
    assert_eq!(fitted.pixels(), reduced.as_slice());

    let tight = DecodeLimits::new(4, 4, 16, 0);
    assert_eq!(
        decode_fitted_as(ImageFormat::Sprite, &bytes, &tight, FitBox::new(2, 2)),
        Err(DecodeError::WidthExceedsLimit)
    );
}
