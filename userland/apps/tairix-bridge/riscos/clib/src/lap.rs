//! A walk through links a client could have overwritten, checked for coming
//! round again by Brent's method (R. P. Brent, "An improved Monte Carlo
//! factorization algorithm", BIT 20, 1980): a few words of state, told each
//! place the walk reaches, answering whether the walk has been there within
//! twice the steps it took to start going round.

/// A walk's check for coming round again.
pub(crate) struct Lap {
    saved: u32,
    power: u32,
    taken: u32,
}

impl Lap {
    /// The check of a walk starting at `start`.
    pub(crate) const fn from(start: u32) -> Self {
        Self {
            saved: start,
            power: 1,
            taken: 0,
        }
    }

    /// Whether the walk, reaching `at`, has come round to a place it was.
    pub(crate) fn repeated(&mut self, at: u32) -> bool {
        if at == self.saved {
            return true;
        }
        self.taken += 1;
        if self.taken == self.power {
            self.saved = at;
            self.power = self.power.saturating_mul(2);
            self.taken = 0;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lap_is_found_within_twice_the_steps_to_it_and_never_on_a_straight_walk() {
        // 0 to 5, then round 5 to 11 for ever.
        let next = |n: u32| if n < 5 { n + 1 } else { 5 + (n - 4) % 7 };
        let (mut lap, mut at) = (Lap::from(0), 0);
        let steps = (1u32..=1000)
            .find(|_| {
                at = next(at);
                lap.repeated(at)
            })
            .expect("found");
        assert!(steps <= 2 * (5 + 7), "{steps} steps");
        let mut lap = Lap::from(0);
        assert!((1..10_000).all(|n| !lap.repeated(n)));
    }
}
