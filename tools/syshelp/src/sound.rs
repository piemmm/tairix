//! The shipped sounds' authoring contract (`plans/SOUND.md` §The shipped
//! set): what the build holds every theme asset to, and what
//! `cargo xtask sound-encode` holds a master to before it writes one.
//!
//! A bad sound degrades to silence at runtime, which looks exactly like
//! working, so the build decodes each asset in full rather than trusting its
//! name.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::audio::{ChannelMap, SampleFormat};
use tairix_sound::{DecodeLimits, PcmSource, SoundFormat};
use tairix_soundtheme::{MAX_CUE_SAMPLES, MAX_CUE_SECONDS};

/// The rate every shipped sound is authored at: the sinks' own rate, so a cue
/// at unity gain reaches the device with no conversion at all.
pub const SOUND_RATE_HZ: u32 = 48_000;

/// Samples one stereo frame interleaves.
const FRAME_SAMPLES: usize = 2;

/// Bytes of one interleaved stereo 16-bit frame.
const FRAME_BYTES: usize = FRAME_SAMPLES * 2;

/// Frames a shipped sound may hold: as long as a cue may sound. At the
/// shipped shape that is within the samples a cue may hold, so the one check
/// is both.
const MAX_FRAMES: usize = MAX_CUE_SECONDS as usize * SOUND_RATE_HZ as usize;

const _: () = assert!(MAX_FRAMES * FRAME_SAMPLES <= MAX_CUE_SAMPLES);

/// Decode `bytes` in full as `format`, answering its interleaved samples, or
/// why it breaks the contract: not 48 kHz stereo 16-bit, longer than a cue
/// may sound, a FLAC that does not prove its own digest, or a first or last
/// frame that is not silence, which would click.
///
/// # Errors
///
/// What is wrong with it, for the build to fail on.
pub fn shipped_samples(bytes: &[u8], format: SoundFormat) -> Result<Vec<i16>, String> {
    let mut input = bytes;
    let mut source = PcmSource::open_as(format, &mut input, &DecodeLimits::new(2, 0, 0))
        .map_err(|err| format!("does not open as {format:?}: {err}"))?;
    let info = source.info();
    if info.rate.hz() != SOUND_RATE_HZ
        || info.channels != ChannelMap::STEREO
        || info.sample != SampleFormat::S16
    {
        return Err(format!(
            "is {} Hz {:?} {:?}, not {SOUND_RATE_HZ} Hz stereo 16-bit",
            info.rate.hz(),
            info.channels,
            info.sample
        ));
    }
    let mut block = vec![0u8; 4096 * FRAME_BYTES];
    let mut samples = Vec::new();
    loop {
        let frames = source
            .next_block(&mut input, &mut block)
            .map_err(|err| format!("does not decode: {err}"))?;
        if frames == 0 {
            break;
        }
        if samples.len() / FRAME_SAMPLES + frames > MAX_FRAMES {
            return Err(format!(
                "sounds for longer than the {MAX_CUE_SECONDS} seconds a cue may"
            ));
        }
        samples.extend(
            block[..frames * FRAME_BYTES]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|&pair| i16::from_le_bytes(pair)),
        );
    }
    if format == SoundFormat::Flac && !source.verified() {
        return Err(String::from(
            "carries no digest to prove its samples against",
        ));
    }
    match (samples.first_chunk::<2>(), samples.last_chunk::<2>()) {
        (Some(&[0, 0]), Some(&[0, 0])) => Ok(samples),
        (None, _) | (_, None) => Err(String::from("holds no sound")),
        _ => Err(String::from("does not begin and end in silence")),
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::{shipped_samples, SOUND_RATE_HZ};
    use tairix_sound::flac_encode::{encode, Options, Params};
    use tairix_sound::SoundFormat;
    use tairix_soundtheme::MAX_CUE_SECONDS;

    fn flac(samples: &[i32], digest: bool) -> Vec<u8> {
        let params = Params {
            rate: SOUND_RATE_HZ,
            channels: 2,
            bits: 16,
        };
        let mut writer = encode(params, samples, Options::default()).expect("encodes");
        if !digest {
            writer.without_digest();
        }
        writer.finish()
    }

    fn chime() -> Vec<i32> {
        let mut samples = vec![0i32; 2];
        samples.extend((1..2_000).flat_map(|n| [(n % 300) - 150, 150 - (n % 300)]));
        samples.extend([0, 0]);
        samples
    }

    #[test]
    fn a_self_verifying_silent_ended_stereo_sound_passes() {
        let samples = chime();
        let decoded = shipped_samples(&flac(&samples, true), SoundFormat::Flac).expect("passes");
        let expected: Vec<i16> = samples
            .iter()
            .map(|&s| i16::try_from(s).expect("fits"))
            .collect();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn a_sound_breaking_the_contract_is_refused_with_its_reason() {
        let digestless = shipped_samples(&flac(&chime(), false), SoundFormat::Flac);
        assert!(digestless.unwrap_err().contains("digest"));
        let mut clicks = chime();
        let last = clicks.len() - 1;
        clicks[last] = 9;
        let ending = shipped_samples(&flac(&clicks, true), SoundFormat::Flac);
        assert!(ending.unwrap_err().contains("silence"));
        let mono = encode(
            Params {
                rate: SOUND_RATE_HZ,
                channels: 1,
                bits: 16,
            },
            &[0, 5, 0],
            Options::default(),
        )
        .expect("encodes")
        .finish();
        assert!(shipped_samples(&mono, SoundFormat::Flac)
            .unwrap_err()
            .contains("stereo"));
        assert!(shipped_samples(b"not a sound", SoundFormat::Flac).is_err());
        let bound = 2 * MAX_CUE_SECONDS as usize * SOUND_RATE_HZ as usize;
        let long = flac(&vec![0; bound + 2], true);
        assert!(shipped_samples(&long, SoundFormat::Flac)
            .unwrap_err()
            .contains("seconds a cue may"));
        let at_bound = flac(&vec![0; bound], true);
        assert_eq!(
            shipped_samples(&at_bound, SoundFormat::Flac).map(|s| s.len()),
            Ok(bound)
        );
    }
}
