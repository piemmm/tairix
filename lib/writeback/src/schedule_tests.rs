use super::*;

#[test]
fn each_command_cost_is_served_its_own_window() {
    let calm = PressureBand::Normal;
    assert_eq!(
        writeback_window_ns(BlkCommandCost::Bridged, calm),
        30_000_000_000
    );
    assert_eq!(
        writeback_window_ns(BlkCommandCost::Seek, calm),
        15_000_000_000
    );
    assert_eq!(
        writeback_window_ns(BlkCommandCost::Cheap, calm),
        5_000_000_000
    );
}

#[test]
fn a_usb_unit_keeps_the_bridged_window_whatever_is_known_of_its_medium() {
    use tairix_abi::blkio::{BlkMedium, BlkRotation, BlkRotationEvidence, BlkTransport};

    let calm = PressureBand::Normal;
    let usb = BlkMedium::of_transport(BlkTransport::Usb);
    let bridged = writeback_window_ns(BlkCommandCost::Bridged, calm);
    for rotation in [
        BlkRotation::Unknown,
        BlkRotation::NonRotating(BlkRotationEvidence::RotationRateVpd),
        BlkRotation::from_rate_field(7200, BlkRotationEvidence::AtaIdentify),
    ] {
        let medium = usb.with_rotation(rotation);
        assert_eq!(
            writeback_window_ns(BlkCommandCost::of(Some(medium)), calm),
            bridged,
            "{rotation:?}"
        );
    }
    let unknown_disk = BlkMedium::of_transport(BlkTransport::Ata);
    assert_eq!(
        writeback_window_ns(BlkCommandCost::of(Some(unknown_disk)), calm),
        writeback_window_ns(BlkCommandCost::Seek, calm),
        "a disk not known not to rotate is batched as one that seeks"
    );
    assert_eq!(
        writeback_window_ns(BlkCommandCost::of(None), calm),
        writeback_window_ns(BlkCommandCost::Cheap, calm),
        "an undeclared device holds the least unpublished work"
    );
}

#[test]
fn tightening_memory_only_ever_shortens_the_window() {
    let mut previous = u64::MAX;
    for band in PressureBand::ALL {
        let window = writeback_window_ns(BlkCommandCost::Bridged, band);
        assert!(
            window < previous,
            "each deeper band must publish sooner than the one above it"
        );
        assert!(window > 0, "a window must never vanish into a busy commit");
        previous = window;
    }
}

#[test]
fn a_deepening_band_brings_an_open_transactions_deadline_forward() {
    let host = TestVolumeHost::leaked(0);
    let mut schedule = CommitScheduler::new(BlkCommandCost::Bridged);
    schedule.set_host(TestVolumeHost::volume(), host);
    schedule.opened();
    let calm = host.reported().expect("an open transaction is due");
    schedule.set_band(PressureBand::Moderate);
    let tightened = host.reported().expect("still open, and due sooner");
    assert!(tightened < calm);
    // A steady band must not cost a timer arm per operation.
    let reports = host.reports();
    schedule.set_band(PressureBand::Moderate);
    assert_eq!(host.reports(), reports);
}

#[test]
fn a_scheduler_with_no_host_publishes_every_operation() {
    let mut schedule = CommitScheduler::new(BlkCommandCost::Bridged);
    schedule.opened();
    assert!(schedule.expired());
}

#[test]
fn a_host_that_reads_no_clock_publishes_every_operation() {
    struct Clockless;
    impl VolumeHost for Clockless {
        fn now_ns(&self) -> Option<u64> {
            None
        }
        fn wall_time(&self) -> Option<tairix_abi::time::Time64> {
            None
        }
        fn writeback_due(&self, _volume: DriverHandle, _deadline_ns: Option<u64>) {}
        fn reserve_crossed(&self, _volume: DriverHandle, _in_reserve: bool) {}
    }
    static CLOCKLESS: Clockless = Clockless;

    let mut schedule = CommitScheduler::new(BlkCommandCost::Bridged);
    schedule.set_host(TestVolumeHost::volume(), &CLOCKLESS);
    schedule.opened();
    assert!(schedule.expired());
}

/// A transaction opened before the host's clock answered was never dated, so
/// no timer will publish it: the operation that opened it must.
#[test]
fn a_transaction_opened_before_the_clock_answered_is_not_deferred() {
    struct Arming(core::sync::atomic::AtomicBool);
    impl VolumeHost for Arming {
        fn now_ns(&self) -> Option<u64> {
            self.0
                .load(core::sync::atomic::Ordering::Acquire)
                .then_some(5)
        }
        fn wall_time(&self) -> Option<tairix_abi::time::Time64> {
            None
        }
        fn writeback_due(&self, _volume: DriverHandle, _deadline_ns: Option<u64>) {}
        fn reserve_crossed(&self, _volume: DriverHandle, _in_reserve: bool) {}
    }
    let host: &'static Arming = alloc::boxed::Box::leak(alloc::boxed::Box::new(Arming(
        core::sync::atomic::AtomicBool::new(false),
    )));

    let mut schedule = CommitScheduler::new(BlkCommandCost::Bridged);
    schedule.set_host(TestVolumeHost::volume(), host);
    schedule.opened();
    host.0.store(true, core::sync::atomic::Ordering::Release);
    assert!(
        !schedule.defers(),
        "an undated transaction has no deadline for a timer to fire"
    );
}

#[test]
fn a_transaction_expires_when_its_window_elapses() {
    let window = writeback_window_ns(BlkCommandCost::Cheap, PressureBand::Normal);
    let host = TestVolumeHost::leaked(0);
    let mut schedule = CommitScheduler::new(BlkCommandCost::Cheap);
    schedule.set_host(TestVolumeHost::volume(), host);
    schedule.opened();
    assert!(schedule.is_open());
    assert!(!schedule.expired());
    host.set_now(window - 1);
    assert!(!schedule.expired());
    host.set_now(window);
    assert!(schedule.expired());
    schedule.closed();
    assert!(!schedule.is_open());
}

#[test]
fn an_opening_transaction_reports_its_deadline_and_a_closing_one_reports_none() {
    let window = writeback_window_ns(BlkCommandCost::Seek, PressureBand::Normal);
    let host = TestVolumeHost::leaked(700);
    let mut schedule = CommitScheduler::new(BlkCommandCost::Seek);
    schedule.set_host(TestVolumeHost::volume(), host);
    assert_eq!(host.reported(), None);
    schedule.opened();
    assert_eq!(host.reported(), Some(700 + window));
    let reports = host.reports();
    schedule.opened();
    assert_eq!(
        host.reports(),
        reports,
        "an operation joining the open transaction moves no deadline"
    );
    schedule.closed();
    assert_eq!(host.reported(), None);
}

#[test]
fn a_host_installed_over_an_open_transaction_dates_it_from_now() {
    let window = writeback_window_ns(BlkCommandCost::Cheap, PressureBand::Normal);
    let mut schedule = CommitScheduler::new(BlkCommandCost::Cheap);
    schedule.opened();
    assert!(schedule.expired());
    let host = TestVolumeHost::leaked(5_000);
    schedule.set_host(TestVolumeHost::volume(), host);
    assert_eq!(host.reported(), Some(5_000 + window));
    assert!(!schedule.expired());
}

#[test]
fn a_closed_transaction_forgets_the_operations_it_carried() {
    let host = TestVolumeHost::leaked(0);
    let mut schedule = CommitScheduler::new(BlkCommandCost::Cheap);
    schedule.set_host(TestVolumeHost::volume(), host);
    schedule.opened();
    schedule.joined();
    schedule.joined();
    assert_eq!(schedule.acknowledged(), 2);
    schedule.closed();
    assert_eq!(schedule.acknowledged(), 0);
}
