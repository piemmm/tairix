use super::*;

use tairix_reclaim::PressureBand;

/// A gauge parked at one band: the model's own receiving gauge, so the tests
/// exercise the same policy a mount does.
fn gauge(band: PressureBand) -> &'static tairix_reclaim::ReportedPressure {
    let gauge = alloc::boxed::Box::leak(alloc::boxed::Box::new(
        tairix_reclaim::ReportedPressure::unknown(),
    ));
    gauge.report(band);
    gauge
}

/// A budget wide enough that the band ladder, not the floor, decides the
/// first few bands.
const AMPLE_BACKING: usize = 64 * WRITEBACK_FLOOR_BYTES * 16;

fn bound(band: PressureBand) -> WritebackBound {
    WritebackBound::new(
        CacheBudget::from_backing(AMPLE_BACKING),
        gauge(band),
        Arc::new(PinnedAccounting::new()),
    )
    .expect("an ample machine bounds a volume")
}

#[test]
fn the_ceiling_falls_with_the_band_and_stops_at_the_floor() {
    let mut previous = usize::MAX;
    for band in PressureBand::ALL {
        let ceiling = bound(band).reading(0).ceiling;
        assert!(
            ceiling <= previous,
            "a deeper band must never raise the ceiling"
        );
        assert!(
            ceiling >= WRITEBACK_FLOOR_BYTES,
            "pressure lowers the ceiling to the floor and no further"
        );
        previous = ceiling;
    }
    assert_eq!(
        bound(PressureBand::Normal).reading(0).ceiling,
        CacheBudget::from_backing(AMPLE_BACKING).hard(),
        "an unpressured machine gives the volume its whole share"
    );
    assert!(
        bound(PressureBand::Mild).reading(0).ceiling
            < bound(PressureBand::Normal).reading(0).ceiling,
        "the first tightening must actually bite"
    );
}

#[test]
fn a_machine_too_small_for_one_transfer_is_refused_a_bound() {
    let short = CacheBudget::from_backing(WRITEBACK_FLOOR_BYTES * 16 - 16);
    assert!(short.hard() < WRITEBACK_FLOOR_BYTES);
    assert_eq!(
        WritebackBound::new(
            short,
            gauge(PressureBand::Normal),
            Arc::new(PinnedAccounting::new()),
        )
        .err(),
        Some(DriverError::NoSpace)
    );
    let exact = CacheBudget::from_backing(WRITEBACK_FLOOR_BYTES * 16);
    assert_eq!(exact.hard(), WRITEBACK_FLOOR_BYTES);
    assert!(
        WritebackBound::new(
            exact,
            gauge(PressureBand::Normal),
            Arc::new(PinnedAccounting::new()),
        )
        .is_ok(),
        "exactly one transfer is enough"
    );
}

#[test]
fn the_machine_wide_reserve_caps_the_ceiling_however_large_the_budget() {
    struct Starved;
    impl tairix_reclaim::FreeMemorySource for Starved {
        fn free_bytes(&self) -> usize {
            0
        }
        fn total_bytes(&self) -> usize {
            AMPLE_BACKING
        }
    }
    static STARVED: Starved = Starved;
    let measured: &'static tairix_reclaim::MemoryPressure = alloc::boxed::Box::leak(
        alloc::boxed::Box::new(tairix_reclaim::MemoryPressure::over(&STARVED)),
    );
    let bound = WritebackBound::new(
        CacheBudget::from_backing(AMPLE_BACKING),
        measured,
        Arc::new(PinnedAccounting::new()),
    )
    .expect("the budget itself is ample");
    assert_eq!(
        bound.reading(0).ceiling,
        WRITEBACK_FLOOR_BYTES,
        "no headroom above the reserve leaves only the forward-progress floor"
    );
}

#[test]
fn volumes_sharing_a_machine_divide_one_ceiling_rather_than_taking_one_each() {
    let share: &'static tairix_reclaim::PinnedShare =
        alloc::boxed::Box::leak(alloc::boxed::Box::new(tairix_reclaim::PinnedShare::new()));
    let bound = |slot: &Arc<PinnedAccounting>| {
        WritebackBound::new(
            CacheBudget::from_backing(AMPLE_BACKING),
            gauge(PressureBand::Normal),
            Arc::clone(slot),
        )
        .expect("the machine bounds a volume")
    };
    let whole = CacheBudget::from_backing(AMPLE_BACKING).hard();

    let alone = Arc::new(PinnedAccounting::within(share));
    let alone_bound = bound(&alone);
    assert_eq!(
        alone_bound.reading(0).ceiling,
        whole,
        "one volume drawing on the machine has the whole ceiling"
    );

    let slots: alloc::vec::Vec<Arc<PinnedAccounting>> = (0..4)
        .map(|_| Arc::new(PinnedAccounting::within(share)))
        .collect();
    let bounds: alloc::vec::Vec<WritebackBound> = slots.iter().map(bound).collect();
    for slot in &slots {
        slot.set(1, 1);
    }
    for held in &bounds {
        assert_eq!(held.reading(1).ceiling, whole / 4);
    }

    drop(bounds);
    assert_eq!(share.bytes(), 0, "a torn-down mount draws nothing");
    assert_eq!(alone_bound.reading(0).ceiling, whole);

    let pair: &'static tairix_reclaim::PinnedShare =
        alloc::boxed::Box::leak(alloc::boxed::Box::new(tairix_reclaim::PinnedShare::new()));
    let hog = Arc::new(PinnedAccounting::within(pair));
    let beside = Arc::new(PinnedAccounting::within(pair));
    let _hog_bound = bound(&hog);
    let beside_bound = bound(&beside);
    beside.set(1, 1);
    hog.set(1, 1);
    assert_eq!(
        beside_bound.reading(1).ceiling,
        whole / 2,
        "two volumes drawing, half the machine each"
    );
    hog.set(3 * whole / 4, 1);
    assert_eq!(beside_bound.reading(1).ceiling, whole / 4);

    hog.set(0, 0);
    assert_eq!(beside_bound.reading(1).ceiling, whole);
}
