//! RAR 5's compression (its algorithm 0, and RAR 7's algorithm 1, which
//! reaches further with sixteen more distance codes): blocks of Huffman-coded
//! literals and matches, and the DELTA, x86 and ARM filters.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_abi::DriverError;

use super::bits::Reader;
use super::filter::{self, PENDING_MAX};
use super::huffman::Code;
use super::window::Window;
use super::Sink;

/// Main table symbols: 256 bytes, a filter, the last match, four repeated
/// distances, then 44 length slots.
const NC: usize = 306;
/// Low distance bits' symbols.
const LDC: usize = 16;
/// Length symbols of a repeated distance.
const RC: usize = 44;
/// Distance symbols: 64, or 80 for RAR 7's algorithm.
const DC_V0: usize = 64;
const DC_V1: usize = 80;
/// The largest table a block carries.
const MAX_TABLE: usize = NC + DC_V1 + LDC + RC;
/// The longest block a filter may cover.
const FILTER_MAX: u64 = 0x40_0000;

/// A filter a block defined: its kind and the stretch of the stream it
/// covers.
#[derive(Copy, Clone)]
struct Filter {
    start: u64,
    len: u64,
    kind: Kind,
}

#[derive(Copy, Clone)]
enum Kind {
    Delta(usize),
    E8,
    E8E9,
    Arm,
}

/// A RAR 5 decoder: its tables, distances and filters, carried from one
/// member of a solid run to the next.
pub(super) struct V50 {
    dc: usize,
    lengths: [u8; MAX_TABLE],
    main: Code,
    dist: Code,
    low: Code,
    len: Code,
    have_tables: bool,
    old: [u64; 4],
    last_len: u32,
    /// The bit at which the current block ends, and whether it is the
    /// member's last.
    block_end: u64,
    last_block: bool,
    /// The member's data has been decoded to its last block's end.
    finished: bool,
    filters: VecDeque<Filter>,
    /// Where the member's output began, and the stream position owed next.
    file_start: u64,
    owed: u64,
    /// A filtered block being given out.
    staged: Vec<u8>,
    staged_at: usize,
}

impl V50 {
    /// A decoder of RAR 5's algorithm, or RAR 7's where `v1`.
    pub(super) fn new(v1: bool) -> Self {
        Self {
            dc: if v1 { DC_V1 } else { DC_V0 },
            lengths: [0; MAX_TABLE],
            main: Code::empty(),
            dist: Code::empty(),
            low: Code::empty(),
            len: Code::empty(),
            have_tables: false,
            old: [0; 4],
            last_len: 0,
            block_end: 0,
            last_block: false,
            finished: true,
            filters: VecDeque::new(),
            file_start: 0,
            owed: 0,
            staged: Vec::new(),
            staged_at: 0,
        }
    }

    /// Stand at a member's first block, the window at the member's first
    /// byte. A member of no bytes and no data has no block.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for a first block out of shape.
    pub(super) fn begin(
        &mut self,
        br: &mut Reader<'_>,
        win: &Window,
        len_bits: u64,
    ) -> Result<(), DriverError> {
        self.file_start = win.pos();
        self.owed = win.pos();
        self.filters.clear();
        self.staged.clear();
        self.staged_at = 0;
        self.finished = len_bits == 0;
        if !self.finished {
            self.block_header(br, len_bits)?;
        }
        Ok(())
    }

    /// Give out the member's next bytes, up to `len` of them all.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for data out of shape or ending short.
    pub(super) fn read(
        &mut self,
        br: &mut Reader<'_>,
        win: &mut Window,
        len: u64,
        len_bits: u64,
        sink: &mut Sink<'_>,
    ) -> Result<(), DriverError> {
        let end = self
            .file_start
            .checked_add(len)
            .ok_or(DriverError::MediumError)?;
        while sink.wants() && self.owed < end {
            if self.staged_at < self.staged.len() {
                let taken = sink.give(&self.staged[self.staged_at..], end - self.owed);
                self.staged_at += taken;
                self.owed += taken as u64;
                continue;
            }
            if self.run_ready_filter(win)? {
                continue;
            }
            // A filter starting behind what was given out can never be run.
            if self.filters.front().is_some_and(|f| f.start < self.owed) {
                return Err(DriverError::MediumError);
            }
            let stop = self.filters.front().map_or(u64::MAX, |f| f.start);
            let upto = win.pos().min(stop).min(end);
            if upto > self.owed {
                self.owed += sink.give_window(win, self.owed, upto - self.owed)?;
                continue;
            }
            if self.finished {
                return Err(DriverError::MediumError);
            }
            let ahead = win.ahead(sink.room());
            self.decode(br, win, self.target(ahead), len_bits)?;
        }
        Ok(())
    }

    /// Decode the rest of the member's data, past the bytes it was asked
    /// for, so a member after it in a solid run starts where it should.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for data out of shape.
    pub(super) fn finish(
        &mut self,
        br: &mut Reader<'_>,
        win: &mut Window,
        len_bits: u64,
    ) -> Result<(), DriverError> {
        loop {
            self.owed = self.owed.max(self.staged_end());
            self.staged.clear();
            self.staged_at = 0;
            while let Some(&f) = self.filters.front() {
                if win.pos() < f.start + f.len {
                    break;
                }
                self.filters.pop_front();
                self.owed = self.owed.max(f.start + f.len);
            }
            if self.filters.is_empty() {
                self.owed = self.owed.max(win.pos());
            }
            if self.finished {
                return if self.filters.is_empty() {
                    Ok(())
                } else {
                    Err(DriverError::MediumError)
                };
            }
            // Always past where decoding stands: a filter pending beyond the
            // stretch leaves the stretch's target behind.
            let target = self
                .target(win.stretch())
                .max(win.pos().saturating_add(win.stretch()));
            self.decode(br, win, target, len_bits)?;
        }
    }

    /// Where the staged block ends in the stream.
    fn staged_end(&self) -> u64 {
        self.owed + (self.staged.len() - self.staged_at) as u64
    }

    /// How far to decode next: `ahead` past the bytes owed, and at least
    /// to the end of a filter that starts within it.
    fn target(&self, ahead: u64) -> u64 {
        let target = self.owed + ahead;
        match self.filters.front() {
            Some(f) if f.start <= target => target.max(f.start + f.len),
            _ => target,
        }
    }

    /// Run the first pending filter if output is owed from its start and
    /// its block is decoded, staging what it gives.
    fn run_ready_filter(&mut self, win: &Window) -> Result<bool, DriverError> {
        let Some(&f) = self.filters.front() else {
            return Ok(false);
        };
        if self.owed != f.start || win.pos() < f.start + f.len {
            return Ok(false);
        }
        self.filters.pop_front();
        let len = usize::try_from(f.len).map_err(|_| DriverError::MediumError)?;
        let mut block = tairix_util::fallible::filled(len, 0u8).ok_or(DriverError::OutOfMemory)?;
        win.read(f.start, &mut block)?;
        let file_pos = f.start - self.file_start;
        match f.kind {
            Kind::Delta(channels) => {
                let mut out =
                    tairix_util::fallible::filled(len, 0u8).ok_or(DriverError::OutOfMemory)?;
                filter::delta(&block, &mut out, channels);
                block = out;
            }
            Kind::E8 => filter::e8(&mut block, file_pos, false, 1 << 24),
            Kind::E8E9 => filter::e8(&mut block, file_pos, true, 1 << 24),
            Kind::Arm => filter::arm(&mut block, file_pos),
        }
        self.staged = block;
        self.staged_at = 0;
        Ok(true)
    }

    fn block_header(&mut self, br: &mut Reader<'_>, len_bits: u64) -> Result<(), DriverError> {
        br.align();
        let flags = br.byte()?;
        let check = br.byte()?;
        let size_len = ((flags >> 3) & 3) + 1;
        if size_len == 4 {
            return Err(DriverError::MediumError);
        }
        let mut size = 0u64;
        let mut sum = 0x5A ^ flags;
        for k in 0..size_len {
            let byte = br.byte()?;
            size |= u64::from(byte) << (8 * k);
            sum ^= byte;
        }
        if sum != check || size == 0 {
            return Err(DriverError::MediumError);
        }
        self.block_end = br.taken() + size * 8 - 8 + u64::from(flags & 7) + 1;
        if self.block_end > len_bits {
            return Err(DriverError::MediumError);
        }
        self.last_block = flags & 0x40 != 0;
        if flags & 0x80 != 0 {
            self.read_tables(br)
        } else if self.have_tables {
            Ok(())
        } else {
            Err(DriverError::MediumError)
        }
    }

    fn read_tables(&mut self, br: &mut Reader<'_>) -> Result<(), DriverError> {
        let mut pre = [0u8; 20];
        let mut i = 0;
        while i < pre.len() {
            let len = br.bits(4)?;
            if len == 15 {
                let zeros = br.bits(4)?;
                if zeros == 0 {
                    pre[i] = 15;
                    i += 1;
                } else {
                    i = (i + usize::try_from(zeros).unwrap_or(0) + 2).min(pre.len());
                }
            } else {
                pre[i] = len.to_le_bytes()[0];
                i += 1;
            }
        }
        let mut precode = Code::empty();
        precode.build(&pre)?;
        let total = NC + self.dc + LDC + RC;
        let lengths = &mut self.lengths[..total];
        lengths.fill(0);
        let mut i = 0;
        while i < total {
            let symbol = precode.decode(br)?;
            if symbol < 16 {
                lengths[i] = symbol.to_le_bytes()[0];
                i += 1;
                continue;
            }
            let run = match symbol {
                16 | 18 => br.bits(3)? + 3,
                _ => br.bits(7)? + 11,
            };
            let run = usize::try_from(run).unwrap_or(0).min(total - i);
            let value = if symbol < 18 {
                *i.checked_sub(1)
                    .and_then(|prev| lengths.get(prev))
                    .ok_or(DriverError::MediumError)?
            } else {
                0
            };
            lengths[i..i + run].fill(value);
            i += run;
        }
        let (main, rest) = lengths.split_at(NC);
        let (dist, rest) = rest.split_at(self.dc);
        let (low, len) = rest.split_at(LDC);
        self.main.build(main)?;
        self.dist.build(dist)?;
        self.low.build(low)?;
        self.len.build(len)?;
        self.have_tables = true;
        Ok(())
    }

    /// Decode until the window reaches `target`, or the member's data ends.
    fn decode(
        &mut self,
        br: &mut Reader<'_>,
        win: &mut Window,
        target: u64,
        len_bits: u64,
    ) -> Result<(), DriverError> {
        while win.pos() < target {
            win.has_room(
                self.owed
                    .min(self.filters.front().map_or(u64::MAX, |f| f.start)),
            )?;
            if br.taken() >= self.block_end {
                if self.last_block {
                    self.finished = true;
                    return Ok(());
                }
                self.block_header(br, len_bits)?;
                continue;
            }
            let symbol = self.main.decode(br)?;
            match symbol {
                0..=255 => win.literal(symbol.to_le_bytes()[0]),
                256 => self.define_filter(br, win)?,
                257 => {
                    if self.last_len != 0 {
                        win.copy(self.old[0], self.last_len)?;
                    }
                }
                258..=261 => {
                    let at = usize::from(symbol - 258);
                    let dist = self.old[at];
                    self.old.copy_within(0..at, 1);
                    self.old[0] = dist;
                    let slot = self.len.decode(br)?;
                    let len = slot_length(br, slot)?;
                    self.last_len = len;
                    win.copy(dist, len)?;
                }
                _ => {
                    let mut len = slot_length(br, symbol - 262)?;
                    let dist = self.distance(br)?;
                    len += u32::from(dist > 0x100)
                        + u32::from(dist > 0x2000)
                        + u32::from(dist > 0x4_0000);
                    self.old.copy_within(0..3, 1);
                    self.old[0] = dist;
                    self.last_len = len;
                    win.copy(dist, len)?;
                }
            }
        }
        Ok(())
    }

    fn distance(&self, br: &mut Reader<'_>) -> Result<u64, DriverError> {
        let slot = u32::from(self.dist.decode(br)?);
        if slot < 4 {
            return Ok(u64::from(slot) + 1);
        }
        let bits = slot / 2 - 1;
        let mut dist = (u64::from(2 | (slot & 1)) << bits) + 1;
        if bits >= 4 {
            if bits > 4 {
                dist += long_bits(br, bits - 4)? << 4;
            }
            dist += u64::from(self.low.decode(br)?);
        } else {
            dist += u64::from(br.bits(bits)?);
        }
        Ok(dist)
    }

    fn define_filter(&mut self, br: &mut Reader<'_>, win: &Window) -> Result<(), DriverError> {
        let start = filter_int(br)? + win.pos();
        let len = filter_int(br)?;
        let kind = match br.bits(3)? {
            0 => Kind::Delta(usize::try_from(br.bits(5)?).unwrap_or(0) + 1),
            1 => Kind::E8,
            2 => Kind::E8E9,
            3 => Kind::Arm,
            _ => return Err(DriverError::MediumError),
        };
        if !(4..=FILTER_MAX).contains(&len) || self.filters.len() >= PENDING_MAX {
            return Err(DriverError::MediumError);
        }
        let prev_end = self.filters.back().map_or(self.owed, |f| f.start + f.len);
        if start < prev_end {
            return Err(DriverError::MediumError);
        }
        self.filters
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        self.filters.push_back(Filter { start, len, kind });
        Ok(())
    }
}

/// A match length its slot and extra bits give.
fn slot_length(br: &mut Reader<'_>, slot: u16) -> Result<u32, DriverError> {
    let slot = u32::from(slot);
    if slot < 8 {
        return Ok(slot + 2);
    }
    let bits = slot / 4 - 1;
    Ok(((4 | (slot & 3)) << bits) + 2 + br.bits(bits)?)
}

/// A filter's start or length: one to four bytes, least significant first.
fn filter_int(br: &mut Reader<'_>) -> Result<u64, DriverError> {
    let count = br.bits(2)? + 1;
    let mut value = 0u64;
    for k in 0..count {
        value |= u64::from(br.byte()?) << (8 * k);
    }
    Ok(value)
}

/// `n` bits, up to sixty-four.
fn long_bits(br: &mut Reader<'_>, n: u32) -> Result<u64, DriverError> {
    if n <= 32 {
        return Ok(u64::from(br.bits(n)?));
    }
    let high = u64::from(br.bits(n - 32)?);
    Ok(high << 32 | u64::from(br.bits(32)?))
}
