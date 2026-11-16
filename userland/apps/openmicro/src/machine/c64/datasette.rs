//! The datasette: the tape runs past the head while the C64's motor line is
//! on and PLAY is down, each pulse it hears a falling edge on CIA1's FLAG;
//! with RECORD down too, the C64's write line is recorded instead, a pulse
//! from each rising edge to the next. Its sense line reads low while PLAY
//! is down.

use alloc::vec::Vec;

use crate::media::tap::{Tape, MAX_PULSE, MAX_PULSES};
use crate::snapshot::{self, HORIZON};

/// The datasette.
#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is an independent key or line of the deck"
)]
pub struct Datasette {
    tape: Option<Tape>,
    play: bool,
    record: bool,
    motor: bool,
    /// The pulse under the head.
    pulse: usize,
    /// While the tape runs, the machine cycle the pulse under the head ends
    /// at; while it stands, the cycles left of it.
    edge_at: Option<u64>,
    left: u64,
    /// What scaling the tape's clock to the machine's carries over.
    fraction: u64,
    machine_clock: u64,
    write: bool,
    /// The machine cycle the write line last rose at, while recording.
    rose_at: Option<u64>,
    recorded: Vec<u32>,
    record_from: usize,
}

impl Datasette {
    /// An empty deck on a machine clocked at `machine_clock`.
    #[must_use]
    pub const fn new(machine_clock: u64) -> Self {
        Self {
            tape: None,
            play: false,
            record: false,
            motor: false,
            pulse: 0,
            edge_at: None,
            left: 0,
            fraction: 0,
            machine_clock,
            write: false,
            rose_at: None,
            recorded: Vec::new(),
            record_from: 0,
        }
    }

    /// The motor line, as last set.
    #[must_use]
    pub const fn motor(&self) -> bool {
        self.motor
    }

    /// The write line's level, as last set.
    #[must_use]
    pub const fn write_line(&self) -> bool {
        self.write
    }

    /// Whether the sense line is held low: PLAY is down.
    #[must_use]
    pub const fn sensed(&self) -> bool {
        self.play
    }

    /// Whether the tape moves.
    #[must_use]
    pub const fn running(&self) -> bool {
        self.motor && self.play && self.tape.is_some()
    }

    /// Whether RECORD is down.
    #[must_use]
    pub const fn record_down(&self) -> bool {
        self.record
    }

    /// Whether the tape records: it runs with RECORD down.
    #[must_use]
    pub const fn recording(&self) -> bool {
        self.running() && self.record
    }

    /// The machine cycle the next pulse's falling edge reaches FLAG, if
    /// the tape is playing one.
    #[must_use]
    pub const fn next_edge(&self) -> Option<u64> {
        if self.recording() {
            None
        } else {
            self.edge_at
        }
    }

    /// The tape, if one is in.
    #[must_use]
    pub const fn tape(&self) -> Option<&Tape> {
        self.tape.as_ref()
    }

    /// The pulse under the head.
    #[must_use]
    pub const fn position(&self) -> usize {
        self.pulse
    }

    /// Pulse `pulse`'s length in machine cycles, carrying the remainder.
    fn pulse_cycles(&mut self, pulse: usize) -> Option<u64> {
        let tape = self.tape.as_ref()?;
        let length = u64::from(*tape.pulses().get(pulse)?);
        let scaled = length * self.machine_clock + self.fraction;
        self.fraction = scaled % tape.clock();
        Some(scaled / tape.clock())
    }

    /// Bring the deck to machine cycle `now` before anything changes.
    fn settle(&mut self, now: u64, was_running: bool) {
        let running = self.running();
        if was_running && !running {
            if let Some(at) = self.edge_at.take() {
                self.left = at.saturating_sub(now);
            }
            self.commit_recording();
        } else if !was_running && running {
            if self.record {
                // Recording, the head plays nothing back.
                self.record_from = self.pulse;
                self.rose_at = None;
                self.edge_at = None;
                return;
            }
            let left = if self.left == 0 {
                self.pulse_cycles(self.pulse)
            } else {
                Some(self.left)
            };
            self.edge_at = left.map(|left| now + left);
            self.left = 0;
        }
    }

    /// Bring what has been recorded so far onto the tape, recording on.
    pub fn commit(&mut self) {
        if self.recording() {
            self.commit_recording();
            self.record_from = self.pulse;
        }
    }

    fn commit_recording(&mut self) {
        if self.recorded.is_empty() {
            return;
        }
        let recorded = core::mem::take(&mut self.recorded);
        if let Some(tape) = self.tape.as_mut() {
            tape.record(self.record_from, &recorded);
            self.pulse = (self.record_from + recorded.len()).min(tape.pulses().len());
        }
        self.left = 0;
    }

    /// The pulse under the head ended at its edge: move on to the next.
    pub fn take_edge(&mut self) {
        let Some(at) = self.edge_at else {
            return;
        };
        self.pulse += 1;
        self.edge_at = self
            .pulse_cycles(self.pulse)
            .map(|length| at + length.max(1));
    }

    /// The motor line from cycle `now`.
    pub fn set_motor(&mut self, now: u64, on: bool) {
        let was = self.running();
        self.motor = on;
        self.settle(now, was);
    }

    /// The write line at `level` from cycle `now`.
    pub fn set_write(&mut self, now: u64, level: bool) {
        let rose = level && !self.write;
        self.write = level;
        if !rose || !self.recording() {
            return;
        }
        if let (Some(previous), Some(tape)) = (self.rose_at, self.tape.as_ref()) {
            let cycles = u128::from(now - previous) * u128::from(tape.clock())
                / u128::from(self.machine_clock);
            // More than a tape holds is not kept.
            if self.recorded.len() < MAX_PULSES {
                let pulse = u32::try_from(cycles).unwrap_or(MAX_PULSE);
                self.recorded.push(pulse.clamp(1, MAX_PULSE));
            }
        }
        self.rose_at = Some(now);
    }

    /// Press PLAY, and RECORD with it if `record`, at cycle `now`; keys
    /// latched down already stay as they are until STOP lets them up.
    pub fn press_play(&mut self, now: u64, record: bool) {
        if self.play {
            return;
        }
        let was = self.running();
        self.play = true;
        self.record = record;
        self.settle(now, was);
    }

    /// Press STOP at cycle `now`.
    pub fn stop(&mut self, now: u64) {
        let was = self.running();
        self.play = false;
        self.record = false;
        self.settle(now, was);
    }

    /// Put `tape` in at its start, stopping the deck, handing back the one
    /// it replaces.
    pub fn insert(&mut self, now: u64, tape: Tape) -> Option<Tape> {
        self.stop(now);
        self.pulse = 0;
        self.left = 0;
        self.fraction = 0;
        self.tape.replace(tape)
    }

    /// Take the tape out, stopping the deck.
    pub fn eject(&mut self, now: u64) -> Option<Tape> {
        self.stop(now);
        self.tape.take()
    }

    /// Wind to pulse `pulse` with the deck stopped.
    pub fn wind(&mut self, now: u64, pulse: usize) {
        self.stop(now);
        self.pulse = pulse.min(self.pulses());
        self.left = 0;
    }

    /// Pulses on the tape; none without one.
    fn pulses(&self) -> usize {
        self.tape.as_ref().map_or(0, |tape| tape.pulses().len())
    }

    /// The deck restored from a snapshot, on a machine clocked at
    /// `machine_clock`.
    #[must_use]
    pub const fn clocked(mut self, machine_clock: u64) -> Self {
        self.machine_clock = machine_clock;
        self
    }
}

snapshot::fields!(
    Datasette {
        tape,
        play,
        record,
        motor,
        pulse,
        edge_at,
        left,
        fraction,
        write,
        rose_at,
        recorded <= MAX_PULSES,
        record_from
    }
    transient { machine_clock },
    check | d,
    r | d.pulse <= d.pulses()
        && d.record_from <= d.pulses()
        && d.edge_at.is_none_or(|at| {
            r.pending(at) && d.running() && !d.record && d.pulse < d.pulses()
        })
        && d.left <= HORIZON
        && d.tape.as_ref().is_none_or(|tape| d.fraction < tape.clock())
        && d.rose_at.is_none_or(|at| r.reached(at))
        && (d.recorded.is_empty() || d.recording())
        && d.recorded.iter().all(|pulse| (1..=MAX_PULSE).contains(pulse))
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::c64::Standard;
    use crate::media::tap;

    const CLOCK: u64 = Standard::Pal.clock();

    /// A version 1 tape of `units` for `standard`.
    fn tape_for(standard: Standard, units: &[u8]) -> Tape {
        let mut image = alloc::vec::Vec::from(&b"C64-TAPE-RAW"[..]);
        image.extend([1, 0, u8::from(standard == Standard::Ntsc), 0]);
        image.extend(u32::try_from(units.len()).expect("short").to_le_bytes());
        image.extend_from_slice(units);
        tap::decode(&image).expect("a TAP")
    }

    fn tape(units: &[u8]) -> Tape {
        tape_for(Standard::Pal, units)
    }

    #[test]
    fn the_tape_plays_only_with_the_motor_on_and_play_down() {
        let mut deck = Datasette::new(CLOCK);
        deck.insert(0, tape(&[10, 20]));
        assert_eq!(deck.next_edge(), None);
        deck.press_play(100, false);
        assert!(deck.sensed());
        assert_eq!(deck.next_edge(), None, "the motor is off");
        deck.set_motor(200, true);
        assert_eq!(deck.next_edge(), Some(280));
        deck.take_edge();
        assert_eq!(deck.next_edge(), Some(440));
        deck.set_motor(300, false);
        assert_eq!(deck.next_edge(), None);
        deck.set_motor(1000, true);
        assert_eq!(deck.next_edge(), Some(1140), "the rest of the pulse");
        deck.take_edge();
        assert_eq!(deck.next_edge(), None, "the tape's end");
    }

    #[test]
    fn recording_takes_a_pulse_from_each_rising_edge_to_the_next() {
        let mut deck = Datasette::new(CLOCK);
        deck.insert(0, tape(&[]));
        deck.press_play(0, true);
        deck.set_motor(10, true);
        for edge in [100u64, 400, 700, 1300] {
            deck.set_write(edge, true);
            deck.set_write(edge + 50, false);
        }
        deck.set_motor(2000, false);
        let tape = deck.tape().expect("a tape");
        assert_eq!(tape.pulses(), [300, 300, 600]);
        assert!(tape.is_dirty());
        assert_eq!(deck.position(), 3);
    }

    /// The regression: a silence longer than a TAP's longest pulse was
    /// recorded as a length its image cut to three bytes.
    #[test]
    fn a_silence_longer_than_the_longest_pulse_records_as_the_longest() {
        let mut deck = Datasette::new(CLOCK);
        deck.insert(0, tape(&[]));
        deck.press_play(0, true);
        deck.set_motor(0, true);
        deck.set_write(10, true);
        deck.set_write(11, false);
        deck.set_write(10 + 3 * u64::from(MAX_PULSE), true);
        deck.set_motor(4 * u64::from(MAX_PULSE), false);
        assert_eq!(deck.tape().expect("a tape").pulses(), [MAX_PULSE]);
    }

    /// The regression: recording kept the edge the tape would have played,
    /// which fell behind the machine's time, so a snapshot taken while
    /// recording would not restore; and PLAY pressed again let RECORD up.
    #[test]
    fn a_deck_recording_plays_nothing_back_and_its_keys_stay_latched() {
        let mut deck = Datasette::new(CLOCK);
        deck.insert(0, tape(&[10, 10, 10]));
        deck.press_play(0, true);
        deck.set_motor(0, true);
        assert_eq!(deck.next_edge(), None);
        deck.press_play(5, false);
        assert!(deck.recording(), "PLAY again leaves RECORD down");
        let mut w = snapshot::Writer::new();
        w.put(&deck);
        let body = snapshot::open(&w.finish().expect("small")).expect("opens");
        let mut r = snapshot::Reader::new(&body);
        r.set_clock(10_000).expect("a time");
        assert!(r.get::<Datasette>().is_ok(), "restores long after");
        deck.stop(20);
        deck.press_play(30, false);
        assert!(!deck.recording(), "STOP let both up");
        assert_eq!(deck.next_edge(), Some(110), "playing from where it stood");
    }

    #[test]
    fn a_recording_is_brought_onto_the_tape_as_it_goes() {
        let mut deck = Datasette::new(CLOCK);
        deck.insert(0, tape(&[]));
        deck.press_play(0, true);
        deck.set_motor(10, true);
        for edge in [100u64, 400, 700] {
            deck.set_write(edge, true);
            deck.set_write(edge + 50, false);
        }
        deck.commit();
        assert_eq!(deck.tape().expect("a tape").pulses(), [300, 300]);
        deck.set_write(1300, true);
        deck.set_motor(2000, false);
        assert_eq!(
            deck.tape().expect("a tape").pulses(),
            [300, 300, 600],
            "recorded on from where it was"
        );
    }

    #[test]
    fn a_deck_restores_from_a_snapshot_mid_tape() {
        let mut deck = Datasette::new(CLOCK);
        deck.insert(0, tape(&[10, 20, 30]));
        deck.press_play(0, false);
        deck.set_motor(0, true);
        deck.take_edge();
        let mut w = snapshot::Writer::new();
        w.put(&deck);
        let body = snapshot::open(&w.finish().expect("small")).expect("opens");
        let mut r = snapshot::Reader::new(&body);
        r.set_clock(100).expect("a time");
        let restored = r.get::<Datasette>().expect("restores").clocked(CLOCK);
        assert_eq!(restored, deck);
        let mut late = snapshot::Reader::new(&body);
        late.set_clock(10_000).expect("a time");
        assert!(
            late.get::<Datasette>().is_err(),
            "its next edge already past"
        );
    }

    #[test]
    fn a_snapshot_with_an_edge_no_running_deck_could_play_is_refused() {
        let mut deck = Datasette::new(CLOCK);
        deck.insert(0, tape(&[10, 20]));
        deck.press_play(0, false);
        deck.set_motor(0, true);
        let refused = |deck: &Datasette| {
            let mut w = snapshot::Writer::new();
            w.put(deck);
            let body = snapshot::open(&w.finish().expect("small")).expect("opens");
            let mut r = snapshot::Reader::new(&body);
            r.set_clock(1).expect("a time");
            r.get::<Datasette>().is_err()
        };
        assert!(!refused(&deck), "an edge the deck is playing");
        let mut past = deck.clone();
        past.pulse = past.pulses();
        assert!(refused(&past), "an edge past the tape's last pulse");
        let mut stopped = deck;
        stopped.motor = false;
        assert!(refused(&stopped), "an edge with the motor off");
    }

    #[test]
    fn a_tape_s_clock_is_scaled_to_the_machine_s() {
        let mut deck = Datasette::new(2 * CLOCK);
        deck.insert(0, tape(&[100]));
        deck.press_play(0, false);
        deck.set_motor(0, true);
        assert_eq!(deck.next_edge(), Some(1600));
    }

    #[test]
    fn a_tape_plays_unscaled_on_a_machine_of_its_own_standard() {
        let mut deck = Datasette::new(Standard::Ntsc.clock());
        // A million cycles, a version 1 escape.
        deck.insert(0, tape_for(Standard::Ntsc, &[0, 0x40, 0x42, 0x0F]));
        deck.press_play(0, false);
        deck.set_motor(0, true);
        assert_eq!(deck.next_edge(), Some(1_000_000));
    }
}
