//! Shortcuts to volumes, and the drives the desktop shows for attached ones.
//!
//! A shortcut to a volume is a symbolic link whose target is the volume's root
//! by the identity its content carries, `id::<identity>/`
//! (`docs/src/filesystem/drives.md` §8), so it finds the medium again however
//! it is attached next. No walk follows such a link, so where the volume is now
//! is what the mount table says, and [`locate`] is the one reading of it. The
//! desktop also shows a drive for each attached volume of a class the file
//! manager's settings name ([`DesktopDrives`]): those are drawn, never written
//! as files.
//!
//! Nothing here opens or lists anything: every answer is read off the
//! [`Volume`]s a caller learned from the mount table.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::sysinfo::{MountAvailability, VolumeClass, VOLUME_ID_LEN, VOLUME_ID_UNPUBLISHED};
use tairix_abi::time::Time64;
use tairix_abi::STORAGE_CATALOG;
use tairix_icon::disk_icon;

use crate::entry::{DriveMark, Entry, EntryKind, LinkTarget, Listed};
use crate::places::{Volume, VolumeFacts};
use crate::vfs::components_from_absolute_path;

/// The identity the shortcut target `target` names a volume's root by, or
/// `None` for a target naming anything else: a path within a volume, another
/// root, or the reserved unpublished identity, which names no volume.
#[must_use]
pub fn reference(target: &str) -> Option<[u8; VOLUME_ID_LEN]> {
    let path = tairix_path::parse(target).ok()?;
    if !path.components().is_empty() {
        return None;
    }
    let identity = *path.volume_id()?.as_bytes();
    (identity != VOLUME_ID_UNPUBLISHED).then_some(identity)
}

/// Where the volume a shortcut names is now, as the mount table last said.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Located<'a> {
    /// Attached and shown to this principal: opened at its catalog entry, as
    /// the storage catalog opens it.
    Present(&'a Volume),
    /// Not attached, or attached but its medium has gone.
    Absent,
    /// Attached, but presented to another principal alone.
    Withheld,
}

impl<'a> Located<'a> {
    /// The volume, where it is there to open.
    #[must_use]
    pub const fn present(self) -> Option<&'a Volume> {
        match self {
            Self::Present(volume) => Some(volume),
            Self::Absent | Self::Withheld => None,
        }
    }

    /// Why the shortcut named `name` cannot be opened, as its user is told —
    /// the headline and the reason — or `None` for a volume there to open.
    #[must_use]
    pub fn refusal(self, name: &str) -> Option<(String, &'static str)> {
        let shown = tairix_util::utf8::shown(name);
        let (state, reason) = match self {
            Self::Present(_) => return None,
            Self::Absent => ("not present", "The drive it stands for is not attached."),
            Self::Withheld => (
                "not available",
                "The drive it stands for is attached for someone else.",
            ),
        };
        Some((format!("\u{201c}{shown}\u{201d} is {state}"), reason))
    }
}

/// Where the volume the shortcut target `target` names is among `volumes`, the
/// mount table as last read.
///
/// The volume is found by the identity its content carries, so a medium
/// attached again — under another catalog name, or as a twin under a key of
/// its own — is found too; of several attachments of one identity, the one
/// published under the identity itself is the one shown. A target naming no
/// volume's root names nothing present.
#[must_use]
pub fn locate<'a>(target: &str, volumes: &'a [Volume]) -> Located<'a> {
    let Some(identity) = reference(target) else {
        return Located::Absent;
    };
    let mut present: Option<&Volume> = None;
    let mut withheld = false;
    for volume in volumes
        .iter()
        .filter(|volume| volume.facts.identity == identity)
    {
        if volume.offered {
            if present.is_none() || volume.facts.volume_id == identity {
                present = Some(volume);
            }
        } else {
            withheld |= !gone(volume.facts.availability);
        }
    }
    match present {
        Some(volume) => Located::Present(volume),
        None if withheld => Located::Withheld,
        None => Located::Absent,
    }
}

/// Whether a volume's medium has gone, which leaves it in the mount table but
/// no place to go.
pub(crate) const fn gone(availability: MountAvailability) -> bool {
    matches!(
        availability,
        MountAvailability::UnavailableDirty | MountAvailability::UnavailableLost
    )
}

/// What kind of drive a volume is, as the settings choosing which drives the
/// desktop shows group them.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DriveClass {
    /// The volume the view's root `/` is.
    System,
    /// A local disk's volume in the storage catalog.
    Fixed,
    /// A volume on a device that can be pulled from the machine.
    Removable,
    /// A network share.
    Network,
    /// A volume a disk image or an archive holds.
    Images,
}

impl DriveClass {
    /// Every class, in the order the settings list them.
    pub const ALL: [Self; 5] = [
        Self::System,
        Self::Fixed,
        Self::Removable,
        Self::Network,
        Self::Images,
    ];

    /// Where the class stands in [`Self::ALL`].
    const fn index(self) -> usize {
        self as usize
    }

    /// What a setting calls the class.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::System => "The system drive",
            Self::Fixed => "Fixed drives",
            Self::Removable => "Removable drives",
            Self::Network => "Network shares",
            Self::Images => "Disk images and archives",
        }
    }

    /// The class of `volume`: the system drive for the view's root, an entry
    /// of the storage catalog by what backs it, and none for any other mount,
    /// the system tree's own.
    #[must_use]
    pub fn of(volume: &Volume) -> Option<Self> {
        if !volume.target.starts_with('/') {
            return None;
        }
        let components = components_from_absolute_path(&volume.target).ok()?;
        match components.as_slice() {
            [] => Some(Self::System),
            [catalog, _] if catalog == STORAGE_CATALOG => Some(match volume.facts.class {
                VolumeClass::Local => Self::Fixed,
                VolumeClass::Removable => Self::Removable,
                VolumeClass::Network => Self::Network,
                VolumeClass::Image => Self::Images,
            }),
            _ => None,
        }
    }
}

/// Which classes of drive the desktop shows an icon for, while each is
/// attached.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DesktopDrives {
    shown: [bool; DriveClass::ALL.len()],
}

impl Default for DesktopDrives {
    /// Removable drives alone: what comes and goes is what is looked for on a
    /// desktop.
    fn default() -> Self {
        Self::NONE.with(DriveClass::Removable, true)
    }
}

impl DesktopDrives {
    /// No class shown.
    pub const NONE: Self = Self {
        shown: [false; DriveClass::ALL.len()],
    };

    /// Whether the desktop shows drives of `class`.
    #[must_use]
    pub const fn shows(self, class: DriveClass) -> bool {
        self.shown[class.index()]
    }

    /// These settings with drives of `class` shown or not.
    #[must_use]
    pub const fn with(mut self, class: DriveClass, shown: bool) -> Self {
        self.shown[class.index()] = shown;
        self
    }

    /// The setting for `class`, for a caller changing it.
    pub fn setting_mut(&mut self, class: DriveClass) -> &mut bool {
        &mut self.shown[class.index()]
    }
}

/// Whether the desktop shows a drive for `volume` under `drives`: attached,
/// shown to this principal, and of a class `drives` names.
#[must_use]
pub fn shown_on_desktop(volume: &Volume, drives: DesktopDrives) -> bool {
    matches!(DriveClass::of(volume), Some(class) if volume.offered && drives.shows(class))
}

/// The icon the desktop shows for the attached `volume`: named by its root,
/// which no listed name can be, since it holds a `/`; shown by the volume's
/// title, else its catalog name; and drawn as the drive it is on. `None` for a
/// volume that publishes no identity to keep its place by.
#[must_use]
pub fn drive_entry(volume: &Volume) -> Option<Entry> {
    let identity = volume.facts.identity;
    if identity == VOLUME_ID_UNPUBLISHED {
        return None;
    }
    let root = crate::mount::volume_root(identity);
    let label = if volume.facts.title.is_empty() {
        &volume.label
    } else {
        &volume.facts.title
    };
    let mut entry = Entry::new(
        root.clone(),
        EntryKind::Link(LinkTarget::Volume),
        0,
        Time64::UNIX_EPOCH,
    )
    .with_target(root)
    .with_label(label.clone());
    entry.set_drive(Some(&drive_mark(volume)));
    Some(entry)
}

/// What an icon standing for the attached `volume` is drawn as: the drive it
/// is on, stating what its record says of it.
#[must_use]
pub fn drive_mark(volume: &Volume) -> DriveMark {
    DriveMark {
        icon: disk_icon(volume.medium),
        facts: volume.facts.clone(),
    }
}

/// The shortcut each of `entries`, entries of the storage catalog, makes where
/// a drop links it — its name, and the volume's root by the identity its
/// content carries, so it finds the medium again however it is attached next
/// — or `None` where one is no attached volume's own root.
#[must_use]
pub fn shortcuts<'a>(
    entries: impl IntoIterator<Item = &'a Entry>,
) -> Option<Vec<(String, String)>> {
    entries
        .into_iter()
        .map(|entry| shortcut_to(entry.name(), entry.volume()?))
        .collect()
}

/// The shortcut each of `items`, dropped by their absolute paths, makes where
/// a drop links it, as [`shortcuts`] answers for the catalog's own entries:
/// each must be an attached volume of `volumes` where the storage catalog
/// shows it, else `None`.
#[must_use]
pub fn catalog_shortcuts(items: &[Listed], volumes: &[Volume]) -> Option<Vec<(String, String)>> {
    items
        .iter()
        .map(|item| {
            let [catalog, name] = item.path.as_slice() else {
                return None;
            };
            if catalog != STORAGE_CATALOG {
                return None;
            }
            let at = crate::vfs::spell_absolute_path(&item.path);
            let volume = volumes.iter().find(|volume| volume.target == at)?;
            shortcut_to(name, &volume.facts)
        })
        .collect()
}

/// The shortcut called `name` to the volume `facts` describes: its root by
/// the identity its content carries, or `None` for one that has none.
fn shortcut_to(name: &str, facts: &VolumeFacts) -> Option<(String, String)> {
    (facts.identity != VOLUME_ID_UNPUBLISHED).then(|| {
        (
            String::from(name),
            crate::mount::volume_root(facts.identity),
        )
    })
}

#[cfg(test)]
#[path = "volume_tests.rs"]
mod tests;
