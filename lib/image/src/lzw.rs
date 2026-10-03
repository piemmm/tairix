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

impl Lzw {
    pub(crate) fn new() -> Option<Self> {
        Some(Self {
            prefix: fallible::filled(MAX_CODES, NO_PREFIX)?,
            suffix: fallible::filled(MAX_CODES, 0u8)?,
            stack: fallible::filled(MAX_CODES + 1, 0u8)?,
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
        let roots = 1u16 << root_bits;
        let end = roots + 1;
        for index in 0..usize::from(roots) {
            self.prefix[index] = NO_PREFIX;
            self.suffix[index] = u8::try_from(index).map_err(|_| invalid.clone())?;
        }
        let mut next = end + 1;
        let mut width = root_bits + 1;
        let mut previous: Option<u16> = None;
        let mut written = 0usize;
        while let Some(code) = source.code(width)? {
            if code == roots {
                next = end + 1;
                width = root_bits + 1;
                previous = None;
                continue;
            }
            if code == end {
                break;
            }
            let mut depth = 0usize;
            let first = match code.cmp(&next) {
                core::cmp::Ordering::Less => self.walk(code, roots, &mut depth, invalid)?,
                // The code this very step defines: its string is the previous
                // one followed by that string's own first byte. The stack
                // fills in reverse, so the trailing byte takes the slot below
                // the walk — which is why the stack carries one extra.
                core::cmp::Ordering::Equal => {
                    let Some(prev) = previous else {
                        return Err(invalid.clone());
                    };
                    depth = 1;
                    let first = self.walk(prev, roots, &mut depth, invalid)?;
                    self.stack[0] = first;
                    first
                }
                core::cmp::Ordering::Greater => return Err(invalid.clone()),
            };
            for slot in (0..depth).rev() {
                if written == out.len() {
                    break;
                }
                out[written] = self.stack[slot];
                written += 1;
            }
            if written == out.len() {
                break;
            }
            if let Some(prev) = previous {
                if usize::from(next) < MAX_CODES {
                    self.prefix[usize::from(next)] = prev;
                    self.suffix[usize::from(next)] = first;
                    next += 1;
                    if widen.reached(next, width) && width < MAX_CODE_BITS {
                        width += 1;
                    }
                }
            }
            previous = Some(code);
        }
        Ok(written)
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
