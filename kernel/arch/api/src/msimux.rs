//! Message-interrupt multiplexer surface of the Arch HAL (optional,
//! `plans/WIRING.md` W18).
//!
//! Some PCIe root complexes capture an endpoint's message write at a doorbell
//! of their own and raise one wired line, the *parent*, for a whole bank of
//! vectors. The kernel must then read which vectors are pending and fan the
//! parent out to a line per vector, a chained handler no user-space driver can
//! be. Only the silicon is here; laying the vectors out on lines, handing them
//! out and fanning the parent out is the kernel's, once for every port.
//!
//! A port whose messages reach its interrupt controller directly implements
//! nothing, and `msi_alloc` fails closed there.

use core::num::NonZeroU32;

use tairix_abi::MsiMessage;

use crate::irq::IrqController;

/// What discovery found: the bank's width and the line it raises.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MsiMuxShape {
    /// How many vectors the bank demultiplexes, numbered `0..vectors`.
    pub vectors: NonZeroU32,
    /// The interrupt-controller line raised while any unmasked vector is
    /// pending.
    pub parent_line: u32,
}

/// Why a multiplexer refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MsiMuxError {
    /// Discovery found no multiplexer.
    NotSupported,
    /// The vector is outside the bank.
    OutOfRange,
}

/// A root complex's bank of message vectors behind one parent line.
///
/// The [`IrqController`] supertrait masks and unmasks *vectors*: an
/// undiscovered multiplexer has none, so every vector is
/// [`crate::IrqControlError::OutOfRange`], and `mask` publishes the masked
/// state before it returns.
pub trait MsiMultiplexer: IrqController {
    /// The discovered bank, or [`None`] where discovery found none.
    fn shape(&self) -> Option<MsiMuxShape>;

    /// Program the doorbell and data pattern so the bank captures messages.
    /// Idempotent: once it has succeeded, a later call touches nothing.
    ///
    /// # Errors
    ///
    /// [`MsiMuxError::NotSupported`] where discovery found none.
    fn enable(&self) -> Result<(), MsiMuxError>;

    /// The message a function writes to raise `vector`.
    ///
    /// # Errors
    ///
    /// [`MsiMuxError::NotSupported`] where discovery found none, and
    /// [`MsiMuxError::OutOfRange`] for a vector outside the bank.
    fn message(&self, vector: u32) -> Result<MsiMessage, MsiMuxError>;

    /// Read the pending vectors once and, lowest first, pass each to `fire`
    /// and then acknowledge it, so a `fire` that masks its vector does so
    /// before the bank stops raising the parent for it.
    fn drain(&self, fire: &mut dyn FnMut(u32));
}

/// The multiplexer conformance verticals, driven per port like
/// [`crate::irq::conformance`]: a discovered bank needs a way to raise its
/// vectors, and an undiscovered one is a separate claim.
pub mod conformance {
    use super::{MsiMultiplexer, MsiMuxError};
    use crate::irq::conformance::run_controller;
    use crate::irq::{IrqControlError, IrqController};

    /// Vectors whose messages are compared pairwise; a wider bank's are
    /// sampled from its start.
    const DISTINCT_CHECKED: u32 = 64;

    /// The most vectors one drain is expected to fire here.
    const FIRED_MAX: usize = 4;

    /// What one drain fired, in order.
    struct Fired {
        vectors: [u32; FIRED_MAX],
        count: usize,
    }

    impl Fired {
        fn seen(&self) -> (usize, &[u32]) {
            (self.count, &self.vectors[..self.count.min(FIRED_MAX)])
        }
    }

    fn drain_once<M: MsiMultiplexer + ?Sized>(mux: &M) -> Fired {
        let mut fired = Fired {
            vectors: [0; FIRED_MAX],
            count: 0,
        };
        mux.drain(&mut |vector| {
            if let Some(slot) = fired.vectors.get_mut(fired.count) {
                *slot = vector;
            }
            fired.count += 1;
        });
        fired
    }

    /// Run the suite against a discovered multiplexer; `raise(v)` makes
    /// vector `v` pending as its function's message would.
    ///
    /// # Panics
    ///
    /// Panics (failing the test) on any breach of the [`MsiMultiplexer`]
    /// contract.
    pub fn run_discovered<M: MsiMultiplexer + ?Sized>(mux: &M, raise: &mut dyn FnMut(u32)) {
        let shape = mux.shape();
        assert!(
            shape.is_some(),
            "a discovered multiplexer must report its shape"
        );
        let Some(shape) = shape else {
            return;
        };
        let vectors = shape.vectors.get();
        let last = vectors - 1;
        assert_eq!(mux.enable(), Ok(()), "a discovered multiplexer must enable");
        assert_eq!(mux.enable(), Ok(()), "enabling must be idempotent");
        run_controller(mux, 0, vectors);
        run_controller(mux, last, u32::MAX);
        let checked = vectors.min(DISTINCT_CHECKED);
        for a in 0..checked {
            let first = mux.message(a);
            assert!(first.is_ok(), "vector {a} must have a message");
            for b in a + 1..checked {
                assert_ne!(
                    first,
                    mux.message(b),
                    "messages must be pairwise distinct (vectors {a} and {b})"
                );
            }
        }
        assert_eq!(
            mux.message(vectors),
            Err(MsiMuxError::OutOfRange),
            "a vector past the bank must have no message"
        );
        assert_eq!(
            drain_once(mux).seen(),
            (0, &[][..]),
            "an idle multiplexer must fire nothing"
        );
        assert_eq!(
            IrqController::unmask(mux, 0),
            Ok(()),
            "vector 0 must unmask"
        );
        assert_eq!(
            IrqController::unmask(mux, last),
            Ok(()),
            "the last vector must unmask"
        );
        raise(last);
        raise(0);
        let expected: &[u32] = if last == 0 { &[0] } else { &[0, last] };
        assert_eq!(
            drain_once(mux).seen(),
            (expected.len(), expected),
            "a drain must fire every pending vector once, lowest first"
        );
        assert_eq!(
            drain_once(mux).seen(),
            (0, &[][..]),
            "a drain must acknowledge every vector it fired"
        );
    }

    /// Run the suite against a multiplexer discovery found none of.
    ///
    /// # Panics
    ///
    /// Panics (failing the test) if it reports a bank, enables, has a
    /// message, masks or fires anything.
    pub fn run_undiscovered<M: MsiMultiplexer + ?Sized>(mux: &M) {
        assert_eq!(
            mux.shape(),
            None,
            "an undiscovered multiplexer must report no shape"
        );
        assert_eq!(
            mux.enable(),
            Err(MsiMuxError::NotSupported),
            "an undiscovered multiplexer must not enable"
        );
        assert_eq!(
            mux.message(0),
            Err(MsiMuxError::NotSupported),
            "an undiscovered multiplexer must have no message"
        );
        assert_eq!(
            IrqController::mask(mux, 0),
            Err(IrqControlError::OutOfRange),
            "an undiscovered multiplexer must fail closed on a mask"
        );
        assert_eq!(
            IrqController::unmask(mux, 0),
            Err(IrqControlError::OutOfRange),
            "an undiscovered multiplexer must fail closed on an unmask"
        );
        assert_eq!(
            drain_once(mux).seen(),
            (0, &[][..]),
            "an undiscovered multiplexer must fire nothing"
        );
    }

    #[cfg(test)]
    mod tests {
        use super::super::{MsiMultiplexer, MsiMuxError, MsiMuxShape};
        use super::{run_discovered, run_undiscovered};
        use crate::irq::{IrqControlError, IrqController};
        use core::num::NonZeroU32;
        use core::sync::atomic::{AtomicU64, Ordering};
        use tairix_abi::MsiMessage;

        /// A bank of up to 64 vectors over two words, faithful unless told
        /// to admit every vector, never acknowledge, or repeat one message.
        struct Bank {
            shape: Option<MsiMuxShape>,
            pending: AtomicU64,
            masked: AtomicU64,
            promiscuous: bool,
            acknowledges: bool,
            distinct: bool,
        }

        impl Bank {
            fn discovered(vectors: u32) -> Self {
                Self {
                    shape: NonZeroU32::new(vectors).map(|vectors| MsiMuxShape {
                        vectors,
                        parent_line: 40,
                    }),
                    pending: AtomicU64::new(0),
                    masked: AtomicU64::new(0),
                    promiscuous: false,
                    acknowledges: true,
                    distinct: true,
                }
            }

            fn vectors(&self) -> u32 {
                self.shape.map_or(0, |shape| shape.vectors.get())
            }

            fn bit(&self, vector: u32) -> Result<u64, IrqControlError> {
                if !self.promiscuous && vector >= self.vectors() {
                    return Err(IrqControlError::OutOfRange);
                }
                Ok(1u64.checked_shl(vector).unwrap_or(0))
            }

            fn raise(&self, vector: u32) {
                self.pending.fetch_or(1 << vector, Ordering::SeqCst);
            }
        }

        impl IrqController for Bank {
            fn mask(&self, vector: u32) -> Result<(), IrqControlError> {
                self.masked.fetch_or(self.bit(vector)?, Ordering::SeqCst);
                Ok(())
            }
            fn unmask(&self, vector: u32) -> Result<(), IrqControlError> {
                self.masked.fetch_and(!self.bit(vector)?, Ordering::SeqCst);
                Ok(())
            }
        }

        impl MsiMultiplexer for Bank {
            fn shape(&self) -> Option<MsiMuxShape> {
                self.shape
            }
            fn enable(&self) -> Result<(), MsiMuxError> {
                if self.shape.is_some() {
                    Ok(())
                } else {
                    Err(MsiMuxError::NotSupported)
                }
            }
            fn message(&self, vector: u32) -> Result<MsiMessage, MsiMuxError> {
                if self.shape.is_none() {
                    return Err(MsiMuxError::NotSupported);
                }
                if vector >= self.vectors() {
                    return Err(MsiMuxError::OutOfRange);
                }
                Ok(MsiMessage {
                    address: 0xFFFF_FFFC,
                    data: if self.distinct { vector } else { 0 },
                })
            }
            fn drain(&self, fire: &mut dyn FnMut(u32)) {
                let pending = self.pending.load(Ordering::SeqCst);
                for vector in (0..64).filter(|v| pending & (1 << v) != 0) {
                    fire(vector);
                    if self.acknowledges {
                        self.pending.fetch_and(!(1 << vector), Ordering::SeqCst);
                    }
                }
            }
        }

        #[test]
        fn discovered_suite_accepts_a_faithful_bank() {
            for vectors in [1, 2, 32, 64] {
                let bank = Bank::discovered(vectors);
                run_discovered(&bank, &mut |vector| bank.raise(vector));
                let fresh = Bank::discovered(vectors);
                let dynamic: &dyn MsiMultiplexer = &fresh;
                run_discovered(dynamic, &mut |vector| fresh.raise(vector));
            }
        }

        #[test]
        #[should_panic(expected = "fail closed")]
        fn discovered_suite_rejects_a_promiscuous_bank() {
            let bank = Bank {
                promiscuous: true,
                ..Bank::discovered(8)
            };
            run_discovered(&bank, &mut |vector| bank.raise(vector));
        }

        #[test]
        #[should_panic(expected = "acknowledge")]
        fn discovered_suite_rejects_a_drain_that_never_acknowledges() {
            let bank = Bank {
                acknowledges: false,
                ..Bank::discovered(8)
            };
            run_discovered(&bank, &mut |vector| bank.raise(vector));
        }

        #[test]
        #[should_panic(expected = "distinct")]
        fn discovered_suite_rejects_repeated_messages() {
            let bank = Bank {
                distinct: false,
                ..Bank::discovered(8)
            };
            run_discovered(&bank, &mut |vector| bank.raise(vector));
        }

        #[test]
        fn undiscovered_suite_accepts_an_absent_bank() {
            run_undiscovered(&Bank::discovered(0));
        }

        #[test]
        #[should_panic(expected = "no shape")]
        fn undiscovered_suite_rejects_a_discovered_bank() {
            run_undiscovered(&Bank::discovered(8));
        }
    }
}
