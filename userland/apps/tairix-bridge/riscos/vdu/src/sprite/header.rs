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

/// The little-endian word at `offset` of a control block read whole.
pub(crate) fn word_at(block: &[u8], offset: u32) -> u32 {
    let at = offset as usize;
    u32::from_le_bytes([block[at], block[at + 1], block[at + 2], block[at + 3]])
}

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
        let word = |offset| word_at(&block, offset);
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
        super::format::stored_pixels(os, self.mode)
    }

    /// As [`Sprite::pixels`], for a change to it: a module asked about its
    /// mode may have changed the area it lies in, so its control block is
    /// read again and must be as it was.
    ///
    /// # Errors
    ///
    /// The word's refusal, or "Unrecognised sprite data" where the control
    /// block is no longer as read.
    pub(crate) fn pixels_kept(&self, os: &mut Os<'_>) -> Result<Pixels, Fail> {
        let pixels = self.pixels(os)?;
        self.still(os)?;
        Ok(pixels)
    }

    /// Refuses unless its control block still reads as it was read.
    ///
    /// # Errors
    ///
    /// "Unrecognised sprite data" where it does not.
    pub(crate) fn still(&self, os: &mut Os<'_>) -> Result<(), Fail> {
        if Self::read(os, self.at)? == *self {
            Ok(())
        } else {
            Err(os.error(UNRECOGNISED))
        }
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

    /// The bytes its image takes; none past what any sum holds.
    pub(crate) fn image_bytes(&self) -> Option<u64> {
        self.stride().checked_mul(u64::from(self.rows()))
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
    /// its image's, a new one's hold only its pixels' mask bits; none where
    /// that row passes what a word counts.
    pub(crate) fn mask_stride(&self, pixels: &Pixels) -> Option<u64> {
        if pixels.old_format {
            return Some(self.stride());
        }
        sprite_mask_stride(self.width(pixels), pixels.mask_bits())
            .ok()
            .map(u64::from)
    }

    /// The bytes its mask takes, were it to have one; none past what any sum
    /// holds.
    pub(crate) fn mask_bytes(&self, pixels: &Pixels) -> Option<u64> {
        self.mask_stride(pixels)?
            .checked_mul(u64::from(self.rows()))
    }

    /// Whether its rows' bits make sense: their first and last bits within
    /// a word, a new-format sprite's rows starting at their first, and its
    /// pixels across held by its words, which the sums [`Sprite::width`]
    /// wraps would otherwise make some 2^32.
    pub(crate) const fn bits_sound(&self, pixels: &Pixels) -> bool {
        let held = self.first_bit as u64 + ((self.width(pixels) as u64) << pixels.log2_field);
        self.first_bit <= 31
            && self.last_bit <= 31
            && (pixels.old_format || self.first_bit == 0)
            && held <= 32 * self.words() as u64
    }

    /// The palette entries it holds, between its control block and the first
    /// of its image and mask.
    pub(crate) fn palette_entries(&self) -> u32 {
        self.image.min(self.mask).saturating_sub(SPRITE_HEADER_LEN) / SPRITE_PALETTE_ENTRY_LEN
    }

    /// Whether it holds a whole 256-entry palette, as an eight-bit sprite
    /// must for its pixels to be palette entries rather than colours and
    /// tints: RISC OS 5 reads no more than 256, however many it holds.
    pub(crate) fn full_palette(&self) -> bool {
        self.palette_entries() >= super::FULL_PALETTE
    }

    /// The palette entries its pixels can name, as RISC OS 5 reads its
    /// palette: those it holds, but no more than its pixels' colours.
    pub(crate) fn named_entries(&self, os: &mut Os<'_>) -> Result<u32, Fail> {
        let pixels = super::format::verified_pixels(os, self.mode)?;
        Ok(self.entries_named_by(&pixels))
    }

    /// As [`Sprite::named_entries`], its pixels `pixels`.
    pub(crate) fn entries_named_by(&self, pixels: &Pixels) -> u32 {
        self.palette_entries().min(pixels.colours())
    }

    /// The address of byte `offset` of it.
    pub(crate) const fn address(&self, offset: u32) -> u32 {
        self.at.wrapping_add(offset)
    }

    /// Checks its rows' bits make sense, its image and its mask if it has
    /// one lie within it, and memory backs all of it, as reading or changing
    /// its pixels relies on: its blocks' extents.
    ///
    /// # Errors
    ///
    /// "Unrecognised sprite data" for a sprite whose blocks do not hold
    /// together.
    pub(crate) fn whole(&self, os: &mut Os<'_>, pixels: &Pixels) -> Result<Extents, Fail> {
        let end = u64::from(self.at) + u64::from(self.next);
        match self.extents(pixels) {
            Some(extents) if os.readable(self.at, end) => Ok(extents),
            _ => Err(os.error(UNRECOGNISED)),
        }
    }

    /// As [`Sprite::whole`], memory taking a change to all of it as well:
    /// what a change in place checks first, so that it is never left half
    /// made.
    ///
    /// # Errors
    ///
    /// "Unrecognised sprite data" as [`Sprite::whole`] gives it, or for a
    /// sprite memory will not let be changed.
    pub(crate) fn changeable(&self, os: &mut Os<'_>, pixels: &Pixels) -> Result<Extents, Fail> {
        let extents = self.whole(os, pixels)?;
        if os.writable(self.at, u64::from(self.at) + u64::from(self.next)) {
            Ok(extents)
        } else {
            Err(os.error(UNRECOGNISED))
        }
    }

    /// Its blocks' extents, where its bits make sense and its blocks lie
    /// within it.
    fn extents(&self, pixels: &Pixels) -> Option<Extents> {
        if !self.next.is_multiple_of(4) || !self.bits_sound(pixels) {
            return None;
        }
        let extents = Extents {
            stride: self.stride(),
            image_bytes: self.image_bytes()?,
            mask_stride: self.mask_stride(pixels)?,
            mask_bytes: self.mask_bytes(pixels)?,
        };
        let within = |start: u32, bytes: u64| {
            start >= SPRITE_HEADER_LEN
                && start.is_multiple_of(4)
                && u64::from(start)
                    .checked_add(bytes)
                    .is_some_and(|end| end <= u64::from(self.next))
        };
        let fits = within(self.image, extents.image_bytes)
            && (!self.masked() || within(self.mask, extents.mask_bytes));
        fits.then_some(extents)
    }
}

/// What [`Sprite::whole`] proves of a sprite: the bytes of its rows and of
/// its blocks, each block within the sprite. A sprite of no rows bounds its
/// rows' bytes by nothing, so a walk of its rows does nothing before it
/// takes room for one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Extents {
    /// The bytes each row of its image takes.
    pub(crate) stride: u64,
    /// The bytes its image takes.
    pub(crate) image_bytes: u64,
    /// The bytes each row of its mask takes, were it to have one.
    pub(crate) mask_stride: u64,
    /// The bytes its mask takes, were it to have one.
    pub(crate) mask_bytes: u64,
}
