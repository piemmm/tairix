//! ITU-T G.722 sub-band ADPCM at 64 kbit/s, decoded: each eight-bit code
//! carries a low band and a high band, each with its own adaptive predictor,
//! and the receive QMF merges them into two 16 kHz samples. Ported from Steve
//! Underwood's decoder (public domain, after CMU's single-channel codec,
//! released for unrestricted use).

const WL: [i32; 8] = [-60, -30, 58, 172, 334, 538, 1198, 3042];
const RL42: [usize; 16] = [0, 7, 6, 5, 4, 3, 2, 1, 7, 6, 5, 4, 3, 2, 1, 0];
const ILB: [i32; 32] = [
    2048, 2093, 2139, 2186, 2233, 2282, 2332, 2383, 2435, 2489, 2543, 2599, 2656, 2714, 2774, 2834,
    2896, 2960, 3025, 3091, 3158, 3228, 3298, 3371, 3444, 3520, 3597, 3676, 3756, 3838, 3922, 4008,
];
const WH: [i32; 3] = [0, -214, 798];
const RH2: [usize; 4] = [2, 1, 2, 1];
const QM2: [i32; 4] = [-7408, -1616, 7408, 1616];
const QM4: [i32; 16] = [
    0, -20456, -12896, -8968, -6288, -4240, -2584, -1200, 20456, 12896, 8968, 6288, 4240, 2584,
    1200, 0,
];
const QM6: [i32; 64] = [
    -136, -136, -136, -136, -24808, -21904, -19008, -16704, -14984, -13512, -12280, -11192, -10232,
    -9360, -8576, -7856, -7192, -6576, -6000, -5456, -4944, -4464, -4008, -3576, -3168, -2776,
    -2400, -2032, -1688, -1360, -1040, -728, 24808, 21904, 19008, 16704, 14984, 13512, 12280,
    11192, 10232, 9360, 8576, 7856, 7192, 6576, 6000, 5456, 4944, 4464, 4008, 3576, 3168, 2776,
    2400, 2032, 1688, 1360, 1040, 728, 432, 136, -432, -136,
];
const QMF: [i32; 12] = [3, -11, 12, 32, -210, 951, 3876, -805, 362, -156, 53, -11];

/// `value` held to 16 bits, as the reference saturates.
fn saturate(value: i32) -> i32 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX))
}

/// One band's adaptive predictor.
#[derive(Clone, Debug, Default)]
struct Band {
    s: i32,
    sp: i32,
    sz: i32,
    r: [i32; 3],
    a: [i32; 3],
    ap: [i32; 3],
    p: [i32; 3],
    d: [i32; 7],
    b: [i32; 7],
    bp: [i32; 7],
    sg: [i32; 7],
    nb: i32,
    det: i32,
}

impl Band {
    fn new(det: i32) -> Self {
        Self {
            det,
            ..Self::default()
        }
    }

    /// Adapt the scale factor by `weight`, within `ceiling`, its step shifted
    /// by `bias`.
    fn scale(&mut self, weight: i32, ceiling: i32, bias: i32) {
        self.nb = (((self.nb * 127) >> 7) + weight).clamp(0, ceiling);
        let mantissa = ILB[usize::try_from((self.nb >> 6) & 31).unwrap_or(0)];
        let shift = bias - (self.nb >> 11);
        let det = if shift < 0 {
            mantissa << -shift
        } else {
            mantissa >> shift
        };
        self.det = det << 2;
    }

    /// Block 4: reconstruct, adapt the poles and zeros, and predict the next
    /// sample, from the quantised difference `d`.
    fn adapt(&mut self, d: i32) {
        self.d[0] = d;
        self.r[0] = saturate(self.s + d);
        self.p[0] = saturate(self.sz + d);
        // UPPOL2.
        for i in 0..3 {
            self.sg[i] = self.p[i] >> 15;
        }
        let wd1 = saturate(self.a[1] << 2);
        let wd2 = (if self.sg[0] == self.sg[1] { -wd1 } else { wd1 }).min(32767);
        let toward = if self.sg[0] == self.sg[2] { 128 } else { -128 };
        let wd3 = toward + (wd2 >> 7) + ((self.a[2] * 32512) >> 15);
        self.ap[2] = wd3.clamp(-12288, 12288);
        // UPPOL1.
        self.sg[0] = self.p[0] >> 15;
        self.sg[1] = self.p[1] >> 15;
        let wd1 = if self.sg[0] == self.sg[1] { 192 } else { -192 };
        let wd2 = (self.a[1] * 32640) >> 15;
        let limit = saturate(15360 - self.ap[2]);
        self.ap[1] = saturate(wd1 + wd2).clamp(-limit, limit);
        // UPZERO.
        let step = if d == 0 { 0 } else { 128 };
        self.sg[0] = d >> 15;
        for i in 1..7 {
            self.sg[i] = self.d[i] >> 15;
            let toward = if self.sg[i] == self.sg[0] {
                step
            } else {
                -step
            };
            self.bp[i] = saturate(toward + ((self.b[i] * 32640) >> 15));
        }
        // DELAYA.
        for i in (1..7).rev() {
            self.d[i] = self.d[i - 1];
            self.b[i] = self.bp[i];
        }
        for i in (1..3).rev() {
            self.r[i] = self.r[i - 1];
            self.p[i] = self.p[i - 1];
            self.a[i] = self.ap[i];
        }
        // FILTEP, FILTEZ, PREDIC.
        let pole = |r: i32, a: i32| (a * saturate(r + r)) >> 15;
        self.sp = saturate(pole(self.r[1], self.a[1]) + pole(self.r[2], self.a[2]));
        let zeros: i32 = (1..7)
            .map(|i| (self.b[i] * saturate(self.d[i] + self.d[i])) >> 15)
            .sum();
        self.sz = saturate(zeros);
        self.s = saturate(self.sp + self.sz);
    }
}

/// One channel's decoder.
#[derive(Clone, Debug)]
pub(crate) struct G722 {
    low: Band,
    high: Band,
    /// The receive QMF's history.
    x: [i32; 24],
}

impl G722 {
    /// A decoder in the reference's initial state.
    pub(crate) fn new() -> Self {
        Self {
            low: Band::new(32),
            high: Band::new(8),
            x: [0; 24],
        }
    }

    /// Decode one eight-bit code to its two 16 kHz samples, in order. A
    /// sample the QMF drives past 16 bits is held at the rail, where the
    /// reference would wrap it.
    pub(crate) fn decode(&mut self, code: u8) -> (i16, i16) {
        let sample = |wide: i32| i16::try_from(saturate(wide)).unwrap_or(0);
        let (first, second) = self.decode_wide(code);
        (sample(first), sample(second))
    }

    /// Decode one code to the reference's own two samples before they are
    /// narrowed.
    pub(crate) fn decode_wide(&mut self, code: u8) -> (i32, i32) {
        let code = usize::from(code);
        // The low band: six bits, of which four adapt the predictor.
        let low = code & 0x3F;
        let rlow = (self.low.s + ((self.low.det * QM6[low]) >> 15)).clamp(-16384, 16383);
        let quantised = low >> 2;
        let dlow = (self.low.det * QM4[quantised]) >> 15;
        self.low.scale(WL[RL42[quantised]], 18432, 8);
        self.low.adapt(dlow);
        // The high band: two bits.
        let high = (code >> 6) & 0x03;
        let dhigh = (self.high.det * QM2[high]) >> 15;
        let rhigh = (dhigh + self.high.s).clamp(-16384, 16383);
        self.high.scale(WH[RH2[high]], 22528, 10);
        self.high.adapt(dhigh);
        // The receive QMF.
        self.x.copy_within(2.., 0);
        self.x[22] = rlow + rhigh;
        self.x[23] = rlow - rhigh;
        let (mut first, mut second) = (0, 0);
        for i in 0..12 {
            second += self.x[2 * i] * QMF[i];
            first += self.x[2 * i + 1] * QMF[11 - i];
        }
        (first >> 11, second >> 11)
    }
}

#[cfg(test)]
#[path = "g722_tests.rs"]
mod tests;
