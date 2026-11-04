//! A running count of something given up on, due for a record each time it
//! doubles.

/// A count of losses recorded by its running total each time it has doubled
/// since the last record: the first loss is always recorded, and a flood of
/// them costs the log a record per doubling rather than one per loss.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Doubling {
    count: u64,
    recorded: u64,
}

impl Doubling {
    /// Nothing counted.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            count: 0,
            recorded: 0,
        }
    }

    /// Count one more.
    pub fn note(&mut self) {
        self.count = self.count.saturating_add(1);
    }

    /// How many have been counted.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// The running total, once it has doubled since it was last taken.
    pub fn take_due(&mut self) -> Option<u64> {
        let due = self.count > self.recorded && self.count >= self.recorded.saturating_mul(2);
        due.then(|| {
            self.recorded = self.count;
            self.count
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Doubling;

    /// The first count and each doubling after it are due, and nothing else.
    #[test]
    fn a_count_is_due_at_its_first_and_each_time_it_doubles() {
        let mut lost = Doubling::new();
        assert_eq!(lost.take_due(), None, "nothing counted, nothing due");
        let (mut due, mut taken) = ([0u64; 4], 0);
        for _ in 0..9 {
            lost.note();
            if let Some(total) = lost.take_due() {
                due[taken] = total;
                taken += 1;
            }
        }
        assert_eq!((taken, due), (4, [1, 2, 4, 8]));
        assert_eq!(lost.count(), 9);
    }

    /// A count at the top of its range stays due no more often than before.
    #[test]
    fn a_saturated_count_is_due_once() {
        let mut lost = Doubling {
            count: u64::MAX - 1,
            recorded: u64::MAX / 2,
        };
        lost.note();
        assert_eq!(lost.take_due(), Some(u64::MAX));
        lost.note();
        assert_eq!(lost.take_due(), None);
    }
}
