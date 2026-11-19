use super::*;
use crate::machine::c64::Standard;
use crate::machine::CLOCK;

const MS: u64 = 1_000_000;
const ACORN: Timing = Timing::ACORN;

const fn ring(queued: u32) -> Ring {
    Ring {
        queued,
        target: 2_400,
    }
}

#[test]
fn the_sound_keeps_the_ring_at_its_target() {
    let mut pacer = Pacer::new(ACORN);
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
    let mut pacer = Pacer::new(ACORN);
    let run = Next::Run {
        cycles: 40_000,
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
    let mut pacer = Pacer::new(ACORN);
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
        cycles: 200_000,
        sound: false,
    };
    let mut pacer = Pacer::new(ACORN);
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
    assert_eq!(cycles_for(ACORN, 48_000), 2_000_000, "a second");
    assert_eq!(cycles_for(ACORN, 1), 42, "a frame, rounded up");
    assert_eq!(cycles_for(ACORN, u32::MAX), MAX_SLICE, "never past a slice");
}

#[test]
fn a_commodore_64_is_paced_in_its_own_cycles() {
    let (pal, ntsc) = (Standard::Pal.timing(), Standard::Ntsc.timing());
    assert_eq!(cycles_for(pal, 48_000), 985_248, "a PAL second");
    assert_eq!(cycles_for(ntsc, 48_000), 1_022_727, "an NTSC second");
    assert_eq!(cycles_for(pal, 1), 21, "a frame, rounded up");
    assert_eq!(cycles_for(ntsc, u32::MAX), 1_022_727, "never past a second");
    let mut pacer = Pacer::new(pal);
    assert_eq!(
        pacer.next(0, None, false),
        Next::Run {
            cycles: 19_704,
            sound: false
        },
        "20 ms of PAL cycles, rounded down"
    );
    assert_eq!(
        pacer.next(MS, None, false),
        Next::At(19_704 * NANOS / 985_248),
        "due when those cycles have passed"
    );
    let mut pacer = Pacer::new(ntsc);
    assert_eq!(pacer.timing(), ntsc);
    assert_eq!(
        pacer.next(MS, None, true),
        Next::Run {
            cycles: 102_272,
            sound: false
        },
        "100 ms of NTSC cycles"
    );
}

/// Every slice is due exactly when the machine time before it has passed,
/// however unevenly a second of it divides.
#[test]
fn a_clock_whose_second_does_not_divide_keeps_its_pace() {
    let timing = Standard::Pal.timing();
    let mut pacer = Pacer::new(timing);
    let (mut now, mut ran) = (0, 0u64);
    for _ in 0..10_000 {
        match pacer.next(now, None, false) {
            Next::Run { cycles, .. } => {
                assert_eq!(now, ran * NANOS / timing.clock(), "on time after {ran}");
                ran += u64::from(cycles);
            }
            Next::At(at) => now = at,
            Next::Wait => panic!("the clock always has a slice due"),
        }
    }
}

/// The regression: the pace was a whole run's cycles in nanoseconds, which
/// overflowed after some hours, and every slice was then due at once.
#[test]
fn the_clock_keeps_its_pace_however_long_the_machine_runs() {
    let mut pacer = Pacer::new(ACORN);
    pacer.set_speed(Speed::Double);
    let slice = u64::from(pacer.slice(CLOCK_SLICES));
    let slice_ns = slice * NANOS / (2 * CLOCK);
    let past_overflow = u64::MAX / NANOS / slice + 1_000;
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

/// Signed 16-bit samples as their bytes.
fn pcm(samples: &[i16]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|sample| sample.to_le_bytes())
        .collect()
}

#[test]
fn sound_the_ring_refuses_waits_and_goes_first_next_time() {
    let mut backlog = Backlog::default();
    let mut ring = Vec::new();
    let room = |ring: &mut Vec<u8>, room: usize, bytes: &[u8]| {
        let taken = bytes.len().min(room);
        ring.extend_from_slice(&bytes[..taken]);
        Ok(taken)
    };
    backlog
        .play(&pcm(&[1, 2, 3]), FULL_VOLUME, |bytes| {
            room(&mut ring, 4, bytes)
        })
        .expect("plays");
    assert_eq!(ring, pcm(&[1, 2]), "the ring took what fitted");
    assert_eq!(
        backlog.frames(),
        1,
        "the frame it refused waits, counted queued"
    );
    backlog
        .play(&pcm(&[4, 5]), FULL_VOLUME, |bytes| {
            room(&mut ring, 64, bytes)
        })
        .expect("plays");
    assert_eq!(ring, pcm(&[1, 2, 3, 4, 5]), "in order, nothing dropped");
    assert_eq!(backlog.frames(), 0);
}

#[test]
fn the_backlog_plays_at_the_volume_and_forgets_a_stopped_sound() {
    let mut backlog = Backlog::default();
    let mut heard = Vec::new();
    backlog
        .play(&pcm(&[1_000, -2_000]), FULL_VOLUME / 2, |bytes| {
            heard.extend_from_slice(bytes);
            Ok(0)
        })
        .expect("plays");
    assert_eq!(heard, pcm(&[500, -1_000]), "half the level");
    assert_eq!(backlog.frames(), 2);
    backlog.forget();
    assert_eq!(backlog.frames(), 0);
    assert_eq!(
        backlog.play(&pcm(&[7]), FULL_VOLUME, |_| Err(Errno::NotConnected)),
        Err(Errno::NotConnected),
        "the ring's refusal is the caller's"
    );
}

#[test]
fn a_load_is_hurried_only_after_a_second_without_sound() {
    let mut quiet = Quiet::default();
    assert!(quiet.settled(0, ACORN), "nothing heard yet");
    quiet.slice(CLOCK, true);
    assert!(!quiet.settled(CLOCK, ACORN), "the sound just moved");
    quiet.slice(CLOCK + CLOCK / 2, false);
    assert!(
        !quiet.settled(CLOCK + CLOCK / 2, ACORN),
        "a rest is still the music"
    );
    assert!(quiet.settled(2 * CLOCK, ACORN), "a second of quiet");
}
