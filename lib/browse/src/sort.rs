//! The one listing sort order the file manager and the trusted picker share.
//!
//! Both views drive the same [`Browser`](crate::Browser), so the order they
//! show a directory in must come from a single definition — otherwise the two
//! could disagree about which row a click lands on. [`sort_entries`] is that
//! definition: a pure, total, stable order over [`Entry`], parameterised by a
//! [`SortMode`] the app may toggle.
//!
//! The order is **directories first, then the chosen key**, unless the mode
//! asks for folders among the files ([`Folders::Among`]). Grouped, folders
//! stay ahead of files (and bundles) whichever the direction — reversing only
//! reverses the order *within* each group — because a file manager that
//! scattered folders through a descending size list would be harder to scan,
//! not easier. Equal keys fall back to a deterministic case-insensitive name
//! order so the result never depends on the source's incidental ordering.

use core::cmp::Ordering;

use alloc::vec::Vec;

use crate::entry::{Entry, EntryKind};
use crate::media::{media_for_named, Site};

/// Which field a listing is ordered by.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub enum SortKey {
    /// Case-insensitive name order — the sensible default for a general
    /// directory listing (a person scanning for a name).
    #[default]
    Name,
    /// The entry's structural kind — directory, then application bundle,
    /// then plain file — with files further ordered by the name of their
    /// content type ([`MediaType::name`](crate::MediaType::name)), the type a
    /// listing's type column shows. A bundle's type needs the listed
    /// directory to tell a service from an application, which one listing
    /// shares, so bundles order among themselves by name.
    Kind,
    /// Apparent byte size. Directories and bundles have size `0`, so within
    /// their group this collapses to the name tiebreak.
    Size,
    /// Last-modification instant.
    Modified,
    /// The space a volume root's volume has free; entries that are no
    /// volume's root come first, as having none.
    Free,
    /// A volume root's filesystem name.
    Filesystem,
    /// How well a volume root's volume is serving, healthiest first.
    Health,
}

impl SortKey {
    /// Every key, in the order a menu offering them numbers its rows.
    pub const ALL: [Self; 7] = [
        Self::Name,
        Self::Kind,
        Self::Size,
        Self::Modified,
        Self::Free,
        Self::Filesystem,
        Self::Health,
    ];
}

/// Ascending or descending within the [`SortKey`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub enum SortDirection {
    /// Smallest / earliest / A→Z first.
    #[default]
    Ascending,
    /// Largest / latest / Z→A first.
    Descending,
}

impl SortDirection {
    /// Both directions, in the order a menu offering them numbers its rows.
    pub const ALL: [Self; 2] = [Self::Ascending, Self::Descending];
}

/// Where folders stand in a listing's order.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub enum Folders {
    /// Ahead of every file and bundle, whichever the key and direction.
    #[default]
    First,
    /// Among the files, ordered by the key like everything else.
    Among,
}

/// How a listing is ordered: a [`SortKey`], a [`SortDirection`], and where
/// [`Folders`] stand.
///
/// The default — [`SortKey::Name`] ascending, folders first — is the
/// general-purpose listing order both views open with.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Default)]
pub struct SortMode {
    /// The field ordered by.
    pub key: SortKey,
    /// The direction within the key.
    pub direction: SortDirection,
    /// Where folders stand.
    pub folders: Folders,
}

impl SortMode {
    /// The listing order both views open with: by name, ascending, folders
    /// first.
    #[must_use]
    pub const fn default_order() -> Self {
        Self {
            key: SortKey::Name,
            direction: SortDirection::Ascending,
            folders: Folders::First,
        }
    }

    /// The next order in the fixed cycle the toolbar's Sort command steps
    /// through: name ↑, name ↓, kind ↑, kind ↓, size ↑, size ↓, modified ↑,
    /// modified ↓, then back to name ↑. A volume's own orders, which only a
    /// list's column header chooses, step back to name ↑. The wrap makes it
    /// total.
    #[must_use]
    pub const fn next(self) -> Self {
        use SortDirection::{Ascending, Descending};
        use SortKey::{Filesystem, Free, Health, Kind, Modified, Name, Size};
        let (key, direction) = match (self.key, self.direction) {
            (Name, Ascending) => (Name, Descending),
            (Name, Descending) => (Kind, Ascending),
            (Kind, Ascending) => (Kind, Descending),
            (Kind, Descending) => (Size, Ascending),
            (Size, Ascending) => (Size, Descending),
            (Size, Descending) => (Modified, Ascending),
            (Modified, Ascending) => (Modified, Descending),
            (Modified, Descending) | (Free | Filesystem | Health, _) => (Name, Ascending),
        };
        Self {
            key,
            direction,
            folders: self.folders,
        }
    }

    /// The order a press on the column header ordering by `key` asks for: by
    /// that key ascending, or reversed when it is the order already in force.
    #[must_use]
    pub fn pressed(self, key: SortKey) -> Self {
        let direction = if self.key == key && self.direction == SortDirection::Ascending {
            SortDirection::Descending
        } else {
            SortDirection::Ascending
        };
        Self {
            key,
            direction,
            ..self
        }
    }
}

/// Sort `entries` in place into the shared listing order for `mode`.
///
/// Stable and total: directories come before files and bundles unless
/// `mode` places them among the files, then the chosen key decides, then a
/// case-insensitive name tiebreak makes the result independent of the input
/// order.
pub fn sort_entries(entries: &mut [Entry], mode: SortMode) {
    if mode.key != SortKey::Kind {
        entries.sort_by(|a, b| entry_cmp(a, b, mode));
        return;
    }
    // Classifying a name scans the type registry's tables, so each entry is
    // classified once rather than at every comparison.
    let mut order: Vec<(usize, &'static str)> = entries
        .iter()
        .enumerate()
        .map(|(at, entry)| (at, type_name(entry)))
        .collect();
    order.sort_by(|a, b| ordered(&entries[a.0], &entries[b.0], mode, (a.1, b.1)));
    permute(entries, order.into_iter().map(|(at, _)| at).collect());
}

/// Rearrange `entries` so the entry now at `order[i]` ends at `i`, following
/// each cycle of the permutation so every entry moves once.
fn permute(entries: &mut [Entry], mut order: Vec<usize>) {
    for start in 0..order.len() {
        let mut at = start;
        while order[at] != start {
            let from = order[at];
            entries.swap(at, from);
            order[at] = at;
            at = from;
        }
        order[at] = at;
    }
}

/// The name of `entry`'s content type, which [`SortKey::Kind`] orders files
/// by within their structural rank.
fn type_name(entry: &Entry) -> &'static str {
    media_for_named(entry.name(), entry.kind(), Site::Plain).name()
}

/// The group an entry sorts into under `folders`: directories (`0`) ahead of
/// everything else (`1`), or one group for all. A bundle is a launchable unit,
/// not a folder to descend, so it sorts with files rather than with
/// directories.
fn group_rank(entry: &Entry, folders: Folders) -> u8 {
    match folders {
        Folders::First => u8::from(!entry.is_directory()),
        Folders::Among => 0,
    }
}

/// The total order two entries take under `mode`.
pub(crate) fn entry_cmp(a: &Entry, b: &Entry, mode: SortMode) -> Ordering {
    let types = if mode.key == SortKey::Kind {
        (type_name(a), type_name(b))
    } else {
        ("", "")
    };
    ordered(a, b, mode, types)
}

/// [`entry_cmp`]'s order, given what [`type_name`] answers for each entry
/// under [`SortKey::Kind`]: the one comparison both the one-off sort and the
/// incremental insertions read.
fn ordered(a: &Entry, b: &Entry, mode: SortMode, types: (&str, &str)) -> Ordering {
    let group = |entry| group_rank(entry, mode.folders);
    group(a).cmp(&group(b)).then_with(|| {
        let primary = match mode.key {
            SortKey::Name => name_cmp(a.name(), b.name()),
            SortKey::Kind => kind_rank(a)
                .cmp(&kind_rank(b))
                .then_with(|| name_cmp(types.0, types.1)),
            SortKey::Size => a.size().cmp(&b.size()),
            SortKey::Modified => a.modified().cmp(&b.modified()),
            SortKey::Free => free(a).cmp(&free(b)),
            SortKey::Filesystem => name_cmp(filesystem(a), filesystem(b)),
            SortKey::Health => health(a).cmp(&health(b)),
        };
        let primary = match mode.direction {
            SortDirection::Ascending => primary,
            SortDirection::Descending => primary.reverse(),
        };
        // A deterministic, direction-independent tiebreak so equal keys never
        // leave the order at the mercy of the source's incidental ordering.
        primary.then_with(|| name_cmp(a.name(), b.name()))
    })
}

/// Compare two names case-insensitively (ASCII fold), breaking an
/// otherwise-equal comparison by the raw bytes so distinct names never
/// compare equal. Allocation-free: it folds byte by byte rather than building
/// lowercased copies.
fn name_cmp(a: &str, b: &str) -> Ordering {
    let folded = a
        .bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .cmp(b.bytes().map(|byte| byte.to_ascii_lowercase()));
    folded.then_with(|| a.as_bytes().cmp(b.as_bytes()))
}

/// The rank [`SortKey::Kind`] clusters an entry's structural kind into:
/// directories first, then application bundles, then plain files. This is
/// finer than [`group_rank`], which only separates directories from
/// everything else.
const fn kind_rank(entry: &Entry) -> u8 {
    // A link ranks with what it *names*, so a shortcut to a folder sorts
    // among the folders; one that names nothing ranks with the files, where
    // an unopenable leaf belongs.
    match entry.kind().resolved() {
        Some(EntryKind::Directory) => 0,
        Some(EntryKind::Bundle) => 1,
        Some(EntryKind::File | EntryKind::Link(_)) | None => 2,
    }
}

/// The space `entry`'s volume has free, `None` for an entry that is no
/// volume's root or a volume that keeps no accounting.
fn free(entry: &Entry) -> Option<u64> {
    entry.volume().and_then(|facts| facts.free)
}

/// The filesystem `entry`'s volume carries, empty for an entry that is no
/// volume's root.
fn filesystem(entry: &Entry) -> &str {
    entry.volume().map_or("", |facts| facts.filesystem.as_str())
}

/// How well `entry`'s volume is serving, `None` for an entry that is no
/// volume's root.
fn health(entry: &Entry) -> Option<tairix_abi::sysinfo::VolumeHealth> {
    entry.volume().map(|facts| facts.availability.health())
}

#[cfg(test)]
#[path = "sort_tests.rs"]
mod tests;
