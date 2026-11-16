//! The SID's filter and the C64's audio output stage.
//!
//! The filter is a two-integrator state-variable loop whose cutoff follows
//! each chip's measured curve from the 11-bit cutoff register — the 6581's
//! steep and broken at $400, the 8580's nearly straight — and whose
//! resonance raises Q from 0.707 to 1.7 (reSID 0.16's measurements). The
//! output stage after it is two RC filters: a 16 kHz low pass and a 16 Hz
//! high pass that takes the chip's DC away.

use alloc::boxed::Box;
use core::f64::consts::PI;

use super::Model;
use crate::snapshot;

/// Mode and volume register bits.
pub(super) mod mode {
    pub const LOW_PASS: u8 = 0x10;
    pub const BAND_PASS: u8 = 0x20;
    pub const HIGH_PASS: u8 = 0x40;
    /// Voice 3 left out of the mix, unless it goes through the filter.
    pub const VOICE3_OFF: u8 = 0x80;
    pub const VOLUME: u8 = 0x0F;
}

/// Each chip's cutoff in hertz at points of the cutoff register, the ends
/// and the 6581's break given twice to steer the curve through them.
const CUTOFF_6581: &[(u16, f64)] = &[
    (0, 220.0),
    (0, 220.0),
    (128, 230.0),
    (256, 250.0),
    (384, 300.0),
    (512, 420.0),
    (640, 780.0),
    (768, 1600.0),
    (832, 2300.0),
    (896, 3200.0),
    (960, 4300.0),
    (992, 5000.0),
    (1008, 5400.0),
    (1016, 5700.0),
    (1023, 6000.0),
    (1023, 6000.0),
    (1024, 4600.0),
    (1024, 4600.0),
    (1032, 4800.0),
    (1056, 5300.0),
    (1088, 6000.0),
    (1120, 6600.0),
    (1152, 7200.0),
    (1280, 9500.0),
    (1408, 12000.0),
    (1536, 14500.0),
    (1664, 16000.0),
    (1792, 17100.0),
    (1920, 17700.0),
    (2047, 18000.0),
    (2047, 18000.0),
];

const CUTOFF_8580: &[(u16, f64)] = &[
    (0, 0.0),
    (0, 0.0),
    (128, 800.0),
    (256, 1600.0),
    (384, 2500.0),
    (512, 3300.0),
    (640, 4100.0),
    (768, 4800.0),
    (896, 5600.0),
    (1024, 6500.0),
    (1152, 7500.0),
    (1280, 8400.0),
    (1408, 9200.0),
    (1536, 9800.0),
    (1664, 10500.0),
    (1792, 11000.0),
    (1920, 11700.0),
    (2047, 12500.0),
    (2047, 12500.0),
];

/// Settings of the cutoff register.
const CUTOFFS: usize = 2048;

/// The highest cutoff a loop clocked once a cycle stays stable at.
const STABLE_CUTOFF: f64 = 16_000.0;

/// The cutoff for every setting of the register: a cubic through each
/// pair of points, its slopes taken from their neighbours.
pub(super) fn cutoff_curve(model: Model) -> Box<[f64]> {
    let points = match model {
        Model::Mos6581 => CUTOFF_6581,
        Model::Mos8580 => CUTOFF_8580,
    };
    let mut curve = alloc::vec![0.0; CUTOFFS].into_boxed_slice();
    for window in points.windows(4) {
        let [(x0, y0), (x1, y1), (x2, y2), (x3, y3)] = [window[0], window[1], window[2], window[3]];
        if x1 == x2 {
            continue;
        }
        let (x0, x1, x2, x3) = (f64::from(x0), f64::from(x1), f64::from(x2), f64::from(x3));
        let span = x2 - x1;
        let slope1 = (y2 - y0) / (x2 - x0);
        let slope2 = (y3 - y1) / (x3 - x1);
        let (first, last) = (usize::from(window[1].0), usize::from(window[2].0));
        for (x, cutoff) in curve.iter_mut().enumerate().take(last + 1).skip(first) {
            let t = (f64::from(u16::try_from(x).unwrap_or(u16::MAX)) - x1) / span;
            let (t2, t3) = (t * t, t * t * t);
            *cutoff = (2.0 * t3 - 3.0 * t2 + 1.0) * y1
                + (t3 - 2.0 * t2 + t) * span * slope1
                + (-2.0 * t3 + 3.0 * t2) * y2
                + (t3 - t2) * span * slope2;
        }
    }
    curve
}

/// The filter's registers and state.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Filter {
    cutoff: u16,
    resonance: u8,
    /// Which voices, and the external input, go through the filter.
    routing: u8,
    mode: u8,
    high: f64,
    band: f64,
    low: f64,
    /// The integrators' gain a cycle, and 1/Q: worked out from the cutoff
    /// and the resonance, never restored.
    gain: f64,
    damping: f64,
}

/// A level no voice, filter or output stage reaches by far: a restored state
/// past it can only be forged, and would run the filter off to infinity.
const LEVEL_BOUND: f64 = 4_294_967_296.0;

fn within_bound(levels: &[f64]) -> bool {
    levels.iter().all(|level| level.abs() <= LEVEL_BOUND)
}

snapshot::fields!(
    Filter {
        cutoff,
        resonance,
        routing,
        mode,
        high,
        band,
        low
    }
    transient { gain, damping },
    check | f | f.cutoff < 0x800
        && f.resonance < 16
        && f.routing < 16
        && within_bound(&[f.high, f.band, f.low])
);

impl Filter {
    pub(super) const fn new() -> Self {
        Self {
            cutoff: 0,
            resonance: 0,
            routing: 0,
            mode: 0,
            high: 0.0,
            band: 0.0,
            low: 0.0,
            gain: 0.0,
            damping: 1.0 / 0.707,
        }
    }

    pub(super) fn write_cutoff_low(&mut self, value: u8, curve: &[f64], clock: u64) {
        self.cutoff = (self.cutoff & 0x7F8) | u16::from(value & 0x07);
        self.update_gain(curve, clock);
    }

    pub(super) fn write_cutoff_high(&mut self, value: u8, curve: &[f64], clock: u64) {
        self.cutoff = (u16::from(value) << 3) | (self.cutoff & 0x007);
        self.update_gain(curve, clock);
    }

    pub(super) fn write_resonance_routing(&mut self, value: u8) {
        self.resonance = value >> 4;
        self.routing = value & 0x0F;
        self.update_damping();
    }

    /// The gain and damping the cutoff and resonance give, as a restored
    /// state needs them worked out again.
    pub(super) fn retune(&mut self, curve: &[f64], clock: u64) {
        self.update_gain(curve, clock);
        self.update_damping();
    }

    fn update_damping(&mut self) {
        self.damping = 1.0 / (0.707 + f64::from(self.resonance) / 15.0);
    }

    pub(super) const fn write_mode_volume(&mut self, value: u8) {
        self.mode = value;
    }

    fn update_gain(&mut self, curve: &[f64], clock: u64) {
        let hertz = curve.get(usize::from(self.cutoff)).copied().unwrap_or(0.0);
        let clock = f64::from(u32::try_from(clock).unwrap_or(u32::MAX));
        self.gain = 2.0 * PI * hertz.min(STABLE_CUTOFF) / clock;
    }

    /// A cycle with the voices at `voices` and the mixer's own offset
    /// `mixer_dc`: the mixed, filtered output before the volume.
    pub(super) fn clock(&mut self, voices: [f64; 3], mixer_dc: f64) -> f64 {
        let mut voices = voices;
        // Voice 3's switch does not reach it through the filter.
        if self.mode & mode::VOICE3_OFF != 0 && self.routing & 0x04 == 0 {
            voices[2] = 0.0;
        }
        let (mut filtered, mut direct) = (0.0, 0.0);
        for (i, voice) in voices.into_iter().enumerate() {
            if self.routing & 1 << i != 0 {
                filtered += voice;
            } else {
                direct += voice;
            }
        }
        let band_step = self.gain * self.high;
        let low_step = self.gain * self.band;
        self.band -= band_step;
        self.low -= low_step;
        self.high = self.band * self.damping - self.low - filtered;
        let mut out = direct + mixer_dc;
        for (bit, value) in [
            (mode::LOW_PASS, self.low),
            (mode::BAND_PASS, self.band),
            (mode::HIGH_PASS, self.high),
        ] {
            if self.mode & bit != 0 {
                out += value;
            }
        }
        out * f64::from(self.mode & mode::VOLUME)
    }
}

/// The output stage: a low pass and a DC-blocking high pass, as their RC
/// time constants give them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Output {
    low: f64,
    high: f64,
}

snapshot::fields!(
    Output { low, high },
    check | o | within_bound(&[o.low, o.high])
);

/// The output stage's corners, in radians a second: 10 kΩ with 1 nF, and
/// 1 kΩ with 10 µF.
const OUTPUT_LOW_PASS: f64 = 100_000.0;
const OUTPUT_HIGH_PASS: f64 = 100.0;

/// The output stage's corners as fractions of a cycle of a machine clock.
#[derive(Clone, Copy, Debug)]
pub(super) struct OutputRates {
    low_pass: f64,
    high_pass: f64,
}

impl OutputRates {
    /// The rates at machine clock `clock`.
    pub(super) fn at(clock: u64) -> Self {
        let clock = f64::from(u32::try_from(clock).unwrap_or(u32::MAX));
        Self {
            low_pass: OUTPUT_LOW_PASS / clock,
            high_pass: OUTPUT_HIGH_PASS / clock,
        }
    }
}

impl Output {
    pub(super) const fn new() -> Self {
        Self {
            low: 0.0,
            high: 0.0,
        }
    }

    /// A cycle of `input`.
    pub(super) fn clock(&mut self, input: f64, rates: OutputRates) -> f64 {
        self.low += rates.low_pass * (input - self.low);
        self.high += rates.high_pass * (self.low - self.high);
        self.low - self.high
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `filter` written into a snapshot and read back.
    fn reloaded(filter: &Filter) -> Result<Filter, snapshot::SnapshotError> {
        let mut w = snapshot::Writer::new();
        w.put(filter);
        let body = snapshot::open(&w.finish().expect("fits")).expect("opens");
        snapshot::Reader::new(&body).get::<Filter>()
    }

    #[test]
    fn a_restored_filter_is_tuned_from_its_registers_and_levels_are_bounded() {
        let curve = cutoff_curve(Model::Mos6581);
        let clock = 985_248;
        let mut filter = Filter::new();
        filter.write_cutoff_high(0x80, &curve, clock);
        filter.write_resonance_routing(0xF1);
        filter.band = 1234.5;
        let mut restored = reloaded(&filter).expect("restores");
        restored.retune(&curve, clock);
        assert_eq!(restored, filter, "gain and damping worked out again");
        filter.low = 1.0e300;
        assert!(reloaded(&filter).is_err(), "a level only a forger writes");
    }

    #[test]
    fn the_curves_pass_through_their_points() {
        let curve = cutoff_curve(Model::Mos6581);
        assert!((curve[0] - 220.0).abs() < 1e-9);
        assert!((curve[1023] - 6000.0).abs() < 1e-9);
        assert!((curve[1024] - 4600.0).abs() < 1e-9, "the break");
        assert!((curve[2047] - 18000.0).abs() < 1e-9);
        let curve = cutoff_curve(Model::Mos8580);
        assert!((curve[1024] - 6500.0).abs() < 1e-9);
        assert!(
            curve.windows(2).all(|pair| pair[1] >= pair[0]),
            "the 8580 only climbs"
        );
    }

    #[test]
    fn the_low_pass_lets_a_constant_through_and_the_high_pass_does_not() {
        let curve = cutoff_curve(Model::Mos8580);
        let mut filter = Filter::new();
        filter.write_cutoff_high(0x80, &curve, 1_000_000);
        filter.write_resonance_routing(0x01);
        filter.write_mode_volume(mode::LOW_PASS | 1);
        let mut out = 0.0;
        for _ in 0..20_000 {
            out = filter.clock([1000.0, 0.0, 0.0], 0.0);
        }
        assert!((out.abs() - 1000.0).abs() < 1.0, "{out}");
        filter.write_mode_volume(mode::HIGH_PASS | 1);
        for _ in 0..20_000 {
            out = filter.clock([1000.0, 0.0, 0.0], 0.0);
        }
        assert!(out.abs() < 1.0, "{out}");
    }

    #[test]
    fn the_output_stage_blocks_dc() {
        let mut output = Output::new();
        let rates = OutputRates::at(1_000_000);
        let mut out = 0.0;
        for _ in 0..1_000_000 {
            out = output.clock(5000.0, rates);
        }
        assert!(out.abs() < 1.0, "{out}");
    }
}
