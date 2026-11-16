//! Commodore cassette tapes: the datasette's read head hears each pulse the
//! KERNAL or a turbo loader recorded, and a tape is the lengths between the
//! falling edges it gives CIA1's FLAG.
//!
//! TAP images (`C64-TAPE-RAW`) hold those lengths a byte each in units of
//! eight cycles of the C64's clock; a zero is, in version 0, a pulse too
//! long to count, and in version 1 the escape to the next three bytes, the
//! pulse's exact length in cycles. A tape encodes back as version 1, and
//! holds no more than a version 1 image of [`MAX_IMAGE`] bytes does.

use alloc::vec::Vec;

use crate::machine::c64::Standard;
use crate::snapshot::{self, Reader};

/// The signature an image opens with.
const SIGNATURE: &[u8; 12] = b"C64-TAPE-RAW";
/// The header: signature, version, platform, video standard, a spare
/// byte, and the length of the pulses that follow.
const HEADER: usize = 20;
/// The header's platform byte for the Commodore 64.
const C64_PLATFORM: u8 = 0;
/// The header's video standard byte for each standard: the old NTSC chips
/// and PAL-N are timed by the NTSC machine's clock, the nearest.
const PAL: u8 = 0;
const NTSC: u8 = 1;
const OLD_NTSC: u8 = 2;
const PAL_N: u8 = 3;
/// The cycles a version-0 overflow stands for.
const OVERFLOW_CYCLES: u32 = 256 * 8;
/// The largest version 1 image a tape is: some twenty minutes of the
/// KERNAL's recording.
pub const MAX_IMAGE: usize = 4 << 20;
/// The most bytes of pulses a tape holds.
const MAX_DATA: usize = MAX_IMAGE - HEADER;
/// The most pulses a tape holds, each at least a byte of its image.
pub const MAX_PULSES: usize = MAX_DATA;
/// The longest pulse, the most a version 1 escape's three bytes hold: some
/// seventeen seconds of silence. A longer silence is kept as the longest.
pub const MAX_PULSE: u32 = 0xFF_FFFF;

/// `pulse` in units of eight cycles, to the nearest, as every loader's
/// timing allows for, where a version 1 image's byte holds it; else it is
/// written exactly after an escape.
fn units(pulse: u32) -> Option<u8> {
    u8::try_from((u64::from(pulse) + 4) / 8)
        .ok()
        .filter(|&units| units != 0)
}

/// The bytes `pulses` take in a version 1 image.
fn encoded_len(pulses: &[u32]) -> usize {
    pulses
        .iter()
        .map(|&pulse| if units(pulse).is_some() { 1 } else { 4 })
        .sum()
}

/// Why an image was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TapError {
    /// Not a C64's TAP image: its signature, version or platform is wrong.
    BadSignature,
    /// Its pulses run past the image or encode past [`MAX_IMAGE`], or its
    /// video standard is none the format names.
    Malformed,
}

/// A tape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tape {
    /// Each pulse's length in cycles of the clock of the tape's standard.
    pulses: Vec<u32>,
    /// The bytes they take in a version 1 image, at most [`MAX_DATA`].
    data: usize,
    /// The standard whose clock the pulses are counted in.
    standard: Standard,
    /// Recorded on since loaded or saved.
    dirty: bool,
}

impl Tape {
    /// An empty tape at the PAL clock.
    #[must_use]
    pub const fn blank() -> Self {
        Self {
            pulses: Vec::new(),
            data: 0,
            standard: Standard::Pal,
            dirty: false,
        }
    }

    /// The pulses, in cycles of [`Self::clock`].
    #[must_use]
    pub fn pulses(&self) -> &[u32] {
        &self.pulses
    }

    /// The cycles a second the pulses are counted in.
    #[must_use]
    pub const fn clock(&self) -> u64 {
        self.standard.clock()
    }

    /// Whether the tape has been recorded on since it was loaded or saved.
    #[must_use]
    pub const fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Replace the pulses from pulse `from` with `recorded`, as far as they
    /// reach: pulses they cover are lost, the first after them shortened by
    /// what they overran into it. Each is kept within 1 and [`MAX_PULSE`],
    /// and what would take the tape past [`MAX_IMAGE`] is not kept.
    pub fn record(&mut self, from: usize, recorded: &[u32]) {
        let from = from.min(self.pulses.len());
        let mut over: u64 = recorded.iter().map(|&p| u64::from(p)).sum();
        let mut end = from;
        let mut cut = None;
        while end < self.pulses.len() && over > 0 {
            let pulse = u64::from(self.pulses[end]);
            // What is left of a pulse under half a unit no loader hears.
            if pulse > over + 3 {
                cut = Some(u32::try_from(pulse - over).unwrap_or(MAX_PULSE));
                over = 0;
            } else {
                over = over.saturating_sub(pulse);
                end += 1;
            }
        }
        let mut freed = encoded_len(&self.pulses[from..end]);
        if let Some(cut) = cut {
            freed += encoded_len(&self.pulses[end..=end]);
            self.pulses[end] = cut;
            self.data += encoded_len(&[cut]);
        }
        self.data -= freed;
        let mut kept = 0;
        for &pulse in recorded {
            let bytes = encoded_len(&[pulse.clamp(1, MAX_PULSE)]);
            if self.data + bytes > MAX_DATA {
                break;
            }
            self.data += bytes;
            kept += 1;
        }
        self.pulses.splice(
            from..end,
            recorded[..kept]
                .iter()
                .map(|&pulse| pulse.clamp(1, MAX_PULSE)),
        );
        self.dirty = true;
    }

    /// The tape as a version 1 TAP image.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut data = Vec::with_capacity(self.data);
        for &pulse in &self.pulses {
            if let Some(units) = units(pulse) {
                data.push(units);
            } else {
                data.push(0);
                data.extend(&pulse.to_le_bytes()[..3]);
            }
        }
        let mut image = Vec::with_capacity(HEADER + data.len());
        image.extend_from_slice(SIGNATURE);
        image.push(1);
        image.push(0);
        image.push(match self.standard {
            Standard::Pal => PAL,
            Standard::Ntsc => NTSC,
        });
        image.push(0);
        image.extend(u32::try_from(data.len()).unwrap_or(u32::MAX).to_le_bytes());
        image.extend(data);
        image
    }
}

/// A TAP image's tape.
///
/// # Errors
///
/// [`TapError::BadSignature`] for another signature, a version past 1 or
/// another machine's tape, and [`TapError::Malformed`] for pulses past the
/// image or the bounds, or a video standard the format does not name.
pub fn decode(image: &[u8]) -> Result<Tape, TapError> {
    if image.get(..SIGNATURE.len()) != Some(SIGNATURE) {
        return Err(TapError::BadSignature);
    }
    let header = image.get(..HEADER).ok_or(TapError::Malformed)?;
    let version = header[12];
    if version > 1 {
        return Err(TapError::BadSignature);
    }
    if header[13] != C64_PLATFORM {
        return Err(TapError::BadSignature);
    }
    let standard = match header[14] {
        PAL => Standard::Pal,
        NTSC | OLD_NTSC | PAL_N => Standard::Ntsc,
        _ => return Err(TapError::Malformed),
    };
    let declared = u32::from_le_bytes([header[16], header[17], header[18], header[19]]);
    let declared = usize::try_from(declared).map_err(|_| TapError::Malformed)?;
    let data = image
        .get(HEADER..HEADER + declared)
        .ok_or(TapError::Malformed)?;
    let mut pulses = Vec::new();
    let mut encoded = 0;
    let mut at = 0;
    while at < data.len() {
        let byte = data[at];
        at += 1;
        let pulse = match (byte, version) {
            (0, 0) => OVERFLOW_CYCLES,
            (0, _) => {
                let long = data.get(at..at + 3).ok_or(TapError::Malformed)?;
                at += 3;
                u32::from_le_bytes([long[0], long[1], long[2], 0]).clamp(1, MAX_PULSE)
            }
            (units, _) => u32::from(units) * 8,
        };
        encoded += encoded_len(&[pulse]);
        if encoded > MAX_DATA {
            return Err(TapError::Malformed);
        }
        pulses.push(pulse);
    }
    Ok(Tape {
        pulses,
        data: encoded,
        standard,
        dirty: false,
    })
}

impl snapshot::Save for Tape {
    fn save(&self, w: &mut snapshot::Writer) {
        w.put(&self.pulses);
        w.put(&self.standard);
        w.put(&self.dirty);
    }
}

impl snapshot::Load for Tape {
    fn load(r: &mut Reader<'_>) -> Result<Self, snapshot::SnapshotError> {
        let pulses: Vec<u32> = r.get_at_most(MAX_PULSES)?;
        let data = encoded_len(&pulses);
        Reader::check(
            data <= MAX_DATA && pulses.iter().all(|pulse| (1..=MAX_PULSE).contains(pulse)),
        )?;
        Ok(Self {
            pulses,
            data,
            standard: r.get()?,
            dirty: r.get()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn image(version: u8, data: &[u8]) -> Vec<u8> {
        let mut image = Vec::from(&SIGNATURE[..]);
        image.extend([version, 0, 0, 0]);
        image.extend(u32::try_from(data.len()).expect("short").to_le_bytes());
        image.extend_from_slice(data);
        image
    }

    #[test]
    fn pulses_are_eight_cycle_units_and_version_1_escapes_long_ones() {
        let tape = decode(&image(1, &[0x30, 0x42, 0, 0x10, 0x27, 0x00, 0x56])).expect("a TAP");
        assert_eq!(tape.pulses(), [0x180, 0x210, 0x2710, 0x2B0]);
        let tape = decode(&image(0, &[0x30, 0])).expect("a version 0 TAP");
        assert_eq!(tape.pulses(), [0x180, OVERFLOW_CYCLES]);
    }

    #[test]
    fn another_machine_s_tape_or_an_unknown_standard_is_refused() {
        let mut vic20 = image(1, &[0x30]);
        vic20[13] = 1;
        assert_eq!(decode(&vic20).map(|_| ()), Err(TapError::BadSignature));
        let mut unknown = image(1, &[0x30]);
        unknown[14] = 4;
        assert_eq!(decode(&unknown).map(|_| ()), Err(TapError::Malformed));
        let mut old = image(1, &[0x30]);
        old[14] = OLD_NTSC;
        assert_eq!(
            decode(&old).expect("an old NTSC tape").clock(),
            Standard::Ntsc.clock()
        );
    }

    #[test]
    fn an_ntsc_tape_counts_the_ntsc_machine_s_clock() {
        let mut ntsc = image(1, &[0x30]);
        ntsc[14] = NTSC;
        let tape = decode(&ntsc).expect("a TAP");
        assert_eq!(tape.clock(), Standard::Ntsc.clock());
        assert_eq!(tape.encode(), ntsc, "and saves as NTSC again");
        assert_eq!(
            decode(&image(1, &[0x30])).expect("a TAP").clock(),
            Standard::Pal.clock()
        );
    }

    #[test]
    fn a_tape_saves_as_it_loaded() {
        let original = image(1, &[0x30, 0x42, 0, 0x10, 0x27, 0x00, 0x56]);
        let tape = decode(&original).expect("a TAP");
        assert_eq!(tape.encode(), original);
    }

    #[test]
    fn recording_replaces_the_pulses_it_covers() {
        let mut tape = decode(&image(1, &[10, 10, 10, 10])).expect("a TAP");
        tape.record(1, &[100, 100]);
        assert_eq!(
            tape.pulses(),
            [80, 100, 100, 40],
            "160 cycles over 200, the last cut short"
        );
        assert!(tape.is_dirty());
        tape.record(4, &[24]);
        assert_eq!(
            tape.pulses(),
            [80, 100, 100, 40, 24],
            "past the end, appended"
        );
        tape.record(0, &[78]);
        assert_eq!(
            tape.pulses(),
            [78, 100, 100, 40, 24],
            "two cycles left of a pulse go with it"
        );
    }

    /// The regression: recording grew a tape without bound, so a long
    /// recording could no longer be handed out of the machine as an image.
    #[test]
    fn a_tape_holds_no_more_than_its_largest_image() {
        assert!(
            decode(&image(1, &vec![0x30; MAX_DATA])).is_ok(),
            "the largest"
        );
        assert_eq!(
            decode(&image(1, &vec![0x30; MAX_DATA + 1])),
            Err(TapError::Malformed),
            "a byte past"
        );
        let mut tape = decode(&image(1, &vec![0x30; MAX_DATA - 2])).expect("two bytes short");
        tape.record(MAX_DATA - 2, &[0x180, 0x180, 0x180]);
        assert_eq!(tape.pulses().len(), MAX_DATA, "two of three fit");
        tape.record(MAX_DATA, &[0x180]);
        assert_eq!(tape.pulses().len(), MAX_DATA, "nothing more is kept");
        tape.record(0, &[0x180 * 5]);
        assert_eq!(
            tape.pulses().len(),
            MAX_DATA - 4,
            "one pulse over five frees their room"
        );
        assert_eq!(tape.encode().len(), MAX_IMAGE - 4);
        assert_eq!(
            decode(&image(0, &vec![0; MAX_DATA / 4 + 1])),
            Err(TapError::Malformed),
            "version 0 overflows each take four bytes again"
        );
    }

    /// The regression: a pulse past three bytes was written cut to its low
    /// three, and one near the top of its width overflowed reckoning its
    /// size.
    #[test]
    fn a_pulse_past_the_longest_is_kept_as_the_longest() {
        let mut tape = Tape::blank();
        tape.record(0, &[u32::MAX, MAX_PULSE + 1, 0]);
        assert_eq!(tape.pulses(), [MAX_PULSE, MAX_PULSE, 1]);
        let again = decode(&tape.encode()).expect("its own image");
        assert_eq!(again.pulses(), tape.pulses(), "written whole");
    }

    #[test]
    fn a_tape_restores_from_a_snapshot_as_it_was() {
        let mut tape = decode(&image(1, &[0x30, 0, 0x10, 0x27, 0x00])).expect("a TAP");
        tape.record(2, &[64]);
        let mut w = snapshot::Writer::new();
        w.put(&tape);
        let body = w.finish().expect("small");
        let opened = snapshot::open(&body).expect("opens");
        assert_eq!(Reader::new(&opened).get::<Tape>(), Ok(tape));
        let mut w = snapshot::Writer::new();
        w.put(&vec![0u32]);
        w.put(&Standard::Pal);
        w.put(&false);
        let opened = snapshot::open(&w.finish().expect("small")).expect("opens");
        assert_eq!(
            Reader::new(&opened).get::<Tape>(),
            Err(snapshot::SnapshotError::Malformed),
            "a pulse of no length"
        );
    }

    #[test]
    fn malformed_images_are_refused() {
        assert_eq!(
            decode(b"C64-TAPE-RAX\x01\0\0\0\0\0\0\0"),
            Err(TapError::BadSignature)
        );
        assert_eq!(decode(&image(2, &[1])), Err(TapError::BadSignature));
        let mut short = image(1, &[1, 2, 3]);
        short.truncate(short.len() - 1);
        assert_eq!(
            decode(&short),
            Err(TapError::Malformed),
            "fewer bytes than declared"
        );
        assert_eq!(
            decode(&image(1, &[0, 1])),
            Err(TapError::Malformed),
            "an escape cut short"
        );
    }
}
