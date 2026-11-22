//! A sprite's control block in guest memory, and the geometry RISC OS 5
//! reads from it: words and bits taken as the ARM's 32-bit arithmetic takes
//! them, so a width of nought, held as -1 words, has no pixels.

use tairix_image::{
    sprite_control as control, sprite_mask_stride, SpriteName, SPRITE_HEADER_LEN,
    SPRITE_PALETTE_ENTRY_LEN,
};
use tairix_riscos_kernel::os::{Fail, Os};

use super::format::Pixels;
use super::UNRECOGNISED;

/// `address` moved on `offset` bytes, wrapping as the ARM's sums do.
pub(crate) fn advanced(address: u32, offset: u64) -> u32 {
    address.wrapping_add(u32::try_from(offset & u64::from(u32::MAX)).unwrap_or(0))
}

/// A sprite: where its control block starts, and what the block holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Sprite {
    pub(crate) at: u32,
    /// The bytes to the next sprite.
    pub(crate) next: u32,
    pub(crate) name: SpriteName,
    /// Its rows' length in words and its rows, each less one, as held.
    pub(crate) words_less_one: u32,
    pub(crate) rows_less_one: u32,
    /// The first and last bits of a row that hold pixels.
    pub(crate) first_bit: u32,
    pub(crate) last_bit: u32,
    /// Where its image and mask start, from its first byte.
    pub(crate) image: u32,
    pub(crate) mask: u32,
    pub(crate) mode: u32,
}

impl Sprite {
    /// The sprite whose control block starts at `at`.
    pub(crate) fn read(os: &mut Os<'_>, at: u32) -> Result<Self, Fail> {
        let mut block = [0u8; SPRITE_HEADER_LEN as usize];
        os.read_bytes(at, &mut block)?;
        let word = |offset: u32| {
            let at = offset as usize;
            u32::from_le_bytes([block[at], block[at + 1], block[at + 2], block[at + 3]])
        };
        let name_at = control::NAME as usize;
        Ok(Self {
            at,
            next: word(control::NEXT),
            name: SpriteName::spelt_by(&block[name_at..name_at + SpriteName::MAX_LEN]),
            words_less_one: word(control::WIDTH_WORDS),
            rows_less_one: word(control::HEIGHT),
            first_bit: word(control::FIRST_BIT),
            last_bit: word(control::LAST_BIT),
            image: word(control::IMAGE),
            mask: word(control::MASK),
            mode: word(control::MODE),
        })
    }

    /// Writes its control block, its name padded with zeros.
    pub(crate) fn write(&self, os: &mut Os<'_>) -> Result<(), Fail> {
        let mut block = [0u8; SPRITE_HEADER_LEN as usize];
        let mut put = |offset: u32, value: u32| {
            let at = offset as usize;
            block[at..at + 4].copy_from_slice(&value.to_le_bytes());
        };
        put(control::NEXT, self.next);
        put(control::WIDTH_WORDS, self.words_less_one);
        put(control::HEIGHT, self.rows_less_one);
        put(control::FIRST_BIT, self.first_bit);
        put(control::LAST_BIT, self.last_bit);
        put(control::IMAGE, self.image);
        put(control::MASK, self.mask);
        put(control::MODE, self.mode);
        let name = self.name.as_bytes();
        let name_at = control::NAME as usize;
        block[name_at..name_at + name.len()].copy_from_slice(name);
        os.write_bytes(self.at, &block)
    }

    /// The pixels its mode word gives.
    ///
    /// # Errors
    ///
    /// The word's refusal: a mode no sprite may be in.
    pub(crate) fn pixels(&self, os: &mut Os<'_>) -> Result<Pixels, Fail> {
        Pixels::stored(self.mode).map_err(|error| os.error(error))
    }

    /// The words of each row.
    pub(crate) const fn words(&self) -> u32 {
        self.words_less_one.wrapping_add(1)
    }

    /// Its rows.
    pub(crate) const fn rows(&self) -> u32 {
        self.rows_less_one.wrapping_add(1)
    }

    /// The bytes each row of its image takes.
    pub(crate) const fn stride(&self) -> u64 {
        4 * self.words() as u64
    }

    /// The bytes its image takes.
    pub(crate) const fn image_bytes(&self) -> u64 {
        self.stride() * self.rows() as u64
    }

    /// Its pixels across, as RISC OS 5 works them out.
    pub(crate) const fn width(&self, pixels: &Pixels) -> u32 {
        self.words_less_one
            .wrapping_mul(32)
            .wrapping_add(self.last_bit)
            .wrapping_add(1)
            .wrapping_sub(self.first_bit)
            >> pixels.log2_field
    }

    /// Whether it has a mask.
    pub(crate) const fn masked(&self) -> bool {
        self.mask != self.image
    }

    /// The bytes each row of its mask takes: an old-format sprite's rows are
    /// its image's, a new one's hold only its pixels' mask bits.
    pub(crate) fn mask_stride(&self, pixels: &Pixels) -> u64 {
        if pixels.old_format {
            return self.stride();
        }
        sprite_mask_stride(self.width(pixels), pixels.mask_bits()).map_or(u64::MAX, u64::from)
    }

    /// The bytes its mask takes, were it to have one.
    pub(crate) fn mask_bytes(&self, pixels: &Pixels) -> u64 {
        self.mask_stride(pixels)
            .saturating_mul(u64::from(self.rows()))
    }

    /// The palette entries it holds, between its control block and the first
    /// of its image and mask.
    pub(crate) fn palette_entries(&self) -> u32 {
        self.image.min(self.mask).saturating_sub(SPRITE_HEADER_LEN) / SPRITE_PALETTE_ENTRY_LEN
    }

    /// The palette entries its pixels can name, as RISC OS 5 reads its
    /// palette: those it holds, but no more than its pixels' colours.
    pub(crate) fn named_entries(&self) -> u32 {
        self.palette_entries()
            .min(Pixels::verified(self.mode).colours())
    }

    /// The address of byte `offset` of it.
    pub(crate) const fn address(&self, offset: u32) -> u32 {
        self.at.wrapping_add(offset)
    }

    /// Checks its image, and its mask if it has one, lie within it, as a
    /// change to its shape relies on.
    ///
    /// # Errors
    ///
    /// "Unrecognised sprite data" for a sprite whose blocks do not hold
    /// together.
    pub(crate) fn whole(&self, os: &mut Os<'_>, pixels: &Pixels) -> Result<(), Fail> {
        let within = |start: u32, bytes: u64| {
            u64::from(start) >= u64::from(SPRITE_HEADER_LEN)
                && u64::from(start) + bytes <= u64::from(self.next)
                && start.is_multiple_of(4)
        };
        let fits = self.next.is_multiple_of(4)
            && within(self.image, self.image_bytes())
            && (!self.masked() || within(self.mask, self.mask_bytes(pixels)));
        if fits {
            Ok(())
        } else {
            Err(os.error(UNRECOGNISED))
        }
    }
}
