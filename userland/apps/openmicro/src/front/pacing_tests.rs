use super::*;

const MS: u64 = 1_000_000;

const fn ring(queued: u32) -> Ring {
    Ring {
        queued,
        target: 2_400,
    }
}

#[test]
fn the_sound_keeps_the_ring_at_its_target() {
    let mut pacer = Pacer::new();
    assert_eq!(
        pacer.next(0, Some(ring(0)), false),
        Next::Run {
            cycles: 100_000,
            sound: true
        },
        "50 ms of sound is 100,000 cycles"
    );
    assert_eq!(
        pacer.next(0, Some(ring(1_200)), false),
        Next::Run {
            cycles: 50_000,
            sound: true
        }
    );
    assert_eq!(
        pacer.next(0, Some(ring(2_300)), false),
        Next::Wait,
        "too little to top up"
    );
    assert_eq!(pacer.next(0, Some(ring(2_400)), false), Next::Wait, "full");
}

#[test]
fn without_sound_a_slice_is_due_each_20_ms_of_the_clock() {
    let mut pacer = Pacer::new();
    let run = Next::Run {
        cycles: CLOCK_SLICE,
        sound: false,
    };
    assert_eq!(pacer.next(5 * MS, None, false), run);
    assert_eq!(pacer.next(6 * MS, None, false), Next::At(25 * MS));
    assert_eq!(pacer.next(25 * MS, None, false), run);
    assert_eq!(pacer.next(30 * MS, None, false), Next::At(45 * MS));
    assert_eq!(pacer.next(50 * MS, None, false), run, "a little late");
    assert_eq!(
        pacer.next(51 * MS, None, false),
        Next::At(65 * MS),
        "and back on its beat"
    );
    assert_eq!(pacer.next(500 * MS, None, false), run, "a long stall");
    assert_eq!(
        pacer.next(501 * MS, None, false),
        Next::At(520 * MS),
        "given up, not raced"
    );
}

#[test]
fn double_speed_runs_twice_as_much_by_the_clock() {
    let mut pacer = Pacer::new();
    pacer.set_speed(Speed::Double);
    assert!(
        matches!(
            pacer.next(0, Some(ring(0)), false),
            Next::Run { sound: false, .. }
        ),
        "silent"
    );
    assert_eq!(pacer.next(0, Some(ring(0)), false), Next::At(10 * MS));
}

#[test]
fn fastest_and_a_fast_tape_run_flat_out_and_pause_runs_nothing() {
    let fast = Next::Run {
        cycles: FAST_SLICE,
        sound: false,
    };
    let mut pacer = Pacer::new();
    assert_eq!(
        pacer.next(0, Some(ring(2_400)), true),
        fast,
        "a tape loading fast"
    );
    pacer.set_speed(Speed::Fastest);
    assert_eq!(pacer.speed(), Speed::Fastest);
    assert_eq!(pacer.next(0, None, false), fast);
    pacer.set_paused(true);
    assert!(pacer.paused());
    assert_eq!(pacer.next(0, None, true), Next::Wait);
}

#[test]
fn sound_of_any_length_maps_to_a_slice() {
    assert_eq!(cycles_for(48_000), 2_000_000, "a second");
    assert_eq!(cycles_for(1), 42, "a frame, rounded up");
    assert_eq!(cycles_for(u32::MAX), MAX_SLICE, "never past a slice");
}

/// The regression: the pace was a whole run's cycles in nanoseconds, which
/// overflowed after some hours, and every slice was then due at once.
#[test]
fn the_clock_keeps_its_pace_however_long_the_machine_runs() {
    let mut pacer = Pacer::new();
    pacer.set_speed(Speed::Double);
    let slice_ns = u64::from(CLOCK_SLICE) * NANOS / (2 * CLOCK);
    let past_overflow = u64::MAX / NANOS / u64::from(CLOCK_SLICE) + 1_000;
    let (mut now, mut run) = (0, 0);
    while run < past_overflow {
        match pacer.next(now, None, false) {
            Next::Run { .. } => {
                assert_eq!(now, run * slice_ns, "slice {run} on time");
                run += 1;
            }
            Next::At(at) => now = at,
            Next::Wait => panic!("the clock always has a slice due"),
        }
    }
}
