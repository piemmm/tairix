//! Every chain a volume links — a hash chain, a file's extension blocks, an
//! OFS file's data blocks, an object's hard links, the bitmap's extension
//! blocks — is untrusted: walking one is bounded, and a loop in it ends the
//! walk as damage.

use tairix_abi::DriverError;

/// Brent's cycle detection over one chain of block numbers, with a bound on
/// the links taken.
pub(crate) struct Guard {
    tortoise: u64,
    power: u64,
    lam: u64,
    left: u64,
}

impl Guard {
    /// A walk from `start`, taking at most `limit` links.
    pub(crate) const fn new(start: u64, limit: u64) -> Self {
        Self {
            tortoise: start,
            power: 1,
            lam: 0,
            left: limit,
        }
    }

    /// Admit the chain's next link, `block`.
    ///
    /// # Errors
    ///
    /// [`DriverError::BadMagic`] once the chain returns to a block it
    /// visited, or outruns its bound.
    pub(crate) fn step(&mut self, block: u64) -> Result<(), DriverError> {
        if block == self.tortoise || self.left == 0 {
            return Err(DriverError::BadMagic);
        }
        self.left -= 1;
        self.lam += 1;
        if self.lam == self.power {
            self.tortoise = block;
            self.power = self.power.saturating_mul(2);
            self.lam = 0;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Guard;

    #[test]
    fn a_loop_of_any_length_is_caught_and_a_long_chain_is_not() {
        for len in 1..40u64 {
            let mut guard = Guard::new(100, u64::MAX);
            let mut caught = false;
            for step in 0..200u64 {
                // 100 -> 101 -> ... -> 100+len-1 -> 100 -> ...
                let next = 100 + (step + 1) % len;
                if guard.step(next).is_err() {
                    caught = true;
                    break;
                }
            }
            assert!(caught, "a loop of {len}");
        }
        let mut guard = Guard::new(0, u64::MAX);
        assert!((1..10_000).all(|block| guard.step(block).is_ok()));
        let mut bounded = Guard::new(0, 3);
        assert!((1..=3).all(|block| bounded.step(block).is_ok()));
        assert!(bounded.step(4).is_err(), "past its bound");
    }
}
