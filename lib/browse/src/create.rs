//! The New ▸ model: the pure core of creating a folder or a blank document in
//! the current listing.
//!
//! Whether a name is acceptable — the one shared spelling rule
//! ([`tairix_path::validate_file_name`]) and a clash with a sibling — and what
//! a new entry is first called are decided here with no kernel. The app
//! supplies the create itself (`fs_mkdir`, or an exclusive `fs_open`); whether
//! it is called, and on what path, is
//! [`Browser::create_entry`](crate::Browser::create_entry)'s.
//!
//! Validation is spelling only. The create is an ordinary permission-checked
//! VFS call under the caller's own identity, which may still refuse an
//! accepted name ([`CreateError::Refused`]).

use alloc::format;
use alloc::string::String;
use alloc::vec;

use tairix_abi::Errno;
use tairix_path::PathError;

use crate::entry::Entry;
use crate::media::BlankDocument;

/// What a new folder is called until the user renames it.
pub const NEW_FOLDER_BASE: &str = "New Folder";

/// What New ▸ makes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NewEntry {
    /// A folder.
    Folder,
    /// An empty document.
    Document(BlankDocument),
}

impl NewEntry {
    /// The first name of this kind no sibling holds: `New Folder` or
    /// `New Text Document.txt`, then the same with ` 2`, ` 3`, … before any
    /// extension.
    ///
    /// Linear in `siblings`: `n` siblings can hold at most `n` of the first
    /// `n + 1` numbers, so only those are tracked. The VFS still has the final
    /// say when the create is attempted.
    #[must_use]
    pub fn suggest_name(self, siblings: &[Entry]) -> String {
        let (stem, extension) = match self {
            Self::Folder => (String::from(NEW_FOLDER_BASE), None),
            Self::Document(document) => (
                format!("New {}", document.noun()),
                Some(document.extension()),
            ),
        };
        let mut taken = vec![false; siblings.len() + 1];
        for entry in siblings {
            let slot = numbered(entry.name(), &stem, extension)
                .and_then(|number| taken.get_mut(number - 1));
            if let Some(slot) = slot {
                *slot = true;
            }
        }
        let number = taken
            .iter()
            .position(|held| !held)
            .map_or(taken.len() + 1, |at| at + 1);
        match (number, extension) {
            (1, None) => stem,
            (1, Some(extension)) => format!("{stem}.{extension}"),
            (number, None) => format!("{stem} {number}"),
            (number, Some(extension)) => format!("{stem} {number}.{extension}"),
        }
    }
}

/// The number `name` spells for `stem` — 1 for the bare name, `n` for
/// `stem n` — or `None` when it spells none.
fn numbered(name: &str, stem: &str, extension: Option<&str>) -> Option<usize> {
    let mut rest = name.strip_prefix(stem)?;
    if let Some(extension) = extension {
        rest = rest.strip_suffix(extension)?.strip_suffix('.')?;
    }
    if rest.is_empty() {
        return Some(1);
    }
    let digits = rest.strip_prefix(' ')?;
    if digits.starts_with('0') || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok().filter(|number| *number >= 2)
}

/// Why a new entry was not created.
///
/// The spelling failures and [`Clash`](Self::Clash) are decided before any
/// syscall, so the listing is untouched; [`Refused`](Self::Refused) and
/// [`Source`](Self::Source) carry the kernel's own reason.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CreateError {
    /// The name is empty.
    Empty,
    /// The name is `.` or `..`.
    Reserved,
    /// The name contains a `/`: it spells a path, not one name.
    Separator,
    /// The name contains a control character (including NUL) or a `:`, the
    /// reserved path delimiter.
    Invalid,
    /// The name, or the path it makes, is longer than the filesystem allows.
    TooLong,
    /// A sibling already has this name.
    Clash,
    /// The VFS refused the create; the listing is unchanged.
    Refused(Errno),
    /// The entry was created but the directory could no longer be re-listed.
    Source(Errno),
}

impl CreateError {
    /// Map a [`PathError`] from the shared name rule onto its spelling variant.
    #[must_use]
    pub(crate) fn from_path(err: PathError) -> Self {
        match err {
            PathError::EmptyComponent => Self::Empty,
            PathError::ReservedName => Self::Reserved,
            PathError::SeparatorInName => Self::Separator,
            PathError::ComponentTooLong => Self::TooLong,
            _ => Self::Invalid,
        }
    }

    /// A terse reason for the refusal line, naming no path.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Empty => "The name cannot be empty.",
            Self::Reserved => "\".\" and \"..\" are not valid names.",
            Self::Separator => "A name cannot contain \"/\".",
            Self::Invalid => "That name contains a character that is not allowed.",
            Self::TooLong => "That name is too long.",
            Self::Clash => "An item with that name already exists here.",
            Self::Refused(_) => "The new item could not be created.",
            Self::Source(_) => "Created, but this folder could not be reloaded.",
        }
    }
}

/// Validate `name` as a new entry in a directory listing `siblings`.
///
/// Pure: the shared spelling rule, then a clash with a sibling. No permission
/// decision is made here; that is the VFS's, at create time.
///
/// # Errors
///
/// The [`CreateError`] naming the first rule the name breaks.
pub fn validate_new_entry_name(name: &str, siblings: &[Entry]) -> Result<(), CreateError> {
    tairix_path::validate_file_name(name).map_err(CreateError::from_path)?;
    if siblings.iter().any(|entry| entry.name() == name) {
        return Err(CreateError::Clash);
    }
    Ok(())
}
