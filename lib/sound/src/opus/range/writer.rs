//! The range encoder (RFC 6716, section 5.1), test-only: it writes the
//! symbol streams the decoder's tests read back.

use alloc::vec::Vec;

use super::ilog;

const SYM_BITS: u32 = 8;
const CODE_BITS: u32 = 32;
const SYM_MAX: u32 = (1 << SYM_BITS) - 1;
const CODE_TOP: u32 = 1 << (CODE_BITS - 1);
const CODE_BOT: u32 = CODE_TOP >> SYM_BITS;
const CODE_SHIFT: u32 = CODE_BITS - SYM_BITS - 1;
const UINT_BITS: u32 = 8;
const WINDOW_BITS: u32 = 32;

pub(crate) struct RangeEncoder {
    front: Vec<u8>,
    back: Vec<u8>,
    end_window: u32,
    end_bits: u32,
    rng: u32,
    val: u32,
    ext: u32,
    rem: Option<u32>,
}

impl RangeEncoder {
    pub(crate) fn new() -> Self {
        Self {
            front: Vec::new(),
            back: Vec::new(),
            end_window: 0,
            end_bits: 0,
            rng: CODE_TOP,
            val: 0,
            ext: 0,
            rem: None,
        }
    }

    fn carry_out(&mut self, c: u32) {
        if c == SYM_MAX {
            self.ext += 1;
            return;
        }
        let carry = c >> SYM_BITS;
        if let Some(rem) = self.rem {
            self.front.push((rem + carry).to_le_bytes()[0]);
        }
        while self.ext > 0 {
            self.front.push(((SYM_MAX + carry) & SYM_MAX) as u8);
            self.ext -= 1;
        }
        self.rem = Some(c & SYM_MAX);
    }

    fn normalize(&mut self) {
        while self.rng <= CODE_BOT {
            self.carry_out(self.val >> CODE_SHIFT);
            self.val = (self.val << SYM_BITS) & (CODE_TOP - 1);
            self.rng <<= SYM_BITS;
        }
    }

    pub(crate) fn encode(&mut self, low: u32, high: u32, total: u32) {
        let r = self.rng / total;
        if low > 0 {
            self.val += self.rng - r * (total - low);
            self.rng = r * (high - low);
        } else {
            self.rng -= r * (total - high);
        }
        self.normalize();
    }

    pub(crate) fn encode_bin(&mut self, low: u32, high: u32, bits: u32) {
        self.encode_with(self.rng >> bits, low, high, 1 << bits);
    }

    fn encode_with(&mut self, r: u32, low: u32, high: u32, total: u32) {
        if low > 0 {
            self.val += self.rng - r * (total - low);
            self.rng = r * (high - low);
        } else {
            self.rng -= r * (total - high);
        }
        self.normalize();
    }

    pub(crate) fn bit_logp(&mut self, set: bool, logp: u32) {
        let s = self.rng >> logp;
        let r = self.rng - s;
        if set {
            self.val += r;
        }
        self.rng = if set { s } else { r };
        self.normalize();
    }

    pub(crate) fn icdf(&mut self, symbol: usize, icdf: &[u8], bits: u32) {
        let r = self.rng >> bits;
        if symbol > 0 {
            self.val += self.rng - r * u32::from(icdf[symbol - 1]);
            self.rng = r * u32::from(icdf[symbol - 1] - icdf[symbol]);
        } else {
            self.rng -= r * u32::from(icdf[symbol]);
        }
        self.normalize();
    }

    pub(crate) fn uint(&mut self, value: u32, total: u32) {
        let top = total - 1;
        let width = ilog(top);
        if width > UINT_BITS {
            let raw = width - UINT_BITS;
            let symbols = (top >> raw) + 1;
            let high = value >> raw;
            self.encode(high, high + 1, symbols);
            self.bits(value & ((1 << raw) - 1), raw);
        } else {
            self.encode(value, value + 1, total);
        }
    }

    pub(crate) fn bits(&mut self, value: u32, count: u32) {
        let mut window = self.end_window;
        let mut used = self.end_bits;
        if used + count > WINDOW_BITS {
            loop {
                self.back.push((window & SYM_MAX) as u8);
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
    }

    /// The state after the symbols so far, which the decoder's must match.
    pub(crate) const fn range(&self) -> u32 {
        self.rng
    }

    /// The frame, `len` bytes long: the range coder's bytes from the front,
    /// the raw bits from the back, zeros between. `None` if they overlap.
    pub(crate) fn finish(mut self, len: usize) -> Option<Vec<u8>> {
        let mut l = CODE_BITS - ilog(self.rng);
        let mut mask = (CODE_TOP - 1) >> l;
        let mut end = (self.val + mask) & !mask;
        if (end | mask) >= self.val.wrapping_add(self.rng) {
            l += 1;
            mask >>= 1;
            end = (self.val + mask) & !mask;
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
            self.back.push((window & SYM_MAX) as u8);
            window >>= SYM_BITS;
            used -= SYM_BITS;
        }
        let tail = usize::from(used > 0);
        if self.front.len() + self.back.len() + tail > len {
            return None;
        }
        let mut frame = self.front;
        frame.resize(len - self.back.len(), 0);
        if used > 0 {
            let last = frame.len() - 1;
            frame[last] |= window.to_le_bytes()[0];
        }
        frame.extend(self.back.iter().rev());
        Some(frame)
    }
}
