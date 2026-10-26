//! The host a served volume's driver reports to.

use tairix_abi::driver::filesystem::VolumeHost;
use tairix_abi::driver::DriverHandle;
use tairix_abi::fsserve::ServerState;
use tairix_abi::time::Time64;
use tairix_sync::SpinLock;

/// The clocks a served driver stamps its operations and paces its commits
/// with.
pub trait Clock: Send + Sync {
    /// Monotonic nanoseconds, or `None` with no monotonic clock.
    fn now_ns(&self) -> Option<u64>;

    /// The wall clock's reading while it is set and plausible.
    fn wall_time(&self) -> Option<Time64>;
}

/// What the driver last reported.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
struct Reported {
    /// When its open transaction is due, until the commit timer takes it.
    deadline: Option<u64>,
    /// It holds a transaction it has not published.
    open: bool,
    in_reserve: bool,
}

/// The [`VolumeHost`] of a served volume: it keeps what the driver reports,
/// so the server can arm the commit timer and tell the kernel what it holds.
pub struct ServedHost<C> {
    clock: C,
    reported: SpinLock<Reported>,
}

impl<C: Clock> ServedHost<C> {
    /// A host reading `clock`, before the driver has reported anything.
    pub const fn new(clock: C) -> Self {
        Self {
            clock,
            reported: SpinLock::new(Reported {
                deadline: None,
                open: false,
                in_reserve: false,
            }),
        }
    }

    /// When the driver's open transaction is due to be published, if it
    /// holds one the commit timer has not yet taken.
    #[must_use]
    pub fn due(&self) -> Option<u64> {
        self.reported.lock().deadline
    }

    /// Take the deadline if it has arrived by `now_ns`, answering whether the
    /// transaction is to be published now.
    ///
    /// Taking it makes the firing single-shot: a publish that fails leaves
    /// the transaction open for the next operation to report again, and a
    /// deadline left in the past would fire for ever.
    pub fn take_due(&self, now_ns: u64) -> bool {
        let mut reported = self.reported.lock();
        match reported.deadline {
            Some(at) if at <= now_ns => {
                reported.deadline = None;
                true
            }
            _ => false,
        }
    }

    /// What the next reply tells the kernel.
    #[must_use]
    pub fn state(&self) -> ServerState {
        let reported = *self.reported.lock();
        ServerState {
            unpublished: reported.open,
            in_reserve: reported.in_reserve,
        }
    }
}

impl<C: Clock> VolumeHost for ServedHost<C> {
    fn now_ns(&self) -> Option<u64> {
        self.clock.now_ns()
    }

    fn wall_time(&self) -> Option<Time64> {
        self.clock.wall_time()
    }

    fn writeback_due(&self, _volume: DriverHandle, deadline_ns: Option<u64>) {
        let mut reported = self.reported.lock();
        reported.deadline = deadline_ns;
        reported.open = deadline_ns.is_some();
    }

    fn reserve_crossed(&self, _volume: DriverHandle, in_reserve: bool) {
        self.reported.lock().in_reserve = in_reserve;
    }
}

#[cfg(test)]
mod tests {
    use super::{Clock, ServedHost};
    use tairix_abi::driver::filesystem::VolumeHost;
    use tairix_abi::driver::DriverHandle;
    use tairix_abi::fsserve::ServerState;
    use tairix_abi::time::Time64;

    struct Fixed;

    impl Clock for Fixed {
        fn now_ns(&self) -> Option<u64> {
            Some(5)
        }

        fn wall_time(&self) -> Option<Time64> {
            None
        }
    }

    #[test]
    fn the_host_reports_what_the_driver_last_said() {
        let host = ServedHost::new(Fixed);
        let volume = DriverHandle::from_raw(1).unwrap();
        assert_eq!(host.state(), ServerState::default());
        assert_eq!(host.due(), None);
        host.writeback_due(volume, Some(40));
        host.reserve_crossed(volume, true);
        assert_eq!(host.due(), Some(40));
        assert_eq!(
            host.state(),
            ServerState {
                unpublished: true,
                in_reserve: true,
            }
        );
        host.writeback_due(volume, None);
        assert_eq!(host.due(), None);
        assert!(!host.state().unpublished);
        assert_eq!(host.now_ns(), Some(5));
    }

    #[test]
    fn a_deadline_fires_once_and_the_transaction_stays_open_until_published() {
        let host = ServedHost::new(Fixed);
        let volume = DriverHandle::from_raw(1).unwrap();
        host.writeback_due(volume, Some(40));
        assert!(!host.take_due(39), "not due yet");
        assert!(host.take_due(40));
        assert_eq!(host.due(), None, "taken, so the timer is not re-armed");
        assert!(!host.take_due(41));
        assert!(
            host.state().unpublished,
            "a publish that failed still holds its writes"
        );
        host.writeback_due(volume, None);
        assert!(!host.state().unpublished);
    }
}
