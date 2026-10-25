//! Microsecond timing seam shared by drivers that need timed waits.
//!
//! [`Delay`] is the one definition of "block for N microseconds, and read a
//! monotonic microsecond clock from the same source" that a driver consumes
//! when a bring-up step has a hardware-dictated settle time (a PCIe link
//! train, a USB hub power-on-good / reset-recovery window). It lives in
//! `lib/abi` so the driver-class crates that need it — a PCIe root-complex
//! bring-up and the bus-agnostic USB stack (`lib/usb`) — depend on one trait
//! rather than each declaring their own.
//!
//! It carries no authority of its own: it is a pure timing facility the host
//! supplies (on metal a generic-timer implementation reading the
//! architecture's monotonic counter; host tests a deterministic stand-in).
//!
//! [`CompletionWait`] adds the wait for a device's own signal, for a driver
//! the host lends the device's interrupt line.

use super::CompletionSignal;

/// A microsecond timing seam: a busy-delay plus a monotonic clock.
///
/// The host supplies the implementation. On metal it is backed by the
/// architecture's monotonic counter (e.g. `CNTPCT_EL0`/`CNTFRQ_EL0` on
/// aarch64); host tests supply a deterministic stand-in.
pub trait Delay {
    /// Block for at least `us` microseconds.
    fn delay_us(&self, us: u32);

    /// A monotonically non-decreasing microsecond timestamp from the same
    /// source [`delay_us`](Delay::delay_us) blocks against, so a caller can
    /// bound a poll loop by elapsed wall time rather than an iteration count
    /// (a single read that itself blocks cannot then inflate the loop). The
    /// epoch is unspecified; only differences are meaningful.
    fn now_us(&self) -> u64;
}

/// A device's completion wait, beside the timed waits its bring-up takes.
///
/// The host binds the device's interrupt line and parks the caller on it; a
/// driver cannot name that machinery, so the host lends it through this.
pub trait CompletionWait: Delay {
    /// Block until the device signals or the host's bounded budget elapses.
    ///
    /// [`CompletionSignal::TimedOut`] is a silent device, which the caller
    /// fails closed on; a spurious wake is a [`CompletionSignal::Fired`] the
    /// caller's re-read of the device's status answers.
    fn await_irq(&self) -> CompletionSignal;
}

impl<T: Delay + ?Sized> Delay for &T {
    fn delay_us(&self, us: u32) {
        (**self).delay_us(us);
    }

    fn now_us(&self) -> u64 {
        (**self).now_us()
    }
}

impl<T: CompletionWait + ?Sized> CompletionWait for &T {
    fn await_irq(&self) -> CompletionSignal {
        (**self).await_irq()
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::{CompletionSignal, CompletionWait, Delay};

    /// A wait that records what it was asked and answers from a fixed clock.
    struct Recorder {
        slept_us: Cell<u32>,
        waits: Cell<u32>,
    }

    impl Delay for Recorder {
        fn delay_us(&self, us: u32) {
            self.slept_us.set(self.slept_us.get() + us);
        }

        fn now_us(&self) -> u64 {
            42
        }
    }

    impl CompletionWait for Recorder {
        fn await_irq(&self) -> CompletionSignal {
            self.waits.set(self.waits.get() + 1);
            CompletionSignal::TimedOut
        }
    }

    /// A host lends one wait to a driver as a shared reference, so the
    /// reference must reach the same wait, unsized or not.
    #[test]
    fn a_borrowed_wait_is_the_wait_it_borrows() {
        let recorder = Recorder {
            slept_us: Cell::new(0),
            waits: Cell::new(0),
        };
        let lent: &dyn CompletionWait = &recorder;
        let borrowed = &lent;
        borrowed.delay_us(7);
        assert_eq!(borrowed.now_us(), 42);
        assert_eq!(borrowed.await_irq(), CompletionSignal::TimedOut);
        assert_eq!(recorder.slept_us.get(), 7);
        assert_eq!(recorder.waits.get(), 1);
    }
}
