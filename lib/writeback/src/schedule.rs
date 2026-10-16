//! When an open transaction must be published: the dirty-age window by
//! device class, and the scheduler that reports it to the host's timer.

use tairix_abi::blkio::BlkDeviceClass;
use tairix_abi::driver::filesystem::VolumeHost;
use tairix_abi::driver::DriverHandle;
use tairix_reclaim::PressureBand;

/// How long a transaction may stay open before it is published, by the
/// class of device the volume sits on and how tight memory is.
///
/// Operations inside the window fold into one commit, and a crash costs the
/// operations it folded, so the window is widest where a device command is
/// dearest. This is the one policy over the class; nothing tunes it per
/// volume.
#[must_use]
pub const fn writeback_window_ns(class: BlkDeviceClass, band: PressureBand) -> u64 {
    let base = match class {
        BlkDeviceClass::Removable => 30_000_000_000,
        BlkDeviceClass::Rotational => 15_000_000_000,
        BlkDeviceClass::SolidState | BlkDeviceClass::Virtual => 5_000_000_000,
    };
    // Pressure only ever buys recency back: each deeper band halves the
    // window, so a quiet volume returns its pinned bytes sooner.
    base >> band.depth()
}

/// When the open transaction must be published.
///
/// A transaction stays open and the next operation joins it. Between
/// operations nothing in the driver runs, so the window is enforced by the
/// host's write-back timer: the transaction reports its deadline as it opens
/// and its absence as it closes, and the host publishes a volume that falls
/// quiet by calling `flush`. A handle with no host, or a host with no clock,
/// publishes every operation: nothing would come back for a deferred one.
pub struct CommitScheduler {
    host: Option<(DriverHandle, &'static dyn VolumeHost)>,
    class: BlkDeviceClass,
    /// The band the last operation read; `Normal` until a gauge-carrying
    /// bound is installed, so an ungoverned handle keeps its class's window.
    band: PressureBand,
    /// Kept apart from `opened_ns`, which is absent when the host's clock
    /// declined to read.
    open: bool,
    opened_ns: Option<u64>,
    /// Operations already reported successful into the open transaction; a
    /// commit failure loses them, which makes it more than the caller's own.
    acknowledged: u32,
}

impl CommitScheduler {
    /// A scheduler for a volume on a device of `class`, with no host yet.
    #[must_use]
    pub const fn new(class: BlkDeviceClass) -> Self {
        Self {
            host: None,
            class,
            band: PressureBand::Normal,
            open: false,
            opened_ns: None,
            acknowledged: 0,
        }
    }

    const fn window_ns(&self) -> u64 {
        writeback_window_ns(self.class, self.band)
    }

    /// Note the memory-pressure band the running operation read.
    ///
    /// A deepening band can bring an open transaction's deadline forward, so
    /// the host is told the new instant; a steady band reports nothing.
    pub fn set_band(&mut self, band: PressureBand) {
        if self.band == band {
            return;
        }
        self.band = band;
        self.report();
    }

    /// The host and the handle this mount is registered under, once
    /// installed.
    #[must_use]
    pub fn host(&self) -> Option<(DriverHandle, &'static dyn VolumeHost)> {
        self.host
    }

    /// Install the host's write-back timer for the mount registered as
    /// `volume`, and report the current state to it.
    ///
    /// A transaction already open is dated from now: its age was never
    /// measured, and one window from here bounds it rather than leaving it
    /// unreported.
    pub fn set_host(&mut self, volume: DriverHandle, host: &'static dyn VolumeHost) {
        self.host = Some((volume, host));
        if self.open && self.opened_ns.is_none() {
            self.opened_ns = host.now_ns();
        }
        self.report();
    }

    /// Note that a transaction is now open, if one was not already.
    pub fn opened(&mut self) {
        if self.open {
            return;
        }
        self.open = true;
        self.opened_ns = self.now_ns();
        self.report();
    }

    /// Whether a transaction is in flight.
    #[must_use]
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Whether a transaction left open is published by the host's timer:
    /// not without a host, nor when its clock does not answer, nor when it
    /// opened before the clock answered and so has no deadline to fire.
    #[must_use]
    pub fn defers(&self) -> bool {
        self.opened_ns.is_some() && self.now_ns().is_some()
    }

    /// Note one more operation reported successful into the open
    /// transaction.
    pub const fn joined(&mut self) {
        self.acknowledged = self.acknowledged.saturating_add(1);
    }

    /// Operations reported successful whose work the open transaction still
    /// holds.
    #[must_use]
    pub const fn acknowledged(&self) -> u32 {
        self.acknowledged
    }

    /// The transaction has been published or abandoned.
    pub fn closed(&mut self) {
        self.open = false;
        self.opened_ns = None;
        self.acknowledged = 0;
        self.report();
    }

    /// Whether the open transaction has reached its window and the operation
    /// that just finished must publish it.
    #[must_use]
    pub fn expired(&self) -> bool {
        let Some(opened_ns) = self.opened_ns else {
            return true;
        };
        match self.now_ns() {
            Some(now) => now.saturating_sub(opened_ns) >= self.window_ns(),
            // The clock stopped answering mid-transaction: publish rather
            // than hold work against an age nothing can measure.
            None => true,
        }
    }

    fn deadline_ns(&self) -> Option<u64> {
        self.opened_ns.map(|at| at.saturating_add(self.window_ns()))
    }

    fn now_ns(&self) -> Option<u64> {
        self.host.and_then(|(_, host)| host.now_ns())
    }

    fn report(&self) {
        if let Some((volume, host)) = self.host {
            host.writeback_due(volume, self.deadline_ns());
        }
    }
}

/// The host, for tests: a settable monotonic clock, the last deadline the
/// driver reported, and the last reserve crossing it reported. It has no wall
/// clock; a test of what a volume stamps installs
/// `tairix_abi::driver::filesystem::clock::TestClock` instead.
#[cfg(any(test, feature = "test-util"))]
pub struct TestVolumeHost {
    now_ns: core::sync::atomic::AtomicU64,
    /// `u64::MAX` for "nothing open".
    due_ns: core::sync::atomic::AtomicU64,
    reports: core::sync::atomic::AtomicU32,
    /// `0` for no crossing reported, `1` out of the reserve, `2` into it.
    crossing: core::sync::atomic::AtomicU8,
}

#[cfg(any(test, feature = "test-util"))]
impl TestVolumeHost {
    /// A leaked host reading `now_ns`, as a driver needs it: `&'static`.
    #[must_use]
    pub fn leaked(now_ns: u64) -> &'static Self {
        alloc::boxed::Box::leak(alloc::boxed::Box::new(Self {
            now_ns: core::sync::atomic::AtomicU64::new(now_ns),
            due_ns: core::sync::atomic::AtomicU64::new(u64::MAX),
            reports: core::sync::atomic::AtomicU32::new(0),
            crossing: core::sync::atomic::AtomicU8::new(0),
        }))
    }

    /// Whether the driver last reported the volume in its reserve, or `None`
    /// if it reported no crossing.
    #[must_use]
    pub fn in_reserve(&self) -> Option<bool> {
        match self.crossing.load(core::sync::atomic::Ordering::Acquire) {
            0 => None,
            reported => Some(reported == 2),
        }
    }

    /// Move the clock to `now_ns`.
    pub fn set_now(&self, now_ns: u64) {
        self.now_ns
            .store(now_ns, core::sync::atomic::Ordering::Release);
    }

    /// The deadline the driver last reported, or `None` for "nothing open".
    #[must_use]
    pub fn reported(&self) -> Option<u64> {
        match self.due_ns.load(core::sync::atomic::Ordering::Acquire) {
            u64::MAX => None,
            deadline => Some(deadline),
        }
    }

    /// How many times the driver has reported.
    #[must_use]
    pub fn reports(&self) -> u32 {
        self.reports.load(core::sync::atomic::Ordering::Acquire)
    }

    /// The handle a test registers its one volume under.
    #[must_use]
    pub fn volume() -> DriverHandle {
        DriverHandle::from_raw(1).expect("a non-zero test handle")
    }
}

#[cfg(any(test, feature = "test-util"))]
impl VolumeHost for TestVolumeHost {
    fn now_ns(&self) -> Option<u64> {
        Some(self.now_ns.load(core::sync::atomic::Ordering::Acquire))
    }

    fn wall_time(&self) -> Option<tairix_abi::time::Time64> {
        None
    }

    fn writeback_due(&self, _volume: DriverHandle, deadline_ns: Option<u64>) {
        self.due_ns.store(
            deadline_ns.unwrap_or(u64::MAX),
            core::sync::atomic::Ordering::Release,
        );
        self.reports
            .fetch_add(1, core::sync::atomic::Ordering::AcqRel);
    }

    fn reserve_crossed(&self, _volume: DriverHandle, in_reserve: bool) {
        self.crossing.store(
            1 + u8::from(in_reserve),
            core::sync::atomic::Ordering::Release,
        );
    }
}

#[cfg(test)]
#[path = "schedule_tests.rs"]
mod tests;
