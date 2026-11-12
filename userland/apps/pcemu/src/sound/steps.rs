//! Band-limited steps: a level that holds and changes at exact instants —
//! the speaker's, a DAC's — drawn at the mix's rate without the aliasing a
//! sampled edge would fold back. Each change adds the windowed sinc's
//! impulse, scaled by its size, at its fractional position; the running sum
//! of the impulses is the band-limited level.

use alloc::collections::VecDeque;
use alloc::vec::Vec;
use tairix_audio::Lowpass;

use crate::snapshot::{check, restore_queue, save_queue, In, Out, State, Torn};

/// Samples an impulse spans: enough for its transition to fit between the
/// stream's passband and the image of it the mix's rate folds back.
pub const TAPS: usize = 64;
/// Positions between two samples the impulse is tabulated at; a step
/// between two is interpolated.
const PHASES: usize = 64;
/// The impulse's half-amplitude point, in cycles per sample.
const CUTOFF: f64 = 0.466;
/// Parts of a sample a position is given in: the bus oscillator's ticks.
pub const FRACTION: u32 = 288;

/// The furthest past the last render a change may land: a second. The
/// board renders far more often; this bounds what a source running away from
/// it could make the mix hold.
pub const MAX_PENDING: u64 = PENDING as u64;
const PENDING: usize = 49_716;

/// The most samples a change may leave pending: those it may land ahead,
/// and an impulse's span.
const MOST_HELD: usize = PENDING + TAPS;

/// Why steps could not be drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutOfMemory;

/// The pending samples and the impulses and sampled sources landing in
/// them.
#[derive(Clone, Debug)]
pub struct Steps {
    /// `PHASES + 1` rows of `TAPS`: the impulse at each fractional position.
    kernel: Vec<f32>,
    impulses: VecDeque<f32>,
    direct: VecDeque<f32>,
    /// The first pending sample.
    base: u64,
    /// The running sum of every impulse before `base`.
    level: f64,
}

impl Steps {
    /// No pending samples, the first at sample `base`.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the kernel cannot be allocated.
    pub fn new(base: u64) -> Result<Self, OutOfMemory> {
        let lowpass = Lowpass::new(TAPS, CUTOFF);
        let mut kernel = Vec::new();
        kernel
            .try_reserve_exact((PHASES + 1) * TAPS)
            .map_err(|_| OutOfMemory)?;
        let real = |n: usize| f64::from(u16::try_from(n).unwrap_or(0));
        for phase in 0..=PHASES {
            let fraction = real(phase) / real(PHASES);
            let row: [f64; TAPS] =
                core::array::from_fn(|tap| lowpass.at(real(tap) - real(TAPS / 2 - 1) - fraction));
            // Each row sums to one, so a step's level settles at its size.
            let sum: f64 = row.iter().sum();
            kernel.extend(row.iter().map(|&tap| tairix_util::mathf::single(tap / sum)));
        }
        Ok(Self {
            kernel,
            impulses: VecDeque::new(),
            direct: VecDeque::new(),
            base,
            level: 0.0,
        })
    }

    /// The first pending sample.
    #[must_use]
    pub const fn base(&self) -> u64 {
        self.base
    }

    /// The samples pending.
    #[must_use]
    pub fn pending(&self) -> usize {
        self.impulses.len()
    }

    /// Makes room through sample `last`.
    fn reach(&mut self, last: u64) -> Result<(), OutOfMemory> {
        let needed =
            usize::try_from(last.saturating_sub(self.base) + 1).map_err(|_| OutOfMemory)?;
        if needed > self.impulses.len() {
            let more = needed - self.impulses.len();
            self.impulses.try_reserve(more).map_err(|_| OutOfMemory)?;
            self.direct.try_reserve(more).map_err(|_| OutOfMemory)?;
            self.impulses.resize(needed, 0.0);
            self.direct.resize(needed, 0.0);
        }
        Ok(())
    }

    /// A change of `delta` at `fraction` of [`FRACTION`] past sample
    /// `sample`. A change too early for samples already taken lands on the
    /// first it still can.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the pending samples cannot grow to hold it.
    pub fn step(&mut self, sample: u64, fraction: u32, delta: f32) -> Result<(), OutOfMemory> {
        let half = u64::try_from(TAPS / 2 - 1).unwrap_or(0);
        let (sample, fraction) = if sample < self.base + half {
            (self.base + half, 0)
        } else {
            (sample, fraction.min(FRACTION - 1))
        };
        let first = sample - half;
        self.reach(first + u64::try_from(TAPS - 1).unwrap_or(0))?;
        let scaled = u64::from(fraction) * u64::try_from(PHASES).unwrap_or(0);
        let phase = usize::try_from(scaled / u64::from(FRACTION)).unwrap_or(0);
        let blend = tairix_util::mathf::single(
            f64::from(u32::try_from(scaled % u64::from(FRACTION)).unwrap_or(0))
                / f64::from(FRACTION),
        );
        let lower = &self.kernel[phase * TAPS..(phase + 1) * TAPS];
        let upper = &self.kernel[(phase + 1) * TAPS..(phase + 2) * TAPS];
        let start = usize::try_from(first - self.base).unwrap_or(0);
        for ((slot, low), high) in self
            .impulses
            .range_mut(start..start + TAPS)
            .zip(lower)
            .zip(upper)
        {
            *slot += delta * (low + (high - low) * blend);
        }
        Ok(())
    }

    /// Adds `value` to sample `sample` as it is, as a sampled source
    /// produces it; a sample already taken is too late and is dropped.
    ///
    /// # Errors
    ///
    /// [`OutOfMemory`] when the pending samples cannot grow to hold it.
    pub fn sample(&mut self, sample: u64, value: f32) -> Result<(), OutOfMemory> {
        if sample < self.base {
            return Ok(());
        }
        self.reach(sample)?;
        if let Some(slot) = self
            .direct
            .get_mut(usize::try_from(sample - self.base).unwrap_or(0))
        {
            *slot += value;
        }
        Ok(())
    }

    /// The samples before `until` that no later step can still reach, in
    /// order, each the band-limited level plus the sampled sources.
    pub fn take(&mut self, until: u64, out: &mut impl FnMut(f32)) {
        let reachable = u64::try_from(TAPS / 2).unwrap_or(0);
        let last = until.saturating_sub(reachable);
        while self.base < last {
            let impulse = self.impulses.pop_front().unwrap_or(0.0);
            let direct = self.direct.pop_front().unwrap_or(0.0);
            self.level += f64::from(impulse);
            out(tairix_util::mathf::single(self.level) + direct);
            self.base += 1;
        }
    }
}

impl State for Steps {
    fn save(&self, out: &mut Out) {
        let Self {
            kernel: _,
            impulses,
            direct,
            base,
            level,
        } = self;
        save_queue(impulses, out);
        save_queue(direct, out);
        base.save(out);
        level.save(out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        restore_queue(&mut self.impulses, MOST_HELD, input)?;
        restore_queue(&mut self.direct, MOST_HELD, input)?;
        self.base.restore(input)?;
        self.level.restore(input)?;
        // Far from the counter's end, so the positions a change lands at do
        // not overflow, and every level a number.
        check(
            self.impulses.len() == self.direct.len()
                && self.base < 1 << 62
                && self.level.is_finite()
                && self
                    .impulses
                    .iter()
                    .chain(&self.direct)
                    .all(|x| x.is_finite()),
        )
    }
}

#[cfg(test)]
#[path = "steps_tests.rs"]
mod tests;
