//! RAR 3's compression: LZ77 over up to four mebibytes, coded through
//! Huffman tables, switching block by block to PPMd; and the filters a
//! stream defines over a stretch of its output, run here when their
//! bytecode is one of the six standard programs, recognised by its CRC and
//! length — no general virtual machine is run, and any other program is
//! refused.

use alloc::collections::VecDeque;
use alloc::vec::Vec;

use tairix_abi::DriverError;

use super::bits::Reader;
use super::filter;
use super::filter::PENDING_MAX;
use super::huffman::Code;
use super::lz::{DIST, DIST_SLOTS, LEN_BASE, LEN_BITS, LEN_SLOTS, SHORT_BASE, SHORT_BITS};
use super::ppmd::{Coder, Model, MAX_ORDER};
use super::window::Window;
use super::Sink;

/// Main table symbols: 256 bytes, the end of a block, a filter, the last
/// match, four repeated distances, eight short distances, then 28 length
/// slots.
const NC: usize = 299;
/// Distance symbols.
const DC: usize = DIST_SLOTS;
/// Low distance bits' symbols.
const LDC: usize = 17;
/// Length symbols of a repeated distance.
const RC: usize = LEN_SLOTS;
const TABLE: usize = NC + DC + LDC + RC;

/// The filter virtual machine's work area: the largest block a filter takes.
const VM_WORK: u64 = 0x3_C000;
/// The most global data a filter carries.
const VM_GLOBAL_MAX: u64 = 0x2000 - 0x40;
/// The most filter programs a stream defines.
const PROGRAMS_MAX: usize = 1024;
/// The most bytes PPMd may decode past a member's end looking for its end
/// marker.
const END_SEARCH: u64 = 16;

/// A filter's seven registers: the work area's size, the block's length and
/// the program's usage count, then any the definition sets.
fn registers(
    bytes: &mut VmBits<'_>,
    flags: u8,
    len: u64,
    usage: u32,
) -> Result<[u32; 7], DriverError> {
    let mut regs = [0u32; 7];
    regs[3] = narrow(VM_WORK);
    regs[4] = narrow(len);
    regs[5] = usage;
    if flags & 0x10 != 0 {
        let mask = bytes.bits(7)?;
        for (k, reg) in regs.iter_mut().enumerate() {
            if mask & (1 << k) != 0 {
                *reg = bytes.number()?;
            }
        }
    }
    Ok(regs)
}

/// A new filter program: its bytecode, held to the XOR its first byte
/// records, recognised as a standard one where it is.
fn read_program(bytes: &mut VmBits<'_>) -> Result<Program, DriverError> {
    let code_len = usize::try_from(bytes.number()?).unwrap_or(0);
    if !(1..=0x1_0000).contains(&code_len) {
        return Err(DriverError::MediumError);
    }
    let mut code = tairix_util::fallible::filled(code_len, 0u8).ok_or(DriverError::OutOfMemory)?;
    for byte in &mut code {
        *byte = bytes.byte()?;
    }
    if code[1..].iter().fold(0u8, |sum, &byte| sum ^ byte) != code[0] {
        return Err(DriverError::MediumError);
    }
    Ok(Program {
        standard: Standard::of(&code),
        last_len: 0,
        usage: 0,
    })
}

/// The standard programs, by the CRC-32 and length of their bytecode.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Standard {
    E8,
    E8E9,
    Itanium,
    Delta,
    Rgb,
    Audio,
}

impl Standard {
    fn of(code: &[u8]) -> Option<Self> {
        Some(match (tairix_crc32::checksum(code), code.len()) {
            (0xAD57_6887, 53) => Self::E8,
            (0x3CD7_E57E, 57) => Self::E8E9,
            (0x3769_893F, 120) => Self::Itanium,
            (0x0E06_077D, 29) => Self::Delta,
            (0x1C2C_5DC8, 149) => Self::Rgb,
            (0xBC85_E701, 216) => Self::Audio,
            _ => return None,
        })
    }
}

/// A filter program a stream defined.
struct Program {
    /// The standard program it is, or `None` for one not run here.
    standard: Option<Standard>,
    last_len: u64,
    usage: u32,
}

/// A filter pending over a stretch of the stream.
struct Filter {
    program: usize,
    start: u64,
    len: u64,
    regs: [u32; 7],
}

/// How a block's symbols are coded.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Coding {
    Lz,
    Ppm,
}

/// What stopped a run of decoding.
#[derive(Copy, Clone, Eq, PartialEq)]
enum Event {
    /// The window reached the limit asked for.
    Limit,
    /// The member's end marker.
    End,
}

/// A RAR 3 decoder, carried from one member of a solid run to the next.
pub(super) struct V29 {
    lengths: [u8; TABLE],
    main: Code,
    dist: Code,
    low: Code,
    len: Code,
    old: [u64; 4],
    last_dist: u64,
    last_len: u32,
    low_repeats: u32,
    prev_low: u64,
    model: Option<Model>,
    coder: Coder,
    escape: u8,
    coding: Coding,
    programs: Vec<Program>,
    filters: VecDeque<Filter>,
    last_filter: usize,
    need_tables: bool,
    have_tables: bool,
    /// Where the last filter run ended: a new one may not start before it.
    filtered_to: u64,
    /// The block the filter defined last covers: one defined after it over
    /// the same block chains onto it.
    last_block: Option<(u64, u64)>,
    /// The stream position owed next, and the current member's span.
    owed: u64,
    file_start: u64,
    file_end: u64,
    /// The member's end marker has been met.
    ended: bool,
    /// The member's data, in bits.
    len_bits: u64,
    staged: Vec<u8>,
    staged_at: usize,
}

impl V29 {
    pub(super) fn new() -> Self {
        Self {
            lengths: [0; TABLE],
            main: Code::empty(),
            dist: Code::empty(),
            low: Code::empty(),
            len: Code::empty(),
            old: [0; 4],
            last_dist: 0,
            last_len: 0,
            low_repeats: 0,
            prev_low: 0,
            model: None,
            coder: Coder::default(),
            escape: 2,
            coding: Coding::Lz,
            programs: Vec::new(),
            filters: VecDeque::new(),
            last_filter: 0,
            need_tables: true,
            have_tables: false,
            filtered_to: 0,
            last_block: None,
            owed: 0,
            file_start: 0,
            file_end: 0,
            ended: false,
            len_bits: 0,
            staged: Vec::new(),
            staged_at: 0,
        }
    }

    /// Stand at a member of `len` bytes whose data is `packed` bytes,
    /// reading its tables where the stream needs them.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for tables out of shape, or a member
    /// with bytes and no data.
    pub(super) fn begin(
        &mut self,
        br: &mut Reader<'_>,
        _win: &Window,
        len: u64,
        packed: u64,
    ) -> Result<(), DriverError> {
        self.file_start = self.owed;
        self.file_end = self.owed.checked_add(len).ok_or(DriverError::MediumError)?;
        self.ended = false;
        self.len_bits = packed.checked_mul(8).ok_or(DriverError::MediumError)?;
        self.staged.clear();
        self.staged_at = 0;
        if self.need_tables || !self.have_tables {
            if packed == 0 {
                return if len == 0 {
                    self.ended = true;
                    Ok(())
                } else {
                    Err(DriverError::MediumError)
                };
            }
            self.read_tables(br)?;
        }
        Ok(())
    }

    /// Give out the member's next bytes.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for data out of shape or ending short.
    pub(super) fn read(
        &mut self,
        br: &mut Reader<'_>,
        win: &mut Window,
        sink: &mut Sink<'_>,
    ) -> Result<(), DriverError> {
        let end = self.file_end;
        while sink.wants() && self.owed < end {
            if self.staged_at < self.staged.len() {
                let taken = sink.give(&self.staged[self.staged_at..], end - self.owed);
                self.staged_at += taken;
                self.owed += taken as u64;
                continue;
            }
            // A filter starting behind what was given out can never be run.
            if self.filters.front().is_some_and(|f| f.start < self.owed) {
                return Err(DriverError::MediumError);
            }
            if self.filters.front().is_some_and(|f| f.start == self.owed) {
                let Some(first) = self.filters.pop_front() else {
                    continue;
                };
                // Filters the decoding below defines start past this one, or
                // chain onto it.
                let need = first.start + first.len;
                self.filtered_to = need;
                if win.pos() < need {
                    if self.ended {
                        return Err(DriverError::MediumError);
                    }
                    if self.step(br, win, need)? == Event::End {
                        self.ended = true;
                    }
                    if win.pos() < need {
                        return Err(DriverError::MediumError);
                    }
                }
                self.run_filters(&first, win)?;
                continue;
            }
            let stop = self.filters.front().map_or(u64::MAX, |f| f.start);
            let upto = win.pos().min(stop).min(end);
            if upto > self.owed {
                self.owed += sink.give_window(win, self.owed, upto - self.owed)?;
                continue;
            }
            if self.ended {
                return Err(DriverError::MediumError);
            }
            let limit = stop.min(end).min(self.owed + win.ahead(sink.room()));
            if self.step(br, win, limit)? == Event::End {
                self.ended = true;
            }
        }
        Ok(())
    }

    /// End the member: its end marker read where one follows, so the next
    /// member of the run starts in step.
    ///
    /// # Errors
    ///
    /// [`DriverError::MediumError`] for data out of shape.
    pub(super) fn finish(
        &mut self,
        br: &mut Reader<'_>,
        win: &mut Window,
    ) -> Result<(), DriverError> {
        if !self.ended {
            self.end_marker(br, win)?;
        }
        self.owed = win.pos();
        self.staged.clear();
        self.staged_at = 0;
        Ok(())
    }

    /// Past the member's bytes, read the end marker its data may hold.
    fn end_marker(&mut self, br: &mut Reader<'_>, win: &mut Window) -> Result<(), DriverError> {
        if self.coding == Coding::Ppm {
            // The model must see the marker to stay in step with its
            // encoder; a marker not found soon leaves the next member to
            // read tables afresh.
            let before = win.pos();
            let mut event = Event::Limit;
            while event == Event::Limit && win.pos() - before < END_SEARCH {
                let at = win.pos();
                event = match self.step(br, win, at + 1) {
                    Ok(event) => event,
                    Err(_) => break,
                };
                // A filter starting here lets no symbol be decoded.
                if event == Event::Limit && win.pos() == at {
                    break;
                }
            }
            if event != Event::End {
                self.need_tables = true;
            }
            return Ok(());
        }
        // An LZ marker is the end-of-block symbol, a zero bit, and whether
        // the next member reads tables; anything else is left unread.
        let left = self.len_bits.saturating_sub(br.taken());
        if let Some((256, len)) = self.main.resolve(br.peek(15)) {
            if u64::from(len) + 2 <= left {
                let after = br.peek(len + 2);
                if after & 2 == 0 {
                    br.skip(len + 2)?;
                    self.need_tables = after & 1 == 1;
                    return Ok(());
                }
            }
        }
        self.need_tables = false;
        Ok(())
    }

    fn read_tables(&mut self, br: &mut Reader<'_>) -> Result<(), DriverError> {
        br.align();
        if br.bit()? {
            let flags = br.bits(7)?;
            let max_mb = if flags & 0x20 != 0 {
                Some(br.bits(8)?)
            } else {
                None
            };
            if flags & 0x40 != 0 {
                self.escape = br.byte()?;
            }
            if let Some(max_mb) = max_mb {
                let mut order = (flags & 0x1F) + 1;
                if order > 16 {
                    order = 16 + (order - 16) * 3;
                }
                if order == 1 || order > MAX_ORDER {
                    return Err(DriverError::MediumError);
                }
                let heap = self.model.take().map(Model::into_heap).unwrap_or_default();
                self.model = Some(Model::new((max_mb + 1) << 20, order, heap)?);
            } else if self.model.is_none() {
                return Err(DriverError::MediumError);
            }
            self.coder = Coder::start(br)?;
            self.coding = Coding::Ppm;
        } else {
            if !br.bit()? {
                self.lengths.fill(0);
            }
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
                        let run = (usize::try_from(zeros).unwrap_or(0) + 2).min(pre.len() - i);
                        pre[i..i + run].fill(0);
                        i += run;
                    }
                } else {
                    pre[i] = len.to_le_bytes()[0];
                    i += 1;
                }
            }
            let mut precode = Code::empty();
            precode.build(&pre)?;
            let mut i = 0;
            while i < TABLE {
                let symbol = precode.decode(br)?;
                if symbol < 16 {
                    self.lengths[i] = (self.lengths[i] + symbol.to_le_bytes()[0]) & 15;
                    i += 1;
                    continue;
                }
                let run = match symbol {
                    16 | 18 => br.bits(3)? + 3,
                    _ => br.bits(7)? + 11,
                };
                let run = usize::try_from(run).unwrap_or(0).min(TABLE - i);
                let value = if symbol < 18 {
                    *i.checked_sub(1)
                        .and_then(|prev| self.lengths.get(prev))
                        .ok_or(DriverError::MediumError)?
                } else {
                    0
                };
                self.lengths[i..i + run].fill(value);
                i += run;
            }
            let (main, rest) = self.lengths.split_at(NC);
            let (dist, rest) = rest.split_at(DC);
            let (low, len) = rest.split_at(LDC);
            self.main.build(main)?;
            self.dist.build(dist)?;
            self.low.build(low)?;
            self.len.build(len)?;
            self.low_repeats = 0;
            self.prev_low = 0;
            self.coding = Coding::Lz;
        }
        self.have_tables = true;
        self.need_tables = false;
        Ok(())
    }

    /// Decode until the window reaches `limit`, stopping short of a pending
    /// filter's start, or at the member's end marker.
    fn step(
        &mut self,
        br: &mut Reader<'_>,
        win: &mut Window,
        mut limit: u64,
    ) -> Result<Event, DriverError> {
        while win.pos() < limit {
            if let Some(f) = self.filters.front() {
                if f.start < limit {
                    limit = f.start;
                    if win.pos() >= limit {
                        break;
                    }
                }
            }
            win.has_room(self.owed)?;
            if self.coding == Coding::Ppm {
                if self.ppm_symbol(br, win)? == Event::End {
                    return Ok(Event::End);
                }
                continue;
            }
            let symbol = self.main.decode(br)?;
            match symbol {
                0..=255 => win.literal(symbol.to_le_bytes()[0]),
                256 => {
                    if br.bit()? {
                        self.read_tables(br)?;
                    } else {
                        self.need_tables = br.bit()?;
                        return Ok(Event::End);
                    }
                }
                257 => {
                    let flags = br.byte()?;
                    let count = match (flags & 7) + 1 {
                        7 => usize::from(br.byte()?) + 7,
                        8 => usize::try_from(br.bits(16)?).unwrap_or(0),
                        n => usize::from(n),
                    };
                    let mut code = tairix_util::fallible::filled(count, 0u8)
                        .ok_or(DriverError::OutOfMemory)?;
                    for byte in &mut code {
                        *byte = br.byte()?;
                    }
                    self.define_filter(flags, &code, win)?;
                }
                258 => {
                    if self.last_len != 0 {
                        win.copy(self.last_dist, self.last_len)?;
                    }
                }
                259..=262 => {
                    let at = usize::from(symbol - 259);
                    let dist = self.old[at];
                    let slot = usize::from(self.len.decode(br)?);
                    let len = LEN_BASE.get(slot).ok_or(DriverError::MediumError)?
                        + 2
                        + br.bits(LEN_BITS[slot])?;
                    self.old.copy_within(0..at, 1);
                    self.old[0] = dist;
                    self.matched(win, dist, len)?;
                }
                263..=270 => {
                    let slot = usize::from(symbol - 263);
                    let dist = SHORT_BASE[slot] + 1 + u64::from(br.bits(SHORT_BITS[slot])?);
                    self.old.copy_within(0..3, 1);
                    self.old[0] = dist;
                    self.matched(win, dist, 2)?;
                }
                _ => {
                    let slot = usize::from(symbol - 271);
                    let mut len = LEN_BASE.get(slot).ok_or(DriverError::MediumError)?
                        + 3
                        + br.bits(LEN_BITS[slot])?;
                    let dist = self.distance(br)?;
                    if dist >= 0x2000 {
                        len += 1;
                        if dist >= 0x4_0000 {
                            len += 1;
                        }
                    }
                    self.old.copy_within(0..3, 1);
                    self.old[0] = dist;
                    self.matched(win, dist, len)?;
                }
            }
        }
        Ok(Event::Limit)
    }

    fn matched(&mut self, win: &mut Window, dist: u64, len: u32) -> Result<(), DriverError> {
        self.last_dist = dist;
        self.last_len = len;
        win.copy(dist, len)
    }

    fn distance(&mut self, br: &mut Reader<'_>) -> Result<u64, DriverError> {
        let slot = usize::from(self.dist.decode(br)?);
        let &(base, bits) = DIST.get(slot).ok_or(DriverError::MediumError)?;
        let mut dist = base + 1;
        if bits == 0 {
            return Ok(dist);
        }
        if slot <= 9 {
            return Ok(dist + u64::from(br.bits(bits)?));
        }
        if bits > 4 {
            dist += u64::from(br.bits(bits - 4)?) << 4;
        }
        if self.low_repeats > 0 {
            self.low_repeats -= 1;
            dist += self.prev_low;
        } else {
            let low = self.low.decode(br)?;
            if low == 16 {
                self.low_repeats = 15;
                dist += self.prev_low;
            } else {
                dist += u64::from(low);
                self.prev_low = u64::from(low);
            }
        }
        Ok(dist)
    }

    fn ppm_symbol(&mut self, br: &mut Reader<'_>, win: &mut Window) -> Result<Event, DriverError> {
        let symbol = self.ppm_byte(br)?;
        if symbol != self.escape {
            win.literal(symbol);
            return Ok(Event::Limit);
        }
        match self.ppm_byte(br)? {
            0 => self.read_tables(br)?,
            2 => {
                self.need_tables = true;
                return Ok(Event::End);
            }
            3 => {
                let flags = self.ppm_byte(br)?;
                let count = match (flags & 7) + 1 {
                    7 => usize::from(self.ppm_byte(br)?) + 7,
                    8 => usize::from(self.ppm_byte(br)?) << 8 | usize::from(self.ppm_byte(br)?),
                    n => usize::from(n),
                };
                let mut code =
                    tairix_util::fallible::filled(count, 0u8).ok_or(DriverError::OutOfMemory)?;
                for byte in &mut code {
                    *byte = self.ppm_byte(br)?;
                }
                self.define_filter(flags, &code, win)?;
            }
            4 => {
                let mut dist = 0u64;
                for _ in 0..3 {
                    dist = dist << 8 | u64::from(self.ppm_byte(br)?);
                }
                let len = u32::from(self.ppm_byte(br)?);
                win.copy(dist + 2, len + 32)?;
            }
            5 => {
                let len = u32::from(self.ppm_byte(br)?);
                win.copy(1, len + 4)?;
            }
            _ => win.literal(self.escape),
        }
        Ok(Event::Limit)
    }

    fn ppm_byte(&mut self, br: &mut Reader<'_>) -> Result<u8, DriverError> {
        self.model
            .as_mut()
            .ok_or(DriverError::MediumError)?
            .decode(&mut self.coder, br)
    }

    /// Take a filter's definition: its program, the stretch it covers, and
    /// its registers.
    fn define_filter(&mut self, flags: u8, code: &[u8], win: &Window) -> Result<(), DriverError> {
        let mut bytes = VmBits { bytes: code, at: 0 };
        let number = if flags & 0x80 != 0 {
            let mut number = usize::try_from(bytes.number()?).unwrap_or(usize::MAX);
            if number == 0 {
                self.programs.clear();
                self.filters.clear();
                self.last_block = None;
            } else {
                number -= 1;
            }
            if number > self.programs.len() || number > PROGRAMS_MAX {
                return Err(DriverError::MediumError);
            }
            self.last_filter = number;
            number
        } else {
            self.last_filter
        };
        let fresh = number >= self.programs.len();
        let mut start = u64::from(bytes.number()?) + win.pos();
        if flags & 0x40 != 0 {
            start += 258;
        }
        let len = if flags & 0x20 != 0 {
            u64::from(bytes.number()?)
        } else if fresh {
            0
        } else {
            self.programs[number].last_len
        };
        if len > VM_WORK {
            return Err(DriverError::MediumError);
        }
        let usage = if fresh {
            0
        } else {
            self.programs[number].usage.wrapping_add(1)
        };
        let regs = registers(&mut bytes, flags, len, usage)?;
        if fresh {
            let program = read_program(&mut bytes)?;
            self.programs
                .try_reserve(1)
                .map_err(|_| DriverError::OutOfMemory)?;
            self.programs.push(program);
        }
        let number = if fresh {
            self.programs.len() - 1
        } else {
            number
        };
        let program = self
            .programs
            .get_mut(number)
            .ok_or(DriverError::MediumError)?;
        program.usage = usage;
        program.last_len = len;
        if flags & 0x08 != 0 {
            let global = u64::from(bytes.number()?);
            if global > VM_GLOBAL_MAX {
                return Err(DriverError::MediumError);
            }
            for _ in 0..global {
                bytes.byte()?;
            }
        }
        let prev_end = self
            .filters
            .back()
            .map_or(self.filtered_to, |f| f.start + f.len);
        // One over the block of the one defined before it runs on what that
        // one gives, pending or running still.
        let chained = self.last_block == Some((start, len));
        if (start < prev_end && !chained) || self.filters.len() >= PENDING_MAX {
            return Err(DriverError::MediumError);
        }
        self.last_block = Some((start, len));
        self.filters
            .try_reserve(1)
            .map_err(|_| DriverError::OutOfMemory)?;
        self.filters.push_back(Filter {
            program: number,
            start,
            len,
            regs,
        });
        Ok(())
    }

    /// Run `first`, whose stretch starts where output is owed, and any
    /// filter chained onto the same stretch, staging what they give.
    fn run_filters(&mut self, first: &Filter, win: &Window) -> Result<(), DriverError> {
        let len = usize::try_from(first.len).map_err(|_| DriverError::MediumError)?;
        let mut block = tairix_util::fallible::filled(len, 0u8).ok_or(DriverError::OutOfMemory)?;
        win.read(first.start, &mut block)?;
        let file_pos = first.start.wrapping_sub(self.file_start);
        block = self.apply(first, block, file_pos)?;
        while let Some(next) = self.filters.front() {
            if next.start != first.start || next.len != block.len() as u64 {
                break;
            }
            let Some(next) = self.filters.pop_front() else {
                break;
            };
            block = self.apply(&next, block, file_pos)?;
        }
        self.staged = block;
        self.staged_at = 0;
        Ok(())
    }

    fn apply(&self, f: &Filter, mut block: Vec<u8>, file_pos: u64) -> Result<Vec<u8>, DriverError> {
        let program = self
            .programs
            .get(f.program)
            .ok_or(DriverError::MediumError)?;
        let standard = program.standard.ok_or(DriverError::Unsupported)?;
        let n = block.len() as u64;
        if u64::from(f.regs[4]) != n {
            return Err(DriverError::MediumError);
        }
        let channels = usize::try_from(f.regs[0]).unwrap_or(0);
        match standard {
            Standard::E8 | Standard::E8E9 => {
                if !(4..=VM_WORK).contains(&n) {
                    return Err(DriverError::MediumError);
                }
                filter::e8(&mut block, file_pos, standard == Standard::E8E9, 1 << 32);
            }
            Standard::Itanium => {
                if n > VM_WORK {
                    return Err(DriverError::MediumError);
                }
                filter::itanium(&mut block, file_pos);
            }
            Standard::Delta | Standard::Audio => {
                if n > VM_WORK / 2 || !(1..=128).contains(&channels) {
                    return Err(DriverError::MediumError);
                }
                let mut out = tairix_util::fallible::filled(block.len(), 0u8)
                    .ok_or(DriverError::OutOfMemory)?;
                if standard == Standard::Delta {
                    filter::delta(&block, &mut out, channels);
                } else {
                    filter::audio(&block, &mut out, channels);
                }
                block = out;
            }
            Standard::Rgb => {
                if n > VM_WORK / 2 {
                    return Err(DriverError::MediumError);
                }
                let stride = usize::try_from(f.regs[0]).unwrap_or(0);
                let red = usize::try_from(f.regs[1]).unwrap_or(usize::MAX);
                let mut out = tairix_util::fallible::filled(block.len(), 0u8)
                    .ok_or(DriverError::OutOfMemory)?;
                filter::rgb(&block, &mut out, stride, red)?;
                block = out;
            }
        }
        Ok(block)
    }
}

/// A filter definition's bytes, read most significant bit first.
struct VmBits<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl VmBits<'_> {
    fn bits(&mut self, n: usize) -> Result<u32, DriverError> {
        let mut value = 0u32;
        for _ in 0..n {
            let byte = *self
                .bytes
                .get(self.at / 8)
                .ok_or(DriverError::MediumError)?;
            value = value << 1 | u32::from((byte >> (7 - self.at % 8)) & 1);
            self.at += 1;
        }
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, DriverError> {
        Ok(self.bits(8)?.to_le_bytes()[0])
    }

    /// A number as filter definitions code them: two bits choose four,
    /// eight (or a small negative), sixteen or thirty-two more.
    fn number(&mut self) -> Result<u32, DriverError> {
        match self.bits(2)? {
            0 => self.bits(4),
            1 => {
                let value = self.bits(8)?;
                if value >= 16 {
                    Ok(value)
                } else {
                    Ok(0xFFFF_FF00 | value << 4 | self.bits(4)?)
                }
            }
            2 => self.bits(16),
            _ => self.bits(32),
        }
    }
}

/// A count within the filter machine's work area, as a register.
fn narrow(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rar::bits::{Bits, Source};

    struct Empty;

    impl Source for Empty {
        fn chunk(&mut self) -> &[u8] {
            &[]
        }

        fn consume(&mut self, _: usize) {}
    }

    /// A filter left starting behind the bytes given out — the member before
    /// having decoded past its end — ends the read with a fault rather than
    /// waiting on the filter forever.
    #[test]
    fn a_filter_behind_what_was_given_out_ends_the_read() {
        let mut decoder = V29::new();
        decoder.filters.push_back(Filter {
            program: 0,
            start: 12,
            len: 4,
            regs: [0; 7],
        });
        decoder.owed = 15;
        decoder.file_end = 25;
        let mut win = Window::new(0x40_0000, 64).unwrap();
        for byte in 0..15u8 {
            win.literal(byte);
        }
        let mut bits = Bits::default();
        let mut source = Empty;
        let mut br = Reader::new(&mut bits, &mut source);
        let mut out = [0u8; 10];
        let mut sink = Sink::new(&mut out);
        assert_eq!(
            decoder.read(&mut br, &mut win, &mut sink),
            Err(DriverError::MediumError)
        );
    }

    /// A filter definition's code: each number in the two-bit-sized form
    /// filter definitions code them, here in four or eight bits.
    fn vm_code(numbers: &[u32]) -> Vec<u8> {
        let mut bits: Vec<bool> = Vec::new();
        for &number in numbers {
            let (selector, width) = if number < 16 { (0u32, 4) } else { (1, 8) };
            for k in (0..2).rev() {
                bits.push(selector >> k & 1 != 0);
            }
            for k in (0..width).rev() {
                bits.push(number >> k & 1 != 0);
            }
        }
        bits.chunks(8)
            .map(|byte| {
                byte.iter()
                    .enumerate()
                    .fold(0u8, |out, (k, &bit)| out | u8::from(bit) << (7 - k))
            })
            .collect()
    }

    /// A decoder knowing the x86 programs, E8 numbered one and E8E9 two.
    fn knowing_x86() -> V29 {
        let mut decoder = V29::new();
        for standard in [Standard::E8, Standard::E8E9] {
            decoder.programs.push(Program {
                standard: Some(standard),
                last_len: 0,
                usage: 0,
            });
        }
        decoder
    }

    /// A definition of program `number` over `len` bytes at the window's
    /// position.
    fn define(decoder: &mut V29, number: u32, len: u32, win: &Window) -> Result<(), DriverError> {
        decoder.define_filter(0x80 | 0x20, &vm_code(&[number, 0, len]), win)
    }

    #[test]
    fn filters_over_one_block_run_one_on_the_others_output() {
        let mut decoder = knowing_x86();
        let mut win = Window::new(0x40_0000, 64).unwrap();
        let plain: Vec<u8> = (0..32u8)
            .map(|k| [0xE8, 0xE9, k, 1][usize::from(k % 4)])
            .collect();
        define(&mut decoder, 1, 32, &win).unwrap();
        define(&mut decoder, 2, 32, &win).unwrap();
        assert_eq!(
            define(&mut decoder, 1, 16, &win),
            Err(DriverError::MediumError),
            "an overlap that chains onto nothing"
        );
        for &byte in &plain {
            win.literal(byte);
        }
        decoder.file_end = 32;
        let mut bits = Bits::default();
        let mut source = Empty;
        let mut br = Reader::new(&mut bits, &mut source);
        let mut out = [0u8; 32];
        let mut sink = Sink::new(&mut out);
        decoder.read(&mut br, &mut win, &mut sink).unwrap();
        let mut want = plain.clone();
        filter::e8(&mut want, 0, false, 1 << 32);
        filter::e8(&mut want, 0, true, 1 << 32);
        assert_eq!(out.to_vec(), want);
    }

    /// One defined over the block of a filter running already — taken off
    /// the queue as its block is decoded — chains onto it.
    #[test]
    fn a_filter_chains_onto_the_one_whose_block_is_being_decoded() {
        let mut decoder = knowing_x86();
        let win = Window::new(0x40_0000, 64).unwrap();
        define(&mut decoder, 1, 32, &win).unwrap();
        let running = decoder.filters.pop_front().unwrap();
        decoder.filtered_to = running.start + running.len;
        define(&mut decoder, 2, 32, &win).unwrap();
        assert_eq!(decoder.filters.len(), 1);
        assert_eq!(
            define(&mut decoder, 1, 32, &win),
            Ok(()),
            "a third on the same block"
        );
        decoder.filters.clear();
        define(&mut decoder, 2, 16, &win).unwrap_err();
    }
}
