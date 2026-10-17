//! A setting that names a file: a canonical absolute path in the session's
//! `/` view, bounded to fit one value.

use alloc::string::{String, ToString};
use core::fmt;

use crate::MAX_VALUE_LEN;

/// The longest path a setting names.
///
/// A fixed bound on untrusted input: a legitimate path is a handful of
/// components, so this caps the hostile work one value can demand before the
/// path parser runs, and it sits inside [`MAX_VALUE_LEN`] so any path accepted
/// here can be stored.
pub const MAX_SETTING_PATH_LEN: usize = 1024;

const _: () = assert!(
    MAX_SETTING_PATH_LEN <= MAX_VALUE_LEN,
    "a setting's path must fit one value"
);

/// Why a path-valued setting was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SettingPathError {
    /// Longer than [`MAX_SETTING_PATH_LEN`].
    TooLong,
    /// Empty, relative, not rooted in the `/` view, or not a path at all.
    Malformed,
}

impl fmt::Display for SettingPathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLong => f.write_str("the path is too long"),
            Self::Malformed => f.write_str("not an absolute path"),
        }
    }
}

/// A validated path a setting names, held in the shared parser's canonical
/// spelling, so two spellings of one path are one value.
///
/// Only the spelling is vouched for: whoever opens it reads untrusted content
/// under its own identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SettingPath(String);

impl SettingPath {
    /// Validate `text` as a path a setting may name.
    ///
    /// # Errors
    ///
    /// [`SettingPathError::TooLong`] or [`SettingPathError::Malformed`].
    pub fn new(text: &str) -> Result<Self, SettingPathError> {
        if text.len() > MAX_SETTING_PATH_LEN {
            return Err(SettingPathError::TooLong);
        }
        let parsed = tairix_path::parse(text).map_err(|_| SettingPathError::Malformed)?;
        if !matches!(parsed.root(), tairix_path::Root::View) || parsed.components().is_empty() {
            return Err(SettingPathError::Malformed);
        }
        Ok(Self(parsed.to_string()))
    }

    /// The canonical spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The canonical spelling, owned.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for SettingPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::{SettingPath, SettingPathError, MAX_SETTING_PATH_LEN};

    #[test]
    fn a_path_is_held_in_its_canonical_spelling() {
        let path = SettingPath::new("/Users/ada/Home/./Music/../Music/bell.flac")
            .expect("an absolute path");
        assert_eq!(path.as_str(), "/Users/ada/Home/Music/bell.flac");
        assert_eq!(path.into_string(), "/Users/ada/Home/Music/bell.flac");
    }

    #[test]
    fn only_a_bounded_absolute_view_path_is_one() {
        for refused in ["", "relative/file.flac", "/", "Music:/bell.flac"] {
            assert_eq!(
                SettingPath::new(refused),
                Err(SettingPathError::Malformed),
                "{refused}"
            );
        }
        let long = alloc::format!("/{}", "a".repeat(MAX_SETTING_PATH_LEN));
        assert_eq!(SettingPath::new(&long), Err(SettingPathError::TooLong));
    }
}
