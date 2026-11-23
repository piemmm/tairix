//! A direct-colour pixel's three fields, as constants a layout's arm sees,
//! so that each layout's shifts and widths fold into its code.

#![expect(
    clippy::inline_always,
    reason = "a field's arithmetic folds only inlined into its layout's arm: measured 2.4 times slower otherwise"
)]

use tairix_image::{sprite_direct_fields, sprite_type};

/// A pixel's fields, lowest first: each one's shift and width, and the bytes
/// the pixel takes.
#[derive(Clone, Copy)]
pub(super) struct Fields {
    shifts: [u32; 3],
    pub(super) widths: [u32; 3],
    pub(super) bytes: usize,
}

pub(super) const TWELVE: Fields = Fields::of_type(sprite_type::DIRECT_12);
pub(super) const FIFTEEN: Fields = Fields::of_type(sprite_type::DIRECT_15);
pub(super) const SIXTEEN: Fields = Fields::of_type(sprite_type::DIRECT_16);
pub(super) const TWENTY_FOUR: Fields = Fields::of_type(sprite_type::DIRECT_24);
const _: () = assert!(
    TWELVE.bytes != 0 && FIFTEEN.bytes != 0 && SIXTEEN.bytes != 0 && TWENTY_FOUR.bytes != 0,
    "each layout a direct-colour sprite type's"
);

impl Fields {
    /// Direct-colour sprite type `sprite_type`'s colour fields; none, of no
    /// bytes, for any other type.
    const fn of_type(sprite_type: u8) -> Self {
        let ([low, middle, high, _], bytes) = match sprite_direct_fields(sprite_type) {
            Some(fields) => fields,
            None => ([0; 4], 0),
        };
        Self {
            shifts: [0, low, low + middle],
            widths: [low, middle, high],
            bytes: bytes as usize,
        }
    }

    /// Every bit the fields take.
    pub(super) const fn colour_bits(self) -> u32 {
        let [low, middle, high] = self.widths;
        most(low + middle + high)
    }

    /// Each field of `pixel`, in its own bits.
    #[inline(always)]
    pub(super) const fn extract(self, pixel: u32) -> [u32; 3] {
        let [low_at, middle_at, high_at] = self.shifts;
        let [low_bits, middle_bits, high_bits] = self.widths;
        [
            (pixel >> low_at) & most(low_bits),
            (pixel >> middle_at) & most(middle_bits),
            (pixel >> high_at) & most(high_bits),
        ]
    }

    /// `values`, each in its own field's bits, placed in a pixel.
    #[inline(always)]
    pub(super) const fn place(self, [low, middle, high]: [u32; 3]) -> u32 {
        let [low_at, middle_at, high_at] = self.shifts;
        (low << low_at) | (middle << middle_at) | (high << high_at)
    }

    /// Eight-bit `values` cut to their fields' bits.
    #[inline(always)]
    pub(super) const fn narrowed(self, [low, middle, high]: [u32; 3]) -> [u32; 3] {
        let [low_bits, middle_bits, high_bits] = self.widths;
        [
            low >> (8 - low_bits),
            middle >> (8 - middle_bits),
            high >> (8 - high_bits),
        ]
    }

    /// `values` of their fields' bits widened to eight by repeating their top
    /// bits below them.
    #[inline(always)]
    pub(super) const fn replicated(self, [low, middle, high]: [u32; 3]) -> [u32; 3] {
        let [low_bits, middle_bits, high_bits] = self.widths;
        [
            replicated(low, low_bits),
            replicated(middle, middle_bits),
            replicated(high, high_bits),
        ]
    }

    /// `values` of their fields' bits widened to eight, rounded down.
    #[inline(always)]
    pub(super) const fn rounded(self, [low, middle, high]: [u32; 3]) -> [u32; 3] {
        let [low_bits, middle_bits, high_bits] = self.widths;
        [
            low * 255 / most(low_bits),
            middle * 255 / most(middle_bits),
            high * 255 / most(high_bits),
        ]
    }

    /// Nibbles widened to their fields' bits as RISC OS 5 widens a colour
    /// number's: to five bits the low bit repeated, to six the top two, to
    /// eight the nibble twice.
    #[inline(always)]
    pub(super) const fn nibbles_widened(self, [low, middle, high]: [u32; 3]) -> [u32; 3] {
        let [low_bits, middle_bits, high_bits] = self.widths;
        [
            nibble_widened(low, low_bits),
            nibble_widened(middle, middle_bits),
            nibble_widened(high, high_bits),
        ]
    }
}

/// The highest value `bits` bits hold.
#[inline(always)]
const fn most(bits: u32) -> u32 {
    (1 << bits) - 1
}

/// A value of `bits` bits, four to eight, widened to eight by repeating its
/// top bits below it.
#[inline(always)]
pub(crate) const fn replicated(value: u32, bits: u32) -> u32 {
    (value << (8 - bits)) | (value >> (2 * bits - 8))
}

/// A nibble widened to `bits` bits as RISC OS 5 widens a colour number's.
#[inline(always)]
const fn nibble_widened(nibble: u32, bits: u32) -> u32 {
    match bits {
        5 => (nibble << 1) | (nibble & 1),
        6 => (nibble << 2) | (nibble >> 2),
        8 => nibble * 0x11,
        _ => nibble,
    }
}
