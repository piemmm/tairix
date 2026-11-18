//! Where a side's bytes lie in a disc image. An image holds whole tracks of
//! ten 256-byte sectors, and may end short of the disc's last track: a side's
//! bytes past the image's end read as unrecorded.

/// The bits a byte's offset within its sector takes.
pub const SECTOR_SHIFT: u32 = 8;
/// Bytes a sector holds.
pub const SECTOR_LEN: usize = 1 << SECTOR_SHIFT;
/// Sectors a track holds.
pub const SECTORS_PER_TRACK: usize = 10;
/// Bytes a track holds.
pub const TRACK_LEN: usize = SECTOR_LEN * SECTORS_PER_TRACK;

/// How an image holds a disc.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Layout {
    /// One side's tracks in order: an `.ssd`.
    Single,
    /// Both sides' tracks, alternating track by track from the first side's
    /// first: a `.dsd`, drive 0's side and drive 2's.
    Double,
}

impl Layout {
    /// The ending an image of this layout is named with.
    #[must_use]
    pub const fn ending(self) -> &'static str {
        match self {
            Self::Single => "ssd",
            Self::Double => "dsd",
        }
    }

    /// The layout an image whose name ends in `ending` holds, case aside.
    #[must_use]
    pub fn of_ending(ending: &str) -> Option<Self> {
        [Self::Single, Self::Double]
            .into_iter()
            .find(|layout| layout.ending().eq_ignore_ascii_case(ending))
    }

    /// The sides the image holds.
    #[must_use]
    pub const fn sides(self) -> usize {
        match self {
            Self::Single => 1,
            Self::Double => 2,
        }
    }

    /// Where byte `offset` of `side` lies in the image.
    #[must_use]
    pub const fn image_offset(self, side: usize, offset: usize) -> usize {
        (offset / TRACK_LEN * self.sides() + side) * TRACK_LEN + offset % TRACK_LEN
    }

    /// How many of `side`'s bytes, from its first, an image of `image_len`
    /// bytes holds.
    #[must_use]
    pub const fn side_len(self, side: usize, image_len: usize) -> usize {
        let round = TRACK_LEN * self.sides();
        let partial = image_len % round;
        let into_side = partial.saturating_sub(side * TRACK_LEN);
        let last = if into_side < TRACK_LEN {
            into_side
        } else {
            TRACK_LEN
        };
        image_len / round * TRACK_LEN + last
    }

    /// The bytes an image needs to hold `side`'s first `len` bytes in whole
    /// sectors: up to the end of the sector holding the last of them.
    #[must_use]
    pub const fn image_end(self, side: usize, len: usize) -> usize {
        if len == 0 {
            return 0;
        }
        self.image_offset(side, len.div_ceil(SECTOR_LEN) * SECTOR_LEN - 1) + 1
    }
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
