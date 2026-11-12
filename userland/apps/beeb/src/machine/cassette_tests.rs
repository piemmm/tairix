use alloc::vec::Vec;

use super::{Cassette, Event};
use crate::machine::acia::Format;
use crate::machine::CLOCK;
use crate::media::tape::{Tape, Tone};
use crate::snapshot::{self, Reader, Writer};

const EIGHT_N_ONE: Format = Format {
    data_bits: 8,
    parity: None,
    stop_bits: 1,
};

/// A tape: a second of carrier, then `bytes` framed 8N1 at 1200 baud, then
/// silence.
fn tape_of(bytes: &[u8]) -> Tape {
    let mut tape = Tape::default();
    tape.push(Tone::High, CLOCK).unwrap();
    let bit = CLOCK / 1200;
    for &byte in bytes {
        tape.push(Tone::Low, bit).unwrap();
        for k in 0..8 {
            tape.push(
                if byte >> k & 1 != 0 {
                    Tone::High
                } else {
                    Tone::Low
                },
                bit,
            )
            .unwrap();
        }
        tape.push(Tone::High, bit).unwrap();
    }
    tape.push(Tone::Silence, CLOCK).unwrap();
    tape
}

fn events(deck: &mut Cassette, until: u64, rate: u64) -> Vec<Event> {
    let mut seen = Vec::new();
    deck.advance(until, rate, EIGHT_N_ONE, true, |event| seen.push(event));
    seen
}

fn frames(events: &[Event]) -> Vec<(u8, bool)> {
    events
        .iter()
        .filter_map(|event| match *event {
            Event::Frame {
                byte,
                framing_error,
                ..
            } => Some((byte, framing_error)),
            Event::Carrier { .. } => None,
        })
        .collect()
}

#[test]
fn the_tape_moves_only_while_the_motor_runs() {
    let mut deck = Cassette::new();
    deck.insert(0, tape_of(b"A"));
    assert_eq!(deck.position(1_000), 0);
    deck.set_motor(1_000, true);
    assert_eq!(deck.position(5_000), 4_000);
    deck.set_motor(5_000, false);
    assert_eq!(deck.position(90_000), 4_000);
    deck.wind(90_000, 0);
    assert_eq!(deck.position(90_000), 0);
}

#[test]
fn the_6850_reads_framed_bytes_at_the_tape_s_rate() {
    let mut deck = Cassette::new();
    deck.insert(0, tape_of(b"BBC"));
    deck.set_motor(0, true);
    let seen = events(&mut deck, 2 * CLOCK, 1200);
    assert_eq!(frames(&seen), [(b'B', false), (b'B', false), (b'C', false)]);
    // The first stop bit is sampled at its middle: 9½ bits after the edge.
    let Some(Event::Frame { at, .. }) = seen
        .iter()
        .find(|event| matches!(event, Event::Frame { .. }))
    else {
        panic!("a frame");
    };
    assert_eq!(*at, CLOCK + 19 * CLOCK / 2400);
}

#[test]
fn read_at_the_wrong_rate_the_bytes_are_rubbish() {
    let mut deck = Cassette::new();
    deck.insert(0, tape_of(b"BBC"));
    deck.set_motor(0, true);
    let read = frames(&events(&mut deck, 2 * CLOCK, 300));
    assert_ne!(read, [(b'B', false), (b'B', false), (b'C', false)]);
}

#[test]
fn the_carrier_is_found_after_its_run_in_and_lost_at_silence() {
    let mut deck = Cassette::new();
    deck.insert(0, tape_of(b"X"));
    deck.set_motor(0, true);
    let carrier: Vec<(u64, bool)> = events(&mut deck, 3 * CLOCK, 1200)
        .iter()
        .filter_map(|event| match *event {
            Event::Carrier { at, present } => Some((at, present)),
            Event::Frame { .. } => None,
        })
        .collect();
    // The test tape's bits are whole cycles: 1666 each.
    let end_of_byte = CLOCK + 10 * (CLOCK / 1200);
    assert_eq!(carrier, [(8 * CLOCK / 2400, true), (end_of_byte, false)]);
    assert!(!deck.carrier());
}

#[test]
fn stopping_the_motor_loses_the_carrier_at_once() {
    let mut deck = Cassette::new();
    deck.insert(0, tape_of(b"X"));
    deck.set_motor(0, true);
    events(&mut deck, CLOCK / 2, 1200);
    assert!(deck.carrier());
    deck.set_motor(CLOCK / 2, false);
    let seen = events(&mut deck, CLOCK, 1200);
    assert_eq!(
        seen,
        [Event::Carrier {
            at: CLOCK / 2,
            present: false
        }]
    );
}

#[test]
fn a_recording_takes_the_carrier_the_frames_and_silence_and_plays_back() {
    let mut deck = Cassette::new();
    deck.insert(0, Tape::default());
    deck.set_record(0, true);
    deck.set_rts(0, true);
    deck.set_motor(0, true);
    let frame = 10 * CLOCK / 1200;
    let first = CLOCK + frame;
    deck.transmitted(first, b'O', EIGHT_N_ONE, frame);
    deck.transmitted(first + frame, b'K', EIGHT_N_ONE, frame);
    deck.set_rts(first + 2 * frame + 1_000, false);
    deck.set_motor(first + 2 * frame + 5_000, false);
    deck.set_record(first + 2 * frame + 5_000, false);
    let tape = deck.eject(first + 2 * frame + 5_000).unwrap();
    assert_eq!(
        tape.tone_at(CLOCK / 2),
        Tone::High,
        "carrier while RTS is asserted"
    );
    assert_eq!(tape.tone_at(first + 2 * frame + 2_000), Tone::Silence);
    assert_eq!(tape.len(), first + 2 * frame + 5_000);

    let mut player = Cassette::new();
    player.insert(0, tape);
    player.set_motor(0, true);
    assert_eq!(
        frames(&events(&mut player, 2 * CLOCK, 1200)),
        [(b'O', false), (b'K', false)]
    );
}

#[test]
fn a_frame_learned_late_replaces_the_carrier_laid_over_its_start() {
    let mut deck = Cassette::new();
    deck.insert(0, Tape::default());
    deck.set_record(0, true);
    deck.set_rts(0, true);
    deck.set_motor(0, true);
    let frame = 10 * CLOCK / 1200;
    // RTS is set again part way through the frame, before the 6850 reports
    // the frame finished.
    deck.set_rts(CLOCK + frame / 2, true);
    deck.transmitted(CLOCK + frame, b'Z', EIGHT_N_ONE, frame);
    // RTS still asserted, the carrier runs on to the eject.
    let tape = deck.eject(CLOCK + 2 * frame).unwrap();
    assert_eq!(tape.len(), CLOCK + 2 * frame);
    let mut player = Cassette::new();
    player.insert(0, tape);
    player.set_motor(0, true);
    assert_eq!(
        frames(&events(&mut player, 2 * CLOCK, 1200)),
        [(b'Z', false)]
    );
}

#[test]
fn recording_over_the_middle_of_a_tape_keeps_what_came_before() {
    let mut deck = Cassette::new();
    deck.insert(0, tape_of(b"KEEP"));
    deck.set_motor(0, true);
    deck.set_motor(CLOCK / 2, false);
    deck.set_record(CLOCK / 2, true);
    deck.set_rts(CLOCK / 2, false);
    deck.set_motor(CLOCK / 2, true);
    let tape = deck.eject(CLOCK).unwrap();
    assert_eq!(tape.tone_at(CLOCK / 4), Tone::High);
    assert_eq!(tape.tone_at(3 * CLOCK / 4), Tone::Silence);
    assert_eq!(tape.len(), CLOCK);
}

#[test]
fn nothing_reaches_the_6850_while_it_listens_to_rs423() {
    let mut deck = Cassette::new();
    deck.insert(0, tape_of(b"QQ"));
    deck.set_motor(0, true);
    let mut seen = Vec::new();
    deck.advance(2 * CLOCK, 1200, EIGHT_N_ONE, false, |event| {
        seen.push(event);
    });
    assert!(frames(&seen).is_empty());
}

/// The deck through a snapshot and back, as a machine restored at `now`
/// holds it.
fn round_trip(deck: &Cassette, now: u64) -> Cassette {
    let mut w = Writer::new();
    w.put(deck);
    let body = snapshot::open(&w.finish().unwrap()).unwrap();
    let mut r = Reader::new(&body);
    r.set_clock(now).unwrap();
    let restored: Cassette = r
        .get()
        .unwrap_or_else(|error| panic!("refused at {now}: {error:?}"));
    assert_eq!(&restored, deck);
    restored
}

#[test]
fn a_tape_plays_and_records_the_same_through_a_snapshot_at_every_step() {
    let play = |snapshots: bool| {
        let mut deck = Cassette::new();
        deck.insert(0, tape_of(b"SNAP"));
        deck.set_motor(0, true);
        let mut seen = Vec::new();
        for now in (0..3 * CLOCK).step_by(397) {
            deck.advance(now, 1200, EIGHT_N_ONE, true, |event| seen.push(event));
            // A machine brings its devices up to its time before a snapshot.
            if snapshots {
                deck = round_trip(&deck, now);
            }
        }
        seen
    };
    let played = play(false);
    assert_eq!(frames(&played).len(), 4);
    assert_eq!(play(true), played);

    let record = |snapshots: bool| {
        type Step<'a> = &'a dyn Fn(&mut Cassette, u64);
        let frame = 10 * CLOCK / 1200;
        let first = CLOCK + frame;
        let steps: [Step<'_>; 6] = [
            &|deck, at| deck.set_record(at, true),
            &|deck, at| deck.set_rts(at, true),
            &|deck, at| deck.set_motor(at, true),
            &|deck, _| deck.transmitted(first, b'O', EIGHT_N_ONE, frame),
            &|deck, at| deck.set_rts(at, false),
            &|deck, at| deck.set_motor(at, false),
        ];
        let mut deck = Cassette::new();
        deck.insert(0, tape_of(b"OLD"));
        for (step, at) in steps
            .iter()
            .zip([0, 0, 0, first, first + 1_000, first + 9_000])
        {
            step(&mut deck, at);
            if snapshots {
                deck = round_trip(&deck, at);
            }
        }
        deck.eject(first + 9_000)
    };
    assert_eq!(record(true), record(false));
}

/// The regression: the carrier detector's next change was bounded only from
/// above, so a restored deck could hold one already past.
#[test]
fn a_restored_deck_with_its_carrier_change_past_is_refused() {
    let mut deck = Cassette::new();
    deck.insert(0, tape_of(b"SNAP"));
    deck.set_motor(0, true);
    deck.advance(100, 1200, EIGHT_N_ONE, true, |_| {});
    let at = deck
        .carrier_change
        .expect("the detector waits for its run-in");
    let mut w = Writer::new();
    w.put(&deck);
    let body = snapshot::open(&w.finish().unwrap()).unwrap();
    let restore = |clock| {
        let mut r = Reader::new(&body);
        r.set_clock(clock).unwrap();
        r.get::<Cassette>()
    };
    assert!(restore(100).is_ok());
    assert!(restore(at + 1).is_err());
}

/// The regression: every toggle of the motor relay searched the tape afresh
/// for the next start bit and run-in, to its end on a tape with neither, so a
/// program toggling it in a loop cost a walk of the tape each time.
#[test]
fn a_relay_toggled_in_a_loop_searches_the_tape_once() {
    let mut tape = Tape::default();
    for _ in 0..10_000 {
        tape.push(Tone::High, 40).unwrap();
        tape.push(Tone::Silence, 40).unwrap();
    }
    let mut deck = Cassette::new();
    deck.insert(0, tape);
    deck.set_motor(0, true);
    let first = deck.found;
    let (from, edge) = first.edge.expect("the receiver looked for a start bit");
    assert_eq!(edge, None, "the tape holds none");
    for toggle in 1..1000 {
        deck.set_motor(8 * toggle, toggle % 2 == 0);
    }
    assert_eq!(
        deck.found.edge.map(|(searched, _)| searched),
        Some(from),
        "never searched again"
    );
    assert!(deck
        .found
        .run
        .is_some_and(|(searched, _)| searched <= 8 * 1000));
}
