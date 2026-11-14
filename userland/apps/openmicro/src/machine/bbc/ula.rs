//! The video ULA: the clock it gives the 6845, the serialiser that turns each
//! byte of screen memory into pixels through its sixteen-entry palette, the
//! flash select, and the cursor.

/// Control register bits.
pub mod control {
    /// Which of a flashing colour's two colours is shown.
    pub const FLASH: u8 = 0x01;
    /// The picture comes from the teletext generator.
    pub const TELETEXT: u8 = 0x02;
    /// Characters per line, `10 << n` for the two bits' value `n`.
    pub const CHARS_PER_LINE: u8 = 0x0C;
    /// The 6845 runs at 2 MHz rather than 1 MHz.
    pub const FAST_CLOCK: u8 = 0x10;
}

/// 16 MHz pixel slots per 2 MHz cycle.
pub const SLOTS_PER_CYCLE: usize = 8;

/// A palette entry's four bits.
const ENTRY_MASK: u8 = 0x0F;
/// The cursor's four segments.
const SEGMENTS: u8 = 0x0F;

/// The chip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ula {
    control: u8,
    /// Physical colour per logical colour, as written: bits 0–2 the colour
    /// inverted, bit 3 the flash bit.
    palette: [u8; 16],
    /// Physical colour per logical colour, as currently shown.
    shown: [u8; 16],
    /// Character periods the cursor still covers, as a mask of segments
    /// shifted down one per character: bit 0 is the current character.
    cursor: u8,
}

impl Default for Ula {
    fn default() -> Self {
        Self::new()
    }
}

impl Ula {
    /// A chip at power-on.
    #[must_use]
    pub fn new() -> Self {
        let mut ula = Self {
            control: 0,
            palette: [0; 16],
            shown: [0; 16],
            cursor: 0,
        };
        ula.refresh_palette();
        ula
    }

    /// The control register.
    #[must_use]
    pub const fn control(&self) -> u8 {
        self.control
    }

    /// Write the control register.
    pub fn write_control(&mut self, value: u8) {
        self.control = value;
        self.refresh_palette();
    }

    /// Write one palette entry: logical colour in bits 4–7, physical colour
    /// (inverted, flash in bit 3) in bits 0–3.
    pub fn write_palette(&mut self, value: u8) {
        self.palette[usize::from(value >> 4)] = value & ENTRY_MASK;
        self.refresh_palette();
    }

    /// Whether the 6845 is clocked at 2 MHz.
    #[must_use]
    pub const fn fast_clock(&self) -> bool {
        self.control & control::FAST_CLOCK != 0
    }

    /// Whether the picture is the teletext generator's.
    #[must_use]
    pub const fn teletext(&self) -> bool {
        self.control & control::TELETEXT != 0
    }

    /// The physical colour (bit 0 red, 1 green, 2 blue) of logical colour
    /// `index`, with the flash select applied.
    #[must_use]
    pub const fn colour(&self, index: u8) -> u8 {
        self.shown[(index & 0x0F) as usize]
    }

    /// The pixel slots between shifts: the serialiser shifts at 16 MHz for
    /// 80 characters per line, halving with each step down to 10.
    #[must_use]
    pub const fn slots_per_shift(&self) -> usize {
        1 << (3 - ((self.control & control::CHARS_PER_LINE) >> 2))
    }

    /// The 6845 raised CURSOR for the character now starting: arm the
    /// segments the control register enables, the first for this character,
    /// the second for the next, the third and fourth for the two after.
    pub fn start_cursor(&mut self) {
        let mut segments = 0;
        if self.control & 0x80 != 0 {
            segments |= 0x01;
        }
        if self.control & 0x40 != 0 {
            segments |= 0x02;
        }
        if self.control & 0x20 != 0 {
            segments |= 0x0C;
        }
        self.cursor |= segments;
    }

    /// Whether the cursor inverts the character now being drawn, then move it
    /// on one character.
    pub fn take_cursor(&mut self) -> bool {
        let covers = self.cursor & 1 != 0;
        self.cursor >>= 1;
        covers
    }

    /// Serialise `byte`, `shifted` shifts in, into the logical colours of
    /// `slots` consecutive pixel slots, as the shift register presents them:
    /// the index of each pixel is bits 7, 5, 3 and 1, and each shift fills
    /// from the bottom with ones.
    pub fn serialise(&self, byte: u8, shifted: usize, slots: &mut [u8]) {
        let per_shift = self.slots_per_shift();
        let mut register = if shifted >= 8 {
            0xFF
        } else {
            (byte << shifted) | ((1u8 << shifted) - 1)
        };
        for chunk in slots.chunks_mut(per_shift) {
            let index = ((register >> 4) & 0x08)
                | ((register >> 3) & 0x04)
                | ((register >> 2) & 0x02)
                | ((register >> 1) & 0x01);
            chunk.fill(index);
            register = (register << 1) | 1;
        }
    }

    fn refresh_palette(&mut self) {
        let flash = self.control & control::FLASH != 0;
        for (shown, &entry) in self.shown.iter_mut().zip(&self.palette) {
            let colour = (entry & 0x07) ^ 0x07;
            *shown = if flash && entry & 0x08 != 0 {
                colour ^ 0x07
            } else {
                colour
            };
        }
    }
}

impl crate::snapshot::Save for Ula {
    fn save(&self, w: &mut crate::snapshot::Writer) {
        w.put(&self.control);
        w.put(&self.palette);
        w.put(&self.cursor);
    }
}

impl crate::snapshot::Load for Ula {
    fn load(r: &mut crate::snapshot::Reader<'_>) -> Result<Self, crate::snapshot::SnapshotError> {
        let mut ula = Self {
            control: r.get()?,
            palette: r.get()?,
            shown: [0; 16],
            cursor: r.get()?,
        };
        crate::snapshot::Reader::check(
            ula.palette.iter().all(|&entry| entry <= ENTRY_MASK) && ula.cursor <= SEGMENTS,
        )?;
        ula.refresh_palette();
        Ok(ula)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_palette_entry_shows_its_colour_inverted_and_flash_swaps_it() {
        let mut ula = Ula::new();
        // Logical 3 to physical 1 (red), written inverted: 6; flashing: 14.
        ula.write_palette(0x36);
        assert_eq!(ula.colour(3), 0x01);
        ula.write_palette(0x3E);
        assert_eq!(ula.colour(3), 0x01);
        ula.write_control(control::FLASH);
        assert_eq!(
            ula.colour(3),
            0x06,
            "the flash shows cyan, red's complement"
        );
        assert_eq!(ula.colour(4), 0x07, "an unwritten entry shows white");
    }

    #[test]
    fn the_serialiser_reads_bits_7_5_3_and_1_and_fills_with_ones() {
        let mut ula = Ula::new();
        // 80 characters a line: one slot per shift.
        ula.write_control(0x0C);
        let mut slots = [0; SLOTS_PER_CYCLE];
        // Pixel n is bits 7, 5, 3 and 1 of the byte shifted n places, ones
        // filling from the bottom.
        ula.serialise(0b1010_0000, 0, &mut slots);
        assert_eq!(slots, [0x0C, 0x00, 0x09, 0x01, 0x03, 0x03, 0x07, 0x07]);
        ula.serialise(0b1010_0000, 4, &mut slots);
        assert_eq!(slots, [0x03, 0x03, 0x07, 0x07, 0x0F, 0x0F, 0x0F, 0x0F]);
        // Twenty characters: two pixels of four slots, bits 7531 then 6420.
        ula.write_control(0x04);
        ula.serialise(0b0101_0101, 0, &mut slots);
        assert_eq!(slots, [0x00, 0x00, 0x00, 0x00, 0x0F, 0x0F, 0x0F, 0x0F]);
    }

    #[test]
    fn the_cursor_s_segments_cover_one_two_and_four_characters() {
        let mut ula = Ula::new();
        let covered = |ula: &mut Ula| {
            (0..6)
                .map(|_| ula.take_cursor())
                .collect::<alloc::vec::Vec<bool>>()
        };
        ula.write_control(0x80);
        ula.start_cursor();
        assert_eq!(covered(&mut ula), [true, false, false, false, false, false]);
        ula.write_control(0xE0);
        ula.start_cursor();
        assert_eq!(covered(&mut ula), [true, true, true, true, false, false]);
        ula.write_control(0x20);
        ula.start_cursor();
        assert_eq!(covered(&mut ula), [false, false, true, true, false, false]);
    }

    #[test]
    fn the_control_register_picks_the_clock_and_the_picture_source() {
        let mut ula = Ula::new();
        ula.write_control(0x9C);
        assert!(ula.fast_clock() && !ula.teletext());
        assert_eq!(ula.slots_per_shift(), 1);
        ula.write_control(0x4B);
        assert!(!ula.fast_clock() && ula.teletext());
        assert_eq!(ula.control(), 0x4B);
    }
}
