//! The LZW dictionary, its expansion loop, and the coder that writes it.
//!
//! GIF and TIFF both code their pixels with LZW over a dictionary of at most
//! 4096 entries whose first `1 << root_bits` codes are the literals, whose
//! next code clears the table and whose next again ends the stream. What
//! differs is only how codes are packed into bytes and when a new entry
//! widens the code that follows it, so those are the caller's
//! ([`CodeSource`], [`CodeSink`], [`Widen`]) and everything else — the
//! dictionary, the string walk, the entry a code defines for itself, and the
//! deferred clear real encoders rely on — is defined once here.

use alloc::vec::Vec;

use tairix_util::fallible;

use crate::encode::EncodeError;
use crate::DecodeError;

/// The widest code either dialect reaches, and the resulting table size.
pub(crate) const MAX_CODE_BITS: u32 = 12;
pub(crate) const MAX_CODES: usize = 1 << MAX_CODE_BITS;

/// The prefix stored for a root code, which has none. Outside the code
/// space, so it can never be mistaken for a code.
const NO_PREFIX: u16 = u16::MAX;

/// Where a stream's codes come from.
///
/// The packing is the caller's because it is what the two dialects disagree
/// on: GIF runs a least-significant-bit-first stream across a chain of data
/// sub-blocks, TIFF a flat most-significant-bit-first run of bytes.
pub(crate) trait CodeSource {
    /// The next `width`-bit code, or `None` once the stream has run out.
    fn code(&mut self, width: u32) -> Result<Option<u16>, DecodeError>;
}

/// When the entry a step defines widens the code that follows it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Widen {
    /// Once the next free code no longer fits the current width. GIF's
    /// schedule, and the one older TIFF writers emit.
    WhenFull,
    /// One code earlier, so the largest code ever read at a given width is
    /// one below the width's own ceiling. TIFF's own schedule.
    OneEarly,
}

impl Widen {
    /// Whether a table whose next free code is `next` needs a wider read.
    const fn reached(self, next: u16, width: u32) -> bool {
        let next = next as u32;
        match self {
            Self::WhenFull => next >= 1 << width,
            Self::OneEarly => next + 1 >= 1 << width,
        }
    }
}

/// The dictionary and output stack, allocated once and reused by every
/// stream a decode expands.
pub(crate) struct Lzw {
    prefix: Vec<u16>,
    suffix: Vec<u8>,
    /// One slot deeper than the longest possible string, for the reserved
    /// leading byte the not-yet-defined-code case fills in.
    stack: Vec<u8>,
}

/// The bytes an [`Lzw`] holds: its prefix, suffix and stack tables.
pub(crate) const TABLE_BYTES: u64 =
    (MAX_CODES * core::mem::size_of::<u16>() + MAX_CODES + STACK_DEPTH) as u64;

/// One slot deeper than the longest possible string.
const STACK_DEPTH: usize = MAX_CODES + 1;

impl Lzw {
    pub(crate) fn new() -> Option<Self> {
        Some(Self {
            prefix: fallible::filled(MAX_CODES, NO_PREFIX)?,
            suffix: fallible::filled(MAX_CODES, 0u8)?,
            stack: fallible::filled(STACK_DEPTH, 0u8)?,
        })
    }

    /// Walk `code`'s string onto the stack in reverse from `depth`, answering
    /// its first byte.
    ///
    /// A dictionary entry's prefix is always a code that already existed when
    /// the entry was defined, so the walk strictly decreases and terminates.
    /// The index and depth are checked anyway, so a table this decoder could
    /// not have built still cannot overrun either buffer.
    fn walk(
        &mut self,
        code: u16,
        roots: u16,
        depth: &mut usize,
        invalid: &DecodeError,
    ) -> Result<u8, DecodeError> {
        let mut cursor = code;
        while cursor >= roots {
            let index = usize::from(cursor);
            if index >= self.suffix.len() || *depth >= self.stack.len() {
                return Err(invalid.clone());
            }
            self.stack[*depth] = self.suffix[index];
            *depth += 1;
            cursor = self.prefix[index];
        }
        if *depth >= self.stack.len() {
            return Err(invalid.clone());
        }
        let root = u8::try_from(cursor).map_err(|_| invalid.clone())?;
        self.stack[*depth] = root;
        *depth += 1;
        Ok(root)
    }

    /// Expand `source`'s codes into `out`, answering how many bytes were
    /// written.
    ///
    /// The table grows in lockstep with the encoder under `widen`'s
    /// schedule. A stream that fills the table and never clears it keeps
    /// decoding at the widest code against the table as it stands — the
    /// deferred clear real encoders rely on — rather than being refused. One
    /// producing more bytes than `out` holds stops at its end, because the
    /// rest is not part of what was asked for; a caller that requires `out`
    /// filled compares the count it gets back.
    pub(crate) fn expand(
        &mut self,
        source: &mut impl CodeSource,
        root_bits: u32,
        widen: Widen,
        invalid: &DecodeError,
        out: &mut [u8],
    ) -> Result<usize, DecodeError> {
        self.expansion(root_bits, widen, invalid)?
            .fill(source, invalid, out)
    }

    /// A stream's expansion, taken by as many [`Expansion::fill`] calls as
    /// the caller wants its output split into.
    pub(crate) fn expansion(
        &mut self,
        root_bits: u32,
        widen: Widen,
        invalid: &DecodeError,
    ) -> Result<Expansion<'_>, DecodeError> {
        let roots = 1u16 << root_bits;
        for index in 0..usize::from(roots) {
            self.prefix[index] = NO_PREFIX;
            self.suffix[index] = u8::try_from(index).map_err(|_| invalid.clone())?;
        }
        Ok(Expansion {
            lzw: self,
            roots,
            root_bits,
            widen,
            next: roots + 2,
            width: root_bits + 1,
            previous: None,
            pending: 0,
            ended: false,
        })
    }
}

/// One stream's expansion, suspended between calls, so its output can be
/// taken a row at a time without the stream ever being held whole.
pub(crate) struct Expansion<'t> {
    lzw: &'t mut Lzw,
    roots: u16,
    root_bits: u32,
    widen: Widen,
    next: u16,
    width: u32,
    previous: Option<u16>,
    /// The bytes of the last string not yet handed out: its stack slots
    /// below this one.
    pending: usize,
    ended: bool,
}

impl Expansion<'_> {
    /// Fill `out` from `source`, resuming where the last call stopped,
    /// answering the bytes written: fewer than `out` holds only where the
    /// stream has ended.
    pub(crate) fn fill(
        &mut self,
        source: &mut impl CodeSource,
        invalid: &DecodeError,
        out: &mut [u8],
    ) -> Result<usize, DecodeError> {
        let mut written = self.drain(out);
        if written == out.len() || self.ended {
            return Ok(written);
        }
        let end = self.roots + 1;
        while let Some(code) = source.code(self.width)? {
            if code == self.roots {
                self.next = end + 1;
                self.width = self.root_bits + 1;
                self.previous = None;
                continue;
            }
            if code == end {
                self.ended = true;
                break;
            }
            let lzw = &mut *self.lzw;
            let mut depth = 0usize;
            let first = match code.cmp(&self.next) {
                core::cmp::Ordering::Less => lzw.walk(code, self.roots, &mut depth, invalid)?,
                // The code this very step defines: its string is the previous
                // one followed by that string's own first byte. The stack
                // fills in reverse, so the trailing byte takes the slot below
                // the walk — which is why the stack carries one extra.
                core::cmp::Ordering::Equal => {
                    let Some(prev) = self.previous else {
                        return Err(invalid.clone());
                    };
                    depth = 1;
                    let first = lzw.walk(prev, self.roots, &mut depth, invalid)?;
                    lzw.stack[0] = first;
                    first
                }
                core::cmp::Ordering::Greater => return Err(invalid.clone()),
            };
            // The entry is defined before any of the string is handed out,
            // so a suspended expansion resumes against the table the encoder
            // had.
            if let Some(prev) = self.previous {
                if usize::from(self.next) < MAX_CODES {
                    lzw.prefix[usize::from(self.next)] = prev;
                    lzw.suffix[usize::from(self.next)] = first;
                    self.next += 1;
                    if self.widen.reached(self.next, self.width) && self.width < MAX_CODE_BITS {
                        self.width += 1;
                    }
                }
            }
            self.previous = Some(code);
            self.pending = depth;
            written += self.drain(&mut out[written..]);
            if written == out.len() {
                break;
            }
        }
        Ok(written)
    }

    /// Hand out as much of the pending string as `out` holds.
    fn drain(&mut self, out: &mut [u8]) -> usize {
        let mut written = 0;
        while self.pending > 0 && written < out.len() {
            self.pending -= 1;
            out[written] = self.lzw.stack[self.pending];
            written += 1;
        }
        written
    }
}

/// Where a coder's codes go.
///
/// The packing is the caller's for the same reason [`CodeSource`] is: GIF
/// packs least-significant bit first into data sub-blocks, TIFF most
/// significant first into a flat run.
pub(crate) trait CodeSink {
    /// Pack `code`, `width` bits wide.
    fn put(&mut self, code: u16, width: u32) -> Result<(), EncodeError>;
}

/// Slots the coder's dictionary is hashed into: twice the codes, so a probe
/// stays short however full the table gets.
const HASH_BITS: u32 = MAX_CODE_BITS + 1;
const HASH_SLOTS: usize = 1 << HASH_BITS;

/// A slot no string occupies.
const VACANT: u16 = u16::MAX;

/// An LZW coder over one dialect.
///
/// The dictionary is a hash from a string — its prefix's code and its last
/// byte — to the code defining it. Every code is written at the width the
/// decoder reads it at, because the coder keeps the decoder's own count beside
/// its own: the decoder defines nothing for the first code after a clear, so
/// it runs one entry behind, and widens on [`Widen`]'s schedule over that
/// count rather than the coder's.
pub(crate) struct Coder {
    keys: Vec<u32>,
    codes: Vec<u16>,
    root_bits: u32,
    widen: Widen,
    /// The code at which the table counts as full and is cleared.
    limit: u16,
    /// The string matched so far.
    prefix: Option<u16>,
    next: u16,
    decoder_next: u16,
    width: u32,
    /// Whether no code has been written since the last clear.
    fresh: bool,
}

impl Coder {
    /// A coder over literals of `root_bits` bits that clears its table on
    /// reaching `limit` codes; `None` where its tables cannot be held.
    pub(crate) fn new(root_bits: u32, widen: Widen, limit: u16) -> Option<Self> {
        let roots = 1u16 << root_bits;
        Some(Self {
            keys: fallible::filled(HASH_SLOTS, 0u32)?,
            codes: fallible::filled(HASH_SLOTS, VACANT)?,
            root_bits,
            widen,
            limit,
            prefix: None,
            next: roots + 2,
            decoder_next: roots + 2,
            width: root_bits + 1,
            fresh: true,
        })
    }

    const fn clear_code(&self) -> u16 {
        1 << self.root_bits
    }

    /// Open a stream with a clear, as both dialects' readers expect; a
    /// coder that has finished one stream begins the next afresh.
    pub(crate) fn begin(&mut self, sink: &mut dyn CodeSink) -> Result<(), EncodeError> {
        self.prefix = None;
        self.width = self.root_bits + 1;
        self.clear(sink)
    }

    /// Code `byte`, the next literal of the stream.
    pub(crate) fn push(&mut self, byte: u8, sink: &mut dyn CodeSink) -> Result<(), EncodeError> {
        let Some(prefix) = self.prefix else {
            self.prefix = Some(u16::from(byte));
            return Ok(());
        };
        let key = (u32::from(prefix) << 8) | u32::from(byte);
        let slot = self.slot(key);
        if self.codes[slot] != VACANT {
            self.prefix = Some(self.codes[slot]);
            return Ok(());
        }
        self.emit(prefix, sink)?;
        if self.next < self.limit {
            self.keys[slot] = key;
            self.codes[slot] = self.next;
            self.next += 1;
        } else {
            self.clear(sink)?;
        }
        self.prefix = Some(u16::from(byte));
        Ok(())
    }

    /// Write the string still held and end the stream.
    pub(crate) fn finish(&mut self, sink: &mut dyn CodeSink) -> Result<(), EncodeError> {
        if let Some(prefix) = self.prefix.take() {
            self.emit(prefix, sink)?;
        }
        sink.put(self.clear_code() + 1, self.width)
    }

    /// The slot `key` occupies, or the vacant one it would take.
    fn slot(&self, key: u32) -> usize {
        let mut slot = (key.wrapping_mul(0x9E37_79B1) >> (u32::BITS - HASH_BITS)) as usize;
        while self.codes[slot] != VACANT && self.keys[slot] != key {
            slot = (slot + 1) & (HASH_SLOTS - 1);
        }
        slot
    }

    fn emit(&mut self, code: u16, sink: &mut dyn CodeSink) -> Result<(), EncodeError> {
        sink.put(code, self.width)?;
        if self.fresh {
            self.fresh = false;
        } else if usize::from(self.decoder_next) < MAX_CODES {
            self.decoder_next += 1;
            if self.widen.reached(self.decoder_next, self.width) && self.width < MAX_CODE_BITS {
                self.width += 1;
            }
        }
        Ok(())
    }

    fn clear(&mut self, sink: &mut dyn CodeSink) -> Result<(), EncodeError> {
        sink.put(self.clear_code(), self.width)?;
        self.codes.fill(VACANT);
        self.next = self.clear_code() + 2;
        self.decoder_next = self.next;
        self.width = self.root_bits + 1;
        self.fresh = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::{CodeSink, CodeSource, Coder, Lzw, Widen};
    use crate::encode::EncodeError;
    use crate::DecodeError;

    /// Codes held as written, read back in the same order.
    #[derive(Default)]
    struct Codes {
        codes: Vec<(u16, u32)>,
        read: usize,
    }

    impl CodeSink for Codes {
        fn put(&mut self, code: u16, width: u32) -> Result<(), EncodeError> {
            self.codes.push((code, width));
            Ok(())
        }
    }

    impl CodeSource for Codes {
        fn code(&mut self, width: u32) -> Result<Option<u16>, DecodeError> {
            let Some(&(code, written)) = self.codes.get(self.read) else {
                return Ok(None);
            };
            assert_eq!(written, width, "read at the width it was written");
            self.read += 1;
            Ok(Some(code))
        }
    }

    /// An expansion taken in pieces of any size hands out exactly the bytes
    /// one call would, across clears and the code a step defines for itself.
    #[test]
    fn an_expansion_taken_in_pieces_is_the_whole_expansion() {
        let data: Vec<u8> = (0..5000u32)
            .map(|at| u8::try_from((at * at / 7 + at / 3) % 16).unwrap_or(0))
            .collect();
        let mut codes = Codes::default();
        let mut encoder = Coder::new(4, Widen::WhenFull, 4095).expect("tables");
        encoder.begin(&mut codes).expect("begins");
        for &byte in &data {
            encoder.push(byte, &mut codes).expect("pushes");
        }
        encoder.finish(&mut codes).expect("ends");
        let invalid = DecodeError::GifInvalidCode;
        let mut lzw = Lzw::new().expect("tables");
        for piece in [1, 3, 7, 64, 4999, 5000] {
            codes.read = 0;
            let mut expansion = lzw.expansion(4, Widen::WhenFull, &invalid).expect("starts");
            let mut out = Vec::new();
            let mut chunk = alloc::vec![0u8; piece];
            loop {
                let written = expansion
                    .fill(&mut codes, &invalid, &mut chunk)
                    .expect("expands");
                out.extend_from_slice(&chunk[..written]);
                if written < piece {
                    break;
                }
            }
            assert_eq!(out, data, "in pieces of {piece}");
        }
    }
}
