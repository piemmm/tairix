//! bzip2: the Burrows–Wheeler block compressor's stream format, decoded and
//! encoded.
//!
//! A stream is `BZh` and a level digit, then blocks of up to a hundred
//! thousand bytes a level, each its own Burrows–Wheeler transform, move-to-
//! front and run coding and Huffman coding, then an end marker carrying the
//! stream's combined CRC. Blocks begin at bit offsets and decode independently
//! of one another, so a reader that recorded where each began can start at
//! any of them.
//!
//! Both directions run over workspace the caller places — a block's
//! transform is four bytes a symbol, up to 900 000 symbols — so the crate
//! still allocates nothing. Every count, length, table and pointer in a
//! stream is checked before it is used; a stream out of shape is refused,
//! never trusted, and never a panic.

use crate::{Input, Output};

/// A block's magic, the digits of pi.
pub const BLOCK_MAGIC: u64 = 0x3141_5926_5359;
/// The end-of-stream magic, the square root of pi.
pub const END_MAGIC: u64 = 0x1772_4538_5090;

/// The symbols one level of block size holds.
const LEVEL_SIZE: usize = 100_000;
/// The largest block, at level 9.
pub const MAX_BLOCK: usize = 9 * LEVEL_SIZE;
/// The longest Huffman code a stream may carry.
const MAX_CODE_LEN: usize = 20;
/// The most selectors a block carries; more are read and ignored, as
/// bzip2 1.0.8 does.
const MAX_SELECTORS: usize = 18_002;
/// Symbols coded with one table before the next selector.
const GROUP_SIZE: usize = 50;
/// The most coding tables a block carries.
const MAX_GROUPS: usize = 6;
/// The largest alphabet: 256 bytes, the two run symbols, less one byte,
/// plus the end of block.
const MAX_ALPHA: usize = 258;

/// `value`, which the format bounds below `u32`'s range: a block holds at
/// most [`MAX_BLOCK`] symbols, and every count and index in one is less.
fn narrow(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// `value`, a field of at most 32 bits.
fn narrow64(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// The low byte of `value`.
const fn low_byte(value: u64) -> u8 {
    value.to_le_bytes()[0]
}

/// Why a bzip2 stream was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// The stream ended inside a block or before its end marker.
    UnexpectedEof,
    /// A header, table, symbol, pointer or count the stream gives is out of
    /// the format's shape.
    Corrupt,
    /// A block's CRC did not match the bytes it decoded to, or the stream's
    /// combined CRC did not match its blocks'.
    Checksum,
    /// A block of the format's old randomised kind, which no encoder in use
    /// writes.
    Randomised,
    /// A block larger than the workspace the decoder was given.
    WorkspaceTooSmall,
    /// The encoder's output refused its bytes.
    OutputRefused,
}

/// The CRC-32 bzip2 checks with: polynomial `0x04C11DB7`, most significant
/// bit first.
const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0u32;
    while i < 256 {
        let mut crc = i << 24;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
            bit += 1;
        }
        table[i as usize] = crc;
        i += 1;
    }
    table
}

static CRC_TABLE: [u32; 256] = crc_table();

fn crc_update(crc: u32, byte: u8) -> u32 {
    (crc << 8) ^ CRC_TABLE[usize::from(((crc >> 24) as u8) ^ byte)]
}

/// The stream's combined CRC once a block of CRC `block` is added.
const fn combine(combined: u32, block: u32) -> u32 {
    combined.rotate_left(1) ^ block
}

/// A stream's bits, most significant first.
#[derive(Default)]
struct Bits {
    held: u64,
    count: u32,
    /// Bits taken since the decoder began.
    taken: u64,
}

impl Bits {
    fn take(&mut self, input: &mut dyn Input, n: u32) -> Result<u32, Error> {
        while self.count < n {
            let byte = input.byte().ok_or(Error::UnexpectedEof)?;
            self.held = (self.held << 8) | u64::from(byte);
            self.count += 8;
        }
        self.count -= n;
        self.taken += u64::from(n);
        let mask = (1u64 << n) - 1;
        Ok(narrow64((self.held >> self.count) & mask))
    }

    fn bit(&mut self, input: &mut dyn Input) -> Result<bool, Error> {
        self.take(input, 1).map(|bit| bit == 1)
    }

    /// Skip to the next byte boundary.
    fn align(&mut self) {
        let drop = self.count % 8;
        self.count -= drop;
        self.taken += u64::from(drop);
    }
}

/// One coding table as a decoder walks it: bzip2's canonical form.
struct Table {
    limit: [i32; MAX_CODE_LEN + 2],
    base: [i32; MAX_CODE_LEN + 2],
    perm: [u16; MAX_ALPHA],
    min_len: u32,
}

impl Table {
    const EMPTY: Self = Self {
        limit: [0; MAX_CODE_LEN + 2],
        base: [0; MAX_CODE_LEN + 2],
        perm: [0; MAX_ALPHA],
        min_len: 0,
    };

    /// The table whose symbols have `lengths`.
    fn build(lengths: &[u8]) -> Self {
        let mut table = Self::EMPTY;
        let min = u32::from(*lengths.iter().min().unwrap_or(&1));
        let max = u32::from(*lengths.iter().max().unwrap_or(&1));
        let mut pp = 0;
        for len in min..=max {
            for (symbol, &l) in lengths.iter().enumerate() {
                if u32::from(l) == len {
                    table.perm[pp] = u16::try_from(symbol).unwrap_or(u16::MAX);
                    pp += 1;
                }
            }
        }
        let mut base = [0i32; MAX_CODE_LEN + 2];
        for &l in lengths {
            base[usize::from(l) + 1] += 1;
        }
        for i in 1..base.len() {
            base[i] += base[i - 1];
        }
        let mut code = 0i32;
        for len in min..=max {
            let len = len as usize;
            code += base[len + 1] - base[len];
            table.limit[len] = code - 1;
            code <<= 1;
        }
        for len in min + 1..=max {
            let len = len as usize;
            base[len] = ((table.limit[len - 1] + 1) << 1) - base[len];
        }
        table.base = base;
        table.min_len = min;
        table
    }

    fn decode(&self, bits: &mut Bits, input: &mut dyn Input, alpha: usize) -> Result<usize, Error> {
        let mut len = self.min_len;
        let mut code = i32::try_from(bits.take(input, len)?).map_err(|_| Error::Corrupt)?;
        loop {
            if len as usize > MAX_CODE_LEN {
                return Err(Error::Corrupt);
            }
            if code <= self.limit[len as usize] {
                break;
            }
            len += 1;
            code = (code << 1) | i32::from(bits.bit(input)?);
        }
        let index = code - self.base[len as usize];
        match usize::try_from(index) {
            Ok(index) if index < alpha => Ok(usize::from(self.perm[index])),
            _ => Err(Error::Corrupt),
        }
    }
}

/// What a block's header says of how its symbols are coded.
struct Tables {
    seq_to_byte: [u8; 256],
    in_use: usize,
    alpha: usize,
    /// How many of the decoder's selectors this block uses.
    selectors: usize,
    coding: [Table; MAX_GROUPS],
}

/// Where a decoder stands in its stream.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Stage {
    /// Before a block or the end marker.
    Between,
    /// Emitting a decoded block.
    Block,
    /// Past the end marker.
    Ended,
}

/// How many `u32` of workspace a [`Decoder`] needs to hold every block a
/// stream at `level` can have: one a block symbol.
#[must_use]
pub const fn decoder_work(level: u8) -> usize {
    level as usize * LEVEL_SIZE
}

/// The most symbols a block decoding to `len` bytes holds: the first coding
/// spells four repeats and their count in five bytes, so a block runs to at
/// most five quarters of what it decodes to.
#[must_use]
pub const fn block_symbols(len: u64) -> u64 {
    len.saturating_add(len.div_ceil(4))
}

/// A bzip2 decoder over the `u32` workspace `W` holds, one `u32` a block
/// symbol: [`decoder_work`] for the stream's level holds every block it can
/// have, which [`MAX_BLOCK`] covers for every level; a smaller one decodes
/// the blocks it holds and refuses a larger one.
pub struct Decoder<W> {
    work: W,
    bits: Bits,
    level: u32,
    stage: Stage,
    // The block being emitted.
    position: u32,
    left: u32,
    last: Option<u8>,
    seen: u32,
    repeat: u32,
    repeat_byte: u8,
    crc: u32,
    expected: u32,
    combined: u32,
    /// Whether every block of the stream has been decoded here, so the end
    /// marker's combined CRC can be held to them.
    whole: bool,
    /// Bits of the next byte to pass over before the block a resume names.
    pending_skip: u8,
    /// How many of each byte the block holds.
    counts: [u32; 256],
    /// Which table codes each group of the block's symbols.
    selectors: [u8; MAX_SELECTORS],
}

/// What [`Decoder::next_block`] found.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Next {
    /// A block, now ready to read, whose magic began at this bit of the
    /// decoder's input.
    Block {
        /// The bit offset, counted from where the decoder began.
        at: u64,
    },
    /// The stream's end marker: the decoder stands on the byte after it.
    End,
}

impl<W: AsMut<[u32]>> Decoder<W> {
    /// A decoder reading a stream from its first byte, `BZh` and the level.
    // The selectors are the format's, and this crate links no allocator, so
    // the value is returned for the caller to place.
    #[allow(clippy::large_stack_arrays)]
    pub fn new(work: W) -> Self {
        Self {
            work,
            bits: Bits::default(),
            level: 0,
            stage: Stage::Between,
            position: 0,
            left: 0,
            last: None,
            seen: 0,
            repeat: 0,
            repeat_byte: 0,
            crc: 0,
            expected: 0,
            combined: 0,
            whole: true,
            pending_skip: 0,
            counts: [0; 256],
            selectors: [0; MAX_SELECTORS],
        }
    }

    /// Read the stream header — `BZh` and its level — answering the level.
    ///
    /// # Errors
    ///
    /// [`Error::Corrupt`] for bytes that open no bzip2 stream.
    pub fn begin(&mut self, input: &mut dyn Input) -> Result<u8, Error> {
        for &want in b"BZh" {
            if self.bits.take(input, 8)? != u32::from(want) {
                return Err(Error::Corrupt);
            }
        }
        let level = self.bits.take(input, 8)?;
        if !(u32::from(b'1')..=u32::from(b'9')).contains(&level) {
            return Err(Error::Corrupt);
        }
        self.level = level - u32::from(b'0');
        self.stage = Stage::Between;
        self.combined = 0;
        self.whole = true;
        Ok(low_byte(u64::from(self.level)))
    }

    /// Stand at a block of a stream of `level` whose magic begins `skip`
    /// bits into `input`'s next byte: what a reader resumes from. The end
    /// marker's combined CRC is not held to blocks this decoder did not see.
    pub fn resume(&mut self, level: u8, skip: u8) -> Result<(), Error> {
        if !(1..=9).contains(&level) || skip >= 8 {
            return Err(Error::Corrupt);
        }
        self.bits = Bits::default();
        self.level = u32::from(level);
        self.stage = Stage::Between;
        self.whole = false;
        self.pending_skip = skip;
        Ok(())
    }

    /// Bits taken from the input since the decoder began.
    #[must_use]
    pub fn taken(&self) -> u64 {
        self.bits.taken
    }

    /// Decode the next block into the workspace, or read the end marker.
    ///
    /// # Errors
    ///
    /// See [`Error`].
    pub fn next_block(&mut self, input: &mut dyn Input) -> Result<Next, Error> {
        if self.stage != Stage::Between {
            return Err(Error::Corrupt);
        }
        if self.pending_skip > 0 {
            let skip = u32::from(self.pending_skip);
            self.pending_skip = 0;
            self.bits.take(input, skip)?;
            self.bits.taken = 0;
        }
        let at = self.bits.taken;
        let magic =
            u64::from(self.bits.take(input, 24)?) << 24 | u64::from(self.bits.take(input, 24)?);
        if magic == END_MAGIC {
            let stored = self.bits.take(input, 32)?;
            if self.whole && stored != self.combined {
                return Err(Error::Checksum);
            }
            self.bits.align();
            self.stage = Stage::Ended;
            return Ok(Next::End);
        }
        if magic != BLOCK_MAGIC {
            return Err(Error::Corrupt);
        }
        self.expected = self.bits.take(input, 32)?;
        if self.bits.bit(input)? {
            return Err(Error::Randomised);
        }
        let origin = self.bits.take(input, 24)?;
        let len = self.read_block(input)?;
        if origin >= len {
            return Err(Error::Corrupt);
        }
        self.invert(origin, len)?;
        self.left = len;
        self.last = None;
        self.seen = 0;
        self.repeat = 0;
        self.crc = 0xFFFF_FFFF;
        self.stage = Stage::Block;
        Ok(Next::Block { at })
    }

    /// The symbols of one block, after its magic, CRC, flag and origin: its
    /// tables, then its move-to-front coded transform, left in the
    /// workspace's low bytes. Answers how many.
    fn read_block(&mut self, input: &mut dyn Input) -> Result<u32, Error> {
        let tables = self.read_tables(input)?;
        self.read_symbols(input, &tables)
    }

    /// A block's byte map, coding tables and selectors.
    fn read_tables(&mut self, input: &mut dyn Input) -> Result<Tables, Error> {
        let bits = &mut self.bits;
        let groups_used = bits.take(input, 16)?;
        let mut used = [false; 256];
        for group in 0..16 {
            if groups_used & (0x8000 >> group) != 0 {
                let mask = bits.take(input, 16)?;
                for (i, slot) in used[group * 16..group * 16 + 16].iter_mut().enumerate() {
                    *slot = mask & (0x8000 >> i) != 0;
                }
            }
        }
        let mut seq_to_byte = [0u8; 256];
        let mut in_use = 0;
        for (byte, &is) in used.iter().enumerate() {
            if is {
                seq_to_byte[in_use] = u8::try_from(byte).unwrap_or(u8::MAX);
                in_use += 1;
            }
        }
        if in_use == 0 {
            return Err(Error::Corrupt);
        }
        let alpha = in_use + 2;
        let groups = bits.take(input, 3)? as usize;
        if !(2..=MAX_GROUPS).contains(&groups) {
            return Err(Error::Corrupt);
        }
        let stated = bits.take(input, 15)? as usize;
        if stated == 0 {
            return Err(Error::Corrupt);
        }
        let mut order: [u8; MAX_GROUPS] = [0, 1, 2, 3, 4, 5];
        for i in 0..stated {
            let mut j = 0;
            while bits.bit(input)? {
                j += 1;
                if j >= groups {
                    return Err(Error::Corrupt);
                }
            }
            if let Some(slot) = self.selectors.get_mut(i) {
                let chosen = order[j];
                order.copy_within(0..j, 1);
                order[0] = chosen;
                *slot = chosen;
            }
        }
        let mut tables = Tables {
            seq_to_byte,
            in_use,
            alpha,
            selectors: stated.min(MAX_SELECTORS),
            coding: [
                Table::EMPTY,
                Table::EMPTY,
                Table::EMPTY,
                Table::EMPTY,
                Table::EMPTY,
                Table::EMPTY,
            ],
        };
        for table in tables.coding.iter_mut().take(groups) {
            let mut lengths = [0u8; MAX_ALPHA];
            let mut current = bits.take(input, 5)?;
            for length in lengths.iter_mut().take(alpha) {
                loop {
                    if !(1..=narrow(MAX_CODE_LEN)).contains(&current) {
                        return Err(Error::Corrupt);
                    }
                    if !bits.bit(input)? {
                        break;
                    }
                    if bits.bit(input)? {
                        current -= 1;
                    } else {
                        current += 1;
                    }
                }
                *length = low_byte(u64::from(current));
            }
            *table = Table::build(&lengths[..alpha]);
        }
        Ok(tables)
    }

    /// A block's move-to-front and run coded symbols, decoded through
    /// `tables` into the workspace. Answers how many.
    fn read_symbols(&mut self, input: &mut dyn Input, tables: &Tables) -> Result<u32, Error> {
        let level_limit = decoder_work(low_byte(u64::from(self.level)));
        let work = self.work.as_mut();
        let limit = work.len().min(level_limit);
        // A block past its level is no block; one past the workspace is
        // refused for the workspace.
        let past = |end: usize| {
            if end > level_limit {
                Error::Corrupt
            } else {
                Error::WorkspaceTooSmall
            }
        };
        let bits = &mut self.bits;
        let selectors = &self.selectors[..tables.selectors];
        let end_of_block = tables.in_use + 1;
        let mut mtf: [u8; 256] = core::array::from_fn(|i| u8::try_from(i).unwrap_or(u8::MAX));
        let mut counts = [0u32; 256];
        let mut len = 0usize;
        let mut group = 0usize;
        let mut group_left = 0usize;
        let mut table = &tables.coding[0];
        let mut next = |bits: &mut Bits, input: &mut dyn Input| -> Result<usize, Error> {
            if group_left == 0 {
                let selector = *selectors.get(group).ok_or(Error::Corrupt)?;
                table = &tables.coding[usize::from(selector)];
                group += 1;
                group_left = GROUP_SIZE;
            }
            group_left -= 1;
            table.decode(bits, input, tables.alpha)
        };
        let mut symbol = next(bits, input)?;
        loop {
            if symbol == end_of_block {
                break;
            }
            if symbol <= 1 {
                // A run of the front byte, its length in bijective base two.
                let mut run = 0usize;
                let mut weight = 1usize;
                while symbol <= 1 {
                    run += (symbol + 1) * weight;
                    weight <<= 1;
                    if weight > 2 * 1024 * 1024 {
                        return Err(Error::Corrupt);
                    }
                    symbol = next(bits, input)?;
                }
                let byte = tables.seq_to_byte[usize::from(mtf[0])];
                if len + run > limit {
                    return Err(past(len + run));
                }
                counts[usize::from(byte)] += narrow(run);
                work[len..len + run].fill(u32::from(byte));
                len += run;
                continue;
            }
            if len >= limit {
                return Err(past(len + 1));
            }
            let at = symbol - 1;
            if at >= tables.in_use {
                return Err(Error::Corrupt);
            }
            let front = mtf[at];
            mtf.copy_within(0..at, 1);
            mtf[0] = front;
            let byte = tables.seq_to_byte[usize::from(front)];
            counts[usize::from(byte)] += 1;
            work[len] = u32::from(byte);
            len += 1;
            symbol = next(bits, input)?;
        }
        self.counts = counts;
        Ok(narrow(len))
    }

    /// Lay the inverse transform's links over the block's `len` symbols in
    /// the workspace, ready to walk from `origin`.
    fn invert(&mut self, origin: u32, len: u32) -> Result<(), Error> {
        let work = self.work.as_mut();
        let mut starts = [0u32; 256];
        let mut sum = 0u32;
        for (start, &count) in starts.iter_mut().zip(self.counts.iter()) {
            *start = sum;
            sum += count;
        }
        if sum != len {
            return Err(Error::Corrupt);
        }
        for i in 0..len as usize {
            let byte = (work[i] & 0xFF) as usize;
            let slot = starts[byte] as usize;
            work[slot] |= narrow(i) << 8;
            starts[byte] += 1;
        }
        self.position = work[origin as usize] >> 8;
        Ok(())
    }

    /// Decode bytes of the current block into `out`, answering how many;
    /// zero once the block is done, its CRC then checked, and the decoder
    /// standing before the next block or the end marker.
    ///
    /// # Errors
    ///
    /// [`Error::Checksum`] for a block whose bytes do not match its CRC.
    pub fn read(&mut self, out: &mut [u8]) -> Result<usize, Error> {
        if self.stage != Stage::Block {
            return Ok(0);
        }
        let work = self.work.as_mut();
        let mut produced = 0;
        while produced < out.len() {
            if self.repeat > 0 {
                out[produced] = self.repeat_byte;
                self.crc = crc_update(self.crc, self.repeat_byte);
                self.repeat -= 1;
                produced += 1;
                continue;
            }
            if self.left == 0 {
                break;
            }
            let link = *work.get(self.position as usize).ok_or(Error::Corrupt)?;
            self.position = link >> 8;
            let byte = (link & 0xFF) as u8;
            self.left -= 1;
            if self.seen == 4 {
                // After four equal bytes, a count of more.
                self.repeat = u32::from(byte);
                self.repeat_byte = self.last.unwrap_or(0);
                self.seen = 0;
                self.last = None;
                continue;
            }
            if self.last == Some(byte) {
                self.seen += 1;
            } else {
                self.last = Some(byte);
                self.seen = 1;
            }
            out[produced] = byte;
            self.crc = crc_update(self.crc, byte);
            produced += 1;
        }
        if produced == 0 && self.left == 0 && self.repeat == 0 {
            let crc = !self.crc;
            if crc != self.expected {
                return Err(Error::Checksum);
            }
            self.combined = combine(self.combined, crc);
            self.stage = Stage::Between;
        }
        Ok(produced)
    }

    /// Whether the decoder stands past its stream's end marker.
    #[must_use]
    pub fn ended(&self) -> bool {
        self.stage == Stage::Ended
    }
}

/// The longest code the encoder makes, as bzip2 does: well within what a
/// decoder takes.
const ENCODE_MAX_LEN: u8 = 17;

/// How many `u32` of workspace an [`Encoder`] at `level` needs: five a block
/// symbol, for the transform's sort.
#[must_use]
pub const fn encoder_work(level: u8) -> usize {
    5 * level as usize * LEVEL_SIZE
}

/// How many bytes of block buffer an [`Encoder`] at `level` needs.
#[must_use]
pub const fn encoder_block(level: u8) -> usize {
    level as usize * LEVEL_SIZE
}

/// Bits the encoder writes, most significant first, gathered into bytes.
struct BitWriter {
    held: u64,
    count: u32,
    buf: [u8; 4096],
    fill: usize,
    written: u64,
}

impl BitWriter {
    const fn new() -> Self {
        Self {
            held: 0,
            count: 0,
            buf: [0; 4096],
            fill: 0,
            written: 0,
        }
    }

    fn put(&mut self, out: &mut dyn Output, n: u32, value: u32) -> Result<(), Error> {
        self.held = (self.held << n) | (u64::from(value) & ((1u64 << n) - 1));
        self.count += n;
        self.written += u64::from(n);
        while self.count >= 8 {
            self.count -= 8;
            self.buf[self.fill] = low_byte(self.held >> self.count);
            self.fill += 1;
            if self.fill == self.buf.len() {
                self.drain(out)?;
            }
        }
        Ok(())
    }

    fn drain(&mut self, out: &mut dyn Output) -> Result<(), Error> {
        if self.fill > 0 && !out.put(&self.buf[..self.fill]) {
            return Err(Error::OutputRefused);
        }
        self.fill = 0;
        Ok(())
    }

    /// Pad to a byte with zeros and write everything gathered.
    fn finish(&mut self, out: &mut dyn Output) -> Result<(), Error> {
        let pad = (8 - self.count % 8) % 8;
        self.put(out, pad, 0)?;
        self.drain(out)
    }
}

/// Where an encoder's blocks begin, for a writer that indexes them.
pub trait BlockIndex {
    /// A block holding the plaintext from byte `plain` begins at bit `bit` of
    /// the stream.
    fn block(&mut self, plain: u64, bit: u64);
}

/// A bzip2 encoder over the `u32` workspace `W` ([`encoder_work`]) and the
/// block buffer `B` ([`encoder_block`]).
pub struct Encoder<W, B> {
    work: W,
    block: B,
    level: u8,
    limit: usize,
    len: usize,
    run_byte: u8,
    run_len: u32,
    block_crc: u32,
    combined: u32,
    bits: BitWriter,
    /// Plaintext taken in all, and where the block being gathered began.
    plain: u64,
    block_plain: u64,
    /// The coding of the block being written.
    coding: Coding,
}

impl<W: AsMut<[u32]>, B: AsMut<[u8]>> Encoder<W, B> {
    /// An encoder at `level`, 1 to 9, over `work` and `block`.
    ///
    /// # Errors
    ///
    /// [`Error::Corrupt`] for a level out of range;
    /// [`Error::WorkspaceTooSmall`] for workspace short of what the level
    /// needs.
    // The coding's selectors are the format's, and this crate links no
    // allocator, so the value is returned for the caller to place.
    #[allow(clippy::large_stack_arrays)]
    pub fn new(mut work: W, mut block: B, level: u8) -> Result<Self, Error> {
        if !(1..=9).contains(&level) {
            return Err(Error::Corrupt);
        }
        if work.as_mut().len() < encoder_work(level) || block.as_mut().len() < encoder_block(level)
        {
            return Err(Error::WorkspaceTooSmall);
        }
        Ok(Self {
            work,
            block,
            level,
            // Room for one more run, whatever its length.
            limit: encoder_block(level) - 19,
            len: 0,
            run_byte: 0,
            run_len: 0,
            block_crc: 0xFFFF_FFFF,
            combined: 0,
            bits: BitWriter::new(),
            plain: 0,
            block_plain: 0,
            coding: Coding {
                groups: 0,
                lengths: [[0; MAX_ALPHA]; MAX_GROUPS],
                selectors: [0; MAX_SELECTORS],
                selector_count: 0,
            },
        })
    }

    /// Compress `input`, writing what is ready to `out`.
    ///
    /// # Errors
    ///
    /// [`Error::OutputRefused`].
    pub fn write(
        &mut self,
        input: &[u8],
        out: &mut dyn Output,
        index: &mut dyn BlockIndex,
    ) -> Result<(), Error> {
        if self.plain == 0 && self.bits.written == 0 {
            for byte in b"BZh" {
                self.bits.put(out, 8, u32::from(*byte))?;
            }
            self.bits.put(out, 8, u32::from(b'0' + self.level))?;
        }
        for &byte in input {
            if self.run_len > 0 && byte == self.run_byte && self.run_len < 255 {
                self.run_len += 1;
            } else {
                self.end_run(out, index)?;
                self.run_byte = byte;
                self.run_len = 1;
            }
        }
        Ok(())
    }

    /// Compress what is left and write the end marker.
    ///
    /// # Errors
    ///
    /// [`Error::OutputRefused`].
    pub fn finish(
        &mut self,
        out: &mut dyn Output,
        index: &mut dyn BlockIndex,
    ) -> Result<(), Error> {
        if self.plain == 0 && self.bits.written == 0 && self.run_len == 0 {
            self.write(&[], out, index)?;
        }
        self.end_run(out, index)?;
        self.compress_block(out, index)?;
        self.bits.put(out, 24, narrow64(END_MAGIC >> 24))?;
        self.bits.put(out, 24, narrow64(END_MAGIC & 0xFF_FFFF))?;
        self.bits.put(out, 32, self.combined)?;
        self.bits.finish(out)
    }

    /// Put the run being gathered into the block, starting another block
    /// first where it would not fit.
    fn end_run(&mut self, out: &mut dyn Output, index: &mut dyn BlockIndex) -> Result<(), Error> {
        if self.run_len == 0 {
            return Ok(());
        }
        if self.len + 5 > self.limit {
            self.compress_block(out, index)?;
        }
        let block = self.block.as_mut();
        let byte = self.run_byte;
        let literal = self.run_len.min(4) as usize;
        block[self.len..self.len + literal].fill(byte);
        self.len += literal;
        if self.run_len >= 4 {
            block[self.len] = low_byte(u64::from(self.run_len - 4));
            self.len += 1;
        }
        for _ in 0..self.run_len {
            self.block_crc = crc_update(self.block_crc, byte);
        }
        self.plain += u64::from(self.run_len);
        self.run_len = 0;
        Ok(())
    }

    /// Transform, code and write the gathered block.
    fn compress_block(
        &mut self,
        out: &mut dyn Output,
        index: &mut dyn BlockIndex,
    ) -> Result<(), Error> {
        let n = self.len;
        if n == 0 {
            return Ok(());
        }
        index.block(self.block_plain, self.bits.written);
        let crc = !self.block_crc;
        self.combined = combine(self.combined, crc);
        let block = &self.block.as_mut()[..n];
        let work = self.work.as_mut();
        let (sorted, rest) = work.split_at_mut(n);
        let origin = sort_rotations(block, sorted, rest);
        // The last column, through the move-to-front and run coding.
        let mut used = [false; 256];
        for &byte in block {
            used[usize::from(byte)] = true;
        }
        let mut byte_to_seq = [0u8; 256];
        let mut in_use = 0usize;
        for (byte, &is) in used.iter().enumerate() {
            if is {
                byte_to_seq[byte] = u8::try_from(in_use).unwrap_or(u8::MAX);
                in_use += 1;
            }
        }
        let alpha = in_use + 2;
        let end_of_block = u16::try_from(in_use + 1).unwrap_or(u16::MAX);
        let symbols = &mut rest[..=n];
        let mut freq = [0u32; MAX_ALPHA];
        let mut mtf: [u8; 256] = core::array::from_fn(|i| u8::try_from(i).unwrap_or(u8::MAX));
        let mut count = 0usize;
        let mut zeros = 0u32;
        let emit =
            |symbol: u16, symbols: &mut [u32], count: &mut usize, freq: &mut [u32; MAX_ALPHA]| {
                symbols[*count] = u32::from(symbol);
                *count += 1;
                freq[usize::from(symbol)] += 1;
            };
        for &start in sorted.iter() {
            let at = if start == 0 {
                n - 1
            } else {
                start as usize - 1
            };
            let seq = byte_to_seq[usize::from(block[at])];
            if mtf[0] == seq {
                zeros += 1;
                continue;
            }
            put_zero_run(&mut zeros, |symbol| {
                emit(symbol, symbols, &mut count, &mut freq);
            });
            let position = mtf.iter().position(|&s| s == seq).unwrap_or(0);
            mtf.copy_within(0..position, 1);
            mtf[0] = seq;
            emit(
                u16::try_from(position + 1).unwrap_or(u16::MAX),
                symbols,
                &mut count,
                &mut freq,
            );
        }
        put_zero_run(&mut zeros, |symbol| {
            emit(symbol, symbols, &mut count, &mut freq);
        });
        emit(end_of_block, symbols, &mut count, &mut freq);
        let symbols = &symbols[..count];
        self.coding.choose(symbols, &freq, alpha);
        // The block's header, tables and symbols.
        let bits = &mut self.bits;
        bits.put(out, 24, narrow64(BLOCK_MAGIC >> 24))?;
        bits.put(out, 24, narrow64(BLOCK_MAGIC & 0xFF_FFFF))?;
        bits.put(out, 32, crc)?;
        bits.put(out, 1, 0)?;
        bits.put(out, 24, origin)?;
        let mut groups_used = 0u32;
        for group in 0..16 {
            if used[group * 16..group * 16 + 16].iter().any(|&is| is) {
                groups_used |= 0x8000 >> group;
            }
        }
        bits.put(out, 16, groups_used)?;
        for group in 0..16 {
            if groups_used & (0x8000 >> group) != 0 {
                let mut mask = 0u32;
                for i in 0..16 {
                    if used[group * 16 + i] {
                        mask |= 0x8000 >> i;
                    }
                }
                bits.put(out, 16, mask)?;
            }
        }
        self.coding.write(bits, out, symbols, alpha)?;
        self.len = 0;
        self.block_crc = 0xFFFF_FFFF;
        self.block_plain = self.plain;
        Ok(())
    }
}

/// Emit the run of `zeros` front bytes as run symbols — its length less one
/// in bijective base two, least significant digit first — and clear it.
fn put_zero_run(zeros: &mut u32, mut emit: impl FnMut(u16)) {
    if *zeros == 0 {
        return;
    }
    let mut run = *zeros - 1;
    loop {
        emit(u16::from(run & 1 == 1));
        if run < 2 {
            break;
        }
        run = (run - 2) / 2;
    }
    *zeros = 0;
}

/// Sort the rotations of `block` into `sorted`, using `scratch` (four
/// `u32` a symbol), answering where the rotation that starts the block
/// landed: prefix doubling over rotation classes, each round two counting
/// sorts, so any block sorts in `O(n log n)`.
fn sort_rotations(block: &[u8], sorted: &mut [u32], scratch: &mut [u32]) -> u32 {
    let n = block.len();
    let (class, rest) = scratch.split_at_mut(n);
    let (moved, rest) = rest.split_at_mut(n);
    let (next_class, rest) = rest.split_at_mut(n);
    let count = &mut rest[..n.max(256)];
    count[..256].fill(0);
    for &byte in block {
        count[usize::from(byte)] += 1;
    }
    for i in 1..256 {
        count[i] += count[i - 1];
    }
    for i in (0..n).rev() {
        let byte = usize::from(block[i]);
        count[byte] -= 1;
        sorted[count[byte] as usize] = narrow(i);
    }
    let mut classes = 1u32;
    class[sorted[0] as usize] = 0;
    for i in 1..n {
        if block[sorted[i] as usize] != block[sorted[i - 1] as usize] {
            classes += 1;
        }
        class[sorted[i] as usize] = classes - 1;
    }
    let mut span = 1usize;
    while span < n && (classes as usize) < n {
        for i in 0..n {
            let start = sorted[i] as usize;
            moved[i] = narrow((start + n - span % n) % n);
        }
        count[..classes as usize].fill(0);
        for &start in moved.iter() {
            count[class[start as usize] as usize] += 1;
        }
        for i in 1..classes as usize {
            count[i] += count[i - 1];
        }
        for i in (0..n).rev() {
            let start = moved[i] as usize;
            let c = class[start] as usize;
            count[c] -= 1;
            sorted[count[c] as usize] = narrow(start);
        }
        classes = 1;
        next_class[sorted[0] as usize] = 0;
        for i in 1..n {
            let (a, b) = (sorted[i] as usize, sorted[i - 1] as usize);
            let key = |at: usize| (class[at], class[(at + span) % n]);
            if key(a) != key(b) {
                classes += 1;
            }
            next_class[a] = classes - 1;
        }
        class.copy_from_slice(next_class);
        span *= 2;
    }
    narrow(sorted.iter().position(|&start| start == 0).unwrap_or(0))
}

/// A block's coding tables and which codes each group of fifty symbols.
struct Coding {
    groups: usize,
    lengths: [[u8; MAX_ALPHA]; MAX_GROUPS],
    selectors: [u8; MAX_SELECTORS],
    selector_count: usize,
}

impl Coding {
    /// Tables for `symbols` over an alphabet of `alpha`, refined as bzip2
    /// refines them: start from frequency bands, then four rounds of giving
    /// each group the table that codes it shortest and fitting each table to
    /// the groups it was given.
    fn choose(&mut self, symbols: &[u32], freq: &[u32; MAX_ALPHA], alpha: usize) {
        let total = symbols.len();
        let groups = match total {
            0..200 => 2,
            200..600 => 3,
            600..1200 => 4,
            1200..2400 => 5,
            _ => 6,
        };
        let coding = self;
        coding.groups = groups;
        coding.lengths = [[15; MAX_ALPHA]; MAX_GROUPS];
        coding.selector_count = 0;
        let mut remaining = narrow(total);
        let mut start = 0usize;
        for part in (1..=groups).rev() {
            let target = remaining / narrow(part);
            let mut end = start;
            let mut taken = 0u32;
            while taken < target && end < alpha {
                taken += freq[end];
                end += 1;
            }
            if end > start + 1 && part != groups && part != 1 && (groups - part) % 2 == 1 {
                end -= 1;
                taken -= freq[end];
            }
            for symbol in start..end.max(start + 1).min(alpha) {
                coding.lengths[part - 1][symbol] = 0;
            }
            remaining = remaining.saturating_sub(taken);
            start = end.max(start + 1);
        }
        for _ in 0..4 {
            let mut fitted = [[0u32; MAX_ALPHA]; MAX_GROUPS];
            coding.selector_count = 0;
            for chunk in symbols.chunks(GROUP_SIZE) {
                let mut best = 0;
                let mut best_cost = u32::MAX;
                for table in 0..groups {
                    let cost: u32 = chunk
                        .iter()
                        .map(|&symbol| u32::from(coding.lengths[table][symbol as usize]))
                        .sum();
                    if cost < best_cost {
                        best_cost = cost;
                        best = table;
                    }
                }
                for &symbol in chunk {
                    fitted[best][symbol as usize] += 1;
                }
                coding.selectors[coding.selector_count] = u8::try_from(best).unwrap_or(0);
                coding.selector_count += 1;
            }
            for (fit, lengths) in fitted.iter().zip(coding.lengths.iter_mut()).take(groups) {
                code_lengths(&fit[..alpha], &mut lengths[..alpha]);
            }
        }
    }

    /// Write the tables, the selectors and `symbols` coded by them.
    fn write(
        &self,
        bits: &mut BitWriter,
        out: &mut dyn Output,
        symbols: &[u32],
        alpha: usize,
    ) -> Result<(), Error> {
        bits.put(out, 3, narrow(self.groups))?;
        bits.put(out, 15, narrow(self.selector_count))?;
        let mut order: [u8; MAX_GROUPS] = [0, 1, 2, 3, 4, 5];
        for &selector in &self.selectors[..self.selector_count] {
            let at = order.iter().position(|&o| o == selector).unwrap_or(0);
            order.copy_within(0..at, 1);
            order[0] = selector;
            for _ in 0..at {
                bits.put(out, 1, 1)?;
            }
            bits.put(out, 1, 0)?;
        }
        let mut codes = [[0u32; MAX_ALPHA]; MAX_GROUPS];
        for (table, codes) in codes.iter_mut().enumerate().take(self.groups) {
            let lengths = &self.lengths[table][..alpha];
            let mut current = u32::from(lengths[0]);
            bits.put(out, 5, current)?;
            for &length in lengths {
                let length = u32::from(length);
                while current < length {
                    bits.put(out, 2, 2)?;
                    current += 1;
                }
                while current > length {
                    bits.put(out, 2, 3)?;
                    current -= 1;
                }
                bits.put(out, 1, 0)?;
            }
            assign_codes(lengths, &mut codes[..alpha]);
        }
        for (group, chunk) in symbols.chunks(GROUP_SIZE).enumerate() {
            let table = usize::from(self.selectors[group]);
            for &symbol in chunk {
                let symbol = symbol as usize;
                bits.put(
                    out,
                    u32::from(self.lengths[table][symbol]),
                    codes[table][symbol],
                )?;
            }
        }
        Ok(())
    }
}

/// Huffman code lengths for `freq`, none longer than [`ENCODE_MAX_LEN`]:
/// built again from flattened weights until the longest fits, as bzip2 does.
fn code_lengths(freq: &[u32], lengths: &mut [u8]) {
    let n = freq.len();
    let mut weight = [0u32; MAX_ALPHA];
    for (w, &f) in weight.iter_mut().zip(freq) {
        *w = f.max(1) << 8;
    }
    loop {
        // A tree by repeated merging of the two lightest, each node's depth
        // in its parent links.
        let mut node_weight = [0u32; 2 * MAX_ALPHA];
        let mut parent = [usize::MAX; 2 * MAX_ALPHA];
        let mut alive = [false; 2 * MAX_ALPHA];
        node_weight[..n].copy_from_slice(&weight[..n]);
        alive[..n].fill(true);
        let mut nodes = n;
        for _ in 1..n {
            let mut lightest = [usize::MAX; 2];
            for node in 0..nodes {
                if !alive[node] {
                    continue;
                }
                if lightest[0] == usize::MAX || node_weight[node] < node_weight[lightest[0]] {
                    lightest[1] = lightest[0];
                    lightest[0] = node;
                } else if lightest[1] == usize::MAX || node_weight[node] < node_weight[lightest[1]]
                {
                    lightest[1] = node;
                }
            }
            let (a, b) = (lightest[0], lightest[1]);
            alive[a] = false;
            alive[b] = false;
            // Weights carry their depth in the low byte, so ties favour the
            // shallower tree.
            let depth = (node_weight[a] & 0xFF).max(node_weight[b] & 0xFF) + 1;
            node_weight[nodes] = ((node_weight[a] & !0xFF) + (node_weight[b] & !0xFF)) | depth;
            parent[a] = nodes;
            parent[b] = nodes;
            alive[nodes] = true;
            nodes += 1;
        }
        let mut longest = 0;
        for (symbol, length) in lengths.iter_mut().enumerate().take(n) {
            let mut depth = 0u8;
            let mut at = symbol;
            while parent[at] != usize::MAX {
                at = parent[at];
                depth = depth.saturating_add(1);
            }
            *length = depth.max(1);
            longest = longest.max(*length);
        }
        if longest <= ENCODE_MAX_LEN {
            return;
        }
        for w in weight.iter_mut().take(n) {
            *w = (1 + (*w >> 8) / 2) << 8;
        }
    }
}

/// Canonical codes for `lengths`: shortest first, then by symbol.
fn assign_codes(lengths: &[u8], codes: &mut [u32]) {
    let min = lengths.iter().copied().min().unwrap_or(1);
    let max = lengths.iter().copied().max().unwrap_or(1);
    let mut code = 0u32;
    for len in min..=max {
        for (symbol, &l) in lengths.iter().enumerate() {
            if l == len {
                codes[symbol] = code;
                code += 1;
            }
        }
        code <<= 1;
    }
}

#[cfg(test)]
#[path = "bzip2_tests.rs"]
mod tests;
