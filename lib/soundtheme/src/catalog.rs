//! Where the themes and the machine's sound document are, and the bounds
//! every theme is read within.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::event::SOUND_FILE_EXTENSION;
use crate::SoundAsset;

/// The directory each theme is a subdirectory of.
pub const SOUND_STORE: &str = "/System/Audio/Sounds";

/// The shipped theme, and the one every other theme falls back to.
pub const DEFAULT_THEME: &str = "TAIRiX";

/// The machine's sound document: administrator policy, the whole of it
/// before a session claims the seat and the layer beneath a user's choices
/// while one holds it.
pub const MACHINE_SETTINGS: &str = "/System/Settings/Sound/sound.conf";

/// The largest sound asset read.
///
/// A containment bound on untrusted input, not a capacity: a user's theme is
/// read in full before it is decoded, and a cue is seconds long, so a few
/// times the largest shipped asset is ample.
pub const MAX_SOUND_BYTES: usize = 1024 * 1024;

/// The most samples, across its channels, a cue decodes to: just under
/// eleven seconds of 48 kHz stereo.
///
/// A containment bound, not a capacity: a FLAC holds a minute of silence in a
/// few hundred bytes, so the byte bound alone does not bound what a cue
/// holds. A decode past it is refused, and the build holds every shipped
/// asset to it.
pub const MAX_CUE_SAMPLES: usize = 1 << 20;

/// The longest a cue may sound. A cue marks a moment; a sound longer than
/// this is not one, however few samples a slow rate spends on it.
pub const MAX_CUE_SECONDS: u32 = 10;

/// Whether `name` is a legal theme directory: a plain leaf name, which is also
/// the label a chooser draws.
#[must_use]
pub fn is_theme_name(name: &str) -> bool {
    tairix_path::validate_file_name(name).is_ok()
}

/// The themes a listing of [`SOUND_STORE`] names: each legal theme name once,
/// in name order, which is the order a chooser offers them in.
#[must_use]
pub fn themes<S: AsRef<str>>(listing: impl IntoIterator<Item = S>) -> Vec<String> {
    let mut themes: Vec<String> = listing
        .into_iter()
        .filter(|name| is_theme_name(name.as_ref()))
        .map(|name| String::from(name.as_ref()))
        .collect();
    themes.sort_unstable();
    themes.dedup();
    themes
}

/// Where `theme` keeps `asset`, or `None` for an illegal theme name.
#[must_use]
pub fn asset_path(theme: &str, asset: SoundAsset) -> Option<String> {
    is_theme_name(theme).then(|| {
        format!(
            "{SOUND_STORE}/{theme}/{}{SOUND_FILE_EXTENSION}",
            asset.name()
        )
    })
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
