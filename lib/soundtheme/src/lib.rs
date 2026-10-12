//! The desktop's sounds (`plans/SOUND.md` §Default desktop sounds): which
//! asset each [`SoundEvent`] plays, the shipped themes and the bounds a theme
//! is read within, the sound settings and the user's own layer over them, and
//! the client a program asks the cue authority with.
//!
//! It knows nothing of decoding (`lib/sound`) or of mixing (`lib/audio`). The
//! event vocabulary crosses a process boundary, so it is defined with the cue
//! protocol in `tairix_abi::sound_cue` and re-exported here.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

extern crate alloc;

pub mod catalog;
pub mod cue;
pub mod event;
#[cfg(feature = "settings")]
pub mod resolve;
#[cfg(feature = "settings")]
pub mod settings;
#[cfg(feature = "settings")]
pub mod user;

pub use catalog::{
    asset_path, is_theme_name, themes, DEFAULT_THEME, MACHINE_SETTINGS, MAX_CUE_SAMPLES,
    MAX_CUE_SECONDS, MAX_SOUND_BYTES, SOUND_STORE,
};
pub use cue::{
    cue, CuePost, CueTransport, PostedCues, CUE_ANSWER_DEADLINE_NS, DRAIN_SLACK_NS, MAX_POSTED,
    PLAYED_OUT_WAIT_NS, POLICY_ANSWER_DEADLINE_NS,
};
pub use event::SoundAsset;
#[cfg(feature = "settings")]
pub use resolve::{resolve, CueSource, Resolution};
#[cfg(feature = "settings")]
pub use settings::{EventSettings, SoundKey, SoundSettings};
pub use tairix_abi::sound_cue::{Answer, CueAuthority, CueOutcome, Silence, SoundEvent};
#[cfg(feature = "settings")]
pub use user::{layered, merge_pushed, pushable, UserChoices, PUSHABLE};
