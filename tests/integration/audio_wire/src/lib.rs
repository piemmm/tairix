//! The one definition both ends of the audio vertical agree on
//! (`plans/SOUND.md` SND4).
//!
//! The guest plays the [`sample`] ramp through the whole stack and QEMU's
//! `wav` audio backend writes what the emulated sound card received to a
//! file on the host; [`payload_matches_signal`] then checks that file holds
//! exactly those samples. Both halves read this crate, so "what was played"
//! and "what is asserted" cannot drift.
//!
//! # Why the run is deterministic rather than a race
//!
//! The ring is sized to hold the **whole** signal, so the guest writes every
//! frame *before* it starts the device, and the harness clocks the machine by
//! its instructions, so the device's clock stops whenever the guest's does.
//! The device can then never run dry however slowly the host runs the guest,
//! which is what lets the assertion be sample-exact and the reported
//! under-run tally be exactly zero — a glitch would be a real defect rather
//! than a slow host.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(feature = "flac")]
extern crate alloc;

use core::iter;
use core::sync::atomic::{AtomicBool, Ordering};

use tairix_abi::sound_cue::SoundEvent;

/// The rate the vertical runs at: every card's own, and the one a recorder
/// that may be told a rate is told.
pub const RATE_HZ: u32 = 48_000;

/// Interleaved channels. Stereo, so a channel swap or a stride error shows
/// up as a mismatch rather than as silence.
pub const CHANNELS: usize = 2;

/// Bytes one interleaved 16-bit stereo frame occupies.
pub const FRAME_BYTES: usize = CHANNELS * 2;

/// Frames the guest plays.
///
/// A quarter of a second, and comfortably inside [`RING_FRAMES`] so the whole
/// signal is queued before the device is started.
pub const SIGNAL_FRAMES: usize = 12_000;

/// The latency target the guest asks for, and therefore the ring it is
/// granted: a power of two holding the whole signal with room to spare, and
/// inside the PCM ring vocabulary's fixed ceiling.
pub const RING_FRAMES: u32 = 16_384;

/// Silent frames the guest plays after the signal, so the signal itself is
/// out of the host backend's buffer before the stream ends.
///
/// A backend writes its file a tick at a time and flushes the rest only
/// when it closes; a guest that ends the run itself never gives it that
/// chance, so without a tail the last tick of *signal* is lost. Padding
/// moves the loss onto silence, which the comparison trims anyway. Far
/// longer than any plausible tick, and sized with the signal to fit one
/// ring load so the whole stream is still queued before the device starts.
pub const SILENCE_TAIL_FRAMES: usize = 4_384;

/// Frames the guest queues in total: the signal then its silent tail.
pub const STREAM_FRAMES: usize = SIGNAL_FRAMES + SILENCE_TAIL_FRAMES;

const _: () = assert!(STREAM_FRAMES <= RING_FRAMES as usize);

/// The witness line the guest prints once the drain has completed and the
/// device has reported no lost frames.
pub const PASS_MARKER: &str = "AUDIO PASS";

/// The fixture's command word: the name its bundle installs under and the
/// `comm` its audited exit carries, so the image builder and the guest's
/// own finisher cannot name different programs.
pub const COMMAND: &str = "audiotone";

/// The argument that has the fixture play the seat run: pause on
/// [`HOLD_FRAME`], hand the seat over, play on while it is elsewhere, and take
/// it back.
pub const SEAT_ARG: &str = "seat";

/// The frame the seat run pauses on and is held at while the seat is
/// elsewhere. Mid-signal, so a resume that lost or repeated a frame lands on
/// a sample the comparison catches.
pub const HOLD_FRAME: usize = SIGNAL_FRAMES / 2;

/// The witness line the seat run prints once it was held at [`HOLD_FRAME`],
/// resumed on it, and drained with no lost frames.
pub const SEAT_PASS_MARKER: &str = "AUDIO SEAT PASS";

/// The sound player the `play` verticals run in the fixture's place.
pub const PLAYER: &str = "play";

/// The argument that has the fixture ask the cue authority for
/// [`CUE_EVENT`], answered once it has played out, and then play a silent
/// tail of its own so the cue has left the host backend's buffer before the
/// run ends.
pub const CUE_ARG: &str = "cue";

/// The desktop sound the cue run asks for: an application's, so the fixture
/// may, and longer than one ring, so the authority must refill it.
pub const CUE_EVENT: SoundEvent = SoundEvent::NotificationReminder;

/// The witness line the cue run prints once the cue played out and its tail
/// drained with no lost frames.
pub const CUE_PASS_MARKER: &str = "AUDIO CUE PASS";

/// The guest's verdict on the run, fed every audited process `exit`.
///
/// The run ends at the first exit that follows a player's: the host script
/// types the shell's own `exit` only once the guest has printed its pass, so a
/// player that failed never reaches it and the run times out instead. A
/// player's exits — its own and its decoder worker's — all count as one.
pub struct Finisher {
    played: AtomicBool,
}

impl Default for Finisher {
    fn default() -> Self {
        Self::new()
    }
}

impl Finisher {
    /// A run no player has ended yet.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            played: AtomicBool::new(false),
        }
    }

    /// Whether the run is over now that the process named `comm` exited.
    pub fn exited(&self, comm: &str) -> bool {
        if comm == COMMAND || comm == PLAYER {
            self.played.store(true, Ordering::Release);
            return false;
        }
        self.played.load(Ordering::Acquire)
    }
}

/// Samples quieter than this count as silence when the host trims the
/// backend's own lead-in and tail.
///
/// Exactly zero: the signal never reaches zero on both channels at once (see
/// [`sample`]), and QEMU's backend writes true zeroes before the device
/// starts, so the trim cannot eat a frame the guest played.
pub const SILENCE: i16 = 0;

/// The `frame`th sample of channel `channel`.
///
/// A pair of counter ramps in opposite directions: every frame differs from
/// its neighbours, the two channels differ from each other, and no frame is
/// silent on both channels — so a dropped frame, a channel swap, a stride
/// error and a truncation are each a visible mismatch rather than a
/// plausible-looking waveform.
///
/// The right ramp is offset by one so the two zero crossings fall on
/// different frames: a frame silent on *every* channel would look like the
/// host backend's own lead-in and be trimmed away, which
/// [`payload_matches_signal`] relies on not happening.
#[must_use]
pub fn sample(frame: usize, channel: usize) -> i16 {
    // Below the ramp's period, so the narrowing is total rather than
    // merely expected to hold.
    let step = i16::try_from(frame % 2_000).unwrap_or(0);
    if channel == 0 {
        step - 1_000
    } else {
        999 - step
    }
}

/// Write the whole signal, interleaved little-endian `s16`, into `out`.
///
/// Answers the bytes written, which is zero when `out` cannot hold
/// [`SIGNAL_FRAMES`] frames — the caller sizes its buffer from this crate, so
/// a short buffer is its own bug and is refused rather than truncated.
pub fn fill_signal(out: &mut [u8]) -> usize {
    let bytes = SIGNAL_FRAMES * FRAME_BYTES;
    if out.len() < bytes {
        return 0;
    }
    out[bytes..].fill(0);
    for frame in 0..SIGNAL_FRAMES {
        for channel in 0..CHANNELS {
            let at = (frame * CHANNELS + channel) * 2;
            out[at..at + 2].copy_from_slice(&sample(frame, channel).to_le_bytes());
        }
    }
    bytes
}

/// Where the audio verticals' disks plant the signal as a FLAC file, for
/// `play` to be run on.
pub const SIGNAL_FILE: &str = "/System/Audio/signal.flac";

/// The signal and its silent tail as a 16-bit FLAC stream at [`RATE_HZ`],
/// written by `lib/sound`'s encoder: the file the `play` verticals plant.
///
/// # Errors
///
/// The encoder's refusal, which the fixture's own geometry never draws.
#[cfg(feature = "flac")]
pub fn signal_flac() -> Result<alloc::vec::Vec<u8>, tairix_sound::flac_encode::EncodeError> {
    use tairix_sound::flac_encode::{encode, EncodeError, Options, Params};
    let mut pcm = alloc::vec![0u8; STREAM_FRAMES * FRAME_BYTES];
    fill_signal(&mut pcm);
    let samples: alloc::vec::Vec<i32> = pcm
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&bytes| i32::from(i16::from_le_bytes(bytes)))
        .collect();
    let params = Params {
        rate: RATE_HZ,
        channels: u8::try_from(CHANNELS).map_err(|_| EncodeError::Unsupported)?,
        bits: 16,
    };
    Ok(encode(params, &samples, Options::default())?.finish())
}

/// What the `play` vertical's shell line echoes once `play` exits zero.
pub const PLAY_PASS_MARKER: &str = "PLAY-PASS";

/// Bytes of the canonical header [`write_signal_wav`] writes.
pub const WAV_HEADER_LEN: usize = 44;

/// Bytes of the signal and its silent tail as a WAV file.
pub const SIGNAL_WAV_LEN: usize = WAV_HEADER_LEN + STREAM_FRAMES * FRAME_BYTES;

/// Write the signal and its silent tail as a canonical 16-bit PCM WAV file at
/// [`RATE_HZ`], the file a player is handed to play.
///
/// Answers the bytes written, which is zero when `out` cannot hold
/// [`SIGNAL_WAV_LEN`].
pub fn write_signal_wav(out: &mut [u8]) -> usize {
    let Some((header, samples)) = out
        .get_mut(..SIGNAL_WAV_LEN)
        .map(|file| file.split_at_mut(WAV_HEADER_LEN))
    else {
        return 0;
    };
    let (Ok(data), Ok(channels)) = (u32::try_from(samples.len()), u16::try_from(CHANNELS)) else {
        return 0;
    };
    header.copy_from_slice(&wav_header(RATE_HZ, channels, data));
    fill_signal(samples);
    SIGNAL_WAV_LEN
}

/// The canonical header of a 16-bit PCM WAV of `data` bytes, `channels`
/// interleaved at `rate_hz`.
#[must_use]
pub fn wav_header(rate_hz: u32, channels: u16, data: u32) -> [u8; WAV_HEADER_LEN] {
    let frame = channels.saturating_mul(2);
    let mut header = [0u8; WAV_HEADER_LEN];
    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&data.saturating_add(36).to_le_bytes());
    header[8..16].copy_from_slice(b"WAVEfmt ");
    header[16..20].copy_from_slice(&16u32.to_le_bytes());
    header[20..22].copy_from_slice(&1u16.to_le_bytes());
    header[22..24].copy_from_slice(&channels.to_le_bytes());
    header[24..28].copy_from_slice(&rate_hz.to_le_bytes());
    header[28..32].copy_from_slice(&rate_hz.saturating_mul(u32::from(frame)).to_le_bytes());
    header[32..34].copy_from_slice(&frame.to_le_bytes());
    header[34..36].copy_from_slice(&16u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&data.to_le_bytes());
    header
}

/// Why a captured WAV did not hold the signal.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WavMismatch {
    /// Shorter than a RIFF/WAVE header.
    TooShort,
    /// Not a RIFF/WAVE file at all.
    NotWave,
    /// The backend recorded a geometry the vertical did not ask for.
    WrongFormat {
        /// Channels the header declares.
        channels: u16,
        /// Rate the header declares.
        rate: u32,
        /// Bits per sample the header declares.
        bits: u16,
    },
    /// The file carries fewer frames than the guest played.
    Short {
        /// Non-silent frames found.
        frames: usize,
    },
    /// A frame differs from the one the guest played.
    Differs {
        /// The first differing frame, counted from the signal's start.
        frame: usize,
    },
}

/// Whole buffers an emulated sound card reports discarding on its own, each
/// `frames` frames long: what a card that drops audio when its host stalls
/// says it dropped, so a capture is held to everything else exactly.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ReportedLoss {
    /// Buffers discarded.
    pub buffers: usize,
    /// Frames each held.
    pub frames: usize,
}

impl ReportedLoss {
    /// A card that reported losing nothing.
    pub const NONE: Self = Self {
        buffers: 0,
        frames: 0,
    };
}

/// Check a host-captured WAV, whose header its recorder labels `label_hz`,
/// holds exactly the signal the guest played, apart from the whole buffers
/// `loss` says the card itself discarded.
///
/// The label is the recorder's and not always the stream's: a recorder that
/// may not be told a rate states its own default over samples it never
/// converted, and the sample comparison is what proves no conversion
/// happened.
///
/// The backend opens its file before the guest starts the device and closes
/// it after the device stops, so it brackets the recording with true silence.
/// That lead-in and tail are the *host* backend's, not the guest's, so they
/// are trimmed before the comparison; everything between them must be the
/// signal byte for byte.
///
/// # Errors
///
/// A [`WavMismatch`] naming what was wrong, so a failure says which frame
/// diverged rather than only that one did.
pub fn payload_matches_signal(
    wav: &[u8],
    label_hz: u32,
    loss: ReportedLoss,
) -> Result<(), WavMismatch> {
    matches(wav, label_hz, (0..SIGNAL_FRAMES).map(signal_frame), loss)
}

/// Check a host-captured WAV holds the signal the seat run played: every frame
/// once and in order, with the silence of the seat's absence allowed at
/// [`HOLD_FRAME`] and nowhere else.
///
/// # Errors
///
/// As [`payload_matches_signal`].
pub fn payload_matches_held_signal(wav: &[u8], label_hz: u32) -> Result<(), WavMismatch> {
    let held = (0..SIGNAL_FRAMES).flat_map(|frame| {
        (frame == HOLD_FRAME)
            .then_some(Expected::Pause)
            .into_iter()
            .chain(iter::once(signal_frame(frame)))
    });
    matches(wav, label_hz, held, ReportedLoss::NONE)
}

/// Check a host-captured WAV holds exactly `expected`, interleaved stereo
/// samples decoded independently of the guest — a shipped sound's master.
///
/// A master begins and ends in silence, which is indistinguishable from the
/// backend's own lead-in and tail, so both are trimmed from it as from the
/// capture: everything from its first sounding frame to its last must arrive
/// byte for byte, and nothing may sound after it.
///
/// # Errors
///
/// As [`payload_matches_signal`]; an `expected` that never sounds is
/// [`WavMismatch::Short`], since it proves nothing.
pub fn payload_matches_pcm(wav: &[u8], label_hz: u32, expected: &[i16]) -> Result<(), WavMismatch> {
    payload_matches_sequence(wav, label_hz, &[expected])
}

/// Check a host-captured WAV holds each of `sounds` in order, as
/// [`payload_matches_pcm`] holds one: each from its first sounding frame to
/// its last, apart only by silence, and nothing sounding after the last.
///
/// # Errors
///
/// As [`payload_matches_pcm`], for the first sound that does not match; a
/// frame is counted across every sound compared before it.
pub fn payload_matches_sequence(
    wav: &[u8],
    label_hz: u32,
    sounds: &[&[i16]],
) -> Result<(), WavMismatch> {
    if sounds.is_empty() || sounds.iter().any(|sound| sounding(sound).is_empty()) {
        return Err(WavMismatch::Short { frames: 0 });
    }
    let steps = sounds.iter().flat_map(|sound| {
        iter::once(Expected::Pause).chain(sounding(sound).iter().copied().map(Expected::Frame))
    });
    matches(wav, label_hz, steps, ReportedLoss::NONE)
}

/// `sound`'s frames from its first sounding one to its last, or none.
fn sounding(sound: &[i16]) -> &[[i16; CHANNELS]] {
    let (frames, _) = sound.as_chunks::<CHANNELS>();
    let silent = |frame: &[i16; CHANNELS]| frame.iter().all(|&sample| sample == SILENCE);
    let Some(first) = frames.iter().position(|frame| !silent(frame)) else {
        return &[];
    };
    let last = frames
        .iter()
        .rposition(|frame| !silent(frame))
        .unwrap_or(first);
    &frames[first..=last]
}

/// One step of what a capture must hold.
#[derive(Copy, Clone)]
enum Expected {
    /// Silence of any length may come here.
    Pause,
    /// This frame, exactly.
    Frame([i16; CHANNELS]),
}

/// The signal's `frame`th frame.
fn signal_frame(frame: usize) -> Expected {
    Expected::Frame(core::array::from_fn(|channel| sample(frame, channel)))
}

/// The comparison every check shares: `expected` in order after the backend's
/// lead-in, and nothing sounding after it. Where a frame differs or the
/// capture ends early, one of the buffers `loss` reports may account for it,
/// exactly as many frames as such a buffer holds.
fn matches(
    wav: &[u8],
    label_hz: u32,
    expected: impl IntoIterator<Item = Expected>,
    loss: ReportedLoss,
) -> Result<(), WavMismatch> {
    let (channels, rate, bits, payload) = parse_wav(wav)?;
    if usize::from(channels) != CHANNELS || rate != label_hz || bits != 16 {
        return Err(WavMismatch::WrongFormat {
            channels,
            rate,
            bits,
        });
    }
    let frames = payload.len() / FRAME_BYTES;
    let first = (0..frames)
        .find(|frame| !frame_is_silent(payload, *frame))
        .ok_or(WavMismatch::Short { frames: 0 })?;
    let last = (0..frames)
        .rev()
        .find(|frame| !frame_is_silent(payload, *frame))
        .unwrap_or(first);
    let mut recorded = first;
    let mut compared = 0;
    let mut buffers_left = loss.buffers;
    let mut steps = expected.into_iter();
    while let Some(step) = steps.next() {
        let Expected::Frame(frame) = step else {
            while recorded <= last && frame_is_silent(payload, recorded) {
                recorded += 1;
            }
            continue;
        };
        if recorded > last {
            let left = 1 + steps
                .filter(|step| matches!(step, Expected::Frame(_)))
                .count();
            if buffers_left > 0 && left <= loss.frames {
                return Ok(());
            }
            return Err(WavMismatch::Short { frames: compared });
        }
        let played = |frame: &[i16; CHANNELS]| {
            frame.iter().enumerate().all(|(channel, &sample)| {
                let at = (recorded * CHANNELS + channel) * 2;
                i16::from_le_bytes([payload[at], payload[at + 1]]) == sample
            })
        };
        if !played(&frame) {
            // The frames a buffers_left buffer held never arrived: what arrived in their
            // place is the frame after them.
            let after_loss = buffers_left > 0
                && loss.frames > 0
                && steps
                    .by_ref()
                    .filter_map(|step| match step {
                        Expected::Frame(frame) => Some(frame),
                        Expected::Pause => None,
                    })
                    .nth(loss.frames - 1)
                    .is_some_and(|after| played(&after));
            if !after_loss {
                return Err(WavMismatch::Differs { frame: compared });
            }
            buffers_left -= 1;
            compared += loss.frames;
        }
        recorded += 1;
        compared += 1;
    }
    // Past what the guest played the device must have received nothing: any
    // sound there means the stack manufactured frames.
    match (recorded..=last).position(|frame| !frame_is_silent(payload, frame)) {
        Some(extra) => Err(WavMismatch::Differs {
            frame: compared + extra,
        }),
        None => Ok(()),
    }
}

/// Whether every channel of `frame` is silent.
fn frame_is_silent(payload: &[u8], frame: usize) -> bool {
    (0..CHANNELS).all(|channel| {
        let at = (frame * CHANNELS + channel) * 2;
        payload
            .get(at..at + 2)
            .is_some_and(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]) == SILENCE)
    })
}

/// The `(channels, rate, bits, payload)` of a canonical RIFF/WAVE file.
fn parse_wav(wav: &[u8]) -> Result<(u16, u32, u16, &[u8]), WavMismatch> {
    if wav.len() < 44 {
        return Err(WavMismatch::TooShort);
    }
    if &wav[0..4] != b"RIFF" || &wav[8..12] != b"WAVE" {
        return Err(WavMismatch::NotWave);
    }
    let mut at = 12;
    let mut format = None;
    while at + 8 <= wav.len() {
        let id = &wav[at..at + 4];
        let len = u32::from_le_bytes([wav[at + 4], wav[at + 5], wav[at + 6], wav[at + 7]]) as usize;
        let body = at + 8;
        let end = body.saturating_add(len).min(wav.len());
        if id == b"fmt " && len >= 16 {
            format = Some((
                u16::from_le_bytes([wav[body + 2], wav[body + 3]]),
                u32::from_le_bytes([wav[body + 4], wav[body + 5], wav[body + 6], wav[body + 7]]),
                u16::from_le_bytes([wav[body + 14], wav[body + 15]]),
            ));
        }
        if id == b"data" {
            let (channels, rate, bits) = format.ok_or(WavMismatch::NotWave)?;
            // A writer patches this length in when it closes the file. A
            // zero means it never got to: the samples are there, only the
            // header never caught up, so the payload is the rest of the
            // file. QEMU's backend leaves exactly this behind whenever the
            // guest ends the run itself.
            let end = if len == 0 { wav.len() } else { end };
            return Ok((channels, rate, bits, &wav[body..end]));
        }
        // Chunks are padded to an even length.
        at = body + len + (len & 1);
    }
    Err(WavMismatch::TooShort)
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate alloc;
    use alloc::vec;
    use alloc::vec::Vec;

    /// A canonical 48 kHz stereo 16-bit WAV around `payload`.
    fn wav(payload: &[u8]) -> Vec<u8> {
        let mut out = wav_header(RATE_HZ, 2, u32::try_from(payload.len()).unwrap_or(0)).to_vec();
        out.extend_from_slice(payload);
        out
    }

    fn signal_bytes() -> Vec<u8> {
        let mut bytes = vec![0u8; SIGNAL_FRAMES * FRAME_BYTES];
        assert_eq!(fill_signal(&mut bytes), bytes.len());
        bytes
    }

    #[test]
    fn the_signal_is_accepted_with_or_without_the_backends_own_silence() {
        let bare = wav(&signal_bytes());
        assert_eq!(
            payload_matches_signal(&bare, RATE_HZ, ReportedLoss::NONE),
            Ok(())
        );
        let mut padded = vec![0u8; 64 * FRAME_BYTES];
        padded.extend_from_slice(&signal_bytes());
        padded.extend(core::iter::repeat_n(0u8, 32 * FRAME_BYTES));
        assert_eq!(
            payload_matches_signal(&wav(&padded), RATE_HZ, ReportedLoss::NONE),
            Ok(())
        );
    }

    /// A master with its own silent lead-in and tail, as every shipped sound
    /// is authored.
    fn master() -> Vec<i16> {
        let mut samples = vec![0i16; 2 * 30];
        samples.extend((1..400).flat_map(|n| [n, -n]));
        // Silent on both channels mid-sound, which the comparison must keep.
        samples.extend([0, 0]);
        samples.extend([7, 0, 0, -7]);
        samples.extend(vec![0i16; 2 * 12]);
        samples
    }

    fn le(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn a_master_is_matched_from_its_first_sound_to_its_last() {
        let master = master();
        let sounding = &master[2 * 30..master.len() - 2 * 12];
        // The backend's lead-in and a tail that lost the master's own
        // trailing silence still hold the master.
        let mut payload = vec![0u8; 100 * FRAME_BYTES];
        payload.extend(le(sounding));
        payload.extend(vec![0u8; 3 * FRAME_BYTES]);
        assert_eq!(
            payload_matches_pcm(&wav(&payload), RATE_HZ, &master),
            Ok(())
        );

        let mut altered = payload.clone();
        altered[(100 + 50) * FRAME_BYTES] ^= 1;
        assert_eq!(
            payload_matches_pcm(&wav(&altered), RATE_HZ, &master),
            Err(WavMismatch::Differs { frame: 50 })
        );
        let mut trailing = payload.clone();
        trailing.extend(le(&[0, 3]));
        assert!(matches!(
            payload_matches_pcm(&wav(&trailing), RATE_HZ, &master),
            Err(WavMismatch::Differs { .. })
        ));
        let cut = &payload[..payload.len() - 6 * FRAME_BYTES];
        assert!(matches!(
            payload_matches_pcm(&wav(cut), RATE_HZ, &master),
            Err(WavMismatch::Short { .. })
        ));
        assert_eq!(
            payload_matches_pcm(&wav(&payload), RATE_HZ, &[0; 8]),
            Err(WavMismatch::Short { frames: 0 }),
            "a master that never sounds proves nothing"
        );
    }

    #[test]
    fn a_sequence_is_matched_in_order_apart_only_by_silence() {
        let first = master();
        let second: Vec<i16> = [0, 0]
            .into_iter()
            .chain((1..200).flat_map(|n| [-n, n]))
            .chain([0, 0])
            .collect();
        let first_frames = sounding(&first).len();
        let capture = |sounds: &[&[i16]], apart: usize| {
            let mut payload = vec![0u8; 40 * FRAME_BYTES];
            for sound in sounds {
                payload.extend(le(sounding(sound).as_flattened()));
                payload.extend(vec![0u8; apart * FRAME_BYTES]);
            }
            wav(&payload)
        };
        for apart in [0, 900] {
            assert_eq!(
                payload_matches_sequence(
                    &capture(&[&first, &second], apart),
                    RATE_HZ,
                    &[&first, &second]
                ),
                Ok(()),
                "{apart} silent frames between"
            );
        }
        assert_eq!(
            payload_matches_sequence(
                &capture(&[&second, &first], 900),
                RATE_HZ,
                &[&first, &second]
            ),
            Err(WavMismatch::Differs { frame: 0 })
        );
        assert_eq!(
            payload_matches_sequence(&capture(&[&first], 900), RATE_HZ, &[&first, &second]),
            Err(WavMismatch::Short {
                frames: first_frames
            })
        );
        assert_eq!(
            payload_matches_sequence(&capture(&[&first, &second], 900), RATE_HZ, &[&first]),
            Err(WavMismatch::Differs {
                frame: first_frames + 900
            }),
            "a sound after the last expected one is manufactured"
        );
        assert_eq!(
            payload_matches_sequence(&capture(&[&first], 0), RATE_HZ, &[]),
            Err(WavMismatch::Short { frames: 0 })
        );
    }

    #[test]
    fn one_altered_frame_is_named_rather_than_tolerated() {
        let mut bytes = signal_bytes();
        let at = 500 * FRAME_BYTES;
        bytes[at] ^= 0x01;
        assert_eq!(
            payload_matches_signal(&wav(&bytes), RATE_HZ, ReportedLoss::NONE),
            Err(WavMismatch::Differs { frame: 500 })
        );
    }

    /// A buffer the card reports discarding accounts for exactly its frames,
    /// once, wherever it fell — the tail included — and for nothing else.
    #[test]
    fn a_reported_loss_accounts_for_its_own_frames_and_no_others() {
        const BUFFER: usize = 2048;
        let one = ReportedLoss {
            buffers: 1,
            frames: BUFFER,
        };
        let lose = |at: usize, frames: usize| {
            let mut bytes = signal_bytes();
            bytes.drain(at * FRAME_BYTES..(at + frames) * FRAME_BYTES);
            wav(&bytes)
        };
        let gapped = lose(7757, BUFFER);
        assert_eq!(payload_matches_signal(&gapped, RATE_HZ, one), Ok(()));
        assert_eq!(
            payload_matches_signal(&gapped, RATE_HZ, ReportedLoss::NONE),
            Err(WavMismatch::Differs { frame: 7757 }),
            "an unreported loss is a mismatch"
        );
        // The ramp repeats every 2000 frames, so a gap of 48 looks like one of
        // 2048 where it falls; the rest of the capture tells them apart.
        assert!(payload_matches_signal(&lose(7757, 48), RATE_HZ, one).is_err());
        assert!(payload_matches_signal(&lose(7757, BUFFER - 1), RATE_HZ, one).is_err());
        let mut twice = signal_bytes();
        twice.drain(9000 * FRAME_BYTES..(9000 + BUFFER) * FRAME_BYTES);
        twice.drain(1000 * FRAME_BYTES..(1000 + BUFFER) * FRAME_BYTES);
        assert!(payload_matches_signal(&wav(&twice), RATE_HZ, one).is_err());
        let two = ReportedLoss { buffers: 2, ..one };
        assert_eq!(payload_matches_signal(&wav(&twice), RATE_HZ, two), Ok(()));
        let tail = lose(SIGNAL_FRAMES - 1000, 1000);
        assert_eq!(payload_matches_signal(&tail, RATE_HZ, one), Ok(()));
        let mut altered = signal_bytes();
        altered[500 * FRAME_BYTES] ^= 1;
        assert_eq!(
            payload_matches_signal(&wav(&altered), RATE_HZ, one),
            Err(WavMismatch::Differs { frame: 500 }),
            "a corrupt frame is no lost buffer"
        );
    }

    #[test]
    fn the_header_must_carry_the_label_its_recorder_states() {
        let mut labelled = wav(&signal_bytes());
        labelled[24..28].copy_from_slice(&44_100u32.to_le_bytes());
        assert_eq!(
            payload_matches_signal(&labelled, 44_100, ReportedLoss::NONE),
            Ok(())
        );
        assert_eq!(
            payload_matches_signal(&labelled, RATE_HZ, ReportedLoss::NONE),
            Err(WavMismatch::WrongFormat {
                channels: 2,
                rate: 44_100,
                bits: 16
            })
        );
    }

    #[test]
    fn a_truncated_or_mis_shaped_capture_is_refused() {
        let short = &signal_bytes()[..1_000 * FRAME_BYTES];
        assert!(matches!(
            payload_matches_signal(&wav(short), RATE_HZ, ReportedLoss::NONE),
            Err(WavMismatch::Short { .. })
        ));
        assert_eq!(
            payload_matches_signal(b"not a wav", RATE_HZ, ReportedLoss::NONE),
            Err(WavMismatch::TooShort)
        );
        assert_eq!(
            payload_matches_signal(&[0u8; 64], RATE_HZ, ReportedLoss::NONE),
            Err(WavMismatch::NotWave)
        );
    }

    #[test]
    fn a_capture_whose_silent_tail_was_never_flushed_still_matches() {
        // What the host loses is the backend's last buffered tick. With a
        // silent tail that loss falls on silence, and the signal is whole.
        let mut stream = vec![0u8; STREAM_FRAMES * FRAME_BYTES];
        assert_eq!(fill_signal(&mut stream), SIGNAL_FRAMES * FRAME_BYTES);
        let kept = (SIGNAL_FRAMES + 8) * FRAME_BYTES;
        let wav = wav(&stream[..kept]);

        assert_eq!(
            payload_matches_signal(&wav, RATE_HZ, ReportedLoss::NONE),
            Ok(())
        );
    }

    #[test]
    fn an_unfinalised_capture_is_read_to_the_end_of_the_file() {
        // A writer killed before it closed the file leaves both size fields
        // zero; the samples are still there, and refusing to read them
        // reports silence where there was sound.
        let mut payload = vec![0u8; SIGNAL_FRAMES * FRAME_BYTES];
        assert_eq!(fill_signal(&mut payload), payload.len());
        let mut wav = wav(&payload);
        wav[4..8].copy_from_slice(&0u32.to_le_bytes());
        let data_len = wav.len() - payload.len() - 4;
        wav[data_len..data_len + 4].copy_from_slice(&0u32.to_le_bytes());

        assert_eq!(
            payload_matches_signal(&wav, RATE_HZ, ReportedLoss::NONE),
            Ok(())
        );
    }

    #[test]
    fn no_frame_of_the_signal_is_silent_on_both_channels() {
        // The host trim relies on this: a frame the guest played must never
        // look like the backend's own lead-in.
        for frame in 0..SIGNAL_FRAMES {
            assert!(
                (0..CHANNELS).any(|channel| sample(frame, channel) != SILENCE),
                "frame {frame} is silent on every channel"
            );
        }
    }

    #[test]
    fn the_signal_file_holds_the_signal_and_its_silent_tail() {
        let mut file = vec![0u8; SIGNAL_WAV_LEN];
        assert_eq!(write_signal_wav(&mut file), SIGNAL_WAV_LEN);
        assert_eq!(
            payload_matches_signal(&file, RATE_HZ, ReportedLoss::NONE),
            Ok(())
        );
        assert_eq!(write_signal_wav(&mut file[..SIGNAL_WAV_LEN - 1]), 0);
    }

    /// The signal with `gap` silent frames before frame `at`.
    fn held(at: usize, gap: usize) -> Vec<u8> {
        let signal = signal_bytes();
        let mut out = signal[..at * FRAME_BYTES].to_vec();
        out.extend(core::iter::repeat_n(0u8, gap * FRAME_BYTES));
        out.extend_from_slice(&signal[at * FRAME_BYTES..]);
        out
    }

    #[test]
    fn a_held_signal_matches_with_its_gap_at_the_hold_or_with_none() {
        for gap in [0, 1, 4_800] {
            assert_eq!(
                payload_matches_held_signal(&wav(&held(HOLD_FRAME, gap)), RATE_HZ),
                Ok(()),
                "{gap} silent frames"
            );
        }
    }

    /// Silence anywhere but the hold is a glitch, and a resume that lost or
    /// repeated a frame is a mismatch at the frame it should have played.
    #[test]
    fn a_held_signal_refuses_a_gap_elsewhere_and_an_inexact_resume() {
        assert_eq!(
            payload_matches_held_signal(&wav(&held(HOLD_FRAME - 1, 10)), RATE_HZ),
            Err(WavMismatch::Differs {
                frame: HOLD_FRAME - 1
            })
        );
        let signal = signal_bytes();
        let resume = HOLD_FRAME * FRAME_BYTES;
        let mut lost = signal[..resume].to_vec();
        lost.extend(core::iter::repeat_n(0u8, 100 * FRAME_BYTES));
        lost.extend_from_slice(&signal[resume + FRAME_BYTES..]);
        assert_eq!(
            payload_matches_held_signal(&wav(&lost), RATE_HZ),
            Err(WavMismatch::Differs { frame: HOLD_FRAME })
        );
        let mut repeated = signal[..resume].to_vec();
        repeated.extend(core::iter::repeat_n(0u8, 100 * FRAME_BYTES));
        repeated.extend_from_slice(&signal[resume - FRAME_BYTES..]);
        assert_eq!(
            payload_matches_held_signal(&wav(&repeated), RATE_HZ),
            Err(WavMismatch::Differs { frame: HOLD_FRAME })
        );
        assert_eq!(
            payload_matches_signal(&wav(&held(HOLD_FRAME, 10)), RATE_HZ, ReportedLoss::NONE),
            Err(WavMismatch::Differs { frame: HOLD_FRAME }),
            "the plain check allows no gap at all"
        );
    }

    #[test]
    fn a_run_ends_at_the_first_exit_after_a_players() {
        let finisher = Finisher::new();
        assert!(!finisher.exited("elsh"), "the shell alone ends nothing");
        assert!(!finisher.exited(PLAYER));
        assert!(
            !finisher.exited(PLAYER),
            "the decoder worker is the player too"
        );
        assert!(finisher.exited("elsh"));
        let tone = Finisher::new();
        assert!(!tone.exited(COMMAND));
        assert!(tone.exited("elsh"));
    }
}
