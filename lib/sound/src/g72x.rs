//! The ITU-T G.721 and G.723 ADPCM decoders — G.726 at 32, 24 and 40 kbit/s
//! — ported from Sun's reference (`g72x.c`, `g721.c`, `g723_24.c`,
//! `g723_40.c`, released for unrestricted use), which passes the CCITT test
//! vectors. A quantity the reference holds in a C `short` is truncated to 16
//! bits wherever the reference assigns it, so the arithmetic is the
//! reference's to the bit.

/// The sizes the reference's base-2 logarithm and floating forms step by.
const POWER2: [i32; 15] = [
    1, 2, 4, 8, 0x10, 0x20, 0x40, 0x80, 0x100, 0x200, 0x400, 0x800, 0x1000, 0x2000, 0x4000,
];

/// How many of `table` lie at or below `value`.
fn quan(value: i32, table: &[i32]) -> i32 {
    let below = table.iter().take_while(|&&entry| value >= entry).count();
    i32::try_from(below).unwrap_or(i32::MAX)
}

/// `value` as the reference's C `short` holds it.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the reference truncates to 16 bits here"
)]
const fn short(value: i32) -> i16 {
    value as i16
}

/// The reference's multiply of a predictor coefficient by a sample held in
/// its 4-bit-exponent, 6-bit-mantissa floating form.
fn fmult(an: i32, srn: i32) -> i32 {
    let anmag = short(if an > 0 { an } else { (-an) & 0x1FFF });
    let anexp = short(quan(i32::from(anmag), &POWER2) - 6);
    let anmant = if anmag == 0 {
        32
    } else if anexp >= 0 {
        short(i32::from(anmag) >> anexp)
    } else {
        short(i32::from(anmag) << -anexp)
    };
    let wanexp = short(i32::from(anexp) + ((srn >> 6) & 0xF) - 13);
    let wanmant = short((i32::from(anmant) * (srn & 0o77) + 0x30) >> 4);
    let product = if wanexp >= 0 {
        short((i32::from(wanmant) << wanexp) & 0x7FFF)
    } else {
        short(i32::from(wanmant) >> -wanexp)
    };
    if (an ^ srn) < 0 {
        -i32::from(product)
    } else {
        i32::from(product)
    }
}

/// The reference's log-domain antilog of a quantised difference.
fn reconstruct(negative: bool, dqln: i32, y: i32) -> i32 {
    let dql = short(dqln + (y >> 2));
    if dql < 0 {
        return if negative { -0x8000 } else { 0 };
    }
    let dex = (i32::from(dql) >> 7) & 15;
    let dqt = 128 + (i32::from(dql) & 127);
    let dq = i32::from(short((dqt << 7) >> (14 - dex)));
    if negative {
        dq - 0x8000
    } else {
        dq
    }
}

/// A code's form in the reference's floating representation, from its
/// magnitude and sign.
fn floating(magnitude: i32, negative: bool) -> i16 {
    let exp = quan(magnitude, &POWER2);
    let value = (exp << 6) + ((magnitude << 6) >> exp);
    short(if negative { value - 0x400 } else { value })
}

/// A rate's tables: the code's sign bit, its log-domain difference, the
/// scale-factor and energy weights, and the magnitude mask a negative
/// difference is taken with.
struct Tables {
    bits: u32,
    dqln: &'static [i16],
    wi: &'static [i16],
    wi_shift: u32,
    fi: &'static [i16],
    magnitude: i32,
}

const G721: Tables = Tables {
    bits: 4,
    dqln: &[
        -2048, 4, 135, 213, 273, 323, 373, 425, 425, 373, 323, 273, 213, 135, 4, -2048,
    ],
    wi: &[
        -12, 18, 41, 64, 112, 198, 355, 1122, 1122, 355, 198, 112, 64, 41, 18, -12,
    ],
    wi_shift: 5,
    fi: &[
        0, 0, 0, 0x200, 0x200, 0x200, 0x600, 0xE00, 0xE00, 0x600, 0x200, 0x200, 0x200, 0, 0, 0,
    ],
    magnitude: 0x3FFF,
};

const G723_24: Tables = Tables {
    bits: 3,
    dqln: &[-2048, 135, 273, 373, 373, 273, 135, -2048],
    wi: &[-128, 960, 4384, 18624, 18624, 4384, 960, -128],
    wi_shift: 0,
    fi: &[0, 0x200, 0x400, 0xE00, 0xE00, 0x400, 0x200, 0],
    magnitude: 0x3FFF,
};

const G723_40: Tables = Tables {
    bits: 5,
    dqln: &[
        -2048, -66, 28, 104, 169, 224, 274, 318, 358, 395, 429, 459, 488, 514, 539, 566, 566, 539,
        514, 488, 459, 429, 395, 358, 318, 274, 224, 169, 104, 28, -66, -2048,
    ],
    wi: &[
        448, 448, 768, 1248, 1280, 1312, 1856, 3200, 4512, 5728, 7008, 8960, 11456, 14080, 16928,
        22272, 22272, 16928, 14080, 11456, 8960, 7008, 5728, 4512, 3200, 1856, 1312, 1280, 1248,
        768, 448, 448,
    ],
    wi_shift: 0,
    fi: &[
        0, 0, 0, 0, 0, 0x200, 0x200, 0x200, 0x200, 0x200, 0x400, 0x600, 0x800, 0xA00, 0xC00, 0xC00,
        0xC00, 0xC00, 0xA00, 0x800, 0x600, 0x400, 0x200, 0x200, 0x200, 0x200, 0x200, 0, 0, 0, 0, 0,
    ],
    magnitude: 0x7FFF,
};

/// Which of the three rates a stream is coded at.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum G72xRate {
    /// G.723 at 24 kbit/s: three-bit codes.
    Kbit24,
    /// G.721 at 32 kbit/s: four-bit codes.
    Kbit32,
    /// G.723 at 40 kbit/s: five-bit codes.
    Kbit40,
}

impl G72xRate {
    const fn tables(self) -> &'static Tables {
        match self {
            Self::Kbit24 => &G723_24,
            Self::Kbit32 => &G721,
            Self::Kbit40 => &G723_40,
        }
    }

    /// Bits a code occupies.
    pub(crate) const fn bits(self) -> u32 {
        self.tables().bits
    }
}

/// One channel's decoder.
#[derive(Clone, Debug)]
pub(crate) struct G72x {
    rate: G72xRate,
    /// The locked, steady-state step size multiplier.
    yl: i32,
    /// The unlocked step size multiplier.
    yu: i16,
    /// The short- and long-term energy estimates.
    dms: i16,
    dml: i16,
    /// The weight between the two multipliers.
    ap: i16,
    /// The predictor's pole and zero coefficients.
    a: [i16; 2],
    b: [i16; 6],
    /// The signs of the last two partially reconstructed samples.
    pk: [i16; 2],
    /// The last six quantised differences and two reconstructed samples, in
    /// floating form.
    dq: [i16; 6],
    sr: [i16; 2],
    /// A tone was detected on the previous sample.
    td: bool,
}

impl G72x {
    /// Bits a code occupies.
    pub(crate) const fn bits(&self) -> u32 {
        self.rate.bits()
    }

    /// A decoder at `rate`, in the reference's initial state.
    pub(crate) const fn new(rate: G72xRate) -> Self {
        Self {
            rate,
            yl: 34816,
            yu: 544,
            dms: 0,
            dml: 0,
            ap: 0,
            a: [0; 2],
            b: [0; 6],
            pk: [0; 2],
            dq: [32; 6],
            sr: [32; 2],
            td: false,
        }
    }

    fn predictor_zero(&self) -> i32 {
        self.b
            .iter()
            .zip(self.dq)
            .map(|(&b, dq)| fmult(i32::from(b) >> 2, i32::from(dq)))
            .sum()
    }

    fn predictor_pole(&self) -> i32 {
        fmult(i32::from(self.a[1]) >> 2, i32::from(self.sr[1]))
            + fmult(i32::from(self.a[0]) >> 2, i32::from(self.sr[0]))
    }

    fn step_size(&self) -> i32 {
        if self.ap >= 256 {
            return i32::from(self.yu);
        }
        let mut y = self.yl >> 6;
        let dif = i32::from(self.yu) - y;
        let al = i32::from(self.ap) >> 2;
        if dif > 0 {
            y += (dif * al) >> 6;
        } else if dif < 0 {
            y += (dif * al + 0x3F) >> 6;
        }
        y
    }

    /// Decode `code`, its bits past the rate's ignored, to a 16-bit linear
    /// sample. The reconstruction is 14-bit; a stream that drives it past
    /// that is held at the rails, where the reference would wrap it.
    pub(crate) fn decode(&mut self, code: u8) -> i16 {
        let wide = self.decode_wide(code);
        short(wide.clamp(i32::from(i16::MIN), i32::from(i16::MAX)))
    }

    /// Decode `code` to the reference's own output before it is narrowed.
    #[allow(
        clippy::similar_names,
        reason = "the reference's names, kept so the port can be read against it"
    )]
    pub(crate) fn decode_wide(&mut self, code: u8) -> i32 {
        let tables = self.rate.tables();
        let i = usize::from(code) & ((1 << tables.bits) - 1);
        let sign = 1 << (tables.bits - 1);
        let sezi = short(self.predictor_zero());
        let sez = short(i32::from(sezi) >> 1);
        let sei = short(i32::from(sezi) + self.predictor_pole());
        let se = i32::from(short(i32::from(sei) >> 1));
        let y = short(self.step_size());
        let dq = short(reconstruct(
            i & sign != 0,
            i32::from(tables.dqln[i]),
            i32::from(y),
        ));
        let sr = short(if dq < 0 {
            se - (i32::from(dq) & tables.magnitude)
        } else {
            se + i32::from(dq)
        });
        let dqsez = short(i32::from(sr) - se + i32::from(sez));
        let wi = i32::from(tables.wi[i]) << tables.wi_shift;
        let fi = i32::from(tables.fi[i]);
        self.update(
            i32::from(y),
            wi,
            fi,
            i32::from(dq),
            i32::from(sr),
            i32::from(dqsez),
        );
        i32::from(sr) << 2
    }

    /// The reference's state update after one sample.
    fn update(&mut self, y: i32, wi: i32, fi: i32, dq: i32, sr: i32, dqsez: i32) {
        let pk0 = i16::from(dqsez < 0);
        let mag = dq & 0x7FFF;
        // TRANS: a large difference after a detected tone is data, not voice.
        let transition = self.td && mag > self.data_threshold();
        // FUNCTW, FILTD, LIMB, FILTE: the step size multipliers.
        self.yu = short(y + ((wi - y) >> 5)).clamp(544, 5120);
        self.yl += i32::from(self.yu) + ((-self.yl) >> 6);
        let a2p = if transition {
            self.a = [0; 2];
            self.b = [0; 6];
            0
        } else {
            self.adapt_predictor(pk0, dq, mag, dqsez)
        };
        self.push_history(dq, mag, sr, pk0);
        // TONE.
        self.td = !transition && a2p < -11776;
        self.adapt_speed(transition, y, fi);
    }

    /// The difference past which a sample after a tone is taken as data:
    /// three quarters of the locked multiplier's threshold.
    fn data_threshold(&self) -> i32 {
        let ylint = self.yl >> 15;
        let ylfrac = (self.yl >> 10) & 0x1F;
        let thr2 = i32::from(short(if ylint > 9 {
            31 << 10
        } else {
            (32 + ylfrac) << ylint
        }));
        i32::from(short((thr2 + (thr2 >> 1)) >> 1))
    }

    /// UPA2, UPA1, LIMC, LIMD, UPB: adapt the poles and zeros, answering the
    /// second pole.
    fn adapt_predictor(&mut self, pk0: i16, dq: i32, mag: i32, dqsez: i32) -> i16 {
        let pks1 = pk0 ^ self.pk[0];
        let mut a2p = short(i32::from(self.a[1]) - (i32::from(self.a[1]) >> 7));
        if dqsez != 0 {
            let fa1 = if pks1 != 0 { self.a[0] } else { -self.a[0] };
            a2p = short(
                i32::from(a2p)
                    + if fa1 < -8191 {
                        -0x100
                    } else if fa1 > 8191 {
                        0xFF
                    } else {
                        i32::from(fa1) >> 5
                    },
            );
            a2p = if pk0 ^ self.pk[1] != 0 {
                if a2p <= -12160 {
                    -12288
                } else if a2p >= 12416 {
                    12288
                } else {
                    a2p - 0x80
                }
            } else if a2p <= -12416 {
                -12288
            } else if a2p >= 12160 {
                12288
            } else {
                a2p + 0x80
            };
        }
        self.a[1] = a2p;
        let mut a1 = i32::from(self.a[0]) - (i32::from(self.a[0]) >> 8);
        if dqsez != 0 {
            a1 += if pks1 == 0 { 192 } else { -192 };
        }
        let a1ul = 15360 - i32::from(a2p);
        self.a[0] = short(a1.clamp(-a1ul, a1ul));
        let leak = if self.rate == G72xRate::Kbit40 { 9 } else { 8 };
        for (b, &history) in self.b.iter_mut().zip(&self.dq) {
            let mut next = i32::from(*b) - (i32::from(*b) >> leak);
            if mag != 0 {
                next += if (dq ^ i32::from(history)) >= 0 {
                    128
                } else {
                    -128
                };
            }
            *b = short(next);
        }
        a2p
    }

    /// FLOAT A, FLOAT B, DELAY: the difference, the reconstruction and its
    /// sign into the history.
    fn push_history(&mut self, dq: i32, mag: i32, sr: i32, pk0: i16) {
        self.dq.copy_within(0..5, 1);
        self.dq[0] = if mag == 0 {
            short(if dq >= 0 { 0x20 } else { 0xFC20 })
        } else {
            floating(mag, dq < 0)
        };
        self.sr[1] = self.sr[0];
        self.sr[0] = match sr {
            0 => 0x20,
            1.. => floating(sr, false),
            -32767..=-1 => floating(-sr, true),
            _ => short(0xFC20),
        };
        self.pk[1] = self.pk[0];
        self.pk[0] = pk0;
    }

    /// FILTA, FILTB, SUBTC: the energies, and the weight between the two
    /// multipliers, which leans to the unlocked one for anything but steady
    /// voice.
    fn adapt_speed(&mut self, transition: bool, y: i32, fi: i32) {
        self.dms = short(i32::from(self.dms) + ((fi - i32::from(self.dms)) >> 5));
        self.dml = short(i32::from(self.dml) + (((fi << 2) - i32::from(self.dml)) >> 7));
        let ap = i32::from(self.ap);
        let unsteady = y < 1536
            || self.td
            || ((i32::from(self.dms) << 2) - i32::from(self.dml)).abs()
                >= (i32::from(self.dml) >> 3);
        self.ap = short(if transition {
            256
        } else if unsteady {
            ap + ((0x200 - ap) >> 4)
        } else {
            ap + ((-ap) >> 4)
        });
    }
}

#[cfg(test)]
#[path = "g72x_tests.rs"]
pub(crate) mod tests;
