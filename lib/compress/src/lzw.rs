//! compress's LZW: the coding Unix `compress` writes, which the ARC family's
//! dynamic LZW and RISC OS's Squash share. Codes start nine bits wide and
//! widen to a chosen most as the table fills, packed least significant bit
//! first in groups of eight that a widening or a clear pads out; code 256
//! clears the table.
//!
//! A coder keeps to whole groups, as a restartable caller of a group-framed
//! stream needs: a decoder takes its input a group at a time, waiting rather
//! than taking part of one while more is to come, and an encoder releases its
//! output a group at a time, holding a finished group its output has no room
//! for. Its tables are workspace the caller places and its own state a few
//! bytes it saves and restores, so a stream can stop after any call and go on
//! from what it saved alone. Restored state is checked before it is used, and
//! the tables as they are read, so workspace a stranger wrote can make a
//! coder refuse, never misbehave.

/// The narrowest code.
pub(crate) const MIN_BITS: u8 = 9;
/// The widest code compress's coding is written with.
pub(crate) const MAX_BITS: u8 = 16;

/// The code that empties the table.
pub(crate) const CLEAR: u32 = 256;
/// The first code a table assigns.
const FIRST: u32 = 257;
/// Codes in a group.
const GROUP: usize = 8;
/// The bytes in the widest group: eight codes take as many bytes as a code
/// takes bits.
pub(crate) const GROUP_BYTES: usize = MAX_BITS as usize;
/// A group with room past its end, which a code read at a group's last bits
/// reaches into.
const GROUP_ROOM: usize = GROUP_BYTES + 4;
/// The bytes a table entry takes: the code its string extends, then the byte
/// that extends it.
const ENTRY: usize = 3;
/// The fewest slots a dictionary has.
const MIN_SLOTS: usize = 1 << 10;
/// Input bytes between compress's checks of whether a full table still
/// pays.
const CHECK_GAP: u64 = 10_000;
/// Input bytes between Squash's clears of a full table, where a stream's
/// input comes in pieces.
const PERIOD: u64 = 1 << 15;
/// Past the input or bits any stream reaches: a saved encoder whose counters
/// claim more is a stranger's, and refused before they can overflow.
const COUNTED_MOST: u64 = 1 << 62;
/// A code saved as absent.
const NONE: u32 = u32::MAX;

/// Whose rule a decoder keeps for a stream's final, short group.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Dialect {
    /// Unix compress's, which the ARC family's methods keep: a code is read
    /// only whole.
    Compress,
    /// RISC OS Squash's: the group's last code is read even with one bit
    /// missing, read as zero.
    Squash,
}

impl Dialect {
    /// The bits a final short group may lack of its last code.
    const fn slack(self) -> usize {
        match self {
            Self::Compress => 0,
            Self::Squash => 1,
        }
    }
}

/// When an encoder clears its table.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Clearing {
    /// compress's, which the ARC family's methods keep: once the table is
    /// full, when a check every [`CHECK_GAP`] bytes finds it compressing
    /// worse than at the last.
    Worsening,
    /// Squash's where a stream's input comes in pieces: once the table is
    /// full, at a check every [`PERIOD`] bytes.
    Periodic,
    /// Squash's where a stream's whole input comes in one call: as the
    /// table's last code would be assigned, which it never is.
    AtLast,
}

impl Clearing {
    /// The code at which a table of `max_bits`-bit codes is full.
    const fn limit(self, max_bits: u32) -> u32 {
        match self {
            Self::Worsening | Self::Periodic => 1 << max_bits,
            Self::AtLast => (1 << max_bits) - 1,
        }
    }

    /// The input taken at the first check.
    const fn first_check(self) -> u64 {
        match self {
            Self::Worsening => CHECK_GAP,
            Self::Periodic | Self::AtLast => PERIOD,
        }
    }

    const fn code(self) -> u8 {
        match self {
            Self::Worsening => 0,
            Self::Periodic => 1,
            Self::AtLast => 2,
        }
    }

    const fn of(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Worsening),
            1 => Some(Self::Periodic),
            2 => Some(Self::AtLast),
            _ => None,
        }
    }
}

/// Why a coder's call returned.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Status {
    /// The stream ended, and everything it made is written.
    Done,
    /// It wants more input.
    Input,
    /// The output has no room for what comes next.
    Output,
}

/// A place a reader can resume a stream from: where its table was cleared,
/// counted in bytes from the start of the call's input and output.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Place {
    /// The plaintext byte the stream resumes with.
    pub plain: usize,
    /// The coded byte it resumes at.
    pub packed: usize,
}

/// What one call of a coder did.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Progress {
    /// Why it returned.
    pub status: Status,
    /// The input bytes it took.
    pub consumed: usize,
    /// The output bytes it wrote.
    pub produced: usize,
}

/// Why a coder refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Error {
    /// The stream codes what no table can: a first code past a byte, or a
    /// code past the next one the table makes.
    Corrupt,
    /// The workspace, or state restored, is not what a coder left.
    Workspace,
}

/// The workspace a decoder of `max_bits`-bit codes needs: its table, then the
/// stack a string is spelt onto.
pub(crate) const fn decoder_work(max_bits: u8) -> usize {
    let codes = 1usize << max_bits;
    ENTRY * (codes - FIRST as usize) + codes
}

/// The workspace an encoder of `max_bits`-bit codes needs for `len` bytes of
/// input: room for every string they can enter, so it codes them as the
/// largest dictionary would, and no more.
pub(crate) fn encoder_work(max_bits: u8, len: u64) -> usize {
    let most = 2usize << max_bits;
    let slots = usize::try_from(len)
        .ok()
        .filter(|&len| len < most / 2)
        .map_or(most, |len| (2 * len).next_power_of_two())
        .clamp(MIN_SLOTS, most);
    dictionary_bytes(u32::from(max_bits), slots)
}

/// The bytes a dictionary of `slots` slots takes: an entry for each code it
/// can assign before it is crowded or full, then its slots.
const fn dictionary_bytes(max_bits: u32, slots: usize) -> usize {
    let codes = (1usize << max_bits) - FIRST as usize;
    let entries = if codes < slots / 2 { codes } else { slots / 2 };
    ENTRY * entries + 2 * slots
}

/// The most slots a dictionary in `len` bytes of workspace has.
fn slots_in(max_bits: u32, len: usize) -> Option<usize> {
    let mut slots = 2usize << max_bits;
    while slots >= MIN_SLOTS {
        if dictionary_bytes(max_bits, slots) <= len {
            return Some(slots);
        }
        slots /= 2;
    }
    None
}

/// Where `code`'s table entry lies.
fn entry_at(code: u32) -> Result<usize, Error> {
    code.checked_sub(FIRST)
        .map(|index| ENTRY * index as usize)
        .ok_or(Error::Workspace)
}

/// The string table entry for `code`: the code its string extends and the
/// byte extending it.
fn entry(table: &[u8], code: u32) -> Result<(u32, u8), Error> {
    let at = entry_at(code)?;
    match table.get(at..at + ENTRY) {
        Some(&[low, high, byte]) => Ok((u32::from(u16::from_le_bytes([low, high])), byte)),
        _ => Err(Error::Workspace),
    }
}

/// Make `code` name the string `prefix` then `byte`.
fn set_entry(table: &mut [u8], code: u32, prefix: u32, byte: u8) -> Result<(), Error> {
    let at = entry_at(code)?;
    let slot = table.get_mut(at..at + ENTRY).ok_or(Error::Workspace)?;
    let prefix = u16::try_from(prefix).map_err(|_| Error::Workspace)?;
    slot[..2].copy_from_slice(&prefix.to_le_bytes());
    slot[2] = byte;
    Ok(())
}

/// Push the string `code` names onto `stack` above `top`, its last byte
/// first, answering the new top. A prefix is always an earlier code, so a
/// walk that finds otherwise has met a table no decoder wrote.
fn spell(table: &[u8], stack: &mut [u8], mut top: usize, mut code: u32) -> Result<usize, Error> {
    while code >= FIRST {
        let (prefix, byte) = entry(table, code)?;
        *stack.get_mut(top).ok_or(Error::Workspace)? = byte;
        top += 1;
        if prefix >= code {
            return Err(Error::Workspace);
        }
        code = prefix;
    }
    *stack.get_mut(top).ok_or(Error::Workspace)? =
        u8::try_from(code).map_err(|_| Error::Workspace)?;
    Ok(top + 1)
}

/// Fields saved in order: bytes, then little-endian words.
struct Saved<'a> {
    bytes: &'a mut [u8],
    at: usize,
}

impl Saved<'_> {
    fn put(&mut self, field: &[u8]) {
        self.bytes[self.at..self.at + field.len()].copy_from_slice(field);
        self.at += field.len();
    }

    /// A count or code below 2^32, which a state saves as a word; one that
    /// is not is saved as [`NONE`], which no restore believes.
    fn word(&mut self, value: usize) {
        self.put(&u32::try_from(value).unwrap_or(NONE).to_le_bytes());
    }
}

/// Fields restored in the order they were saved.
struct Restored<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Restored<'_> {
    fn take<const N: usize>(&mut self) -> [u8; N] {
        let mut field = [0; N];
        field.copy_from_slice(&self.bytes[self.at..self.at + N]);
        self.at += N;
        field
    }

    fn byte(&mut self) -> u8 {
        let [byte] = self.take();
        byte
    }

    fn word(&mut self) -> u32 {
        u32::from_le_bytes(self.take())
    }

    fn long(&mut self) -> u64 {
        u64::from_le_bytes(self.take())
    }

    fn code(&mut self) -> Option<u32> {
        Some(self.word()).filter(|&code| code != NONE)
    }
}

/// A decoder's state, the workspace aside.
#[derive(Clone, Debug)]
pub(crate) struct Decoding {
    dialect: Dialect,
    max_bits: u32,
    width: u32,
    free: u32,
    /// The code read last since the table was cleared.
    prev: Option<u32>,
    /// The first byte of the string `prev` names.
    first: u8,
    group: [u8; GROUP_ROOM],
    /// The bits of `group` read, and the bits it holds.
    offset: usize,
    limit: usize,
    /// The bytes of the last string still on the stack, emitted from its top.
    pending: usize,
}

/// The bytes [`Decoding::save`] writes.
pub(crate) const DECODING_STATE: usize = 4 + 3 * 4 + GROUP_ROOM;

impl Decoding {
    /// A decoder of codes growing to `max_bits`, or `None` for a width out of
    /// range.
    pub(crate) fn new(dialect: Dialect, max_bits: u8) -> Option<Self> {
        (MIN_BITS..=MAX_BITS).contains(&max_bits).then(|| Self {
            dialect,
            max_bits: u32::from(max_bits),
            width: u32::from(MIN_BITS),
            free: FIRST,
            prev: None,
            first: 0,
            group: [0; GROUP_ROOM],
            offset: 0,
            limit: 0,
            pending: 0,
        })
    }

    /// The bytes a whole group holds at the current width: what a caller
    /// feeding input a group at a time gathers.
    pub(crate) const fn group_bytes(&self) -> usize {
        self.width as usize
    }

    /// Decode from `input` into `out`, `more` saying whether input follows
    /// this, each place the table is cleared told to `cleared`. Input is
    /// taken a whole group at a time while more follows, the final short
    /// group only at the end.
    pub(crate) fn decode(
        &mut self,
        work: &mut [u8],
        input: &[u8],
        more: bool,
        out: &mut [u8],
        cleared: &mut dyn FnMut(Place),
    ) -> Result<Progress, Error> {
        let codes = 1usize << self.max_bits;
        let (table, stack) = work
            .get_mut(..ENTRY * (codes - FIRST as usize) + codes)
            .ok_or(Error::Workspace)?
            .split_at_mut(ENTRY * (codes - FIRST as usize));
        let mut progress = Progress {
            status: Status::Done,
            consumed: 0,
            produced: 0,
        };
        loop {
            if self.pending > 0 {
                let n = self.pending.min(out.len() - progress.produced);
                let spelt = stack
                    .get(self.pending - n..self.pending)
                    .ok_or(Error::Workspace)?;
                for (slot, &byte) in out[progress.produced..].iter_mut().zip(spelt.iter().rev()) {
                    *slot = byte;
                }
                self.pending -= n;
                progress.produced += n;
                if self.pending > 0 {
                    progress.status = Status::Output;
                    return Ok(progress);
                }
            }
            if self.offset + self.width as usize > self.limit {
                let whole = self.group_bytes();
                let take = match input.len() - progress.consumed {
                    left if left >= whole => whole,
                    _ if more => {
                        progress.status = Status::Input;
                        return Ok(progress);
                    }
                    0 => return Ok(progress),
                    left => left,
                };
                let from = progress.consumed;
                self.group[..take].copy_from_slice(&input[from..from + take]);
                self.group[take..].fill(0);
                progress.consumed += take;
                self.offset = 0;
                self.limit = 8 * take
                    + if take < whole {
                        self.dialect.slack()
                    } else {
                        0
                    };
                continue;
            }
            let code = self.next_code();
            if code == CLEAR {
                self.width = u32::from(MIN_BITS);
                self.free = FIRST;
                self.prev = None;
                self.offset = 0;
                self.limit = 0;
                cleared(Place {
                    plain: progress.produced,
                    packed: progress.consumed,
                });
                continue;
            }
            let Some(prev) = self.prev else {
                let byte = u8::try_from(code).map_err(|_| Error::Corrupt)?;
                self.prev = Some(code);
                self.first = byte;
                stack[0] = byte;
                self.pending = 1;
                continue;
            };
            let (top, walk_from) = match code {
                code if code < self.free => (0, code),
                code if code == self.free => {
                    stack[0] = self.first;
                    (1, prev)
                }
                _ => return Err(Error::Corrupt),
            };
            let top = spell(table, stack, top, walk_from)?;
            self.first = stack[top - 1];
            if (self.free as usize) < codes {
                set_entry(table, self.free, prev, self.first)?;
                self.free += 1;
                if self.free >= 1 << self.width && self.width < self.max_bits {
                    self.width += 1;
                    self.offset = 0;
                    self.limit = 0;
                }
            }
            self.prev = Some(code);
            self.pending = top;
        }
    }

    /// The code at the group's read bits.
    fn next_code(&mut self) -> u32 {
        let at = self.offset / 8;
        let word = u32::from_le_bytes([
            self.group[at],
            self.group[at + 1],
            self.group[at + 2],
            self.group[at + 3],
        ]);
        let code = (word >> (self.offset % 8)) & ((1 << self.width) - 1);
        self.offset += self.width as usize;
        code
    }

    /// Save the state into `to`.
    pub(crate) fn save(&self, to: &mut [u8; DECODING_STATE]) {
        let mut saved = Saved { bytes: to, at: 0 };
        saved.put(&[
            self.width.to_le_bytes()[0],
            self.offset.to_le_bytes()[0],
            self.limit.to_le_bytes()[0],
            self.first,
        ]);
        saved.put(&self.free.to_le_bytes());
        saved.put(&self.prev.unwrap_or(NONE).to_le_bytes());
        saved.word(self.pending);
        saved.put(&self.group);
    }

    /// The state `from` holds, as [`Self::save`] left it for a decoder of
    /// `dialect` and `max_bits`.
    pub(crate) fn restore(
        from: &[u8; DECODING_STATE],
        dialect: Dialect,
        max_bits: u8,
    ) -> Result<Self, Error> {
        let fresh = Self::new(dialect, max_bits).ok_or(Error::Workspace)?;
        let mut restored = Restored { bytes: from, at: 0 };
        let width = u32::from(restored.byte());
        let (offset, limit) = (usize::from(restored.byte()), usize::from(restored.byte()));
        let first = restored.byte();
        let (free, prev, pending) = (restored.word(), restored.code(), restored.word());
        let group = restored.take();
        let codes = 1u32 << fresh.max_bits;
        let valid = (u32::from(MIN_BITS)..=fresh.max_bits).contains(&width)
            && (FIRST..=codes).contains(&free)
            && free <= 1 << width
            && prev.is_none_or(|prev| prev < free)
            && offset <= limit
            && limit <= 8 * GROUP_BYTES + dialect.slack()
            && pending <= codes;
        if !valid {
            return Err(Error::Workspace);
        }
        Ok(Self {
            width,
            free,
            prev,
            first,
            group,
            offset,
            limit,
            pending: pending as usize,
            ..fresh
        })
    }
}

/// A finished group not yet written.
#[derive(Copy, Clone, Debug)]
struct Held {
    bytes: [u8; GROUP_BYTES],
    len: usize,
}

impl Held {
    const EMPTY: Self = Self {
        bytes: [0; GROUP_BYTES],
        len: 0,
    };
}

/// The most finished groups a step can leave held: the group a code ends and
/// the one a widening or a clear then pads out.
const HELD: usize = 2;

/// An encoder's state, the workspace aside.
#[derive(Clone, Debug)]
pub(crate) struct Encoding {
    clearing: Clearing,
    max_bits: u32,
    /// Dictionary slots: the most the workspace holds.
    slots: usize,
    width: u32,
    free: u32,
    /// The code of the string matched so far.
    string: Option<u32>,
    /// The codes of the group being filled.
    codes: [u32; GROUP],
    in_group: usize,
    held: [Held; HELD],
    held_count: usize,
    /// The final code is coded: only what is held is left.
    finished: bool,
    /// Input taken and bits coded, which the checks are reckoned from: the
    /// input taken at the next, and compress's ratio at the last.
    taken: u64,
    bits: u64,
    checkpoint: u64,
    ratio: u64,
}

/// The bytes [`Encoding::save`] writes.
pub(crate) const ENCODING_STATE: usize = 6 + 2 * 4 + 4 * GROUP + HELD * (1 + GROUP_BYTES) + 4 * 8;

impl Encoding {
    /// An encoder of codes growing to `max_bits` over `work`, its dictionary
    /// emptied, or `None` for a width out of range or workspace short of the
    /// smallest dictionary.
    pub(crate) fn new(clearing: Clearing, max_bits: u8, work: &mut [u8]) -> Option<Self> {
        if !(MIN_BITS..=MAX_BITS).contains(&max_bits) {
            return None;
        }
        let max_bits = u32::from(max_bits);
        let coding = Self {
            clearing,
            max_bits,
            slots: slots_in(max_bits, work.len())?,
            width: u32::from(MIN_BITS),
            free: FIRST,
            string: None,
            codes: [0; GROUP],
            in_group: 0,
            held: [Held::EMPTY; HELD],
            held_count: 0,
            finished: false,
            taken: 0,
            bits: 0,
            checkpoint: clearing.first_check(),
            ratio: 0,
        };
        coding.dictionary(work).ok()?.1.fill(0);
        Some(coding)
    }

    /// The workspace's string table and dictionary slots.
    fn dictionary<'w>(&self, work: &'w mut [u8]) -> Result<(&'w mut [u8], &'w mut [u8]), Error> {
        let len = dictionary_bytes(self.max_bits, self.slots);
        Ok(work
            .get_mut(..len)
            .ok_or(Error::Workspace)?
            .split_at_mut(len - 2 * self.slots))
    }

    /// Encode `input` into `out`, `more` saying whether input follows this,
    /// each place the table is cleared told to `cleared`; with no input to
    /// follow, the stream is ended once `input` is taken.
    pub(crate) fn encode(
        &mut self,
        work: &mut [u8],
        input: &[u8],
        more: bool,
        out: &mut [u8],
        cleared: &mut dyn FnMut(Place),
    ) -> Result<Progress, Error> {
        let (table, slots) = self.dictionary(work)?;
        let mut progress = Progress {
            status: Status::Output,
            consumed: 0,
            produced: self.release(out, 0),
        };
        if self.held_count > 0 {
            return Ok(progress);
        }
        if !self.finished {
            for &byte in input {
                progress.consumed += 1;
                self.taken += 1;
                let Some(string) = self.string else {
                    self.string = Some(u32::from(byte));
                    continue;
                };
                let slot = match self.find(table, slots, string, byte)? {
                    Ok(code) => {
                        self.string = Some(code);
                        continue;
                    }
                    Err(slot) => slot,
                };
                self.code(string);
                if self.free >= 1 << self.width && self.width < self.max_bits {
                    self.pad();
                    self.width += 1;
                }
                let codes_left = self.free < self.clearing.limit(self.max_bits);
                // At half load a probe stays short; a reader adds what a
                // writer adds, so a dictionary out of room clears rather
                // than skips.
                let crowded = (self.free - FIRST) as usize >= self.slots / 2;
                if codes_left && !crowded {
                    let code = u16::try_from(self.free).map_err(|_| Error::Workspace)?;
                    slots[2 * slot..2 * slot + 2].copy_from_slice(&code.to_le_bytes());
                    set_entry(table, self.free, string, byte)?;
                    self.free += 1;
                } else if codes_left || self.full_table_clears() {
                    self.code(CLEAR);
                    self.pad();
                    self.width = u32::from(MIN_BITS);
                    self.free = FIRST;
                    slots.fill(0);
                    cleared(Place {
                        plain: progress.consumed - 1,
                        packed: progress.produced + self.held_bytes(),
                    });
                }
                self.string = Some(u32::from(byte));
                progress.produced = self.release(out, progress.produced);
                if self.held_count > 0 {
                    return Ok(progress);
                }
            }
            if more {
                progress.status = Status::Input;
                return Ok(progress);
            }
            if let Some(string) = self.string.take() {
                self.code(string);
            }
            self.close(true);
            self.finished = true;
            progress.produced = self.release(out, progress.produced);
        }
        if self.held_count == 0 {
            progress.status = Status::Done;
        }
        Ok(progress)
    }

    /// The code of the string `prefix` then `byte`, else the empty
    /// dictionary slot it would take.
    fn find(
        &self,
        table: &[u8],
        slots: &[u8],
        prefix: u32,
        byte: u8,
    ) -> Result<Result<u32, usize>, Error> {
        let mask = self.slots - 1;
        let key = prefix << 8 | u32::from(byte);
        // The product's high bits, which every bit of the key reaches.
        let mut slot = (key.wrapping_mul(0x9E37_79B1) >> (32 - self.slots.ilog2())) as usize & mask;
        // A dictionary at most half full always has an empty slot.
        for _ in 0..self.slots {
            let code = u32::from(u16::from_le_bytes([slots[2 * slot], slots[2 * slot + 1]]));
            if code == 0 {
                return Ok(Err(slot));
            }
            if code >= self.free {
                return Err(Error::Workspace);
            }
            if entry(table, code)? == (prefix, byte) {
                return Ok(Ok(code));
            }
            slot = (slot + 1) & mask;
        }
        Err(Error::Workspace)
    }

    /// Whether the full table is cleared as a code is coded: at once, or at a
    /// check, compress's only once its ratio of input to output, scaled by
    /// 256, has fallen since the last.
    fn full_table_clears(&mut self) -> bool {
        match self.clearing {
            Clearing::AtLast => true,
            _ if self.taken < self.checkpoint => false,
            Clearing::Periodic => {
                self.checkpoint = self.taken + PERIOD;
                true
            }
            Clearing::Worsening => {
                self.checkpoint = self.taken + CHECK_GAP;
                let ratio = self.taken.saturating_mul(256) / (self.bits / 8).max(1);
                let worse = ratio <= self.ratio;
                self.ratio = if worse { 0 } else { ratio };
                worse
            }
        }
    }

    /// Add `code` to the group, finishing it once it holds eight.
    fn code(&mut self, code: u32) {
        self.codes[self.in_group] = code;
        self.in_group += 1;
        self.bits += u64::from(self.width);
        if self.in_group == GROUP {
            self.close(false);
        }
    }

    /// Pad the group out with zero codes, as a reader passes over the rest of
    /// it.
    fn pad(&mut self) {
        while self.in_group != 0 {
            self.code(0);
        }
    }

    /// Hold the group's codes as bytes: all of them, or for the stream's last
    /// group those its codes reach.
    fn close(&mut self, last: bool) {
        let mut group = Held::EMPTY;
        let mut acc = 0u32;
        let mut count = 0;
        for &code in &self.codes[..self.in_group] {
            acc |= (code & ((1 << self.width) - 1)) << count;
            count += self.width;
            while count >= 8 {
                group.bytes[group.len] = acc.to_le_bytes()[0];
                group.len += 1;
                acc >>= 8;
                count -= 8;
            }
        }
        if last && count > 0 {
            group.bytes[group.len] = acc.to_le_bytes()[0];
            group.len += 1;
        }
        self.in_group = 0;
        if group.len > 0 {
            self.held[self.held_count] = group;
            self.held_count += 1;
        }
    }

    /// The bytes of the groups held.
    fn held_bytes(&self) -> usize {
        self.held[..self.held_count]
            .iter()
            .map(|group| group.len)
            .sum()
    }

    /// Write the held groups into `out` from `produced` while each fits,
    /// answering how far it is written.
    fn release(&mut self, out: &mut [u8], mut produced: usize) -> usize {
        while self.held_count > 0 {
            let group = self.held[0];
            let Some(room) = out.get_mut(produced..produced + group.len) else {
                break;
            };
            room.copy_from_slice(&group.bytes[..group.len]);
            produced += group.len;
            self.held.copy_within(1.., 0);
            self.held_count -= 1;
        }
        produced
    }

    /// Save the state into `to`.
    pub(crate) fn save(&self, to: &mut [u8; ENCODING_STATE]) {
        let mut saved = Saved { bytes: to, at: 0 };
        saved.put(&[
            self.clearing.code(),
            self.width.to_le_bytes()[0],
            self.in_group.to_le_bytes()[0],
            self.held_count.to_le_bytes()[0],
            u8::from(self.finished),
            self.slots.ilog2().to_le_bytes()[0],
        ]);
        saved.put(&self.free.to_le_bytes());
        saved.put(&self.string.unwrap_or(NONE).to_le_bytes());
        for code in self.codes {
            saved.put(&code.to_le_bytes());
        }
        for group in &self.held {
            saved.put(&[group.len.to_le_bytes()[0]]);
            saved.put(&group.bytes);
        }
        for field in [self.taken, self.bits, self.checkpoint, self.ratio] {
            saved.put(&field.to_le_bytes());
        }
    }

    /// The state `from` holds, as [`Self::save`] left it for an encoder of
    /// `max_bits` over `work_len` bytes of workspace.
    pub(crate) fn restore(
        from: &[u8; ENCODING_STATE],
        max_bits: u8,
        work_len: usize,
    ) -> Result<Self, Error> {
        if !(MIN_BITS..=MAX_BITS).contains(&max_bits) {
            return Err(Error::Workspace);
        }
        let max_bits = u32::from(max_bits);
        let slots = slots_in(max_bits, work_len).ok_or(Error::Workspace)?;
        let mut restored = Restored { bytes: from, at: 0 };
        let clearing = Clearing::of(restored.byte()).ok_or(Error::Workspace)?;
        let width = u32::from(restored.byte());
        let (in_group, held_count) = (usize::from(restored.byte()), usize::from(restored.byte()));
        let (finished, slots_log) = (restored.byte(), restored.byte());
        let (free, string) = (restored.word(), restored.code());
        let mut codes = [0; GROUP];
        for code in &mut codes {
            *code = restored.word();
        }
        let mut held = [Held::EMPTY; HELD];
        for group in &mut held {
            group.len = usize::from(restored.byte());
            group.bytes = restored.take();
        }
        let (taken, bits, checkpoint, ratio) = (
            restored.long(),
            restored.long(),
            restored.long(),
            restored.long(),
        );
        let valid = (u32::from(MIN_BITS)..=max_bits).contains(&width)
            && (FIRST..=clearing.limit(max_bits)).contains(&free)
            && free <= 1 << width
            && string.is_none_or(|code| code < CLEAR || (FIRST..free).contains(&code))
            && in_group < GROUP
            && codes[..in_group].iter().all(|&code| code < 1 << width)
            && held_count <= HELD
            && held.iter().all(|group| group.len <= GROUP_BYTES)
            && finished <= 1
            && u32::from(slots_log) == slots.ilog2()
            && taken < COUNTED_MOST
            && bits < COUNTED_MOST
            && checkpoint <= taken + clearing.first_check();
        if !valid {
            return Err(Error::Workspace);
        }
        Ok(Self {
            clearing,
            max_bits,
            slots,
            width,
            free,
            string,
            codes,
            in_group,
            held,
            held_count,
            finished: finished == 1,
            taken,
            bits,
            checkpoint,
            ratio,
        })
    }
}

#[cfg(test)]
#[path = "lzw_tests.rs"]
mod tests;
