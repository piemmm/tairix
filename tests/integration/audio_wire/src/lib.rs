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
//! frame *before* it starts the device. The device can then never run dry
//! however slowly the emulated machine runs, which is what lets the
//! assertion be sample-exact and the reported under-run tally be exactly
//! zero — a glitch would be a real defect rather than a slow host.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(feature = "flac")]
extern crate alloc;

use core::sync::atomic::{AtomicBool, Ordering};

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

/// Check a host-captured WAV, whose header its recorder labels `label_hz`,
/// holds exactly the signal the guest played.
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
pub fn payload_matches_signal(wav: &[u8], label_hz: u32) -> Result<(), WavMismatch> {
    matches_signal(wav, label_hz, None)
}

/// Check a host-captured WAV holds the signal the seat run played: every frame
/// once and in order, with the silence of the seat's absence allowed at
/// [`HOLD_FRAME`] and nowhere else.
///
/// # Errors
///
/// As [`payload_matches_signal`].
pub fn payload_matches_held_signal(wav: &[u8], label_hz: u32) -> Result<(), WavMismatch> {
    matches_signal(wav, label_hz, Some(HOLD_FRAME))
}

/// The comparison both checks share, allowing silence before the signal
/// frame `gap_at` where one is named.
fn matches_signal(wav: &[u8], label_hz: u32, gap_at: Option<usize>) -> Result<(), WavMismatch> {
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
    for frame in 0..SIGNAL_FRAMES {
        if gap_at == Some(frame) {
            while recorded <= last && frame_is_silent(payload, recorded) {
                recorded += 1;
            }
        }
        if recorded > last {
            return Err(WavMismatch::Short { frames: frame });
        }
        let played = (0..CHANNELS).all(|channel| {
            let at = (recorded * CHANNELS + channel) * 2;
            i16::from_le_bytes([payload[at], payload[at + 1]]) == sample(frame, channel)
        });
        if !played {
            return Err(WavMismatch::Differs { frame });
        }
        recorded += 1;
    }
    // Past the signal the guest played nothing, so the device must have
    // received nothing either: any sound there means the stack manufactured
    // frames.
    match (recorded..=last).position(|frame| !frame_is_silent(payload, frame)) {
        Some(extra) => Err(WavMismatch::Differs {
            frame: SIGNAL_FRAMES + extra,
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
        assert_eq!(payload_matches_signal(&bare, RATE_HZ), Ok(()));
        let mut padded = vec![0u8; 64 * FRAME_BYTES];
        padded.extend_from_slice(&signal_bytes());
        padded.extend(core::iter::repeat_n(0u8, 32 * FRAME_BYTES));
        assert_eq!(payload_matches_signal(&wav(&padded), RATE_HZ), Ok(()));
    }

    #[test]
    fn one_altered_frame_is_named_rather_than_tolerated() {
        let mut bytes = signal_bytes();
        let at = 500 * FRAME_BYTES;
        bytes[at] ^= 0x01;
        assert_eq!(
            payload_matches_signal(&wav(&bytes), RATE_HZ),
            Err(WavMismatch::Differs { frame: 500 })
        );
    }

    #[test]
    fn the_header_must_carry_the_label_its_recorder_states() {
        let mut labelled = wav(&signal_bytes());
        labelled[24..28].copy_from_slice(&44_100u32.to_le_bytes());
        assert_eq!(payload_matches_signal(&labelled, 44_100), Ok(()));
        assert_eq!(
            payload_matches_signal(&labelled, RATE_HZ),
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
            payload_matches_signal(&wav(short), RATE_HZ),
            Err(WavMismatch::Short { .. })
        ));
        assert_eq!(
            payload_matches_signal(b"not a wav", RATE_HZ),
            Err(WavMismatch::TooShort)
        );
        assert_eq!(
            payload_matches_signal(&[0u8; 64], RATE_HZ),
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

        assert_eq!(payload_matches_signal(&wav, RATE_HZ), Ok(()));
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

        assert_eq!(payload_matches_signal(&wav, RATE_HZ), Ok(()));
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
        assert_eq!(payload_matches_signal(&file, RATE_HZ), Ok(()));
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
            payload_matches_signal(&wav(&held(HOLD_FRAME, 10)), RATE_HZ),
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
