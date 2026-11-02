//! RAR 1.5's compression: LZ77 over a 64 KiB window, its literals, flags
//! and offsets coded through fixed prefix codes chosen by running averages,
//! and passed through self-organising permutations that move the symbols
//! used most toward the shortest codes.

use tairix_abi::DriverError;

use super::bits::Reader;
use super::huffman::Code;
use super::window::Window;
use super::Sink;

/// A fixed code's lengths, as runs of `(length, count)`.
type Runs = &'static [(u8, usize)];

const LEN1: Runs = &[
    (2, 2),
    (3, 1),
    (4, 2),
    (5, 2),
    (6, 4),
    (7, 5),
    (8, 4),
    (9, 4),
    (10, 8),
    (12, 224),
];
const LEN2: Runs = &[
    (3, 5),
    (4, 2),
    (5, 2),
    (6, 4),
    (7, 5),
    (8, 4),
    (9, 4),
    (10, 8),
    (11, 2),
    (12, 220),
];
const HUF: [Runs; 5] = [
    &[(4, 8), (5, 8), (6, 8), (7, 9), (12, 224)],
    &[
        (5, 4),
        (6, 40),
        (7, 16),
        (8, 16),
        (9, 4),
        (11, 47),
        (12, 130),
    ],
    &[(5, 2), (6, 5), (7, 46), (8, 64), (9, 116), (10, 24)],
    &[(6, 2), (7, 14), (8, 202), (9, 33), (10, 6)],
    &[(8, 255), (9, 2)],
];

/// The short-match selector codes, as `(value, code, length)`, the codes
/// read most significant bit first: two while the selector average is
/// below 37, two above, each pair's first while the "bug fix" is on.
const SELECTORS: [&[(u8, u8, u8)]; 4] = [
    &[
        (0, 0b0, 1),
        (1, 0b1010, 4),
        (2, 0b1101, 4),
        (3, 0b1110, 4),
        (4, 0b1_1110, 5),
        (5, 0b11_1110, 6),
        (6, 0b111_1110, 7),
        (7, 0b1111_1110, 8),
        (8, 0b1111_1111, 8),
        (9, 0b1100, 4),
        (10, 0b1000, 4),
        (11, 0b1_0010, 5),
        (12, 0b10_0110, 6),
        (13, 0b10_0111, 6),
        (14, 0b1011, 4),
    ],
    &[
        (0, 0b0, 1),
        (1, 0b101, 3),
        (2, 0b1101, 4),
        (3, 0b1110, 4),
        (4, 0b1_1110, 5),
        (5, 0b11_1110, 6),
        (6, 0b111_1110, 7),
        (7, 0b1111_1110, 8),
        (8, 0b1111_1111, 8),
        (9, 0b1100, 4),
        (10, 0b1000, 4),
        (11, 0b1_0010, 5),
        (12, 0b10_0110, 6),
        (13, 0b10_0111, 6),
    ],
    &[
        (0, 0b00, 2),
        (1, 0b010, 3),
        (2, 0b011, 3),
        (3, 0b1010, 4),
        (4, 0b1101, 4),
        (5, 0b1110, 4),
        (6, 0b1_1110, 5),
        (7, 0b11_1110, 6),
        (8, 0b11_1111, 6),
        (9, 0b1100, 4),
        (10, 0b1000, 4),
        (11, 0b1_0010, 5),
        (12, 0b10_0110, 6),
        (13, 0b10_0111, 6),
        (14, 0b1011, 4),
    ],
    &[
        (0, 0b00, 2),
        (1, 0b010, 3),
        (2, 0b011, 3),
        (3, 0b101, 3),
        (4, 0b1101, 4),
        (5, 0b1110, 4),
        (6, 0b1_1110, 5),
        (7, 0b11_1110, 6),
        (8, 0b11_1111, 6),
        (9, 0b1100, 4),
        (10, 0b1000, 4),
        (11, 0b1_0010, 5),
        (12, 0b10_0110, 6),
        (13, 0b10_0111, 6),
    ],
];

/// A selector code's value and length for each eight-bit prefix; a length
/// of zero for a prefix no code is.
type Selector = [(u8, u8); 256];

fn selector(codes: &[(u8, u8, u8)]) -> Selector {
    let mut table = [(0u8, 0u8); 256];
    for &(value, code, len) in codes {
        let start = usize::from(code) << (8 - len);
        for slot in &mut table[start..start + (1 << (8 - len))] {
            *slot = (value, len);
        }
    }
    table
}

fn fixed(runs: Runs) -> Result<Code, DriverError> {
    let mut lengths = [0u8; 257];
    let mut at = 0;
    for &(len, count) in runs {
        lengths
            .get_mut(at..at + count)
            .ok_or(DriverError::DeviceFault)?
            .fill(len);
        at += count;
    }
    let mut code = Code::empty();
    code.build(&lengths[..at])?;
    Ok(code)
}

/// A self-organising permutation of bytes: each entry a byte and how often
/// it was used, the entries kept ordered by use so a frequent byte's index
/// is small and its code short.
struct Permutation {
    table: [u32; 256],
    /// Where each use count's run of entries begins.
    start: [u32; 256],
    /// The use count past which the counts are reset.
    limit: u32,
}

impl Permutation {
    fn new(init: impl Fn(u32) -> u32, limit: u32) -> Self {
        Self {
            table: core::array::from_fn(|i| init(u32::try_from(i).unwrap_or(0))),
            start: [0; 256],
            limit,
        }
    }

    fn reset(&mut self) {
        for (i, entry) in self.table.iter_mut().enumerate() {
            let group = u32::try_from(i / 32).unwrap_or(7);
            *entry = (*entry & !0xFF) | (7 - group);
        }
        self.start = [0; 256];
        for (k, start) in self.start.iter_mut().take(7).enumerate() {
            *start = (7 - u32::try_from(k).unwrap_or(7)) * 32;
        }
    }

    /// The byte at `index`, moved toward the front for its use.
    fn lookup(&mut self, index: u16) -> Result<u8, DriverError> {
        let index = usize::from(index);
        let mut value = *self.table.get(index).ok_or(DriverError::MediumError)?;
        if value & 0xFF >= self.limit {
            self.reset();
            value = self.table[index];
        }
        let count = usize::try_from(value & 0xFF).unwrap_or(0);
        let moved = usize::try_from(self.start[count]).unwrap_or(usize::MAX);
        self.start[count] += 1;
        let displaced = *self.table.get(moved).ok_or(DriverError::MediumError)?;
        self.table[index] = displaced;
        self.table[moved] = value + 1;
        Ok((value >> 8).to_le_bytes()[0])
    }
}

/// A RAR 1.5 decoder, carried from one member of a solid run to the next.
pub(super) struct V15 {
    len1: Code,
    len2: Code,
    huf: [Code; 5],
    selectors: [Selector; 4],
    avg_selector: u32,
    avg_literal: u32,
    avg_length: u32,
    avg_offset: u32,
    avg_below: u32,
    max_offset: u64,
    literal_weight: u32,
    match_weight: u32,
    flags: Permutation,
    literals: Permutation,
    offsets: Permutation,
    short_offsets: [u8; 256],
    last_dist: u64,
    last_len: u32,
    old: [u64; 4],
    old_at: usize,
    bug_fix: bool,
    repeated_literals: u32,
    // The member being decoded.
    flag_byte: u8,
    flag_bits: u32,
    stored: bool,
    repeated_last: u32,
    /// Where the next member's bytes begin, and the current member's span.
    next: u64,
    file_end: u64,
    owed: u64,
    /// The fixed tables could not be built: a defect, refused at once.
    broken: bool,
}

impl V15 {
    pub(super) fn new() -> Self {
        let codes = || -> Result<(Code, Code, [Code; 5]), DriverError> {
            Ok((
                fixed(LEN1)?,
                fixed(LEN2)?,
                [
                    fixed(HUF[0])?,
                    fixed(HUF[1])?,
                    fixed(HUF[2])?,
                    fixed(HUF[3])?,
                    fixed(HUF[4])?,
                ],
            ))
        };
        let (len1, len2, huf, broken) = match codes() {
            Ok((len1, len2, huf)) => (len1, len2, huf, false),
            Err(_) => (
                Code::empty(),
                Code::empty(),
                core::array::from_fn(|_| Code::empty()),
                true,
            ),
        };
        let mut offsets = Permutation::new(|i| i << 8, 0xFF);
        offsets.reset();
        Self {
            len1,
            len2,
            huf,
            selectors: core::array::from_fn(|k| selector(SELECTORS[k])),
            avg_selector: 0,
            avg_literal: 0x3500,
            avg_length: 0,
            avg_offset: 0,
            avg_below: 0,
            max_offset: 0x2001,
            literal_weight: 0x80,
            match_weight: 0x80,
            flags: Permutation::new(|i| (0u32.wrapping_sub(i) & 0xFF) << 8, 0xFF),
            literals: Permutation::new(|i| i << 8, 0xA1),
            offsets,
            short_offsets: core::array::from_fn(|i| u8::try_from(i).unwrap_or(0)),
            last_dist: 0,
            last_len: 0,
            old: [0; 4],
            old_at: 0,
            bug_fix: false,
            repeated_literals: 0,
            flag_byte: 0,
            flag_bits: 0,
            stored: false,
            repeated_last: 0,
            next: 0,
            file_end: 0,
            owed: 0,
            broken,
        }
    }

    /// Stand at a member of `len` bytes: those after the last member's,
    /// some of which a match of it may already have decoded.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for a member ending past any stream.
    pub(super) fn begin(&mut self, _win: &Window, len: u64) -> Result<(), DriverError> {
        self.owed = self.next;
        self.file_end = self.next.checked_add(len).ok_or(DriverError::MediumError)?;
        self.flag_bits = 0;
        self.stored = false;
        self.repeated_last = 0;
        Ok(())
    }

    /// Give out the member's next bytes.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for data out of shape.
    pub(super) fn read(
        &mut self,
        br: &mut Reader<'_>,
        win: &mut Window,
        sink: &mut Sink<'_>,
    ) -> Result<(), DriverError> {
        if self.broken {
            return Err(DriverError::DeviceFault);
        }
        while sink.wants() && self.owed < self.file_end {
            let upto = win.pos().min(self.file_end);
            if upto > self.owed {
                self.owed += sink.give_window(win, self.owed, upto - self.owed)?;
                continue;
            }
            let target = self.file_end.min(self.owed + win.ahead(sink.room()));
            while win.pos() < target {
                win.has_room(self.owed)?;
                self.step(br, win)?;
            }
        }
        Ok(())
    }

    /// End the member: the next starts after its last byte.
    pub(super) fn finish(&mut self) {
        self.next = self.file_end;
    }

    fn step(&mut self, br: &mut Reader<'_>, win: &mut Window) -> Result<(), DriverError> {
        if self.stored {
            return self.literal(br, win);
        }
        let expect_match = self.match_weight > self.literal_weight;
        if self.flag(br)? {
            return if expect_match {
                self.long_match(br, win)
            } else {
                self.literal(br, win)
            };
        }
        if self.flag(br)? {
            return if expect_match {
                self.literal(br, win)
            } else {
                self.long_match(br, win)
            };
        }
        self.short_match(br, win)
    }

    fn flag(&mut self, br: &mut Reader<'_>) -> Result<bool, DriverError> {
        if self.flag_bits == 0 {
            let index = self.huf[2].decode(br)?;
            if index == 256 {
                return Err(DriverError::MediumError);
            }
            self.flag_byte = self.flags.lookup(index)?;
            self.flag_bits = 8;
        }
        self.flag_bits -= 1;
        Ok((self.flag_byte >> self.flag_bits) & 1 == 1)
    }

    fn copy(win: &mut Window, dist: u64, len: u32) -> Result<(), DriverError> {
        win.copy(dist, len)
    }

    fn literal(&mut self, br: &mut Reader<'_>, win: &mut Window) -> Result<(), DriverError> {
        let table = match self.avg_literal {
            0..0xE00 => 0,
            0xE00..0x3600 => 1,
            0x3600..0x5E00 => 2,
            0x5E00..0x7600 => 3,
            _ => 4,
        };
        let mut index = self.huf[table].decode(br)?;
        if self.stored {
            if index == 0 {
                if br.bit()? {
                    self.stored = false;
                    self.repeated_literals = 0;
                    return Ok(());
                }
                let len = if br.bit()? { 4 } else { 3 };
                let dist = u64::from(self.huf[2].decode(br)?) << 5 | u64::from(br.bits(5)?);
                return Self::copy(win, dist, len);
            }
            index -= 1;
        } else {
            index &= 0xFF;
            if self.repeated_literals >= 16 && self.flag_bits == 0 {
                self.stored = true;
            }
            self.repeated_literals += 1;
        }
        self.avg_literal += u32::from(index);
        self.avg_literal -= self.avg_literal >> 8;
        self.literal_weight += 16;
        if self.literal_weight > 0xFF {
            self.literal_weight = 0x90;
            self.match_weight >>= 1;
        }
        win.literal(self.literals.lookup(index)?);
        Ok(())
    }

    fn long_match(&mut self, br: &mut Reader<'_>, win: &mut Window) -> Result<(), DriverError> {
        self.repeated_literals = 0;
        self.match_weight += 16;
        if self.match_weight > 0xFF {
            self.match_weight = 0x90;
            self.literal_weight >>= 1;
        }
        let raw = if self.avg_length >= 122 {
            u32::from(self.len2.decode(br)?)
        } else if self.avg_length >= 64 {
            u32::from(self.len1.decode(br)?)
        } else {
            let mut zeros = 0;
            while zeros < 8 && !br.bit()? {
                zeros += 1;
            }
            if zeros == 8 {
                br.bits(8)?
            } else {
                zeros
            }
        };
        let table = match self.avg_offset {
            0..0x700 => 0,
            0x700..0x2900 => 1,
            _ => 2,
        };
        let index = self.huf[table].decode(br)?;
        if index == 256 {
            return Err(DriverError::MediumError);
        }
        let dist = u64::from(self.offsets.lookup(index)?) << 7 | u64::from(br.bits(7)?);
        let mut len = raw + 3;
        if dist >= self.max_offset {
            len += 1;
        }
        if dist <= 256 {
            len += 8;
        }
        let old_max = self.max_offset;
        self.max_offset =
            if self.avg_below > 0xB0 || (self.avg_literal >= 0x2A00 && self.avg_length < 0x40) {
                0x7F00
            } else {
                0x2001
            };
        self.avg_length += raw;
        self.avg_length -= self.avg_length >> 5;
        self.avg_offset += u32::from(index);
        self.avg_offset -= self.avg_offset >> 8;
        if raw == 0 && dist <= old_max {
            self.avg_below += 1;
            self.avg_below -= self.avg_below >> 8;
        } else if raw != 1 && raw != 4 && self.avg_below > 0 {
            self.avg_below -= 1;
        }
        self.remember(dist);
        self.last_dist = dist;
        self.last_len = len;
        Self::copy(win, dist, len)
    }

    fn remember(&mut self, dist: u64) {
        self.old[self.old_at & 3] = dist;
        self.old_at = self.old_at.wrapping_add(1);
    }

    fn short_match(&mut self, br: &mut Reader<'_>, win: &mut Window) -> Result<(), DriverError> {
        self.repeated_literals = 0;
        if self.repeated_last == 2 {
            if br.bit()? {
                return Self::copy(win, self.last_dist, self.last_len);
            }
            self.repeated_last = 0;
        }
        let table = match (self.avg_selector < 37, self.bug_fix) {
            (true, true) => 0,
            (true, false) => 1,
            (false, true) => 2,
            (false, false) => 3,
        };
        let (selector, len) = self.selectors[table][usize::from(br.peek(8).to_le_bytes()[0])];
        if len == 0 {
            return Err(DriverError::MediumError);
        }
        br.skip(u32::from(len))?;
        match selector {
            0..=8 => {
                self.repeated_last = 0;
                self.avg_selector += u32::from(selector);
                self.avg_selector -= self.avg_selector >> 4;
                let index = usize::from(self.huf[2].decode(br)? & 0xFF);
                let offset = self.short_offsets[index];
                if index > 0 {
                    self.short_offsets[index] = self.short_offsets[index - 1];
                    self.short_offsets[index - 1] = offset;
                }
                let dist = u64::from(offset) + 1;
                let len = u32::from(selector) + 2;
                self.remember(dist);
                self.last_dist = dist;
                self.last_len = len;
                Self::copy(win, dist, len)
            }
            9 => {
                self.repeated_last += 1;
                Self::copy(win, self.last_dist, self.last_len)
            }
            10..=13 => {
                self.repeated_last = 0;
                let back = usize::from(selector - 9);
                let dist = self.old[self.old_at.wrapping_sub(back) & 3];
                let mut len = u32::from(self.len1.decode(br)?) + 2;
                if len == 0x101 && selector == 10 {
                    self.bug_fix = !self.bug_fix;
                    return Ok(());
                }
                if dist > 256 {
                    len += 1;
                }
                if dist >= self.max_offset {
                    len += 1;
                }
                self.remember(dist);
                self.last_dist = dist;
                self.last_len = len;
                Self::copy(win, dist, len)
            }
            _ => {
                self.repeated_last = 0;
                let len = u32::from(self.len2.decode(br)?) + 5;
                let dist = u64::from(br.bits(15)?) + 0x8000;
                self.last_dist = dist;
                self.last_len = len;
                Self::copy(win, dist, len)
            }
        }
    }
}
