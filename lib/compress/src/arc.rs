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

use crate::lzw::{self, Clearing, Dialect, Status, GROUP_BYTES};
use crate::{Input, Output};

/// The byte that opens a run in RLE90.
const RUN: u8 = 0x90;

/// The narrowest dynamic LZW code.
pub const LZW_MIN_BITS: u8 = lzw::MIN_BITS;
/// The widest dynamic LZW code any of the family's methods uses.
pub const LZW_MAX_BITS: u8 = lzw::MAX_BITS;

/// The entries in ARC 4's crunching table: its codes are 12 bits.
const CRUNCH_SLOTS: usize = 4096;
/// The prefix an ARC 4 table root carries: none, which its hash reads as
/// the 16-bit `-1`.
const NO_PREFIX: u16 = 0xFFFF;

/// The most nodes a squeezed member's tree holds.
const SQUEEZE_NODES: usize = 256;
/// The symbol that ends a squeezed member.
const SQUEEZE_END: u16 = 256;

/// The `u32` table workspace the most demanding [`Decoder`] needs: ARC 4's
/// crunching table and the links between its colliding entries.
pub const DECODER_TABLE: usize = 2 * CRUNCH_SLOTS;
/// The byte workspace the most demanding [`Decoder`] needs: the widest
/// dynamic LZW's table and the longest string a code names.
pub const DECODER_STACK: usize = lzw::decoder_work(LZW_MAX_BITS);

/// The workspace a [`Decoder`] of a member coded as `method` says needs:
/// `u32` table words, then bytes. A method out of shape needs none, as no
/// decoder of it is made.
#[must_use]
pub const fn decoder_workspace(method: Method) -> (usize, usize) {
    match method {
        Method::Squeezed => (2 * SQUEEZE_NODES, 0),
        Method::Crunched { .. } => (2 * CRUNCH_SLOTS, CRUNCH_SLOTS),
        Method::Lzw { max_bits, .. } if max_bits >= LZW_MIN_BITS && max_bits <= LZW_MAX_BITS => {
            (0, lzw::decoder_work(max_bits))
        }
        Method::Packed | Method::Lzw { .. } => (0, 0),
    }
}

/// The byte workspace an [`Encoder`] of codes growing to `max_bits` needs for
/// `len` bytes of input: room for every string they can enter, so it
/// compresses them as the largest dictionary would, and no more — a
/// dictionary is cleared whole as an encoder starts. A width out of range is
/// taken as the nearest in range.
#[must_use]
pub fn encoder_work(max_bits: u8, len: u64) -> usize {
    let max_bits = max_bits.clamp(LZW_MIN_BITS, LZW_MAX_BITS);
    lzw::encoder_work(max_bits, len)
}

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

/// Bits taken least significant first, as squeezing packs them.
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
    /// Dynamic LZW, over the byte workspace.
    Lzw { member: LzwMember },
}

/// The bytes an LZW stage under RLE90 decodes at a time, so RLE90 calls the
/// coder once for each of them rather than for every byte.
const RUN_DECODED: usize = 1024;

/// A dynamic LZW member's coder and the input gathered for its next group.
struct LzwMember {
    coding: lzw::Decoding,
    carry: [u8; GROUP_BYTES],
    carried: usize,
    input_ended: bool,
}

impl LzwMember {
    /// Decode into `out` over `work`, answering how many bytes it made:
    /// fewer only where the stream ends. Input is gathered a group at a
    /// time, as the coder takes it.
    fn decode(
        &mut self,
        work: &mut [u8],
        input: &mut dyn Input,
        out: &mut [u8],
    ) -> Result<usize, Error> {
        let mut made = 0;
        loop {
            let progress = self
                .coding
                .decode(
                    work,
                    &self.carry[..self.carried],
                    !self.input_ended,
                    &mut out[made..],
                    &mut |_| {},
                )
                .map_err(|_| Error::Corrupt)?;
            self.carry.copy_within(progress.consumed..self.carried, 0);
            self.carried -= progress.consumed;
            made += progress.produced;
            if progress.status != Status::Input {
                return Ok(made);
            }
            while self.carried < self.coding.group_bytes() {
                let Some(byte) = input.byte() else {
                    self.input_ended = true;
                    break;
                };
                self.carry[self.carried] = byte;
                self.carried += 1;
            }
        }
    }
}

/// RLE90's state between bytes.
#[derive(Default)]
struct Rle {
    last: u8,
    repeat: u32,
}

/// A streaming decoder of one member, over the `u32` table workspace `T` and
/// the byte workspace `S` its method needs ([`decoder_workspace`]).
pub struct Decoder<T, S> {
    table: T,
    stack: S,
    stage: Stage,
    rle: Option<Rle>,
    bits: Bits,
    /// Bytes of the last code's string still to emit, from the stack's top.
    pending: usize,
    /// The bytes an LZW member under RLE90 decoded last, and where RLE90
    /// has taken them to.
    run: [u8; RUN_DECODED],
    run_taken: usize,
    run_made: usize,
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
        if let Method::Lzw { max_bits, .. } = method {
            if !(LZW_MIN_BITS..=LZW_MAX_BITS).contains(&max_bits) {
                return Err(Error::Corrupt);
            }
        }
        let (table_need, stack_need) = decoder_workspace(method);
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
                member: LzwMember {
                    coding: lzw::Decoding::new(Dialect::Compress, max_bits)
                        .ok_or(Error::Corrupt)?,
                    carry: [0; GROUP_BYTES],
                    carried: 0,
                    input_ended: false,
                },
            },
        };
        Ok(Self {
            table,
            stack,
            stage,
            rle: method.rle().then(Rle::default),
            bits: Bits::default(),
            pending: 0,
            run: [0; RUN_DECODED],
            run_taken: 0,
            run_made: 0,
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
        if let (None, Stage::Lzw { member, .. }) = (&self.rle, &mut self.stage) {
            return member.decode(self.stack.as_mut(), input, out);
        }
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
        if self.run_taken < self.run_made {
            self.run_taken += 1;
            return Ok(Some(self.run[self.run_taken - 1]));
        }
        if self.pending > 0 {
            self.pending -= 1;
            return Ok(Some(self.stack.as_mut()[self.pending]));
        }
        if self.ended {
            return Ok(None);
        }
        match &mut self.stage {
            Stage::Bytes => Ok(input.byte()),
            Stage::Squeeze { .. } => self.squeezed(input),
            Stage::Crunch { .. } => self.crunched(input),
            Stage::Lzw { member } => {
                self.run_made = member.decode(self.stack.as_mut(), input, &mut self.run)?;
                self.run_taken = usize::from(self.run_made > 0);
                Ok((self.run_made > 0).then_some(self.run[0]))
            }
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
}

/// A little-endian word of a squeezed member's tree.
fn word(input: &mut dyn Input) -> Result<u16, Error> {
    match (input.byte(), input.byte()) {
        (Some(low), Some(high)) => Ok(u16::from_le_bytes([low, high])),
        _ => Err(Error::UnexpectedEof),
    }
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

/// A dynamic LZW encoder, compress-style, over the byte workspace `W`
/// ([`encoder_work`]): codes from 9 bits to the width it is made for, the
/// table cleared once it is full and compressing worse, or once a dictionary
/// smaller than the codes allow is half full.
pub struct Encoder<W> {
    work: W,
    coding: lzw::Encoding,
    buf: [u8; 4096],
    fill: usize,
}

impl<W: AsMut<[u8]>> Encoder<W> {
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
        let coding = lzw::Encoding::new(Clearing::Worsening, max_bits, work.as_mut())
            .ok_or(Error::WorkspaceTooSmall)?;
        Ok(Self {
            work,
            coding,
            buf: [0; 4096],
            fill: 0,
        })
    }

    /// Compress `input`, writing what is ready to `out`.
    ///
    /// # Errors
    ///
    /// [`Error::OutputRefused`].
    pub fn write(&mut self, input: &[u8], out: &mut dyn Output) -> Result<(), Error> {
        self.code(input, true, out)
    }

    /// Write the string matched last and everything held.
    ///
    /// # Errors
    ///
    /// [`Error::OutputRefused`].
    pub fn finish(&mut self, out: &mut dyn Output) -> Result<(), Error> {
        self.code(&[], false, out)?;
        self.drain(out)
    }

    /// Code `input` into the buffer, drained into `out` as it fills.
    fn code(&mut self, mut input: &[u8], more: bool, out: &mut dyn Output) -> Result<(), Error> {
        loop {
            let progress = self
                .coding
                .encode(
                    self.work.as_mut(),
                    input,
                    more,
                    &mut self.buf[self.fill..],
                    &mut |_| {},
                )
                .map_err(|_| Error::Corrupt)?;
            input = &input[progress.consumed..];
            self.fill += progress.produced;
            if progress.status != Status::Output {
                return Ok(());
            }
            self.drain(out)?;
        }
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
