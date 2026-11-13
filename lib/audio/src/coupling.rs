//! AC coupling: the one-pole high-pass a capacitor in a line output makes,
//! taking a signal's DC away and passing what lies above its corner.

/// A one-pole high-pass, `y = x - x₋₁ + pole · y₋₁`, with the last input
/// and output it remembers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AcCoupling {
    pole: f32,
    last_in: f32,
    last_out: f32,
}

impl AcCoupling {
    /// A coupling at rest of `pole`, below 1: the nearer 1, the lower its
    /// corner, e^(−2π · corner / rate).
    #[must_use]
    pub const fn new(pole: f32) -> Self {
        Self {
            pole,
            last_in: 0.0,
            last_out: 0.0,
        }
    }

    /// The output for the next input `x`.
    pub fn step(&mut self, x: f32) -> f32 {
        let y = x - self.last_in + self.pole * self.last_out;
        self.last_in = x;
        self.last_out = y;
        y
    }

    /// The last input and output, as a snapshot keeps them.
    #[must_use]
    pub const fn state(&self) -> (f32, f32) {
        (self.last_in, self.last_out)
    }

    /// The last input and output restored from a snapshot, refused unless
    /// both are finite.
    #[must_use]
    pub fn with_state(self, (last_in, last_out): (f32, f32)) -> Option<Self> {
        (last_in.is_finite() && last_out.is_finite()).then_some(Self {
            last_in,
            last_out,
            ..self
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_steady_level_decays_to_nothing_and_a_step_passes_at_once() {
        let mut coupling = AcCoupling::new(0.99);
        assert_eq!(
            coupling.step(1.0).to_bits(),
            1.0f32.to_bits(),
            "a step passes whole"
        );
        let mut last = 1.0;
        for _ in 0..2000 {
            last = coupling.step(1.0);
        }
        assert!(last.abs() < 1e-6, "{last}");
    }

    #[test]
    fn state_survives_a_round_trip_and_refuses_what_is_not_finite() {
        let mut coupling = AcCoupling::new(0.5);
        coupling.step(0.25);
        let state = coupling.state();
        assert_eq!(AcCoupling::new(0.5).with_state(state), Some(coupling));
        assert_eq!(AcCoupling::new(0.5).with_state((f32::NAN, 0.0)), None);
        assert_eq!(AcCoupling::new(0.5).with_state((0.0, f32::INFINITY)), None);
    }
}
