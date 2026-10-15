//! Kernel wall-clock time with an honest provenance state
//! (`PREREQUISITES.md` P-D).
//!
//! The kernel keeps an absolute wall-clock time alongside the per-CPU
//! monotonic clock the scheduler and timeouts run on. The two are different
//! kinds of clock and serve different jobs:
//!
//! * The **monotonic** clock ([`crate::sched::SchedulerArch`]'s
//!   `monotonic_ns`) is the ordering authority — it never goes backwards and
//!   never jumps, so event ordering and timeouts depend only on it.
//! * The **wall** clock answers "what is the real-world date and time?". It
//!   is only as good as the source that set it, so every reading carries a
//!   [`WallTimeState`] saying how trustworthy it is: [`WallTimeState::Unset`]
//!   until something sets it, then [`WallTimeState::Firmware`] /
//!   [`WallTimeState::Trusted`] / [`WallTimeState::Adjusted`].
//!
//! # How the wall time is computed
//!
//! Setting the wall clock records the wall instant *and* the monotonic
//! reading at that moment. A later read projects the stored instant forward
//! by the monotonic time that has elapsed since, so the wall clock advances
//! at the monotonic clock's rate and never needs a periodic tick. This keeps
//! the wall time consistent with the ordering clock between sets.
//!
//! # Why a seam, not a global static
//!
//! Like every other mutable kernel subsystem, the wall clock is reached
//! through a borrowed seam ([`WallClockSource`]) with a fail-closed default
//! ([`NULL_WALL_CLOCK`]): there is no global mutable static. The boot path
//! keeps one [`KernelWallClock`] in the kernel's state and threads it to every
//! consumer; a trusted time source then drives it through the
//! `wall_time_set` syscall.

use tairix_abi::{Duration64, Errno, Time64, WallClockReading, WallTimeState};
use tairix_sync::{SeqLock, SpinLock};

/// The mutable wall-clock state: the provenance state plus the
/// (wall, monotonic) pair captured the last time it was set.
///
/// Whole words with no padding between them: a sequence lock copies its
/// payload byte for byte, and a padding byte has no value to copy.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(C)]
struct WallClockState {
    wall_secs: i64,
    monotonic_at_set_ns: u64,
    wall_nanos: u32,
    /// The [`WallTimeState`] discriminant.
    state: u32,
}

const _: () = assert!(
    size_of::<WallClockState>() == 2 * size_of::<u64>() + 2 * size_of::<u32>(),
    "the wall-clock state must hold no padding"
);

impl WallClockState {
    const UNSET: Self = Self {
        wall_secs: 0,
        monotonic_at_set_ns: 0,
        wall_nanos: 0,
        state: WallTimeState::Unset as u32,
    };

    /// The stored provenance; a discriminant no state has reads as unset.
    fn state(&self) -> WallTimeState {
        u8::try_from(self.state)
            .ok()
            .and_then(|raw| WallTimeState::from_u8(raw).ok())
            .unwrap_or(WallTimeState::Unset)
    }

    /// Project the stored wall instant forward by the monotonic time elapsed
    /// since it was set.
    ///
    /// `monotonic_now_ns` is read on the same monotonic clock that was
    /// captured at set time; the subtraction saturates at zero so a clock
    /// that has not advanced (or an out-of-order cross-CPU read) never yields
    /// a wall time *before* the set point.
    fn read(&self, monotonic_now_ns: u64) -> WallClockReading {
        let state = self.state();
        let Ok(wall_at_set) = Time64::new(self.wall_secs, self.wall_nanos) else {
            return WallClockReading::UNSET;
        };
        if !state.is_set() {
            return WallClockReading::UNSET;
        }
        let elapsed_ns = monotonic_now_ns.saturating_sub(self.monotonic_at_set_ns);
        let now = wall_at_set.saturating_add(Duration64::from_nanos(elapsed_ns));
        WallClockReading::new(now, state)
    }

    /// Record a new wall instant and provenance state captured at
    /// `monotonic_now_ns`.
    ///
    /// Rejects [`WallTimeState::Unset`] — "unset" is the *absence* of a set,
    /// never a value a caller may write (fail closed) — and enforces the
    /// provenance ladder ([`WallTimeState::supersedes`]): a `Firmware`
    /// source cannot overwrite a reading a network sync or a deliberate
    /// correction established. Either refusal leaves the stored reading and
    /// the monotonic clock untouched.
    fn set(
        &mut self,
        wall: Time64,
        monotonic_now_ns: u64,
        state: WallTimeState,
    ) -> Result<(), Errno> {
        if !state.is_set() {
            return Err(Errno::OutOfRange);
        }
        if !state.supersedes(self.state()) {
            return Err(Errno::AlreadyExists);
        }
        *self = Self {
            wall_secs: wall.secs(),
            monotonic_at_set_ns: monotonic_now_ns,
            wall_nanos: wall.subsec_nanos(),
            state: u32::from(state.as_u8()),
        };
        Ok(())
    }
}

/// The kernel wall clock's read/set seam.
///
/// Threaded into the syscall handlers as a `&'static dyn WallClockSource`
/// with the fail-closed [`NULL_WALL_CLOCK`] default, exactly like the other
/// kernel subsystem seams. The handler supplies `monotonic_now_ns` (read from
/// the arch monotonic clock on the issuing CPU) so this trait stays
/// independent of the architecture handle.
pub trait WallClockSource: Sync {
    /// Read the current wall time and its provenance state.
    fn read(&self, monotonic_now_ns: u64) -> WallClockReading;

    /// Set the wall time from a trusted source, recording the provenance
    /// `state`. Returns [`Errno::OutOfRange`] for a non-settable state
    /// ([`WallTimeState::Unset`]), [`Errno::AlreadyExists`] when the ladder
    /// refuses the write because the stored reading came from a better source
    /// ([`WallTimeState::supersedes`]), and [`Errno::NotImplemented`] from a
    /// clock that cannot be set (the fail-closed default).
    fn set(&self, wall: Time64, monotonic_now_ns: u64, state: WallTimeState) -> Result<(), Errno>;
}

/// The production wall clock: the provenance state plus the wall/monotonic
/// pair captured at the last set.
///
/// Every filesystem write on every CPU reads it to stamp what it changed, and
/// a time source sets it a few times a day, so the state is behind a sequence
/// lock, which a reader reads without taking a lock and retries only across a
/// write in flight; the one writer that lock admits at a time is serialised by
/// `setting`.
pub struct KernelWallClock {
    current: SeqLock<WallClockState>,
    setting: SpinLock<()>,
}

impl KernelWallClock {
    /// A fresh, unset wall clock: reads report the Unix epoch tagged
    /// [`WallTimeState::Unset`] until a trusted source sets it.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            current: SeqLock::new(WallClockState::UNSET),
            setting: SpinLock::new(()),
        }
    }
}

impl Default for KernelWallClock {
    fn default() -> Self {
        Self::new()
    }
}

impl WallClockSource for KernelWallClock {
    fn read(&self, monotonic_now_ns: u64) -> WallClockReading {
        self.current.read().read(monotonic_now_ns)
    }

    fn set(&self, wall: Time64, monotonic_now_ns: u64, state: WallTimeState) -> Result<(), Errno> {
        let _setting = self.setting.lock();
        let mut next = self.current.read();
        next.set(wall, monotonic_now_ns, state)?;
        // SAFETY: `setting` is held across the read and the write, so no
        // other write is in flight and none can interleave with this one.
        unsafe { self.current.write(next) };
        Ok(())
    }
}

/// Fail-closed wall clock installed until the boot path wires the real one.
///
/// Reads report [`WallClockReading::UNSET`] (the Unix epoch, tagged
/// [`WallTimeState::Unset`]) and a set fails closed with
/// [`Errno::NotImplemented`] — never silently pretending a time was
/// established.
pub struct NullWallClock;

impl WallClockSource for NullWallClock {
    fn read(&self, _monotonic_now_ns: u64) -> WallClockReading {
        WallClockReading::UNSET
    }

    fn set(
        &self,
        _wall: Time64,
        _monotonic_now_ns: u64,
        _state: WallTimeState,
    ) -> Result<(), Errno> {
        Err(Errno::NotImplemented)
    }
}

/// Shared fail-closed default ([`NullWallClock`]) the handler borrows until
/// the boot path installs a [`KernelWallClock`].
pub static NULL_WALL_CLOCK: NullWallClock = NullWallClock;

#[cfg(test)]
mod tests {
    use super::{KernelWallClock, NullWallClock, WallClockSource, NULL_WALL_CLOCK};
    use tairix_abi::{Errno, Time64, WallTimeState, NANOS_PER_SEC};

    fn secs(t: i64) -> Time64 {
        Time64::from_secs(t)
    }

    #[test]
    fn fresh_clock_reads_unset() {
        let clock = KernelWallClock::new();
        let r = clock.read(123_456);
        assert_eq!(r.state(), WallTimeState::Unset);
        assert_eq!(r.time(), Time64::UNIX_EPOCH);
    }

    #[test]
    fn set_then_read_projects_forward_by_monotonic_elapsed() {
        let clock = KernelWallClock::new();
        // Set wall = 1000s at monotonic 500ns.
        clock
            .set(secs(1_000), 500, WallTimeState::Trusted)
            .expect("settable state");
        // One full second of monotonic elapses.
        let r = clock.read(500 + u64::from(NANOS_PER_SEC));
        assert_eq!(r.state(), WallTimeState::Trusted);
        assert_eq!(r.time().secs(), 1_001);
        assert_eq!(r.time().subsec_nanos(), 0);
    }

    #[test]
    fn read_before_the_set_point_never_goes_backwards() {
        let clock = KernelWallClock::new();
        clock
            .set(secs(1_000), 1_000_000, WallTimeState::Firmware)
            .unwrap();
        // A monotonic reading below the captured one (e.g. an out-of-order
        // cross-CPU read) saturates to the set instant, never earlier.
        let r = clock.read(0);
        assert_eq!(r.time(), secs(1_000));
        assert_eq!(r.state(), WallTimeState::Firmware);
    }

    #[test]
    fn state_transitions_are_recorded() {
        let clock = KernelWallClock::new();
        clock.set(secs(10), 0, WallTimeState::Firmware).unwrap();
        assert_eq!(clock.read(0).state(), WallTimeState::Firmware);
        clock.set(secs(20), 0, WallTimeState::Trusted).unwrap();
        assert_eq!(clock.read(0).state(), WallTimeState::Trusted);
        clock.set(secs(21), 0, WallTimeState::Adjusted).unwrap();
        assert_eq!(clock.read(0).state(), WallTimeState::Adjusted);
    }

    #[test]
    fn a_firmware_source_cannot_overwrite_a_validated_reading() {
        // The regression this guards: an RTC driver that binds late (or a
        // hostile one) must not be able to roll the clock back over a
        // network sync, which would revive expired certificate lifetimes
        // and reorder audit reasoning.
        let clock = KernelWallClock::new();
        clock.set(secs(2_000), 0, WallTimeState::Trusted).unwrap();
        assert_eq!(
            clock.set(secs(1_000), 0, WallTimeState::Firmware),
            Err(Errno::AlreadyExists)
        );
        // Neither the instant nor the provenance moved.
        assert_eq!(clock.read(0).time(), secs(2_000));
        assert_eq!(clock.read(0).state(), WallTimeState::Trusted);

        // A deliberate manual correction is equally protected.
        let clock = KernelWallClock::new();
        clock.set(secs(2_000), 0, WallTimeState::Adjusted).unwrap();
        assert_eq!(
            clock.set(secs(1_000), 0, WallTimeState::Firmware),
            Err(Errno::AlreadyExists)
        );
        assert_eq!(clock.read(0).time(), secs(2_000));
        assert_eq!(clock.read(0).state(), WallTimeState::Adjusted);
    }

    #[test]
    fn the_ladder_still_admits_every_legitimate_write() {
        let clock = KernelWallClock::new();
        // Firmware establishes an unset clock and may refresh itself.
        clock.set(secs(10), 0, WallTimeState::Firmware).unwrap();
        clock.set(secs(11), 0, WallTimeState::Firmware).unwrap();
        assert_eq!(clock.read(0).time(), secs(11));
        // A network sync replaces it, and a later sync refreshes that.
        clock.set(secs(20), 0, WallTimeState::Trusted).unwrap();
        clock.set(secs(21), 0, WallTimeState::Trusted).unwrap();
        // A manual correction may still step a synced clock, and a later
        // sync may still correct the manual step.
        clock.set(secs(30), 0, WallTimeState::Adjusted).unwrap();
        clock.set(secs(31), 0, WallTimeState::Trusted).unwrap();
        assert_eq!(clock.read(0).time(), secs(31));
        assert_eq!(clock.read(0).state(), WallTimeState::Trusted);
    }

    #[test]
    fn setting_to_unset_is_rejected_fail_closed() {
        let clock = KernelWallClock::new();
        assert_eq!(
            clock.set(secs(10), 0, WallTimeState::Unset),
            Err(Errno::OutOfRange)
        );
        // The clock is untouched: still unset.
        assert_eq!(clock.read(0).state(), WallTimeState::Unset);
    }

    #[test]
    fn pre_1970_and_post_2038_round_trip_through_the_clock() {
        let clock = KernelWallClock::new();
        // 1901-ish, well before the epoch.
        clock
            .set(secs(-2_147_483_648), 0, WallTimeState::Trusted)
            .unwrap();
        assert_eq!(clock.read(0).time().secs(), -2_147_483_648);
        // Past the 32-bit ceiling.
        clock
            .set(secs(4_294_967_296), 0, WallTimeState::Trusted)
            .unwrap();
        assert_eq!(clock.read(0).time().secs(), 4_294_967_296);
    }

    #[test]
    fn a_read_racing_a_set_sees_one_whole_reading() {
        let clock = KernelWallClock::new();
        let first = Time64::new(1_000, 1).expect("canonical");
        let second = Time64::new(-2_000, 999_999_999).expect("canonical");
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for round in 0..200 {
                    let wall = if round % 2 == 0 { first } else { second };
                    clock.set(wall, 0, WallTimeState::Trusted).expect("sets");
                }
            });
            for _ in 0..200 {
                let reading = clock.read(0);
                assert!(
                    reading == tairix_abi::WallClockReading::UNSET
                        || (reading.state() == WallTimeState::Trusted
                            && (reading.time() == first || reading.time() == second)),
                    "a torn reading: {reading:?}"
                );
            }
        });
    }

    #[test]
    fn null_clock_fails_closed() {
        let null = NullWallClock;
        assert_eq!(null.read(999).state(), WallTimeState::Unset);
        assert_eq!(
            null.set(secs(1), 0, WallTimeState::Trusted),
            Err(Errno::NotImplemented)
        );
        // The shared static behaves identically.
        assert_eq!(NULL_WALL_CLOCK.read(0).state(), WallTimeState::Unset);
    }
}
