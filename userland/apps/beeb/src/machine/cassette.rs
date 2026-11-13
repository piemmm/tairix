//! The cassette: a tape moving under the motor relay, the serial ULA's data
//! separator and carrier detector between it and the 6850, and the recording
//! the 6850's output makes.
//!
//! The data separator gives the 6850's receive line the tone as it is heard,
//! low for the 1200 Hz tone and high for the 2400 Hz tone or silence, and the
//! 6850 samples it at its own rate: it takes a falling edge after the line
//! has been high as a start bit, confirms it half a bit later, and reads each
//! bit at its middle, so a tape read at the wrong rate reads as rubbish, as
//! it does on the machine. The carrier detector reports carrier once a tone
//! has played unbroken for its run-in, and loses it the moment the tape falls
//! silent, ends or stops.
//!
//! Recording, the tape takes what the ULA makes of the 6850's output: the
//! 2400 Hz carrier while RTS is asserted, each transmitted frame's bits as
//! tones, and silence while it is not. A recording ends at the tape's end.

use crate::media::tape::{self, Tape, Tone};

use super::acia::Format;
use super::CLOCK;

/// How long the carrier detector needs a tone before it reports carrier:
/// eight cycles of the high tone.
const RUN_IN: u64 = 8 * CLOCK / tape::HIGH_TONE_HZ;

/// What the cassette tells the 6850.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A frame arrived on the receive line, its first stop bit sampled at
    /// `at`.
    Frame {
        /// The cycle.
        at: u64,
        /// Its data bits.
        byte: u8,
        /// Its stop bit was low.
        framing_error: bool,
        /// Its parity bit disagreed.
        parity_error: bool,
    },
    /// The carrier detector changed its mind at `at`.
    Carrier {
        /// The cycle.
        at: u64,
        /// Whether carrier is present.
        present: bool,
    },
}

/// The 6850's receiver, as the cassette drives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Receiver {
    /// No start bit is coming: the tape is stopped or played out.
    Waiting,
    /// A start bit's falling edge at `edge`, to be confirmed half a bit on.
    Edge { edge: u64 },
    /// A confirmed start bit at `edge`, its frame complete at its stop bit.
    Frame { edge: u64 },
}

/// The record key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RecordKey {
    Up,
    Down,
    /// Popped up when a recording ran into the tape's end.
    Ended,
}

/// A recording in progress.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Recording {
    /// Where on the tape it started.
    start: u64,
    /// What it holds.
    tape: Tape,
    /// The machine cycle it has been made up to.
    until: u64,
}

/// The cassette deck.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cassette {
    tape: Option<Tape>,
    /// The tape's position when the motor last started or stopped, and the
    /// cycle it started at while it runs.
    position: u64,
    started: Option<u64>,
    /// The record key, and what is being recorded while the motor runs.
    record: RecordKey,
    recording: Option<Recording>,
    /// RTS is asserted: the ULA sends the carrier.
    rts: bool,
    receiver: Receiver,
    carrier: bool,
    /// When the carrier detector next changes its mind.
    carrier_change: Option<u64>,
    found: Found,
}

/// What the last searches along the tape found, in tape positions, so a
/// motor relay a program toggles in a loop costs no search of the tape each
/// time. Each answer holds for any later start short of it; a change to the
/// tape forgets them.
#[derive(Clone, Copy, Debug, Default)]
struct Found {
    /// From `.0`, the first start bit falls at `.1`.
    edge: Option<(u64, Option<u64>)>,
    /// From `.0`, the first run-in plays unbroken from `.1.0` until `.1.1`.
    run: Option<(u64, Option<(u64, u64)>)>,
    /// From `.0`, the tone first falls silent at `.1`.
    silence: Option<(u64, Option<u64>)>,
}

impl Found {
    fn edge(&mut self, tape: &Tape, from: u64) -> Option<u64> {
        if let Some((searched, edge)) = self.edge {
            if searched <= from && edge.is_none_or(|edge| from < edge) {
                return edge;
            }
        }
        let start = if tape.tone_at(from) == Tone::Low {
            change(tape, from, |tone| tone != Tone::Low)
        } else {
            Some(from)
        };
        let edge = start.and_then(|start| change(tape, start, |tone| tone == Tone::Low));
        self.edge = Some((from, edge));
        edge
    }

    fn run_in(&mut self, tape: &Tape, from: u64) -> Option<u64> {
        if let Some((searched, run)) = self.run {
            if searched <= from {
                match run {
                    None => return None,
                    Some((start, _)) if from <= start => return Some(start + RUN_IN),
                    Some((_, end)) if from + RUN_IN <= end => return Some(from + RUN_IN),
                    Some(_) => {}
                }
            }
        }
        let run = run_from(tape, from);
        self.run = Some((from, run));
        run.map(|(start, _)| start + RUN_IN)
    }

    fn silence(&mut self, tape: &Tape, from: u64) -> Option<u64> {
        if let Some((searched, silence)) = self.silence {
            if searched <= from && silence.is_none_or(|silence| from < silence) {
                return silence;
            }
        }
        let silence = change(tape, from, |tone| tone == Tone::Silence);
        self.silence = Some((from, silence));
        silence
    }
}

/// A memo: it compares equal to any other, because what the deck does never
/// depends on it.
impl PartialEq for Found {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl Eq for Found {}

/// Where, after tape position `from`, the tone first changes to one `wanted`
/// accepts; the tape's end is a change to silence.
fn change(tape: &Tape, from: u64, wanted: impl Fn(Tone) -> bool) -> Option<u64> {
    let mut at = from;
    loop {
        let (change, tone) = tape.next_change(at)?;
        if wanted(tone) {
            return Some(change);
        }
        at = change;
    }
}

/// Where, from tape position `from`, a tone first plays unbroken for the
/// run-in: when it starts, at `from` itself or later, and when it stops.
fn run_from(tape: &Tape, from: u64) -> Option<(u64, u64)> {
    let mut start = if tape.tone_at(from) == Tone::Silence {
        change(tape, from, |tone| tone != Tone::Silence)?
    } else {
        from
    };
    loop {
        let gap = change(tape, start, |tone| tone == Tone::Silence)?;
        if gap >= start + RUN_IN {
            return Some((start, gap));
        }
        start = change(tape, gap, |tone| tone != Tone::Silence)?;
    }
}

impl Default for Cassette {
    fn default() -> Self {
        Self::new()
    }
}

impl Cassette {
    /// An empty deck, motor off.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            tape: None,
            position: 0,
            started: None,
            record: RecordKey::Up,
            recording: None,
            rts: false,
            receiver: Receiver::Waiting,
            carrier: false,
            carrier_change: None,
            found: Found {
                edge: None,
                run: None,
                silence: None,
            },
        }
    }

    /// Load `tape`, wound to its start, returning the old one with what was
    /// recorded on it.
    pub fn insert(&mut self, now: u64, tape: Tape) -> Option<Tape> {
        let old = self.eject(now);
        self.tape = Some(tape);
        self.found = Found::default();
        self.position = 0;
        self.resume(now);
        old
    }

    /// Take the tape out, with what has been recorded on it.
    pub fn eject(&mut self, now: u64) -> Option<Tape> {
        self.finish_recording(now);
        let tape = self.tape.take();
        self.resume(now);
        tape
    }

    /// The tape, with what has been recorded on it so far laid onto it: a
    /// copy of the tape for a recording under way, so for saving it.
    pub fn tape(&mut self, now: u64) -> Option<&Tape> {
        if self.recording.is_some() {
            self.finish_recording(now);
            self.resume(now);
        }
        self.tape.as_ref()
    }

    /// Where the tape stands at `now`, in cycles from its start.
    #[must_use]
    pub fn position(&self, now: u64) -> u64 {
        self.position
            + self
                .started
                .map_or(0, |started| now.saturating_sub(started))
    }

    /// Wind the tape to `position`, as rewinding or fast-forwarding does.
    pub fn wind(&mut self, now: u64, position: u64) {
        self.finish_recording(now);
        self.position = position.min(self.tape.as_ref().map_or(0, Tape::len));
        self.resume(now);
    }

    /// Press or release record: while it is down, the tape takes what the
    /// 6850 sends while the motor runs.
    pub fn set_record(&mut self, now: u64, record: bool) {
        self.finish_recording(now);
        self.record = if record {
            RecordKey::Down
        } else {
            RecordKey::Up
        };
        self.resume(now);
    }

    /// Whether a tape is in the deck.
    #[must_use]
    pub const fn holds_tape(&self) -> bool {
        self.tape.is_some()
    }

    /// Whether record is down.
    #[must_use]
    pub fn recording(&self) -> bool {
        self.record == RecordKey::Down
    }

    /// Whether the last recording stopped at the tape's end.
    #[must_use]
    pub fn full(&self) -> bool {
        self.record == RecordKey::Ended
    }

    /// The motor relay opens or closes.
    pub fn set_motor(&mut self, now: u64, on: bool) {
        if on == self.started.is_some() {
            return;
        }
        if !on {
            self.fill(now);
            self.position = self.position(now);
        }
        self.started = on.then_some(now);
        if let Some(recording) = &mut self.recording {
            recording.until = now;
        }
        self.resume(now);
    }

    /// Whether the motor runs.
    #[must_use]
    pub const fn motor(&self) -> bool {
        self.started.is_some()
    }

    /// Whether the carrier detector reports carrier.
    #[must_use]
    pub const fn carrier(&self) -> bool {
        self.carrier
    }

    /// RTS changes: the ULA sends the carrier while it is asserted, silence
    /// otherwise.
    pub fn set_rts(&mut self, now: u64, asserted: bool) {
        self.fill(now);
        self.rts = asserted;
    }

    /// The 6850 finished sending `byte` at `done`, framed as `format` in
    /// `frame` cycles: a recording takes its bits.
    pub fn transmitted(&mut self, done: u64, byte: u8, format: Format, frame: u64) {
        let start = done.saturating_sub(frame);
        if let Some(recording) = self
            .recording
            .as_mut()
            .filter(|recording| recording.until > start)
        {
            // The carrier was laid past the frame's start before the frame
            // was known: the frame replaces it.
            let excess = recording.until - start;
            recording
                .tape
                .truncate(recording.tape.len().saturating_sub(excess));
            recording.until = start;
        }
        self.fill(start);
        if !self.rts || self.started.is_none() || self.recording.is_none() {
            return;
        }
        let bits = format.frame_bits();
        let data = u64::from(format.data_bits);
        let even = even_ones(byte, format);
        let mut at = 0;
        for index in 0..bits {
            let high = match index {
                0 => false,
                b if b <= data => byte >> (b - 1) & 1 != 0,
                // Odd parity makes the ones odd, so its bit is set for an
                // even count.
                b if b == data + 1 && format.parity.is_some() => {
                    even == format.parity.unwrap_or(false)
                }
                _ => true,
            };
            let end = frame * (index + 1) / bits;
            if !self.record_tone(if high { Tone::High } else { Tone::Low }, end - at) {
                return;
            }
            at = end;
        }
        if let Some(recording) = &mut self.recording {
            recording.until = done;
        }
    }

    /// Bring a recording up to `now` with what the ULA sends between frames.
    fn fill(&mut self, now: u64) {
        let Some(until) = self.recording.as_ref().map(|recording| recording.until) else {
            return;
        };
        if self.started.is_some() && now > until {
            let idle = if self.rts { Tone::High } else { Tone::Silence };
            if self.record_tone(idle, now - until) {
                if let Some(recording) = &mut self.recording {
                    recording.until = now;
                }
            }
        }
    }

    /// Add `cycles` of `tone` to the recording. At the tape's physical end,
    /// or as many tone changes as a tape holds, recording stops, keeping
    /// what fitted.
    fn record_tone(&mut self, tone: Tone, cycles: u64) -> bool {
        let Some(recording) = &mut self.recording else {
            return false;
        };
        let room = tape::MAX_CYCLES.saturating_sub(recording.start + recording.tape.len());
        let fits = cycles.min(room);
        if recording.tape.push(tone, fits).is_ok() && fits == cycles {
            return true;
        }
        self.record = RecordKey::Ended;
        self.lay_recording();
        false
    }

    /// Lay what has been recorded onto the tape.
    fn finish_recording(&mut self, now: u64) {
        self.fill(now);
        self.lay_recording();
    }

    fn lay_recording(&mut self) {
        if let (Some(recording), Some(tape)) = (self.recording.take(), self.tape.as_mut()) {
            if tape.record_over(recording.start, &recording.tape).is_err() {
                self.record = RecordKey::Ended;
            }
            self.found = Found::default();
        }
    }

    /// Whether RTS is asserted.
    #[must_use]
    pub(crate) const fn rts(&self) -> bool {
        self.rts
    }

    /// The tape moved, stopped or changed at `now`: recording starts if it
    /// should, the receiver looks afresh, and the carrier detector starts
    /// again from silence.
    fn resume(&mut self, now: u64) {
        if self.record == RecordKey::Down
            && self.started.is_some()
            && self.recording.is_none()
            && self.tape.is_some()
        {
            self.recording = Some(Recording {
                start: self.position(now),
                tape: Tape::default(),
                until: now,
            });
        }
        self.receiver = self
            .edge_after(now)
            .map_or(Receiver::Waiting, |edge| Receiver::Edge { edge });
        self.carrier_change = if self.carrier {
            Some(now)
        } else {
            self.run_in_from(now)
        };
    }

    /// The tape the head plays, while the motor runs and nothing records.
    fn playing(&self) -> Option<&Tape> {
        self.tape
            .as_ref()
            .filter(|_| self.started.is_some() && self.recording.is_none())
    }

    /// The tone the ULA hears at `now`.
    fn tone(&self, now: u64) -> Tone {
        self.playing()
            .map_or(Tone::Silence, |tape| tape.tone_at(self.position(now)))
    }

    /// Search the tape the head plays with `search`, from where it stands at
    /// `from`, for a tape position, given back as the cycle the head reaches
    /// it.
    fn search(
        &mut self,
        from: u64,
        search: impl FnOnce(&mut Found, &Tape, u64) -> Option<u64>,
    ) -> Option<u64> {
        let offset = self.position(from);
        let tape = self
            .tape
            .as_ref()
            .filter(|_| self.started.is_some() && self.recording.is_none())?;
        let at = search(&mut self.found, tape, offset)?;
        Some(from + (at - offset))
    }

    /// The first start bit's falling edge at or after `from`: the line must
    /// be high before it falls.
    fn edge_after(&mut self, from: u64) -> Option<u64> {
        self.search(from, Found::edge)
    }

    /// When a tone first plays unbroken for the run-in, looking from `from`.
    fn run_in_from(&mut self, from: u64) -> Option<u64> {
        self.search(from, Found::run_in)
    }

    /// The cycle at which the cassette next tells the 6850 something, for a
    /// receiver sampling at `rate` bits a second and expecting `format`.
    #[must_use]
    pub fn next_event(&self, rate: u64, format: Format) -> Option<u64> {
        let receiver = match self.receiver {
            Receiver::Waiting => None,
            Receiver::Edge { edge } => Some(edge + middle(0, rate)),
            Receiver::Frame { edge } => Some(edge + middle(stop_bit(format), rate)),
        };
        [receiver, self.carrier_change].into_iter().flatten().min()
    }

    /// Run everything due by `now`, telling the 6850 through `on_event`;
    /// frames reach it only while it is `listening` to the cassette.
    pub fn advance(
        &mut self,
        now: u64,
        rate: u64,
        format: Format,
        listening: bool,
        mut on_event: impl FnMut(Event),
    ) {
        while let Some(at) = self.next_event(rate, format).filter(|&at| at <= now) {
            if self.carrier_change == Some(at) {
                self.carrier = !self.carrier;
                on_event(Event::Carrier {
                    at,
                    present: self.carrier,
                });
                // The tape's end reads as a change to silence.
                self.carrier_change = if self.carrier {
                    self.search(at, Found::silence)
                } else {
                    self.run_in_from(at)
                };
                continue;
            }
            match self.receiver {
                Receiver::Edge { edge } => {
                    self.receiver = if self.tone(at) == Tone::Low {
                        Receiver::Frame { edge }
                    } else {
                        self.edge_after(at)
                            .map_or(Receiver::Waiting, |edge| Receiver::Edge { edge })
                    };
                }
                Receiver::Frame { edge } => {
                    if listening {
                        on_event(self.frame(edge, at, rate, format));
                    }
                    self.receiver = self
                        .edge_after(at)
                        .map_or(Receiver::Waiting, |edge| Receiver::Edge { edge });
                }
                Receiver::Waiting => break,
            }
        }
    }

    /// The frame whose start bit fell at `edge`, read at its bits' middles.
    fn frame(&self, edge: u64, at: u64, rate: u64, format: Format) -> Event {
        let high = |k: u64| self.tone(edge + middle(k, rate)) != Tone::Low;
        let data = u64::from(format.data_bits);
        let byte = (0..data).fold(0u8, |byte, k| byte | (u8::from(high(k + 1)) << k));
        let even = even_ones(byte, format);
        Event::Frame {
            at,
            byte,
            framing_error: !high(stop_bit(format)),
            parity_error: format
                .parity
                .is_some_and(|odd| high(data + 1) != (even == odd)),
        }
    }
}

impl crate::snapshot::Save for Receiver {
    fn save(&self, w: &mut crate::snapshot::Writer) {
        match *self {
            Self::Waiting => w.put(&0u8),
            Self::Edge { edge } => {
                w.put(&1u8);
                w.put(&edge);
            }
            Self::Frame { edge } => {
                w.put(&2u8);
                w.put(&edge);
            }
        }
    }
}

impl crate::snapshot::Load for Receiver {
    fn load(r: &mut crate::snapshot::Reader<'_>) -> Result<Self, crate::snapshot::SnapshotError> {
        let receiver = match r.get::<u8>()? {
            0 => Self::Waiting,
            1 => Self::Edge { edge: r.get()? },
            2 => Self::Frame { edge: r.get()? },
            _ => return Err(crate::snapshot::SnapshotError::Malformed),
        };
        Ok(receiver)
    }
}

impl crate::snapshot::Save for RecordKey {
    fn save(&self, w: &mut crate::snapshot::Writer) {
        w.put(&match self {
            Self::Up => 0u8,
            Self::Down => 1,
            Self::Ended => 2,
        });
    }
}

impl crate::snapshot::Load for RecordKey {
    fn load(r: &mut crate::snapshot::Reader<'_>) -> Result<Self, crate::snapshot::SnapshotError> {
        match r.get::<u8>()? {
            0 => Ok(Self::Up),
            1 => Ok(Self::Down),
            2 => Ok(Self::Ended),
            _ => Err(crate::snapshot::SnapshotError::Malformed),
        }
    }
}

crate::snapshot::fields!(Recording { start, tape, until });
crate::snapshot::fields!(
    Cassette {
        tape,
        position,
        started,
        record,
        recording,
        rts,
        receiver,
        carrier,
        carrier_change,
    }
    transient { found },
    check | cassette,
    r | cassette.restorable(r)
);

impl Cassette {
    /// Whether a restored deck is one the machine could leave: the tape no
    /// further on than it could have run, a recording only onto a tape with
    /// record down, and its times at the machine's. The receiver's next
    /// sample, which a start bit's edge sets, is held to the clock with the
    /// 6850's rate.
    fn restorable(&self, r: &crate::snapshot::Reader<'_>) -> bool {
        let furthest = tape::MAX_CYCLES + r.clock();
        let recording = self.recording.as_ref().is_none_or(|recording| {
            self.record == RecordKey::Down
                && self.tape.is_some()
                && recording.start <= furthest
                && r.reached(recording.until)
        });
        let receiver = match self.receiver {
            Receiver::Waiting => true,
            Receiver::Edge { edge } | Receiver::Frame { edge } => edge <= r.horizon(),
        };
        self.position <= furthest
            && self.started.is_none_or(|started| r.reached(started))
            && recording
            && receiver
            && self.carrier_change.is_none_or(|at| r.pending(at))
    }
}

/// The index of a frame's first stop bit: after the start bit, the data and
/// any parity.
fn stop_bit(format: Format) -> u64 {
    1 + u64::from(format.data_bits) + u64::from(format.parity.is_some())
}

/// Cycles from a frame's start to the middle of its bit `k` at `rate` bits a
/// second.
fn middle(k: u64, rate: u64) -> u64 {
    (2 * k + 1) * CLOCK / (2 * rate.max(1))
}

/// Whether the data bits of `byte` hold an even number of ones.
fn even_ones(byte: u8, format: Format) -> bool {
    let data = u16::from(byte) & ((1 << format.data_bits) - 1);
    data.count_ones().is_multiple_of(2)
}

#[cfg(test)]
#[path = "cassette_tests.rs"]
mod tests;
