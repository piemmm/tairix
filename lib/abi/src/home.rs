//! The fixed shape of a user's home directory (`/Users/<name>/`).
//!
//! Every home holds the same named folders, so a program finds a user's
//! desktop, their files or their settings by name rather than by asking. The
//! names are defined here once: account provisioning creates them, and the
//! file manager, the desktop and every other reader import them. The user's
//! two program stores are named beside the store definitions
//! ([`HOME_COMMAND_STORE_DIR`](crate::HOME_COMMAND_STORE_DIR),
//! [`HOME_APPLICATION_STORE_DIR`](crate::HOME_APPLICATION_STORE_DIR)).

/// The folder whose contents the desktop shows as its icons.
pub const HOME_DESKTOP_DIR: &str = "Desktop";

/// Per-user caches and state, including the trash and the gated per-app data
/// root. Not shared libraries.
pub const HOME_LIBRARY_DIR: &str = "Library";

/// Per-user settings, including the gated per-app configuration root.
pub const HOME_SETTINGS_DIR: &str = "Settings";

/// The user's own files: where a bare file-manager window opens, holding one
/// folder per kind of document ([`USER_FILES_SUBDIRS`]).
pub const HOME_USER_FILES_DIR: &str = "UserFiles";

/// Documents in [`HOME_USER_FILES_DIR`].
pub const USER_FILES_DOCUMENTS_DIR: &str = "Documents";

/// Music in [`HOME_USER_FILES_DIR`].
pub const USER_FILES_MUSIC_DIR: &str = "Music";

/// Pictures in [`HOME_USER_FILES_DIR`].
pub const USER_FILES_PICTURES_DIR: &str = "Pictures";

/// Videos in [`HOME_USER_FILES_DIR`].
pub const USER_FILES_VIDEOS_DIR: &str = "Videos";

/// The folders a home's [`HOME_USER_FILES_DIR`] is provisioned with, sorted.
pub const USER_FILES_SUBDIRS: [&str; 4] = [
    USER_FILES_DOCUMENTS_DIR,
    USER_FILES_MUSIC_DIR,
    USER_FILES_PICTURES_DIR,
    USER_FILES_VIDEOS_DIR,
];

#[cfg(test)]
mod tests {
    use super::{
        HOME_DESKTOP_DIR, HOME_LIBRARY_DIR, HOME_SETTINGS_DIR, HOME_USER_FILES_DIR,
        USER_FILES_SUBDIRS,
    };

    /// A provisioner creates each name as a child of its parent, so each must
    /// be one plain component; a sorted, unique table lists deterministically.
    #[test]
    fn every_home_name_is_one_plain_component_and_the_user_files_are_sorted() {
        let all = [
            HOME_DESKTOP_DIR,
            HOME_LIBRARY_DIR,
            HOME_SETTINGS_DIR,
            HOME_USER_FILES_DIR,
        ]
        .into_iter()
        .chain(USER_FILES_SUBDIRS);
        for name in all {
            assert!(!name.is_empty() && !name.contains('/'), "{name:?}");
            assert!(name != "." && name != "..", "{name:?}");
        }
        assert!(USER_FILES_SUBDIRS.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
