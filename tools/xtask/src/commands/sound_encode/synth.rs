//! The shipped theme's synthesised masters: sounds made by code rather than
//! recorded, so each is remade from the tree and a test holds the shipped
//! file to what its synthesis makes.
//!
//! Every sample is computed with `tairix_util::mathf`, one fixed order of
//! basic operations on every host, so a synthesis made anywhere is the one
//! committed.

use core::f64::consts::{PI, TAU};

use tairix_soundtheme::SoundAsset;
use tairix_syshelp::sound::SOUND_RATE_HZ;
use tairix_util::mathf::{cos, exp, fabs, fmax, round_i32, sin};

/// A master's interleaved stereo samples, or why they could not be made.
pub type Synthesis = fn() -> Result<Vec<i16>, String>;

/// Every synthesised master, and the asset it is.
pub const SYNTHESISED: [(SoundAsset, Synthesis); 1] = [(SoundAsset::Volume, volume)];

/// Frames of silence the volume's feedback opens with.
const LEAD_FRAMES: u32 = 240;

/// Frames its tone sounds for, its fall included.
const TONE_FRAMES: u32 = 4_320;

/// Frames of silence after the tone: the shipped family's 12 ms tail.
const TAIL_FRAMES: u32 = 576;

/// Frames the tone rises over from silence, so it starts without a click.
const RISE_FRAMES: u32 = 96;

/// Frames the tone falls back to silence over.
const FALL_FRAMES: u32 = 960;

/// The pitch the tone settles on, in hertz: C6, clear on the smallest speaker.
const PITCH_HZ: f64 = 1_046.5;

/// How far above that pitch the tone starts, against it: the fall onto the
/// pitch is what makes it a pop rather than a beep.
const DROP: f64 = 0.12;

/// The time constant the pitch falls with, in seconds.
const DROP_SECONDS: f64 = 0.012;

/// The time constant the tone dies away with, in seconds.
const DECAY_SECONDS: f64 = 0.03;

/// The octave above, against the fundamental.
const OVERTONE: f64 = 0.18;

/// The time constant the octave dies away with, in seconds.
const OVERTONE_SECONDS: f64 = 0.012;

/// The loudest sample: -12 dBFS, among the family's notifications.
const PEAK: f64 = 8_231.0;

/// The volume's feedback: a short pop falling onto C6 as it dies away.
///
/// # Errors
///
/// A tone that sounds nothing or a sample past full scale, which the
/// constants above cannot make.
pub fn volume() -> Result<Vec<i16>, String> {
    let rate = f64::from(SOUND_RATE_HZ);
    let tone: Vec<f64> = (0..TONE_FRAMES)
        .map(|frame| {
            let at = f64::from(frame) / rate;
            let turns = PITCH_HZ * (at + DROP * DROP_SECONDS * (1.0 - exp(-at / DROP_SECONDS)));
            let voice = exp(-at / DECAY_SECONDS) * sin(TAU * turns)
                + OVERTONE * exp(-at / OVERTONE_SECONDS) * sin(2.0 * TAU * turns);
            voice * envelope(frame)
        })
        .collect();
    let loudest = tone
        .iter()
        .fold(0.0, |peak, &sample| fmax(peak, fabs(sample)));
    if loudest <= 0.0 {
        return Err(String::from("sounds nothing"));
    }
    let silence = |frames: u32| (0..frames).flat_map(|_| [0i16; 2]);
    let mut samples: Vec<i16> = silence(LEAD_FRAMES).collect();
    for value in tone {
        let sample = i16::try_from(round_i32(value * PEAK / loudest))
            .map_err(|_| String::from("a sample is past full scale"))?;
        samples.extend([sample; 2]);
    }
    samples.extend(silence(TAIL_FRAMES));
    Ok(samples)
}

/// How far the tone stands at `frame`: risen from silence and, at its end,
/// fallen back to it, each along a half cosine.
fn envelope(frame: u32) -> f64 {
    let rise = if frame < RISE_FRAMES {
        half_cosine(frame, RISE_FRAMES)
    } else {
        1.0
    };
    let left = TONE_FRAMES.saturating_sub(frame);
    let fall = if left < FALL_FRAMES {
        half_cosine(left, FALL_FRAMES)
    } else {
        1.0
    };
    rise * fall
}

/// From silence to full along a half cosine, `done` frames of `over` in.
fn half_cosine(done: u32, over: u32) -> f64 {
    0.5 - 0.5 * cos(PI * f64::from(done) / f64::from(over))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tairix_sound::SoundFormat;
    use tairix_soundtheme::DEFAULT_THEME;
    use tairix_syshelp::sound::shipped_samples;

    use super::super::{encode_samples, ASSETS_DIR};

    use super::{volume, LEAD_FRAMES, SYNTHESISED, TAIL_FRAMES, TONE_FRAMES};

    /// The feedback is the same every time: a short pop at its stated peak,
    /// alike in both channels, silent where it opens, and what the encoder
    /// ships as it is.
    #[test]
    fn the_volumes_feedback_is_a_short_click_free_pop_at_its_peak() {
        let samples = volume().expect("synthesised");
        assert_eq!(
            samples,
            volume().expect("synthesised"),
            "the same every time"
        );
        let frames = LEAD_FRAMES + TONE_FRAMES + TAIL_FRAMES;
        assert_eq!(
            samples.len(),
            2 * usize::try_from(frames).expect("a length")
        );
        assert!(
            samples
                .as_chunks::<2>()
                .0
                .iter()
                .all(|[left, right]| left == right),
            "centred"
        );
        let loudest = samples.iter().copied().map(i16::unsigned_abs).max();
        assert_eq!(loudest, Some(8_231), "-12 dBFS");
        let lead = 2 * usize::try_from(LEAD_FRAMES).expect("a length");
        assert!(samples[..lead].iter().all(|&sample| sample == 0));
        let flac = encode_samples(&samples).expect("true to the authoring contract");
        assert!(flac.starts_with(b"fLaC"));
    }

    /// The shipped file is its synthesis sample for sample, so the theme's
    /// copy is remade by `cargo xtask sound-encode --synthesised`, never
    /// edited.
    #[test]
    fn every_synthesised_master_ships_as_its_synthesis() {
        let theme = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("xtask sits two levels below the workspace root")
            .join(ASSETS_DIR)
            .join(DEFAULT_THEME);
        for (asset, synthesise) in SYNTHESISED {
            let file = theme.join(format!("{}.flac", asset.name()));
            let shipped = std::fs::read(&file).expect("the theme ships every synthesised master");
            assert_eq!(
                shipped_samples(&shipped, SoundFormat::Flac),
                synthesise(),
                "{}: run `cargo xtask sound-encode --synthesised`",
                file.display()
            );
        }
    }
}
