//! Cassette tapes as the serial ULA hears them: stretches of the high tone,
//! the low tone, or silence, timed in 2 MHz machine cycles of tape motion.
//!
//! UEF images (the chunked format emulators share) decode chunk by chunk:
//! a data byte becomes its framed bits, each one cycle of the base frequency
//! (1200 Hz, the low tone, for 0) or two of twice it (the high tone, for 1)
//! at 1200 baud, four times as long at 300. CSW images, a recording's square
//! wave as pulse lengths, decode pulse by pulse: a half-cycle shorter than
//! the midpoint between the tones' is the high tone, a longer one the low,
//! and one too long for either is silence. Either may arrive gzipped. A tape
//! encodes back to UEF: what reads as framed bytes as data chunks, the rest a
//! cycle at a time, with its markers and base frequencies where they lay.
//!
//! Time is kept exactly: the tones' periods are no whole number of machine
//! cycles, so durations add up in cycles with 32 fractional bits and each
//! stretch's end is rounded from the exact total.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_compress::{gzip, zlib};

use crate::machine::CLOCK;

/// The high tone, in hertz: a 1, or the carrier.
pub const HIGH_TONE_HZ: u64 = 2 * LOW_TONE_HZ;
/// The low tone, in hertz: a 0.
pub const LOW_TONE_HZ: u64 = 1200;
/// Fractional bits of fine time.
const FRACTION: u32 = 32;
/// The base frequency tones are built on by default, in millihertz: the
/// low tone's.
const BASE_MILLIHERTZ: u64 = LOW_TONE_HZ * 1000;
/// The base frequencies a UEF may name, in millihertz.
const BASES: core::ops::RangeInclusive<u64> = 100_000..=20_000_000;
/// The bit rate a tape is recorded at by default: a bit to a low-tone cycle.
const DEFAULT_BAUD: u32 = 1200;
/// The bit rates a tape is recorded at, slowest first.
const BAUDS: [u32; 2] = [300, DEFAULT_BAUD];
/// A CSW half-cycle no longer than the midpoint between the tones' halves is
/// the high tone; one up to a whole low-tone cycle, the low; longer, silence.
const HIGH_HALF_CYCLE_HZ: u64 = 4 * HIGH_TONE_HZ * LOW_TONE_HZ / (HIGH_TONE_HZ + LOW_TONE_HZ);
/// The dummy byte the 0x0111 chunk puts between its two carriers.
const DUMMY_BYTE: u8 = 0xAA;
/// Cycles either side of a bit's edges where rounding may move a change.
const EDGE_SLACK: u64 = 4;

/// The most tape there is, in cycles: one side of a C120.
pub const MAX_CYCLES: u64 = 3600 * CLOCK;
/// The most tone changes a tape holds.
const MAX_SPANS: usize = 1 << 22;
/// The most markers a tape holds, and the longest label each gives.
const MAX_MARKERS: usize = 1 << 10;
const MAX_LABEL: usize = 255;
/// The most changes of base frequency a tape holds.
const MAX_BASES: usize = 1 << 10;
/// The largest image read, once any gzip around it is unwrapped.
pub const MAX_IMAGE: usize = 16 << 20;

const UEF_MAGIC: &[u8; 10] = b"UEF File!\0";
const CSW_MAGIC: &[u8; 23] = b"Compressed Square Wave\x1A";

/// Fine time: machine cycles with `FRACTION` fractional bits.
type Fine = u128;

/// `count` of something lasting `numerator / denominator` seconds each, in
/// fine time.
fn lasting(count: u64, numerator: u64, denominator: u64) -> Fine {
    (Fine::from(count) * Fine::from(numerator) * (Fine::from(CLOCK) << FRACTION))
        / Fine::from(denominator.max(1))
}

/// The nearest whole cycle to a fine time.
fn whole(fine: Fine) -> u64 {
    u64::try_from((fine + (1 << (FRACTION - 1))) >> FRACTION).unwrap_or(u64::MAX)
}

/// The nearest whole number of periods of `millihertz` in `cycles`.
fn periods(cycles: u64, millihertz: u64) -> u64 {
    let clock = u128::from(CLOCK) * 1000;
    let periods = (u128::from(cycles) * u128::from(millihertz) + clock / 2) / clock;
    u64::try_from(periods).unwrap_or(u64::MAX)
}

/// What the tape carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// The high tone: a 1, or the carrier.
    High,
    /// The low tone: a 0.
    Low,
    /// No tone: the gap between blocks.
    Silence,
}

impl Tone {
    const fn code(self) -> u64 {
        match self {
            Self::High => 0,
            Self::Low => 1,
            Self::Silence => 2,
        }
    }

    const fn from_code(code: u64) -> Self {
        match code {
            0 => Self::High,
            1 => Self::Low,
            _ => Self::Silence,
        }
    }
}

/// A stretch of one tone, packed: its tone in the top two bits, the cycle it
/// ends at in the rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span(u64);

impl Span {
    const END: u64 = (1 << 62) - 1;

    const fn new(tone: Tone, end: u64) -> Self {
        Self(tone.code() << 62 | (end & Self::END))
    }

    const fn tone(self) -> Tone {
        Tone::from_code(self.0 >> 62)
    }

    const fn end(self) -> u64 {
        self.0 & Self::END
    }

    const fn set_end(&mut self, end: u64) {
        *self = Self::new(self.tone(), end);
    }
}

/// A label a tape image gives a place on the tape.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Marker {
    /// Cycles into the tape.
    at: u64,
    /// The image's text.
    label: String,
}

/// From `at` cycles into the tape, its tones are cycles of `millihertz`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Base {
    at: u64,
    millihertz: u64,
}

/// A tape: its tones in order, and what its image said of them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tape {
    spans: Vec<Span>,
    markers: Vec<Marker>,
    /// Where the base frequency changes from the default, in order.
    bases: Vec<Base>,
}

/// Why an image was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapeError {
    /// Neither a UEF nor a CSW image.
    Unrecognised,
    /// The image ends inside a structure it declared.
    Truncated,
    /// A field holds a value the format does not allow.
    Malformed,
    /// The image is larger, or decodes to more tape, markers or bytes, than
    /// a cassette holds.
    TooLong,
    /// The compressed data does not decode.
    Corrupt,
}

impl Tape {
    /// Decode a UEF or CSW image, gzipped or not, of no more than
    /// [`MAX_IMAGE`] bytes; a gzip inside the gzip is refused rather than
    /// unwrapped again.
    pub fn decode(bytes: &[u8]) -> Result<Self, TapeError> {
        if bytes.starts_with(&gzip::MAGIC) {
            Self::decode_unwrapped(&gunzip(bytes)?)
        } else if bytes.len() > MAX_IMAGE {
            Err(TapeError::TooLong)
        } else {
            Self::decode_unwrapped(bytes)
        }
    }

    fn decode_unwrapped(bytes: &[u8]) -> Result<Self, TapeError> {
        if bytes.starts_with(UEF_MAGIC) {
            decode_uef(bytes)
        } else if bytes.starts_with(CSW_MAGIC) {
            decode_csw(bytes)
        } else {
            Err(TapeError::Unrecognised)
        }
    }

    /// Cycles of tape, start to end.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.spans.last().map_or(0, |span| span.end())
    }

    /// Whether the tape holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// The tone at `at` cycles into the tape: silence past its end.
    #[must_use]
    pub fn tone_at(&self, at: u64) -> Tone {
        let index = self.spans.partition_point(|span| span.end() <= at);
        self.spans
            .get(index)
            .map_or(Tone::Silence, |span| span.tone())
    }

    /// When, after `at`, the tone next changes, and to what; `None` past the
    /// tape's end.
    #[must_use]
    pub fn next_change(&self, at: u64) -> Option<(u64, Tone)> {
        let index = self.spans.partition_point(|span| span.end() <= at);
        let current = self.spans.get(index)?;
        let next = self
            .spans
            .get(index + 1)
            .map_or(Tone::Silence, |span| span.tone());
        Some((current.end(), next))
    }

    /// Record `recorded` over the tape from `at`, as a recording head does:
    /// what lay there and after is gone, and a recording that starts past
    /// the tape's end follows silence. The recording is at the default base
    /// frequency. A result longer than a tape holds is refused, the tape
    /// unchanged.
    pub fn record_over(&mut self, at: u64, recorded: &Self) -> Result<(), TapeError> {
        let keep = self.spans.partition_point(|span| span.end() <= at);
        let cut = self.spans.get(keep).map(|span| span.tone());
        // Only what is recorded over moves, so a refusal can put it back.
        let tail = self.spans.split_off(keep);
        let last_kept = self.spans.last().copied();
        let mut builder = Builder::continuing(core::mem::take(&mut self.spans));
        let recording = builder
            .push_cycles(cut.unwrap_or(Tone::Silence), at - builder.end())
            .and_then(|()| {
                let mut from = 0;
                recorded.spans.iter().try_for_each(|span| {
                    let pushed = builder.push_cycles(span.tone(), span.end() - from);
                    from = span.end();
                    pushed
                })
            });
        self.spans = builder.spans;
        if let Err(error) = recording {
            self.spans.truncate(keep);
            if let (Some(last), Some(kept)) = (self.spans.last_mut(), last_kept) {
                *last = kept;
            }
            self.spans.extend(tail);
            return Err(error);
        }
        self.markers.retain(|marker| marker.at < at);
        self.bases.retain(|base| base.at < at);
        if !recorded.is_empty() && self.base_at(at) != BASE_MILLIHERTZ {
            self.bases.push(Base {
                at,
                millihertz: BASE_MILLIHERTZ,
            });
        }
        Ok(())
    }

    /// The base frequency in force at `at`, in millihertz.
    fn base_at(&self, at: u64) -> u64 {
        let index = self.bases.partition_point(|base| base.at <= at);
        index
            .checked_sub(1)
            .map_or(BASE_MILLIHERTZ, |index| self.bases[index].millihertz)
    }

    /// Cut the tape at `at` cycles, dropping everything after.
    pub fn truncate(&mut self, at: u64) {
        // The spans that start before `at`: those ending before it, and the
        // one it falls in.
        let ending_before = self.spans.partition_point(|span| span.end() < at);
        self.spans
            .truncate(if at == 0 { 0 } else { ending_before + 1 });
        if let Some(last) = self.spans.last_mut().filter(|last| last.end() > at) {
            last.set_end(at);
        }
        self.markers.retain(|marker| marker.at < at);
        self.bases.retain(|base| base.at < at);
    }

    /// Append `cycles` of `tone` to the tape: what a recording is built of.
    pub fn push(&mut self, tone: Tone, cycles: u64) -> Result<(), TapeError> {
        let mut builder = Builder::continuing(core::mem::take(&mut self.spans));
        let pushed = builder.push_cycles(tone, cycles);
        self.spans = builder.spans;
        pushed
    }

    /// Encode the tape as UEF, its markers and base frequencies with it.
    #[must_use]
    pub fn encode_uef(&self) -> Vec<u8> {
        let mut encoder = Encoder {
            tape: self,
            out: UEF_MAGIC.iter().copied().chain([10, 0]).collect(),
            baud: DEFAULT_BAUD,
            base: BASE_MILLIHERTZ,
        };
        let (mut markers, mut bases) =
            (self.markers.iter().peekable(), self.bases.iter().peekable());
        let len = self.len();
        let mut start = 0;
        loop {
            while let Some(marker) = markers.next_if(|marker| marker.at <= start) {
                let label = marker
                    .label
                    .chars()
                    .map(|c| u8::try_from(c).unwrap_or(b'?'));
                chunk(
                    &mut encoder.out,
                    0x0120,
                    &label.chain([0]).collect::<Vec<u8>>(),
                );
            }
            while let Some(base) = bases.next_if(|base| base.at <= start) {
                encoder.set_base(base.millihertz);
            }
            if start >= len {
                break;
            }
            // A segment runs to the next marker or base change, and holds
            // silence or tone but not both.
            let boundary = [markers.peek().map(|m| m.at), bases.peek().map(|b| b.at)]
                .into_iter()
                .flatten()
                .fold(len, u64::min);
            let silent = self.tone_at(start) == Tone::Silence;
            let change = if silent {
                self.next_change(start).map(|(end, _)| end)
            } else {
                change_to(self, start, |tone| tone == Tone::Silence)
            };
            let end = change.unwrap_or(len).min(boundary);
            if silent {
                encoder.silence(end - start);
            } else {
                encoder.tone(start, end);
            }
            start = end;
        }
        encoder.out
    }

    /// The tone held unchanged from `from` to `to`, if one is.
    fn held(&self, from: u64, to: u64) -> Option<Tone> {
        let tone = self.tone_at(from);
        let unbroken = self
            .next_change(from)
            .is_none_or(|(change, _)| change >= to);
        (tone != Tone::Silence && unbroken).then_some(tone)
    }
}

/// Where, after `from`, the tape's tone first changes to one `wanted`
/// accepts.
fn change_to(tape: &Tape, from: u64, wanted: impl Fn(Tone) -> bool) -> Option<u64> {
    let mut at = from;
    loop {
        let (change, tone) = tape.next_change(at)?;
        if wanted(tone) {
            return Some(change);
        }
        at = change;
    }
}

/// Writes a tape as UEF chunks, keeping the bit rate and base frequency the
/// chunks so far have set.
struct Encoder<'a> {
    tape: &'a Tape,
    out: Vec<u8>,
    baud: u32,
    /// In millihertz.
    base: u64,
}

impl Encoder<'_> {
    fn set_base(&mut self, millihertz: u64) {
        if millihertz != self.base {
            // Within f64's exact integers, and the nearest single to the
            // quotient is the one the image held.
            #[expect(
                clippy::cast_precision_loss,
                clippy::cast_possible_truncation,
                reason = "a base frequency is at most 20 MHz in millihertz"
            )]
            let hertz = (millihertz as f64 / 1000.0) as f32;
            chunk(&mut self.out, 0x0113, &hertz.to_bits().to_le_bytes());
            self.base = millihertz;
        }
    }

    /// `cycles` of silence, in high-tone cycles.
    fn silence(&mut self, cycles: u64) {
        for units in pieces(periods(cycles, 2 * self.base)) {
            chunk(&mut self.out, 0x0112, &units.to_le_bytes());
        }
    }

    /// `start..end`, a stretch of tone: as framed bytes where it reads as
    /// them, a cycle at a time otherwise.
    fn tone(&mut self, start: u64, end: u64) {
        // Slow bytes can read as fast ones, never the other way round.
        let reading = BAUDS.into_iter().find_map(|baud| {
            frames(self.tape, start, end, baud, self.base).map(|pieces| (baud, pieces))
        });
        let Some((baud, read)) = reading else {
            self.security_cycles(start, end);
            return;
        };
        // Carrier alone reads at any rate.
        let bytes = read.iter().any(|piece| matches!(piece, Piece::Bytes(_)));
        if bytes && baud != self.baud {
            let rate = u16::try_from(baud).unwrap_or(u16::MAX);
            chunk(&mut self.out, 0x0117, &rate.to_le_bytes());
            self.baud = baud;
        }
        for piece in read {
            match piece {
                Piece::Carrier(cycles) => {
                    for count in pieces(cycles) {
                        chunk(&mut self.out, 0x0110, &count.to_le_bytes());
                    }
                }
                Piece::Bytes(bytes) => chunk(&mut self.out, 0x0100, &bytes),
            }
        }
    }

    /// `start..end` as security cycles: each tone a whole number of its own
    /// cycles, a bit a cycle, most significant first.
    fn security_cycles(&mut self, start: u64, end: u64) {
        const MOST: usize = 0xFF_FFFF;
        let spans = &self.tape.spans;
        let mut index = spans.partition_point(|span| span.end() <= start);
        let (mut from, mut bits, mut count) = (start, Vec::new(), 0usize);
        while from < end {
            let Some(span) = spans.get(index) else {
                break;
            };
            let to = span.end().min(end);
            let high = span.tone() == Tone::High;
            let frequency = if high { 2 * self.base } else { self.base };
            for _ in 0..periods(to - from, frequency) {
                if count == MOST {
                    security_chunk(&mut self.out, count, &bits);
                    (bits, count) = (Vec::new(), 0);
                }
                if count % 8 == 0 {
                    bits.push(0);
                }
                if high {
                    if let Some(byte) = bits.last_mut() {
                        *byte |= 0x80 >> (count % 8);
                    }
                }
                count += 1;
            }
            (from, index) = (to, index + 1);
        }
        if count > 0 {
            security_chunk(&mut self.out, count, &bits);
        }
    }
}

/// A 0x0114 chunk of `count` cycles, their bits packed in `bits`.
fn security_chunk(out: &mut Vec<u8>, count: usize, bits: &[u8]) {
    let count = u32::try_from(count).unwrap_or(0).to_le_bytes();
    let mut data = Vec::with_capacity(5 + bits.len());
    data.extend_from_slice(&count[..3]);
    data.extend_from_slice(b"WW");
    data.extend_from_slice(bits);
    chunk(out, 0x0114, &data);
}

/// Read `start..end` of `tape`, a stretch of tone, as carrier and 8N1 bytes
/// at `baud` on a base of `base` millihertz, or `None` where it is anything
/// else: every bit of a frame must hold one tone throughout. Each frame is
/// read from its start bit's own edge, and the carrier before it, which a
/// UEF counts in whole cycles, measured from the exact end of the last stop
/// bit.
fn frames(tape: &Tape, start: u64, end: u64, baud: u32, base: u64) -> Option<Vec<Piece>> {
    let bit = lasting(u64::from(DEFAULT_BAUD / baud), 1000, base);
    let frame = 10 * bit;
    let cell = |low: u64, k: u128| {
        let from = low + whole(k * bit);
        let to = low + whole((k + 1) * bit);
        tape.held(from + EDGE_SLACK, to - EDGE_SLACK)
    };
    let mut pieces = Vec::new();
    let mut idle = Fine::from(start) << FRACTION;
    while whole(idle) < end {
        let low = next_low(tape, whole(idle), end);
        let cycles = periods(low.saturating_sub(whole(idle)), 2 * base);
        if cycles > 0 {
            pieces.push(Piece::Carrier(cycles));
        }
        if low >= end {
            break;
        }
        let after = (Fine::from(low) << FRACTION) + frame;
        if whole(after) > end {
            return None;
        }
        let mut tones = [Tone::Silence; 10];
        for (k, tone) in (0..).zip(&mut tones) {
            *tone = cell(low, k)?;
        }
        if tones[0] != Tone::Low || tones[9] != Tone::High {
            return None;
        }
        let byte = (0..8).fold(0u8, |byte, k| {
            byte | (u8::from(tones[k + 1] == Tone::High) << k)
        });
        match pieces.last_mut() {
            Some(Piece::Bytes(bytes)) => bytes.push(byte),
            _ => pieces.push(Piece::Bytes(vec![byte])),
        }
        idle = after;
    }
    Some(pieces)
}

/// Where the low tone next starts at or after `at`, or `end`.
fn next_low(tape: &Tape, mut at: u64, end: u64) -> u64 {
    while at < end && tape.tone_at(at) != Tone::Low {
        match tape.next_change(at) {
            Some((change, _)) => at = change,
            None => return end,
        }
    }
    at.min(end)
}

/// A stretch of a tape the UEF encoder could read.
enum Piece {
    /// So many cycles of the high tone.
    Carrier(u64),
    /// Framed bytes back to back.
    Bytes(Vec<u8>),
}

/// Unwrap a gzipped image.
fn gunzip(bytes: &[u8]) -> Result<Vec<u8>, TapeError> {
    let mut member = Box::new(gzip::Member::new());
    let mut scratch = vec![0; 1 << 14];
    let mut out = Vec::new();
    gzip::decompress(bytes, &mut member, &mut scratch, |piece| {
        out.extend_from_slice(piece);
        out.len() <= MAX_IMAGE
    })
    .map_err(|error| match error {
        gzip::Error::Refused => TapeError::TooLong,
        gzip::Error::Truncated => TapeError::Truncated,
        _ => TapeError::Corrupt,
    })?;
    Ok(out)
}

/// Write a UEF chunk.
fn chunk(out: &mut Vec<u8>, id: u16, data: &[u8]) {
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(&u32::try_from(data.len()).unwrap_or(u32::MAX).to_le_bytes());
    out.extend_from_slice(data);
}

/// A count split into the 16-bit pieces a UEF length field holds.
fn pieces(count: u64) -> impl Iterator<Item = u16> {
    let whole = usize::try_from(count / u64::from(u16::MAX)).unwrap_or(0);
    let rest = u16::try_from(count % u64::from(u16::MAX)).unwrap_or(0);
    core::iter::repeat_n(u16::MAX, whole).chain((rest > 0).then_some(rest))
}

/// Builds spans from durations, merging runs of one tone and rounding each
/// end from the exact running time so no error accumulates.
struct Builder {
    spans: Vec<Span>,
    fine: Fine,
}

impl Builder {
    const fn new() -> Self {
        Self {
            spans: Vec::new(),
            fine: 0,
        }
    }

    fn continuing(spans: Vec<Span>) -> Self {
        let fine = spans
            .last()
            .map_or(0, |span| Fine::from(span.end()) << FRACTION);
        Self { spans, fine }
    }

    fn end(&self) -> u64 {
        self.spans.last().map_or(0, |span| span.end())
    }

    /// Append `fine` time of `tone`.
    fn push(&mut self, tone: Tone, fine: Fine) -> Result<(), TapeError> {
        self.fine += fine;
        let end = whole(self.fine);
        if end > MAX_CYCLES {
            return Err(TapeError::TooLong);
        }
        self.extend(tone, end)
    }

    /// Append `cycles` of `tone`, exactly.
    fn push_cycles(&mut self, tone: Tone, cycles: u64) -> Result<(), TapeError> {
        let end = self.end() + cycles;
        if end > MAX_CYCLES {
            return Err(TapeError::TooLong);
        }
        self.fine = Fine::from(end) << FRACTION;
        self.extend(tone, end)
    }

    fn extend(&mut self, tone: Tone, end: u64) -> Result<(), TapeError> {
        let last_end = self.end();
        match self.spans.last_mut() {
            Some(last) if last.tone() == tone => last.set_end(end),
            _ if end > last_end => {
                if self.spans.len() == MAX_SPANS {
                    return Err(TapeError::TooLong);
                }
                self.spans.push(Span::new(tone, end));
            }
            _ => {}
        }
        Ok(())
    }
}

/// The bits a framed byte goes out as: start, data least significant first,
/// any parity, stops.
fn framed(
    byte: u8,
    data_bits: u8,
    parity: Option<bool>,
    stop_bits: u8,
) -> impl Iterator<Item = bool> {
    let data = (0..data_bits).map(move |bit| byte >> bit & 1 != 0);
    let ones = (0..data_bits).filter(|&bit| byte >> bit & 1 != 0).count();
    // Odd parity makes the count of ones odd.
    let parity = parity.map(|odd| (ones % 2 == 0) == odd);
    core::iter::once(false)
        .chain(data)
        .chain(parity)
        .chain(core::iter::repeat_n(true, usize::from(stop_bits)))
}

/// The UEF decoder's state: what the timing chunks set, and what the image
/// says of the tape.
struct Uef {
    builder: Builder,
    /// The base frequency, in millihertz.
    base: u64,
    baud: u32,
    markers: Vec<Marker>,
    bases: Vec<Base>,
}

impl Uef {
    /// `count` cycles of the high tone, twice the base frequency.
    fn high_cycles(&self, count: u64) -> Fine {
        lasting(count, 1000, 2 * self.base)
    }

    fn bits(&mut self, bits: impl Iterator<Item = bool>) -> Result<(), TapeError> {
        // A bit is one base cycle at 1200 baud, four at 300.
        let bit = lasting(u64::from(DEFAULT_BAUD / self.baud), 1000, self.base);
        for high in bits {
            self.builder
                .push(if high { Tone::High } else { Tone::Low }, bit)?;
        }
        Ok(())
    }

    fn carrier(&mut self, count: u16) -> Result<(), TapeError> {
        let fine = self.high_cycles(u64::from(count));
        self.builder.push(Tone::High, fine)
    }

    /// Security cycles, a bit a whole cycle of its tone, but a first or last
    /// marked `P` only half of one.
    fn cycles(&mut self, body: &[u8]) -> Result<(), TapeError> {
        let [a, b, c, first, last, bits @ ..] = body else {
            return Err(TapeError::Truncated);
        };
        let count = usize::from(*a) | usize::from(*b) << 8 | usize::from(*c) << 16;
        if bits.len() * 8 < count {
            return Err(TapeError::Truncated);
        }
        // Whole and half cycles of each tone, low then high.
        let cycle = [self.base, 2 * self.base]
            .map(|frequency| [lasting(1, 1000, frequency), lasting(1, 1000, 2 * frequency)]);
        for index in 0..count {
            let high = bits[index / 8] >> (7 - index % 8) & 1 != 0;
            let half = (index == 0 && *first == b'P') || (index + 1 == count && *last == b'P');
            let tone = if high { Tone::High } else { Tone::Low };
            self.builder
                .push(tone, cycle[usize::from(high)][usize::from(half)])?;
        }
        Ok(())
    }

    fn marker(&mut self, label: &[u8]) -> Result<(), TapeError> {
        let label = &label[..label
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(label.len())];
        if self.markers.len() == MAX_MARKERS || label.len() > MAX_LABEL {
            return Err(TapeError::TooLong);
        }
        self.markers.push(Marker {
            at: self.builder.end(),
            label: label.iter().map(|&byte| char::from(byte)).collect(),
        });
        Ok(())
    }

    /// The base frequency changes here: only a real change is kept, the
    /// last of several at one place.
    fn set_base(&mut self, millihertz: u64) -> Result<(), TapeError> {
        let at = self.builder.end();
        if self.bases.last().is_some_and(|last| last.at == at) {
            self.bases.pop();
        }
        let before = self
            .bases
            .last()
            .map_or(BASE_MILLIHERTZ, |base| base.millihertz);
        if millihertz != before {
            if self.bases.len() == MAX_BASES {
                return Err(TapeError::TooLong);
            }
            self.bases.push(Base { at, millihertz });
        }
        self.base = millihertz;
        Ok(())
    }
}

fn le16(bytes: &[u8], at: usize) -> Result<u16, TapeError> {
    let field = bytes.get(at..at + 2).ok_or(TapeError::Truncated)?;
    Ok(u16::from_le_bytes([field[0], field[1]]))
}

fn le32(bytes: &[u8], at: usize) -> Result<u32, TapeError> {
    let field = bytes.get(at..at + 4).ok_or(TapeError::Truncated)?;
    Ok(u32::from_le_bytes([field[0], field[1], field[2], field[3]]))
}

/// A UEF float scaled by `scale` to a whole number, refused outside
/// `range`.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "the value is checked finite and within a small positive range before the conversion"
)]
fn float(
    bytes: &[u8],
    scale: f32,
    range: core::ops::RangeInclusive<u64>,
) -> Result<u64, TapeError> {
    let scaled = f32::from_bits(le32(bytes, 0)?) * scale;
    let max = *range.end() as f32;
    if !(scaled.is_finite() && scaled >= 0.0 && scaled <= max) {
        return Err(TapeError::Malformed);
    }
    let value = (scaled + 0.5) as u64;
    if range.contains(&value) {
        Ok(value)
    } else {
        Err(TapeError::Malformed)
    }
}

fn decode_uef(bytes: &[u8]) -> Result<Tape, TapeError> {
    let mut uef = Uef {
        builder: Builder::new(),
        base: BASE_MILLIHERTZ,
        baud: DEFAULT_BAUD,
        markers: Vec::new(),
        bases: Vec::new(),
    };
    let mut at = UEF_MAGIC.len() + 2;
    while at < bytes.len() {
        let id = le16(bytes, at)?;
        let len = usize::try_from(le32(bytes, at + 2)?).map_err(|_| TapeError::TooLong)?;
        let body = bytes
            .get(at + 6..)
            .and_then(|rest| rest.get(..len))
            .ok_or(TapeError::Truncated)?;
        at += 6 + len;
        match id {
            0x0100 => {
                for &byte in body {
                    uef.bits(framed(byte, 8, None, 1))?;
                }
            }
            0x0102 => {
                let (&excess, data) = body.split_first().ok_or(TapeError::Malformed)?;
                let total = (data.len() * 8)
                    .checked_sub(usize::from(excess))
                    .ok_or(TapeError::Malformed)?;
                let bits = data
                    .iter()
                    .flat_map(|&byte| (0..8).map(move |bit| byte >> bit & 1 != 0));
                uef.bits(bits.take(total))?;
            }
            0x0104 => {
                let [data_bits, parity, stop, data @ ..] = body else {
                    return Err(TapeError::Malformed);
                };
                let parity = match parity {
                    b'N' => None,
                    b'E' => Some(false),
                    b'O' => Some(true),
                    _ => return Err(TapeError::Malformed),
                };
                if !(1..=8).contains(data_bits) {
                    return Err(TapeError::Malformed);
                }
                // A negative stop count adds a single high-tone cycle.
                let stops = stop.cast_signed();
                for &byte in data {
                    uef.bits(framed(byte, *data_bits, parity, stops.unsigned_abs()))?;
                    if stops < 0 {
                        uef.carrier(1)?;
                    }
                }
            }
            0x0110 => uef.carrier(le16(body, 0)?)?,
            0x0111 => {
                uef.carrier(le16(body, 0)?)?;
                uef.bits(framed(DUMMY_BYTE, 8, None, 1))?;
                uef.carrier(le16(body, 2)?)?;
            }
            0x0112 => {
                let fine = uef.high_cycles(u64::from(le16(body, 0)?));
                uef.builder.push(Tone::Silence, fine)?;
            }
            0x0113 => uef.set_base(float(body, 1000.0, BASES)?)?,
            0x0114 => uef.cycles(body)?,
            0x0116 => {
                let microseconds = float(body, 1_000_000.0, 0..=MAX_CYCLES / 2)?;
                uef.builder
                    .push(Tone::Silence, lasting(microseconds, 1, 1_000_000))?;
            }
            0x0117 => {
                let baud = u32::from(le16(body, 0)?);
                if !BAUDS.contains(&baud) {
                    return Err(TapeError::Malformed);
                }
                uef.baud = baud;
            }
            0x0120 => uef.marker(body)?,
            _ => {}
        }
    }
    // A base set after the last tone changes nothing.
    let end = uef.builder.end();
    uef.bases.retain(|base| base.at < end);
    Ok(Tape {
        spans: uef.builder.spans,
        markers: uef.markers,
        bases: uef.bases,
    })
}

/// A CSW image's pulse bytes, unpacked from its zlib stream as they are
/// read.
struct Pulses<'a> {
    packed: &'a [u8],
    inflater: Option<Box<zlib::Decoder>>,
    buffer: Box<[u8]>,
    /// The buffer's bytes the last refill produced, and those taken.
    filled: usize,
    at: usize,
    produced: usize,
}

impl Pulses<'_> {
    fn next(&mut self) -> Result<Option<u8>, TapeError> {
        let Some(inflater) = self.inflater.as_mut() else {
            let byte = self.packed.get(self.at).copied();
            self.at += 1;
            return Ok(byte);
        };
        while self.at == self.filled {
            if inflater.is_finished() {
                return Ok(None);
            }
            let progress = inflater
                .decompress(self.packed, &mut self.buffer)
                .map_err(|_| TapeError::Corrupt)?;
            self.packed = &self.packed[progress.consumed..];
            self.filled = progress.produced;
            self.at = 0;
            self.produced += progress.produced;
            if self.produced > MAX_IMAGE {
                return Err(TapeError::TooLong);
            }
            if progress.consumed == 0 && progress.produced == 0 && !inflater.is_finished() {
                return Err(TapeError::Truncated);
            }
        }
        self.at += 1;
        Ok(Some(self.buffer[self.at - 1]))
    }
}

fn decode_csw(bytes: &[u8]) -> Result<Tape, TapeError> {
    let major = *bytes.get(0x17).ok_or(TapeError::Truncated)?;
    let (rate, compression, flags, data_at) = match major {
        1 => (
            u32::from(le16(bytes, 0x19)?),
            *bytes.get(0x1B).ok_or(TapeError::Truncated)?,
            *bytes.get(0x1C).ok_or(TapeError::Truncated)?,
            0x20,
        ),
        2 => {
            let extension = usize::from(*bytes.get(0x23).ok_or(TapeError::Truncated)?);
            (
                le32(bytes, 0x19)?,
                *bytes.get(0x21).ok_or(TapeError::Truncated)?,
                *bytes.get(0x22).ok_or(TapeError::Truncated)?,
                0x34 + extension,
            )
        }
        _ => return Err(TapeError::Malformed),
    };
    if !(1_000..=1_000_000).contains(&rate) || flags & !1 != 0 {
        return Err(TapeError::Malformed);
    }
    let packed = bytes.get(data_at..).ok_or(TapeError::Truncated)?;
    let inflater = match compression {
        1 => None,
        2 if major == 2 => Some(Box::new(zlib::Decoder::new())),
        _ => return Err(TapeError::Malformed),
    };
    let mut pulses = Pulses {
        packed,
        inflater,
        buffer: vec![0; 1 << 14].into_boxed_slice(),
        filled: 0,
        at: 0,
        produced: 0,
    };
    let rate = u64::from(rate);
    let mut builder = Builder::new();
    while let Some(short) = pulses.next()? {
        let samples = if short == 0 {
            let mut long = [0; 4];
            for byte in &mut long {
                *byte = pulses.next()?.ok_or(TapeError::Truncated)?;
            }
            u64::from(u32::from_le_bytes(long))
        } else {
            u64::from(short)
        };
        let tone = if samples * HIGH_HALF_CYCLE_HZ <= rate {
            Tone::High
        } else if samples * LOW_TONE_HZ <= rate {
            Tone::Low
        } else {
            Tone::Silence
        };
        builder.push(tone, lasting(samples, 1, rate))?;
    }
    Ok(Tape {
        spans: builder.spans,
        markers: Vec::new(),
        bases: Vec::new(),
    })
}

crate::snapshot::fields!(
    Marker { at, label <= MAX_LABEL },
    check
        | marker
        | marker
            .label
            .chars()
            .all(|c| c != '\0' && u32::from(c) <= 0xFF)
);
crate::snapshot::fields!(
    Base { at, millihertz },
    check | base | BASES.contains(&base.millihertz)
);

impl crate::snapshot::Save for Tape {
    /// Each span as its length and its tone in one number, as short as the
    /// length allows.
    fn save(&self, w: &mut crate::snapshot::Writer) {
        w.put(&u32::try_from(self.spans.len()).unwrap_or(u32::MAX));
        let mut from = 0;
        for span in &self.spans {
            w.put(&crate::snapshot::Varint(
                (span.end() - from) << 2 | span.tone().code(),
            ));
            from = span.end();
        }
        w.put(&self.markers);
        w.put(&self.bases);
    }
}

impl crate::snapshot::Load for Tape {
    /// A tape the decoder could have made: no more spans than a tape holds,
    /// none empty, no two alike side by side, no longer than a tape; its
    /// markers in order along it; and each base change a real one, before
    /// the tape's end.
    fn load(r: &mut crate::snapshot::Reader<'_>) -> Result<Self, crate::snapshot::SnapshotError> {
        use crate::snapshot::{Reader, SnapshotError, Varint};
        let count = r.count_at_most(MAX_SPANS)?;
        let mut spans = Vec::with_capacity(count);
        let mut end = 0u64;
        let mut last = None;
        for _ in 0..count {
            let Varint(packed) = r.get()?;
            let (cycles, code) = (packed >> 2, packed & 0x03);
            Reader::check(code <= Tone::Silence.code() && cycles > 0)?;
            let tone = Tone::from_code(code);
            end = end.checked_add(cycles).ok_or(SnapshotError::Malformed)?;
            Reader::check(last != Some(tone) && end <= MAX_CYCLES)?;
            spans.push(Span::new(tone, end));
            last = Some(tone);
        }
        let markers: Vec<Marker> = r.get_at_most(MAX_MARKERS)?;
        Reader::check(
            markers.is_sorted_by_key(|marker| marker.at)
                && markers.last().is_none_or(|marker| marker.at <= end),
        )?;
        let bases: Vec<Base> = r.get_at_most(MAX_BASES)?;
        let mut before = Base {
            at: 0,
            millihertz: BASE_MILLIHERTZ,
        };
        for (index, base) in bases.iter().enumerate() {
            Reader::check(
                (index == 0 || base.at > before.at)
                    && base.at < end
                    && base.millihertz != before.millihertz,
            )?;
            before = *base;
        }
        Ok(Self {
            spans,
            markers,
            bases,
        })
    }
}

#[cfg(test)]
#[path = "tape_tests.rs"]
mod tests;
