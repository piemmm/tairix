//! Tickless load-average accounting.
//!
//! [`LoadTracker`] keeps the three classic exponentially damped run-queue
//! averages (1, 5 and 15 minutes) without a periodic timer. Each average
//! moves toward the runnable census by the factor `d = e^(−5 s / window)`
//! once every 5-second period, `load ← load·d + census·(1 − d)`; the kernel
//! is tickless, so instead of a sampling tick the tracker folds in the whole
//! periods elapsed at each *observation* (a `sysinfo_introspect`
//! load-average read). With the census held still, `n` periods are the
//! recurrence's closed form `load·dⁿ + census·(1 − dⁿ)`, so a long quiet gap
//! costs one power, never a step per missed period.
//!
//! Between observations the census is not sampled, so the census read *at*
//! the observation stands in for the whole gap: a sampling tick armed only
//! for load accounting is exactly the needless interrupt the tickless kernel
//! forbids.

use core::sync::atomic::{AtomicU64, Ordering};

use tairix_abi::sysinfo::LOAD_FIXED_SHIFT;

/// A load of 1.00 in the fixed-point representation.
const ONE: u64 = 1 << LOAD_FIXED_SHIFT;

/// One damping period, in nanoseconds.
const PERIOD_NS: u64 = 5_000_000_000;

/// Each window's per-period factor `e^(−5 s / window)` for the 1, 5 and 15
/// minute windows, rounded to [`LOAD_FIXED_SHIFT`]-bit fixed point.
const DECAY: [u64; 3] = [1884, 2014, 2037];

/// A product of two fixed-point values, rounded to nearest.
const fn product(a: u64, b: u64) -> u64 {
    (a * b + ONE / 2) >> LOAD_FIXED_SHIFT
}

/// `factor^n` in fixed point, by squaring (Knuth, TAOCP vol. 2, §4.6.3), so a
/// gap of any length costs a logarithmic number of products.
fn power(factor: u64, mut n: u64) -> u64 {
    let mut result = ONE;
    let mut square = factor;
    while n > 0 {
        if n & 1 == 1 {
            result = product(result, square);
        }
        n >>= 1;
        if n > 0 {
            square = product(square, square);
        }
    }
    result
}

/// `load` after periods whose combined factor is `decay`, toward `census`.
///
/// Rounded once, over the whole sum: two truncations would leave the
/// recurrence's resting point below the census.
const fn fold(load: u64, decay: u64, census: u64) -> u64 {
    (load * decay + census * (ONE - decay) + ONE / 2) >> LOAD_FIXED_SHIFT
}

/// The three damped run-queue averages, advanced at observation points.
///
/// Lock-free, so an observation never blocks a syscall path: the instant
/// the averages reach is claimed whole periods at a time by one
/// compare-exchange, and each average folds in the periods its observer
/// claimed atomically, so two observers that claim consecutive spans both
/// count.
pub struct LoadTracker {
    /// The averages in [`LOAD_FIXED_SHIFT`]-bit fixed point.
    loads: [AtomicU64; 3],
    /// The monotonic instant (ns) the averages have been advanced to.
    reached_ns: AtomicU64,
}

impl LoadTracker {
    /// A tracker at boot: no load, no period elapsed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            loads: [AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)],
            reached_ns: AtomicU64::new(0),
        }
    }

    /// Fold the periods since the last advance into the averages with the
    /// runnable census `runnable`, and return the fixed-point
    /// `[load1, load5, load15]` (saturated to `u32`, the wire width).
    ///
    /// A reading that crosses no period boundary, or a clock that seems to
    /// run backwards, leaves the averages as they are.
    pub fn observe(&self, now_ns: u64, runnable: u64) -> [u32; 3] {
        let reached = self.reached_ns.load(Ordering::Acquire);
        let periods = now_ns.saturating_sub(reached) / PERIOD_NS;
        let claimed = periods > 0
            && self
                .reached_ns
                .compare_exchange(
                    reached,
                    reached + periods * PERIOD_NS,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok();
        if claimed {
            let census = runnable.saturating_mul(ONE);
            for (load, factor) in self.loads.iter().zip(DECAY) {
                let decay = power(factor, periods);
                // Never `None`: the update always answers a value.
                let _ = load.fetch_update(Ordering::AcqRel, Ordering::Acquire, |now| {
                    Some(fold(now, decay, census))
                });
            }
        }
        self.loads
            .each_ref()
            .map(|load| u32::try_from(load.load(Ordering::Acquire)).unwrap_or(u32::MAX))
    }
}

impl Default for LoadTracker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{fold, power, LoadTracker, DECAY, ONE, PERIOD_NS};

    #[test]
    fn each_factor_is_its_window_s_decay_rounded() {
        for (factor, minutes) in DECAY.into_iter().zip([1.0f64, 5.0, 15.0]) {
            let exact = (-5.0 / (minutes * 60.0)).exp() * f64::from(1u32 << 11);
            let factor = f64::from(u32::try_from(factor).expect("a factor below one"));
            assert!(
                (factor - exact).abs() <= 0.5,
                "{minutes} min: {factor} vs {exact}"
            );
        }
    }

    #[test]
    fn a_power_agrees_with_its_repeated_product() {
        for n in 0..64u64 {
            let mut repeated = ONE;
            for _ in 0..n {
                repeated = super::product(repeated, DECAY[0]);
            }
            // Squaring rounds at other points, so the two agree to a few
            // units in the last place, never more.
            let squared = power(DECAY[0], n);
            assert!(
                squared.abs_diff(repeated) <= 8,
                "n = {n}: {squared} vs {repeated}"
            );
        }
        assert_eq!(power(3, 0), ONE);
    }

    #[test]
    fn a_steady_census_is_where_every_average_settles() {
        let census = 2 * ONE;
        for factor in DECAY {
            let (mut rising, mut falling) = (0, 8 * ONE);
            for _ in 0..2000 {
                rising = fold(rising, factor, census);
                falling = fold(falling, factor, census);
            }
            // The rounded recurrence rests within (ONE / 2) / (ONE − d) of
            // the census, widest for the longest window.
            let band = ONE / (ONE - factor) + 2;
            assert!(rising.abs_diff(census) <= band, "rising {rising}");
            assert!(falling.abs_diff(census) <= band, "falling {falling}");
        }
    }

    #[test]
    fn a_long_gap_is_its_periods_taken_one_at_a_time() {
        let census = 3 * ONE;
        for factor in DECAY {
            let mut stepped = 5 * ONE;
            for _ in 0..17 {
                stepped = fold(stepped, factor, census);
            }
            let jumped = fold(5 * ONE, power(factor, 17), census);
            assert!(stepped.abs_diff(jumped) <= 17, "{stepped} vs {jumped}");
        }
    }

    #[test]
    fn only_a_crossed_period_boundary_advances_the_averages() {
        let tracker = LoadTracker::new();
        assert_eq!(tracker.observe(PERIOD_NS - 1, 100), [0, 0, 0]);
        let after = tracker.observe(PERIOD_NS, 100);
        assert!(after[0] > 0);
        assert!(after[0] > after[2], "the short window moves fastest");
    }

    #[test]
    fn a_clock_running_backwards_changes_nothing() {
        let tracker = LoadTracker::new();
        let loads = tracker.observe(10 * PERIOD_NS, 4);
        assert_eq!(tracker.observe(0, 400), loads);
    }

    #[test]
    fn an_idle_system_decays_to_nothing() {
        let tracker = LoadTracker::new();
        assert!(tracker.observe(PERIOD_NS, 8)[0] > 0);
        assert_eq!(tracker.observe(3 * 3600 * 1_000_000_000, 0), [0, 0, 0]);
    }

    /// Two observers that claim consecutive spans each fold theirs in: a
    /// load-then-store per average lost one span to the other's store, which
    /// leaves about half the load the two spans give.
    #[test]
    fn concurrent_observers_never_lose_a_claimed_span() {
        use core::sync::atomic::{AtomicUsize, Ordering};
        // The interpreter preempts at random points in every round, so it
        // needs far fewer rounds than hardware to reach the racing orders.
        const ROUNDS: usize = if cfg!(miri) { 1_000 } else { 400_000 };
        const STOP: usize = usize::MAX;
        let spans = |periods: &[u64]| {
            let tracker = LoadTracker::new();
            periods
                .iter()
                .fold([0; 3], |_, &n| tracker.observe(n * PERIOD_NS, 50))
        };
        // Whichever claims first, the averages end at one of these, which
        // differ only in rounding.
        let (one_by_one, at_once) = (spans(&[1, 2]), spans(&[2]));
        let tracker = LoadTracker::new();
        let (round, finished) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let wait = |until: &dyn Fn() -> bool| {
            while !until() {
                std::thread::yield_now();
            }
        };
        let mut lost = None;
        std::thread::scope(|scope| {
            for now in [PERIOD_NS, 2 * PERIOD_NS] {
                let (tracker, round, finished) = (&tracker, &round, &finished);
                scope.spawn(move || {
                    for this in 1..=ROUNDS {
                        wait(&|| matches!(round.load(Ordering::Acquire), r if r == this || r == STOP));
                        if round.load(Ordering::Acquire) == STOP {
                            return;
                        }
                        tracker.observe(now, 50);
                        finished.fetch_add(1, Ordering::AcqRel);
                    }
                });
            }
            for this in 1..=ROUNDS {
                tracker
                    .loads
                    .iter()
                    .for_each(|load| load.store(0, Ordering::Relaxed));
                tracker.reached_ns.store(0, Ordering::Relaxed);
                finished.store(0, Ordering::Relaxed);
                round.store(this, Ordering::Release);
                wait(&|| finished.load(Ordering::Acquire) == 2);
                let reached = tracker.observe(2 * PERIOD_NS, 50);
                let legal = (0..3).all(|window| {
                    let (a, b) = (one_by_one[window], at_once[window]);
                    (a.min(b)..=a.max(b)).contains(&reached[window])
                });
                if !legal {
                    lost = Some((this, reached));
                    // Release the observers, or the scope waits on them for ever.
                    round.store(STOP, Ordering::Release);
                    break;
                }
            }
        });
        assert_eq!(
            lost, None,
            "a span was lost: {one_by_one:?} or {at_once:?} expected"
        );
    }
}
