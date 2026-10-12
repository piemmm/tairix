//! The range decoder every Opus layer reads its symbols through (RFC 6716,
//! section 4.1), and the raw bits CELT reads from the frame's far end.
//!
//! A read past either end of the frame yields zeros, as the specification
//! requires, so a short frame decodes to defined values and the caller
//! judges it by [`RangeDecoder::tell`].

#![allow(
    clippy::cast_possible_wrap,
    reason = "the reference's bit counts and symbols, reinterpreted as its C reinterprets them"
)]

/// The bits of fraction [`RangeDecoder::tell_frac`] counts in.
pub(super) const BITRES: u32 = 3;

const SYM_BITS: u32 = 8;
const CODE_BITS: u32 = 32;
const SYM_MAX: u32 = (1 << SYM_BITS) - 1;
const CODE_TOP: u32 = 1 << (CODE_BITS - 1);
const CODE_BOT: u32 = CODE_TOP >> SYM_BITS;
const CODE_EXTRA: u32 = (CODE_BITS - 2) % SYM_BITS + 1;
/// [`RangeDecoder::uint`] decodes a value this wide or narrower as one
/// symbol, and the rest of a wider one as raw bits.
const UINT_BITS: u32 = 8;
const WINDOW_BITS: u32 = 32;

/// The bit length of `value`: `0` for zero.
pub(super) const fn ilog(value: u32) -> u32 {
    u32::BITS - value.leading_zeros()
}

/// The decoder's state over one frame's bytes.
pub(super) struct RangeDecoder<'f> {
    bytes: &'f [u8],
    /// The bytes the decoder reads: a redundant frame's bytes at the end are
    /// cut off once the frame states them.
    storage: usize,
    offs: usize,
    end_offs: usize,
    end_window: u32,
    end_bits: u32,
    total_bits: i32,
    rng: u32,
    val: u32,
    ext: u32,
    rem: u32,
}

impl<'f> RangeDecoder<'f> {
    pub(super) fn new(bytes: &'f [u8]) -> Self {
        let mut decoder = Self {
            bytes,
            storage: bytes.len(),
            offs: 0,
            end_offs: 0,
            end_window: 0,
            end_bits: 0,
            total_bits: (CODE_BITS + 1 - (CODE_BITS - CODE_EXTRA) / SYM_BITS * SYM_BITS) as i32,
            rng: 1 << CODE_EXTRA,
            val: 0,
            ext: 0,
            rem: 0,
        };
        decoder.rem = decoder.byte();
        decoder.val = decoder.rng - 1 - (decoder.rem >> (SYM_BITS - CODE_EXTRA));
        decoder.normalize();
        decoder
    }

    fn byte(&mut self) -> u32 {
        if self.offs >= self.storage {
            return 0;
        }
        let byte = self.bytes.get(self.offs).copied().unwrap_or(0);
        self.offs += 1;
        u32::from(byte)
    }

    fn byte_from_end(&mut self) -> u32 {
        if self.end_offs >= self.storage {
            return 0;
        }
        self.end_offs += 1;
        u32::from(
            self.bytes
                .get(self.storage - self.end_offs)
                .copied()
                .unwrap_or(0),
        )
    }

    fn normalize(&mut self) {
        while self.rng <= CODE_BOT {
            self.total_bits += SYM_BITS as i32;
            self.rng <<= SYM_BITS;
            let carried = self.rem;
            self.rem = self.byte();
            let symbol = ((carried << SYM_BITS) | self.rem) >> (SYM_BITS - CODE_EXTRA);
            self.val = ((self.val << SYM_BITS) + (SYM_MAX & !symbol)) & (CODE_TOP - 1);
        }
    }

    /// The cumulative frequency of the next symbol in a distribution of
    /// total `total`; [`update`](Self::update) then consumes it.
    pub(super) fn decode(&mut self, total: u32) -> u32 {
        self.ext = self.rng / total.max(1);
        let s = self.val / self.ext.max(1);
        total - (s + 1).min(total)
    }

    /// [`decode`](Self::decode) for a total of `2^bits`.
    pub(super) fn decode_bin(&mut self, bits: u32) -> u32 {
        self.ext = self.rng >> bits;
        let s = self.val / self.ext.max(1);
        (1 << bits) - (s + 1).min(1 << bits)
    }

    /// Consume the symbol spanning `[low, high)` of `total`.
    pub(super) fn update(&mut self, low: u32, high: u32, total: u32) {
        let s = self.ext.wrapping_mul(total - high);
        self.val = self.val.wrapping_sub(s);
        self.rng = if low > 0 {
            self.ext.wrapping_mul(high - low)
        } else {
            self.rng.wrapping_sub(s)
        };
        self.normalize();
    }

    /// One bit that is set with probability `2^-logp`.
    pub(super) fn bit_logp(&mut self, logp: u32) -> bool {
        let r = self.rng;
        let d = self.val;
        let s = r >> logp;
        let set = d < s;
        if !set {
            self.val = d - s;
        }
        self.rng = if set { s } else { r - s };
        self.normalize();
        set
    }

    /// A symbol from an inverse cumulative distribution of total `2^bits`,
    /// which ends in zero.
    pub(super) fn icdf(&mut self, icdf: &[u8], bits: u32) -> usize {
        let d = self.val;
        let r = self.rng >> bits;
        let mut high = self.rng;
        let mut symbol = 0;
        loop {
            let low = r * u32::from(icdf.get(symbol).copied().unwrap_or(0));
            if d >= low {
                self.val = d - low;
                self.rng = high - low;
                break;
            }
            high = low;
            symbol += 1;
        }
        self.normalize();
        symbol
    }

    /// A value uniform over `0..total`, `total` above one. A value the frame
    /// states out of range reads as `total - 1`, as the reference decoder's
    /// does.
    pub(super) fn uint(&mut self, total: u32) -> u32 {
        let top = total - 1;
        let width = ilog(top);
        if width > UINT_BITS {
            let raw = width - UINT_BITS;
            let symbols = (top >> raw) + 1;
            let s = self.decode(symbols);
            self.update(s, s + 1, symbols);
            let value = (s << raw) | self.bits(raw);
            return value.min(top);
        }
        let s = self.decode(total);
        self.update(s, s + 1, total);
        s
    }

    /// `count` raw bits, at most 24, read from the frame's far end.
    pub(super) fn bits(&mut self, count: u32) -> u32 {
        debug_assert!(count <= 24);
        let mut window = self.end_window;
        let mut available = self.end_bits;
        if available < count {
            loop {
                window |= self.byte_from_end() << available;
                available += SYM_BITS;
                if available > WINDOW_BITS - SYM_BITS {
                    break;
                }
            }
        }
        let value = window & ((1 << count) - 1);
        self.end_window = window >> count;
        self.end_bits = available - count;
        self.total_bits += count as i32;
        value
    }

    /// The whole bits the symbols decoded so far take, rounded up.
    pub(super) fn tell(&self) -> i32 {
        self.total_bits - ilog(self.rng) as i32
    }

    /// [`tell`](Self::tell) in eighths of a bit.
    pub(super) fn tell_frac(&self) -> i32 {
        const CORRECTION: [u32; 8] = [35733, 38967, 42495, 46340, 50535, 55109, 60097, 65535];
        let bits = self.total_bits << BITRES;
        let width = ilog(self.rng);
        let r = self.rng >> (width - 16);
        let mut eighths = (r >> 12) - 8;
        eighths += u32::from(r > CORRECTION[eighths as usize]);
        bits - ((width << 3) + eighths) as i32
    }

    /// Account the frame's remaining bits as read, as a frame coded as
    /// silence does.
    pub(super) fn skip_to(&mut self, bits: i32) {
        self.total_bits += bits - self.tell();
    }

    /// The frame's bytes the decoder reads.
    pub(super) const fn storage(&self) -> usize {
        self.storage
    }

    /// Cut the last `bytes` off what the decoder reads.
    pub(super) fn shrink(&mut self, bytes: usize) {
        self.storage = self.storage.saturating_sub(bytes);
    }

    /// The state the frame ended in, which the encoder's matches exactly
    /// when every symbol was read as written.
    pub(super) const fn range(&self) -> u32 {
        self.rng
    }
}

#[cfg(test)]
pub(super) mod writer;

#[cfg(test)]
#[path = "range_tests.rs"]
mod tests;
