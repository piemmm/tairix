//! The external interrupt vectors, one pool every x86_64 source draws from.
//!
//! Every external vector's interrupt-table entry is installed at boot, so a
//! vector delivers the moment the arch routing table names the line it
//! raises. A message-signalled source takes its vector when it is allocated
//! and gives it back once nothing can raise it; an IO-APIC pin takes one the
//! first time it is bound and keeps it for the boot, since nothing proves an
//! interrupt a pin raised has left every CPU. A machine's pins therefore cost
//! vectors only as their lines are used, however many its IO-APICs carry.

use core::sync::atomic::{AtomicU64, Ordering};

use tairix_arch_x86_64::irq::{EXTERNAL_VECTOR_COUNT, EXTERNAL_VECTOR_FIRST};

const WORDS: usize = EXTERNAL_VECTOR_COUNT.div_ceil(64);

/// Which external vectors are claimed, and the CPU whose interrupt table
/// holds them.
pub struct VectorPool {
    destination: u32,
    /// Bit `i` of word `i / 64`: vector `EXTERNAL_VECTOR_FIRST + i` is
    /// claimed.
    claimed: [AtomicU64; WORDS],
}

impl VectorPool {
    /// Every external vector free, installed on the CPU whose APIC id is
    /// `destination`.
    #[must_use]
    pub const fn new(destination: u32) -> Self {
        Self {
            destination,
            claimed: [const { AtomicU64::new(0) }; WORDS],
        }
    }

    /// The APIC id of the CPU the vectors are installed on: the only one an
    /// interrupt raising one may name.
    #[must_use]
    pub const fn destination(&self) -> u32 {
        self.destination
    }

    /// Claim the lowest free vector, or [`None`] when every one is claimed.
    #[must_use]
    pub fn claim(&self) -> Option<u8> {
        for (word, claimed) in self.claimed.iter().enumerate() {
            let first = word * 64;
            let usable = (EXTERNAL_VECTOR_COUNT - first).min(64);
            let mask = u64::MAX >> (64 - usable);
            let mut current = claimed.load(Ordering::Acquire);
            while !current & mask != 0 {
                let bit = (!current & mask).trailing_zeros();
                match claimed.compare_exchange_weak(
                    current,
                    current | (1 << bit),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => {
                        let offset = u8::try_from(first + bit as usize).ok()?;
                        return EXTERNAL_VECTOR_FIRST.checked_add(offset);
                    }
                    Err(seen) => current = seen,
                }
            }
        }
        None
    }

    /// Give `vector` back: one nothing can raise any more. A vector outside
    /// the external range is ignored.
    pub fn release(&self, vector: u8) {
        let Some(index) = vector
            .checked_sub(EXTERNAL_VECTOR_FIRST)
            .map(usize::from)
            .filter(|&index| index < EXTERNAL_VECTOR_COUNT)
        else {
            return;
        };
        self.claimed[index / 64].fetch_and(!(1 << (index % 64)), Ordering::Release);
    }
}

#[cfg(all(freestanding, kernel_isa = "x86_64"))]
mod live {
    use tairix_arch_x86_64::irq as arch_irq;
    use tairix_arch_x86_64::percpu;
    use tairix_sync::Once;

    use super::VectorPool;

    /// The boot's pool, published once [`install`] installed its vectors.
    static POOL: Once<VectorPool> = Once::new();

    /// Why the external vectors could not be installed.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    pub struct Uninstalled;

    /// Install every external vector's entry in the boot CPU's interrupt
    /// table, that CPU's APIC id being `destination`, and publish the pool
    /// they are claimed from. Once per boot.
    ///
    /// # Errors
    ///
    /// [`Uninstalled`] for a second call, or an entry the table refused.
    pub fn install(destination: u32) -> Result<&'static VectorPool, Uninstalled> {
        if !matches!(POOL.get(), Ok(None)) {
            return Err(Uninstalled);
        }
        for vector in arch_irq::EXTERNAL_VECTOR_FIRST..=arch_irq::EXTERNAL_VECTOR_LAST {
            let isr = arch_irq::external_isr_addr(vector).ok_or(Uninstalled)?;
            // SAFETY: the boot CPU finished `percpu::init(0)` before the boot
            // reaches interrupt setup, interrupts are masked, and `vector` is
            // in the reserved external range, never an exception's.
            unsafe { percpu::install_vector(0, vector, isr) }.map_err(|_| Uninstalled)?;
        }
        POOL.call_once_infallible(|| VectorPool::new(destination))
            .map_err(|_| Uninstalled)
    }

    /// The boot's pool, or [`None`] before [`install`].
    #[must_use]
    pub fn published() -> Option<&'static VectorPool> {
        POOL.get().ok().flatten()
    }
}

#[cfg(all(freestanding, kernel_isa = "x86_64"))]
pub use live::{install, published, Uninstalled};

#[cfg(test)]
mod tests {
    use super::*;

    use tairix_arch_x86_64::irq::EXTERNAL_VECTOR_LAST;

    #[test]
    fn every_external_vector_is_claimed_once_lowest_first() {
        let pool = VectorPool::new(7);
        let mut expected = EXTERNAL_VECTOR_FIRST;
        while let Some(vector) = pool.claim() {
            assert_eq!(vector, expected);
            expected = expected.wrapping_add(1);
        }
        assert_eq!(
            usize::from(expected - EXTERNAL_VECTOR_FIRST),
            EXTERNAL_VECTOR_COUNT
        );
        assert_eq!(pool.claim(), None);
        assert_eq!(pool.destination(), 7);
    }

    #[test]
    fn a_vector_given_back_is_claimed_again() {
        let pool = VectorPool::new(0);
        let kept = pool.claim().expect("a vector");
        for _ in 0..1000 {
            let vector = pool.claim().expect("a vector");
            assert_ne!(vector, kept);
            pool.release(vector);
        }
        while pool.claim().is_some() {}
        pool.release(EXTERNAL_VECTOR_LAST);
        assert_eq!(pool.claim(), Some(EXTERNAL_VECTOR_LAST));
    }

    #[test]
    fn a_vector_outside_the_external_range_gives_nothing_back() {
        let pool = VectorPool::new(0);
        while pool.claim().is_some() {}
        pool.release(EXTERNAL_VECTOR_FIRST - 1);
        pool.release(0xFF);
        assert_eq!(pool.claim(), None);
    }
}
