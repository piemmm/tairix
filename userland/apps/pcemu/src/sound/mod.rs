//! Sound: the PC speaker, the YM3812, the Sound Blaster's DSP and the
//! parallel port's DACs ([`crate::ports::parallel`]), mixed at the YM3812's own rate — the bus oscillator
//! over 288, 49.716 kHz — and carried to the audio stream's rate by the one
//! resampler.
//!
//! A source that holds a level and changes it at an instant hands the mixer
//! the change and its instant in bus-oscillator ticks, and the change is drawn
//! band-limited ([`steps`]); the YM3812 hands it samples already at the mix's
//! rate. The mix is AC-coupled, as every card's output is, then resampled.
//! The front-end reports how full its audio ring is, and the mixer steers the
//! resampling ratio by at most ±0.5 % to hold it at its target.

pub mod blaster;
pub mod opl2;
pub mod speaker;
pub mod steps;

use crate::snapshot::{check, In, Out, State, Torn};
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use steps::{Steps, FRACTION, MAX_PENDING};
use tairix_audio::{FilterBank, Ratio, Resampler};

/// Bus-oscillator ticks a mix sample lasts.
pub const TICKS_PER_SAMPLE: u64 = FRACTION as u64;

/// The steering's resolution and reach: steps of 0.05 %, at most ten either
/// way.
const STEER_SCALE: u64 = 2_000;
const STEER_REACH: i64 = 10;

/// The AC coupling's pole: a 10 Hz high-pass at the mix's rate.
const COUPLING: f32 = 0.998_736;

/// The share of each fill report the steering's error takes: a ring's fill
/// jitters as the emulation runs in bursts, and only its drift should move
/// the ratio, each step of which rebuilds a filter bank.
const SMOOTHING: f64 = 1.0 / 16.0;

/// Where a sound comes from, for its level in the mix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The PC speaker.
    Speaker,
    /// The YM3812.
    Fm,
    /// The Sound Blaster's DAC.
    Dsp,
    /// A DAC on the parallel port.
    Parallel,
}

/// Why a mixer could not be built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MixError {
    /// A buffer or the resampler's filter could not be allocated.
    OutOfMemory,
    /// The stream's rate is zero.
    Rate,
}

/// The mix and its way to the stream.
#[derive(Debug)]
pub struct Mixer {
    steps: Steps,
    gains: [f32; 4],
    coupled: (f32, f32),
    scratch: Vec<f32>,
    stream: u32,
    steering: i64,
    /// The fill's error, smoothed, and its integral.
    error: f64,
    integral: f64,
    bank: FilterBank,
    resampler: Resampler,
    out: VecDeque<f32>,
    /// Changes and samples the mix could not hold, and rendered frames
    /// dropped because nothing drained them.
    dropped: u64,
}

/// The greatest common divisor, by Euclid.
const fn gcd(a: u64, b: u64) -> u64 {
    let (mut a, mut b) = (a, b);
    while b != 0 {
        let next = a % b;
        a = b;
        b = next;
    }
    a
}

/// Mix frames per stream frame for a stream at `stream` hertz, steered by
/// `steering` steps, reduced to terms the resampler takes.
fn ratio(stream: u32, steering: i64) -> Option<Ratio> {
    // The mix's rate is 315 MHz over 22 × 288.
    let scale = i64::try_from(STEER_SCALE).ok()?;
    let mut input = 315_000_000u64 * u64::try_from(scale + steering).ok()?;
    let mut output = 6_336 * u64::from(stream) * STEER_SCALE;
    let divisor = gcd(input, output).max(1);
    input /= divisor;
    output /= divisor;
    // A rate with no common factor is carried within a part in 2^32.
    while input > u64::from(u32::MAX) || output > u64::from(u32::MAX) {
        input >>= 1;
        output >>= 1;
    }
    Ratio::new(u32::try_from(input).ok()?, u32::try_from(output).ok()?)
}

impl Mixer {
    /// A silent mix carried to a stream at `stream` hertz.
    ///
    /// # Errors
    ///
    /// [`MixError::Rate`] for a zero rate; [`MixError::OutOfMemory`] when
    /// the filter or the kernel cannot be allocated.
    pub fn new(stream: u32) -> Result<Self, MixError> {
        let ratio = ratio(stream, 0).ok_or(MixError::Rate)?;
        let bank = FilterBank::for_ratio(ratio).map_err(|_| MixError::OutOfMemory)?;
        let resampler = Resampler::new(&bank, 1).map_err(|_| MixError::OutOfMemory)?;
        Ok(Self {
            steps: Steps::new(0).map_err(|_| MixError::OutOfMemory)?,
            gains: [0.25, 1.0, 1.0, 1.0],
            coupled: (0.0, 0.0),
            scratch: Vec::new(),
            stream,
            steering: 0,
            error: 0.0,
            integral: 0.0,
            bank,
            resampler,
            out: VecDeque::new(),
            dropped: 0,
        })
    }

    /// The stream's rate.
    #[must_use]
    pub const fn stream_rate(&self) -> u32 {
        self.stream
    }

    /// Sets `source`'s level in the mix.
    pub fn set_gain(&mut self, source: Source, gain: f32) {
        self.gains[source as usize] = gain;
    }

    /// Changes and frames lost to memory or to nothing draining the
    /// stream.
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Whether sample `sample` is too far past the last render to hold.
    fn too_far(&self, sample: u64) -> bool {
        sample > self.steps.base() + MAX_PENDING
    }

    /// `source`'s level changes by `delta` at bus-oscillator tick `tick`.
    pub fn step(&mut self, source: Source, tick: u64, delta: f32) {
        let sample = tick / TICKS_PER_SAMPLE;
        let fraction = u32::try_from(tick % TICKS_PER_SAMPLE).unwrap_or(0);
        let gain = self.gains[source as usize];
        if self.too_far(sample) || self.steps.step(sample, fraction, delta * gain).is_err() {
            self.dropped += 1;
        }
    }

    /// `source`'s sample `index` at the mix's rate.
    pub fn sample(&mut self, source: Source, index: u64, value: f32) {
        let gain = self.gains[source as usize];
        if self.too_far(index) || self.steps.sample(index, value * gain).is_err() {
            self.dropped += 1;
        }
    }

    /// Renders everything due by bus-oscillator tick `tick` into the
    /// stream, but the last samples a later change could still reach.
    pub fn render(&mut self, tick: u64) {
        self.render_samples(tick / TICKS_PER_SAMPLE);
    }

    fn render_samples(&mut self, until: u64) {
        self.scratch.clear();
        let wanted = usize::try_from(until.saturating_sub(self.steps.base())).unwrap_or(0);
        if self.scratch.try_reserve(wanted).is_err() {
            self.dropped += 1;
            self.steps.take(until, &mut |_| {});
            return;
        }
        let (scratch, coupled) = (&mut self.scratch, &mut self.coupled);
        self.steps.take(until, &mut |x| {
            let (last_in, last_out) = *coupled;
            let y = x - last_in + COUPLING * last_out;
            *coupled = (x, y);
            scratch.push(y);
        });
        let mut at = 0;
        let mut frames = [0.0f32; 512];
        while at < self.scratch.len() {
            let input = &self.scratch[at..];
            let Ok((consumed, produced)) = self.resampler.process(&self.bank, input, &mut frames)
            else {
                self.dropped += 1;
                return;
            };
            self.push_frames(&frames[..produced]);
            at += consumed;
            if consumed == 0 && produced == 0 {
                return;
            }
        }
    }

    /// Queues rendered frames for the stream, dropping the oldest past a
    /// second's worth.
    fn push_frames(&mut self, frames: &[f32]) {
        let limit = usize::try_from(self.stream).unwrap_or(usize::MAX);
        let frames = &frames[frames.len().saturating_sub(limit)..];
        let excess = (self.out.len() + frames.len()).saturating_sub(limit);
        self.out.drain(..excess.min(self.out.len()));
        self.dropped += u64::try_from(excess).unwrap_or(0);
        if self.out.try_reserve(frames.len()).is_err() {
            self.dropped += u64::try_from(frames.len()).unwrap_or(0);
            return;
        }
        self.out.extend(frames);
    }

    /// The first sample the mix has not yet rendered.
    #[must_use]
    pub const fn unrendered(&self) -> u64 {
        self.steps.base()
    }

    /// Stream frames rendered and waiting.
    #[must_use]
    pub fn buffered(&self) -> usize {
        self.out.len()
    }

    /// Moves rendered frames into `out` as interleaved stereo; returns the
    /// frames moved.
    pub fn take(&mut self, out: &mut [f32]) -> usize {
        let mut moved = 0;
        for frame in out.as_chunks_mut::<2>().0 {
            let Some(level) = self.out.pop_front() else {
                break;
            };
            frame.fill(level);
            moved += 1;
        }
        moved
    }

    /// Steers the resampling toward holding the stream's ring at `target`
    /// frames when it holds `fill`: fuller plays the mix faster, emptier
    /// slower, by at most ±0.5 %. Called once each time the stream is fed.
    pub fn steer(&mut self, fill: u32, target: u32) {
        if target == 0 {
            return;
        }
        let error = (f64::from(fill) - f64::from(target)) / f64::from(target);
        self.error += (error - self.error) * SMOOTHING;
        self.integral = (self.integral + self.error / 100.0).clamp(-1.0, 1.0);
        let reach = f64::from(i32::try_from(STEER_REACH).unwrap_or(0));
        let wanted = tairix_util::mathf::round_i32((self.error + self.integral) * reach);
        let steering = i64::from(wanted).clamp(-STEER_REACH, STEER_REACH);
        if steering == self.steering {
            return;
        }
        let Some(ratio) = ratio(self.stream, steering) else {
            return;
        };
        let Ok(bank) = FilterBank::for_ratio(ratio) else {
            self.dropped += 1;
            return;
        };
        if self.resampler.retune(&bank).is_ok() {
            self.bank = bank;
            self.steering = steering;
        }
    }
}

/// Measuring the sound a test makes.
#[cfg(test)]
pub(crate) mod testing {
    use tairix_util::mathf;

    /// The mix's rate, in hertz.
    pub const RATE: f64 = 315_000_000.0 / 6_336.0;

    /// The power of `samples`, taken at `rate`, at `frequency`, under a
    /// Hann window: the Goertzel filter's.
    pub fn power(samples: &[f32], rate: f64, frequency: f64) -> f64 {
        let n = mathf::real(samples.len());
        let coefficient = 2.0 * mathf::cos(2.0 * core::f64::consts::PI * frequency / rate);
        let (mut s1, mut s2) = (0.0, 0.0);
        for (i, &x) in samples.iter().enumerate() {
            let window = 0.5 - 0.5 * mathf::cos(2.0 * core::f64::consts::PI * mathf::real(i) / n);
            let s0 = f64::from(x) * window + coefficient * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s1 * s1 + s2 * s2 - coefficient * s1 * s2) / (n * n)
    }

    /// Rising zero crossings: a tone's cycles.
    pub fn cycles(samples: &[f32]) -> usize {
        samples
            .windows(2)
            .filter(|pair| pair[0] < 0.0 && pair[1] >= 0.0)
            .count()
    }
}

/// The mix keeps its pending samples, where they begin and its levels; what
/// it has already rendered for the stream, and how it steers to it, begin
/// again with the stream a resumed machine opens.
impl State for Mixer {
    fn save(&self, out: &mut Out) {
        let Self {
            steps,
            gains,
            coupled,
            scratch: _,
            stream: _,
            steering: _,
            error: _,
            integral: _,
            bank: _,
            resampler: _,
            out: _,
            dropped: _,
        } = self;
        steps.save(out);
        gains.save(out);
        coupled.save(out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        self.steps.restore(input)?;
        self.gains.restore(input)?;
        self.coupled.restore(input)?;
        check(
            self.gains.iter().all(|gain| gain.is_finite())
                && self.coupled.0.is_finite()
                && self.coupled.1.is_finite(),
        )
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
