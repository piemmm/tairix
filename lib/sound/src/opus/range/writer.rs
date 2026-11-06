//! The range encoder (RFC 6716, section 5.1), the decoder's exact inverse:
//! the range coder's bytes written from the front of the frame, the raw bits
//! from its back, and the bytes between them zeroed when it finishes.

use super::{
    eighths, ilog, skipped_to, whole_bits, Coder, CODE_BITS, CODE_BOT, CODE_TOP, SYM_BITS, SYM_MAX,
    UINT_BITS, WINDOW_BITS,
};

/// A byte leaves the top of the range from this bit, its carry above it.
const CODE_SHIFT: u32 = CODE_BITS - SYM_BITS - 1;

/// The encoder's state over the frame it writes into.
pub(crate) struct RangeEncoder<'f> {
    frame: &'f mut [u8],
    /// The bytes written at the front.
    offs: usize,
    /// The bytes written at the back.
    end_offs: usize,
    end_window: u32,
    end_bits: u32,
    total_bits: i32,
    rng: u32,
    val: u32,
    /// The run of `0xFF` bytes a carry may yet settle.
    ext: u32,
    /// The last byte, which a carry may yet settle.
    rem: Option<u32>,
    /// Whether the two ends ever met.
    overflowed: bool,
}

impl<'f> RangeEncoder<'f> {
    /// An encoder over all of `frame`.
    pub(crate) fn new(frame: &'f mut [u8]) -> Self {
        Self {
            frame,
            offs: 0,
            end_offs: 0,
            end_window: 0,
            end_bits: 0,
            total_bits: CODE_BITS as i32 + 1,
            rng: CODE_TOP,
            val: 0,
            ext: 0,
            rem: None,
            overflowed: false,
        }
    }

    /// Whether a byte fits between the two ends, noting where none does.
    fn room(&mut self) -> bool {
        let room = self.offs + self.end_offs < self.frame.len();
        self.overflowed |= !room;
        room
    }

    fn push_front(&mut self, byte: u32) {
        if self.room() {
            self.frame[self.offs] = byte.to_le_bytes()[0];
            self.offs += 1;
        }
    }

    fn push_back(&mut self, byte: u32) {
        if self.room() {
            self.end_offs += 1;
            let at = self.frame.len() - self.end_offs;
            self.frame[at] = byte.to_le_bytes()[0];
        }
    }

    fn carry_out(&mut self, c: u32) {
        if c == SYM_MAX {
            self.ext += 1;
            return;
        }
        let carry = c >> SYM_BITS;
        if let Some(rem) = self.rem {
            self.push_front(rem + carry);
        }
        while self.ext > 0 {
            self.push_front(SYM_MAX + carry);
            self.ext -= 1;
        }
        self.rem = Some(c & SYM_MAX);
    }

    fn normalize(&mut self) {
        while self.rng <= CODE_BOT {
            self.carry_out(self.val >> CODE_SHIFT);
            self.val = (self.val << SYM_BITS) & (CODE_TOP - 1);
            self.rng <<= SYM_BITS;
            self.total_bits += SYM_BITS as i32;
        }
    }

    /// Write the symbol spanning `[low, high)` of `total`.
    fn encode(&mut self, low: u32, high: u32, total: u32) {
        let r = self.rng / total.max(1);
        if low > 0 {
            self.val += self.rng - r * (total - low);
            self.rng = r * (high - low);
        } else {
            self.rng -= r * (total - high);
        }
        self.normalize();
    }

    /// Write `count` raw bits of `value` at the frame's far end.
    fn raw(&mut self, value: u32, count: u32) {
        let mut window = self.end_window;
        let mut used = self.end_bits;
        if used + count > WINDOW_BITS {
            loop {
                self.push_back(window & SYM_MAX);
                window >>= SYM_BITS;
                used -= SYM_BITS;
                if used < SYM_BITS {
                    break;
                }
            }
        }
        window |= value << used;
        self.end_window = window;
        self.end_bits = used + count;
        self.total_bits += count as i32;
    }

    /// The state after the symbols so far, which the decoder's ends in
    /// exactly when it read every one as written.
    pub(crate) const fn range(&self) -> u32 {
        self.rng
    }

    /// Write the range coder's last bytes and the raw bits, zeroing the
    /// bytes between them; `false` where the two ends met, the frame holding
    /// less than was coded.
    pub(crate) fn finish(mut self) -> bool {
        let mut l = CODE_BITS - ilog(self.rng);
        let mut mask = (CODE_TOP - 1) >> l;
        let mut end = self.val.wrapping_add(mask) & !mask;
        if (end | mask) >= self.val.wrapping_add(self.rng) {
            l += 1;
            mask >>= 1;
            end = self.val.wrapping_add(mask) & !mask;
        }
        let mut l = l as i32;
        while l > 0 {
            self.carry_out(end >> CODE_SHIFT);
            end = (end << SYM_BITS) & (CODE_TOP - 1);
            l -= SYM_BITS as i32;
        }
        if self.rem.is_some() || self.ext > 0 {
            self.carry_out(0);
        }
        let mut window = self.end_window;
        let mut used = self.end_bits;
        while used >= SYM_BITS {
            self.push_back(window & SYM_MAX);
            window >>= SYM_BITS;
            used -= SYM_BITS;
        }
        if self.overflowed {
            return false;
        }
        let back = self.frame.len() - self.end_offs;
        self.frame[self.offs..back].fill(0);
        if used == 0 {
            return true;
        }
        // The last raw bits share the range coder's last byte, whose low
        // bits its flush left clear, only where the two ends meet.
        if back == 0 || (self.offs == back && used > l.unsigned_abs()) {
            return false;
        }
        self.frame[back - 1] |= window.to_le_bytes()[0];
        true
    }
}

impl Coder for RangeEncoder<'_> {
    const WRITES: bool = true;

    fn storage(&self) -> usize {
        self.frame.len()
    }

    fn tell(&self) -> i32 {
        whole_bits(self.total_bits, self.rng)
    }

    fn tell_frac(&self) -> i32 {
        eighths(self.total_bits, self.rng)
    }

    fn point(&mut self, _: u32) -> Option<u32> {
        None
    }

    fn point_bin(&mut self, _: u32) -> Option<u32> {
        None
    }

    fn take(&mut self, low: u32, high: u32, total: u32) {
        self.encode(low, high, total);
    }

    fn bit_logp(&mut self, value: bool, logp: u32) -> bool {
        let s = self.rng >> logp;
        let r = self.rng - s;
        if value {
            self.val += r;
        }
        self.rng = if value { s } else { r };
        self.normalize();
        value
    }

    fn icdf(&mut self, value: usize, icdf: &[u8], bits: u32) -> usize {
        let value = value.min(icdf.len().saturating_sub(1));
        let at = |index: usize| u32::from(icdf.get(index).copied().unwrap_or(0));
        let r = self.rng >> bits;
        if value > 0 {
            self.val += self.rng - r * at(value - 1);
            self.rng = r * (at(value - 1) - at(value));
        } else {
            self.rng -= r * at(value);
        }
        self.normalize();
        value
    }

    fn uint(&mut self, value: u32, total: u32) -> u32 {
        if total < 2 {
            return 0;
        }
        let top = total - 1;
        let value = value.min(top);
        let width = ilog(top);
        if width > UINT_BITS {
            let raw = width - UINT_BITS;
            let symbols = (top >> raw) + 1;
            let high = value >> raw;
            self.encode(high, high + 1, symbols);
            self.raw(value & ((1 << raw) - 1), raw);
        } else {
            self.encode(value, value + 1, total);
        }
        value
    }

    fn bits(&mut self, value: u32, count: u32) -> u32 {
        if count == 0 {
            return 0;
        }
        let value = value & ((1 << count) - 1);
        self.raw(value, count);
        value
    }

    fn skip_to(&mut self, bits: i32) {
        self.total_bits = skipped_to(self.total_bits, self.rng, bits);
    }
}
