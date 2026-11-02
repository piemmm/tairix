//! The ARC family's compression methods, as ARC, Spark and `ArcFS` members use
//! them: run-length packing (RLE90), squeezing (static Huffman), ARC 4's
//! crunching (fixed 12-bit LZW over a hashed table), and the dynamic,
//! compress-style LZW of ARC 5's crunching, PKARC's squashing and Spark's
//! and `ArcFS`'s compressing — every one decoded, and the dynamic LZW encoded,
//! which is how a Spark or `ArcFS` member is written.
//!
//! A member's method byte and header say which method and how wide its
//! codes may grow; that is the archive's business. A decoder stops where it
//! is asked: a member states its original length, and some carry bytes past
//! it. Every count, code and tree a stream gives is checked before it is
//! used, so a stream out of shape is refused, never a panic. The code tables
//! are workspace the caller places, so nothing here allocates.
//!
//! Where implementations disagree — whether a literal `0x90` becomes the byte
//! a later run repeats — this follows deark and `XADMaster`: it does.

use crate::{Input, Output};

/// The byte that opens a run in RLE90.
const RUN: u8 = 0x90;

/// The narrowest dynamic LZW code.
pub const LZW_MIN_BITS: u8 = 9;
/// The widest dynamic LZW code any of the family's methods uses.
pub const LZW_MAX_BITS: u8 = 16;

/// The dynamic LZW code that empties the table.
const CLEAR: u32 = 256;
/// The first code a dynamic LZW table assigns.
const FIRST: u32 = 257;
/// Codes in a group, which a code width's change or a clear pads out.
const GROUP: u32 = 8;

/// The entries in ARC 4's crunching table: its codes are 12 bits.
const CRUNCH_SLOTS: usize = 4096;
/// The prefix an ARC 4 table root carries: none, which its hash reads as
/// the 16-bit `-1`.
const NO_PREFIX: u16 = 0xFFFF;

/// The most nodes a squeezed member's tree holds.
const SQUEEZE_NODES: usize = 256;
/// The symbol that ends a squeezed member.
const SQUEEZE_END: u16 = 256;

/// The `u32` table workspace a [`Decoder`] needs: an entry for every code
/// the widest method can make.
pub const DECODER_TABLE: usize = 1 << LZW_MAX_BITS;
/// The `u32` table workspace a squeezed member's [`Decoder`] needs: its
/// tree's two children a node.
pub const SQUEEZE_TABLE: usize = 2 * SQUEEZE_NODES;
/// The byte workspace a [`Decoder`] needs: the longest string a code names.
pub const DECODER_STACK: usize = 1 << LZW_MAX_BITS;

/// Slots in the largest [`Encoder`] dictionary: twice the codes the widest
/// method can make, so a probe is short.
const ENCODER_SLOTS: usize = 2 << LZW_MAX_BITS;
/// Slots in the smallest.
const ENCODER_MIN_SLOTS: usize = 1 << 10;
/// The `u32` workspace the largest [`Encoder`] dictionary takes: two words a
/// slot.
pub const ENCODER_WORK: usize = 2 * ENCODER_SLOTS;

/// The `u32` workspace an [`Encoder`] needs for `len` bytes of input: room
/// for every string they can enter, so it compresses them as the largest
/// would, and no more — a dictionary is cleared whole as an encoder starts.
#[must_use]
pub fn encoder_work(len: u64) -> usize {
    let wanted = usize::try_from(len)
        .ok()
        .filter(|&len| len < ENCODER_SLOTS / 2)
        .map_or(ENCODER_SLOTS, |len| (2 * len).next_power_of_two());
    2 * wanted.max(ENCODER_MIN_SLOTS)
}

/// Input bytes between an encoder's checks of whether a full table still
/// pays, as compress checks.
const CHECK_GAP: u64 = 10_000;

/// Why a stream was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Error {
    /// A code, tree or parameter the stream or the caller gives is out of
    /// the method's shape.
    Corrupt,
    /// A squeezed member's tree ended before it was whole.
    UnexpectedEof,
    /// Workspace short of what the method needs.
    WorkspaceTooSmall,
    /// The encoder's output refused its bytes.
    OutputRefused,
}

/// How ARC 4's crunching hashes a string into its table.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Hash {
    /// ARC 4.0 and 4.1's: the middle bits of a square.
    Square,
    /// ARC 4.6's faster product.
    Product,
}

/// How a member's bytes are coded: what its method byte and header say.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Method {
    /// RLE90 alone: "packed".
    Packed,
    /// Static Huffman, then RLE90: "squeezed".
    Squeezed,
    /// ARC 4's 12-bit LZW, then RLE90 where `rle`: methods 5, 6 and 7.
    Crunched {
        /// How its table is hashed.
        hash: Hash,
        /// Whether RLE90 follows.
        rle: bool,
    },
    /// Dynamic LZW, its codes growing to `max_bits`, then RLE90 where `rle`.
    Lzw {
        /// The widest its codes grow, 9 to 16.
        max_bits: u8,
        /// Whether RLE90 follows.
        rle: bool,
    },
}

impl Method {
    const fn rle(self) -> bool {
        match self {
            Self::Packed | Self::Squeezed => true,
            Self::Crunched { rle, .. } | Self::Lzw { rle, .. } => rle,
        }
    }
}

/// Bits taken least significant first, as squeezing and dynamic LZW pack
/// them.
#[derive(Default)]
struct Bits {
    held: u64,
    count: u32,
}

impl Bits {
    /// The next `n` bits, or `None` where the input ends first.
    fn take(&mut self, input: &mut dyn Input, n: u32) -> Option<u32> {
        while self.count < n {
            self.held |= u64::from(input.byte()?) << self.count;
            self.count += 8;
        }
        let value = self.held & ((1 << n) - 1);
        self.held >>= n;
        self.count -= n;
        u32::try_from(value).ok()
    }
}

/// Where the first stage of a decoder stands.
enum Stage {
    /// Packed: the input's bytes as they are.
    Bytes,
    /// Squeezed, its tree of `nodes` nodes kept in the table workspace once
    /// read.
    Squeeze { nodes: Option<usize> },
    /// ARC 4's crunching: the last code, how many entries the table holds,
    /// and the low nibble a code read has left.
    Crunch {
        hash: Hash,
        prev: Option<u16>,
        entries: usize,
        nibble: Option<u8>,
    },
    /// Dynamic LZW.
    Lzw {
        max_bits: u32,
        width: u32,
        free: u32,
        prev: Option<u32>,
        in_group: u32,
    },
}

/// RLE90's state between bytes.
#[derive(Default)]
struct Rle {
    last: u8,
    repeat: u32,
}

/// A streaming decoder of one member, over the `u32` table workspace `T`
/// ([`DECODER_TABLE`] for crunching and LZW; squeezing takes 512) and the
/// byte workspace `S` ([`DECODER_STACK`] for crunching and LZW; none
/// otherwise).
pub struct Decoder<T, S> {
    table: T,
    stack: S,
    stage: Stage,
    rle: Option<Rle>,
    bits: Bits,
    /// Bytes of the last code's string still to emit, from the stack's top.
    pending: usize,
    /// The first byte of the last string a code named.
    first: u8,
    /// The stream's end was coded: what follows is padding.
    ended: bool,
}

impl<T: AsMut<[u32]>, S: AsMut<[u8]>> Decoder<T, S> {
    /// A decoder of a member coded as `method` says.
    ///
    /// # Errors
    ///
    /// [`Error::Corrupt`] for an LZW width out of range;
    /// [`Error::WorkspaceTooSmall`] for workspace short of the method's.
    pub fn new(mut table: T, mut stack: S, method: Method) -> Result<Self, Error> {
        let (table_need, stack_need) = match method {
            Method::Packed => (0, 0),
            Method::Squeezed => (SQUEEZE_TABLE, 0),
            Method::Crunched { .. } => (2 * CRUNCH_SLOTS, CRUNCH_SLOTS),
            Method::Lzw { max_bits, .. } => {
                if !(LZW_MIN_BITS..=LZW_MAX_BITS).contains(&max_bits) {
                    return Err(Error::Corrupt);
                }
                (1 << max_bits, 1 << max_bits)
            }
        };
        if table.as_mut().len() < table_need || stack.as_mut().len() < stack_need {
            return Err(Error::WorkspaceTooSmall);
        }
        let stage = match method {
            Method::Packed => Stage::Bytes,
            Method::Squeezed => Stage::Squeeze { nodes: None },
            Method::Crunched { hash, .. } => {
                // Every byte is a root, entered as its own string.
                let work = table.as_mut();
                work[..2 * CRUNCH_SLOTS].fill(0);
                for byte in 0..=u8::MAX {
                    crunch_insert(work, hash, NO_PREFIX, byte)?;
                }
                Stage::Crunch {
                    hash,
                    prev: None,
                    entries: 256,
                    nibble: None,
                }
            }
            Method::Lzw { max_bits, .. } => Stage::Lzw {
                max_bits: u32::from(max_bits),
                width: u32::from(LZW_MIN_BITS),
                free: FIRST,
                prev: None,
                in_group: 0,
            },
        };
        Ok(Self {
            table,
            stack,
            stage,
            rle: method.rle().then(Rle::default),
            bits: Bits::default(),
            pending: 0,
            first: 0,
            ended: false,
        })
    }

    /// Decode into `out` from `input`, answering how many bytes it made:
    /// fewer only where the stream ends.
    ///
    /// # Errors
    ///
    /// [`Error::Corrupt`] for a code or tree out of shape;
    /// [`Error::UnexpectedEof`] for a squeezed member whose tree is cut off.
    pub fn read(&mut self, input: &mut dyn Input, out: &mut [u8]) -> Result<usize, Error> {
        for (made, slot) in out.iter_mut().enumerate() {
            match self.next_byte(input)? {
                Some(byte) => *slot = byte,
                None => return Ok(made),
            }
        }
        Ok(out.len())
    }

    /// The next decoded byte, RLE90 undone where the method packs.
    fn next_byte(&mut self, input: &mut dyn Input) -> Result<Option<u8>, Error> {
        if self.rle.is_none() {
            return self.stage_byte(input);
        }
        loop {
            if let Some(rle) = self.rle.as_mut() {
                if rle.repeat > 0 {
                    rle.repeat -= 1;
                    return Ok(Some(rle.last));
                }
            }
            let Some(byte) = self.stage_byte(input)? else {
                return Ok(None);
            };
            if byte != RUN {
                if let Some(rle) = self.rle.as_mut() {
                    rle.last = byte;
                }
                return Ok(Some(byte));
            }
            // A run's count, the repeated byte already emitted once; zero
            // stands for the byte itself.
            let Some(count) = self.stage_byte(input)? else {
                return Ok(None);
            };
            if let Some(rle) = self.rle.as_mut() {
                if count == 0 {
                    rle.last = RUN;
                    return Ok(Some(RUN));
                }
                rle.repeat = u32::from(count) - 1;
            }
        }
    }

    /// The next byte of the first stage.
    fn stage_byte(&mut self, input: &mut dyn Input) -> Result<Option<u8>, Error> {
        if self.pending > 0 {
            self.pending -= 1;
            return Ok(Some(self.stack.as_mut()[self.pending]));
        }
        if self.ended {
            return Ok(None);
        }
        match self.stage {
            Stage::Bytes => Ok(input.byte()),
            Stage::Squeeze { .. } => self.squeezed(input),
            Stage::Crunch { .. } => self.crunched(input),
            Stage::Lzw { .. } => self.lzw(input),
        }
    }

    /// The next symbol of a squeezed member, its tree read first.
    fn squeezed(&mut self, input: &mut dyn Input) -> Result<Option<u8>, Error> {
        let Stage::Squeeze { nodes } = &mut self.stage else {
            return Err(Error::Corrupt);
        };
        let count = if let Some(count) = *nodes {
            count
        } else {
            let count = usize::from(word(input)?);
            if count > SQUEEZE_NODES {
                return Err(Error::Corrupt);
            }
            let tree = self.table.as_mut();
            for child in &mut tree[..2 * count] {
                let value = i32::from(i16::from_le_bytes(word(input)?.to_le_bytes()));
                // A leaf is a symbol's complement; nothing names more
                // than the end symbol or a node past the tree.
                let within = if value < 0 {
                    -(value + 1) <= i32::from(SQUEEZE_END)
                } else {
                    usize::try_from(value).is_ok_and(|node| node < count)
                };
                if !within {
                    return Err(Error::Corrupt);
                }
                *child = u32::from_le_bytes(value.to_le_bytes());
            }
            *nodes = Some(count);
            count
        };
        if count == 0 {
            return Ok(None);
        }
        let tree = self.table.as_mut();
        let mut node = 0usize;
        // A walk longer than the tree has nodes is a loop, not a code.
        for _ in 0..count {
            let Some(bit) = self.bits.take(input, 1) else {
                return Ok(None);
            };
            let child = i32::from_le_bytes(tree[2 * node + bit as usize].to_le_bytes());
            if child >= 0 {
                node = usize::try_from(child).map_err(|_| Error::Corrupt)?;
                continue;
            }
            let symbol = u16::try_from(-(child + 1)).map_err(|_| Error::Corrupt)?;
            if symbol == SQUEEZE_END {
                self.ended = true;
                return Ok(None);
            }
            return u8::try_from(symbol).map(Some).map_err(|_| Error::Corrupt);
        }
        Err(Error::Corrupt)
    }

    /// The next byte of an ARC 4 crunched member: a code read, its string
    /// pushed onto the stack.
    fn crunched(&mut self, input: &mut dyn Input) -> Result<Option<u8>, Error> {
        let Stage::Crunch {
            hash,
            prev,
            entries,
            nibble,
        } = &mut self.stage
        else {
            return Err(Error::Corrupt);
        };
        // Two 12-bit codes in three bytes, the high nibble first.
        let code = match nibble.take() {
            None => {
                let (Some(high), Some(low)) = (input.byte(), input.byte()) else {
                    return Ok(None);
                };
                *nibble = Some(low & 0x0F);
                u16::from(high) << 4 | u16::from(low >> 4)
            }
            Some(top) => {
                let Some(low) = input.byte() else {
                    return Ok(None);
                };
                u16::from(top) << 8 | u16::from(low)
            }
        };
        let work = self.table.as_mut();
        let stack = self.stack.as_mut();
        let mut top = 0;
        let (walk_from, after) = match *prev {
            // The string the table cannot yet name is the last one and its
            // own first byte.
            Some(last) if !crunch_used(work, code) => {
                stack[0] = self.first;
                top = 1;
                (last, Some(last))
            }
            last => (code, last),
        };
        top = crunch_string(work, stack, top, walk_from)?;
        self.first = stack[top - 1];
        if let Some(last) = after {
            if *entries < CRUNCH_SLOTS {
                crunch_insert(work, *hash, last, self.first)?;
                *entries += 1;
            }
        }
        *prev = Some(code);
        self.pending = top - 1;
        Ok(Some(stack[top - 1]))
    }

    /// The next byte of a dynamic LZW member: a code read, its string
    /// pushed onto the stack.
    fn lzw(&mut self, input: &mut dyn Input) -> Result<Option<u8>, Error> {
        let Stage::Lzw {
            max_bits,
            width,
            free,
            prev,
            in_group,
        } = &mut self.stage
        else {
            return Err(Error::Corrupt);
        };
        loop {
            let Some(code) = self.bits.take(input, *width) else {
                return Ok(None);
            };
            *in_group = (*in_group + 1) % GROUP;
            if code == CLEAR {
                // The rest of the clear's group is padding.
                if !realign(&mut self.bits, input, in_group, *width) {
                    return Ok(None);
                }
                *width = u32::from(LZW_MIN_BITS);
                *free = FIRST;
                *prev = None;
                continue;
            }
            let table = self.table.as_mut();
            let stack = self.stack.as_mut();
            let Some(last) = *prev else {
                let byte = u8::try_from(code).map_err(|_| Error::Corrupt)?;
                self.first = byte;
                *prev = Some(code);
                return Ok(Some(byte));
            };
            let mut top = 0;
            let walk_from = match code {
                code if code < *free => code,
                code if code == *free => {
                    stack[0] = self.first;
                    top = 1;
                    last
                }
                _ => return Err(Error::Corrupt),
            };
            top = lzw_string(table, stack, top, walk_from)?;
            self.first = stack[top - 1];
            if *free < 1 << *max_bits {
                table[*free as usize] = last << 8 | u32::from(self.first);
                *free += 1;
                if *free == 1 << *width && *width < *max_bits {
                    realign(&mut self.bits, input, in_group, *width);
                    *width += 1;
                }
            }
            *prev = Some(code);
            self.pending = top - 1;
            return Ok(Some(stack[top - 1]));
        }
    }
}

/// A little-endian word of a squeezed member's tree.
fn word(input: &mut dyn Input) -> Result<u16, Error> {
    match (input.byte(), input.byte()) {
        (Some(low), Some(high)) => Ok(u16::from_le_bytes([low, high])),
        _ => Err(Error::UnexpectedEof),
    }
}

/// Pass over what is left of the current group of codes, answering whether
/// the input held it.
fn realign(bits: &mut Bits, input: &mut dyn Input, in_group: &mut u32, width: u32) -> bool {
    if *in_group == 0 {
        return true;
    }
    let left = GROUP - *in_group;
    *in_group = 0;
    (0..left).all(|_| bits.take(input, width).is_some())
}

/// Where an ARC 4 table entry for the string `prefix` then `byte` belongs.
fn crunch_hash(hash: Hash, prefix: u16, byte: u8) -> usize {
    let key = prefix.wrapping_add(u16::from(byte));
    let slot = match hash {
        Hash::Square => {
            let t = u32::from(key | 0x800);
            (t * t) >> 6
        }
        Hash::Product => u32::from(key).wrapping_mul(15_073),
    };
    slot as usize & (CRUNCH_SLOTS - 1)
}

/// Whether ARC 4 table slot `code` holds an entry. An entry is its use bit,
/// its prefix and its byte; the links between colliding entries follow the
/// slots.
fn crunch_used(work: &[u32], code: u16) -> bool {
    work[usize::from(code)] & 1 << 31 != 0
}

/// Enter the string `prefix` then `byte` into ARC 4's table: at its hash,
/// else at the first free slot past the end of the chain the hash starts,
/// linked from it.
fn crunch_insert(work: &mut [u32], hash: Hash, prefix: u16, byte: u8) -> Result<(), Error> {
    let entry = 1 << 31 | u32::from(prefix) << 8 | u32::from(byte);
    let home = crunch_hash(hash, prefix, byte);
    if work[home] & 1 << 31 == 0 {
        work[home] = entry;
        return Ok(());
    }
    let (slots, links) = work.split_at_mut(CRUNCH_SLOTS);
    let mut end = home;
    for _ in 0..CRUNCH_SLOTS {
        match links[end] as usize {
            0 => break,
            next => end = next & (CRUNCH_SLOTS - 1),
        }
    }
    let mut free = (end + 101) & (CRUNCH_SLOTS - 1);
    for _ in 0..CRUNCH_SLOTS {
        if slots[free] & 1 << 31 == 0 {
            slots[free] = entry;
            links[end] = u32::try_from(free).map_err(|_| Error::Corrupt)?;
            return Ok(());
        }
        free = (free + 1) & (CRUNCH_SLOTS - 1);
    }
    Err(Error::Corrupt)
}

/// Push the string ARC 4 table entry `code` names onto `stack` above `top`,
/// its last byte first, answering the new top.
fn crunch_string(
    work: &[u32],
    stack: &mut [u8],
    mut top: usize,
    code: u16,
) -> Result<usize, Error> {
    let mut code = usize::from(code);
    loop {
        let entry = work[code];
        if entry & 1 << 31 == 0 || top >= stack.len() {
            return Err(Error::Corrupt);
        }
        stack[top] = entry.to_le_bytes()[0];
        top += 1;
        let prefix = u16::try_from(entry >> 8 & 0xFFFF).map_err(|_| Error::Corrupt)?;
        if prefix == NO_PREFIX {
            return Ok(top);
        }
        code = usize::from(prefix) & (CRUNCH_SLOTS - 1);
    }
}

/// Push the string dynamic LZW code `code` names onto `stack` above `top`,
/// its last byte first, answering the new top.
fn lzw_string(
    table: &[u32],
    stack: &mut [u8],
    mut top: usize,
    mut code: u32,
) -> Result<usize, Error> {
    while code >= CLEAR {
        let entry = *table.get(code as usize).ok_or(Error::Corrupt)?;
        let slot = stack.get_mut(top).ok_or(Error::Corrupt)?;
        *slot = entry.to_le_bytes()[0];
        top += 1;
        let prefix = entry >> 8;
        // A prefix is always an earlier code, so a walk ends.
        if prefix >= code {
            return Err(Error::Corrupt);
        }
        code = prefix;
    }
    let slot = stack.get_mut(top).ok_or(Error::Corrupt)?;
    *slot = u8::try_from(code).map_err(|_| Error::Corrupt)?;
    Ok(top + 1)
}

/// A dynamic LZW encoder, compress-style, over the `u32` workspace `W`
/// ([`encoder_work`]): codes from 9 bits to the width it is made for, the
/// table cleared once it is full and compressing worse, or once a dictionary
/// smaller than the codes allow is half full.
pub struct Encoder<W> {
    work: W,
    /// Dictionary slots: the largest power of two the workspace holds.
    slots: usize,
    max_bits: u32,
    width: u32,
    free: u32,
    /// The code of the string matched so far.
    string: Option<u32>,
    held: u64,
    count: u32,
    in_group: u32,
    buf: [u8; 4096],
    fill: usize,
    taken: u64,
    made: u64,
    checkpoint: u64,
    ratio: u64,
}

impl<W: AsMut<[u32]>> Encoder<W> {
    /// An encoder whose codes grow to `max_bits`, 9 to 16.
    ///
    /// # Errors
    ///
    /// [`Error::Corrupt`] for a width out of range;
    /// [`Error::WorkspaceTooSmall`] for workspace short of the smallest
    /// dictionary, [`encoder_work`] of nothing.
    pub fn new(mut work: W, max_bits: u8) -> Result<Self, Error> {
        if !(LZW_MIN_BITS..=LZW_MAX_BITS).contains(&max_bits) {
            return Err(Error::Corrupt);
        }
        let words = work.as_mut();
        let room = (words.len() / 2).min(ENCODER_SLOTS);
        if room < ENCODER_MIN_SLOTS {
            return Err(Error::WorkspaceTooSmall);
        }
        // The largest power of two the workspace holds.
        let slots = 1 << room.ilog2();
        words[..2 * slots].fill(0);
        Ok(Self {
            work,
            slots,
            max_bits: u32::from(max_bits),
            width: u32::from(LZW_MIN_BITS),
            free: FIRST,
            string: None,
            held: 0,
            count: 0,
            in_group: 0,
            buf: [0; 4096],
            fill: 0,
            taken: 0,
            made: 0,
            checkpoint: CHECK_GAP,
            ratio: 0,
        })
    }

    /// Compress `input`, writing what is ready to `out`.
    ///
    /// # Errors
    ///
    /// [`Error::OutputRefused`].
    pub fn write(&mut self, input: &[u8], out: &mut dyn Output) -> Result<(), Error> {
        for &byte in input {
            self.taken += 1;
            let Some(string) = self.string else {
                self.string = Some(u32::from(byte));
                continue;
            };
            let key = string << 8 | u32::from(byte);
            let slot = self.find(key);
            let work = self.work.as_mut();
            if work[2 * slot] == key + 1 {
                self.string = Some(work[2 * slot + 1]);
                continue;
            }
            self.put(out, string)?;
            if self.free > (1 << self.width) - 1 && self.width < self.max_bits {
                self.pad(out)?;
                self.width += 1;
            }
            let codes_left = self.free < 1 << self.max_bits;
            // At half load a probe stays short; a reader adds what a writer
            // adds, so a dictionary out of room clears rather than skips.
            let crowded = (self.free - FIRST) as usize >= self.slots / 2;
            if codes_left && !crowded {
                let work = self.work.as_mut();
                work[2 * slot] = key + 1;
                work[2 * slot + 1] = self.free;
                self.free += 1;
            } else if codes_left || (self.taken >= self.checkpoint && self.worsening()) {
                self.put(out, CLEAR)?;
                self.pad(out)?;
                self.width = u32::from(LZW_MIN_BITS);
                self.free = FIRST;
                let slots = self.slots;
                self.work.as_mut()[..2 * slots].fill(0);
            }
            self.string = Some(u32::from(byte));
        }
        Ok(())
    }

    /// Write the string matched last and everything held.
    ///
    /// # Errors
    ///
    /// [`Error::OutputRefused`].
    pub fn finish(&mut self, out: &mut dyn Output) -> Result<(), Error> {
        if let Some(string) = self.string.take() {
            self.put(out, string)?;
        }
        if self.count > 0 {
            self.byte(out, self.held.to_le_bytes()[0])?;
            self.held = 0;
            self.count = 0;
        }
        self.drain(out)
    }

    /// The dictionary slot `key` occupies, or the free one it would take.
    fn find(&mut self, key: u32) -> usize {
        let mask = self.slots - 1;
        let work = self.work.as_mut();
        // The product's high bits, which every bit of the key reaches.
        let mut slot = (key.wrapping_mul(0x9E37_79B1) >> (32 - self.slots.ilog2())) as usize & mask;
        while work[2 * slot] != 0 && work[2 * slot] != key + 1 {
            slot = (slot + 1) & mask;
        }
        slot
    }

    /// Whether a full table compresses worse than it did at the last check,
    /// as compress judges it: the ratio in to out, scaled by 256.
    fn worsening(&mut self) -> bool {
        self.checkpoint = self.taken + CHECK_GAP;
        let ratio = (self.taken << 8) / self.made.max(1);
        if ratio > self.ratio {
            self.ratio = ratio;
            false
        } else {
            self.ratio = 0;
            true
        }
    }

    fn put(&mut self, out: &mut dyn Output, code: u32) -> Result<(), Error> {
        self.held |= u64::from(code) << self.count;
        self.count += self.width;
        while self.count >= 8 {
            self.byte(out, self.held.to_le_bytes()[0])?;
            self.held >>= 8;
            self.count -= 8;
        }
        self.in_group = (self.in_group + 1) % GROUP;
        Ok(())
    }

    /// Pad the current group of codes out with zero codes, as a reader
    /// passes over the rest of it.
    fn pad(&mut self, out: &mut dyn Output) -> Result<(), Error> {
        while self.in_group != 0 {
            self.put(out, 0)?;
        }
        Ok(())
    }

    fn byte(&mut self, out: &mut dyn Output, byte: u8) -> Result<(), Error> {
        self.buf[self.fill] = byte;
        self.fill += 1;
        self.made += 1;
        if self.fill == self.buf.len() {
            self.drain(out)?;
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
}

#[cfg(test)]
#[path = "arc_tests.rs"]
mod tests;
