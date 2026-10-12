//! The polyphase synthesis filterbank (ISO/IEC 11172-3 §2.4.3.2.2, Annex A
//! Figure A.2): each slot of 32 subband samples becomes 32 PCM samples.
//!
//! The matrixing `V[i] = Σ cos((16 + i)(2k + 1)π/64) S[k]` is a 32-point
//! DCT-II, `X[m] = Σ cos(m(2k + 1)π/64) S[k]`, read at shifted and mirrored
//! indices: `V[i]` is `X[16 + i]` up to 16, `X[32]` is zero, and the rest are
//! `X` mirrored and negated. So a slot costs one 32-point DCT-II, over the
//! shared transform's 16-point FFT, and the 512-tap window rather than a
//! 64 × 32 product.

use alloc::vec::Vec;

use tairix_util::fallible::filled;

use super::tables::WINDOW_STEPS;
use crate::transform::Dct2;
use crate::{single, DecodeError};

/// Subbands, and samples a slot makes.
pub(super) const SUBBANDS: usize = 32;

/// Vectors the window spans.
const HISTORY: usize = 16;

/// The filterbank's matrixing and window, shared by every channel.
pub(super) struct SynthesisTables {
    dct: Dct2,
    /// The window D.
    window: Vec<f32>,
}

impl SynthesisTables {
    /// The constants, computed once.
    ///
    /// # Errors
    ///
    /// [`DecodeError::OutOfMemory`] when they cannot be allocated.
    pub(super) fn build() -> Result<Self, DecodeError> {
        let dct = Dct2::new(SUBBANDS).ok_or(DecodeError::OutOfMemory)?;
        let mut window = filled(WINDOW_STEPS.len(), 0f32).ok_or(DecodeError::OutOfMemory)?;
        for (slot, &steps) in window.iter_mut().zip(WINDOW_STEPS.iter()) {
            *slot = single(f64::from(steps) / 65_536.0);
        }
        Ok(Self { dct, window })
    }
}

/// One channel's filterbank state: the last sixteen matrixed vectors.
pub(super) struct Synthesis {
    history: [[f32; 2 * SUBBANDS]; HISTORY],
    /// Where the newest vector is.
    newest: usize,
}

impl Synthesis {
    /// A filterbank that has heard nothing.
    pub(super) const fn new() -> Self {
        Self {
            history: [[0.0; 2 * SUBBANDS]; HISTORY],
            newest: 0,
        }
    }

    /// Forget everything heard, as at a stream's start.
    pub(super) fn reset(&mut self) {
        *self = Self::new();
    }

    /// Turn one slot of subband samples into 32 PCM samples, written to
    /// `out` every `stride` floats.
    pub(super) fn slot(
        &mut self,
        tables: &mut SynthesisTables,
        subbands: &[f32; SUBBANDS],
        out: &mut [f32],
        stride: usize,
    ) {
        let mut dct = [0f32; SUBBANDS];
        tables.dct.process(subbands, &mut dct);
        self.newest = (self.newest + HISTORY - 1) % HISTORY;
        let v = &mut self.history[self.newest];
        v[..16].copy_from_slice(&dct[16..]);
        v[16] = 0.0;
        for i in 17..48 {
            v[i] = -dct[48 - i];
        }
        for i in 48..64 {
            v[i] = -dct[i - 48];
        }
        for (j, sample) in out.iter_mut().step_by(stride).take(SUBBANDS).enumerate() {
            let mut sum = 0.0f32;
            for i in 0..8 {
                let even = &self.history[(self.newest + 2 * i) % HISTORY];
                let odd = &self.history[(self.newest + 2 * i + 1) % HISTORY];
                sum += even[j] * tables.window[64 * i + j];
                sum += odd[SUBBANDS + j] * tables.window[64 * i + SUBBANDS + j];
            }
            *sample = sum;
        }
    }
}

#[cfg(test)]
mod tests {
    use tairix_util::mathf::cos;

    use super::*;

    /// The direct form the standard writes: a 64 × 32 matrixing, then the
    /// window over the vectors' U arrangement.
    struct Reference {
        v: [f64; 1024],
    }

    impl Reference {
        fn slot(&mut self, subbands: &[f32; SUBBANDS]) -> [f64; SUBBANDS] {
            self.v.copy_within(0..1024 - 64, 64);
            for (v, i) in self.v[..64].iter_mut().zip(0u32..) {
                *v = subbands
                    .iter()
                    .zip(0u32..)
                    .map(|(&subband, k)| {
                        let angle =
                            f64::from((16 + i) * (2 * k + 1)) * core::f64::consts::PI / 64.0;
                        cos(angle) * f64::from(subband)
                    })
                    .sum();
            }
            let mut u = [0f64; 512];
            for i in 0..8 {
                for j in 0..32 {
                    u[i * 64 + j] = self.v[i * 128 + j];
                    u[i * 64 + 32 + j] = self.v[i * 128 + 96 + j];
                }
            }
            let mut out = [0f64; SUBBANDS];
            for (j, sample) in out.iter_mut().enumerate() {
                *sample = (0..16)
                    .map(|i| u[j + 32 * i] * f64::from(WINDOW_STEPS[j + 32 * i]) / 65_536.0)
                    .sum();
            }
            out
        }
    }

    /// The fast form matches the standard's direct one well inside the
    /// compliance limit on every slot of a stream that excites every
    /// subband.
    #[test]
    fn the_filterbank_matches_the_standards_direct_form() {
        let mut tables = SynthesisTables::build().expect("tables");
        let mut fast = Synthesis::new();
        let mut reference = Reference { v: [0.0; 1024] };
        let mut worst = 0f64;
        for slot in 0..40u16 {
            let mut subbands = [0f32; SUBBANDS];
            for (subband, k) in subbands.iter_mut().zip(0u16..) {
                *subband = (f32::from((slot * 7 + k * 13) % 29) - 14.0) / 16.0;
            }
            let mut out = [0f32; SUBBANDS];
            fast.slot(&mut tables, &subbands, &mut out, 1);
            let expected = reference.slot(&subbands);
            for (got, want) in out.iter().zip(expected) {
                worst = worst.max((f64::from(*got) - want).abs());
            }
        }
        assert!(worst < 1e-5, "worst {worst}");
    }
}
