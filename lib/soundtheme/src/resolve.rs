//! What a cue plays: nothing if the event is off, else the user's own file
//! for it, else the active theme's asset, else the shipped theme's, else
//! silence.
//!
//! Silence is an answer, not a fallback beep: a sound that is turned off,
//! or that no theme holds, makes none, and the caller is told why.

use tairix_abi::audio::AudioGain;

use crate::{Silence, SoundAsset, SoundEvent, SoundSettings, DEFAULT_THEME};

/// Where a cue's sound is read from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CueSource<'a> {
    /// The file the user chose for the event, which their session handed over.
    Own,
    /// A theme's copy of an asset.
    Theme {
        /// The theme.
        theme: &'a str,
        /// The asset.
        asset: SoundAsset,
    },
}

/// What a cue resolves to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Resolution<'a> {
    /// Play `source` at `gain`.
    Play {
        /// Where the sound is read from.
        source: CueSource<'a>,
        /// The level the settings turn every sound down to.
        gain: AudioGain,
    },
    /// Make no sound, for this reason.
    Silent(Silence),
}

/// Resolve `event` under `settings`.
///
/// `own` is whether the user's own file for the event was handed over, and
/// `holds(theme, asset)` whether a theme holds an asset; it is asked about the
/// active theme first and the shipped one only if the active theme does not.
pub fn resolve(
    event: SoundEvent,
    settings: &SoundSettings,
    own: bool,
    mut holds: impl FnMut(&str, SoundAsset) -> bool,
) -> Resolution<'_> {
    if !settings.enabled() || !settings.event(event).enabled {
        return Resolution::Silent(Silence::Disabled);
    }
    let gain = settings.gain();
    if own {
        return Resolution::Play {
            source: CueSource::Own,
            gain,
        };
    }
    let asset = SoundAsset::of(event);
    let active = settings.theme();
    core::iter::once(active)
        .chain((active != DEFAULT_THEME).then_some(DEFAULT_THEME))
        .find(|theme| holds(theme, asset))
        .map_or(Resolution::Silent(Silence::NoAsset), |theme| {
            Resolution::Play {
                source: CueSource::Theme { theme, asset },
                gain,
            }
        })
}

#[cfg(test)]
#[path = "resolve_tests.rs"]
mod tests;
